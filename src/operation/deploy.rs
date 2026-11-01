use crate::adt::{AdtMessage, parse_messages, prog_uri};
use crate::backend::{Failure, Selection};
use crate::config::{Connection, tls_warning};
use crate::domain::{Outcome, Status};
use crate::hash::{normalize, source_hash, validate_prog};
use crate::transport::{Request, Response, SadtTransport, TransportError};
use quick_xml::escape::{escape, resolve_predefined_entity};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use similar::TextDiff;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::adt::format_messages;

const LOCK_ACCEPT: &str =
    "application/*,application/vnd.sap.as+xml;charset=UTF-8;dataname=com.sap.adt.lock.result";
const ROLLBACK: &str = "ROLLBACK: the inactive version belongs to you; GET without a version returns it; restore only by deploying the base source again on your order";

#[derive(Clone, Debug)]
pub struct DeployInput {
    pub name: String,
    pub source: Vec<String>,
    pub base: Option<String>,
    pub simulate: bool,
    pub tr: Option<String>,
}

/// Exit code of an interrupt that arrives while no lock is held: 128 + SIGINT.
pub const INTERRUPT_EXIT_CODE: i32 = 130;

/// What a SIGINT means right now: leave at once, or let the lock holder unlock first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptAction {
    ExitNow,
    Deferred,
}

/// Set from the Ctrl-C handler; checked once the lock is held.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag {
    cancelled: Arc<AtomicBool>,
    lock_held: Arc<AtomicBool>,
}

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Marks the window in which the deploy holds a lock; the guard of the caller's scope ends it.
    pub fn hold_lock(&self) -> LockScope<'_> {
        self.lock_held.store(true, Ordering::SeqCst);

        LockScope(self)
    }

    pub fn lock_held(&self) -> bool {
        self.lock_held.load(Ordering::SeqCst)
    }

    /// A SIGINT must not end the process while a lock is held: the flag then makes the deploy
    /// unlock before it reports. Without a lock there is nothing to clean up, so exit at once.
    pub fn on_interrupt(&self) -> InterruptAction {
        if !self.lock_held() {
            return InterruptAction::ExitNow;
        }

        self.set();

        InterruptAction::Deferred
    }
}

/// Clears the lock window when the holding scope ends, however it ends.
pub struct LockScope<'a>(&'a CancelFlag);

impl Drop for LockScope<'_> {
    fn drop(&mut self) {
        self.0.lock_held.store(false, Ordering::SeqCst);
    }
}

/// An outcome plus what the wiring needs to route: the failure class and whether a backend
/// switch is still safe (only when the very first request failed before it was sent).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployResult {
    pub outcome: Outcome,
    pub failure: Option<Failure>,
    pub fallback_allowed: bool,
    /// The message of the failing request, quoted as the reason of a fallback.
    pub detail: Option<String>,
}

pub struct DeployProg;

impl DeployProg {
    pub fn run(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        connection: Option<&Connection>,
    ) -> Outcome {
        Self::run_classified(transport, input, connection, &CancelFlag::new()).outcome
    }

    pub fn run_with_cancel(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        cancel: &CancelFlag,
    ) -> Outcome {
        Self::run_classified(transport, input, None, cancel).outcome
    }

    pub fn run_classified(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
    ) -> DeployResult {
        Self::run_with(transport, input, None, connection, cancel)
    }

    /// Like `run_classified`, but prints the routing decision first; the CLI always has one.
    pub fn run_selected(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        selection: &Selection,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
    ) -> DeployResult {
        Self::run_with(transport, input, Some(selection), connection, cancel)
    }

    fn run_with(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        selection: Option<&Selection>,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
    ) -> DeployResult {
        let mut run = Run {
            transport,
            name: input.name.clone(),
            uri: prog_uri(&input.name),
            stdout: selection.map(selection_report).unwrap_or_default(),
            sent: 0,
        };
        let mut finish = match run.flow(&input, cancel) {
            Ok(finish) | Err(finish) => finish,
        };

        if let Err(error) = run.transport.close()
            && !finish.lock_unreleased
        {
            finish.close_failed(&error.to_string());
        }

        if let Some(warning) = connection.and_then(tls_warning) {
            run.stdout.insert(0, warning);
        }

        run.stdout.extend(finish.stdout);

        DeployResult {
            outcome: Outcome {
                status: finish.status,
                exit: finish.exit,
                stdout: run.stdout.join("\n"),
                stderr: finish.stderr.unwrap_or_default(),
            },
            failure: finish.failure,
            fallback_allowed: finish.fallback_allowed,
            detail: finish.detail,
        }
    }
}

