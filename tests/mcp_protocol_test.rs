//! The MCP protocol foundation against a fake client: framing, negotiation, correlation, tool
//! discovery and stdout integrity, with no process and no SAP.

use abapreach::mcp::{
    MAX_TOOL_NAME, NoTools, SUPPORTED_VERSIONS, Server, ToolHandler, ToolResult, serve,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// A handler that panics for one tool, to prove the server survives a bug in a tool.
struct Panicking;

impl ToolHandler for Panicking {
    fn call(&mut self, name: &str, _: &Map<String, Value>) -> ToolResult {
        assert_ne!(name, "abap_read_source", "boom");

        ToolResult::error(format!("ERROR: {name} ran"))
    }
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

struct Run {
    frames: Vec<Value>,
    stdout: String,
    log: String,
}

fn exchange<H: ToolHandler>(mut server: Server<H>, input: &[u8]) -> Run {
    let (stdout, mut log) = (Capture::default(), Vec::new());

    serve(
        &mut server,
        std::io::Cursor::new(input.to_vec()),
        stdout.clone(),
        &mut log,
    )
    .unwrap();

    let stdout = String::from_utf8(stdout.bytes()).unwrap();

    Run {
        frames: stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        stdout,
        log: String::from_utf8(log).unwrap(),
    }
}

fn lines(messages: &[Value]) -> Vec<u8> {
    messages
        .iter()
        .flat_map(|message| format!("{message}\n").into_bytes())
        .collect()
}

fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn one(messages: &[Value]) -> Vec<Value> {
    exchange(Server::new(NoTools, true), &lines(messages)).frames
}

#[test]
fn initialize_echoes_each_of_the_four_supported_versions() {
    assert_eq!(
        SUPPORTED_VERSIONS,
        ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]
    );

    for version in SUPPORTED_VERSIONS {
        let reply = &one(&[request(
            json!(1),
            "initialize",
            json!({"protocolVersion": version, "capabilities": {}}),
        )])[0];

        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["protocolVersion"], version);
        assert_eq!(reply["result"]["capabilities"], json!({"tools": {}}));
        assert_eq!(reply["result"]["serverInfo"]["name"], "abapreach");
        assert_eq!(
            reply["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
    }
}

#[test]
fn initialize_answers_an_unknown_version_with_the_newest_supported_one() {
    for version in ["1999-01-01", "2026-07-28", ""] {
        let reply = &one(&[request(
            json!(1),
            "initialize",
            json!({"protocolVersion": version}),
        )])[0];

        assert_eq!(
            reply["result"]["protocolVersion"], "2025-11-25",
            "{version}"
        );
    }
}

#[test]
fn initialize_without_a_string_version_is_invalid_params() {
    for params in [json!({}), json!({"protocolVersion": 5}), json!(null)] {
        let reply = &one(&[request(json!(1), "initialize", params.clone())])[0];

        assert_eq!(reply["error"]["code"], -32602, "{params}");
        assert_eq!(reply["id"], 1);
    }
}

#[test]
fn notifications_and_client_responses_get_no_reply() {
    let frames = one(&[
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "method": "bogus/method"}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}}),
        json!({"jsonrpc": "2.0", "id": 7, "result": {"action": "accept"}}),
        request(json!(2), "ping", json!({})),
    ]);

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["id"], 2);
    assert_eq!(frames[0]["result"], json!({}));
}

#[test]
fn server_discover_gets_32601_and_the_server_keeps_running() {
    let frames = one(&[
        request(
            json!(0),
            "server/discover",
            json!({"_meta": {"protocolVersion": "2026-07-28"}}),
        ),
        request(
            json!(1),
            "initialize",
            json!({"protocolVersion": "2025-11-25"}),
        ),
        request(json!(2), "tools/list", json!({})),
    ]);

    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0]["id"], 0);
    assert_eq!(frames[0]["error"]["code"], -32601);
    assert_eq!(frames[1]["result"]["protocolVersion"], "2025-11-25");
    assert!(frames[2]["result"]["tools"].is_array());
}

#[test]
fn ids_are_echoed_exactly_whatever_their_type() {
    let ids = [json!(7), json!("abc"), json!(0), json!(-1), json!(1.5)];
    let frames = one(&ids
        .iter()
        .map(|id| request(id.clone(), "ping", json!({})))
        .collect::<Vec<_>>());

    assert_eq!(
        frames
            .iter()
            .map(|frame| frame["id"].clone())
            .collect::<Vec<_>>(),
        ids
    );
}

