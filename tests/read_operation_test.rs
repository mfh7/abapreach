mod common;

use abapreach::{
    adt::prog_uri,
    backend::Failure,
    config::Connection,
    domain::Status,
    operation::read::ReadProg,
    transport::{Response, TransportError},
};
use std::fs;

const PROGRAM_URI: &str = "/sap/bc/adt/programs/programs/ytest_abapreach01";
const SOURCE_URI: &str =
    "/sap/bc/adt/programs/programs/ytest_abapreach01/source/main?version=active";
const V1_RESPONSE_BODY: &str = "REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v1'.";
const V1_WRITTEN_SOURCE: &str = "REPORT ytest_abapreach01.\n\nWRITE / 'v1'.\n";
const V1_HASH: &str = "63886692bf652fa419147f746d6b7760d6c991606fa9603d8ade3caf08ac7c64";

#[test]
fn reads_fixture_normalizes_crlf_writes_hash_and_warns_for_tls() {
    let mut transport =
        common::from_fixture(common::fixture_path("dev-750-rfc.json"), |exchange| {
            exchange.request.method == "GET"
                && exchange.request.uri == SOURCE_URI
                && exchange.request.headers.get("Accept").map(String::as_str) == Some("text/plain")
                && exchange.response.status == 200
                && exchange.response.body == V1_RESPONSE_BODY
        });
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("program.abap");
    let outcome = ReadProg::run(
        &mut transport,
        "YTEST_ABAPREACH01",
        &output,
        false,
        Some(&Connection::test_insecure()),
    )
    .unwrap();

    assert_eq!(prog_uri("YTEST_ABAPREACH01"), PROGRAM_URI);
    assert_eq!(outcome.status, Some(Status::Active));
    assert_eq!(outcome.exit, 0);
    assert_eq!(outcome.stderr, "");
    assert_eq!(
        outcome.stdout,
        format!(
            "WARNING: connection DEV_100 uses insecure TLS or HTTP.\n3 lines -> {}\nhash {V1_HASH}",
            output.display()
        )
    );
    assert_eq!(transport.calls.len(), 1);
    assert_eq!(transport.calls[0].uri, SOURCE_URI);
    assert_eq!(
        transport.calls[0].headers.get("Accept").map(String::as_str),
        Some("text/plain")
    );
    assert_eq!(fs::read_to_string(output).unwrap(), V1_WRITTEN_SOURCE);
}

#[test]
fn refuses_existing_output_before_transport_request() {
    let mut transport = common::ScriptedTransport::replies(Vec::new());
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("program.abap");
    fs::write(&output, "do not overwrite\n").unwrap();

    let error = ReadProg::run(
        &mut transport,
        "YTEST_ABAPREACH01",
        &output,
        false,
        Some(&Connection::test_insecure()),
    )
    .unwrap_err();

    assert_eq!(
        error,
        format!(
            "{} already exists; use --force to overwrite",
            output.display()
        )
    );
    assert!(transport.calls.is_empty());
    assert_eq!(fs::read_to_string(output).unwrap(), "do not overwrite\n");
}

#[test]
fn failures_carry_the_routing_class_and_run_keeps_the_string_contract() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("program.abap");
    let cases = [
        (
            Err(TransportError::PreSendConnectivity("no route".to_owned())),
            Failure::PreSendConnectivity,
        ),
        (
            Err(TransportError::PostSendUncertain("reset".to_owned())),
            Failure::PostSendUncertain,
        ),
        (
            Err(TransportError::NoResponse("closed".to_owned())),
            Failure::PreSendConnectivity,
        ),
        (Ok(Response::empty(401)), Failure::Authorization),
        (Ok(Response::empty(403)), Failure::Validation),
        (Ok(Response::empty(404)), Failure::Validation),
        (
            Ok(Response {
                body: vec![0xff, 0xfe],
                ..Response::empty(200)
            }),
            Failure::Validation,
        ),
    ];

    for (reply, expected) in cases {
        let mut transport = common::ScriptedTransport::replies(vec![reply.clone()]);
        let error =
            ReadProg::run_classified(&mut transport, "Z", &output, false, None).unwrap_err();

        assert_eq!(error.failure, expected, "{reply:?}");
        assert!(!output.exists());
    }

    let mut transport = common::ScriptedTransport::replies(vec![Ok(Response::empty(404))]);
    let message = ReadProg::run(&mut transport, "Z", &output, false, None).unwrap_err();

    assert_eq!(message, "read PROG Z: HTTP 404");
}

/// The status of a refused logon travels in the message, so the read reports what the RFC
/// backend reports instead of a read that failed for its own reason.
#[test]
fn a_refused_logon_is_the_authorization_failure_with_its_status() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("program.abap");
    let mut transport = common::ScriptedTransport::replies(vec![Ok(Response::empty(401))]);
    let error = ReadProg::run_classified(&mut transport, "Z", &output, false, None).unwrap_err();

    assert_eq!(error.failure, Failure::Authorization);
    assert_eq!(error.message, "authorization failed: HTTP 401");
    assert_eq!(transport.calls.len(), 1);
    assert!(!output.exists());
}
