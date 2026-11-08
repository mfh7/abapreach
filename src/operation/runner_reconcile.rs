//! Reconciliation of an uncertain runner operation.
//!
//! A classrun POST whose reply is lost, answered 5xx, cut off, timed out, or broken leaves the
//! outcome in SAP unknown. The POST is never resent. This module settles the outcome read-only
//! from a NEW session: first through the data previews of the fixed tables, then, only when the
//! previews cannot tell, with at most one extra `READ` run (a new runner class). The reads wait
//! for a stable state first (see [`stable_state`]), because on RFC-SADT the server run goes on
//! after the reply is lost, and on HTTP a cancelled run may still be rolling back.
//!
//! The mapping from what was seen to the status line is in the three `classify_*` functions: they
//! take plain values and have no side effect. Every verdict exits 1, or 130 after Ctrl-C.

use crate::domain::Status;
use crate::operation::delete_adt::Opener;
use crate::operation::deploy::{CancelFlag, INTERRUPT_EXIT_CODE};
use crate::operation::payload::Kind;
use crate::operation::runner::{RunnerEnd, RunnerRun};
use crate::operation::runner_evidence::{Probe, read_evidence, read_inactive};
use crate::runner_protocol::{Nonce, Parsed, RunMode, RunStatus, parse_post_reply};
use std::time::{Duration, Instant};

/// The wait before the first read: a run that goes on in SAP is usually over by then.
pub const FIRST_WAIT: Duration = Duration::from_secs(20);
/// The time between two reads.
pub const READ_INTERVAL: Duration = Duration::from_secs(10);
/// The wait ends without a verdict at this time (above the HTTP timeout of about 73 s).
pub const GIVE_UP: Duration = Duration::from_secs(120);

const ADVICE: &str = "check with read before retrying";
const SLICE: Duration = Duration::from_millis(50);

/// Why a status is uncertain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Cause {
    /// The POST was sent and no usable reply came back (lost reply, dropped connection,
    /// timeout, or an ABAP runtime failure over SADT).
    LostReply(String),
    /// A reply came back but cannot be believed (5xx, truncated or broken protocol).
    Broken(String),
}

impl Cause {
    /// The `W:` line for the stderr.
    pub fn note(&self) -> String {
        let (Self::LostReply(text) | Self::Broken(text)) = self;

        format!(
            "W: the classrun status is uncertain ({text}); reconciling from a new session, the run is never resent"
        )
    }
}

/// Whether the end of a cycle is an uncertain status. A refused POST, a stage failure before the
/// POST, and a whole well formed run are not.
pub fn uncertainty(end: &RunnerEnd, nonce: &Nonce, mode: RunMode) -> Option<Cause> {
    match end {
        RunnerEnd::PostUncertain(error) => Some(Cause::LostReply(error.to_string())),
        RunnerEnd::Posted(response) => {
            let body = String::from_utf8_lossy(&response.body);

            match parse_post_reply(nonce, mode, response.status, &body) {
                Parsed::Uncertain { reason, .. } => Some(Cause::Broken(reason.to_string())),
                Parsed::Complete(_) | Parsed::NotRun(_) => None,
            }
        }
        RunnerEnd::Rejected(_)
        | RunnerEnd::Stop(_)
        | RunnerEnd::Interrupted
        | RunnerEnd::NotStarted(_) => None,
    }
}

/// Marks the row uncertain when its end is, so that the batch stops after it, and returns the
/// cause. The cause also goes to the notes of the row.
pub fn settle(run: &mut RunnerRun, nonce: &Nonce, mode: RunMode) -> Option<Cause> {
    let cause = uncertainty(&run.end, nonce, mode);

    run.uncertain = cause.is_some();

    if let Some(cause) = &cause {
        run.notes.push(cause.note());
    }

    cause
}

/// What the operation asked for, with the hashes a deploy compares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Intent<'a> {
    /// The hash of the file and the hash of the base the file was made against.
    Deploy {
        file_hash: &'a str,
        base_hash: &'a str,
    },
    Create,
    Delete,
}

/// The object and the runner class of the uncertain run.
#[derive(Clone, Copy, Debug)]
pub struct Subject<'a> {
    pub kind: Kind,
    pub name: &'a str,
    pub runner: &'a str,
}

/// Whether an object is there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Presence {
    Present,
    Absent,
    Unknown(String),
}

/// The active version of a definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Active {
    Missing,
    Hash(String),
}

/// Whether an inactive version exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Flag {
    Yes,
    No,
    Unknown(String),
}

/// The settled outcome of an uncertain operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    ActiveReconciled,
    Inactive,
    PartialUnknown,
    PartialUnexpected,
    CreatePartial,
    CreateRejected,
    DeletedReconciled,
    DeletePartial,
    /// Nothing could be settled; the text says why.
    Incomplete(String),
}

