//! The MCP read tools of 0.18.0 (`abap_sql`, `abap_search`, `abap_where_used`, `abap_dumps` and
//! `abap_dump`) on scripted ADT transports: the text of the CLI, the structured fields, the
//! refusals, the secret check, and the listing under `mcp_write = off`. No SAP.

mod common;

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{
    READ_TOOLS, ToolHandler, ToolResult, tool_list, tool_list_for, tool_list_running,
};
use abapreach::mcp_read::ReadTools;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::rfc::{FakeRfcApi, RfcEndpointResponse, RfcTransport};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use common::{fixture_path_in, preview};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::rc::Rc;

const PASSWORD: &str = "secret-pw";
const SECRET_REJECTED: &str = "REJECTED: payload contains a configured secret";

type Opened = Result<Box<dyn SadtTransport>, TransportError>;
type Reply = Result<Response, TransportError>;
type Calls = Rc<RefCell<Vec<Request>>>;

/// Replies in order and records every request.
struct Recording {
    replies: VecDeque<Reply>,
    calls: Calls,
}

impl SadtTransport for Recording {
    fn request(&mut self, request: Request) -> Reply {
        self.calls.borrow_mut().push(request);
        self.replies
            .pop_front()
            .expect("request without a scripted reply")
    }

    fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    opened: Rc<Cell<usize>>,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.opened.set(self.opened.get() + 1);
        self.adt.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no ADT transport scripted".to_owned(),
            ))
        })
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no RFC transport scripted".to_owned(),
            ))
        })
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
    _directory: tempfile::TempDir,
}

fn rig() -> Rig {
    rig_with("{}")
}

fn rig_with(policies: &str) -> Rig {
    rig_full(policies, "", PASSWORD)
}

/// The route of the connection in `rig_routed`, and the two hosts a library may report alone.
const ROUTE: &str = "/H/router.example.net/S/3299/H/sap.internal.example/S/3200";
const FRAGMENTS: [&str; 2] = ["router.example.net", "sap.internal.example"];

/// A connection with an RFC route, so that the router string and its fragments are secrets.
fn rig_routed() -> Rig {
    rig_full(
        "{}",
        &format!(r#","rfc":{{"ashost":"sap.example.com","sysnr":"00","saprouter":"{ROUTE}"}}"#),
        PASSWORD,
    )
}

fn rig_full(policies: &str, rfc: &str, password: &str) -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");

    fs::write(
        &path,
        format!(
            r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"clients":["100"]{rfc},"policies":{policies}}}]}}"#
        ),
    )
    .unwrap();

    Rig {
        fake: Fake::default(),
        env: BTreeMap::from([
            ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
            ("DEV_100_USER".to_owned(), "TESTUSER".to_owned()),
            ("DEV_100_PASS".to_owned(), password.to_owned()),
        ]),
        _directory: directory,
    }
}

impl Rig {
    /// Scripts one ADT session; the returned handle lists its requests.
    fn session(&mut self, replies: Vec<Reply>) -> Calls {
        let calls: Calls = Rc::default();

        self.fake.adt.push_back(Ok(Box::new(Recording {
            replies: replies.into(),
            calls: calls.clone(),
        })));

        calls
    }

    fn call(&mut self, name: &str, arguments: Value) -> ToolResult {
        let config = Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap();
        let mut tools = ReadTools::new(config, self.env.clone(), &mut self.fake);

        tools.call(name, arguments.as_object().unwrap())
    }

    fn opened(&self) -> usize {
        self.fake.opened.get()
    }
}

fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_path_in("m9", name)).expect("read fixture")
}

fn ok(body: impl AsRef<str>) -> Reply {
    Ok(Response::text(200, body))
}

fn transparent(name: &str) -> Reply {
    ok(preview(&[
        ("TABNAME", &[Some(name)]),
        ("TABCLASS", &[Some("TRANSP")]),
        ("VIEWCLASS", &[None]),
    ]))
}

fn body(call: &Request) -> String {
    String::from_utf8(call.body.clone()).unwrap()
}

// ---- tools/list ---------------------------------------------------------------------------------

const NEW_TOOLS: [&str; 6] = [
    "abap_sql",
    "abap_search",
    "abap_where_used",
    "abap_dumps",
    "abap_dump",
    "abap_ping",
];

#[test]
fn the_new_tools_are_read_tools_and_stay_listed_with_writes_off() {
    for tool in NEW_TOOLS {
        assert!(READ_TOOLS.contains(&tool), "{tool}");
    }

    let keys = |tools: &[Value], name: &str| -> Vec<String> {
        tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    let required = |tools: &[Value], name: &str| -> Vec<String> {
        serde_json::from_value(
            tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"]["required"]
                .clone(),
        )
        .unwrap()
    };

    // No server allows a write, one allows runs, one allows writes: the same five tools each time.
    let writable = ["DEV_100".to_owned()];

    for tools in [
        tool_list(false),
        tool_list_for(&[]),
        tool_list_running(&[], &["DEV_100".to_owned()], &[]),
        tool_list_for(&writable),
    ] {
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();

        for tool in NEW_TOOLS {
            assert!(names.contains(&tool), "{tool} missing from {names:?}");
        }

        assert_eq!(keys(&tools, "abap_sql"), ["conn", "query", "rows"]);
        assert_eq!(
            keys(&tools, "abap_search"),
            ["conn", "max", "pattern", "type"]
        );
        assert_eq!(
            keys(&tools, "abap_where_used"),
            ["conn", "max", "name", "type"]
        );
        assert_eq!(
            keys(&tools, "abap_dumps"),
            ["all", "conn", "date", "rows", "user"]
        );
        assert_eq!(keys(&tools, "abap_dump"), ["all", "conn", "id"]);
        assert_eq!(keys(&tools, "abap_ping"), ["conn"]);
        assert_eq!(required(&tools, "abap_sql"), ["conn", "query"]);
        assert_eq!(required(&tools, "abap_search"), ["conn", "pattern"]);
        assert_eq!(
            required(&tools, "abap_where_used"),
            ["conn", "type", "name"]
        );
        assert_eq!(required(&tools, "abap_dumps"), ["conn"]);
        assert_eq!(required(&tools, "abap_dump"), ["conn", "id"]);
        assert_eq!(required(&tools, "abap_ping"), ["conn"]);

        for tool in tools
            .iter()
            .filter(|tool| NEW_TOOLS.contains(&tool["name"].as_str().unwrap()))
        {
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
            assert_eq!(tool["annotations"]["destructiveHint"], false, "{tool}");
            assert!(
                tool["description"]
                    .as_str()
                    .unwrap()
                    .contains("untrusted data"),
                "{tool}"
            );
        }
    }
}

// ---- abap_sql -----------------------------------------------------------------------------------

const JOIN: &str = "SELECT TADIR~OBJ_NAME, TDEVC~DLVUNIT FROM TADIR INNER JOIN TDEVC ON TADIR~DEVCLASS = TDEVC~DEVCLASS WHERE TADIR~OBJECT = 'CLAS' AND TADIR~OBJ_NAME LIKE 'CL_ABAP_TYPED%'";

#[test]
fn abap_sql_returns_the_rows_as_text_and_as_data() {
    let mut rig = rig();
    let calls = rig.session(vec![
        transparent("TADIR"),
        transparent("TDEVC"),
        ok(fixture("freestyle-join-750.xml")),
    ]);
    let result = rig.call("abap_sql", json!({"conn": "DEV_100", "query": JOIN}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        result.text,
        "OBJ_NAME\tDLVUNIT\nCL_ABAP_TYPEDESCR\tSAP_BASIS"
    );
    assert_eq!(result.structured["columns"], json!(["OBJ_NAME", "DLVUNIT"]));
    assert_eq!(
        result.structured["rows"],
        json!([["CL_ABAP_TYPEDESCR", "SAP_BASIS"]])
    );
    assert_eq!(result.structured["row_count"], 1);
    assert_eq!(result.structured["truncated"], false);

    // The dictionary check comes first; the default of the tool is 100 rows.
    let calls = calls.borrow();

    assert_eq!(calls.len(), 3);
    assert!(calls[0].uri.contains("ddicEntityName=DD02L"));
    assert!(
        calls[2]
            .uri
            .ends_with("/datapreview/freestyle?rowNumber=100")
    );
    assert_eq!(body(&calls[2]), JOIN);
}

#[test]
fn abap_sql_takes_the_row_limit_of_abap_read_table() {
    let mut rig = rig();

    for rows in [json!(0), json!(1001), json!("5"), json!(-1)] {
        let result = rig.call(
            "abap_sql",
            json!({"conn": "DEV_100", "query": JOIN, "rows": rows}),
        );

        assert!(result.is_error, "{rows}");
        assert_eq!(
            result.text, "STOP: rows must be between 1 and 1000",
            "{rows}"
        );
    }

    assert_eq!(rig.opened(), 0);

    let calls = rig.session(vec![
        transparent("TADIR"),
        transparent("TDEVC"),
        ok(fixture("freestyle-join-750.xml")),
    ]);
    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": JOIN, "rows": 1000}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(calls.borrow()[2].uri.ends_with("rowNumber=1000"));
}

#[test]
fn abap_sql_refuses_a_denied_table_like_the_cli_and_opens_no_connection() {
    let mut rig = rig();
    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT BNAME FROM USR02"}),
    );

    assert!(result.is_error);
    assert_eq!(
        result.text,
        "STOP: table USR02 is on the read_table_denied list"
    );
    assert_eq!(rig.opened(), 0);

    // A join that names the denied table is refused as well, and so is a statement that is no
    // SELECT of the grammar.
    let joined = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT a~BNAME FROM TADIR AS a INNER JOIN USR02 AS b ON a~OBJ_NAME = b~BNAME"}),
    );
    let update = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "DELETE FROM TADIR"}),
    );

    assert!(
        joined.is_error && joined.text.starts_with("STOP: "),
        "{}",
        joined.text
    );
    assert!(joined.text.contains("USR02"), "{}", joined.text);
    assert!(
        update.is_error && update.text.starts_with("STOP: "),
        "{}",
        update.text
    );
    assert_eq!(rig.opened(), 0);
}

