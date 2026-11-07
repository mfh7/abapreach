//! M5 Task 14: the MCP bridge for `create PROG` and the `main` argument of an include, against a
//! fake client and scripted ADT transports. Nothing here touches SAP.

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{Answer, Form, Session, ToolHandler, ToolResult, tool_list_for};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{Limits, McpTools, TokenGate};
use abapreach::object::{check_deploy_options, check_read};
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::rc::Rc;
use std::time::Duration;

const HOST: &str = "example.com";
const NEW_PROG: &str = "YTEST_ABAPREACH_M5N1";
const TEXT: &str = "ABAPReach M5 probe";

type Reply = Result<Response, TransportError>;
type Opened = Result<Box<dyn SadtTransport>, TransportError>;
type Log = Rc<RefCell<Vec<String>>>;

// ---- a scripted ADT ----------------------------------------------------------------------------

/// Replies per method and exact URI; the last reply of a rule repeats. An unscripted request is a
/// failed test, so a request the contract forbids cannot pass unseen.
struct Script {
    rules: Vec<(String, String, Vec<Reply>, usize)>,
    log: Log,
}

impl Script {
    fn new(log: &Log) -> Self {
        Self {
            rules: Vec::new(),
            log: log.clone(),
        }
    }

    fn on(mut self, method: &str, uri: &str, reply: Reply) -> Self {
        match self
            .rules
            .iter_mut()
            .find(|(rule_method, rule_uri, _, _)| rule_method == method && rule_uri == uri)
        {
            Some((_, _, replies, _)) => replies.push(reply),
            None => self
                .rules
                .push((method.to_owned(), uri.to_owned(), vec![reply], 0)),
        }

        self
    }

    fn get(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("GET", uri, Ok(Response::text(status, body)))
    }

    fn post(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("POST", uri, Ok(Response::text(status, body)))
    }

    fn put(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("PUT", uri, Ok(Response::text(status, body)))
    }

    fn delete(self, uri: &str, status: u16) -> Self {
        self.on("DELETE", uri, Ok(Response::empty(status)))
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

        let rule = self
            .rules
            .iter_mut()
            .find(|(method, uri, _, _)| *method == request.method && *uri == request.uri)
            .unwrap_or_else(|| panic!("unscripted request: {} {}", request.method, request.uri));
        let index = rule.3.min(rule.2.len() - 1);

        rule.3 += 1;

        rule.2[index].clone()
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.log.borrow_mut().push("CLOSE".to_owned());

        Ok(())
    }
}

// ---- the fake client and the rig ---------------------------------------------------------------

struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    adt_opened: usize,
    rfc_opened: usize,
}

impl Fake {
    fn new() -> Self {
        Self {
            adt: VecDeque::new(),
            rfc: VecDeque::new(),
            adt_opened: 0,
            rfc_opened: 0,
        }
    }
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened += 1;
        self.adt.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no ADT transport scripted".to_owned(),
            ))
        })
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened += 1;
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
}

struct Rig {
    limits: Option<Limits>,
    fake: Fake,
    env: BTreeMap<String, String>,
    gate: TokenGate,
    directory: tempfile::TempDir,
}

fn servers() -> String {
    let rfc = format!(r#""rfc":{{"ashost":"{HOST}","sysnr":"00"}}"#);
    let adt = r#""adt":{"insecure":true}"#;
    let entry = |id: &str, parts: &str, mode: &str| {
        format!(
            r#"{{"systemId":"{id}","host":"{HOST}","port":443,"clients":["100"],{parts},"policies":{{"mcp_write":"{mode}"}}}}"#
        )
    };

    format!(
        r#"{{"servers":[{},{},{},{}]}}"#,
        entry("DEV", &format!("{rfc},{adt}"), "token"),
        entry("CNF", &format!("{rfc},{adt}"), "confirm"),
        entry("OFF", &format!("{rfc},{adt}"), "off"),
        entry("AOL", adt, "token"),
    )
}

fn rig() -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");

    fs::write(&path, servers()).unwrap();

    let mut env = BTreeMap::from([("ABAP_CONNECTIONS".to_owned(), path.display().to_string())]);

    for id in ["DEV_100", "CNF_100", "OFF_100", "AOL_100"] {
        env.insert(format!("{id}_USER"), "user".to_owned());
        env.insert(format!("{id}_PASS"), "secret-pw".to_owned());
    }

    Rig {
        limits: None,
        fake: Fake::new(),
        env,
        gate: TokenGate::new(),
        directory,
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

impl Rig {
    fn config(&self) -> Config {
        Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap()
    }

    fn adt(&mut self, transport: impl SadtTransport + 'static) {
        self.fake.adt.push_back(Ok(Box::new(transport)));
    }

    fn call(&mut self, name: &str, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.fake),
            self.gate.clone(),
        )
        .with_limits(self.limits.unwrap_or_default());

        tools.call(name, arguments.as_object().unwrap())
    }

    fn call_as(&mut self, session: &mut dyn Session, name: &str, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.fake),
            self.gate.clone(),
        )
        .with_limits(self.limits.unwrap_or_default());

        tools.call_in(name, arguments.as_object().unwrap(), session)
    }

    fn cli(&mut self, args: &[&str]) -> Exit {
        let mut all = vec!["abapreach".to_owned()];

        all.extend(args.iter().map(|arg| (*arg).to_owned()));
        run(all, self.env.clone(), &mut self.fake)
    }

    fn opened(&self) -> (usize, usize) {
        (self.fake.adt_opened, self.fake.rfc_opened)
    }

    fn file(&self, name: &str, text: &str) -> String {
        let path = self.directory.path().join(name);

        fs::write(&path, text).unwrap();

        path.display().to_string()
    }
}

