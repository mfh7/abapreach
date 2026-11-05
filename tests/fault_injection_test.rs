//! Test-only fault injection: the RFC call runs and SAP executes the write, then the reply is
//! dropped, so the deploy has to reconcile an unknown outcome. Only built with the feature.
#![cfg(feature = "fault-injection")]

mod common;
mod replay;

use abapreach::backend::{BackendKind, Selection, SelectionReason};
use abapreach::cli::CreateKind;
use abapreach::config::Connection;
use abapreach::fault::{Fault, Target};
use abapreach::operation::create::{CreateInput, CreateProg};
use abapreach::operation::deploy::{CancelFlag, DeployInput, DeployProg};
use abapreach::transport::rfc::{
    FakeRfcApi, RfcApi, RfcEndpointRequest, RfcEndpointResponse, RfcTransport,
    SADT_REST_RFC_ENDPOINT,
};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, Opened, RfcLibraryInfo, run};
use replay::{Replay, Step, fx, no_inactive, replay};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const FIXTURE: &str = "dev-750-rfc.json";
const RETRY: &str = "check with read before retrying";
const WARNING: &str = "WARNING: fault injection active (drop-reply:PUT)";
const UNKNOWN: &str = "STOP: unknown ABAPREACH_FAULT value";
const PUT_URI: &str =
    "/sap/bc/adt/programs/programs/ytest_abapreach01/source/main?lockHandle=qdbcyR%2BxAqwY%3D";
const ACTIVATION_URI: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";

fn with_content_type(mut request: Request, content_type: &str) -> Request {
    request
        .headers
        .insert("Content-Type".to_owned(), content_type.to_owned());
    request
}

fn put(uri: &str, body: Vec<u8>) -> Request {
    with_content_type(Request::put(uri, body), "text/plain; charset=utf-8")
}

fn activation() -> Request {
    with_content_type(Request::post(ACTIVATION_URI, Vec::new()), "application/xml")
}

fn create(uri: &str, body: Vec<u8>) -> Request {
    with_content_type(Request::post(uri, body), "application/*")
}

fn v2() -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            "WRITE / 'v2'.".to_owned(),
        ],
        base: None,
        simulate: false,
        tr: None,
    }
}

fn injection(target: &str) -> Fault {
    Fault::from_lookup(Some(target)).unwrap().pop().unwrap()
}

/// The recorded deploy of v2 over v1 up to and including the PUT.
fn through_put() -> Vec<Step> {
    vec![
        fx(6),
        no_inactive(),
        fx(7),
        fx(6),
        no_inactive(),
        fx(8),
        fx(17),
    ]
}

fn then(mut steps: Vec<Step>, rest: Vec<Step>) -> Vec<Step> {
    steps.extend(rest);
    steps
}

/// A `Replay` behind `RfcApi`: the requests of a deploy reach the fake RFC endpoint, so a fault
/// added to the transport sits exactly where it sits in a live run.
struct FixtureApi {
    inner: Replay,
}

impl FixtureApi {
    fn uris(&self) -> Vec<String> {
        self.inner.uris()
    }

    fn count(&self, needle: &str) -> usize {
        self.inner
            .calls
            .iter()
            .filter(|c| c.uri.contains(needle))
            .count()
    }
}

