//! `run PROG` and `run SNIPPET`: code that runs inside a temporary runner class.
//!
//! ADT has no endpoint that runs a report or a snippet, so both go through the runner cycle
//! (`runner.rs`): the class is generated from a constant template and one payload, the single
//! classrun `POST` runs it, and the class is deleted again. The simulate reads the facts a plan
//! binds and runs nothing. The `POST` is sent once and never repeated, on any transport.
//!
//! No outcome reconciles, because there is no object state to compare. The cleanup and its proof
//! run after every outcome.
//!
//! | Outcome | When | Database | Exit |
//! | :--- | :--- | :--- | :--- |
//! | `RAN` | the code ran to its end | no rollback by ABAPReach (as SA38) | 0 |
//! | `RUN_FAILED: exception ...` | a catchable exception reached the runner | rolled back | 1 |
//! | `RUN_FAILED: runtime error ...` | the runtime-error reply of the server | rolled back by the server | 1 |
//! | `RUN_UNKNOWN: ...` | any other 5xx, no reply, a reply without a status line | unknown | 1 |
//! | `SNIPPET_ERROR: line n: ...` | the snippet does not compile; nothing ran | none | 2 |

use crate::adt::{package_of, responsible_of};
use crate::backend::{BackendKind, Failure};
use crate::cli::{AlvColumns, AlvMode, ObjectKind, RunKind};
use crate::config::Connection;
use crate::domain::Outcome;
use crate::hash::{local_source_lines, source_hash};
use crate::operation::delete_adt::Opener;
use crate::operation::deploy::{CancelFlag, INTERRUPT_EXIT_CODE};
use crate::operation::payload::{RunProg, RunRequest, encode_run};
use crate::operation::quality::STATELESS_HTTP;
use crate::operation::run::{
    ClassFacts, ClassRun, NOT_A_DRY_RUN, RUN_FAILED, RUN_UNKNOWN, RunResult, Stop, Verdict,
    check_client, column, cut, get, is_runtime_error_body, join_lines, object_version, preview,
};
use crate::operation::run_alv::format_alv;
use crate::operation::run_plan::{RunArgs, RunPlan, plan_changed_text, plan_mismatch_text};
use crate::operation::runner::{
    CycleExtras, CycleInput, PostRoute, RunnerEnd, RunnerRun, SNIPPET_ERROR_PREFIX, run_cycle_with,
};
use crate::operation::runner_evidence::preflight;
use crate::operation::runner_template::{
    OutType, RUN_PROG_TEMPLATE, SnippetSource, generate_for, generate_snippet,
};
use crate::runner_protocol::{Level, Nonce, Parsed, Report, RunMode, RunStatus, parse_output};
use crate::transport::rfc::is_abap_runtime_failure;
use crate::transport::{Response, SadtTransport};
use std::time::Duration;

/// The output bytes a run prints when `--max-output` is not given.
pub const DEFAULT_MAX_OUTPUT: u64 = 6 << 20;
/// The line the first Ctrl-C prints while the runner class runs.
pub const RUNNER_WAIT_TEXT: &str = "I: run in progress on the server; waiting for the reply to clean up (Ctrl-C again to leave the runner class behind)";
/// A payload expires this long after the run could have ended over ADT HTTP: it tolerates a clock
/// skew of up to 15 minutes between this machine and the server.
const HTTP_GRACE_SECS: u64 = 20 * 60;
/// Over RFC there is no timeout, so the grace is longer: the dialog work process limit decides.
const RFC_EXPIRY_SECS: u64 = 75 * 60;
/// A class name for the one generation that only measures where the snippet stands.
const MEASURE_NAME: &str = "YCL_ABAPREACH_RUN_AAAAAAAA";
/// A placeholder payload line for that generation.
const MEASURE_PAYLOAD: &str = "QUJD";

/// The UTC time `YYYYMMDDHHMMSS` after which the runner class of a run refuses to run: the time
/// the run was started, plus the timeout and a grace over ADT HTTP, or a fixed span over RFC.
pub fn expiry(now_unix: u64, backend: BackendKind, timeout: u32) -> String {
    let span = match backend {
        BackendKind::AdtHttp => u64::from(timeout) + HTTP_GRACE_SECS,
        BackendKind::RfcSadt => RFC_EXPIRY_SECS,
    };

    utc_stamp(now_unix + span)
}

