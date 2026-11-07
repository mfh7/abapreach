//! The MCP read tools against a fake client and fake transports: full result in `content` and
//! `structuredContent`, dedicated source/rows fields, excerpt semantics, bounds, error
//! equivalence, scrub and one connection per call. No SAP.

mod common;

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::hash::source_hash;
use abapreach::mcp::{Server, ToolHandler, ToolResult, serve};
use abapreach::mcp_read::ReadTools;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use common::{ScriptedTransport, preview};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::rc::Rc;

const PASSWORD: &str = "secret-pw";
const HOST: &str = "example.com";

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

/// Counts the closes of a transport the handler owns.
struct Counted {
    inner: ScriptedTransport,
    closes: Rc<Cell<usize>>,
    sent: Rc<RefCell<Vec<Request>>>,
}

impl SadtTransport for Counted {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.sent.borrow_mut().push(request.clone());

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes.set(self.closes.get() + 1);

        Ok(())
    }
}

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    adt_opened: Rc<Cell<usize>>,
    rfc_opened: Rc<Cell<usize>>,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened.set(self.adt_opened.get() + 1);
        self.adt.pop_front().expect("unexpected ADT connect")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened.set(self.rfc_opened.get() + 1);
        self.rfc.pop_front().expect("unexpected RFC connect")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        unreachable!("a read tool never probes the library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        unreachable!("a read tool never pings")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

struct Rig {
    fake: Fake,
    env: BTreeMap<String, String>,
    closes: Rc<Cell<usize>>,
    sent: Rc<RefCell<Vec<Request>>>,
    _directory: tempfile::TempDir,
}

fn rig() -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");
    let config = format!(
        r#"{{"servers":[{{"systemId":"DEV","host":"{HOST}","port":443,"clients":["100"],"rfc":{{"ashost":"{HOST}","sysnr":"00"}},"adt":{{"insecure":true}}}}]}}"#
    );

    fs::write(&path, config).unwrap();

    Rig {
        fake: Fake::default(),
        env: BTreeMap::from([
            ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
            ("DEV_100_USER".to_owned(), "user".to_owned()),
            ("DEV_100_PASS".to_owned(), PASSWORD.to_owned()),
        ]),
        closes: Rc::new(Cell::new(0)),
        sent: Rc::default(),
        _directory: directory,
    }
}

impl Rig {
    fn adt(&mut self, replies: Vec<Result<Response, TransportError>>) {
        self.adt_transport(ScriptedTransport::replies(replies));
    }

    fn adt_transport(&mut self, inner: ScriptedTransport) {
        self.fake.adt.push_back(Ok(Box::new(Counted {
            inner,
            closes: self.closes.clone(),
            sent: self.sent.clone(),
        })));
    }

    fn call(&mut self, name: &str, arguments: Value) -> ToolResult {
        let config = Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap();
        let mut tools = ReadTools::new(config, self.env.clone(), &mut self.fake);
        let arguments: Map<String, Value> = arguments.as_object().unwrap().clone();

        tools.call(name, &arguments)
    }
}

fn source(text: &str) -> Result<Response, TransportError> {
    Ok(Response::text(200, text))
}

fn lines_of(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Server stdout the test can read back after the server ended: the server owns its writer.
#[derive(Clone, Default)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);

        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

const PROGRAM: &str = "REPORT ytest.\nWRITE 'a'.\nWRITE 'b'.";

#[test]
fn a_source_read_carries_the_same_data_in_text_and_structured_fields() {
    let mut rig = rig();

    rig.adt(vec![source(PROGRAM)]);

    let result = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );
    let hash = source_hash(&lines_of(PROGRAM));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["source"], PROGRAM);
    assert_eq!(result.structured["hash"], hash);
    assert_eq!(result.structured["lines"], 3);
    assert_eq!(result.structured["status"], "ACTIVE");
    assert_eq!(result.structured["partial"], false);
    assert!(result.text.contains(PROGRAM), "{}", result.text);
    assert!(result.text.contains(&hash), "{}", result.text);
    assert_eq!(rig.closes.get(), 1);
}