impl RfcApi for FixtureApi {
    fn open(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    fn invoke(
        &mut self,
        request: RfcEndpointRequest,
    ) -> Result<RfcEndpointResponse, TransportError> {
        assert_eq!(request.function, SADT_REST_RFC_ENDPOINT);

        let response = self.inner.request(Request {
            method: request.method,
            uri: request.uri,
            headers: request.headers,
            body: request.body,
        })?;

        Ok(RfcEndpointResponse {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }

    fn close(&mut self) {}
}

fn faulty(steps: Vec<Step>, target: &str) -> RfcTransport<FixtureApi> {
    RfcTransport::with_fault(
        FixtureApi {
            inner: replay(FIXTURE, steps),
        },
        Some(injection(target)),
    )
}

#[test]
fn a_dropped_put_reply_sends_the_call_and_then_reconciles() {
    let inactive_is_the_file = Step::Pair(26, 20);
    let mut transport = faulty(
        then(through_put(), vec![fx(18), fx(10), inactive_is_the_file]),
        "drop-reply:PUT",
    );
    let result = DeployProg::run_classified(&mut transport, v2(), None, &CancelFlag::new());

    assert_eq!(
        (result.outcome.status, result.outcome.exit),
        (Some(abapreach::domain::Status::Inactive), 1)
    );
    assert!(
        result
            .outcome
            .stdout
            .contains("INACTIVE: the PUT reached SAP but the activation did not")
    );
    assert!(result.outcome.stdout.ends_with(RETRY));
    // The RFC call ran: the request reached the fake endpoint, the verdict is not "not sent".
    assert_eq!(transport.api().count("source/main?lockHandle="), 1);
    assert_eq!(transport.api().count("activation"), 0);
    assert!(!result.fallback_allowed);
    transport.api().inner.assert_finished();
}

#[test]
fn a_dropped_activation_reply_reconciles_to_active() {
    let no_inactive_source = Step::ReplyTo(26, Ok(Response::empty(404)));
    let mut transport = faulty(
        then(
            through_put(),
            vec![fx(18), fx(19), fx(20), no_inactive_source],
        ),
        "drop-reply:ACTIVATE",
    );
    let result = DeployProg::run_classified(&mut transport, v2(), None, &CancelFlag::new());
    let uris = transport.api().uris();

    assert_eq!(
        (result.outcome.status, result.outcome.exit),
        (Some(abapreach::domain::Status::Active), 1)
    );
    assert!(result.outcome.stdout.ends_with(&format!(
        "ACTIVE (reconciled): the active source equals the file; {RETRY}"
    )));
    assert_eq!(
        uris.iter().filter(|uri| uri.contains("activation")).count(),
        1
    );
    assert!(!result.fallback_allowed);
    transport.api().inner.assert_finished();
}

#[test]
fn a_request_that_is_not_the_target_is_untouched() {
    let simulate = vec![
        fx(6),
        no_inactive(),
        fx(7),
        fx(6),
        no_inactive(),
        fx(8),
        fx(9),
    ];
    let mut input = v2();
    input.simulate = true;
    let mut transport = faulty(simulate, "drop-reply:PUT");
    let result = DeployProg::run_classified(&mut transport, input, None, &CancelFlag::new());

    assert_eq!(
        (result.outcome.status, result.outcome.exit),
        (Some(abapreach::domain::Status::SimulatedOk), 0)
    );
    transport.api().inner.assert_finished();
}

#[test]
fn the_put_is_dropped_and_the_activation_is_not() {
    let mut transport =
        RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("drop-reply:PUT")));
    let get = transport.request(Request::get("/sap/bc/adt/discovery"));

    assert!(get.is_ok());

    let put = transport.request(put(PUT_URI, b"REPORT ytest_abapreach01.\n".to_vec()));

    assert_eq!(
        put.unwrap_err(),
        TransportError::PostSendUncertain("injected fault: reply dropped after PUT".to_owned())
    );
    assert_eq!(transport.api().counts(), (1, 0, 2));

    let activation = transport.request(activation());

    assert!(activation.is_ok());
}

#[test]
fn the_activation_is_dropped_and_the_put_is_not() {
    let mut transport =
        RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("drop-reply:ACTIVATE")));
    let put = transport.request(put(PUT_URI, Vec::new()));

    assert!(put.is_ok());

    let activation = transport.request(activation());

    assert_eq!(
        activation.unwrap_err(),
        TransportError::PostSendUncertain(
            "injected fault: reply dropped after ACTIVATE".to_owned()
        )
    );
    assert_eq!(transport.api().counts(), (1, 0, 2));
}