fn log() -> Log {
    Rc::default()
}

fn token_of(result: &ToolResult) -> String {
    result.structured["plan_token"]
        .as_str()
        .unwrap_or_else(|| panic!("no plan_token: {}", result.text))
        .to_owned()
}

fn with_token(mut arguments: Value, token: &str) -> Value {
    arguments["plan_token"] = json!(token);

    arguments
}

// ---- tool schemas ------------------------------------------------------------------------------

#[test]
fn the_tool_schemas_carry_the_m5_arguments() {
    let tools = tool_list_for(&["DEV_100".to_owned()]);
    let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap();

    for name in ["abap_create_simulate", "abap_create"] {
        let properties = &find(name)["inputSchema"]["properties"];
        let kinds: Vec<&str> = properties["type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| kind.as_str().unwrap())
            .collect();

        assert_eq!(kinds, ["PROG", "FUGR", "FUNC", "CLAS", "INTF"], "{name}");
        assert_eq!(properties["subc"]["enum"], json!(["1", "I"]), "{name}");
        assert!(properties.get("prog").is_none(), "{name}");
        // The create tools stay without source and without a file.
        assert!(properties.get("source").is_none(), "{name}");
        assert!(properties.get("file").is_none(), "{name}");
    }

    for name in ["abap_read_source", "abap_simulate", "abap_deploy"] {
        assert_eq!(
            find(name)["inputSchema"]["properties"]["main"]["type"],
            "string",
            "{name}"
        );
    }

    // A read or deploy still offers the M4 object types only.
    for name in ["abap_read_source", "abap_simulate", "abap_deploy"] {
        assert_eq!(
            find(name)["inputSchema"]["properties"]["type"]["enum"],
            json!(["PROG", "FUNC", "CLAS", "INTF"]),
            "{name}"
        );
    }

    for name in ["abap_delete_simulate", "abap_delete"] {
        let description = find(name)["inputSchema"]["properties"]["objects"]["description"]
            .as_str()
            .unwrap();

        assert!(
            description.ends_with("PROG|FUGR|FUNC|CLAS|INTF"),
            "{name}: {description}"
        );
    }

    // The annotations are untouched hints: the server never relies on them.
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        let read = name == "abap_read_source" || name == "abap_read_table";

        assert_eq!(tool["annotations"]["readOnlyHint"], read, "{name}");
        assert_eq!(tool["annotations"]["destructiveHint"], !read, "{name}");
    }
}

// ---- create PROG -------------------------------------------------------------------------------

const PROGRAMS: &str = "/sap/bc/adt/programs/programs";
const INCLUDES: &str = "/sap/bc/adt/programs/includes";
const CHECK_OK: &str = concat!(
    r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
    r#"<CHECK_RESULT>X</CHECK_RESULT></DATA></asx:values></asx:abap>"#,
);

fn new_object(name: &str) -> (String, String) {
    let lower = name.to_ascii_lowercase();

    (format!("{PROGRAMS}/{lower}"), format!("{INCLUDES}/{lower}"))
}

fn validation(object_type: &str) -> String {
    format!(
        "/sap/bc/adt/programs/validation?objname={NEW_PROG}&packagename=%24TMP&description=ABAPReach%20M5%20probe&objtype={object_type}"
    )
}

