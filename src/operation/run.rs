//! `run CLAS`: executes an existing class that implements `IF_OO_ADT_CLASSRUN` with one classrun
//! `POST`, from a new session, and prints the reply as is.
//!
//! A `run` is not a dry run: what the code writes stays written. The simulate reads the facts a
//! plan binds (the class exists and is active, it implements the interface, its package, author
//! and active source hash) and refuses a productive client; it executes nothing. The `POST` is
//! sent once and never repeated, on any transport.

use crate::adt::{package_of, responsible_of};
use crate::backend::{BackendKind, Failure};
use crate::cli::ObjectKind;
use crate::config::Connection;
use crate::domain::{Outcome, Status};
use crate::hash::{local_source_lines, source_hash};
use crate::operation::delete_adt::Opener;
use crate::operation::deploy::{CancelFlag, INTERRUPT_EXIT_CODE};
use crate::operation::quality::{self, QualityError};
use crate::operation::read_table::{ReadTableError, TableData, fetch};
use crate::operation::run_plan::{PlanFacts, RunArgs, RunPlan, plan_changed_text};
use crate::operation::runner::RunnerGated;
use crate::transport::rfc::{is_abap_runtime_failure, mentions_known_dump_text};
use crate::transport::{Request, Response, RunnerArm, SadtTransport, TransportError};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::time::Duration;

/// The classrun endpoint of ADT.
const CLASSRUN: &str = "/sap/bc/adt/oo/classrun";
/// The interface a class must implement to be run.
const CLASSRUN_INTERFACE: &str = "IF_OO_ADT_CLASSRUN";
/// The seconds the run `POST` may take over ADT HTTP when `--timeout` is not given.
pub const DEFAULT_TIMEOUT_SECS: u32 = 600;
/// The most superclasses the interface check follows.
const SUPERCLASS_DEPTH: usize = 8;
/// The line the first Ctrl-C prints while the class runs.
pub const WAIT_TEXT: &str =
    "I: run in progress on the server; waiting for the reply (Ctrl-C again to leave)";
/// Printed with every outcome that is not a dry run.
pub const NOT_A_DRY_RUN: &str =
    "W: run is not a dry run: data the code writes stays written (--simulate executes nothing)";
pub(crate) const RUN_FAILED: &str = "RUN_FAILED: runtime error; see ST22 (dumps)";
pub(crate) const RUN_UNKNOWN: &str = "RUN_UNKNOWN: the reply was lost; the code may have run";
const MESSAGE_LIMIT: usize = 200;

/// Why `--timeout` is refused on the RFC path.
pub const NO_RFC_TIMEOUT: &str = "--timeout is not supported over RFC: the RFC library has no call \
                                  timeout and no cancel, so a run waits until SAP answers";

/// What the facts of a class show, for the plan and for the human.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassFacts {
    pub package: String,
    pub author: String,
    pub hash: String,
    /// The program type of a report (`TRDIR-SUBC`); `None` for a class and a snippet.
    pub subc: Option<String>,
}

impl ClassFacts {
    /// What the plan binds. A snippet has no object, so its facts are empty and bind nothing; its
    /// own hash is bound by the arguments.
    pub fn plan_facts(&self) -> PlanFacts {
        PlanFacts {
            hash: Some(self.hash.clone()).filter(|hash| !hash.is_empty()),
            package: Some(self.package.clone()).filter(|package| !package.is_empty()),
            subc: self.subc.clone(),
        }
    }
}

/// How the facts could not be read: the system refuses (`REJECTED`), or a request failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Stop {
    /// The system says the class cannot be run; nothing ran. Exit 1.
    Rejected(String),
    /// A read failed with this class; a failure before anything was sent may fall back to RFC.
    Failed { failure: Failure, message: String },
}

/// What one attempt on one backend produced, with what the wiring needs to route.
#[derive(Clone, Debug)]
pub struct RunResult {
    pub outcome: Outcome,
    pub failure: Option<Failure>,
    /// Whether another backend may still take the run: only before the `POST` was sent.
    pub fallback_allowed: bool,
    pub detail: Option<String>,
    /// Set by a simulate that passed: the plan to write, with the backend of this attempt.
    pub plan: Option<RunPlan>,
    /// The facts a passed simulate read, for the confirmation of a human.
    pub facts: Option<ClassFacts>,
    /// The token of how a run that sent its `POST` ended; `None` for a simulate and a refusal.
    pub verdict: Option<Verdict>,
}