#[test]
fn abap_sql_follows_the_denied_list_of_the_connection() {
    let mut rig = rig_with(r#"{"read_table_denied":["TADIR"]}"#);
    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"}),
    );

    assert!(result.is_error);
    assert_eq!(
        result.text,
        "STOP: table TADIR is on the read_table_denied list"
    );
    assert_eq!(rig.opened(), 0);
}

#[test]
fn abap_sql_replaces_rows_that_hold_a_configured_secret() {
    let mut rig = rig();

    rig.session(vec![
        transparent("TADIR"),
        ok(preview(&[("OBJ_NAME", &[Some(PASSWORD)])])),
    ]);

    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"}),
    );

    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
    assert!(!result.text.contains(PASSWORD));
}

// ---- abap_search --------------------------------------------------------------------------------

#[test]
fn abap_search_returns_the_cli_lines_and_the_objects() {
    let mut rig = rig();
    let calls = rig.session(vec![ok(fixture("search-clas-758.xml"))]);
    let result = rig.call(
        "abap_search",
        json!({"conn": "DEV_100", "pattern": "cl_abap_typedescr", "type": "CLAS"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        result.text,
        "CLAS/OC CL_ABAP_TYPEDESCR package=SABP_RTTI Runtime Type Services\nSEARCH: 1 object"
    );
    assert_eq!(result.structured["count"], 1);
    assert_eq!(
        result.structured["objects"][0],
        json!({
            "type": "CLAS/OC",
            "name": "CL_ABAP_TYPEDESCR",
            "package": "SABP_RTTI",
            "description": "Runtime Type Services",
        })
    );

    let calls = calls.borrow();

    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].uri.contains("query=CL_ABAP_TYPEDESCR"),
        "{}",
        calls[0].uri
    );
    assert!(calls[0].uri.contains("maxResults=50"), "{}", calls[0].uri);
}

