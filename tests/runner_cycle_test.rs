//! The runner class cycle (M6 Task 14) against a fake SAP that keeps class state and injects one
//! failure at a chosen step. ADT HTTP sessions are real `HttpTransport`s over the fake, so the
//! logoff requests are the ones the product builds; RFC-SADT sessions are fakes that apply the
//! same runner rules as the admission layer. No SAP.

use abapreach::backend::BackendKind;
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::runner::{
    CycleInput, PostRoute, PostTransport, Row, RunnerEnd, RunnerRun, post_transport, run_cycle,
    run_rows,
};
use abapreach::transport::http::HttpTransport;
use abapreach::transport::rfc::{FakeRfcApi, RfcTransport};
use abapreach::transport::{Request, Response, SadtTransport, TransportError, runner_verdict};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

mod common;

use common::preview;

const PREFIX: &str = "YTEST_ABAPREACH_RUN_";
const SOURCE: &str = "CLASS <N> DEFINITION.\nENDCLASS.\n";
const LOGOFF: &str = "/sap/public/bc/icf/logoff";
const HINT: &str =
    "the temporary runner class needs CLAS create/activate in $TMP and ADT classrun access";
const LOCK_BODY: &str = concat!(
    r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
    r#"<LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>"#,
    r#"</DATA></asx:values></asx:abap>"#,
);
const MESSAGES_E: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="1"><shortText><txt>broken</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_NO_INTERFACE: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="2"><shortText><txt>Interface IF_OO_ADT_CLASSRUN is unknown</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_OK: &str = r#"<chkl:messages xmlns:chkl="x"></chkl:messages>"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Preflight,
    Preview,
    Create,
    Lock,
    Put,
    Unlock,
    Activate,
    GetSource,
    Post,
    Delete,
    Proof,
    Logoff,
}