impl Verdict {
    pub fn token(&self) -> &'static str {
        match self {
            Self::ActiveReconciled => "ACTIVE (reconciled)",
            Self::Inactive => "INACTIVE",
            Self::PartialUnknown => "PARTIAL: outcome unknown",
            Self::PartialUnexpected => "PARTIAL: unexpected state",
            Self::CreatePartial => "CREATE: PARTIAL",
            Self::CreateRejected => "CREATE: REJECTED",
            Self::DeletedReconciled => "DELETED (reconciled)",
            Self::DeletePartial => "PARTIAL",
            Self::Incomplete(_) => "PARTIAL: reconciliation incomplete",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::ActiveReconciled => "the active definition equals the file",
            Self::Inactive => "the write reached SAP but the activation did not",
            Self::PartialUnknown => {
                "the active definition equals the base and there is no inactive version"
            }
            Self::PartialUnexpected => "the active definition is neither the base nor the file",
            Self::CreatePartial => "the object exists after an uncertain create",
            Self::CreateRejected => "the object does not exist after an uncertain create",
            Self::DeletedReconciled => "the object is gone in a new session",
            Self::DeletePartial => "the object still exists after an uncertain delete",
            Self::Incomplete(reason) => reason,
        }
    }

    /// The status line with the advice to read before trying again.
    pub fn line(&self) -> String {
        let token = self.token();
        let separator = if token.contains(':') { ';' } else { ':' };

        format!("{token}{separator} {}; {ADVICE}", self.detail())
    }

    pub fn status(&self) -> Status {
        match self {
            Self::ActiveReconciled | Self::DeletedReconciled => Status::Active,
            Self::Inactive => Status::Inactive,
            Self::CreateRejected => Status::Rejected,
            Self::PartialUnknown
            | Self::PartialUnexpected
            | Self::CreatePartial
            | Self::DeletePartial
            | Self::Incomplete(_) => Status::Partial,
        }
    }

    /// A reconciled status is never a clean success.
    pub fn exit(&self) -> i32 {
        1
    }
}

fn same(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// A deploy: the file hash wins, then the base, which decides between `INACTIVE` and an unknown
/// outcome by the inactive version. Anything else, a definition that is gone included, is
/// unexpected.
pub fn classify_deploy(
    file_hash: &str,
    base_hash: &str,
    active: &Active,
    inactive: &Flag,
) -> Verdict {
    let Active::Hash(hash) = active else {
        return Verdict::PartialUnexpected;
    };

    if same(hash, file_hash) {
        return Verdict::ActiveReconciled;
    }

    if !same(hash, base_hash) {
        return Verdict::PartialUnexpected;
    }

    match inactive {
        Flag::Yes => Verdict::Inactive,
        Flag::No => Verdict::PartialUnknown,
        Flag::Unknown(reason) => {
            Verdict::Incomplete(format!("the inactive version cannot be read ({reason})"))
        }
    }
}

fn unreadable(reason: &str) -> Verdict {
    Verdict::Incomplete(format!("the object cannot be read ({reason})"))
}

/// A create: the object is there (`PARTIAL`) or it is not (`REJECTED`).
pub fn classify_create(presence: &Presence) -> Verdict {
    match presence {
        Presence::Present => Verdict::CreatePartial,
        Presence::Absent => Verdict::CreateRejected,
        Presence::Unknown(reason) => unreadable(reason),
    }
}

/// A delete: the object is gone (`DELETED (reconciled)`) or it is not (`PARTIAL`).
pub fn classify_delete(presence: &Presence) -> Verdict {
    match presence {
        Presence::Present => Verdict::DeletePartial,
        Presence::Absent => Verdict::DeletedReconciled,
        Presence::Unknown(reason) => unreadable(reason),
    }
}

/// What the extra `READ` run saw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadFacts {
    Found { hash: String },
    NotFound,
}

/// What the `READ` run reported, read from the end of its cycle. Anything but a whole run with
/// `OK` and a `HASH` fact, or `NOT_FOUND`, is an error.
pub fn read_facts(end: &RunnerEnd, nonce: &Nonce) -> Result<ReadFacts, String> {
    match end {
        RunnerEnd::Posted(response) => {
            let body = String::from_utf8_lossy(&response.body);

            match parse_post_reply(nonce, RunMode::Read, response.status, &body) {
                Parsed::Complete(report) => match report.status {
                    RunStatus::Ok => report
                        .facts
                        .iter()
                        .find(|fact| fact.key == "HASH")
                        .map(|fact| ReadFacts::Found {
                            hash: fact.value.clone(),
                        })
                        .ok_or_else(|| "the READ run reported no HASH".to_owned()),
                    RunStatus::NotFound => Ok(ReadFacts::NotFound),
                    other => Err(format!("the READ run ended {}", other.token())),
                },
                Parsed::Uncertain { reason, .. } => Err(reason.to_string()),
                Parsed::NotRun(status) => Err(format!("the READ run was refused (HTTP {status})")),
            }
        }
        RunnerEnd::PostUncertain(error) => Err(format!("the READ run reply is uncertain: {error}")),
        RunnerEnd::Rejected(text) => Err(text.clone()),
        RunnerEnd::Stop(text) => Err(format!("STOP: {text}")),
        RunnerEnd::Interrupted => Err("the READ run was interrupted".to_owned()),
        RunnerEnd::NotStarted(error) => Err(format!("the READ run did not start: {error}")),
    }
}

