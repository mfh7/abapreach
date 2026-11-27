//! The request boundary of the `where-used` command: exactly
//! `POST usageReferences?uri=<object URI of an admitted kind>` with the usage request body, and the
//! data previews of the cross-reference fallback, which the existing DDIC data preview shape
//! already admits. Nothing broader under `informationsystem` or `ddic`.

mod common;

use abapreach::adt::{USAGE_REQUEST_XML, usage_references_uri};
use abapreach::operation::runner::RunnerGated;
use abapreach::operation::where_used::{DEFAULT_MAX, Query, run};
use abapreach::policy::Policies;
use abapreach::transport::{
    Request, Response, RunnerArm, SadtTransport, TransportError,
    http::HttpTransport,
    rfc::{FakeRfcApi, RfcTransport},
};
use common::{ScriptedTransport, preview};

const USAGE: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";

fn admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

/// The request the commands build: the exact body, and both headers as `application/*`.
fn with_headers(mut request: Request) -> Request {
    for header in ["Content-Type", "Accept"] {
        request
            .headers
            .insert(header.to_owned(), "application/*".to_owned());
    }

    request
}

fn usage_post(object_uri: &str) -> Request {
    with_headers(Request::post(
        usage_references_uri(object_uri),
        USAGE_REQUEST_XML.as_bytes().to_vec(),
    ))
}

fn post(uri: impl Into<String>, body: Vec<u8>) -> Request {
    with_headers(Request::post(uri, body))
}

fn raw_post(uri: &str) -> Request {
    with_headers(Request::post(uri, USAGE_REQUEST_XML.as_bytes().to_vec()))
}

#[track_caller]
fn assert_refused(requests: &[Request]) {
    for request in requests {
        assert!(!admits(request), "must be refused: {request:?}");
    }
}

#[test]
fn the_object_of_each_admitted_kind_is_admitted_with_the_exact_body() {
    for object in [
        "/sap/bc/adt/programs/programs/zprog_a1",
        "/sap/bc/adt/programs/includes/zincl_a1",
        "/sap/bc/adt/functions/groups/zfg_a1",
        "/sap/bc/adt/functions/groups/zfg_a1/fmodules/zfm_a1",
        "/sap/bc/adt/oo/classes/zcl_a1",
        "/sap/bc/adt/oo/interfaces/zif_a1",
        "/sap/bc/adt/ddic/dataelements/zde_a1",
        "/sap/bc/adt/ddic/domains/zdo_a1",
        "/sap/bc/adt/ddic/tables/zt_a1",
        "/sap/bc/adt/ddic/structures/zs_a1",
        "/sap/bc/adt/ddic/tabletypes/ztt_a1",
        // The case of a name is not part of the shape.
        "/sap/bc/adt/oo/classes/ZCL_A1",
        "/sap/bc/adt/ddic/domains/ZDO_A1",
    ] {
        assert!(admits(&usage_post(object)), "{object}");
    }
}

