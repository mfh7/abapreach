//! Every ADT HTTP stateful session ends with exactly one logoff request, on every exit path of the
//! CLI operations; a failed logoff is a `W:` line and never changes the verdict. The transports
//! here are real `HttpTransport`s answered by a script, so the logoff request is the one the
//! product builds. No SAP.

mod common;

use abapreach::config::Connection;
use abapreach::operation::deploy::{CancelFlag, InterruptAction};
use abapreach::transport::http::HttpTransport;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;

const LOGOFF: &str = "GET /sap/public/bc/icf/logoff";
const SOURCE_V1: &str = "REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v1'.";
const NO_INACTIVE: &str = concat!(
    r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom">"#,
    r#"<atom:entry><atom:author><atom:name>DEVUSER</atom:name></atom:author>"#,
    r#"<atom:id>00001</atom:id></atom:entry></atom:feed>"#,
);
const LOCK_BODY: &str = concat!(
    r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
    r#"<LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>"#,
    r#"</DATA></asx:values></asx:abap>"#,
);
const DELETE_LOCK: &str = "<DATA><LOCK_HANDLE>a/b+c=</LOCK_HANDLE><CORRNR></CORRNR>\
    <IS_LOCAL>X</IS_LOCAL></DATA>";

type Reply = Result<Response, TransportError>;
type Opened = Result<Box<dyn SadtTransport>, TransportError>;
type Log = Rc<RefCell<Vec<String>>>;

/// Answers each request in order and logs it as `METHOD uri`; at a LOCK it can raise Ctrl-C.
struct Sequence {
    replies: VecDeque<Reply>,
    log: Log,
    interrupt: Option<CancelFlag>,
}

impl SadtTransport for Sequence {
    fn request(&mut self, request: Request) -> Reply {
        self.log
            .borrow_mut()
            .push(format!("{} {}", request.method, request.uri));

        if let Some(cancel) = &self.interrupt
            && request.uri.contains("_action=LOCK")
        {
            assert_eq!(cancel.on_interrupt(), InterruptAction::Deferred);
        }

        self.replies
            .pop_front()
            .unwrap_or_else(|| Err(TransportError::PreSendConnectivity("unscripted".to_owned())))
    }

    fn close(&mut self) -> Result<(), TransportError> {
        unreachable!("an HttpTransport never closes its script")
    }
}

/// A reply that carries the context cookie of a stateful ADT session.
fn ctx(response: Response) -> Reply {
    let mut response = response;

    response.headers.insert(
        "Set-Cookie".to_owned(),
        "SAP_SESSIONID_DEV_100=s1; path=/; sap-contextid=ctx".to_owned(),
    );

    Ok(response)
}

fn plain(status: u16, body: &str) -> Reply {
    Ok(Response::text(status, body))
}

fn gone() -> Reply {
    Err(TransportError::PostSendUncertain("gone".to_owned()))
}

fn http(replies: Vec<Reply>) -> (Log, Opened) {
    http_with(replies, None)
}

fn http_with(replies: Vec<Reply>, interrupt: Option<CancelFlag>) -> (Log, Opened) {
    let log = Log::default();
    let inner = Sequence {
        replies: replies.into(),
        log: log.clone(),
        interrupt,
    };

    (log.clone(), Ok(Box::new(HttpTransport::wrapping(inner))))
}

fn logoffs(log: &Log) -> usize {
    log.borrow()
        .iter()
        .filter(|line| line.starts_with(LOGOFF))
        .count()
}

