//! `delete` of the DDIC types (`DTEL`, `DOMA`, `TABL`, `TTYP`) and of `TRAN` through the temporary
//! runner class.
//!
//! The generic delete engine ([`crate::operation::delete`]) owns the batch: order, plan file,
//! statuses, summary and stop rules. This backend answers for the DDIC rows with ONE run per row:
//! `DELETE_SIM` for the plan (the hash of the live definition is the plan hash) and `DELETE` for
//! the real delete, which sends the plan hash back to the run (for `TRAN` the hash of the six live
//! lines). The class re-reads the live definition and rejects with
//! `Plan changed since simulate; simulate again`. The run also owns the rules of the Python tool:
//! rows of a transparent table, an object still in use outside the batch, and the batch order;
//! Rust passes the whole batch in the payload and maps the answer.
//!
//! A delete counts as done only when the run said `DELETED` AND a new session finds the
//! definition and the TADIR row gone. A lost reply is settled once, read-only, and never resent.
//! A leftover runner class, a partial delete, an uncertain status, a signal, or a STOP ends the
//! batch; the rest is `SKIPPED`.

use crate::backend::{BackendKind, Failure};
use crate::cli::DeleteKind;
use crate::config::Connection;
use crate::domain::{Outcome, Status};
use crate::operation::delete::{DeleteBackend, DeleteReply, ObjectFacts, Reconcile, RowPlan};
use crate::operation::delete_adt::Opener;
use crate::operation::deploy::CancelFlag;
use crate::operation::payload::{self, Kind, Mode};
use crate::operation::runner::{CycleInput, PostRoute, RunnerEnd, RunnerRun, run_cycle};
use crate::operation::runner_evidence::{Expect, Proof, confirm, judge, read_evidence};
use crate::operation::runner_reconcile::{
    Clock, Intent, ReadRun, Subject, SystemClock, Verdict, reconcile, settle,
};
use crate::operation::runner_template::{TEMPLATES, generate};
use crate::runner_protocol::parse_post_reply;
use crate::runner_protocol::{Level, Nonce, Parsed, Report, RunMode, RunStatus, Stop};
use std::cell::RefCell;

/// A nonce that only validates a batch before any connection.
const CHECK_NONCE: &str = "00000000000000000000000000000000";

/// What the connection and the chosen backend give the runner cycle.
#[derive(Clone, Debug)]
pub struct DdicEnv {
    pub sid: String,
    pub client: String,
    pub conn: String,
    pub prefix: String,
    pub user: String,
    pub language: String,
    pub backend: BackendKind,
    pub rfc_available: bool,
}

impl DdicEnv {
    pub fn of(connection: &Connection, backend: BackendKind) -> Self {
        Self {
            sid: connection.system_id.clone(),
            client: connection.client.clone(),
            conn: connection.id.clone(),
            prefix: connection.runner_prefix.clone(),
            user: connection.user.clone(),
            language: connection.lang.clone(),
            backend,
            rfc_available: connection.rfc.is_some(),
        }
    }
}

/// How one run ended, as far as the delete is concerned.
enum Ended {
    /// A whole, well formed run.
    Report(Report),
    /// The POST was sent and its outcome is not known.
    Uncertain(String),
    /// Nothing ran, or a stage before the POST failed; the text is the reason.
    Refused(String),
}

pub struct DdicDelete<'a> {
    env: DdicEnv,
    open: &'a mut Opener<'a>,
    cancel: CancelFlag,
    nonce: Box<dyn FnMut() -> String + 'a>,
    clock: Box<dyn Clock + 'a>,
    /// `TYPE`, `NAME` of every row of the batch in execution order.
    rows: Vec<(String, String)>,
    /// Printed for the row just handled.
    lines: Vec<String>,
    /// `W:` lines for the stderr.
    notes: Vec<String>,
    /// `RUNNER_LEFTOVER` and the two cleanup commands.
    leftover: Option<String>,
    /// A condition that stops the command (exit 2).
    stop: Option<String>,
    interrupted: bool,
    stopped: bool,
    /// Runs that started: a batch that has none can still use another backend.
    started: usize,
    unsent: Option<String>,
    /// The runner class of the last run, for the reconciliation.
    runner: Option<String>,
}

