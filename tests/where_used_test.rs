//! `where-used`: the type and name rules, the ADT request of each kind, the reply of the three
//! releases (HD1 750, FED 740, S4D 758, from `tests/fixtures/m9`), the discovery decision for the
//! DDIC types, and the cross-reference fallback.

mod common;

use abapreach::adt::{USAGE_REQUEST_XML, usage_references_uri};
use abapreach::backend::Failure;
use abapreach::operation::dictionary::PreviewError;
use abapreach::operation::where_used::{DEFAULT_MAX, Query, run};
use abapreach::policy::Policies;
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, fixture_path_in, preview};
use std::fs;

const DISCOVERY: &str = "/sap/bc/adt/discovery";
const DATA_PREVIEW: &str = "/sap/bc/adt/datapreview/ddic";

fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_path_in("m9", name)).expect("read fixture")
}

fn query(type_token: &str, name: &str) -> Query {
    Query::new(type_token, name, DEFAULT_MAX)
        .unwrap_or_else(|error| panic!("{type_token} {name}: {error}"))
}

fn ok(body: impl AsRef<str>) -> Result<Response, TransportError> {
    Ok(Response::text(200, body))
}

fn discovery(hrefs: &[&str]) -> String {
    let collections: String = hrefs
        .iter()
        .map(|href| {
            format!(r#"<app:collection href="{href}"><atom:title>t</atom:title></app:collection>"#)
        })
        .collect();

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><app:service xmlns:app="http://www.w3.org/2007/app" xmlns:atom="http://www.w3.org/2005/Atom"><app:workspace><atom:title>w</atom:title>{collections}</app:workspace></app:service>"#
    )
}

fn dd02l(name: &str) -> Result<Response, TransportError> {
    dd02l_class(name, "TRANSP")
}

fn dd02l_class(name: &str, class: &str) -> Result<Response, TransportError> {
    ok(preview(&[
        ("TABNAME", &[Some(name)]),
        ("TABCLASS", &[Some(class)]),
        ("VIEWCLASS", &[None]),
    ]))
}

/// The dictionary check of `TRDIR` as the live systems answer it (740, 750 and 758): a database
/// view whose only base table is `REPOSRC`. Four lookups: the view, the generated CDS views of
/// it, its base tables, and the base table itself.
fn trdir_check() -> Vec<Result<Response, TransportError>> {
    vec![
        ok(preview(&[
            ("TABNAME", &[Some("TRDIR")]),
            ("TABCLASS", &[Some("VIEW")]),
            ("VIEWCLASS", &[Some("D")]),
        ])),
        ok(preview(&[
            ("DDLNAME", &[]),
            ("OBJECTNAME", &[]),
            ("OBJECTTYPE", &[]),
        ])),
        ok(preview(&[
            ("VIEWNAME", &[Some("TRDIR")]),
            ("TABNAME", &[Some("REPOSRC")]),
            ("TABPOS", &[Some("0001")]),
        ])),
        dd02l("REPOSRC"),
    ]
}

/// The `WBCROSSI` rows: each pair is an include and its master program.
fn wbcrossi(pairs: &[(&str, &str)]) -> Result<Response, TransportError> {
    let includes: Vec<Option<&str>> = pairs.iter().map(|(include, _)| Some(*include)).collect();
    let masters: Vec<Option<&str>> = pairs.iter().map(|(_, master)| Some(*master)).collect();

    ok(preview(&[("INCLUDE", &includes), ("MASTER", &masters)]))
}

fn tadir(rows: &[(&str, &str, &str)]) -> Result<Response, TransportError> {
    let objects: Vec<Option<&str>> = rows.iter().map(|(object, _, _)| Some(*object)).collect();
    let names: Vec<Option<&str>> = rows.iter().map(|(_, name, _)| Some(*name)).collect();
    let packages: Vec<Option<&str>> = rows.iter().map(|(_, _, package)| Some(*package)).collect();

    ok(preview(&[
        ("OBJECT", &objects),
        ("OBJ_NAME", &names),
        ("DEVCLASS", &packages),
    ]))
}

fn column(name: &str, values: &[&str]) -> Result<Response, TransportError> {
    let values: Vec<Option<&str>> = values.iter().map(|value| Some(*value)).collect();

    ok(preview(&[(name, &values)]))
}

fn execute(
    query: &Query,
    replies: Vec<Result<Response, TransportError>>,
) -> (
    Result<abapreach::domain::Outcome, PreviewError>,
    ScriptedTransport,
) {
    let mut transport = ScriptedTransport::replies(replies);
    let outcome = run(&mut transport, query, &Policies::default());

    (outcome, transport)
}

fn succeeded(
    query: &Query,
    replies: Vec<Result<Response, TransportError>>,
) -> (String, String, ScriptedTransport) {
    let (outcome, transport) = execute(query, replies);
    let outcome = outcome.unwrap_or_else(|error| panic!("{error:?}"));

    assert_eq!(outcome.exit, 0);
    transport.assert_finished();

    (outcome.stdout, outcome.stderr, transport)
}

fn bodies(transport: &ScriptedTransport) -> Vec<String> {
    transport
        .calls
        .iter()
        .filter(|call| call.uri.starts_with(DATA_PREVIEW))
        .map(|call| String::from_utf8(call.body.clone()).unwrap())
        .collect()
}

#[test]
fn a_type_is_one_of_the_nine_in_any_case_and_msag_and_tran_are_out_of_scope() {
    for token in [
        "PROG", "FUNC", "FUGR", "CLAS", "INTF", "DTEL", "DOMA", "TABL", "TTYP", "prog", "Func",
    ] {
        assert!(Query::new(token, "ZNAME", 10).is_ok(), "{token}");
    }

    let msag = Query::new("MSAG", "ZMSG", 10).unwrap_err();
    let tran = Query::new("tran", "SE38", 10).unwrap_err();

    assert!(
        msag.contains("MSAG") && msag.contains("not available"),
        "{msag}"
    );
    assert!(
        tran.contains("TRAN") && tran.contains("not supported"),
        "{tran}"
    );

    for token in ["", "BOGUS", "CLAS/OC", "PROG ", "DDLS"] {
        let error = Query::new(token, "ZNAME", 10).unwrap_err();

        assert!(error.starts_with("unknown type"), "{token:?}: {error}");
        assert!(
            error.contains("PROG, FUNC, FUGR, CLAS, INTF, DTEL, DOMA, TABL, TTYP"),
            "{error}"
        );
    }
}

#[test]
fn an_adt_type_takes_a_plain_name_and_a_ddic_type_takes_a_ddic_identifier() {
    assert_eq!(query("clas", "zcl_a1").name(), "ZCL_A1");
    assert!(Query::new("PROG", &"Z".repeat(40), 1).is_ok());
    assert!(Query::new("PROG", &"Z".repeat(41), 1).is_err());
    assert!(Query::new("CLAS", &"Z".repeat(31), 1).is_err());
    assert!(Query::new("DTEL", "/NS/ZDE_A", 1).is_ok());
    assert!(Query::new("DTEL", &"Z".repeat(31), 1).is_err());

    for (token, name) in [
        ("PROG", ""),
        ("PROG", "Z PROG"),
        ("PROG", "Z$PROG"),
        ("PROG", "ZPROG-1"),
        ("PROG", "ZPROG.1"),
        ("PROG", "ZPROG%2f"),
        ("PROG", "ZPROG/1"),
        ("PROG", "../ZPROG"),
        ("PROG", "ZPROG?x=1"),
        ("PROG", "ZPROG&x=1"),
        ("PROG", "ZPROÄ"),
        ("CLAS", "ZCL'A"),
        ("FUNC", "Z\nFM"),
        ("DTEL", ""),
        ("DTEL", "ZDE A"),
        ("DTEL", "ZDE'A"),
        ("DOMA", "ZDO%27"),
        ("TABL", "ZT;DROP"),
        ("TTYP", "ZTT*"),
    ] {
        assert!(Query::new(token, name, 10).is_err(), "{token} {name:?}");
    }

    // A namespaced name has no verified ADT path for the ADT types.
    for token in ["PROG", "FUNC", "FUGR", "CLAS", "INTF"] {
        let error = Query::new(token, "/NS/ZNAME", 10).unwrap_err();

        assert!(error.contains("namespaced"), "{token}: {error}");
    }
}

