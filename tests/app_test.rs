mod common;
mod replay;

use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::http::HttpTransport;
use abapreach::transport::{Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryIdentity, RfcLibraryInfo, run};
use common::ScriptedTransport;
use replay::{Step, fx, no_inactive, replay};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;

const WARNING: &str = "WARNING: connection DEV_100 uses insecure TLS or HTTP.";
const NO_STATEFUL_SESSION: &str = "ADT HTTP stateful session unavailable (no sap-contextid)";
const V1_HASH: &str = "63886692bf652fa419147f746d6b7760d6c991606fa9603d8ade3caf08ac7c64";
const SOURCE_URI: &str =
    "/sap/bc/adt/programs/programs/ytest_abapreach01/source/main?version=active";

struct Shared<T>(Rc<RefCell<T>>);

impl<T: SadtTransport> SadtTransport for Shared<T> {
    fn request(
        &mut self,
        request: abapreach::transport::Request,
    ) -> Result<Response, TransportError> {
        self.0.borrow_mut().request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.0.borrow_mut().close()
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.0.borrow().stateful_supported()
    }
}

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    library: VecDeque<Result<RfcLibraryInfo, String>>,
    ping: VecDeque<Result<(), TransportError>>,
    adt_opened: usize,
    rfc_opened: usize,
    library_opened: usize,
    ping_opened: usize,
    cancel: CancelFlag,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened += 1;
        self.adt.pop_front().expect("unexpected ADT connect")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened += 1;
        self.rfc.pop_front().expect("unexpected RFC connect")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        self.library_opened += 1;
        self.library
            .pop_front()
            .expect("unexpected RFC library probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        self.ping_opened += 1;
        self.ping.pop_front().expect("unexpected RFC ping")
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

fn shared<T: SadtTransport + 'static>(transport: T) -> (Rc<RefCell<T>>, Opened) {
    let cell = Rc::new(RefCell::new(transport));

    (cell.clone(), Ok(Box::new(Shared(cell))))
}

fn scripted(replies: Vec<Result<Response, TransportError>>) -> Opened {
    shared(ScriptedTransport::replies(replies)).1
}

fn source(text: &str) -> Result<Response, TransportError> {
    Ok(Response::text(200, text))
}

fn pre_send() -> Result<Response, TransportError> {
    Err(TransportError::PreSendConnectivity("no route".to_owned()))
}

/// A read-only request whose reply never arrived: ADT reached, nothing written.
const LOST: &str = "no response to a read-only request: connection closed before message completed";

fn lost() -> Result<Response, TransportError> {
    Err(TransportError::NoResponse(
        "connection closed before message completed".to_owned(),
    ))
}

fn env(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"},"adt":{"insecure":true}},"#,
        r#"{"systemId":"DV2","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}},"#,
        r#"{"systemId":"QAS","clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"02"}}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
        ("DV2_100_USER".to_owned(), "user".to_owned()),
        ("DV2_100_PASS".to_owned(), "secret-pw".to_owned()),
        ("QAS_100_USER".to_owned(), "user".to_owned()),
        ("QAS_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

/// An environment whose config file is `config`, for the tests that need a `policies` block.
fn env_with(directory: &Path, config: &str) -> BTreeMap<String, String> {
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

fn read_args<'a>(out: &'a str, conn: &'a str) -> Vec<&'a str> {
    vec!["abapreach", "read", "PROG", "ZTEST", out, "--conn", conn]
}

fn simulate_args<'a>(file: &'a Path, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args: Vec<&'a str> = vec![
        "abapreach",
        "deploy",
        "PROG",
        "YTEST_ABAPREACH01",
        file.to_str().unwrap(),
        "--simulate",
    ];

    args.extend_from_slice(extra);
    args.extend_from_slice(&["--conn", "dev_100"]);

    args
}

fn rfc_only_simulate_args<'a>(file: &'a Path, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = simulate_args(file, extra);

    *args.last_mut().unwrap() = "qas_100";

    args
}

/// The recorded simulate of v2 over v1: live read, version list, lock, locked re-read, version
/// list again, check, unlock.
fn simulate_steps() -> Vec<Step> {
    vec![
        fx(6),
        no_inactive(),
        fx(7),
        fx(6),
        no_inactive(),
        fx(8),
        fx(9),
    ]
}