/// Seconds since the epoch as UTC `YYYYMMDDHHMMSS` (the civil calendar of Howard Hinnant).
pub fn utc_stamp(unix: u64) -> String {
    let days = i64::try_from(unix / 86_400).unwrap_or(i64::MAX / 2);
    let seconds = unix % 86_400;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

/// One `run PROG` or `run SNIPPET` on one backend.
pub struct RunnerRunOp<'a> {
    pub connection: &'a Connection,
    pub args: &'a RunArgs,
    pub backend: BackendKind,
    /// The seconds the `POST` may take over ADT HTTP; unused on RFC.
    pub timeout: u32,
    pub cancel: &'a CancelFlag,
    /// The most output bytes the class emits.
    pub max_output: u64,
    /// The 128-bit nonce of the run as 32 upper-case hex digits.
    pub nonce: &'a str,
    /// The start of the run, seconds since the epoch.
    pub now_unix: u64,
}

impl RunnerRunOp<'_> {
    fn label(&self) -> &'static str {
        match self.args.kind {
            RunKind::Prog => "PROG",
            RunKind::Snippet | RunKind::Clas => "SNIPPET",
        }
    }

    /// Reads the facts and builds the plan; nothing is executed and nothing is created.
    pub fn simulate(&self, transport: &mut dyn SadtTransport) -> RunResult {
        let facts = match self.inspect(transport) {
            Ok(facts) => facts,
            Err(stop) => return RunResult::from_stop(stop),
        };
        let found = match preflight(transport, &self.connection.client) {
            Ok(found) => found,
            Err(error) => return RunResult::failed(Failure::from(&error), error.to_string()),
        };

        if let Some(reason) = found.refusal {
            return RunResult::stopped(format!(
                "run {} needs a temporary class; repository changes are not allowed on {}/{} ({reason})",
                self.label(),
                self.connection.system_id,
                self.connection.client
            ));
        }

        let connection = self.connection;
        let mut lines = vec![self.header(&facts)];

        lines.push(format!("HASH {}", facts.hash));

        if let Some(subc) = &facts.subc {
            lines.push(format!("SUBC {subc}"));
        }

        for (name, value) in &self.args.params {
            lines.push(format!("PARAM {name}={value}"));
        }

        if let Some(variant) = &self.args.variant {
            lines.push(format!("VARIANT {variant}"));
        }

        if let (Some(alv), Some(columns)) = (self.args.alv, self.args.alv_columns) {
            lines.push(format!("ALV {} columns {}", alv.token(), columns.token()));
        } else if let Some(alv) = self.args.alv {
            lines.push(format!("ALV {}", alv.token()));
        }

        lines.push(format!("CLIENT {} (not productive)", connection.client));
        lines.push(format!("BACKEND {}", self.backend.token()));
        lines.push(found.line);
        lines.push(format!("OUTPUT LIMIT {} bytes", self.max_output));

        if let Some(snippet) = &self.args.snippet {
            lines.push("SNIPPET:".to_owned());
            lines.extend(snippet.text.lines().map(|line| format!("  {line}")));
        }

        lines.push(NOT_A_DRY_RUN.to_owned());

        let plan = self.args.plan(
            &connection.id,
            &connection.client,
            self.backend.token(),
            facts.plan_facts(),
        );

        RunResult {
            plan: Some(plan),
            facts: Some(facts),
            ..RunResult::finished(
                Outcome {
                    status: Some(crate::domain::Status::SimulatedOk),
                    exit: 0,
                    stdout: lines.join("\n"),
                    stderr: found.notes.join("\n"),
                },
                None,
            )
        }
    }

    fn header(&self, facts: &ClassFacts) -> String {
        match self.args.kind {
            RunKind::Snippet => format!(
                "SIMULATED_OK run SNIPPET {} lines",
                self.args
                    .snippet
                    .as_ref()
                    .map_or(0, |snippet| snippet.text.lines().count())
            ),
            _ => format!(
                "SIMULATED_OK run PROG {} package {} author {}",
                self.args.name,
                facts.package,
                if facts.author.is_empty() {
                    "-"
                } else {
                    &facts.author
                }
            ),
        }
    }

    /// Reads the facts again, compares them with the plan when there is one, and runs the cycle.
    /// `open` yields the sessions of the cycle.
    pub fn execute(
        &self,
        transport: &mut dyn SadtTransport,
        plan: Option<&RunPlan>,
        open: &mut Opener<'_>,
    ) -> RunResult {
        if self.cancel.is_set() {
            return RunResult::finished(
                Outcome {
                    status: None,
                    exit: INTERRUPT_EXIT_CODE,
                    stdout: String::new(),
                    stderr: "CANCELED: stopped before anything was sent".to_owned(),
                },
                None,
            );
        }

        let facts = match self.inspect(transport) {
            Ok(facts) => facts,
            Err(stop) => return RunResult::from_stop(stop),
        };

        if let Some(plan) = plan {
            if plan.backend != self.backend.token() {
                return RunResult::stopped(plan_mismatch_text("backend"));
            }

            if let Some(field) = plan.fact_mismatch(&facts.plan_facts()) {
                return RunResult::refused(plan_changed_text(field));
            }
        }

        // The reads are over: their session must not stay open for the length of the run.
        let note = transport
            .close()
            .err()
            .map(|error| format!("W: session close failed: {error}"));
        let mut result = self.cycle(&facts, open);

        if let Some(note) = note {
            result.outcome.stderr = join_lines([result.outcome.stderr.as_str(), note.as_str()]);
        }

        result.facts = Some(facts);

        result
    }

    /// The client, and for a report its facts. A snippet has no object, so its facts are empty
    /// and the plan binds its hash through the arguments.
    fn inspect(&self, transport: &mut dyn SadtTransport) -> Result<ClassFacts, Stop> {
        check_client(transport, &self.connection.client)?;

        // The first reply of the session shows whether it keeps state. The temporary class of a
        // report or snippet needs that, and the reads of a stateless session cannot be trusted
        // (an existing report may answer 404), so the command leaves before it reads anything
        // else and before anything is written. Without an RFC route the cycle reports it.
        if self.backend == BackendKind::AdtHttp
            && self.connection.rfc.is_some()
            && transport.stateful_supported() == Some(false)
        {
            return Err(Stop::Failed {
                failure: Failure::CapabilityUnavailable,
                message: STATELESS_HTTP.to_owned(),
            });
        }

        match &self.args.snippet {
            Some(snippet) => {
                // The rules of the snippet run before any connection; the check here only keeps
                // the simulate and the run on the same text.
                crate::operation::run_snippet::check(&snippet.text).map_err(Stop::Rejected)?;

                Ok(ClassFacts {
                    package: String::new(),
                    author: String::new(),
                    hash: String::new(),
                    subc: None,
                })
            }
            None => self.inspect_report(transport),
        }
    }

    fn inspect_report(&self, transport: &mut dyn SadtTransport) -> Result<ClassFacts, Stop> {
        let name = self.args.name.as_str();
        let uri = ObjectKind::Prog
            .object_uri(name, None)
            .map_err(Stop::Rejected)?;
        let response = get(transport, &uri, "application/*")?;

        match response.status {
            200 => {}
            404 if transport.stateful_supported() == Some(false) => {
                return Err(Stop::Rejected(format!(
                    "report {name} was not found, but this ADT HTTP session is stateless and \
                     cannot show that it does not exist; use --backend rfc"
                )));
            }
            404 => return Err(Stop::Rejected(format!("report {name} does not exist"))),
            status => {
                return Err(Stop::Rejected(format!(
                    "report {name} cannot be read: HTTP {status}"
                )));
            }
        }

        let xml = String::from_utf8_lossy(&response.body).into_owned();

        if object_version(&xml).as_deref() == Some("inactive") {
            return Err(Stop::Rejected(format!(
                "report {name} is not active; activate it first"
            )));
        }

        let package = package_of(&xml)
            .map_err(|error| Stop::Rejected(format!("report {name}: {error}")))?
            .ok_or_else(|| Stop::Rejected(format!("the package of report {name} is unknown")))?;
        let author = responsible_of(&xml)
            .map_err(|error| Stop::Rejected(format!("report {name}: {error}")))?
            .unwrap_or_default();
        let source = get(
            transport,
            &format!("{uri}/source/main?version=active"),
            "text/plain",
        )?;

        match source.status {
            200 => {}
            404 => {
                return Err(Stop::Rejected(format!(
                    "report {name} has no active version; activate it first"
                )));
            }
            status => {
                return Err(Stop::Rejected(format!(
                    "the active source of report {name} cannot be read: HTTP {status}"
                )));
            }
        }

        let hash = source_hash(&local_source_lines(&String::from_utf8_lossy(&source.body)));
        let data = preview(transport, "TRDIR", &subc_query(name))?;
        let subc = column(&data, "SUBC")
            .and_then(|index| data.rows.first().and_then(|row| row.get(index)))
            .map(|value| value.trim().to_owned())
            .ok_or_else(|| Stop::Rejected(format!("report {name} has no row in TRDIR")))?;

        if subc != "1" {
            return Err(Stop::Rejected(format!(
                "report {name} is not an executable program (SUBC={subc}); only type 1 runs"
            )));
        }

        Ok(ClassFacts {
            package,
            author,
            hash,
            subc: Some(subc),
        })
    }

    /// The runner cycle: the payload, the class, the single `POST`, the cleanup, and the outcome.
    fn cycle(&self, facts: &ClassFacts, open: &mut Opener<'_>) -> RunResult {
        let nonce = match Nonce::parse(self.nonce) {
            Ok(nonce) => nonce,
            Err(message) => return RunResult::stopped(message),
        };
        let prog = (self.args.kind == RunKind::Prog).then(|| RunProg {
            report: &self.args.name,
            base: &facts.hash,
            params: &self.args.params,
            variant: self.args.variant.as_deref(),
            alv: self.args.alv.unwrap_or(AlvMode::On),
            alv_columns: self.args.alv_columns.unwrap_or(AlvColumns::Visible),
        });
        let expires = expiry(self.now_unix, self.backend, self.timeout);
        let payload = match encode_run(&RunRequest {
            nonce: self.nonce,
            user: &self.connection.user,
            expires: &expires,
            max_bytes: self.max_output,
            prog,
        }) {
            Ok(payload) => payload,
            Err(message) => return RunResult::stopped(message),
        };
        let snippet_text = self
            .args
            .snippet
            .as_ref()
            .map(|snippet| snippet.text.clone());
        let snippet_at = match &snippet_text {
            Some(text) => match generate_snippet(
                MEASURE_NAME,
                &[MEASURE_PAYLOAD.to_owned()],
                text,
                OutType::Internal,
            ) {
                Ok(SnippetSource {
                    first_line,
                    line_count,
                    ..
                }) => Some((first_line, line_count)),
                Err(message) => return RunResult::stopped(message),
            },
            None => None,
        };
        let build = |runner: &str, out: OutType| -> Result<String, String> {
            match &snippet_text {
                Some(text) => generate_snippet(runner, &payload, text, out).map(|made| made.source),
                None => generate_for(&RUN_PROG_TEMPLATE, runner, &payload, out),
            }
        };
        let connection = self.connection;
        let operation = format!("run {}", self.label());
        let input = CycleInput {
            operation: &operation,
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
        let extras = CycleExtras {
            post_limit: (self.backend == BackendKind::AdtHttp)
                .then(|| Duration::from_secs(u64::from(self.timeout))),
            wait_text: Some(RUNNER_WAIT_TEXT),
            snippet: snippet_at,
        };
        let mode = match self.args.kind {
            RunKind::Prog => RunMode::RunProg,
            _ => RunMode::RunSnippet,
        };
        let mut run = run_cycle_with(&input, open, PostRoute::Cycle, self.cancel, &extras);
        let interrupted = self.cancel.run_interrupted();

        if interrupted {
            run.interrupted = true;
        }

        map_run(
            &run,
            &nonce,
            mode,
            self.args.alv_columns.unwrap_or(AlvColumns::Visible),
        )
    }
}