#[test]
fn the_number_of_objects_is_between_one_and_five_thousand() {
    assert_eq!(DEFAULT_MAX, 200);

    for max in [1, 200, 5000] {
        assert!(Query::new("PROG", "ZP", max).is_ok(), "{max}");
    }

    for max in [0, 5001, u32::MAX] {
        assert!(Query::new("PROG", "ZP", max).is_err(), "{max}");
    }
}

#[test]
fn the_usage_request_of_each_adt_kind_is_a_post_on_the_object_query() {
    for (token, name, object) in [
        ("PROG", "zprog_a", "/sap/bc/adt/programs/programs/zprog_a"),
        ("CLAS", "zcl_a", "/sap/bc/adt/oo/classes/zcl_a"),
        ("INTF", "zif_a", "/sap/bc/adt/oo/interfaces/zif_a"),
        ("FUGR", "zfg", "/sap/bc/adt/functions/groups/zfg"),
    ] {
        let (stdout, stderr, transport) =
            succeeded(&query(token, name), vec![ok(fixture("usage-prog-758.xml"))]);

        assert_eq!(stdout, "WHERE-USED: 0");
        assert_eq!(stderr, "");

        let [call] = transport.calls.as_slice() else {
            panic!("{token}: one request expected");
        };

        assert_eq!(call.method, "POST");
        assert_eq!(call.uri, usage_references_uri(object), "{token}");
        assert_eq!(call.body, USAGE_REQUEST_XML.as_bytes());
        assert_eq!(
            call.headers.get("Content-Type").map(String::as_str),
            Some("application/*")
        );
        assert_eq!(
            call.headers.get("Accept").map(String::as_str),
            Some("application/*")
        );
    }
}

#[test]
fn a_function_module_is_resolved_to_its_group_before_the_usage_request() {
    let group = r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/zfg/fmodules/zfm" adtcore:type="FUGR/FF" adtcore:name="ZFM"/></adtcore:objectReferences>"#;
    let (stdout, _, transport) = succeeded(
        &query("func", "zfm"),
        vec![ok(group), ok(fixture("usage-func-758.xml"))],
    );

    assert_eq!(
        stdout,
        "USED-BY PINF/KI SRCX_INTERNAL package=SRCX via=ADT\n\
         USED-BY PINF/KI SRCX_PUBLIC package=SRCX via=ADT\n\
         WHERE-USED: 2"
    );
    assert_eq!(transport.calls[0].method, "GET");
    assert_eq!(
        transport.calls[1].uri,
        usage_references_uri("/sap/bc/adt/functions/groups/zfg/fmodules/zfm")
    );
}

#[test]
fn the_usage_replies_of_the_three_releases_list_the_main_objects_once() {
    for (token, name, reply, expected) in [
        ("PROG", "RSPARAM", "usage-prog-740.xml", "WHERE-USED: 0"),
        ("PROG", "RSPARAM", "usage-prog-758.xml", "WHERE-USED: 0"),
        (
            "PROG",
            "RSPARAM",
            "usage-prog-750.xml",
            "USED-BY PROG RFVISL00 package=FVVI via=ADT\nWHERE-USED: 1",
        ),
        (
            "INTF",
            "IF_OO_ADT_CLASSRUN",
            "usage-intf-750.xml",
            "WHERE-USED: 0",
        ),
        (
            "INTF",
            "IF_OO_ADT_CLASSRUN",
            "usage-intf-740.xml",
            "USED-BY CLAS CL_OO_ADT_RES_CLASSRUN package=SEO_ADT via=ADT\nWHERE-USED: 1",
        ),
        (
            "INTF",
            "IF_OO_ADT_CLASSRUN",
            "usage-intf-758.xml",
            "USED-BY PINF/KI SEO_ADT_PUBLIC package=SEO_ADT via=ADT\n\
             USED-BY PINF/KI SEO_MAIN_PRIVATE package=SEO_MAIN via=ADT\n\
             USED-BY PINF/KI SEO_MAIN_PUBLIC package=SEO_MAIN via=ADT\n\
             WHERE-USED: 3",
        ),
        (
            "CLAS",
            "CL_ABAP_ZIP",
            "usage-clas-740.xml",
            "USED-BY FUGR /UI5/UI5_REPOSITORY_LOAD package=/UI5/UI5_INFRA_APP via=ADT\n\
             USED-BY FUGR FDT_WD_CONNECTION package=SFDT_WD_ISLAND_INFRASTRUCTURE via=ADT\n\
             WHERE-USED: 2",
        ),
        (
            "CLAS",
            "CL_ABAP_ZIP",
            "usage-clas-750.xml",
            "USED-BY FUGR FDT_WD_CONNECTION package=SFDT_WD_ISLAND_INFRASTRUCTURE via=ADT\n\
             USED-BY FUGR GEN_EBPP package=FKKB via=ADT\n\
             USED-BY FUGR J7LRPARTNER_IMPORT package=J7LRPARTNER_SETUP via=ADT\n\
             WHERE-USED: 3",
        ),
        (
            "CLAS",
            "CL_ABAP_ZIP",
            "usage-clas-758.xml",
            "USED-BY CLAS ZTEST_OBJECT package=ZTEST_OBJECT via=ADT\n\
             USED-BY FUGR /SMB/ZIP_UPLOAD_DOWNLOAD package=/SMB/COMMON via=ADT\n\
             USED-BY FUGR PAOC_SBP_TOOLS package=PAOC_SBP_PYP via=ADT\n\
             USED-BY PROG ZTEST_OBJECT package=$TMP via=ADT\n\
             USED-BY PROG/I ZTEST_OBJECT package=ZTEST_OBJECT via=ADT\n\
             WHERE-USED: 5",
        ),
        (
            "FUNC",
            "RFC_SYSTEM_INFO",
            "usage-func-740.xml",
            "USED-BY FUGR /BDL/BDL11 package=/BDL/TASKMANAGER via=ADT\n\
             USED-BY FUGR /BDL/BDL3 package=/BDL/BDL3N via=ADT\n\
             WHERE-USED: 2",
        ),
        (
            "FUNC",
            "RFC_SYSTEM_INFO",
            "usage-func-750.xml",
            "USED-BY FUGR AADM package=SARC via=ADT\n\
             USED-BY FUGR ARCHIVOBJECT package=SAOA via=ADT\n\
             USED-BY FUGR ARFC package=SRFC via=ADT\n\
             WHERE-USED: 3",
        ),
        (
            "FUNC",
            "RFC_SYSTEM_INFO",
            "usage-func-758.xml",
            "USED-BY PINF/KI SRCX_INTERNAL package=SRCX via=ADT\n\
             USED-BY PINF/KI SRCX_PUBLIC package=SRCX via=ADT\n\
             WHERE-USED: 2",
        ),
    ] {
        let mut replies = vec![];

        if token == "FUNC" {
            replies.push(ok(
                r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/srfc/fmodules/rfc_system_info" adtcore:type="FUGR/FF" adtcore:name="RFC_SYSTEM_INFO"/></adtcore:objectReferences>"#,
            ));
        }

        replies.push(ok(fixture(reply)));

        let (stdout, stderr, _) = succeeded(&query(token, name), replies);

        assert_eq!(stdout, expected, "{reply}");
        assert_eq!(stderr, "", "{reply}");
    }
}

