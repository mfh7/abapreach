//! Test-only fault injection: the RFC call runs and SAP executes the write, then the reply is
//! dropped, so the deploy has to reconcile an unknown outcome. Only built with the feature.
#![cfg(feature = "fault-injection")]

mod common;
mod replay;

use abapreach::config::Connection;
use abapreach::fault::{Fault, Target};
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
    Fault::from_lookup(Some(target)).unwrap().unwrap()
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

    let put = transport.request(Request::put(
        PUT_URI,
        b"REPORT ytest_abapreach01.\n".to_vec(),
    ));

    assert_eq!(
        put.unwrap_err(),
        TransportError::PostSendUncertain("injected fault: reply dropped after PUT".to_owned())
    );
    assert_eq!(transport.api().counts(), (1, 0, 2));

    let activation = transport.request(Request::post(ACTIVATION_URI, Vec::new()));

    assert!(activation.is_ok());
}

#[test]
fn the_activation_is_dropped_and_the_put_is_not() {
    let mut transport =
        RfcTransport::with_fault(FakeRfcApi::ok(), Some(injection("drop-reply:ACTIVATE")));
    let put = transport.request(Request::put(PUT_URI, Vec::new()));

    assert!(put.is_ok());

    let activation = transport.request(Request::post(ACTIVATION_URI, Vec::new()));

    assert_eq!(
        activation.unwrap_err(),
        TransportError::PostSendUncertain(
            "injected fault: reply dropped after ACTIVATE".to_owned()
        )
    );
    assert_eq!(transport.api().counts(), (1, 0, 2));
}

#[test]
fn without_a_fault_every_request_is_answered() {
    let mut transport = RfcTransport::with_fault(FakeRfcApi::ok(), None);

    assert!(transport.request(Request::put(PUT_URI, Vec::new())).is_ok());
    assert!(
        transport
            .request(Request::post(ACTIVATION_URI, Vec::new()))
            .is_ok()
    );
}

#[test]
fn a_target_only_matches_its_own_write() {
    let put = injection("drop-reply:PUT");
    let activation = injection("drop-reply:ACTIVATE");

    assert_eq!(put.target(), Target::Put);
    assert_eq!(activation.target(), Target::Activate);
    assert_eq!(put.drop_reply(false), None);
    assert_eq!(activation.drop_reply(false), None);
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
        env(directory.path(), Some("drop-reply:DELETE")),
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