#[test]
fn the_create_post_is_dropped_after_it_ran_and_nothing_else_is() {
    let creates = [
        "/sap/bc/adt/functions/groups",
        "/sap/bc/adt/functions/groups/zfg/fmodules?corrNr=DEVK900001",
        "/sap/bc/adt/oo/classes",
        "/sap/bc/adt/oo/classes?corrNr=DEVK900001",
        "/sap/bc/adt/oo/interfaces",
    ];

    for uri in creates {
        let mut transport =
            RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("drop-reply:CREATE")));
        let create = transport.request(create(uri, b"<x/>".to_vec()));

        assert_eq!(
            create.unwrap_err(),
            TransportError::PostSendUncertain(
                "injected fault: reply dropped after CREATE".to_owned()
            ),
            "{uri}"
        );
        // The request ran: only its reply is gone.
        assert_eq!(transport.api().counts(), (1, 0, 1), "{uri}");
    }
}

#[test]
fn a_create_fault_leaves_validation_reconciliation_and_the_other_writes_alone() {
    let mut transport =
        RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("drop-reply:CREATE")));
    let others = [
        Request::post(
            "/sap/bc/adt/oo/validation/objectname?objtype=CLAS%2FOC&objname=ZCL&description=T&packagename=%24TMP",
            Vec::new(),
        ),
        Request::post(
            "/sap/bc/adt/functions/validation?objtype=FUGR%2FF&objname=ZFG&description=T&packagename=%24TMP",
            Vec::new(),
        ),
        Request::get(
            "/sap/bc/adt/oo/classes/zcl/objectstructure?version=active&withShortDescriptions=true",
        ),
        Request::get("/sap/bc/adt/functions/groups/zfg"),
        Request::post(
            "/sap/bc/adt/oo/classes/zcl?_action=LOCK&accessMode=MODIFY",
            Vec::new(),
        ),
        put(
            "/sap/bc/adt/oo/classes/zcl/source/main?lockHandle=a%2Bb",
            Vec::new(),
        ),
        create(
            "/sap/bc/adt/oo/classes/zcl/includes?lockHandle=a%2Bb",
            Vec::new(),
        ),
        activation(),
    ];

    for request in others {
        assert!(transport.request(request.clone()).is_ok(), "{request:?}");
    }

    // The PUT and activation faults do not touch the create either.
    for target in ["drop-reply:PUT", "drop-reply:ACTIVATE"] {
        let mut transport = RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection(target)));

        assert!(
            transport
                .request(create("/sap/bc/adt/oo/classes", Vec::new()))
                .is_ok(),
            "{target}"
        );
    }
}

const RECONCILE_URI: &str =
    "/sap/bc/adt/oo/classes/zcl/objectstructure?version=active&withShortDescriptions=true";

#[test]
fn the_read_after_a_create_fails_without_reaching_sap_and_no_other_read_does() {
    let mut transport =
        RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("fail-read:RECONCILE")));

    // Reads before the create, and the create itself, are untouched.
    assert!(transport.request(Request::get(RECONCILE_URI)).is_ok());
    assert!(
        transport
            .request(Request::get("/sap/bc/adt/functions/groups/zfg"))
            .is_ok()
    );
    assert!(
        transport
            .request(create("/sap/bc/adt/oo/classes", Vec::new()))
            .is_ok()
    );
    assert_eq!(transport.api().counts(), (1, 0, 3));

    let read = transport.request(Request::get(RECONCILE_URI));

    assert_eq!(
        read.unwrap_err(),
        TransportError::NoResponse("injected fault: the reconciliation read failed".to_owned())
    );
    // The read never reached the endpoint.
    assert_eq!(transport.api().counts(), (1, 0, 3));
}

#[test]
fn the_reconciliation_fault_leaves_a_run_without_a_create_alone() {
    for fault in [Some("drop-reply:CREATE"), Some("drop-reply:PUT"), None] {
        let mut transport = RfcTransport::with_fault(FakeRfcApi::ok(), fault.map(injection));

        assert!(
            transport
                .request(create("/sap/bc/adt/oo/classes", Vec::new()))
                .is_err()
                == (fault == Some("drop-reply:CREATE"))
        );
        // Only the reconciliation fault fails a read; a drop-reply fault never does.
        assert!(
            transport.request(Request::get(RECONCILE_URI)).is_ok(),
            "{fault:?}"
        );
    }
}

