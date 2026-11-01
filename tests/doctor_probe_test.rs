mod common;

use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::http::HttpTransport;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, Opened, RfcLibraryInfo, run};
use common::ScriptedTransport;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;

const DISCOVERY_URI: &str = "/sap/bc/adt/discovery";
const WARNING: &str = "WARNING: connection DEV_100 uses insecure TLS or HTTP.";
const LIBRARY_PATH: &str = "/opt/sap/nwrfcsdk/lib/libsapnwrfc.so";

fn library() -> Result<RfcLibraryInfo, String> {
    Ok(RfcLibraryInfo {
        path: LIBRARY_PATH.to_owned(),
        version: "7.6.2".to_owned(),
    })
}

struct Shared<T>(Rc<RefCell<T>>);

impl<T: SadtTransport> SadtTransport for Shared<T> {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.0.borrow_mut().request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.0.borrow_mut().close()
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.0.borrow().stateful_supported()
    }
}

fn shared<T: SadtTransport + 'static>(transport: T) -> (Rc<RefCell<T>>, Opened) {
    let cell = Rc::new(RefCell::new(transport));

    (cell.clone(), Ok(Box::new(Shared(cell))))
}

fn discovery() -> Opened {
    shared(ScriptedTransport::replies(vec![Ok(Response::text(
        200,
        "<app:service/>",
    ))]))
    .1
}

/// Doctor must never open the SADT session; a ping is the only RFC logon it may make.
#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    library: VecDeque<Result<RfcLibraryInfo, String>>,
    ping: VecDeque<Result<(), TransportError>>,
    adt_calls: usize,
    library_calls: usize,
    ping_calls: usize,
    sadt_calls: usize,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_calls += 1;
        self.adt.pop_front().expect("unexpected ADT connect")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.sadt_calls += 1;

        panic!("doctor must not open an SADT RFC session")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        self.library_calls += 1;
        self.library
            .pop_front()
            .expect("unexpected RFC library probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        self.ping_calls += 1;
        self.ping.pop_front().expect("unexpected RFC ping")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
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

fn rfc_only_env(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"QAS","clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"02"}}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("QAS_100_USER".to_owned(), "user".to_owned()),
        ("QAS_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn doctor(directory: &Path, fake: &mut Fake) -> Exit {
    run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env(directory),
        fake,
    )
}

#[test]
fn doctor_probes_both_backends_with_one_logon_each() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let (cell, opened) = shared(ScriptedTransport::replies(vec![Ok(Response::text(
        200,
        "<app:service/>",
    ))]));

    fake.adt.push_back(opened);
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert_eq!(exit.stderr, "");
    assert_eq!(
        exit.stdout,
        [
            "connection DEV_100: configuration available",
            "credential available",
            "policy foreign inactive: reject",
            "ADT HTTP configured: https://example.com:443",
            "RFC SADT configured: example.com system 00",
            "ADT HTTP probe: ok (HTTP 200)",
            &format!("RFC library: {LIBRARY_PATH} version 7.6.2"),
            "RFC SADT probe: ok",
            "capability read PROG: ADT HTTP, RFC SADT",
            "capability deploy PROG: ADT HTTP, RFC SADT",
            "capability doctor: ADT HTTP, RFC SADT",
            WARNING,
        ]
        .join("\n")
    );
    assert!(!exit.stdout.contains("secret-pw"));
    assert_eq!(
        (
            fake.adt_calls,
            fake.library_calls,
            fake.ping_calls,
            fake.sadt_calls
        ),
        (1, 1, 1, 0)
    );

    let transport = cell.borrow();

    assert_eq!(transport.calls.len(), 1);
    assert_eq!(transport.calls[0].method, "GET");
    assert_eq!(transport.calls[0].uri, DISCOVERY_URI);
    assert_eq!(transport.closes, 1);
    transport.assert_finished();
}

#[test]
fn an_rfc_only_connection_skips_the_adt_probe_and_reports_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = run(
        ["abapreach", "doctor", "--conn", "qas_100"]
            .map(str::to_owned)
            .to_vec(),
        rfc_only_env(directory.path()),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert_eq!(exit.stderr, "");
    assert_eq!(
        exit.stdout,
        [
            "connection QAS_100: configuration available",
            "credential available",
            "policy foreign inactive: reject",
            "ADT HTTP: not configured",
            "RFC SADT configured: example.com system 02",
            "ADT HTTP probe: not run (ADT not configured)",
            &format!("RFC library: {LIBRARY_PATH} version 7.6.2"),
            "RFC SADT probe: ok",
            "capability read PROG: RFC SADT",
            "capability deploy PROG: RFC SADT",
            "capability doctor: RFC SADT",
        ]
        .join("\n")
    );
    assert!(!exit.stdout.contains("WARNING"));
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (0, 1, 1)
    );
}

#[test]
fn a_failed_adt_probe_does_not_stop_the_rfc_probes() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    )));
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stdout
            .contains("ADT HTTP probe: failed: pre-send connectivity failure: no route"),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stdout.contains("RFC SADT probe: ok"),
        "{}",
        exit.stdout
    );
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (1, 1, 1)
    );
}

#[test]
fn doctor_reports_the_stateful_session_and_leaves_the_deploy_capability_with_rfc_when_absent() {
    let mut kept = Response::text(200, "<app:service/>");
    kept.headers
        .insert("Set-Cookie".to_owned(), "sap-contextid=ctx".to_owned());

    let cases = [
        (
            kept,
            "ADT HTTP stateful session: ok",
            "capability deploy PROG: ADT HTTP, RFC SADT",
        ),
        (
            Response::text(200, "<app:service/>"),
            "ADT HTTP stateful session: unavailable (no sap-contextid)",
            "capability deploy PROG: RFC SADT",
        ),
    ];

    for (discovery, stateful, deploy) in cases {
        let directory = tempfile::tempdir().unwrap();
        let mut fake = Fake::default();

        fake.adt.push_back(Ok(Box::new(HttpTransport::for_test(vec![
            Ok(discovery),
            Ok(Response::empty(200)),
        ]))));
        fake.library.push_back(library());
        fake.ping.push_back(Ok(()));

        let exit = doctor(directory.path(), &mut fake);

        assert_eq!(exit.code, 0, "{stateful}");
        assert!(
            exit.stdout.contains("ADT HTTP probe: ok (HTTP 200)"),
            "{}",
            exit.stdout
        );
        assert!(exit.stdout.contains(stateful), "{}", exit.stdout);
        assert!(exit.stdout.contains(deploy), "{}", exit.stdout);
        assert!(
            exit.stdout
                .contains("capability read PROG: ADT HTTP, RFC SADT"),
            "{}",
            exit.stdout
        );
        assert_eq!(
            (fake.adt_calls, fake.library_calls, fake.ping_calls),
            (1, 1, 1),
            "{stateful}"
        );
    }
}

#[test]
fn a_missing_rfc_library_skips_the_ping_and_fails_the_doctor() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    fake.adt.push_back(discovery());
    fake.library.push_back(Err(
        "RFC library not found; set ABAPREACH_RFC_LIBRARY".to_owned()
    ));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stdout
            .contains("RFC library: not found: RFC library not found; set ABAPREACH_RFC_LIBRARY"),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stdout
            .contains("RFC SADT probe: not run (RFC library not found)"),
        "{}",
        exit.stdout
    );
    assert!(exit.stdout.contains("ADT HTTP probe: ok (HTTP 200)"));
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (1, 1, 0)
    );
}