/// The preview of `TRDIR` that gives the program type of a report. The name is validated by the
/// argument rules, so it holds no quote.
pub fn subc_query(name: &str) -> String {
    format!("SELECT SUBC FROM TRDIR WHERE NAME = '{name}'")
}

/// What a finished cycle means for the user: the outcome token, the output, and the exit code.
pub fn map_run(run: &RunnerRun, nonce: &Nonce, mode: RunMode, columns: AlvColumns) -> RunResult {
    let mut stderr: Vec<String> = Vec::new();
    let mut stdout = String::new();
    let mut verdict = None;
    let mut posted_exit = 1;
    let mut failure = None;
    let mut fallback_allowed = false;
    let mut detail = None;

    stderr.extend(run.runner_line());
    stderr.extend(run.preflight.clone());
    stderr.extend(run.notes.clone());

    match &run.end {
        RunnerEnd::Posted(response) => {
            let mapped = map_reply(response, nonce, mode, columns);

            stdout = mapped.stdout;
            stderr.extend(mapped.stderr);
            verdict = Some(mapped.verdict);
            posted_exit = mapped.exit;
        }
        RunnerEnd::PostUncertain(error) => {
            if is_abap_runtime_failure(error) {
                stderr.push(RUN_FAILED.to_owned());
                verdict = Some(Verdict::Failed);
            } else {
                stderr.push(format!("{RUN_UNKNOWN}\nW: {}", cut(&error.to_string())));
                verdict = Some(Verdict::Unknown);
            }
        }
        RunnerEnd::Rejected(text) => stderr.push(format!("ERROR: {text}")),
        RunnerEnd::Stop(text) if text.starts_with(SNIPPET_ERROR_PREFIX) => {
            stderr.push(text.clone());
        }
        RunnerEnd::Stop(text) => stderr.push(format!("STOP: {text}")),
        RunnerEnd::Interrupted => {
            stderr.push("CANCELED: interrupted before the classrun POST; nothing ran".to_owned());
        }
        RunnerEnd::NotStarted(error) => {
            let message = error.to_string();

            stderr.push(format!("ERROR: {message}"));
            failure = Some(Failure::from(error));
            fallback_allowed = failure == Some(Failure::PreSendConnectivity)
                && run.leftover.is_none()
                && !run.interrupted;
            detail = Some(message);
        }
    }

    if run.interrupted && matches!(run.end, RunnerEnd::Posted(_)) && posted_exit == 0 {
        stderr.push("I: interrupted; the run had finished before the reply was read".to_owned());
    }

    stderr.extend(run.leftover_text());

    // As for CLAS: a Ctrl-C turns only a successful reply into 130; a reply that failed keeps
    // its own exit (RUN_FAILED, RUN_UNKNOWN, SNIPPET_ERROR, REJECTED).
    let exit = match run.exit_code(posted_exit) {
        INTERRUPT_EXIT_CODE if matches!(run.end, RunnerEnd::Posted(_)) && posted_exit != 0 => {
            posted_exit
        }
        code => code,
    };

    RunResult {
        failure,
        fallback_allowed,
        detail,
        ..RunResult::finished(
            Outcome {
                status: None,
                exit,
                stdout,
                stderr: stderr.join("\n"),
            },
            verdict,
        )
    }
}

