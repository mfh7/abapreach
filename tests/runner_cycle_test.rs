//! The runner class cycle (M6 Task 14) against a fake SAP that keeps class state and injects one
//! failure at a chosen step. ADT HTTP sessions are real `HttpTransport`s over the fake, so the
//! logoff requests are the ones the product builds; RFC-SADT sessions are fakes that apply the
//! same runner rules as the admission layer. No SAP.

use abapreach::backend::BackendKind;
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::runner::{
    CycleExtras, CycleInput, PostRoute, PostTransport, Row, RunnerEnd, RunnerRun, post_transport,
    run_cycle, run_cycle_with, run_rows,
};
use abapreach::operation::runner_template::OutType;
use abapreach::transport::http::HttpTransport;
use abapreach::transport::rfc::{FakeRfcApi, RfcTransport};
use abapreach::transport::{Request, Response, SadtTransport, TransportError, runner_verdict};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

mod common;

use common::preview;

const PREFIX: &str = "YTEST_ABAPREACH_RUN_";
const SOURCE: &str = "CLASS <N> DEFINITION.\nENDCLASS.\n";
const INTERNAL: &str = "IF_OO_ADT_INTRNL_CLASSRUN";
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
const MESSAGES_IMPLEMENTATION_MISSING: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="134"><shortText><txt>Implementation missing for method "CHECK_BASE"</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_CLASSRUN_METHOD_MISSING: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="9"><shortText><txt>Implementation missing for method IF_OO_ADT_CLASSRUN~MAIN</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_UNRELATED_INTERFACE: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="3"><shortText><txt>Statement not allowed after IF_OO_ADT_CLASSRUN~MAIN</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_METHOD_DOES_NOT_EXIST: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="9"><shortText><txt>IF_OO_ADT_CLASSRUN~MAIN does not exist</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_METHOD_NOT_FOUND: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="9"><shortText><txt>IF_OO_ADT_CLASSRUN~MAIN not found</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_CLASS_AND_INTERFACE: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="9"><shortText><txt>Class <N> does not exist; Interface IF_OO_ADT_CLASSRUN is unknown</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_SNIPPET_IMPLEMENTATION_MISSING: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="22"><shortText><txt>Implementation missing for method "LOCAL_METHOD"</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_FRAME_AND_SNIPPET: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="9"><shortText><txt>Implementation missing for method IF_OO_ADT_CLASSRUN~MAIN</txt></shortText></msg><msg type="E" line="22"><shortText><txt>Statement is not accessible</txt></shortText></msg></chkl:messages>"#;
const DISCOVERY_WITHOUT_CLASSRUN: &str = r#"<app:service xmlns:app="x"><app:workspace><app:collection href="/sap/bc/adt/oo/classes"/></app:workspace></app:service>"#;
/// "classrun" appears, but in no collection href.
const DISCOVERY_CLASSRUN_ONLY_IN_TEXT: &str = r#"<app:service xmlns:app="x"><app:workspace><app:collection href="/sap/bc/adt/oo/classes" title="classrun"/><app:collection href="/sap/bc/adt/oo/classrunner"/></app:workspace></app:service>"#;
const DISCOVERY_WITH_CLASSRUN: &str =
    r#"<app:service xmlns:app="x"><app:collection href="/sap/bc/adt/oo/classrun"/></app:service>"#;
const CLASS_MISSING: &str = "RfcInvoke: Internal error: Class <N> does not exist (RFC code 3)";
/// The kernel error as FED 7.40 reports it: several lines, the code at the end.
const CLASS_MISSING_KERNEL: &str =
    "RfcInvoke: Internal error: Class <N> does not exist\nLINE 10026\nCOUNTER 7 (RFC code 3)";
const CLASS_MISSING_CODE_4: &str = "RfcInvoke: Class <N> does not exist (RFC code 4)";
const MESSAGES_FOREIGN_CLASS_MISSING: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="9"><shortText><txt>Class ZOTHER does not exist</txt></shortText></msg></chkl:messages>"#;
/// The FED 7.40 failure of the live matrix: the activation says the class lost one of the
/// runner's own methods after a parallel activation disturbed it.
const MESSAGES_OWN_METHOD_NOT_DECLARED: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="2205"><shortText><txt>Method "." is not declared or inherited in class "<N>".</txt></shortText></msg></chkl:messages>"#;
/// The same sentence for a foreign class: no disturbance of this run.
const MESSAGES_FOREIGN_METHOD_NOT_DECLARED: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="2205"><shortText><txt>Method "ACTION_ALLOWED" is not declared or inherited in class "ZOTHER".</txt></shortText></msg></chkl:messages>"#;
/// A longer token that ends in `method`: not the Method sentence.
const MESSAGES_SUBMETHOD_NOT_DECLARED: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="2205"><shortText><txt>Submethod "ACTION_ALLOWED" is not declared or inherited in class "<N>".</txt></shortText></msg></chkl:messages>"#;
const METHOD_NOT_DECLARED: &str = "RfcInvoke: Internal error: Method \"ACTION_ALLOWED\" is not declared or inherited in class \"<N>\" (RFC code 3)";
const METHOD_NOT_DECLARED_FOREIGN: &str = "RfcInvoke: Internal error: Method \"ACTION_ALLOWED\" is not declared or inherited in class \"ZOTHER\" (RFC code 3)";
/// The FED 7.40 failure of the live matrix: the class of the standard activation work area, or a
/// parallel runner's class, not the cell's own.
const CLASS_MISSING_FOREIGN: &str = "protocol error: RfcInvoke: Internal error: Class YTEST_ABAPREACH_RUN6F_G3LX9PRM does not exist (RFC code 3)";
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
    GetInactive,
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
    /// The request fails with an RFC error text after it was sent, and the session is dead from
    /// then on; `<N>` is the runner name.
    Rfc(&'static str),
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
    /// The type of the classrun `OUT` parameter; `None` answers no row.
    out_type: Option<&'static str>,
    /// The `Accept` header of every source PUT.
    put_accepts: Vec<Option<String>>,
    /// `TADIR` still holds a row for the runner after its delete.
    tadir_row: bool,
    /// The discovery document; `None` is a document without classrun.
    discovery: Option<&'static str>,
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
        ("GET", path) if path.ends_with("/source/main") && query == "version=inactive" => {
            Step::GetInactive
        }
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

        if step == Step::Put {
            self.put_accepts
                .push(request.headers.get("Accept").cloned());
        }

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
                let name = self.classes.keys().next().cloned().unwrap_or_default();

                return Ok(self.reply(Response::text(status, body.replace("<N>", &name))));
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
            Some(Fault::Rfc(text)) => {
                let name = self.classes.keys().next().cloned().unwrap_or_default();

                // The RFC library closes the connection after the error.
                self.dead.push(session);

                return Err(TransportError::Protocol(text.replace("<N>", &name)).after_send());
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
            Step::Preflight => self.reply(Response::text(
                200,
                self.discovery.unwrap_or(DISCOVERY_WITHOUT_CLASSRUN),
            )),
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
                } else if body.contains(" FROM TADIR ") {
                    let runner = body.split('\'').nth(1).unwrap_or_default().to_owned();

                    assert_eq!(
                        body,
                        format!(
                            "SELECT PGMID, OBJECT, OBJ_NAME FROM TADIR WHERE OBJ_NAME IN ( '{runner}' )"
                        )
                    );

                    if self.tadir_row {
                        preview(&[
                            ("PGMID", &[Some("R3TR")]),
                            ("OBJECT", &[Some("CLAS")]),
                            ("OBJ_NAME", &[Some(runner.as_str())]),
                        ])
                    } else {
                        preview(&[("PGMID", none), ("OBJECT", none), ("OBJ_NAME", none)])
                    }
                } else if body.contains(" FROM SEOSUBCODF ") {
                    match self.out_type {
                        Some(found) => preview(&[
                            ("CLSNAME", &[Some("IF_OO_ADT_CLASSRUN")]),
                            ("CMPNAME", &[Some("MAIN")]),
                            ("SCONAME", &[Some("OUT")]),
                            ("TYPE", &[Some(found)]),
                        ]),
                        None => preview(&[
                            ("CLSNAME", none),
                            ("CMPNAME", none),
                            ("SCONAME", none),
                            ("TYPE", none),
                        ]),
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
            Step::GetSource | Step::GetInactive => match self.classes.get(&name) {
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
        out_type: Some(INTERNAL),
        ..Sap::default()
    }))
}

