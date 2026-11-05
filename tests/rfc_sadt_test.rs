mod common;

use abapreach::adt::{function_group_search_uri, usage_references_uri};
use abapreach::cli::{CreateKind, IncludeKind, ObjectKind};
use abapreach::object::PackageScope;
use abapreach::operation::deploy::{activation_request_xml, encode_lock_handle};
use abapreach::transport::{
    Request, SadtTransport, TransportError,
    rfc::{FakeRfcApi, RfcEndpointResponse, RfcTransport, SADT_REST_RFC_ENDPOINT},
};
use common::{Fixture, fixture_path, fixture_path_in};
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
        .request(with_content_type(
            Request::put(
                "/sap/bc/adt/programs/programs/Z/source/main?lockHandle=a%2Bb%2Fc%3D",
                "é".as_bytes().to_vec(),
            ),
            "text/plain; charset=utf-8",
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
        Request::get(format!("{program}/source/main/versions")),
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
        Request::get(format!("{program}/versions")),
        Request::get(format!("{program}/versions/99999")),
        Request::get(format!("{program}/source/main/versions/99999")),
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
        Request::delete(format!("{program}/source/main?lockHandle=a")),
    ];
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    for request in &allowed {
        assert!(admits(&mut transport, request), "{request:?}");
    }
    for request in &denied {
        assert!(!admits(&mut transport, request), "{request:?}");
    }

    assert_eq!(transport.api().counts(), (1, 0, allowed.len()));
}

// ---- M3 admissibility boundary -------------------------------------------------------------

const ACTIVATE_FIRST: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";
const ACTIVATE_RESEND: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=false";
const CLASS: &str = "/sap/bc/adt/oo/classes/zcl_a";
const INCLUDES: [IncludeKind; 4] = [
    IncludeKind::Ccimp,
    IncludeKind::Ccdef,
    IncludeKind::Ccmac,
    IncludeKind::Ccau,
];

fn fresh() -> RfcTransport<FakeRfcApi> {
    RfcTransport::new(FakeRfcApi::ok())
}

fn with_activation_response(body: String) -> RfcTransport<FakeRfcApi> {
    RfcTransport::new(FakeRfcApi::with_responses(vec![RfcEndpointResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: body.into_bytes(),
    }]))
}

/// Whether the allow-list lets the request through; any other failure is a test error.
fn admits(transport: &mut RfcTransport<FakeRfcApi>, request: &Request) -> bool {
    let mut request = request.clone();
    if request.headers.is_empty() {
        let content_type = match request.method.as_str() {
            "PUT" => Some("text/plain; charset=utf-8"),
            "POST" if request.uri.starts_with("/sap/bc/adt/activation?") => Some("application/xml"),
            "POST"
                if request.uri == "/sap/bc/adt/checkruns?reporters=abapCheckRun"
                    || request.uri.starts_with("/sap/bc/adt/functions/groups")
                    || request.uri == "/sap/bc/adt/oo/classes"
                    || request.uri.starts_with("/sap/bc/adt/oo/classes?")
                    || request.uri == "/sap/bc/adt/oo/interfaces"
                    || request.uri.starts_with("/sap/bc/adt/oo/interfaces?")
                    || request.uri.starts_with("/sap/bc/adt/oo/classes/") =>
            {
                Some("application/*")
            }
            _ => None,
        };
        if let Some(content_type) = content_type {
            request
                .headers
                .insert("Content-Type".to_owned(), content_type.to_owned());
        }
    }

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

#[track_caller]
fn assert_admitted(requests: &[Request]) {
    for request in requests {
        assert!(
            admits(&mut fresh(), request),
            "must be admitted: {request:?}"
        );
    }
}

#[track_caller]
fn assert_refused(requests: &[Request]) {
    for request in requests {
        assert!(
            !admits(&mut fresh(), request),
            "must be refused: {request:?}"
        );
    }
}

fn handle() -> String {
    encode_lock_handle("qdbc+yR6/xoaD=")
}

fn with_content_type(mut request: Request, content_type: &str) -> Request {
    request
        .headers
        .insert("Content-Type".to_owned(), content_type.to_owned());
    request
}

fn uri_of(kind: ObjectKind, name: &str) -> String {
    kind.object_uri(name, Some("zfg")).unwrap()
}

fn activation(preaudit: bool, body: String) -> Request {
    Request::post(
        format!("/sap/bc/adt/activation?method=activate&preauditRequested={preaudit}"),
        body.into_bytes(),
    )
}

fn worklist(entries: &[&str]) -> String {
    let mut body = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
    );

    for uri in entries {
        body.push_str(&format!(
            r#"<adtcore:objectReference adtcore:uri="{uri}" adtcore:type="CLAS/OC" adtcore:name="ZCL_A"/>"#
        ));
    }

    body.push_str("</adtcore:objectReferences>");
    body
}

