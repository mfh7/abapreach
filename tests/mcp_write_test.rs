//! The MCP write tools against a fake client and fake transports: policy visibility, per-connection
//! refusal, argument parity with the CLI, backend selection, the gate seam and the rule that
//! nothing a client sends authorizes a write. No SAP.

mod common;
mod replay;

use abapreach::backend::BackendKind;
use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{
    Answer, Form, Server, Session, ToolHandler, ToolResult, serve, tool_list_for,
};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{McpTools, Operation, Ticket, TokenGate, WriteGate};
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo, run};
use replay::{fx, no_inactive, replay};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::rc::Rc;
use std::time::Duration;

const PASSWORD: &str = "secret-pw";
const HOST: &str = "example.com";
const WRITE_TOOLS: [&str; 6] = [
    "abap_simulate",
    "abap_deploy",
    "abap_create_simulate",
    "abap_create",
    "abap_delete_simulate",
    "abap_delete",
];
const PROGRAM: &str = "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.";
const CLASS_SOURCE: &str = "CLASS zcl_a DEFINITION PUBLIC.\nENDCLASS.";
const FUNCTION_SOURCE: &str =
    "FUNCTION zfm\n  IMPORTING\n    VALUE(iv_in) TYPE i.\n\n  WRITE / 'x'.\nENDFUNCTION.";
const PROG: &str = "/sap/bc/adt/programs/programs/zprog";
const HANDLE: &str = "a/b+c=";
const ENCODED: &str = "a%2Fb%2Bc%3D";

type Reply = Result<Response, TransportError>;
type Opened = Result<Box<dyn SadtTransport>, TransportError>;
type Log = Rc<RefCell<Vec<String>>>;

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

