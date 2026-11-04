mod common;
mod replay;

use abapreach::cli::{Cli, Command, DeleteKind};
use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::http::HttpTransport;
use abapreach::transport::{Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryIdentity, RfcLibraryInfo, run};
use clap::Parser;
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
            chars: "UTF-16".to_owned(),
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

    assert_eq!(exit.code, 130);
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

// ---- M3 reads through the CLI: kinds, `--lines`, and the pre-connection rules ----------

const CLAS_HASH: &str = "637bb4092dc9670f5d76a3a2c7f6426d697b31cdb39952cd69452041ff6451a3";
const CLAS_BODY: &str = concat!(
    "class YTEST_ABAPREACH_CL3 definition\r\n  public\r\n  create private .\r\n\r\n",
    "public section.\r\nprotected section.\r\nprivate section.\r\nENDCLASS.\r\n\r\n\r\n\r\n",
    "CLASS YTEST_ABAPREACH_CL3 IMPLEMENTATION.\r\nENDCLASS."
);

fn read_kind_args<'a>(kind: &'a str, name: &'a str, out: &'a str, conn: &'a str) -> Vec<&'a str> {
    vec!["abapreach", "read", kind, name, out, "--conn", conn]
}

#[test]
fn a_class_read_runs_on_adt_and_writes_the_active_source() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("clas.abap");
    let mut fake = Fake::default();

    fake.adt
        .push_back(scripted(vec![Ok(Response::text(200, CLAS_BODY))]));

    let exit = go(
        &read_kind_args(
            "CLAS",
            "YTEST_ABAPREACH_CL3",
            out.to_str().unwrap(),
            "dev_100",
        ),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(
        exit.stdout,
        format!("{WARNING}\n13 lines -> {}\nhash {CLAS_HASH}", out.display())
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
    assert!(fs::read_to_string(out).unwrap().contains("ENDCLASS."));
}

#[test]
fn a_lines_read_prints_the_interval_and_writes_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt
        .push_back(scripted(vec![Ok(Response::text(200, CLAS_BODY))]));

    let exit = go(
        &[
            "abapreach",
            "read",
            "CLAS",
            "YTEST_ABAPREACH_CL3",
            "-",
            "--lines",
            "1-2",
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(exit.stderr, "");
    assert_eq!(
        exit.stdout,
        format!(
            "{WARNING}\nclass YTEST_ABAPREACH_CL3 definition\n  public\n\
             2 lines -> - (lines 1-2)\nhash {CLAS_HASH}"
        )
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
}

#[test]
fn a_lines_read_to_a_file_path_stops_before_any_transport() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("partial.abap");
    let mut fake = Fake::default();

    let exit = go(
        &[
            "abapreach",
            "read",
            "PROG",
            "ZTEST",
            out.to_str().unwrap(),
            "--lines",
            "1-2",
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert_eq!(exit.stdout, WARNING);
    assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    assert!(exit.stderr.contains("stdout"), "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
    assert!(!out.exists());
}

#[test]
fn a_supplied_group_that_does_not_match_the_resolution_stops_with_exit_2() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("fm.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![Ok(Response::text(
        200,
        concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><adtcore:objectReferences ",
            "xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference ",
            "adtcore:uri=\"/sap/bc/adt/functions/groups/ytest_abapreach_fg3/fmodules/",
            "ytest_abapreach_fm3\" adtcore:type=\"FUGR/FF\" ",
            "adtcore:name=\"YTEST_ABAPREACH_FM3\"/></adtcore:objectReferences>"
        ),
    ))]));

    let exit = go(
        &[
            "abapreach",
            "read",
            "FUNC",
            "YTEST_ABAPREACH_FM3",
            out.to_str().unwrap(),
            "--fugr",
            "ZOTHER",
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 2, "{}", exit.stderr);
    assert_eq!(exit.stdout, WARNING, "a stop prints no result line");
    assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    assert!(exit.stderr.contains("--fugr ZOTHER"), "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
    assert!(!out.exists());
}

#[test]
fn a_read_of_a_fresh_class_reports_the_unreadable_state_and_exits_1() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("clas.abap");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![
        Ok(Response::empty(500)),
        Ok(Response::empty(200)),
    ]));

    let exit = go(
        &read_kind_args(
            "CLAS",
            "YTEST_ABAPREACH_CL4",
            out.to_str().unwrap(),
            "dev_100",
        ),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1, "{}", exit.stdout);
    assert!(exit.stderr.starts_with("ERROR: "), "{}", exit.stderr);
    assert!(
        exit.stderr.contains("no readable source yet"),
        "{}",
        exit.stderr
    );
    assert!(!out.exists());
}