#[test]
fn the_list_is_sorted_and_each_line_is_printed_once() {
    let xml = r#"<usageReferences:usageReferenceResult numberOfResults="999" xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences" xmlns:adtcore="http://www.sap.com/adt/core"><usageReferences:referencedObjects>
        <usageReferences:referencedObject uri="/a/zb" isResult="true"><usageReferences:adtObject adtcore:name="ZB" adtcore:type="PROG/P"><adtcore:packageRef adtcore:name="ZP"/></usageReferences:adtObject></usageReferences:referencedObject>
        <usageReferences:referencedObject uri="/a/za" isResult="true"><usageReferences:adtObject adtcore:name="ZA" adtcore:type="PROG/P"><adtcore:packageRef adtcore:name="ZP"/></usageReferences:adtObject></usageReferences:referencedObject>
        <usageReferences:referencedObject uri="/a/zb" isResult="true"><usageReferences:adtObject adtcore:name="ZB" adtcore:type="PROG/P"><adtcore:packageRef adtcore:name="ZP"/></usageReferences:adtObject></usageReferences:referencedObject>
        <usageReferences:referencedObject uri="/a/zc" isResult="true"><usageReferences:adtObject adtcore:name="ZC" adtcore:type="CLAS/OC"/></usageReferences:referencedObject>
        </usageReferences:referencedObjects></usageReferences:usageReferenceResult>"#;
    let (stdout, _, _) = succeeded(&query("PROG", "ZP"), vec![ok(xml)]);

    assert_eq!(
        stdout,
        "USED-BY CLAS ZC package=? via=ADT\n\
         USED-BY PROG ZA package=ZP via=ADT\n\
         USED-BY PROG ZB package=ZP via=ADT\n\
         WHERE-USED: 3"
    );
}

#[test]
fn a_list_longer_than_max_is_cut_after_sorting_with_a_warning() {
    let query = Query::new("CLAS", "CL_ABAP_ZIP", 2).unwrap();
    let (stdout, stderr, _) = succeeded(&query, vec![ok(fixture("usage-clas-758.xml"))]);

    assert_eq!(
        stdout,
        "USED-BY CLAS ZTEST_OBJECT package=ZTEST_OBJECT via=ADT\n\
         USED-BY FUGR /SMB/ZIP_UPLOAD_DOWNLOAD package=/SMB/COMMON via=ADT\n\
         WHERE-USED: 2"
    );
    assert_eq!(stderr, "W: 5 objects found; the first 2 are shown (--max)");

    // Exactly max objects is not a cut.
    let query = Query::new("CLAS", "CL_ABAP_ZIP", 5).unwrap();
    let (stdout, stderr, _) = succeeded(&query, vec![ok(fixture("usage-clas-758.xml"))]);

    assert!(stdout.ends_with("WHERE-USED: 5"), "{stdout}");
    assert_eq!(stderr, "");
}

#[test]
fn a_field_of_a_line_loses_control_and_format_characters() {
    let xml = "<r xmlns:adtcore=\"c\"><referencedObject uri=\"/a\"><adtObject adtcore:name=\"Z\u{202e}A\nB\" adtcore:type=\"PROG/P\"><packageRef adtcore:name=\"Z\tP\u{200b}\"/></adtObject></referencedObject></r>";
    let (stdout, _, _) = succeeded(&query("PROG", "ZP"), vec![ok(xml)]);

    assert_eq!(
        stdout,
        "USED-BY PROG ZA B package=Z P via=ADT\nWHERE-USED: 1"
    );
}

#[test]
fn a_reply_that_is_not_a_usage_result_a_failed_status_or_a_lost_session_is_an_error() {
    let (outcome, _) = execute(&query("PROG", "ZP"), vec![ok("<a><b></a>")]);

    assert!(
        matches!(&outcome, Err(PreviewError::Rejected(message)) if message.starts_with("cannot read the where-used reply")),
        "{outcome:?}"
    );

    let (outcome, _) = execute(
        &query("PROG", "ZP"),
        vec![Ok(Response::text(404, "<html/>"))],
    );

    assert!(
        matches!(&outcome, Err(PreviewError::Failed { message, .. }) if message.starts_with("the where-used search answered HTTP 404")),
        "{outcome:?}"
    );

    let (outcome, _) = execute(&query("PROG", "ZP"), vec![Ok(Response::text(401, ""))]);

    assert!(
        matches!(
            &outcome,
            Err(PreviewError::Failed {
                failure: Failure::Authorization,
                ..
            })
        ),
        "{outcome:?}"
    );

    let (outcome, _) = execute(
        &query("PROG", "ZP"),
        vec![Err(TransportError::PreSendConnectivity(
            "no route".to_owned(),
        ))],
    );

    assert!(
        matches!(
            &outcome,
            Err(PreviewError::Failed {
                failure: Failure::PreSendConnectivity,
                ..
            })
        ),
        "{outcome:?}"
    );
}

// ---- DDIC types: the discovery decides ------------------------------------------------------

const USAGE_PROG: &str = "usage-prog-750.xml";
const ONE_USER: &str = "USED-BY PROG RFVISL00 package=FVVI via=ADT\nWHERE-USED: 1";

#[test]
fn a_ddic_type_goes_through_adt_when_the_discovery_lists_its_collection() {
    for (token, collection, name, before) in [
        ("DTEL", "/sap/bc/adt/ddic/dataelements", "zde_a", vec![]),
        ("DOMA", "/sap/bc/adt/ddic/domains", "zdo_a", vec![]),
        ("TTYP", "/sap/bc/adt/ddic/tabletypes", "ztt_a", vec![]),
        // A table takes `tables` for a transparent table and `structures` for a structure, by
        // its table class.
        (
            "TABL",
            "/sap/bc/adt/ddic/tables",
            "zt_a",
            vec![dd02l_class("ZT_A", "TRANSP")],
        ),
        (
            "TABL",
            "/sap/bc/adt/ddic/structures",
            "zs_a",
            vec![dd02l_class("ZS_A", "INTTAB")],
        ),
    ] {
        let mut replies = vec![ok(discovery(&[
            "/sap/bc/adt/programs/programs",
            "/sap/bc/adt/ddic/dataelements",
            "/sap/bc/adt/ddic/domains",
            "/sap/bc/adt/ddic/tables",
            "/sap/bc/adt/ddic/structures",
            "/sap/bc/adt/ddic/tabletypes",
        ]))];

        replies.extend(before);
        replies.push(ok(fixture(USAGE_PROG)));

        let (stdout, stderr, transport) = succeeded(&query(token, name), replies);

        assert_eq!(stdout, ONE_USER);
        assert_eq!(stderr, "", "{token}");
        assert_eq!(transport.calls[0].method, "GET");
        assert_eq!(transport.calls[0].uri, DISCOVERY);

        let last = transport.calls.last().unwrap();

        assert_eq!(
            last.uri,
            usage_references_uri(&format!("{collection}/{name}")),
            "{token}"
        );
        assert_eq!(last.body, USAGE_REQUEST_XML.as_bytes());
        // The ADT path reads no table content: at most the one class lookup of a table.
        assert!(bodies(&transport).len() <= 1, "{token}");
    }
}

#[test]
fn a_table_takes_the_collection_of_its_class_or_the_tables_when_that_is_not_listed() {
    let listed_tables = || ok(discovery(&["/sap/bc/adt/ddic/tables"]));
    let listed_structures = || ok(discovery(&["/sap/bc/adt/ddic/structures"]));
    let nothing = || vec![dd02l("WBCROSSGT"), column("INCLUDE", &[])];

    for (discovery_reply, class, falls_back) in [
        (listed_tables(), "INTTAB", true),
        (listed_structures(), "TRANSP", true),
        (listed_tables(), "VIEW", true),
        (listed_tables(), "APPENDSTRU", true),
        (listed_structures(), "INTTAB", false),
        // A pool or cluster table is a table: it takes `tables`, which is listed here.
        (listed_tables(), "POOL", false),
        (listed_tables(), "CLUSTER", false),
        (listed_structures(), "POOL", true),
    ] {
        let mut replies = vec![discovery_reply, dd02l_class("ZT", class)];

        if falls_back {
            replies.extend(nothing());
        } else {
            replies.push(ok(fixture(USAGE_PROG)));
        }

        let (stdout, stderr, _) = succeeded(&query("TABL", "ZT"), replies);

        if falls_back {
            assert_eq!(stdout, "WHERE-USED: 0", "{class}");
            assert_eq!(
                stderr,
                "W: no ADT where-used for TABL on this release; using cross-reference tables"
            );
        } else {
            assert_eq!(stdout, ONE_USER);
        }
    }

    // A table the dictionary does not know has no collection either.
    let (stdout, _, _) = succeeded(
        &query("TABL", "ZT"),
        vec![
            listed_tables(),
            ok(preview(&[
                ("TABNAME", &[]),
                ("TABCLASS", &[]),
                ("VIEWCLASS", &[]),
            ])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &[]),
        ],
    );

    assert_eq!(stdout, "WHERE-USED: 0");
}