#[test]
fn abap_search_without_a_hit_is_a_success_with_no_objects() {
    let mut rig = rig();

    rig.session(vec![ok(
        r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"/>"#,
    )]);

    let result = rig.call(
        "abap_search",
        json!({"conn": "DEV_100", "pattern": "ZNOTHING*", "max": 5}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.text, "SEARCH: 0 objects");
    assert_eq!(result.structured["count"], 0);
    assert_eq!(result.structured["objects"], json!([]));
}

#[test]
fn abap_search_refuses_bad_input_before_any_connection() {
    let mut rig = rig();

    for (arguments, expected) in [
        (json!({"conn": "DEV_100", "pattern": "ZCL X"}), "STOP: "),
        (
            json!({"conn": "DEV_100", "pattern": "ZCL*", "type": "NOPE"}),
            "STOP: unknown type NOPE",
        ),
        (
            json!({"conn": "DEV_100", "pattern": "ZCL*", "max": 0}),
            "STOP: max must be between 1 and 1000",
        ),
        (
            json!({"conn": "DEV_100", "pattern": "ZCL*", "max": 1001}),
            "STOP: max must be between 1 and 1000",
        ),
        (
            json!({"conn": "DEV_100", "pattern": "ZCL*", "max": "5"}),
            "STOP: max must be between 1 and 1000",
        ),
        (json!({"conn": "DEV_100"}), "STOP: pattern is required"),
    ] {
        let result = rig.call("abap_search", arguments.clone());

        assert!(result.is_error, "{arguments}");
        assert!(
            result.text.starts_with(expected),
            "{arguments}: {}",
            result.text
        );
    }

    assert_eq!(rig.opened(), 0);
}

#[test]
fn abap_search_maps_a_rejection_and_a_failure_like_the_cli() {
    let mut rig = rig();

    rig.session(vec![ok("<a><b></a>")]);

    let rejected = rig.call("abap_search", json!({"conn": "DEV_100", "pattern": "ZCL*"}));

    assert!(rejected.is_error);
    assert!(
        rejected
            .text
            .starts_with("REJECTED: cannot read the search reply"),
        "{}",
        rejected.text
    );

    rig.session(vec![Ok(Response::text(404, ""))]);

    let failed = rig.call("abap_search", json!({"conn": "DEV_100", "pattern": "ZCL*"}));

    assert!(failed.is_error);
    assert!(
        failed.text.contains("the object search answered HTTP 404"),
        "{}",
        failed.text
    );
}

// ---- abap_where_used ----------------------------------------------------------------------------

#[test]
fn abap_where_used_returns_the_cli_lines_and_the_using_objects() {
    let mut rig = rig();
    let calls = rig.session(vec![ok(fixture("usage-prog-750.xml"))]);
    let result = rig.call(
        "abap_where_used",
        json!({"conn": "DEV_100", "type": "PROG", "name": "rsparam"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        result.text,
        "USED-BY PROG RFVISL00 package=FVVI via=ADT\nWHERE-USED: 1"
    );
    assert_eq!(result.structured["count"], 1);
    assert_eq!(
        result.structured["used_by"][0],
        json!({"type": "PROG", "name": "RFVISL00", "package": "FVVI", "via": "ADT"})
    );
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(calls.borrow()[0].method, "POST");
}

#[test]
fn abap_where_used_refuses_what_the_cli_refuses() {
    let mut rig = rig();

    for (arguments, expected) in [
        (
            json!({"conn": "DEV_100", "type": "TRAN", "name": "SE38"}),
            "STOP: ",
        ),
        (
            json!({"conn": "DEV_100", "type": "PROG", "name": "ZP", "max": 5001}),
            "STOP: max must be between 1 and 5000",
        ),
        (
            json!({"conn": "DEV_100", "type": "PROG"}),
            "STOP: name is required",
        ),
    ] {
        let result = rig.call("abap_where_used", arguments.clone());

        assert!(result.is_error, "{arguments}");
        assert!(
            result.text.starts_with(expected),
            "{arguments}: {}",
            result.text
        );
    }

    assert_eq!(rig.opened(), 0);
}

// ---- SNAP refused by the data preview of 7.50 ------------------------------------------------------

/// The reply of the 7.50 data preview that refuses `SNAP`.
fn snap_refused() -> Reply {
    Ok(Response::text(400, fixture("snap-list-750-400.xml")))
}

fn endpoint(reply: Reply) -> RfcEndpointResponse {
    let response = reply.unwrap();

    RfcEndpointResponse {
        status: response.status,
        headers: BTreeMap::new(),
        body: response.body,
    }
}

#[test]
fn abap_dumps_and_dump_fall_back_to_rfc_when_adt_refuses_snap_and_rfc_is_configured() {
    let list_widths = [
        ("DATUM", 8),
        ("UZEIT", 6),
        ("AHOST", 32),
        ("UNAME", 12),
        ("MANDT", 3),
        ("MODNO", 3),
        ("SEQNO", 3),
        ("FLIST", 200),
    ];
    let row = "FC019MESSAGE_TYPE_X_TEXTAP005ZPROGAI006ZINCL AL0023 ";
    let mut rig = rig_routed();

    rig.session(vec![transparent("SNAP"), snap_refused()]);
    rig.fake.rfc.push_back(Ok(Box::new(RfcTransport::new(
        FakeRfcApi::with_responses(vec![
            endpoint(transparent("SNAP")),
            endpoint(snap_refused()),
        ])
        .with_table_replies(vec![Ok(common::read_table_reply(
            &list_widths,
            &[vec![
                "20261103", "053724", "apphost", "TESTUSER", "110", "21", "000", row,
            ]],
        ))]),
    ))));

    let result = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["count"], 1);
    assert_eq!(
        result.structured["dumps"][0]["id"],
        "20261103/053724/apphost/TESTUSER/21"
    );

    let widths = [("SEQNO", 3), ("MANDT", 3), ("FLIST", 200)];
    let mut rig = rig_routed();

    rig.session(vec![transparent("SNAP"), snap_refused()]);
    rig.fake.rfc.push_back(Ok(Box::new(RfcTransport::new(
        FakeRfcApi::with_responses(vec![
            endpoint(transparent("SNAP")),
            endpoint(snap_refused()),
            endpoint(transparent("SNAPT")),
            endpoint(Ok(Response::empty(404))),
        ])
        .with_table_replies(
            std::iter::once(Ok(common::read_table_reply(
                &widths,
                &[vec!["000", "110", "FC007X_ERRORAP005ZPROG"]],
            )))
            .chain((0..7).map(|_| {
                Err(TransportError::PostSendUncertain(
                    "RfcInvoke [FIELD_NOT_VALID]".to_owned(),
                ))
            }))
            .collect(),
        ),
    ))));

    let result = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("error: X_ERROR"), "{}", result.text);
}

#[test]
fn abap_dumps_and_dump_stop_with_the_hint_when_only_adt_is_configured() {
    for (tool, arguments) in [
        ("abap_dumps", json!({"conn": "DEV_100"})),
        ("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP})),
    ] {
        let mut rig = rig();

        rig.session(vec![transparent("SNAP"), snap_refused()]);

        let result = rig.call(tool, arguments);

        assert!(result.is_error, "{tool}: {}", result.text);
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
        assert!(result.text.contains("--backend rfc"), "{}", result.text);
        assert_eq!(rig.opened(), 1, "{tool}");
    }
}

// ---- abap_dumps ---------------------------------------------------------------------------------

#[test]
fn abap_dumps_lists_the_dumps_of_the_logon_user() {
    let mut rig = rig();
    let calls = rig.session(vec![transparent("SNAP"), ok(fixture("snap-list-758.xml"))]);
    let result = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);

    let lines: Vec<&str> = result.text.lines().collect();

    assert_eq!(lines.len(), 3, "{}", result.text);
    assert_eq!(
        lines[0],
        "20261103 053726 TESTUSER 110 error=MESSAGE_TYPE_X_TEXT program=CL_SWF_RUN_WIM_LOCAL==========CP include=CL_SWF_RUN_WIM_LOCAL==========CM0EI line=247 id=20261103/053726/apphost/TESTUSER/21"
    );
    assert_eq!(result.structured["count"], 3);
    assert_eq!(
        result.structured["dumps"][0],
        json!({
            "date": "20261103",
            "time": "053726",
            "user": "TESTUSER",
            "client": "110",
            "error": "MESSAGE_TYPE_X_TEXT",
            "program": "CL_SWF_RUN_WIM_LOCAL==========CP",
            "include": "CL_SWF_RUN_WIM_LOCAL==========CM0EI",
            "line": "247",
            "id": "20261103/053726/apphost/TESTUSER/21",
        })
    );
    assert!(body(&calls.borrow()[1]).contains("UNAME = 'TESTUSER'"));
}

#[test]
fn abap_dumps_all_drops_the_user_filter_and_rows_limits_the_list() {
    let mut rig = rig();
    let calls = rig.session(vec![transparent("SNAP"), ok(fixture("snap-list-758.xml"))]);
    let result = rig.call(
        "abap_dumps",
        json!({"conn": "DEV_100", "all": true, "rows": 2, "date": "20261101"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["count"], 2);

    let sent = body(&calls.borrow()[1]);

    assert!(!sent.contains("UNAME ="), "{sent}");
    assert!(sent.contains("DATUM >= '20261101'"), "{sent}");
}

#[test]
fn abap_dumps_refuses_all_with_a_user_and_a_bad_argument() {
    let mut rig = rig();

    for (arguments, expected) in [
        (
            json!({"conn": "DEV_100", "all": true, "user": "OTHER"}),
            "STOP: --all and --user cannot be used together",
        ),
        (
            json!({"conn": "DEV_100", "all": "yes"}),
            "STOP: all must be a boolean",
        ),
        (
            json!({"conn": "DEV_100", "rows": 1001}),
            "STOP: rows must be between 1 and 1000",
        ),
        (
            json!({"conn": "DEV_100", "date": "2026-11-03"}),
            "STOP: --date",
        ),
    ] {
        let result = rig.call("abap_dumps", arguments.clone());

        assert!(result.is_error, "{arguments}");
        assert!(
            result.text.starts_with(expected),
            "{arguments}: {}",
            result.text
        );
    }

    assert_eq!(rig.opened(), 0);
}

#[test]
fn abap_dumps_honors_a_snap_on_the_denied_list() {
    let mut rig = rig_with(r#"{"read_table_denied":["SNAP"]}"#);
    let result = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(result.is_error);
    assert_eq!(
        result.text,
        "STOP: table SNAP is on the read_table_denied list"
    );
    assert_eq!(rig.opened(), 0);
}

// ---- abap_dump ----------------------------------------------------------------------------------

const OWN_DUMP: &str = "20261103/053726/apphost/TESTUSER/21";
const OTHER_DUMP: &str = "20261103/053726/apphost/OTHERUSER/21";

/// The preview of the rows of a dump whose stream is `stream`: one `SEQNO` with the client.
fn dump_rows(stream: &str) -> String {
    preview(&[
        ("SEQNO", &[Some("000")]),
        ("MANDT", &[Some("110")]),
        ("FLIST", &[Some(stream)]),
    ])
}

fn snapt(lines: &[&str]) -> String {
    let seqno: Vec<String> = (0..lines.len()).map(|n| format!("{n:03}")).collect();
    let seqno: Vec<Option<&str>> = seqno.iter().map(|s| Some(s.as_str())).collect();
    let text: Vec<Option<&str>> = lines.iter().map(|line| Some(*line)).collect();

    preview(&[("SEQNO", &seqno), ("TLINE", &text)])
}

const STREAM: &str = "FC015SOME_ERROR_CODEAP008ZPROGRAMAL001";

#[test]
fn abap_dump_returns_the_cli_text_and_the_header_fields() {
    let mut rig = rig();
    let calls = rig.session(vec![
        transparent("SNAP"),
        ok(dump_rows(&format!("{STREAM}7"))),
        transparent("SNAPT"),
        ok(snapt(&["Something went wrong."])),
    ]);
    let result = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        result.text,
        "error: SOME_ERROR_CODE\n\
         program: ZPROGRAM\n\
         include: -\n\
         line: 7\n\
         user: TESTUSER\n\
         client: 110\n\
         short text:\n\
         Something went wrong."
    );
    assert_eq!(
        result.structured,
        json!({
            "error": "SOME_ERROR_CODE",
            "program": "ZPROGRAM",
            "include": null,
            "line": "7",
            "user": "TESTUSER",
            "client": "110",
            "short_text": ["Something went wrong."],
            "source": [],
        })
    );
    assert_eq!(calls.borrow().len(), 4);
}

#[test]
fn abap_dump_of_another_user_needs_all() {
    let mut rig = rig();
    let result = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OTHER_DUMP}));

    assert!(result.is_error);
    assert!(
        result
            .text
            .starts_with("STOP: the dump belongs to user OTHERUSER"),
        "{}",
        result.text
    );
    assert!(result.text.contains("needs --all"), "{}", result.text);
    assert_eq!(rig.opened(), 0);

    rig.session(vec![
        transparent("SNAP"),
        ok(dump_rows(&format!("{STREAM}7"))),
        transparent("SNAPT"),
        ok(snapt(&["Something went wrong."])),
    ]);

    let result = rig.call(
        "abap_dump",
        json!({"conn": "DEV_100", "id": OTHER_DUMP, "all": true}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["user"], "OTHERUSER");
}

#[test]
fn abap_dump_refuses_a_bad_id_and_a_missing_dump() {
    let mut rig = rig();
    let bad = rig.call("abap_dump", json!({"conn": "DEV_100", "id": "nonsense"}));

    assert!(
        bad.is_error && bad.text.starts_with("STOP: "),
        "{}",
        bad.text
    );
    assert_eq!(rig.opened(), 0);

    rig.session(vec![
        transparent("SNAP"),
        ok(preview(&[("SEQNO", &[]), ("MANDT", &[]), ("FLIST", &[])])),
    ]);

    let missing = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(missing.is_error);
    assert_eq!(
        missing.text,
        format!("REJECTED: dump not found: {OWN_DUMP}")
    );
}

#[test]
fn abap_dump_replaces_a_dump_that_holds_a_configured_secret() {
    let mut rig = rig();

    rig.session(vec![
        transparent("SNAP"),
        ok(dump_rows(&format!("{STREAM}7"))),
        transparent("SNAPT"),
        ok(snapt(&[&format!("the password is {PASSWORD}")])),
    ]);

    let result = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
}

#[test]
fn abap_dump_honors_a_snap_on_the_denied_list() {
    let mut rig = rig_with(r#"{"read_table_denied":["SNAP"]}"#);
    let result = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(result.is_error);
    assert_eq!(
        result.text,
        "STOP: table SNAP is on the read_table_denied list"
    );
    assert_eq!(rig.opened(), 0);
}

// ---- values the structured result must not mix up -------------------------------------------------

/// A search reply of `(type, name, package, description)` objects; a package of `None` is no
/// `packageName` attribute.
fn search_reply(objects: &[(&str, &str, Option<&str>, &str)]) -> String {
    let refs: String = objects
        .iter()
        .map(|(kind, name, package, description)| {
            let package = package
                .map(|package| format!(r#" adtcore:packageName="{package}""#))
                .unwrap_or_default();

            format!(
                r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/x/{}" adtcore:type="{kind}" adtcore:name="{name}"{package} adtcore:description="{description}"/>"#,
                name.to_ascii_lowercase().replace(' ', "_")
            )
        })
        .collect();

    format!(
        r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">{refs}</adtcore:objectReferences>"#
    )
}

#[test]
fn a_search_name_with_spaces_and_a_missing_package_stay_whole_fields() {
    let mut rig = rig();

    rig.session(vec![ok(search_reply(&[
        ("SUSO/B", "Z  AUTH   DEFAULT", None, "two  words   here"),
        ("PROG/P", "ZPLAIN", Some("ZPKG"), "user=X package=Y"),
    ]))]);

    let result = rig.call("abap_search", json!({"conn": "DEV_100", "pattern": "Z*"}));

    assert!(!result.is_error, "{}", result.text);
    // The count of the data is the count of the text.
    assert_eq!(result.structured["count"], 2);
    assert_eq!(result.structured["objects"].as_array().unwrap().len(), 2);
    assert!(
        result.text.ends_with("SEARCH: 2 objects"),
        "{}",
        result.text
    );
    assert_eq!(
        result.structured["objects"][0],
        json!({
            "type": "SUSO/B",
            "name": "Z AUTH DEFAULT",
            "package": null,
            "description": "two words here",
        })
    );
    assert_eq!(result.structured["objects"][1]["package"], "ZPKG");
    assert_eq!(
        result.structured["objects"][1]["description"],
        "user=X package=Y"
    );
    assert!(
        result
            .text
            .contains("SUSO/B Z AUTH DEFAULT package=? two words here")
    );
}

#[test]
fn a_search_object_without_a_usable_name_is_left_out_with_a_note() {
    let mut rig = rig();

    rig.session(vec![ok(search_reply(&[
        ("PROG/P", "ZOK", Some("ZPKG"), "fine"),
        ("PROG/P", "&#x200B;", Some("ZPKG"), "hidden name"),
    ]))]);

    let result = rig.call("abap_search", json!({"conn": "DEV_100", "pattern": "*"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["count"], 1);
    assert!(
        result.structured["notes"]
            .as_str()
            .unwrap()
            .contains("W: 1 object without a usable type or name was left out"),
        "{}",
        result.structured
    );
    assert!(
        result
            .text
            .contains("W: 1 object without a usable type or name")
    );
}

#[test]
fn a_search_result_that_holds_a_configured_secret_is_replaced() {
    let mut rig = rig();

    rig.session(vec![ok(search_reply(&[(
        "PROG/P",
        "ZSECRET",
        Some("ZPKG"),
        &format!("password {PASSWORD}"),
    )]))]);

    let result = rig.call(
        "abap_search",
        json!({"conn": "DEV_100", "pattern": "ZSECRET"}),
    );

    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
}

/// A where-used reply of `(type, name, package)` objects.
fn usage_reply(objects: &[(&str, &str, Option<&str>)]) -> String {
    let refs: String = objects
        .iter()
        .map(|(kind, name, package)| {
            let package = package
                .map(|package| {
                    format!(
                        r#"<adtcore:packageRef adtcore:uri="/sap/bc/adt/vit/wb/object_type/devck/object_name/{package}" adtcore:name="{package}"/>"#
                    )
                })
                .unwrap_or_default();

            format!(
                r#"<usageReferences:referencedObject uri="/sap/bc/adt/programs/programs/x" isResult="false" canHaveChildren="true"><usageReferences:adtObject adtcore:name="{name}" adtcore:type="{kind}" xmlns:adtcore="http://www.sap.com/adt/core">{package}</usageReferences:adtObject></usageReferences:referencedObject>"#
            )
        })
        .collect();

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><usageReferences:usageReferenceResult numberOfResults="{}" resultDescription="d" referencedObjectIdentifier="" xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences"><usageReferences:referencedObjects>{refs}</usageReferences:referencedObjects></usageReferences:usageReferenceResult>"#,
        objects.len()
    )
}

#[test]
fn a_where_used_name_with_spaces_and_a_missing_package_stay_whole_fields() {
    let mut rig = rig();

    rig.session(vec![ok(usage_reply(&[
        ("PROG/P", "ZSPACED   NAME", None),
        ("PROG/P", "ZPLAIN", Some("ZPKG")),
    ]))]);

    let result = rig.call(
        "abap_where_used",
        json!({"conn": "DEV_100", "type": "PROG", "name": "ZTARGET"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["count"], 2);
    assert_eq!(result.structured["used_by"].as_array().unwrap().len(), 2);
    assert!(result.text.ends_with("WHERE-USED: 2"), "{}", result.text);

    let used_by = result.structured["used_by"].as_array().unwrap();
    let spaced = used_by
        .iter()
        .find(|hit| hit["name"] == "ZSPACED NAME")
        .unwrap_or_else(|| panic!("{}", result.structured));

    assert_eq!(spaced["package"], Value::Null);
    assert_eq!(spaced["via"], "ADT");
    assert!(
        result
            .text
            .contains("USED-BY PROG ZSPACED NAME package=? via=ADT"),
        "{}",
        result.text
    );
}

#[test]
fn a_where_used_result_that_holds_a_configured_secret_is_replaced() {
    let mut rig = rig();

    rig.session(vec![ok(usage_reply(&[(
        "PROG/P",
        &format!("Z{PASSWORD}"),
        Some("ZPKG"),
    )]))]);

    let result = rig.call(
        "abap_where_used",
        json!({"conn": "DEV_100", "type": "PROG", "name": "ZTARGET"}),
    );

    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
}

/// The preview of the list of SNAP: one dump per `(date, time, host, user, client, modno, flist)`;
/// a user or a client of `None` is the empty value.
type DumpRowSpec<'a> = (
    &'a str,
    &'a str,
    &'a str,
    Option<&'a str>,
    Option<&'a str>,
    &'a str,
    String,
);

fn snap_list(rows: &[DumpRowSpec<'_>]) -> String {
    let column = |pick: &dyn Fn(&DumpRowSpec<'_>) -> Option<String>| -> Vec<Option<String>> {
        rows.iter().map(pick).collect()
    };
    let columns: Vec<(&str, Vec<Option<String>>)> = vec![
        ("DATUM", column(&|row| Some(row.0.to_owned()))),
        ("UZEIT", column(&|row| Some(row.1.to_owned()))),
        ("AHOST", column(&|row| Some(row.2.to_owned()))),
        ("UNAME", column(&|row| row.3.map(str::to_owned))),
        ("MANDT", column(&|row| row.4.map(str::to_owned))),
        ("MODNO", column(&|row| Some(row.5.to_owned()))),
        ("SEQNO", column(&|_| Some("000".to_owned()))),
        ("FLIST", column(&|row| Some(row.6.clone()))),
    ];
    let borrowed: Vec<(String, Vec<Option<&str>>)> = columns
        .iter()
        .map(|(name, values)| {
            (
                (*name).to_owned(),
                values.iter().map(Option::as_deref).collect(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[Option<&str>])> = borrowed
        .iter()
        .map(|(name, values)| (name.as_str(), values.as_slice()))
        .collect();

    preview(&refs)
}

/// The tag stream: `ID` + three-digit length + value.
fn tags(tags: &[(&str, &str)]) -> String {
    tags.iter()
        .map(|(id, value)| format!("{id}{:03}{value}", value.chars().count()))
        .collect()
}

#[test]
fn hostile_dump_tag_values_stay_in_their_own_fields() {
    let mut rig = rig();

    rig.session(vec![
        transparent("SNAP"),
        ok(snap_list(&[(
            "20261103",
            "053726",
            "apphost",
            Some("TESTUSER"),
            Some("110"),
            "21",
            tags(&[
                ("FC", " user=X client=999 id=FAKE"),
                ("AP", "ZCL_X=====CP"),
                ("AI", "ZCL_X=====CM001"),
                ("AL", "12"),
            ]),
        )])),
    ]);

    let result = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["count"], 1);

    let dump = &result.structured["dumps"][0];

    assert_eq!(dump["user"], "TESTUSER");
    assert_eq!(dump["client"], "110");
    assert_eq!(dump["error"], " user=X client=999 id=FAKE");
    assert_eq!(dump["program"], "ZCL_X=====CP");
    assert_eq!(dump["include"], "ZCL_X=====CM001");
    assert_eq!(dump["line"], "12");
    assert_eq!(dump["id"], "20261103/053726/apphost/TESTUSER/21");
}

#[test]
fn a_dump_with_an_empty_user_or_client_has_null_fields_and_one_line() {
    let mut rig = rig();

    rig.session(vec![
        transparent("SNAP"),
        ok(snap_list(&[
            (
                "20261103",
                "053726",
                "apphost",
                None,
                None,
                "21",
                tags(&[("FC", "SOME_ERROR")]),
            ),
            (
                "20261103",
                "053725",
                "apphost",
                Some("OTHER"),
                Some("110"),
                "22",
                tags(&[("FC", "OTHER_ERROR")]),
            ),
        ])),
    ]);

    let result = rig.call("abap_dumps", json!({"conn": "DEV_100", "all": true}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["count"], 2);
    assert_eq!(result.text.lines().count(), 2, "{}", result.text);

    let empty = &result.structured["dumps"][0];

    assert_eq!(empty["user"], Value::Null);
    assert_eq!(empty["client"], Value::Null);
    assert_eq!(empty["error"], "SOME_ERROR");
    // The row the dump command would refuse says so, in the text and in the data.
    assert_eq!(empty["id"], Value::Null);
    assert!(result.text.lines().next().unwrap().ends_with("id=unusable"));
    assert_eq!(result.structured["dumps"][1]["user"], "OTHER");
}

#[test]
fn a_warning_of_the_dumps_run_lands_in_the_notes() {
    let mut rig = rig();

    rig.session(vec![
        transparent("SNAP"),
        ok(snap_list(&[
            (
                "20261103",
                "053726",
                "apphost",
                Some("TESTUSER"),
                Some("110"),
                "21",
                tags(&[("FC", "MINE")]),
            ),
            (
                "20261103",
                "053725",
                "apphost",
                Some("OTHER"),
                Some("110"),
                "22",
                tags(&[("FC", "NOT_MINE")]),
            ),
        ])),
    ]);

    let result = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["count"], 1);

    let notes = result.structured["notes"].as_str().unwrap();

    assert!(
        notes.starts_with("W: 1 rows of other users than TESTUSER were dropped"),
        "{notes}"
    );
    assert!(result.text.ends_with(notes), "{}", result.text);
}

#[test]
fn no_dumps_is_a_count_of_zero_and_the_text_of_the_cli() {
    let mut rig = rig();

    rig.session(vec![transparent("SNAP"), ok(snap_list(&[]))]);

    let result = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.text, "(no dumps)");
    assert_eq!(result.structured["count"], 0);
    assert_eq!(result.structured["dumps"], json!([]));
}

#[test]
fn a_dump_list_that_holds_a_configured_secret_is_replaced() {
    let mut rig = rig();

    rig.session(vec![
        transparent("SNAP"),
        ok(snap_list(&[(
            "20261103",
            "053726",
            "apphost",
            Some("TESTUSER"),
            Some("110"),
            "21",
            tags(&[("FC", &format!("PASSWORD_{PASSWORD}"))]),
        )])),
    ]);

    let result = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
}

#[test]
fn hostile_short_text_lines_stay_in_the_text_arrays() {
    let mut rig = rig();

    rig.session(vec![
        transparent("SNAP"),
        ok(dump_rows(&format!("{STREAM}7"))),
        transparent("SNAPT"),
        ok(snapt(&["user: ROOT", "source ZX:", "client: 999"])),
    ]);

    let result = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["user"], "TESTUSER");
    assert_eq!(result.structured["client"], "110");
    assert_eq!(
        result.structured["short_text"],
        json!(["user: ROOT", "source ZX:", "client: 999"])
    );
}

// ---- the secret check of table rows ---------------------------------------------------------------

#[test]
fn abap_sql_checks_the_rows_past_the_output_cut_for_a_secret() {
    let mut rig = rig();
    let big = "x".repeat(1_100_000);

    rig.session(vec![
        transparent("TADIR"),
        ok(preview(&[(
            "OBJ_NAME",
            &[Some(big.as_str()), Some(PASSWORD)],
        )])),
    ]);

    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"}),
    );

    // The second row is cut from the text, but the result must not show that it exists.
    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
}

#[test]
fn abap_sql_names_a_cut_once() {
    let mut rig = rig();
    let big = "x".repeat(1_100_000);

    rig.session(vec![
        transparent("TADIR"),
        ok(preview(&[(
            "OBJ_NAME",
            &[Some(big.as_str()), Some("ZSMALL")],
        )])),
    ]);

    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"}),
    );

    assert!(!result.is_error);
    assert!(result.structured["omitted"].as_u64().unwrap() >= 1);
    assert_eq!(
        result
            .text
            .matches("W: output cut at 1048576 bytes")
            .count(),
        1,
        "the cut is said once"
    );
}

// ---- every result leaves through the scrub --------------------------------------------------------

/// What a client can send to make the server say it back: the password, the whole route, and a
/// host of the route that an RFC library may report alone.
fn secrets_to_echo() -> Vec<&'static str> {
    let mut all = vec![PASSWORD, ROUTE];

    all.extend(FRAGMENTS);

    all
}

/// The arguments of each new tool whose refusal quotes the value of `field`.
fn echoing_calls(secret: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "abap_sql",
            json!({"conn": "DEV_100", "query": format!("SELECT {secret} FROM T000")}),
        ),
        (
            "abap_sql",
            json!({"conn": "DEV_100", "query": format!("SELECT * FROM {secret}")}),
        ),
        (
            "abap_search",
            json!({"conn": "DEV_100", "pattern": "Z*", "type": secret}),
        ),
        ("abap_search", json!({"conn": "DEV_100", "pattern": secret})),
        (
            "abap_where_used",
            json!({"conn": "DEV_100", "type": secret, "name": "ZX"}),
        ),
        (
            "abap_where_used",
            json!({"conn": "DEV_100", "type": "PROG", "name": secret}),
        ),
        ("abap_dumps", json!({"conn": "DEV_100", "user": secret})),
        ("abap_dumps", json!({"conn": "DEV_100", "date": secret})),
        ("abap_dump", json!({"conn": "DEV_100", "id": secret})),
        (
            "abap_dump",
            json!({"conn": "DEV_100", "id": format!("{secret}/1/2/3/4")}),
        ),
    ]
}

#[test]
fn a_refusal_never_quotes_a_secret_the_client_sent_in_a_bad_argument() {
    let mut rig = rig_routed();

    for secret in secrets_to_echo() {
        for (tool, arguments) in echoing_calls(secret) {
            let result = rig.call(tool, arguments.clone());
            let structured = result.structured.to_string();

            assert!(result.is_error, "{tool} {arguments}: {}", result.text);
            assert!(
                !result.text.contains(secret),
                "{tool} {arguments}: {}",
                result.text
            );
            assert!(
                !structured.contains(secret),
                "{tool} {arguments}: {structured}"
            );
        }
    }
}

#[test]
fn a_refusal_that_folds_the_case_of_the_secret_is_scrubbed_too() {
    let mut rig = rig_routed();

    // `abap_where_used` upper-cases the type of the client before it names it.
    let result = rig.call(
        "abap_where_used",
        json!({"conn": "DEV_100", "type": PASSWORD, "name": "ZX"}),
    );

    assert!(result.is_error);
    assert!(
        !result.text.to_ascii_lowercase().contains(PASSWORD),
        "{}",
        result.text
    );
    assert!(
        !result
            .structured
            .to_string()
            .to_ascii_lowercase()
            .contains(PASSWORD),
        "{}",
        result.structured
    );
}

#[test]
fn the_route_and_its_fragments_in_a_payload_are_rejected_by_each_new_tool() {
    for secret in [ROUTE, FRAGMENTS[0], FRAGMENTS[1]] {
        let mut rig = rig_routed();
        let calls: Vec<(&str, Value, Vec<Reply>)> = vec![
            (
                "abap_sql",
                json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"}),
                vec![
                    transparent("TADIR"),
                    ok(preview(&[("OBJ_NAME", &[Some(secret)])])),
                ],
            ),
            (
                "abap_search",
                json!({"conn": "DEV_100", "pattern": "Z*"}),
                vec![ok(search_reply(&[("PROG/P", "ZX", Some("ZPKG"), secret)]))],
            ),
            (
                "abap_where_used",
                json!({"conn": "DEV_100", "type": "PROG", "name": "ZTARGET"}),
                vec![ok(usage_reply(&[(
                    "PROG/P",
                    &format!("Z{secret}"),
                    Some("ZPKG"),
                )]))],
            ),
            (
                "abap_dumps",
                json!({"conn": "DEV_100"}),
                vec![
                    transparent("SNAP"),
                    ok(snap_list(&[(
                        "20261103",
                        "053726",
                        "apphost",
                        Some("TESTUSER"),
                        Some("110"),
                        "21",
                        tags(&[("FC", secret)]),
                    )])),
                ],
            ),
            (
                "abap_dump",
                json!({"conn": "DEV_100", "id": OWN_DUMP}),
                vec![
                    transparent("SNAP"),
                    ok(dump_rows(&format!("{STREAM}7"))),
                    transparent("SNAPT"),
                    ok(snapt(&[secret])),
                ],
            ),
        ];

        for (tool, arguments, replies) in calls {
            rig.session(replies);

            let result = rig.call(tool, arguments);

            assert!(result.is_error, "{tool} {secret}: {}", result.text);
            assert_eq!(result.text, SECRET_REJECTED, "{tool} {secret}");
            assert!(
                !result.structured.to_string().contains(secret),
                "{tool} {secret}"
            );
        }
    }
}

#[test]
fn a_failure_that_quotes_a_secret_reaches_the_client_scrubbed() {
    let mut rig = rig_routed();
    let boom = || -> Reply {
        Err(TransportError::Protocol(format!(
            "connection to {ROUTE} failed with password {PASSWORD} via {}",
            FRAGMENTS[0]
        )))
    };

    for (tool, arguments) in [
        ("abap_search", json!({"conn": "DEV_100", "pattern": "Z*"})),
        (
            "abap_where_used",
            json!({"conn": "DEV_100", "type": "PROG", "name": "ZX"}),
        ),
    ] {
        rig.session(vec![boom()]);

        let result = rig.call(tool, arguments);
        let both = format!("{}{}", result.text, result.structured);

        assert!(result.is_error, "{tool}: {}", result.text);
        for secret in secrets_to_echo() {
            assert!(!both.contains(secret), "{tool}: {both}");
        }
    }
}

#[test]
fn a_note_that_quotes_a_secret_reaches_the_client_scrubbed() {
    let mut rig = rig_routed();

    // The packages of a cross-reference listing cannot be read; the warning quotes the failure.
    rig.session(vec![
        ok(discovery_of(&["/sap/bc/adt/ddic/dataelements"])),
        Ok(Response::text(404, "<exc/>")),
        transparent("WBCROSSGT"),
        ok(preview(&[("INCLUDE", &[])])),
        transparent("DD03L"),
        ok(preview(&[("TABNAME", &[Some("ZSTRUCT")])])),
        transparent("TADIR"),
        Err(TransportError::Protocol(format!(
            "cannot read via {ROUTE} with {PASSWORD}"
        ))),
    ]);

    let result = rig.call(
        "abap_where_used",
        json!({"conn": "DEV_100", "type": "DTEL", "name": "ZDE_A"}),
    );
    let both = format!("{}{}", result.text, result.structured);

    assert!(!result.is_error, "{}", result.text);
    assert!(
        result.structured["notes"]
            .as_str()
            .unwrap()
            .contains("W: the packages could not be read"),
        "{}",
        result.structured
    );
    for secret in secrets_to_echo() {
        assert!(!both.contains(secret), "{both}");
    }
}

fn discovery_of(hrefs: &[&str]) -> String {
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

// ---- table text and structure ---------------------------------------------------------------------

#[test]
fn the_text_of_a_table_replaces_tab_and_line_breaks_and_the_rows_keep_the_raw_value() {
    let mut rig = rig();

    rig.session(vec![
        transparent("TADIR"),
        ok(preview(&[("OBJ_NAME", &[Some("a\tb\nc")])])),
    ]);

    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.text, "OBJ_NAME\na b c");
    assert_eq!(result.structured["rows"], json!([["a\tb\nc"]]));
}

#[test]
fn a_secret_that_holds_a_tab_is_found_in_the_raw_cell_by_sql_and_read_table() {
    for tool in ["abap_sql", "abap_read_table"] {
        let mut rig = rig_full("{}", "", "pa\tss-word");
        let arguments = if tool == "abap_sql" {
            json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"})
        } else {
            json!({"conn": "DEV_100", "table": "TADIR"})
        };

        rig.session(vec![
            transparent("TADIR"),
            ok(preview(&[("OBJ_NAME", &[Some("pa\tss-word")])])),
        ]);

        let result = rig.call(tool, arguments);

        // The text would read `pa ss-word`; the raw cell is the secret.
        assert!(result.is_error, "{tool}: {}", result.text);
        assert_eq!(result.text, SECRET_REJECTED, "{tool}");
    }
}

#[test]
fn a_secret_past_the_output_cut_is_found_by_read_table() {
    let mut rig = rig();
    let big = "x".repeat(1_100_000);

    rig.session(vec![
        transparent("TADIR"),
        ok(preview(&[(
            "OBJ_NAME",
            &[Some(big.as_str()), Some(PASSWORD)],
        )])),
    ]);

    let result = rig.call(
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "TADIR"}),
    );

    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
}

