mod common;

use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::http::HttpTransport;
use abapreach::transport::rfc::TraceTarget;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, Opened, RfcLibraryIdentity, RfcLibraryInfo, run};
use common::ScriptedTransport;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;

const DISCOVERY_URI: &str = "/sap/bc/adt/discovery";
const WARNING: &str = "WARNING: connection DEV_100 uses insecure TLS or HTTP.";
const DENIED_LINE: &str = "policy read_table_denied: USR02, USH02, USH02_ARC_TMP, USRPWDHISTORY, RFCDES, RSECTAB, RSECACTB, SSF_PSE_D";
const PREFLIGHT_LINE: &str = "RUNNER preflight: T000 CCCATEGORY=C CCCORACTIV=1 CCNOCLIIND=- \
    TRNSPACE /0CUST/ EDITFLAG=X";
const LIBRARY_PATH: &str = "/opt/sap/nwrfcsdk/lib/libsapnwrfc.so";

fn library() -> Result<RfcLibraryInfo, String> {
    Ok(RfcLibraryInfo {
        identity: RfcLibraryIdentity {
            path: LIBRARY_PATH.to_owned(),
            version: "7.6.2".to_owned(),
            architecture: "x86-64".to_owned(),
            family: "NW RFC SDK".to_owned(),
            source: "discovered".to_owned(),
            chars: "UTF-16".to_owned(),
        },
        notes: Vec::new(),
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
    shared(ScriptedTransport::replies(discovery_with(runner_replies(
        classrun_found(),
        search_reply(&[]),
        Vec::new(),
    ))))
    .1
}

/// The discovery reply followed by the replies of the runner reads.
fn discovery_with(
    runner: Vec<Result<Response, TransportError>>,
) -> Vec<Result<Response, TransportError>> {
    let mut replies = vec![Ok(Response::text(200, "<app:service/>"))];

    replies.extend(runner);

    replies
}

/// A SADT session whose runner reads find nothing to report.
fn sadt_runner() -> Opened {
    shared(ScriptedTransport::replies(runner_replies(
        classrun_found(),
        search_reply(&[]),
        Vec::new(),
    )))
    .1
}

fn rows(columns: &[(&str, &[Option<&str>])]) -> Result<Response, TransportError> {
    Ok(Response::text(200, common::preview(columns)))
}

fn classrun_found() -> Result<Response, TransportError> {
    rows(&[("CLSNAME", &[Some("IF_OO_ADT_CLASSRUN")])])
}

fn classrun_missing() -> Result<Response, TransportError> {
    let none: &[Option<&str>] = &[];

    rows(&[("CLSNAME", none)])
}

fn search_reply(names: &[&str]) -> Result<Response, TransportError> {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
    );

    for name in names {
        xml.push_str(&format!(
            r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/oo/classes/{}" adtcore:type="CLAS/OC" adtcore:name="{name}" adtcore:packageName="$TMP"/>"#,
            name.to_ascii_lowercase()
        ));
    }

    xml.push_str("</adtcore:objectReferences>");

    Ok(Response::text(200, xml))
}

/// The metadata of a runner class; `None` leaves out the time stamps.
fn class_meta(created_at: Option<&str>) -> Result<Response, TransportError> {
    let stamp = created_at
        .map(|value| format!(r#" adtcore:createdAt="{value}""#))
        .unwrap_or_default();

    Ok(Response::text(
        200,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><class:abapClass xmlns:class="http://www.sap.com/adt/oo/classes" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="X"{stamp}/>"#
        ),
    ))
}

fn preflight_replies(editflag: &str) -> Vec<Result<Response, TransportError>> {
    vec![
        rows(&[
            ("CCCATEGORY", &[Some("C")]),
            ("CCCORACTIV", &[Some("1")]),
            ("CCNOCLIIND", &[None]),
        ]),
        rows(&[("EDITFLAG", &[Some(editflag)])]),
    ]
}

/// The replies of the runner reads after the discovery: the interface preview, the leftover
/// search, one metadata read per listed class, and the two preflight previews.
fn runner_replies(
    classrun: Result<Response, TransportError>,
    search: Result<Response, TransportError>,
    metadata: Vec<Result<Response, TransportError>>,
) -> Vec<Result<Response, TransportError>> {
    let mut replies = vec![classrun, search];

    replies.extend(metadata);
    replies.extend(preflight_replies("X"));

    replies
}

/// An ISO time stamp this many seconds before now, in UTC.
fn seconds_ago(seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - seconds;
    let (days, rest) = ((now / 86_400) as i64, now % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

fn adt_only_env(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""adt":{"insecure":true}}]}"#,
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

/// Doctor opens a SADT session only when no ADT session answered (the runner facts need one
/// transport); a ping is the only other RFC logon it may make.
#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    sadt: VecDeque<Opened>,
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
        self.sadt.pop_front().expect("unexpected SADT RFC session")
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
    let (cell, opened) = shared(ScriptedTransport::replies(discovery_with(runner_replies(
        classrun_found(),
        search_reply(&[]),
        Vec::new(),
    ))));

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
            "local credentials: not present",
            "policy foreign inactive: reject",
            DENIED_LINE,
            "policy mcp_write: off",
            "ADT HTTP configured: https://***:443",
            "RFC SADT configured: *** system 00",
            "ADT HTTP probe: ok (HTTP 200)",
            &format!(
                "RFC library: {LIBRARY_PATH} version 7.6.2 architecture x86-64 family NW RFC SDK \
                 source discovered chars UTF-16"
            ),
            "RFC SADT probe: ok",
            "RUNNER classrun: available",
            "RUNNER prefix: YCL_ABAPREACH_RUN_",
            "RUNNER post transport: http",
            "RUNNER leftovers: none",
            PREFLIGHT_LINE,
            "RUNNER authorization: not probed",
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

    // The discovery, then the interface preview, the leftover search and two preflight previews
    // on the same session.
    assert_eq!(transport.calls.len(), 5);
    assert_eq!(transport.calls[0].method, "GET");
    assert_eq!(transport.calls[0].uri, DISCOVERY_URI);
    assert_eq!(transport.closes, 1);
    transport.assert_finished();
}

#[test]
fn doctor_reports_why_a_discovered_library_was_skipped() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let reason = concat!(
        "RFC SAP GUI 32-bit: skipped C:\\Program Files (x86)\\SAP\\FrontEnd\\SAPGUI\\",
        "sapnwrfccm.dll (a 32-bit library cannot load into the 64-bit process)",
    );

    fake.adt.push_back(discovery());
    fake.library.push_back(Ok(RfcLibraryInfo {
        identity: RfcLibraryIdentity {
            path: LIBRARY_PATH.to_owned(),
            version: "7.6.2".to_owned(),
            architecture: "x86-64".to_owned(),
            family: "NW RFC SDK".to_owned(),
            source: "discovered".to_owned(),
            chars: "UTF-16".to_owned(),
        },
        notes: vec![reason.to_owned()],
    }));
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 0);
    assert!(
        exit.stdout.contains(reason),
        "missing reason in: {}",
        exit.stdout
    );
}

#[test]
fn an_rfc_only_connection_skips_the_adt_probe_and_reports_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));
    fake.sadt.push_back(
        shared(ScriptedTransport::replies(runner_replies(
            classrun_found(),
            search_reply(&[]),
            Vec::new(),
        )))
        .1,
    );

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
            "local credentials: not present",
            "policy foreign inactive: reject",
            DENIED_LINE,
            "policy mcp_write: off",
            "ADT HTTP: not configured",
            "RFC SADT configured: *** system 02",
            "ADT HTTP probe: not run (ADT not configured)",
            &format!(
                "RFC library: {LIBRARY_PATH} version 7.6.2 architecture x86-64 family NW RFC SDK \
                 source discovered chars UTF-16"
            ),
            "RFC SADT probe: ok",
            "RUNNER classrun: available",
            "RUNNER prefix: YCL_ABAPREACH_RUN_",
            "RUNNER post transport: sadt",
            "RUNNER leftovers: none",
            PREFLIGHT_LINE,
            "RUNNER authorization: not probed",
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
    fake.sadt.push_back(sadt_runner());

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
    assert_capability_lines(
        &exit,
        [
            "capability read PROG: RFC SADT",
            "capability deploy PROG: RFC SADT",
            "capability doctor: RFC SADT",
        ],
    );
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (1, 1, 1)
    );
}