#[test]
fn malformed_input_gets_an_error_and_the_server_survives() {
    let mut input = Vec::new();

    for garbage in [
        &b"not json\n"[..],
        &b"{\"jsonrpc\":\"2.0\",\"id\":1,\n"[..],
        &b"\xff\xfe{\"x\"\n"[..],
    ] {
        input.extend_from_slice(garbage);
    }

    for invalid in [
        "[]",
        "7",
        "\"text\"",
        "null",
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":5}"#,
        r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
        r#"{"id":1,"method":"ping"}"#,
    ] {
        input.extend_from_slice(format!("{invalid}\n").as_bytes());
    }

    input.extend_from_slice(b"\n   \r\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"ping\"}\r\n");

    let frames = exchange(Server::new(NoTools, false), &input).frames;
    let (errors, last) = frames.split_at(frames.len() - 1);
    let codes = errors
        .iter()
        .map(|frame| frame["error"]["code"].as_i64().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(codes[..3], [-32700, -32700, -32700]);
    assert!(codes[3..].iter().all(|code| *code == -32600), "{codes:?}");
    assert_eq!(codes.len(), 3 + 11);
    // A request whose id is a valid string or number gets that id back; any other id is null.
    let ids = errors
        .iter()
        .map(|frame| frame["id"].clone())
        .collect::<Vec<_>>();

    assert!(ids[..11].iter().all(Value::is_null), "{ids:?}");
    assert_eq!(ids[11..], [json!(1), json!(1), json!(1)]);
    assert_eq!(last[0]["id"], 99);
    assert_eq!(last[0]["result"], json!({}));
}

#[test]
fn tools_call_rejects_bad_names_and_arguments_with_32602() {
    for params in [
        json!({}),
        json!({"name": ["abap_read_source"]}),
        json!({"name": "no_such_tool", "arguments": {}}),
        json!({"name": "abap_read_source", "arguments": "text"}),
        json!({"name": "abap_read_source", "arguments": [1]}),
    ] {
        let reply = &one(&[request(json!(1), "tools/call", params.clone())])[0];

        assert_eq!(reply["error"]["code"], -32602, "{params}");
        assert_eq!(reply["id"], 1);
    }
}

#[test]
fn a_known_tool_returns_a_full_result_in_content_and_structured_content() {
    let reply = &one(&[request(
        json!(1),
        "tools/call",
        json!({"name": "abap_read_source", "_meta": {"progressToken": 1}}),
    )])[0];
    let result = &reply["result"];

    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(
        result["content"][0]["text"],
        "ERROR: abap_read_source is not available in this build yet"
    );
    assert_eq!(
        result["structuredContent"]["message"],
        result["content"][0]["text"]
    );
}

#[test]
fn a_tool_offered_only_to_writers_is_unknown_when_no_server_allows_writes() {
    let reply = &exchange(
        Server::new(NoTools, false),
        &lines(&[request(
            json!(1),
            "tools/call",
            json!({"name": "abap_deploy", "arguments": {}}),
        )]),
    )
    .frames[0];

    assert_eq!(reply["error"]["code"], -32602);
}

#[test]
fn a_panicking_tool_gives_32603_and_the_server_survives() {
    let run = exchange(
        Server::new(Panicking, true),
        &lines(&[
            request(json!(1), "tools/call", json!({"name": "abap_read_source"})),
            json!({"jsonrpc": "2.0", "method": "notifications/boom"}),
            request(json!(2), "tools/call", json!({"name": "abap_read_table"})),
        ]),
    );

    assert_eq!(run.frames.len(), 2);
    assert_eq!(run.frames[0]["id"], 1);
    assert_eq!(run.frames[0]["error"]["code"], -32603);
    assert_eq!(run.frames[1]["result"]["isError"], true);
    assert_eq!(
        run.frames[1]["result"]["content"][0]["text"],
        "ERROR: abap_read_table ran"
    );
}

fn tools(write_offered: bool) -> Vec<Value> {
    let run = exchange(
        Server::new(NoTools, write_offered),
        &lines(&[request(json!(1), "tools/list", json!({}))]),
    );

    run.frames[0]["result"]["tools"].as_array().unwrap().clone()
}

#[test]
fn tools_list_offers_the_two_read_tools_when_no_server_allows_writes() {
    let names = tools(false)
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(names, ["abap_read_source", "abap_read_table"]);
}

#[test]
fn tools_list_offers_all_eight_contract_tools_when_a_server_allows_writes() {
    let names = tools(true)
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        names,
        [
            "abap_create",
            "abap_create_simulate",
            "abap_delete",
            "abap_delete_simulate",
            "abap_deploy",
            "abap_read_source",
            "abap_read_table",
            "abap_simulate",
        ]
        .map(str::to_owned)
        .into()
    );
}