/// How the run ends; `stdout` lines come after the lines printed so far.
struct Finish {
    status: Option<Status>,
    exit: i32,
    stdout: Vec<String>,
    stderr: Option<String>,
    failure: Option<Failure>,
    fallback_allowed: bool,
    detail: Option<String>,
    /// Set once the unlock failed: the run already reported a lock that may remain.
    lock_unreleased: bool,
}

impl Finish {
    fn new(status: Option<Status>, exit: i32) -> Self {
        Self {
            status,
            exit,
            stdout: Vec::new(),
            stderr: None,
            failure: None,
            fallback_allowed: false,
            detail: None,
            lock_unreleased: false,
        }
    }

    fn plain(line: &str) -> Self {
        Self {
            stdout: vec![line.to_owned()],
            ..Self::new(None, 0)
        }
    }

    fn stop(message: impl Display) -> Self {
        Self {
            stderr: Some(format!("STOP: {message}")),
            ..Self::new(None, 2)
        }
    }

    fn status(status: Status, line: impl Into<String>) -> Self {
        let line = line.into();
        // A reconciliation line ends with the retry instruction, which must stay last.
        let rollback = status == Status::Inactive && !line.ends_with(RETRY);
        let mut stdout = vec![line];

        if rollback {
            stdout.push(ROLLBACK.to_owned());
        }

        Self {
            stdout,
            ..Self::new(Some(status), status.exit())
        }
    }

    fn error(error: &TransportError, first_request: bool) -> Self {
        let failure = Failure::from(error);

        Self {
            stderr: Some(format!("ERROR: {error}")),
            failure: Some(failure),
            fallback_allowed: first_request && failure == Failure::PreSendConnectivity,
            detail: Some(error.to_string()),
            ..Self::new(None, 1)
        }
    }

    fn stop_http(message: impl Display, code: u16) -> Self {
        let error = TransportError::Http {
            status: code,
            message: String::new(),
        };

        Self {
            failure: Some(Failure::from(&error)),
            ..Self::stop(format!("{message}: HTTP {code}"))
        }
    }

    fn cancelled() -> Self {
        Self {
            stderr: Some("CANCELED: interrupted while the lock was held".to_owned()),
            ..Self::new(None, 1)
        }
    }

    fn http(status: Status, what: &str, code: u16) -> Self {
        let error = TransportError::Http {
            status: code,
            message: String::new(),
        };

        Self {
            failure: Some(Failure::from(&error)),
            ..Self::status(status, format!("{}: {what}: HTTP {code}", token(status)))
        }
    }

    fn exit_one(mut self) -> Self {
        self.exit = 1;
        self
    }

    fn is_uncertain(&self) -> bool {
        self.failure == Some(Failure::PostSendUncertain)
    }

    /// A read that failed after a sent write: nothing proves what happened.
    fn unreadable(self) -> Self {
        Self::status(
            Status::Partial,
            format!(
                "PARTIAL: outcome unknown, the active source cannot be read ({}); {RETRY}",
                self.stderr.unwrap_or_default().trim()
            ),
        )
        .exit_one()
    }

    fn unlock_failed(mut self, reason: &str, name: &str) -> Self {
        let note = format!("UNLOCK FAILED ({reason}); the lock on {name} may remain");

        self.warn(note);
        self.exit = 1;
        self.lock_unreleased = true;
        self
    }

    /// The 2xx lock reply could not be read, so the handle is unknown and the lock cannot be
    /// released: every later claim that it was would be false.
    fn lock_unknown(mut self, name: &str) -> Self {
        let note = format!("W: the lock on {name} may remain (the lock handle is unknown)");

        self.warn(note);
        self.lock_unreleased = true;
        self
    }

    /// The work is done and the lock is gone, so a failed session end is a warning only.
    fn close_failed(&mut self, reason: &str) {
        let note = format!("W: session close failed: {reason} (lock was released)");

        self.warn(note);
    }

