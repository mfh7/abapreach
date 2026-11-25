//! `search`: the pattern and type rules, the request, and the replies of the three releases
//! (HD1 750, FED 740, S4D 758, from `tests/fixtures/m9`).

mod common;

use abapreach::backend::Failure;
use abapreach::operation::quality::QualityError;
use abapreach::operation::search::{self, DEFAULT_MAX, Query, TYPES, validate_pattern};
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, fixture_path_in};
use std::fs;

const SEARCH: &str = "/sap/bc/adt/repository/informationsystem/search";

fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_path_in("m9", name)).expect("read fixture")
}

fn query(pattern: &str, type_token: Option<&str>, max: u32) -> Query {
    Query::new(pattern, type_token, max).unwrap_or_else(|error| panic!("{pattern:?}: {error}"))
}

fn run(
    reply: Result<Response, TransportError>,
    query: &Query,
) -> (
    Result<abapreach::domain::Outcome, QualityError>,
    ScriptedTransport,
) {
    let mut transport = ScriptedTransport::replies(vec![reply]);
    let outcome = search::run(&mut transport, query);

    (outcome, transport)
}

fn ok(body: String) -> Result<Response, TransportError> {
    Ok(Response::text(200, body))
}

#[test]
fn a_pattern_is_upper_cased_and_limited_to_name_characters_and_one_wildcard() {
    let longest = "Z".repeat(40);

    for (given, canonical) in [
        ("ZCL_ABAP", "ZCL_ABAP"),
        ("zcl_abap", "ZCL_ABAP"),
        ("Z*", "Z*"),
        ("*", "*"),
        ("*CL*TYPE*", "*CL*TYPE*"),
        ("/NS/ZCL_X", "/NS/ZCL_X"),
        ("/ns/zcl_*", "/NS/ZCL_*"),
        ("/NS/*", "/NS/*"),
        ("Z1_2", "Z1_2"),
        (longest.as_str(), longest.as_str()),
    ] {
        assert_eq!(validate_pattern(given).as_deref(), Ok(canonical), "{given}");
    }
}

#[test]
fn a_pattern_outside_the_rules_is_refused() {
    for pattern in [
        "",
        " ",
        "ZCL ABAP",
        " ZCL",
        "ZCL\t",
        "ZCL\n",
        "ZC%L",
        "ZCL%2A",
        "ZCL?",
        "ZCL+",
        "ZCL-X",
        "ZCL.X",
        "ZCL&X",
        "ZCL=X",
        "ZCL'X",
        "ZCL\"X",
        "ZCL;X",
        "ZCL#X",
        "ZCL~X",
        "$TMP",
        "ZCLÄ",
        // A slash outside the namespace form.
        "/",
        "//",
        "/NS",
        "/NS/",
        "NS/ZCL",
        "ZCL/",
        "/NS/ZCL/X",
        "/NS//ZCL",
        "//NS/ZCL",
        "/N*S/ZCL",
        "/NS*/ZCL",
        "/ABCDEFGHIJK/ZCL",
        &"Z".repeat(41),
        &format!("/NS/{}", "Z".repeat(37)),
    ] {
        assert!(validate_pattern(pattern).is_err(), "{pattern:?}");
    }
}

#[test]
fn a_type_is_one_of_the_closed_list_in_any_case() {
    assert_eq!(
        TYPES.map(|(token, _)| token),
        [
            "PROG", "FUNC", "FUGR", "CLAS", "INTF", "DTEL", "DOMA", "TABL", "TTYP", "TRAN", "DDLS",
            "MSAG"
        ]
    );

    for (token, adt) in TYPES {
        let encoded = adt.replace('/', "%2F");

        assert_eq!(
            query("Z*", Some(token), 7).uri(),
            format!("{SEARCH}?operation=quickSearch&query=Z*&maxResults=7&objectType={encoded}")
        );
        assert_eq!(
            query("Z*", Some(&token.to_ascii_lowercase()), 7).uri(),
            query("Z*", Some(token), 7).uri()
        );
    }

    for token in ["BOGUS", "", "CLAS/OC", "CLAS ", "PROG/P", "*", "FUGR/FF"] {
        let error = Query::new("Z*", Some(token), 5).unwrap_err();

        assert!(error.starts_with("unknown type"), "{token:?}: {error}");
        assert!(error.contains("PROG, FUNC, FUGR, CLAS"), "{error}");
    }
}