/// A client that can elicit and types `text` into every form.
struct Typing(&'static str);

impl Session for Typing {
    fn can_elicit(&self) -> bool {
        true
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
        Answer::Accepted(self.0.to_owned())
    }

    fn log(&mut self, _: &str) {}
}

struct CancelTyping {
    cancel: CancelFlag,
}

impl CancelTyping {
    fn new() -> Self {
        Self {
            cancel: CancelFlag::new(),
        }
    }
}

impl Session for CancelTyping {
    fn can_elicit(&self) -> bool {
        true
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }

    fn elicit(
        &mut self,
        _: &Form,
        _: Duration,
        _: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
    ) -> Answer {
        Answer::Accepted("ZPROG".to_owned())
    }

    fn log(&mut self, _: &str) {}
}

struct CancelDuringInspection {
    inner: Script,
    cancel: CancelFlag,
    fired: bool,
}

impl SadtTransport for CancelDuringInspection {
    fn request(&mut self, request: Request) -> Reply {
        if !self.fired && request.method == "GET" && request.uri == PROG {
            self.fired = true;
            self.cancel.set();
        }

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }
}

/// A scripted ADT: replies per method and exact URI, a fallback status for everything else, and a
/// log of every request with its body.
struct Script {
    rules: Vec<(String, String, VecDeque<Reply>)>,
    fallback: Option<u16>,
    log: Log,
}

impl Script {
    fn new(log: &Log) -> Self {
        Self {
            rules: Vec::new(),
            fallback: None,
            log: log.clone(),
        }
    }

    /// Answers every unscripted request with `status`.
    fn always(log: &Log, status: u16) -> Self {
        Self {
            fallback: Some(status),
            ..Self::new(log)
        }
    }

    fn on(mut self, method: &str, uri: &str, reply: Reply) -> Self {
        match self
            .rules
            .iter_mut()
            .find(|(rule_method, rule_uri, _)| rule_method == method && rule_uri == uri)
        {
            Some((_, _, replies)) => replies.push_back(reply),
            None => self
                .rules
                .push((method.to_owned(), uri.to_owned(), VecDeque::from([reply]))),
        }

        self
    }

    fn get(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("GET", uri, Ok(Response::text(status, body)))
    }

    fn post(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("POST", uri, Ok(Response::text(status, body)))
    }
}

impl SadtTransport for Script {
    fn request(&mut self, request: Request) -> Reply {
        self.log.borrow_mut().push(format!(
            "{} {} {}",
            request.method,
            request.uri,
            String::from_utf8_lossy(&request.body)
        ));

        if let Some(reply) = self
            .rules
            .iter_mut()
            .find(|(method, uri, _)| *method == request.method && *uri == request.uri)
            .and_then(|(_, _, replies)| replies.pop_front())
        {
            return reply;
        }

        match self.fallback {
            Some(status) => Ok(Response::empty(status)),
            None => Err(TransportError::RequestNotAllowed(format!(
                "unscripted {} {}",
                request.method, request.uri
            ))),
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
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
        self.adt.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no ADT transport scripted".to_owned(),
            ))
        })
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened.set(self.rfc_opened.get() + 1);
        self.rfc.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no RFC transport scripted".to_owned(),
            ))
        })
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        unreachable!("a write tool never probes the library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        unreachable!("a write tool never pings")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

/// The gate of a test: it issues `tok-<n>`, remembers every ticket and every redemption attempt,
/// and admits a presented ticket whose operation and fields equal the stored one.
#[derive(Default)]
struct GateState {
    stored: BTreeMap<String, Ticket>,
    issued: Vec<Ticket>,
    presented: Vec<(String, Ticket)>,
}

#[derive(Clone, Default)]
struct TestGate(Rc<RefCell<GateState>>);

impl WriteGate for TestGate {
    fn issue(&mut self, ticket: Ticket) -> Option<String> {
        let mut state = self.0.borrow_mut();
        let token = format!("tok-{}", state.issued.len() + 1);

        state.issued.push(ticket.clone());
        state.stored.insert(token.clone(), ticket);

        Some(token)
    }

    fn admit(&mut self, token: &str, presented: &Ticket) -> Result<Ticket, String> {
        let mut state = self.0.borrow_mut();

        state.presented.push((token.to_owned(), presented.clone()));

        match state.stored.remove(token) {
            Some(stored)
                if stored.operation == presented.operation
                    && stored.conn == presented.conn
                    && stored.fields == presented.fields =>
            {
                Ok(stored)
            }
            _ => Err("plan_token does not match this call".to_owned()),
        }
    }
}

impl TestGate {
    fn issued(&self) -> Vec<Ticket> {
        self.0.borrow().issued.clone()
    }

    fn touched(&self) -> bool {
        let state = self.0.borrow();

        !state.issued.is_empty() || !state.presented.is_empty()
    }
}

struct Rig {
    fake: Fake,
    env: BTreeMap<String, String>,
    gate: TestGate,
    _directory: tempfile::TempDir,
}

fn servers(dev: &str, rfc: &str) -> String {
    format!(
        r#"{{"servers":[
            {{"systemId":"DEV","host":"{HOST}","port":443,"clients":["100"],"rfc":{{"ashost":"{HOST}","sysnr":"00"}},"adt":{{"insecure":true}},"policies":{dev}}},
            {{"systemId":"QAS","host":"{HOST}","port":443,"clients":["100"],"rfc":{{"ashost":"{HOST}","sysnr":"00"}},"adt":{{"insecure":true}}}},
            {{"systemId":"RFC","clients":["100"],"rfc":{{"ashost":"{HOST}","sysnr":"00"}},"policies":{rfc}}}
        ]}}"#
    )
}

fn rig_with(config: &str) -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");

    fs::write(&path, config).unwrap();

    let mut env = BTreeMap::from([("ABAP_CONNECTIONS".to_owned(), path.display().to_string())]);

    for id in ["DEV_100", "QAS_100", "RFC_100"] {
        env.insert(format!("{id}_USER"), "user".to_owned());
        env.insert(format!("{id}_PASS"), PASSWORD.to_owned());
    }

    Rig {
        fake: Fake::default(),
        env,
        gate: TestGate::default(),
        _directory: directory,
    }
}

/// DEV allows writes with confirmation, RFC with a token, QAS is off.
fn rig() -> Rig {
    rig_with(&servers(
        r#"{"mcp_write":"confirm"}"#,
        r#"{"mcp_write":"token"}"#,
    ))
}