#[test]
fn a_listed_collection_that_answers_404_or_500_takes_the_tables_with_a_warning() {
    for status in [404, 500] {
        let (stdout, stderr, transport) = succeeded(
            &query("DTEL", "ZDE_A"),
            vec![
                ok(discovery(&["/sap/bc/adt/ddic/dataelements"])),
                Ok(Response::text(status, "<exc/>")),
                dd02l("WBCROSSGT"),
                column("INCLUDE", &[]),
                dd02l("DD03L"),
                column("TABNAME", &["ZSTRUCT"]),
                dd02l("TADIR"),
                tadir(&[("TABL", "ZSTRUCT", "ZP")]),
            ],
        );

        assert_eq!(
            stdout,
            "USED-BY TABL ZSTRUCT package=ZP via=DD03L\nWHERE-USED: 1"
        );
        assert_eq!(
            stderr,
            format!(
                "W: the ADT where-used for DTEL answered HTTP {status}; using cross-reference tables"
            )
        );
        assert_eq!(transport.calls[1].method, "POST");
    }

    // Another status, and any status of a type ADT always serves, is an error.
    let (outcome, _) = execute(
        &query("DTEL", "ZDE_A"),
        vec![
            ok(discovery(&["/sap/bc/adt/ddic/dataelements"])),
            Ok(Response::text(403, "")),
        ],
    );

    assert!(
        matches!(&outcome, Err(PreviewError::Failed { .. })),
        "{outcome:?}"
    );

    for status in [404, 500] {
        let (outcome, _) = execute(
            &query("CLAS", "ZCL_A"),
            vec![Ok(Response::text(status, ""))],
        );

        assert!(
            matches!(&outcome, Err(PreviewError::Failed { .. })),
            "{status}: {outcome:?}"
        );
    }
}

#[test]
fn a_collection_the_discovery_does_not_list_takes_the_tables_with_the_release_warning() {
    // 7.40: no `/ddic/*`; 7.50: data elements and structures only, so a domain still falls back;
    // a trailing slash on the listed collection is the same collection.
    for (token, listed, falls_back) in [
        ("DOMA", discovery(&["/sap/bc/adt/ddic/dataelements"]), true),
        ("TTYP", discovery(&["/sap/bc/adt/programs/programs"]), true),
        ("TABL", discovery(&[]), true),
        ("DTEL", discovery(&[]), true),
        ("DTEL", String::new(), true),
        (
            "DTEL",
            discovery(&["/sap/bc/adt/ddic/dataelements/"]),
            false,
        ),
        ("DTEL", discovery(&["/sap/bc/adt/ddic/dataelements2"]), true),
    ] {
        let mut replies = vec![ok(listed)];

        if falls_back {
            // The `WBCROSSGT` read finds nothing: dictionary check, then the rows.
            replies.extend([dd02l("WBCROSSGT"), column("INCLUDE", &[])]);

            if token == "DTEL" {
                replies.extend([dd02l("DD03L"), column("TABNAME", &[])]);
            }

            if token == "DOMA" {
                replies.extend([dd02l("DD04L"), column("ROLLNAME", &[])]);
            }
        } else {
            replies.push(ok(fixture("usage-prog-758.xml")));
        }

        let (stdout, stderr, _) = succeeded(&query(token, "ZOBJ"), replies);

        assert_eq!(stdout, "WHERE-USED: 0");

        if falls_back {
            assert_eq!(
                stderr,
                format!(
                    "W: no ADT where-used for {token} on this release; using cross-reference tables"
                )
            );
        } else {
            assert_eq!(stderr, "");
        }
    }
}

#[test]
fn a_discovery_that_is_not_200_lists_nothing_and_an_unreadable_one_is_rejected() {
    let (stdout, stderr, _) = succeeded(
        &query("TTYP", "ZTT_A"),
        vec![
            Ok(Response::text(404, "no discovery")),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &[]),
        ],
    );

    assert_eq!(stdout, "WHERE-USED: 0");
    assert!(
        stderr.starts_with("W: no ADT where-used for TTYP"),
        "{stderr}"
    );

    let (outcome, _) = execute(&query("TTYP", "ZTT_A"), vec![ok("<a><b></a>")]);

    assert!(
        matches!(&outcome, Err(PreviewError::Rejected(message)) if message.starts_with("cannot read the ADT discovery")),
        "{outcome:?}"
    );

    let (outcome, _) = execute(&query("TTYP", "ZTT_A"), vec![Ok(Response::text(401, ""))]);

    assert!(
        matches!(
            &outcome,
            Err(PreviewError::Failed {
                failure: Failure::Authorization,
                ..
            })
        ),
        "{outcome:?}"
    );
}

#[test]
fn a_namespaced_ddic_name_takes_the_tables_without_asking_the_discovery() {
    let (stdout, stderr, transport) = succeeded(
        &query("DTEL", "/ns/zde_a"),
        vec![
            dd02l("WBCROSSGT"),
            column("INCLUDE", &[]),
            dd02l("DD03L"),
            column("TABNAME", &[]),
        ],
    );

    assert_eq!(stdout, "WHERE-USED: 0");
    assert_eq!(
        stderr,
        "W: no ADT where-used path for a namespaced DTEL name; using cross-reference tables"
    );
    assert_eq!(
        bodies(&transport)[1],
        "SELECT INCLUDE FROM WBCROSSGT WHERE OTYPE = 'TY' AND NAME = '/NS/ZDE_A'"
    );
}

// ---- the cross-reference fallback -----------------------------------------------------------

fn class_include(class: &str, suffix: &str) -> String {
    format!("{class:=<30}{suffix}")
}