/// The request the command builds for each kind is admitted: the whole `run` on a scripted
/// transport, with the requests it sent replayed through the boundary.
#[test]
fn every_request_the_command_builds_is_admitted() {
    let group = r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/zfg/fmodules/zfm" adtcore:type="FUGR/FF" adtcore:name="ZFM"/></adtcore:objectReferences>"#;
    let discovery = |hrefs: &[&str]| {
        let collections: String = hrefs
            .iter()
            .map(|href| format!(r#"<app:collection href="{href}"/>"#))
            .collect();

        format!(
            r#"<app:service xmlns:app="http://www.w3.org/2007/app">{collections}</app:service>"#
        )
    };
    let empty_usage = r#"<usageReferences:usageReferenceResult xmlns:usageReferences="u"><usageReferences:referencedObjects/></usageReferences:usageReferenceResult>"#;
    let dd02l = |name: &str| {
        preview(&[
            ("TABNAME", &[Some(name)]),
            ("TABCLASS", &[Some("TRANSP")]),
            ("VIEWCLASS", &[None]),
        ])
    };
    let rows = |column: &str, values: &[&str]| {
        let values: Vec<Option<&str>> = values.iter().map(|value| Some(*value)).collect();

        preview(&[(column, &values)])
    };
    let trdir_check = || {
        vec![
            preview(&[
                ("TABNAME", &[Some("TRDIR")]),
                ("TABCLASS", &[Some("VIEW")]),
                ("VIEWCLASS", &[Some("D")]),
            ]),
            preview(&[("DDLNAME", &[]), ("OBJECTNAME", &[]), ("OBJECTTYPE", &[])]),
            preview(&[
                ("VIEWNAME", &[Some("TRDIR")]),
                ("TABNAME", &[Some("REPOSRC")]),
                ("TABPOS", &[Some("0001")]),
            ]),
            dd02l("REPOSRC"),
        ]
    };
    let long_names: Vec<String> = (0..40).map(|index| format!("Z{index:039}")).collect();
    let cases: Vec<(&str, &str, Vec<String>)> = vec![
        ("PROG", "ZPROG", vec![empty_usage.to_owned()]),
        ("FUGR", "ZFG", vec![empty_usage.to_owned()]),
        ("CLAS", "ZCL_A", vec![empty_usage.to_owned()]),
        ("INTF", "ZIF_A", vec![empty_usage.to_owned()]),
        (
            "FUNC",
            "ZFM",
            vec![group.to_owned(), empty_usage.to_owned()],
        ),
        (
            "DOMA",
            "ZDO_A",
            vec![
                discovery(&["/sap/bc/adt/ddic/domains"]),
                empty_usage.to_owned(),
            ],
        ),
        (
            "TTYP",
            "ZTT_A",
            vec![
                discovery(&["/sap/bc/adt/ddic/tabletypes"]),
                empty_usage.to_owned(),
            ],
        ),
        // The fallback: every table the data previews read, with the view lookups of `TRDIR`,
        // the master, group and package reads.
        (
            "DTEL",
            "ZDE_A",
            [
                vec![
                    discovery(&[]),
                    dd02l("WBCROSSGT"),
                    rows(
                        "INCLUDE",
                        &[
                            "LZFGU01",
                            "ZCL_FOO===================CP",
                            "ZIF_FOO===================IU",
                            "/NS/LFGU01",
                            "ZR_TOP",
                        ],
                    ),
                ],
                trdir_check(),
                vec![
                    rows("NAME", &["SAPLZFG"]),
                    dd02l("WBCROSSI"),
                    preview(&[("INCLUDE", &[Some("ZR_TOP")]), ("MASTER", &[Some("ZR")])]),
                    dd02l("DD03L"),
                    rows("TABNAME", &["ZSTRUCT"]),
                    dd02l("TADIR"),
                    preview(&[
                        ("OBJECT", &[Some("PROG")]),
                        ("OBJ_NAME", &[Some("ZR")]),
                        ("DEVCLASS", &[Some("ZP")]),
                    ]),
                ],
            ]
            .concat(),
        ),
        (
            "DOMA",
            "/NS/ZDO_A",
            vec![
                dd02l("WBCROSSGT"),
                rows("INCLUDE", &["ZR"]),
                dd02l("WBCROSSI"),
                preview(&[("INCLUDE", &[]), ("MASTER", &[])]),
                dd02l("DD04L"),
                rows("ROLLNAME", &["ZDE_A"]),
                dd02l("TADIR"),
                preview(&[
                    ("OBJECT", &[Some("PROG")]),
                    ("OBJ_NAME", &[Some("ZR")]),
                    ("DEVCLASS", &[Some("ZP")]),
                ]),
            ],
        ),
        // The longest lists: 40 names of 40 characters in the master and package reads.
        (
            "TTYP",
            "ZTT_A",
            vec![
                discovery(&[]),
                dd02l("WBCROSSGT"),
                rows(
                    "INCLUDE",
                    &long_names.iter().map(String::as_str).collect::<Vec<_>>(),
                ),
                dd02l("WBCROSSI"),
                preview(&[("INCLUDE", &[]), ("MASTER", &[])]),
                dd02l("TADIR"),
                preview(&[("OBJECT", &[]), ("OBJ_NAME", &[]), ("DEVCLASS", &[])]),
            ],
        ),
        // A table by its class.
        (
            "TABL",
            "ZS_A",
            vec![
                discovery(&["/sap/bc/adt/ddic/structures"]),
                preview(&[
                    ("TABNAME", &[Some("ZS_A")]),
                    ("TABCLASS", &[Some("INTTAB")]),
                    ("VIEWCLASS", &[None]),
                ]),
                empty_usage.to_owned(),
            ],
        ),
    ];

    for (token, name, replies) in cases {
        let replies = replies
            .iter()
            .map(|body| Ok(Response::text(200, body)))
            .collect();
        let mut transport = ScriptedTransport::replies(replies);
        let query = Query::new(token, name, DEFAULT_MAX).unwrap();

        run(&mut transport, &query, &Policies::default()).unwrap();

        assert!(!transport.calls.is_empty(), "{token}");

        for request in &transport.calls {
            assert!(admits(request), "{token} {name}: {request:?}");
        }
    }
}

#[test]
fn another_parameter_a_repeated_uri_or_another_method_is_refused() {
    let class = usage_references_uri("/sap/bc/adt/oo/classes/zcl_a");
    let body = USAGE_REQUEST_XML.as_bytes().to_vec();

    assert!(admits(&post(class.clone(), body.clone())));
    assert_refused(&[
        post(format!("{class}&x=1"), body.clone()),
        post(format!("{class}&maxResults=5"), body.clone()),
        post(format!("{class}&uri={class}"), body.clone()),
        post(
            format!("{USAGE}?x=1&uri=/sap/bc/adt/oo/classes/zcl_a"),
            body.clone(),
        ),
        post(
            format!("{USAGE}?uri=/sap/bc/adt/oo/classes/zcl_a?x=1"),
            body.clone(),
        ),
        post(format!("{class}#frag"), body.clone()),
        post(format!("{class}%26x%3D1"), body.clone()),
        post(USAGE, body.clone()),
        post(format!("{USAGE}?"), body.clone()),
        post(format!("{USAGE}?uri="), body.clone()),
        post(
            format!("{USAGE}?Uri=/sap/bc/adt/oo/classes/zcl_a"),
            body.clone(),
        ),
        post(
            format!("{USAGE}/x?uri=/sap/bc/adt/oo/classes/zcl_a"),
            body.clone(),
        ),
        post(
            format!("{USAGE}?operation=quickSearch&query=ZCL"),
            body.clone(),
        ),
        Request::get(class.clone()),
        Request::put(class.clone(), body.clone()),
        Request::delete(class.clone()),
    ]);
}

#[test]
fn an_object_uri_of_another_kind_or_part_is_refused() {
    assert_refused(&[
        // Other DDIC collections and parts, a collection, an extra segment.
        usage_post("/sap/bc/adt/ddic/structures"),
        usage_post("/sap/bc/adt/ddic/structures/zs_a/source/main"),
        usage_post("/sap/bc/adt/ddic/structure/zs_a"),
        usage_post("/sap/bc/adt/ddic/ddl/sources/zddl_a"),
        usage_post("/sap/bc/adt/ddic/dataelements"),
        usage_post("/sap/bc/adt/ddic/dataelements/"),
        usage_post("/sap/bc/adt/ddic/domains/zdo_a/extra"),
        usage_post("/sap/bc/adt/ddic/dataelements/zde_a/source/main"),
        usage_post("/sap/bc/adt/ddic/tables/zt_a/source/main"),
        usage_post("/sap/bc/adt/ddic/dataelement/zde_a"),
        usage_post("/sap/bc/adt/ddic/tables2/zt_a"),
        usage_post("/sap/bc/adt/ddic/zde_a"),
        // Parts of the admitted kinds, collections, and kinds of other families.
        usage_post("/sap/bc/adt/oo/classes/zcl_a/source/main"),
        usage_post("/sap/bc/adt/oo/classes/zcl_a/includes/testclasses"),
        usage_post("/sap/bc/adt/oo/classes/zcl_a/objectstructure"),
        usage_post("/sap/bc/adt/oo/classes"),
        usage_post("/sap/bc/adt/oo/classes/"),
        usage_post("/sap/bc/adt/oo/interfaces/zif_a/source/main"),
        usage_post("/sap/bc/adt/functions/groups"),
        usage_post("/sap/bc/adt/functions/groups/zfg/includes/zlfgtop"),
        usage_post("/sap/bc/adt/functions/groups/zfg/fmodules/zfm/source/main"),
        usage_post("/sap/bc/adt/functions/groups/zfg/fmodules"),
        usage_post("/sap/bc/adt/packages/zpack"),
        usage_post("/sap/bc/adt/messageclass/zmsg"),
        usage_post("/sap/bc/adt/vit/wb/object_type/devck/object_name/ZPACK"),
        usage_post("/sap/bc/adt/oo/classrun/zcl_a"),
        usage_post("/sap/bc/adt/datapreview/freestyle"),
        usage_post("/sap/bc/adt/repository/informationsystem/search?query=Z"),
        usage_post("/sap/bc/adt/discovery"),
        usage_post("/sap/bc/adt/programs/programs/zprog_a/source/main"),
        usage_post("/sap/bc/adt/programs/includes/zincl_a/source/main"),
        usage_post("/sap/bc/adt/ddic"),
        usage_post("/something/else"),
        usage_post(""),
        // A case other than the path as ADT writes it.
        usage_post("/SAP/BC/ADT/OO/CLASSES/zcl_a"),
        usage_post("/sap/bc/adt/DDIC/domains/zdo_a"),
        usage_post("/sap/bc/adt/ddic/Domains/zdo_a"),
    ]);
}

#[test]
fn a_traversal_an_encoding_or_a_name_outside_the_plain_rule_is_refused() {
    assert_refused(&[
        usage_post("/sap/bc/adt/oo/classes/../interfaces/zif_a"),
        usage_post("/sap/bc/adt/oo/classes/.."),
        usage_post("/sap/bc/adt/oo/classes/."),
        usage_post("/sap/bc/adt/ddic/domains/../tables/zt_a"),
        usage_post("/sap/bc/adt/ddic/domains/zdo_a/.."),
        usage_post("/sap/bc/adt/ddic/dataelements/../../../../discovery"),
        usage_post("/sap/bc/adt/functions/groups/../fmodules/zfm"),
        usage_post("/sap/bc/adt/functions/groups/zfg/fmodules/../zfm"),
        usage_post("/sap/bc/adt/programs/programs/../includes/zincl"),
        usage_post("/sap/bc/adt/ddic//domains/zdo_a"),
        usage_post("/sap/bc/adt/ddic/./domains/zdo_a"),
        usage_post("/sap/bc/adt//oo/classes/zcl_a"),
        // The query encodes everything but `/` and the unreserved characters, so an encoded
        // slash, dot or `%` in the object is a different request.
        raw_post(&format!(
            "{USAGE}?uri=%2Fsap%2Fbc%2Fadt%2Foo%2Fclasses%2Fzcl_a"
        )),
        raw_post(&format!(
            "{USAGE}?uri=%2fsap%2fbc%2fadt%2fddic%2fdomains%2fzdo_a"
        )),
        raw_post(&format!(
            "{USAGE}?uri=/sap/bc/adt/oo/classes/zcl_a%2f..%2fzif_a"
        )),
        raw_post(&format!(
            "{USAGE}?uri=/sap/bc/adt/oo/classes/%2e%2e/interfaces/zif_a"
        )),
        raw_post(&format!("{USAGE}?uri=/sap/bc/adt/oo/classes/zcl%5Fa")),
        raw_post(&format!("{USAGE}?uri=/sap/bc/adt/ddic/domains/zdo%2Fa")),
        raw_post(&format!(
            "{USAGE}?uri=/sap/bc/adt/ddic/domains/%2fns%2fzdo_a"
        )),
        raw_post(&format!(
            "{USAGE}?uri=/sap/bc/adt/ddic/domains/%252fns%252fzdo_a"
        )),
        raw_post(&format!("{USAGE}?uri=/sap/bc/adt/oo/classes/%2Fns%2Fzcl_a")),
        usage_post("/sap/bc/adt/oo/classes/zcl a"),
        usage_post("/sap/bc/adt/oo/classes/zcl-a"),
        usage_post("/sap/bc/adt/oo/classes/zcl.a"),
        usage_post("/sap/bc/adt/oo/classes/zcl$a"),
        usage_post("/sap/bc/adt/oo/classes/zcl_a;x=1"),
        usage_post("/sap/bc/adt/oo/classes/zcl_a\n"),
        usage_post("/sap/bc/adt/oo/classes/zcl_\u{e4}"),
        usage_post("/sap/bc/adt/ddic/dataelements/zde a"),
        usage_post("/sap/bc/adt/ddic/dataelements/zde'a"),
        usage_post("/sap/bc/adt/ddic/dataelements/zde-a"),
        usage_post("/sap/bc/adt/ddic/dataelements/zde$a"),
        usage_post("/sap/bc/adt/ddic/dataelements/zde.a"),
        usage_post("/sap/bc/adt/ddic/dataelements/*"),
        usage_post("/sap/bc/adt/ddic/dataelements/zde_a%00"),
        // The raw absolute-form and scheme tricks.
        raw_post(
            "http://evil.example/sap/bc/adt/repository/informationsystem/usageReferences?uri=/sap/bc/adt/oo/classes/zcl_a",
        ),
        raw_post(&format!(
            "{USAGE}?uri=http://evil.example/sap/bc/adt/oo/classes/zcl_a"
        )),
    ]);
}

#[test]
fn a_body_other_than_the_usage_request_is_refused_for_every_kind() {
    let mut trailing = USAGE_REQUEST_XML.as_bytes().to_vec();

    trailing.push(b' ');

    let mut other = USAGE_REQUEST_XML.replace("affectedObjects", "affectedObject");

    other.push('\n');

    for object in [
        "/sap/bc/adt/programs/programs/zprog_a",
        "/sap/bc/adt/programs/includes/zincl_a",
        "/sap/bc/adt/functions/groups/zfg",
        "/sap/bc/adt/functions/groups/zfg/fmodules/zfm",
        "/sap/bc/adt/oo/classes/zcl_a",
        "/sap/bc/adt/oo/interfaces/zif_a",
        "/sap/bc/adt/ddic/dataelements/zde_a",
        "/sap/bc/adt/ddic/domains/zdo_a",
        "/sap/bc/adt/ddic/tables/zt_a",
        "/sap/bc/adt/ddic/structures/zs_a",
        "/sap/bc/adt/ddic/tabletypes/ztt_a",
    ] {
        let uri = usage_references_uri(object);

        assert_refused(&[
            post(uri.clone(), Vec::new()),
            post(uri.clone(), b"<x/>".to_vec()),
            post(uri.clone(), trailing.clone()),
            post(uri.clone(), other.clone().into_bytes()),
            post(uri.clone(), b"SELECT * FROM T001".to_vec()),
        ]);
    }
}

/// The cross-reference previews are admitted by the existing DDIC data preview shape, which this
/// command does not broaden: the bodies the command builds pass it, and the same tables with a
/// body of another shape do not.
#[test]
fn the_cross_reference_previews_are_admitted_by_the_existing_data_preview_shape() {
    use abapreach::operation::read_table::preview_request;

    for (table, body) in [
        (
            "WBCROSSGT",
            "SELECT INCLUDE FROM WBCROSSGT WHERE OTYPE = 'TY' AND NAME = 'ZDE_A'",
        ),
        (
            "WBCROSSGT",
            "SELECT INCLUDE FROM WBCROSSGT WHERE OTYPE = 'TY' AND NAME = '/NS/ZDE_A'",
        ),
        (
            "DD03L",
            "SELECT TABNAME FROM DD03L WHERE ROLLNAME = 'ZDE_A' AND AS4LOCAL = 'A'",
        ),
        (
            "DD04L",
            "SELECT ROLLNAME FROM DD04L WHERE DOMNAME = 'ZDO_A' AND AS4LOCAL = 'A'",
        ),
        (
            "TRDIR",
            "SELECT NAME FROM TRDIR WHERE NAME IN ( 'SAPLZFG', '/NS/SAPLFG' )",
        ),
        (
            "TADIR",
            "SELECT OBJECT, OBJ_NAME, DEVCLASS FROM TADIR WHERE PGMID = 'R3TR' AND OBJECT IN ( 'CLAS', 'DTEL', 'FUGR', 'PROG', 'TABL' ) AND OBJ_NAME IN ( 'ZCL_A', 'ZFG' )",
        ),
    ] {
        let request = preview_request(table, body, 5000).unwrap();

        assert!(admits(&request), "{body}");
    }

    // The shape is still the canonical rendering of one table: a body that reads another
    // table, joins, or is not canonical is refused, as before.
    let exact = preview_request(
        "WBCROSSGT",
        "SELECT INCLUDE FROM WBCROSSGT WHERE OTYPE = 'TY' AND NAME = 'ZDE_A'",
        5000,
    )
    .unwrap();

    for body in [
        "SELECT INCLUDE FROM USR02 WHERE OTYPE = 'TY' AND NAME = 'ZDE_A'",
        "SELECT INCLUDE FROM WBCROSSGT, USR02",
        "SELECT INCLUDE FROM WBCROSSGT WHERE OTYPE = 'TY' AND NAME = 'ZDE_A' OR 1 = 1 UNION SELECT BNAME FROM USR02",
        "select include from wbcrossgt where otype = 'TY' and name = 'ZDE_A'",
        "SELECT INCLUDE  FROM WBCROSSGT WHERE OTYPE = 'TY' AND NAME = 'ZDE_A'",
    ] {
        let mut request = exact.clone();

        request.body = body.as_bytes().to_vec();

        assert!(!admits(&request), "{body}");
    }
}

/// Decision: the runner arm does not refuse where-used. Like `search`, it is a read of object
/// names and packages, with a fixed body and no table content; while a runner is armed its POST
/// stays admitted in the same shape, and the freestyle preview stays refused (the 0.17 rule).
#[test]
fn a_where_used_post_stays_admitted_while_a_runner_is_armed() {
    const RUNNER: &str = "YCL_ABAPREACH_RUN_AB12CD34";

    let _arm = RunnerArm::arm(RUNNER).unwrap();
    let runner_class = usage_post(&format!("/sap/bc/adt/oo/classes/{}", RUNNER.to_lowercase()));

    for request in [
        usage_post("/sap/bc/adt/oo/classes/zcl_a"),
        usage_post("/sap/bc/adt/programs/programs/zprog_a"),
        usage_post("/sap/bc/adt/ddic/domains/zdo_a"),
        runner_class,
    ] {
        assert!(admits(&request), "{request:?}");
    }

    // The broader shapes stay refused while armed, and so does the freestyle preview.
    assert_refused(&[
        usage_post("/sap/bc/adt/oo/classes/zcl_a/source/main"),
        post(USAGE, USAGE_REQUEST_XML.as_bytes().to_vec()),
        {
            let mut request = Request::post(
                "/sap/bc/adt/datapreview/freestyle?rowNumber=1",
                b"SELECT * FROM T001".to_vec(),
            );

            request
                .headers
                .insert("Content-Type".to_owned(), "text/plain".to_owned());
            request
                .headers
                .insert("Accept".to_owned(), "application/*".to_owned());

            request
        },
    ]);
}

/// The ADT HTTP gate of a runner operation takes the same decision: it judges only the classrun
/// resource and the runner create, so a where-used POST passes it.
#[test]
fn the_http_gate_of_an_armed_runner_does_not_refuse_a_where_used_post() {
    const RUNNER: &str = "YCL_ABAPREACH_RUN_AB12CD34";

    let _arm = RunnerArm::arm(RUNNER).unwrap();
    let mut http = HttpTransport::for_test(vec![Ok(Response::empty(200))]);
    let mut gated = RunnerGated::new(&mut http);

    gated
        .request(usage_post("/sap/bc/adt/oo/classes/zcl_a"))
        .expect("a where-used POST is not judged by the runner gate");
}

/// The preview shapes check the two headers, and so does this one: a request without either, or
/// with another value, is refused for every kind.
#[test]
fn a_missing_or_other_content_type_or_accept_is_refused() {
    for object in [
        "/sap/bc/adt/programs/programs/zprog_a",
        "/sap/bc/adt/functions/groups/zfg",
        "/sap/bc/adt/oo/classes/zcl_a",
        "/sap/bc/adt/ddic/structures/zs_a",
    ] {
        let exact = usage_post(object);

        assert!(admits(&exact), "{object}");

        for (name, value) in [
            ("Content-Type", None),
            ("Accept", None),
            ("Content-Type", Some("text/plain")),
            ("Accept", Some("text/plain")),
            ("Content-Type", Some("application/xml")),
            ("Accept", Some("application/xml")),
            ("Accept", Some("application/*, text/html")),
        ] {
            let mut request = exact.clone();

            match value {
                Some(value) => request.headers.insert(name.to_owned(), value.to_owned()),
                None => request.headers.remove(name),
            };

            assert!(!admits(&request), "{object} {name}={value:?}");
        }
    }
}