/// A clean create of `NEW_PROG` in `$TMP`: both URIs answer 404, then validation and the POST.
fn clean_create(log: &Log, subc: &str) -> Script {
    let (program, include) = new_object(NEW_PROG);
    let (collection, object_type) = match subc {
        "I" => (INCLUDES, "PROG/I"),
        _ => (PROGRAMS, "PROG/P"),
    };

    Script::new(log)
        .get(&program, 404, "<exc/>")
        .get(&include, 404, "<exc/>")
        .post(&validation(object_type), 200, CHECK_OK)
        .post(collection, 200, "")
}

fn prog_arguments(subc: &str) -> Value {
    json!({"conn": "DEV_100", "type": "PROG", "name": NEW_PROG, "text": TEXT,
           "package": "$TMP", "subc": subc})
}

#[test]
fn create_prog_through_mcp_creates_a_program_and_an_include_on_their_own_collections() {
    for (subc, collection, kind) in [("1", PROGRAMS, "PROG/P"), ("I", INCLUDES, "PROG/I")] {
        let mut rig = rig();
        let requests = log();

        rig.adt(clean_create(&requests, subc));
        rig.adt(clean_create(&requests, subc));

        let simulated = rig.call("abap_create_simulate", prog_arguments(subc));

        assert!(!simulated.is_error, "{subc}: {}", simulated.text);
        assert_eq!(simulated.structured["status"], "SIMULATED_OK");

        let created = rig.call(
            "abap_create",
            with_token(prog_arguments(subc), &token_of(&simulated)),
        );

        assert!(!created.is_error, "{subc}: {}", created.text);
        assert_eq!(created.structured["status"], "ACTIVE");
        assert!(created.text.contains("CREATE: ACTIVE"), "{}", created.text);

        let posts: Vec<String> = requests
            .borrow()
            .iter()
            .filter(|line| line.starts_with(&format!("POST {collection} ")))
            .cloned()
            .collect();

        assert_eq!(posts.len(), 1, "{subc}: only the create writes");
        assert!(
            posts[0].contains(&format!(r#"adtcore:type="{kind}""#)),
            "{}",
            posts[0]
        );
        assert!(
            posts[0].contains(&format!(r#"adtcore:name="{NEW_PROG}""#)),
            "{}",
            posts[0]
        );
        assert_eq!(rig.opened(), (2, 0));
    }
}

#[test]
fn the_create_token_binds_subc_package_and_text_of_a_prog_create() {
    let mut rig = rig();
    let requests = log();

    rig.adt(clean_create(&requests, "I"));

    let simulated = rig.call("abap_create_simulate", prog_arguments("I"));
    let token = token_of(&simulated);
    // Each change of a bound argument is refused before any connection, and the token survives.
    let variations = [
        ("subc", json!("1")),
        ("package", json!("$OTHER")),
        ("text", json!("another text")),
        ("name", json!("YTEST_ABAPREACH_M5N2")),
    ];

    for (key, value) in variations {
        let mut arguments = with_token(prog_arguments("I"), &token);

        arguments[key] = value;

        let refused = rig.call("abap_create", arguments);

        assert!(refused.is_error, "{key}");
        assert!(
            refused.text.contains("plan_token does not match"),
            "{key}: {}",
            refused.text
        );
        assert_eq!(rig.opened(), (1, 0), "{key}: nothing was opened");
    }

    // The same token with the original arguments is still good.
    rig.adt(clean_create(&requests, "I"));

    let created = rig.call("abap_create", with_token(prog_arguments("I"), &token));

    assert!(!created.is_error, "{}", created.text);
}

#[test]
fn an_omitted_subc_is_the_default_program_and_binds_as_one() {
    let mut rig = rig();
    let requests = log();
    let mut implicit = prog_arguments("1");

    implicit.as_object_mut().unwrap().remove("subc");
    rig.adt(clean_create(&requests, "1"));
    rig.adt(clean_create(&requests, "1"));

    let simulated = rig.call("abap_create_simulate", implicit);

    let created = rig.call(
        "abap_create",
        with_token(prog_arguments("1"), &token_of(&simulated)),
    );

    assert!(!created.is_error, "{}", created.text);
    assert_eq!(created.structured["status"], "ACTIVE");
}

// ---- adapter validation and CLI parity ---------------------------------------------------------

#[test]
fn invalid_combinations_fail_with_the_cli_text_and_without_any_activity() {
    let cases: [(Value, Vec<&str>); 2] = [
        // --subc only applies to PROG
        (
            json!({"type": "CLAS", "name": "ZCL_A", "text": "t", "package": "$TMP", "subc": "I"}),
            vec![
                "create",
                "CLAS",
                "ZCL_A",
                "--text",
                "t",
                "--package",
                "$TMP",
                "--subc",
                "I",
            ],
        ),
        // a transportable package needs a request
        (
            json!({"type": "PROG", "name": NEW_PROG, "text": "t", "package": "ZPKG"}),
            vec![
                "create",
                "PROG",
                NEW_PROG,
                "--text",
                "t",
                "--package",
                "ZPKG",
            ],
        ),
    ];

    for (mut arguments, cli) in cases {
        let mut rig = rig();
        let mut full: Vec<&str> = cli.clone();

        full.extend(["--conn", "dev_100"]);

        let cli = rig.cli(&full);

        arguments["conn"] = json!("DEV_100");

        for tool in ["abap_create_simulate", "abap_create"] {
            let mut arguments = arguments.clone();

            if tool == "abap_create" {
                arguments["plan_token"] = json!("tok");
            }

            let result = rig.call(tool, arguments);

            assert_eq!(cli.code, 2, "{}", cli.stderr);
            assert!(result.is_error, "{tool}");
            assert_eq!(result.text, cli.stderr, "{tool}");
        }

        assert_eq!(rig.opened(), (0, 0));
    }

    // A bad subc value is refused as well.
    let mut rig = rig();
    let mut arguments = prog_arguments("1");

    arguments["subc"] = json!("X");

    let result = rig.call("abap_create_simulate", arguments);

    assert_eq!(result.text, "STOP: subc must be one of 1, I");
    assert_eq!(rig.opened(), (0, 0));
}

// ---- the main program of an include ------------------------------------------------------------

const PROG_A: &str = "/sap/bc/adt/programs/programs/zprog_a";
const INCL_A: &str = "/sap/bc/adt/programs/includes/zincl_a";
const INCL_AS_PROG: &str = "/sap/bc/adt/programs/programs/zincl_a";
const CHECKRUNS: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const ACTIVATE: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";
const HANDLE_QUERY: &str = "lockHandle=h%2B1%2F%3D";
const LIVE: &str = "WRITE / 'v1'.";
const FILE: &str = "WRITE / 'v2'.";

fn active(uri: &str) -> String {
    format!("{uri}/source/main?version=active")
}

fn versions(uri: &str) -> String {
    format!("{uri}/source/main/versions")
}

fn not_found() -> &'static str {
    r#"<exc:exception xmlns:exc="http://www.sap.com/abapxml/types/communicationframework"><type id="ExceptionResourceNotFound"/><message lang="EN">does not exist.</message></exc:exception>"#
}

fn main_programs(names: &[&str]) -> String {
    let references: String = names
        .iter()
        .map(|name| {
            format!(
                r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/programs/programs/{}" adtcore:type="PROG/P" adtcore:name="{name}"/>"#,
                name.to_ascii_lowercase()
            )
        })
        .collect();

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">{references}</adtcore:objectReferences>"#
    )
}

fn feed() -> &'static str {
    r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom"><atom:entry><atom:author><atom:name>user</atom:name></atom:author><atom:id>00000</atom:id></atom:entry></atom:feed>"#
}

fn lock_body() -> &'static str {
    r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA><LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL></DATA></asx:values></asx:abap>"#
}