#[derive(Clone, Debug)]
enum Fault {
    /// The request is refused and changes nothing.
    Status(u16, &'static str),
    /// The request runs, then its reply is lost.
    Lost,
    /// The request is lost before it runs.
    LostBefore,
    /// The request never leaves the process.
    NotSent,
    /// The transport refuses the logon and answers with an error instead of a reply.
    Unauthorized,
}

#[derive(Clone, Copy, Debug)]
enum Signal {
    /// The Ctrl-C handler path, shared by SIGINT, SIGTERM, and SIGHUP.
    Handler,
    /// The closed MCP input.
    Shutdown,
}

struct Class {
    active: String,
}

#[derive(Default)]
struct Sap {
    classes: BTreeMap<String, Class>,
    /// `(session, "METHOD uri")` of every request.
    log: Vec<(usize, String)>,
    counts: BTreeMap<String, usize>,
    sessions: usize,
    /// RFC sessions that were closed.
    closes: usize,
    stateful: bool,
    faults: Vec<(Step, usize, Fault)>,
    signal: Option<(Step, Signal, CancelFlag)>,
    /// The active source differs from what was written.
    corrupt_source: bool,
    /// Activation answer; `None` means no message.
    activation: Option<&'static str>,
    /// The class survives a DELETE that answered 2xx.
    delete_is_ignored: bool,
    /// A session that ran the POST is dead afterwards: every later request on it fails.
    dies_after_post: bool,
    dead: Vec<usize>,
    /// `EDITFLAG` of the customer namespace; `None` answers no row.
    namespace_flag: Option<&'static str>,
}

type Shared = Rc<RefCell<Sap>>;

fn step_of(request: &Request) -> Option<Step> {
    let (path, query) = request
        .uri
        .split_once('?')
        .unwrap_or((request.uri.as_str(), ""));

    Some(match (request.method.as_str(), path) {
        ("GET", LOGOFF) => Step::Logoff,
        ("GET", "/sap/bc/adt/discovery") => Step::Preflight,
        ("POST", "/sap/bc/adt/datapreview/ddic") => Step::Preview,
        ("POST", "/sap/bc/adt/oo/classes") => Step::Create,
        ("POST", "/sap/bc/adt/activation") => Step::Activate,
        ("POST", path) if path.starts_with("/sap/bc/adt/oo/classrun/") => Step::Post,
        ("POST", _) if query.contains("_action=LOCK") => Step::Lock,
        ("POST", _) if query.contains("_action=UNLOCK") => Step::Unlock,
        ("PUT", _) => Step::Put,
        ("DELETE", _) => Step::Delete,
        ("GET", path) if path.ends_with("/source/main") => Step::GetSource,
        ("GET", path) if path.starts_with("/sap/bc/adt/oo/classes/") => Step::Proof,
        _ => return None,
    })
}

fn class_of(request: &Request) -> String {
    let path = request.uri.split('?').next().unwrap_or_default();

    path.trim_start_matches("/sap/bc/adt/oo/classes/")
        .trim_start_matches("/sap/bc/adt/oo/classrun/")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

impl Sap {
    fn handle(
        &mut self,
        session: usize,
        request: Request,
        admit: bool,
    ) -> Result<Response, TransportError> {
        self.log
            .push((session, format!("{} {}", request.method, request.uri)));

        if admit && runner_verdict(&request) == Some(false) {
            return Err(TransportError::RequestNotAllowed(request.uri));
        }

        let step = step_of(&request).unwrap_or_else(|| panic!("unexpected {request:?}"));
        let seen = {
            let count = self.counts.entry(format!("{step:?}")).or_default();

            *count += 1;
            *count
        };

        if let Some((at, signal, cancel)) = &self.signal
            && *at == step
        {
            match signal {
                Signal::Handler => {
                    assert_eq!(
                        cancel.on_interrupt(),
                        abapreach::operation::deploy::InterruptAction::Deferred,
                        "the process must not exit while a runner class exists"
                    );
                }
                Signal::Shutdown => cancel.request_shutdown(),
            }
        }

        if self.dead.contains(&session) && step != Step::Logoff {
            return Ok(Response::text(500, "session expired"));
        }

        let fault = self
            .faults
            .iter()
            .find(|(at, nth, _)| *at == step && *nth == seen)
            .map(|(_, _, fault)| fault.clone());

        match fault {
            Some(Fault::Status(status, body)) => {
                return Ok(self.reply(Response::text(status, body)));
            }
            Some(Fault::LostBefore) => {
                return Err(TransportError::PostSendUncertain("reply lost".to_owned()));
            }
            Some(Fault::NotSent) => {
                return Err(TransportError::PreSendConnectivity(
                    "unreachable".to_owned(),
                ));
            }
            Some(Fault::Unauthorized) => {
                return Err(TransportError::Authorization("HTTP 401".to_owned()));
            }
            _ => {}
        }

        let response = self.execute(step, &request);

        if step == Step::Post && self.dies_after_post {
            self.dead.push(session);
        }

        if matches!(fault, Some(Fault::Lost)) {
            return Err(TransportError::PostSendUncertain("reply lost".to_owned()));
        }

        Ok(response)
    }

    fn execute(&mut self, step: Step, request: &Request) -> Response {
        let name = class_of(request);

        match step {
            Step::Preflight => self.reply(Response::text(200, "<intf/>")),
            Step::Preview => {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                let none: &[Option<&str>] = &[];
                let xml = if body.contains(" FROM T000 ") {
                    preview(&[
                        ("CCCATEGORY", &[Some("C")]),
                        ("CCCORACTIV", &[Some("1")]),
                        ("CCNOCLIIND", &[None]),
                    ])
                } else if body.contains(" FROM TRNSPACE ") {
                    match self.namespace_flag {
                        Some(flag) => preview(&[("EDITFLAG", &[Some(flag)])]),
                        None => preview(&[("EDITFLAG", none)]),
                    }
                } else {
                    panic!("unexpected preview {body}");
                };

                self.reply(Response::text(200, xml))
            }
            Step::Create => {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                let name = body
                    .split("adtcore:name=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .expect("name in create body")
                    .to_owned();

                if self.classes.contains_key(&name) {
                    return self.reply(Response::text(409, "exists"));
                }

                self.classes.insert(
                    name,
                    Class {
                        active: String::new(),
                    },
                );

                self.reply(Response::text(201, ""))
            }
            Step::Lock => self.reply(Response::text(200, LOCK_BODY)),
            Step::Put => {
                let source = String::from_utf8_lossy(&request.body).into_owned();

                if let Some(class) = self.classes.get_mut(&name) {
                    class.active = if self.corrupt_source {
                        format!("{source}-changed")
                    } else {
                        source
                    };
                }

                self.reply(Response::text(200, ""))
            }
            Step::Unlock => self.reply(Response::text(200, "")),
            Step::Activate => {
                self.reply(Response::text(200, self.activation.unwrap_or(MESSAGES_OK)))
            }
            Step::GetSource => match self.classes.get(&name) {
                Some(class) => self.reply(Response::text(200, &class.active)),
                None => self.reply(Response::text(404, "")),
            },
            Step::Post => self.reply(Response::text(200, "run output")),
            Step::Delete => {
                if !self.delete_is_ignored {
                    self.classes.remove(&name);
                }

                self.reply(Response::text(200, ""))
            }
            Step::Proof => {
                if self.classes.contains_key(&name) {
                    self.reply(Response::text(200, "<class/>"))
                } else {
                    self.reply(Response::text(404, ""))
                }
            }
            Step::Logoff => Response::text(200, ""),
        }
    }

    /// A stateful server sets the context cookie on every reply except a refused logon.
    fn reply(&self, mut response: Response) -> Response {
        if self.stateful && !matches!(response.status, 401 | 403) {
            response
                .headers
                .insert("Set-Cookie".to_owned(), "sap-contextid=ctx".to_owned());
        }

        response
    }

    fn count(&self, step: Step) -> usize {
        self.log
            .iter()
            .filter(|(_, line)| {
                let (method, uri) = line.split_once(' ').unwrap();

                step_of(&Request::new(method, uri, Vec::new())) == Some(step)
            })
            .count()
    }

    fn lines(&self) -> Vec<String> {
        self.log.iter().map(|(_, line)| line.clone()).collect()
    }

    fn on_session(&self, session: usize, step: Step) -> usize {
        self.log
            .iter()
            .filter(|(id, line)| {
                let (method, uri) = line.split_once(' ').unwrap();

                *id == session && step_of(&Request::new(method, uri, Vec::new())) == Some(step)
            })
            .count()
    }
}

struct Session {
    sap: Shared,
    id: usize,
    admit: bool,
}

impl SadtTransport for Session {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.sap.borrow_mut().handle(self.id, request, self.admit)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.sap.borrow_mut().closes += 1;

        Ok(())
    }
}

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

fn session(sap: &Shared, admit: bool) -> Session {
    let mut state = sap.borrow_mut();

    state.sessions += 1;

    Session {
        sap: sap.clone(),
        id: state.sessions,
        admit,
    }
}

fn http_session(sap: &Shared) -> Opened {
    Ok(Box::new(HttpTransport::wrapping(session(sap, false))))
}

fn rfc_session(sap: &Shared) -> Opened {
    Ok(Box::new(session(sap, true)))
}

fn new_sap(stateful: bool) -> Shared {
    Rc::new(RefCell::new(Sap {
        stateful,
        namespace_flag: Some("X"),
        ..Sap::default()
    }))
}

fn build(name: &str) -> Result<String, String> {
    Ok(SOURCE.replace("<N>", name))
}

fn input(backend: BackendKind, rfc_available: bool) -> CycleInput<'static> {
    CycleInput {
        operation: "read DTEL",
        sid: "DEV",
        client: "100",
        conn: "dev",
        prefix: PREFIX,
        user: "DEVUSER",
        language: "EN",
        backend,
        rfc_available,
        build: &build,
    }
}

/// One row over ADT HTTP.
fn http_row(sap: &Shared, cancel: &CancelFlag) -> RunnerRun {
    let mut open = || http_session(sap);

    run_cycle(
        &input(BackendKind::AdtHttp, true),
        &mut open,
        PostRoute::Cycle,
        cancel,
    )
}

/// One row over RFC-SADT.
fn rfc_row(sap: &Shared, cancel: &CancelFlag) -> RunnerRun {
    let mut open = || rfc_session(sap);

    run_cycle(
        &input(BackendKind::RfcSadt, true),
        &mut open,
        PostRoute::Cycle,
        cancel,
    )
}

fn fault(sap: &Shared, step: Step, nth: usize, fault: Fault) {
    sap.borrow_mut().faults.push((step, nth, fault));
}

fn signal(sap: &Shared, step: Step, signal: Signal, cancel: &CancelFlag) {
    sap.borrow_mut().signal = Some((step, signal, cancel.clone()));
}

#[track_caller]
fn assert_clean(sap: &Shared, run: &RunnerRun) {
    assert!(sap.borrow().classes.is_empty(), "class left: {:?}", run.end);
    assert!(run.leftover.is_none(), "{:?}", run.leftover);
}

fn rejected(run: &RunnerRun) -> &str {
    match &run.end {
        RunnerEnd::Rejected(text) => text,
        other => panic!("expected REJECTED, got {other:?}"),
    }
}

// --- the normal cycle -----------------------------------------------------------------------

#[test]
fn the_http_cycle_runs_every_step_in_order_and_removes_the_class() {
    let sap = new_sap(true);
    let run = http_row(&sap, &CancelFlag::new());
    let sap = sap.borrow();
    let name = run.name.clone().expect("a runner name");

    assert!(
        name.starts_with(PREFIX) && name.len() == PREFIX.len() + 8,
        "{name}"
    );
    assert!(matches!(&run.end, RunnerEnd::Posted(response) if response.status == 200));
    assert!(sap.classes.is_empty());
    assert!(run.leftover.is_none() && !run.interrupted);

    let lines = sap.lines();
    let object = format!("/sap/bc/adt/oo/classes/{}", name.to_ascii_lowercase());
    let expected = [
        "GET /sap/bc/adt/discovery".to_owned(),
        "POST /sap/bc/adt/datapreview/ddic?rowNumber=1&ddicEntityName=T000".to_owned(),
        "POST /sap/bc/adt/datapreview/ddic?rowNumber=1&ddicEntityName=TRNSPACE".to_owned(),
        "POST /sap/bc/adt/oo/classes".to_owned(),
        format!("POST {object}?_action=LOCK&accessMode=MODIFY"),
        format!("PUT {object}/source/main?lockHandle=h%2B1%2F%3D"),
        format!("POST {object}?_action=UNLOCK&lockHandle=h%2B1%2F%3D"),
        "POST /sap/bc/adt/activation?method=activate&preauditRequested=true".to_owned(),
        format!("GET {object}/source/main?version=active"),
        format!("POST /sap/bc/adt/oo/classrun/{name}"),
        format!("POST {object}?_action=LOCK&accessMode=MODIFY"),
        format!("DELETE {object}?lockHandle=h%2B1%2F%3D"),
        format!("GET {LOGOFF}"),
        format!("GET {object}"),
        format!("GET {LOGOFF}"),
    ];

    assert_eq!(lines, expected);
    assert_eq!(
        run.runner_line().as_deref(),
        Some(
            format!("RUNNER: {name} (temporary, $TMP, post via http, removed after the run)")
                .as_str()
        )
    );
}

#[test]
fn the_class_is_created_in_the_local_package_without_a_request() {
    let sap = new_sap(true);

    http_row(&sap, &CancelFlag::new());

    // The admission rule would refuse any other create; the body is the one `create` builds.
    let log = sap.borrow().lines();

    assert!(log.contains(&"POST /sap/bc/adt/oo/classes".to_owned()));
    assert!(log.iter().all(|line| !line.contains("corrNr")));
}

#[test]
fn the_rfc_cycle_needs_no_logoff_and_closes_its_connections() {
    let sap = new_sap(false);
    let run = rfc_row(&sap, &CancelFlag::new());
    let sap = sap.borrow();

    assert!(matches!(&run.end, RunnerEnd::Posted(_)));
    assert!(sap.classes.is_empty());
    assert_eq!(sap.count(Step::Logoff), 0);
    assert_eq!(sap.count(Step::Preflight), 0);
    // The cycle connection and the connection of the proof.
    assert_eq!(sap.sessions, 2);
    assert_eq!(sap.closes, 2);
    assert_eq!(run.post, PostTransport::Sadt);
    assert!(run.runner_line().unwrap().contains("post via sadt"));
}

#[test]
fn the_http_cycle_logs_off_once_per_class_and_once_for_the_proof() {
    let sap = new_sap(true);

    http_row(&sap, &CancelFlag::new());

    let sap = sap.borrow();

    // Session 1 is the cycle (ended by the DELETE 2xx), session 2 the proof.
    assert_eq!(sap.count(Step::Logoff), 2);
    assert_eq!(sap.on_session(1, Step::Logoff), 1);
    assert_eq!(sap.on_session(2, Step::Logoff), 1);
}

#[test]
fn every_row_opens_a_fresh_session_and_the_batch_logs_off_per_class() {
    let sap = new_sap(true);
    let cancel = CancelFlag::new();
    let rows = run_rows(&[1, 2, 3], |_| http_row(&sap, &cancel));

    assert!(rows.iter().all(|row| matches!(row, Row::Ran(_))));
    assert_eq!(sap.borrow().sessions, 6);
    assert_eq!(sap.borrow().count(Step::Logoff), 6);
}

// --- the repository preflight ----------------------------------------------------------------

const PREFLIGHT_LINE: &str =
    "RUNNER: preflight T000 CCCATEGORY=C CCCORACTIV=1 CCNOCLIIND=- TRNSPACE /0CUST/ EDITFLAG=X";

#[test]
fn the_preflight_runs_before_the_class_on_the_cycle_session_and_prints_its_facts() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);
        let cancel = CancelFlag::new();
        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &cancel),
            BackendKind::RfcSadt => rfc_row(&sap, &cancel),
        };
        let sap = sap.borrow();
        let lines = sap.lines();
        let first_create = lines
            .iter()
            .position(|line| line == "POST /sap/bc/adt/oo/classes")
            .expect("a create");

        assert_eq!(
            run.preflight.as_deref(),
            Some(PREFLIGHT_LINE),
            "{backend:?}"
        );
        assert_eq!(sap.count(Step::Preview), 2, "{backend:?}: two previews");
        assert_eq!(
            lines[..first_create]
                .iter()
                .filter(|line| line.contains("datapreview"))
                .count(),
            2,
            "{backend:?}: both before the create"
        );
        // Same session as the cycle: no extra session for the preflight.
        assert_eq!(sap.on_session(1, Step::Preview), 2, "{backend:?}");
        assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    }
}