fn simulate_replay() -> Opened {
    shared(replay("dev-750-rfc.json", simulate_steps())).1
}

/// The report block plus the data it must precede.
fn assert_report(stdout: &str, backend: &str, reason: &str) {
    let report = format!(
        concat!(
            "backend: {}\n",
            "reason: {}\n",
            "guards: SAP lock/CSRF/transport/authorization, base hash, ",
            "foreign inactive (policy reject)\n",
            "not applied (FM guards): Z/Y name, productive client, system change option, SE06\n",
        ),
        backend, reason
    );
    let start = stdout
        .find(&report)
        .unwrap_or_else(|| panic!("no backend report in {stdout}"));
    let diff = stdout
        .find("--- live\n")
        .unwrap_or_else(|| panic!("no diff in {stdout}"));

    assert!(start < diff, "the report must come before the diff");
}

#[test]
fn simulate_prints_the_backend_report_before_the_diff() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(simulate_replay());

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.ends_with("SIMULATED_OK"));
    assert_report(&exit.stdout, "adt", "default ADT");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
}

#[test]
fn a_leading_bom_and_crlf_in_the_deploy_file_do_not_reach_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(
        &file,
        "\u{feff}REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v2'.\r\n",
    )
    .unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(simulate_replay());

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.ends_with("SIMULATED_OK"));
    assert!(!exit.stdout.contains('\u{feff}'));
}

#[test]
fn a_failed_session_end_is_a_warning_and_changes_neither_status_nor_exit_code() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    let (cell, opened) = shared(
        replay("dev-750-rfc.json", simulate_steps())
            .failing_close(TransportError::PostSendUncertain("gone".to_owned())),
    );

    fake.adt.push_back(opened);

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.ends_with("SIMULATED_OK"));
    assert_eq!(
        exit.stderr,
        "W: session close failed: post-send outcome uncertain: gone (lock was released)"
    );
    assert_eq!(cell.borrow().closes, 1);
}

#[test]
fn an_explicit_rfc_backend_is_reported_in_the_simulate_output() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();

    fake.rfc.push_back(simulate_replay());

    let exit = go(
        &simulate_args(&file, &["--backend", "rfc"]),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert_report(&exit.stdout, "rfc", "explicit --backend");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1));
}

#[test]
fn a_pre_send_fallback_to_rfc_is_reported_with_its_cause() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![pre_send()]));
    fake.rfc.push_back(simulate_replay());

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stderr.contains("falling back to RFC SADT"));
    assert_report(
        &exit.stdout,
        "rfc",
        "fallback to RFC: pre-send connectivity failure: no route",
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn a_get_that_lost_its_reply_falls_back_to_rfc_with_its_cause() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![lost()]));
    fake.rfc.push_back(simulate_replay());

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.ends_with("SIMULATED_OK"));
    assert_report(&exit.stdout, "rfc", &format!("fallback to RFC: {LOST}"));
    assert!(
        exit.stderr.contains(&format!("NOTE: {LOST}")),
        "{}",
        exit.stderr
    );
    assert!(
        exit.stderr.contains(
            "NOTE: ADT HTTP failed before anything was written; falling back to RFC SADT"
        )
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn a_csrf_refetch_401_after_a_put_is_final_and_unlocks_without_rfc() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");
    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let (cell, opened) = shared(replay(
        "dev-750-rfc.json",
        vec![
            fx(6),
            no_inactive(),
            fx(7),
            fx(6),
            no_inactive(),
            fx(8),
            Step::ReplyTo(
                17,
                Err(TransportError::Authorization("HTTP 401".to_owned())),
            ),
            fx(18),
        ],
    ));
    let mut fake = Fake::default();
    fake.adt.push_back(opened);

    let args = [
        "abapreach",
        "deploy",
        "PROG",
        "YTEST_ABAPREACH01",
        file.to_str().unwrap(),
        "--conn",
        "dev_100",
    ];
    let exit = go(&args, directory.path(), &mut fake);
    let transport = cell.borrow();

    assert_eq!(exit.code, 1);
    assert_eq!(exit.stderr, "ERROR: authorization failed: HTTP 401");
    assert_eq!(fake.rfc_opened, 0);
    assert_eq!(
        transport
            .calls
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
    assert_eq!(
        transport
            .calls
            .iter()
            .filter(|request| request.uri.contains("_action=UNLOCK"))
            .count(),
        1
    );
    transport.assert_finished();
}

#[test]
fn a_get_that_lost_its_reply_falls_back_to_rfc_for_read() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![lost()]));
    fake.rfc.push_back(scripted(vec![source("A")]));

    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(
        exit.stderr.contains(&format!("NOTE: {LOST}")),
        "{}",
        exit.stderr
    );
    assert_eq!(fs::read_to_string(out).unwrap(), "A\n");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn a_lost_get_never_falls_back_when_base_pins_the_backend() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![lost()]));

    let exit = go(
        &simulate_args(&file, &["--base", "00"]),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert_eq!(
        exit.stderr,
        format!(
            "ERROR: {LOST}\n\
             NOTE: deploy PROG cannot fall back after pre-send connectivity: --base pins the backend"
        )
    );
    assert_eq!(fake.rfc_opened, 0);
}

