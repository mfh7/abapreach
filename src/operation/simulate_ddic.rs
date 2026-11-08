//! `deploy --simulate` and `create --simulate` of a `DTEL`, `DOMA`, `TABL`, or `TTYP` through the
//! temporary runner class (M6 Task 19).
//!
//! One run of the runner cycle (`runner.rs`) in mode `SIMULATE` or `CREATE_SIM`. The class parses
//! the XML, checks the inactive version, the package, the request, and the change options, and
//! decides the conversion verdict (`DD_DECIDE`, also for the transparent tables that a data
//! element, a domain, or a structure reaches); it writes no definition. A deploy run answers with
//! the live XML rows and their hash, so this module prints the diff and the live hash and compares
//! `--base` itself (a different base is a STOP after the one run, as in M1). The run for `deploy`
//! is the first run of the two-run deploy of Task 20 and is shared with it.
//!
//! The status is decided here, not by the class. A `SIMULATED_OK` is printed only when the report
//! holds no contradiction (a transparent table without a verdict, a verdict that is not allowed)
//! AND a NEW session finds the object as before the run: still there for a deploy, still absent
//! for a create. A verdict that cannot be computed (row count unavailable) is a `REJECTED` of the
//! run and is never replaced by a claim. A run whose reply is lost, cut, or broken writes nothing
//! that needs settling and is never resent.
//!
//! Differences from the Python tool, on purpose: the simulate creates and deletes a temporary
//! class in `$TMP` and prints the `RUNNER:` line first, the status is the bare token of the M1
//! deploy, a different `--base` is exit 2, `WHERE-USED` says `(transparent)`, and the guards are
//! the lines the class reports.

use crate::app::{Exit, Factories, Opened, stop};
use crate::backend::{BackendKind, Failure, Selection};
use crate::config::Connection;
use crate::hash::{local_source_lines, normalize, source_hash};
use crate::object::DEFAULT_PACKAGE;
use crate::operation::deploy::CancelFlag;
use crate::operation::payload::{Kind, Mode, Request, encode};
use crate::operation::read_ddic::template_of;
use crate::operation::runner::{CycleInput, PostRoute, RunnerEnd, RunnerRun, run_cycle};
use crate::operation::runner_evidence::{Expect, Proof, judge, read_evidence};
use crate::operation::runner_reconcile::{Cause, uncertainty};
use crate::operation::runner_route::{Routed, random_nonce, route};
use crate::operation::runner_template::{TypeTemplate, generate};
use crate::runner_protocol::{Level, Nonce, Parsed, Report, RunMode, RunStatus, parse_post_reply};
use similar::TextDiff;

/// A valid nonce for the checks that run before any connection.
pub(crate) const CHECK_NONCE: &str = "00000000000000000000000000000000";
/// The row of a transparent table in the XML file; the class decides a verdict for such a table.
const TRANSPARENT_ROW: &str = "<TABCLASS>TRANSP</TABCLASS>";

/// Which simulate runs and the values only it takes.
#[derive(Clone, Copy, Debug)]
pub enum Simulation<'a> {
    /// `deploy --simulate`: `--base` is compared with the live hash after the run.
    Deploy { base: Option<&'a str> },
    /// `create --simulate`: the package and the description that must agree with the XML.
    Create {
        package: Option<&'a str>,
        text: Option<&'a str>,
    },
}

impl Simulation<'_> {
    fn mode(self) -> Mode {
        match self {
            Self::Deploy { .. } => Mode::Simulate,
            Self::Create { .. } => Mode::CreateSim,
        }
    }

    fn run_mode(self) -> RunMode {
        match self {
            Self::Deploy { .. } => RunMode::Simulate,
            Self::Create { .. } => RunMode::CreateSim,
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Deploy { .. } => "deploy",
            Self::Create { .. } => "create",
        }
    }
}

/// `deploy --simulate <TYPE> <NAME> <FILE>` or `create --simulate <TYPE> <NAME> <FILE>` of a DDIC
/// type. The name and option rules are checked by `runner_cli` before this runs.
pub struct DdicSimulate<'a> {
    pub kind: Kind,
    pub name: &'a str,
    /// The text of the XML file.
    pub xml: &'a str,
    pub tr: Option<&'a str>,
    pub simulation: Simulation<'a>,
}

