mod common;

use abapreach::{
    adt::prog_uri,
    backend::Failure,
    cli::{IncludeKind, LineRange, ObjectKind},
    config::Connection,
    domain::Status,
    operation::read::{ReadProg, ReadTarget, project_lines},
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

    // A 404 on the program source asks which object the name is: a program that has no source
    // keeps the plain HTTP error, a name neither URI knows is NOT_FOUND.
    let mut transport = common::ScriptedTransport::replies(vec![
        Ok(Response::empty(404)),
        Ok(Response::empty(200)),
    ]);
    let message = ReadProg::run(&mut transport, "Z", &output, false, None).unwrap_err();

    assert_eq!(message, "read PROG Z: HTTP 404");
    transport.assert_finished();

    let mut transport = common::ScriptedTransport::replies(vec![
        Ok(Response::empty(404)),
        Ok(Response::empty(404)),
        Ok(Response::empty(404)),
    ]);
    let error = ReadProg::run_classified(&mut transport, "Z", &output, false, None).unwrap_err();

    assert_eq!(
        error.message,
        "read PROG Z: NOT_FOUND (no program or include of that name)"
    );
    assert_eq!((error.failure, error.stop), (Failure::Validation, false));
    transport.assert_finished();
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

#[test]
fn read_output_is_lf_only_even_for_crlf_and_trailing_space_replies() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("program.abap");
    let mut transport = common::ScriptedTransport::replies(vec![Ok(Response::text(
        200,
        "REPORT z.  \r\n\r\nWRITE / 'v1'.\r\n\r\n",
    ))]);

    ReadProg::run(&mut transport, "Z", &output, false, None).unwrap();

    let written = fs::read(output).unwrap();

    assert_eq!(written, b"REPORT z.\n\nWRITE / 'v1'.\n");
    assert!(!written.contains(&b'\r'));
}

// ---- M3 source kinds: URI selection, normalization, hash, output ---------------------

const READ_FIXTURE: &str = "dev-750-http-read.json";
const SEARCH_URI: &str = concat!(
    "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch",
    "&query=YTEST_ABAPREACH_FM3&maxResults=10&objectType=FUGR%2FFF"
);
const FM_SOURCE_URI: &str = concat!(
    "/sap/bc/adt/functions/groups/ytest_abapreach_fg3/fmodules/ytest_abapreach_fm3",
    "/source/main?version=active"
);
const CLAS_SOURCE_URI: &str =
    "/sap/bc/adt/oo/classes/ytest_abapreach_cl3/source/main?version=active";
const CCAU_SOURCE_URI: &str =
    "/sap/bc/adt/oo/classes/ytest_abapreach_cl3/includes/definitions?version=active";
const INTF_SOURCE_URI: &str =
    "/sap/bc/adt/oo/interfaces/ytest_abapreach_if3/source/main?version=active";
const EMPTY_FM_SOURCE_URI: &str = concat!(
    "/sap/bc/adt/functions/groups/ytest_abapreach_fg4/fmodules/ytest_abapreach_fm4",
    "/source/main?version=active"
);
const NUL_SOURCE_URI: &str =
    "/sap/bc/adt/functions/groups/zfg740/fmodules/zfm740/source/main?version=active";
const NUL_SEARCH_URI: &str = concat!(
    "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch",
    "&query=ZFM740&maxResults=10&objectType=FUGR%2FFF"
);
const FM4_SEARCH_URI: &str = concat!(
    "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch",
    "&query=YTEST_ABAPREACH_FM4&maxResults=10&objectType=FUGR%2FFF"
);
const FRESH_CLAS_SOURCE_URI: &str =
    "/sap/bc/adt/oo/classes/ytest_abapreach_cl4/source/main?version=active";