fn activation_worklist_response(entries: &[&str]) -> String {
    let entries: String = entries
        .iter()
        .map(|uri| {
            format!(
                concat!(
                    r#"<ioc:entry><ioc:object><ioc:ref adtcore:uri="{uri}" "#,
                    r#"adtcore:type="CLAS/OC" adtcore:name="ZCL_A" "#,
                    r#"xmlns:adtcore="http://www.sap.com/adt/core"/>"#,
                    r#"</ioc:object></ioc:entry>"#,
                ),
                uri = uri
            )
        })
        .collect();

    format!(
        r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects">{entries}</ioc:inactiveObjects>"#
    )
}

#[test]
fn every_kind_admits_its_read_lock_put_check_and_activation_shapes() {
    let handle = handle();

    for kind in [
        ObjectKind::Prog,
        ObjectKind::Func,
        ObjectKind::Clas,
        ObjectKind::Intf,
    ] {
        let object = uri_of(kind, "zobj");
        let source = kind.source_uri("zobj", Some("zfg"), None).unwrap();
        let versions = kind.versions_uri("zobj", Some("zfg"), None).unwrap();

        assert_admitted(&[
            Request::get(format!("{source}?version=active")),
            Request::get(format!("{source}?version=inactive")),
            Request::get(versions),
            Request::post(
                format!("{object}?_action=LOCK&accessMode=MODIFY"),
                Vec::new(),
            ),
            Request::post(
                format!("{object}?_action=UNLOCK&lockHandle={handle}"),
                Vec::new(),
            ),
            Request::put(format!("{source}?lockHandle={handle}"), Vec::new()),
            Request::put(
                format!("{source}?lockHandle={handle}&corrNr=DEVK900001"),
                b"text".to_vec(),
            ),
            Request::post("/sap/bc/adt/checkruns?reporters=abapCheckRun", Vec::new()),
            activation(true, activation_request_xml("zobj", &object)),
        ]);
    }
}

#[test]
fn class_includes_admit_source_versions_and_put_but_never_their_own_lock() {
    let handle = handle();

    for include in INCLUDES {
        let source = ObjectKind::Clas
            .source_uri("zcl_a", None, Some(include))
            .unwrap();
        let versions = ObjectKind::Clas
            .versions_uri("zcl_a", None, Some(include))
            .unwrap();

        assert_admitted(&[
            Request::get(format!("{source}?version=active")),
            Request::get(format!("{source}?version=inactive")),
            Request::get(versions),
            Request::put(format!("{source}?lockHandle={handle}"), Vec::new()),
            Request::put(
                format!("{source}?lockHandle={handle}&corrNr=DEVK900001"),
                Vec::new(),
            ),
        ]);
        // The class owns the lock; an include URI is never locked or unlocked on its own.
        assert_refused(&[
            Request::post(
                format!("{source}?_action=LOCK&accessMode=MODIFY"),
                Vec::new(),
            ),
            Request::post(
                format!("{source}?_action=UNLOCK&lockHandle={handle}"),
                Vec::new(),
            ),
        ]);
    }

    assert_admitted(&[
        Request::get(format!("{CLASS}/includes/main/versions")),
        Request::post(format!("{CLASS}/includes?lockHandle={handle}"), Vec::new()),
        Request::post(
            format!("{CLASS}/includes?lockHandle={handle}&corrNr=DEVK900001"),
            b"<x/>".to_vec(),
        ),
        Request::get(format!(
            "{CLASS}/objectstructure?version=active&withShortDescriptions=true"
        )),
    ]);
}

#[test]
fn function_group_resolution_inactive_list_and_metadata_reads_are_admitted() {
    assert_admitted(&[
        Request::get(function_group_search_uri("ztest_fm")),
        Request::get("/sap/bc/adt/activation/inactiveobjects"),
        Request::get("/sap/bc/adt/functions/groups/zfg"),
        Request::get("/sap/bc/adt/functions/groups/zfg/fmodules/zfm"),
        Request::get("/sap/bc/adt/oo/interfaces/zif_a"),
    ]);
    assert_admitted(&[Request::post(
        usage_references_uri(&uri_of(ObjectKind::Func, "zfm")),
        Vec::new(),
    )]);
}

#[test]
fn create_validation_and_creation_posts_are_admitted_for_every_kind_and_scope() {
    let scopes = [
        PackageScope::Local,
        PackageScope::Transportable {
            request: "DEVK900001".to_owned(),
        },
    ];

    for kind in [
        CreateKind::Fugr,
        CreateKind::Func,
        CreateKind::Clas,
        CreateKind::Intf,
    ] {
        let scope_value = if kind == CreateKind::Func {
            "zfg"
        } else {
            "$TMP"
        };

        assert_admitted(&[Request::post(
            kind.validation_uri("zobj", "A text, with: chars/é", scope_value),
            Vec::new(),
        )]);

        for scope in &scopes {
            assert_admitted(&[Request::post(
                kind.creation_request_uri(Some("zfg"), scope).unwrap(),
                b"<x/>".to_vec(),
            )]);
        }
    }
}