impl RunResult {
    pub(crate) fn refused(message: String) -> Self {
        Self::finished(
            Outcome {
                status: None,
                exit: 1,
                stdout: String::new(),
                stderr: format!("REJECTED: {message}"),
            },
            None,
        )
    }

    pub(crate) fn failed(failure: Failure, message: String) -> Self {
        Self {
            fallback_allowed: matches!(
                failure,
                Failure::PreSendConnectivity | Failure::CapabilityUnavailable
            ),
            failure: Some(failure),
            detail: Some(message.clone()),
            ..Self::finished(
                Outcome {
                    status: None,
                    exit: 1,
                    stdout: String::new(),
                    stderr: format!("ERROR: {message}"),
                },
                None,
            )
        }
    }

    pub(crate) fn finished(outcome: Outcome, verdict: Option<Verdict>) -> Self {
        Self {
            outcome,
            failure: None,
            fallback_allowed: false,
            detail: None,
            plan: None,
            facts: None,
            verdict,
        }
    }

    pub(crate) fn stopped(message: impl std::fmt::Display) -> Self {
        Self::finished(
            Outcome {
                status: None,
                exit: 2,
                stdout: String::new(),
                stderr: format!("STOP: {message}"),
            },
            None,
        )
    }

    pub(crate) fn from_stop(stop: Stop) -> Self {
        match stop {
            Stop::Rejected(message) => Self::refused(message),
            Stop::Failed { failure, message } => Self::failed(failure, message),
        }
    }
}

/// How a `POST` that was sent ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// The class answered; the reply is the output.
    Replied,
    /// A report or snippet ran to its end (`RAN`).
    Ran,
    /// The server refused the `POST`: the class did not run.
    Refused,
    /// A runtime error ended the class: rolled back by the server since its last commit.
    Failed,
    /// The reply is lost or an ICM error: the class may have run.
    Unknown,
}

impl Verdict {
    pub fn token(self) -> &'static str {
        match self {
            Self::Replied => "REPLY",
            Self::Ran => "RAN",
            Self::Refused => "REJECTED",
            Self::Failed => "RUN_FAILED",
            Self::Unknown => "RUN_UNKNOWN",
        }
    }
}

/// One `run CLAS` on one backend.
pub struct ClassRun<'a> {
    pub connection: &'a Connection,
    pub args: &'a RunArgs,
    pub backend: BackendKind,
    /// The seconds the `POST` may take over ADT HTTP; unused on RFC, where an explicit value was
    /// refused before the attempt.
    pub timeout: u32,
    pub cancel: &'a CancelFlag,
}