thread_local! {
    /// The `OUT` types the cycle passed to `build`, in order.
    static BUILT_WITH: RefCell<Vec<OutType>> = const { RefCell::new(Vec::new()) };
}

fn build(name: &str, out: OutType) -> Result<String, String> {
    BUILT_WITH.with(|seen| seen.borrow_mut().push(out));

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

thread_local! {
    /// How often the cycle paused before a retry.
    static PAUSES: Cell<usize> = const { Cell::new(0) };
}

/// The pause of the tests: counted, never slept.
fn count_pause() {
    PAUSES.with(|pauses| pauses.set(pauses.get() + 1));
}

fn pauses() -> usize {
    PAUSES.with(Cell::get)
}

fn extras() -> CycleExtras {
    CycleExtras {
        pause: Some(count_pause),
        ..CycleExtras::default()
    }
}

/// One row over ADT HTTP.
fn http_row(sap: &Shared, cancel: &CancelFlag) -> RunnerRun {
    let mut open = || http_session(sap);

    run_cycle_with(
        &input(BackendKind::AdtHttp, true),
        &mut open,
        PostRoute::Cycle,
        cancel,
        &extras(),
    )
}

/// One row over RFC-SADT.
fn rfc_row(sap: &Shared, cancel: &CancelFlag) -> RunnerRun {
    let mut open = || rfc_session(sap);

    run_cycle_with(
        &input(BackendKind::RfcSadt, true),
        &mut open,
        PostRoute::Cycle,
        cancel,
        &extras(),
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
        "POST /sap/bc/adt/datapreview/ddic?rowNumber=1&ddicEntityName=SEOSUBCODF".to_owned(),
        "POST /sap/bc/adt/oo/classes".to_owned(),
        format!("POST {object}?_action=LOCK&accessMode=MODIFY"),
        format!("PUT {object}/source/main?lockHandle=h%2B1%2F%3D"),
        format!("POST {object}?_action=UNLOCK&lockHandle=h%2B1%2F%3D"),
        format!("GET {object}/source/main?version=inactive"),
        "POST /sap/bc/adt/activation?method=activate&preauditRequested=true".to_owned(),
        format!("GET {object}/source/main?version=active"),
        format!("POST /sap/bc/adt/oo/classrun/{name}"),
        format!("GET {LOGOFF}"),
        format!("POST {object}?_action=LOCK&accessMode=MODIFY"),
        format!("DELETE {object}?lockHandle=h%2B1%2F%3D"),
        format!("GET {LOGOFF}"),
        format!("GET {object}"),
        format!("GET {LOGOFF}"),
        "POST /sap/bc/adt/datapreview/ddic?rowNumber=1&ddicEntityName=TADIR".to_owned(),
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
    // The cycle connection, the connection of the POST, the connection of the class proof and
    // that of the directory proof.
    assert_eq!(sap.sessions, 4);
    assert_eq!(sap.closes, 4);
    assert_eq!(run.post, PostTransport::Sadt);
    assert!(run.runner_line().unwrap().contains("post via sadt"));
}

#[test]
fn the_http_cycle_logs_off_once_per_class_and_once_for_the_proof() {
    let sap = new_sap(true);

    http_row(&sap, &CancelFlag::new());

    let sap = sap.borrow();

    // Session 1 is the cycle (ended by the DELETE 2xx), session 2 the POST, session 3 the class
    // proof, session 4 the directory proof.
    assert_eq!(sap.count(Step::Logoff), 4);

    for session in 1..=4 {
        assert_eq!(
            sap.on_session(session, Step::Logoff),
            1,
            "session {session}"
        );
    }
}

#[test]
fn every_row_opens_a_fresh_session_and_the_batch_logs_off_per_class() {
    let sap = new_sap(true);
    let cancel = CancelFlag::new();
    let rows = run_rows(&[1, 2, 3], |_| http_row(&sap, &cancel));

    assert!(rows.iter().all(|row| matches!(row, Row::Ran(_))));
    assert_eq!(sap.borrow().sessions, 12);
    assert_eq!(sap.borrow().count(Step::Logoff), 12);
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
        assert_eq!(sap.count(Step::Preview), 4, "{backend:?}: four previews");
        assert_eq!(
            lines[..first_create]
                .iter()
                .filter(|line| line.contains("datapreview"))
                .count(),
            3,
            "{backend:?}: all before the create"
        );
        // Same session as the cycle: no extra session for the preflight.
        assert_eq!(sap.on_session(1, Step::Preview), 3, "{backend:?}");
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

#[test]
fn the_source_put_names_the_text_it_accepts() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);

        match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };

        assert_eq!(
            sap.borrow().put_accepts,
            vec![Some("text/plain".to_owned())],
            "{backend:?}"
        );
    }
}