#[test]
fn the_number_of_results_is_between_one_and_a_thousand() {
    assert_eq!(DEFAULT_MAX, 50);

    for max in [1, 50, 1000] {
        assert!(Query::new("Z*", None, max).is_ok(), "{max}");
    }

    for max in [0, 1001, u32::MAX] {
        assert!(Query::new("Z*", None, max).is_err(), "{max}");
    }
}

#[test]
fn the_request_is_the_untyped_or_typed_quick_search_in_one_order() {
    let untyped = query("zcl_abap*", None, 50);

    assert_eq!(
        untyped.uri(),
        format!("{SEARCH}?operation=quickSearch&query=ZCL_ABAP*&maxResults=50")
    );
    assert_eq!(untyped.pattern(), "ZCL_ABAP*");
    assert_eq!(untyped.max(), 50);
    assert_eq!(
        query("/ns/zcl_*", Some("clas"), 1000).uri(),
        format!(
            "{SEARCH}?operation=quickSearch&query=/NS/ZCL_*&maxResults=1000&objectType=CLAS%2FOC"
        )
    );

    let (outcome, transport) = run(ok(fixture("search-clas-750.xml")), &untyped);

    outcome.unwrap();
    assert_eq!(transport.calls.len(), 1);
    assert_eq!(transport.calls[0].method, "GET");
    assert_eq!(transport.calls[0].uri, untyped.uri());
}

#[test]
fn a_typed_search_prints_one_line_per_object_on_every_release() {
    for (release, description) in [
        ("740", "Class"),
        ("750", "Class"),
        ("758", "Runtime Type Services"),
    ] {
        let (outcome, _) = run(
            ok(fixture(&format!("search-clas-{release}.xml"))),
            &query("CL_ABAP_TYPEDESCR", Some("CLAS"), 50),
        );
        let outcome = outcome.unwrap_or_else(|error| panic!("{release}: {error:?}"));

        assert_eq!(
            outcome.stdout,
            format!("CLAS/OC CL_ABAP_TYPEDESCR package=SABP_RTTI {description}\nSEARCH: 1 object"),
            "{release}"
        );
        assert_eq!(outcome.stderr, "", "{release}");
        assert_eq!((outcome.exit, outcome.status), (0, None), "{release}");
    }
}

#[test]
fn an_untyped_search_prints_every_object_the_release_answers() {
    let expected = [
        (
            "740",
            "TABL/DS RFC_SYSTEM_IDENTITY package=SQRFC1 Structure\n\
             FUGR/FF RFC_SYSTEM_INFO package=SRCX Function Module\n\
             TABL/DS RFC_SYSTEM_INFO package=SOAP_CONFIG Structure\n\
             TABL/DS RFC_SYSTEM_INFO_RESPONSE package=SOAP_CONFIG Structure\n\
             SEARCH: 4 objects",
        ),
        (
            "750",
            "TABL/DS RFC_SYSTEM_IDENTITY package=SQRFC1 Structure\n\
             FUGR/FF RFC_SYSTEM_INFO package=SRCX Function Module\n\
             TABL/DS RFC_SYSTEM_INFO package=SOAP_CONFIG Structure\n\
             FUGR/FF RFC_SYSTEM_INFO_FASTSER package=SRCX Function Module\n\
             TABL/DS RFC_SYSTEM_INFO_RESPONSE package=SOAP_CONFIG Structure\n\
             SEARCH: 5 objects",
        ),
        (
            "758",
            "TABL/DS RFC_SYSTEM_IDENTITY package=SQRFC1 System ID\n\
             TABL/DS RFC_SYSTEM_INFO package=SOAP_CONFIG Proxy Structure (Generated)\n\
             FUGR/FF RFC_SYSTEM_INFO package=SRCX Liefert versch. Informationen über das System.\n\
             SUSH RFC_SYSTEM_INFO RF package=SRCX RFC_SYSTEM_INFO\n\
             TABL/DS RFC_SYSTEM_INFO_RESPONSE package=SOAP_CONFIG Proxy Structure (generated)\n\
             SEARCH: 5 objects",
        ),
    ];

    for (release, text) in expected {
        let (outcome, _) = run(
            ok(fixture(&format!("search-untyped-{release}.xml"))),
            &query("RFC_SYSTEM_I*", None, 50),
        );
        let outcome = outcome.unwrap_or_else(|error| panic!("{release}: {error:?}"));

        assert_eq!(outcome.stdout, text, "{release}");
        assert_eq!(outcome.stderr, "", "{release}");
    }
}