#[test]
fn a_failed_rfc_ping_leaves_every_capability_line_with_adt() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    )));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stdout
            .contains("RFC SADT probe: failed: pre-send connectivity failure: no route"),
        "{}",
        exit.stdout
    );
    assert_capability_lines(
        &exit,
        [
            "capability read PROG: ADT HTTP",
            "capability deploy PROG: ADT HTTP",
            "capability doctor: ADT HTTP",
        ],
    );
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (1, 1, 1)
    );
}

#[test]
fn a_missing_rfc_library_leaves_every_capability_line_with_adt() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let mut kept = Response::text(200, "<app:service/>");
    kept.headers
        .insert("Set-Cookie".to_owned(), "sap-contextid=ctx".to_owned());

    fake.adt.push_back(Ok(Box::new(HttpTransport::for_test(vec![
        Ok(kept),
        Ok(Response::empty(200)),
    ]))));
    fake.library.push_back(Err(
        "RFC library not found; set ABAPREACH_RFC_LIBRARY".to_owned()
    ));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stdout
            .contains("RFC SADT probe: not run (RFC library not found)"),
        "{}",
        exit.stdout
    );
    assert_capability_lines(
        &exit,
        [
            "capability read PROG: ADT HTTP",
            "capability deploy PROG: ADT HTTP",
            "capability doctor: ADT HTTP",
        ],
    );
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (1, 1, 0)
    );
}

