//! `read` and `create` of a `TRAN` through the temporary runner class (M6 Task 22).
//!
//! Each operation is one run of the runner cycle (`runner.rs`): the class is generated from the
//! constant `TRAN` template and one encoded payload, and the run answers with a nonce-bound report.
//! Rust decides the status, never the class: a `create` is `ACTIVE` only when the run AND a new
//! session agree (`runner_evidence.rs`), and a run whose reply is lost, cut, or broken is settled
//! read-only from a new session (`runner_reconcile.rs`) and never resent.
//!
//! Differences from the Python tool, on purpose: both operations create and delete a temporary
//! class in `$TMP` (Python calls one function module), both stop before a connection on a value the
//! payload refuses, and a simulate prints the guards of the create as constant lines because the
//! `TRAN` template emits none.

use crate::app::{Exit, Factories, Opened, stop};
use crate::backend::{BackendKind, Failure, Selection};
use crate::config::Connection;
use crate::domain::{Outcome, Status};
use crate::hash::source_hash;
use crate::object::DEFAULT_PACKAGE;
use crate::operation::deploy::CancelFlag;
use crate::operation::payload::{Kind, Mode, Request, encode};
use crate::operation::read::emit_lines;
use crate::operation::runner::{CycleInput, PostRoute, RunnerEnd, RunnerRun, run_cycle};
use crate::operation::runner_evidence::{Expect, Proof, confirm, judge, read_evidence};
use crate::operation::runner_reconcile::{
    Clock, Intent, ReadFacts, ReadRun, Subject, SystemClock, read_facts, reconcile, settle,
};
use crate::operation::runner_route::{Routed, random_nonce, route};
use crate::operation::runner_template::{TRAN_TEMPLATE, generate};
use crate::operation::simulate_ddic::{CHECK_NONCE, texts};
use crate::runner_protocol::{Level, Nonce, Parsed, Report, RunMode, RunStatus, parse_post_reply};
use std::path::Path;

/// A transaction is six lines: `TRAN`, `program`, `dynpro`, `cinfo`, `text`, `package`.
const TRAN_LINES: usize = 6;
/// The guards of a `TRAN` create, in the order the class applies them. The template emits no
/// `GUARD` fact, so these are the constant text of what `tran_create` and the core check.
const GUARDS: [&str; 4] = [
    "Customer names only (Z or Y)",
    "System change option, client role, and customer namespace",
    "Request owner and state, and the package of the object",
    "A name not held in TSTC or TADIR, and an executable customer report",
];

/// `read TRAN <TCODE> <OUT>`.
pub struct TranRead<'a> {
    pub name: &'a str,
    pub out: &'a Path,
}

/// `create [--simulate] TRAN <TCODE> --prog <REPORT> --text <TEXT> [--package P] [--tr TR]`. The
/// package and request rules are checked by `runner_cli::check_create` before this runs.
pub struct TranCreate<'a> {
    pub name: &'a str,
    pub report: &'a str,
    pub text: Option<&'a str>,
    pub package: Option<&'a str>,
    pub tr: Option<&'a str>,
    pub simulate: bool,
}

pub fn read(
    connection: &Connection,
    explicit: Option<BackendKind>,
    read: &TranRead<'_>,
    factories: &mut dyn Factories,
) -> Exit {
    read_with(
        connection,
        explicit,
        read,
        factories,
        &mut random_nonce,
        &mut SystemClock::new(),
    )
}

pub fn create(
    connection: &Connection,
    explicit: Option<BackendKind>,
    create: &TranCreate<'_>,
    factories: &mut dyn Factories,
) -> Exit {
    create_with(
        connection,
        explicit,
        create,
        factories,
        &mut random_nonce,
        &mut SystemClock::new(),
    )
}

