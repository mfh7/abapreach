use crate::adt::{AdtMessage, parse_messages, parse_versions, prog_uri, versions_uri};
use crate::backend::{Failure, Selection, SelectionReason};
use crate::config::{Connection, tls_warning};
use crate::domain::{Outcome, Status};
use crate::hash::{normalize, source_hash, validate_prog};
use crate::policy::{Confirmer, Decision, InactiveVersion, Policies, Prompt, Terminal};
use crate::transport::{Request, Response, SadtTransport, TransportError};
use quick_xml::escape::{escape, resolve_predefined_entity};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::adt::format_messages;

const LOCK_ACCEPT: &str =
    "application/*,application/vnd.sap.as+xml;charset=UTF-8;dataname=com.sap.adt.lock.result";
const VERSIONS_ACCEPT: &str = "application/atom+xml;type=feed";
const ROLLBACK: &str = "ROLLBACK: the inactive version belongs to you; GET without a version returns it; restore only by deploying the base source again on your order";
const NO_STATEFUL_SESSION: &str = "ADT HTTP stateful session unavailable (no sap-contextid)";

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
/// switch is still safe (only before the deploy sent a write request).
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
        Self::run_with(transport, input, None, connection, cancel, &Terminal)
    }

    /// Like `run_classified`, but with a replacement for the terminal the `ask` policy prompts on.
    pub fn run_classified_with(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
        confirmer: &dyn Confirmer,
    ) -> DeployResult {
        Self::run_with(transport, input, None, connection, cancel, confirmer)
    }

    /// Like `run_classified`, but prints the routing decision first; the CLI always has one.
    pub fn run_selected(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        selection: &Selection,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
    ) -> DeployResult {
        Self::run_with(
            transport,
            input,
            Some(selection),
            connection,
            cancel,
            &Terminal,
        )
    }

    fn run_with(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        selection: Option<&Selection>,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
        confirmer: &dyn Confirmer,
    ) -> DeployResult {
        let policies = connection
            .map(|connection| connection.policies.clone())
            .unwrap_or_default();
        let mut run = Run {
            transport,
            name: input.name.clone(),
            uri: prog_uri(&input.name),
            stdout: selection
                .map(|selection| selection_report(selection, &policies))
                .unwrap_or_default(),
            warnings: Vec::new(),
            policies,
            // Without a connection there is no logon user, so no inactive version can match it.
            user: connection
                .map(|connection| connection.user.clone())
                .unwrap_or_default(),
            seen: InactiveSeen::default(),
            confirmer,
            write_sent: false,
            lock_sent: false,
        };
        // `--base` and an explicit `--backend` bind the run to the backend that was picked.
        let pinned = input.base.is_some()
            || selection
                .is_some_and(|selection| selection.reason == SelectionReason::ExplicitBackend);
        let mut finish = match run.flow(&input, pinned, cancel) {
            Ok(finish) | Err(finish) => finish,
        };

        if let Err(error) = run.transport.close() {
            finish.close_failed(&error.to_string(), &input.name, run.lock_sent);
        }

        run.prepend_warnings(&mut finish);

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
    /// True only after this run received a successful reply to its UNLOCK request.
    lock_released: bool,
    /// The UNLOCK failure already warned that the lock may remain.
    lock_unreleased_reported: bool,
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
            lock_released: false,
            lock_unreleased_reported: false,
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

    fn error(error: &TransportError, before_write: bool) -> Self {
        let failure = Failure::from(error);

        Self {
            stderr: Some(format!("ERROR: {error}")),
            failure: Some(failure),
            fallback_allowed: before_write && failure == Failure::PreSendConnectivity,
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

    /// No stateful session, so no LOCK was sent and nothing was written: another backend may
    /// still run the deploy.
    fn stateful_unavailable() -> Self {
        Self {
            stderr: Some(format!("ERROR: {NO_STATEFUL_SESSION}")),
            failure: Some(Failure::CapabilityUnavailable),
            fallback_allowed: true,
            detail: Some(NO_STATEFUL_SESSION.to_owned()),
            ..Self::new(None, 1)
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
        self.lock_unreleased_reported = true;
        self
    }

    /// The 2xx lock reply could not be read, so the handle is unknown and the lock cannot be
    /// released: every later claim that it was would be false.
    fn lock_unknown(mut self, name: &str) -> Self {
        let note = format!("W: the lock on {name} may remain (the lock handle is unknown)");

        self.warn(note);
        self
    }

    fn lock_released(mut self) -> Self {
        self.lock_released = true;
        self
    }

    fn lock_request_unanswered(mut self, name: &str) -> Self {
        self.warn(format!(
            "W: the lock request got no reply; a lock on {name} may remain, check SM12"
        ));
        self
    }

    /// A session-end failure is informative only. The preceding UNLOCK reply is the only proof
    /// that no lock remains.
    fn close_failed(&mut self, reason: &str, name: &str, lock_sent: bool) {
        if self.lock_unreleased_reported {
            return;
        }

        let note = if !lock_sent {
            format!("W: session close failed: {reason}")
        } else if self.lock_released {
            format!("W: session close failed: {reason} (lock was released)")
        } else {
            format!("W: session close failed: {reason}; a lock on {name} may remain, check SM12")
        };

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

/// What the version list said about the inactive version, kept from the check before the lock so
/// that the check after it acts only on a change.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum InactiveSeen {
    /// There is no inactive version.
    #[default]
    Absent,
    /// The inactive version belongs to this user.
    Owned(String),
    /// The version list could not be read.
    Unknown,
}

impl InactiveSeen {
    fn of(read: &Result<Option<String>, String>) -> Self {
        match read {
            Ok(Some(owner)) => Self::Owned(owner.clone()),
            Ok(None) => Self::Absent,
            Err(_) => Self::Unknown,
        }
    }

    /// The same inactive version, with owners compared as SAP compares users.
    fn unchanged(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Owned(owner), Self::Owned(other)) => owner.eq_ignore_ascii_case(other),
            (Self::Absent, Self::Absent) | (Self::Unknown, Self::Unknown) => true,
            _ => false,
        }
    }
}

struct Run<'a> {
    transport: &'a mut dyn SadtTransport,
    name: String,
    uri: String,
    stdout: Vec<String>,
    /// `W:` lines of the guard policies, kept until the verdict is known.
    warnings: Vec<String>,
    policies: Policies,
    /// The logon user of the connection, compared with the owner of the inactive version.
    user: String,
    /// The inactive version as the check before the lock saw it.
    seen: InactiveSeen,
    confirmer: &'a dyn Confirmer,
    /// A completed mutating request closes the routing window, even when later reads fail.
    write_sent: bool,
    /// Whether a lock may exist: a LOCK reply that was a 2xx, or a LOCK request whose reply was
    /// lost. A refused LOCK leaves no lock, so a later session-end failure stays quiet about it.
    lock_sent: bool,
}

enum InactiveOwnerError {
    Request(TransportError),
    Unreadable(String),
}

impl Run<'_> {
    /// The guard warnings come first, so they stay above the notes of a later failure.
    fn prepend_warnings(&self, finish: &mut Finish) {
        if self.warnings.is_empty() {
            return;
        }

        let mut lines = self.warnings.clone();

        if let Some(existing) = finish.stderr.take().filter(|text| !text.is_empty()) {
            lines.push(existing);
        }

        finish.stderr = Some(lines.join("\n"));
    }

    fn call(&mut self, request: Request) -> Result<Response, TransportError> {
        let mutates = request.method != "GET" && !request.uri.contains("/checkruns");
        let response = self.transport.request(request);

        if mutates && (response.is_ok() || response.as_ref().is_err_and(TransportError::was_sent)) {
            self.write_sent = true;
        }

        let response = response?;

        // The logon was refused, whatever this request was: the run ends here, without a retry
        // and without the other backend (a wrong password must not lock the SAP user).
        match response.authorization_failure() {
            Some(error) => Err(error),
            None => Ok(response),
        }
    }

    fn send(&mut self, request: Request) -> Result<Response, Finish> {
        let before_write = !self.write_sent;

        self.call(request)
            .map_err(|error| Finish::error(&error, before_write))
    }

    fn flow(
        &mut self,
        input: &DeployInput,
        pinned: bool,
        cancel: &CancelFlag,
    ) -> Result<Finish, Finish> {
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

        let lines = TextDiff::from_lines(join(&live), join(&file));
        // The prompt says how much is at stake, because stdout holds the diff until the run ends.
        let changed = lines
            .iter_all_changes()
            .filter(|change| change.tag() != ChangeTag::Equal)
            .count();
        let diff = lines
            .unified_diff()
            .context_radius(3)
            .header("live", "file")
            .to_string();

        self.stdout.push(diff.trim_end().to_owned());
        self.stdout.push(format!("live hash {live_hash}"));

        // The server would answer the LOCK and then reject the PUT (423, invalid lock handle),
        // so the capability is checked before the first request that writes.
        if self.transport.stateful_supported() == Some(false) {
            return Err(if pinned {
                Finish::stop(
                    "ADT HTTP on this system keeps no stateful session (no sap-contextid); \
                     use --backend rfc",
                )
            } else {
                Finish::stateful_unavailable()
            });
        }

        // Still read-only: a refused guard leaves no lock behind and writes nothing.
        self.foreign_inactive(Some(changed))?;

        let _lock_scope = cancel.hold_lock();
        // Ctrl-C is deferred while the LOCK request is in flight too: its 2xx reply may already
        // have created the lock, so every path from that request onward must clean it up.
        let lock = self.lock()?;

        match self.locked(&lock, input, &file, &live_hash, cancel) {
            Locked::Finished(finish) => Ok(self.release(&lock, &input.name, finish)),
            Locked::Checked => Ok(match self.unlock(&lock) {
                Ok(()) => Finish::status(Status::SimulatedOk, "SIMULATED_OK").lock_released(),
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
                    Ok(()) => finish.lock_released(),
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

                Ok(self
                    .activate(&input.name, &live_hash, &file)
                    .lock_released())
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

    /// The `foreign_inactive` guard: the version list says whether the inactive version belongs
    /// to another user, whose work the next deploy would overwrite. Read-only; it runs before the
    /// LOCK and again right after it. `changed` is the line count of the diff and enables the
    /// prompt: the second check passes `None`, because the user would answer while the lock is
    /// held. Only a version that appeared in between is new to that second check.
    fn foreign_inactive(&mut self, changed: Option<usize>) -> Result<(), Finish> {
        let read = match self.inactive_owner() {
            Ok(owner) => Ok(owner),
            Err(InactiveOwnerError::Request(error)) => {
                return Err(Finish::error(&error, !self.write_sent));
            }
            Err(InactiveOwnerError::Unreadable(reason)) => Err(reason),
        };
        let seen = InactiveSeen::of(&read);

        if changed.is_none() && self.seen.unchanged(&seen) {
            return Ok(());
        }

        let state = match &read {
            Ok(Some(owner)) => InactiveVersion::Owned { owner },
            Ok(None) => InactiveVersion::Absent,
            Err(reason) => InactiveVersion::Unknown { reason },
        };
        let prompt = changed.map(|changed| Prompt {
            name: &self.name,
            changed,
        });

        match self
            .policies
            .inactive_version(state, &self.user, prompt, self.confirmer)
        {
            Decision::Continue(None) => {
                self.seen = seen;

                Ok(())
            }
            Decision::Continue(Some(warning)) => {
                self.seen = seen;
                self.warnings.push(warning);

                Ok(())
            }
            Decision::Refuse(reason) => Err(Finish::status(
                Status::Rejected,
                format!("REJECTED: {reason}"),
            )),
        }
    }

    /// The owner of the inactive version, or why the version list could not tell.
    fn inactive_owner(&mut self) -> Result<Option<String>, InactiveOwnerError> {
        let mut request = Request::get(versions_uri(&self.uri));
        request
            .headers
            .insert("Accept".to_owned(), VERSIONS_ACCEPT.to_owned());
        let response = self
            .call(request)
            .map_err(|error| match Failure::from(&error) {
                Failure::PreSendConnectivity | Failure::Authorization => {
                    InactiveOwnerError::Request(error)
                }
                _ => InactiveOwnerError::Unreadable(format!("version list unavailable: {error}")),
            })?;

        if response.status != 200 {
            return Err(InactiveOwnerError::Unreadable(format!(
                "version list unavailable: HTTP {}",
                response.status
            )));
        }

        let entries =
            parse_versions(&String::from_utf8_lossy(&response.body)).map_err(|error| {
                InactiveOwnerError::Unreadable(format!("cannot parse the version list: {error}"))
            })?;

        match entries.iter().find(|entry| entry.is_inactive()) {
            Some(entry) if entry.author.is_empty() => Err(InactiveOwnerError::Unreadable(
                "the version list names no owner of the inactive version".to_owned(),
            )),
            Some(entry) => Ok(Some(entry.author.clone())),
            None => Ok(None),
        }
    }

    fn lock(&mut self) -> Result<LockInfo, Finish> {
        let mut request = Request::post(
            format!("{}?_action=LOCK&accessMode=MODIFY", self.uri),
            Vec::new(),
        );
        request
            .headers
            .insert("Accept".to_owned(), LOCK_ACCEPT.to_owned());
        let before_write = !self.write_sent;
        let response = self.call(request);
        // A refused LOCK creates no lock, so only a 2xx reply, or a reply that never arrived, can
        // leave one behind.
        self.lock_sent = match &response {
            Ok(response) => (200..300).contains(&response.status),
            Err(error) => error.was_sent(),
        };
        let response = response.map_err(|error| {
            let finish = Finish::error(&error, before_write);

            if error.was_sent() {
                finish.lock_request_unanswered(&self.name)
            } else {
                finish
            }
        })?;

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

        // Another user may have saved an inactive version between the version list and the LOCK,
        // so the guard runs again: a refusal unlocks and writes nothing.
        if let Err(finish) = self.foreign_inactive(None) {
            return Locked::Finished(finish);
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
            Ok(()) => finish.lock_released(),
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

/// FM guards that only the Python fallback implements; they never hold on this path.
const UNSUPPORTED_GUARDS: &str =
    "not applied (FM guards): Z/Y name, productive client, system change option, SE06";

/// Lines printed before the diff of every simulate and deploy: the backend that runs, why it
/// was chosen, and which guards hold there.
pub fn selection_report(selection: &Selection, policies: &Policies) -> Vec<String> {
    vec![
        format!("backend: {}", selection.backend.token()),
        format!("reason: {}", selection.reason),
        format!(
            "guards: SAP lock/CSRF/transport/authorization, base hash, foreign inactive (policy {})",
            policies.foreign_inactive
        ),
        UNSUPPORTED_GUARDS.to_owned(),
    ]
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