#[test]
fn a_release_that_ignores_the_type_filter_is_filtered_locally_with_a_warning() {
    for (release, kept, left_out) in [("740", 1, 3), ("750", 2, 3), ("758", 1, 4)] {
        let (outcome, _) = run(
            ok(fixture(&format!("search-untyped-{release}.xml"))),
            &query("RFC_SYSTEM_I*", Some("FUNC"), 50),
        );
        let outcome = outcome.unwrap_or_else(|error| panic!("{release}: {error:?}"));

        assert_eq!(
            outcome.stdout.lines().last(),
            Some(if kept == 1 {
                "SEARCH: 1 object"
            } else {
                "SEARCH: 2 objects"
            }),
            "{release}"
        );
        assert!(
            outcome
                .stdout
                .lines()
                .filter(|line| !line.starts_with("SEARCH:"))
                .all(|line| line.starts_with("FUGR/FF ")),
            "{release}: {}",
            outcome.stdout
        );
        assert_eq!(
            outcome.stderr,
            format!(
                "W: the reply held objects of other types; {left_out} left out (the result may hold fewer than --max objects)"
            ),
            "{release}"
        );
        assert_eq!(outcome.exit, 0);
    }
}

#[test]
fn a_typed_search_whose_reply_matches_the_type_has_no_warning() {
    let (outcome, _) = run(
        ok(fixture("search-clas-758.xml")),
        &query("CL_ABAP_TYPEDESCR", Some("CLAS"), 50),
    );

    assert_eq!(outcome.unwrap().stderr, "");

    // A type that no object of the reply has: every object is of another type.
    let (outcome, _) = run(
        ok(fixture("search-clas-758.xml")),
        &query("CL_ABAP_TYPEDESCR", Some("INTF"), 50),
    );
    let outcome = outcome.unwrap();

    assert_eq!(outcome.stdout, "SEARCH: 0 objects");
    assert!(
        outcome
            .stderr
            .starts_with("W: the reply held objects of other types")
    );
}

#[test]
fn no_hit_is_exit_zero_and_a_longer_reply_is_cut_at_the_maximum() {
    let empty = r#"<?xml version="1.0" encoding="utf-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"/>"#;
    let (outcome, _) = run(ok(empty.to_owned()), &query("ZNOTHING*", None, 50));
    let outcome = outcome.unwrap();

    assert_eq!(outcome.stdout, "SEARCH: 0 objects");
    assert_eq!((outcome.exit, outcome.stderr.as_str()), (0, ""));

    let (outcome, _) = run(
        ok(fixture("search-untyped-750.xml")),
        &query("RFC_SYSTEM_I*", None, 2),
    );
    let stdout = outcome.unwrap().stdout;

    assert_eq!(stdout.lines().count(), 3, "{stdout}");
    assert!(stdout.ends_with("SEARCH: 2 objects"), "{stdout}");
}

#[test]
fn an_object_without_package_or_description_and_a_padded_name_stay_one_line() {
    let reply = r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:type="PROG/P" adtcore:name="ZPROG"/><adtcore:objectReference adtcore:type="PROG/P" adtcore:name="ZB" adtcore:packageName="" adtcore:description="two&#10;lines  here"/><adtcore:objectReference adtcore:type="" adtcore:name="NOTYPE"/><adtcore:objectReference adtcore:type="PROG/P" adtcore:name=""/></adtcore:objectReferences>"#;
    let (outcome, _) = run(ok(reply.to_owned()), &query("Z*", None, 50));

    assert_eq!(
        outcome.unwrap().stdout,
        "PROG/P ZPROG package=?\nPROG/P ZB package=? two lines here\nSEARCH: 2 objects"
    );
}

#[test]
fn control_characters_other_than_whitespace_are_dropped_from_every_printed_field() {
    let reply = "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference adtcore:type=\"PROG/P\" adtcore:name=\"Z&#27;P\" adtcore:packageName=\"ZP&#7;K\" adtcore:description=\"a&#27;[31mbc\"/></adtcore:objectReferences>";
    let (outcome, _) = run(ok(reply.to_owned()), &query("Z*", None, 50));
    let stdout = outcome.unwrap().stdout;

    assert_eq!(stdout, "PROG/P ZP package=ZPK a[31mbc\nSEARCH: 1 object");
    assert!(!stdout.chars().any(|c| c.is_control() && c != '\n'));
}