#[test]
fn an_unanswered_versions_get_before_lock_falls_back_to_rfc() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    fake.adt.push_back(
        shared(replay(
            "dev-750-rfc.json",
            vec![fx(6), Step::Versions(lost())],
        ))
        .1,
    );
    fake.rfc.push_back(simulate_replay());

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert_report(&exit.stdout, "rfc", &format!("fallback to RFC: {LOST}"));
    assert!(exit.stderr.contains("falling back to RFC SADT"));
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn an_unanswered_versions_get_before_lock_does_not_fall_back_when_base_pins_adt() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    fake.adt.push_back(
        shared(replay(
            "dev-750-rfc.json",
            vec![fx(6), Step::Versions(lost())],
        ))
        .1,
    );

    let exit = go(
        &simulate_args(&file, &["--base", V1_HASH]),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert!(
        exit.stderr.contains(&format!("ERROR: {LOST}")),
        "{}",
        exit.stderr
    );
    assert!(
        exit.stderr.contains("--base pins the backend"),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
}

#[test]
fn an_unanswered_get_after_lock_does_not_fall_back_and_releases_the_lock() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    let (adt, opened) = shared(replay(
        "dev-750-rfc.json",
        vec![fx(6), no_inactive(), fx(7), Step::ReplyTo(6, lost()), fx(9)],
    ));
    fake.adt.push_back(opened);

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stderr.contains(&format!("ERROR: {LOST}")),
        "{}",
        exit.stderr
    );
    assert_eq!(fake.rfc_opened, 0);
    assert_eq!(
        adt.borrow()
            .calls
            .iter()
            .filter(|request| request.uri.contains("_action=UNLOCK"))
            .count(),
        1
    );
}

#[test]
fn a_put_that_lost_its_reply_reconciles_and_never_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    let (_, opened) = shared(replay(
        "dev-750-http.json",
        vec![
            fx(6),
            no_inactive(),
            fx(7),
            fx(6),
            no_inactive(),
            fx(8),
            Step::ReplyTo(
                17,
                Err(TransportError::PostSendUncertain(
                    "connection closed before response".to_owned(),
                )),
            ),
            fx(18),
            fx(20),
            fx(26),
        ],
    ));

    fake.adt.push_back(opened);

    let exit = go(
        &[
            "abapreach",
            "deploy",
            "PROG",
            "YTEST_ABAPREACH01",
            file.to_str().unwrap(),
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert!(
        exit.stdout.contains("ACTIVE (reconciled)"),
        "{}",
        exit.stdout
    );
    assert!(exit.stdout.ends_with("check with read before retrying"));
    assert_eq!(fake.rfc_opened, 0);
}

#[test]
fn read_runs_on_adt_first_and_warns_for_an_insecure_connection() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![source("A\nB")]));

    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.starts_with(&format!("{WARNING}\n2 lines ->")));
    assert_eq!(exit.stderr, "");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
    assert_eq!(fs::read_to_string(out).unwrap(), "A\nB\n");
}

