//! The MCP server re-reads `connections.json` on every tool call to resolve the connection, but
//! the policies stay the ones it started with. Fake client, scripted ADT, no SAP.

mod common;

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{Answer, Form, Session, ToolHandler, ToolResult};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{McpTools, TokenGate};
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use common::ScriptedTransport;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const WARNING: &str =
    "W: policies changed in connections.json; restart the MCP server to apply them";

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt.pop_front().expect("unexpected ADT connect")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        unreachable!("no RFC in this test")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        unreachable!("a tool never probes the library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        unreachable!("a tool never pings")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

/// A session that keeps the stderr lines.
#[derive(Default)]
struct Logging {
    lines: Vec<String>,
}

impl Session for Logging {
    fn can_elicit(&self) -> bool {
        false
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }

    fn elicit(
        &mut self,
        _: &Form,
        _: Duration,
        _: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
    ) -> Answer {
        Answer::Cancelled
    }

    fn log(&mut self, line: &str) {
        self.lines.push(line.to_owned());
    }
}

struct Rig {
    paths: ConfigPaths,
    env: BTreeMap<String, String>,
    _directory: tempfile::TempDir,
}

fn entry(id: &str, policies: &str) -> String {
    format!(
        r#"{{"systemId":"{id}","host":"example.com","port":443,"clients":["100"],"adt":{{"insecure":true}}{policies}}}"#
    )
}

fn rig(servers: &[String], top: &str) -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let connections: PathBuf = directory.path().join("connections.json");
    let mut env = BTreeMap::from([(
        "ABAP_CONNECTIONS".to_owned(),
        connections.display().to_string(),
    )]);

    for id in ["DEV_100", "DEV_200", "NEW_100"] {
        env.insert(format!("{id}_USER"), "user".to_owned());
        env.insert(format!("{id}_PASS"), "secret-pw".to_owned());
    }

    let rig = Rig {
        paths: ConfigPaths {
            local: directory.path().join("connections.local.json"),
            connections,
        },
        env,
        _directory: directory,
    };

    rig.write(servers, top);

    rig
}

impl Rig {
    fn write(&self, servers: &[String], top: &str) {
        fs::write(
            &self.paths.connections,
            format!(r#"{{{top}"servers":[{}]}}"#, servers.join(",")),
        )
        .unwrap();
    }

    fn start(&self) -> Config {
        Config::load(self.paths.clone(), &self.env).unwrap()
    }

    fn tools<'a>(&self, fake: &'a mut Fake) -> McpTools<'a, TokenGate> {
        McpTools::new(
            ReadTools::new(self.start(), self.env.clone(), fake).with_reload(self.paths.clone()),
            TokenGate::new(),
        )
    }
}

fn call(
    tools: &mut McpTools<'_, TokenGate>,
    session: &mut Logging,
    name: &str,
    arguments: Value,
) -> ToolResult {
    tools.call_in(name, arguments.as_object().unwrap(), session)
}

fn source(text: &str) -> Opened {
    Ok(Box::new(ScriptedTransport::replies(vec![Ok(
        Response::text(200, text),
    )])))
}

const PROGRAM: &str = "REPORT ytest.\nWRITE 'a'.";

#[test]
fn a_connection_added_after_start_resolves() {
    let rig = rig(&[entry("DEV", "")], "");
    let mut fake = Fake::default();

    fake.adt.push_back(source(PROGRAM));

    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();
    let read = json!({"conn": "NEW_100", "type": "PROG", "name": "YTEST"});

    let before = call(&mut tools, &mut session, "abap_read_source", read.clone());

    assert!(before.is_error);

    rig.write(&[entry("DEV", ""), entry("NEW", "")], "");

    let after = call(&mut tools, &mut session, "abap_read_source", read);

    assert!(!after.is_error, "{}", after.text);
    assert_eq!(after.structured["source"], PROGRAM);
    assert!(session.lines.is_empty(), "{:?}", session.lines);
}