// ---- a denied view, end to end ----------------------------------------------------------------------

#[test]
fn abap_sql_refuses_a_view_over_a_denied_table_after_the_lookups_and_sends_nothing_else() {
    let mut rig = rig();
    let calls = rig.session(vec![
        ok(preview(&[
            ("TABNAME", &[Some("ZV_USERS")]),
            ("TABCLASS", &[Some("VIEW")]),
            ("VIEWCLASS", &[Some("D")]),
        ])),
        ok(preview(&[
            ("DDLNAME", &[]),
            ("OBJECTNAME", &[]),
            ("OBJECTTYPE", &[]),
        ])),
        ok(preview(&[
            ("VIEWNAME", &[Some("ZV_USERS"), Some("ZV_USERS")]),
            ("TABNAME", &[Some("T000"), Some("USR02")]),
            ("TABPOS", &[Some("0001"), Some("0002")]),
        ])),
        transparent("T000"),
    ]);
    let result = rig.call(
        "abap_sql",
        json!({"conn": "DEV_100", "query": "SELECT * FROM ZV_USERS"}),
    );

    assert!(result.is_error);
    assert!(result.text.starts_with("STOP: "), "{}", result.text);
    assert!(
        result.text.contains("USR02") && result.text.contains("ZV_USERS"),
        "{}",
        result.text
    );
    assert!(
        result.text.ends_with("no statement was sent"),
        "{}",
        result.text
    );
    assert!(
        calls
            .borrow()
            .iter()
            .all(|call| !call.uri.contains("/datapreview/freestyle")),
        "no freestyle request"
    );
    assert_eq!(calls.borrow().len(), 4);
}