pub fn simulate(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicSimulate<'_>,
    factories: &mut dyn Factories,
) -> Exit {
    let name = request.name.to_ascii_uppercase();
    let Some(template) = template_of(request.kind) else {
        return stop(format!("{} is not a DDIC type", request.kind.token()));
    };

    // The payload refuses a value, and a base that is no hash, before any connection is opened.
    if let Err(message) = check(request, &name) {
        return stop(message);
    }

    let operation = format!("{} {}", request.simulation.verb(), request.kind.token());

    route(
        &operation,
        connection,
        explicit,
        factories,
        |selection, open, cancel| {
            run_attempt(
                &Attempt {
                    connection,
                    selection,
                    open,
                    cancel,
                    request,
                    name: &name,
                    operation: &operation,
                    template,
                },
                &mut random_nonce,
            )
            .routed()
        },
    )
}

/// Whether the payload of the first run is accepted: the values are checked before any connection.
pub(crate) fn check(request: &DdicSimulate<'_>, name: &str) -> Result<(), String> {
    payload(request, name, CHECK_NONCE, true).map(drop)
}

/// The payload of one run. The base is validated by the check but never sent: the live hash comes
/// back and is compared here.
fn payload(
    request: &DdicSimulate<'_>,
    name: &str,
    nonce: &str,
    validate_base: bool,
) -> Result<Vec<String>, String> {
    let mut run = Request::new(request.simulation.mode(), request.kind, name, nonce);

    run.xml = Some(request.xml);
    run.tr = request.tr;

    match request.simulation {
        Simulation::Deploy { base } => run.base = base.filter(|_| validate_base),
        Simulation::Create { package, text } => {
            run.package = package;
            run.text = text;
        }
    }

    encode(&run)
}

// --- one attempt -------------------------------------------------------------------------------

/// Everything one attempt on one backend needs.
pub(crate) struct Attempt<'a> {
    pub(crate) connection: &'a Connection,
    pub(crate) selection: &'a Selection,
    pub(crate) open: &'a dyn Fn() -> Opened,
    pub(crate) cancel: &'a CancelFlag,
    pub(crate) request: &'a DdicSimulate<'a>,
    /// The object name, upper case.
    pub(crate) name: &'a str,
    /// `<verb> <TYPE>`, for the STOP texts.
    pub(crate) operation: &'a str,
    pub(crate) template: &'static TypeTemplate,
}

/// One attempt, and whether the router may try the other backend.
pub(crate) struct Done {
    pub(crate) code: i32,
    pub(crate) stdout: Vec<String>,
    pub(crate) stderr: Vec<String>,
    /// Set when nothing was sent and the failure allows the other backend.
    pub(crate) fallback: Option<(Failure, String)>,
}

impl Done {
    pub(crate) fn routed(self) -> Routed {
        Routed {
            code: self.code,
            stdout: self.stdout.join("\n"),
            stderr: self.stderr,
            fallback: self.fallback,
        }
    }
}

fn run_attempt(attempt: &Attempt<'_>, nonce: &mut dyn FnMut() -> String) -> Done {
    match first_run(attempt, nonce) {
        Step::Done(done) => done,
        Step::Verified(verified) => simulated(attempt, verified),
    }
}

/// One run of the class cycle with the payload `lines`: a new class, the POST, the cleanup. The
/// two runs of a deploy and the read run of a reconciliation share it.
pub(crate) fn run_lines(
    attempt: &Attempt<'_>,
    operation: &str,
    lines: &[String],
    cancel: &CancelFlag,
) -> RunnerRun {
    let build = |runner: &str| generate(attempt.template, runner, lines);
    let connection = attempt.connection;
    let input = CycleInput {
        operation,
        sid: &connection.system_id,
        client: &connection.client,
        conn: &connection.id,
        prefix: &connection.runner_prefix,
        user: &connection.user,
        language: &connection.lang,
        backend: attempt.selection.backend,
        rfc_available: connection.rfc.is_some(),
        build: &build,
    };
    let open = attempt.open;
    let mut opener = || open();

    run_cycle(&input, &mut opener, PostRoute::Cycle, cancel)
}

/// How the first run ended for a caller that goes on with a write.
pub(crate) enum Step {
    /// The command is over: a rejection, a stop, a status, or a failure.
    Done(Done),
    /// A deploy run that passed every check; a simulate ends here with `SIMULATED_OK`, a deploy
    /// goes on with a second run.
    Verified(Verified),
}

