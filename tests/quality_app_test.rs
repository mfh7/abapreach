//! `inactive`, `activate` and `aunit` through the application: argument parsing, the policy `run`,
//! the local stops before any connection, and the routed outcome. A scripted ADT answers.

mod common;

use abapreach::config::Connection;
use abapreach::transport::{Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use common::{ScriptedTransport, preview};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;

const INACTIVE: &str = include_str!("fixtures/m7/inactive-objects-assumed.xml");
const CLEAN: &str = include_str!("fixtures/m7/aunit-result-clean-assumed.xml");
const FAILED: &str = include_str!("fixtures/m7/aunit-result-failed-assumed.xml");

type Opened = Result<Box<dyn SadtTransport>, TransportError>;
type Reply = Result<Response, TransportError>;

struct Shared(Rc<RefCell<ScriptedTransport>>);

impl SadtTransport for Shared {
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

/// A session whose first reply sets no context cookie: the stateless ADT HTTP of a 7.40 system.
struct Stateless(Rc<RefCell<ScriptedTransport>>);

impl SadtTransport for Stateless {
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
        Some(false)
    }
}

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    adt_opened: usize,
    rfc_opened: usize,
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
        unreachable!("no library probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        unreachable!("no ping")
    }
}

fn env(directory: &Path, policies: &str) -> BTreeMap<String, String> {
    let config = format!(
        r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"rfc":{{"ashost":"example.com","sysnr":"00"}},"policies":{policies}}}]}}"#
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "devuser".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn go(args: &[&str], policies: &str, fake: &mut Fake) -> Exit {
    let directory = tempfile::tempdir().unwrap();
    let mut all = vec!["abapreach".to_owned()];

    all.extend(args.iter().map(|arg| (*arg).to_owned()));
    run(all, env(directory.path(), policies), fake)
}

fn adt(replies: Vec<Reply>) -> (Rc<RefCell<ScriptedTransport>>, Opened) {
    let cell = Rc::new(RefCell::new(ScriptedTransport::replies(replies)));

    (cell.clone(), Ok(Box::new(Shared(cell))))
}

fn stateless(replies: Vec<Reply>) -> (Rc<RefCell<ScriptedTransport>>, Opened) {
    let cell = Rc::new(RefCell::new(ScriptedTransport::replies(replies)));

    (cell.clone(), Ok(Box::new(Stateless(cell))))
}

fn client(category: &str) -> Reply {
    Ok(Response::text(
        200,
        preview(&[("CCCATEGORY", &[Some(category)])]),
    ))
}

fn ok(body: &str) -> Reply {
    Ok(Response::text(200, body))
}

