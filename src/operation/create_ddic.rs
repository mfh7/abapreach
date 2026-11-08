//! `create` of a `DTEL`, `DOMA`, `TABL`, or `TTYP` through the temporary runner class (M6
//! Task 21).
//!
//! One run of the runner cycle (`runner.rs`) in mode `CREATE`. The class checks the package and
//! the request, records the object in the package and the request (`RS_CORR_INSERT`, class
//! `DICT`, global lock, no dialog), writes the definition, activates it, and proves it. The local
//! rules (the default package `$TMP`, `--tr` only for a transportable package, the FILE, the
//! options of other types) are the ones of `runner_cli::check_create`, shared with `TRAN`; the
//! payload refuses every value and a `--text` that differs from the XML before any connection.
//! `create --simulate` is the `CREATE_SIM` run of Task 19 (`simulate_ddic.rs`).
//!
//! The status is decided here, not by the class. `CREATE: ACTIVE` needs the run to say so (with
//! the hash of the written definition) AND a NEW session to find the definition active, in the
//! package of the command, with no inactive version left. A run that ends `INACTIVE` leaves the
//! inactive version without a rollback. A run whose reply is lost, cut, or broken is settled
//! read-only from a new session (`runner_reconcile.rs`) and is never resent.
//!
//! Transportable creation is supported (live fact U4: the class registers the object itself), so
//! there is no UNSUPPORTED path.
//!
//! Differences from the Python tool, on purpose: the create creates and deletes a temporary class
//! in `$TMP` and prints the `RUNNER:` line, and the XML travels inside the payload of that class.

use crate::app::{Exit, Factories, stop};
use crate::backend::BackendKind;
use crate::config::Connection;
use crate::object::DEFAULT_PACKAGE;
use crate::operation::deploy::CancelFlag;
use crate::operation::deploy_ddic::{disagreement, fact};
use crate::operation::payload::{Kind, Mode, Request, encode};
use crate::operation::read_ddic::template_of;
use crate::operation::runner::RunnerRun;
use crate::operation::runner_reconcile::{
    Clock, Intent, ReadRun, Subject, SystemClock, reconcile, settle,
};
use crate::operation::runner_route::{random_nonce, route};
use crate::operation::simulate_ddic::{
    Attempt, CHECK_NONCE, DdicSimulate, Done, Print, Simulation, Stage, reason, refusal, run_lines,
    stage, texts, warnings,
};
use crate::runner_protocol::{Level, Nonce, Report, RunMode, RunStatus};

/// `create <TYPE> <NAME> <FILE>` of a DDIC type. The name and option rules are checked by
/// `runner_cli` before this runs.
pub struct DdicCreate<'a> {
    pub kind: Kind,
    pub name: &'a str,
    /// The text of the XML file.
    pub xml: &'a str,
    pub package: Option<&'a str>,
    pub text: Option<&'a str>,
    pub tr: Option<&'a str>,
}

pub fn create(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicCreate<'_>,
    factories: &mut dyn Factories,
) -> Exit {
    create_with(
        connection,
        explicit,
        request,
        factories,
        &mut random_nonce,
        &mut SystemClock::new(),
    )
}

/// [`create`] over any nonce source and clock, so that tests script the run.
pub fn create_with(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicCreate<'_>,
    factories: &mut dyn Factories,
    nonce: &mut dyn FnMut() -> String,
    clock: &mut dyn Clock,
) -> Exit {
    let name = request.name.to_ascii_uppercase();
    let Some(template) = template_of(request.kind) else {
        return stop(format!("{} is not a DDIC type", request.kind.token()));
    };

    // The payload refuses a value, and a `--text` that differs from the XML, before any connection.
    if let Err(message) = payload(request, &name, CHECK_NONCE) {
        return stop(message);
    }

    // The part of a simulate request that the shared helpers read: the kind, the XML, the request,
    // and how a rejection is printed.
    let shape = DdicSimulate {
        kind: request.kind,
        name: &name,
        xml: request.xml,
        tr: request.tr,
        simulation: Simulation::Create {
            package: request.package,
            text: request.text,
        },
    };
    let operation = format!("create {}", request.kind.token());

    route(
        &operation,
        connection,
        explicit,
        factories,
        |selection, open, cancel| {
            let attempt = Attempt {
                connection,
                selection,
                open,
                cancel,
                request: &shape,
                name: &name,
                operation: &operation,
                template,
            };

            attempt_create(&attempt, request, &mut *nonce, &mut *clock).routed()
        },
    )
}

fn payload(request: &DdicCreate<'_>, name: &str, nonce: &str) -> Result<Vec<String>, String> {
    let mut run = Request::new(Mode::Create, request.kind, name, nonce);

    run.xml = Some(request.xml);
    run.package = request.package;
    run.text = request.text;
    run.tr = request.tr;

    encode(&run)
}