    fn warn(&mut self, note: String) {
        self.stderr = Some(match self.stderr.take() {
            Some(existing) => format!("{existing}\n{note}"),
            None => note,
        });
    }
}

fn token(status: Status) -> &'static str {
    match status {
        Status::Active => "ACTIVE",
        Status::SimulatedOk => "SIMULATED_OK",
        Status::Inactive => "INACTIVE",
        Status::Rejected => "REJECTED",
        Status::Partial => "PARTIAL",
    }
}

enum PutResult {
    Accepted,
    Uncertain,
}

struct LockInfo {
    handle: String,
    corrnr: String,
    is_local: bool,
}

/// What the locked phase decided; the lock is released afterwards in every case.
enum Locked {
    Finished(Finish),
    /// Simulate: the check found no error and nothing was written.
    Checked,
    /// Deploy: the check passed and the PUT was accepted; the lock is still held.
    Written,
    /// Deploy: the PUT may or may not have reached SAP; the lock is still held.
    PutUncertain,
}

struct Run<'a> {
    transport: &'a mut dyn SadtTransport,
    name: String,
    uri: String,
    stdout: Vec<String>,
    sent: usize,
}

impl Run<'_> {
    fn call(&mut self, request: Request) -> Result<Response, TransportError> {
        self.sent += 1;
        self.transport.request(request)
    }

    fn send(&mut self, request: Request) -> Result<Response, Finish> {
        let first = self.sent == 0;

        self.call(request)
            .map_err(|error| Finish::error(&error, first))
    }

    fn flow(&mut self, input: &DeployInput, cancel: &CancelFlag) -> Result<Finish, Finish> {
        validate_prog(&input.name, &input.source).map_err(Finish::stop)?;

        let file = normalize(input.source.clone());
        let live = normalize(self.read_active("read live source")?);
        let live_hash = source_hash(&live);

        if let Some(base) = &input.base
            && *base != live_hash
        {
            return Err(Finish::stop(format!(
                "live changed: live hash {live_hash} != --base {base}"
            )));
        }

        if source_hash(&file) == live_hash {
            return Ok(Finish::plain("no changes"));
        }

        let diff = TextDiff::from_lines(join(&live), join(&file))
            .unified_diff()
            .context_radius(3)
            .header("live", "file")
            .to_string();

        self.stdout.push(diff.trim_end().to_owned());
        self.stdout.push(format!("live hash {live_hash}"));

        let _lock_scope = cancel.hold_lock();
        // Ctrl-C is deferred while the LOCK request is in flight too: its 2xx reply may already
        // have created the lock, so every path from that request onward must clean it up.
        let lock = self.lock()?;

        match self.locked(&lock, input, &file, &live_hash, cancel) {
            Locked::Finished(finish) => Ok(self.release(&lock, &input.name, finish)),
            Locked::Checked => Ok(match self.unlock(&lock) {
                Ok(()) => Finish::status(Status::SimulatedOk, "SIMULATED_OK"),
                Err(reason) => Finish::status(
                    Status::Partial,
                    "PARTIAL: the check passed but the lock was not released",
                )
                .unlock_failed(&reason, &input.name),
            }),
            Locked::PutUncertain => {
                let unlocked = self.unlock(&lock);
                let finish = self.reconcile(&live_hash, &file);

                Ok(match unlocked {
                    Ok(()) => finish,
                    Err(reason) => finish.unlock_failed(&reason, &input.name),
                })
            }
            Locked::Written => {
                if let Err(reason) = self.unlock(&lock) {
                    return Ok(Finish::status(
                        Status::Partial,
                        "PARTIAL: the new source is saved inactive, the lock was not \
                         released and nothing was activated",
                    )
                    .unlock_failed(&reason, &input.name));
                }

                if cancel.is_set() {
                    return Ok(Finish {
                        stderr: Some("CANCELED: interrupted before the activation".to_owned()),
                        ..Finish::status(
                            Status::Partial,
                            "PARTIAL: the new source is saved inactive and nothing was activated",
                        )
                    });
                }

                Ok(self.activate(&input.name, &live_hash, &file))
            }
        }
    }

    fn read_active(&mut self, what: &str) -> Result<Vec<String>, Finish> {
        let mut request = Request::get(format!("{}/source/main?version=active", self.uri));
        request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());
        let response = self.send(request)?;

        if response.status != 200 {
            return Err(Finish::stop_http(format!("cannot {what}"), response.status));
        }

        let text = String::from_utf8(response.body)
            .map_err(|error| Finish::stop(format!("cannot {what}: not UTF-8: {error}")))?;

        Ok(text.split('\n').map(str::to_owned).collect())
    }

    fn lock(&mut self) -> Result<LockInfo, Finish> {
        let mut request = Request::post(
            format!("{}?_action=LOCK&accessMode=MODIFY", self.uri),
            Vec::new(),
        );
        request
            .headers
            .insert("Accept".to_owned(), LOCK_ACCEPT.to_owned());
        let response = self.send(request)?;

        if !(200..300).contains(&response.status) {
            return Err(Finish::http(
                Status::Rejected,
                "lock failed",
                response.status,
            ));
        }

        parse_lock(&String::from_utf8_lossy(&response.body)).map_err(|message| {
            Finish::stop(format!("unreadable lock response: {message}")).lock_unknown(&self.name)
        })
    }

    fn locked(
        &mut self,
        lock: &LockInfo,
        input: &DeployInput,
        file: &[String],
        live_hash: &str,
        cancel: &CancelFlag,
    ) -> Locked {
        if cancel.is_set() {
            return Locked::Finished(Finish::cancelled());
        }

        let transport_request = match self.transport_request(lock, input.tr.as_deref()) {
            Ok(transport_request) => transport_request,
            Err(finish) => return Locked::Finished(finish),
        };

        let reread = match self.read_active("re-read live source after lock") {
            Ok(lines) => source_hash(&lines),
            Err(finish) => return Locked::Finished(finish),
        };

        if reread != live_hash {
            return Locked::Finished(Finish::stop(format!(
                "live changed since initial read: live hash {reread} != {live_hash}"
            )));
        }

        if let Err(finish) = self.check(file) {
            return Locked::Finished(finish);
        }

        if cancel.is_set() {
            return Locked::Finished(Finish::cancelled());
        }

        if input.simulate {
            return Locked::Checked;
        }

        match self.put(lock, file, transport_request.as_deref()) {
            Ok(PutResult::Accepted) => Locked::Written,
            Ok(PutResult::Uncertain) => Locked::PutUncertain,
            Err(finish) => Locked::Finished(finish),
        }
    }

    /// `IS_LOCAL = X` is `$TMP`: no request. Otherwise the lock's `CORRNR` is the request and
    /// `--tr` must equal it; this runs right after the lock, before anything is checked or sent.
    fn transport_request(
        &mut self,
        lock: &LockInfo,
        requested: Option<&str>,
    ) -> Result<Option<String>, Finish> {
        let corrnr = lock.corrnr.as_str();

        match (lock.is_local, requested) {
            (true, Some(_)) => Err(Finish::stop(
                "--tr is not allowed for a local ($TMP) object",
            )),
            (true, None) => Ok(None),
            (false, _) if corrnr.is_empty() => Err(Finish::stop(
                "the object is not local but the lock returned no transport request",
            )),
            (false, Some(tr)) if !tr.eq_ignore_ascii_case(corrnr) => Err(Finish::stop(format!(
                "--tr {tr} does not match the transport request {corrnr} of the lock"
            ))),
            (false, _) => {
                self.stdout.push(format!("transport request {corrnr}"));

                Ok(Some(corrnr.to_owned()))
            }
        }
    }

    fn put(
        &mut self,
        lock: &LockInfo,
        file: &[String],
        transport_request: Option<&str>,
    ) -> Result<PutResult, Finish> {
        let mut uri = format!(
            "{}/source/main?lockHandle={}",
            self.uri,
            encode_lock_handle(&lock.handle)
        );

        if let Some(corrnr) = transport_request {
            uri.push_str(&format!("&corrNr={corrnr}"));
        }

        let mut request = Request::put(uri, join(file).into_bytes());
        request.headers.insert(
            "Content-Type".to_owned(),
            "text/plain; charset=utf-8".to_owned(),
        );

        match self.call(request) {
            Ok(response) if (200..300).contains(&response.status) => Ok(PutResult::Accepted),
            Ok(response) => Err(Finish::http(
                Status::Rejected,
                "PUT failed",
                response.status,
            )),
            Err(error) if error.was_sent() => Ok(PutResult::Uncertain),
            Err(error) => Err(Finish::error(&error, false)),
        }
    }

    /// Runs after the unlock; the verdict is the active content, not the activation status.
    fn activate(&mut self, name: &str, live_hash: &str, file: &[String]) -> Finish {
        let body = activation_request_xml(name, &self.uri);
        let mut request = Request::post(
            "/sap/bc/adt/activation?method=activate&preauditRequested=true",
            body.into_bytes(),
        );
        request
            .headers
            .insert("Content-Type".to_owned(), "application/xml".to_owned());
        request
            .headers
            .insert("Accept".to_owned(), "application/xml".to_owned());
        let response = match self.call(request) {
            Ok(response) => response,
            Err(error) if error.was_sent() => {
                return self.reconcile(live_hash, file);
            }
            Err(error) => {
                return Finish {
                    failure: Some(Failure::from(&error)),
                    ..Finish::status(
                        Status::Inactive,
                        format!("INACTIVE: the activation was not sent: {error}"),
                    )
                };
            }
        };
        let text = String::from_utf8_lossy(&response.body).into_owned();

        self.stdout.extend(format_messages(&text));

        if !(200..300).contains(&response.status) {
            let mut finish = self.reconcile_non_2xx_activation(response.status, file);
            finish.failure = Some(Failure::from(&TransportError::Http {
                status: response.status,
                message: String::new(),
            }));

            return finish;
        }

        if let Some(status) = activation_status(response.status, &response.body) {
            return Finish::status(status, "INACTIVE: activation reported errors");
        }

        match self.read_active("read back the active source") {
            Ok(active) if source_hash(&active) == source_hash(file) => Finish {
                stdout: vec![
                    "ACTIVE".to_owned(),
                    format!("hash {}", source_hash(&active)),
                ],
                ..Finish::new(Some(Status::Active), 0)
            },
            Ok(_) => Finish::status(
                Status::Inactive,
                "INACTIVE: activation reported no error but the active source differs",
            ),
            Err(finish) if finish.is_uncertain() => self.reconcile(live_hash, file),
            Err(finish) => finish.unreadable(),
        }
    }

    fn reconcile_non_2xx_activation(&mut self, status: u16, file: &[String]) -> Finish {
        match self.read_active("read back the active source after an activation failure") {
            Ok(active) if source_hash(&active) == source_hash(file) => Finish::status(
                Status::Active,
                format!(
                    "ACTIVE (reconciled): the active source equals the file after activation HTTP \
                     {status}; {RETRY}"
                ),
            )
            .exit_one(),
            Ok(_) => Finish::status(
                Status::Inactive,
                format!(
                    "INACTIVE: activation failed with HTTP {status} and the active source differs"
                ),
            ),
            Err(finish) => finish.unreadable(),
        }
    }

    /// Read-only classification after a sent write whose outcome is unknown: the content of the
    /// active and the inactive source decides, never a status code. No retry, no fallback.
    fn reconcile(&mut self, live_hash: &str, file: &[String]) -> Finish {
        let active = match self.read_active("read the active source for reconciliation") {
            Ok(active) => active,
            Err(finish) => return finish.unreadable(),
        };
        let inactive = self.read_inactive();
        let (status, message) = reconcile(&active, &inactive, live_hash, file);

        Finish::status(status, message).exit_one()
    }

    /// The inactive source, or why it cannot be read: a failed read is never "no inactive".
    fn read_inactive(&mut self) -> InactiveRead {
        let mut request = Request::get(format!("{}/source/main?version=inactive", self.uri));
        request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());
        let response = match self.call(request) {
            Ok(response) => response,
            Err(error) => return InactiveRead::Unreadable(error.to_string()),
        };

        if response.status == 404 {
            return InactiveRead::Absent;
        }

        if response.status != 200 {
            return InactiveRead::Unreadable(format!("HTTP {}", response.status));
        }

        match String::from_utf8(response.body) {
            Ok(text) => InactiveRead::Read(text.split('\n').map(str::to_owned).collect()),
            Err(error) => InactiveRead::Unreadable(format!("not UTF-8: {error}")),
        }
    }

    /// Syntax check without saving: the source travels as the checkrun artifact content.
    fn check(&mut self, file: &[String]) -> Result<(), Finish> {
        let mut request = Request::post(
            "/sap/bc/adt/checkruns?reporters=abapCheckRun",
            checkrun_xml(&self.name, file).into_bytes(),
        );
        request
            .headers
            .insert("Content-Type".to_owned(), "application/*".to_owned());
        request
            .headers
            .insert("Accept".to_owned(), "application/*".to_owned());
        let response = self.send(request)?;

        if response.status != 200 {
            return Err(Finish::stop_http(
                "the check without save is unavailable",
                response.status,
            ));
        }

        let messages = parse_messages(&String::from_utf8_lossy(&response.body))
            .map_err(|message| Finish::stop(format!("cannot read the check result: {message}")))?;

        self.stdout
            .extend(messages.iter().map(|message| message.render()));

        if messages.iter().any(|message| message.is_error()) {
            return Err(Finish::status(
                Status::Rejected,
                "REJECTED: the check found errors",
            ));
        }

        Ok(())
    }

    fn release(&mut self, lock: &LockInfo, name: &str, finish: Finish) -> Finish {
        match self.unlock(lock) {
            Ok(()) => finish,
            Err(reason) => finish.unlock_failed(&reason, name),
        }
    }

    fn unlock(&mut self, lock: &LockInfo) -> Result<(), String> {
        let request = Request::post(
            format!(
                "{}?_action=UNLOCK&lockHandle={}",
                self.uri,
                encode_lock_handle(&lock.handle)
            ),
            Vec::new(),
        );

        match self.call(request) {
            Ok(response) if (200..300).contains(&response.status) => Ok(()),
            Ok(response) => Err(format!("HTTP {}", response.status)),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Request body of `POST /sap/bc/adt/checkruns`; `file` is the normalized source.
pub fn checkrun_xml(name: &str, file: &[String]) -> String {
    let uri = escape(prog_uri(name));
    let content = base64(join(file).as_bytes());

    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<chkrun:checkObjectList xmlns:chkrun="http://www.sap.com/adt/checkrun" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<chkrun:checkObject adtcore:uri="{uri}" chkrun:version="active">"#,
            r#"<chkrun:artifacts><chkrun:artifact "#,
            r#"chkrun:contentType="text/plain; charset=utf-8" chkrun:uri="{uri}/source/main">"#,
            r#"<chkrun:content>{content}</chkrun:content></chkrun:artifact>"#,
            r#"</chkrun:artifacts></chkrun:checkObject></chkrun:checkObjectList>"#,
        ),
        uri = uri,
        content = content
    )
}