#[track_caller]
fn assert_stops_before_connecting(args: &[&str], policies: &str, needle: &str) {
    let mut fake = Fake::default();
    let exit = go(args, policies, &mut fake);

    assert_eq!(exit.code, 2, "{args:?}: {}", exit.stderr);
    assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    assert!(exit.stderr.contains(needle), "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0), "{args:?}");
}

#[test]
fn inactive_lists_the_own_entries_through_the_router() {
    let (sap, opened) = adt(vec![ok(INACTIVE)]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(&["inactive", "--conn", "dev_100"], "{}", &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.contains("PROG/P ZTEST_MAIN DEVUSER"));
    assert!(!exit.stdout.contains("ZOTHER"));
    assert_eq!(sap.borrow().calls.len(), 1);
    assert_eq!(sap.borrow().closes, 1);
}

#[test]
fn inactive_all_asks_for_every_user() {
    let (sap, opened) = adt(vec![ok(INACTIVE)]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(&["inactive", "--all", "--conn", "dev_100"], "{}", &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.contains("PROG/P ZOTHER OTHERUSER"));
    assert!(sap.borrow().calls[0].uri.ends_with("?USERNAME=*"));
}

#[test]
fn activate_stops_locally_on_a_bad_argument() {
    for (args, needle) in [
        (
            vec!["activate", "FUGR/F:ZFG", "--conn", "dev_100"],
            "FUGR/F is refused",
        ),
        (
            vec!["activate", "TABL/DT:ZTAB", "--conn", "dev_100"],
            "DDIC",
        ),
        (
            vec!["activate", "PROG/P:SAPMSYST", "--conn", "dev_100"],
            "Z and Y",
        ),
        (
            vec!["activate", "PROG/P:ZA", "PROG/P:za", "--conn", "dev_100"],
            "named twice",
        ),
    ] {
        assert_stops_before_connecting(&args, "{}", needle);
    }

    let mut fake = Fake::default();
    let exit = go(&["activate", "--conn", "dev_100"], "{}", &mut fake);

    assert_eq!(exit.code, 2);
    assert_eq!(fake.adt_opened, 0);
}

#[test]
fn activate_simulate_runs_the_checks_and_activates_nothing() {
    let (sap, opened) = adt(vec![client("T"), ok(INACTIVE)]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(
        &[
            "activate",
            "PROG/P:ztest_main",
            "--simulate",
            "--conn",
            "dev_100",
        ],
        "{}",
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(
        exit.stdout
            .starts_with("SIMULATED_OK PROG/P:ZTEST_MAIN user DEVUSER")
    );
    assert!(
        sap.borrow()
            .calls
            .iter()
            .all(|call| !call.uri.contains("activation?"))
    );
}

#[test]
fn activate_refuses_a_productive_client_with_exit_1_and_sends_nothing_else() {
    let (sap, opened) = adt(vec![client("P")]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(
        &["activate", "PROG/P:ZTEST_MAIN", "--conn", "dev_100"],
        "{}",
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert!(
        exit.stderr
            .starts_with("REJECTED: client 100 is productive")
    );
    assert_eq!(sap.borrow().calls.len(), 1);
}

#[test]
fn activate_runs_and_proves_through_the_application() {
    let without = INACTIVE
        .split("<ioc:entry>")
        .filter(|chunk| !chunk.contains("name=\"ZTEST_MAIN\""))
        .collect::<Vec<_>>()
        .join("<ioc:entry>");
    let (_, opened) = adt(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        ok(&without),
    ]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(
        &["activate", "PROG/P:ZTEST_MAIN", "--conn", "dev_100"],
        "{}",
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(
        exit.stdout,
        "ACTIVE PROG/P:ZTEST_MAIN\nSUMMARY: 1 ACTIVE, 0 INACTIVE"
    );
}

#[test]
fn aunit_stops_locally_on_a_bad_scope() {
    for args in [
        vec!["aunit", "--conn", "dev_100"],
        vec!["aunit", "PROG", "SAPMSYST", "--conn", "dev_100"],
        vec![
            "aunit",
            "PROG",
            "ZP",
            "--package",
            "ZPKG",
            "--conn",
            "dev_100",
        ],
        vec![
            "aunit",
            "--package",
            "ZPKG",
            "--tr",
            "DEVK900001",
            "--conn",
            "dev_100",
        ],
        vec!["aunit", "--package", "Z PKG", "--conn", "dev_100"],
        vec!["aunit", "PROG", "--conn", "dev_100"],
        vec!["aunit", "TABL", "ZT", "--conn", "dev_100"],
    ] {
        assert_stops_before_connecting(&args, "{}", "");
    }
}

#[test]
fn the_policy_run_off_refuses_aunit_before_any_connection() {
    assert_stops_before_connecting(
        &["aunit", "CLAS", "ZCL_TEST", "--conn", "dev_100"],
        r#"{"run":"off"}"#,
        "policy run",
    );
}

#[test]
fn the_policy_run_plan_and_direct_both_allow_aunit_without_a_plan_file() {
    for policy in [r#"{"run":"plan"}"#, r#"{"run":"direct"}"#, "{}"] {
        let (_, opened) = adt(vec![client("T"), ok("<class/>"), ok(CLEAN)]);
        let mut fake = Fake::default();

        fake.adt.push_back(opened);

        let exit = go(
            &["aunit", "CLAS", "ZCL_TEST", "--conn", "dev_100"],
            policy,
            &mut fake,
        );

        assert_eq!(exit.code, 0, "{policy}: {}", exit.stderr);
        assert_eq!(exit.stdout, "TESTS: 2 methods, 0 alerts\nAUNIT: ACTIVE");
    }
}

#[test]
fn failed_tests_exit_1_and_a_productive_client_runs_nothing() {
    let (_, opened) = adt(vec![client("T"), ok("<class/>"), ok(FAILED)]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(
        &["aunit", "CLAS", "ZCL_TEST", "--conn", "dev_100"],
        "{}",
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert!(exit.stdout.ends_with("AUNIT: REJECTED"));

    let (sap, opened) = adt(vec![client("P")]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(
        &["aunit", "CLAS", "ZCL_TEST", "--conn", "dev_100"],
        "{}",
        &mut fake,
    );

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.contains("productive"));
    assert_eq!(sap.borrow().calls.len(), 1);
}

#[test]
fn a_connection_that_never_opens_falls_back_to_rfc_before_anything_is_sent() {
    let mut fake = Fake::default();

    fake.adt.push_back(Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    )));

    let (_, rfc) = adt(vec![ok(INACTIVE)]);

    fake.rfc.push_back(rfc);

    let exit = go(&["inactive", "--conn", "dev_100"], "{}", &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
}

#[test]
fn the_policy_keys_are_known_and_a_bad_value_stops_the_load() {
    let mut fake = Fake::default();
    let exit = go(
        &["inactive", "--conn", "dev_100"],
        r#"{"run":"sometimes"}"#,
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert_eq!(fake.adt_opened, 0);

    let exit = go(
        &["inactive", "--conn", "dev_100"],
        r#"{"mcp_run":"token"}"#,
        &mut fake,
    );

    assert_eq!(exit.code, 2);
}

// ---- a stateless ADT HTTP session ------------------------------------------------------------------

#[track_caller]
fn assert_left_for_rfc(exit: &Exit, fake: &Fake, adt: &Rc<RefCell<ScriptedTransport>>) {
    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(
        exit.stdout.starts_with("backend: rfc\nreason: "),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stderr
            .contains("NOTE: ADT HTTP session is stateless on this release"),
        "{}",
        exit.stderr
    );
    assert!(
        exit.stderr.contains(
            "NOTE: ADT HTTP failed before anything was written; falling back to RFC SADT"
        ),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
    // Only the first read reached the stateless session; nothing was written there.
    assert_eq!(adt.borrow().calls.len(), 1);
    assert!(
        adt.borrow()
            .calls
            .iter()
            .all(|call| call.method == "POST" || call.method == "GET")
    );
}

#[test]
fn aunit_leaves_a_stateless_http_session_for_rfc_before_reading_the_object() {
    // The stateless session would answer 404 for an object that exists.
    let (adt, opened) = stateless(vec![client("T"), Ok(Response::text(404, ""))]);
    let (_, rfc) = self::adt(vec![client("T"), ok("<class/>"), ok(CLEAN)]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);
    fake.rfc.push_back(rfc);

    let exit = go(
        &["aunit", "CLAS", "ZCL_TEST", "--conn", "dev_100"],
        "{}",
        &mut fake,
    );

    assert_left_for_rfc(&exit, &fake, &adt);
    assert!(exit.stdout.ends_with("AUNIT: ACTIVE"), "{}", exit.stdout);
    assert!(!exit.stdout.contains("does not exist"));
}

#[test]
fn activate_and_inactive_leave_a_stateless_http_session_for_rfc() {
    let (adt, opened) = stateless(vec![client("T")]);
    let (_, rfc) = self::adt(vec![client("T"), ok(INACTIVE)]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);
    fake.rfc.push_back(rfc);

    let exit = go(
        &[
            "activate",
            "PROG/P:ztest_main",
            "--simulate",
            "--conn",
            "dev_100",
        ],
        "{}",
        &mut fake,
    );

    assert_left_for_rfc(&exit, &fake, &adt);
    assert!(
        exit.stdout
            .contains("SIMULATED_OK PROG/P:ZTEST_MAIN user DEVUSER")
    );

    let (adt, opened) = stateless(vec![ok("<empty/>")]);
    let (_, rfc) = self::adt(vec![ok(INACTIVE)]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);
    fake.rfc.push_back(rfc);

    let exit = go(&["inactive", "--conn", "dev_100"], "{}", &mut fake);

    assert_left_for_rfc(&exit, &fake, &adt);
    assert!(
        exit.stdout.contains("PROG/P ZTEST_MAIN DEVUSER"),
        "{}",
        exit.stdout
    );
}

#[test]
fn backend_pins_aunit_activate_and_inactive() {
    for args in [
        vec!["inactive", "--backend", "rfc", "--conn", "dev_100"],
        vec![
            "aunit",
            "CLAS",
            "ZCL_TEST",
            "--backend",
            "rfc",
            "--conn",
            "dev_100",
        ],
    ] {
        let (_, rfc) = adt(vec![client("T"), ok(INACTIVE), ok("<class/>"), ok(CLEAN)]);
        let mut fake = Fake::default();

        fake.rfc.push_back(rfc);

        let exit = go(&args, "{}", &mut fake);

        assert_eq!(exit.code, 0, "{args:?}: {}", exit.stderr);
        assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1), "{args:?}");
        assert!(
            exit.stdout
                .starts_with("backend: rfc\nreason: explicit --backend")
        );
    }

    // A pinned ADT session is never left, and its 404 is not taken for a missing object.
    let (_, opened) = stateless(vec![client("T"), Ok(Response::text(404, ""))]);
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    let exit = go(
        &[
            "aunit",
            "CLAS",
            "ZCL_TEST",
            "--backend",
            "adt",
            "--conn",
            "dev_100",
        ],
        "{}",
        &mut fake,
    );

    assert_eq!(exit.code, 1, "{}", exit.stdout);
    assert!(exit.stderr.contains("stateless"), "{}", exit.stderr);
    assert!(!exit.stdout.contains("does not exist"), "{}", exit.stdout);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
}