#[test]
fn a_namespace_that_is_not_modifiable_stops_before_any_write_and_differs_from_the_hint() {
    for flag in [Some("N"), Some(""), None] {
        for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
            let sap = new_sap(backend == BackendKind::AdtHttp);
            let cancel = CancelFlag::new();

            sap.borrow_mut().namespace_flag = flag;

            let run = match backend {
                BackendKind::AdtHttp => http_row(&sap, &cancel),
                BackendKind::RfcSadt => rfc_row(&sap, &cancel),
            };
            let RunnerEnd::Stop(text) = &run.end else {
                if flag.is_none() {
                    // No row at all is not an unambiguous refusal.
                    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);

                    continue;
                }

                panic!("expected STOP, got {:?}", run.end);
            };

            assert!(
                text.starts_with(
                    "read DTEL needs a temporary class; repository changes are not allowed on DEV/100 ("
                ) && text.contains("EDITFLAG"),
                "{text}"
            );
            assert!(!text.contains(HINT), "{text}");
            assert_eq!(run.exit_code(0), 2);
            assert!(run.name.is_none() && run.runner_line().is_none());
            assert_eq!(sap.borrow().count(Step::Create), 0);
            assert_eq!(sap.borrow().count(Step::Delete), 0);
            assert!(run.leftover.is_none());
            assert!(run.halts_batch());
        }
    }
}

