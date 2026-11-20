//! The request boundary of `run CLAS`: the classrun POST of one named, existing class. The same
//! cases run on both transports, RFC-SADT through its admission and ADT HTTP through the gate.

use abapreach::operation::runner::RunnerGated;
use abapreach::transport::{
    Request, Response, RunnerArm, SadtTransport, TransportError,
    http::HttpTransport,
    rfc::{FakeRfcApi, RfcTransport},
};
use std::time::Duration;

const CLASSRUN: &str = "/sap/bc/adt/oo/classrun";

fn rfc_admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

fn http_admits(request: &Request) -> bool {
    let mut http = HttpTransport::for_test(vec![Ok(Response::empty(200))]);
    let mut gated = RunnerGated::new(&mut http);

    match gated.request_within(request.clone(), Duration::from_secs(600)) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

#[track_caller]
fn assert_both(name: Option<&str>, request: &Request, expected: bool) {
    for (transport, admits) in [
        ("rfc", rfc_admits as fn(&Request) -> bool),
        ("http", http_admits),
    ] {
        let _arm = name.map(|name| RunnerArm::arm_class(name).unwrap());

        assert_eq!(
            admits(request),
            expected,
            "{transport} armed with {name:?}: {request:?}"
        );
    }
}

fn classrun(name: &str) -> Request {
    let mut request = Request::post(format!("{CLASSRUN}/{name}"), Vec::new());

    request
        .headers
        .insert("Accept".to_owned(), "text/plain".to_owned());

    request
}

#[test]
fn the_classrun_post_of_the_armed_class_is_admitted_on_both_transports() {
    assert_both(Some("ZCL_A"), &classrun("ZCL_A"), true);
    // A standard class is a class like any other once the command armed it.
    assert_both(Some("CL_DEMO_OUTPUT"), &classrun("CL_DEMO_OUTPUT"), true);
}

#[test]
fn an_unarmed_classrun_post_or_another_class_is_refused() {
    assert_both(None, &classrun("ZCL_A"), false);
    assert_both(Some("ZCL_A"), &classrun("ZCL_B"), false);
    assert_both(Some("ZCL_A"), &classrun("zcl_a"), false);
    assert_both(Some("ZCL_A"), &classrun("ZCL_A2"), false);
    assert_both(Some("ZCL_A"), &classrun("ZCL_A/x"), false);
}

#[test]
fn the_shape_of_the_post_is_exact() {
    let mut with_body = classrun("ZCL_A");
    let mut with_query = classrun("ZCL_A");
    let mut wrong_accept = classrun("ZCL_A");
    let mut no_accept = classrun("ZCL_A");

    with_body.body = b"x".to_vec();
    with_query.uri.push_str("?x=1");
    wrong_accept
        .headers
        .insert("Accept".to_owned(), "application/xml".to_owned());
    no_accept.headers.clear();

    for refused in [with_body, with_query, wrong_accept, no_accept] {
        assert_both(Some("ZCL_A"), &refused, false);
    }

    let mut get = classrun("ZCL_A");

    get.method = "GET".to_owned();
    assert_both(Some("ZCL_A"), &get, false);
}

#[test]
fn only_one_post_is_admitted_per_arm() {
    for admits in [rfc_admits as fn(&Request) -> bool, http_admits] {
        let _arm = RunnerArm::arm_class("ZCL_A").unwrap();

        // The spent arm is shared by both checks: the first call spends it.
        let first = admits(&classrun("ZCL_A"));
        let second = admits(&classrun("ZCL_A"));

        assert!(first);
        assert!(!second);
    }
}

#[test]
fn a_name_that_is_not_a_plain_class_name_cannot_be_armed() {
    for name in [
        "",
        "zcl_a",
        "/NS/CL_A",
        "1ZCL",
        "_ZCL",
        "ZCL A",
        "ZCL/A",
        "ZCL%2FA",
        &"Z".repeat(31),
    ] {
        assert!(RunnerArm::arm_class(name).is_err(), "{name:?}");
    }

    assert!(RunnerArm::arm_class(&"Z".repeat(30)).is_ok());
}

#[test]
fn the_arm_of_a_class_is_exclusive_with_the_arm_of_a_runner() {
    let _class = RunnerArm::arm_class("ZCL_A").unwrap();

    assert!(RunnerArm::arm("YCL_ABAPREACH_RUN_AB12CD34").is_err());
    assert!(RunnerArm::arm_class("ZCL_B").is_err());
}

#[test]
fn a_standard_class_cannot_arm_as_a_runner() {
    assert!(RunnerArm::arm("CL_DEMO_OUTPUT").is_err());
}