/// How an answered `POST` ended.
struct Replied {
    stdout: String,
    stderr: Vec<String>,
    verdict: Verdict,
    exit: i32,
}

fn replied(verdict: Verdict, exit: i32, stderr: Vec<String>) -> Replied {
    Replied {
        stdout: String::new(),
        stderr,
        verdict,
        exit,
    }
}

/// The reading of a classrun reply by its HTTP status, then by its protocol lines.
fn map_reply(response: &Response, nonce: &Nonce, mode: RunMode, columns: AlvColumns) -> Replied {
    match response.status {
        200..=299 => {
            let body = String::from_utf8_lossy(&response.body);

            match parse_output(nonce, mode, &body) {
                Parsed::Complete(report) => map_report(&report, columns),
                Parsed::Uncertain { reason, warnings } => {
                    let mut lines = vec![format!("{RUN_UNKNOWN}\nW: {reason}")];

                    lines.extend(warnings);

                    replied(Verdict::Unknown, 1, lines)
                }
                // A 2xx status never means a refusal; kept for the exhaustive match.
                Parsed::NotRun(status) => replied(
                    Verdict::Unknown,
                    1,
                    vec![format!(
                        "{RUN_UNKNOWN}\nW: the classrun reply was HTTP {status}"
                    )],
                ),
            }
        }
        500 if is_runtime_error_body(&response.body) => {
            replied(Verdict::Failed, 1, vec![RUN_FAILED.to_owned()])
        }
        500..=599 => replied(
            Verdict::Unknown,
            1,
            vec![format!(
                "{RUN_UNKNOWN}\nW: the classrun reply was HTTP {}",
                response.status
            )],
        ),
        status => replied(
            Verdict::Refused,
            1,
            vec![format!(
                "REJECTED: the classrun POST was refused: HTTP {status}; the code did not run"
            )],
        ),
    }
}