#[test]
fn the_includes_of_a_type_are_mapped_to_their_main_objects_with_the_packages_from_tadir() {
    let class_pool = class_include("ZCL_FOO", "CP");
    let class_method = class_include("ZCL_FOO", "CM001");
    let namespaced = class_include("/NS/ZCL_BAR", "CCAU");
    let interface_pool = class_include("ZIF_BAR", "IU");
    let interface_pool_public = class_include("ZIF_BAR", "IP");
    let includes = [
        class_pool.as_str(),
        class_method.as_str(),
        namespaced.as_str(),
        interface_pool.as_str(),
        interface_pool_public.as_str(),
        "LZFGU01",
        "LZFGTOP",
        "/NS/LFGU01",
        "LZNOGRPF01",
        "ZREPORT",
        "ZREP_TOP",
        "LAB",
        // Not a plain name: no candidate for a group, and no literal in a read.
        "LZF\u{c4}U01",
    ];
    let mut replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &includes),
    ];

    replies.extend(trdir_check());
    replies.push(column("NAME", &["SAPLZFG", "/NS/SAPLFG"]));
    replies.extend([
        dd02l("WBCROSSI"),
        wbcrossi(&[("ZREP_TOP", "ZREP"), ("LZNOGRPF01", "ZNOGRP")]),
        dd02l("DD03L"),
        column("TABNAME", &["ZSTRUCT", "ZTABLE", "ZSTRUCT"]),
        dd02l("TADIR"),
        tadir(&[
            ("CLAS", "ZCL_FOO", "ZPACK"),
            ("CLAS", "/NS/ZCL_BAR", "/NS/PACK"),
            ("INTF", "ZIF_BAR", "ZPACK_I"),
            ("FUGR", "ZFG", "ZPACK_FG"),
            ("PROG", "ZREP", "ZP_REP"),
            ("PROG", "ZREPORT", "$TMP"),
            ("TABL", "ZTABLE", "ZPACK_T"),
        ]),
    ]);

    let (stdout, stderr, transport) = succeeded(&query("DTEL", "ZDE_A"), replies);

    assert_eq!(
        stdout,
        "USED-BY CLAS /NS/ZCL_BAR package=/NS/PACK via=WBCROSSGT\n\
         USED-BY CLAS ZCL_FOO package=ZPACK via=WBCROSSGT\n\
         USED-BY FUGR /NS/FG package=? via=WBCROSSGT\n\
         USED-BY FUGR ZFG package=ZPACK_FG via=WBCROSSGT\n\
         USED-BY INTF ZIF_BAR package=ZPACK_I via=WBCROSSGT\n\
         USED-BY PROG LAB package=? via=WBCROSSGT\n\
         USED-BY PROG LZF\u{c4}U01 package=? via=WBCROSSGT\n\
         USED-BY PROG ZNOGRP package=? via=WBCROSSGT\n\
         USED-BY PROG ZREP package=ZP_REP via=WBCROSSGT\n\
         USED-BY PROG ZREPORT package=$TMP via=WBCROSSGT\n\
         USED-BY TABL ZSTRUCT package=? via=DD03L\n\
         USED-BY TABL ZTABLE package=ZPACK_T via=DD03L\n\
         WHERE-USED: 12"
    );
    assert_eq!(
        stderr,
        "W: no ADT where-used for DTEL on this release; using cross-reference tables"
    );

    let sent = bodies(&transport);

    assert_eq!(sent.len(), 13);
    assert_eq!(
        sent[1],
        "SELECT INCLUDE FROM WBCROSSGT WHERE OTYPE = 'TY' AND NAME = 'ZDE_A'"
    );
    assert!(
        sent[2].contains("FROM DD02L") && sent[2].contains("'TRDIR'"),
        "{}",
        sent[2]
    );
    assert_eq!(
        sent[6],
        "SELECT NAME FROM TRDIR WHERE NAME IN ( '/NS/SAPLFG', 'SAPLZFG', 'SAPLZNOGRP' )"
    );
    assert_eq!(
        sent[8],
        "SELECT INCLUDE, MASTER FROM WBCROSSI WHERE INCLUDE IN ( 'LAB', 'LZNOGRPF01', 'ZREPORT', 'ZREP_TOP' )"
    );
    assert_eq!(
        sent[10],
        "SELECT TABNAME FROM DD03L WHERE ROLLNAME = 'ZDE_A' AND AS4LOCAL = 'A'"
    );
    assert!(
        sent[12].starts_with(
            "SELECT OBJECT, OBJ_NAME, DEVCLASS FROM TADIR WHERE PGMID = 'R3TR' AND OBJECT IN ( 'CLAS', 'DTEL', 'FUGR', 'INTF', 'PROG', 'TABL' ) AND OBJ_NAME IN ( '/NS/FG', '/NS/ZCL_BAR',"
        ),
        "{}",
        sent[12]
    );
}

#[test]
fn a_report_include_is_printed_as_its_master_program() {
    let (stdout, _, transport) = succeeded(
        &query("TTYP", "ZTT_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &["ZREP_TOP", "ZLONE"]),
            dd02l("WBCROSSI"),
            wbcrossi(&[("ZREP_TOP", "ZREP"), ("ZREP_TOP", "ZREP2")]),
            dd02l("TADIR"),
            tadir(&[("PROG", "ZREP", "ZP1"), ("PROG", "ZLONE", "ZP2")]),
        ],
    );

    // The include with two masters is listed under both; one that no master names stays itself.
    assert_eq!(
        stdout,
        "USED-BY PROG ZLONE package=ZP2 via=WBCROSSGT\n\
         USED-BY PROG ZREP package=ZP1 via=WBCROSSGT\n\
         USED-BY PROG ZREP2 package=? via=WBCROSSGT\n\
         WHERE-USED: 3"
    );
    assert_eq!(
        bodies(&transport)[3],
        "SELECT INCLUDE, MASTER FROM WBCROSSI WHERE INCLUDE IN ( 'ZLONE', 'ZREP_TOP' )"
    );
}

#[test]
fn a_master_that_is_a_class_pool_prints_as_its_class() {
    let master = class_include("ZCL_M", "CP");
    let (stdout, _, _) = succeeded(
        &query("TTYP", "ZTT_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &["ZREP_TOP"]),
            dd02l("WBCROSSI"),
            wbcrossi(&[("ZREP_TOP", master.as_str())]),
            dd02l("TADIR"),
            tadir(&[("CLAS", "ZCL_M", "ZP")]),
        ],
    );

    assert_eq!(
        stdout,
        "USED-BY CLAS ZCL_M package=ZP via=WBCROSSGT\nWHERE-USED: 1"
    );
}

#[test]
fn a_domain_reads_the_data_elements_of_dd04l_and_a_table_or_a_table_type_only_wbcrossgt() {
    let replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &["ZREPORT"]),
        dd02l("WBCROSSI"),
        wbcrossi(&[]),
        dd02l("DD04L"),
        column("ROLLNAME", &["ZDE_B", "ZDE_A"]),
        dd02l("TADIR"),
        tadir(&[("DTEL", "ZDE_A", "ZPACK"), ("PROG", "ZREPORT", "ZPACK2")]),
    ];
    let (stdout, _, transport) = succeeded(&query("DOMA", "ZDO_A"), replies);

    assert_eq!(
        stdout,
        "USED-BY DTEL ZDE_A package=ZPACK via=DD04L\n\
         USED-BY DTEL ZDE_B package=? via=DD04L\n\
         USED-BY PROG ZREPORT package=ZPACK2 via=WBCROSSGT\n\
         WHERE-USED: 3"
    );
    assert_eq!(
        bodies(&transport)[5],
        "SELECT ROLLNAME FROM DD04L WHERE DOMNAME = 'ZDO_A' AND AS4LOCAL = 'A'"
    );

    for token in ["TABL", "TTYP"] {
        let (stdout, _, transport) = succeeded(
            &query(token, "ZOBJ"),
            vec![
                ok(discovery(&[])),
                dd02l("WBCROSSGT"),
                column("INCLUDE", &["ZREPORT"]),
                dd02l("WBCROSSI"),
                wbcrossi(&[]),
                dd02l("TADIR"),
                tadir(&[("PROG", "ZREPORT", "ZPACK2")]),
            ],
        );

        assert_eq!(
            stdout,
            "USED-BY PROG ZREPORT package=ZPACK2 via=WBCROSSGT\nWHERE-USED: 1"
        );
        assert_eq!(bodies(&transport).len(), 6);
    }
}

#[test]
fn a_user_found_by_two_tables_is_listed_once_per_table() {
    let mut replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &["LZFGU01", "LZFGU02"]),
    ];

    replies.extend(trdir_check());
    replies.extend([
        column("NAME", &["SAPLZFG"]),
        dd02l("DD03L"),
        column("TABNAME", &["ZFG"]),
        dd02l("TADIR"),
        tadir(&[("FUGR", "ZFG", "ZP1"), ("TABL", "ZFG", "ZP2")]),
    ]);

    let (stdout, _, _) = succeeded(&query("DTEL", "ZDE_A"), replies);

    assert_eq!(
        stdout,
        "USED-BY FUGR ZFG package=ZP1 via=WBCROSSGT\n\
         USED-BY TABL ZFG package=ZP2 via=DD03L\n\
         WHERE-USED: 2"
    );
}

#[test]
fn a_cross_reference_list_longer_than_max_is_cut_before_the_packages_are_read() {
    let query = Query::new("TABL", "ZT", 2).unwrap();
    let replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &["ZC", "ZA", "ZB"]),
        dd02l("WBCROSSI"),
        wbcrossi(&[]),
        dd02l("TADIR"),
        tadir(&[
            ("PROG", "ZA", "PA"),
            ("PROG", "ZB", "PB"),
            ("PROG", "ZC", "PC"),
        ]),
    ];
    let (stdout, stderr, transport) = succeeded(&query, replies);

    assert_eq!(
        stdout,
        "USED-BY PROG ZA package=PA via=WBCROSSGT\n\
         USED-BY PROG ZB package=PB via=WBCROSSGT\n\
         WHERE-USED: 2"
    );
    assert!(
        stderr.contains("W: 3 objects found; the first 2 are shown (--max)"),
        "{stderr}"
    );
    assert!(
        bodies(&transport)[5].ends_with("OBJ_NAME IN ( 'ZA', 'ZB' )"),
        "{}",
        bodies(&transport)[5]
    );
}

