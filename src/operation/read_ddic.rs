//! `read` of a `DTEL`, `DOMA`, `TABL`, or `TTYP` through the temporary runner class (M6 Task 18).
//!
//! One run of the runner cycle (`runner.rs`) in mode `READ`: the class is generated from the
//! constant DDIC template and one encoded payload that holds the object and the nonce, and the run
//! answers with the XML rows of the active definition, their hash, and one status. The rows are
//! data from a remote system: they go to the output file only, after the same client rule that
//! `deploy` applies to a file (`normalize`), and are never printed or spliced into a message.
//!
//! The status is decided here, not by the class: the file is written only when the run says `OK`,
//! the hash of the rows agrees with the hash the run reported, and, when it can be read, a NEW
//! session confirms that the definition exists (`runner_evidence.rs`). A definite disagreement is
//! `PARTIAL`; a preview that cannot be read is a note, because a read changes nothing in SAP. A
//! run whose reply is lost, cut, or broken has nothing to reconcile and is never resent.
//!
//! Differences from the Python tool, on purpose: the read creates and deletes a temporary class
//! in `$TMP` (Python calls one function module), a status other than `OK` is exit 1 (Python stops
//! with exit 2), a hash that differs from the runner hash is exit 1, and `--lines` is refused.

use crate::app::{Exit, Factories, Opened};
use crate::backend::{BackendKind, Failure};
use crate::config::Connection;
use crate::hash::{normalize, source_hash};
use crate::operation::deploy::CancelFlag;
use crate::operation::payload::{Kind, Mode, Request, encode};
use crate::operation::read::emit_lines;
use crate::operation::runner::{CycleInput, PostRoute, RunnerEnd, RunnerRun, run_cycle};
use crate::operation::runner_evidence::{Expect, Proof, judge, read_evidence};
use crate::operation::runner_reconcile::{Cause, uncertainty};
use crate::operation::runner_route::{Routed, random_nonce, route};
use crate::operation::runner_template::{
    DOMA_TEMPLATE, DTEL_TEMPLATE, TABL_TEMPLATE, TTYP_TEMPLATE, TypeTemplate, generate,
};
use crate::runner_protocol::{Level, Nonce, Parsed, Report, RunMode, RunStatus, parse_post_reply};
use std::path::Path;

/// A valid nonce for the checks that run before any connection.
const CHECK_NONCE: &str = "00000000000000000000000000000000";

/// `read <TYPE> <NAME> <OUT> [--force]` of a DDIC type. The name and the file rules are checked by
/// `runner_cli::check_read` before this runs.
pub struct DdicRead<'a> {
    pub kind: Kind,
    pub name: &'a str,
    pub out: &'a Path,
    pub force: bool,
}

pub fn read(
    connection: &Connection,
    explicit: Option<BackendKind>,
    request: &DdicRead<'_>,
    factories: &mut dyn Factories,
) -> Exit {
    let name = request.name.to_ascii_uppercase();
    let Some(template) = template_of(request.kind) else {
        return stop(format!("{} is not a DDIC type", request.kind.token()));
    };

    // The payload refuses a value before any connection is opened.
    if let Err(message) = payload(request.kind, &name, CHECK_NONCE) {
        return stop(message);
    }

    route(
        &format!("read {}", request.kind.token()),
        connection,
        explicit,
        factories,
        |selection, open, cancel| {
            run_attempt(
                &Attempt {
                    connection,
                    backend: selection.backend,
                    open,
                    cancel,
                    request,
                    name: &name,
                    template,
                },
                &mut random_nonce,
            )
            .routed()
        },
    )
}

pub(crate) fn template_of(kind: Kind) -> Option<&'static TypeTemplate> {
    match kind {
        Kind::Dtel => Some(&DTEL_TEMPLATE),
        Kind::Doma => Some(&DOMA_TEMPLATE),
        Kind::Tabl => Some(&TABL_TEMPLATE),
        Kind::Ttyp => Some(&TTYP_TEMPLATE),
        Kind::Tran => None,
    }
}

fn payload(kind: Kind, name: &str, nonce: &str) -> Result<Vec<String>, String> {
    encode(&Request::new(Mode::Read, kind, name, nonce))
}

fn stop(message: impl std::fmt::Display) -> Exit {
    Exit {
        code: 2,
        stdout: String::new(),
        stderr: format!("STOP: {message}"),
    }
}

// --- one attempt -------------------------------------------------------------------------------

/// Everything one attempt on one backend needs.
struct Attempt<'a> {
    connection: &'a Connection,
    backend: BackendKind,
    open: &'a dyn Fn() -> Opened,
    cancel: &'a CancelFlag,
    request: &'a DdicRead<'a>,
    /// The object name, upper case.
    name: &'a str,
    template: &'static TypeTemplate,
}