/// [`read`] over any nonce source and clock, so that tests script the run.
pub fn read_with(
    connection: &Connection,
    explicit: Option<BackendKind>,
    read: &TranRead<'_>,
    factories: &mut dyn Factories,
    nonce: &mut dyn FnMut() -> String,
    clock: &mut dyn Clock,
) -> Exit {
    let spec = Spec::of_read(read);

    if let Err(message) = payload(Mode::Read, &spec, CHECK_NONCE) {
        return stop(message);
    }

    route(
        "read TRAN",
        connection,
        explicit,
        factories,
        |selection, open, cancel| {
            let mut runtime = Runtime {
                cycler: Cycler {
                    connection,
                    backend: selection.backend,
                    open,
                    nonce: &mut *nonce,
                },
                clock: &mut *clock,
                cancel,
            };

            attempt_read(&mut runtime, read, &spec).routed()
        },
    )
}

/// [`create`] over any nonce source and clock.
pub fn create_with(
    connection: &Connection,
    explicit: Option<BackendKind>,
    create: &TranCreate<'_>,
    factories: &mut dyn Factories,
    nonce: &mut dyn FnMut() -> String,
    clock: &mut dyn Clock,
) -> Exit {
    let spec = Spec::of_create(create);

    if let Err(message) = payload(create_mode(create), &spec, CHECK_NONCE) {
        return stop(message);
    }

    route(
        "create TRAN",
        connection,
        explicit,
        factories,
        |selection, open, cancel| {
            let mut runtime = Runtime {
                cycler: Cycler {
                    connection,
                    backend: selection.backend,
                    open,
                    nonce: &mut *nonce,
                },
                clock: &mut *clock,
                cancel,
            };

            attempt_create(&mut runtime, selection, create, &spec).routed()
        },
    )
}

fn create_mode(create: &TranCreate<'_>) -> Mode {
    if create.simulate {
        Mode::CreateSim
    } else {
        Mode::Create
    }
}

// --- the payload -------------------------------------------------------------------------------

/// The values of one operation; the payload encoder validates them and refuses a field that the
/// mode does not take.
#[derive(Clone, Copy)]
struct Spec<'a> {
    name: &'a str,
    report: Option<&'a str>,
    text: Option<&'a str>,
    package: Option<&'a str>,
    tr: Option<&'a str>,
}

impl<'a> Spec<'a> {
    fn of_read(read: &TranRead<'a>) -> Self {
        Self {
            name: read.name,
            report: None,
            text: None,
            package: None,
            tr: None,
        }
    }

    fn of_create(create: &TranCreate<'a>) -> Self {
        Self {
            name: create.name,
            report: Some(create.report),
            text: create.text,
            package: create.package,
            tr: create.tr,
        }
    }
}

fn payload(mode: Mode, spec: &Spec<'_>, nonce: &str) -> Result<Vec<String>, String> {
    let mut request = Request::new(mode, Kind::Tran, spec.name, nonce);

    if matches!(mode, Mode::CreateSim | Mode::Create) {
        request.report = spec.report;
        request.text = spec.text;
        request.package = spec.package;
        request.tr = spec.tr;
    }

    encode(&request)
}

fn run_mode(mode: Mode) -> RunMode {
    match mode {
        Mode::Read => RunMode::Read,
        Mode::Simulate => RunMode::Simulate,
        Mode::Deploy => RunMode::Deploy,
        Mode::CreateSim => RunMode::CreateSim,
        Mode::Create => RunMode::Create,
        Mode::DeleteSim => RunMode::DeleteSim,
        Mode::Delete => RunMode::Delete,
    }
}

// --- one cycle ---------------------------------------------------------------------------------

/// Everything a run of the cycle needs except the clock of the reconciliation, so that the extra
/// `READ` run of a reconciliation can borrow it while the clock is in use.
struct Cycler<'a> {
    connection: &'a Connection,
    backend: BackendKind,
    open: &'a dyn Fn() -> Opened,
    nonce: &'a mut dyn FnMut() -> String,
}

