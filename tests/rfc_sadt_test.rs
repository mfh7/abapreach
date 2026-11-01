mod common;

use abapreach::transport::{
    Request, SadtTransport,
    rfc::{FakeRfcApi, RfcTransport, SADT_REST_RFC_ENDPOINT},
};
use common::{Fixture, fixture_path};
use std::collections::BTreeMap;
use std::fs;

const V1_PUT_URI: &str = concat!(
    "/sap/bc/adt/programs/programs/ytest_abapreach01/source/main",
    "?lockHandle=qdbcyR6xoaD%2BxAqwYue9KQ4XHbM%3D"
);

#[test]
fn reuse() {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    for _ in 0..4 {
        transport
            .request(Request::get("/sap/bc/adt/discovery"))
            .unwrap();
    }

    transport.close().unwrap();

    assert_eq!(transport.api().counts(), (1, 1, 4));
}

#[test]
fn payload() {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    transport
        .request(Request::put(
            "/sap/bc/adt/programs/programs/Z/source/main?lockHandle=a%2Bb%2Fc%3D",
            "é".as_bytes().to_vec(),
        ))
        .unwrap();

    let request = transport.api().last_request();

    assert_eq!(request.function, SADT_REST_RFC_ENDPOINT);
    assert_eq!(
        (request.method.as_str(), request.body.as_slice()),
        ("PUT", "é".as_bytes())
    );
    assert_eq!(request.version, "HTTP/1.1");

    let fixture_text = fs::read_to_string(fixture_path("dev-750-rfc.json")).unwrap();
    let fixture: Fixture = serde_json::from_str(&fixture_text).unwrap();
    let write = fixture
        .exchanges
        .iter()
        .find(|exchange| exchange.request.method == "PUT" && exchange.request.uri == V1_PUT_URI)
        .unwrap();
    let mut fixture_transport = RfcTransport::new(FakeRfcApi::ok());

    fixture_transport
        .request(Request {
            method: write.request.method.clone(),
            uri: write.request.uri.clone(),
            headers: write.request.headers.clone(),
            body: write.request.body.as_bytes().to_vec(),
        })
        .unwrap();

    let fixture_request = fixture_transport.api().last_request();

    assert_eq!(fixture_request.function, "SADT_REST_RFC_ENDPOINT");
    assert_eq!(fixture_request.uri, V1_PUT_URI);
    assert_eq!(
        fixture_request.headers,
        BTreeMap::from([(
            "Content-Type".to_owned(),
            "text/plain; charset=utf-8".to_owned(),
        )])
    );
    assert_eq!(
        fixture_request.body.as_slice(),
        b"REPORT ytest_abapreach01.\n\nWRITE / 'v1'.\n"
    );
}

#[test]
fn deny() {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    assert!(transport.request(Request::delete("/anything")).is_err());
    assert_eq!(transport.api().counts(), (0, 0, 0));
}

#[test]
fn allow_list_accepts_only_the_m1_program_requests() {
    let program = "/sap/bc/adt/programs/programs/z";
    let allowed = [
        Request::get("/sap/bc/adt/discovery"),
        Request::get(format!("{program}/source/main?version=active")),
        Request::get(format!("{program}/source/main?version=inactive")),
        Request::post(
            format!("{program}?_action=LOCK&accessMode=MODIFY"),
            Vec::new(),
        ),
        Request::post(
            format!("{program}?_action=UNLOCK&lockHandle=a%2Bb"),
            Vec::new(),
        ),
        Request::post("/sap/bc/adt/checkruns?reporters=abapCheckRun", Vec::new()),
        Request::post(
            "/sap/bc/adt/activation?method=activate&preauditRequested=true",
            Vec::new(),
        ),
        Request::put(
            format!("{program}/source/main?lockHandle=a%2Bb"),
            Vec::new(),
        ),
        Request::put(
            format!("{program}/source/main?lockHandle=a&corrNr=DEVK000001"),
            Vec::new(),
        ),
    ];
    let denied = [
        Request::get("/sap/bc/adt/repository/nodestructure"),
        Request::get(format!("{program}/source/main?version=workingArea")),
        Request::get("/sap/bc/adt/programs/programs/a/b/source/main?version=active"),
        Request::post(
            format!("{program}?_action=LOCK&accessMode=READ"),
            Vec::new(),
        ),
        Request::post(format!("{program}?_action=UNLOCK&lockHandle="), Vec::new()),
        Request::put(format!("{program}/source/main"), Vec::new()),
        Request::put(
            format!("{program}/source/main?lockHandle=a&corrNr="),
            Vec::new(),
        ),
        Request::delete(format!("{program}?lockHandle=a")),
    ];
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    for request in &allowed {
        transport
            .request(request.clone())
            .unwrap_or_else(|error| panic!("{request:?}: {error}"));
    }
    for request in &denied {
        assert!(transport.request(request.clone()).is_err(), "{request:?}");
    }

    assert_eq!(transport.api().counts(), (1, 0, allowed.len()));
}