#[test]
fn both_faults_combine_a_lost_create_reply_and_a_failed_reconciliation() {
    let mut transport = RfcTransport::with_faults(
        FakeRfcApi::ok(),
        Fault::from_lookup(Some("drop-reply:CREATE,fail-read:RECONCILE")).unwrap(),
    );
    let post = transport.request(create("/sap/bc/adt/oo/classes", Vec::new()));

    assert!(post.unwrap_err().is_post_send_uncertain());
    assert!(transport.request(Request::get(RECONCILE_URI)).is_err());
    assert_eq!(transport.api().counts(), (1, 0, 1));
}

fn clas_create() -> CreateInput {
    CreateInput {
        kind: CreateKind::Clas,
        name: "ZCL".to_owned(),
        text: "T".to_owned(),
        package: None,
        group: None,
        tr: None,
        simulate: false,
    }
}

fn reply(status: u16, body: &str) -> RfcEndpointResponse {
    RfcEndpointResponse {
        status,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    }
}

#[test]
fn a_failed_reconciliation_read_ends_the_create_partial_without_a_second_post() {
    let check_ok = concat!(
        r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
        r#"<CHECK_RESULT>X</CHECK_RESULT></DATA></asx:values></asx:abap>"#,
    );
    let selection = Selection::new(BackendKind::RfcSadt, SelectionReason::DefaultAdt);
    let mut connection = Connection::test_insecure();

    connection.user = "TESTUSER".to_owned();

    for (value, detail) in [
        (
            "drop-reply:CREATE,fail-read:RECONCILE",
            "no response to a read-only request: injected fault: the reconciliation read failed",
        ),
        // Without the read fault the same lost reply reconciles to an existing object.
        ("drop-reply:CREATE", "ZCL exists"),
    ] {
        let mut transport = RfcTransport::with_faults(
            FakeRfcApi::with_responses(vec![
                reply(200, check_ok),
                reply(200, ""),
                reply(200, "<x/>"),
            ]),
            Fault::from_lookup(Some(value)).unwrap(),
        );
        let result = CreateProg::run(
            &mut transport,
            clas_create(),
            Some(&selection),
            Some(&connection),
        );

        assert_eq!(
            result.outcome.status,
            Some(abapreach::domain::Status::Partial),
            "{value}"
        );
        assert_eq!(result.outcome.exit, 1, "{value}");
        assert!(
            result.outcome.stdout.contains("CREATE: PARTIAL"),
            "{value}: {}",
            result.outcome.stdout
        );
        assert!(
            result.outcome.stdout.contains(detail),
            "{value}: {}",
            result.outcome.stdout
        );
        assert!(!result.fallback_allowed, "{value}");
    }
}

#[test]
fn without_a_fault_every_request_is_answered() {
    let mut transport = RfcTransport::with_fault(FakeRfcApi::ok(), None);

    assert!(transport.request(put(PUT_URI, Vec::new())).is_ok());
    assert!(transport.request(activation()).is_ok());
}

#[test]
fn a_target_only_matches_its_own_write() {
    let put = injection("drop-reply:PUT");
    let activation = injection("drop-reply:ACTIVATE");
    let create = injection("drop-reply:CREATE");

    assert_eq!(put.target(), Some(Target::Put));
    assert_eq!(activation.target(), Some(Target::Activate));
    assert_eq!(create.target(), Some(Target::Create));
    assert_eq!(put.drop_reply(false), None);
    assert_eq!(activation.drop_reply(false), None);
    assert_eq!(create.drop_reply(false), None);
}

#[derive(Default)]
struct Unreachable {
    opens: usize,
}

impl Factories for Unreachable {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.opens += 1;
        Err(TransportError::PreSendConnectivity("no route".to_owned()))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.opens += 1;
        Err(TransportError::PreSendConnectivity("no route".to_owned()))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        self.opens += 1;
        Err("no library".to_owned())
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        self.opens += 1;
        Err(TransportError::PreSendConnectivity("no route".to_owned()))
    }
}

fn env(directory: &Path, fault: Option<&str>) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    let mut env = BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ]);

    if let Some(fault) = fault {
        env.insert("ABAPREACH_FAULT".to_owned(), fault.to_owned());
    }

    env
}