impl<'a> DdicDelete<'a> {
    pub fn new(env: DdicEnv, open: &'a mut Opener<'a>, cancel: CancelFlag) -> Self {
        Self {
            env,
            open,
            cancel,
            nonce: Box::new(random_nonce),
            clock: Box::new(SystemClock::new()),
            rows: Vec::new(),
            lines: Vec::new(),
            notes: Vec::new(),
            leftover: None,
            stop: None,
            interrupted: false,
            stopped: false,
            started: 0,
            unsent: None,
            runner: None,
        }
    }

    /// A fixed source of run nonces, for tests.
    pub fn with_nonce(mut self, nonce: Box<dyn FnMut() -> String + 'a>) -> Self {
        self.nonce = nonce;

        self
    }

    /// A clock for the wait of the reconciliation, for tests.
    pub fn with_clock(mut self, clock: Box<dyn Clock + 'a>) -> Self {
        self.clock = clock;

        self
    }

    /// The failure of a batch whose first run never left the process: the router may run the
    /// whole batch on the other backend, because nothing was changed.
    pub fn unsent_failure(&self) -> Option<(Failure, String)> {
        if self.started > 0 {
            return None;
        }

        self.unsent
            .clone()
            .map(|message| (Failure::PreSendConnectivity, message))
    }

    /// Adds the notes, the STOP, and the leftover to the outcome of the batch, and sets the exit
    /// code the runner rules ask for: a signal 130, a leftover class 1, a STOP 2.
    pub fn finish(&mut self, outcome: &mut Outcome) {
        let mut parts: Vec<String> = Vec::new();

        if !outcome.stderr.is_empty() {
            parts.push(std::mem::take(&mut outcome.stderr));
        }

        parts.append(&mut self.notes);

        if let Some(stop) = &self.stop {
            parts.push(format!("STOP: {stop}"));
        }

        if let Some(leftover) = &self.leftover {
            parts.push(leftover.clone());
        }

        outcome.stderr = parts.join("\n");

        if let Some(code) = exit_override(
            self.interrupted || self.cancel.is_set(),
            self.leftover.is_some(),
            self.stop.is_some(),
        ) {
            outcome.exit = code;
        }
    }

    /// Runs one row: the payload first (nothing is opened when it is refused), then the cycle.
    fn run_row(
        &mut self,
        mode: Mode,
        kind: Kind,
        name: &str,
        tr: Option<&str>,
        plan: Option<&str>,
    ) -> Result<Ended, String> {
        let nonce_text = (self.nonce)();
        let nonce = Nonce::parse(&nonce_text)?;
        let pairs: Vec<(&str, &str)> = self
            .rows
            .iter()
            .map(|(kind, name)| (kind.as_str(), name.as_str()))
            .collect();
        let mut request = payload::Request::new(mode, kind, name, &nonce_text);

        request.tr = tr;
        request.plan = plan;
        request.batch = &pairs;

        let lines = payload::encode(&request)?;
        let run_mode = match mode {
            Mode::Delete => RunMode::Delete,
            _ => RunMode::DeleteSim,
        };
        let mut run = cycle(&self.env, &mut *self.open, kind, &lines, &self.cancel)?;

        settle(&mut run, &nonce, run_mode);

        self.absorb(&run);

        let ended = match &run.end {
            RunnerEnd::Posted(response) => {
                let body = String::from_utf8_lossy(&response.body);

                match parse_post_reply(&nonce, run_mode, response.status, &body) {
                    Parsed::Complete(report) => Ended::Report(report),
                    Parsed::Uncertain { reason, warnings } => {
                        self.notes.extend(warnings);

                        Ended::Uncertain(reason.to_string())
                    }
                    Parsed::NotRun(status) => {
                        Ended::Refused(format!("runner POST failed: HTTP {status}"))
                    }
                }
            }
            RunnerEnd::PostUncertain(error) => Ended::Uncertain(error.to_string()),
            RunnerEnd::Rejected(text) => Ended::Refused(text.clone()),
            RunnerEnd::Stop(text) => {
                self.stop = Some(text.clone());

                Ended::Refused(text.clone())
            }
            RunnerEnd::Interrupted => Ended::Refused("interrupted before the run".to_owned()),
            RunnerEnd::NotStarted(error) => {
                if self.started == 0 {
                    self.unsent.get_or_insert_with(|| error.to_string());
                }

                Ended::Refused(format!("runner start failed: {error}"))
            }
        };

        Ok(ended)
    }