#[test]
fn both_probes_failing_leaves_no_capability_backend() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    )));
    fake.library.push_back(Err(
        "RFC library not found; set ABAPREACH_RFC_LIBRARY".to_owned()
    ));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 1);
    assert_capability_lines(
        &exit,
        [
            "capability read PROG: none",
            "capability deploy PROG: none",
            "capability doctor: none",
        ],
    );
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (1, 1, 0)
    );
}

fn assert_capability_lines(exit: &Exit, expected: [&str; 3]) {
    let lines: Vec<&str> = exit.stdout.lines().collect();

    for line in expected {
        assert!(lines.contains(&line), "missing {line:?} in {}", exit.stdout);
    }
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

/// A stateless ADT session with an RFC route runs the runner commands over RFC, so the POST
/// travels over SADT; with a stateful session it travels over HTTP.
#[test]
fn doctor_reports_the_transport_a_runner_would_really_use() {
    let mut kept = Response::text(200, "<app:service/>");
    kept.headers
        .insert("Set-Cookie".to_owned(), "sap-contextid=ctx".to_owned());

    for (discovery, expected) in [
        (kept, "RUNNER post transport: http"),
        (
            Response::text(200, "<app:service/>"),
            "RUNNER post transport: sadt",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut fake = Fake::default();

        fake.adt.push_back(Ok(Box::new(HttpTransport::for_test(vec![
            Ok(discovery),
            Ok(Response::empty(200)),
        ]))));
        fake.library.push_back(library());
        fake.ping.push_back(Ok(()));

        let exit = doctor(directory.path(), &mut fake);

        assert_eq!(runner_line(&exit, "RUNNER post transport"), expected);
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

/// A route with a route password lives in the environment, never in the shared config file.
#[test]
fn doctor_names_the_route_source_and_never_prints_the_route() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let route = "/H/router.example.com/S/3299/W/route-secret";

    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let mut env = env(directory.path());
    env.insert("DEV_SAPROUTER".to_owned(), route.to_owned());

    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env,
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(
        exit.stdout
            .lines()
            .any(|line| line == "SAProuter: configured (from environment)"),
        "{}",
        exit.stdout
    );
    assert!(
        !exit.stdout.contains("router.example.com"),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("route-secret"), "{}", exit.stdout);
}

#[test]
fn doctor_without_a_configured_route_prints_no_saprouter_line() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    assert!(!exit.stdout.contains("SAProuter"), "{}", exit.stdout);
}

/// The local file holds the credentials here, so the doctor output proves the command read it and
/// still never printed the secret.
#[test]
fn doctor_never_prints_the_local_credentials_secret() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fs::write(
        directory.path().join("connections.local.json"),
        r#"{"DEV_100_USER":"local-user","DEV_100_PASS":"local-secret-pw"}"#,
    )
    .unwrap();
    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let mut env = env(directory.path());
    env.remove("DEV_100_USER");
    env.remove("DEV_100_PASS");

    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env,
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert!(
        exit.stdout.contains("credential available"),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("local-secret-pw"), "{}", exit.stdout);
}

/// A file that only the owner may touch needs no warning, and doctor exits 0 either way.
#[cfg(unix)]
#[test]
fn doctor_reports_a_user_only_local_credentials_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let still_local = directory.path().join("connections.local.json");
    let mut fake = Fake::default();

    fs::write(&still_local, r#"{"DEV_100_USER":"local-user"}"#).unwrap();
    fs::set_permissions(&still_local, fs::Permissions::from_mode(0o600)).unwrap();
    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert!(
        exit.stdout
            .lines()
            .any(|line| line == "local credentials: user-only permission"),
        "{}",
        exit.stdout
    );
    assert!(
        !exit.stdout.contains("readable by group"),
        "{}",
        exit.stdout
    );
}

#[cfg(unix)]
#[test]
fn doctor_warns_when_the_local_credentials_are_readable_by_group_or_other() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let exposed = directory.path().join("connections.local.json");
    let mut fake = Fake::default();

    fs::write(
        &exposed,
        r#"{"DEV_100_USER":"local-user","DEV_100_PASS":"local-secret-pw"}"#,
    )
    .unwrap();
    fs::set_permissions(&exposed, fs::Permissions::from_mode(0o644)).unwrap();
    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert!(
        exit.stdout.lines().any(|line| line
            .starts_with("WARNING: local credentials are readable by group or other (mode 0644)")
            && line.contains("docs/getting-started.md")),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("local-secret-pw"), "{}", exit.stdout);
}