fn processed() -> &'static str {
    r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkReport chkrun:reporter="abapCheckRun" chkrun:status="processed" chkrun:statusText="checked"/></chkrun:checkRunReports>"#
}

/// The resolution, the main programs and the checks of a `main` the caller names.
fn include_reads(log: &Log, listed: &[&str], main: Option<(&str, &str)>) -> Script {
    include_reads_over(log, LIVE, listed, main)
}

/// Like `include_reads` for an include whose active source is `live` (empty for a new include).
fn include_reads_over(
    log: &Log,
    live: &str,
    listed: &[&str],
    main: Option<(&str, &str)>,
) -> Script {
    let script = Script::new(log)
        .get(&active(INCL_AS_PROG), 404, not_found())
        .get(INCL_AS_PROG, 404, not_found())
        .get(INCL_A, 200, "<include:abapInclude/>")
        .get(&active(INCL_A), 200, live)
        .get(
            &format!("{INCL_A}/mainprograms"),
            200,
            &main_programs(listed),
        );

    match main {
        Some((name, source)) => {
            let uri = format!(
                "/sap/bc/adt/programs/programs/{}",
                name.to_ascii_lowercase()
            );

            script
                .get(&uri, 200, "<program:abapProgram/>")
                .get(&active(&uri), 200, source)
        }
        None => script,
    }
}