    /// What a finished cycle adds to the output and to the batch state.
    fn absorb(&mut self, run: &RunnerRun) {
        self.lines.extend(run.preflight.clone());
        self.lines.extend(run.runner_line());
        self.notes.extend(run.notes.iter().cloned());
        self.runner = run.name.clone();

        if !matches!(run.end, RunnerEnd::NotStarted(_)) {
            self.started += 1;
        }

        if let Some(text) = run.leftover_text() {
            self.leftover = Some(text);
        }

        if run.interrupted {
            self.interrupted = true;
        }

        if run.halts_batch() || matches!(run.end, RunnerEnd::NotStarted(_)) {
            self.stopped = true;
        }
    }

    /// The definition and the TADIR row must be gone in a new session; the run's word alone does
    /// not make a delete.
    fn confirm_gone(&mut self, kind: Kind, name: &str) -> DeleteReply {
        let runner = self.runner.clone().unwrap_or_default();
        let mut evidence = read_evidence(&mut *self.open, kind, name, &runner);

        self.notes.extend(evidence.notes.iter().cloned());

        // A class that stays is reported as the leftover of the run; it must not turn a delete
        // that the new session confirms into a partial one.
        if self.leftover.is_some() {
            evidence.runner = Ok(None);
        }

        let proof = judge(&Expect::Absent, kind, &evidence);

        if confirm(true, &proof) == Status::Active {
            DeleteReply::Deleted
        } else {
            DeleteReply::Partial(unproven(&proof))
        }
    }
}