const FRESH_CLAS_STRUCTURE_URI: &str = concat!(
    "/sap/bc/adt/oo/classes/ytest_abapreach_cl4",
    "/objectstructure?version=active&withShortDescriptions=true"
);
const FM_HASH: &str = "09140b690c1ad8465017452731367df32b9471044ec52b887fa1c718678c1cef";
/// The hash of one canonical empty source, whichever form the release returns for it.
const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The recorded exchanges whose request URI is listed, in fixture order.
fn read_fixture(uris: &'static [&'static str]) -> common::ScriptedTransport {
    common::from_fixture(
        common::fixture_path_in("m3", READ_FIXTURE),
        move |exchange| uris.contains(&exchange.request.uri.as_str()),
    )
}

const FM_READ_URIS: &[&str] = &[SEARCH_URI, FM_SOURCE_URI];

#[test]
fn a_function_module_read_resolves_its_group_then_reads_the_active_source() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("fm.abap");
    let mut transport = read_fixture(FM_READ_URIS);
    let target = ReadTarget::new(ObjectKind::Func, "YTEST_ABAPREACH_FM3");

    let outcome = ReadProg::run_target(&mut transport, target, &output, false, None).unwrap();

    assert_eq!(transport.calls.len(), 2);
    assert_eq!(transport.calls[0].uri, SEARCH_URI);
    assert_eq!(
        transport.calls[0].headers.get("Accept").map(String::as_str),
        Some("application/*")
    );
    assert_eq!(transport.calls[1].uri, FM_SOURCE_URI);
    assert_eq!(
        transport.calls[1].headers.get("Accept").map(String::as_str),
        Some("text/plain")
    );
    assert_eq!(outcome.status, Some(Status::Active));
    assert_eq!((outcome.exit, outcome.stderr.as_str()), (0, ""));
    assert_eq!(
        outcome.stdout,
        format!("10 lines -> {}\nhash {FM_HASH}", output.display())
    );
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        "FUNCTION YTEST_ABAPREACH_FM3\n  IMPORTING\n    VALUE(IV_IN) TYPE I\n  \
         EXPORTING\n    VALUE(EV_OUT) TYPE I.\n\n\n\n  ev_out = iv_in.\nENDFUNCTION.\n"
    );
}

#[test]
fn a_supplied_group_that_the_server_resolves_differently_stops_without_the_source_read() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("fm.abap");
    let mut transport = read_fixture(&[SEARCH_URI]);
    let target = ReadTarget::new(ObjectKind::Func, "YTEST_ABAPREACH_FM3").group(Some("ZOTHER"));

    let error =
        ReadProg::run_target_classified(&mut transport, target, &output, false, None).unwrap_err();

    assert!(error.stop, "{error:?}");
    assert!(error.message.contains("--fugr ZOTHER"), "{}", error.message);
    assert!(
        error.message.contains("YTEST_ABAPREACH_FG3"),
        "{}",
        error.message
    );
    assert_eq!(transport.calls.len(), 1);
    assert!(!output.exists());
}

#[test]
fn a_supplied_group_that_matches_the_resolution_is_accepted_case_insensitively() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("fm.abap");
    let mut transport = read_fixture(FM_READ_URIS);
    let target =
        ReadTarget::new(ObjectKind::Func, "YTEST_ABAPREACH_FM3").group(Some("ytest_abapreach_fg3"));

    ReadProg::run_target(&mut transport, target, &output, false, None).unwrap();

    assert_eq!(transport.calls[1].uri, FM_SOURCE_URI);
    assert_eq!(fs::read_to_string(output).unwrap().lines().count(), 10);
}

#[test]
fn a_search_without_a_reference_for_the_module_fails_before_the_source_read() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("fm.abap");
    let mut transport = common::ScriptedTransport::replies(vec![Ok(Response::text(
        200,
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><adtcore:objectReferences \
         xmlns:adtcore=\"http://www.sap.com/adt/core\"/>",
    ))]);

    let error = ReadProg::run_target(
        &mut transport,
        ReadTarget::new(ObjectKind::Func, "ZFM"),
        &output,
        false,
        None,
    )
    .unwrap_err();

    assert!(
        error.contains("cannot resolve the function group of ZFM"),
        "{error}"
    );
    assert_eq!(transport.calls.len(), 1);
    assert!(!output.exists());
}