#[test]
fn an_existing_output_stops_before_any_transport() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");

    fs::write(&out, "do not overwrite\n").unwrap();

    let mut fake = Fake::default();
    let exit = go(
        &read_args(out.to_str().unwrap(), "dev_100"),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.contains("--force"), "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
    assert_eq!(fs::read_to_string(&out).unwrap(), "do not overwrite\n");
}

#[test]
fn deploy_of_the_m3_kinds_reaches_the_transport_instead_of_the_not_available_stop() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("src.abap");

    fs::write(&file, "CLASS ltc_a DEFINITION FOR TESTING.\nENDCLASS.\n").unwrap();

    // A class include that is absent while the class exists: no flag, so nothing is written.
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![
        Ok(Response::empty(404)),
        Ok(Response::text(200, "<structure/>")),
    ]));

    let exit = go(
        &[
            "abapreach",
            "deploy",
            "CLAS",
            "YTEST_ABAPREACH_CL",
            file.to_str().unwrap(),
            "--include",
            "CCAU",
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 1, "{}", exit.stderr);
    assert!(
        exit.stdout
            .ends_with("INCLUDE_MISSING: CCAU (re-run with --create-include)"),
        "{}",
        exit.stdout
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));

    // A FUNC whose --fugr names another group than the server resolves stops after the lookup.
    let mut fake = Fake::default();
    let search = concat!(
        r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
        r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/YTEST_FG/fmodules/YTEST_FM" "#,
        r#"adtcore:type="FUGR/FF" adtcore:name="YTEST_FM"/></adtcore:objectReferences>"#,
    );

    fake.adt
        .push_back(scripted(vec![Ok(Response::text(200, search))]));

    fs::write(&file, "FUNCTION ytest_fm.\n  WRITE 'x'.\nENDFUNCTION.\n").unwrap();

    let exit = go(
        &[
            "abapreach",
            "deploy",
            "FUNC",
            "YTEST_FM",
            file.to_str().unwrap(),
            "--fugr",
            "OTHER_FG",
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 2, "{}", exit.stderr);
    assert!(
        exit.stderr
            .contains("--fugr OTHER_FG does not match function group YTEST_FG"),
        "{}",
        exit.stderr
    );
}

// ---- create ----------------------------------------------------------------------------

const CREATE_VALIDATION: &str = "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA><SEVERITY>OK</SEVERITY><SHORT_TEXT/></DATA></asx:values></asx:abap>";

fn create_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "abapreach",
        "create",
        "FUGR",
        "YTEST_ABAPREACH_FG",
        "--text",
        "probe",
    ];

    args.extend_from_slice(extra);
    args.extend_from_slice(&["--conn", "dev_100"]);

    args
}

#[test]
fn create_runs_on_adt_and_reports_the_backend_and_the_token() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![
        source(CREATE_VALIDATION),
        Ok(Response::empty(200)),
    ]));

    let exit = go(&create_args(&[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(
        exit.stdout.contains("backend: adt\nreason: default ADT"),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stdout.lines().any(|line| line == "CREATE: ACTIVE"),
        "{}",
        exit.stdout
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
}

#[test]
fn create_simulate_prints_its_token_and_a_pre_send_failure_falls_back_to_rfc() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![pre_send()]));
    fake.rfc
        .push_back(scripted(vec![source(CREATE_VALIDATION)]));

    let exit = go(&create_args(&["--simulate"]), directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(
        exit.stdout
            .lines()
            .any(|line| line == "CREATE_SIM: SIMULATED_OK"),
        "{}",
        exit.stdout
    );
    assert!(exit.stdout.contains("backend: rfc"), "{}", exit.stdout);
    assert!(
        exit.stderr.contains("falling back to RFC SADT"),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn an_uncertain_create_never_falls_back_to_the_other_backend() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![
        source(CREATE_VALIDATION),
        Err(TransportError::PostSendUncertain("dropped".to_owned())),
        Ok(Response::empty(200)),
    ]));

    let exit = go(&create_args(&[]), directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stdout.lines().any(|line| line == "CREATE: PARTIAL"),
        "{}",
        exit.stdout
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
}

#[test]
fn create_with_a_transportable_package_and_no_request_stops_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let exit = go(
        &create_args(&["--package", "ZPKG_TEST"]),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));

    let exit = go(
        &create_args(&["--package", "ZPKG_TEST", "--tr", "DEVK9&x"]),
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.contains("invalid --tr"), "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
}