impl DeleteBackend for DdicDelete<'_> {
    /// A DDIC row is planned in its own run, never read.
    fn inspect(&mut self, kind: DeleteKind, _name: &str) -> Result<Option<ObjectFacts>, String> {
        Err(format!("{kind} is planned in a runner run, not read"))
    }

    fn delete(&mut self, kind: DeleteKind, _name: &str, _tr: Option<&str>) -> DeleteReply {
        DeleteReply::Rejected(format!("{kind} needs the plan hash of its simulation"))
    }

    fn reconcile(&mut self, kind: DeleteKind, name: &str) -> Reconcile {
        let DeleteKind::Runner(kind) = kind else {
            return Reconcile::Failed(format!("{kind} is not a runner type"));
        };
        let runner = self.runner.clone().unwrap_or_default();
        let subject = Subject {
            kind,
            name,
            runner: &runner,
        };
        let Self {
            env,
            open,
            nonce,
            clock,
            cancel,
            ..
        } = self;
        let shared = RefCell::new(&mut **open);
        let mut open_proof = || {
            let mut guard = shared.borrow_mut();

            (**guard)()
        };
        let mut read_run = |fresh: &CancelFlag| -> ReadRun {
            let mut open_cycle = || {
                let mut guard = shared.borrow_mut();

                (**guard)()
            };

            read_once(env, &mut open_cycle, &mut **nonce, fresh, kind, name)
        };
        let result = reconcile(
            &Intent::Delete,
            &subject,
            &mut open_proof,
            &mut read_run,
            &mut **clock,
            cancel,
        );

        self.notes.extend(result.notes.iter().cloned());

        if let Some(read) = &result.read_run {
            self.lines.extend(read.runner_line.clone());
            self.notes.extend(read.notes.iter().cloned());

            if let Some(text) = &read.leftover_text {
                self.leftover = Some(text.clone());
                self.stopped = true;
            }
        }

        if result.skipped {
            self.interrupted = true;
        }

        settled(&result.verdict)
    }

    /// Validates every DDIC row of the batch (names, request, batch list) before the first
    /// connection, and keeps the batch for the payloads.
    fn begin(&mut self, rows: &[(DeleteKind, String)], tr: Option<&str>) -> Result<(), String> {
        self.rows = rows
            .iter()
            .map(|(kind, name)| (kind.token().to_owned(), name.clone()))
            .collect();

        let pairs: Vec<(&str, &str)> = self
            .rows
            .iter()
            .map(|(kind, name)| (kind.as_str(), name.as_str()))
            .collect();

        for (kind, name) in rows {
            if let DeleteKind::Runner(kind) = kind {
                let mut request = payload::Request::new(Mode::DeleteSim, *kind, name, CHECK_NONCE);

                request.tr = tr;
                request.batch = &pairs;
                payload::encode(&request)?;
            }
        }

        Ok(())
    }

    fn plans_in_run(&self, kind: DeleteKind) -> bool {
        matches!(kind, DeleteKind::Runner(_))
    }

    fn plan_row(&mut self, kind: DeleteKind, name: &str, tr: Option<&str>) -> RowPlan {
        let DeleteKind::Runner(kind) = kind else {
            return RowPlan::Rejected(format!("{kind} is not a runner type"));
        };

        match self.run_row(Mode::DeleteSim, kind, name, tr, None) {
            Ok(Ended::Report(report)) => {
                self.notes.extend(report.warnings.iter().cloned());

                row_plan(kind, name, &report)
            }
            Ok(Ended::Uncertain(reason)) => {
                RowPlan::Rejected(format!("the simulation status is uncertain: {reason}"))
            }
            Ok(Ended::Refused(text)) | Err(text) => RowPlan::Rejected(text),
        }
    }

    fn delete_planned(
        &mut self,
        kind: DeleteKind,
        name: &str,
        tr: Option<&str>,
        plan_hash: &str,
    ) -> DeleteReply {
        let DeleteKind::Runner(kind) = kind else {
            return DeleteReply::Rejected(format!("{kind} is not a runner type"));
        };

        match self.run_row(Mode::Delete, kind, name, tr, Some(plan_hash)) {
            Ok(Ended::Report(report)) => {
                self.notes.extend(report.warnings.iter().cloned());

                match report.status {
                    RunStatus::Deleted => self.confirm_gone(kind, name),
                    RunStatus::NotFound => DeleteReply::NotFound,
                    RunStatus::Rejected => DeleteReply::Rejected(reason_of(&report)),
                    _ => DeleteReply::Partial(partial_text(&report)),
                }
            }
            Ok(Ended::Uncertain(reason)) => DeleteReply::Uncertain(reason),
            Ok(Ended::Refused(text)) | Err(text) => DeleteReply::Rejected(text),
        }
    }

    fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }

    fn stops_batch(&self) -> bool {
        self.stopped
    }
}

/// The batch of an operation that mixes the M4 kinds with DDIC rows: a DDIC row goes to the
/// runner backend, every other row to the ADT backend.
pub struct RunnerBatch<'a> {
    adt: &'a mut dyn DeleteBackend,
    runner: &'a mut dyn DeleteBackend,
}

impl<'a> RunnerBatch<'a> {
    pub fn new(adt: &'a mut dyn DeleteBackend, runner: &'a mut dyn DeleteBackend) -> Self {
        Self { adt, runner }
    }

    fn owner(&mut self, kind: DeleteKind) -> &mut dyn DeleteBackend {
        if matches!(kind, DeleteKind::Runner(_)) {
            &mut *self.runner
        } else {
            &mut *self.adt
        }
    }
}