#[test]
fn an_unreadable_preview_does_not_block_and_leaves_a_warning() {
    let sap = new_sap(true);

    fault(&sap, Step::Preview, 2, Fault::Status(403, "forbidden"));

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert!(
        run.notes
            .iter()
            .any(|note| note.starts_with("W: ") && note.contains("TRNSPACE")),
        "{:?}",
        run.notes
    );
    assert!(run.preflight.as_deref().unwrap().contains("EDITFLAG=?"));
    assert_clean(&sap, &run);
}

#[test]
fn a_preview_that_is_never_sent_does_not_start_the_cycle() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);

        fault(&sap, Step::Preview, 1, Fault::NotSent);

        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };

        assert!(
            matches!(&run.end, RunnerEnd::NotStarted(_)),
            "{:?}",
            run.end
        );
        assert_eq!(sap.borrow().count(Step::Create), 0);
        assert_clean(&sap, &run);
    }
}

// --- failures before the POST ----------------------------------------------------------------

#[test]
fn a_stage_failure_before_the_post_is_rejected_with_cleanup() {
    // (step, nth, fault, expected text, class was created)
    let cases = [
        (
            Step::Create,
            1,
            Fault::Status(400, "x"),
            "runner create failed: HTTP 400",
            false,
        ),
        (
            Step::Lock,
            1,
            Fault::Status(423, "x"),
            "runner lock failed: HTTP 423",
            true,
        ),
        (
            Step::Put,
            1,
            Fault::Status(500, "x"),
            "runner PUT failed: HTTP 500",
            true,
        ),
        (Step::Put, 1, Fault::Lost, "runner PUT failed:", true),
        (
            Step::Unlock,
            1,
            Fault::Status(400, "x"),
            "runner unlock failed: HTTP 400",
            true,
        ),
        (
            Step::Activate,
            1,
            Fault::Status(500, "x"),
            "runner activate failed: HTTP 500",
            true,
        ),
        (
            Step::Activate,
            1,
            Fault::Lost,
            "runner activate failed:",
            true,
        ),
        (
            Step::GetSource,
            1,
            Fault::Status(404, "x"),
            "runner source check failed: HTTP 404",
            true,
        ),
    ];

    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        for (step, nth, injected, text, created) in cases.clone() {
            let sap = new_sap(true);
            let cancel = CancelFlag::new();

            fault(&sap, step, nth, injected);

            let run = match backend {
                BackendKind::AdtHttp => http_row(&sap, &cancel),
                BackendKind::RfcSadt => rfc_row(&sap, &cancel),
            };

            assert!(
                rejected(&run).starts_with(text),
                "{backend:?} {step:?}: {:?}",
                run.end
            );
            assert_eq!(run.exit_code(0), 1);
            assert_eq!(sap.borrow().count(Step::Post), 0, "{step:?}: no POST");
            assert_clean(&sap, &run);
            // Cleanup was attempted on every path where a class existed.
            assert_eq!(
                sap.borrow().count(Step::Delete),
                usize::from(created),
                "{step:?}"
            );
        }
    }
}

