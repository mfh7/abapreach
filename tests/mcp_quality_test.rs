//! The MCP tools of the quality commands: `abap_inactive`, `abap_activate_simulate`,
//! `abap_activate` and `abap_aunit`, with their policies, tokens and confirmations.

mod common;

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{
    Answer, Form, NoSession, Session, ToolHandler, ToolResult, tool_list_for, tool_list_running,
};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{McpTools, TokenGate};
use abapreach::operation::deploy::CancelFlag;
use abapreach::policy::McpRun;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use common::preview;
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::rc::Rc;
use std::time::Duration;

const INACTIVE: &str = include_str!("fixtures/m7/inactive-objects-assumed.xml");
const CLEAN: &str = include_str!("fixtures/m7/aunit-result-clean-assumed.xml");
const FAILED: &str = include_str!("fixtures/m7/aunit-result-failed-assumed.xml");

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

/// A session whose first reply sets no context cookie: the stateless ADT HTTP of a 7.40 system.
struct Stateless(Recording);

impl SadtTransport for Stateless {
    fn request(&mut self, request: Request) -> Reply {
        self.0.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    fn stateful_supported(&self) -> Option<bool> {
        Some(false)
    }
}

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
        unreachable!("no library probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        unreachable!("no ping")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

struct Rig {
    fake: Fake,
    env: BTreeMap<String, String>,
    gate: TokenGate,
    _directory: tempfile::TempDir,
}

const ALLOW_TEST: &str = r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:ZCL_TEST"]}"#;
const ALLOW_AB: &str = r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:ZCL_A","CLAS:ZCL_B"]}"#;

fn rig(policies: &str) -> Rig {
    rig_with(policies, "")
}

/// The same connection with an RFC route, so that a stateless ADT HTTP session can leave for it.
fn rig_with_rfc(policies: &str) -> Rig {
    rig_with(policies, r#","rfc":{"ashost":"example.com","sysnr":"00"}"#)
}

fn rig_with(policies: &str, rfc: &str) -> Rig {
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
        fake: Fake {
            adt: VecDeque::new(),
            rfc: VecDeque::new(),
            opened: Rc::new(Cell::new(0)),
        },
        env: BTreeMap::from([
            ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
            ("DEV_100_USER".to_owned(), "devuser".to_owned()),
            ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
        ]),
        gate: TokenGate::new(),
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

    /// Scripts one stateless ADT session (no context cookie), like a 7.40 system.
    fn stateless_session(&mut self, replies: Vec<Reply>) -> Calls {
        let calls: Calls = Rc::default();

        self.fake.adt.push_back(Ok(Box::new(Stateless(Recording {
            replies: replies.into(),
            calls: calls.clone(),
        }))));

        calls
    }

    /// Scripts one RFC session.
    fn rfc_session(&mut self, replies: Vec<Reply>) -> Calls {
        let calls: Calls = Rc::default();

        self.fake.rfc.push_back(Ok(Box::new(Recording {
            replies: replies.into(),
            calls: calls.clone(),
        })));

        calls
    }

    fn call(&mut self, session: &mut dyn Session, name: &str, arguments: Value) -> ToolResult {
        let config = Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap();
        let mut tools = McpTools::new(
            ReadTools::new(config, self.env.clone(), &mut self.fake),
            self.gate.clone(),
        );

        tools.call_in(name, arguments.as_object().unwrap(), session)
    }

    fn opened(&self) -> usize {
        self.fake.opened.get()
    }
}

/// A client that can answer a form and types `text`; it keeps the text of the last form.
struct Typing(&'static str, usize, String);

impl Typing {
    fn new(text: &'static str) -> Self {
        Self(text, 0, String::new())
    }
}

impl Session for Typing {
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
        self.1 += 1;
        assert!(!form.message.is_empty());
        self.2 = form.message.clone();

        Answer::Accepted(self.0.to_owned())
    }

    fn log(&mut self, _: &str) {}
}

fn client(category: &str) -> Reply {
    Ok(Response::text(
        200,
        preview(&[("CCCATEGORY", &[Some(category)])]),
    ))
}

fn ok(body: &str) -> Reply {
    Ok(Response::text(200, body))
}

fn without(entry: &str) -> String {
    without_all(&[entry])
}

fn without_all(entries: &[&str]) -> String {
    INACTIVE
        .split("<ioc:entry>")
        .filter(|chunk| {
            !entries
                .iter()
                .any(|entry| chunk.contains(&format!("name=\"{entry}\"")))
        })
        .collect::<Vec<_>>()
        .join("<ioc:entry>")
}

fn names(tools: &[Value]) -> Vec<&str> {
    tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect()
}

// ---- tools/list ----------------------------------------------------------------------------------

#[test]
fn abap_aunit_is_listed_only_for_a_server_that_allows_runs() {
    let writable = ["DEV_100".to_owned()];
    let plain = tool_list_for(&writable);
    let running = tool_list_running(&writable, &["DEV_100".to_owned()], &[]);

    assert!(!names(&plain).contains(&"abap_aunit"));
    assert!(!names(&plain).contains(&"abap_run"));
    assert!(names(&running).contains(&"abap_run"));
    assert!(names(&running).contains(&"abap_aunit"));

    // A server that allows runs but no writes lists the read tools and the two run tools only.
    let only_runs = tool_list_running(&[], &["DEV_100".to_owned()], &[]);

    assert_eq!(
        names(&only_runs),
        [
            "abap_read_source",
            "abap_read_table",
            "abap_inactive",
            "abap_sql",
            "abap_search",
            "abap_where_used",
            "abap_dumps",
            "abap_dump",
            "abap_ping",
            "abap_run",
            "abap_aunit"
        ]
    );

    let aunit = running
        .iter()
        .find(|tool| tool["name"] == "abap_aunit")
        .unwrap();

    assert_eq!(aunit["annotations"]["readOnlyHint"], false);
    assert!(aunit["description"].as_str().unwrap().contains("RUNS CODE"));
    assert!(aunit["description"].as_str().unwrap().contains("DEV_100"));
    assert!(abapreach::mcp::RUN_TOOLS.contains(&"abap_aunit"));
}

#[test]
fn the_activation_tools_name_their_arguments_and_the_token() {
    let tools = tool_list_for(&["DEV_100".to_owned()]);
    let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap();
    let keys = |name: &str| -> Vec<String> {
        find(name)["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };

    assert_eq!(keys("abap_activate_simulate"), ["conn", "objects"]);
    assert_eq!(keys("abap_activate"), ["conn", "objects", "plan_token"]);
    assert_eq!(keys("abap_inactive"), ["all", "conn"]);
    assert_eq!(find("abap_inactive")["annotations"]["readOnlyHint"], true);
    assert_eq!(
        find("abap_activate")["annotations"]["destructiveHint"],
        true
    );
}

// ---- abap_inactive -------------------------------------------------------------------------------

#[test]
fn abap_inactive_returns_the_own_entries_as_text_and_data() {
    let mut rig = rig("{}");
    let calls = rig.session(vec![ok(INACTIVE)]);
    let result = rig.call(
        &mut NoSession::new(),
        "abap_inactive",
        json!({"conn": "DEV_100"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(
        result
            .text
            .contains("PROG/I ZTEST_INC DEVUSER main ZTEST_MAIN")
    );
    assert!(!result.text.contains("ZOTHER"));
    assert_eq!(result.structured["count"], 7);
    assert_eq!(result.structured["objects"][1]["main"], "ZTEST_MAIN");
    assert_eq!(calls.borrow().len(), 1);

    // The summary is a line of its own after the entries.
    let lines: Vec<&str> = result.text.lines().collect();

    assert_eq!(lines.len(), 8, "{}", result.text);
    assert_eq!(lines[7], "SUMMARY: 7 inactive objects");
    assert_eq!(lines[6], "TABL/DT ZTEST_TAB DEVUSER");
}

#[test]
fn abap_inactive_without_entries_is_the_summary_alone() {
    let mut rig = rig("{}");

    rig.session(vec![ok(
        r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#,
    )]);

    let result = rig.call(
        &mut NoSession::new(),
        "abap_inactive",
        json!({"conn": "DEV_100"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.text, "SUMMARY: 0 inactive objects");
    assert_eq!(result.structured["count"], 0);
}

#[test]
fn abap_inactive_needs_no_policy_and_refuses_a_bad_flag() {
    let mut rig = rig("{}");
    let result = rig.call(
        &mut NoSession::new(),
        "abap_inactive",
        json!({"conn": "DEV_100", "all": "yes"}),
    );

    assert!(result.is_error);
    assert!(result.text.contains("all must be a boolean"));
    assert_eq!(rig.opened(), 0);
}

// ---- abap_activate -------------------------------------------------------------------------------

fn activate_arguments() -> Value {
    json!({"conn": "DEV_100", "objects": ["PROG/P:ZTEST_MAIN"]})
}

#[test]
fn activation_is_off_without_mcp_write() {
    let mut rig = rig("{}");
    let result = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );

    assert!(result.is_error);
    assert!(result.text.contains("MCP writes are off"));
    assert_eq!(rig.opened(), 0);
}

#[test]
fn a_simulation_issues_a_token_and_the_token_activates_once() {
    let mut rig = rig(r#"{"mcp_write":"token"}"#);
    let simulate = rig.session(vec![client("T"), ok(INACTIVE)]);
    let result = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(
        result
            .text
            .contains("SIMULATED_OK PROG/P:ZTEST_MAIN user DEVUSER")
    );
    assert!(
        simulate
            .borrow()
            .iter()
            .all(|call| !call.uri.contains("activation?"))
    );

    let token = result.structured["plan_token"].as_str().unwrap().to_owned();
    let execute = rig.session(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        ok(&without("ZTEST_MAIN")),
    ]);
    let mut arguments = activate_arguments();

    arguments["plan_token"] = json!(token);

    let result = rig.call(&mut NoSession::new(), "abap_activate", arguments.clone());

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["status"], "ACTIVE");
    assert!(result.text.contains("ACTIVE PROG/P:ZTEST_MAIN"));
    assert!(
        execute
            .borrow()
            .iter()
            .any(|call| call.uri.contains("activation?"))
    );

    // The token is spent.
    let again = rig.call(&mut NoSession::new(), "abap_activate", arguments);

    assert!(again.is_error);
    assert!(again.text.contains("unknown or already used"));
}

#[test]
fn the_token_pins_the_backend_the_simulation_ran_on() {
    let mut rig = rig_with_rfc(r#"{"mcp_write":"token"}"#);

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let simulated = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );

    assert!(!simulated.is_error, "{}", simulated.text);

    // The simulation ran on ADT HTTP. The execution is pinned there, so it does not probe the
    // session for state and does not leave for RFC (no RFC session is scripted).
    let execute = rig.stateless_session(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        ok(&without("ZTEST_MAIN")),
    ]);
    let mut arguments = activate_arguments();

    arguments["plan_token"] = simulated.structured["plan_token"].clone();

    let result = rig.call(&mut NoSession::new(), "abap_activate", arguments);

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["status"], "ACTIVE");
    assert!(
        execute
            .borrow()
            .iter()
            .any(|call| call.uri.contains("activation?"))
    );
}

#[test]
fn a_token_of_other_objects_does_not_activate() {
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let result = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );
    let token = result.structured["plan_token"].as_str().unwrap().to_owned();
    let opened = rig.opened();
    let other = rig.call(
        &mut NoSession::new(),
        "abap_activate",
        json!({"conn": "DEV_100", "objects": ["CLAS/OC:ZCL_TEST"], "plan_token": token}),
    );

    assert!(other.is_error);
    assert!(other.text.contains("does not match"));
    assert_eq!(rig.opened(), opened, "nothing was connected");
}

#[test]
fn a_failed_simulation_issues_no_token() {
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let result = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        json!({"conn": "DEV_100", "objects": ["PROG/P:ZOTHER"]}),
    );

    assert!(result.is_error);
    assert!(result.text.contains("belongs to OTHERUSER"));
    assert!(result.structured.get("plan_token").is_none());
}

#[test]
fn the_local_rules_stop_a_bad_object_before_a_connection() {
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    for objects in [
        json!(["FUGR/F:ZFG"]),
        json!(["PROG/P:SAPMSYST"]),
        json!([]),
        json!("PROG/P:ZA"),
        json!([5]),
    ] {
        let result = rig.call(
            &mut NoSession::new(),
            "abap_activate_simulate",
            json!({"conn": "DEV_100", "objects": objects}),
        );

        assert!(result.is_error, "{objects}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
    }

    assert_eq!(rig.opened(), 0);
}

#[test]
fn under_confirm_the_human_types_the_object_name_before_the_activation() {
    let mut rig = rig(r#"{"mcp_write":"confirm"}"#);

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let simulated = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );
    let mut arguments = activate_arguments();

    arguments["plan_token"] = simulated.structured["plan_token"].clone();

    // A wrong answer activates nothing and spends the token.
    let wrong = rig.call(
        &mut Typing::new("ZOTHER"),
        "abap_activate",
        arguments.clone(),
    );

    assert!(wrong.is_error);
    assert!(wrong.text.contains("did not match"));
    assert_eq!(rig.opened(), 1);

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let simulated = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );

    arguments["plan_token"] = simulated.structured["plan_token"].clone();
    rig.session(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        ok(&without("ZTEST_MAIN")),
    ]);

    let mut typing = Typing::new("ZTEST_MAIN");
    let result = rig.call(&mut typing, "abap_activate", arguments);

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(typing.1, 1);
}

#[test]
fn a_batch_activation_is_confirmed_by_typing_activate_and_its_count() {
    let mut rig = rig(r#"{"mcp_write":"confirm"}"#);
    let mut arguments = json!({
        "conn": "DEV_100",
        "objects": ["PROG/P:ZTEST_MAIN", "INTF/OI:ZIF_TEST"],
    });

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let simulated = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        arguments.clone(),
    );

    assert!(!simulated.is_error, "{}", simulated.text);

    arguments["plan_token"] = simulated.structured["plan_token"].clone();

    // The count of a batch is typed with the verb of the operation, not the one of a test run.
    let mut wrong = Typing::new("RUN 2");
    let refused = rig.call(&mut wrong, "abap_activate", arguments.clone());

    assert!(refused.is_error);
    assert!(refused.text.contains("did not match"), "{}", refused.text);
    assert!(
        wrong.2.contains("Type ACTIVATE 2 to confirm."),
        "{}",
        wrong.2
    );

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let simulated = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        arguments.clone(),
    );

    arguments["plan_token"] = simulated.structured["plan_token"].clone();
    rig.session(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        Ok(Response::empty(200)),
        ok(&without_all(&["ZTEST_MAIN", "ZIF_TEST"])),
    ]);

    let mut typing = Typing::new("ACTIVATE 2");
    let result = rig.call(&mut typing, "abap_activate", arguments);

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(typing.1, 1);
    assert_eq!(result.structured["status"], "ACTIVE");
}

#[test]
fn a_client_that_cannot_confirm_cannot_activate_under_confirm() {
    let mut rig = rig(r#"{"mcp_write":"confirm"}"#);

    rig.session(vec![client("T"), ok(INACTIVE)]);

    let simulated = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );
    let mut arguments = activate_arguments();

    arguments["plan_token"] = simulated.structured["plan_token"].clone();

    let result = rig.call(&mut NoSession::new(), "abap_activate", arguments);

    assert!(result.is_error);
    assert!(result.text.contains("cannot answer a confirmation"));
    assert_eq!(rig.opened(), 1);
}

#[test]
fn a_productive_client_gives_no_token() {
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.session(vec![client("P")]);

    let result = rig.call(
        &mut NoSession::new(),
        "abap_activate_simulate",
        activate_arguments(),
    );

    assert!(result.is_error);
    assert!(result.text.contains("productive"));
    assert!(result.structured.get("plan_token").is_none());
}

// ---- abap_aunit ----------------------------------------------------------------------------------

fn aunit_arguments() -> Value {
    json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_TEST"})
}

#[test]
fn mcp_run_off_refuses_aunit_even_when_writes_are_on() {
    for policies in [
        r#"{"mcp_write":"token"}"#,
        r#"{"mcp_write":"confirm","run":"direct"}"#,
        r#"{"mcp_run":"off"}"#,
    ] {
        let mut rig = rig(policies);
        let result = rig.call(
            &mut Typing::new("ZCL_TEST"),
            "abap_aunit",
            aunit_arguments(),
        );

        assert!(result.is_error);
        assert!(result.text.contains("policy mcp_run"), "{}", result.text);
        assert_eq!(rig.opened(), 0);
    }
}

#[test]
fn run_off_refuses_aunit_in_mcp_whatever_mcp_run_says() {
    let mut rig = rig(r#"{"mcp_run":"confirm","run":"off"}"#);
    let result = rig.call(
        &mut Typing::new("ZCL_TEST"),
        "abap_aunit",
        aunit_arguments(),
    );

    assert!(result.is_error);
    assert!(result.text.contains("policy run"), "{}", result.text);
    assert_eq!(rig.opened(), 0);
}

#[test]
fn aunit_needs_a_client_that_can_confirm() {
    let mut rig = rig(r#"{"mcp_run":"confirm"}"#);
    let result = rig.call(&mut NoSession::new(), "abap_aunit", aunit_arguments());

    assert!(result.is_error);
    assert!(result.text.contains("always needs a human confirmation"));
    assert_eq!(rig.opened(), 0);
}

#[test]
fn aunit_arguments_follow_the_cli_scope_rules() {
    let mut rig = rig(r#"{"mcp_run":"confirm"}"#);

    for arguments in [
        json!({"conn": "DEV_100"}),
        json!({"conn": "DEV_100", "type": "TABL", "name": "ZT"}),
        json!({"conn": "DEV_100", "type": "CLAS"}),
        json!({"conn": "DEV_100", "name": "ZCL_TEST"}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "SAPMSYST"}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_TEST", "package": "ZPKG"}),
        json!({"conn": "DEV_100", "package": "ZPKG", "tr": "DEVK900001"}),
    ] {
        let result = rig.call(
            &mut Typing::new("ZCL_TEST"),
            "abap_aunit",
            arguments.clone(),
        );

        assert!(result.is_error, "{arguments}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
    }

    assert_eq!(rig.opened(), 0);
}

#[test]
fn the_human_confirms_and_the_tests_run() {
    let mut rig = rig(ALLOW_TEST);
    let calls = rig.session(vec![client("T"), ok("<class/>"), ok(CLEAN)]);
    let mut typing = Typing::new("ZCL_TEST");
    let result = rig.call(&mut typing, "abap_aunit", aunit_arguments());

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(typing.1, 1);
    assert_eq!(result.structured["status"], "ACTIVE");
    assert!(result.text.contains("TESTS: 2 methods, 0 alerts"));
    assert!(
        calls
            .borrow()
            .iter()
            .any(|call| call.uri == "/sap/bc/adt/abapunit/testruns")
    );
}

#[test]
fn a_wrong_answer_runs_no_test() {
    let mut rig = rig(ALLOW_TEST);
    let result = rig.call(&mut Typing::new("ZOTHER"), "abap_aunit", aunit_arguments());

    assert!(result.is_error);
    assert!(result.text.contains("did not match"));
    // The human is asked before any connection is opened.
    assert_eq!(rig.opened(), 0);
}

#[test]
fn failing_tests_are_an_error_result_with_the_alert_lines() {
    let mut rig = rig(ALLOW_TEST);

    rig.session(vec![client("T"), ok("<class/>"), ok(FAILED)]);

    let result = rig.call(
        &mut Typing::new("ZCL_TEST"),
        "abap_aunit",
        aunit_arguments(),
    );

    assert!(result.is_error);
    assert_eq!(result.structured["status"], "REJECTED");
    assert!(
        result
            .text
            .contains("failedAssertion critical ZCL_TEST LTCL_TEST->DIVIDE")
    );
}

#[test]
fn a_productive_client_runs_no_test_in_mcp() {
    let mut rig = rig(ALLOW_TEST);
    let calls = rig.session(vec![client("P")]);
    let result = rig.call(
        &mut Typing::new("ZCL_TEST"),
        "abap_aunit",
        aunit_arguments(),
    );

    assert!(result.is_error);
    assert!(result.text.contains("productive"));
    assert_eq!(calls.borrow().len(), 1);
}

#[test]
fn a_package_batch_is_resolved_first_and_the_human_types_the_count() {
    let mut rig = rig(ALLOW_AB);
    let tadir = ok(&preview(&[
        ("OBJECT", &[Some("CLAS"), Some("CLAS")]),
        ("OBJ_NAME", &[Some("ZCL_A"), Some("ZCL_B")]),
    ]));

    // The resolving session, then the running session.
    rig.session(vec![tadir]);

    let calls = rig.session(vec![
        client("T"),
        ok("<class/>"),
        ok(CLEAN),
        ok("<class/>"),
        ok(CLEAN),
    ]);
    let mut typing = Typing::new("RUN 2");
    let result = rig.call(
        &mut typing,
        "abap_aunit",
        json!({"conn": "DEV_100", "package": "ZPKG"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("SUMMARY: 2 OK, 0 FAILED"));
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|call| call.uri.contains("testruns"))
            .count(),
        2
    );
}

#[test]
fn an_object_missing_from_mcp_run_allow_refuses_the_call_before_any_request() {
    let mut rig = rig(r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:ZCL_OTHER"]}"#);
    let calls = rig.session(vec![client("T"), ok("<class/>"), ok(CLEAN)]);
    let mut typing = Typing::new("ZCL_TEST");
    let result = rig.call(&mut typing, "abap_aunit", aunit_arguments());

    assert!(result.is_error);
    assert!(
        result
            .text
            .contains("not on mcp_run_allow of DEV_100: CLAS:ZCL_TEST"),
        "{}",
        result.text
    );
    // The human is not asked and no connection is opened.
    assert_eq!(typing.1, 0);
    assert_eq!(rig.opened(), 0);
    assert!(calls.borrow().is_empty());
}

#[test]
fn an_empty_or_absent_mcp_run_allow_refuses_every_object() {
    for policies in [
        r#"{"mcp_run":"confirm"}"#,
        r#"{"mcp_run":"confirm","mcp_run_allow":[]}"#,
    ] {
        let mut rig = rig(policies);
        let result = rig.call(
            &mut Typing::new("ZCL_TEST"),
            "abap_aunit",
            aunit_arguments(),
        );

        assert!(result.is_error);
        assert!(
            result.text.contains("not on mcp_run_allow"),
            "{}",
            result.text
        );
        assert_eq!(rig.opened(), 0);
    }
}

#[test]
fn the_allow_list_matches_exact_names_without_regard_to_case() {
    let mut lower = rig(r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:zcl_test"]}"#);

    lower.session(vec![client("T"), ok("<class/>"), ok(CLEAN)]);

    let result = lower.call(
        &mut Typing::new("ZCL_TEST"),
        "abap_aunit",
        aunit_arguments(),
    );

    assert!(!result.is_error, "{}", result.text);

    // A prefix of the name is not a match: a pattern entry never loads (see the config tests).
    let mut prefix =
        rig(r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:ZCL_TES","CLAS:ZCL_TEST2"]}"#);
    let result = prefix.call(
        &mut Typing::new("ZCL_TEST"),
        "abap_aunit",
        aunit_arguments(),
    );

    assert!(result.is_error);
    assert_eq!(prefix.opened(), 0);
}

#[test]
fn a_batch_with_one_object_off_the_list_refuses_all_of_it() {
    let mut rig = rig(r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:ZCL_A"]}"#);
    let tadir = ok(&preview(&[
        ("OBJECT", &[Some("CLAS"), Some("CLAS")]),
        ("OBJ_NAME", &[Some("ZCL_A"), Some("ZCL_B")]),
    ]));

    // Only the resolving session is scripted: no test session may open.
    rig.session(vec![tadir]);

    let mut typing = Typing::new("RUN 2");
    let result = rig.call(
        &mut typing,
        "abap_aunit",
        json!({"conn": "DEV_100", "package": "ZPKG"}),
    );

    assert!(result.is_error);
    assert!(
        result
            .text
            .contains("not on mcp_run_allow of DEV_100: CLAS:ZCL_B; nothing was run"),
        "{}",
        result.text
    );
    assert!(!result.text.contains("CLAS:ZCL_A"));
    assert_eq!(typing.1, 0);
}

#[test]
fn an_allow_entry_covers_its_own_object_type_only() {
    // `CLAS:ZCL_TEST` does not allow the report or the function group of that name, and the
    // entries of the other types do not allow the class.
    for (policies, kind) in [
        (
            r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:ZCL_TEST"]}"#,
            "PROG",
        ),
        (
            r#"{"mcp_run":"confirm","mcp_run_allow":["CLAS:ZCL_TEST"]}"#,
            "FUGR",
        ),
        (
            r#"{"mcp_run":"confirm","mcp_run_allow":["PROG:ZCL_TEST","FUGR:ZCL_TEST"]}"#,
            "CLAS",
        ),
    ] {
        let mut rig = rig(policies);
        let mut typing = Typing::new("ZCL_TEST");
        let result = rig.call(
            &mut typing,
            "abap_aunit",
            json!({"conn": "DEV_100", "type": kind, "name": "ZCL_TEST"}),
        );

        assert!(result.is_error, "{kind}");
        assert!(
            result
                .text
                .contains(&format!("not on mcp_run_allow of DEV_100: {kind}:ZCL_TEST")),
            "{}",
            result.text
        );
        assert_eq!(typing.1, 0);
        assert_eq!(rig.opened(), 0);
    }
}

#[test]
fn a_batch_of_exactly_the_listed_maximum_is_confirmed_and_run() {
    let names: Vec<String> = (0..60).map(|number| format!("ZCL_{number:02}")).collect();
    let allow: Vec<String> = names
        .iter()
        .map(|name| format!("\"CLAS:{name}\""))
        .collect();
    let mut rig = rig(&format!(
        r#"{{"mcp_run":"confirm","mcp_run_allow":[{}]}}"#,
        allow.join(",")
    ));
    let kinds = vec![Some("CLAS"); names.len()];
    let objects: Vec<Option<&str>> = names.iter().map(|name| Some(name.as_str())).collect();

    rig.session(vec![ok(&preview(&[
        ("OBJECT", &kinds),
        ("OBJ_NAME", &objects),
    ]))]);

    let mut replies = vec![client("T")];

    for _ in 0..60 {
        replies.push(ok("<class/>"));
        replies.push(ok(CLEAN));
    }

    rig.session(replies);

    let mut typing = Typing::new("RUN 60");
    let result = rig.call(
        &mut typing,
        "abap_aunit",
        json!({"conn": "DEV_100", "package": "ZPKG"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(typing.1, 1);
    assert!(
        result.text.contains("SUMMARY: 60 OK, 0 FAILED"),
        "{}",
        result.text
    );
}

#[test]
fn a_batch_the_confirmation_form_cannot_list_is_refused() {
    let names: Vec<String> = (0..61).map(|number| format!("ZCL_{number:02}")).collect();
    let allow: Vec<String> = names
        .iter()
        .map(|name| format!("\"CLAS:{name}\""))
        .collect();
    let mut rig = rig(&format!(
        r#"{{"mcp_run":"confirm","mcp_run_allow":[{}]}}"#,
        allow.join(",")
    ));
    let kinds = vec![Some("CLAS"); names.len()];
    let objects: Vec<Option<&str>> = names.iter().map(|name| Some(name.as_str())).collect();

    // Only the resolving session is scripted: no test session may open.
    rig.session(vec![ok(&preview(&[
        ("OBJECT", &kinds),
        ("OBJ_NAME", &objects),
    ]))]);

    let mut typing = Typing::new("RUN 61");
    let result = rig.call(
        &mut typing,
        "abap_aunit",
        json!({"conn": "DEV_100", "package": "ZPKG"}),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("61 objects found")
            && result.text.contains("lists at most 60")
            && result.text.contains("narrow the package or request"),
        "{}",
        result.text
    );
    assert_eq!(typing.1, 0);
    assert_eq!(rig.opened(), 1);
}

#[test]
fn run_off_refuses_abap_aunit_before_the_allow_list_and_hides_the_tools() {
    let mut rig = rig(r#"{"mcp_run":"confirm","run":"off","mcp_run_allow":["CLAS:ZCL_TEST"]}"#);
    let result = rig.call(
        &mut Typing::new("ZCL_TEST"),
        "abap_aunit",
        aunit_arguments(),
    );

    assert!(result.is_error);
    assert!(result.text.contains("policy run"), "{}", result.text);
    assert_eq!(rig.opened(), 0);

    let config = Config::load(ConfigPaths::from_env(&rig.env).unwrap(), &rig.env).unwrap();

    assert!(config.mcp_runnable().is_empty());
}

#[test]
fn the_aunit_form_names_the_backend_the_run_will_use() {
    // One backend configured: the form needs no request to name it.
    let mut rig = rig(ALLOW_TEST);

    rig.session(vec![client("T"), ok("<class/>"), ok(CLEAN)]);

    let mut typing = Typing::new("ZCL_TEST");
    let result = rig.call(&mut typing, "abap_aunit", aunit_arguments());

    assert!(!result.is_error, "{}", result.text);
    assert!(
        typing.2.contains("(SID DEV, client 100), backend adt"),
        "{}",
        typing.2
    );
    assert!(!typing.2.contains("unknown"), "{}", typing.2);
}

#[test]
fn a_stateful_adt_session_with_an_rfc_route_stays_on_adt_in_form_and_run() {
    let mut rig = rig_with_rfc(ALLOW_TEST);
    // The probe session, then the running session; both keep state.
    let probe = rig.session(vec![ok("<class/>")]);
    let run = rig.session(vec![client("T"), ok("<class/>"), ok(CLEAN)]);
    let mut typing = Typing::new("ZCL_TEST");
    let result = rig.call(&mut typing, "abap_aunit", aunit_arguments());

    assert!(!result.is_error, "{}", result.text);
    assert!(typing.2.contains("backend adt"), "{}", typing.2);
    assert_eq!(probe.borrow().len(), 1);
    assert!(
        run.borrow()
            .iter()
            .any(|call| call.uri == "/sap/bc/adt/abapunit/testruns")
    );
}

#[test]
fn a_stateless_adt_session_makes_the_form_name_rfc_and_the_run_goes_there() {
    let mut rig = rig_with_rfc(ALLOW_TEST);
    // The probe reaches the stateless session once and nothing is written there; the RFC route
    // of the probe is opened but needs no request, its backend is already settled.
    let adt = rig.stateless_session(vec![ok("<class/>")]);
    let rfc = rig.rfc_session(vec![]);
    let run = rig.rfc_session(vec![client("T"), ok("<class/>"), ok(CLEAN)]);
    let mut typing = Typing::new("ZCL_TEST");
    let result = rig.call(&mut typing, "abap_aunit", aunit_arguments());

    assert!(!result.is_error, "{}", result.text);
    assert!(
        typing.2.contains("(SID DEV, client 100), backend rfc"),
        "{}",
        typing.2
    );
    assert!(!typing.2.contains("unknown"), "{}", typing.2);
    assert_eq!(adt.borrow().len(), 1);
    assert!(rfc.borrow().is_empty());
    assert!(
        run.borrow()
            .iter()
            .any(|call| call.uri == "/sap/bc/adt/abapunit/testruns")
    );
    assert!(
        result.text.contains("TESTS: 2 methods, 0 alerts"),
        "{}",
        result.text
    );
    // The pin is the confirmation, not a flag of the caller.
    assert!(
        result
            .text
            .contains("backend: rfc\nreason: backend of the confirmation"),
        "{}",
        result.text
    );
    assert!(
        !result.text.contains("explicit --backend"),
        "{}",
        result.text
    );
}

#[test]
fn a_form_whose_backend_cannot_be_known_leaves_the_phrase_out() {
    use abapreach::mcp_write::{Operation, Ticket, confirmation_summary};

    let ticket = Ticket::new(
        Operation::Aunit,
        "DEV_100",
        BTreeMap::from([("objects".to_owned(), "CLAS:ZCL_TEST".to_owned())]),
    );
    let summary = confirmation_summary(&Connection::test_insecure(), &ticket);
    let first = summary.lines().next().unwrap();

    assert!(first.starts_with("RUN ABAP UNIT"), "{summary}");
    assert!(first.ends_with(')'), "{summary}");
    assert!(!first.contains("backend"), "{summary}");
    assert!(!summary.contains("unknown"), "{summary}");
}

// ---- the policy keys -----------------------------------------------------------------------------

#[test]
fn the_run_policies_default_to_a_closed_mcp_and_the_plan_flow() {
    use abapreach::policy::{Policies, RunPolicy};

    let defaults = Policies::default();

    assert_eq!(defaults.mcp_run, McpRun::Off);
    assert_eq!(defaults.run, RunPolicy::Plan);
    assert_eq!(
        (McpRun::Confirm.token(), RunPolicy::Direct.token()),
        ("confirm", "direct")
    );
}

#[test]
fn a_server_lists_as_runnable_only_with_mcp_run_confirm_and_the_start_snapshot_keeps_it() {
    let config = |policies: &str| {
        let rig = rig(policies);

        Config::load(ConfigPaths::from_env(&rig.env).unwrap(), &rig.env).unwrap()
    };

    assert!(config("{}").mcp_runnable().is_empty());
    assert!(config(r#"{"mcp_run":"off"}"#).mcp_runnable().is_empty());
    assert_eq!(
        config(r#"{"mcp_run":"confirm"}"#).mcp_runnable(),
        ["DEV_100"]
    );
    assert!(
        config(r#"{"mcp_run":"confirm"}"#)
            .mcp_policies()
            .run_offered()
    );
    assert!(!config("{}").mcp_policies().run_offered());
    // A server that was not in the file at start never gets runs.
    assert_eq!(
        config(r#"{"mcp_run":"confirm"}"#)
            .mcp_policies()
            .absent()
            .mcp_run,
        McpRun::Off
    );
}