/// A server that answers more than the type filter asks for: the extra types are dropped and the
/// warning says how many.
#[test]
fn a_typed_reply_with_objects_of_other_types_drops_them_and_warns() {
    for (token, wanted, extra, extra_name) in [
        ("PROG", "PROG/P", "SOBJ/P", "ZSOBJ"),
        ("TABL", "TABL/DT", "TABL/DS", "ZSTRUCT"),
    ] {
        let reply = format!(
            "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference adtcore:type=\"{wanted}\" adtcore:name=\"Z_WANTED\" adtcore:packageName=\"ZP\" adtcore:description=\"kept\"/><adtcore:objectReference adtcore:type=\"{extra}\" adtcore:name=\"{extra_name}\" adtcore:packageName=\"ZP\" adtcore:description=\"extra\"/></adtcore:objectReferences>"
        );
        let (outcome, _) = run(ok(reply), &query("Z*", Some(token), 50));
        let outcome = outcome.unwrap();

        assert_eq!(
            outcome.stdout,
            format!("{wanted} Z_WANTED package=ZP kept\nSEARCH: 1 object"),
            "{token}"
        );
        assert_eq!(
            outcome.stderr,
            "W: the reply held objects of other types; 1 left out (the result may hold fewer than --max objects)",
            "{token}"
        );
        assert_eq!(outcome.exit, 0);
    }
}