// ---- format characters in dump values ---------------------------------------------------------------

#[test]
fn format_characters_of_a_dump_are_removed_from_every_field_the_text_and_the_arrays() {
    let mut rig = rig();

    rig.session(vec![
        transparent("SNAP"),
        ok(snap_list(&[(
            "20261103",
            "053726",
            "apphost",
            Some("TESTUSER"),
            Some("110"),
            "21",
            tags(&[("FC", "BAD\u{202e}_ERR\u{2066}OR"), ("AP", "ZP\u{202e}ROG")]),
        )])),
    ]);

    let list = rig.call("abap_dumps", json!({"conn": "DEV_100"}));

    assert!(!list.is_error, "{}", list.text);
    assert_eq!(list.structured["dumps"][0]["error"], "BAD_ERROR");
    assert_eq!(list.structured["dumps"][0]["program"], "ZPROG");
    assert!(!list.text.contains('\u{202e}') && !list.text.contains('\u{2066}'));

    let stream = tags(&[
        ("FC", "SOME\u{202e}_ERROR"),
        ("AP", "ZPROGRAM"),
        ("AI", "ZINCL"),
        ("AL", "2"),
    ]);

    rig.session(vec![
        transparent("SNAP"),
        ok(dump_rows(&stream)),
        transparent("SNAPT"),
        ok(snapt(&["text \u{2066}hidden\u{202e} here"])),
        ok("line one\nline \u{202e}two\u{2066}\nline three"),
    ]);

    let dump = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(!dump.is_error, "{}", dump.text);
    assert!(
        !dump.text.contains('\u{202e}') && !dump.text.contains('\u{2066}'),
        "{}",
        dump.text
    );
    assert_eq!(dump.structured["error"], "SOME_ERROR");
    assert_eq!(dump.structured["short_text"], json!(["text hidden here"]));

    let source = dump.structured["source"].to_string();

    assert!(
        !source.contains("\\u202e") && !source.contains('\u{202e}'),
        "{source}"
    );
    assert!(source.contains("line two"), "{source}");
}