// --- the cleanup proof reads the repository directory ------------------------------------------

#[test]
fn a_class_that_is_gone_but_still_has_a_directory_row_is_a_leftover() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);

        sap.borrow_mut().tadir_row = true;

        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };
        let name = run.name.clone().unwrap();

        assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
        assert_eq!(run.leftover.as_deref(), Some(name.as_str()), "{backend:?}");
        assert!(
            run.notes
                .iter()
                .any(|note| note == "W: runner cleanup proof: TADIR still holds the class"),
            "{:?}",
            run.notes
        );
        assert_eq!(run.exit_code(0), 1);
        // Only the directory row is left: the delete commands cannot remove it, so the text says
        // where to.
        assert_eq!(
            run.leftover_text(),
            Some(format!(
                "RUNNER_LEFTOVER: {name}\n\
                 abapreach delete --simulate CLAS:{name} --plan <FILE> --conn dev\n\
                 abapreach delete CLAS:{name} --plan <FILE> --conn dev\n\
                 hint: only the object directory entry (TADIR) is left; remove it in SE03 \
                 (Change Object Directory Entries) or SE80"
            )),
            "{backend:?}"
        );
    }
}

#[test]
fn a_class_that_is_gone_with_no_directory_row_is_clean() {
    let sap = new_sap(true);
    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        run.leftover.is_none() && run.notes.is_empty(),
        "{:?}",
        run.notes
    );
    // The directory is read in a new session after the class proof, once.
    assert_eq!(sap.borrow().count(Step::Preview), 4);
    assert_clean(&sap, &run);
}

#[test]
fn a_directory_read_that_fails_is_a_leftover() {
    for injected in [Fault::Status(500, "boom"), Fault::NotSent] {
        let sap = new_sap(true);

        // The fourth preview: three before the class, then the directory read; the retry on a
        // fresh session reads it again.
        fault(&sap, Step::Preview, 4, injected.clone());
        fault(&sap, Step::Preview, 5, injected);

        let run = http_row(&sap, &CancelFlag::new());
        let name = run.name.clone().unwrap();

        assert_eq!(run.leftover.as_deref(), Some(name.as_str()));
        assert!(
            run.notes
                .iter()
                .any(|note| note.starts_with("W: runner cleanup proof failed: TADIR ")),
            "{:?}",
            run.notes
        );
        // The row is not known to be there, so the text names no SE03 step.
        assert!(!run.leftover_text().unwrap().contains("hint:"));
    }
}

// --- the type of the classrun OUT parameter --------------------------------------------------

fn built_with() -> Vec<OutType> {
    BUILT_WITH.with(|seen| std::mem::take(&mut *seen.borrow_mut()))
}

#[test]
fn the_class_source_is_built_with_the_out_type_the_system_reports() {
    for (found, expected) in [
        (INTERNAL, OutType::Internal),
        ("IF_OO_ADT_CLASSRUN_OUT", OutType::Out),
    ] {
        for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
            let sap = new_sap(backend == BackendKind::AdtHttp);

            sap.borrow_mut().out_type = Some(found);
            built_with();

            let run = match backend {
                BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
                BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
            };

            assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
            assert_eq!(built_with(), vec![expected], "{found} {backend:?}");
            assert_clean(&sap, &run);
        }
    }
}

#[test]
fn an_unknown_or_missing_out_type_stops_before_anything_is_created() {
    for found in [Some("IF_SOMETHING_ELSE"), Some(""), None] {
        for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
            let sap = new_sap(backend == BackendKind::AdtHttp);

            sap.borrow_mut().out_type = found;
            built_with();

            let run = match backend {
                BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
                BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
            };
            let RunnerEnd::Stop(text) = &run.end else {
                panic!("a stop is expected: {:?}", run.end);
            };

            assert!(
                text.starts_with("read DTEL cannot use the classrun runner: "),
                "{text}"
            );
            assert!(
                text.contains(&format!("{:?}", found.unwrap_or_default())),
                "{text}"
            );
            assert!(built_with().is_empty());
            assert_eq!(sap.borrow().count(Step::Create), 0);
            assert_clean(&sap, &run);
        }
    }
}

#[test]
fn an_out_type_preview_that_is_never_sent_does_not_start_the_cycle() {
    let sap = new_sap(true);

    fault(&sap, Step::Preview, 3, Fault::NotSent);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        matches!(&run.end, RunnerEnd::NotStarted(_)),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Create), 0);
    assert_clean(&sap, &run);
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
    // The corrupt source is already wrong in the inactive version: one retry, then the stop.
    let sap = new_sap(true);

    sap.borrow_mut().corrupt_source = true;

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner source check (inactive) failed after one retry:"),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

// --- the retry of a disturbed write or activation --------------------------------------------

#[test]
fn an_undisturbed_cycle_never_pauses() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);
    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(pauses(), 0);
    assert!(run.notes.iter().all(|note| !note.contains("retried")));
}

