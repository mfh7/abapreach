//! The RFC/SADT request boundary for the quality commands: the inactive-objects list with its
//! user filter, activation of the accepted types, and the ABAP Unit run configuration.

use abapreach::operation::aunit::{AunitKind, AunitTarget, run_configuration, run_request};
use abapreach::transport::{
    Request, SadtTransport, TransportError,
    rfc::{FakeRfcApi, RfcTransport},
};

fn admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

fn run_with(body: &str) -> Request {
    let mut request = Request::post("/sap/bc/adt/abapunit/testruns", body.as_bytes().to_vec());

    request
        .headers
        .insert("Content-Type".to_owned(), "application/xml".to_owned());
    request
}

fn configuration(uri: &str) -> String {
    run_configuration(uri)
}

fn activation(uri: &str) -> Request {
    let mut request = Request::post(
        "/sap/bc/adt/activation?method=activate&preauditRequested=true",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="{uri}" adtcore:name="X"/></adtcore:objectReferences>"#
        )
        .into_bytes(),
    );

    request
        .headers
        .insert("Content-Type".to_owned(), "application/xml".to_owned());
    request
}

#[test]
fn the_inactive_list_is_admitted_plain_and_with_a_user_filter() {
    for uri in [
        "/sap/bc/adt/activation/inactiveobjects",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=*",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=DEVUSER",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=DEV%24USER",
    ] {
        assert!(admits(&Request::get(uri)), "{uri}");
    }
}

#[test]
fn any_other_shape_of_the_inactive_list_is_refused() {
    for uri in [
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=",
        "/sap/bc/adt/activation/inactiveobjects?username=DEVUSER",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=A&x=1",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=A%20B%",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=a b",
        "/sap/bc/adt/activation/inactiveobjects?x=1",
        "/sap/bc/adt/activation/inactiveobjects/x",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=A#f",
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=**",
    ] {
        assert!(!admits(&Request::get(uri)), "{uri}");
    }

    assert!(!admits(&Request::post(
        "/sap/bc/adt/activation/inactiveobjects",
        Vec::new()
    )));
}

#[test]
fn a_function_module_is_activated_and_a_function_group_never() {
    assert!(admits(&activation(
        "/sap/bc/adt/functions/groups/zfg/fmodules/zfm"
    )));
    assert!(!admits(&activation("/sap/bc/adt/functions/groups/zfg")));
    assert!(admits(&activation("/sap/bc/adt/oo/interfaces/zif_a")));
    assert!(admits(&activation(
        "/sap/bc/adt/programs/includes/zinc?context=%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzmain"
    )));
    assert!(!admits(&activation("/sap/bc/adt/programs/includes/zinc")));
}

#[test]
fn the_run_of_a_program_a_class_and_a_function_group_is_admitted() {
    for (kind, name) in [
        (AunitKind::Prog, "ZPROG"),
        (AunitKind::Clas, "ZCL_A"),
        (AunitKind::Fugr, "ZFG"),
    ] {
        let target = AunitTarget::new(kind, name).unwrap();

        assert!(admits(&run_request(&target)), "{name}");
    }
}

#[test]
fn a_run_needs_the_exact_content_type_and_no_query() {
    let body = configuration("/sap/bc/adt/programs/programs/zprog");
    let mut other_type = run_with(&body);

    other_type
        .headers
        .insert("Content-Type".to_owned(), "application/*".to_owned());

    let mut no_type = run_with(&body);

    no_type.headers.clear();

    let mut query = run_with(&body);

    query.uri.push_str("?x=1");

    let mut get = run_with(&body);

    get.method = "GET".to_owned();
    get.body.clear();

    assert!(admits(&run_with(&body)));
    assert!(!admits(&other_type));
    assert!(!admits(&no_type));
    assert!(!admits(&query));
    assert!(!admits(&get));
}