/// One attempt, and whether the router may try the other backend.
struct Done {
    code: i32,
    stdout: Vec<String>,
    stderr: Vec<String>,
    /// Set when nothing was sent and the failure allows the other backend.
    fallback: Option<(Failure, String)>,
}

impl Done {
    fn routed(self) -> Routed {
        Routed {
            code: self.code,
            stdout: self.stdout.join("\n"),
            stderr: self.stderr,
            fallback: self.fallback,
        }
    }
}

fn run_attempt(attempt: &Attempt<'_>, nonce: &mut dyn FnMut() -> String) -> Done {
    let kind = attempt.request.kind;
    let text = nonce();
    let nonce = match Nonce::parse(&text) {
        Ok(nonce) => nonce,
        Err(message) => return refusal(&message),
    };
    let lines = match payload(kind, attempt.name, &text) {
        Ok(lines) => lines,
        Err(message) => return refusal(&message),
    };
    let build = |runner: &str| generate(attempt.template, runner, &lines);
    let operation = format!("read {}", kind.token());
    let connection = attempt.connection;
    let input = CycleInput {
        operation: &operation,
        sid: &connection.system_id,
        client: &connection.client,
        conn: &connection.id,
        prefix: &connection.runner_prefix,
        user: &connection.user,
        language: &connection.lang,
        backend: attempt.backend,
        rfc_available: connection.rfc.is_some(),
        build: &build,
    };
    let open = attempt.open;
    let mut opener = || open();
    let run = run_cycle(&input, &mut opener, PostRoute::Cycle, attempt.cancel);
    let mut print = Print::start(&run);

    match stage(&run, &nonce) {
        Stage::Report(report) => report_done(attempt, &run, &report, print),
        Stage::Uncertain(cause) => {
            // A read writes nothing in SAP, so there is nothing to reconcile.
            print.stderr.push(format!(
                "ERROR: read {} {}: the runner status is uncertain ({cause}); nothing was \
                 written; run the read again",
                kind.token(),
                attempt.name
            ));

            print.finish(&run, 1)
        }
        other => print.ended(&run, other),
    }
}

fn refusal(message: &str) -> Done {
    Done {
        code: 2,
        stdout: Vec::new(),
        stderr: vec![format!("STOP: {message}")],
        fallback: None,
    }
}

/// How the cycle of the run ended, seen from the read.
enum Stage {
    /// A whole, well formed run.
    Report(Report),
    /// The reply is lost, cut, or broken; the text says why.
    Uncertain(String),
    /// A stage before the POST failed or the POST was refused: nothing ran.
    Rejected(String),
    /// Nothing was sent: another backend may still take the read.
    NotStarted(String, Failure),
    Stop(String),
    Interrupted,
}