#[test]
fn an_inactive_source_that_differs_is_written_again_once() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        PAUSES.with(|pauses| pauses.set(0));

        let sap = new_sap(backend == BackendKind::AdtHttp);

        fault(
            &sap,
            Step::GetInactive,
            1,
            Fault::Status(200, "CLASS other DEFINITION."),
        );

        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };
        let state = sap.borrow();

        assert!(
            matches!(&run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(
            state.count(Step::Lock),
            3,
            "{backend:?}: two writes, one delete"
        );
        assert_eq!(state.count(Step::Put), 2, "{backend:?}");
        assert_eq!(state.count(Step::Unlock), 2, "{backend:?}");
        assert_eq!(state.count(Step::GetInactive), 2, "{backend:?}");
        assert_eq!(state.count(Step::Activate), 1, "{backend:?}");
        assert_eq!(state.count(Step::Create), 1, "{backend:?}: same class");
        assert_eq!(state.count(Step::Post), 1, "{backend:?}");
        assert_eq!(state.count(Step::Delete), 1, "{backend:?}");
        assert_eq!(pauses(), 1, "{backend:?}");
        assert_eq!(
            run.notes
                .iter()
                .filter(|note| note.starts_with("W: runner source check (inactive) retried once: "))
                .count(),
            1,
            "{:?}",
            run.notes
        );
        drop(state);
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_404_on_the_inactive_read_is_retried_once() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        PAUSES.with(|pauses| pauses.set(0));

        let sap = new_sap(backend == BackendKind::AdtHttp);

        fault(&sap, Step::GetInactive, 1, Fault::Status(404, ""));

        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };
        let state = sap.borrow();

        assert!(
            matches!(&run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(state.count(Step::Put), 2, "{backend:?}: one retry");
        assert_eq!(state.count(Step::GetInactive), 2, "{backend:?}");
        assert_eq!(state.count(Step::Create), 1, "{backend:?}: same class");
        assert_eq!(pauses(), 1, "{backend:?}");
        assert!(
            run.notes.iter().any(|note| note
                == "W: runner source check (inactive) retried once: the class has no inactive source"),
            "{backend:?}: {:?}",
            run.notes
        );
        drop(state);
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_missing_implementation_is_retried_once_and_then_rejected_without_a_bug_note() {
    // One disturbed activation: the retry succeeds.
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_IMPLEMENTATION_MISSING),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().count(Step::Activate), 2);
    assert_eq!(pauses(), 1);
    assert!(
        run.notes.iter().any(|note| note
            == "W: runner activate retried once: E row 134: Implementation missing for method \"CHECK_BASE\""),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);

    // Two in a row: one retry only, then the clear rejection.
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    for nth in [1, 2, 3] {
        fault(
            &sap,
            Step::Activate,
            nth,
            Fault::Status(200, MESSAGES_IMPLEMENTATION_MISSING),
        );
    }

    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(
        rejected(&run),
        "runner activate failed after one retry: E row 134: Implementation missing for method \"CHECK_BASE\""
    );
    assert_eq!(run.exit_code(0), 1);
    assert_eq!(
        sap.borrow().count(Step::Activate),
        2,
        "never a second retry"
    );
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_eq!(pauses(), 1);
    assert!(
        run.notes.iter().all(|note| !note.contains("report")),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

#[test]
fn any_rfc_failure_of_a_disturbed_class_gets_a_new_session_and_a_repeat_is_rejected() {
    // The RFC library closes the connection after a runtime failure (code 3) as it does after a
    // lost one (code 1): the retry runs on a new session, the old one is closed.
    for text in [CLASS_MISSING, CLASS_MISSING_KERNEL, CLASS_MISSING_CODE_4] {
        PAUSES.with(|pauses| pauses.set(0));

        let sap = new_sap(false);

        fault(&sap, Step::Activate, 1, Fault::Rfc(text));

        let run = rfc_row(&sap, &CancelFlag::new());
        let state = sap.borrow();

        assert!(
            matches!(&run.end, RunnerEnd::Posted(_)),
            "{text}: {:?}",
            run.end
        );
        assert_eq!(state.on_session(1, Step::Activate), 1, "{text}");
        assert_eq!(state.on_session(2, Step::Activate), 1, "{text}");
        assert_eq!(
            state.on_session(2, Step::Lock),
            2,
            "{text}: write, then cleanup delete"
        );
        assert!(state.closes >= 2, "{text}: the old session is closed");
        assert_eq!(pauses(), 1, "{text}");
        assert!(
            run.notes
                .iter()
                .any(|note| note.starts_with("W: runner activate retried once: ")
                    && !note.contains('\n')),
            "{text}: {:?}",
            run.notes
        );
        drop(state);
        assert_clean(&sap, &run);
    }

    // Repeated: a rejection after one retry, never a classrun STOP.
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(&sap, Step::Activate, 1, Fault::Rfc(CLASS_MISSING));
    fault(&sap, Step::Activate, 2, Fault::Rfc(CLASS_MISSING));

    let run = rfc_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed after one retry: protocol error: "),
        "{:?}",
        run.end
    );
    assert_eq!(run.exit_code(0), 1);
    assert_eq!(pauses(), 1);
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

/// The cycle session is lost, the opener of the retry fails; `later_opens_fail` keeps it failing
/// for the cleanup as well.
fn run_with_failing_swap(sap: &Shared, later_opens_fail: bool) -> RunnerRun {
    let calls = Cell::new(0);
    let mut open = || -> Opened {
        calls.set(calls.get() + 1);

        if calls.get() == 2 || (later_opens_fail && calls.get() > 2) {
            return Err(TransportError::PreSendConnectivity("no route".to_owned()));
        }

        rfc_session(sap)
    };

    run_cycle_with(
        &input(BackendKind::RfcSadt, true),
        &mut open,
        PostRoute::Cycle,
        &CancelFlag::new(),
        &extras(),
    )
}

#[test]
fn a_new_session_that_cannot_be_opened_keeps_the_sap_reason_and_cleans_up_on_another_one() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(&sap, Step::Activate, 1, Fault::Rfc(CLASS_MISSING));

    let run = run_with_failing_swap(&sap, false);
    let text = rejected(&run);

    assert!(
        text.starts_with("runner activate failed after one retry: protocol error: ")
            && text.contains("does not exist (RFC code 3); a new session could not be opened: ")
            && text.contains("no route"),
        "{text}"
    );
    assert_eq!(run.exit_code(0), 1);

    let state = sap.borrow();

    assert_eq!(state.count(Step::Post), 0);
    assert_eq!(
        state.on_session(1, Step::Delete),
        0,
        "never on the dead session"
    );
    assert_eq!(
        state.on_session(2, Step::Delete),
        1,
        "cleanup on a new session"
    );
    drop(state);
    assert_clean(&sap, &run);
}

#[test]
fn a_cleanup_that_cannot_open_a_session_after_a_lost_one_leaves_a_clear_note() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(&sap, Step::Activate, 1, Fault::Rfc(CLASS_MISSING));

    let run = run_with_failing_swap(&sap, true);

    assert!(
        rejected(&run).contains("a new session could not be opened: "),
        "{:?}",
        run.end
    );
    assert!(run.leftover.is_some(), "the class stays");
    assert!(
        run.notes.iter().any(|note| note.starts_with(
            "W: runner cleanup skipped: the cycle session is lost and a new session cannot be opened: "
        )),
        "{:?}",
        run.notes
    );

    let state = sap.borrow();

    assert_eq!(
        state.count(Step::Delete),
        0,
        "no request on the dead session"
    );
    assert_eq!(state.on_session(1, Step::Lock), 1, "only the write lock");
}

#[test]
fn a_class_that_does_not_exist_in_an_http_reply_is_retried() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(
            500,
            "<exc:exception xmlns:exc=\"x\"><message>Class <N> does not exist</message></exc:exception>",
        ),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().count(Step::Activate), 2);
    assert_eq!(pauses(), 1);
    assert!(
        run.notes
            .iter()
            .any(|note| note.starts_with("W: runner activate retried once: ")
                && note.contains("does not exist")),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);

    // A foreign class that does not exist at activation is the standard activation work area
    // of a parallel session: retried once with one `W:` line and ends active.
    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(
            500,
            "<exc:exception xmlns:exc=\"x\"><message>Class CL_WB_ACTIVATION_WORK_AREA====CP does not exist</message></exc:exception>",
        ),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().count(Step::Activate), 2);
    assert_eq!(
        run.notes
            .iter()
            .filter(|note| note.starts_with("W: runner activate retried once: "))
            .count(),
        1,
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

#[test]
fn a_foreign_class_that_does_not_exist_twice_at_activation_is_rejected_after_one_retry() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(
            500,
            "<exc:exception xmlns:exc=\"x\"><message>Class ZOTHER does not exist</message></exc:exception>",
        ),
    );
    fault(
        &sap,
        Step::Activate,
        2,
        Fault::Status(
            500,
            "<exc:exception xmlns:exc=\"x\"><message>Class ZOTHER does not exist</message></exc:exception>",
        ),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed after one retry: "),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Activate), 2);
    assert_clean(&sap, &run);
}