impl Rig {
    fn config(&self) -> Config {
        Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap()
    }

    fn adt(&mut self, transport: impl SadtTransport + 'static) {
        self.fake.adt.push_back(Ok(Box::new(transport)));
    }

    fn rfc(&mut self, transport: impl SadtTransport + 'static) {
        self.fake.rfc.push_back(Ok(Box::new(transport)));
    }

    fn call(&mut self, name: &str, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.fake),
            self.gate.clone(),
        );

        tools.call(name, arguments.as_object().unwrap())
    }

    fn call_as(&mut self, session: &mut dyn Session, name: &str, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.fake),
            self.gate.clone(),
        );

        tools.call_in(name, arguments.as_object().unwrap(), session)
    }

    fn opened(&self) -> (usize, usize) {
        (self.fake.adt_opened.get(), self.fake.rfc_opened.get())
    }

    /// The CLI on the same configuration, for the parity checks.
    fn cli(&mut self, args: &[&str]) {
        let mut all = vec!["abapreach".to_owned()];

        all.extend(args.iter().map(|arg| (*arg).to_owned()));
        run(all, self.env.clone(), &mut self.fake);
    }
}

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn deploy_arguments() -> Value {
    json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST", "source": PROGRAM})
}

fn valid_arguments(tool: &str, conn: &str) -> Value {
    let mut arguments = match tool {
        "abap_simulate" | "abap_deploy" => deploy_arguments(),
        "abap_create_simulate" | "abap_create" => {
            json!({"type": "FUGR", "name": "ZTESTFG", "text": "T", "package": "ZPKG"})
        }
        _ => json!({"objects": ["PROG:ZPROG"]}),
    };

    arguments["conn"] = json!(conn);

    if tool == "abap_deploy" || tool == "abap_create" || tool == "abap_delete" {
        arguments["plan_token"] = json!("tok-1");
    }

    arguments
}

// ---- tools/list ------------------------------------------------------------------------------

#[test]
fn write_tools_are_listed_with_the_connections_that_may_write() {
    let rig = rig();
    let writable = rig.config().mcp_writable();

    assert_eq!(writable, ["DEV_100", "RFC_100"]);

    let tools = tool_list_for(&writable);
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();

    assert_eq!(names.len(), 8);

    for tool in &tools {
        let description = tool["description"].as_str().unwrap();
        let is_write = WRITE_TOOLS.contains(&tool["name"].as_str().unwrap());

        assert_eq!(description.contains("DEV_100, RFC_100"), is_write, "{tool}");
        assert!(!description.contains("QAS_100"), "{tool}");
        assert_eq!(tool["annotations"]["readOnlyHint"], !is_write);
        assert_eq!(tool["annotations"]["destructiveHint"], is_write);
    }
}

#[test]
fn an_all_off_configuration_lists_reads_only_and_a_write_tool_is_unknown() {
    let mut rig = rig_with(&servers("{}", "{}"));
    let writable = rig.config().mcp_writable();

    assert!(writable.is_empty());
    assert_eq!(tool_list_for(&writable).len(), 2);

    let mut server = Server::new(
        McpTools::new(
            ReadTools::new(rig.config(), rig.env.clone(), &mut rig.fake),
            rig.gate.clone(),
        ),
        false,
    )
    .with_writable(writable);
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "abap_simulate", "arguments": valid_arguments("abap_simulate", "DEV_100"),
    }});
    let (out, mut sink) = (Capture::default(), Vec::new());

    serve(
        &mut server,
        std::io::Cursor::new(format!("{request}\n").into_bytes()),
        out.clone(),
        &mut sink,
    )
    .unwrap();

    let frame: Value = serde_json::from_slice(&out.0.lock().unwrap()).unwrap();

    assert_eq!(frame["error"]["code"], -32602);
    assert_eq!(rig.opened(), (0, 0));
}

// ---- policy and refusal ----------------------------------------------------------------------