#[test]
fn a_class_main_and_a_local_include_each_read_their_own_source_uri() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("clas.abap");
    let mut transport = read_fixture(&[CLAS_SOURCE_URI]);

    ReadProg::run_target(
        &mut transport,
        ReadTarget::new(ObjectKind::Clas, "YTEST_ABAPREACH_CL3"),
        &main,
        false,
        None,
    )
    .unwrap();

    assert_eq!(transport.calls.len(), 1);
    assert_eq!(transport.calls[0].uri, CLAS_SOURCE_URI);
    assert!(fs::read_to_string(&main).unwrap().contains("ENDCLASS."));

    let include = directory.path().join("ccdef.abap");
    let mut transport = read_fixture(&[CCAU_SOURCE_URI]);

    ReadProg::run_target(
        &mut transport,
        ReadTarget::new(ObjectKind::Clas, "YTEST_ABAPREACH_CL3").include(Some(IncludeKind::Ccdef)),
        &include,
        false,
        None,
    )
    .unwrap();

    assert_eq!(transport.calls[0].uri, CCAU_SOURCE_URI);
    assert_eq!(
        fs::read_to_string(&include).unwrap(),
        "*\"* use this source file for any type of declarations\n\
         *\"* the class needs in its definition part\n"
    );
}

#[test]
fn an_interface_reads_its_main_source() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("intf.abap");
    let mut transport = read_fixture(&[INTF_SOURCE_URI]);

    let outcome = ReadProg::run_target(
        &mut transport,
        ReadTarget::new(ObjectKind::Intf, "YTEST_ABAPREACH_IF3"),
        &output,
        false,
        None,
    )
    .unwrap();

    assert_eq!(transport.calls[0].uri, INTF_SOURCE_URI);
    assert!(outcome.stdout.contains("4 lines ->"), "{}", outcome.stdout);
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        "interface YTEST_ABAPREACH_IF3\n  public .\n\nendinterface.\n"
    );
}

#[test]
fn the_empty_source_of_a_function_module_hashes_like_no_source() {
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty.abap");
    let mut transport = read_fixture(&[FM4_SEARCH_URI, EMPTY_FM_SOURCE_URI]);

    let outcome = ReadProg::run_target(
        &mut transport,
        ReadTarget::new(ObjectKind::Func, "YTEST_ABAPREACH_FM4").group(Some("YTEST_ABAPREACH_FG4")),
        &empty,
        false,
        None,
    )
    .unwrap();

    assert_eq!(
        outcome.stdout,
        format!("0 lines -> {}\nhash {EMPTY_HASH}", empty.display())
    );
    assert_eq!(fs::read(&empty).unwrap(), b"");
}

/// 7.40 returns one NUL character for an empty source: the same canonical empty source.
#[test]
fn a_lone_nul_source_normalizes_like_an_empty_body() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("nul.abap");
    let mut transport = read_fixture(&[NUL_SEARCH_URI, NUL_SOURCE_URI]);

    let outcome = ReadProg::run_target(
        &mut transport,
        ReadTarget::new(ObjectKind::Func, "ZFM740").group(Some("ZFG740")),
        &output,
        false,
        None,
    )
    .unwrap();

    assert_eq!(
        outcome.stdout,
        format!("0 lines -> {}\nhash {EMPTY_HASH}", output.display())
    );
    assert_eq!(fs::read(&output).unwrap(), b"");
    assert_eq!(transport.calls[1].uri, NUL_SOURCE_URI);
}