/// A deploy run that passed the checks, with everything the caller needs to go on.
pub(crate) struct Verified {
    pub(crate) run: RunnerRun,
    /// The lines printed so far: the diff, the live hash, the verdict, and the guards.
    pub(crate) print: Print,
    /// The live hash, which becomes the base of the write.
    pub(crate) base: Option<String>,
}

/// The first run of a simulate or a deploy: the class cycle in mode `SIMULATE` or `CREATE_SIM`,
/// then the report, the `--base` comparison, and the diff. A deploy that returns `Verified` has
/// shown the diff and the live hash and has written nothing.
pub(crate) fn first_run(attempt: &Attempt<'_>, nonce: &mut dyn FnMut() -> String) -> Step {
    let text = nonce();
    let nonce = match Nonce::parse(&text) {
        Ok(nonce) => nonce,
        Err(message) => return Step::Done(refusal(&message)),
    };
    let lines = match payload(attempt.request, attempt.name, &text, false) {
        Ok(lines) => lines,
        Err(message) => return Step::Done(refusal(&message)),
    };
    let run = run_lines(attempt, attempt.operation, &lines, attempt.cancel);
    let mut print = Print::start(attempt.selection, &run);

    match stage(&run, &nonce, attempt.request.simulation.run_mode()) {
        Stage::Report(report) => match report_done(attempt, &run, &report, print) {
            Flow::Done(done) => Step::Done(done),
            Flow::Verified { print, base } => Step::Verified(Verified { run, print, base }),
        },
        Stage::Uncertain(cause) => {
            // A first run writes nothing in SAP, so there is nothing to settle.
            print.stderr.push(format!(
                "ERROR: {} {}: the simulate status is uncertain ({cause}); nothing was written; \
                 run the command again",
                attempt.operation, attempt.name
            ));

            Step::Done(print.finish(&run, 1))
        }
        other => Step::Done(print.ended(&run, other, attempt.request.simulation)),
    }
}

/// The end of a simulate: the new session must find the object as before, then `SIMULATED_OK`.
fn simulated(attempt: &Attempt<'_>, verified: Verified) -> Done {
    let Verified { run, mut print, .. } = verified;
    let expect = Expect::Present {
        package: None,
        report: None,
    };

    match confirm(attempt, &run, &expect, &mut print) {
        Some(why) => partial_deploy(&run, "SIMULATED_OK", &why, print),
        None => {
            print.stdout.push("SIMULATED_OK".to_owned());

            print.finish(&run, 0)
        }
    }
}

pub(crate) fn refusal(message: &str) -> Done {
    Done {
        code: 2,
        stdout: Vec::new(),
        stderr: vec![format!("STOP: {message}")],
        fallback: None,
    }
}

/// How the cycle of the run ended, seen from the simulate.
pub(crate) enum Stage {
    /// A whole, well formed run.
    Report(Box<Report>),
    /// The reply is lost, cut, or broken; the text says why.
    Uncertain(String),
    /// A stage before the POST failed or the POST was refused: nothing ran.
    Rejected(String),
    /// Nothing was sent: another backend may still take the simulate.
    NotStarted(String, Failure),
    Stop(String),
    Interrupted,
}

pub(crate) fn stage(run: &RunnerRun, nonce: &Nonce, mode: RunMode) -> Stage {
    if let Some(cause) = uncertainty(&run.end, nonce, mode) {
        let (Cause::LostReply(text) | Cause::Broken(text)) = cause;

        return Stage::Uncertain(text);
    }

    match &run.end {
        RunnerEnd::Posted(response) => {
            let body = String::from_utf8_lossy(&response.body);

            match parse_post_reply(nonce, mode, response.status, &body) {
                Parsed::Complete(report) => Stage::Report(Box::new(report)),
                Parsed::NotRun(status) => {
                    Stage::Rejected(format!("runner POST failed: HTTP {status}"))
                }
                Parsed::Uncertain { reason, .. } => Stage::Uncertain(reason.to_string()),
            }
        }
        RunnerEnd::PostUncertain(error) => Stage::Uncertain(error.to_string()),
        RunnerEnd::Rejected(text) => Stage::Rejected(text.clone()),
        RunnerEnd::NotStarted(error) => Stage::NotStarted(error.to_string(), Failure::from(error)),
        RunnerEnd::Stop(text) => Stage::Stop(text.clone()),
        RunnerEnd::Interrupted => Stage::Interrupted,
    }
}