#[test]
fn a_full_row_limit_and_a_long_list_of_objects_are_reported() {
    let includes: Vec<String> = (0..5000).map(|index| format!("ZPROG{index:04}")).collect();
    let refs: Vec<&str> = includes.iter().map(String::as_str).collect();
    let query = Query::new("TABL", "ZT", 5000).unwrap();
    let mut replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &refs),
        dd02l("WBCROSSI"),
    ];

    // Three master reads of 40 names; the rest stays as the includes are.
    for _ in 0..3 {
        replies.push(wbcrossi(&[]));
    }

    replies.push(dd02l("TADIR"));

    // Six package reads of 40 names; the rest keeps no package.
    for _ in 0..6 {
        replies.push(tadir(&[("PROG", "ZPROG0000", "ZP")]));
    }

    let (stdout, stderr, transport) = succeeded(&query, replies);

    assert!(
        stdout.starts_with("USED-BY PROG ZPROG0000 package=ZP via=WBCROSSGT\n"),
        "{}",
        &stdout[..80]
    );
    assert!(stdout.ends_with("WHERE-USED: 5000"));
    assert!(
        stderr.contains(
            "W: WBCROSSGT returned 5000 rows, the limit of one read; the list may be incomplete"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "W: the master programs of 4880 includes were not looked up; they show as themselves"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("W: the packages of 4760 objects were not looked up (package=?)"),
        "{stderr}"
    );
    assert_eq!(bodies(&transport).len(), 2 + 4 + 7);
}

/// The most one run asks for stays inside the 36 requests of a connection: the discovery, the
/// reads of every table, the four lookups of the `TRDIR` view, and the capped batches.
#[test]
fn the_largest_run_stays_within_the_requests_of_a_connection() {
    // 300 candidate groups: three `TRDIR` reads that find nothing, so each include stays a
    // program, three `WBCROSSI` reads, and six `TADIR` reads of the 300 names.
    let includes: Vec<String> = (0..300)
        .map(|index| format!("LZG{index:03}XYZU01"))
        .collect();
    let refs: Vec<&str> = includes.iter().map(String::as_str).collect();
    let mut replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &refs),
    ];

    replies.extend(trdir_check());
    replies.extend(std::iter::repeat_with(|| column("NAME", &[])).take(3));
    replies.push(dd02l("WBCROSSI"));
    replies.extend(std::iter::repeat_with(|| wbcrossi(&[])).take(3));
    replies.extend([dd02l("DD03L"), column("TABNAME", &[]), dd02l("TADIR")]);
    replies.extend(std::iter::repeat_with(|| tadir(&[])).take(6));

    let (stdout, _, transport) = succeeded(&Query::new("DTEL", "ZDE_A", 5000).unwrap(), replies);

    assert!(stdout.ends_with("WHERE-USED: 300"), "{stdout}");
    assert_eq!(transport.calls.len(), 1 + 2 + 7 + 4 + 2 + 7);
    assert!(transport.calls.len() <= 36);
}

#[test]
fn names_of_forty_characters_fit_a_list_of_forty() {
    let names: Vec<String> = (0..40).map(|index| format!("Z{index:039}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (stdout, _, transport) = succeeded(
        &query("TTYP", "ZTT_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &refs),
            dd02l("WBCROSSI"),
            wbcrossi(&[]),
            dd02l("TADIR"),
            tadir(&[("PROG", names[0].as_str(), "ZP")]),
        ],
    );

    assert!(stdout.ends_with("WHERE-USED: 40"), "{stdout}");

    let sent = bodies(&transport);

    // Each list is one read of all 40 names, and the longest condition stays below 2000.
    for body in [&sent[3], &sent[5]] {
        assert!(body.matches("'Z0").count() >= 40, "{body}");
    }

    let condition = sent[5].split(" WHERE ").nth(1).unwrap();

    assert!(condition.len() < 2000, "{}", condition.len());
    assert!(condition.len() > 1800, "{}", condition.len());
}

#[test]
fn a_package_read_that_fails_leaves_the_users_with_a_warning() {
    let denied = {
        let mut policies = Policies::default();

        policies.read_table_denied.push("TADIR".to_owned());

        policies
    };
    let found = || {
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &["ZR"]),
            dd02l("WBCROSSI"),
            wbcrossi(&[]),
        ]
    };
    let expected = "USED-BY PROG ZR package=? via=WBCROSSGT\nWHERE-USED: 1";

    for (replies, policies, reason) in [
        (
            [
                found(),
                vec![dd02l("TADIR"), Ok(Response::text(500, "<x/>"))],
            ]
            .concat(),
            Policies::default(),
            "HTTP 500",
        ),
        (
            [found(), vec![dd02l("TADIR"), column("OTHER", &["X"])]].concat(),
            Policies::default(),
            "the TADIR reply lacks OBJECT, OBJ_NAME or DEVCLASS",
        ),
        (
            found(),
            denied,
            "table TADIR is on the read_table_denied list",
        ),
    ] {
        let mut transport = ScriptedTransport::replies(replies);
        let outcome = run(&mut transport, &query("TTYP", "ZTT_A"), &policies).unwrap();

        assert_eq!(outcome.stdout, expected, "{reason}");
        assert!(
            outcome
                .stderr
                .contains(&format!("W: the packages could not be read ({reason}")),
            "{}",
            outcome.stderr
        );
        assert!(
            outcome.stderr.ends_with("); package=?"),
            "{}",
            outcome.stderr
        );
    }
}

#[test]
fn a_table_on_the_deny_list_or_unknown_to_the_dictionary_is_a_refusal() {
    let mut policies = Policies::default();

    policies.read_table_denied.push("WBCROSSGT".to_owned());

    let mut transport = ScriptedTransport::replies(vec![ok(discovery(&[]))]);
    let outcome = run(&mut transport, &query("DTEL", "ZDE_A"), &policies);

    assert!(
        matches!(&outcome, Err(PreviewError::Refused(message)) if message.contains("table WBCROSSGT")),
        "{outcome:?}"
    );
    // Only the discovery left the process.
    assert_eq!(transport.calls.len(), 1);

    let (outcome, transport) = execute(
        &query("DTEL", "ZDE_A"),
        vec![
            ok(discovery(&[])),
            ok(preview(&[
                ("TABNAME", &[]),
                ("TABCLASS", &[]),
                ("VIEWCLASS", &[]),
            ])),
        ],
    );

    assert!(
        matches!(&outcome, Err(PreviewError::Refused(_))),
        "{outcome:?}"
    );
    assert_eq!(transport.calls.len(), 2);
}

#[test]
fn a_reply_without_the_column_and_a_failed_read_are_errors() {
    let (outcome, _) = execute(
        &query("DTEL", "ZDE_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("OTHER", &["X"]),
        ],
    );

    assert!(
        matches!(&outcome, Err(PreviewError::Rejected(message)) if message == "the WBCROSSGT reply has no INCLUDE column"),
        "{outcome:?}"
    );

    let (outcome, _) = execute(
        &query("DTEL", "ZDE_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            Ok(Response::text(500, "<x/>")),
        ],
    );

    assert!(
        matches!(&outcome, Err(PreviewError::Failed { .. })),
        "{outcome:?}"
    );

    let (outcome, _) = execute(
        &query("TTYP", "ZTT_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &["ZR"]),
            dd02l("WBCROSSI"),
            column("INCLUDE", &["ZR"]),
        ],
    );

    assert!(
        matches!(&outcome, Err(PreviewError::Rejected(message)) if message == "the WBCROSSI reply lacks INCLUDE or MASTER"),
        "{outcome:?}"
    );
}