#[test]
fn a_stateful_session_the_server_does_not_keep_falls_back_to_rfc_for_deploy() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    let (cell, opened) = shared(HttpTransport::for_test(vec![
        Ok(Response::text(
            200,
            "REPORT ytest_abapreach01.\n\nWRITE / 'v1'.",
        )),
        Ok(Response::empty(200)),
    ]));

    fake.adt.push_back(opened);
    fake.rfc.push_back(simulate_replay());

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.ends_with("SIMULATED_OK"));
    assert!(exit.stderr.contains("falling back to RFC SADT"));
    assert!(
        exit.stderr
            .lines()
            .any(|line| line == format!("NOTE: {NO_STATEFUL_SESSION}")),
        "a fallback that worked reports its cause as a note: {}",
        exit.stderr
    );
    assert!(!exit.stderr.contains("ERROR: "), "{}", exit.stderr);
    assert_report(
        &exit.stdout,
        "rfc",
        &format!("fallback to RFC: {NO_STATEFUL_SESSION}"),
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
    assert!(
        cell.borrow()
            .recorded()
            .iter()
            .all(|call| !call.uri.contains("_action=LOCK")),
        "the LOCK must not be sent"
    );
}

#[test]
fn a_stateful_fallback_that_then_fails_keeps_the_error_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    let (cell, opened) = shared(HttpTransport::for_test(vec![
        Ok(Response::text(
            200,
            "REPORT ytest_abapreach01.\n\nWRITE / 'v1'.",
        )),
        Ok(Response::empty(200)),
    ]));

    fake.adt.push_back(opened);
    fake.rfc.push_back(scripted(vec![pre_send()]));

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stderr
            .lines()
            .any(|line| line == format!("ERROR: {NO_STATEFUL_SESSION}")),
        "a fallback that failed keeps the error: {}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
    assert!(
        cell.borrow()
            .recorded()
            .iter()
            .all(|call| !call.uri.contains("_action=LOCK")),
        "the LOCK must not be sent"
    );
}

#[test]
fn no_warning_for_a_verified_https_connection() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![source("A")]));

    let exit = go(
        &read_args(out.to_str().unwrap(), "dv2_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(!exit.stdout.contains("WARNING"));
}

#[test]
fn a_pre_send_failure_falls_back_to_rfc_for_read() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![pre_send()]));
    fake.rfc.push_back(scripted(vec![source("A")]));

    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(exit.stderr.contains("falling back to RFC SADT"));
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn a_factory_failure_before_send_also_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(Err(TransportError::PreSendConnectivity(
        "bad CA".to_owned(),
    )));
    fake.rfc.push_back(scripted(vec![source("A")]));

    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(exit.stderr.contains("bad CA"));
}

#[test]
fn a_failing_rfc_fallback_is_final_and_keeps_both_errors() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![pre_send()]));
    fake.rfc.push_back(scripted(vec![pre_send()]));

    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert_eq!(exit.stderr.matches("no route").count(), 2);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn a_post_send_or_http_failure_never_falls_back() {
    for reply in [
        Err(TransportError::PostSendUncertain("timeout".to_owned())),
        Ok(Response::empty(404)),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("out.abap");
        let mut fake = Fake::default();

        fake.adt.push_back(scripted(vec![reply]));

        let exit = go(
            &read_args(out.to_str().unwrap(), "dev_100"),
            directory.path(),
            &mut fake,
        );

        assert_eq!(exit.code, 1);
        assert!(exit.stderr.starts_with("ERROR: "));
        assert_eq!(fake.rfc_opened, 0);
        assert!(!out.exists());
    }
}

/// A refused logon is neither a stopped run nor a fallback, so every operation reports HTTP 401
/// the same way: one authorization failure, exit 1 and one attempt.
#[test]
fn a_refused_logon_on_the_read_get_is_a_single_authorization_failure() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();
    let (cell, opened) = shared(ScriptedTransport::replies(vec![Ok(Response::empty(401))]));

    fake.adt.push_back(opened);

    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert_eq!(exit.stderr, "ERROR: authorization failed: HTTP 401");
    assert_eq!(cell.borrow().calls.len(), 1, "the logon is never retried");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
    assert!(!out.exists());
}

