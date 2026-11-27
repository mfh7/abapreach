//! The request boundary reads each header by its exact name, so two headers that differ only in
//! case would let one value pass the check while SAP reads the other. A request that carries two
//! such names is refused in every shape; the discovery is read as `doctor` and `where-used` send
//! it, with no body and no header.

use abapreach::adt::{USAGE_REQUEST_XML, usage_references_uri};
use abapreach::operation::read_table::preview_request;
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

fn with(mut request: Request, headers: &[(&str, &str)]) -> Request {
    for (name, value) in headers {
        request
            .headers
            .insert((*name).to_owned(), (*value).to_owned());
    }

    request
}

/// One admitted request of each family of shapes.
fn admitted() -> Vec<(&'static str, Request)> {
    let object = "/sap/bc/adt/programs/programs/zprog";

    vec![
        (
            "search",
            Request::get(
                "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch&query=Z*&maxResults=5",
            ),
        ),
        (
            "inactive",
            Request::get("/sap/bc/adt/activation/inactiveobjects"),
        ),
        (
            "source read",
            Request::get(format!("{object}/source/main?version=active")),
        ),
        (
            "lock",
            Request::post(
                format!("{object}?_action=LOCK&accessMode=MODIFY"),
                Vec::new(),
            ),
        ),
        (
            "source write",
            with(
                Request::put(
                    format!("{object}/source/main?lockHandle=abc"),
                    b"REPORT zprog.".to_vec(),
                ),
                &[("Content-Type", "text/plain; charset=utf-8")],
            ),
        ),
        (
            "preview",
            preview_request("T001", "SELECT BUKRS FROM T001", 5).unwrap(),
        ),
        (
            "freestyle",
            with(
                Request::post(
                    "/sap/bc/adt/datapreview/freestyle?rowNumber=1",
                    b"SELECT * FROM T001".to_vec(),
                ),
                &[("Content-Type", "text/plain"), ("Accept", "application/*")],
            ),
        ),
        (
            "usage",
            with(
                Request::post(
                    usage_references_uri(object),
                    USAGE_REQUEST_XML.as_bytes().to_vec(),
                ),
                &[
                    ("Content-Type", "application/*"),
                    ("Accept", "application/*"),
                ],
            ),
        ),
    ]
}

#[test]
fn the_requests_of_every_family_are_admitted_as_they_are() {
    for (name, request) in admitted() {
        assert!(admits(&request), "{name}: {request:?}");
    }
}

#[test]
fn a_header_named_twice_in_different_case_is_refused_in_every_shape() {
    for (name, request) in admitted() {
        for extra in [
            vec![("accept", "text/plain")],
            vec![("ACCEPT", "application/*")],
            vec![("content-type", "text/html")],
            vec![("CONTENT-TYPE", "text/plain")],
            vec![("X-Test", "1"), ("x-test", "2")],
            vec![("X-Test", "1"), ("X-TEST", "1")],
        ] {
            let mut ambiguous = with(request.clone(), &extra);
            let names: Vec<String> = ambiguous
                .headers
                .keys()
                .map(|key| key.to_ascii_lowercase())
                .collect();
            let unique: std::collections::HashSet<&String> = names.iter().collect();

            // The case that adds a name the request does not carry is not a pair.
            if unique.len() == names.len() {
                ambiguous = with(ambiguous, &[("x-pair", "1"), ("X-Pair", "2")]);
            }

            assert!(!admits(&ambiguous), "{name}: {extra:?}");
        }
    }
}

#[test]
fn the_discovery_is_admitted_with_no_body_and_no_header_only() {
    let discovery = || Request::get("/sap/bc/adt/discovery");

    assert!(admits(&discovery()));

    let mut with_body = discovery();

    with_body.body = b"x".to_vec();

    assert!(!admits(&with_body));

    for headers in [
        vec![("Accept", "application/*")],
        vec![("Accept", "*/*")],
        vec![("accept", "application/xml")],
        vec![("Content-Type", "application/*")],
        vec![("X-Other", "1")],
        vec![
            ("Accept", "application/*"),
            ("Content-Type", "application/*"),
        ],
        vec![("Accept", "application/*"), ("accept", "text/plain")],
    ] {
        assert!(!admits(&with(discovery(), &headers)), "{headers:?}");
    }

    // The path stays exact, and so does the method.
    for request in [
        Request::get("/sap/bc/adt/discovery?x=1"),
        Request::get("/sap/bc/adt/discovery/"),
        Request::get("/sap/bc/adt/discovery#x"),
        Request::post("/sap/bc/adt/discovery", Vec::new()),
    ] {
        assert!(!admits(&request), "{request:?}");
    }

    // The inactive worklist, which shared the discovery rule, is unchanged.
    assert!(admits(&Request::get(
        "/sap/bc/adt/activation/inactiveobjects"
    )));
}