// ---- reply size and repeats -----------------------------------------------------------------

/// A usage reply of `count` program entries of about 1.5 KB each.
fn big_usage(count: usize) -> String {
    let mut xml = String::from(
        r#"<usageReferences:usageReferenceResult xmlns:usageReferences="u" xmlns:adtcore="c" numberOfResults="1"><usageReferences:referencedObjects>"#,
    );

    for index in 0..count {
        xml.push_str(&format!(
            r#"<usageReferences:referencedObject uri="/sap/bc/adt/programs/programs/zp{index:06}" parentUri="/sap/bc/adt/packages/zpack" isResult="false" canHaveChildren="true" usageInformation="gradeDirect,includeProductive"><usageReferences:adtObject adtcore:responsible="SAP" adtcore:name="ZP{index:06}" adtcore:type="PROG/P" adtcore:description="{}" xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zpack" adtcore:type="DEVC/K" adtcore:name="ZPACK"/></usageReferences:adtObject><objectIdentifier>ABAPFullName;ZP{index:06};ZP{index:06};\TY:BAPIRET2;2</objectIdentifier></usageReferences:referencedObject>"#,
            "d".repeat(1000)
        ));
    }

    xml.push_str("</usageReferences:referencedObjects></usageReferences:usageReferenceResult>");

    xml
}

#[test]
fn a_reply_of_thirty_megabytes_is_read_and_max_cuts_the_output() {
    let xml = big_usage(20_000);

    assert!(xml.len() > 20 << 20, "{}", xml.len());

    let query = Query::new("TABL", "BAPIRET2", 3).unwrap();
    let (stdout, stderr, _) = succeeded(
        &query,
        vec![
            ok(discovery(&["/sap/bc/adt/ddic/structures"])),
            dd02l_class("BAPIRET2", "INTTAB"),
            ok(xml),
        ],
    );

    assert_eq!(
        stdout,
        "USED-BY PROG ZP000000 package=ZPACK via=ADT\n\
         USED-BY PROG ZP000001 package=ZPACK via=ADT\n\
         USED-BY PROG ZP000002 package=ZPACK via=ADT\n\
         WHERE-USED: 3"
    );
    assert_eq!(
        stderr,
        "W: 20000 objects found; the first 3 are shown (--max)"
    );
}

#[test]
fn a_reply_over_the_size_limit_is_rejected_with_its_size() {
    use abapreach::operation::where_used::MAX_REPLY_BYTES;

    let body = " ".repeat(MAX_REPLY_BYTES + 1);
    let (outcome, _) = execute(&query("PROG", "ZP"), vec![ok(body)]);

    match outcome {
        Err(PreviewError::Rejected(message)) => {
            assert!(
                message.contains(&format!("{} bytes", MAX_REPLY_BYTES + 1)),
                "{message}"
            );
            assert!(message.contains("--max cannot help"), "{message}");
        }
        other => panic!("{other:?}"),
    }

    // The limit itself is still read.
    let (stdout, _, _) = succeeded(
        &query("PROG", "ZP"),
        vec![ok(format!(
            "{}{}",
            fixture("usage-prog-740.xml"),
            " ".repeat(MAX_REPLY_BYTES - fixture("usage-prog-740.xml").len())
        ))],
    );

    assert_eq!(stdout, "WHERE-USED: 0");
}

#[test]
fn repeats_that_differ_only_in_the_package_keep_the_one_that_names_it() {
    let xml = r#"<r xmlns:adtcore="c">
        <referencedObject uri="/a/1"><adtObject adtcore:name="ZA" adtcore:type="PROG/P"/></referencedObject>
        <referencedObject uri="/a/2"><adtObject adtcore:name="ZA" adtcore:type="PROG/P"><packageRef adtcore:name="ZP"/></adtObject></referencedObject>
        <referencedObject uri="/a/3"><adtObject adtcore:name="ZA" adtcore:type="PROG/P"/></referencedObject>
        <referencedObject uri="/b/1"><adtObject adtcore:name="ZB" adtcore:type="PROG/P"/></referencedObject>
        </r>"#;
    let (stdout, _, _) = succeeded(&query("PROG", "ZP"), vec![ok(xml)]);

    assert_eq!(
        stdout,
        "USED-BY PROG ZA package=ZP via=ADT\nUSED-BY PROG ZB package=? via=ADT\nWHERE-USED: 2"
    );
}

#[test]
fn a_parse_error_text_is_printed_without_control_characters() {
    let (outcome, _) = execute(&query("PROG", "ZP"), vec![ok("<a>\u{202e}<b></a>")]);

    match outcome {
        Err(PreviewError::Rejected(message)) => {
            assert!(
                message.starts_with("cannot read the where-used reply: "),
                "{message}"
            );
            assert!(
                !message.contains('\u{202e}') && !message.contains('\n'),
                "{message:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

// ---- one request budget, pool names, cleaned lines, hostile names ----------------------------

/// The dictionary check of a `TRDIR` view over `bases` base tables: the view, its generated CDS
/// views, its base tables, and one lookup per base table.
fn wide_trdir_check(bases: usize) -> Vec<Result<Response, TransportError>> {
    let names: Vec<String> = std::iter::once("REPOSRC".to_owned())
        .chain((1..bases).map(|index| format!("ZBASE{index:02}")))
        .collect();
    let refs: Vec<Option<&str>> = names.iter().map(|name| Some(name.as_str())).collect();
    let views: Vec<Option<&str>> = names.iter().map(|_| Some("TRDIR")).collect();
    let positions: Vec<Option<&str>> = names.iter().map(|_| Some("0001")).collect();
    let mut replies = vec![
        ok(preview(&[
            ("TABNAME", &[Some("TRDIR")]),
            ("TABCLASS", &[Some("VIEW")]),
            ("VIEWCLASS", &[Some("D")]),
        ])),
        ok(preview(&[
            ("DDLNAME", &[]),
            ("OBJECTNAME", &[]),
            ("OBJECTTYPE", &[]),
        ])),
        ok(preview(&[
            ("VIEWNAME", &views),
            ("TABNAME", &refs),
            ("TABPOS", &positions),
        ])),
    ];

    replies.extend(names.iter().map(|name| dd02l(name)));

    replies
}

/// The run of the largest candidate list over a wide `TRDIR` view: the three groups of reads
/// each take their cap.
fn wide_run_replies(bases: usize) -> Vec<Result<Response, TransportError>> {
    let includes: Vec<String> = (0..300)
        .map(|index| format!("LZG{index:03}XYZU01"))
        .collect();
    let refs: Vec<&str> = includes.iter().map(String::as_str).collect();
    let mut replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &refs),
    ];

    replies.extend(wide_trdir_check(bases));
    replies.extend(std::iter::repeat_with(|| column("NAME", &[])).take(3));
    replies.push(dd02l("WBCROSSI"));
    replies.extend(std::iter::repeat_with(|| wbcrossi(&[])).take(3));
    replies.extend([dd02l("DD03L"), column("TABNAME", &[]), dd02l("TADIR")]);
    replies.extend(std::iter::repeat_with(|| tadir(&[])).take(6));

    replies
}

#[test]
fn a_wide_trdir_view_spends_the_one_budget_and_the_packages_take_what_is_left() {
    // 1 discovery, 2 for WBCROSSGT, 24 lookups and 3 reads of TRDIR, 4 for WBCROSSI and 2 for
    // DD03L: 36 requests, so the package reads have none left and say so.
    let mut replies = wide_run_replies(21);

    replies.truncate(36);

    let (stdout, stderr, transport) =
        succeeded(&Query::new("DTEL", "ZDE_A", 5000).unwrap(), replies);

    assert_eq!(transport.calls.len(), 36);
    assert!(stdout.ends_with("WHERE-USED: 300"), "{stdout}");
    assert!(stdout.contains("package=?"), "{stdout}");
    assert!(
        stderr.contains("W: the packages of 300 objects were not looked up: the request limit of one connection is reached (package=?)"),
        "{stderr}"
    );
}

#[test]
fn a_run_that_needs_a_request_past_the_budget_is_rejected_and_prints_nothing() {
    use abapreach::operation::where_used::{REQUEST_BUDGET, run_within};

    assert_eq!(REQUEST_BUDGET, 36);

    for limit in [1, 10, 27, 35] {
        let mut transport = ScriptedTransport::replies(wide_run_replies(21));
        let outcome = run_within(
            &mut transport,
            &Query::new("DTEL", "ZDE_A", 5000).unwrap(),
            &Policies::default(),
            limit,
        );

        match outcome {
            Err(PreviewError::Rejected(message)) => {
                assert!(
                    message.starts_with(&format!(
                        "where-used needs more than {limit} requests on one connection"
                    )),
                    "{message}"
                );
                assert!(message.contains("nothing is printed"), "{message}");
            }
            other => panic!("{limit}: {other:?}"),
        }

        // The request past the budget never left.
        assert_eq!(transport.calls.len(), limit, "{limit}");
    }

    // A view of 22 base tables is over the lookups of one dictionary check, whatever the budget.
    let mut replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &["LZFGU01"]),
    ];

    replies.extend(wide_trdir_check(22));

    let (outcome, _) = execute(&query("DTEL", "ZDE_A"), replies);

    assert!(
        matches!(&outcome, Err(PreviewError::Refused(message)) if message.contains("more than 24 lookups")),
        "{outcome:?}"
    );
}