#[test]
fn a_lines_read_is_a_partial_excerpt_without_hash_or_source() {
    let mut rig = rig();

    rig.adt(vec![source(PROGRAM)]);

    let result = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST", "lines": "2-3"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["excerpt"], "WRITE 'a'.\nWRITE 'b'.");
    assert_eq!(result.structured["partial"], true);
    assert_eq!(result.structured["lines"], 2);
    assert!(result.structured.get("hash").is_none());
    assert!(result.structured.get("source").is_none());
    assert!(result.text.contains("WRITE 'a'.\nWRITE 'b'."));
    assert!(!result.text.contains("hash"), "{}", result.text);
}

#[test]
fn a_local_rule_and_a_failed_read_are_error_results_equal_in_text_and_structure() {
    let mut rig = rig();

    // include is only valid for CLAS: a STOP before any connection.
    let stop = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST", "include": "CCIMP"}),
    );

    assert!(stop.is_error);
    assert!(stop.text.starts_with("STOP: "), "{}", stop.text);
    assert_eq!(stop.structured["message"], stop.text);
    assert_eq!(rig.fake.adt_opened.get(), 0);

    rig.adt(vec![
        Ok(Response::text(404, "")),
        Ok(Response::text(404, "")),
        Ok(Response::text(404, "")),
    ]);

    let failed = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );

    assert!(failed.is_error);
    assert!(failed.text.starts_with("ERROR: "), "{}", failed.text);
    assert_eq!(failed.structured["message"], failed.text);
}

#[test]
fn a_bad_argument_is_a_stop_result() {
    let mut rig = rig();

    for arguments in [
        json!({"type": "PROG", "name": "YTEST"}),
        json!({"conn": "DEV_100", "type": "XXXX", "name": "YTEST"}),
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST", "lines": "9-2"}),
        json!({"conn": "NOPE_100", "type": "PROG", "name": "YTEST"}),
    ] {
        let result = rig.call("abap_read_source", arguments.clone());

        assert!(result.is_error, "{arguments}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
    }

    assert_eq!(rig.fake.adt_opened.get(), 0);
}

fn table_replies(rows: &[(&str, &str)]) -> Vec<Result<Response, TransportError>> {
    let first: Vec<Option<&str>> = rows.iter().map(|row| Some(row.0)).collect();
    let second: Vec<Option<&str>> = rows.iter().map(|row| Some(row.1)).collect();

    vec![
        Ok(Response::text(
            200,
            preview(&[
                ("TABNAME", &[Some("T001")]),
                ("TABCLASS", &[Some("TRANSP")]),
                ("VIEWCLASS", &[None]),
            ]),
        )),
        Ok(Response::text(
            200,
            preview(&[("BUKRS", &first), ("BUTXT", &second)]),
        )),
    ]
}

#[test]
fn a_table_read_carries_rows_in_a_dedicated_field_and_the_same_text() {
    let mut rig = rig();

    rig.adt(table_replies(&[("1000", "Alpha"), ("2000", "Beta")]));

    let result = rig.call(
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "t001", "fields": "bukrs, butxt", "rows": 2}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["columns"], json!(["BUKRS", "BUTXT"]));
    assert_eq!(
        result.structured["rows"],
        json!([["1000", "Alpha"], ["2000", "Beta"]])
    );
    assert_eq!(result.structured["truncated"], false);
    assert_eq!(result.text, "BUKRS\tBUTXT\n1000\tAlpha\n2000\tBeta");
    assert_eq!(rig.closes.get(), 1);
}

#[test]
fn rows_default_to_100_and_stop_above_1000_before_any_connection() {
    let mut rig = rig();

    rig.adt(table_replies(&[("1000", "Alpha")]));

    let default = rig.call(
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "T001"}),
    );

    assert!(!default.is_error, "{}", default.text);

    for rows in [json!(0), json!(1001)] {
        let result = rig.call(
            "abap_read_table",
            json!({"conn": "DEV_100", "table": "T001", "rows": rows}),
        );

        assert!(result.is_error);
        assert_eq!(result.text, "STOP: rows must be between 1 and 1000");
        assert_eq!(result.structured["message"], result.text);
    }

    assert_eq!(rig.fake.adt_opened.get(), 1);
}