impl Cycler<'_> {
    /// One run: a fresh nonce, the payload, the class, the POST, the cleanup. An `Err` is a
    /// local refusal (exit 2) that happened before any connection was used.
    fn run(
        &mut self,
        operation: &str,
        mode: Mode,
        spec: &Spec<'_>,
        cancel: &CancelFlag,
    ) -> Result<(RunnerRun, Nonce), String> {
        let text = (self.nonce)();
        let nonce = Nonce::parse(&text)?;
        let lines = payload(mode, spec, &text)?;
        let build = |runner: &str| generate(&TRAN_TEMPLATE, runner, &lines);
        let connection = self.connection;
        let input = CycleInput {
            operation,
            sid: &connection.system_id,
            client: &connection.client,
            conn: &connection.id,
            prefix: &connection.runner_prefix,
            user: &connection.user,
            language: &connection.lang,
            backend: self.backend,
            rfc_available: connection.rfc.is_some(),
            build: &build,
        };
        let open = self.open;
        let mut opener = || open();
        let mut run = run_cycle(&input, &mut opener, PostRoute::Cycle, cancel);

        settle(&mut run, &nonce, run_mode(mode));

        Ok((run, nonce))
    }
}

struct Runtime<'a> {
    cycler: Cycler<'a>,
    clock: &'a mut dyn Clock,
    cancel: &'a CancelFlag,
}

/// How the cycle of a run ended, seen from the operation.
enum Stage {
    /// A whole, well formed run.
    Report(Report),
    /// A stage before the POST failed or the POST was refused: nothing ran.
    Rejected(String),
    /// Nothing was sent: another backend may still take the operation.
    NotStarted(String, Failure),
    Stop(String),
    Interrupted,
    /// The reply is lost, cut, or broken: reconcile, never resend.
    Uncertain,
}

fn stage(run: &RunnerRun, nonce: &Nonce, mode: RunMode) -> Stage {
    if run.uncertain {
        return Stage::Uncertain;
    }

    match &run.end {
        RunnerEnd::Posted(response) => {
            let body = String::from_utf8_lossy(&response.body);

            match parse_post_reply(nonce, mode, response.status, &body) {
                Parsed::Complete(report) => Stage::Report(report),
                Parsed::NotRun(status) => {
                    Stage::Rejected(format!("runner POST failed: HTTP {status}"))
                }
                Parsed::Uncertain { .. } => Stage::Uncertain,
            }
        }
        RunnerEnd::PostUncertain(_) => Stage::Uncertain,
        RunnerEnd::Rejected(text) => Stage::Rejected(text.clone()),
        RunnerEnd::NotStarted(error) => Stage::NotStarted(error.to_string(), Failure::from(error)),
        RunnerEnd::Stop(text) => Stage::Stop(text.clone()),
        RunnerEnd::Interrupted => Stage::Interrupted,
    }
}

// --- the result of an attempt ------------------------------------------------------------------

/// One attempt on one backend, and whether the router may try the other one.
struct Done {
    outcome: Outcome,
    /// The failure and its text, when nothing was sent and the other backend may still run.
    fallback: Option<(Failure, String)>,
}

impl Done {
    fn routed(self) -> Routed {
        Routed {
            code: self.outcome.exit,
            stdout: self.outcome.stdout,
            stderr: vec![self.outcome.stderr],
            fallback: self.fallback,
        }
    }
}

/// The lines of an attempt while it is built.
struct Print {
    stdout: Vec<String>,
    stderr: Vec<String>,
}

impl Print {
    /// The lines that come before the verdict: the selection of a create, then the runner and the
    /// repository preflight.
    fn start(prefix: Vec<String>, run: &RunnerRun) -> Self {
        let mut stdout = prefix;

        stdout.extend(run.runner_line());
        stdout.extend(run.preflight.clone());

        Self {
            stdout,
            stderr: Vec::new(),
        }
    }

    /// The exit follows the run: a signal wins, then a class that stays, then `posted_exit`. The
    /// notes of the run come first on the stderr and the cleanup commands last.
    fn finish(self, run: &RunnerRun, status: Option<Status>, posted_exit: i32) -> Done {
        let mut stderr = run.notes.clone();

        stderr.extend(self.stderr);
        stderr.extend(run.leftover_text());

        Done {
            outcome: Outcome {
                status,
                exit: run.exit_code(posted_exit),
                stdout: self.stdout.join("\n"),
                stderr: stderr.join("\n"),
            },
            fallback: None,
        }
    }