#[test]
fn a_foreign_class_missing_in_an_rfc_activation_failure_is_retried_on_a_new_session() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(&sap, Step::Activate, 1, Fault::Rfc(CLASS_MISSING_FOREIGN));

    let run = rfc_row(&sap, &CancelFlag::new());
    let state = sap.borrow();

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(state.on_session(1, Step::Activate), 1);
    assert_eq!(state.on_session(2, Step::Activate), 1);
    assert_eq!(pauses(), 1);
    assert_eq!(
        run.notes
            .iter()
            .filter(|note| note.starts_with("W: runner activate retried once: "))
            .count(),
        1,
        "{:?}",
        run.notes
    );
    drop(state);
    assert_clean(&sap, &run);
}

#[test]
fn a_foreign_class_missing_in_an_activation_message_is_a_template_defect_and_not_retried() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    sap.borrow_mut().activation = Some(MESSAGES_FOREIGN_CLASS_MISSING);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed: "),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_eq!(pauses(), 0);
    assert!(
        run.notes
            .iter()
            .any(|note| note.starts_with("W: ") && note.contains("report")),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

#[test]
fn an_own_method_that_is_not_declared_in_an_activation_message_is_retried_on_the_same_session() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_OWN_METHOD_NOT_DECLARED),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().on_session(1, Step::Activate), 2);
    assert_eq!(sap.borrow().on_session(2, Step::Activate), 0);
    assert_eq!(sap.borrow().on_session(1, Step::GetSource), 1);
    assert_eq!(pauses(), 1);
    assert_eq!(
        run.notes
            .iter()
            .filter(|note| note.starts_with("W: runner activate retried once"))
            .count(),
        1,
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

#[test]
fn a_foreign_class_in_the_not_declared_activation_message_is_a_template_defect_and_not_retried() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    sap.borrow_mut().activation = Some(MESSAGES_FOREIGN_METHOD_NOT_DECLARED);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed: "),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_eq!(pauses(), 0);
    assert!(
        run.notes
            .iter()
            .any(|note| note.contains("please report this as a bug")),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

#[test]
fn a_method_keyword_inside_a_longer_token_is_not_the_disturbance_sentence() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_SUBMETHOD_NOT_DECLARED),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed: "),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_eq!(pauses(), 0);
    assert!(
        run.notes
            .iter()
            .any(|note| note.contains("please report this as a bug")),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

#[test]
fn an_own_method_that_is_not_declared_in_an_rfc_activation_failure_is_retried_on_a_new_session() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(&sap, Step::Activate, 1, Fault::Rfc(METHOD_NOT_DECLARED));

    let run = rfc_row(&sap, &CancelFlag::new());
    let state = sap.borrow();

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(state.on_session(1, Step::Activate), 1);
    assert_eq!(state.on_session(2, Step::Activate), 1);
    drop(state);
    assert_eq!(pauses(), 1);
    assert_clean(&sap, &run);
}

#[test]
fn a_foreign_class_in_the_not_declared_rfc_activation_failure_is_not_retried() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Rfc(METHOD_NOT_DECLARED_FOREIGN),
    );

    let run = rfc_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed: "),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_eq!(pauses(), 0);
    assert_clean(&sap, &run);
}

#[test]
fn an_activation_error_that_names_the_classrun_interface_is_no_missing_classrun() {
    // A method of the interface without its implementation: retried, then rejected.
    let sap = new_sap(true);

    sap.borrow_mut().activation = Some(MESSAGES_CLASSRUN_METHOD_MISSING);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run)
            .starts_with("runner activate failed after one retry: E row 9: Implementation missing"),
        "{:?}",
        run.end
    );
    assert_eq!(run.exit_code(0), 1);
    assert_eq!(sap.borrow().count(Step::Activate), 2);
    assert_clean(&sap, &run);

    // Any other error that names the interface: a defect of the template, no retry, no STOP.
    let sap = new_sap(true);

    sap.borrow_mut().activation = Some(MESSAGES_UNRELATED_INTERFACE);

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed: E row 3: Statement not allowed"),
        "{:?}",
        run.end
    );
    assert!(run.notes.iter().any(|note| note.contains("report")));
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_clean(&sap, &run);
}