#[test]
fn an_off_connection_refuses_every_write_tool_before_any_sap_activity() {
    let mut rig = rig();

    for tool in WRITE_TOOLS {
        let result = rig.call(tool, valid_arguments(tool, "QAS_100"));

        assert!(result.is_error, "{tool}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
        assert!(result.text.contains("mcp_write"), "{}", result.text);
        assert_eq!(result.structured["message"], result.text);
    }

    assert_eq!(rig.opened(), (0, 0));
    assert!(!rig.gate.touched());
}

#[test]
fn a_bad_argument_stops_before_any_connection_or_token() {
    let mut rig = rig();
    let cases = [
        ("abap_simulate", json!({"type": "XXXX"})),
        ("abap_simulate", json!({"source": null})),
        ("abap_simulate", json!({"include": "CCIMP"})),
        ("abap_simulate", json!({"fugr": "ZFG"})),
        (
            "abap_simulate",
            json!({"type": "CLAS", "name": "ZCL_A", "create_include": true}),
        ),
        ("abap_simulate", json!({"create_include": "yes"})),
        ("abap_simulate", json!({"source": ""})),
        ("abap_simulate", json!({"name": ["YTEST"]})),
        ("abap_deploy", json!({"plan_token": null})),
        ("abap_create_simulate", json!({"type": "FUNC"})),
        ("abap_create_simulate", json!({"type": "PROG"})),
        ("abap_create_simulate", json!({"name": "NOTZ"})),
        ("abap_create", json!({"plan_token": 5})),
        ("abap_delete_simulate", json!({"objects": ["XXXX:ZFOO"]})),
        ("abap_delete_simulate", json!({"objects": []})),
        ("abap_delete_simulate", json!({"objects": "PROG:ZFOO"})),
        ("abap_delete_simulate", json!({"objects": [1]})),
        (
            "abap_delete_simulate",
            json!({"objects": ["PROG:ZFOO", "PROG:zfoo"]}),
        ),
        ("abap_delete", json!({"plan_token": null})),
    ];

    for (tool, change) in cases {
        let mut arguments = valid_arguments(tool, "DEV_100");

        for (key, value) in change.as_object().unwrap() {
            if value.is_null() {
                arguments.as_object_mut().unwrap().remove(key);
            } else {
                arguments[key] = value.clone();
            }
        }

        let result = rig.call(tool, arguments.clone());

        assert!(result.is_error, "{tool} {arguments}");
        assert!(result.text.starts_with("STOP: "), "{tool}: {}", result.text);
    }

    assert_eq!(rig.opened(), (0, 0));
    assert!(!rig.gate.touched());
}

#[test]
fn nothing_a_client_sends_authorizes_a_write() {
    let mut rig = rig();
    let mut closed = McpTools::new(
        ReadTools::new(rig.config(), rig.env.clone(), &mut rig.fake),
        TokenGate::new(),
    );

    for tool in ["abap_deploy", "abap_create", "abap_delete"] {
        let mut arguments = valid_arguments(tool, "DEV_100");

        // Hints, a confirmation flag and a made-up token: none of them is an authorization.
        arguments["readOnlyHint"] = json!(true);
        arguments["destructiveHint"] = json!(false);
        arguments["confirm"] = json!(true);

        let arguments: Map<String, Value> = arguments.as_object().unwrap().clone();
        let result = closed.call(tool, &arguments);

        assert!(result.is_error, "{tool}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
    }

    drop(closed);

    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn a_simulation_that_found_nothing_issues_no_token() {
    let mut rig = rig();
    let requests = log();

    rig.adt(Script::always(&requests, 404));

    let mut closed = McpTools::new(
        ReadTools::new(rig.config(), rig.env.clone(), &mut rig.fake),
        TokenGate::new(),
    );
    let arguments = valid_arguments("abap_delete_simulate", "DEV_100");
    let result = closed.call("abap_delete_simulate", arguments.as_object().unwrap());

    assert!(result.structured.get("plan_token").is_none());
    assert!(!result.text.contains("plan_token"));
}

// ---- argument parity with the CLI ------------------------------------------------------------

/// Runs the same operation through MCP and through the CLI, both against an ADT that answers
/// 404 to everything, and returns the two request logs.
fn parity(tool: &str, arguments: Value, cli: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut mcp = rig();
    let mcp_log = log();

    mcp.adt(Script::always(&mcp_log, 404));
    mcp.call(tool, arguments);

    let mut command = rig();
    let cli_log = log();

    command.adt(Script::always(&cli_log, 404));
    command.cli(cli);

    let mcp_requests = mcp_log.borrow().clone();
    let cli_requests = cli_log.borrow().clone();

    (mcp_requests, cli_requests)
}

fn source_file(text: &str) -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.abap");

    fs::write(&path, text).unwrap();

    let path = path.display().to_string();

    (directory, path)
}

#[test]
fn a_deploy_simulation_sends_the_requests_of_the_cli_for_every_argument() {
    let (_class, class_file) = source_file(CLASS_SOURCE);
    let (_function, function_file) = source_file(FUNCTION_SOURCE);
    let (_program, program_file) = source_file(PROGRAM);
    let cases = [
        (
            json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "source": CLASS_SOURCE,
                   "include": "CCIMP", "create_include": true, "tr": "DEVK900001"}),
            vec![
                "deploy",
                "CLAS",
                "ZCL_A",
                &class_file,
                "--simulate",
                "--include",
                "CCIMP",
                "--create-include",
                "--tr",
                "DEVK900001",
                "--conn",
                "DEV_100",
            ],
            "/includes/implementations",
        ),
        (
            json!({"conn": "DEV_100", "type": "FUNC", "name": "ZFM", "source": FUNCTION_SOURCE,
                   "fugr": "ZFG"}),
            vec![
                "deploy",
                "FUNC",
                "ZFM",
                &function_file,
                "--simulate",
                "--fugr",
                "ZFG",
                "--conn",
                "DEV_100",
            ],
            "query=ZFM",
        ),
        (
            json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST_ABAPREACH01",
                   "source": PROGRAM}),
            vec![
                "deploy",
                "PROG",
                "YTEST_ABAPREACH01",
                &program_file,
                "--simulate",
                "--conn",
                "DEV_100",
            ],
            "/programs/programs/ytest_abapreach01",
        ),
    ];

    for (arguments, cli, expected) in cases {
        let (mcp, command) = parity("abap_simulate", arguments, &cli);

        assert!(!mcp.is_empty(), "{cli:?}");
        assert_eq!(mcp, command, "{cli:?}");
        assert!(mcp[0].contains(expected), "{}", mcp[0]);
    }
}