impl DeleteBackend for RunnerBatch<'_> {
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        self.owner(kind).inspect(kind, name)
    }

    fn delete(&mut self, kind: DeleteKind, name: &str, tr: Option<&str>) -> DeleteReply {
        self.owner(kind).delete(kind, name, tr)
    }

    fn reconcile(&mut self, kind: DeleteKind, name: &str) -> Reconcile {
        self.owner(kind).reconcile(kind, name)
    }

    fn is_include(&mut self, kind: DeleteKind, name: &str) -> bool {
        self.owner(kind).is_include(kind, name)
    }

    /// The runner side validates first, so that its STOP comes before the ADT side reads.
    fn begin(&mut self, rows: &[(DeleteKind, String)], tr: Option<&str>) -> Result<(), String> {
        self.runner.begin(rows, tr)?;
        self.adt.begin(rows, tr)
    }

    fn begin_failure(&self, message: &str) -> (i32, String) {
        self.runner.begin_failure(message)
    }

    fn halted(&self) -> bool {
        self.adt.halted() || self.runner.halted()
    }

    fn plans_in_run(&self, kind: DeleteKind) -> bool {
        matches!(kind, DeleteKind::Runner(_)) && self.runner.plans_in_run(kind)
    }

    fn plan_row(&mut self, kind: DeleteKind, name: &str, tr: Option<&str>) -> RowPlan {
        self.runner.plan_row(kind, name, tr)
    }

    fn delete_planned(
        &mut self,
        kind: DeleteKind,
        name: &str,
        tr: Option<&str>,
        plan_hash: &str,
    ) -> DeleteReply {
        self.runner.delete_planned(kind, name, tr, plan_hash)
    }

    fn take_lines(&mut self) -> Vec<String> {
        let mut lines = self.adt.take_lines();

        lines.extend(self.runner.take_lines());

        lines
    }

    fn stops_batch(&self) -> bool {
        self.runner.stops_batch()
    }
}

/// The class cycle of one run: the template of the type with the payload lines.
fn cycle(
    env: &DdicEnv,
    open: &mut Opener<'_>,
    kind: Kind,
    payload_lines: &[String],
    cancel: &CancelFlag,
) -> Result<RunnerRun, String> {
    let template = TEMPLATES
        .iter()
        .find(|template| template.name == kind.token())
        .ok_or_else(|| format!("no runner template for {}", kind.token()))?;
    let build = |class: &str| generate(template, class, payload_lines);
    let operation = format!("delete {}", kind.token());
    let input = CycleInput {
        operation: &operation,
        sid: &env.sid,
        client: &env.client,
        conn: &env.conn,
        prefix: &env.prefix,
        user: &env.user,
        language: &env.language,
        backend: env.backend,
        rfc_available: env.rfc_available,
        build: &build,
    };

    Ok(run_cycle(&input, open, PostRoute::Cycle, cancel))
}

/// The extra `READ` run of a reconciliation, with its own nonce and the flag it is given.
fn read_once(
    env: &DdicEnv,
    open: &mut Opener<'_>,
    nonce: &mut dyn FnMut() -> String,
    fresh: &CancelFlag,
    kind: Kind,
    name: &str,
) -> ReadRun {
    let failed = |reason: String| ReadRun {
        facts: Err(reason),
        runner_line: None,
        leftover: None,
        leftover_text: None,
        notes: Vec::new(),
    };
    let text = nonce();
    let Ok(parsed) = Nonce::parse(&text) else {
        return failed("the runner nonce is not valid".to_owned());
    };
    let request = payload::Request::new(Mode::Read, kind, name, &text);
    let lines = match payload::encode(&request) {
        Ok(lines) => lines,
        Err(reason) => return failed(reason),
    };

    match cycle(env, open, kind, &lines, fresh) {
        Ok(run) => ReadRun::from_run(&run, &parsed),
        Err(reason) => failed(reason),
    }
}

/// The plan of a row from the answer of a `DELETE_SIM` run.
fn row_plan(kind: Kind, name: &str, report: &Report) -> RowPlan {
    match report.status {
        RunStatus::NotFound => RowPlan::NotFound,
        RunStatus::SimulatedOk => ready(kind, name, report),
        _ => RowPlan::Rejected(reason_of(report)),
    }
}

fn fact<'r>(report: &'r Report, key: &str) -> Option<&'r str> {
    report
        .facts
        .iter()
        .find(|fact| fact.key == key)
        .map(|fact| fact.value.as_str())
}

