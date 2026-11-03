mod common;

use abapreach::adt::{
    AdtMessage, Caller, format_messages, function_group_of, function_group_search_uri,
    parse_callers, parse_messages, parse_object_refs, usage_references_uri,
};
use common::Fixture;
use std::fs;

fn exchange_response_body(fixture: &str, uri_part: &str, nth: usize) -> String {
    let path = if fixture.starts_with("dev-750-http-where-used") {
        common::fixture_path_in("m3", fixture)
    } else {
        common::fixture_path(fixture)
    };
    let text = fs::read_to_string(path).unwrap();
    let fixture: Fixture = serde_json::from_str(&text).unwrap();

    fixture
        .exchanges
        .into_iter()
        .filter(|exchange| exchange.request.uri.contains(uri_part))
        .nth(nth)
        .unwrap()
        .response
        .body
}

const ERROR_TEXT: &str = "E row 3: Field \"NO_SUCH_VAR\" is unknown.";

#[test]
fn recorded_checkrun_message_unescapes_entities() {
    let valid = exchange_response_body("dev-750-rfc.json", "checkruns", 0);
    let invalid = exchange_response_body("dev-750-rfc.json", "checkruns", 1);

    assert!(invalid.contains("&quot;NO_SUCH_VAR&quot;"));
    assert_eq!(format_messages(&valid), Vec::<String>::new());
    assert_eq!(format_messages(&invalid), vec![ERROR_TEXT]);
}

#[test]
fn recorded_activation_message_is_printed_like_a_checkrun_message() {
    let rejected = exchange_response_body("dev-750-rfc.json", "activation", 2);
    let accepted = exchange_response_body("dev-750-rfc.json", "activation", 0);

    assert_eq!(format_messages(&rejected), vec![ERROR_TEXT]);
    assert_eq!(format_messages(&accepted), Vec::<String>::new());
    assert!(parse_messages(&rejected).unwrap()[0].is_error());
}

#[test]
fn checkrun_fragment_with_several_messages() {
    let xml = concat!(
        r#"<chkrun:checkMessage chkrun:type="E" chkrun:uri="x#start=3,8" "#,
        r#"chkrun:shortText="bad"/>"#,
        r#"<chkrun:checkMessage chkrun:type="W" chkrun:uri="x#start=0,0" "#,
        r#"chkrun:shortText="warn"/>"#,
    );

    assert_eq!(format_messages(xml), vec!["E row 3: bad", "W row 0: warn"]);
}

#[test]
fn activation_message_uses_line_when_the_href_has_no_position() {
    let xml = concat!(
        r#"<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist">"#,
        r#"<msg type="W" line="7" href="x"><shortText><txt>a &amp; b &lt;c&gt;</txt>"#,
        r#"</shortText></msg><msg type="I"/></chkl:messages>"#,
    );

    assert_eq!(
        parse_messages(xml).unwrap(),
        vec![
            AdtMessage {
                kind: "W".to_owned(),
                row: 7,
                text: "a & b <c>".to_owned()
            },
            AdtMessage {
                kind: "I".to_owned(),
                row: 0,
                text: String::new()
            },
        ]
    );
    assert_eq!(
        format_messages(xml),
        vec!["W row 7: a & b <c>", "I row 0: "]
    );
}

#[test]
fn empty_and_malformed_bodies() {
    assert_eq!(parse_messages("").unwrap(), Vec::new());
    assert!(parse_messages("<a><b></a>").is_err());
    assert_eq!(format_messages("<a><b></a>"), Vec::<String>::new());
    assert!(
        !AdtMessage {
            kind: "W".to_owned(),
            row: 1,
            text: String::new()
        }
        .is_error()
    );
}

// ---- function-group resolution from a quickSearch result ------------------------------

const SEARCH_RESULT: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"utf-8\"?><adtcore:objectReferences ",
    "xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference ",
    "adtcore:uri=\"/sap/bc/adt/functions/groups/ytest_abapreach_fg3/fmodules/",
    "ytest_abapreach_fm3\" adtcore:type=\"FUGR/FF\" ",
    "adtcore:name=\"YTEST_ABAPREACH_FM3\"/></adtcore:objectReferences>"
);

#[test]
fn the_group_resolution_query_is_typed_to_the_function_module() {
    assert_eq!(
        function_group_search_uri("YTEST_ABAPREACH_FM3"),
        concat!(
            "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch",
            "&query=YTEST_ABAPREACH_FM3&maxResults=10&objectType=FUGR%2FFF"
        )
    );
}

#[test]
fn a_typed_search_result_yields_the_group_of_the_module() {
    assert_eq!(
        function_group_of(SEARCH_RESULT, "YTEST_ABAPREACH_FM3").unwrap(),
        Some("YTEST_ABAPREACH_FG3".to_owned())
    );
    assert_eq!(
        function_group_of(SEARCH_RESULT, "ytest_abapreach_fm3").unwrap(),
        Some("YTEST_ABAPREACH_FG3".to_owned())
    );
}

#[test]
fn an_empty_result_or_another_object_has_no_group() {
    let empty = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><adtcore:objectReferences ",
        "xmlns:adtcore=\"http://www.sap.com/adt/core\"/>"
    );

    assert_eq!(function_group_of(empty, "ZFM").unwrap(), None);
    assert_eq!(function_group_of(SEARCH_RESULT, "ZOTHER").unwrap(), None);
}