fn read_args(out: &str) -> Vec<&str> {
    vec![
        "abapreach",
        "read",
        "PROG",
        "ZTEST",
        out,
        "--conn",
        "dev_100",
    ]
}

#[test]
fn every_command_prints_the_warning_first() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("ZTEST.abap");
    let mut fake = Unreachable::default();
    let exit = run(
        read_args(out.to_str().unwrap())
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        env(directory.path(), Some("drop-reply:PUT")),
        &mut fake,
    );

    assert!(exit.stderr.starts_with(WARNING), "{}", exit.stderr);
    assert!(fake.opens > 0, "the command still ran");
}

#[test]
fn an_unknown_value_stops_before_anything_is_opened() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("ZTEST.abap");
    let mut fake = Unreachable::default();
    let exit = run(
        read_args(out.to_str().unwrap())
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        env(directory.path(), Some("drop-reply:ROLLBACK")),
        &mut fake,
    );

    assert_eq!(exit.code, 2);
    assert_eq!(exit.stderr, UNKNOWN);
    assert_eq!(fake.opens, 0);
}

#[test]
fn a_valid_fault_warning_precedes_every_early_stop() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("ZTEST.abap");
    let mut unknown_connection: Vec<String> = read_args(out.to_str().unwrap())
        .into_iter()
        .map(str::to_owned)
        .collect();
    *unknown_connection.last_mut().unwrap() = "unknown_100".to_owned();
    let cases = [
        unknown_connection,
        vec![
            "abapreach".to_owned(),
            "read".to_owned(),
            "PROG".to_owned(),
            "Z?TEST".to_owned(),
            out.display().to_string(),
            "--conn".to_owned(),
            "dev_100".to_owned(),
        ],
        vec!["abapreach".to_owned(), "bogus".to_owned()],
    ];

    for args in cases {
        let mut fake = Unreachable::default();
        let exit = run(
            args,
            env(directory.path(), Some("drop-reply:PUT")),
            &mut fake,
        );

        assert_eq!(exit.code, 2);
        assert!(
            exit.stderr.starts_with(&format!("{WARNING}\n")),
            "{}",
            exit.stderr
        );
        assert_eq!(fake.opens, 0);
    }
}

#[test]
fn a_combined_value_prints_every_fault_in_one_warning() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Unreachable::default();
    let exit = run(
        vec!["abapreach".to_owned(), "bogus".to_owned()],
        env(
            directory.path(),
            Some("drop-reply:CREATE,fail-read:RECONCILE"),
        ),
        &mut fake,
    );

    assert!(
        exit.stderr.starts_with(
            "WARNING: fault injection active (drop-reply:CREATE,fail-read:RECONCILE)\n"
        ),
        "{}",
        exit.stderr
    );
}

#[test]
fn without_the_variable_nothing_is_printed_or_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("ZTEST.abap");
    let mut fake = Unreachable::default();
    let exit = run(
        read_args(out.to_str().unwrap())
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        env(directory.path(), None),
        &mut fake,
    );

    assert!(
        !exit.stderr.contains("WARNING: fault injection"),
        "{}",
        exit.stderr
    );
}

const CHILD_MARKER: &str = "ABAPREACH_FAULT_TEST_CHILD";

/// The environment is process-wide and `set_var` is unsafe in a threaded test run, so the check
/// runs in a child process that inherits the variable.
#[test]
fn the_process_environment_selects_the_faults() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_faults_of_the_inherited_environment",
            "--nocapture",
        ])
        .env(abapreach::fault::ENV, "drop-reply:PUT,fail-read:RECONCILE")
        .env(CHILD_MARKER, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("1 passed"),
        "the child test did not run: {stdout}"
    );
}

#[test]
fn the_faults_of_the_inherited_environment() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }

    assert_eq!(
        Fault::from_env(),
        [Fault::DropReply(Target::Put), Fault::FailReconcileRead]
    );
}