impl ClassRun<'_> {
    /// Reads the facts and builds the plan; nothing is executed.
    pub fn simulate(&self, transport: &mut dyn SadtTransport) -> RunResult {
        let facts = match self.inspect(transport) {
            Ok(facts) => facts,
            Err(stop) => return RunResult::from_stop(stop),
        };
        let name = &self.args.name;
        let connection = self.connection;
        let stdout = format!(
            "SIMULATED_OK run CLAS {name} package {} author {}\n\
             HASH {}\n\
             CLIENT {} (not productive)\n\
             BACKEND {}\n\
             {NOT_A_DRY_RUN}",
            facts.package,
            if facts.author.is_empty() {
                "-"
            } else {
                &facts.author
            },
            facts.hash,
            connection.client,
            self.backend.token(),
        );
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
                    status: Some(Status::SimulatedOk),
                    exit: 0,
                    stdout,
                    stderr: String::new(),
                },
                None,
            )
        }
    }

    /// Reads the facts again, compares them with the plan when there is one, and sends the
    /// `POST` from a new session. `open` yields that session.
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
                return RunResult::stopped(crate::operation::run_plan::plan_mismatch_text(
                    "backend",
                ));
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
        let mut result = self.post(open);

        if let Some(note) = note {
            result.outcome.stderr = join_lines([result.outcome.stderr.as_str(), note.as_str()]);
        }

        result.facts = Some(facts);

        result
    }

    /// The single classrun `POST` of the class, from a new session.
    fn post(&self, open: &mut Opener<'_>) -> RunResult {
        let name = &self.args.name;
        let arm = match RunnerArm::arm_class(name, &self.connection.runner_prefix) {
            Ok(arm) => arm,
            Err(message) => return RunResult::stopped(format!("run admission: {message}")),
        };
        let mut session = match open() {
            Ok(session) => session,
            Err(error) => {
                return RunResult::failed(
                    Failure::from(&error),
                    format!("cannot open the session for the classrun POST: {error}"),
                );
            }
        };
        let mut request = Request::post(format!("{CLASSRUN}/{name}"), Vec::new());

        request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());

        let reply = {
            let _running = self.cancel.hold_run(WAIT_TEXT);

            if self.backend == BackendKind::AdtHttp {
                let limit = Duration::from_secs(u64::from(self.timeout));

                RunnerGated::new(&mut *session).request_within(request, limit)
            } else {
                session.request(request)
            }
        };
        let close = session
            .close()
            .err()
            .map(|error| format!("W: session close failed: {error}"));

        drop(arm);

        let interrupted = self.cancel.run_interrupted();
        let mut result = reply_result(reply, name, &self.connection.system_id);

        if interrupted && result.outcome.exit == 0 {
            result.outcome.exit = INTERRUPT_EXIT_CODE;
            result.outcome.stderr = join_lines([
                result.outcome.stderr.as_str(),
                "I: interrupted; the run had finished before the reply was read",
            ]);
        }

        if let Some(close) = close {
            result.outcome.stderr = join_lines([result.outcome.stderr.as_str(), close.as_str()]);
        }

        result
    }

    /// The facts of the class and the checks that come before them, on one session. The client is
    /// checked first: a productive client refuses every run.
    fn inspect(&self, transport: &mut dyn SadtTransport) -> Result<ClassFacts, Stop> {
        let name = self.args.name.as_str();

        check_client(transport, &self.connection.client)?;

        let uri = ObjectKind::Clas
            .object_uri(name, None)
            .map_err(Stop::Rejected)?;
        let response = get(transport, &uri, "application/*")?;

        match response.status {
            200 => {}
            404 => return Err(Stop::Rejected(format!("class {name} does not exist"))),
            status => {
                return Err(Stop::Rejected(format!(
                    "class {name} cannot be read: HTTP {status}"
                )));
            }
        }

        let xml = String::from_utf8_lossy(&response.body).into_owned();

        if object_version(&xml).as_deref() == Some("inactive") {
            return Err(Stop::Rejected(format!(
                "class {name} is not active; activate it first"
            )));
        }

        let package = package_of(&xml)
            .map_err(|error| Stop::Rejected(format!("class {name}: {error}")))?
            .ok_or_else(|| Stop::Rejected(format!("the package of class {name} is unknown")))?;
        let author = responsible_of(&xml)
            .map_err(|error| Stop::Rejected(format!("class {name}: {error}")))?
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
                    "class {name} has no active version; activate it first"
                )));
            }
            status => {
                return Err(Stop::Rejected(format!(
                    "the active source of class {name} cannot be read: HTTP {status}"
                )));
            }
        }

        let hash = source_hash(&local_source_lines(&String::from_utf8_lossy(&source.body)));

        if !implements_classrun(transport, name)? {
            return Err(Stop::Rejected(format!(
                "class {name} does not implement {CLASSRUN_INTERFACE}"
            )));
        }

        Ok(ClassFacts {
            package,
            author,
            hash,
            subc: None,
        })
    }
}

pub(crate) fn join_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn get(
    transport: &mut dyn SadtTransport,
    uri: &str,
    accept: &str,
) -> Result<Response, Stop> {
    let mut request = Request::get(uri);

    request
        .headers
        .insert("Accept".to_owned(), accept.to_owned());

    let response = transport.request(request).map_err(|error| Stop::Failed {
        failure: Failure::from(&error),
        message: error.to_string(),
    })?;

    match response.authorization_failure() {
        Some(error) => Err(Stop::Failed {
            failure: Failure::from(&error),
            message: error.to_string(),
        }),
        None => Ok(response),
    }
}