#[test]
fn a_refused_logon_on_the_deploy_get_is_a_single_authorization_failure() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    let (cell, opened) = shared(ScriptedTransport::replies(vec![Ok(Response::empty(401))]));

    fake.adt.push_back(opened);

    let exit = go(&simulate_args(&file, &[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert_eq!(exit.stderr, "ERROR: authorization failed: HTTP 401");
    assert_eq!(cell.borrow().calls.len(), 1, "the logon is never retried");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
}

#[test]
fn an_explicit_backend_is_used_alone_and_base_blocks_the_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");
    let args = |extra: &[&'static str]| {
        let mut args = vec![
            "abapreach",
            "deploy",
            "PROG",
            "ZTEST",
            file.to_str().unwrap(),
        ];

        args.extend_from_slice(extra);
        args.extend_from_slice(&["--conn", "dev_100"]);
        args
    };
    fs::write(&file, "REPORT ztest.\nWRITE / 'x'.\n").unwrap();

    let mut fake = Fake::default();

    fake.rfc
        .push_back(scripted(vec![source("REPORT ztest.\nWRITE / 'x'.")]));

    let exit = go(&args(&["--backend", "rfc"]), directory.path(), &mut fake);

    assert_eq!((exit.code, exit.stdout.ends_with("no changes")), (0, true));
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1));

    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![pre_send()]));

    let exit = go(&args(&["--base", "00"]), directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.contains("--base pins the backend"));
    assert_eq!(fake.rfc_opened, 0);
}

#[test]
fn a_deploy_pre_send_failure_before_lock_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");
    let args = [
        "abapreach",
        "deploy",
        "PROG",
        "ZTEST",
        file.to_str().unwrap(),
        "--conn",
        "dev_100",
    ];
    fs::write(&file, "REPORT ztest.\nWRITE / 'x'.\n").unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![pre_send()]));
    fake.rfc
        .push_back(scripted(vec![source("REPORT ztest.\nWRITE / 'x'.")]));

    let exit = go(&args, directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.ends_with("no changes"));

    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![
        source("REPORT ztest."),
        Err(TransportError::PreSendConnectivity("lock".to_owned())),
    ]));
    fake.rfc
        .push_back(scripted(vec![source("REPORT ztest.\nWRITE / 'x'.")]));

    let exit = go(&args, directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(exit.stderr.contains("falling back to RFC SADT"));
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn local_problems_stop_with_exit_2_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("existing.abap");
    let empty = directory.path().join("empty.abap");
    fs::write(&existing, "old").unwrap();
    fs::write(&empty, "\n").unwrap();
    let missing = directory.path().join("missing.abap");
    let cases: Vec<Vec<&str>> = vec![
        read_args(existing.to_str().unwrap(), "dev_100"),
        read_args("o", "nope"),
        read_args("o", "xyz_100"),
        vec!["abapreach", "bogus"],
        vec![
            "abapreach",
            "deploy",
            "PROG",
            "Z",
            missing.to_str().unwrap(),
            "--conn",
            "dev_100",
        ],
        vec![
            "abapreach",
            "deploy",
            "PROG",
            "Z",
            empty.to_str().unwrap(),
            "--conn",
            "dev_100",
        ],
        vec![
            "abapreach",
            "deploy",
            "PROG",
            "Z?PROG",
            existing.to_str().unwrap(),
            "--conn",
            "dev_100",
        ],
    ];

    for args in cases {
        let mut fake = Fake::default();
        let exit = go(&args, directory.path(), &mut fake);

        assert_eq!(exit.code, 2, "{args:?}");
        assert!(
            exit.stderr.starts_with("STOP: "),
            "{args:?}: {}",
            exit.stderr
        );
        assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0), "{args:?}");
    }
}

#[test]
fn an_invalid_policy_stops_at_load() {
    let directory = tempfile::tempdir().unwrap();
    let config = concat!(
        r#"{"policies":{"foreign_inactive":"maybe"},"servers":[{"systemId":"DEV","#,
        r#""host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
    );

    let mut fake = Fake::default();
    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env_with(directory.path(), config),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr.starts_with("STOP: cannot parse"),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
}

#[test]
fn the_configured_policy_reaches_the_deploy_guard() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");
    let config = concat!(
        r#"{"policies":{"foreign_inactive":"allow"},"servers":[{"systemId":"DEV","#,
        r#""host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
    );

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(
        shared(replay(
            "dev-750-rfc.json",
            vec![
                fx(6),
                replay::feed(replay::FOREIGN_INACTIVE_FEED),
                fx(7),
                fx(6),
                replay::feed(replay::FOREIGN_INACTIVE_FEED),
                fx(8),
                fx(9),
            ],
        ))
        .1,
    );

    let exit = run(
        simulate_args(&file, &[])
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        env_with(directory.path(), config),
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.ends_with("SIMULATED_OK"));
    assert!(exit.stdout.contains("foreign inactive (policy allow)"));
    assert_eq!(
        exit.stderr,
        "W: overwriting the inactive version of OTHERUSER (policy allow)"
    );
}