#[test]
fn the_delete_is_dropped_after_it_ran_and_nothing_else_is() {
    let delete = |uri: &str| Request::delete(format!("{uri}?lockHandle=a%2Bb&corrNr=DEVK900001"));

    for uri in [
        "/sap/bc/adt/programs/programs/zprog",
        "/sap/bc/adt/functions/groups/zfg",
        "/sap/bc/adt/functions/groups/zfg/fmodules/zfm",
        "/sap/bc/adt/oo/classes/zcl_a",
        "/sap/bc/adt/oo/interfaces/zif_a",
    ] {
        let mut transport =
            RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("drop-reply:DELETE")));
        let reply = transport.request(delete(uri));

        assert_eq!(
            reply.unwrap_err(),
            TransportError::PostSendUncertain(
                "injected fault: reply dropped after DELETE".to_owned()
            ),
            "{uri}"
        );
        // The request ran: only its reply is gone.
        assert_eq!(transport.api().counts(), (1, 0, 1), "{uri}");
    }

    let mut transport =
        RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("drop-reply:DELETE")));

    assert!(
        transport
            .request(Request::get("/sap/bc/adt/programs/programs/zprog"))
            .is_ok()
    );
    assert!(
        transport
            .request(put(PUT_URI, b"REPORT z.\n".to_vec()))
            .is_ok()
    );
    assert!(transport.request(activation()).is_ok());
}

#[test]
fn the_other_faults_leave_a_delete_alone() {
    for value in ["drop-reply:PUT", "drop-reply:CREATE", "drop-reply:ACTIVATE"] {
        let mut transport = RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection(value)));

        assert!(
            transport
                .request(Request::delete(
                    "/sap/bc/adt/programs/programs/zprog?lockHandle=a%2Bb"
                ))
                .is_ok(),
            "{value}"
        );
    }
}

#[test]
fn a_dropped_delete_reply_is_uncertain_and_reconciles_from_a_new_session() {
    use abapreach::cli::DeleteKind;
    use abapreach::operation::delete::{DeleteBackend, DeleteReply, Reconcile};
    use abapreach::operation::delete_adt::AdtDelete;

    let reply = |status: u16, body: &str| RfcEndpointResponse {
        status,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    };
    let lock =
        "<DATA><LOCK_HANDLE>a/b+c=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL></DATA>";
    let mut main = RfcTransport::with_fault(
        FakeRfcApi::with_responses(vec![reply(200, lock), reply(200, "")]),
        Some(injection("drop-reply:DELETE")),
    );
    let mut open = || -> Result<Box<dyn SadtTransport>, TransportError> {
        Ok(Box::new(RfcTransport::new(FakeRfcApi::with_responses(
            vec![reply(404, "<exc/>")],
        ))))
    };
    let mut backend = AdtDelete::new(&mut main, "TESTUSER", &mut open);
    let outcome = backend.delete(DeleteKind::Prog, "ZPROG", None);

    assert!(
        matches!(&outcome, DeleteReply::Uncertain(reason)
            if reason.contains("reply dropped after DELETE") && !reason.contains("a/b+c=")),
        "{outcome:?}"
    );
    assert_eq!(
        backend.reconcile(DeleteKind::Prog, "ZPROG"),
        Reconcile::Gone
    );
    // The lock and the DELETE ran once each; nothing was repeated.
    drop(backend);
    assert_eq!(main.api().counts(), (1, 0, 2));
}

// ---- delete reconciliation ---------------------------------------------------------------

mod delete_reconciliation {
    use super::injection;
    use abapreach::cli::DeleteTarget;
    use abapreach::fault::Fault;
    use abapreach::operation::delete::{self, Context, DeleteStatus};
    use abapreach::operation::delete_adt::AdtDelete;
    use abapreach::policy::{Confirmer, Policies};
    use abapreach::transport::rfc::{FakeRfcApi, RfcEndpointResponse, RfcTransport};
    use abapreach::transport::{Request, SadtTransport, TransportError};
    use std::collections::BTreeMap;

    struct Yes;

    impl Confirmer for Yes {
        fn interactive(&self) -> bool {
            true
        }

        fn confirm(&self, _: &str) -> bool {
            true
        }
    }

