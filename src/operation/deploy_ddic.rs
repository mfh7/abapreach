//! `deploy` of a `DTEL`, `DOMA`, `TABL`, or `TTYP` as two runs of the temporary runner class (M6
//! Task 20).
//!
//! Run 1 is the simulate of Task 19 (`simulate_ddic.rs`): mode `SIMULATE` returns the live XML
//! rows, the live hash, and the conversion verdict, and writes nothing. This module then holds
//! the command until the diff (live XML against the file) and the live hash are in the output,
//! so a write never comes before them. A `--base` that differs from the live hash is a STOP after
//! run 1, without a second run; without `--base` the live hash becomes the base; a file equal to
//! the live definition is `no changes` after run 1.
//!
//! Run 2 is mode `DEPLOY` with a new class and a new nonce. It carries the base, takes the DDIC
//! lock, reads the live hash again under it, runs the conversion verdict once more, and only then
//! writes. A base that moved between the runs is a STOP with exit 2 and nothing is written.
//!
//! The status is decided here, not by the class. `ACTIVE` needs the run to say so (with the hash
//! of the written definition) AND a NEW session to find the definition active, in the package the
//! run reported, with no inactive version left. A run that ends `INACTIVE` leaves the inactive
//! version without a rollback; the way back is a new deploy from the file that `read` wrote. A run
//! whose reply is lost, cut, or broken is settled read-only from a new session
//! (`runner_reconcile.rs`) and is never resent.
//!
//! Differences from the Python tool, on purpose: the deploy creates and deletes two temporary
//! classes in `$TMP`, one per run, and prints a `RUNNER:` line for each; the status is the bare
//! token of the M1 deploy (`ACTIVE`), not `DEPLOY: ACTIVE hash <h>`; and the warning that the
//! active definition differs from the file goes to the stderr.

use crate::app::{Exit, Factories};
use crate::backend::BackendKind;
use crate::config::Connection;
use crate::hash::{local_source_lines, normalize, source_hash};
use crate::operation::deploy::CancelFlag;
use crate::operation::payload::{Kind, Mode, Request, encode};
use crate::operation::read_ddic::template_of;
use crate::operation::runner::RunnerRun;
use crate::operation::runner_evidence::{Expect, Proof, judge, read_evidence, read_inactive};
use crate::operation::runner_reconcile::{
    Clock, Intent, ReadRun, Subject, SystemClock, reconcile, settle,
};
use crate::operation::runner_route::{random_nonce, route};
use crate::operation::simulate_ddic::{
    Attempt, DdicSimulate, Done, Print, Simulation, Stage, Step, Verified, check, first_run,
    reason, refusal, run_lines, stage, texts, warnings,
};
use crate::runner_protocol::{Level, Nonce, Report, RunMode, RunStatus};

/// A valid nonce and base for the checks that run before any connection.
const CHECK_NONCE: &str = "00000000000000000000000000000000";
const CHECK_BASE: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// The text of the class when the live hash moved between the two runs (the template's text).
const BASE_CHANGED: &str = "Base changed since the diff was shown; run deploy again";
const DIFFERS_FROM_FILE: &str = "W: active definition differs from the file (SAP canonical); run read \
                                 again before the next edit";

/// `deploy <TYPE> <NAME> <FILE>` of a DDIC type. The name and option rules are checked by
/// `runner_cli` before this runs.
pub struct DdicDeploy<'a> {
    pub kind: Kind,
    pub name: &'a str,
    /// The text of the XML file.
    pub xml: &'a str,
    pub tr: Option<&'a str>,
    pub base: Option<&'a str>,
}

pub fn deploy(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicDeploy<'_>,
    factories: &mut dyn Factories,
) -> Exit {
    deploy_gated(connection, explicit, request, factories, &mut |_| Ok(()))
}

/// What run 1 of a deploy found, handed to the gate before run 2 starts.
pub struct Preview<'a> {
    /// The runner class of run 1.
    pub runner: Option<&'a str>,
    /// The lines printed so far, one entry per printed block (a diff is one entry).
    pub printed: &'a [String],
}