#[test]
fn a_dump_source_excerpt_that_holds_a_secret_is_rejected() {
    let mut rig = rig();
    let stream = tags(&[
        ("FC", "SOME_ERROR"),
        ("AP", "ZPROGRAM"),
        ("AI", "ZINCL"),
        ("AL", "2"),
    ]);

    rig.session(vec![
        transparent("SNAP"),
        ok(dump_rows(&stream)),
        transparent("SNAPT"),
        ok(snapt(&["A short text."])),
        ok(format!("line one\nDATA pw = '{PASSWORD}'.\nline three")),
    ]);

    let result = rig.call("abap_dump", json!({"conn": "DEV_100", "id": OWN_DUMP}));

    assert!(result.is_error);
    assert_eq!(result.text, SECRET_REJECTED);
}

// ---- the types of the arguments ---------------------------------------------------------------------

#[test]
fn every_argument_of_the_new_tools_refuses_a_value_of_another_type() {
    let mut rig = rig();
    let wrong_for_string = [json!(5), json!(true), json!({}), json!([]), json!(1.5)];
    let wrong_for_count = [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("5"),
        json!(true),
        json!({}),
        json!([]),
        json!(4_294_967_296_u64),
        json!(18_446_744_073_709_551_615_u64),
        json!(1.0e30),
    ];
    let wrong_for_flag = [
        json!("yes"),
        json!(1),
        json!({}),
        json!([]),
        json!(1.5),
        json!(0),
    ];
    let base = |tool: &str| -> Map<String, Value> {
        let value = match tool {
            "abap_sql" => json!({"conn": "DEV_100", "query": "SELECT OBJ_NAME FROM TADIR"}),
            "abap_search" => json!({"conn": "DEV_100", "pattern": "Z*"}),
            "abap_where_used" => json!({"conn": "DEV_100", "type": "PROG", "name": "ZX"}),
            "abap_dumps" => json!({"conn": "DEV_100"}),
            _ => json!({"conn": "DEV_100", "id": OWN_DUMP}),
        };

        value.as_object().unwrap().clone()
    };
    let strings = [
        ("abap_sql", "query"),
        ("abap_sql", "conn"),
        ("abap_search", "pattern"),
        ("abap_search", "type"),
        ("abap_where_used", "type"),
        ("abap_where_used", "name"),
        ("abap_dumps", "date"),
        ("abap_dumps", "user"),
        ("abap_dump", "id"),
    ];
    let counts = [
        ("abap_sql", "rows", "STOP: rows must be between 1 and 1000"),
        ("abap_search", "max", "STOP: max must be between 1 and 1000"),
        (
            "abap_where_used",
            "max",
            "STOP: max must be between 1 and 5000",
        ),
        (
            "abap_dumps",
            "rows",
            "STOP: rows must be between 1 and 1000",
        ),
    ];
    let flags = [("abap_dumps", "all"), ("abap_dump", "all")];

    for (tool, key) in strings {
        for wrong in &wrong_for_string {
            let mut arguments = base(tool);

            arguments.insert(key.to_owned(), wrong.clone());

            let result = rig.call(tool, Value::Object(arguments));

            assert!(result.is_error, "{tool} {key}={wrong}");
            assert_eq!(
                result.text,
                format!("STOP: {key} must be a string"),
                "{tool} {key}={wrong}"
            );
        }
    }

    for (tool, key, message) in counts {
        for wrong in &wrong_for_count {
            let mut arguments = base(tool);

            arguments.insert(key.to_owned(), wrong.clone());

            let result = rig.call(tool, Value::Object(arguments));

            assert!(result.is_error, "{tool} {key}={wrong}");
            assert_eq!(result.text, message, "{tool} {key}={wrong}");
        }
    }

    for (tool, key) in flags {
        for wrong in &wrong_for_flag {
            let mut arguments = base(tool);

            arguments.insert(key.to_owned(), wrong.clone());

            let result = rig.call(tool, Value::Object(arguments));

            assert!(result.is_error, "{tool} {key}={wrong}");
            assert_eq!(
                result.text,
                format!("STOP: {key} must be a boolean"),
                "{tool} {key}={wrong}"
            );
        }
    }

    // A required argument that is absent or null.
    for (tool, key) in [
        ("abap_sql", "query"),
        ("abap_search", "pattern"),
        ("abap_where_used", "type"),
        ("abap_where_used", "name"),
        ("abap_dump", "id"),
        ("abap_dump", "conn"),
        ("abap_ping", "conn"),
    ] {
        for present in [None, Some(Value::Null)] {
            let mut arguments = base(tool);

            arguments.remove(key);

            if let Some(value) = present.clone() {
                arguments.insert(key.to_owned(), value);
            }

            let result = rig.call(tool, Value::Object(arguments));

            assert!(result.is_error, "{tool} {key}");
            assert_eq!(
                result.text,
                format!("STOP: {key} is required"),
                "{tool} {key}"
            );
        }
    }

    assert_eq!(rig.opened(), 0);
}