    /// An end that is no verdict: the stage of the cycle that stopped the operation.
    fn ended(mut self, run: &RunnerRun, stage: Stage, verdict: &str) -> Done {
        match stage {
            Stage::Stop(text) => self.stderr.push(format!("STOP: {text}")),
            Stage::Interrupted => self
                .stderr
                .push("CANCELED: interrupted before the classrun POST; nothing ran".to_owned()),
            Stage::Rejected(text) => {
                if verdict.is_empty() {
                    self.stderr.push(format!("ERROR: {text}"));
                } else {
                    self.stdout.push(verdict.to_owned());
                    self.stdout.push(text);
                }

                return self.finish(run, Some(Status::Rejected), 1);
            }
            Stage::NotStarted(text, failure) => {
                self.stderr.push(format!("ERROR: {text}"));

                let mut done = self.finish(run, None, 1);

                done.fallback = (failure == Failure::PreSendConnectivity
                    && run.leftover.is_none()
                    && !run.interrupted)
                    .then_some((failure, text));

                return done;
            }
            Stage::Report(_) | Stage::Uncertain => {
                unreachable!("a report and an uncertain status are handled by the operation")
            }
        }

        self.finish(run, None, 1)
    }
}

/// The messages of a run: errors are the reason of a verdict, warnings go to the stderr, and
/// information goes to the stdout.
fn side_messages(report: &Report, print: &mut Print) {
    print.stdout.extend(texts(report, Level::Info));
    print.stderr.extend(
        texts(report, Level::Warning)
            .into_iter()
            .map(|text| format!("W: {text}")),
    );
    print.stderr.extend(report.warnings.clone());
}

// --- read --------------------------------------------------------------------------------------

fn attempt_read(runtime: &mut Runtime<'_>, read: &TranRead<'_>, spec: &Spec<'_>) -> Done {
    let (run, nonce) = match runtime
        .cycler
        .run("read TRAN", Mode::Read, spec, runtime.cancel)
    {
        Ok(result) => result,
        Err(message) => return refusal(message),
    };
    let mut print = Print::start(Vec::new(), &run);

    match stage(&run, &nonce, RunMode::Read) {
        Stage::Report(report) => {
            side_messages(&report, &mut print);

            match report.status {
                RunStatus::Ok => match tran_lines(&report, read.name) {
                    Ok(lines) => match emit_lines(&lines, None, read.out, None) {
                        Ok(written) => {
                            print.stdout.push(written.stdout);

                            print.finish(&run, Some(Status::Active), 0)
                        }
                        Err(error) => {
                            print.stderr.push(format!("ERROR: {}", error.message));

                            print.finish(&run, None, 1)
                        }
                    },
                    Err(message) => {
                        print.stderr.push(format!("ERROR: {message}"));

                        print.finish(&run, None, 1)
                    }
                },
                _ => {
                    print.stderr.push(format!(
                        "ERROR: {}",
                        reason(&report, &format!("TRAN {} was not read", read.name))
                    ));

                    print.finish(&run, None, 1)
                }
            }
        }
        Stage::Uncertain => {
            // A read writes nothing in SAP, so there is nothing to reconcile.
            print
                .stderr
                .push("ERROR: the read status is unknown; nothing was written to SAP".to_owned());

            print.finish(&run, None, 1)
        }
        other => print.ended(&run, other, ""),
    }
}

/// The six lines of the transaction. The file receives SAP data, so a line with a control
/// character, which could forge another line, is refused.
fn tran_lines(report: &Report, name: &str) -> Result<Vec<String>, String> {
    let lines: Vec<String> = report
        .lines
        .iter()
        .map(|line| line.as_str().to_owned())
        .collect();

    if lines.len() != TRAN_LINES {
        return Err(format!(
            "the runner returned {} lines for the transaction; expected {TRAN_LINES}",
            lines.len()
        ));
    }

    if lines.iter().any(|line| line.chars().any(char::is_control)) {
        return Err("the runner returned a line with a control character".to_owned());
    }

    if lines[0] != format!("TRAN {}", name.to_ascii_uppercase()) {
        return Err("the runner returned another transaction".to_owned());
    }

    Ok(lines)
}