    fn reply(status: u16, body: &str) -> RfcEndpointResponse {
        RfcEndpointResponse {
            status,
            headers: BTreeMap::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn reads() -> Vec<RfcEndpointResponse> {
        vec![
            reply(
                200,
                "<x xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:responsible=\"OWNER\">\
                 <adtcore:packageRef adtcore:name=\"$TMP\"/></x>",
            ),
            reply(200, "REPORT z."),
            reply(200, "<feed xmlns=\"http://www.w3.org/2005/Atom\"/>"),
        ]
    }

    fn targets() -> Vec<DeleteTarget> {
        vec![
            "PROG:ZPROG".parse().unwrap(),
            "PROG:ZOTHER".parse().unwrap(),
        ]
    }

    /// Simulates both objects on a fault-free transport, then executes the plan on `faults`.
    /// The new session of the reconciliation answers that the object is gone.
    fn execute(faults: Vec<Fault>) -> delete::DeleteReport {
        let policies = Policies::default();
        let context = Context {
            conn: "DEV_100",
            user: "TESTUSER",
            policies: &policies,
            confirmer: &Yes,
        };
        let mut both = reads();

        both.extend(reads());

        let mut plain = RfcTransport::new(FakeRfcApi::with_responses(both));
        let mut no_open = || -> Result<Box<dyn SadtTransport>, TransportError> {
            Err(TransportError::PreSendConnectivity("unused".to_owned()))
        };
        let plan = {
            let mut backend = AdtDelete::new(&mut plain, "TESTUSER", &mut no_open);

            delete::simulate(&mut backend, &context, &targets(), None)
                .unwrap()
                .plan
                .expect("a plan")
        };
        let mut responses = reads();

        responses.push(reply(
            200,
            "<DATA><LOCK_HANDLE>a/b+c=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL></DATA>",
        ));
        responses.push(reply(200, ""));

        let mut main =
            RfcTransport::with_faults(FakeRfcApi::with_responses(responses), faults.clone());
        let mut open = || -> Result<Box<dyn SadtTransport>, TransportError> {
            Ok(Box::new(RfcTransport::with_faults(
                FakeRfcApi::with_responses(vec![reply(404, "<exc/>")]),
                faults.clone(),
            )))
        };
        let mut backend = AdtDelete::new(&mut main, "TESTUSER", &mut open);

        delete::execute(&mut backend, &context, &targets(), None, &plan).unwrap()
    }

    #[test]
    fn a_dropped_delete_reply_with_a_failing_reconciliation_read_is_partial_and_stops_the_batch() {
        let report = execute(vec![
            injection("drop-reply:DELETE"),
            injection("fail-read:RECONCILE"),
        ]);
        let statuses: Vec<_> = report.results.iter().map(|result| result.status).collect();

        assert_eq!(statuses, [DeleteStatus::Partial, DeleteStatus::Skipped]);
        assert_eq!(report.outcome.exit, 1, "{}", report.outcome.stdout);
        assert!(
            report.outcome.stdout.contains("the check failed"),
            "{}",
            report.outcome.stdout
        );
        assert!(!report.outcome.stdout.contains("DELETED"));
    }

    #[test]
    fn a_dropped_delete_reply_alone_still_reconciles_to_deleted() {
        let report = execute(vec![injection("drop-reply:DELETE")]);
        let statuses: Vec<_> = report.results.iter().map(|result| result.status).collect();

        assert_eq!(
            statuses,
            [DeleteStatus::DeletedReconciled, DeleteStatus::Skipped]
        );
    }

    #[test]
    fn the_fresh_session_of_a_dropped_delete_fails_its_reads_too() {
        let mut main = RfcTransport::with_faults(
            FakeRfcApi::with_responses(vec![reply(200, "")]),
            vec![injection("drop-reply:DELETE")],
        );

        assert!(
            main.request(Request::delete(
                "/sap/bc/adt/programs/programs/zprog?lockHandle=a%2Bb"
            ))
            .is_err()
        );

        // A new transport knows nothing of the drop but the process does.
        let mut fresh = RfcTransport::with_faults(
            FakeRfcApi::with_responses(vec![reply(404, "")]),
            vec![injection("fail-read:RECONCILE")],
        );

        assert!(matches!(
            fresh.request(Request::get("/sap/bc/adt/programs/programs/zprog")),
            Err(TransportError::NoResponse(_))
        ));
    }
}