// ---- a connection id that names nothing ---------------------------------------------------------

#[test]
fn an_unknown_connection_id_is_refused_without_quoting_it() {
    let mut rig = rig_routed();
    let message =
        "STOP: unknown connection id; use <SID>_<client> of a configured system and client";

    for secret in secrets_to_echo() {
        // No `_`, an unknown system, an unknown client, and an empty part.
        for id in [
            secret.to_owned(),
            format!("{secret}_100"),
            format!("DEV_{secret}"),
            format!("{secret}_"),
            format!("_{secret}"),
        ] {
            for (tool, arguments) in [
                ("abap_search", json!({"conn": id, "pattern": "Z*"})),
                (
                    "abap_sql",
                    json!({"conn": id, "query": "SELECT A FROM T000"}),
                ),
                (
                    "abap_where_used",
                    json!({"conn": id, "type": "PROG", "name": "ZX"}),
                ),
                ("abap_dumps", json!({"conn": id})),
                ("abap_dump", json!({"conn": id, "id": OWN_DUMP})),
                ("abap_ping", json!({"conn": id})),
                ("abap_read_table", json!({"conn": id, "table": "T000"})),
                ("abap_inactive", json!({"conn": id})),
            ] {
                let result = rig.call(tool, arguments);

                assert!(result.is_error, "{tool} {id}");
                assert_eq!(result.text, message, "{tool} {id}");
                assert_eq!(result.structured["message"], message, "{tool} {id}");
            }
        }
    }

    assert_eq!(rig.opened(), 0);
}