// --- create ------------------------------------------------------------------------------------

fn attempt_create(
    runtime: &mut Runtime<'_>,
    selection: &Selection,
    create: &TranCreate<'_>,
    spec: &Spec<'_>,
) -> Done {
    let mode = create_mode(create);
    let (run, nonce) = match runtime
        .cycler
        .run("create TRAN", mode, spec, runtime.cancel)
    {
        Ok(result) => result,
        Err(message) => return refusal(message),
    };
    let selected = vec![
        format!("backend: {}", selection.backend.token()),
        format!("reason: {}", selection.reason),
    ];
    let mut print = Print::start(selected, &run);

    match stage(&run, &nonce, run_mode(mode)) {
        Stage::Report(report) => create_report(runtime, &run, &report, create, print),
        Stage::Uncertain if create.simulate => {
            // A simulate writes nothing in SAP, so there is nothing to reconcile.
            print.stderr.push(
                "ERROR: the simulate status is unknown; nothing was written to SAP".to_owned(),
            );

            print.finish(&run, None, 1)
        }
        Stage::Uncertain => reconcile_create(runtime, &run, create, spec, print),
        other => print.ended(&run, other, "CREATE: REJECTED"),
    }
}

fn refusal(message: String) -> Done {
    Done {
        outcome: Outcome {
            status: None,
            exit: 2,
            stdout: String::new(),
            stderr: format!("STOP: {message}"),
        },
        fallback: None,
    }
}

/// The reason of a verdict: the errors of the run, or `fallback` when it gave none.
fn reason(report: &Report, fallback: &str) -> String {
    let errors = texts(report, Level::Error);

    if errors.is_empty() {
        fallback.to_owned()
    } else {
        errors.join("; ")
    }
}

fn create_report(
    runtime: &mut Runtime<'_>,
    run: &RunnerRun,
    report: &Report,
    create: &TranCreate<'_>,
    mut print: Print,
) -> Done {
    side_messages(report, &mut print);

    let name = create.name.to_ascii_uppercase();
    let package = create
        .package
        .map_or_else(|| DEFAULT_PACKAGE.to_owned(), str::to_ascii_uppercase);
    let program = create.report.to_ascii_uppercase();
    let token = |token: &str| format!("CREATE: {token}");
    let (status, line, detail) = match report.status {
        RunStatus::SimulatedOk => {
            print.stdout.push(match create.tr {
                Some(request) => format!(
                    "WILL CREATE TRAN {name} in package {package} (request {})",
                    request.to_ascii_uppercase()
                ),
                None => format!("WILL CREATE TRAN {name} in package {package}"),
            });
            print
                .stdout
                .extend(GUARDS.iter().map(|guard| format!("GUARD {guard}")));

            (
                Status::SimulatedOk,
                "CREATE_SIM: SIMULATED_OK".to_owned(),
                "validated; nothing was written".to_owned(),
            )
        }
        RunStatus::Active => {
            let (status, line, detail) = confirm_active(runtime, run, &name, &package, &program);

            (status, line, detail)
        }
        RunStatus::Inactive => (
            Status::Inactive,
            token("INACTIVE"),
            reason(report, "the transaction is incomplete"),
        ),
        RunStatus::Partial => (
            Status::Partial,
            token("PARTIAL"),
            reason(report, "the create ended partially"),
        ),
        _ => (
            Status::Rejected,
            token("REJECTED"),
            reason(report, "the create was rejected"),
        ),
    };

    print.stdout.push(line);
    print.stdout.push(detail);

    print.finish(run, Some(status), status.exit())
}

