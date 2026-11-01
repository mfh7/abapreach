mod common;
mod replay;

use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, run};
use common::ScriptedTransport;
use replay::{fx, replay};
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

    fn close(&mut self) {
        self.0.borrow_mut().close();
    }
}

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    adt_opened: usize,
    rfc_opened: usize,
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
        r#""rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
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
fn doctor_reports_offline_with_the_warning_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let exit = go(
        &["abapreach", "doctor", "--conn", "dev_100"],
        directory.path(),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert_eq!(exit.stdout.matches(WARNING).count(), 1);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
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
