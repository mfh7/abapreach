use crate::adt::{
    ActivationBody, AdtMessage, Caller, ObjectRef, USAGE_REQUEST_XML, classify_activation,
    parse_callers, parse_inactive_feed_refs, parse_messages, parse_object_refs, parse_versions,
    prog_uri, usage_references_uri,
};
use crate::backend::{Failure, Selection, SelectionReason};
use crate::cli::{IncludeKind, ObjectKind};
use crate::config::{Connection, tls_warning};
use crate::domain::{Outcome, Status};
use crate::hash::{normalize, source_hash};
use crate::object::{check_deploy_source, check_request};
use crate::operation::read::{ReadError, resolve_group};
use crate::policy::{Confirmer, Decision, InactiveVersion, Policies, Prompt, Terminal};
use crate::signature::{interface_diff, validate_signature};
use crate::transport::{Request, Response, SadtTransport, TransportError};
use quick_xml::escape::{escape, resolve_predefined_entity};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub use crate::adt::format_messages;

pub(crate) const LOCK_ACCEPT: &str =
    "application/*,application/vnd.sap.as+xml;charset=UTF-8;dataname=com.sap.adt.lock.result";
pub(crate) const VERSIONS_ACCEPT: &str = "application/atom+xml;type=feed";
const ROLLBACK: &str = "ROLLBACK: the inactive version belongs to you; GET without a version returns it; restore only by deploying the base source again on your order";
const NO_STATEFUL_SESSION: &str = "ADT HTTP stateful session unavailable (no sap-contextid)";
const INACTIVE_OBJECTS_ACCEPT: &str =
    "application/vnd.sap.adt.inactivectsobjects.v1+xml, application/xml;q=0.8";
const WHERE_USED_LIMIT: usize = 50;
const RUNTIME_WARNING: &str = "W: callers' syntax check does not catch missing CALL FUNCTION parameters; they dump at runtime";
const OBJECT_STRUCTURE: &str = "objectstructure?version=active&withShortDescriptions=true";

#[derive(Clone, Debug)]
pub struct DeployInput {
    pub name: String,
    pub source: Vec<String>,
    pub base: Option<String>,
    pub simulate: bool,
    pub tr: Option<String>,
}

/// What a deploy targets besides its name: the kind, the `--fugr` of a FUNC, the class include
/// and the opt-in creation of a missing include. The default is the PROG of milestone 1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployScope {
    pub kind: ObjectKind,
    pub group: Option<String>,
    pub include: Option<IncludeKind>,
    pub create_include: bool,
    /// Stop before the LOCK when the cancel flag is already set. The CLI exits on its own
    /// before a lock, so only a long-lived process (the MCP server) needs this.
    pub cancel_before_lock: bool,
}

impl DeployScope {
    pub fn new(kind: ObjectKind) -> Self {
        Self {
            kind,
            group: None,
            include: None,
            create_include: false,
            cancel_before_lock: false,
        }
    }

    pub fn cancel_before_lock(mut self, stop: bool) -> Self {
        self.cancel_before_lock = stop;

        self
    }

    pub fn group(mut self, group: Option<&str>) -> Self {
        self.group = group.map(str::to_owned);

        self
    }

    pub fn include(mut self, include: Option<IncludeKind>) -> Self {
        self.include = include;

        self
    }

    pub fn create_include(mut self, create_include: bool) -> Self {
        self.create_include = create_include;

        self
    }
}

impl Default for DeployScope {
    fn default() -> Self {
        Self::new(ObjectKind::Prog)
    }
}

/// Exit code of an interrupt (128 + SIGINT): at once when no lock or create POST is in flight,
/// otherwise after the safe operation outcome is known.
pub const INTERRUPT_EXIT_CODE: i32 = 130;

/// What a SIGINT means right now: leave at once, or let a critical operation finish safely.
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
    write_in_flight: Arc<AtomicBool>,
    /// The process was told to end (signal or closed input), not just one request cancelled.
    shutdown: Arc<AtomicBool>,
}

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears a cancellation that belonged to a request that is over. A shutdown stays.
    pub fn reset(&self) {
        if !self.is_shutdown() {
            self.cancelled.store(false, Ordering::SeqCst);
        }
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.set();
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
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

    /// Marks the one-shot create POST and its reconciliation. A signal in this window must wait
    /// until the outcome is known: a server may have created the object even when its reply races
    /// with Ctrl-C.
    pub fn hold_write(&self) -> WriteScope<'_> {
        self.write_in_flight.store(true, Ordering::SeqCst);

        WriteScope(self)
    }

    /// A SIGINT must not end the process while a lock is held or a create write is unresolved.
    /// The flag then makes the operation settle safely before it reports.
    pub fn on_interrupt(&self) -> InterruptAction {
        if !self.lock_held() && !self.write_in_flight.load(Ordering::SeqCst) {
            return InterruptAction::ExitNow;
        }

        self.request_shutdown();

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

/// Clears the create POST window after its reply or read-only reconciliation has settled.
pub struct WriteScope<'a>(&'a CancelFlag);