/// Asked once run 1 has shown the diff and the live hash and before run 2 writes anything. An
/// `Err` ends the command with exit 1 and that text; run 2 never starts.
pub type Gate<'a> = &'a mut dyn FnMut(&Preview<'_>) -> Result<(), String>;

/// [`deploy`] with a gate between the two runs: the MCP deploy asks its human there.
pub fn deploy_gated(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicDeploy<'_>,
    factories: &mut dyn Factories,
    gate: Gate<'_>,
) -> Exit {
    deploy_gated_with(
        connection,
        explicit,
        request,
        factories,
        (&mut random_nonce, &mut SystemClock::new()),
        gate,
    )
}

/// [`deploy`] over any nonce source and clock, so that tests script the runs.
pub fn deploy_with(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicDeploy<'_>,
    factories: &mut dyn Factories,
    nonce: &mut dyn FnMut() -> String,
    clock: &mut dyn Clock,
) -> Exit {
    deploy_gated_with(
        connection,
        explicit,
        request,
        factories,
        (nonce, clock),
        &mut |_| Ok(()),
    )
}

fn deploy_gated_with(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicDeploy<'_>,
    factories: &mut dyn Factories,
    (nonce, clock): (&mut dyn FnMut() -> String, &mut dyn Clock),
    gate: Gate<'_>,
) -> Exit {
    let name = request.name.to_ascii_uppercase();
    let Some(template) = template_of(request.kind) else {
        return stop(format!("{} is not a DDIC type", request.kind.token()));
    };
    let first = DdicSimulate {
        kind: request.kind,
        name: &name,
        xml: request.xml,
        tr: request.tr,
        simulation: Simulation::Deploy { base: request.base },
    };

    // Both payloads refuse a value, and a base that is no hash, before any connection is opened.
    if let Err(message) = check(&first, &name) {
        return stop(message);
    }

    if let Err(message) = deploy_payload(request, &name, CHECK_NONCE, CHECK_BASE) {
        return stop(message);
    }

    let operation = format!("deploy {}", request.kind.token());

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
                request: &first,
                name: &name,
                operation: &operation,
                template,
            };

            attempt_deploy(&attempt, request, (&mut *nonce, &mut *clock), &mut *gate).routed()
        },
    )
}

fn stop(message: impl std::fmt::Display) -> Exit {
    Exit {
        code: 2,
        stdout: String::new(),
        stderr: format!("STOP: {message}"),
    }
}

/// The payload of run 2: the file, the request, and the base the diff was shown against.
fn deploy_payload(
    request: &DdicDeploy<'_>,
    name: &str,
    nonce: &str,
    base: &str,
) -> Result<Vec<String>, String> {
    let mut run = Request::new(Mode::Deploy, request.kind, name, nonce);

    run.xml = Some(request.xml);
    run.tr = request.tr;
    run.base = Some(base);

    encode(&run)
}

/// The payload of the extra `READ` run of a reconciliation.
fn read_payload(request: &DdicDeploy<'_>, name: &str, nonce: &str) -> Result<Vec<String>, String> {
    encode(&Request::new(Mode::Read, request.kind, name, nonce))
}

// --- one attempt -------------------------------------------------------------------------------

fn attempt_deploy(
    attempt: &Attempt<'_>,
    deploy: &DdicDeploy<'_>,
    (nonce, clock): (&mut dyn FnMut() -> String, &mut dyn Clock),
    gate: Gate<'_>,
) -> Done {
    match first_run(attempt, &mut *nonce) {
        Step::Done(done) => done,
        Step::Verified(verified) => write(attempt, deploy, verified, (nonce, clock), gate),
    }
}

/// Run 2, after run 1 has shown the diff and the live hash.
fn write(
    attempt: &Attempt<'_>,
    deploy: &DdicDeploy<'_>,
    verified: Verified,
    (nonce, clock): (&mut dyn FnMut() -> String, &mut dyn Clock),
    gate: Gate<'_>,
) -> Done {
    let Verified {
        run: first,
        mut print,
        base,
    } = verified;
    // Nothing is written when run 1 cannot be trusted to have left a clean system behind.
    let Some(base) = base else {
        print
            .stdout
            .push("REJECTED: the runner returned no live definition".to_owned());

        return print.finish(&first, 1);
    };

    if first.leftover.is_some() {
        print.stderr.push(format!(
            "ERROR: {} {}: the temporary class of the diff run stays; nothing was written",
            attempt.operation, attempt.name
        ));

        return print.finish(&first, 1);
    }

    if first.interrupted {
        print
            .stderr
            .push("CANCELED: interrupted before the write; nothing was written".to_owned());

        return print.finish(&first, 1);
    }

    let preview = Preview {
        runner: first.name.as_deref(),
        printed: &print.stdout,
    };

    if let Err(message) = gate(&preview) {
        print.stderr.push(message);

        return print.finish(&first, 1);
    }

    let text = nonce();
    let second_nonce = match Nonce::parse(&text) {
        Ok(nonce) => nonce,
        Err(message) => return refusal(&message),
    };
    let lines = match deploy_payload(deploy, attempt.name, &text, &base) {
        Ok(lines) => lines,
        Err(message) => return refusal(&message),
    };
    let mut second = run_lines(attempt, attempt.operation, &lines, attempt.cancel);

    settle(&mut second, &second_nonce, RunMode::Deploy);

    // The lines of run 1 stay in front of everything run 2 adds, on both streams.
    let mut notes = first.notes.clone();

    notes.extend(std::mem::take(&mut print.stderr));
    notes.append(&mut second.notes);
    second.notes = notes;
    print.stdout.extend(second.runner_line());

    let file_hash = source_hash(&normalize(local_source_lines(deploy.xml)));

    match stage(&second, &second_nonce, RunMode::Deploy) {
        Stage::Report(report) => written(attempt, &second, &report, &file_hash, print),
        Stage::Uncertain(_) => reconcile_deploy(
            attempt,
            deploy,
            &second,
            Hashes {
                file: &file_hash,
                base: &base,
            },
            (nonce, clock),
            print,
        ),
        other => print.ended(&second, other, Simulation::Deploy { base: None }),
    }
}