fn ready(kind: Kind, name: &str, report: &Report) -> RowPlan {
    let Some(hash) = fact(report, "HASH") else {
        return RowPlan::Rejected("the runner reported no plan hash".to_owned());
    };

    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return RowPlan::Rejected("the runner reported a malformed plan hash".to_owned());
    }

    let value = |key: &str| fact(report, key).unwrap_or("-");
    let request = value("REQUEST");
    let mut lines = vec![format!(
        "DELETE {} {name} package {} author {} TR {request}",
        kind.token(),
        value("PACKAGE"),
        value("AUTHOR")
    )];

    if kind == Kind::Tran {
        // The hashed definition is the six live lines; a missing transaction is NOT_FOUND, so
        // there is no `NO_ACTIVE` marker. The program and cinfo are shown with the plan.
        lines.push(format!("PROGRAM {}", value("PROGRAM")));
        lines.push(format!("CINFO {}", value("CINFO")));
        lines.push(format!("SOURCE LINES {hash}"));
    } else {
        let marker = if value("STATE") == "NO_ACTIVE" {
            "NO_ACTIVE"
        } else {
            hash
        };

        lines.push(format!("SOURCE XML {marker}"));
    }

    if let Some(rows) = fact(report, "ROWS") {
        lines.push(format!("ROWS {rows}"));
    }

    for message in &report.messages {
        lines.push(match message.level {
            Level::Info => message.text.clone(),
            Level::Warning => format!("W: {}", message.text),
            Level::Error => format!("E: {}", message.text),
        });
    }

    let guards: Vec<&str> = report
        .facts
        .iter()
        .filter(|fact| fact.key == "GUARD")
        .map(|fact| fact.value.as_str())
        .collect();

    if !guards.is_empty() {
        lines.push(format!("guards active: {}", guards.join("; ")));
    }

    RowPlan::Ready {
        hash: hash.to_ascii_lowercase(),
        tr: (request != "-").then(|| request.to_owned()),
        lines,
    }
}

/// The first error of the run, or the status when it gave none.
fn reason_of(report: &Report) -> String {
    report
        .messages
        .iter()
        .find(|message| message.level == Level::Error)
        .map_or_else(
            || format!("the runner answered {}", report.status.token()),
            |message| message.text.clone(),
        )
}

/// What a run says about a partial delete: its markers, else its first error.
fn partial_text(report: &Report) -> String {
    let stops: Vec<&str> = report
        .stops
        .iter()
        .map(|stop| match stop {
            Stop::PartialDelete(text) | Stop::ObjectStillExists(text) => text.as_str(),
        })
        .collect();

    if stops.is_empty() {
        reason_of(report)
    } else {
        stops.join("; ")
    }
}

/// The text of a delete that the run reported and a new session did not confirm.
fn unproven(proof: &Proof) -> String {
    match proof {
        Proof::Mismatch(difference) => {
            format!("the run reported DELETED but the new session disagrees: {difference}")
        }
        Proof::Unreadable(reason) => {
            format!("the run reported DELETED but the new session could not confirm it: {reason}")
        }
        Proof::Matches => "the delete is confirmed".to_owned(),
    }
}

/// The verdict of a reconciliation as the engine's three outcomes.
fn settled(verdict: &Verdict) -> Reconcile {
    match verdict {
        Verdict::DeletedReconciled => Reconcile::Gone,
        Verdict::DeletePartial => Reconcile::StillExists,
        Verdict::Incomplete(reason) => Reconcile::Failed(reason.clone()),
        other => Reconcile::Failed(other.line()),
    }
}

/// The exit code a runner rule forces: a signal 130, then a leftover class 1, then a STOP 2.
pub fn exit_override(interrupted: bool, leftover: bool, stop: bool) -> Option<i32> {
    if interrupted {
        Some(130)
    } else if leftover {
        Some(1)
    } else if stop {
        Some(2)
    } else {
        None
    }
}

fn random_nonce() -> String {
    let mut bytes = [0u8; 16];

    getrandom::fill(&mut bytes).expect("the OS random source is available");

    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}