#[test]
fn a_reference_that_is_not_a_function_module_uri_has_no_group() {
    let cases = [
        concat!(
            "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\">",
            "<adtcore:objectReference adtcore:uri=\"/sap/bc/adt/functions/groups/zfg\" ",
            "adtcore:type=\"FUGR/F\" adtcore:name=\"ZFM\"/></adtcore:objectReferences>"
        ),
        concat!(
            "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\">",
            "<adtcore:objectReference adtcore:uri=\"/sap/bc/adt/oo/classes/zcl_x\" ",
            "adtcore:type=\"CLAS/OC\" adtcore:name=\"ZFM\"/></adtcore:objectReferences>"
        ),
        concat!(
            "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\">",
            "<adtcore:objectReference ",
            "adtcore:uri=\"/sap/bc/adt/functions/groups//fmodules/zfm\" ",
            "adtcore:type=\"FUGR/FF\" adtcore:name=\"ZFM\"/></adtcore:objectReferences>"
        ),
        concat!(
            "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\">",
            "<adtcore:objectReference ",
            "adtcore:uri=\"/sap/bc/adt/functions/groups/zfg/fmodules/zfm/extra\" ",
            "adtcore:type=\"FUGR/FF\" adtcore:name=\"ZFM\"/></adtcore:objectReferences>"
        ),
    ];

    for xml in cases {
        assert_eq!(function_group_of(xml, "ZFM").unwrap(), None, "{xml}");
    }
}

#[test]
fn malformed_xml_is_an_error_not_an_empty_result() {
    assert!(function_group_of("<a><b></a>", "ZFM").is_err());
}

#[test]
fn recorded_where_used_lists_the_caller_and_drops_the_package_entry() {
    let found = exchange_response_body("dev-750-http-where-used.json", "usageReferences", 0);

    assert!(found.contains(r#"adtcore:type="DEVC/K""#));
    assert_eq!(
        parse_callers(&found),
        Ok(vec![Caller {
            kind: "PROG/P".to_owned(),
            name: "YTEST_ABAPREACH_P3".to_owned(),
        }])
    );
}

#[test]
fn recorded_empty_where_used_has_no_callers() {
    let empty = exchange_response_body("dev-750-http-where-used.json", "usageReferences", 1);

    assert_eq!(parse_callers(&empty), Ok(Vec::new()));
}

#[test]
fn where_used_counts_a_repeated_caller_once_and_rejects_broken_xml() {
    let twice = r#"<r:result xmlns:r="x" xmlns:adtcore="y"><r:o><r:adtObject adtcore:name="ZA" adtcore:type="PROG/P"/></r:o><r:o><r:adtObject adtcore:name="ZA" adtcore:type="PROG/P"/></r:o><r:o><r:adtObject adtcore:name="ZB" adtcore:type="CLAS/OC"/></r:o><r:o><r:adtObject adtcore:name="" adtcore:type="PROG/P"/></r:o></r:result>"#;
    let names: Vec<String> = parse_callers(twice)
        .unwrap()
        .into_iter()
        .map(|caller| caller.name)
        .collect();

    assert_eq!(names, ["ZA", "ZB"]);
    assert!(parse_callers("<a><b></a>").is_err());
}

#[test]
fn the_where_used_uri_keeps_slashes_literal_and_encodes_the_rest() {
    assert_eq!(
        usage_references_uri("/sap/bc/adt/functions/groups/yfg/fmodules/yfm"),
        "/sap/bc/adt/repository/informationsystem/usageReferences?uri=/sap/bc/adt/functions/groups/yfg/fmodules/yfm"
    );
    assert_eq!(
        usage_references_uri("/sap/bc/adt/x/a b&c=d?e#f"),
        "/sap/bc/adt/repository/informationsystem/usageReferences?uri=/sap/bc/adt/x/a%20b%26c%3Dd%3Fe%23f"
    );
}

#[test]
fn only_references_inside_an_object_element_are_listed_as_objects() {
    let xml = concat!(
        r#"<ioc:inactiveObjects xmlns:ioc="i" xmlns:adtcore="a">"#,
        // A reference that has a body, as `Start` and `End` events rather than one empty element.
        r#"<ioc:entry><ioc:object><ioc:ref adtcore:uri="/a"></ioc:ref>"#,
        // An unrelated element closing inside the object does not end it.
        r#"<ioc:note></ioc:note><ioc:ref adtcore:uri="/b"/><ioc:ref adtcore:uri="/c"></ioc:ref>"#,
        r#"</ioc:object>"#,
        // After the object closes, a reference is the transport request, not an object.
        r#"<ioc:transport><ioc:ref adtcore:uri="/request"></ioc:ref>"#,
        r#"<ioc:ref adtcore:uri="/request2"/></ioc:transport></ioc:entry>"#,
        r#"<ioc:ref adtcore:uri="/outside"></ioc:ref><ioc:ref adtcore:uri="/outside2"/>"#,
        r#"</ioc:inactiveObjects>"#,
    );
    let uris: Vec<String> = parse_object_refs(xml)
        .unwrap()
        .into_iter()
        .map(|object| object.uri)
        .collect();

    assert_eq!(uris, ["/a", "/b", "/c"]);
}
