use abapreach::adt::prog_uri;
use abapreach::hash::validate_prog;
use abapreach::operation::deploy::{activation_request_xml, checkrun_xml};

fn source() -> Vec<String> {
    vec!["REPORT ztest.".to_owned()]
}

#[test]
fn names_outside_the_program_grammar_are_rejected() {
    let rejected = [
        "",
        "Z?PROG",
        "Z#PROG",
        "Z%PROG",
        "Z&PROG",
        "Z<PROG",
        "Z\"PROG",
        "Z PROG",
        "/ABC/",
        "/ZPROG",
        "ZPROG/",
        "/AB-C/ZPROG",
        "/ABC/ZPROG/X",
    ];

    for name in rejected {
        assert!(
            validate_prog(name, &source()).is_err(),
            "name not rejected: {name:?}"
        );
    }

    assert!(
        validate_prog(&"X".repeat(41), &source()).is_err(),
        "41 characters not rejected"
    );
    assert!(
        validate_prog(&format!("/{}/ZPROG", "N".repeat(36)), &source()).is_err(),
        "namespace name > 40 characters not rejected"
    );
}

#[test]
fn program_and_namespace_names_are_accepted() {
    for name in [
        "Z",
        "ZTEST_01",
        "Z_A1B2C3",
        "ytest_abapreach01",
        &"X".repeat(40),
        "/ABC/ZPROG",
        "/abc/zprog",
        "/ABC_01/ZTEST_01",
    ] {
        assert!(
            validate_prog(name, &source()).is_ok(),
            "name rejected: {name:?}"
        );
    }
}

#[test]
fn uri_lowercases_the_name_and_percent_encodes_the_namespace_slashes() {
    assert_eq!(
        prog_uri("YTEST_ABAPREACH01"),
        "/sap/bc/adt/programs/programs/ytest_abapreach01"
    );
    assert_eq!(
        prog_uri("/ABC/ZPROG"),
        "/sap/bc/adt/programs/programs/%2fabc%2fzprog"
    );
    assert_eq!(
        prog_uri("/abc/zprog"),
        "/sap/bc/adt/programs/programs/%2fabc%2fzprog"
    );
}

#[test]
fn xml_values_are_escaped() {
    let checkrun = checkrun_xml("Z<A&B\"", &["REPORT ztest.".to_owned()]);

    assert!(
        checkrun.contains("adtcore:uri=\"/sap/bc/adt/programs/programs/z&lt;a&amp;b&quot;\""),
        "{checkrun}"
    );
    assert!(
        checkrun.contains("/sap/bc/adt/programs/programs/z&lt;a&amp;b&quot;/source/main"),
        "{checkrun}"
    );

    let activation = activation_request_xml("Z<A&B\"", "/sap/bc/adt/programs/programs/a&b");

    assert!(
        activation.contains("adtcore:name=\"Z&lt;A&amp;B&quot;\""),
        "{activation}"
    );
    assert!(
        activation.contains("adtcore:uri=\"/sap/bc/adt/programs/programs/a&amp;b\""),
        "{activation}"
    );
}

#[test]
fn a_namespace_name_is_used_encoded_and_escaped_in_the_checkrun() {
    let checkrun = checkrun_xml("/ABC/ZPROG", &["REPORT ztest.".to_owned()]);

    assert!(
        checkrun.contains("adtcore:uri=\"/sap/bc/adt/programs/programs/%2fabc%2fzprog\""),
        "{checkrun}"
    );
    assert!(
        checkrun.contains("/sap/bc/adt/programs/programs/%2fabc%2fzprog/source/main"),
        "{checkrun}"
    );
}