/// The `adtcore:version` of the root element of an object's metadata, when it names one.
pub(crate) fn object_version(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event().ok()? {
            Event::Start(element) | Event::Empty(element) => {
                return element.attributes().flatten().find_map(|attribute| {
                    (attribute.key.local_name().into_inner() == "version")
                        .then(|| attribute.value.trim().to_ascii_lowercase())
                });
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Refuses a productive client or one whose role cannot be read: the one fail-closed T000 check
/// of `quality`, mapped to what a run reports.
pub(crate) fn check_client(transport: &mut dyn SadtTransport, client: &str) -> Result<(), Stop> {
    quality::check_client(transport, client).map_err(|error| match error {
        QualityError::Rejected(message) => Stop::Rejected(message),
        QualityError::Failed { failure, message } => Stop::Failed { failure, message },
    })
}

/// Whether the class, or a superclass of it, implements the classrun interface: `SEOMETAREL`
/// holds the interface of a class and its superclass.
fn implements_classrun(transport: &mut dyn SadtTransport, name: &str) -> Result<bool, Stop> {
    let mut current = name.to_owned();

    for _ in 0..=SUPERCLASS_DEPTH {
        if !current.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'/'
        }) {
            return Err(Stop::Rejected(format!(
                "{current:?} is not a class name for a repository preview"
            )));
        }

        let direct = preview(
            transport,
            "SEOMETAREL",
            &format!(
                "SELECT REFCLSNAME FROM SEOMETAREL WHERE CLSNAME IN ( '{current}' ) AND \
                 REFCLSNAME IN ( '{CLASSRUN_INTERFACE}' )"
            ),
        )?;

        if !direct.rows.is_empty() {
            return Ok(true);
        }

        let parent = preview(
            transport,
            "SEOMETAREL",
            &format!(
                "SELECT REFCLSNAME FROM SEOMETAREL WHERE CLSNAME IN ( '{current}' ) AND RELTYPE IN ( '2' )"
            ),
        )?;
        let next = column(&parent, "REFCLSNAME")
            .and_then(|index| parent.rows.first().and_then(|row| row.get(index)))
            .map(|value| value.trim().to_ascii_uppercase())
            .filter(|value| !value.is_empty());

        match next {
            Some(next) => current = next,
            None => return Ok(false),
        }
    }

    Ok(false)
}

pub(crate) fn column(data: &TableData, name: &str) -> Option<usize> {
    data.columns.iter().position(|column| column == name)
}

/// One data preview of a table this module names; a failure to connect or log on is a failed
/// attempt, anything else a refusal with the table named.
pub(crate) fn preview(
    transport: &mut dyn SadtTransport,
    table: &str,
    body: &str,
) -> Result<TableData, Stop> {
    fetch(transport, table, body, 5).map_err(|error| match error {
        ReadTableError::Failed {
            failure: failure @ (Failure::PreSendConnectivity | Failure::Authorization),
            message,
        } => Stop::Failed { failure, message },
        ReadTableError::Rejected(message) | ReadTableError::Failed { message, .. } => {
            Stop::Rejected(format!("cannot read {table}: {message}"))
        }
    })
}

/// The result of a classrun `POST`: the reply as the output, or the outcome token the failure
/// maps to. `name` and `sid` only word the messages.
pub fn reply_result(reply: Result<Response, TransportError>, name: &str, sid: &str) -> RunResult {
    let sent = |verdict: Verdict, exit: i32, stdout: String, stderr: String| {
        RunResult::finished(
            Outcome {
                status: None,
                exit,
                stdout,
                stderr,
            },
            Some(verdict),
        )
    };

    match reply {
        Ok(response) if (200..300).contains(&response.status) => sent(
            Verdict::Replied,
            0,
            String::from_utf8_lossy(&response.body).into_owned(),
            String::new(),
        ),
        Ok(response) if response.status == 404 => RunResult::stopped(format!(
            "run CLAS needs ADT classrun; not available on {sid}"
        )),
        Ok(response) if matches!(response.status, 401 | 403) => sent(
            Verdict::Refused,
            1,
            String::new(),
            format!(
                "REJECTED: the classrun POST of {name} was refused: HTTP {}; the class did not run",
                response.status
            ),
        ),
        Ok(response) if (400..500).contains(&response.status) => sent(
            Verdict::Refused,
            1,
            String::new(),
            format!(
                "REJECTED: the classrun POST of {name} was refused: HTTP {}{}; the class did not run",
                response.status,
                reply_text(&response)
            ),
        ),
        Ok(response) if response.status == 500 && is_runtime_error_body(&response.body) => {
            sent(Verdict::Failed, 1, String::new(), RUN_FAILED.to_owned())
        }
        Ok(response) => sent(
            Verdict::Unknown,
            1,
            String::new(),
            format!(
                "{RUN_UNKNOWN}\nW: the classrun reply was HTTP {}",
                response.status
            ),
        ),
        Err(error) if is_abap_runtime_failure(&error) => {
            sent(Verdict::Failed, 1, String::new(), RUN_FAILED.to_owned())
        }
        Err(error) if error.was_sent() => sent(
            Verdict::Unknown,
            1,
            String::new(),
            format!("{RUN_UNKNOWN}\nW: {}", cut(&error.to_string())),
        ),
        Err(error) => RunResult::finished(
            Outcome {
                status: None,
                exit: 1,
                stdout: String::new(),
                stderr: format!(
                    "ERROR: the classrun POST of {name} was not sent: {}",
                    cut(&error.to_string())
                ),
            },
            None,
        ),
    }
}

/// The first line of the reply as a short quoted text for a refusal, or nothing.
fn reply_text(response: &Response) -> String {
    let text = String::from_utf8_lossy(&response.body);
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('<'))
        .unwrap_or_default();

    if line.is_empty() {
        String::new()
    } else {
        format!(" ({})", cut(line))
    }
}