#[test]
fn an_activation_error_on_the_template_asks_for_a_bug_report() {
    let sap = new_sap(true);

    sap.borrow_mut().activation = Some(MESSAGES_E);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(rejected(&run).starts_with("runner activate failed: E row 1: broken"));
    assert!(
        run.notes
            .iter()
            .any(|note| note.starts_with("W: ") && note.contains("report")),
        "{:?}",
        run.notes
    );
    assert_eq!(run.exit_code(0), 1);
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

#[test]
fn a_source_hash_mismatch_stops_before_the_post() {
    let sap = new_sap(true);

    sap.borrow_mut().corrupt_source = true;

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner source check failed:"),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

#[test]
fn an_authorization_refusal_is_rejected_with_the_hint_and_no_fallback() {
    for (step, class_exists) in [
        (Step::Create, false),
        (Step::Lock, true),
        (Step::Activate, true),
        (Step::Post, true),
    ] {
        let sap = new_sap(true);

        fault(&sap, step, 1, Fault::Status(403, "forbidden"));

        let run = http_row(&sap, &CancelFlag::new());

        assert!(rejected(&run).contains(HINT), "{step:?}: {:?}", run.end);
        assert_eq!(run.exit_code(0), 1);
        assert_eq!(
            sap.borrow().count(Step::Post),
            usize::from(step == Step::Post)
        );
        assert_eq!(sap.borrow().count(Step::Delete), usize::from(class_exists));
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_taken_name_is_replaced_once_and_the_second_collision_stops() {
    let sap = new_sap(true);

    fault(&sap, Step::Create, 1, Fault::Status(409, "exists"));

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().count(Step::Create), 2);
    assert_clean(&sap, &run);

    let sap = new_sap(true);

    fault(&sap, Step::Create, 1, Fault::Status(409, "exists"));
    fault(&sap, Step::Create, 2, Fault::Status(409, "exists"));

    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(
        run.end,
        RunnerEnd::Stop("runner class name collided twice; retry later".to_owned())
    );
    assert_eq!(run.exit_code(0), 2);
    // Nothing was written: no lock, no delete.
    assert_eq!(sap.borrow().count(Step::Lock), 0);
    assert_eq!(sap.borrow().count(Step::Delete), 0);
}

// --- lost create reply -----------------------------------------------------------------------

#[test]
fn a_lost_create_reply_is_checked_from_a_new_session_and_the_class_is_removed() {
    let sap = new_sap(true);

    fault(&sap, Step::Create, 1, Fault::Lost);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner create failed:"),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Proof), 2);
    assert_eq!(sap.borrow().count(Step::Delete), 1);
    assert_clean(&sap, &run);
    // The create was never repeated.
    assert_eq!(sap.borrow().count(Step::Create), 1);
}

#[test]
fn a_lost_create_that_never_ran_leaves_nothing_and_deletes_nothing() {
    let sap = new_sap(true);

    fault(&sap, Step::Create, 1, Fault::LostBefore);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(rejected(&run).starts_with("runner create failed:"));
    assert_eq!(sap.borrow().count(Step::Delete), 0);
    assert_clean(&sap, &run);
}

#[test]
fn a_lost_create_that_cannot_be_checked_is_a_leftover() {
    let sap = new_sap(true);

    fault(&sap, Step::Create, 1, Fault::Lost);
    fault(&sap, Step::Proof, 1, Fault::Status(500, "x"));

    let run = http_row(&sap, &CancelFlag::new());
    let name = run.name.clone().unwrap();

    assert_eq!(run.leftover.as_deref(), Some(name.as_str()));
    assert_eq!(run.exit_code(0), 1);
    assert!(run.halts_batch());
}

#[test]
fn a_create_that_was_not_sent_may_use_the_other_backend() {
    let sap = new_sap(true);

    fault(&sap, Step::Create, 1, Fault::NotSent);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        matches!(
            &run.end,
            RunnerEnd::NotStarted(TransportError::PreSendConnectivity(_))
        ),
        "{:?}",
        run.end
    );
    assert_eq!(run.exit_code(0), 1);
    assert_eq!(sap.borrow().count(Step::Delete), 0);
}

#[test]
fn a_failed_open_is_not_started() {
    let mut open = || -> Opened { Err(TransportError::PreSendConnectivity("no route".to_owned())) };
    let run = run_cycle(
        &input(BackendKind::AdtHttp, true),
        &mut open,
        PostRoute::Cycle,
        &CancelFlag::new(),
    );

    assert!(matches!(&run.end, RunnerEnd::NotStarted(_)));
    assert!(run.name.is_none() && run.runner_line().is_none());
}

// --- STOP texts ------------------------------------------------------------------------------

#[test]
fn a_stateless_http_session_stops_before_any_write() {
    let sap = new_sap(false);
    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(
        run.end,
        RunnerEnd::Stop(
            "ADT HTTP session is stateless on this release; the temporary runner class needs a \
             stateful session (use the RFC path)"
                .to_owned()
        )
    );
    assert_eq!(run.exit_code(0), 2);
    assert_eq!(sap.borrow().count(Step::Create), 0);
    // The probe session is logged off too.
    assert_eq!(sap.borrow().count(Step::Logoff), 1);
}

#[test]
fn a_connection_without_rfc_and_without_a_stateful_session_gets_the_generic_text() {
    let sap = new_sap(false);
    let mut open = || http_session(&sap);
    let run = run_cycle(
        &input(BackendKind::AdtHttp, false),
        &mut open,
        PostRoute::Cycle,
        &CancelFlag::new(),
    );

    assert_eq!(
        run.end,
        RunnerEnd::Stop("read DTEL needs a stateful ADT session or the RFC path".to_owned())
    );
    assert_eq!(sap.borrow().count(Step::Create), 0);
}

#[test]
fn a_missing_classrun_interface_or_handler_stops_without_a_fallback() {
    // A system without the interface refuses the activation of the class that implements it.
    let sap = new_sap(true);

    sap.borrow_mut().activation = Some(MESSAGES_NO_INTERFACE);

    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(
        run.end,
        RunnerEnd::Stop("read DTEL needs ADT classrun; not available on DEV".to_owned())
    );
    assert_eq!(run.exit_code(0), 2);
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);

    // A missing handler shows only at the POST: the class exists and is removed.
    let sap = new_sap(true);

    fault(&sap, Step::Post, 1, Fault::Status(404, "x"));

    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(
        run.end,
        RunnerEnd::Stop("read DTEL needs ADT classrun; not available on DEV".to_owned())
    );
    assert_eq!(sap.borrow().count(Step::Delete), 1);
    assert_clean(&sap, &run);
}

#[test]
fn a_failed_probe_before_the_first_write_is_not_started_or_rejected() {
    let sap = new_sap(true);

    fault(&sap, Step::Preflight, 1, Fault::NotSent);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        matches!(&run.end, RunnerEnd::NotStarted(_)),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Create), 0);

    let sap = new_sap(true);

    fault(&sap, Step::Preflight, 1, Fault::Status(500, "x"));

    let run = http_row(&sap, &CancelFlag::new());

    assert!(rejected(&run).starts_with("runner preflight failed: HTTP 500"));
    assert_eq!(sap.borrow().count(Step::Create), 0);
}

// --- the POST --------------------------------------------------------------------------------

#[test]
fn a_post_failure_is_never_resent_and_the_class_is_removed() {
    // (fault, expected end)
    let lost = Fault::Lost;

    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(true);
        let cancel = CancelFlag::new();

        fault(&sap, Step::Post, 1, lost.clone());

        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &cancel),
            BackendKind::RfcSadt => rfc_row(&sap, &cancel),
        };

        assert!(
            matches!(&run.end, RunnerEnd::PostUncertain(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(sap.borrow().count(Step::Post), 1, "{backend:?}");
        assert_clean(&sap, &run);
    }

    // A 5xx is a reply the protocol parser judges; the cycle only never resends.
    let sap = new_sap(true);

    fault(
        &sap,
        Step::Post,
        1,
        Fault::Status(502, "<html>Bad Gateway</html>"),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(response) if response.status == 502));
    assert_eq!(sap.borrow().count(Step::Post), 1);
    assert_clean(&sap, &run);
}