/// The lines of an attempt while it is built.
pub(crate) struct Print {
    pub(crate) stdout: Vec<String>,
    pub(crate) stderr: Vec<String>,
}

impl Print {
    /// The backend and why it was chosen, then the runner line and the repository preflight.
    pub(crate) fn start(selection: &Selection, run: &RunnerRun) -> Self {
        let mut stdout = vec![
            format!("backend: {}", selection.backend.token()),
            format!("reason: {}", selection.reason),
        ];

        stdout.extend(run.runner_line());
        stdout.extend(run.preflight.clone());

        Self {
            stdout,
            stderr: Vec::new(),
        }
    }

    /// The exit follows the run: a signal wins, then a class that stays, then `posted_exit`. The
    /// notes of the run come first on the stderr and the cleanup commands last.
    pub(crate) fn finish(self, run: &RunnerRun, posted_exit: i32) -> Done {
        let mut stderr = run.notes.clone();

        stderr.extend(self.stderr);
        stderr.extend(run.leftover_text());

        Done {
            code: run.exit_code(posted_exit),
            stdout: self.stdout,
            stderr,
            fallback: None,
        }
    }

    /// An end that is no report: the stage of the cycle that stopped the simulate.
    pub(crate) fn ended(
        mut self,
        run: &RunnerRun,
        stage: Stage,
        simulation: Simulation<'_>,
    ) -> Done {
        match stage {
            Stage::Stop(text) => self.stderr.push(format!("STOP: {text}")),
            Stage::Interrupted => self
                .stderr
                .push("CANCELED: interrupted before the classrun POST; nothing ran".to_owned()),
            Stage::Rejected(text) => {
                self.rejected(simulation, &text);

                return self.finish(run, 1);
            }
            Stage::NotStarted(text, failure) => {
                self.stderr.push(format!("ERROR: {text}"));

                let mut done = self.finish(run, 1);

                done.fallback = (failure == Failure::PreSendConnectivity
                    && run.leftover.is_none()
                    && !run.interrupted)
                    .then_some((failure, text));

                return done;
            }
            Stage::Report(_) | Stage::Uncertain(_) => {
                unreachable!("a report and an uncertain status are handled by the simulate")
            }
        }

        self.finish(run, 1)
    }

    /// The status line of a rejection: `REJECTED: <reason>` for a deploy, `CREATE: REJECTED` and
    /// the reason for a create.
    fn rejected(&mut self, simulation: Simulation<'_>, reason: &str) {
        match simulation {
            Simulation::Deploy { .. } => self.stdout.push(format!("REJECTED: {reason}")),
            Simulation::Create { .. } => {
                self.stdout.push("CREATE: REJECTED".to_owned());
                self.stdout.push(reason.to_owned());
            }
        }
    }
}

// --- the report --------------------------------------------------------------------------------

pub(crate) fn texts(report: &Report, level: Level) -> Vec<String> {
    report
        .messages
        .iter()
        .filter(|message| message.level == level)
        .map(|message| message.text.clone())
        .collect()
}

/// Warnings go to the stderr; information is printed in place by the status that owns it.
pub(crate) fn warnings(report: &Report, print: &mut Print) {
    print.stderr.extend(
        texts(report, Level::Warning)
            .into_iter()
            .map(|text| format!("W: {text}")),
    );
    print.stderr.extend(report.warnings.clone());
}

/// The reason of a rejection: the errors of the run, or `fallback` when it gave none.
pub(crate) fn reason(report: &Report, fallback: &str) -> String {
    let errors = texts(report, Level::Error);

    if errors.is_empty() {
        fallback.to_owned()
    } else {
        errors.join("; ")
    }
}

/// The guards the class applied, as reported.
fn guard_lines(report: &Report) -> impl Iterator<Item = String> + '_ {
    report
        .facts
        .iter()
        .filter(|fact| fact.key == "GUARD")
        .map(|fact| format!("GUARD {}", fact.value))
}

/// What the report of the first run leads to.
enum Flow {
    Done(Done),
    /// A deploy that passed: the lines so far and the live hash.
    Verified {
        print: Print,
        base: Option<String>,
    },
}