pub(crate) fn cut(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();

    if clean.chars().count() <= MESSAGE_LIMIT {
        return clean;
    }

    let mut cut: String = clean.chars().take(MESSAGE_LIMIT).collect();

    cut.push_str("...");
    cut
}

/// Whether the body of an HTTP 500 is the report of an ABAP runtime error (a short dump), as
/// opposed to a gateway or ICM error that leaves the work process running.
///
/// The generic markers are loose: any 500 body that mentions `st22` or `runtime error` (in a link,
/// a header or ICM text) counts, and the run then reads `RUN_FAILED` ("rolled back") where
/// `RUN_UNKNOWN` would be the safer word. The exit code is the same for both.
///
/// The live probe (2026-11-22, SAP_BASIS 7.50) showed that a dump of a report is not marked by
/// them: SAP answers the standard ICF "Application Server Error" page whose text is only the short
/// text of the dump (`Screen output without connection to user.`, fixture
/// `tests/fixtures/m8/classrun_500_screen_output.html`). The generic markers stay; the known short
/// texts of [`crate::transport::rfc::KNOWN_DUMP_SHORT_TEXTS`] are added to them.
///
/// The SAP_BASIS 758 ADT page of a screen dump is covered too: it carries the text `Sending of
/// dynpro <program> <screen> not possible: No window system type specified` (fixture
/// `tests/fixtures/m8/classrun_500_dynpro_send_758.html`).
pub fn is_runtime_error_body(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    let lower = text.to_ascii_lowercase();

    ["abap runtime error", "runtime error", "short dump", "st22"]
        .iter()
        .any(|marker| lower.contains(marker))
        || mentions_known_dump_text(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::read_table::validate_preview_body;

    #[test]
    fn the_previews_of_the_run_pass_the_data_preview_grammar() {
        validate_preview_body("T000", &quality::client_query("100")).unwrap();
        validate_preview_body(
            "SEOMETAREL",
            "SELECT REFCLSNAME FROM SEOMETAREL WHERE CLSNAME IN ( 'ZCL_A' ) AND REFCLSNAME IN ( 'IF_OO_ADT_CLASSRUN' )",
        )
        .unwrap();
        validate_preview_body(
            "SEOMETAREL",
            "SELECT REFCLSNAME FROM SEOMETAREL WHERE CLSNAME IN ( 'ZCL_A' ) AND RELTYPE IN ( '2' )",
        )
        .unwrap();
    }

    #[test]
    fn the_object_version_is_read_from_the_root_element() {
        let active = r#"<class:abapClass xmlns:class="c" xmlns:adtcore="a" adtcore:version="active" adtcore:name="X"/>"#;
        let inactive = r#"<?xml version="1.0"?><class:abapClass xmlns:adtcore="a" adtcore:version="Inactive"><x adtcore:version="active"/></class:abapClass>"#;

        assert_eq!(object_version(active).as_deref(), Some("active"));
        assert_eq!(object_version(inactive).as_deref(), Some("inactive"));
        assert_eq!(object_version("<a/>"), None);
        assert_eq!(object_version(""), None);
    }

    #[test]
    fn a_runtime_error_body_is_told_from_an_icm_error() {
        assert!(is_runtime_error_body(
            b"<html>ABAP Runtime Error: RAISE_EXCEPTION"
        ));
        assert!(is_runtime_error_body(
            b"500 SAP Internal Server Error ... see transaction ST22"
        ));
        assert!(!is_runtime_error_body(
            b"<html>503 Service Unavailable: ICM timeout while waiting for the application</html>"
        ));
        assert!(!is_runtime_error_body(b""));
    }

    #[test]
    fn the_screen_output_dump_page_is_a_runtime_error_body() {
        let page = include_bytes!("../../tests/fixtures/m8/classrun_500_screen_output.html");

        assert!(is_runtime_error_body(page));
        assert!(is_runtime_error_body(
            b"<span>SCREEN OUTPUT WITHOUT CONNECTION TO USER.</span>"
        ));
        // An ICM-style error page without the dump text stays a lost reply.
        assert!(!is_runtime_error_body(
            b"<html><title>Application Server Error</title><p>ICM: connection to the work process was reset.</p></html>"
        ));
    }

    #[test]
    fn join_lines_drops_the_empty_lines_and_joins_the_rest_with_line_feeds() {
        assert_eq!(join_lines(["a", "", "b", ""]), "a\nb");
        assert_eq!(join_lines(["only"]), "only");
        assert_eq!(join_lines(["", ""]), "");
        assert_eq!(join_lines([]), "");
    }

    #[test]
    fn a_failed_attempt_keeps_its_message_as_the_detail_and_falls_back_only_before_a_send() {
        let result = RunResult::failed(Failure::PreSendConnectivity, "no route to host".to_owned());

        assert_eq!(result.detail.as_deref(), Some("no route to host"));
        assert_eq!(result.failure, Some(Failure::PreSendConnectivity));
        assert!(result.fallback_allowed);
        assert_eq!(result.outcome.exit, 1);
        assert_eq!(result.outcome.stderr, "ERROR: no route to host");

        let sent = RunResult::failed(Failure::Initial, "reset".to_owned());

        assert_eq!(sent.detail.as_deref(), Some("reset"));
        assert!(!sent.fallback_allowed);
    }

    fn reply(status: u16, body: &str) -> RunResult {
        reply_result(Ok(Response::text(status, body)), "ZCL_A", "DEV")
    }

    #[test]
    fn a_2xx_reply_is_printed_as_is_with_exit_0() {
        let result = reply(200, "line 1\n  indented\n");

        assert_eq!(result.outcome.exit, 0);
        assert_eq!(result.outcome.stdout, "line 1\n  indented\n");
        assert_eq!(result.outcome.stderr, "");
        assert_eq!(result.verdict, Some(Verdict::Replied));
        assert!(!result.fallback_allowed);
    }

    #[test]
    fn a_missing_classrun_is_a_stop_and_a_refusal_ran_nothing() {
        let missing = reply(404, "");

        assert_eq!(missing.outcome.exit, 2);
        assert_eq!(
            missing.outcome.stderr,
            "STOP: run CLAS needs ADT classrun; not available on DEV"
        );

        for status in [401, 403, 400, 405, 409] {
            let refused = reply(status, "");

            assert_eq!(refused.outcome.exit, 1, "{status}");
            assert!(
                refused.outcome.stderr.starts_with("REJECTED: "),
                "{status}: {}",
                refused.outcome.stderr
            );
            assert!(refused.outcome.stderr.contains("did not run"));
            assert_eq!(refused.verdict, Some(Verdict::Refused));
        }

        assert!(
            reply(
                400,
                "<xml/>\nClass does not implement if_oo_adt_classrun~main method"
            )
            .outcome
            .stderr
            .contains("(Class does not implement")
        );
    }

    #[test]
    fn a_runtime_error_is_run_failed_and_every_other_5xx_is_run_unknown() {
        let failed = reply(500, "<html>ABAP runtime error; see ST22</html>");

        assert_eq!(failed.outcome.exit, 1);
        assert_eq!(
            failed.outcome.stderr,
            "RUN_FAILED: runtime error; see ST22 (dumps)"
        );
        assert_eq!(failed.verdict, Some(Verdict::Failed));
        assert_eq!(failed.outcome.stdout, "");

        for (status, body) in [
            (500, "internal server error"),
            (502, "bad gateway"),
            (503, "ICM timeout"),
            // A 502 whose body mentions a runtime error is a gateway, not a dump of ours.
            (502, "runtime error"),
            (504, ""),
        ] {
            let unknown = reply(status, body);

            assert_eq!(unknown.outcome.exit, 1, "{status}");
            assert!(
                unknown
                    .outcome
                    .stderr
                    .starts_with("RUN_UNKNOWN: the reply was lost; the code may have run"),
                "{status}: {}",
                unknown.outcome.stderr
            );
            assert_eq!(unknown.verdict, Some(Verdict::Unknown));
        }
    }

    #[test]
    fn a_lost_reply_is_run_unknown_and_an_unsent_request_is_an_error() {
        let lost = reply_result(
            Err(crate::transport::http::HttpTransport::timeout_after_send()),
            "ZCL_A",
            "DEV",
        );

        assert_eq!(lost.outcome.exit, 1);
        assert!(lost.outcome.stderr.starts_with("RUN_UNKNOWN: "));
        assert!(
            lost.outcome
                .stderr
                .contains("HTTP timeout after request send")
        );

        let sent_then_refused = reply_result(
            Err(TransportError::Http {
                status: 502,
                message: "x".to_owned(),
            }
            .after_send()),
            "ZCL_A",
            "DEV",
        );

        assert_eq!(sent_then_refused.verdict, Some(Verdict::Unknown));

        let unsent = reply_result(
            Err(TransportError::PreSendConnectivity("refused".to_owned())),
            "ZCL_A",
            "DEV",
        );

        assert_eq!(unsent.outcome.exit, 1);
        assert!(
            unsent
                .outcome
                .stderr
                .starts_with("ERROR: the classrun POST of ZCL_A was not sent")
        );
        assert_eq!(unsent.verdict, None);
    }

    #[test]
    fn the_758_dynpro_send_page_is_a_runtime_error_body_and_run_failed() {
        let page = include_str!("../../tests/fixtures/m8/classrun_500_dynpro_send_758.html");

        assert!(is_runtime_error_body(page.as_bytes()));

        let failed = reply(500, page);

        assert_eq!(failed.outcome.stderr, RUN_FAILED);
        assert_eq!(failed.verdict, Some(Verdict::Failed));
        // The program-specific start alone is not enough.
        assert!(!is_runtime_error_body(b"Sending of dynpro ZREPORT 1001"));
    }

    #[test]
    fn the_screen_output_dump_page_is_run_failed_and_an_icm_page_is_run_unknown() {
        let page = include_str!("../../tests/fixtures/m8/classrun_500_screen_output.html");
        let failed = reply(500, page);

        assert_eq!(failed.outcome.stderr, RUN_FAILED);
        assert_eq!(failed.verdict, Some(Verdict::Failed));

        let icm = reply(
            500,
            "<html><title>Application Server Error</title><p>ICM: the work process did not answer.</p></html>",
        );

        assert!(icm.outcome.stderr.starts_with(RUN_UNKNOWN));
        assert_eq!(icm.verdict, Some(Verdict::Unknown));
    }

    #[test]
    fn an_rfc_runtime_failure_is_run_failed() {
        let failure = TransportError::Protocol("call failed (RFC code 3)".to_owned()).after_send();
        let result = reply_result(Err(failure), "ZCL_A", "DEV");

        assert_eq!(result.outcome.stderr, RUN_FAILED);
        assert_eq!(result.verdict, Some(Verdict::Failed));

        // Another ABAP exception is not a runtime error.
        let other = TransportError::Protocol("call failed (RFC code 5)".to_owned()).after_send();

        assert_eq!(
            reply_result(Err(other), "ZCL_A", "DEV").verdict,
            Some(Verdict::Unknown)
        );
    }

    #[test]
    fn an_rfc_closed_with_a_known_dump_text_is_run_failed_and_any_other_is_run_unknown() {
        use crate::transport::rfc::{RfcPhase, classify_rfc_error};

        let screen = classify_rfc_error(
            RfcPhase::Invoke,
            6,
            "Screen output without connection to user.",
        );
        let result = reply_result(Err(screen), "ZREPORT", "DEV");

        assert_eq!(result.outcome.stderr, RUN_FAILED);
        assert_eq!(result.verdict, Some(Verdict::Failed));

        let lost = classify_rfc_error(RfcPhase::Invoke, 6, "connection closed");
        let result = reply_result(Err(lost), "ZREPORT", "DEV");

        assert!(result.outcome.stderr.starts_with(RUN_UNKNOWN));
        assert_eq!(result.verdict, Some(Verdict::Unknown));
    }
}