/// The report of run 2: the status the class gave, checked by a new session for `ACTIVE`.
fn written(
    attempt: &Attempt<'_>,
    run: &RunnerRun,
    report: &Report,
    file_hash: &str,
    mut print: Print,
) -> Done {
    warnings(report, &mut print);

    match report.status {
        RunStatus::Active => active(attempt, run, report, file_hash, print),
        RunStatus::Inactive => {
            print.stdout.extend(texts(report, Level::Info));
            print.stdout.push(format!(
                "INACTIVE: {}; the inactive version stays, deploy again from the read file",
                reason(report, "the definition was saved but not activated")
            ));

            print.finish(run, 1)
        }
        RunStatus::Partial => {
            print.stdout.extend(texts(report, Level::Info));
            print.stdout.push(format!(
                "PARTIAL: {}",
                reason(report, "the write ended partially")
            ));

            print.finish(run, 1)
        }
        _ if texts(report, Level::Error)
            .iter()
            .any(|text| text == BASE_CHANGED) =>
        {
            print.stderr.push(format!("STOP: {BASE_CHANGED}"));

            print.finish(run, 2)
        }
        _ => {
            print.stdout.extend(texts(report, Level::Info));
            print.stdout.push(format!(
                "REJECTED: {}",
                reason(report, "the runner rejected the write")
            ));

            print.finish(run, 1)
        }
    }
}

pub(crate) fn fact(report: &Report, key: &str) -> Option<String> {
    report
        .facts
        .iter()
        .find(|fact| fact.key == key)
        .map(|fact| fact.value.clone())
}

/// `ACTIVE` only when the run reported it with the hash of the written definition AND a new
/// session finds the definition active, in the package the run reported, with no inactive version.
fn active(
    attempt: &Attempt<'_>,
    run: &RunnerRun,
    report: &Report,
    file_hash: &str,
    mut print: Print,
) -> Done {
    let Some(result) = fact(report, "RESULT_HASH") else {
        print.stdout.push(
            "PARTIAL: the run reported ACTIVE without a result hash; check with read before \
             retrying"
                .to_owned(),
        );

        return print.finish(run, 1);
    };

    if let Some(why) = disagreement(attempt, run, fact(report, "PACKAGE").as_deref(), &mut print) {
        print.stdout.push(format!(
            "PARTIAL: the run reported ACTIVE but {why}; check with read before retrying"
        ));

        return print.finish(run, 1);
    }

    // SAP may normalize the definition: the file is then not the active canonical form.
    if !result.eq_ignore_ascii_case(file_hash) {
        print.stderr.push(DIFFERS_FROM_FILE.to_owned());
    }

    print.stdout.push("ACTIVE".to_owned());

    print.finish(run, 0)
}

/// What a new session says against the run: `None` when it agrees. The runner class that stays
/// (reported on its own) does not count against the definition.
pub(crate) fn disagreement(
    attempt: &Attempt<'_>,
    run: &RunnerRun,
    package: Option<&str>,
    print: &mut Print,
) -> Option<String> {
    let kind = attempt.request.kind;
    let open = attempt.open;
    let mut opener = || open();
    let mut evidence = read_evidence(
        &mut opener,
        kind,
        attempt.name,
        run.name.as_deref().unwrap_or_default(),
    );

    if run.leftover.is_some() {
        evidence.runner = Ok(None);
    }

    print.stderr.extend(evidence.notes.clone());

    let proof = judge(
        &Expect::Present {
            package,
            report: None,
        },
        kind,
        &evidence,
    );
    let inactive = read_inactive(&mut opener, kind, attempt.name);

    match (proof, inactive) {
        (Proof::Mismatch(text), _) => Some(format!("a new session disagrees ({text})")),
        (_, Ok(Some(_))) => Some("a new session disagrees (an inactive version exists)".to_owned()),
        (Proof::Unreadable(text), _) => {
            Some(format!("a new session could not confirm it ({text})"))
        }
        (_, Err(text)) => Some(format!(
            "a new session could not confirm it (inactive version: {text})"
        )),
        (Proof::Matches, Ok(None)) => None,
    }
}

// --- an uncertain run 2 ------------------------------------------------------------------------

/// The hashes a reconciliation compares: the normalized file and the base of the diff.
struct Hashes<'a> {
    file: &'a str,
    base: &'a str,
}

/// The POST of run 2 was sent and its reply cannot be believed: settle from a new session, with at
/// most one extra `READ` run, and print the reconciled verdict. Run 2 is never resent.
fn reconcile_deploy(
    attempt: &Attempt<'_>,
    deploy: &DdicDeploy<'_>,
    run: &RunnerRun,
    hashes: Hashes<'_>,
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
            read_payload(deploy, attempt.name, &text),
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
        &Intent::Deploy {
            file_hash: hashes.file,
            base_hash: hashes.base,
        },
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