#[test]
fn a_fresh_class_answers_500_for_its_source_and_is_not_absence() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("clas.abap");
    let mut transport = read_fixture(&[FRESH_CLAS_SOURCE_URI, FRESH_CLAS_STRUCTURE_URI]);

    let error = ReadProg::run_target_classified(
        &mut transport,
        ReadTarget::new(ObjectKind::Clas, "YTEST_ABAPREACH_CL4"),
        &output,
        false,
        None,
    )
    .unwrap_err();

    assert!(!error.stop, "{error:?}");
    assert_eq!(error.failure, Failure::Validation);
    assert!(error.message.contains("HTTP 500"), "{}", error.message);
    assert!(
        error.message.contains("no readable source yet"),
        "{}",
        error.message
    );
    assert_eq!(transport.calls.len(), 2);
    assert_eq!(transport.calls[0].uri, FRESH_CLAS_SOURCE_URI);
    assert_eq!(transport.calls[1].uri, FRESH_CLAS_STRUCTURE_URI);
    assert!(!output.exists());
}

#[test]
fn a_fresh_class_whose_structure_is_also_missing_is_reported_as_not_found() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("clas.abap");
    let mut transport = common::ScriptedTransport::replies(vec![
        Ok(Response::empty(500)),
        Ok(Response::empty(404)),
    ]);

    let error = ReadProg::run_target_classified(
        &mut transport,
        ReadTarget::new(ObjectKind::Clas, "ZCL_GONE"),
        &output,
        false,
        None,
    )
    .unwrap_err();

    assert!(
        error.message.contains("object not found"),
        "{}",
        error.message
    );
    assert_eq!(
        transport.calls[1].uri,
        "/sap/bc/adt/oo/classes/zcl_gone/objectstructure?version=active&withShortDescriptions=true"
    );
}

// ---- --lines: the projection after reading and hashing the whole source ---------------

#[test]
fn the_first_middle_and_last_line_intervals_are_inclusive() {
    let lines = (1..=10).map(|n| format!("L{n}")).collect::<Vec<_>>();
    let slice = |start, end| project_lines(&lines, Some(LineRange { start, end }));

    assert_eq!(slice(1, 1), ["L1"]);
    assert_eq!(slice(1, 3), ["L1", "L2", "L3"]);
    assert_eq!(slice(4, 6), ["L4", "L5", "L6"]);
    assert_eq!(slice(10, 10), ["L10"]);
    assert_eq!(slice(9, 10), ["L9", "L10"]);
}

#[test]
fn an_interval_past_the_end_returns_the_remainder_and_an_empty_slice_past_it() {
    let lines = (1..=3).map(|n| format!("L{n}")).collect::<Vec<_>>();

    assert_eq!(
        project_lines(&lines, Some(LineRange { start: 2, end: 99 })),
        ["L2", "L3"]
    );
    assert_eq!(
        project_lines(&lines, Some(LineRange { start: 4, end: 9 })),
        Vec::<String>::new()
    );
    assert_eq!(
        project_lines(&[], Some(LineRange { start: 1, end: 1 })),
        Vec::<String>::new()
    );
    assert_eq!(project_lines(&lines, None), lines);
}

#[test]
fn a_projection_prints_the_partial_source_and_hashes_the_whole_one() {
    let directory = tempfile::tempdir().unwrap();
    let mut transport = read_fixture(FM_READ_URIS);
    let target = ReadTarget::new(ObjectKind::Func, "YTEST_ABAPREACH_FM3")
        .lines(Some(LineRange { start: 1, end: 2 }));

    let outcome = ReadProg::run_target(&mut transport, target, stdout(), false, None).unwrap();

    assert_eq!(
        outcome.stdout,
        format!(
            "FUNCTION YTEST_ABAPREACH_FM3\n  IMPORTING\n2 lines -> - (lines 1-2)\nhash {FM_HASH}"
        )
    );
    assert_eq!(transport.calls.len(), 2);
    assert_eq!(
        fs::read_dir(directory.path()).unwrap().count(),
        0,
        "a projection never writes a file"
    );
}