#[test]
fn every_tool_has_a_short_name_a_complete_schema_and_advisory_annotations() {
    assert_eq!(MAX_TOOL_NAME, 30);

    for tool in tools(true) {
        let name = tool["name"].as_str().unwrap();
        let schema = &tool["inputSchema"];
        let properties = schema["properties"].as_object().unwrap();
        let required = schema["required"].as_array().unwrap();

        assert!(name.len() <= MAX_TOOL_NAME, "{name}");
        assert!(name.starts_with("abap_"), "{name}");
        assert_eq!(schema["type"], "object", "{name}");
        assert!(!required.is_empty(), "{name}");
        assert!(
            required
                .iter()
                .all(|field| properties.contains_key(field.as_str().unwrap())),
            "{name}"
        );
        assert!(
            properties
                .values()
                .all(|property| property["type"].is_string() && property["description"].is_string()),
            "{name}"
        );
        assert!(required.iter().any(|field| field == "conn"), "{name}");
        assert!(
            tool["description"].as_str().unwrap().contains("untrusted"),
            "{name}"
        );

        let read = name == "abap_read_source" || name == "abap_read_table";
        let annotations = &tool["annotations"];

        // Reading a DDIC or transaction type creates a temporary class (M6): only the table read
        // is read-only, and no read tool is destructive.
        assert_eq!(
            annotations["readOnlyHint"],
            name == "abap_read_table",
            "{name}"
        );
        assert_eq!(annotations["destructiveHint"], !read, "{name}");
        assert_eq!(annotations["openWorldHint"], false, "{name}");
        // A write tool takes its plan token only where the contract says so.
        assert_eq!(
            properties.contains_key("plan_token"),
            ["abap_deploy", "abap_create", "abap_delete"].contains(&name),
            "{name}"
        );
    }
}

#[test]
fn tool_arguments_follow_the_contract() {
    let tools = tools(true);
    let properties = |name: &str| {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();

        tool["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    let rows = tools
        .iter()
        .find(|tool| tool["name"] == "abap_read_table")
        .unwrap()["inputSchema"]["properties"]["rows"]
        .clone();

    assert_eq!(
        properties("abap_read_source"),
        set(&["conn", "type", "name", "include", "fugr", "main", "lines"])
    );
    assert_eq!(
        properties("abap_read_table"),
        set(&["conn", "table", "fields", "where", "rows"])
    );
    assert_eq!(
        properties("abap_deploy"),
        set(&[
            "conn",
            "type",
            "name",
            "source",
            "tr",
            "include",
            "fugr",
            "create_include",
            "main",
            "plan_token"
        ])
    );
    assert_eq!(
        properties("abap_create_simulate"),
        set(&[
            "conn", "type", "name", "text", "source", "prog", "package", "tr", "fugr", "subc"
        ])
    );
    assert_eq!(
        properties("abap_delete"),
        set(&["conn", "objects", "tr", "plan_token"])
    );
    assert_eq!(
        (rows["minimum"].clone(), rows["maximum"].clone()),
        (json!(1), json!(1000))
    );
}

#[test]
fn stdout_holds_one_compact_frame_per_line_and_the_log_holds_no_input() {
    let run = exchange(
        Server::new(NoTools, true),
        b"garbage secret-host.example.com\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"leak/s3cret\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
    );

    assert_eq!(run.stdout.lines().count(), 3);
    assert!(run.stdout.ends_with('\n'));
    assert!(!run.stdout.contains("\n\n"));
    assert!(run.frames.iter().all(|frame| frame["jsonrpc"] == "2.0"));
    assert_eq!(run.log, "abapreach-mcp: start\nabapreach-mcp: stop\n");
}

#[test]
fn a_failing_stdout_ends_the_serve_loop_with_an_error() {
    struct Closed;

    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut log = Vec::new();
    let input = lines(&[request(json!(1), "ping", json!({}))]);

    assert!(
        serve(
            &mut Server::new(NoTools, true),
            std::io::Cursor::new(input),
            Closed,
            &mut log
        )
        .is_err()
    );
}