fn stage(run: &RunnerRun, nonce: &Nonce) -> Stage {
    if let Some(cause) = uncertainty(&run.end, nonce, RunMode::Read) {
        let (Cause::LostReply(text) | Cause::Broken(text)) = cause;

        return Stage::Uncertain(text);
    }

    match &run.end {
        RunnerEnd::Posted(response) => {
            let body = String::from_utf8_lossy(&response.body);

            match parse_post_reply(nonce, RunMode::Read, response.status, &body) {
                Parsed::Complete(report) => Stage::Report(report),
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
struct Print {
    stdout: Vec<String>,
    stderr: Vec<String>,
}

impl Print {
    /// The runner line and the repository preflight come before everything else.
    fn start(run: &RunnerRun) -> Self {
        let mut stdout = Vec::new();

        stdout.extend(run.runner_line());
        stdout.extend(run.preflight.clone());

        Self {
            stdout,
            stderr: Vec::new(),
        }
    }

    /// The exit follows the run: a signal wins, then a class that stays, then `posted_exit`. The
    /// notes of the run come first on the stderr and the cleanup commands last.
    fn finish(self, run: &RunnerRun, posted_exit: i32) -> Done {
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

    /// An end that is no report: the stage of the cycle that stopped the read.
    fn ended(mut self, run: &RunnerRun, stage: Stage) -> Done {
        match stage {
            Stage::Stop(text) => self.stderr.push(format!("STOP: {text}")),
            Stage::Interrupted => self
                .stderr
                .push("CANCELED: interrupted before the classrun POST; nothing ran".to_owned()),
            Stage::Rejected(text) => self.stderr.push(format!("REJECTED: {text}")),
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
                unreachable!("a report and an uncertain status are handled by the read")
            }
        }

        self.finish(run, 1)
    }
}

// --- the report --------------------------------------------------------------------------------

fn texts(report: &Report, level: Level) -> Vec<String> {
    report
        .messages
        .iter()
        .filter(|message| message.level == level)
        .map(|message| message.text.clone())
        .collect()
}

/// Information goes to the stdout and warnings to the stderr; errors are the reason of a verdict.
fn side_messages(report: &Report, print: &mut Print) {
    print.stdout.extend(texts(report, Level::Info));
    print.stderr.extend(
        texts(report, Level::Warning)
            .into_iter()
            .map(|text| format!("W: {text}")),
    );
    print.stderr.extend(report.warnings.clone());
}

fn report_done(attempt: &Attempt<'_>, run: &RunnerRun, report: &Report, mut print: Print) -> Done {
    side_messages(report, &mut print);

    let label = format!("{} {}", attempt.request.kind.token(), attempt.name);
    let errors = texts(report, Level::Error);
    let reason = |fallback: &str| {
        if errors.is_empty() {
            fallback.to_owned()
        } else {
            errors.join("; ")
        }
    };

    match report.status {
        RunStatus::Ok => ok_done(attempt, run, report, print),
        RunStatus::NotFound => {
            print.stderr.push(format!(
                "read {label}: NOT_FOUND ({})",
                reason("no active version")
            ));

            print.finish(run, 1)
        }
        RunStatus::Rejected => {
            print.stderr.push(format!(
                "REJECTED: {}",
                reason("the runner rejected the request")
            ));

            print.finish(run, 1)
        }
        other => {
            print.stderr.push(format!(
                "ERROR: read {label}: the run ended {}",
                other.token()
            ));

            print.finish(run, 1)
        }
    }
}

/// The run read the definition. The rows are checked, a new session confirms the definition, and
/// only then is the file written.
fn ok_done(attempt: &Attempt<'_>, run: &RunnerRun, report: &Report, mut print: Print) -> Done {
    let kind = attempt.request.kind;
    let label = format!("{} {}", kind.token(), attempt.name);
    let fail = |mut print: Print, message: String| {
        print.stderr.push(message);

        print.finish(run, 1)
    };
    let lines: Vec<String> = report
        .lines
        .iter()
        .map(|line| line.as_str().to_owned())
        .collect();
    // The same rule as a file read back by `deploy`: the XML must be deployable again.
    let bad = lines
        .iter()
        .position(|line| line.chars().any(|c| c.is_control() && c != '\t'));

    if let Some(index) = bad {
        return fail(
            print,
            format!(
                "ERROR: read {label}: XML row {} has a control character; nothing was written",
                index + 1
            ),
        );
    }

    let rows = normalize(lines);

    if rows.is_empty() {
        return fail(
            print,
            format!("ERROR: read {label}: the runner returned no XML rows; nothing was written"),
        );
    }

    let hash = source_hash(&rows);
    let reported = report
        .facts
        .iter()
        .find(|fact| fact.key == "HASH")
        .map(|fact| fact.value.as_str());

    if !reported.is_some_and(|value| value.eq_ignore_ascii_case(&hash)) {
        return fail(
            print,
            format!(
                "ERROR: read {label}: the client hash differs from the runner hash \
                 (normalization rules out of sync); nothing was written"
            ),
        );
    }

    // A class that stays would make the evidence disagree for a reason that is not the read.
    if let (None, Some(runner)) = (&run.leftover, &run.name) {
        let open = attempt.open;
        let evidence = read_evidence(&mut || open(), kind, attempt.name, runner);

        print.stderr.extend(evidence.notes.clone());

        match judge(
            &Expect::Present {
                package: None,
                report: None,
            },
            kind,
            &evidence,
        ) {
            Proof::Matches => {}
            Proof::Unreadable(why) => print.stderr.push(format!(
                "W: the new session could not confirm the definition ({why})"
            )),
            Proof::Mismatch(why) => {
                return fail(
                    print,
                    format!(
                        "PARTIAL: read {label}: the runner read the definition but a new \
                         session does not confirm it ({why}); nothing was written"
                    ),
                );
            }
        }
    }

    // The file was checked before the connection; a file that appeared since is not overwritten.
    if !attempt.request.force && attempt.request.out.exists() {
        return fail(
            print,
            format!(
                "ERROR: {} already exists; use --force to overwrite",
                attempt.request.out.display()
            ),
        );
    }

    match emit_lines(&rows, None, attempt.request.out, None) {
        Ok(written) => {
            print.stdout.push(written.stdout);

            print.finish(run, 0)
        }
        Err(error) => fail(print, format!("ERROR: {}", error.message)),
    }
}