#[test]
fn a_reply_that_is_not_xml_is_rejected_and_nothing_is_printed() {
    let (outcome, _) = run(ok("<a><b></a>".to_owned()), &query("Z*", None, 50));

    match outcome {
        Err(QualityError::Rejected(message)) => {
            assert!(
                message.starts_with("cannot read the search reply"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_status_other_than_200_is_a_failure_without_a_fallback_class() {
    for (status, hint) in [(404, true), (500, false), (403, false)] {
        let (outcome, _) = run(
            Ok(Response::text(status, "<html/>")),
            &query("Z*", None, 50),
        );

        match outcome {
            Err(QualityError::Failed { failure, message }) => {
                assert_eq!(failure, Failure::Validation, "{status}");
                assert!(
                    message.starts_with(&format!("the object search answered HTTP {status}")),
                    "{message}"
                );
                assert_eq!(message.contains("does not offer"), hint, "{message}");
            }
            other => panic!("{status}: {other:?}"),
        }
    }
}

#[test]
fn a_transport_failure_keeps_its_routing_class() {
    let (outcome, _) = run(
        Err(TransportError::PreSendConnectivity("no route".to_owned())),
        &query("Z*", None, 50),
    );

    match outcome {
        Err(QualityError::Failed { failure, .. }) => {
            assert_eq!(failure, Failure::PreSendConnectivity);
        }
        other => panic!("{other:?}"),
    }
}

fn reply_of(objects: &[(&str, &str)]) -> String {
    let references: String = objects
        .iter()
        .map(|(kind, name)| {
            format!(
                "<adtcore:objectReference adtcore:type=\"{kind}\" adtcore:name=\"{name}\" adtcore:packageName=\"ZP\" adtcore:description=\"d\"/>"
            )
        })
        .collect();

    format!(
        "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\">{references}</adtcore:objectReferences>"
    )
}

const NAME_WARNING: &str = "W: the reply held objects whose name does not match the pattern; 1 left out (the result may hold fewer than --max objects)";

/// A pattern without `*` is the whole name, whatever the server answers.
#[test]
fn an_exact_pattern_keeps_only_the_object_of_that_name() {
    let reply = reply_of(&[("PROG/P", "ZFOO"), ("PROG/P", "ZFOOBAR")]);
    let (outcome, _) = run(ok(reply), &query("zfoo", None, 50));
    let outcome = outcome.unwrap();

    assert_eq!(outcome.stdout, "PROG/P ZFOO package=ZP d\nSEARCH: 1 object");
    assert_eq!(outcome.stderr, NAME_WARNING);

    // The namespace is part of the name.
    let reply = reply_of(&[("CLAS/OC", "/NS/ZCL_A"), ("CLAS/OC", "ZCL_A")]);
    let (outcome, _) = run(ok(reply), &query("/ns/zcl_a", None, 50));

    assert_eq!(
        outcome.unwrap().stdout,
        "CLAS/OC /NS/ZCL_A package=ZP d\nSEARCH: 1 object"
    );
}

#[test]
fn a_wildcard_keeps_only_the_names_it_matches() {
    for (pattern, names, kept) in [
        (
            "ZFOO*",
            ["ZFOO", "ZFOOBAR", "XZFOO"],
            vec!["ZFOO", "ZFOOBAR"],
        ),
        ("*FOO", ["ZFOO", "FOO", "ZFOOX"], vec!["ZFOO", "FOO"]),
        (
            "Z*BAR",
            ["ZBAR", "ZFOOBAR", "ZFOOBARX"],
            vec!["ZBAR", "ZFOOBAR"],
        ),
        ("Z*O*R", ["ZFOOBAR", "ZOR", "ZRO"], vec!["ZFOOBAR", "ZOR"]),
        ("*", ["A", "B", "C"], vec!["A", "B", "C"]),
        ("/NS/Z*", ["/NS/ZA", "/OTHER/ZA", "ZA"], vec!["/NS/ZA"]),
        ("ZA*A", ["ZA", "ZAA", "ZAB"], vec!["ZAA"]),
    ] {
        let reply = reply_of(&names.map(|name| ("PROG/P", name)));
        let (outcome, _) = run(ok(reply), &query(pattern, None, 50));
        let outcome = outcome.unwrap();
        let printed: Vec<&str> = outcome
            .stdout
            .lines()
            .filter(|line| !line.starts_with("SEARCH:"))
            .map(|line| line.split(' ').nth(1).unwrap())
            .collect();

        assert_eq!(printed, kept, "{pattern}");
        assert_eq!(
            outcome.stdout.lines().last().unwrap(),
            if kept.len() == 1 {
                "SEARCH: 1 object".to_owned()
            } else {
                format!("SEARCH: {} objects", kept.len())
            },
            "{pattern}"
        );
        assert_eq!(
            outcome.stderr.is_empty(),
            kept.len() == names.len(),
            "{pattern}"
        );
    }
}

#[test]
fn objects_of_another_type_and_of_another_name_are_both_reported() {
    let reply = reply_of(&[
        ("PROG/P", "ZFOO"),
        ("PROG/P", "ZFOOBAR"),
        ("CLAS/OC", "ZFOO"),
    ]);
    let (outcome, _) = run(ok(reply), &query("ZFOO", Some("PROG"), 50));
    let outcome = outcome.unwrap();

    assert_eq!(outcome.stdout, "PROG/P ZFOO package=ZP d\nSEARCH: 1 object");
    assert_eq!(
        outcome.stderr.lines().collect::<Vec<_>>(),
        [
            "W: the reply held objects of other types; 1 left out (the result may hold fewer than --max objects)",
            NAME_WARNING
        ]
    );
}

#[test]
fn bidi_and_format_characters_are_dropped_from_every_printed_field() {
    let reply = "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference adtcore:type=\"PROG/P\" adtcore:name=\"ZP\" adtcore:packageName=\"Z&#x202E;P\" adtcore:description=\"ok &#x2066;evil&#x2069; &#x200B;a&#xFEFF;b&#xAD;c&#x61C;d&#x180E;e&#x2060;f&#x202A;g\"/></adtcore:objectReferences>";
    let (outcome, _) = run(ok(reply.to_owned()), &query("ZP", None, 50));
    let stdout = outcome.unwrap().stdout;

    assert_eq!(
        stdout,
        "PROG/P ZP package=ZP ok evil abcdefg\nSEARCH: 1 object"
    );
}

/// Each of the 12 types drops a reply object of another type, with the warning.
#[test]
fn each_type_filter_drops_a_reply_object_of_another_type() {
    for (index, (token, wanted)) in TYPES.iter().enumerate() {
        let other = TYPES[(index + 1) % TYPES.len()].1;
        let reply = reply_of(&[(other, "ZOTHER"), (wanted, "ZWANTED")]);
        let (outcome, _) = run(ok(reply), &query("Z*", Some(token), 50));
        let outcome = outcome.unwrap();

        assert_eq!(
            outcome.stdout,
            format!("{wanted} ZWANTED package=ZP d\nSEARCH: 1 object"),
            "{token}"
        );
        assert_eq!(
            outcome.stderr,
            "W: the reply held objects of other types; 1 left out (the result may hold fewer than --max objects)",
            "{token}"
        );
    }
}