/// Request body of `POST /sap/bc/adt/activation`; name and uri are XML-escaped values.
pub fn activation_request_xml(name: &str, uri: &str) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<adtcore:objectReference adtcore:uri="{uri}" adtcore:name="{name}"/>"#,
            r#"</adtcore:objectReferences>"#,
        ),
        uri = escape(uri),
        name = escape(name.to_ascii_uppercase())
    )
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();

    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |sum, (index, byte)| {
            sum | u32::from(*byte) << (16 - 8 * index)
        });

        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(char::from(
                    ALPHABET[(group >> (18 - 6 * index) & 63) as usize],
                ));
            } else {
                encoded.push('=');
            }
        }
    }

    encoded
}

const RETRY: &str = "check with read before retrying";

const GUARD_REPORT: [&str; 2] = [
    "guards: SAP lock/CSRF/transport/authorization, base hash",
    concat!(
        "not applied (FM guards): Z/Y name, productive client, system change option, ",
        "SE06, inactive owned by another user",
    ),
];

/// Lines printed before the diff of every simulate and deploy: the backend that runs, why it
/// was chosen, and which guards hold there.
pub fn selection_report(selection: &Selection) -> Vec<String> {
    let mut lines = vec![
        format!("backend: {}", selection.backend.token()),
        format!("reason: {}", selection.reason),
    ];

    lines.extend(GUARD_REPORT.iter().map(|line| (*line).to_owned()));

    lines
}

