//! The request boundary of `ping`: exactly `GET /sap/bc/adt/system/information` with no query, no
//! body and the `Accept` the command sends. The RFC `ping` itself uses the typed
//! `RFC_SYSTEM_INFO` call, so this shape is only for a request that arrives through the SADT
//! endpoint.

use abapreach::operation::ping::{SYSTEM_INFORMATION_URI, request};
use abapreach::transport::{
    Request, RunnerArm, SadtTransport, TransportError,
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

fn get(uri: &str, accept: Option<&str>) -> Request {
    let mut request = Request::get(uri);

    if let Some(accept) = accept {
        request
            .headers
            .insert("Accept".to_owned(), accept.to_owned());
    }

    request
}

#[test]
fn the_request_the_command_builds_is_admitted() {
    let request = request();

    assert_eq!(request.method, "GET");
    assert_eq!(request.uri, "/sap/bc/adt/system/information");
    assert!(request.body.is_empty());
    assert!(admits(&request));
}

#[test]
fn a_query_string_is_refused() {
    for suffix in [
        "?",
        "?a=1",
        "?sap-client=100",
        "?version=active",
        "?%00",
        "#",
        "#x",
        "?#",
    ] {
        let uri = format!("{SYSTEM_INFORMATION_URI}{suffix}");

        assert!(
            !admits(&get(&uri, Some("application/atom+xml;type=feed"))),
            "{uri}"
        );
    }
}

#[test]
fn another_method_is_refused() {
    for method in ["POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "get"] {
        let mut sent = request();

        sent.method = method.to_owned();

        assert!(!admits(&sent), "{method}");
    }
}

#[test]
fn a_path_variant_is_refused() {
    for uri in [
        "/sap/bc/adt/system/information/",
        "/sap/bc/adt/system/information//",
        "/sap/bc/adt/system/informationx",
        "/sap/bc/adt/system/information.xml",
        "/sap/bc/adt/system/information;a=b",
        "/sap/bc/adt/system/information%2F",
        "/sap/bc/adt/system/information%00",
        "/sap/bc/adt/system/Information",
        "/SAP/BC/ADT/SYSTEM/INFORMATION",
        "/sap/bc/adt/system",
        "/sap/bc/adt/system/",
        "/sap/bc/adt/system/info",
        "/sap/bc/adt/system/users",
        "/sap/bc/adt/system/information/users",
        "/sap/bc/adt//system/information",
        "/sap/bc/adt/./system/information",
        "/sap/bc/adt/x/../system/information",
        "/sap/bc/adt/system/x/../information",
        "//sap/bc/adt/system/information",
        "/sap/bc/adt/%73ystem/information",
        "/sap/bc/adt/system/%69nformation",
        "sap/bc/adt/system/information",
        "http://host/sap/bc/adt/system/information",
        "/sap/bc/adt/system/information ",
        " /sap/bc/adt/system/information",
        "",
    ] {
        assert!(
            !admits(&get(uri, Some("application/atom+xml;type=feed"))),
            "{uri:?}"
        );
    }
}

#[test]
fn a_body_is_refused() {
    let mut sent = request();

    sent.body = b"x".to_vec();

    assert!(!admits(&sent));
}

#[test]
fn another_accept_or_none_is_refused() {
    for accept in [
        None,
        Some(""),
        Some("*/*"),
        Some("application/atom+xml"),
        Some("application/*"),
        Some("application/atom+xml;type=feed "),
        Some("application/atom+xml; type=feed"),
        Some("APPLICATION/ATOM+XML;TYPE=FEED"),
        Some("application/atom+xml;type=entry"),
        Some("text/plain"),
        Some("application/atom+xml;type=feed, text/plain"),
    ] {
        assert!(!admits(&get(SYSTEM_INFORMATION_URI, accept)), "{accept:?}");
    }
}

/// The feed is metadata only, so a running runner leaves it admitted; the runner arm still refuses
/// what it is there for (the freestyle preview), which the runner admission tests cover.
#[test]
fn an_armed_runner_does_not_change_the_verdict() {
    let _arm = RunnerArm::arm("YCL_ABAPREACH_RUN_AB12CD").unwrap();

    assert!(admits(&request()));
    assert!(!admits(&get(
        &format!("{SYSTEM_INFORMATION_URI}?a=1"),
        Some("application/atom+xml;type=feed")
    )));
}

/// Two headers that differ only in case are one header to SAP: the global check refuses the
/// request even when one of them is the `Accept` the command sends.
#[test]
fn duplicate_accept_headers_that_differ_in_case_are_refused() {
    for (first, second) in [
        ("Accept", "accept"),
        ("Accept", "ACCEPT"),
        ("accept", "Accept"),
    ] {
        let mut sent = request();

        sent.headers.clear();
        sent.headers.insert(
            first.to_owned(),
            "application/atom+xml;type=feed".to_owned(),
        );
        sent.headers.insert(second.to_owned(), "*/*".to_owned());

        assert!(!admits(&sent), "{first} / {second}");
    }
}