/// Windows reports the mode bits as unavailable and sends the user to the documented ACL
/// requirement instead of claiming a check it cannot make.
#[cfg(windows)]
#[test]
fn doctor_reports_that_windows_permission_was_not_checked() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fs::write(
        directory.path().join("connections.local.json"),
        r#"{"DEV_100_USER":"local-user"}"#,
    )
    .unwrap();
    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert!(
        exit.stdout.lines().any(|line| line
            == "local credentials: permission not checked; restrict the file to a user-only ACL \
                (see docs/getting-started.md)"),
        "{}",
        exit.stdout
    );
}

// --- Task 3b: doctor library identity ---

/// The identity the doctor prints: path, version, architecture, family, and source.
fn sdk_identity() -> RfcLibraryIdentity {
    RfcLibraryIdentity {
        path: LIBRARY_PATH.to_owned(),
        version: "7.53.1629".to_owned(),
        architecture: "x86-64".to_owned(),
        family: "NW RFC SDK".to_owned(),
        source: "discovered".to_owned(),
        chars: "UTF-16".to_owned(),
    }
}

fn identified(path: &str, family: &str, source: &str) -> Result<RfcLibraryInfo, String> {
    Ok(RfcLibraryInfo {
        identity: RfcLibraryIdentity {
            path: path.to_owned(),
            version: "7.6.2".to_owned(),
            architecture: "x86-64".to_owned(),
            family: family.to_owned(),
            source: source.to_owned(),
            chars: "UTF-16".to_owned(),
        },
        notes: Vec::new(),
    })
}

#[test]
fn doctor_library_line_names_path_version_architecture_family_and_source() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(discovery());
    fake.library.push_back(Ok(RfcLibraryInfo {
        identity: sdk_identity(),
        notes: Vec::new(),
    }));
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);
    let expected = concat!(
        "RFC library: /opt/sap/nwrfcsdk/lib/libsapnwrfc.so version 7.53.1629 ",
        "architecture x86-64 family NW RFC SDK source discovered chars UTF-16"
    );

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert!(
        exit.stdout.lines().any(|line| line == expected),
        "missing {expected:?} in {}",
        exit.stdout
    );
}

#[test]
fn doctor_prints_the_trace_directory_line_right_after_the_library_line() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let label = TraceTarget::Rejected(20).label();

    fake.adt.push_back(discovery());
    fake.library.push_back(Ok(RfcLibraryInfo {
        identity: sdk_identity(),
        notes: vec![label.clone()],
    }));
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);
    let lines: Vec<&str> = exit.stdout.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with("RFC library: "))
        .unwrap();

    assert_eq!(lines[at + 1], label, "{}", exit.stdout);
}