/// The lock cycle of a simulate: the include is locked, checked and unlocked, never written.
fn simulate_flow(script: Script) -> Script {
    script
        .get(&versions(INCL_A), 200, feed())
        .post(
            &format!("{INCL_A}?_action=LOCK&accessMode=MODIFY"),
            200,
            lock_body(),
        )
        .post(CHECKRUNS, 200, processed())
        .post(&format!("{INCL_A}?_action=UNLOCK&{HANDLE_QUERY}"), 200, "")
}

fn deploy_flow(script: Script) -> Script {
    simulate_flow(script)
        .put(&format!("{INCL_A}/source/main?{HANDLE_QUERY}"), 200, "")
        .post(ACTIVATE, 200, "")
        // The locked re-read still sees the live source; the read after activation sees the file.
        .get(&active(INCL_A), 200, LIVE)
        .get(&active(INCL_A), 200, FILE)
}

fn include_arguments(main: Option<&str>) -> Value {
    let mut arguments =
        json!({"conn": "DEV_100", "type": "PROG", "name": "ZINCL_A", "source": FILE});

    if let Some(main) = main {
        arguments["main"] = json!(main);
    }

    arguments
}

fn count(log: &Log, needle: &str) -> usize {
    log.borrow()
        .iter()
        .filter(|line| line.contains(needle))
        .count()
}

#[test]
fn an_include_simulate_resolves_the_main_and_the_token_binds_it() {
    let mut rig = rig();
    let requests = log();

    rig.adt(simulate_flow(include_reads(&requests, &["ZPROG_A"], None)));

    let simulated = rig.call("abap_simulate", include_arguments(None));

    assert!(!simulated.is_error, "{}", simulated.text);
    assert_eq!(simulated.structured["status"], "SIMULATED_OK");
    assert!(
        simulated.text.contains("main ZPROG_A"),
        "{}",
        simulated.text
    );
    assert_eq!(count(&requests, "PUT "), 0);

    // The argument stays unbound; the resolved main is what the ticket carries.
    let token = token_of(&simulated);

    // The main program includes the include only through another include (ZPROG_A ->
    // INCLUDE ZINC_TOP. -> INCLUDE ZINCL_A.): the execute must not look for a direct statement.
    rig.adt(deploy_flow(include_reads(&requests, &["ZPROG_A"], None)));

    let deployed = rig.call("abap_deploy", with_token(include_arguments(None), &token));

    assert!(!deployed.is_error, "{}", deployed.text);
    assert_eq!(deployed.structured["status"], "ACTIVE");
    assert_eq!(count(&requests, &active(PROG_A)), 0);
    assert_eq!(count(&requests, "PUT "), 1);
}

/// Runs the execute of an include deploy with a client-given `main` after a simulate that saw
/// `INCLUDE zincl_a.` in the main program; `now` is the main program source at execute time.
fn deploy_with_given_main(now: &str) -> (ToolResult, Log) {
    let mut rig = rig();
    let requests = log();

    rig.adt(simulate_flow(include_reads(
        &requests,
        &["ZPROG_A"],
        Some(("ZPROG_A", "INCLUDE zincl_a.")),
    )));

    let simulated = rig.call("abap_simulate", include_arguments(Some("zprog_a")));

    assert!(!simulated.is_error, "{}", simulated.text);
    assert_eq!(count(&requests, &active(PROG_A)), 1);

    rig.adt(deploy_flow(include_reads(
        &requests,
        &["ZPROG_A"],
        Some(("ZPROG_A", now)),
    )));
    requests.borrow_mut().clear();

    let result = rig.call(
        "abap_deploy",
        with_token(include_arguments(Some("zprog_a")), &token_of(&simulated)),
    );

    (result, requests)
}

#[test]
fn a_client_given_main_is_still_checked_for_the_direct_statement_at_execute() {
    let (deployed, requests) = deploy_with_given_main("INCLUDE zincl_a.");

    assert!(!deployed.is_error, "{}", deployed.text);
    assert_eq!(deployed.structured["status"], "ACTIVE");
    assert_eq!(count(&requests, &active(PROG_A)), 1);
}

#[test]
fn a_client_given_main_that_lost_the_statement_stops_before_the_lock() {
    let (refused, requests) = deploy_with_given_main("REPORT zprog_a.");

    assert!(refused.is_error, "{}", refused.text);
    assert!(
        refused
            .text
            .contains("main program ZPROG_A does not contain the statement INCLUDE ZINCL_A."),
        "{}",
        refused.text
    );
    assert_eq!(count(&requests, "_action=LOCK"), 0);
    assert_eq!(count(&requests, "PUT "), 0);
}