#[test]
fn a_name_outside_the_program_grammar_stops_the_read_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();
    let args = vec![
        "abapreach",
        "read",
        "PROG",
        "Z?PROG",
        out.to_str().unwrap(),
        "--conn",
        "dev_100",
    ];

    let exit = go(&args, directory.path(), &mut fake);

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
    assert!(!out.exists());
}

#[test]
fn a_missing_config_file_is_a_stop() {
    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        BTreeMap::from([(
            "ABAP_CONNECTIONS".to_owned(),
            "/nonexistent/c.json".to_owned(),
        )]),
        &mut Fake::default(),
    );

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.starts_with("STOP: "));
}

#[test]
fn help_prints_to_stdout_with_exit_0() {
    let exit = run(
        ["abapreach", "--help"].map(str::to_owned).to_vec(),
        BTreeMap::new(),
        &mut Fake::default(),
    );

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.contains("Usage"));
}

#[test]
fn version_prints_the_cargo_version_with_exit_0() {
    let exit = run(
        ["abapreach", "--version"].map(str::to_owned).to_vec(),
        BTreeMap::new(),
        &mut Fake::default(),
    );

    assert_eq!(exit.code, 0);
    assert_eq!(
        exit.stdout,
        format!("abapreach {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(exit.stderr, "");
}

#[test]
fn a_pinned_backend_failure_prints_only_the_no_fallback_note() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    for (extra, pin) in [
        (vec!["--backend", "adt"], "--backend"),
        (vec!["--base", "00"], "--base"),
    ] {
        let mut fake = Fake::default();

        fake.adt.push_back(scripted(vec![pre_send()]));

        let exit = go(&simulate_args(&file, &extra), directory.path(), &mut fake);

        assert_eq!(exit.code, 1, "{extra:?}");
        assert_eq!(
            exit.stderr,
            format!(
                "ERROR: pre-send connectivity failure: no route\n\
                 NOTE: deploy PROG cannot fall back after pre-send connectivity: {pin} pins the backend"
            ),
            "{extra:?}"
        );
        assert_eq!(fake.rfc_opened, 0, "{extra:?}");
    }
}

#[test]
fn the_password_never_reaches_the_output() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(Err(TransportError::Protocol(
        "login failed for secret-pw".to_owned(),
    )));

    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.contains("login failed for ***"));
    assert!(!exit.stderr.contains("secret-pw"));
    assert!(exit.stdout.starts_with(WARNING));
}

#[test]
fn an_rfc_only_connection_reads_on_rfc_without_opening_adt() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fake.rfc.push_back(scripted(vec![source("A\nB")]));

    let exit = go(
        &read_args(out.to_str().unwrap(), "qas_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.starts_with("2 lines ->"), "{}", exit.stdout);
    assert!(!exit.stdout.contains("WARNING"));
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1));
    assert_eq!(fs::read_to_string(out).unwrap(), "A\nB\n");
}

#[test]
fn an_rfc_only_connection_simulates_on_rfc_and_names_the_reason() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();

    fake.rfc.push_back(simulate_replay());

    let exit = go(
        &rfc_only_simulate_args(&file, &[]),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(exit.stdout.ends_with("SIMULATED_OK"));
    assert_report(
        &exit.stdout,
        "rfc",
        "RFC-only connection (ADT not configured)",
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1));
}