/// The extra `READ` run: what it saw, and what the caller prints about its class.
#[derive(Clone, Debug)]
pub struct ReadRun {
    pub facts: Result<ReadFacts, String>,
    /// The `RUNNER:` line of the run.
    pub runner_line: Option<String>,
    /// The class that could not be removed.
    pub leftover: Option<String>,
    /// `RUNNER_LEFTOVER` and the two cleanup commands.
    pub leftover_text: Option<String>,
    /// `W:` lines for the stderr.
    pub notes: Vec<String>,
}

impl ReadRun {
    pub fn from_run(run: &RunnerRun, nonce: &Nonce) -> Self {
        Self {
            facts: read_facts(&run.end, nonce),
            runner_line: run.runner_line(),
            leftover: run.leftover.clone(),
            leftover_text: run.leftover_text(),
            notes: run.notes.clone(),
        }
    }
}

/// Time for the stable-state wait; a fake in tests.
pub trait Clock {
    /// Waits `duration`, or returns `false` at once when `abort` is true.
    fn wait(&mut self, duration: Duration, abort: &dyn Fn() -> bool) -> bool;
    /// The time since the reconciliation began.
    fn elapsed(&self) -> Duration;
}

/// The wall clock. A wait sleeps in short slices so that an abort is noticed at once.
pub struct SystemClock {
    started: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wait(&mut self, duration: Duration, abort: &dyn Fn() -> bool) -> bool {
        let deadline = Instant::now() + duration;

        loop {
            if abort() {
                return false;
            }

            let left = deadline.saturating_duration_since(Instant::now());

            if left.is_zero() {
                return true;
            }

            std::thread::sleep(left.min(SLICE));
        }
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

/// How the wait for a stable state ended.
#[derive(Debug, Eq, PartialEq)]
pub enum Stable<T> {
    /// Two reads in a row were identical.
    Settled(T),
    /// No two reads agreed by [`GIVE_UP`].
    GaveUp,
    /// `abort` was true.
    Skipped,
}

/// Waits [`FIRST_WAIT`], then reads every [`READ_INTERVAL`] until two reads in a row are
/// identical, and gives up when the clock reaches [`GIVE_UP`].
pub fn stable_state<T: PartialEq>(
    clock: &mut dyn Clock,
    abort: &dyn Fn() -> bool,
    mut read: impl FnMut() -> T,
) -> Stable<T> {
    if !clock.wait(FIRST_WAIT, abort) {
        return Stable::Skipped;
    }

    let mut previous = None;

    loop {
        if abort() {
            return Stable::Skipped;
        }

        let current = read();

        if previous.as_ref() == Some(&current) {
            return Stable::Settled(current);
        }

        previous = Some(current);

        if clock.elapsed() >= GIVE_UP {
            return Stable::GaveUp;
        }

        if !clock.wait(READ_INTERVAL, abort) {
            return Stable::Skipped;
        }
    }
}

/// What one read of the evidence saw. Facts the intent does not need are held at "no row".
#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    definition: Probe,
    directory: Probe,
    inactive: Probe,
}

fn snapshot(
    open: &mut Opener<'_>,
    intent: &Intent<'_>,
    subject: &Subject<'_>,
    notes: &mut Vec<String>,
) -> Snapshot {
    let evidence = read_evidence(open, subject.kind, subject.name, subject.runner);

    for note in evidence.notes {
        if !notes.contains(&note) {
            notes.push(note);
        }
    }

    match intent {
        Intent::Deploy { .. } => Snapshot {
            definition: evidence.definition,
            directory: Ok(None),
            inactive: read_inactive(open, subject.kind, subject.name),
        },
        Intent::Create | Intent::Delete => Snapshot {
            definition: evidence.definition,
            directory: evidence.directory,
            inactive: Ok(None),
        },
    }
}

/// Present when any row is there, absent when every preview read no row, otherwise unknown.
fn presence_of(snapshot: &Snapshot) -> Presence {
    let probes = [&snapshot.definition, &snapshot.directory];

    if probes.iter().any(|probe| matches!(probe, Ok(Some(_)))) {
        return Presence::Present;
    }

    let reasons: Vec<&str> = probes
        .iter()
        .filter_map(|probe| probe.as_ref().err().map(String::as_str))
        .collect();

    if reasons.is_empty() {
        Presence::Absent
    } else {
        Presence::Unknown(reasons.join("; "))
    }
}

fn flag_of(probe: &Probe) -> Flag {
    match probe {
        Ok(Some(_)) => Flag::Yes,
        Ok(None) => Flag::No,
        Err(reason) => Flag::Unknown(reason.clone()),
    }
}

/// The result of a reconciliation.
#[derive(Clone, Debug)]
pub struct Reconciled {
    pub verdict: Verdict,
    /// A second Ctrl-C ended the reconciliation early.
    pub skipped: bool,
    /// The extra `READ` run, when one ran.
    pub read_run: Option<ReadRun>,
    /// `W:` lines for the stderr.
    pub notes: Vec<String>,
}

impl Reconciled {
    fn ended(verdict: Verdict, notes: Vec<String>) -> Self {
        Self {
            verdict,
            skipped: false,
            read_run: None,
            notes,
        }
    }