#[test]
fn write_capable_rfc_shapes_reject_an_unsupported_content_type() {
    let handle = handle();
    let class_source = format!("{CLASS}/source/main?lockHandle={handle}");
    let class_create = Request::post("/sap/bc/adt/oo/classes", b"<x/>".to_vec());
    let include_create = Request::post(
        format!("{CLASS}/includes?lockHandle={handle}"),
        b"<x/>".to_vec(),
    );
    let checkrun = Request::post(
        "/sap/bc/adt/checkruns?reporters=abapCheckRun",
        b"<x/>".to_vec(),
    );
    let activation = activation(true, activation_request_xml("zcl_a", CLASS));

    assert_refused(&[
        with_content_type(
            Request::put(class_source, b"source".to_vec()),
            "application/json",
        ),
        with_content_type(class_create, "text/plain"),
        with_content_type(include_create, "application/xml"),
        with_content_type(checkrun, "application/xml"),
        with_content_type(activation, "application/*"),
    ]);
}

#[test]
fn the_recorded_transportable_class_flow_is_admitted_except_its_helper_requests() {
    let text = std::fs::read_to_string(fixture_path_in(
        "m3",
        "dev-750-rfc-transportable-class.json",
    ))
    .unwrap();
    let fixture: Fixture = serde_json::from_str(&text).unwrap();
    let mut transport = fresh();
    let mut refused = Vec::new();

    for exchange in &fixture.exchanges {
        let request = Request {
            method: exchange.request.method.clone(),
            uri: exchange.request.uri.clone(),
            headers: exchange.request.headers.clone(),
            body: exchange.request.body.as_bytes().to_vec(),
        };

        if !admits(&mut transport, &request) {
            refused.push(format!("{} {}", request.method, request.uri));
        }
    }

    // The fixture records no first activation response, so its resend is refused. Its DELETE and
    // bare-class GET are the M4 delete shapes and are admitted now.
    assert_eq!(
        refused,
        ["POST /sap/bc/adt/activation?method=activate&preauditRequested=false"]
    );
}

#[test]
fn the_recorded_read_and_where_used_requests_are_admitted() {
    for name in ["dev-750-http-read.json", "dev-750-http-where-used.json"] {
        let text = std::fs::read_to_string(fixture_path_in("m3", name)).unwrap();
        let fixture: Fixture = serde_json::from_str(&text).unwrap();

        for exchange in &fixture.exchanges {
            let request = Request {
                method: exchange.request.method.clone(),
                uri: exchange.request.uri.clone(),
                headers: exchange.request.headers.clone(),
                body: exchange.request.body.as_bytes().to_vec(),
            };

            assert!(
                admits(&mut fresh(), &request),
                "{name}: {} {}",
                request.method,
                request.uri
            );
        }
    }
}

#[test]
fn the_worklist_resend_is_admitted_only_for_entries_under_the_activated_class() {
    let member = format!("{CLASS}/source/main#type=CLAS%2FOM;name=PROBE");
    let own = worklist(&[
        CLASS,
        &format!("{CLASS}/includes/definitions"),
        &member,
        &format!("{CLASS}/source/main#type=CLAS%2FOSI;name=ZCL_A"),
    ]);
    let first = activation(true, activation_request_xml("zcl_a", CLASS));
    let resend = |body: String| activation(false, body);

    let mut transport = with_activation_response(activation_worklist_response(&[
        CLASS,
        &format!("{CLASS}/includes/definitions"),
        &member,
        &format!("{CLASS}/source/main#type=CLAS%2FOSI;name=ZCL_A"),
    ]));
    assert!(admits(&mut transport, &first));
    assert!(admits(&mut transport, &resend(own.clone())));

    // Every refusal below follows an admitted first step for ZCL_A.
    let foreign = [
        worklist(&["/sap/bc/adt/oo/classes/zcl_b"]),
        worklist(&[CLASS, "/sap/bc/adt/oo/classes/zcl_b/includes/definitions"]),
        worklist(&["/sap/bc/adt/oo/classes/zcl_a2"]),
        worklist(&["/sap/bc/adt/oo/classes/zcl_a2/source/main"]),
        worklist(&["/sap/bc/adt/programs/programs/zprog"]),
        worklist(&["/sap/bc/adt/functions/groups/zfg/fmodules/zfm"]),
        worklist(&["/sap/bc/adt/oo/interfaces/zif_a"]),
        worklist(&[&format!("{CLASS}/../zcl_b/source/main")]),
        worklist(&[&format!("{CLASS}/includes/%2e%2e/x")]),
        worklist(&[&format!("{CLASS}//source/main")]),
        worklist(&[&format!("{CLASS}/source/main#evil")]),
        worklist(&[&format!("{CLASS}/source/main#type=A;name=B;name=C")]),
        worklist(&[&format!("{CLASS}?x=1")]),
        worklist(&[]),
        String::new(),
        "not xml".to_owned(),
    ];

    for body in foreign {
        let mut transport = fresh();
        assert!(admits(&mut transport, &first));
        assert!(
            !admits(&mut transport, &resend(body.clone())),
            "must be refused: {body}"
        );
    }

    // No first step for the class, a first step for another object, or a non-class first step.
    assert!(!admits(&mut fresh(), &resend(own.clone())));

    let mut other = fresh();
    assert!(admits(
        &mut other,
        &activation(
            true,
            activation_request_xml("zcl_b", "/sap/bc/adt/oo/classes/zcl_b")
        )
    ));
    assert!(!admits(&mut other, &resend(own.clone())));

    let function = uri_of(ObjectKind::Func, "zfm");
    let mut func = fresh();
    assert!(admits(
        &mut func,
        &activation(true, activation_request_xml("zfm", &function))
    ));
    assert!(!admits(&mut func, &resend(own)));
}

