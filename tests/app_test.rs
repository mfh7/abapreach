mod common;
mod replay;

use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use common::ScriptedTransport;
use replay::{Step, fx, replay};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;

const WARNING: &str = "WARNING: connection DEV_100 uses insecure TLS or HTTP.";
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

/// The recorded simulate of v2 over v1: live read, lock, locked re-read, check, unlock.
fn simulate_steps() -> Vec<Step> {
    vec![fx(6), fx(7), fx(6), fx(8), fx(9)]
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
            "guards: SAP lock/CSRF/transport/authorization, base hash\n",
            "not applied (FM guards): Z/Y name, productive client, system change option, ",
            "SE06, inactive owned by another user\n",
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
fn a_deploy_pre_send_failure_falls_back_but_a_later_failure_does_not() {
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

    let exit = go(&args, directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert_eq!(fake.rfc_opened, 0);
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
        path: "/opt/sap/nwrfcsdk/lib/libsapnwrfc.so".to_owned(),
        version: "7.6.2".to_owned(),
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
    let (cell, opened) = shared(replay("dev-750-rfc.json", vec![fx(6), fx(7), fx(9)]));

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