#[test]
fn a_run_configuration_with_more_than_harmless_tests_is_refused() {
    let good = configuration("/sap/bc/adt/programs/programs/zprog");

    for (from, to) in [
        (r#"critical="false""#, r#"critical="true""#),
        (r#"dangerous="false""#, r#"dangerous="true""#),
        (r#"harmless="true""#, r#"harmless="false""#),
        (
            r#"<coverage active="false"/>"#,
            r#"<coverage active="true"/>"#,
        ),
        (r#"<coverage active="false"/>"#, ""),
    ] {
        let body = good.replace(from, to);

        // Dropping the coverage element is harmless; the others change what may run.
        let expected = from == r#"<coverage active="false"/>"# && to.is_empty();

        assert_eq!(admits(&run_with(&body)), expected, "{from} -> {to}");
    }
}

#[test]
fn a_run_configuration_names_exactly_one_testable_object() {
    let good = configuration("/sap/bc/adt/programs/programs/zprog");
    let reference =
        r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/programs/programs/zprog"/>"#;

    // Two objects.
    assert!(!admits(&run_with(&good.replace(
        reference,
        &format!(
            r#"{reference}<adtcore:objectReference adtcore:uri="/sap/bc/adt/oo/classes/zcl_a"/>"#
        )
    ))));
    // No object.
    assert!(!admits(&run_with(&good.replace(reference, ""))));

    for uri in [
        "/sap/bc/adt/programs/includes/zinc",
        "/sap/bc/adt/oo/interfaces/zif_a",
        "/sap/bc/adt/functions/groups/zfg/fmodules/zfm",
        "/sap/bc/adt/oo/classes/zcl_a/includes/testclasses",
        "/sap/bc/adt/oo/classes/zcl_a?x=1",
        "/sap/bc/adt/oo/classes/zcl_a#type=x",
        "/sap/bc/adt/oo/classes/../programs/programs/zprog",
        "/sap/bc/adt/oo/classes/zcl_a%3Fx",
        "/sap/bc/adt/repository/nodestructure",
        "",
    ] {
        assert!(
            !admits(&run_with(&configuration(uri))),
            "must be refused: {uri}"
        );
    }
}

#[test]
fn a_run_configuration_with_anything_else_in_it_is_refused() {
    let good = configuration("/sap/bc/adt/oo/classes/zcl_a");
    let cases = [
        // Another root.
        good.replace("aunit:runConfiguration", "aunit:other"),
        // An element the command never sends.
        good.replace("<external>", "<external><extra/>"),
        // Text in the document.
        good.replace("<external>", "<external>payload"),
        // A doctype.
        format!("<!DOCTYPE x [<!ENTITY a \"b\">]>{good}"),
        // Not XML at all.
        "not xml".to_owned(),
        String::new(),
    ];

    for body in cases {
        assert!(!admits(&run_with(&body)), "must be refused: {body}");
    }
}

#[test]
fn a_run_configuration_that_changes_the_test_selection_is_refused() {
    let good = configuration("/sap/bc/adt/oo/classes/zcl_a");

    assert!(admits(&run_with(&good)));

    let cases = [
        // Tests assigned to the object from other programs would run too.
        ("assignedTests=\"false\"", "assignedTests=\"true\""),
        ("sameProgram=\"true\"", "sameProgram=\"false\""),
        (
            "appendAssignedTestsPreview=\"true\"",
            "appendAssignedTestsPreview=\"false\"",
        ),
        // Another object set kind changes the selection.
        (
            "<objectSet kind=\"inclusive\">",
            "<objectSet kind=\"exclusive\">",
        ),
        ("<objectSet kind=\"inclusive\">", "<objectSet>"),
        (
            "<objectSet kind=\"inclusive\">",
            "<objectSet kind=\"inclusive\" extra=\"x\">",
        ),
        // A duration or the URI type left out or changed.
        ("long=\"true\"", "long=\"false\""),
        (
            "<uriType value=\"semantic\"/>",
            "<uriType value=\"other\"/>",
        ),
        // An attribute the command never sends.
        (
            "assignedTests=\"false\"",
            "assignedTests=\"false\" extra=\"x\"",
        ),
    ];

    for (from, to) in cases {
        let body = good.replace(from, to);

        assert_ne!(body, good, "{from}");
        assert!(!admits(&run_with(&body)), "must be refused: {to}");
    }
}

#[test]
fn the_previews_of_a_batch_and_of_the_client_check_are_admitted() {
    use abapreach::operation::read_table::preview_request;

    for (entity, body) in [
        ("T000", "SELECT CCCATEGORY FROM T000 WHERE MANDT = '100'"),
        (
            "TADIR",
            "SELECT OBJECT, OBJ_NAME FROM TADIR WHERE PGMID = 'R3TR' AND DEVCLASS = 'ZPKG' AND OBJECT IN ( 'PROG', 'CLAS', 'FUGR' ) AND DELFLAG = ' '",
        ),
        (
            "TRDIR",
            "SELECT NAME, SUBC FROM TRDIR WHERE NAME IN ( 'ZTEST_INC', 'ZTEST_MAIN' )",
        ),
        (
            "E070",
            "SELECT TRKORR FROM E070 WHERE TRKORR = 'DEVK900001' OR STRKORR = 'DEVK900001'",
        ),
        (
            "E071",
            "SELECT PGMID, OBJECT, OBJ_NAME FROM E071 WHERE TRKORR IN ( 'DEVK900001', 'DEVK900002' )",
        ),
    ] {
        let request = preview_request(entity, body, 100).unwrap();

        assert!(admits(&request), "{entity}");
    }
}