#[test]
fn the_deploy_token_refuses_a_main_that_differs_from_the_resolved_one() {
    let mut rig = rig();
    let requests = log();

    rig.adt(simulate_flow(include_reads(&requests, &["ZPROG_A"], None)));

    let simulated = rig.call("abap_simulate", include_arguments(None));
    let token = token_of(&simulated);

    // The live index changed: the include now belongs to another main program.
    rig.adt(include_reads(
        &requests,
        &["ZPROG_B"],
        Some(("ZPROG_A", "INCLUDE zincl_a.")),
    ));
    requests.borrow_mut().clear();

    let refused = rig.call("abap_deploy", with_token(include_arguments(None), &token));

    assert!(refused.is_error, "{}", refused.text);
    assert!(
        refused
            .text
            .contains("the main program of include ZINCL_A changed since the simulation (ZPROG_A, now ZPROG_B)"),
        "{}",
        refused.text
    );
    assert_eq!(refused.structured["exit"], 2);
    // Nothing was locked, written or activated.
    assert_eq!(count(&requests, "_action=LOCK"), 0);
    assert_eq!(count(&requests, "PUT "), 0);
    assert_eq!(count(&requests, "activation"), 0);
}

#[test]
fn the_main_argument_is_bound_like_the_other_arguments() {
    let mut rig = rig();
    let requests = log();

    rig.adt(simulate_flow(include_reads(
        &requests,
        &["ZPROG_A"],
        Some(("ZPROG_A", "INCLUDE zincl_a.")),
    )));

    let simulated = rig.call("abap_simulate", include_arguments(Some("zprog_a")));

    assert!(!simulated.is_error, "{}", simulated.text);

    let refused = rig.call(
        "abap_deploy",
        with_token(include_arguments(Some("ZPROG_B")), &token_of(&simulated)),
    );

    assert!(refused.is_error);
    assert!(
        refused.text.contains("plan_token does not match"),
        "{}",
        refused.text
    );
    assert_eq!(rig.opened(), (1, 0));
}

#[test]
fn the_main_stop_cases_match_the_cli_and_write_nothing() {
    // (listed, main, main source, expected text)
    let cases: [(&[&str], Option<&str>, &str, &str); 4] = [
        (
            &[],
            None,
            "",
            "STOP: include ZINCL_A has no main program; pass --main <PROG>",
        ),
        (
            &["ZPROG_A", "ZPROG_B"],
            None,
            "",
            "STOP: include ZINCL_A has several main programs (ZPROG_A, ZPROG_B); pass --main <PROG>",
        ),
        (
            &["ZPROG_A"],
            Some("ZPROG_C"),
            "",
            "STOP: main program ZPROG_C is not a main program of include ZINCL_A (listed: ZPROG_A)",
        ),
        (
            &["ZPROG_A"],
            Some("ZPROG_A"),
            "REPORT zprog_a.\n\" INCLUDE zincl_a.",
            "STOP: main program ZPROG_A does not contain the statement INCLUDE ZINCL_A.",
        ),
    ];

    for (listed, main, main_source, expected) in cases {
        let mut rig = rig();
        let requests = log();
        let script = || include_reads(&requests, listed, main.map(|main| (main, main_source)));
        let file = rig.file("zincl_a.abap", FILE);
        let mut cli_args = vec![
            "deploy",
            "PROG",
            "ZINCL_A",
            file.as_str(),
            "--simulate",
            "--conn",
            "dev_100",
        ];

        if let Some(main) = main {
            cli_args.extend(["--main", main]);
        }

        rig.adt(script());

        let cli = rig.cli(&cli_args);

        assert_eq!(cli.code, 2, "{expected}: {}", cli.stderr);
        assert!(cli.stderr.contains(expected), "{expected}: {}", cli.stderr);

        requests.borrow_mut().clear();
        rig.adt(script());

        let result = rig.call("abap_simulate", include_arguments(main));

        assert!(result.is_error, "{expected}");
        assert!(
            result.text.contains(expected),
            "{expected}: {}",
            result.text
        );
        assert!(result.structured.get("plan_token").is_none(), "{expected}");
        assert_eq!(count(&requests, "_action=LOCK"), 0, "{expected}");
        assert_eq!(count(&requests, "PUT "), 0, "{expected}");
    }
}

