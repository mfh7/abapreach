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
const LIBRARY_PATH: &str = "/opt/sap/nwrfcsdk/lib/libsapnwrfc.so";

fn library() -> Result<RfcLibraryInfo, String> {
    Ok(RfcLibraryInfo {
        identity: RfcLibraryIdentity {
            path: LIBRARY_PATH.to_owned(),
            version: "7.6.2".to_owned(),
            architecture: "x86-64".to_owned(),
            family: "NW RFC SDK".to_owned(),
            source: "discovered".to_owned(),
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
            "local credentials: not present",
            "policy foreign inactive: reject",
            "ADT HTTP configured: https://example.com:443",
            "RFC SADT configured: example.com system 00",
            "ADT HTTP probe: ok (HTTP 200)",
            &format!(
                "RFC library: {LIBRARY_PATH} version 7.6.2 architecture x86-64 family NW RFC SDK \
                 source discovered"
            ),
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
            "ADT HTTP: not configured",
            "RFC SADT configured: example.com system 02",
            "ADT HTTP probe: not run (ADT not configured)",
            &format!(
                "RFC library: {LIBRARY_PATH} version 7.6.2 architecture x86-64 family NW RFC SDK \
                 source discovered"
            ),
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
        "architecture x86-64 family NW RFC SDK source discovered"
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
            .contains("family SAP GUI source explicit ABAPREACH_RFC_LIBRARY"),
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