#[test]
fn a_denied_table_stops_before_any_connection() {
    let mut rig = rig();
    let result = rig.call(
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "USR02"}),
    );

    assert!(result.is_error);
    assert!(result.text.starts_with("STOP: "), "{}", result.text);
    assert!(result.text.contains("read_table_denied"));
    assert_eq!(rig.fake.adt_opened.get(), 0);
}

#[test]
fn a_payload_with_a_configured_secret_is_rejected_unaltered_in_no_field() {
    let mut rig = rig();

    rig.adt(vec![source(&format!("REPORT y.\n\" {PASSWORD}"))]);

    let source = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );

    assert!(source.is_error);
    assert_eq!(
        source.text,
        "REJECTED: payload contains a configured secret"
    );
    assert_eq!(source.structured["message"], source.text);

    rig.adt(table_replies(&[(PASSWORD, "Alpha")]));

    let table = rig.call(
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "T001"}),
    );

    assert!(table.is_error);
    assert_eq!(table.text, "REJECTED: payload contains a configured secret");
    assert!(!table.structured.to_string().contains(PASSWORD));
}

#[test]
fn diagnostics_hide_the_secret_and_the_host() {
    let mut rig = rig();
    let leak = format!("cannot reach {HOST} as user with {PASSWORD}");

    rig.adt(vec![Err(TransportError::PreSendConnectivity(leak.clone()))]);
    rig.fake
        .rfc
        .push_back(Err(TransportError::PreSendConnectivity(leak)));

    let result = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );

    assert!(result.is_error);

    for text in [result.text.clone(), result.structured.to_string()] {
        assert!(!text.contains(PASSWORD), "{text}");
        assert!(!text.contains(HOST), "{text}");
    }
}

#[test]
fn every_read_uses_its_own_connection() {
    let mut rig = rig();

    rig.adt(vec![source(PROGRAM)]);
    rig.adt(vec![source(PROGRAM)]);

    for _ in 0..2 {
        let result = rig.call(
            "abap_read_source",
            json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
        );

        assert!(!result.is_error, "{}", result.text);
    }

    assert_eq!(rig.fake.adt_opened.get(), 2);
    assert_eq!(rig.closes.get(), 2);
}

#[test]
fn the_read_tools_say_their_results_are_untrusted_and_are_read_only() {
    for tool in abapreach::mcp::tool_list(false) {
        assert!(
            tool["description"]
                .as_str()
                .unwrap()
                .contains("untrusted data"),
            "{tool}"
        );
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }
}

#[test]
fn a_tools_call_over_the_wire_returns_content_structured_content_and_is_error() {
    let mut rig = rig();

    rig.adt(vec![source(PROGRAM)]);

    let config = Config::load(ConfigPaths::from_env(&rig.env).unwrap(), &rig.env).unwrap();
    let mut server = Server::new(
        ReadTools::new(config, rig.env.clone(), &mut rig.fake),
        false,
    );
    let request = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
        "name": "abap_read_source",
        "arguments": {"conn": "DEV_100", "type": "PROG", "name": "YTEST"},
    }});
    let (out, mut log) = (Capture::default(), Vec::new());

    serve(
        &mut server,
        std::io::Cursor::new(format!("{request}\n").into_bytes()),
        out.clone(),
        &mut log,
    )
    .unwrap();

    let frame: Value = serde_json::from_slice(&out.bytes()).unwrap();
    let result = &frame["result"];

    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["source"], PROGRAM);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(PROGRAM)
    );
}