#[test]
fn a_class_worklist_resend_must_exactly_match_the_first_rfc_response_and_is_consumed() {
    let member = format!("{CLASS}/source/main#type=CLAS%2FOM;name=PROBE");
    let expected = worklist(&[CLASS, &format!("{CLASS}/includes/definitions"), &member]);
    let first = activation(true, activation_request_xml("zcl_a", CLASS));
    let resend = |body: String| activation(false, body);

    let mut transport = with_activation_response(activation_worklist_response(&[
        CLASS,
        &format!("{CLASS}/includes/definitions"),
        &member,
    ]));
    assert!(admits(&mut transport, &first));
    assert!(admits(&mut transport, &resend(expected.clone())));
    assert!(
        !admits(&mut transport, &resend(expected.clone())),
        "the worklist is consumed by its sole resend"
    );

    let cases = [
        worklist(&[CLASS, &format!("{CLASS}/includes/definitions")]),
        worklist(&[
            CLASS,
            &format!("{CLASS}/includes/definitions"),
            &member,
            &format!("{CLASS}/source/main#type=CLAS%2FOSI;name=ZCL_A"),
        ]),
        worklist(&[
            CLASS,
            &format!("{CLASS}/includes/definitions"),
            &format!("{CLASS}/source/main#type=CLAS%2FOM;name=OTHER"),
        ]),
        worklist(&[&format!("{CLASS}/includes/definitions"), CLASS, &member]),
    ];

    for sent in cases {
        let mut transport = with_activation_response(activation_worklist_response(&[
            CLASS,
            &format!("{CLASS}/includes/definitions"),
            &member,
        ]));
        assert!(admits(&mut transport, &first));
        assert!(
            !admits(&mut transport, &resend(sent.clone())),
            "must refuse a resend different from the received RFC worklist: {sent}"
        );
    }
}