#[test]
fn a_create_simulation_sends_the_requests_of_the_cli() {
    let (mcp, command) = parity(
        "abap_create_simulate",
        json!({"conn": "DEV_100", "type": "FUGR", "name": "ZTESTFG", "text": "T",
               "package": "ZPKG", "tr": "DEVK900001"}),
        &[
            "create",
            "FUGR",
            "ZTESTFG",
            "--text",
            "T",
            "--package",
            "ZPKG",
            "--tr",
            "DEVK900001",
            "--simulate",
            "--conn",
            "DEV_100",
        ],
    );

    assert!(!mcp.is_empty());
    assert_eq!(mcp, command);

    let (mcp, command) = parity(
        "abap_create_simulate",
        json!({"conn": "DEV_100", "type": "FUNC", "name": "ZFM", "text": "T", "fugr": "ZFG"}),
        &[
            "create",
            "FUNC",
            "ZFM",
            "--text",
            "T",
            "--fugr",
            "ZFG",
            "--simulate",
            "--conn",
            "DEV_100",
        ],
    );

    assert!(!mcp.is_empty());
    assert_eq!(mcp, command);
}

#[test]
fn a_delete_simulation_sends_the_requests_of_the_cli() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json").display().to_string();
    let (mcp, command) = parity(
        "abap_delete_simulate",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG", "CLAS:ZCL_A"], "tr": "DEVK900001"}),
        &[
            "delete",
            "--simulate",
            "PROG:ZPROG",
            "CLAS:ZCL_A",
            "--tr",
            "DEVK900001",
            "--plan",
            &plan,
            "--conn",
            "DEV_100",
        ],
    );

    assert_eq!(mcp.len(), 2);
    assert_eq!(mcp, command);
}