/// What the read-only `?version=inactive` answered during reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InactiveRead {
    /// The inactive source to compare against the file and the live hash.
    Read(Vec<String>),
    /// SAP answered that there is no inactive version.
    Absent,
    /// The read failed (HTTP status, timeout, bad UTF-8), so the inactive state is unknown.
    Unreadable(String),
}

/// Classifies an unknown write outcome by content: `active` and `inactive` are what was read
/// afterwards, `live_hash` the hash before the write and `file` the source being deployed.
/// Every result is reported with exit 1 by the caller; the message always says to read first.
pub fn reconcile(
    active: &[String],
    inactive: &InactiveRead,
    live_hash: &str,
    file: &[String],
) -> (Status, String) {
    let active_hash = source_hash(active);
    let file_hash = source_hash(file);

    if active_hash == file_hash {
        return match inactive {
            InactiveRead::Unreadable(reason) => (
                Status::Active,
                format!(
                    "ACTIVE (reconciled), inactive state unknown: the active source equals \
                     the file but the inactive source cannot be read ({reason}); {RETRY}"
                ),
            ),
            _ => (
                Status::Active,
                format!("ACTIVE (reconciled): the active source equals the file; {RETRY}"),
            ),
        };
    }

    if active_hash != live_hash {
        return (
            Status::Partial,
            format!("PARTIAL: unexpected state; {RETRY}"),
        );
    }

    match inactive {
        InactiveRead::Read(lines) if source_hash(lines) == file_hash => (
            Status::Inactive,
            format!("INACTIVE: the PUT reached SAP but the activation did not; {RETRY}"),
        ),
        InactiveRead::Absent => (
            Status::Partial,
            format!("PARTIAL: outcome unknown; {RETRY}"),
        ),
        InactiveRead::Read(lines)
            if source_hash(lines) == live_hash || source_hash(lines) == source_hash(&[]) =>
        {
            (
                Status::Partial,
                format!("PARTIAL: outcome unknown; {RETRY}"),
            )
        }
        InactiveRead::Read(_) => (
            Status::Partial,
            format!("PARTIAL: unexpected state; {RETRY}"),
        ),
        InactiveRead::Unreadable(reason) => (
            Status::Partial,
            format!(
                "PARTIAL: reconciliation incomplete, inactive source unreadable ({reason}); \
                 {RETRY}"
            ),
        ),
    }
}