/// `ACTIVE` only when the run reported it AND a new session finds the transaction in TSTC with the
/// report and in TADIR with the package. The runner class that stays (reported on its own) does
/// not count against the transaction.
fn confirm_active(
    runtime: &mut Runtime<'_>,
    run: &RunnerRun,
    name: &str,
    package: &str,
    program: &str,
) -> (Status, String, String) {
    let runner = run.name.clone().unwrap_or_default();
    let open = runtime.cycler.open;
    let mut opener = || open();
    let mut evidence = read_evidence(&mut opener, Kind::Tran, name, &runner);

    if run.leftover.is_some() {
        evidence.runner = Ok(None);
    }

    let proof = judge(
        &Expect::Present {
            package: Some(package),
            report: Some(program),
        },
        Kind::Tran,
        &evidence,
    );

    if confirm(true, &proof) == Status::Active {
        return (
            Status::Active,
            "CREATE: ACTIVE".to_owned(),
            format!("TRAN {name} created (program {program}, package {package})"),
        );
    }

    let disagreement = match proof {
        Proof::Mismatch(text) => format!("a new session disagrees ({text})"),
        Proof::Unreadable(text) => format!("a new session could not confirm it ({text})"),
        Proof::Matches => "a new session did not confirm it".to_owned(),
    };

    (
        Status::Partial,
        "CREATE: PARTIAL".to_owned(),
        format!("the run reported ACTIVE but {disagreement}; check with read before retrying"),
    )
}

/// The POST of the create was sent and its reply cannot be believed: settle from a new session,
/// with at most one extra `READ` run, and print the reconciled verdict.
fn reconcile_create(
    runtime: &mut Runtime<'_>,
    run: &RunnerRun,
    create: &TranCreate<'_>,
    spec: &Spec<'_>,
    mut print: Print,
) -> Done {
    let name = create.name.to_ascii_uppercase();
    let runner = run.name.clone().unwrap_or_default();
    let subject = Subject {
        kind: Kind::Tran,
        name: &name,
        runner: &runner,
    };
    let read_spec = Spec {
        name: spec.name,
        report: None,
        text: None,
        package: None,
        tr: None,
    };
    let open = runtime.cycler.open;
    let cycler = &mut runtime.cycler;
    let mut open_session = || open();
    let mut read_run = |fresh: &CancelFlag| -> ReadRun {
        match cycler.run("read TRAN", Mode::Read, &read_spec, fresh) {
            Ok((read, nonce)) => ReadRun {
                facts: tran_read_facts(&read.end, &nonce),
                runner_line: read.runner_line(),
                leftover: read.leftover.clone(),
                leftover_text: read.leftover_text(),
                notes: read.notes.clone(),
            },
            Err(message) => ReadRun {
                facts: Err(message),
                runner_line: None,
                leftover: None,
                leftover_text: None,
                notes: Vec::new(),
            },
        }
    };
    let settled = reconcile(
        &Intent::Create,
        &subject,
        &mut open_session,
        &mut read_run,
        &mut *runtime.clock,
        runtime.cancel,
    );

    print.stdout.push(settled.verdict.line());

    if let Some(extra) = &settled.read_run {
        print.stdout.extend(extra.runner_line.clone());
        print.stderr.extend(extra.notes.clone());
        print.stderr.extend(extra.leftover_text.clone());
    }

    print.stderr.extend(settled.notes.clone());

    let exit = settled.exit_code(run.interrupted);

    print.finish(run, Some(settled.verdict.status()), exit)
}

/// What the extra `READ` run saw. The `TRAN` template reports the six lines and no `HASH` fact,
/// so the hash of a found transaction is the hash of those lines.
fn tran_read_facts(end: &RunnerEnd, nonce: &Nonce) -> Result<ReadFacts, String> {
    if let RunnerEnd::Posted(response) = end
        && let Parsed::Complete(report) = parse_post_reply(
            nonce,
            RunMode::Read,
            response.status,
            &String::from_utf8_lossy(&response.body),
        )
        && report.status == RunStatus::Ok
    {
        let lines: Vec<String> = report
            .lines
            .iter()
            .map(|line| line.as_str().to_owned())
            .collect();

        return Ok(ReadFacts::Found {
            hash: source_hash(&lines),
        });
    }

    read_facts(end, nonce)
}