#[test]
fn a_post_that_was_not_sent_is_rejected_and_not_retried() {
    let sap = new_sap(true);

    fault(&sap, Step::Post, 1, Fault::NotSent);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner POST failed:"),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Post), 1);
    assert_clean(&sap, &run);
}

#[test]
fn the_post_transport_follows_the_u2_result() {
    assert_eq!(
        post_transport(BackendKind::AdtHttp, true),
        PostTransport::Http
    );
    assert_eq!(
        post_transport(BackendKind::AdtHttp, false),
        PostTransport::Http
    );
    assert_eq!(
        post_transport(BackendKind::RfcSadt, true),
        PostTransport::Sadt
    );
    assert_eq!(
        post_transport(BackendKind::RfcSadt, false),
        PostTransport::Http
    );
    assert_eq!(PostTransport::Sadt.token(), "sadt");
    assert_eq!(PostTransport::Http.token(), "http");
}

// --- mixed transport -------------------------------------------------------------------------

#[test]
fn the_class_cycle_over_sadt_may_post_over_http_in_its_own_session() {
    let sap = new_sap(true);
    let http_sap = sap.clone();
    let mut open = || rfc_session(&sap);
    let mut open_post = || http_session(&http_sap);
    let run = run_cycle(
        &input(BackendKind::RfcSadt, true),
        &mut open,
        PostRoute::Http(&mut open_post),
        &CancelFlag::new(),
    );
    let state = sap.borrow();

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(run.post, PostTransport::Http);
    assert!(run.runner_line().unwrap().contains("post via http"));
    assert!(state.classes.is_empty());
    // Sessions: 1 cycle (SADT), 2 POST (HTTP), 3 proof (SADT).
    assert_eq!(state.on_session(1, Step::Post), 0);
    assert_eq!(state.on_session(2, Step::Post), 1);
    assert_eq!(state.on_session(2, Step::Logoff), 1);
    assert_eq!(state.count(Step::Logoff), 1);
    // The GET of the source stays on the cycle connection.
    assert_eq!(state.on_session(1, Step::GetSource), 1);
}

#[test]
fn a_failed_http_post_is_not_resent_on_the_cycle_transport() {
    let sap = new_sap(true);
    let http_sap = sap.clone();
    let mut open = || rfc_session(&sap);
    let mut open_post = || http_session(&http_sap);

    fault(&sap, Step::Post, 1, Fault::Lost);

    let run = run_cycle(
        &input(BackendKind::RfcSadt, true),
        &mut open,
        PostRoute::Http(&mut open_post),
        &CancelFlag::new(),
    );
    let state = sap.borrow();

    assert!(matches!(&run.end, RunnerEnd::PostUncertain(_)));
    assert_eq!(state.count(Step::Post), 1);
    assert_eq!(state.on_session(1, Step::Post), 0);
    assert!(state.classes.is_empty());
    // No reply ever opened that HTTP session on the client, so its close sends no logoff.
    assert_eq!(state.on_session(2, Step::Logoff), 0);
}

#[test]
fn a_post_session_that_cannot_open_is_rejected_and_the_class_is_removed() {
    let sap = new_sap(true);
    let mut open = || rfc_session(&sap);
    let mut open_post =
        || -> Opened { Err(TransportError::PreSendConnectivity("no HTTP".to_owned())) };
    let run = run_cycle(
        &input(BackendKind::RfcSadt, true),
        &mut open,
        PostRoute::Http(&mut open_post),
        &CancelFlag::new(),
    );

    assert!(
        rejected(&run).starts_with("runner POST failed:"),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

#[test]
fn the_arm_ends_with_the_cycle_so_a_late_post_is_refused_on_sadt() {
    let sap = new_sap(true);
    let run = rfc_row(&sap, &CancelFlag::new());
    let name = run.name.unwrap();
    let mut late = RfcTransport::new(FakeRfcApi::ok());
    let mut request = Request::post(format!("/sap/bc/adt/oo/classrun/{name}"), Vec::new());

    request
        .headers
        .insert("Accept".to_owned(), "text/plain".to_owned());

    assert!(matches!(
        late.request(request),
        Err(TransportError::RequestNotAllowed(_))
    ));
}

#[test]
fn the_real_sadt_admission_admits_the_cycle_post_only_while_armed() {
    // The same request on the real RFC-SADT transport: refused unarmed, admitted while the cycle
    // holds the arm (checked from inside the fake's POST step).
    struct Probe {
        inner: Session,
        verdicts: Rc<RefCell<Vec<bool>>>,
    }

    impl SadtTransport for Probe {
        fn request(&mut self, request: Request) -> Result<Response, TransportError> {
            if request.method == "POST" && request.uri.contains("/classrun/") {
                let mut sadt = RfcTransport::new(FakeRfcApi::ok());

                self.verdicts
                    .borrow_mut()
                    .push(sadt.request(request.clone()).is_ok());
            }

            self.inner.request(request)
        }

        fn close(&mut self) -> Result<(), TransportError> {
            self.inner.close()
        }
    }

    let sap = new_sap(true);
    let verdicts = Rc::new(RefCell::new(Vec::new()));
    let mut open = || -> Opened {
        Ok(Box::new(Probe {
            // The probe applies the real admission itself, so the fake must not judge again.
            inner: session(&sap, false),
            verdicts: verdicts.clone(),
        }))
    };
    let run = run_cycle(
        &input(BackendKind::RfcSadt, true),
        &mut open,
        PostRoute::Cycle,
        &CancelFlag::new(),
    );

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(*verdicts.borrow(), [true]);
}

// --- cleanup that fails ----------------------------------------------------------------------

#[test]
fn a_failed_delete_is_a_leftover_with_both_cleanup_commands() {
    let cases = [
        (Step::Delete, Fault::Status(500, "x")),
        (Step::Delete, Fault::LostBefore),
        (Step::Lock, Fault::Status(423, "x")),
    ];

    for (step, injected) in cases {
        let sap = new_sap(true);
        // The write lock is the first LOCK, the cleanup lock the second.
        let nth = if step == Step::Lock { 2 } else { 1 };

        // Both attempts fail: the one on the cycle session and the retry on a fresh session.
        fault(&sap, step, nth, injected.clone());
        fault(&sap, step, nth + 1, injected);

        let run = http_row(&sap, &CancelFlag::new());
        let name = run.name.clone().unwrap();

        // Status truth: the POST answered; the leftover does not hide it.
        assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{step:?}");
        assert_eq!(run.leftover.as_deref(), Some(name.as_str()), "{step:?}");
        assert_eq!(run.exit_code(0), 1);
        assert!(run.halts_batch());
        assert_eq!(
            run.leftover_text(),
            Some(format!(
                "RUNNER_LEFTOVER: {name}\n\
                 abapreach delete --simulate CLAS:{name} --plan <FILE> --conn dev\n\
                 abapreach delete CLAS:{name} --plan <FILE> --conn dev"
            ))
        );
    }
}

#[test]
fn a_lost_delete_reply_is_settled_by_the_new_session() {
    // The DELETE ran and its reply was lost: the proof finds the class gone, so no leftover.
    let sap = new_sap(true);

    fault(&sap, Step::Delete, 1, Fault::Lost);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)));
    assert_clean(&sap, &run);
    assert_eq!(run.exit_code(0), 0);
}