#[test]
fn a_text_that_names_a_method_of_the_interface_is_no_missing_classrun_on_any_marker() {
    for activation in [MESSAGES_METHOD_DOES_NOT_EXIST, MESSAGES_METHOD_NOT_FOUND] {
        let sap = new_sap(true);

        sap.borrow_mut().activation = Some(activation);

        let run = http_row(&sap, &CancelFlag::new());

        assert!(
            rejected(&run).starts_with("runner activate failed: E row 9: IF_OO_ADT_CLASSRUN~MAIN"),
            "{activation}: {:?}",
            run.end
        );
        assert_eq!(sap.borrow().count(Step::Activate), 1, "{activation}");
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_disturbed_class_is_retried_even_when_the_text_names_the_unknown_interface() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_CLASS_AND_INTERFACE),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().count(Step::Activate), 2);
    assert_eq!(pauses(), 1);
    assert_clean(&sap, &run);
}

/// A snippet that stands on rows 20 to 24 of the class source.
fn snippet_extras() -> CycleExtras {
    CycleExtras {
        snippet: Some((20, 5)),
        ..extras()
    }
}

fn http_row_with(sap: &Shared, extras: &CycleExtras) -> RunnerRun {
    let mut open = || http_session(sap);

    run_cycle_with(
        &input(BackendKind::AdtHttp, true),
        &mut open,
        PostRoute::Cycle,
        &CancelFlag::new(),
        extras,
    )
}

#[test]
fn a_snippet_error_that_says_implementation_missing_is_never_retried() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    sap.borrow_mut().activation = Some(MESSAGES_SNIPPET_IMPLEMENTATION_MISSING);

    let run = http_row_with(&sap, &snippet_extras());

    assert_eq!(
        run.end,
        RunnerEnd::Stop(
            "SNIPPET_ERROR: line 3: Implementation missing for method \"LOCAL_METHOD\"".to_owned()
        ),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Activate), 1, "no retry");
    assert_eq!(pauses(), 0);
    assert!(
        run.notes.iter().all(|note| !note.contains("retried")),
        "{:?}",
        run.notes
    );
    assert_clean(&sap, &run);
}

#[test]
fn an_implementation_missing_of_the_runner_frame_is_retried_with_a_snippet_present() {
    // Row 9 is the runner's own frame (the classrun method), outside the snippet rows 20-24.
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_CLASSRUN_METHOD_MISSING),
    );

    let run = http_row_with(&sap, &snippet_extras());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().count(Step::Activate), 2);
    assert_eq!(pauses(), 1);
    assert_clean(&sap, &run);
}

/// The requests a session answered after its first RFC error: none may follow.
#[track_caller]
fn assert_no_request_after_rfc_failure(
    sap: &Shared,
    run: &RunnerRun,
    session: usize,
    kept: &[Step],
) {
    let state = sap.borrow();

    for step in [
        Step::Lock,
        Step::Unlock,
        Step::Delete,
        Step::Proof,
        Step::Post,
    ] {
        if !kept.contains(&step) {
            assert_eq!(
                state.on_session(session, step),
                0,
                "session {session} was used for {step:?}"
            );
        }
    }

    assert!(
        run.notes
            .iter()
            .all(|note| !note.contains("cleanup proof") && !note.contains("cleanup lock failed")),
        "{:?}",
        run.notes
    );
}

#[test]
fn an_rfc_error_that_is_not_retried_still_marks_the_session_dead_for_the_cleanup() {
    // A PUT that fails with an RFC error that names no disturbed class: rejected, no retry.
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(
        &sap,
        Step::Put,
        1,
        Fault::Rfc("RfcInvoke: Internal error: something else (RFC code 3)"),
    );

    let run = rfc_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner PUT failed: "),
        "{:?}",
        run.end
    );
    assert_eq!(pauses(), 0);
    // Session 1 holds only the write lock; the delete runs on a new session.
    assert_no_request_after_rfc_failure(&sap, &run, 1, &[Step::Lock]);
    assert_eq!(sap.borrow().on_session(1, Step::Lock), 1);
    assert_eq!(sap.borrow().on_session(2, Step::Delete), 1);
    assert_clean(&sap, &run);
}

#[test]
fn a_second_disturbed_rfc_failure_leaves_the_cleanup_to_a_new_session() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(&sap, Step::Activate, 1, Fault::Rfc(CLASS_MISSING));
    fault(&sap, Step::Activate, 2, Fault::Rfc(CLASS_MISSING));

    let run = rfc_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner activate failed after one retry: "),
        "{:?}",
        run.end
    );
    // Session 2 ran the retry's write only; the cleanup delete is on session 3.
    assert_no_request_after_rfc_failure(&sap, &run, 2, &[Step::Lock, Step::Unlock]);
    assert_eq!(sap.borrow().on_session(2, Step::Lock), 1);
    assert_eq!(sap.borrow().on_session(3, Step::Delete), 1);
    assert_clean(&sap, &run);
}

#[test]
fn an_inactive_read_that_fails_with_an_rfc_error_is_retried_on_a_new_session() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(false);

    fault(&sap, Step::GetInactive, 1, Fault::Rfc(CLASS_MISSING));

    let run = rfc_row(&sap, &CancelFlag::new());

    assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);
    assert_eq!(sap.borrow().on_session(1, Step::GetInactive), 1);
    assert_eq!(sap.borrow().on_session(2, Step::GetInactive), 1);
    assert_eq!(pauses(), 1);
    assert_clean(&sap, &run);
}

#[test]
fn an_error_inside_the_snippet_wins_over_a_disturbed_message_of_the_same_reply() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_FRAME_AND_SNIPPET),
    );

    let run = http_row_with(&sap, &snippet_extras());

    assert_eq!(
        run.end,
        RunnerEnd::Stop("SNIPPET_ERROR: line 3: Statement is not accessible".to_owned()),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_eq!(pauses(), 0);
    assert_clean(&sap, &run);
}