#[test]
fn main_for_a_program_or_another_type_is_refused_with_the_shared_text() {
    let mut rig = rig();
    let requests = log();

    // The program answers; --main names nothing it may take.
    rig.adt(Script::new(&requests).get(&active(PROG_A), 200, LIVE).get(
        PROG_A,
        200,
        "<program:abapProgram/>",
    ));

    let read = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "ZPROG_A", "main": "ZOTHER"}),
    );

    assert!(read.is_error, "{}", read.text);
    assert!(
        read.text
            .contains("--main is only valid for an include; PROG ZPROG_A is a program"),
        "{}",
        read.text
    );

    // A type without includes is refused locally, with the text the CLI uses, before any request.
    let expected = check_deploy_options(
        abapreach::cli::ObjectKind::Clas,
        "ZCL_A",
        None,
        false,
        None,
        Some("ZPROG_A"),
    )
    .unwrap_err();
    let mut arguments = json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A",
                               "source": "CLASS zcl_a DEFINITION PUBLIC.\nENDCLASS.", "main": "ZPROG_A"});

    for tool in ["abap_simulate", "abap_deploy"] {
        if tool == "abap_deploy" {
            arguments["plan_token"] = json!("tok");
        }

        let result = rig.call(tool, arguments.clone());

        assert_eq!(result.text, format!("STOP: {expected}"), "{tool}");
    }

    let before = rig.opened();
    let read_expected = check_read(
        abapreach::cli::ObjectKind::Func,
        "ZFM",
        std::path::Path::new("-"),
        None,
        Some("ZFG"),
        None,
        Some("ZPROG_A"),
    )
    .unwrap_err();
    let read = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "FUNC", "name": "ZFM", "fugr": "ZFG", "main": "ZPROG_A"}),
    );

    assert_eq!(read.text, format!("STOP: {read_expected}"));
    assert_eq!(rig.opened(), before);
}

#[test]
fn an_include_is_read_through_mcp_with_and_without_a_main() {
    let mut rig = rig();
    let requests = log();

    rig.adt(include_reads(
        &requests,
        &["ZPROG_A"],
        Some(("ZPROG_A", "INCLUDE zincl_a.")),
    ));

    let read = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "ZINCL_A", "main": "ZPROG_A"}),
    );

    assert!(!read.is_error, "{}", read.text);
    assert_eq!(read.structured["source"], LIVE);
    assert!(read.structured["hash"].as_str().is_some());

    rig.adt(include_reads(&requests, &["ZPROG_A"], None));

    let plain = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "PROG", "name": "ZINCL_A"}),
    );

    assert!(!plain.is_error, "{}", plain.text);
    assert_eq!(plain.structured["hash"], read.structured["hash"]);
}

// ---- delete of an include program ----------------------------------------------------------------------------

const ZINC: &str = "/sap/bc/adt/programs/includes/zinc";
const ZINC_AS_PROG: &str = "/sap/bc/adt/programs/programs/zinc";
const HANDLE: &str = "a/b+c=";
const ENCODED: &str = "a%2Fb%2Bc%3D";

fn metadata() -> String {
    "<x:object xmlns:x=\"urn:x\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
     adtcore:responsible=\"OWNER\"><adtcore:packageRef adtcore:name=\"$TMP\"/></x:object>"
        .to_owned()
}

fn lock_xml() -> String {
    format!(
        "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA>\
         <LOCK_HANDLE>{HANDLE}</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>\
         </DATA></asx:values></asx:abap>"
    )
}

fn usage() -> &'static str {
    "<usageReferences:usageReferenceResult xmlns:usageReferences=\"u\"/>"
}

#[test]
fn an_include_prog_is_deleted_through_mcp_on_adt_alone() {
    let mut rig = rig();
    let requests = log();
    let reads = |requests: &Log| {
        Script::new(requests)
            .get(ZINC_AS_PROG, 404, "<exc/>")
            .get(ZINC, 200, &metadata())
            .get(&active(ZINC), 200, "WRITE 1.")
            .get(
                &versions(ZINC),
                200,
                "<feed xmlns=\"http://www.w3.org/2005/Atom\"/>",
            )
            .post(&abapreach::adt::usage_references_uri(ZINC), 200, usage())
    };
    let arguments = json!({"conn": "DEV_100", "objects": ["PROG:ZINC"]});

    rig.adt(reads(&requests));

    let simulated = rig.call("abap_delete_simulate", arguments.clone());

    assert!(!simulated.is_error, "{}", simulated.text);
    rig.adt(
        reads(&requests)
            .post(
                &format!("{ZINC}?_action=LOCK&accessMode=MODIFY"),
                200,
                &lock_xml(),
            )
            .delete(&format!("{ZINC}?lockHandle={ENCODED}"), 200),
    );
    rig.adt(Script::new(&requests).get(ZINC, 404, "<exc/>"));

    let done = rig.call_as(
        &mut Typing("ZINC"),
        "abap_delete",
        with_token(arguments, &token_of(&simulated)),
    );

    assert!(!done.is_error, "{}", done.text);
    assert_eq!(done.structured["objects"][0]["status"], "DELETED");
}