#[test]
fn doctor_reports_a_sap_gui_family_library_named_by_the_override() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let path = "/opt/sapgui/bin/libsapnwrfccm.so";

    fake.library.push_back(identified(
        path,
        "SAP GUI",
        "explicit ABAPREACH_RFC_LIBRARY",
    ));
    fake.ping.push_back(Ok(()));
    fake.sadt.push_back(sadt_runner());

    let exit = run(
        ["abapreach", "doctor", "--conn", "qas_100"]
            .map(str::to_owned)
            .to_vec(),
        rfc_only_env(directory.path()),
        &mut fake,
    );

    // Without an ADT configuration the ping needs no ADT transport, so only the library line is
    // asserted here; the family must name the library that was actually loaded.
    assert!(
        exit.stdout
            .contains("family SAP GUI source explicit ABAPREACH_RFC_LIBRARY chars UTF-16"),
        "{}",
        exit.stdout
    );
    assert!(exit.stdout.contains(path), "{}", exit.stdout);
    assert!(
        !exit.stdout.contains("family NW RFC SDK"),
        "{}",
        exit.stdout
    );
}

/// A connection whose ADT host and application server host are both unique strings.
fn env_with_two_hosts(directory: &Path) -> BTreeMap<String, String> {
    let config = r#"{"servers":[{"systemId":"DEV","host":"adt-host.example.net","port":8443,"clients":["100"],"rfc":{"ashost":"10.20.30.40","sysnr":"00"},"adt":{}}]}"#;

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

fn assert_no_host(exit: &Exit) {
    for text in [&exit.stdout, &exit.stderr] {
        assert!(!text.contains("10.20.30.40"), "{text}");
        assert!(!text.contains("adt-host.example.net"), "{text}");
    }
}

#[test]
fn doctor_masks_the_application_server_host_and_the_adt_host_in_the_configured_lines() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(discovery());
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env_with_two_hosts(directory.path()),
        &mut fake,
    );
    let lines = exit.stdout.lines().collect::<Vec<_>>();

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert!(
        lines.contains(&"ADT HTTP configured: https://***:8443"),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"RFC SADT configured: *** system 00"),
        "{lines:?}"
    );
    assert_no_host(&exit);
}

#[test]
fn doctor_masks_both_hosts_in_the_failed_probe_lines() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(Err(TransportError::PreSendConnectivity(
        "connect to adt-host.example.net:8443 (10.20.30.40) refused".to_owned(),
    )));
    fake.library.push_back(library());
    fake.ping.push_back(Err(TransportError::PreSendConnectivity(
        "RfcOpenConnection: partner 10.20.30.40:3300 not reached (adt-host.example.net)".to_owned(),
    )));

    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env_with_two_hosts(directory.path()),
        &mut fake,
    );

    assert_eq!(exit.code, 1, "{}", exit.stdout);
    assert!(
        exit.stdout
            .lines()
            .any(|line| line.starts_with("ADT HTTP probe: failed: ") && line.contains("***")),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stdout
            .lines()
            .any(|line| line.starts_with("RFC SADT probe: failed: ") && line.contains("***")),
        "{}",
        exit.stdout
    );
    assert_no_host(&exit);
}

/// A one-server config with `policies`, doctor run on it with the probes answered.
fn doctor_with_policies(policies: &str) -> Exit {
    let directory = tempfile::tempdir().unwrap();
    let config = format!(
        concat!(
            r#"{{"policies":{policies},"servers":[{{"systemId":"QAS","clients":["100"],"#,
            r#""rfc":{{"ashost":"example.com","sysnr":"02"}}}}]}}"#,
        ),
        policies = policies
    );
    fs::write(directory.path().join("connections.json"), config).unwrap();
    let env = BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory
                .path()
                .join("connections.json")
                .display()
                .to_string(),
        ),
        ("QAS_100_USER".to_owned(), "user".to_owned()),
        ("QAS_100_PASS".to_owned(), "secret-pw".to_owned()),
    ]);
    let mut fake = Fake::default();

    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));
    fake.sadt.push_back(sadt_runner());

    run(
        ["abapreach", "doctor", "--conn", "qas_100"]
            .map(str::to_owned)
            .to_vec(),
        env,
        &mut fake,
    )
}