#[test]
fn an_explicit_adt_backend_stops_on_an_rfc_only_connection() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();

    let mut fake = Fake::default();
    let exit = go(
        &rfc_only_simulate_args(&file, &["--backend", "adt"]),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert_eq!(
        exit.stderr,
        "STOP: ADT HTTP is not available for deploy PROG"
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
}

#[test]
fn doctor_probes_both_backends_and_warns_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt
        .push_back(scripted(vec![Ok(Response::text(200, "<app:service/>"))]));
    fake.library.push_back(Ok(RfcLibraryInfo {
        identity: RfcLibraryIdentity {
            path: "/opt/sap/nwrfcsdk/lib/libsapnwrfc.so".to_owned(),
            version: "7.6.2".to_owned(),
            architecture: "x86-64".to_owned(),
            family: "NW RFC SDK".to_owned(),
            source: "discovered".to_owned(),
        },
        notes: Vec::new(),
    }));
    fake.ping.push_back(Ok(()));

    let exit = go(
        &["abapreach", "doctor", "--conn", "dev_100"],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert_eq!(exit.stdout.matches(WARNING).count(), 1);
    assert!(exit.stdout.contains("ADT HTTP probe: ok (HTTP 200)"));
    assert!(exit.stdout.contains("RFC SADT probe: ok"));
    assert_eq!(
        (
            fake.adt_opened,
            fake.rfc_opened,
            fake.library_opened,
            fake.ping_opened
        ),
        (1, 0, 1, 1)
    );
}

#[test]
fn the_cancel_flag_of_the_factories_reaches_the_deploy() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");
    fs::write(&file, "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n").unwrap();
    let mut fake = Fake::default();
    let (cell, opened) = shared(replay(
        "dev-750-rfc.json",
        vec![fx(6), no_inactive(), fx(7), fx(9)],
    ));

    fake.adt.push_back(opened);
    fake.cancel.set();

    let args = [
        "abapreach",
        "deploy",
        "PROG",
        "YTEST_ABAPREACH01",
        file.to_str().unwrap(),
        "--conn",
        "dev_100",
    ];
    let exit = go(&args, directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.starts_with("CANCELED"));
    assert_eq!(cell.borrow().closes, 1);
    assert!(
        cell.borrow()
            .calls
            .last()
            .unwrap()
            .uri
            .contains("_action=UNLOCK")
    );
    assert_eq!(cell.borrow().calls[0].uri, SOURCE_URI);
}

/// `QAS_100` is RFC-only with a route from `connections.local.json`; a route that reaches a
/// report line is scrubbed like a password.
#[test]
fn the_route_never_reaches_an_error_message() {
    const ROUTE: &str = "/H/router.example.com/S/3299/W/route-secret";
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fs::write(
        directory.path().join("connections.local.json"),
        format!(r#"{{"QAS_SAPROUTER":"{ROUTE}"}}"#),
    )
    .unwrap();
    fake.rfc
        .push_back(Err(TransportError::PreSendConnectivity(format!(
            "cannot reach {ROUTE}: connection refused"
        ))));

    let exit = go(
        &read_args(out.to_str().unwrap(), "qas_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert!(
        exit.stderr.contains("connection refused"),
        "{}",
        exit.stderr
    );
    assert!(!exit.stderr.contains("route-secret"), "{}", exit.stderr);
    assert!(
        !exit.stderr.contains("router.example.com"),
        "{}",
        exit.stderr
    );
}

/// RFC libraries can report a masked route or a single hop rather than the configured route.
#[test]
fn partial_or_masked_router_routes_are_scrubbed() {
    const ROUTE: &str = "/H/edge.example.com/S/3299/W/route-secret/H/core.example.com/S/3299";
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fs::write(
        directory.path().join("connections.local.json"),
        format!(r#"{{"QAS_SAPROUTER":"{ROUTE}"}}"#),
    )
    .unwrap();
    fake.rfc
        .push_back(Err(TransportError::PreSendConnectivity(
            "cannot reach /H/edge.example.com/S/3299/W/*** via /H/core.example.com/S/3299 (route-secret)"
                .to_owned(),
        )));

    let exit = go(
        &read_args(out.to_str().unwrap(), "qas_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    for secret in ["route-secret", "edge.example.com", "core.example.com"] {
        assert!(!exit.stderr.contains(secret), "{}", exit.stderr);
    }
}

#[test]
fn router_hosts_without_a_route_password_are_scrubbed() {
    const ROUTE: &str = "/H/edge.example.com/S/3299/H/core.example.com/S/3299";
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();

    fs::write(
        directory.path().join("connections.local.json"),
        format!(r#"{{"QAS_SAPROUTER":"{ROUTE}"}}"#),
    )
    .unwrap();
    fake.rfc.push_back(Err(TransportError::PreSendConnectivity(
        "cannot reach /H/core.example.com/S/3299 after /H/edge.example.com/S/3299".to_owned(),
    )));

    let exit = go(
        &read_args(out.to_str().unwrap(), "qas_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    for host in ["edge.example.com", "core.example.com"] {
        assert!(!exit.stderr.contains(host), "{}", exit.stderr);
    }
}