#[test]
fn an_out_of_range_projection_prints_no_source_line_and_still_reports_the_full_hash() {
    let mut transport = read_fixture(FM_READ_URIS);
    let target = ReadTarget::new(ObjectKind::Func, "YTEST_ABAPREACH_FM3")
        .lines(Some(LineRange { start: 50, end: 60 }));

    let outcome = ReadProg::run_target(&mut transport, target, stdout(), false, None).unwrap();

    assert_eq!(
        outcome.stdout,
        format!("0 lines -> - (lines 50-60)\nhash {FM_HASH}")
    );
    assert!(!outcome.stdout.contains("FUNCTION"));
}

#[test]
fn a_projection_to_a_file_path_is_refused_before_the_transport_request() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("partial.abap");
    let mut transport = common::ScriptedTransport::replies(Vec::new());
    let target =
        ReadTarget::new(ObjectKind::Prog, "ZP").lines(Some(LineRange { start: 1, end: 2 }));

    let error =
        ReadProg::run_target_classified(&mut transport, target, &output, false, None).unwrap_err();

    assert!(error.message.contains("stdout"), "{}", error.message);
    assert!(transport.calls.is_empty());
    assert!(!output.exists());
}

fn stdout() -> &'static std::path::Path {
    std::path::Path::new("-")
}

#[test]
fn a_refused_structure_probe_is_the_authorization_failure() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("clas.abap");
    let mut transport = common::ScriptedTransport::replies(vec![
        Ok(Response::empty(500)),
        Ok(Response::empty(401)),
    ]);

    let error = ReadProg::run_target_classified(
        &mut transport,
        ReadTarget::new(ObjectKind::Clas, "ZCL_X"),
        &output,
        false,
        None,
    )
    .unwrap_err();

    assert_eq!(error.failure, Failure::Authorization);
    assert_eq!(error.message, "authorization failed: HTTP 401");
    assert!(!output.exists());
}

#[test]
fn a_failed_group_search_is_a_read_failure_not_a_stop() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("fm.abap");
    let mut transport = common::ScriptedTransport::replies(vec![Ok(Response::empty(500))]);

    let error = ReadProg::run_target_classified(
        &mut transport,
        ReadTarget::new(ObjectKind::Func, "ZFM"),
        &output,
        false,
        None,
    )
    .unwrap_err();

    assert!(!error.stop, "{error:?}");
    assert_eq!(error.failure, Failure::Validation);
    assert!(
        error
            .message
            .contains("cannot resolve the function group of ZFM"),
        "{}",
        error.message
    );
    assert!(error.message.contains("HTTP 500"), "{}", error.message);
    assert_eq!(transport.calls.len(), 1);
}

#[test]
fn a_failed_source_read_after_a_pre_send_failure_stays_a_fallback_cause() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("fm.abap");
    let mut transport = common::ScriptedTransport::replies(vec![Err(
        TransportError::PreSendConnectivity("no route".to_owned()),
    )]);

    let error = ReadProg::run_target_classified(
        &mut transport,
        ReadTarget::new(ObjectKind::Func, "ZFM"),
        &output,
        false,
        None,
    )
    .unwrap_err();

    assert_eq!(error.failure, Failure::PreSendConnectivity);
    assert_eq!(error.message, "pre-send connectivity failure: no route");
    assert!(!output.exists());
}

#[test]
fn a_non_class_kind_reports_its_http_status_with_the_kind_and_name() {
    let message = ReadProg::run_target(
        &mut common::ScriptedTransport::replies(vec![Ok(Response::empty(404))]),
        ReadTarget::new(ObjectKind::Intf, "ZIF_X"),
        stdout(),
        true,
        None,
    )
    .unwrap_err();

    assert!(
        message.starts_with("read INTF ZIF_X: HTTP 404"),
        "{message}"
    );

    let error = ReadProg::run_target_classified(
        &mut common::ScriptedTransport::replies(vec![Ok(Response::empty(404))]),
        ReadTarget::new(ObjectKind::Intf, "ZIF_X"),
        stdout(),
        true,
        None,
    )
    .unwrap_err();

    assert_eq!(error.failure, Failure::Validation);
}