#[test]
fn doctor_prints_the_effective_policy_lines() {
    let exit =
        doctor_with_policies(r#"{"read_table_denied":["usr02","Custom_T"],"mcp_write":"token"}"#);
    let lines = exit.stdout.lines().collect::<Vec<_>>();

    assert!(
        lines.contains(&"policy read_table_denied: USR02, CUSTOM_T"),
        "{}",
        exit.stdout
    );
    assert!(
        lines.contains(&"policy mcp_write: token"),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("secret-pw"));
}

#[test]
fn doctor_warns_about_the_default_entries_a_deny_list_lacks() {
    let exit = doctor_with_policies(r#"{"read_table_denied":["usr02","rfcdes"]}"#);

    assert!(
        exit.stdout.lines().any(|line| line
            == "WARNING: policy read_table_denied lacks default entries: USH02, USH02_ARC_TMP, USRPWDHISTORY, RSECTAB, RSECACTB, SSF_PSE_D"),
        "{}",
        exit.stdout
    );
}

#[test]
fn doctor_warns_for_an_empty_deny_list_and_not_for_the_default() {
    let empty = doctor_with_policies(r#"{"read_table_denied":[]}"#);

    assert!(
        empty.stdout.contains("policy read_table_denied: (none)"),
        "{}",
        empty.stdout
    );
    assert!(
        empty
            .stdout
            .contains("WARNING: policy read_table_denied lacks default entries: USR02,")
    );
    assert!(
        !doctor_with_policies("{}")
            .stdout
            .contains("lacks default entries")
    );
    // Case does not matter, and extra entries do not trigger the warning.
    assert!(
        !doctor_with_policies(
            r#"{"read_table_denied":["usr02","ush02","ush02_arc_tmp","usrpwdhistory","rfcdes","rsectab","rsecactb","ssf_pse_d","extra"]}"#
        )
        .stdout
        .contains("lacks default entries")
    );
}

#[test]
fn an_adt_only_connection_reports_ext_as_not_applicable() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt.push_back(discovery());

    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        adt_only_env(directory.path()),
        &mut fake,
    );

    assert_eq!(exit.code, 0);
    assert!(
        exit.stdout.contains("RFC library: not run (no RFC)"),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("EXT"), "{}", exit.stdout);
    assert_eq!(
        (fake.adt_calls, fake.library_calls, fake.ping_calls),
        (1, 0, 0)
    );
}

const RUN_A: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const RUN_B: &str = "YCL_ABAPREACH_RUN_ZZ000000";
const SEARCH_URI: &str = "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch\
                          &query=YCL_ABAPREACH_RUN_*&maxResults=20&objectType=CLAS%2FOC";

/// A doctor run on the dual connection whose ADT session follows `runner`; returns the exit and
/// the session for call assertions.
fn adt_doctor(
    runner: Vec<Result<Response, TransportError>>,
) -> (Exit, Rc<RefCell<ScriptedTransport>>) {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let (cell, opened) = shared(ScriptedTransport::replies(discovery_with(runner)));

    fake.adt.push_back(opened);
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));

    let exit = doctor(directory.path(), &mut fake);

    (exit, cell)
}

fn runner_line<'a>(exit: &'a Exit, label: &str) -> &'a str {
    exit.stdout
        .lines()
        .find(|line| line.starts_with(label))
        .unwrap_or_else(|| panic!("no {label} line in {}", exit.stdout))
}

#[test]
fn doctor_lists_leftovers_with_their_age_and_marks_the_stale_one() {
    let (exit, cell) = adt_doctor(runner_replies(
        classrun_found(),
        search_reply(&[RUN_A, RUN_B]),
        vec![
            class_meta(Some(&seconds_ago(120))),
            class_meta(Some(&seconds_ago(3 * 3_600))),
        ],
    ));

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert_eq!(
        runner_line(&exit, "RUNNER leftovers"),
        format!(
            "RUNNER leftovers: 2 ({RUN_A} 2m, {RUN_B} 3h stale; remove with abapreach delete \
             --simulate CLAS:<name> --plan <FILE> --conn DEV_100, then without --simulate)"
        )
    );

    let transport = cell.borrow();

    assert_eq!(transport.calls[2].method, "GET");
    assert_eq!(transport.calls[2].uri, SEARCH_URI);
    assert_eq!(
        transport.calls[3].uri,
        format!("/sap/bc/adt/oo/classes/{}", RUN_A.to_ascii_lowercase())
    );
    assert_eq!(
        transport.calls[4].uri,
        format!("/sap/bc/adt/oo/classes/{}", RUN_B.to_ascii_lowercase())
    );
    transport.assert_finished();
}