/// The report of a whole, well formed run.
fn map_report(report: &Report, columns: AlvColumns) -> Replied {
    let mut lines: Vec<String> = Vec::new();
    let mut stderr: Vec<String> = Vec::new();
    let errors: Vec<&str> = report
        .messages
        .iter()
        .filter(|message| message.level == Level::Error)
        .map(|message| message.text.as_str())
        .collect();

    if let Some(alv) = &report.alv {
        lines.extend(format_alv(alv, columns));
    }

    lines.extend(report.lines.iter().map(|line| line.as_str().to_owned()));

    for message in &report.messages {
        match message.level {
            Level::Warning => stderr.push(format!("W: {}", message.text)),
            Level::Info => stderr.push(format!("I: {}", message.text)),
            Level::Error => {}
        }
    }

    stderr.extend(report.warnings.clone());

    let (verdict, exit) = match report.status {
        RunStatus::Ran => (Verdict::Ran, 0),
        RunStatus::RunFailed => {
            stderr.insert(0, format!("RUN_FAILED: {}", errors.join("; ")));
            stderr.push("W: the work since the last COMMIT WORK was rolled back".to_owned());

            (Verdict::Failed, 1)
        }
        // The class refused before it ran anything: nothing ran.
        _ => {
            stderr.insert(0, format!("REJECTED: {}", errors.join("; ")));

            (Verdict::Refused, 1)
        }
    };

    Replied {
        stdout: lines.join("\n"),
        stderr,
        verdict,
        exit,
    }
}