#[test]
fn a_missing_inactive_source_on_the_second_attempt_goes_on_to_the_activation() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        PAUSES.with(|pauses| pauses.set(0));

        let sap = new_sap(backend == BackendKind::AdtHttp);

        fault(&sap, Step::GetInactive, 1, Fault::Status(404, ""));
        fault(&sap, Step::GetInactive, 2, Fault::Status(404, ""));

        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };
        let state = sap.borrow();

        assert!(
            matches!(&run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(state.count(Step::Put), 2, "{backend:?}");
        assert_eq!(state.count(Step::GetInactive), 2, "{backend:?}");
        assert_eq!(state.count(Step::Activate), 1, "{backend:?}");
        assert_eq!(pauses(), 1, "{backend:?}: one retry only");
        drop(state);
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_missing_inactive_source_on_the_second_attempt_leaves_the_decision_to_the_active_check() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(&sap, Step::GetInactive, 1, Fault::Status(404, ""));
    fault(&sap, Step::GetInactive, 2, Fault::Status(404, ""));
    sap.borrow_mut().corrupt_source = true;

    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(
        rejected(&run),
        "runner source check failed: the active class source differs from the generated source"
    );
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

#[test]
fn a_failed_active_source_read_is_not_retried() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(&sap, Step::GetSource, 1, Fault::Status(500, "boom"));

    let run = http_row(&sap, &CancelFlag::new());

    assert!(
        rejected(&run).starts_with("runner source check failed: "),
        "{:?}",
        run.end
    );
    assert_eq!(pauses(), 0);
    assert_eq!(sap.borrow().count(Step::Put), 1, "no retry");
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

#[test]
fn the_one_retry_is_shared_by_every_stage() {
    // An inactive mismatch uses the retry; an Implementation missing after it is rejected.
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);

    fault(
        &sap,
        Step::GetInactive,
        1,
        Fault::Status(200, "CLASS other DEFINITION."),
    );
    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_IMPLEMENTATION_MISSING),
    );

    let run = http_row(&sap, &CancelFlag::new());

    assert_eq!(
        rejected(&run),
        "runner activate failed after one retry: E row 134: Implementation missing for method \"CHECK_BASE\""
    );
    assert_eq!(pauses(), 1);
    assert_eq!(sap.borrow().count(Step::Put), 2);
    assert_eq!(sap.borrow().count(Step::Activate), 1);
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

thread_local! {
    /// The flag the pause of `cancel_in_pause` sets.
    static PAUSE_CANCEL: RefCell<Option<CancelFlag>> = const { RefCell::new(None) };
}

/// A pause during which the process gets a shutdown request.
fn cancel_in_pause() {
    count_pause();
    PAUSE_CANCEL.with(|flag| {
        if let Some(flag) = flag.borrow().as_ref() {
            flag.request_shutdown();
        }
    });
}

#[test]
fn a_cancel_during_the_pause_before_the_retry_still_removes_the_class() {
    PAUSES.with(|pauses| pauses.set(0));

    let sap = new_sap(true);
    let cancel = CancelFlag::new();

    PAUSE_CANCEL.with(|flag| *flag.borrow_mut() = Some(cancel.clone()));
    fault(
        &sap,
        Step::Activate,
        1,
        Fault::Status(200, MESSAGES_IMPLEMENTATION_MISSING),
    );

    let mut open = || http_session(&sap);
    let run = run_cycle_with(
        &input(BackendKind::AdtHttp, true),
        &mut open,
        PostRoute::Cycle,
        &cancel,
        &CycleExtras {
            pause: Some(cancel_in_pause),
            ..CycleExtras::default()
        },
    );

    PAUSE_CANCEL.with(|flag| *flag.borrow_mut() = None);

    assert_eq!(run.end, RunnerEnd::Interrupted, "{:?}", run.end);
    assert!(run.interrupted);
    assert_eq!(pauses(), 1);
    assert_eq!(
        sap.borrow().count(Step::Activate),
        1,
        "no retry after the cancel"
    );
    assert_eq!(sap.borrow().count(Step::Post), 0);
    assert_clean(&sap, &run);
}