// ---- backend selection -----------------------------------------------------------------------

#[test]
fn the_router_picks_the_backend_like_the_cli() {
    let mut adt = rig();
    let requests = log();

    adt.adt(Script::always(&requests, 404));
    adt.call("abap_simulate", deploy_arguments());

    assert_eq!(adt.opened(), (1, 0));

    let mut rfc_only = rig();
    let requests = log();
    let mut arguments = deploy_arguments();

    arguments["conn"] = json!("RFC_100");
    rfc_only.rfc(Script::always(&requests, 404));
    rfc_only.call("abap_simulate", arguments);

    assert_eq!(rfc_only.opened(), (0, 1));
}

#[test]
fn a_connectivity_failure_before_any_write_falls_back_to_rfc_as_in_the_cli() {
    let mut rig = rig();
    let requests = log();

    rig.fake
        .adt
        .push_back(Err(TransportError::PreSendConnectivity("down".to_owned())));
    rig.rfc(Script::always(&requests, 404));

    let result = rig.call("abap_simulate", deploy_arguments());

    assert_eq!(rig.opened(), (1, 1));
    assert!(
        result.text.contains("falling back to RFC SADT"),
        "{}",
        result.text
    );
}

// ---- the seam: tokens and tickets ------------------------------------------------------------

#[test]
fn a_passed_simulation_issues_a_token_bound_to_the_contract_fields() {
    let mut rig = rig();

    rig.adt(replay(
        "dev-750-rfc.json",
        vec![
            fx(6),
            no_inactive(),
            fx(7),
            fx(6),
            no_inactive(),
            fx(8),
            fx(9),
        ],
    ));

    let source = "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.";
    let result = rig.call(
        "abap_simulate",
        json!({"conn": "DEV_100", "type": "PROG", "name": "ytest_abapreach01", "source": source}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["status"], "SIMULATED_OK");
    assert_eq!(result.structured["plan_token"], "tok-1");
    assert!(result.text.contains("plan_token tok-1"), "{}", result.text);

    let issued = rig.gate.issued();

    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].operation, Operation::Deploy);
    assert_eq!(issued[0].conn, "DEV_100");
    assert_eq!(issued[0].backend, Some(BackendKind::AdtHttp));
    assert_eq!(
        issued[0].fields,
        BTreeMap::from([
            ("type".to_owned(), "PROG".to_owned()),
            ("name".to_owned(), "YTEST_ABAPREACH01".to_owned()),
            ("source_sha256".to_owned(), sha256(source)),
            ("create_include".to_owned(), "false".to_owned()),
        ])
    );
}

#[test]
fn a_failed_simulation_and_a_partly_failed_delete_plan_issue_no_token() {
    let mut rig = rig();
    let requests = log();

    rig.adt(Script::always(&requests, 404));

    let deploy = rig.call("abap_simulate", deploy_arguments());

    assert!(deploy.is_error);
    assert!(deploy.structured.get("plan_token").is_none());

    rig.adt(Script::always(&requests, 404));

    let delete = rig.call(
        "abap_delete_simulate",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]}),
    );

    assert!(delete.is_error);
    assert!(delete.structured.get("plan_token").is_none());
    assert_eq!(delete.structured["objects"][0]["status"], "NOT_FOUND");
    assert!(rig.gate.issued().is_empty());
}

fn deploy_ticket(backend: BackendKind) -> Ticket {
    Ticket {
        backend: Some(backend),
        ..Ticket::new(
            Operation::Deploy,
            "DEV_100",
            BTreeMap::from([
                ("type".to_owned(), "PROG".to_owned()),
                ("name".to_owned(), "YTEST".to_owned()),
                ("source_sha256".to_owned(), sha256(PROGRAM)),
                ("create_include".to_owned(), "false".to_owned()),
            ]),
        )
    }
}