#[test]
fn a_class_without_a_time_stamp_has_an_unknown_age_and_no_stale_mark() {
    let (exit, _) = adt_doctor(runner_replies(
        classrun_found(),
        search_reply(&[RUN_A]),
        vec![class_meta(None)],
    ));
    let line = runner_line(&exit, "RUNNER leftovers");

    assert!(
        line.starts_with(&format!("RUNNER leftovers: 1 ({RUN_A} age unknown;")),
        "{line}"
    );
    assert!(!line.contains("stale"), "{line}");
}

#[test]
fn a_search_that_fills_its_limit_prints_a_truncated_count() {
    let names: Vec<String> = (0..20)
        .map(|index| format!("YCL_ABAPREACH_RUN_AB12CD{index:02}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let metadata = (0..20)
        .map(|_| class_meta(Some(&seconds_ago(30))))
        .collect();
    let (exit, _) = adt_doctor(runner_replies(
        classrun_found(),
        search_reply(&refs),
        metadata,
    ));
    let line = runner_line(&exit, "RUNNER leftovers");

    assert!(line.starts_with("RUNNER leftovers: 20+ ("), "{line}");
    assert!(line.contains("YCL_ABAPREACH_RUN_AB12CD19 0m"), "{line}");
}

#[test]
fn an_unavailable_classrun_handler_is_reported_with_its_reason() {
    let (exit, _) = adt_doctor(runner_replies(
        classrun_missing(),
        search_reply(&[]),
        Vec::new(),
    ));

    assert_eq!(exit.code, 0);
    assert_eq!(
        runner_line(&exit, "RUNNER classrun"),
        "RUNNER classrun: unavailable (interface IF_OO_ADT_CLASSRUN not found)"
    );
}

#[test]
fn an_empty_discovery_document_still_detects_the_classrun_interface_by_preview() {
    // A 7.40 system answers the discovery with an empty service document, so the interface is
    // read from the repository table instead.
    let (exit, cell) = adt_doctor(runner_replies(
        classrun_found(),
        search_reply(&[]),
        Vec::new(),
    ));

    assert_eq!(
        runner_line(&exit, "RUNNER classrun"),
        "RUNNER classrun: available"
    );

    let transport = cell.borrow();
    let request = &transport.calls[1];

    assert_eq!(request.method, "POST");
    assert!(
        request.uri.contains("ddicEntityName=SEOCLASS"),
        "{}",
        request.uri
    );
    assert_eq!(
        String::from_utf8_lossy(&request.body),
        "SELECT CLSNAME FROM SEOCLASS WHERE CLSNAME = 'IF_OO_ADT_CLASSRUN'"
    );
}

#[test]
fn a_preview_that_cannot_answer_leaves_the_classrun_state_unknown() {
    let (exit, _) = adt_doctor(runner_replies(
        Ok(Response::text(500, "")),
        search_reply(&[]),
        Vec::new(),
    ));

    assert_eq!(exit.code, 0);
    assert_eq!(
        runner_line(&exit, "RUNNER classrun"),
        "RUNNER classrun: unknown (not detectable: SEOCLASS: HTTP 500)"
    );
}

#[test]
fn a_failed_leftover_search_is_unknown_and_does_not_fail_the_doctor() {
    let (exit, _) = adt_doctor(runner_replies(
        classrun_found(),
        Ok(Response::text(500, "")),
        Vec::new(),
    ));

    assert_eq!(exit.code, 0);
    assert_eq!(
        runner_line(&exit, "RUNNER leftovers"),
        "RUNNER leftovers: unknown (search failed: HTTP 500)"
    );
}

#[test]
fn a_refusing_preflight_is_printed_with_its_reason() {
    let mut replies = runner_replies(classrun_found(), search_reply(&[]), Vec::new());

    replies.truncate(2);
    replies.extend(preflight_replies(""));

    let (exit, _) = adt_doctor(replies);

    assert_eq!(exit.code, 0);
    assert_eq!(
        runner_line(&exit, "RUNNER preflight"),
        "RUNNER preflight: T000 CCCATEGORY=C CCCORACTIV=1 CCNOCLIIND=- TRNSPACE /0CUST/ \
         EDITFLAG=-; repository changes are not allowed (customer namespace /0CUST/ is not \
         modifiable; TRNSPACE EDITFLAG -)"
    );
}

#[test]
fn doctor_reads_the_runner_facts_over_sadt_when_no_adt_session_answered() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let (cell, opened) = shared(ScriptedTransport::replies(runner_replies(
        classrun_found(),
        search_reply(&[RUN_A]),
        vec![class_meta(Some(&seconds_ago(900)))],
    )));

    fake.adt
        .push_back(Ok(Box::new(ScriptedTransport::replies(vec![Ok(
            Response::text(503, ""),
        )]))));
    fake.library.push_back(library());
    fake.ping.push_back(Ok(()));
    fake.sadt.push_back(opened);

    let exit = doctor(directory.path(), &mut fake);

    assert_eq!(exit.code, 1, "{}", exit.stdout);
    assert_eq!(fake.sadt_calls, 1);
    assert_eq!(
        runner_line(&exit, "RUNNER post transport"),
        "RUNNER post transport: http"
    );
    assert!(
        runner_line(&exit, "RUNNER leftovers").contains(&format!("{RUN_A} 15m stale")),
        "{}",
        exit.stdout
    );
    assert_eq!(cell.borrow().closes, 1);
    cell.borrow().assert_finished();
}

#[test]
fn unreachable_backends_leave_the_runner_facts_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    fake.adt
        .push_back(Ok(Box::new(ScriptedTransport::replies(vec![Ok(
            Response::text(503, ""),
        )]))));
    fake.library.push_back(Err("not found".to_owned()));

    let exit = doctor(directory.path(), &mut fake);
    let reason = "no backend answered";

    assert_eq!(exit.code, 1);
    assert_eq!(fake.sadt_calls, 0);
    assert_eq!(
        runner_line(&exit, "RUNNER classrun"),
        format!("RUNNER classrun: unknown ({reason})")
    );
    assert_eq!(
        runner_line(&exit, "RUNNER leftovers"),
        format!("RUNNER leftovers: unknown ({reason})")
    );
    assert_eq!(
        runner_line(&exit, "RUNNER preflight"),
        format!("RUNNER preflight: unknown ({reason})")
    );
    assert_eq!(
        runner_line(&exit, "RUNNER authorization"),
        "RUNNER authorization: not probed"
    );
}