#[test]
fn an_include_is_a_pool_only_when_its_name_is_padded_to_30_and_ends_in_a_known_suffix() {
    let method = class_include("ZCL_M", "CM00A");
    let short_method = class_include("ZCL_M", "CM01");
    let unknown = class_include("ZCL_U", "XX");
    let lower = class_include("ZCL_L", "cp");
    let narrow = format!("{:=<29}CP", "ZCL_N");
    let wide = format!("{:=<31}CP", "ZCL_W");
    let thirty = format!("{}CP", "Z".repeat(30));
    let includes = [
        method.as_str(),
        short_method.as_str(),
        unknown.as_str(),
        lower.as_str(),
        narrow.as_str(),
        wide.as_str(),
        thirty.as_str(),
        "ZREPORT=BAD",
        "ZREPORT=CP",
        "=CP",
    ];
    let (stdout, _, transport) = succeeded(
        &query("TTYP", "ZTT_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &includes),
            dd02l("WBCROSSI"),
            wbcrossi(&[]),
            dd02l("TADIR"),
            tadir(&[("CLAS", "ZCL_M", "ZP")]),
        ],
    );
    let mut expected: Vec<String> = [
        "CLAS ZCL_M package=ZP".to_owned(),
        format!("PROG {thirty} package=?"),
    ]
    .into_iter()
    .chain(
        [&short_method, &unknown, &lower, &narrow, &wide]
            .map(|include| format!("PROG {include} package=?")),
    )
    .chain(
        [
            "PROG =CP package=?",
            "PROG ZREPORT=BAD package=?",
            "PROG ZREPORT=CP package=?",
        ]
        .map(str::to_owned),
    )
    .collect();

    expected.sort();

    let mut printed: Vec<&str> = stdout.lines().collect();
    let count = printed.pop().unwrap();

    printed.sort();

    assert_eq!(count, "WHERE-USED: 10");
    assert_eq!(
        printed,
        expected
            .iter()
            .map(|line| format!("USED-BY {line} via=WBCROSSGT"))
            .collect::<Vec<_>>()
    );
    // Only the plain name reached the master read: no `=` and no quote is in any list.
    assert_eq!(
        bodies(&transport)[3],
        format!("SELECT INCLUDE, MASTER FROM WBCROSSI WHERE INCLUDE IN ( '{thirty}' )")
    );
}

#[test]
fn a_hit_that_prints_the_same_counts_once_after_the_cleaning() {
    let xml = "<r xmlns:adtcore=\"c\">\
        <referencedObject uri=\"/a/1\"><adtObject adtcore:name=\"Z\u{200b}A\" adtcore:type=\"PROG/P\"><packageRef adtcore:name=\"ZP\"/></adtObject></referencedObject>\
        <referencedObject uri=\"/a/2\"><adtObject adtcore:name=\"ZA\" adtcore:type=\"PROG/P\"/></referencedObject>\
        <referencedObject uri=\"/a/3\"><adtObject adtcore:name=\"Z\u{202e} A\" adtcore:type=\"PROG/P\"/></referencedObject>\
        <referencedObject uri=\"/a/4\"><adtObject adtcore:name=\"\u{200b}\" adtcore:type=\"PROG/P\"/></referencedObject>\
        </r>";
    let (stdout, _, _) = succeeded(&query("PROG", "ZP"), vec![ok(xml)]);

    // `ZA` once, with the package of the entry that names one; `Z A` is another name; the
    // entry that is nothing but a format character has no name and is dropped.
    assert_eq!(
        stdout,
        "USED-BY PROG Z A package=? via=ADT\nUSED-BY PROG ZA package=ZP via=ADT\nWHERE-USED: 2"
    );

    let query = Query::new("PROG", "ZP", 1).unwrap();
    let (stdout, stderr, _) = succeeded(&query, vec![ok(xml)]);

    assert_eq!(stdout, "USED-BY PROG Z A package=? via=ADT\nWHERE-USED: 1");
    assert_eq!(stderr, "W: 2 objects found; the first 1 are shown (--max)");
}

#[test]
fn a_name_from_the_server_with_a_quote_or_sql_is_never_put_into_a_read() {
    let hostile = [
        "ZR'; DROP TABLE USR02; --",
        "ZR' OR '1'='1",
        "LZ'XU01",
        "ZR\" OR 1=1",
        "ZR\nUNION",
        "LZFGU01'",
    ];
    let mut includes = hostile.to_vec();

    includes.push("ZGOOD");

    let (stdout, _, transport) = succeeded(
        &query("DTEL", "ZDE_A"),
        vec![
            ok(discovery(&[])),
            dd02l("WBCROSSGT"),
            column("INCLUDE", &includes),
            dd02l("WBCROSSI"),
            wbcrossi(&[]),
            dd02l("DD03L"),
            column("TABNAME", &["ZT'; DROP", "ZT_OK"]),
            dd02l("TADIR"),
            tadir(&[("PROG", "ZGOOD", "ZP"), ("TABL", "ZT_OK", "ZP")]),
        ],
    );

    for body in bodies(&transport) {
        for bad in ["DROP", "'1'", "UNION", "ZR'", "LZ'", "\"", "1=1", "ZT'"] {
            assert!(!body.contains(bad), "{bad} in {body}");
        }
    }

    assert_eq!(
        bodies(&transport)[3],
        "SELECT INCLUDE, MASTER FROM WBCROSSI WHERE INCLUDE IN ( 'ZGOOD' )"
    );
    // They are only listed, as text.
    assert!(stdout.contains("USED-BY PROG ZGOOD package=ZP"), "{stdout}");
    assert!(
        stdout.contains("USED-BY PROG ZR'; DROP TABLE USR02; -- package=?"),
        "{stdout}"
    );
    assert!(
        stdout.contains("USED-BY TABL ZT_OK package=ZP via=DD03L"),
        "{stdout}"
    );
}

#[test]
fn reposrc_on_the_deny_list_refuses_the_trdir_view() {
    let mut policies = Policies::default();

    policies.read_table_denied.push("REPOSRC".to_owned());

    let mut replies = vec![
        ok(discovery(&[])),
        dd02l("WBCROSSGT"),
        column("INCLUDE", &["LZFGU01"]),
    ];

    replies.extend(trdir_check().into_iter().take(3));

    let mut transport = ScriptedTransport::replies(replies);
    let outcome = run(&mut transport, &query("DTEL", "ZDE_A"), &policies);

    assert!(
        matches!(&outcome, Err(PreviewError::Refused(message)) if message.contains("view TRDIR reads table REPOSRC (read_table_denied)")),
        "{outcome:?}"
    );
    // The lookups of the view left, the TRDIR rows did not.
    assert_eq!(transport.calls.len(), 3 + 3);
}