fn report_done(attempt: &Attempt<'_>, run: &RunnerRun, report: &Report, mut print: Print) -> Flow {
    warnings(report, &mut print);

    match attempt.request.simulation {
        Simulation::Deploy { base } => deploy_report(attempt, run, report, base, print),
        Simulation::Create { package, .. } => {
            Flow::Done(create_report(attempt, run, report, package, print))
        }
    }
}

/// What a new session must find after a simulate: the object as before for a deploy, nothing for
/// a create. A class that stays would make the evidence disagree for a reason that is not the
/// simulate, so it is not read then.
fn confirm(
    attempt: &Attempt<'_>,
    run: &RunnerRun,
    expect: &Expect<'_>,
    print: &mut Print,
) -> Option<String> {
    let (None, Some(runner)) = (&run.leftover, &run.name) else {
        return None;
    };
    let open = attempt.open;
    let evidence = read_evidence(&mut || open(), attempt.request.kind, attempt.name, runner);

    print.stderr.extend(evidence.notes.clone());

    match judge(expect, attempt.request.kind, &evidence) {
        Proof::Matches => None,
        Proof::Unreadable(why) => {
            print.stderr.push(format!(
                "W: the new session could not confirm the simulate ({why})"
            ));

            None
        }
        Proof::Mismatch(why) => Some(why),
    }
}

// --- deploy ------------------------------------------------------------------------------------

/// The live definition of a deploy run: the XML rows after the client rule and their hash.
struct Live {
    rows: Vec<String>,
    hash: String,
}

/// The rows and the hash of the live definition. A run that rejected before it read the definition
/// has neither. The rows are data from a remote system: they must be deployable XML, and their
/// hash must be the hash the run reported.
fn live_of(report: &Report) -> Result<Option<Live>, String> {
    let reported = report
        .facts
        .iter()
        .find(|fact| fact.key == "HASH")
        .map(|fact| fact.value.as_str());
    let lines: Vec<String> = report
        .lines
        .iter()
        .map(|line| line.as_str().to_owned())
        .collect();

    if lines.is_empty() {
        // A row wider than the protocol allows ends the run after the hash; no rows come back.
        return match (reported, report.status) {
            (None, _) | (Some(_), RunStatus::Rejected) => Ok(None),
            (Some(_), _) => Err("the runner returned a hash without XML rows".to_owned()),
        };
    }

    if let Some(index) = lines
        .iter()
        .position(|line| line.chars().any(|c| c.is_control() && c != '\t'))
    {
        return Err(format!(
            "XML row {} of the live definition has a control character",
            index + 1
        ));
    }

    let rows = normalize(lines);
    let hash = source_hash(&rows);

    if !reported.is_some_and(|value| value.eq_ignore_ascii_case(&hash)) {
        return Err(
            "the client hash differs from the runner hash (normalization rules out of sync)"
                .to_owned(),
        );
    }

    Ok(Some(Live { rows, hash }))
}

fn join(lines: &[String]) -> String {
    let mut text = lines.join("\n");

    if !text.is_empty() {
        text.push('\n');
    }

    text
}

fn deploy_report(
    attempt: &Attempt<'_>,
    run: &RunnerRun,
    report: &Report,
    base: Option<&str>,
    mut print: Print,
) -> Flow {
    let live = match live_of(report) {
        Ok(live) => live,
        Err(message) => {
            print.stderr.push(format!(
                "ERROR: {} {}: {message}; nothing was written",
                attempt.operation, attempt.name
            ));

            return Flow::Done(print.finish(run, 1));
        }
    };

    // M1: a base that is not the live hash ends the command before anything else is shown.
    if let (Some(base), Some(live)) = (base, &live)
        && !base.eq_ignore_ascii_case(&live.hash)
    {
        print.stderr.push(format!(
            "STOP: live changed: live hash {} != --base {base}",
            live.hash
        ));

        return Flow::Done(print.finish(run, 2));
    }

    let expect = Expect::Present {
        package: None,
        report: None,
    };

    if report.status == RunStatus::NoChanges {
        return Flow::Done(match confirm(attempt, run, &expect, &mut print) {
            Some(why) => partial_deploy(run, report.status.token(), &why, print),
            None => {
                print.stdout.push("no changes".to_owned());

                print.finish(run, 0)
            }
        });
    }

    let file = normalize(local_source_lines(attempt.request.xml));

    if let Some(live) = &live {
        let diff = TextDiff::from_lines(join(&live.rows), join(&file))
            .unified_diff()
            .context_radius(3)
            .header("live", "file")
            .to_string();

        if !diff.is_empty() {
            print.stdout.push(diff.trim_end().to_owned());
        }

        print.stdout.push(format!("live hash {}", live.hash));
    }

    print.stdout.extend(texts(report, Level::Info));

    let rejection = match report.status {
        RunStatus::SimulatedOk => contradiction(attempt, &file, report),
        _ => Some(reason(report, "the runner rejected the request")),
    };

    if let Some(reason) = rejection {
        print.stdout.push(format!("REJECTED: {reason}"));

        return Flow::Done(print.finish(run, 1));
    }

    print.stdout.extend(guard_lines(report));

    Flow::Verified {
        print,
        base: live.map(|live| live.hash),
    }
}