    fn skipped(notes: Vec<String>) -> Self {
        Self {
            skipped: true,
            ..Self::ended(
                Verdict::Incomplete("reconciliation skipped by a second interrupt".to_owned()),
                notes,
            )
        }
    }

    /// 130 after a Ctrl-C (`interrupted`) or when the reconciliation was skipped, else 1.
    pub fn exit_code(&self, interrupted: bool) -> i32 {
        if interrupted || self.skipped {
            INTERRUPT_EXIT_CODE
        } else {
            self.verdict.exit()
        }
    }
}

/// Settles an uncertain operation read-only. `open` yields a new session each time; `read_run`
/// runs one `READ` cycle with a fresh class and the cancel flag it is given. The first Ctrl-C
/// does not stop the reconciliation (the process stays alive until it is over); a second one
/// skips whatever is left and ends with an incomplete verdict. Nothing here writes to SAP.
pub fn reconcile(
    intent: &Intent<'_>,
    subject: &Subject<'_>,
    open: &mut Opener<'_>,
    read_run: &mut dyn FnMut(&CancelFlag) -> ReadRun,
    clock: &mut dyn Clock,
    cancel: &CancelFlag,
) -> Reconciled {
    let _write = cancel.hold_write();
    let abort = || cancel.is_second_interrupt();
    let mut notes = Vec::new();
    let settled = stable_state(clock, &abort, || {
        snapshot(&mut *open, intent, subject, &mut notes)
    });
    let snapshot = match settled {
        Stable::Settled(snapshot) => snapshot,
        Stable::Skipped => return Reconciled::skipped(notes),
        Stable::GaveUp => {
            return Reconciled::ended(
                Verdict::Incomplete(format!(
                    "the state did not settle within {} s",
                    GIVE_UP.as_secs()
                )),
                notes,
            );
        }
    };
    // The one extra READ run: none after a second Ctrl-C.
    let mut extra = None;
    let mut read = |extra: &mut Option<ReadRun>| -> Option<Result<ReadFacts, String>> {
        if abort() {
            return None;
        }

        let run = read_run(&CancelFlag::new());
        let facts = run.facts.clone();

        *extra = Some(run);

        Some(facts)
    };
    let verdict = match *intent {
        Intent::Deploy {
            file_hash,
            base_hash,
        } => {
            let inactive = flag_of(&snapshot.inactive);

            match snapshot.definition {
                Ok(None) => classify_deploy(file_hash, base_hash, &Active::Missing, &inactive),
                _ => match read(&mut extra) {
                    None => return Reconciled::skipped(notes),
                    Some(Ok(ReadFacts::Found { hash })) => {
                        classify_deploy(file_hash, base_hash, &Active::Hash(hash), &inactive)
                    }
                    Some(Ok(ReadFacts::NotFound)) => {
                        classify_deploy(file_hash, base_hash, &Active::Missing, &inactive)
                    }
                    Some(Err(error)) => Verdict::Incomplete(format!("READ run failed: {error}")),
                },
            }
        }
        Intent::Create | Intent::Delete => {
            let presence = match presence_of(&snapshot) {
                Presence::Unknown(reason) => match read(&mut extra) {
                    None => return Reconciled::skipped(notes),
                    Some(Ok(ReadFacts::Found { .. })) => Presence::Present,
                    Some(Ok(ReadFacts::NotFound)) => Presence::Absent,
                    Some(Err(error)) => {
                        Presence::Unknown(format!("READ run failed: {error}; previews: {reason}"))
                    }
                },
                known => known,
            };

            if *intent == Intent::Create {
                classify_create(&presence)
            } else {
                classify_delete(&presence)
            }
        }
    };

    Reconciled {
        verdict,
        skipped: false,
        read_run: extra,
        notes,
    }
}