#[test]
fn the_runner_lines_sit_between_the_sadt_probe_and_the_capability_lines() {
    let (exit, _) = adt_doctor(runner_replies(
        classrun_found(),
        search_reply(&[]),
        Vec::new(),
    ));
    let lines: Vec<&str> = exit.stdout.lines().collect();
    let sadt = lines
        .iter()
        .position(|line| line.starts_with("RFC SADT probe"))
        .unwrap();
    let capability = lines
        .iter()
        .position(|line| line.starts_with("capability"))
        .unwrap();
    let runner: Vec<usize> = (0..lines.len())
        .filter(|&at| lines[at].starts_with("RUNNER "))
        .collect();

    assert_eq!(
        runner,
        (sadt + 1..capability).collect::<Vec<_>>(),
        "{}",
        exit.stdout
    );
    assert_eq!(runner.len(), 6);
}

#[test]
fn the_runner_probes_are_read_only_and_leak_neither_host_nor_secret() {
    let (exit, cell) = adt_doctor(runner_replies(
        classrun_found(),
        search_reply(&[RUN_A]),
        vec![class_meta(Some(&seconds_ago(60)))],
    ));

    for secret in ["example.com", "secret-pw"] {
        assert!(!exit.stdout.contains(secret), "{}", exit.stdout);
        assert!(!exit.stderr.contains(secret), "{}", exit.stderr);
    }

    let transport = cell.borrow();

    for call in &transport.calls {
        assert!(
            call.method == "GET"
                || (call.method == "POST" && call.uri.contains("datapreview/ddic")),
            "{} {}",
            call.method,
            call.uri
        );
    }

    // Discovery, interface preview, search, one metadata read, two preflight previews.
    assert_eq!(transport.calls.len(), 6);
}