#[test]
fn a_class_that_survives_an_accepted_delete_is_a_leftover() {
    let sap = new_sap(true);

    sap.borrow_mut().delete_is_ignored = true;

    let run = http_row(&sap, &CancelFlag::new());

    assert!(run.leftover.is_some());
    assert_eq!(run.exit_code(0), 1);
}

#[test]
fn a_proof_that_cannot_be_read_is_a_leftover_never_a_success() {
    let sap = new_sap(true);

    fault(&sap, Step::Proof, 1, Fault::LostBefore);
    fault(&sap, Step::Proof, 2, Fault::LostBefore);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(run.leftover.is_some());
    assert_eq!(run.exit_code(0), 1);
    assert!(
        run.notes
            .iter()
            .any(|note| note.contains("runner cleanup proof failed")),
        "{:?}",
        run.notes
    );
}

#[test]
fn a_leftover_stops_the_batch_and_the_later_rows_are_skipped() {
    let sap = new_sap(true);
    let cancel = CancelFlag::new();

    // The delete of the second row fails.
    fault(&sap, Step::Delete, 2, Fault::Status(500, "x"));
    fault(&sap, Step::Delete, 3, Fault::Status(500, "x"));

    let rows = run_rows(&[1, 2, 3, 4], |_| http_row(&sap, &cancel));

    assert!(matches!(&rows[0], Row::Ran(run) if run.leftover.is_none()));
    assert!(matches!(&rows[1], Row::Ran(run) if run.leftover.is_some()));
    assert!(matches!(&rows[2], Row::Skipped));
    assert!(matches!(&rows[3], Row::Skipped));
    assert_eq!(sap.borrow().count(Step::Create), 2);
}

// --- interrupts ------------------------------------------------------------------------------

#[test]
fn a_signal_before_the_post_removes_the_class_and_exits_130() {
    let steps = [
        Step::Create,
        Step::Lock,
        Step::Put,
        Step::Unlock,
        Step::Activate,
        Step::GetSource,
    ];

    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        for step in steps {
            for kind in [Signal::Handler, Signal::Shutdown] {
                let sap = new_sap(true);
                let cancel = CancelFlag::new();

                signal(&sap, step, kind, &cancel);

                let run = match backend {
                    BackendKind::AdtHttp => http_row(&sap, &cancel),
                    BackendKind::RfcSadt => rfc_row(&sap, &cancel),
                };

                assert_eq!(
                    run.end,
                    RunnerEnd::Interrupted,
                    "{backend:?} {step:?} {kind:?}"
                );
                assert!(run.interrupted);
                assert_eq!(run.exit_code(0), 130);
                assert_eq!(
                    sap.borrow().count(Step::Post),
                    0,
                    "{step:?}: no POST after a signal"
                );
                assert_eq!(sap.borrow().count(Step::Delete), 1, "{step:?}");
                assert_clean(&sap, &run);
            }
        }
    }
}

#[test]
fn a_signal_during_the_post_waits_for_the_reply_then_cleans_up() {
    for kind in [Signal::Handler, Signal::Shutdown] {
        let sap = new_sap(true);
        let cancel = CancelFlag::new();

        signal(&sap, Step::Post, kind, &cancel);

        let run = http_row(&sap, &cancel);

        // The reply is kept for the reconciliation; the exit is 130.
        assert!(matches!(&run.end, RunnerEnd::Posted(_)));
        assert!(run.interrupted);
        assert_eq!(run.exit_code(0), 130);
        assert_eq!(sap.borrow().count(Step::Post), 1);
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_signal_before_the_class_exists_stops_without_a_write() {
    let sap = new_sap(true);
    let cancel = CancelFlag::new();

    cancel.request_shutdown();

    let run = http_row(&sap, &cancel);

    assert_eq!(run.end, RunnerEnd::Interrupted);
    assert_eq!(sap.borrow().count(Step::Create), 0);
    assert_eq!(run.exit_code(0), 130);
}

#[test]
fn a_leftover_after_a_signal_still_exits_130() {
    let sap = new_sap(true);
    let cancel = CancelFlag::new();

    signal(&sap, Step::Put, Signal::Handler, &cancel);
    fault(&sap, Step::Delete, 1, Fault::Status(500, "x"));
    fault(&sap, Step::Delete, 2, Fault::Status(500, "x"));

    let run = http_row(&sap, &cancel);

    assert!(run.leftover.is_some());
    assert_eq!(run.exit_code(0), 130);
}

#[test]
fn the_process_is_kept_alive_while_a_runner_class_exists() {
    // The Ctrl-C handler would exit at once when nothing is held; the cycle holds the write scope
    // for the whole life of the class. Asserted inside the fake (`Signal::Handler`); here the
    // scope is shown to be released afterwards.
    let cancel = CancelFlag::new();
    let sap = new_sap(true);

    http_row(&sap, &cancel);

    assert_eq!(
        cancel.on_interrupt(),
        abapreach::operation::deploy::InterruptAction::ExitNow
    );
}

// --- exit codes ------------------------------------------------------------------------------

#[test]
fn the_exit_code_follows_the_end_and_a_leftover() {
    let sap = new_sap(true);
    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(run.exit_code(0), 0);
    assert_eq!(run.exit_code(1), 1);
}

// --- live defect of the 12A gate: warnings, a large class, a dead session, silent leftovers ----

const MESSAGES_W: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="W" line="1378"><shortText><txt>The exception CX_ABAP_MESSAGE_DIGEST is not caught or declared in the RAISING clause of "ROWS_HASH"</txt></shortText></msg><msg type="W" line="400"><shortText><txt>Redundant conversion for type DDOBJNAME</txt></shortText></msg></chkl:messages>"#;

#[test]
fn an_activation_with_warnings_only_proceeds_to_the_post() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(true);

        sap.borrow_mut().activation = Some(MESSAGES_W);

        let cancel = CancelFlag::new();
        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &cancel),
            BackendKind::RfcSadt => rfc_row(&sap, &cancel),
        };

        assert!(
            matches!(&run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(sap.borrow().count(Step::Post), 1);
        assert_clean(&sap, &run);
        assert_eq!(run.exit_code(0), 0);
    }
}