#[test]
fn a_post_404_is_a_missing_classrun_only_when_the_discovery_lacks_it() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let http = backend == BackendKind::AdtHttp;
        let row = |sap: &Shared| match backend {
            BackendKind::AdtHttp => http_row(sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(sap, &CancelFlag::new()),
        };

        // The discovery lists classrun: the 404 is a failure of this run.
        let sap = new_sap(http);

        sap.borrow_mut().discovery = Some(DISCOVERY_WITH_CLASSRUN);
        fault(&sap, Step::Post, 1, Fault::Status(404, "x"));

        let run = row(&sap);

        assert!(
            rejected(&run).starts_with("runner POST failed: HTTP 404"),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(run.exit_code(0), 1, "{backend:?}");
        assert_eq!(
            sap.borrow().count(Step::Post),
            1,
            "{backend:?}: never repeated"
        );
        assert_eq!(sap.borrow().count(Step::Delete), 1, "{backend:?}");
        assert_clean(&sap, &run);

        // The discovery lacks it: classrun is not available, as before.
        let sap = new_sap(http);

        fault(&sap, Step::Post, 1, Fault::Status(404, "x"));

        let run = row(&sap);

        assert_eq!(
            run.end,
            RunnerEnd::Stop(
                "read DTEL needs ADT classrun; not available on DEV (POST 404)".to_owned()
            ),
            "{backend:?}"
        );
        assert_eq!(sap.borrow().count(Step::Post), 1, "{backend:?}");
        // ADT HTTP reads the discovery once, in the probe; RFC-SADT reads it for the 404.
        assert_eq!(
            sap.borrow().count(Step::Preflight),
            1,
            "{backend:?}: one read"
        );
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_discovery_that_names_classrun_outside_a_collection_href_does_not_list_it() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);

        sap.borrow_mut().discovery = Some(DISCOVERY_CLASSRUN_ONLY_IN_TEXT);
        fault(&sap, Step::Post, 1, Fault::Status(404, "x"));

        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };

        assert_eq!(
            run.end,
            RunnerEnd::Stop(
                "read DTEL needs ADT classrun; not available on DEV (POST 404)".to_owned()
            ),
            "{backend:?}"
        );
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_post_404_whose_discovery_cannot_be_read_is_rejected_and_never_a_missing_classrun() {
    // Only RFC-SADT reads the discovery for the 404; ADT HTTP has it from the probe.
    for fault_on_discovery in [
        Fault::Status(500, "x"),
        Fault::LostBefore,
        // A 2xx body that is no discovery document (no collection) says nothing about classrun.
        Fault::Status(200, "<html>proxy page</html>"),
    ] {
        let sap = new_sap(false);

        fault(&sap, Step::Post, 1, Fault::Status(404, "x"));
        fault(&sap, Step::Preflight, 1, fault_on_discovery.clone());

        let run = rfc_row(&sap, &CancelFlag::new());

        assert!(
            rejected(&run).starts_with("runner POST failed: HTTP 404"),
            "{fault_on_discovery:?}: {:?}",
            run.end
        );
        assert_eq!(run.exit_code(0), 1, "{fault_on_discovery:?}");
        assert_eq!(sap.borrow().count(Step::Post), 1, "never repeated");
        assert_eq!(sap.borrow().count(Step::Preflight), 1, "one read");
        assert_clean(&sap, &run);
    }
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
fn a_lost_create_whose_class_is_gone_but_whose_directory_row_stays_is_a_leftover() {
    let sap = new_sap(true);

    sap.borrow_mut().tadir_row = true;
    // The create reply is lost; the class is not there, but its directory entry is.
    fault(&sap, Step::Create, 1, Fault::LostBefore);

    let run = http_row(&sap, &CancelFlag::new());
    let name = run.name.clone().unwrap();

    assert_eq!(run.leftover.as_deref(), Some(name.as_str()));
    assert!(
        run.notes
            .iter()
            .any(|note| note == "W: runner cleanup proof: TADIR still holds the class"),
        "{:?}",
        run.notes
    );
    assert_eq!(sap.borrow().count(Step::Delete), 0);
    assert!(
        run.leftover_text()
            .unwrap()
            .ends_with("remove it in SE03 (Change Object Directory Entries) or SE80")
    );
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
        RunnerEnd::Stop("read DTEL needs ADT classrun; not available on DEV (activate)".to_owned())
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
        RunnerEnd::Stop("read DTEL needs ADT classrun; not available on DEV (POST 404)".to_owned())
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

/// The session each logged request ran on, for the first request whose line starts with `prefix`.
fn session_of(sap: &Shared, prefix: &str) -> usize {
    sap.borrow()
        .log
        .iter()
        .find(|(_, line)| line.starts_with(prefix))
        .map(|(session, _)| *session)
        .unwrap_or_else(|| panic!("no request {prefix}"))
}

#[test]
fn the_classrun_post_runs_on_a_new_session_of_the_cycle_backend_after_the_source_check() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);
        let run = match backend {
            BackendKind::AdtHttp => http_row(&sap, &CancelFlag::new()),
            BackendKind::RfcSadt => rfc_row(&sap, &CancelFlag::new()),
        };

        assert!(matches!(&run.end, RunnerEnd::Posted(_)), "{:?}", run.end);

        let cycle = session_of(&sap, "POST /sap/bc/adt/oo/classes");
        let post = session_of(&sap, "POST /sap/bc/adt/oo/classrun/");
        let lock = session_of(&sap, "DELETE ");
        let sap_ref = sap.borrow();
        let posts: Vec<usize> = sap_ref
            .log
            .iter()
            .filter(|(_, line)| line.starts_with("POST /sap/bc/adt/oo/classrun/"))
            .map(|(session, _)| *session)
            .collect();

        assert_eq!(posts, vec![post], "{backend:?}: one POST");
        assert_ne!(post, cycle, "{backend:?}: not the cycle session");
        // The cycle session keeps the lock and the delete, and the POST session is the one
        // opened after the source check, so it is younger than the cycle session.
        assert_eq!(lock, cycle, "{backend:?}");
        assert!(post > cycle, "{backend:?}");
        // The POST session carries nothing but the POST (and its logoff over HTTP).
        assert!(
            sap_ref
                .log
                .iter()
                .filter(|(session, _)| *session == post)
                .all(
                    |(_, line)| line.starts_with("POST /sap/bc/adt/oo/classrun/")
                        || line.contains("logoff")
                ),
            "{backend:?}: {:?}",
            sap_ref.log
        );
        drop(sap_ref);
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_post_session_of_the_cycle_backend_that_cannot_open_is_rejected_before_any_post() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);
        let opened = std::cell::Cell::new(0);
        // The first open is the cycle session, the second the POST session; later ones (the
        // cleanup proof) work.
        let mut open = || -> Opened {
            opened.set(opened.get() + 1);

            if opened.get() == 2 {
                return Err(TransportError::PreSendConnectivity("no session".to_owned()));
            }

            match backend {
                BackendKind::AdtHttp => http_session(&sap),
                BackendKind::RfcSadt => rfc_session(&sap),
            }
        };
        let run = run_cycle(
            &input(backend, true),
            &mut open,
            PostRoute::Cycle,
            &CancelFlag::new(),
        );

        assert!(
            rejected(&run).starts_with("runner POST failed: cannot open the POST session"),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(sap.borrow().count(Step::Post), 0);
        assert_eq!(
            sap.borrow().count(Step::Delete),
            1,
            "{backend:?}: cleanup ran"
        );
        assert_clean(&sap, &run);
    }
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
    let big = |name: &str, _: OutType| -> Result<String, String> {
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
fn a_session_that_dies_in_the_post_leaves_the_cycle_session_and_its_cleanup_alone() {
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
        // The POST ran on its own session, so the lock and the delete of the cycle session work
        // at the first attempt: no retry, no note.
        assert!(run.notes.is_empty(), "{backend:?}: {:?}", run.notes);
        assert_eq!(sap.borrow().count(Step::Delete), 1, "{backend:?}");
    }
}

#[test]
fn a_cleanup_that_fails_once_is_retried_on_a_fresh_session_and_ends_clean() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let sap = new_sap(backend == BackendKind::AdtHttp);

        // The write lock is the first LOCK, the cleanup lock the second; the retry takes the
        // third and works.
        fault(&sap, Step::Lock, 2, Fault::Status(500, "x"));

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
        assert_eq!(run.exit_code(0), 0, "{backend:?}");
        assert!(
            run.notes
                .iter()
                .any(|note| note.starts_with("W: runner cleanup lock failed: HTTP 500")),
            "{backend:?}: {:?}",
            run.notes
        );
        assert_eq!(sap.borrow().count(Step::Lock), 3, "{backend:?}");
        assert_eq!(sap.borrow().count(Step::Delete), 1, "{backend:?}");
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