#[test]
fn a_changed_policy_is_ignored_and_logged_once_per_call() {
    let rig = rig(&[entry("DEV", "")], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();
    let denied = json!({"conn": "DEV_100", "table": "USR02"});

    let first = call(&mut tools, &mut session, "abap_read_table", denied.clone());

    assert!(first.is_error);
    assert!(first.text.contains("read_table_denied"), "{}", first.text);
    assert!(session.lines.is_empty(), "{:?}", session.lines);

    // An agent empties the deny list in the file while the server runs.
    rig.write(
        &[entry("DEV", "")],
        r#""policies":{"read_table_denied":[]},"#,
    );

    let second = call(&mut tools, &mut session, "abap_read_table", denied);

    assert!(second.is_error);
    assert!(second.text.contains("read_table_denied"), "{}", second.text);
    assert_eq!(session.lines, vec![WARNING.to_owned()]);
}

#[test]
fn a_write_policy_cannot_be_loosened_and_a_new_server_cannot_write() {
    let rig = rig(&[entry("DEV", "")], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(
        &[
            entry("DEV", r#","policies":{"mcp_write":"token"}"#),
            entry("NEW", r#","policies":{"mcp_write":"token"}"#),
        ],
        "",
    );

    for conn in ["DEV_100", "NEW_100"] {
        let result = call(
            &mut tools,
            &mut session,
            "abap_simulate",
            json!({"conn": conn, "type": "PROG", "name": "YTEST", "source": "REPORT ytest."}),
        );

        assert!(result.is_error);
        assert!(
            result
                .text
                .contains(&format!("MCP writes are off for {conn} (policy mcp_write)")),
            "{}",
            result.text
        );
    }

    assert_eq!(session.lines, vec![WARNING.to_owned(), WARNING.to_owned()]);
}

#[test]
fn a_new_server_with_default_policies_logs_nothing_and_keeps_the_default_deny_list() {
    let rig = rig(&[entry("DEV", "")], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(&[entry("DEV", ""), entry("NEW", "")], "");

    let result = call(
        &mut tools,
        &mut session,
        "abap_read_table",
        json!({"conn": "NEW_100", "table": "USR02"}),
    );

    assert!(result.is_error);
    assert!(result.text.contains("read_table_denied"), "{}", result.text);
    assert!(session.lines.is_empty(), "{:?}", session.lines);
}

#[test]
fn a_file_that_no_longer_parses_stops_the_call_and_the_server_stays_up() {
    let rig = rig(&[entry("DEV", "")], "");
    let mut fake = Fake::default();

    fake.adt.push_back(source(PROGRAM));

    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();
    let read = json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"});

    fs::write(&rig.paths.connections, "{ not json").unwrap();

    let broken = call(&mut tools, &mut session, "abap_read_source", read.clone());

    assert!(broken.is_error);
    assert!(
        broken.text.starts_with("STOP: cannot parse "),
        "{}",
        broken.text
    );

    rig.write(&[entry("DEV", "")], "");

    let fixed = call(&mut tools, &mut session, "abap_read_source", read);

    assert!(!fixed.is_error, "{}", fixed.text);
}

#[test]
fn a_server_absent_at_start_keeps_the_start_deny_list_and_the_defaults() {
    // The start list holds a table that is no default; a list replaces the defaults in the file.
    let rig = rig(
        &[entry("DEV", "")],
        r#""policies":{"read_table_denied":["PA0008"],"mcp_write":"token"},"#,
    );
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    // An alias server appears: no deny list of its own, and a looser top-level file.
    rig.write(
        &[entry("DEV", ""), entry("NEW", "")],
        r#""policies":{"read_table_denied":[],"mcp_write":"token"},"#,
    );

    for table in ["PA0008", "USR02"] {
        let result = call(
            &mut tools,
            &mut session,
            "abap_read_table",
            json!({"conn": "NEW_100", "table": table}),
        );

        assert!(result.is_error, "{table}");
        assert!(result.text.contains("read_table_denied"), "{}", result.text);
    }

    let write = call(
        &mut tools,
        &mut session,
        "abap_simulate",
        json!({"conn": "NEW_100", "type": "PROG", "name": "YTEST", "source": "REPORT ytest."}),
    );

    assert!(
        write
            .text
            .contains("MCP writes are off for NEW_100 (policy mcp_write)"),
        "{}",
        write.text
    );
}

// ---- the target of a connection that the server started with is frozen -----------------------

const CHANGED: &str = "STOP: connection DEV_100 changed in connections.json since the MCP server started; restart the MCP server to use it";

fn target(id: &str, host: &str, insecure: bool) -> String {
    format!(
        r#"{{"systemId":"{id}","host":"{host}","port":443,"clients":["100"],"adt":{{"insecure":{insecure}}}}}"#
    )
}

/// A read and a write call of `DEV_100`; neither may open a connection (the fake has no
/// transport to give, so a connect panics) and both stop with the same text.
#[track_caller]
fn assert_refused(tools: &mut McpTools<'_, TokenGate>, session: &mut Logging) {
    let read = call(
        tools,
        session,
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );
    let table = call(
        tools,
        session,
        "abap_read_table",
        json!({"conn": "DEV_100", "table": "T001"}),
    );
    let write = call(
        tools,
        session,
        "abap_simulate",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST", "source": "REPORT ytest."}),
    );

    for result in [read, table, write] {
        assert!(result.is_error);
        assert_eq!(result.text, CHANGED);
    }
}

#[test]
fn a_changed_host_of_a_known_connection_is_refused_and_nothing_is_contacted() {
    let rig = rig(&[target("DEV", "example.com", true)], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(&[target("DEV", "attacker.example.net", true)], "");

    assert_refused(&mut tools, &mut session);
}

#[test]
fn a_changed_insecure_flag_of_a_known_connection_is_refused() {
    let rig = rig(&[target("DEV", "example.com", false)], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(&[target("DEV", "example.com", true)], "");

    assert_refused(&mut tools, &mut session);
}

#[test]
fn a_removed_server_entry_of_a_known_connection_is_refused() {
    let rig = rig(&[target("DEV", "example.com", true)], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(&[target("NEW", "example.com", true)], "");

    assert_refused(&mut tools, &mut session);
}

#[test]
fn a_changed_credential_in_the_local_file_is_refused() {
    // `LOC_100` has no credential in the environment, so they come from the local file.
    let mut rig = rig(&[target("LOC", "example.com", true)], "");

    rig.env.remove("LOC_100_USER");
    std::fs::write(
        &rig.paths.local,
        r#"{"LOC_100_USER":"user","LOC_100_PASS":"first"}"#,
    )
    .unwrap();

    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    std::fs::write(
        &rig.paths.local,
        r#"{"LOC_100_USER":"user","LOC_100_PASS":"second"}"#,
    )
    .unwrap();

    let result = call(
        &mut tools,
        &mut session,
        "abap_read_source",
        json!({"conn": "LOC_100", "type": "PROG", "name": "YTEST"}),
    );

    assert!(result.is_error);
    assert_eq!(
        result.text,
        "STOP: connection LOC_100 changed in connections.json since the MCP server started; \
         restart the MCP server to use it"
    );
}

#[test]
fn an_unchanged_known_connection_works_while_another_entry_changes() {
    let rig = rig(&[target("DEV", "example.com", true)], "");
    let mut fake = Fake::default();

    fake.adt.push_back(source(PROGRAM));

    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(
        &[
            target("DEV", "example.com", true),
            target("NEW", "other.example.com", false),
        ],
        "",
    );

    let result = call(
        &mut tools,
        &mut session,
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["source"], PROGRAM);
}

// ---- an id that the start did not know gets the strictest policies ----------------------------

fn clients(id: &str, host: &str, clients: &str, policies: &str) -> String {
    format!(
        r#"{{"systemId":"{id}","host":"{host}","port":443,"clients":[{clients}],"adt":{{"insecure":true}}{policies}}}"#
    )
}

#[track_caller]
fn assert_write_off(tools: &mut McpTools<'_, TokenGate>, session: &mut Logging, conn: &str) {
    let result = call(
        tools,
        session,
        "abap_simulate",
        json!({"conn": conn, "type": "PROG", "name": "YTEST", "source": "REPORT ytest."}),
    );

    assert!(result.is_error);
    assert!(
        result
            .text
            .contains(&format!("MCP writes are off for {conn} (policy mcp_write)")),
        "{}",
        result.text
    );
}

#[test]
fn a_new_client_of_a_write_enabled_server_cannot_write() {
    let token = r#","policies":{"mcp_write":"token"}"#;
    let rig = rig(&[clients("DEV", "example.com", r#""100""#, token)], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(
        &[clients("DEV", "example.com", r#""100","200""#, token)],
        "",
    );

    assert_write_off(&mut tools, &mut session, "DEV_200");
}

#[test]
fn a_second_entry_with_a_known_system_id_is_not_reachable() {
    let token = r#","policies":{"mcp_write":"token"}"#;
    let rig = rig(&[clients("DEV", "example.com", r#""100""#, token)], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(
        &[
            clients("DEV", "example.com", r#""100""#, token),
            clients("DEV", "other.example.com", r#""200""#, token),
        ],
        "",
    );

    // The first entry of a system id wins at resolution, so the second one is not reachable at
    // all; it can neither write nor connect.
    let result = call(
        &mut tools,
        &mut session,
        "abap_simulate",
        json!({"conn": "DEV_200", "type": "PROG", "name": "YTEST", "source": "REPORT ytest."}),
    );

    assert!(result.is_error);
    assert_eq!(result.text, "STOP: client 200 not configured for DEV");
}

#[test]
fn a_lower_case_start_entry_counts_as_known_and_stays_as_the_start_saw_it() {
    // A system id in lower case never resolved: the start keeps that answer for the session, so
    // upper-casing the entry later does not give the id a fresh target.
    let rig = rig(&[clients("dev", "example.com", r#""100""#, "")], "");
    let mut fake = Fake::default();
    let mut tools = rig.tools(&mut fake);
    let mut session = Logging::default();

    rig.write(
        &[clients("DEV", "attacker.example.net", r#""100""#, "")],
        "",
    );

    let result = call(
        &mut tools,
        &mut session,
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
    );

    assert!(result.is_error);
    assert_eq!(result.text, "STOP: unknown system DEV");
}