fn ends_with_logoff(log: &Log) {
    let log = log.borrow();

    assert!(
        log.last().is_some_and(|line| line.starts_with(LOGOFF)),
        "{log:?}"
    );
}

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    library: VecDeque<Result<RfcLibraryInfo, String>>,
    cancel: CancelFlag,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt.pop_front().expect("unexpected ADT connect")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        panic!("an ADT-pinned run never opens RFC");
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        self.library.pop_front().expect("unexpected library probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        unreachable!("the library is never found here")
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

fn env(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"},"adt":{"insecure":true}}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn go(args: &[&str], directory: &Path, fake: &mut Fake) -> Exit {
    run(
        args.iter().map(|arg| (*arg).to_owned()).collect(),
        env(directory),
        fake,
    )
}

fn read(directory: &Path, fake: &mut Fake) -> Exit {
    let out = directory.join("out.abap");

    go(
        &[
            "abapreach",
            "read",
            "PROG",
            "ZTEST",
            out.to_str().unwrap(),
            "--backend",
            "adt",
            "--force",
            "--conn",
            "dev_100",
        ],
        directory,
        fake,
    )
}

/// The report line a closing failure adds; the secret never appears in it.
fn only_the_close_warning(exit: &Exit, reason: &str) {
    assert_eq!(
        exit.stderr,
        format!("W: session close failed: {reason}"),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("secret-pw") && !exit.stderr.contains("secret-pw"));
    assert!(!exit.stderr.contains("sap-contextid") && !exit.stderr.contains("SAP_SESSIONID"));
}

// ---- read ------------------------------------------------------------------------------------

#[test]
fn a_read_ends_its_session_with_one_logoff() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![ctx(Response::text(200, "A")), plain(200, "")]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = read(directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(exit.stderr, "");
    assert_eq!(log.borrow().len(), 2);
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

#[test]
fn a_read_that_ends_in_a_sap_error_still_logs_off() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![ctx(Response::text(500, "boom")), plain(200, "")]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = read(directory.path(), &mut fake);

    assert_ne!(exit.code, 0);
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

#[test]
fn a_call_that_never_opened_a_session_sends_no_logoff() {
    for first in [
        Err(TransportError::PreSendConnectivity("no route".to_owned())),
        Ok(Response::text(401, "denied")),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (log, opened) = http(vec![first]);
        let mut fake = Fake::default();

        fake.adt.push_back(opened);

        let exit = read(directory.path(), &mut fake);

        assert_ne!(exit.code, 0);
        assert_eq!(log.borrow().len(), 1, "{:?}", log.borrow());
        assert_eq!(logoffs(&log), 0);
    }
}

#[test]
fn a_failed_logoff_is_a_w_line_and_changes_neither_status_nor_exit_code() {
    let directory = tempfile::tempdir().unwrap();
    let (good_log, good) = http(vec![ctx(Response::text(200, "A")), plain(200, "")]);
    let mut fake = Fake::default();

    fake.adt.push_back(good);

    let baseline = read(directory.path(), &mut fake);

    for (second, reason) in [
        (gone(), "post-send outcome uncertain: gone"),
        (plain(500, ""), "HTTP 500: HTTP request failed"),
    ] {
        let (log, opened) = http(vec![ctx(Response::text(200, "A")), second]);

        fake.adt.push_back(opened);

        let exit = read(directory.path(), &mut fake);

        assert_eq!(exit.code, baseline.code);
        assert_eq!(exit.stdout, baseline.stdout);
        only_the_close_warning(&exit, reason);
        assert_eq!(logoffs(&log), 1, "a failed logoff is not retried");
    }

    assert_eq!(logoffs(&good_log), 1);
}

// ---- read-table ------------------------------------------------------------------------------

fn read_table(directory: &Path, fake: &mut Fake) -> Exit {
    go(
        &[
            "abapreach",
            "read-table",
            "T001",
            "--fields",
            "bukrs,butxt",
            "--conn",
            "dev_100",
        ],
        directory,
        fake,
    )
}

fn dd02l() -> Reply {
    ctx(Response::text(
        200,
        common::preview(&[
            ("TABNAME", &[Some("T001")]),
            ("TABCLASS", &[Some("TRANSP")]),
            ("VIEWCLASS", &[None]),
        ]),
    ))
}

fn t001_rows() -> Reply {
    ctx(Response::text(
        200,
        common::preview(&[
            ("BUKRS", &[Some("1000"), Some("2000")]),
            ("BUTXT", &[Some("Alpha"), Some("Beta")]),
        ]),
    ))
}

#[test]
fn read_table_ends_its_session_with_one_logoff() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![dd02l(), t001_rows(), plain(200, "")]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = read_table(directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.contains("1000\tAlpha"), "{}", exit.stdout);
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

#[test]
fn a_failed_read_table_and_a_failed_logoff_report_both_without_a_new_status() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![ctx(Response::text(500, "boom")), gone()]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = read_table(directory.path(), &mut fake);

    assert_ne!(exit.code, 0);
    assert!(
        exit.stderr
            .ends_with("W: session close failed: post-send outcome uncertain: gone"),
        "{}",
        exit.stderr
    );
    assert_eq!(logoffs(&log), 1);
}

// ---- deploy ----------------------------------------------------------------------------------

fn deploy(directory: &Path, fake: &mut Fake) -> Exit {
    let file = directory.join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    go(
        &[
            "abapreach",
            "deploy",
            "PROG",
            "YTEST_ABAPREACH01",
            file.to_str().unwrap(),
            "--simulate",
            "--backend",
            "adt",
            "--conn",
            "dev_100",
        ],
        directory,
        fake,
    )
}

fn simulate_replies(logoff: Reply) -> Vec<Reply> {
    vec![
        ctx(Response::text(200, SOURCE_V1)),
        plain(200, NO_INACTIVE),
        plain(200, LOCK_BODY),
        ctx(Response::text(200, SOURCE_V1)),
        plain(200, NO_INACTIVE),
        plain(200, ""),
        plain(200, ""),
        logoff,
    ]
}

#[test]
fn a_deploy_simulation_ends_with_one_logoff_after_the_unlock() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(simulate_replies(plain(200, "")));
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = deploy(directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.ends_with("SIMULATED_OK"), "{}", exit.stdout);
    assert_eq!(log.borrow().len(), 8, "{:?}", log.borrow());
    assert!(
        log.borrow()[6].contains("_action=UNLOCK"),
        "{:?}",
        log.borrow()
    );
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

#[test]
fn a_deploy_stopped_by_a_refused_lock_still_logs_off() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![
        ctx(Response::text(200, SOURCE_V1)),
        plain(200, NO_INACTIVE),
        plain(403, "locked"),
        plain(200, ""),
    ]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = deploy(directory.path(), &mut fake);

    assert_ne!(exit.code, 0, "{}", exit.stdout);
    assert!(
        log.borrow()[2].contains("_action=LOCK"),
        "{:?}",
        log.borrow()
    );
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

#[test]
fn a_deploy_failing_after_the_session_started_still_logs_off_once() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![
        ctx(Response::text(200, SOURCE_V1)),
        Err(TransportError::NoResponse("closed".to_owned())),
        plain(200, ""),
    ]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = deploy(directory.path(), &mut fake);

    assert_ne!(exit.code, 0, "{}", exit.stdout);
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

#[test]
fn a_failed_logoff_after_a_deploy_keeps_status_and_exit_code() {
    let directory = tempfile::tempdir().unwrap();
    let (_, good) = http(simulate_replies(plain(200, "")));
    let mut fake = Fake::default();

    fake.adt.push_back(good);

    let baseline = deploy(directory.path(), &mut fake);
    let (log, failing) = http(simulate_replies(gone()));

    fake.adt.push_back(failing);

    let exit = deploy(directory.path(), &mut fake);

    assert_eq!(exit.code, baseline.code);
    assert_eq!(exit.stdout, baseline.stdout);
    assert!(exit.stdout.ends_with("SIMULATED_OK"), "{}", exit.stdout);
    only_the_close_warning(
        &exit,
        "post-send outcome uncertain: gone (lock was released)",
    );
    assert_eq!(logoffs(&log), 1);
}

#[test]
fn ctrl_c_inside_the_lock_unlocks_logs_off_once_and_exits_130() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let (log, opened) = http_with(simulate_replies(plain(200, "")), Some(fake.cancel.clone()));

    fake.adt.push_back(opened);

    let exit = deploy(directory.path(), &mut fake);

    assert_eq!(exit.code, 130, "{} {}", exit.stdout, exit.stderr);
    assert!(
        log.borrow()
            .iter()
            .any(|line| line.contains("_action=UNLOCK")),
        "{:?}",
        log.borrow()
    );
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

// ---- delete ----------------------------------------------------------------------------------

fn delete_facts() -> Vec<Reply> {
    vec![
        ctx(Response::text(
            200,
            "<x xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:responsible=\"OWNER\">\
             <adtcore:packageRef adtcore:name=\"$TMP\"/></x>",
        )),
        plain(200, "REPORT ztest_del."),
        // The where-used search of a PROG, between its source and its version list.
        plain(
            200,
            "<usageReferences:usageReferenceResult xmlns:usageReferences=\"u\"/>",
        ),
        plain(200, "<feed xmlns=\"http://www.w3.org/2005/Atom\"/>"),
    ]
}

fn delete(directory: &Path, fake: &mut Fake, simulate: bool) -> Exit {
    let plan = directory.join("plan.json");
    let mut args = vec![
        "abapreach",
        "delete",
        "PROG:ZTEST_DEL",
        "--plan",
        plan.to_str().unwrap(),
    ];

    if simulate {
        args.push("--simulate");
    }

    args.extend_from_slice(&["--conn", "dev_100"]);

    go(&args, directory, fake)
}

#[test]
fn a_delete_simulation_and_a_delete_each_end_every_session_with_one_logoff() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let mut simulated = delete_facts();

    simulated.push(plain(200, ""));

    let (log, opened) = http(simulated);

    fake.adt.push_back(opened);

    let exit = delete(directory.path(), &mut fake, true);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.contains("SIMULATED_OK"), "{}", exit.stdout);
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);

    // The delete itself: the main session, and the fresh session that proves the object is gone.
    // The fresh session got no cookie from the 404, so it never opened and sends no logoff.
    let mut replies = delete_facts();

    replies.extend([plain(200, DELETE_LOCK), plain(200, ""), plain(200, "")]);

    let (main_log, main) = http(replies);
    let (fresh_log, fresh) = http(vec![plain(404, "<exc/>")]);

    fake.adt.push_back(main);
    fake.adt.push_back(fresh);

    let exit = delete(directory.path(), &mut fake, false);

    assert_eq!(exit.code, 0, "{} {}", exit.stdout, exit.stderr);
    assert!(exit.stdout.contains("DELETED"), "{}", exit.stdout);
    assert_eq!(logoffs(&main_log), 1);
    ends_with_logoff(&main_log);
    assert_eq!(fresh_log.borrow().len(), 1);
    assert_eq!(logoffs(&fresh_log), 0);
}