#[test]
fn a_class_of_about_ninety_kilobytes_runs_the_whole_cycle() {
    let big = |name: &str| -> Result<String, String> {
        let mut source = format!("CLASS {name} DEFINITION.\nENDCLASS.\n");

        for row in 0..2300 {
            source.push_str(&format!("  \" row {row:05} {}\n", "x".repeat(30)));
        }

        Ok(source)
    };

    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(true);
        let mut input = input(backend, true);

        input.build = &big;
        sap.borrow_mut().activation = Some(MESSAGES_W);

        let cancel = CancelFlag::new();
        let run = match backend {
            BackendKind::AdtHttp => {
                let mut open = || http_session(&sap);

                run_cycle(&input, &mut open, PostRoute::Cycle, &cancel)
            }
            BackendKind::RfcSadt => {
                let mut open = || rfc_session(&sap);

                run_cycle(&input, &mut open, PostRoute::Cycle, &cancel)
            }
        };

        assert!(
            matches!(&run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_session_that_died_in_the_post_is_replaced_for_the_cleanup() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(true);

        sap.borrow_mut().dies_after_post = true;

        let cancel = CancelFlag::new();
        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &cancel),
            BackendKind::RfcSadt => rfc_row(&sap, &cancel),
        };

        assert!(
            matches!(&run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_clean(&sap, &run);
        assert_eq!(run.exit_code(0), 0);
        // The first attempt is not silent even though the retry removed the class.
        assert!(
            run.notes
                .iter()
                .any(|note| note.starts_with("W: runner cleanup lock failed: HTTP 500")),
            "{:?}",
            run.notes
        );
    }
}

#[test]
fn a_leftover_always_carries_its_reason_in_the_notes() {
    let sap = new_sap(true);

    for nth in [1, 2] {
        fault(&sap, Step::Delete, nth, Fault::Status(500, "boom"));
    }

    let run = http_row(&sap, &CancelFlag::new());

    assert!(run.leftover.is_some());
    assert!(
        run.notes
            .iter()
            .any(|note| note.starts_with("W: runner cleanup DELETE failed: HTTP 500")),
        "{:?}",
        run.notes
    );
    assert!(
        run.notes
            .iter()
            .any(|note| note.contains("runner cleanup proof") && note.contains("still exists")),
        "{:?}",
        run.notes
    );
}

#[test]
fn a_failed_stage_carries_its_stage_and_sap_text_in_the_notes() {
    let sap = new_sap(true);

    fault(
        &sap,
        Step::Put,
        1,
        Fault::Status(500, "<m><localizedMessage>no space</localizedMessage></m>"),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        run.notes
            .contains(&"W: runner PUT failed: HTTP 500: no space".to_owned()),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

// ---- cargo-mutants triage (M6 Task 33): cycle rules that no test pinned ---------------------------

#[test]
fn a_forbidden_discovery_probe_is_rejected_with_the_hint() {
    let sap = new_sap(true);

    fault(&sap, Step::Preflight, 1, Fault::Status(403, "forbidden"));

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner preflight failed: HTTP 403"),
        "{:?}",
        run.end
    );
    assert!(rejected(&run).contains(HINT), "{:?}", run.end);
    assert_eq!(sap.borrow().count(Step::Create), 0);
}

#[test]
fn a_create_below_500_leaves_nothing_to_check_and_a_5xx_is_checked_from_a_new_session() {
    // 409 is the taken-name path and 403 the authorization path; both have their own tests.
    for (status, checked) in [
        (400, false),
        (404, false),
        (499, false),
        (500, true),
        (503, true),
        (599, true),
    ] {
        let sap = new_sap(true);

        fault(&sap, Step::Create, 1, Fault::Status(status, "x"));

        let run = http_row(&sap, &CancelFlag::new());

        assert!(
            rejected(&run).starts_with(&format!("runner create failed: HTTP {status}")),
            "{status}: {:?}",
            run.end
        );
        assert_eq!(
            sap.borrow().count(Step::Proof),
            usize::from(checked),
            "{status}"
        );
        assert_eq!(sap.borrow().count(Step::Delete), 0, "{status}");
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_refused_logon_at_any_stage_carries_the_hint_and_a_lost_reply_does_not() {
    for step in [
        Step::Create,
        Step::Lock,
        Step::Put,
        Step::Unlock,
        Step::Activate,
        Step::GetSource,
        Step::Post,
    ] {
        let sap = new_sap(true);

        fault(&sap, step, 1, Fault::Unauthorized);

        let run = http_row(&sap, &CancelFlag::new());

        assert!(rejected(&run).contains(HINT), "{step:?}: {:?}", run.end);
        assert_clean(&sap, &run);
    }

    for step in [Step::Lock, Step::Put, Step::Unlock, Step::Activate] {
        let sap = new_sap(true);

        fault(&sap, step, 1, Fault::Lost);

        let run = http_row(&sap, &CancelFlag::new());

        assert!(!rejected(&run).contains(HINT), "{step:?}: {:?}", run.end);
    }
}