/// Verdict from the activation answer alone: `Some(Rejected)` for a non-2xx status,
/// `Some(Inactive)` for an `E` message, `None` when only the active content can tell.
pub fn activation_status(status: u16, body: &[u8]) -> Option<Status> {
    if !(200..300).contains(&status) {
        return Some(Status::Rejected);
    }

    let messages = parse_messages(&String::from_utf8_lossy(body)).ok()?;

    messages
        .iter()
        .any(AdtMessage::is_error)
        .then_some(Status::Inactive)
}

fn join(lines: &[String]) -> String {
    let mut text = lines.join("\n");

    if !text.is_empty() {
        text.push('\n');
    }

    text
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn encode_lock_handle(handle: &str) -> String {
    let mut encoded = String::new();

    for byte in handle.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }

    encoded
}

fn parse_lock(xml: &str) -> Result<LockInfo, String> {
    let fields = leaf_texts(xml)?;
    let handle = fields
        .get("LOCK_HANDLE")
        .filter(|value| !value.is_empty())
        .ok_or("no LOCK_HANDLE")?
        .clone();

    Ok(LockInfo {
        handle,
        corrnr: fields.get("CORRNR").cloned().unwrap_or_default(),
        is_local: fields.get("IS_LOCAL").is_some_and(|value| value == "X"),
    })
}

/// Text of every element by local name; the last occurrence wins.
fn leaf_texts(xml: &str) -> Result<BTreeMap<String, String>, String> {
    let mut reader = Reader::from_str(xml);
    let mut texts = BTreeMap::new();
    let mut current: Option<String> = None;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) => {
                let name = element.local_name().into_inner().to_owned();

                texts.insert(name.clone(), String::new());
                current = Some(name);
            }
            Event::Text(text) => {
                if let Some(entry) = current.as_ref().and_then(|name| texts.get_mut(name)) {
                    entry.push_str(&text.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref().map_err(|e| e.to_string())? {
                    Some(character) => character.to_string(),
                    None => resolve_predefined_entity(&reference)
                        .ok_or_else(|| format!("unknown entity &{};", &*reference))?
                        .to_owned(),
                };

                if let Some(entry) = current.as_ref().and_then(|name| texts.get_mut(name)) {
                    entry.push_str(&resolved);
                }
            }
            Event::End(_) => current = None,
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(texts)
}