impl Drop for WriteScope<'_> {
    fn drop(&mut self) {
        self.0.write_in_flight.store(false, Ordering::SeqCst);
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
    /// The function group the run resolved for a FUNC.
    pub group: Option<String>,
    /// The live hash the run read first: a later write that repeats it as its base is refused
    /// when the live source moved on.
    pub base: Option<String>,
    /// What the simulated change adds and removes, for a human to confirm.
    pub change: Option<Change>,
}

/// The size of a change and a bounded excerpt of its diff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    pub added: usize,
    pub removed: usize,
    /// Lines of the live source: `removed` of them leave when the new source is shorter.
    pub live_lines: usize,
    /// The unified diff lines (header and context included), cut at `EXCERPT_LINES`.
    pub excerpt: Vec<String>,
    /// How many diff lines the excerpt left out.
    pub omitted: usize,
}

/// The diff lines a confirmation shows at most.
pub const EXCERPT_LINES: usize = 40;

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
        Self::run_with(
            transport,
            input,
            DeployScope::default(),
            None,
            connection,
            cancel,
            &Terminal,
        )
    }

    /// Like `run_classified`, but with a replacement for the terminal the `ask` policy prompts on.
    pub fn run_classified_with(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
        confirmer: &dyn Confirmer,
    ) -> DeployResult {
        Self::run_with(
            transport,
            input,
            DeployScope::default(),
            None,
            connection,
            cancel,
            confirmer,
        )
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
            DeployScope::default(),
            Some(selection),
            connection,
            cancel,
            &Terminal,
        )
    }

    /// The deploy of any M3 kind: `scope` names the kind, the group, the include and the opt-in
    /// include creation. The CLI always has a routing decision to print.
    pub fn run_scoped(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        scope: DeployScope,
        selection: Option<&Selection>,
        connection: Option<&Connection>,
        cancel: &CancelFlag,
    ) -> DeployResult {
        Self::run_with(
            transport, input, scope, selection, connection, cancel, &Terminal,
        )
    }

    fn run_with(
        transport: &mut dyn SadtTransport,
        input: DeployInput,
        scope: DeployScope,
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
            kind: scope.kind,
            include: scope.include,
            create_include: scope.create_include,
            supplied_group: scope.group,
            group: None,
            uri: String::new(),
            source_uri: String::new(),
            feeds: Vec::new(),
            include_absent: false,
            rendered: None,
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
            live_hash: None,
            change: None,
            cancel_before_lock: scope.cancel_before_lock,
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
            group: run.group.clone(),
            base: run.live_hash.clone(),
            change: run.change.take(),
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
            ..Self::new(None, INTERRUPT_EXIT_CODE)
        }
    }

    fn cancelled_before_lock() -> Self {
        Self {
            stderr: Some("CANCELED: interrupted before the lock was taken".to_owned()),
            ..Self::new(None, INTERRUPT_EXIT_CODE)
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

enum IncludePost {
    Created,
    Uncertain,
}

pub(crate) struct LockInfo {
    pub(crate) handle: String,
    pub(crate) corrnr: String,
    /// The owner of the request in `corrnr`; empty when the lock reply names none.
    pub(crate) corruser: String,
    pub(crate) is_local: bool,
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
    /// Deploy: the include POST may or may not have reached SAP; the lock is still held and the
    /// POST is never repeated.
    IncludeUncertain,
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
    kind: ObjectKind,
    include: Option<IncludeKind>,
    create_include: bool,
    /// The `--fugr` of the command; the resolved group replaces it in `group`.
    supplied_group: Option<String>,
    group: Option<String>,
    /// The object the lock and the activation use: for a class include the class.
    uri: String,
    /// The source that is read, checked and written.
    source_uri: String,
    /// The version feeds the foreign-inactive guard reads.
    feeds: Vec<String>,
    /// The class include does not exist yet and `--create-include` will create it: a read of it
    /// answers 404, which then means empty.
    include_absent: bool,
    /// The source as SAP rendered it in the PUT reply; only a FUNC is re-rendered by the server.
    rendered: Option<Vec<String>>,
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
    /// The hash of the live source the run read first.
    live_hash: Option<String>,
    change: Option<Change>,
    cancel_before_lock: bool,
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
        // A checkrun without save and a where-used query are POSTs that write nothing.
        let mutates = request.method != "GET"
            && !request.uri.contains("/checkruns")
            && !request.uri.contains("/usageReferences?");
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
        check_deploy_source(self.kind, &input.name, &input.source).map_err(Finish::stop)?;
        if self.kind == ObjectKind::Func {
            validate_signature(&input.source).map_err(|error| {
                Finish::stop(format!("FUNC source signature unreadable: {error}"))
            })?;
        }
        self.resolve_target()?;

        let file = normalize(input.source.clone());
        let live = normalize(self.read_live()?);
        let live_hash = source_hash(&live);

        self.live_hash = Some(live_hash.clone());

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

        self.change = Some(change_of(&lines, &diff, live.len()));
        self.stdout.push(diff.trim_end().to_owned());
        self.report_interface_change(&live, &file)?;
        self.stdout.push(format!("live hash {live_hash}"));

        if let Some(include) = self
            .include
            .filter(|_| self.include_absent && input.simulate)
        {
            self.stdout
                .push(format!("WILL CREATE INCLUDE {}", include.token()));
        }

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
        self.warn_inactive_siblings();

        if self.cancel_before_lock && cancel.is_set() {
            return Err(Finish::cancelled_before_lock());
        }

        let _lock_scope = cancel.hold_lock();
        // Ctrl-C is deferred while the LOCK request is in flight too: its 2xx reply may already
        // have created the lock, so every path from that request onward must clean it up.
        let lock = self.lock()?;

        // Test-only: keeps the lock held so a harness can interrupt inside the window.
        #[cfg(feature = "fault-injection")]
        crate::fault::hold_lock(cancel);

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
            Locked::IncludeUncertain => {
                let unlocked = self.unlock(&lock);
                let finish = self.reconcile_include(&file);

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
                        exit: INTERRUPT_EXIT_CODE,
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

    /// A FUNC whose readable signature differs from the live one: the parameter diff, callers,
    /// and runtime-risk warning are advisory. An unreadable live signature stops before a write.
    fn report_interface_change(&mut self, live: &[String], file: &[String]) -> Result<(), Finish> {
        if self.kind != ObjectKind::Func {
            return Ok(());
        }

        let Some(diff) = interface_diff(live, file)
            .map_err(|error| Finish::stop(format!("live FUNC signature unreadable: {error}")))?
        else {
            return Ok(());
        };

        self.stdout.push("INTERFACE CHANGED".to_owned());
        self.stdout.extend(diff);

        match self.where_used() {
            Ok(callers) if callers.is_empty() => self.stdout.push("WHERE-USED: none".to_owned()),
            Ok(callers) => {
                for caller in callers.iter().take(WHERE_USED_LIMIT) {
                    let kind = caller.kind.split('/').next().unwrap_or_default();

                    self.stdout
                        .push(format!("WHERE-USED {kind} {}", caller.name));
                }

                if callers.len() > WHERE_USED_LIMIT {
                    self.stdout.push(format!(
                        "WHERE-USED ... {} more",
                        callers.len() - WHERE_USED_LIMIT
                    ));
                }
            }
            Err(reason) => self
                .warnings
                .push(format!("W: where-used unavailable: {reason}")),
        }

        self.warnings.push(RUNTIME_WARNING.to_owned());

        Ok(())
    }

    fn where_used(&mut self) -> Result<Vec<Caller>, String> {
        let mut request = Request::post(
            usage_references_uri(&self.uri),
            USAGE_REQUEST_XML.as_bytes().to_vec(),
        );

        for header in ["Content-Type", "Accept"] {
            request
                .headers
                .insert(header.to_owned(), "application/*".to_owned());
        }

        let response = self.call(request).map_err(|error| error.to_string())?;

        if response.status != 200 {
            return Err(format!("HTTP {}", response.status));
        }

        let text = String::from_utf8(response.body).map_err(|error| error.to_string())?;

        parse_callers(&text).map_err(|error| format!("unreadable reply: {error}"))
    }

    /// Resolves the URIs of the target. A FUNC first asks the server for its function group; a
    /// supplied `--fugr` that names another group is a `STOP`, like on the read path.
    fn resolve_target(&mut self) -> Result<(), Finish> {
        if self.kind == ObjectKind::Func {
            let resolved = resolve_group(
                &mut *self.transport,
                &self.name,
                self.supplied_group.as_deref(),
            )
            .map_err(|error| read_failure(error, !self.write_sent))?;

            self.group = Some(resolved);
        }

        let group = self.group.as_deref();
        let uri = |result: Result<String, String>| result.map_err(Finish::stop);

        self.uri = uri(self.kind.object_uri(&self.name, group))?;
        self.source_uri = uri(self.kind.source_uri(&self.name, group, self.include))?;
        self.feeds = vec![uri(self.kind.versions_uri(
            &self.name,
            group,
            self.include,
        ))?];

        // Activating a class activates all of it, so the inactive state of its main source
        // counts next to the include that is deployed.
        if self.kind == ObjectKind::Clas && self.include.is_some() {
            self.feeds
                .push(uri(self.kind.versions_uri(&self.name, group, None))?);
        }

        Ok(())
    }

    /// The first read of the run. A class include that does not exist answers 404 before the
    /// lock; if the class itself exists the include is missing, which stops the run unless
    /// `--create-include` was given.
    fn read_live(&mut self) -> Result<Vec<String>, Finish> {
        let response = self.get_active()?;

        if response.status == 404
            && let Some(include) = self.include
        {
            self.check_include_missing(include)?;
        }

        self.source_lines(response, "read live source")
    }

    /// Whether the class exists, probed with its structure, which answers 200 in every state of
    /// an existing class. A class include that is not there is `INCLUDE_MISSING`, nothing else.
    fn check_include_missing(&mut self, include: IncludeKind) -> Result<(), Finish> {
        let mut request = Request::get(format!("{}/{OBJECT_STRUCTURE}", self.uri));
        request
            .headers
            .insert("Accept".to_owned(), "application/*".to_owned());
        let structure = self.send(request)?;

        if structure.status != 200 {
            // The class is not there either: the plain 404 of the read stands.
            return Ok(());
        }

        if !self.create_include {
            return Err(Finish {
                stdout: vec![format!(
                    "INCLUDE_MISSING: {} (re-run with --create-include)",
                    include.token()
                )],
                ..Finish::new(None, 1)
            });
        }

        self.include_absent = true;
        // A missing include has no versions to inspect.
        self.feeds.remove(0);

        Ok(())
    }

    fn get_active(&mut self) -> Result<Response, Finish> {
        let mut request = Request::get(format!("{}?version=active", self.source_uri));
        request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());

        self.send(request)
    }

    fn source_lines(&self, response: Response, what: &str) -> Result<Vec<String>, Finish> {
        if response.status == 404 && self.include_absent {
            return Ok(Vec::new());
        }

        if response.status != 200 {
            return Err(Finish::stop_http(format!("cannot {what}"), response.status));
        }

        let text = String::from_utf8(response.body)
            .map_err(|error| Finish::stop(format!("cannot {what}: not UTF-8: {error}")))?;

        Ok(text.split('\n').map(str::to_owned).collect())
    }

    fn read_active(&mut self, what: &str) -> Result<Vec<String>, Finish> {
        let response = self.get_active()?;

        self.source_lines(response, what)
    }

    /// Other inactive objects of the function group do not stop the deploy, but the syntax check
    /// of this module can fail because of them. Advisory: an unreadable list says nothing.
    fn warn_inactive_siblings(&mut self) {
        let Some(group) = self.group.clone() else {
            return;
        };
        let mut request = Request::get("/sap/bc/adt/activation/inactiveobjects");
        request
            .headers
            .insert("Accept".to_owned(), INACTIVE_OBJECTS_ACCEPT.to_owned());
        let Ok(response) = self.call(request) else {
            return;
        };
        let refs = (response.status == 200)
            .then(|| String::from_utf8(response.body).ok())
            .flatten()
            .and_then(|text| parse_inactive_feed_refs(&text).ok());
        let Some(refs) = refs else {
            return;
        };
        let group_uri = format!(
            "/sap/bc/adt/functions/groups/{}",
            group.to_ascii_lowercase()
        );
        let siblings: Vec<&str> = refs
            .iter()
            .filter(|object| {
                object
                    .parent_uri
                    .as_deref()
                    .is_some_and(|parent| parent.eq_ignore_ascii_case(&group_uri))
                    && !object.uri.eq_ignore_ascii_case(&self.uri)
            })
            .map(|object| object.name.as_str())
            .collect();

        if siblings.is_empty() {
            return;
        }

        self.warnings.push(format!(
            "W: inactive objects in function group {group}: {}",
            siblings.join(", ")
        ));
        self.warnings.push(format!(
            "W: the syntax check of {} can fail because of them",
            self.name.to_ascii_uppercase()
        ));
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

        // A function module with any inactive version is refused, whoever owns it and whatever
        // the policy says: SAP derives the interface from the inactive version.
        if self.kind == ObjectKind::Func && matches!(read, Ok(Some(_))) {
            return Err(Finish::status(
                Status::Rejected,
                format!(
                    "REJECTED: Function module {} has an inactive version; activate or discard \
                     it first in SE37",
                    self.name.to_ascii_uppercase()
                ),
            ));
        }

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

    /// The owner of the inactive version over all feeds of the target: another user's version
    /// wins, an unreadable feed fails closed, and only then does the user's own version count.
    fn inactive_owner(&mut self) -> Result<Option<String>, InactiveOwnerError> {
        let mut own = None;
        let mut unreadable = None;

        for feed in self.feeds.clone() {
            match self.feed_owner(&feed) {
                Ok(Some(owner)) if !owner.eq_ignore_ascii_case(&self.user) => {
                    return Ok(Some(owner));
                }
                Ok(Some(owner)) => own = Some(owner),
                Ok(None) => {}
                Err(InactiveOwnerError::Unreadable(reason)) => {
                    unreadable.get_or_insert(reason);
                }
                Err(request) => return Err(request),
            }
        }

        match unreadable {
            Some(reason) => Err(InactiveOwnerError::Unreadable(reason)),
            None => Ok(own),
        }
    }

    /// The owner of the inactive version of one feed, or why the feed could not tell.
    fn feed_owner(&mut self, feed: &str) -> Result<Option<String>, InactiveOwnerError> {
        let mut request = Request::get(feed);
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

        // The checkrun cannot see an include that does not exist yet, so it runs first; the
        // POST is the only extra write of a deploy and is sent once.
        if self.include_absent {
            match self.create_include_post(lock, transport_request.as_deref()) {
                Ok(IncludePost::Created) => {}
                Ok(IncludePost::Uncertain) => return Locked::IncludeUncertain,
                Err(finish) => return Locked::Finished(finish),
            }

            if cancel.is_set() {
                return Locked::Finished(Finish::cancelled());
            }
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
                check_request(corrnr).map_err(|_| {
                    Finish::stop(format!(
                        "the lock returned an invalid transport request {corrnr}"
                    ))
                })?;
                self.stdout.push(format!("transport request {corrnr}"));

                Ok(Some(corrnr.to_owned()))
            }
        }
    }

    /// `POST <class>/includes` under the class lock; it creates the include that the PUT then
    /// fills. Not idempotent, so any answer but a 2xx leaves the outcome to the reconciliation.
    fn create_include_post(
        &mut self,
        lock: &LockInfo,
        transport_request: Option<&str>,
    ) -> Result<IncludePost, Finish> {
        let Some(include) = self.include else {
            return Ok(IncludePost::Created);
        };
        let mut uri = format!(
            "{}/includes?lockHandle={}",
            self.uri,
            encode_lock_handle(&lock.handle)
        );

        if let Some(corrnr) = transport_request {
            uri.push_str(&format!("&corrNr={corrnr}"));
        }

        let mut request = Request::post(uri, include_xml(include).into_bytes());
        request
            .headers
            .insert("Content-Type".to_owned(), "application/*".to_owned());

        match self.call(request) {
            Ok(response) if (200..300).contains(&response.status) => {
                self.include_absent = false;

                Ok(IncludePost::Created)
            }
            Ok(_) => Ok(IncludePost::Uncertain),
            Err(error) if error.was_sent() => Ok(IncludePost::Uncertain),
            Err(error) => Err(Finish::error(&error, false)),
        }
    }

    fn put(
        &mut self,
        lock: &LockInfo,
        file: &[String],
        transport_request: Option<&str>,
    ) -> Result<PutResult, Finish> {
        let mut uri = format!(
            "{}?lockHandle={}",
            self.source_uri,
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
            Ok(response) if (200..300).contains(&response.status) => {
                // SAP re-renders a function module (upper-case signature, blank lines): its reply
                // is the text that becomes active.
                if self.kind == ObjectKind::Func {
                    let text = String::from_utf8_lossy(&response.body);
                    let rendered = normalize(text.split('\n').map(str::to_owned).collect());

                    self.rendered = (!rendered.is_empty()).then_some(rendered);
                }

                Ok(PutResult::Accepted)
            }
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
        let response = match self.post_activation(name, true, None, live_hash, file) {
            Ok(response) => response,
            Err(finish) => return finish,
        };
        let response = match self.resend_worklist(name, response, live_hash, file) {
            Ok(response) => response,
            Err(finish) => return finish,
        };
        let text = String::from_utf8_lossy(&response.body).into_owned();

        self.stdout.extend(format_messages(&text));

        if !(200..300).contains(&response.status) {
            let mut finish = self.reconcile_non_2xx_activation(response.status, live_hash, file);
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
            Ok(active) if self.wanted(file, live_hash).is(&active) => self.confirmed(&active),
            Ok(_) => Finish::status(
                Status::Inactive,
                "INACTIVE: activation reported no error but the active source differs",
            ),
            Err(finish) if finish.is_uncertain() => self.reconcile(live_hash, file),
            Err(finish) => finish.unreadable(),
        }
    }

    /// The activation request. An error here is already a verdict: a request that was sent is
    /// reconciled by content, one that was not is `INACTIVE`.
    fn post_activation(
        &mut self,
        name: &str,
        preaudit: bool,
        references: Option<&[ObjectRef]>,
        live_hash: &str,
        file: &[String],
    ) -> Result<Response, Finish> {
        let body = match references {
            Some(references) => worklist_request_xml(references),
            None => activation_request_xml(name, &self.uri),
        };
        let mut request = Request::post(
            format!("/sap/bc/adt/activation?method=activate&preauditRequested={preaudit}"),
            body.into_bytes(),
        );
        request
            .headers
            .insert("Content-Type".to_owned(), "application/xml".to_owned());
        request
            .headers
            .insert("Accept".to_owned(), "application/xml".to_owned());

        match self.call(request) {
            Ok(response) => Ok(response),
            Err(error) if error.was_sent() => Err(self.reconcile(live_hash, file)),
            Err(error) => Err(Finish {
                failure: Some(Failure::from(&error)),
                ..Finish::status(
                    Status::Inactive,
                    format!("INACTIVE: the activation was not sent: {error}"),
                )
            }),
        }
    }

    /// A transportable class answers the activation with a worklist and activates nothing. The
    /// second request sends every object of the worklist as listed, member entries included,
    /// without the pre-audit. A list that reaches beyond the class is not touched.
    fn resend_worklist(
        &mut self,
        name: &str,
        response: Response,
        live_hash: &str,
        file: &[String],
    ) -> Result<Response, Finish> {
        let is_worklist = |response: &Response| {
            (200..300).contains(&response.status)
                && classify_activation(&String::from_utf8_lossy(&response.body))
                    == ActivationBody::Worklist
        };

        if self.kind != ObjectKind::Clas || !is_worklist(&response) {
            return Ok(response);
        }

        let references =
            parse_object_refs(&String::from_utf8_lossy(&response.body)).map_err(|error| {
                Finish::status(
                    Status::Inactive,
                    format!("INACTIVE: cannot read the activation worklist: {error}"),
                )
            })?;

        if references.is_empty() {
            return Err(Finish::status(
                Status::Inactive,
                "INACTIVE: the activation worklist lists no object",
            ));
        }

        if let Some(outside) = references
            .iter()
            .find(|object| !under(&self.uri, &object.uri))
        {
            return Err(Finish::status(
                Status::Inactive,
                format!(
                    "INACTIVE: the activation worklist lists an object outside the class, \
                     nothing was resent: {}",
                    outside.uri
                ),
            ));
        }

        let second = self.post_activation(name, false, Some(&references), live_hash, file)?;

        if is_worklist(&second) {
            return Err(Finish::status(
                Status::Inactive,
                "INACTIVE: the class was not activated after the worklist was resent",
            ));
        }

        Ok(second)
    }

    /// The active source equals what was deployed. A class is also read inactive, because its
    /// activation covers its includes: a different inactive version means it was not activated.
    fn confirmed(&mut self, active: &[String]) -> Finish {
        if self.kind == ObjectKind::Clas {
            match self.read_inactive() {
                InactiveRead::Read(inactive) if source_hash(&inactive) != source_hash(active) => {
                    return Finish::status(
                        Status::Inactive,
                        "INACTIVE: the active source equals the file but the inactive source differs",
                    );
                }
                InactiveRead::Unreadable(reason) => {
                    return Finish::status(
                        Status::Inactive,
                        format!(
                            "INACTIVE: the active source equals the file but the inactive source \
                             is unreadable ({reason})"
                        ),
                    );
                }
                InactiveRead::Read(_) | InactiveRead::Absent => {}
            }
        }

        Finish {
            stdout: vec!["ACTIVE".to_owned(), format!("hash {}", source_hash(active))],
            ..Finish::new(Some(Status::Active), 0)
        }
    }

    fn reconcile_non_2xx_activation(
        &mut self,
        status: u16,
        live_hash: &str,
        file: &[String],
    ) -> Finish {
        match self.read_active("read back the active source after an activation failure") {
            Ok(active) if self.wanted(file, live_hash).is(&active) => Finish::status(
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

    /// What the active source must equal for the deploy to count as done. A function module is
    /// re-rendered by SAP, so the text of the PUT reply stands for the file; without that reply
    /// only a changed source with the same lines (ignoring case and blanks) can be it.
    fn wanted(&self, file: &[String], live_hash: &str) -> Wanted {
        match (&self.kind, &self.rendered) {
            (ObjectKind::Func, Some(rendered)) => Wanted::Hash(source_hash(rendered)),
            (ObjectKind::Func, None) => Wanted::Rendered {
                file: file.to_vec(),
                live_hash: live_hash.to_owned(),
            },
            _ => Wanted::Hash(source_hash(file)),
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
        let wanted = self.wanted(file, live_hash);
        let (status, message) = reconcile_with(&active, &inactive, live_hash, &wanted);

        Finish::status(status, message).exit_one()
    }

    /// The include POST was sent and its reply is unknown. It is never sent again: the inactive
    /// source of the include tells whether it exists and what it holds.
    fn reconcile_include(&mut self, file: &[String]) -> Finish {
        let kind = self.include.map_or("", IncludeKind::token);

        match self.read_inactive() {
            InactiveRead::Absent => Finish::status(
                Status::Rejected,
                format!(
                    "REJECTED: include {kind} does not exist after the unconfirmed create; \
                     nothing was written, the run can be repeated"
                ),
            ),
            InactiveRead::Read(inactive) if source_hash(&inactive) == source_hash(file) => {
                Finish::status(
                    Status::Partial,
                    format!(
                        "PARTIAL: include {kind} exists with the file as its inactive source; \
                         re-run without --create-include to activate it"
                    ),
                )
            }
            InactiveRead::Read(_) => {
                let mut finish = Finish::status(
                    Status::Partial,
                    format!(
                        "PARTIAL: include {kind} exists but its inactive source is not the file"
                    ),
                );

                finish.stdout.push("INCLUDE_SOURCE_DIFFERS".to_owned());
                finish
            }
            InactiveRead::Unreadable(reason) => Finish::status(
                Status::Partial,
                format!(
                    "PARTIAL: the include {kind} create is unconfirmed and the include cannot be \
                     read ({reason}); {RETRY}"
                ),
            ),
        }
    }

    /// The inactive source, or why it cannot be read: a failed read is never "no inactive".
    fn read_inactive(&mut self) -> InactiveRead {
        let mut request = Request::get(format!("{}?version=inactive", self.source_uri));
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
            checkrun_xml_for(&self.uri, &self.source_uri, file).into_bytes(),
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

/// The change from `live` to `file`, both whole texts.
pub fn summarize_change(live: &str, file: &str) -> Change {
    let lines = TextDiff::from_lines(live, file);
    let diff = lines
        .unified_diff()
        .context_radius(3)
        .header("live", "file")
        .to_string();

    change_of(&lines, &diff, live.lines().count())
}

/// The counts and the bounded excerpt of a diff that the confirmation of a write shows.
fn change_of(lines: &TextDiff<'_, '_, str>, diff: &str, live_lines: usize) -> Change {
    let (mut added, mut removed) = (0, 0);

    for change in lines.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }

    let all: Vec<String> = diff.trim_end().lines().map(str::to_owned).collect();
    let omitted = all.len().saturating_sub(EXCERPT_LINES);

    Change {
        added,
        removed,
        live_lines,
        excerpt: all.into_iter().take(EXCERPT_LINES).collect(),
        omitted,
    }
}

/// Request body of `POST /sap/bc/adt/checkruns` for a program; `file` is the normalized source.
pub fn checkrun_xml(name: &str, file: &[String]) -> String {
    let uri = prog_uri(name);

    checkrun_xml_for(&uri, &format!("{uri}/source/main"), file)
}

/// The checkrun of any object: `object_uri` is the object that is checked and `source_uri` the
/// source that carries `file`, which for a class include is the include of the class.
pub fn checkrun_xml_for(object_uri: &str, source_uri: &str, file: &[String]) -> String {
    let uri = escape(object_uri);
    let source = escape(source_uri);
    let content = base64(join(file).as_bytes());

    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<chkrun:checkObjectList xmlns:chkrun="http://www.sap.com/adt/checkrun" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<chkrun:checkObject adtcore:uri="{uri}" chkrun:version="active">"#,
            r#"<chkrun:artifacts><chkrun:artifact "#,
            r#"chkrun:contentType="text/plain; charset=utf-8" chkrun:uri="{source}">"#,
            r#"<chkrun:content>{content}</chkrun:content></chkrun:artifact>"#,
            r#"</chkrun:artifacts></chkrun:checkObject></chkrun:checkObjectList>"#,
        ),
        uri = uri,
        source = source,
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

/// The second activation request of a class: every object of the worklist as listed, member
/// entries with their `#type=` fragment included.
fn worklist_request_xml(references: &[ObjectRef]) -> String {
    let mut body = String::from(concat!(
        r#"<?xml version="1.0" encoding="UTF-8"?>"#,
        r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
    ));

    for object in references {
        body.push_str(&format!(
            r#"<adtcore:objectReference adtcore:uri="{}" adtcore:type="{}" adtcore:name="{}""#,
            escape(&object.uri),
            escape(&object.kind),
            escape(&object.name)
        ));

        if let Some(parent) = &object.parent_uri {
            body.push_str(&format!(r#" adtcore:parentUri="{}""#, escape(parent)));
        }

        body.push_str("/>");
    }

    body.push_str("</adtcore:objectReferences>");
    body
}

/// Whether `uri` is the object or lies below it (`/includes/...`, `/source/main#...`).
fn under(object_uri: &str, uri: &str) -> bool {
    let (object, uri) = (object_uri.to_ascii_lowercase(), uri.to_ascii_lowercase());

    uri == object || uri.starts_with(&format!("{object}/"))
}

/// Body of the include POST: the name is a dummy, the include type decides.
fn include_xml(include: IncludeKind) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<class:abapClassInclude xmlns:class="http://www.sap.com/adt/oo/classes" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="dummy" "#,
            r#"class:includeType="{}"/>"#,
        ),
        include.segment()
    )
}

/// A failed group resolution as the end of the run; before any write only a connectivity failure
/// may fall back to the other backend.
fn read_failure(error: ReadError, before_write: bool) -> Finish {
    if error.stop {
        return Finish::stop(error.message);
    }

    Finish {
        stderr: Some(format!("ERROR: {}", error.message)),
        failure: Some(error.failure),
        fallback_allowed: before_write && error.failure == Failure::PreSendConnectivity,
        detail: Some(error.message),
        ..Finish::new(None, 1)
    }
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
    reconcile_with(
        active,
        inactive,
        live_hash,
        &Wanted::Hash(source_hash(file)),
    )
}

/// What the active source must equal after the deploy.
enum Wanted {
    /// This hash: the file itself, or for a function module the text SAP rendered.
    Hash(String),
    /// A function module whose PUT reply is unknown: a source that changed and whose lines are
    /// those of the file when case and blank lines are ignored.
    Rendered {
        file: Vec<String>,
        live_hash: String,
    },
}

impl Wanted {
    fn is(&self, lines: &[String]) -> bool {
        match self {
            Self::Hash(hash) => source_hash(lines) == *hash,
            Self::Rendered { file, live_hash } => {
                source_hash(lines) != *live_hash && same_lines(lines, file)
            }
        }
    }
}

fn same_lines(left: &[String], right: &[String]) -> bool {
    let significant = |lines: &[String]| {
        lines
            .iter()
            .map(|line| line.trim().to_ascii_lowercase())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
    };

    significant(left) == significant(right)
}

fn reconcile_with(
    active: &[String],
    inactive: &InactiveRead,
    live_hash: &str,
    wanted: &Wanted,
) -> (Status, String) {
    let active_hash = source_hash(active);

    if wanted.is(active) {
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
        InactiveRead::Read(lines) if wanted.is(lines) => (
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

/// The ADT lock handles this process has seen. A handle is not bound to the session, so it
/// must not reach a client: diagnostics are scrubbed of every one of them.
static LOCK_HANDLES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The most handles kept; a process holds one lock at a time, so older ones are long released.
const KEPT_HANDLES: usize = 64;

fn remember_lock_handle(handle: &str) {
    let mut known = LOCK_HANDLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if known.iter().any(|known| known == handle) {
        return;
    }

    if known.len() == KEPT_HANDLES {
        known.remove(0);
    }

    known.push(handle.to_owned());
}

/// Every lock handle seen so far, plain and percent-encoded, longest first.
pub fn known_lock_handles() -> Vec<String> {
    let known = LOCK_HANDLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut spellings: Vec<String> = known
        .iter()
        .flat_map(|handle| [handle.clone(), encode_lock_handle(handle)])
        .collect();

    spellings.sort_by_key(|spelling| std::cmp::Reverse(spelling.len()));
    spellings.dedup();

    spellings
}

pub(crate) fn parse_lock(xml: &str) -> Result<LockInfo, String> {
    let fields = leaf_texts(xml)?;
    let handle = fields
        .get("LOCK_HANDLE")
        .filter(|value| !value.is_empty())
        .ok_or("no LOCK_HANDLE")?
        .clone();

    remember_lock_handle(&handle);

    Ok(LockInfo {
        handle,
        corrnr: fields.get("CORRNR").cloned().unwrap_or_default(),
        corruser: fields.get("CORRUSER").cloned().unwrap_or_default(),
        is_local: fields.get("IS_LOCAL").is_some_and(|value| value == "X"),
    })
}

/// Text of every element by local name; the last occurrence wins.
pub(crate) fn leaf_texts(xml: &str) -> Result<BTreeMap<String, String>, String> {
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
