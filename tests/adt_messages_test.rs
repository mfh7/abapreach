mod common;

use abapreach::adt::{AdtMessage, format_messages, parse_messages};
use common::Fixture;
use std::fs;

fn exchange_response_body(fixture: &str, uri_part: &str, nth: usize) -> String {
    let text = fs::read_to_string(common::fixture_path(fixture)).unwrap();
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