#[test]
fn an_admitted_write_runs_on_the_backend_the_ticket_names() {
    // `token`: a deploy under `confirm` would first need a client that can elicit.
    let mut rig = rig_with(&servers(
        r#"{"mcp_write":"token"}"#,
        r#"{"mcp_write":"token"}"#,
    ));
    let requests = log();

    rig.gate
        .0
        .borrow_mut()
        .stored
        .insert("tok-9".to_owned(), deploy_ticket(BackendKind::RfcSadt));
    rig.rfc(Script::always(&requests, 404));

    let mut arguments = deploy_arguments();

    arguments["plan_token"] = json!("tok-9");

    let result = rig.call("abap_deploy", arguments);
    let state = rig.gate.0.borrow();

    assert_eq!(state.presented.len(), 1);
    assert_eq!(state.presented[0].0, "tok-9");
    assert_eq!(state.presented[0].1.operation, Operation::Deploy);
    drop(state);
    // The ticket pinned RFC although ADT is configured: no ADT connection, no fallback.
    assert_eq!(rig.opened(), (0, 1), "{}", result.text);
    assert!(!requests.borrow().is_empty());
}

#[test]
fn a_token_for_other_arguments_runs_nothing() {
    // `token`: the gate answers before any question to a human.
    let mut rig = rig_with(&servers(
        r#"{"mcp_write":"token"}"#,
        r#"{"mcp_write":"token"}"#,
    ));

    rig.gate
        .0
        .borrow_mut()
        .stored
        .insert("tok-9".to_owned(), deploy_ticket(BackendKind::AdtHttp));

    let mut arguments = deploy_arguments();

    arguments["plan_token"] = json!("tok-9");
    arguments["name"] = json!("OTHER");

    let result = rig.call("abap_deploy", arguments);

    assert!(result.is_error);
    assert!(result.text.starts_with("STOP: "), "{}", result.text);
    assert!(result.text.contains("plan_token"), "{}", result.text);
    assert_eq!(rig.opened(), (0, 0));
}

fn lock_xml() -> String {
    format!(
        "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA>\
         <LOCK_HANDLE>{HANDLE}</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>\
         </DATA></asx:values></asx:abap>"
    )
}

fn metadata(responsible: &str) -> String {
    format!(
        "<x:object xmlns:x=\"urn:x\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
         adtcore:responsible=\"{responsible}\"><adtcore:packageRef adtcore:name=\"$TMP\"/>\
         </x:object>"
    )
}

fn prog_reads(requests: &Log, responsible: &str) -> Script {
    Script::new(requests)
        .get(PROG, 200, &metadata(responsible))
        .get(
            &format!("{PROG}/source/main?version=active"),
            200,
            "REPORT zprog.",
        )
        .get(
            &format!("{PROG}/source/main/versions"),
            200,
            "<feed xmlns=\"http://www.w3.org/2005/Atom\"/>",
        )
}

#[test]
fn a_delete_plan_is_simulated_then_executed_from_the_plan_the_ticket_holds() {
    let mut rig = rig();
    let requests = log();

    rig.adt(prog_reads(&requests, "OWNER"));

    let simulated = rig.call(
        "abap_delete_simulate",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]}),
    );

    assert!(!simulated.is_error, "{}", simulated.text);
    assert_eq!(simulated.structured["plan_token"], "tok-1");
    assert_eq!(simulated.structured["objects"][0]["status"], "SIMULATED_OK");
    assert!(simulated.text.contains("DELETE_SIM: SIMULATED_OK"));

    let issued = rig.gate.issued();

    assert_eq!(issued[0].operation, Operation::Delete);
    assert_eq!(
        issued[0].fields,
        BTreeMap::from([("objects".to_owned(), "PROG:ZPROG".to_owned())])
    );
    assert_eq!(issued[0].plan.as_ref().unwrap().conn, "DEV_100");

    rig.adt(
        prog_reads(&requests, "OWNER")
            .post(
                &format!("{PROG}?_action=LOCK&accessMode=MODIFY"),
                200,
                &lock_xml(),
            )
            .on(
                "DELETE",
                &format!("{PROG}?lockHandle={ENCODED}"),
                Ok(Response::empty(200)),
            ),
    );
    rig.adt(Script::new(&requests).get(PROG, 404, ""));

    // A delete always asks the human, whatever the policy: here the human types the name.
    let deleted = rig.call_as(
        &mut Typing("ZPROG"),
        "abap_delete",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"], "plan_token": "tok-1"}),
    );

    assert!(!deleted.is_error, "{}", deleted.text);
    assert_eq!(deleted.structured["objects"][0]["status"], "DELETED");
    assert!(deleted.text.contains("DELETE: DELETED"), "{}", deleted.text);
    assert!(deleted.structured.get("plan_token").is_none());
}