fn assert_stops_locally(args: &[&str]) {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let exit = go(args, directory.path(), &mut fake);

    assert_eq!(exit.code, 2, "{args:?}: {}", exit.stderr);
    assert!(
        exit.stderr.starts_with("STOP: "),
        "{args:?}: {}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0), "{args:?}");
}

#[test]
fn read_table_accepts_its_grammar_and_rejects_bad_rows_locally() {
    let cli = |extra: &[&str]| {
        let mut args = vec!["abapreach", "read-table", "T001"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--conn", "dev_100"]);

        Cli::try_parse_from(args)
    };

    match cli(&[
        "--fields",
        "BUKRS,BUTXT",
        "--where",
        "BUKRS = '1000'",
        "--rows",
        "5000",
    ])
    .unwrap()
    .command
    {
        Command::ReadTable {
            table,
            fields,
            where_clause,
            rows,
            ..
        } => {
            assert_eq!(table, "T001");
            assert_eq!(fields, ["BUKRS", "BUTXT"]);
            assert_eq!(where_clause.as_deref(), Some("BUKRS = '1000'"));
            assert_eq!(rows, 5000);
        }
        other => panic!("{other:?}"),
    }
    match cli(&[]).unwrap().command {
        Command::ReadTable {
            fields,
            where_clause,
            rows,
            ..
        } => {
            assert!(fields.is_empty() && where_clause.is_none());
            assert_eq!(rows, 100);
        }
        other => panic!("{other:?}"),
    }
    for rows in ["0", "5001", "-1", "x"] {
        assert!(cli(&["--rows", rows]).is_err(), "{rows}");
        assert_stops_locally(&[
            "abapreach",
            "read-table",
            "T001",
            "--rows",
            rows,
            "--conn",
            "dev_100",
        ]);
    }
    assert!(Cli::try_parse_from(["abapreach", "read-table", "--conn", "dev_100"]).is_err());
    assert!(Cli::try_parse_from(["abapreach", "read-table", "T001"]).is_err());
}

#[test]
fn delete_accepts_type_name_targets_and_rejects_bad_ones_locally() {
    let parse = |args: &[&str]| {
        let mut full = vec!["abapreach", "delete"];
        full.extend_from_slice(args);

        Cli::try_parse_from(full)
    };

    match parse(&[
        "--simulate",
        "FUNC:ZFM_A",
        "CLAS:ZCL_A",
        "--tr",
        "DEVK900001",
        "--plan",
        "p.txt",
        "--conn",
        "dev_100",
    ])
    .unwrap()
    .command
    {
        Command::Delete {
            targets,
            simulate,
            tr,
            plan,
            ..
        } => {
            assert!(simulate);
            assert_eq!(targets.len(), 2);
            assert_eq!(targets[0].kind, DeleteKind::Func);
            assert_eq!(targets[1].name, "ZCL_A");
            assert_eq!(tr.as_deref(), Some("DEVK900001"));
            assert_eq!(plan, std::path::PathBuf::from("p.txt"));
        }
        other => panic!("{other:?}"),
    }
    for kinds in ["PROG", "FUGR", "FUNC", "CLAS", "INTF"] {
        assert!(parse(&[&format!("{kinds}:ZX"), "--plan", "p", "--conn", "dev_100"]).is_ok());
    }
    // No target, no plan, no conn, an unknown or unsupported type, an empty name, no colon.
    for bad in [
        &["--plan", "p", "--conn", "dev_100"][..],
        &["PROG:ZX", "--conn", "dev_100"],
        &["PROG:ZX", "--plan", "p"],
        &["TABL:ZX", "--plan", "p", "--conn", "dev_100"],
        &["PROG:", "--plan", "p", "--conn", "dev_100"],
        &["ZX", "--plan", "p", "--conn", "dev_100"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
        let mut args = vec!["abapreach", "delete"];
        args.extend_from_slice(bad);
        assert_stops_locally(&args);
    }
}

#[test]
fn mcp_takes_no_arguments_and_stops_on_an_invalid_policy_with_exit_2() {
    assert!(matches!(
        Cli::try_parse_from(["abapreach", "mcp"]).unwrap().command,
        Command::Mcp
    ));
    assert!(Cli::try_parse_from(["abapreach", "mcp", "--conn", "dev_100"]).is_err());

    let directory = tempfile::tempdir().unwrap();
    let config = |policies: &str| {
        format!(
            r#"{{"policies":{policies},"servers":[{{"systemId":"DEV","clients":["100"],"rfc":{{"ashost":"example.com","sysnr":"00"}}}}]}}"#
        )
    };
    let mut fake = Fake::default();
    let exit = run(
        ["abapreach", "mcp"].map(str::to_owned).to_vec(),
        env_with(directory.path(), &config(r#"{"mcp_write":"always"}"#)),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr.starts_with("STOP: cannot parse"),
        "{}",
        exit.stderr
    );
    assert!(
        exit.stderr.contains("unknown variant `always`"),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
}

fn dd02l(name: &str, class: &str, view_class: Option<&str>) -> Result<Response, TransportError> {
    source(&common::preview(&[
        ("TABNAME", &[Some(name)]),
        ("TABCLASS", &[Some(class)]),
        ("VIEWCLASS", &[view_class]),
    ]))
}

fn t001_rows() -> Result<Response, TransportError> {
    source(&common::preview(&[
        ("BUKRS", &[Some("1000"), Some("2000")]),
        ("BUTXT", &[Some("Alpha"), Some("Beta")]),
    ]))
}

/// The stdout without the insecure-TLS warning line the DEV test server adds.
fn table_of(exit: &Exit) -> String {
    exit.stdout
        .lines()
        .filter(|line| *line != WARNING)
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_table(args: &[&str], fake: &mut Fake) -> Exit {
    let directory = tempfile::tempdir().unwrap();
    let mut full = vec!["abapreach", "read-table"];

    full.extend_from_slice(args);

    go(&full, directory.path(), fake)
}

#[test]
fn read_table_stops_locally_on_a_denied_table_or_a_bad_grammar() {
    for args in [
        vec!["usr02", "--conn", "dev_100"],
        vec![
            "T001",
            "--where",
            "BUKRS IN ( SELECT BNAME FROM USR02 )",
            "--conn",
            "dev_100",
        ],
        vec!["T001", "--where", "BUKRS = 'open", "--conn", "dev_100"],
        vec!["T001", "--fields", "BUKRS,COUNT(*)", "--conn", "dev_100"],
        vec!["T001~X", "--conn", "dev_100"],
    ] {
        let mut fake = Fake::default();
        let exit = read_table(&args, &mut fake);

        assert_eq!(exit.code, 2, "{args:?}: {}", exit.stderr);
        assert!(
            exit.stderr.starts_with("STOP: "),
            "{args:?}: {}",
            exit.stderr
        );
        assert_eq!(table_of(&exit), "", "{args:?}");
        assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0), "{args:?}");
    }

    let mut fake = Fake::default();
    let exit = read_table(&["usr02", "--conn", "dev_100"], &mut fake);

    assert!(exit.stderr.contains("USR02") && exit.stderr.contains("read_table_denied"));
}

#[test]
fn read_table_prints_a_tab_separated_table_over_adt() {
    let (calls, adt) = shared(ScriptedTransport::replies(vec![
        dd02l("T001", "TRANSP", None),
        t001_rows(),
    ]));
    let mut fake = Fake::default();

    fake.adt.push_back(adt);

    let exit = read_table(
        &[
            "t001",
            "--fields",
            "bukrs,butxt",
            "--where",
            "bukrs <> '0001'",
            "--rows",
            "2",
            "--conn",
            "dev_100",
        ],
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(table_of(&exit), "BUKRS\tBUTXT\n1000\tAlpha\n2000\tBeta");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
    assert_eq!(calls.borrow().closes, 1);

    let calls = calls.borrow();

    assert_eq!(calls.calls.len(), 2);
    assert_eq!(
        calls.calls[1].uri,
        "/sap/bc/adt/datapreview/ddic?rowNumber=2&ddicEntityName=T001"
    );
    assert_eq!(
        String::from_utf8(calls.calls[1].body.clone()).unwrap(),
        "SELECT BUKRS, BUTXT FROM T001 WHERE BUKRS <> '0001'"
    );
}

#[test]
fn an_emptied_deny_list_lets_read_table_through_the_policy() {
    let directory = tempfile::tempdir().unwrap();
    let config = concat!(
        r#"{"policies":{"read_table_denied":[]},"servers":[{"systemId":"DEV","host":"example.com","#,
        r#""port":443,"clients":["100"],"rfc":{"ashost":"example.com","sysnr":"00"},"adt":{"insecure":true}}]}"#,
    );
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![
        dd02l("USR02", "TRANSP", None),
        source(&common::preview(&[("BNAME", &[Some("X")])])),
    ]));

    let exit = run(
        ["abapreach", "read-table", "usr02", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env_with(directory.path(), config),
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(table_of(&exit).starts_with("BNAME"));
}

#[test]
fn a_view_over_a_denied_table_is_rejected_after_the_lookups_only() {
    let (calls, adt) = shared(ScriptedTransport::replies(vec![
        dd02l("ZV_USERS", "VIEW", Some("D")),
        source(&common::preview(&[
            ("VIEWNAME", &[Some("ZV_USERS")]),
            ("TABNAME", &[Some("USR02")]),
            ("TABPOS", &[Some("0001")]),
        ])),
    ]));
    let mut fake = Fake::default();

    fake.adt.push_back(adt);

    let exit = read_table(&["zv_users", "--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.starts_with("REJECTED: "), "{}", exit.stderr);
    assert!(exit.stderr.contains("USR02"), "{}", exit.stderr);
    assert_eq!(table_of(&exit), "");
    assert_eq!(calls.borrow().calls.len(), 2);
    assert_eq!(fake.rfc_opened, 0, "a rejection never falls back");
}

#[test]
fn a_failed_data_read_is_an_error_without_fallback() {
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![
        dd02l("T001", "TRANSP", None),
        Ok(Response::text(400, "<exc:exception><localizedMessage>Unknown column name \"NOPE\"</localizedMessage></exc:exception>")),
    ]));

    let exit = read_table(&["T001", "--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.starts_with("ERROR: "), "{}", exit.stderr);
    assert!(
        exit.stderr.contains("Unknown column name"),
        "{}",
        exit.stderr
    );
    assert_eq!(fake.rfc_opened, 0);
}

#[test]
fn read_table_falls_back_to_rfc_only_before_anything_was_read() {
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(vec![pre_send()]));
    fake.rfc
        .push_back(scripted(vec![dd02l("T001", "TRANSP", None), t001_rows()]));

    let exit = read_table(&["T001", "--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(
        table_of(&exit).starts_with("BUKRS\tBUTXT\n1000\tAlpha"),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stderr.contains("falling back to RFC SADT"),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn an_rfc_only_connection_reads_over_a_fresh_rfc_connection() {
    let mut fake = Fake::default();

    fake.rfc
        .push_back(scripted(vec![dd02l("T001", "TRANSP", None), t001_rows()]));

    let exit = read_table(&["T001", "--conn", "qas_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1));
}

// ---- delete ----------------------------------------------------------------------------------

const DELETE_PROG: &str = "/sap/bc/adt/programs/programs/ztest_del";
const DELETE_LOCK: &str = "<DATA><LOCK_HANDLE>a/b+c=</LOCK_HANDLE><CORRNR></CORRNR>\
    <IS_LOCAL>X</IS_LOCAL></DATA>";

fn delete_facts() -> Vec<Result<Response, TransportError>> {
    vec![
        source(
            "<x xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:responsible=\"OWNER\">\
             <adtcore:packageRef adtcore:name=\"$TMP\"/></x>",
        ),
        source("REPORT ztest_del."),
        source("<feed xmlns=\"http://www.w3.org/2005/Atom\"/>"),
    ]
}

fn delete_args(plan: &str, simulate: bool) -> Vec<&str> {
    let mut args = vec!["abapreach", "delete", "PROG:ZTEST_DEL", "--plan", plan];

    if simulate {
        args.push("--simulate");
    }

    args.extend_from_slice(&["--conn", "dev_100"]);

    args
}

#[test]
fn delete_simulates_then_deletes_on_adt_and_proves_it_from_a_fresh_session() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let plan = plan.to_str().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(delete_facts()));

    let simulated = go(&delete_args(plan, true), directory.path(), &mut fake);

    assert_eq!(simulated.code, 0, "{}", simulated.stderr);
    assert!(
        simulated.stdout.contains("SIMULATED_OK"),
        "{}",
        simulated.stdout
    );

    let mut replies = delete_facts();

    replies.push(Ok(Response::text(200, DELETE_LOCK)));
    replies.push(Ok(Response::empty(200)));
    fake.adt.push_back(scripted(replies));

    let (cell, session) = shared(ScriptedTransport::replies(vec![Ok(Response::text(
        404, "<exc/>",
    ))]));

    fake.adt.push_back(session);

    let deleted = go(&delete_args(plan, false), directory.path(), &mut fake);

    assert_eq!(deleted.code, 0, "{} {}", deleted.stdout, deleted.stderr);
    assert!(deleted.stdout.contains("DELETED"), "{}", deleted.stdout);
    assert_eq!(fake.adt_opened, 3);
    assert_eq!(cell.borrow().calls[0].uri, DELETE_PROG);
    assert!(!deleted.stdout.contains("a/b+c="));
    assert!(!deleted.stderr.contains("a/b+c="));
}

#[test]
fn a_lost_delete_reply_is_reconciled_once_and_the_batch_exits_1() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let plan = plan.to_str().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(delete_facts()));
    assert_eq!(
        go(&delete_args(plan, true), directory.path(), &mut fake).code,
        0
    );

    let mut replies = delete_facts();

    replies.push(Ok(Response::text(200, DELETE_LOCK)));
    replies.push(Err(TransportError::PostSendUncertain(
        "reply lost".to_owned(),
    )));
    fake.adt.push_back(scripted(replies));
    fake.adt
        .push_back(scripted(vec![Ok(Response::text(404, "<exc/>"))]));

    let exit = go(&delete_args(plan, false), directory.path(), &mut fake);

    assert_eq!(exit.code, 1, "{} {}", exit.stdout, exit.stderr);
    assert!(
        exit.stdout.contains("DELETED (reconciled)"),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stdout.contains("check before retrying"),
        "{}",
        exit.stdout
    );
    assert!(exit.stdout.contains("a lock may remain"), "{}", exit.stdout);
    // The DELETE was sent once; the only other session is the read-only look.
    assert_eq!((fake.adt_opened, fake.rfc_opened), (3, 0));
}

#[test]
fn delete_on_a_session_without_state_falls_back_to_rfc_before_anything_is_sent() {
    struct Stateless;

    impl SadtTransport for Stateless {
        fn request(
            &mut self,
            request: abapreach::transport::Request,
        ) -> Result<Response, TransportError> {
            panic!("nothing may be sent: {request:?}");
        }

        fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }

        fn stateful_supported(&self) -> Option<bool> {
            Some(false)
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let plan = plan.to_str().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(Ok(Box::new(Stateless)));
    fake.rfc.push_back(scripted(delete_facts()));

    let exit = go(&delete_args(plan, true), directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{} {}", exit.stdout, exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}