/// Everything one run needs, whatever it runs.
pub struct RunSettings<'a> {
    pub connection: &'a Connection,
    pub args: &'a RunArgs,
    pub backend: BackendKind,
    pub timeout: u32,
    pub cancel: &'a CancelFlag,
    pub max_output: u64,
    pub nonce: &'a str,
    pub now_unix: u64,
}

/// One run of any kind: an existing class, or code inside a runner class.
pub enum RunCall<'a> {
    Class(ClassRun<'a>),
    Runner(RunnerRunOp<'a>),
}

impl<'a> RunCall<'a> {
    pub fn new(settings: &RunSettings<'a>) -> Self {
        match settings.args.kind {
            RunKind::Clas => Self::Class(ClassRun {
                connection: settings.connection,
                args: settings.args,
                backend: settings.backend,
                timeout: settings.timeout,
                cancel: settings.cancel,
            }),
            RunKind::Prog | RunKind::Snippet => Self::Runner(RunnerRunOp {
                connection: settings.connection,
                args: settings.args,
                backend: settings.backend,
                timeout: settings.timeout,
                cancel: settings.cancel,
                max_output: settings.max_output,
                nonce: settings.nonce,
                now_unix: settings.now_unix,
            }),
        }
    }

    pub fn simulate(&self, transport: &mut dyn SadtTransport) -> RunResult {
        match self {
            Self::Class(call) => call.simulate(transport),
            Self::Runner(call) => call.simulate(transport),
        }
    }

    pub fn execute(
        &self,
        transport: &mut dyn SadtTransport,
        plan: Option<&RunPlan>,
        open: &mut Opener<'_>,
    ) -> RunResult {
        match self {
            Self::Class(call) => call.execute(transport, plan, open),
            Self::Runner(call) => call.execute(transport, plan, open),
        }
    }
}

/// Seconds since the epoch now; the start of a run.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