#[test]
fn an_mcp_cancel_during_delete_inspection_sends_no_lock_or_delete() {
    let mut rig = rig();
    let requests = log();
    let mut session = CancelTyping::new();

    rig.adt(prog_reads(&requests, "OWNER"));
    let simulated = rig.call(
        "abap_delete_simulate",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]}),
    );
    let token = simulated.structured["plan_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let cancel = session.cancel();
    rig.adt(CancelDuringInspection {
        inner: prog_reads(&requests, "OWNER"),
        cancel,
        fired: false,
    });
    let result = rig.call_as(
        &mut session,
        "abap_delete",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"], "plan_token": token}),
    );

    assert!(result.is_error, "{}", result.text);
    assert_eq!(result.structured["objects"][0]["status"], "SKIPPED");
    assert!(result.text.contains("W: stopped before PROG ZPROG"));
    assert!(
        !requests
            .borrow()
            .iter()
            .any(|request| request.contains("_action=LOCK"))
    );
    assert!(
        !requests
            .borrow()
            .iter()
            .any(|request| request.starts_with("DELETE "))
    );
}

// ---- diagnostics and payload -----------------------------------------------------------------

#[test]
fn diagnostics_hide_the_secret_and_the_host() {
    let mut rig = rig();
    let leak = format!("cannot reach {HOST} as user with {PASSWORD}");

    rig.fake
        .adt
        .push_back(Err(TransportError::PreSendConnectivity(leak.clone())));
    rig.fake
        .rfc
        .push_back(Err(TransportError::PreSendConnectivity(leak)));

    let result = rig.call("abap_simulate", deploy_arguments());

    assert!(result.is_error);

    for text in [result.text.clone(), result.structured.to_string()] {
        assert!(!text.contains(PASSWORD), "{text}");
        assert!(!text.contains(HOST), "{text}");
    }
}

#[test]
fn delete_diagnostics_scrub_host_password_and_route_in_every_client_field() {
    let route = "/H/router.example.net/S/3299/H/target.example.net/W/route-password";
    let mut rig = rig();
    rig.env.insert("DEV_SAPROUTER".to_owned(), route.to_owned());
    let requests = log();
    let leak = format!("cannot reach {HOST} with {PASSWORD} through {route}");

    rig.adt(Script::new(&requests).on("GET", PROG, Err(TransportError::PreSendConnectivity(leak))));

    let result = rig.call(
        "abap_delete_simulate",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]}),
    );

    assert!(result.is_error);
    for text in [
        result.text.clone(),
        result.structured["output"].to_string(),
        result.structured["objects"][0]["reason"].to_string(),
    ] {
        for secret in [HOST, PASSWORD, route] {
            assert!(!text.contains(secret), "{text}");
        }
    }
}

#[test]
fn a_result_payload_with_a_configured_secret_is_rejected_and_issues_no_token() {
    let mut rig = rig();
    let requests = log();

    // The object author is printed in the plan lines: here it is the password.
    rig.adt(prog_reads(&requests, PASSWORD));

    let result = rig.call(
        "abap_delete_simulate",
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]}),
    );

    assert!(result.is_error);
    assert!(result.text.contains("payload contains a configured secret"));
    assert!(!result.structured.to_string().contains(PASSWORD));
    assert!(!result.text.contains(PASSWORD));
    assert!(rig.gate.issued().is_empty());
}