#[test]
fn a_class_worklist_resend_rejects_changed_or_missing_reference_metadata() {
    let member = format!("{CLASS}/source/main#type=CLAS%2FOM;name=PROBE");
    let first = activation(true, activation_request_xml("zcl_a", CLASS));
    let resend = |body: String| activation(false, body);
    let response = format!(
        r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"><ioc:entry><ioc:object><ioc:ref adtcore:uri="{member}" adtcore:type="CLAS/OM/public" adtcore:name="ZCL_A           PROBE" adtcore:parentUri="{CLASS}" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object></ioc:entry></ioc:inactiveObjects>"#
    );
    let body = |kind: Option<&str>, name: Option<&str>, parent: Option<&str>| {
        let mut reference = format!(r#"<adtcore:objectReference adtcore:uri="{member}""#);
        if let Some(kind) = kind {
            reference.push_str(&format!(r#" adtcore:type="{kind}""#));
        }
        if let Some(name) = name {
            reference.push_str(&format!(r#" adtcore:name="{name}""#));
        }
        if let Some(parent) = parent {
            reference.push_str(&format!(r#" adtcore:parentUri="{parent}""#));
        }
        reference.push_str(r#"/>"#);
        format!(
            r#"<?xml version="1.0"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">{reference}</adtcore:objectReferences>"#
        )
    };

    let mut transport = with_activation_response(response.clone());
    assert!(admits(&mut transport, &first));
    assert!(
        admits(
            &mut transport,
            &resend(body(
                Some("CLAS/OM/public"),
                Some("ZCL_A           PROBE"),
                Some(CLASS)
            ))
        ),
        "the exact reference set must be admitted"
    );

    for actual in [
        body(
            Some("CLAS/OM/private"),
            Some("ZCL_A           PROBE"),
            Some(CLASS),
        ),
        body(None, Some("ZCL_A           PROBE"), Some(CLASS)),
        body(Some("CLAS/OM/public"), Some("OTHER"), Some(CLASS)),
        body(Some("CLAS/OM/public"), None, Some(CLASS)),
        body(
            Some("CLAS/OM/public"),
            Some("ZCL_A           PROBE"),
            Some("/sap/bc/adt/oo/classes/zcl_b"),
        ),
        body(Some("CLAS/OM/public"), Some("ZCL_A           PROBE"), None),
    ] {
        let mut transport = with_activation_response(response.clone());
        assert!(admits(&mut transport, &first));
        assert!(
            !admits(&mut transport, &resend(actual.clone())),
            "must refuse an attribute-only worklist mutation: {actual}"
        );
    }
}

#[test]
fn the_first_activation_step_names_exactly_one_object_of_a_supported_kind() {
    let two = worklist(&[CLASS, "/sap/bc/adt/oo/classes/zcl_b"]);
    let fragment = worklist(&[&format!("{CLASS}/source/main#type=CLAS%2FOM;name=P")]);

    assert_refused(&[
        activation(true, two),
        activation(true, fragment),
        activation(true, worklist(&["/sap/bc/adt/vit/wb/object_type/devck"])),
        activation(true, worklist(&["/sap/bc/adt/oo/classes"])),
        activation(true, worklist(&[&format!("{CLASS}/includes/definitions")])),
        activation(true, "<broken".to_owned()),
        activation(true, worklist(&[])),
        Request::post(
            "/sap/bc/adt/activation?method=activate&preauditRequested=maybe",
            Vec::new(),
        ),
        Request::post(
            "/sap/bc/adt/activation?method=activate&preauditRequested=true&x=1",
            Vec::new(),
        ),
        Request::post("/sap/bc/adt/activation?method=deactivate", Vec::new()),
        Request::post(ACTIVATE_RESEND, Vec::new()),
    ]);
    // The M1 request without a body stays admitted.
    assert_admitted(&[Request::post(ACTIVATE_FIRST, Vec::new())]);
}

#[test]
fn corr_nr_is_admitted_only_on_create_put_and_include_post() {
    let handle = handle();
    let put = "/sap/bc/adt/oo/classes/zcl_a/source/main";

    assert_admitted(&[
        Request::post("/sap/bc/adt/oo/classes?corrNr=DEVK900001", Vec::new()),
        Request::post("/sap/bc/adt/oo/interfaces?corrNr=DEVK900001", Vec::new()),
        Request::post("/sap/bc/adt/functions/groups?corrNr=DEVK900001", Vec::new()),
        Request::post(
            "/sap/bc/adt/functions/groups/zfg/fmodules?corrNr=DEVK900001",
            Vec::new(),
        ),
        Request::put(
            format!("{put}?lockHandle={handle}&corrNr=DEVK900001"),
            Vec::new(),
        ),
    ]);
    assert_refused(&[
        Request::post(
            format!("{CLASS}?_action=LOCK&accessMode=MODIFY&corrNr=DEVK900001"),
            Vec::new(),
        ),
        Request::post(
            format!("{CLASS}?_action=UNLOCK&lockHandle={handle}&corrNr=DEVK900001"),
            Vec::new(),
        ),
        Request::get(format!("{put}?version=active&corrNr=DEVK900001")),
        Request::post(
            "/sap/bc/adt/activation?method=activate&preauditRequested=true&corrNr=DEVK900001",
            Vec::new(),
        ),
        Request::post(
            "/sap/bc/adt/checkruns?reporters=abapCheckRun&corrNr=DEVK900001",
            Vec::new(),
        ),
        Request::get("/sap/bc/adt/oo/classes?corrNr=DEVK900001"),
        // Empty, malformed, overlong, repeated or misplaced corrNr.
        Request::post("/sap/bc/adt/oo/classes?corrNr=", Vec::new()),
        Request::post("/sap/bc/adt/oo/classes?corrNr=DEV-1", Vec::new()),
        Request::post(
            "/sap/bc/adt/oo/classes?corrNr=ABCDEFGHIJKLMNOPQRSTU",
            Vec::new(),
        ),
        Request::post("/sap/bc/adt/oo/classes?corrNr=A&corrNr=B", Vec::new()),
        Request::post("/sap/bc/adt/oo/classes?corrNr=A&x=1", Vec::new()),
        Request::post("/sap/bc/adt/oo/classes?x=1", Vec::new()),
        Request::put(
            format!("{put}?corrNr=DEVK900001&lockHandle={handle}"),
            Vec::new(),
        ),
        Request::put(format!("{put}?lockHandle={handle}&corrNr="), Vec::new()),
        Request::put(
            format!("{put}?lockHandle={handle}&corrNr=A&x=1"),
            Vec::new(),
        ),
        Request::post(format!("{CLASS}/includes?corrNr=DEVK900001"), Vec::new()),
    ]);
}

#[test]
fn a_missing_empty_or_unencoded_lock_handle_is_refused() {
    let source = "/sap/bc/adt/functions/groups/zfg/fmodules/zfm/source/main";

    assert_refused(&[
        Request::put(source, Vec::new()),
        Request::put(format!("{source}?lockHandle="), Vec::new()),
        Request::put(format!("{source}?lockHandle=a+b"), Vec::new()),
        Request::put(format!("{source}?lockHandle=a/b"), Vec::new()),
        Request::put(format!("{source}?lockHandle=a=b"), Vec::new()),
        Request::put(format!("{source}?lockHandle=%2"), Vec::new()),
        Request::put(format!("{source}?lockHandle=%zz"), Vec::new()),
        Request::put(format!("{source}?lockHandle=a&lockHandle=b"), Vec::new()),
        Request::post(format!("{CLASS}/includes"), Vec::new()),
        Request::post(format!("{CLASS}/includes?lockHandle="), Vec::new()),
        Request::post(format!("{CLASS}/includes?lockHandle=a&corrNr="), Vec::new()),
        Request::post(format!("{CLASS}?_action=UNLOCK&lockHandle="), Vec::new()),
        Request::post(format!("{CLASS}?_action=UNLOCK"), Vec::new()),
        Request::post(format!("{CLASS}?_action=UNLOCK&lockHandle=a+b"), Vec::new()),
    ]);
}

#[test]
fn wrong_object_kind_uris_are_refused() {
    let handle = handle();
    let interface = "/sap/bc/adt/oo/interfaces/zif_a";
    let function = "/sap/bc/adt/functions/groups/zfg/fmodules/zfm";
    let program = "/sap/bc/adt/programs/programs/zprog";

    assert_refused(&[
        // Includes exist on classes only.
        Request::get(format!("{interface}/includes/testclasses?version=active")),
        Request::put(
            format!("{interface}/includes/testclasses?lockHandle={handle}"),
            Vec::new(),
        ),
        Request::post(
            format!("{interface}/includes?lockHandle={handle}"),
            Vec::new(),
        ),
        Request::post(
            format!("{function}/includes?lockHandle={handle}"),
            Vec::new(),
        ),
        Request::post(
            format!("{program}/includes?lockHandle={handle}"),
            Vec::new(),
        ),
        Request::get(format!("{function}/includes/main/versions")),
        Request::get(format!("{program}/includes/main/versions")),
        // A class has no source/main/versions; its feeds are per include.
        Request::get(format!("{CLASS}/source/main/versions")),
        Request::get(format!("{CLASS}/includes/other/versions")),
        Request::get(format!("{CLASS}/includes/other?version=active")),
        // main is a feed name, not a readable or writable include.
        Request::get(format!("{CLASS}/includes/main?version=active")),
        Request::put(
            format!("{CLASS}/includes/main?lockHandle={handle}"),
            Vec::new(),
        ),
        // objectstructure is a class read.
        Request::get(format!(
            "{interface}/objectstructure?version=active&withShortDescriptions=true"
        )),
        Request::get(format!(
            "{function}/objectstructure?version=active&withShortDescriptions=true"
        )),
        Request::get(format!(
            "{program}/objectstructure?version=active&withShortDescriptions=true"
        )),
        Request::get(format!("{CLASS}/objectstructure")),
        Request::get(format!(
            "{CLASS}/objectstructure?version=inactive&withShortDescriptions=true"
        )),
        Request::get(format!(
            "{CLASS}/objectstructure?version=active&withShortDescriptions=true&x=1"
        )),
        // A function module needs its group; collections are not lockable (the group lock for
        // a delete is covered by the delete admission tests).
        Request::get("/sap/bc/adt/functions/groups/zfg/source/main?version=active"),
        Request::get("/sap/bc/adt/functions/fmodules/zfm/source/main?version=active"),
        Request::post(
            "/sap/bc/adt/oo/classes?_action=LOCK&accessMode=MODIFY",
            Vec::new(),
        ),
        Request::put(
            format!("{function}/fmodules/zfm2/source/main?lockHandle={handle}"),
            Vec::new(),
        ),
        // Bare object reads never take a query or name a collection.
        Request::get("/sap/bc/adt/oo/classes"),
        Request::get("/sap/bc/adt/functions/groups"),
        // Metadata of a function module and interface have no query.
        Request::get(format!("{function}?version=active")),
        Request::get(format!("{interface}?x=1")),
    ]);
}

#[test]
fn traversal_empty_and_namespaced_paths_are_refused() {
    assert_refused(&[
        Request::get("/sap/bc/adt/oo/classes/../programs/programs/z/source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes/./source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes/%2e%2e/source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes/zcl_a/includes/../source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes//source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes/zcl_a/source/main?version=active#frag"),
        Request::get("/sap/bc/adt/programs/programs/../x/source/main?version=active"),
        Request::get("/sap/bc/adt/functions/groups/../fmodules/zfm/source/main?version=active"),
        Request::get("/sap/bc/adt/functions/groups/zfg/fmodules/../source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes/zcl a/source/main?version=active"),
        // Namespaced source paths have no verified form for the new kinds.
        Request::get("/sap/bc/adt/oo/classes/%2fns%2fcl/source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes//ns/cl/source/main?version=active"),
        Request::get("/sap/bc/adt/oo/interfaces/%2Fns%2Fif/source/main/versions"),
        Request::get(
            "/sap/bc/adt/functions/groups/%2fns%2ffg/fmodules/zfm/source/main?version=active",
        ),
        Request::post(
            "/sap/bc/adt/oo/classes/%2fns%2fcl?_action=LOCK&accessMode=MODIFY",
            Vec::new(),
        ),
        Request::post(
            usage_references_uri("/sap/bc/adt/functions/groups/../fmodules/zfm"),
            Vec::new(),
        ),
    ]);
    // The evidenced PROG namespace form is retained.
    assert_admitted(&[Request::get(
        "/sap/bc/adt/programs/programs/%2fns%2fprog/source/main?version=active",
    )]);
    // M1 PROG names retain their legal `$` and `-` segment characters.
    assert_admitted(&[
        Request::get("/sap/bc/adt/programs/programs/z$include-1/source/main?version=active"),
        Request::put(
            "/sap/bc/adt/programs/programs/z$include-1/source/main?lockHandle=a%2Bb",
            Vec::new(),
        ),
    ]);
}

#[test]
fn unapproved_queries_and_repository_shapes_are_refused() {
    let class_source = format!("{CLASS}/source/main");
    let search = "/sap/bc/adt/repository/informationsystem/search";

    assert_refused(&[
        Request::get(format!("{class_source}?version=active&x=1")),
        Request::get(format!("{class_source}?version=workingArea")),
        Request::get(format!("{class_source}?x=1&version=active")),
        Request::get(class_source.clone()),
        // Group search: exactly the typed quickSearch the resolver sends.
        Request::get(format!(
            "{search}?operation=quickSearch&query=ZFM&maxResults=10"
        )),
        Request::get(format!(
            "{search}?operation=quickSearch&query=ZFM&maxResults=100&objectType=FUGR%2FFF"
        )),
        Request::get(format!(
            "{search}?operation=quickSearch&query=ZFM&maxResults=10&objectType=CLAS%2FOC"
        )),
        Request::get(format!(
            "{search}?operation=quickSearch&query=ZF%26M&maxResults=10&objectType=FUGR%2FFF"
        )),
        Request::get(format!(
            "{search}?operation=quickSearch&query=&maxResults=10&objectType=FUGR%2FFF"
        )),
        Request::get(format!(
            "{search}?operation=quickSearch&query=Z*&maxResults=10&objectType=FUGR%2FFF"
        )),
        Request::get(format!(
            "{search}?operation=quickSearch&query=ZFM&maxResults=10&objectType=FUGR%2FFF&x=1"
        )),
        Request::post(
            format!("{search}?operation=quickSearch&query=ZFM&maxResults=10&objectType=FUGR%2FFF"),
            Vec::new(),
        ),
        Request::get("/sap/bc/adt/repository/nodestructure"),
        Request::post(
            "/sap/bc/adt/repository/nodepath?uri=/sap/bc/adt/functions/groups/zfg/fmodules/zfm",
            Vec::new(),
        ),
        Request::get("/sap/bc/adt/activation/inactiveobjects?x=1"),
        Request::post("/sap/bc/adt/activation/inactiveobjects", Vec::new()),
        // where-used: a function module only, one uri parameter.
        Request::post(usage_references_uri(CLASS), Vec::new()),
        Request::post(
            usage_references_uri("/sap/bc/adt/programs/programs/zprog"),
            Vec::new(),
        ),
        Request::post(
            "/sap/bc/adt/repository/informationsystem/usageReferences",
            Vec::new(),
        ),
        Request::post(
            "/sap/bc/adt/repository/informationsystem/usageReferences?uri=",
            Vec::new(),
        ),
        Request::post(
            format!(
                "{}&x=1",
                usage_references_uri("/sap/bc/adt/functions/groups/zfg/fmodules/zfm")
            ),
            Vec::new(),
        ),
        Request::get(usage_references_uri(
            "/sap/bc/adt/functions/groups/zfg/fmodules/zfm",
        )),
    ]);
}

#[test]
fn create_validation_and_creation_shapes_outside_the_four_kinds_are_refused() {
    let functions = "/sap/bc/adt/functions/validation";
    let oo = "/sap/bc/adt/oo/validation/objectname";

    assert_refused(&[
        // Validation: fixed key order, known object types per path, encoded values only.
        Request::post(
            format!("{functions}?objtype=FUGR%2FF&objname=ZA&description=T&fugrname=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{functions}?objtype=FUGR%2FFF&objname=ZA&description=T&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{functions}?objtype=CLAS%2FOC&objname=ZA&description=T&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objtype=FUGR%2FF&objname=ZA&description=T&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objtype=PROG%2FP&objname=ZA&description=T&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objname=ZA&objtype=CLAS%2FOC&description=T&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objtype=CLAS%2FOC&objname=ZA&description=T&packagename=ZG&x=1"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objtype=CLAS%2FOC&objname=ZA&description=a b&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objtype=CLAS%2FOC&objname=ZA&description=a&b&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objtype=CLAS%2FOC&objname=&description=T&packagename=ZG"),
            Vec::new(),
        ),
        Request::post(
            format!("{oo}?objtype=CLAS%2FOC&objname=ZA&description=T&packagename="),
            Vec::new(),
        ),
        Request::post(oo, Vec::new()),
        Request::get(format!(
            "{oo}?objtype=CLAS%2FOC&objname=ZA&description=T&packagename=ZG"
        )),
        Request::post(
            "/sap/bc/adt/programs/validation?objtype=PROG%2FP&objname=ZA&description=T&packagename=%24TMP",
            Vec::new(),
        ),
        // Creation: only the four collections, never programs, packages or transports.
        Request::post("/sap/bc/adt/programs/programs", Vec::new()),
        Request::post("/sap/bc/adt/packages", Vec::new()),
        Request::post("/sap/bc/adt/cts/transportrequests", Vec::new()),
        Request::post("/sap/bc/adt/oo/classes/", Vec::new()),
        Request::post("/sap/bc/adt/functions/groups/zfg/fmodules/", Vec::new()),
        Request::post("/sap/bc/adt/functions/groups/zfg/fmodules/zfm", Vec::new()),
        Request::post("/sap/bc/adt/functions/groups//fmodules", Vec::new()),
        Request::post("/sap/bc/adt/functions/groups/../fmodules", Vec::new()),
        Request::get("/sap/bc/adt/functions/groups/zfg/fmodules"),
        Request::put("/sap/bc/adt/oo/classes", Vec::new()),
    ]);
}

#[test]
fn delete_of_a_part_mcp_function_module_fallback_and_unrelated_endpoints_are_refused() {
    let handle = handle();

    assert_refused(&[
        Request::delete(format!("{CLASS}/includes/testclasses?lockHandle={handle}")),
        Request::new(
            "PATCH",
            format!("{CLASS}/source/main?lockHandle={handle}"),
            Vec::new(),
        ),
        Request::new("HEAD", "/sap/bc/adt/discovery", Vec::new()),
        Request::new("get", "/sap/bc/adt/discovery", Vec::new()),
        Request::put(
            format!("{CLASS}?_action=LOCK&accessMode=MODIFY"),
            Vec::new(),
        ),
        Request::post(
            format!("{CLASS}/source/main?lockHandle={handle}"),
            Vec::new(),
        ),
        Request::post("/sap/bc/adt/discovery", Vec::new()),
        // MCP and function-module fallback paths.
        Request::post("/mcp", Vec::new()),
        Request::post("/sap/bc/adt/mcp", Vec::new()),
        Request::post("/sap/bc/mcp/tools/call", Vec::new()),
        Request::post("/sap/bc/rfc/YFM_UTILS_SOURCE_DEPLOY", Vec::new()),
        Request::post("/sap/bc/soap/rfc?sap-client=100", Vec::new()),
        Request::post(
            "/sap/bc/adt/rfc/functions/YFM_UTILS_SOURCE_DEPLOY",
            Vec::new(),
        ),
        // Unrelated ADT endpoints.
        Request::get("/sap/bc/adt/vit/wb/object_type/devck/object_name/%24TMP"),
        Request::get("/sap/bc/adt/packages/ztest"),
        Request::get("/sap/bc/adt/cts/transportrequests"),
        Request::get("/sap/bc/adt/ddic/tables/zt/source/main?version=active"),
        Request::get("/sap/bc/adt/programs/includes/zi/source/main?version=active"),
        Request::get("/sap/bc/adt/oo/classes.json"),
        Request::get("/sap/bc/adt/discovery/x"),
        Request::get("sap/bc/adt/discovery"),
        Request::get(""),
    ]);
}

fn with_response(status: u16, body: String) -> RfcTransport<FakeRfcApi> {
    RfcTransport::new(FakeRfcApi::with_responses(vec![RfcEndpointResponse {
        status,
        headers: BTreeMap::new(),
        body: body.into_bytes(),
    }]))
}

#[test]
fn a_resend_needs_a_successful_worklist_answer_to_the_first_step() {
    let first = activation(true, activation_request_xml("zcl_a", CLASS));
    let resend = activation(false, worklist(&[CLASS]));
    let reply_of_entries = activation_worklist_response(&[CLASS]);
    // Parses to the same reference as a worklist, but its root is not `inactiveObjects`.
    let foreign_root = reply_of_entries.replace("ioc:inactiveObjects", "ioc:other");

    // The control: a 200 worklist makes exactly this resend legal.
    let mut transport = with_response(200, reply_of_entries.clone());
    assert!(admits(&mut transport, &first));
    assert!(admits(&mut transport, &resend));

    for (status, body) in [
        (500, reply_of_entries.clone()),
        (404, reply_of_entries.clone()),
        (200, foreign_root),
    ] {
        let mut transport = with_response(status, body.clone());
        assert!(admits(&mut transport, &first), "HTTP {status}");
        assert!(
            !admits(&mut transport, &resend),
            "HTTP {status} must not arm a resend: {body}"
        );
    }
}

#[test]
fn a_refused_request_or_a_closed_session_discards_the_pending_resend() {
    let first = activation(true, activation_request_xml("zcl_a", CLASS));
    let resend = activation(false, worklist(&[CLASS]));
    let armed = || {
        let mut transport = with_response(200, activation_worklist_response(&[CLASS]));
        assert!(admits(&mut transport, &first));
        transport
    };

    let mut transport = armed();
    assert!(!admits(
        &mut transport,
        &Request::get("/sap/bc/adt/programs/programs/../source/main?version=active")
    ));
    assert!(
        !admits(&mut transport, &resend),
        "a refused request must clear the worklist"
    );

    let mut transport = armed();
    transport.close().unwrap();
    assert!(
        !admits(&mut transport, &resend),
        "closing the session must clear the worklist"
    );

    // The control: nothing in between keeps the worklist.
    assert!(admits(&mut armed(), &resend));
}

#[test]
fn a_program_name_that_is_empty_or_a_dot_segment_is_refused() {
    let uri =
        |name: &str| format!("/sap/bc/adt/programs/programs/{name}/source/main?version=active");

    assert_refused(&[
        Request::get(uri("")),
        Request::get(uri(".")),
        Request::get(uri("..")),
    ]);
    assert_admitted(&[
        Request::get(uri("zprog")),
        Request::get(uri("z.prog")),
        Request::get(uri("%2fns%2fprog")),
    ]);
}