fn partial_deploy(run: &RunnerRun, reported: &str, why: &str, mut print: Print) -> Done {
    print.stdout.push(format!(
        "PARTIAL: the run reported {reported} but a new session disagrees ({why}); check with \
         read before retrying"
    ));

    print.finish(run, 1)
}

/// A `SIMULATED_OK` that the facts of the report contradict is a rejection: the verdict is never
/// claimed. A transparent table needs a `CONVERSION` line of its own, and no `CONVERSION` line may
/// say that the change is not allowed.
fn contradiction(attempt: &Attempt<'_>, file: &[String], report: &Report) -> Option<String> {
    let conversions: Vec<String> = texts(report, Level::Info)
        .into_iter()
        .filter(|text| text.starts_with("CONVERSION "))
        .collect();

    if let Some(line) = conversions.iter().find(|line| !conversion_allowed(line)) {
        let table = line.split(':').next().unwrap_or(line);

        return Some(format!(
            "the run reported SIMULATED_OK next to a conversion that is not allowed ({table}); \
             the change is rejected"
        ));
    }

    let transparent =
        attempt.request.kind == Kind::Tabl && file.iter().any(|row| row.trim() == TRANSPARENT_ROW);
    let own = format!("CONVERSION {}:", attempt.name);

    (transparent && !conversions.iter().any(|line| line.starts_with(&own))).then(|| {
        format!(
            "the run reported SIMULATED_OK without a conversion verdict for table {}; the \
             change is rejected",
            attempt.name
        )
    })
}

/// Whether a `CONVERSION <name>: ... allowed=<X| > rows=<n>` line allows the change. A line
/// without the field does not.
fn conversion_allowed(line: &str) -> bool {
    line.split_once("allowed=")
        .and_then(|(_, rest)| rest.split(" rows=").next())
        .is_some_and(|flag| flag.trim() == "X")
}

// --- create ------------------------------------------------------------------------------------

fn create_report(
    attempt: &Attempt<'_>,
    run: &RunnerRun,
    report: &Report,
    package: Option<&str>,
    mut print: Print,
) -> Done {
    print.stdout.extend(texts(report, Level::Info));

    if report.status != RunStatus::SimulatedOk {
        print.rejected(
            attempt.request.simulation,
            &reason(report, "the create was rejected"),
        );

        return print.finish(run, 1);
    }

    if let Some(why) = confirm(attempt, run, &Expect::Absent, &mut print) {
        print.stdout.push("CREATE: PARTIAL".to_owned());
        print.stdout.push(format!(
            "the run reported SIMULATED_OK but a new session disagrees ({why}); check with \
             read before retrying"
        ));

        return print.finish(run, 1);
    }

    let package = report
        .facts
        .iter()
        .find(|fact| fact.key == "PACKAGE")
        .map_or_else(
            || package.map_or_else(|| DEFAULT_PACKAGE.to_owned(), str::to_ascii_uppercase),
            |fact| fact.value.clone(),
        );
    let kind = attempt.request.kind.token();

    print.stdout.push(match attempt.request.tr {
        Some(request) => format!(
            "WILL CREATE {kind} {} in package {package} (request {})",
            attempt.name,
            request.to_ascii_uppercase()
        ),
        None => format!("WILL CREATE {kind} {} in package {package}", attempt.name),
    });
    print.stdout.extend(guard_lines(report));
    print.stdout.push("CREATE_SIM: SIMULATED_OK".to_owned());
    print
        .stdout
        .push("validated; nothing was written".to_owned());

    print.finish(run, 0)
}