/// The payload of the extra `READ` run of a reconciliation.
fn read_payload(request: &DdicCreate<'_>, name: &str, nonce: &str) -> Result<Vec<String>, String> {
    encode(&Request::new(Mode::Read, request.kind, name, nonce))
}

// --- one attempt -------------------------------------------------------------------------------

fn attempt_create(
    attempt: &Attempt<'_>,
    create: &DdicCreate<'_>,
    nonce: &mut dyn FnMut() -> String,
    clock: &mut dyn Clock,
) -> Done {
    let text = nonce();
    let parsed = match Nonce::parse(&text) {
        Ok(nonce) => nonce,
        Err(message) => return refusal(&message),
    };
    let lines = match payload(create, attempt.name, &text) {
        Ok(lines) => lines,
        Err(message) => return refusal(&message),
    };
    let mut run = run_lines(attempt, attempt.operation, &lines, attempt.cancel);

    settle(&mut run, &parsed, RunMode::Create);

    let print = Print::start(attempt.selection, &run);

    match stage(&run, &parsed, RunMode::Create) {
        Stage::Report(report) => created(attempt, create, &run, &report, print),
        Stage::Uncertain(_) => reconcile_create(attempt, create, &run, (nonce, clock), print),
        other => print.ended(&run, other, attempt.request.simulation),
    }
}

/// The report of the run: the status the class gave, checked by a new session for `ACTIVE`.
fn created(
    attempt: &Attempt<'_>,
    create: &DdicCreate<'_>,
    run: &RunnerRun,
    report: &Report,
    mut print: Print,
) -> Done {
    print.stdout.extend(texts(report, Level::Info));
    warnings(report, &mut print);

    let (token, detail) = match report.status {
        RunStatus::Active => active(attempt, create, run, report, &mut print),
        RunStatus::Inactive => (
            "INACTIVE",
            reason(report, "the definition was saved but not activated"),
        ),
        RunStatus::Partial => ("PARTIAL", reason(report, "the create ended partially")),
        _ => ("REJECTED", reason(report, "the create was rejected")),
    };
    let exit = i32::from(token != "ACTIVE");

    print.stdout.push(format!("CREATE: {token}"));
    print.stdout.push(detail);

    print.finish(run, exit)
}

/// `ACTIVE` only when the run reported it with the hash of the written definition AND a new
/// session finds the definition active, in the package of the command, with no inactive version.
fn active(
    attempt: &Attempt<'_>,
    create: &DdicCreate<'_>,
    run: &RunnerRun,
    report: &Report,
    print: &mut Print,
) -> (&'static str, String) {
    let package = create
        .package
        .map_or_else(|| DEFAULT_PACKAGE.to_owned(), str::to_ascii_uppercase);

    if fact(report, "RESULT_HASH").is_none() {
        return (
            "PARTIAL",
            "the run reported ACTIVE without a result hash; check with read before retrying"
                .to_owned(),
        );
    }

    if let Some(why) = disagreement(attempt, run, Some(&package), print) {
        return (
            "PARTIAL",
            format!("the run reported ACTIVE but {why}; check with read before retrying"),
        );
    }

    let request = create.tr.map_or_else(String::new, |request| {
        format!(" (request {})", request.to_ascii_uppercase())
    });

    (
        "ACTIVE",
        format!(
            "{} {} created in package {package}{request}",
            attempt.request.kind.token(),
            attempt.name
        ),
    )
}

// --- an uncertain run --------------------------------------------------------------------------

/// The POST of the create was sent and its reply cannot be believed: settle from a new session,
/// with at most one extra `READ` run, and print the reconciled verdict. The create is never
/// resent.
fn reconcile_create(
    attempt: &Attempt<'_>,
    create: &DdicCreate<'_>,
    run: &RunnerRun,
    (nonce, clock): (&mut dyn FnMut() -> String, &mut dyn Clock),
    mut print: Print,
) -> Done {
    let kind = attempt.request.kind;
    let runner = run.name.clone().unwrap_or_default();
    let subject = Subject {
        kind,
        name: attempt.name,
        runner: &runner,
    };
    let open = attempt.open;
    let mut open_session = || open();
    let mut read_run = |fresh: &CancelFlag| -> ReadRun {
        let text = nonce();

        match (
            Nonce::parse(&text),
            read_payload(create, attempt.name, &text),
        ) {
            (Ok(parsed), Ok(lines)) => {
                let operation = format!("read {}", kind.token());
                let read = run_lines(attempt, &operation, &lines, fresh);

                ReadRun::from_run(&read, &parsed)
            }
            (Err(message), _) | (_, Err(message)) => ReadRun {
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
        clock,
        attempt.cancel,
    );

    print.stdout.push(settled.verdict.line());

    if let Some(extra) = &settled.read_run {
        print.stdout.extend(extra.runner_line.clone());
        print.stderr.extend(extra.notes.clone());
        print.stderr.extend(extra.leftover_text.clone());
    }

    print.stderr.extend(settled.notes.clone());

    let exit = settled.exit_code(run.interrupted);

    print.finish(run, exit)
}