#[test]
fn a_write_window_held_twice_stays_open_until_both_scopes_end() {
    use abapreach::operation::deploy::InterruptAction;

    // The MCP delete holds the whole call while an extension delete holds its own request.
    let cancel = CancelFlag::new();
    let outer = cancel.hold_write();
    let inner = cancel.hold_write();

    drop(inner);
    assert_eq!(cancel.on_interrupt(), InterruptAction::Deferred);
    drop(outer);

    let idle = CancelFlag::new();

    assert_eq!(idle.on_interrupt(), InterruptAction::ExitNow);
}

// ---- the new-include warning ---------------------------------------------------------------------

const NEW_INCLUDE_WARNING: &str =
    "W: no syntax check before the first activation of a new include (main ZPROG_A)";

fn not_processed() -> &'static str {
    r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkReport chkrun:reporter="abapCheckRun" chkrun:status="notProcessed" chkrun:statusText="Select a master program for include ZINCL_A in the properties view"/></chkrun:checkRunReports>"#
}

/// A new include (nothing indexed, empty source) under `--main`: simulate and deploy flows.
fn new_include(log: &Log, write: bool) -> Script {
    let script = include_reads_over(
        log,
        "",
        &[],
        Some(("ZPROG_A", "REPORT zprog_a.\nINCLUDE zincl_a.\n")),
    )
    .get(&versions(INCL_A), 200, feed())
    .post(
        &format!("{INCL_A}?_action=LOCK&accessMode=MODIFY"),
        200,
        lock_body(),
    )
    .post(CHECKRUNS, 200, not_processed())
    .post(&format!("{INCL_A}?_action=UNLOCK&{HANDLE_QUERY}"), 200, "");

    if write {
        script
            .put(&format!("{INCL_A}/source/main?{HANDLE_QUERY}"), 200, "")
            .post(ACTIVATE, 200, "")
            // The locked re-read is empty as well; the read after activation sees the file.
            .get(&active(INCL_A), 200, "")
            .get(&active(INCL_A), 200, FILE)
    } else {
        script
    }
}

/// A client that types `ZINCL_A` into the form and keeps what the form showed.
struct Recording(Rc<RefCell<Vec<String>>>);

impl Session for Recording {
    fn can_elicit(&self) -> bool {
        true
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }

    fn elicit(
        &mut self,
        form: &Form,
        _: Duration,
        _: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
    ) -> Answer {
        self.0.borrow_mut().push(form.message.clone());

        Answer::Accepted("ZINCL_A".to_owned())
    }

    fn log(&mut self, _: &str) {}
}

#[test]
fn the_new_include_warning_reaches_the_confirmation_and_both_results() {
    let mut rig = rig();
    let requests = log();
    let forms = Rc::new(RefCell::new(Vec::new()));
    let mut arguments = include_arguments(Some("ZPROG_A"));

    arguments["conn"] = json!("CNF_100");
    rig.adt(new_include(&requests, false));

    let simulated = rig.call("abap_simulate", arguments.clone());

    assert!(!simulated.is_error, "{}", simulated.text);
    assert_eq!(simulated.structured["status"], "SIMULATED_OK");
    assert!(
        simulated
            .text
            .lines()
            .any(|line| line == NEW_INCLUDE_WARNING),
        "{}",
        simulated.text
    );
    assert!(
        simulated.structured["notes"]
            .as_str()
            .unwrap()
            .contains(NEW_INCLUDE_WARNING)
    );

    rig.adt(new_include(&requests, true));

    let deployed = rig.call_as(
        &mut Recording(forms.clone()),
        "abap_deploy",
        with_token(arguments, &token_of(&simulated)),
    );

    assert!(!deployed.is_error, "{}", deployed.text);
    assert_eq!(deployed.structured["status"], "ACTIVE");

    let shown = forms.borrow();

    assert_eq!(shown.len(), 1);
    // The human sees the exact line before anything is written.
    assert!(
        shown[0].lines().any(|line| line == NEW_INCLUDE_WARNING),
        "{}",
        shown[0]
    );
    assert!(
        deployed
            .text
            .lines()
            .any(|line| line == NEW_INCLUDE_WARNING),
        "{}",
        deployed.text
    );
    assert!(
        deployed.structured["notes"]
            .as_str()
            .unwrap()
            .contains(NEW_INCLUDE_WARNING)
    );
}