#[test]
fn the_fresh_session_of_a_delete_logs_off_and_a_failure_there_is_a_w_line() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let mut simulated = delete_facts();

    simulated.push(plain(200, ""));
    fake.adt.push_back(http(simulated).1);

    assert_eq!(delete(directory.path(), &mut fake, true).code, 0);

    let mut replies = delete_facts();

    replies.extend([plain(200, DELETE_LOCK), plain(200, ""), plain(200, "")]);

    let (main_log, main) = http(replies);
    let (fresh_log, fresh) = http(vec![ctx(Response::text(404, "<exc/>")), gone()]);

    fake.adt.push_back(main);
    fake.adt.push_back(fresh);

    let exit = delete(directory.path(), &mut fake, false);

    assert_eq!(exit.code, 0, "{} {}", exit.stdout, exit.stderr);
    assert!(exit.stdout.contains("DELETED"), "{}", exit.stdout);
    only_the_close_warning(&exit, "post-send outcome uncertain: gone");
    assert_eq!(logoffs(&fresh_log), 1);
    assert_eq!(logoffs(&main_log), 1);
}

// ---- doctor ----------------------------------------------------------------------------------

fn doctor(directory: &Path, fake: &mut Fake) -> Exit {
    fake.library.push_back(Err("not found".to_owned()));

    go(
        &["abapreach", "doctor", "--conn", "dev_100"],
        directory,
        fake,
    )
}

#[test]
fn doctor_ends_its_probe_session_with_one_logoff() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![
        ctx(Response::text(200, "<app:service/>")),
        plain(200, ""),
    ]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = doctor(directory.path(), &mut fake);

    assert!(
        exit.stdout.contains("ADT HTTP probe: ok (HTTP 200)"),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("W: "), "{}", exit.stdout);
    assert_eq!(logoffs(&log), 1);
    ends_with_logoff(&log);
}

#[test]
fn doctor_reports_a_failed_logoff_as_a_w_line_and_keeps_the_probe_result() {
    let directory = tempfile::tempdir().unwrap();
    let (log, opened) = http(vec![ctx(Response::text(200, "<app:service/>")), gone()]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = doctor(directory.path(), &mut fake);
    let lines: Vec<&str> = exit.stdout.lines().collect();
    let probe = lines
        .iter()
        .position(|line| *line == "ADT HTTP probe: ok (HTTP 200)")
        .unwrap();

    assert!(
        lines.contains(&"W: session close failed: post-send outcome uncertain: gone"),
        "{}",
        exit.stdout
    );
    assert!(probe < lines.len());
    assert_eq!(logoffs(&log), 1);
}