#[test]
fn a_table_output_is_cut_at_a_row_boundary_in_text_and_structure() {
    let mut rig = rig();
    let big = "x".repeat(400_000);
    let rows: Vec<(&str, &str)> = vec![("1", big.as_str()); 4];

    rig.adt(table_replies(&rows));

    let result = rig.call(
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "T001", "rows": 4}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["truncated"], true);
    assert_eq!(result.structured["row_count"], 2);
    assert_eq!(result.structured["omitted"], 2);
    assert_eq!(result.structured["rows"].as_array().unwrap().len(), 2);
    assert!(
        result
            .text
            .contains("W: output cut at 1048576 bytes; 2 rows left out")
    );
}

#[test]
fn a_function_group_mismatch_is_a_stop_without_a_fallback() {
    let mut rig = rig();
    let search = abapreach::adt::function_group_search_uri("YTEST_ABAPREACH_FM3");

    rig.adt_transport(common::from_fixture(
        common::fixture_path_in("m3", "dev-750-http-read.json"),
        move |exchange| exchange.request.uri == search,
    ));

    let result = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "FUNC", "name": "YTEST_ABAPREACH_FM3", "fugr": "ZOTHER"}),
    );

    assert!(result.is_error);
    assert!(
        result
            .text
            .starts_with("STOP: --fugr ZOTHER does not match function group"),
        "{}",
        result.text
    );
    assert_eq!(result.structured["message"], result.text);
    // The resolution stopped the read: no source request and no RFC attempt.
    assert_eq!(rig.sent.borrow().len(), 1);
    assert_eq!(rig.fake.rfc_opened.get(), 0);
}

#[test]
fn table_fields_pick_the_columns_and_a_blank_list_picks_them_all() {
    for (fields, select) in [
        (json!("bukrs, butxt"), "SELECT BUKRS, BUTXT FROM T001"),
        (json!("bukrs"), "SELECT BUKRS FROM T001"),
        (json!(""), "SELECT * FROM T001"),
        (json!("   "), "SELECT * FROM T001"),
        (Value::Null, "SELECT * FROM T001"),
    ] {
        let mut rig = rig();

        rig.adt(table_replies(&[("1000", "Alpha")]));

        let result = rig.call(
            "abap_read_table",
            json!({"conn": "DEV_100", "table": "T001", "fields": fields}),
        );

        assert!(!result.is_error, "{fields}: {}", result.text);

        // The first request resolves the table, the second one reads it.
        let sent = rig.sent.borrow();

        assert_eq!(String::from_utf8_lossy(&sent[1].body), select, "{fields}");
    }
}

#[test]
fn a_note_of_a_fallback_reaches_a_successful_result_in_text_and_structure() {
    let mut rig = rig();

    // ADT cannot be reached; the read succeeds over RFC and says so.
    rig.fake
        .adt
        .push_back(Err(TransportError::PreSendConnectivity("down".to_owned())));
    rig.fake
        .rfc
        .push_back(Ok(Box::new(ScriptedTransport::replies(vec![source(
            PROGRAM,
        )]))));

    let read = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );

    assert!(!read.is_error, "{}", read.text);
    assert!(read.text.starts_with(PROGRAM), "{}", read.text);
    assert!(
        read.text.ends_with("falling back to RFC SADT"),
        "{}",
        read.text
    );
    assert!(
        read.structured["notes"]
            .as_str()
            .unwrap()
            .contains("falling back to RFC SADT"),
        "{}",
        read.structured
    );
    assert_eq!(read.structured["source"], PROGRAM);

    rig.fake
        .adt
        .push_back(Err(TransportError::PreSendConnectivity("down".to_owned())));
    rig.fake
        .rfc
        .push_back(Ok(Box::new(ScriptedTransport::replies(table_replies(&[
            ("1000", "Alpha"),
        ])))));

    let table = rig.call(
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "T001"}),
    );

    assert!(!table.is_error, "{}", table.text);
    assert!(
        table.text.ends_with("falling back to RFC SADT"),
        "{}",
        table.text
    );
    assert!(
        table.structured["notes"]
            .as_str()
            .unwrap()
            .contains("falling back to RFC SADT"),
        "{}",
        table.structured
    );
    assert_eq!(table.structured["rows"], json!([["1000", "Alpha"]]));
}
