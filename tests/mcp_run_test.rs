//! The MCP tool `abap_run` against a fake client and a scripted SAP: the policy matrix (`run`,
//! `mcp_run`, `mcp_run_allow`, `mcp_run_snippet`), the human confirmation that `mcp_write` cannot
//! waive, the facts the run is bound to, the secret check of the output, and the slot. No SAP.

mod common;

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{Answer, Form, Server, Session, ToolHandler, ToolResult, holds_slot, serve};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{McpTools, TokenGate};
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, Opened, RfcLibraryInfo};
use common::preview;
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::rc::Rc;
use std::time::Duration;

const PASSWORD: &str = "secret-pw";
const SOURCE: &str = "CLASS zcl_a DEFINITION PUBLIC.\n  PUBLIC SECTION.\n    INTERFACES if_oo_adt_classrun.\nENDCLASS.\n";
const CLASS_XML: &str = r#"<class:abapClass xmlns:class="http://www.sap.com/adt/oo/classes" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:responsible="DEVUSER" adtcore:version="active" adtcore:name="ZCL_A"><adtcore:packageRef adtcore:name="ZPKG"/></class:abapClass>"#;

type Reply = Result<Response, TransportError>;
type Log = Rc<RefCell<Vec<(usize, Request, Option<Duration>)>>>;

struct Session1 {
    number: usize,
    replies: VecDeque<Reply>,
    log: Log,
}

impl SadtTransport for Session1 {
    fn request(&mut self, request: Request) -> Reply {
        self.log.borrow_mut().push((self.number, request, None));

        self.replies.pop_front().expect("an unscripted request")
    }

    fn request_within(&mut self, request: Request, limit: Duration) -> Reply {
        self.log
            .borrow_mut()
            .push((self.number, request, Some(limit)));

        self.replies.pop_front().expect("an unscripted request")
    }

    fn supports_request_timeout(&self) -> bool {
        true
    }

    fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[derive(Default)]
struct Sap {
    adt: VecDeque<Vec<Reply>>,
    log: Log,
    opened: usize,
}

impl Factories for Sap {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.opened += 1;

        let replies = self.adt.pop_front().expect("an ADT session not scripted");

        Ok(Box::new(Session1 {
            number: self.opened,
            replies: replies.into(),
            log: self.log.clone(),
        }))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        panic!("no RFC in this test")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("no probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("no ping")
    }
}

/// A client that answers every form with `typed` and keeps the forms it saw.
struct Human {
    typed: Option<String>,
    can_elicit: bool,
    forms: Vec<Form>,
    cancel: CancelFlag,
}

impl Human {
    fn typing(text: &str) -> Self {
        Self {
            typed: Some(text.to_owned()),
            can_elicit: true,
            forms: Vec::new(),
            cancel: CancelFlag::new(),
        }
    }

    fn declining() -> Self {
        Self {
            typed: None,
            ..Self::typing("")
        }
    }

    fn blind() -> Self {
        Self {
            can_elicit: false,
            ..Self::typing("ZCL_A")
        }
    }
}

impl Session for Human {
    fn can_elicit(&self) -> bool {
        self.can_elicit
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }

    fn elicit(
        &mut self,
        form: &Form,
        _: Duration,
        _: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
    ) -> Answer {
        self.forms.push(form.clone());

        match &self.typed {
            Some(text) => Answer::Accepted(text.clone()),
            None => Answer::Declined,
        }
    }

    fn log(&mut self, _: &str) {}
}

fn ok(body: &str) -> Reply {
    Ok(Response::text(200, body))
}

fn reads() -> Vec<Reply> {
    vec![
        ok(&preview(&[("CCCATEGORY", &[Some("C")])])),
        ok(CLASS_XML),
        ok(SOURCE),
        ok(&preview(&[("REFCLSNAME", &[Some("IF_OO_ADT_CLASSRUN")])])),
    ]
}

struct Rig {
    sap: Sap,
    env: BTreeMap<String, String>,
    _directory: tempfile::TempDir,
}

fn rig(policies: &str) -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");

    fs::write(
        &path,
        format!(
            r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"policies":{policies}}}]}}"#
        ),
    )
    .unwrap();

    Rig {
        sap: Sap::default(),
        env: BTreeMap::from([
            ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
            ("DEV_100_USER".to_owned(), "user".to_owned()),
            ("DEV_100_PASS".to_owned(), PASSWORD.to_owned()),
        ]),
        _directory: directory,
    }
}

const OPEN: &str = r#"{"mcp_run":"confirm","mcp_run_allow":["ZCL_A"]}"#;

impl Rig {
    fn config(&self) -> Config {
        Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap()
    }

    fn call(&mut self, session: &mut dyn Session, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.sap),
            TokenGate::new(),
        );

        tools.call_in("abap_run", arguments.as_object().unwrap(), session)
    }

    fn posts(&self) -> Vec<(usize, Request, Option<Duration>)> {
        self.sap
            .log
            .borrow()
            .iter()
            .filter(|(_, request, _)| request.uri.contains("/oo/classrun/"))
            .cloned()
            .collect()
    }
}

fn args() -> Value {
    json!({"conn": "DEV_100", "type": "CLAS", "name": "zcl_a"})
}

#[test]
fn the_tool_is_offered_only_with_a_connection_that_opted_in_and_names_it() {
    let rig = rig(OPEN);

    assert_eq!(rig.config().mcp_runnable(), ["DEV_100"]);
    assert!(rig.config().mcp_policies().run_offered());

    let names = |server: &mut Server<abapreach::mcp::NoTools>| {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let out = Capture::default();
        let mut sink = Vec::new();

        serve(
            server,
            std::io::Cursor::new(format!("{request}\n").into_bytes()),
            out.clone(),
            &mut sink,
        )
        .unwrap();

        let frame: Value = serde_json::from_slice(&out.0.lock().unwrap()).unwrap();

        frame["result"]["tools"].as_array().unwrap().to_vec()
    };
    let without = names(&mut Server::new(abapreach::mcp::NoTools, false));
    let with = names(
        &mut Server::new(abapreach::mcp::NoTools, false).with_runnable(rig.config().mcp_runnable()),
    );

    assert!(without.iter().all(|tool| tool["name"] != "abap_run"));

    let run = with.iter().find(|tool| tool["name"] == "abap_run").unwrap();

    assert!(run["description"].as_str().unwrap().contains("DEV_100"));
    assert!(
        run["description"]
            .as_str()
            .unwrap()
            .contains("NOT a dry run")
    );
    assert_eq!(run["annotations"]["readOnlyHint"], false);
    assert_eq!(run["annotations"]["destructiveHint"], true);
    assert_eq!(run["inputSchema"]["required"], json!(["conn", "type"]));
    // Reads and writes keep their own switches.
    assert_eq!(with.len(), without.len() + 1);
    assert!(
        run["inputSchema"]["properties"]
            .get("alv_columns")
            .is_none()
    );
    assert!(abapreach::mcp::MAX_TOOL_NAME >= "abap_run".len());
}

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

#[test]
fn a_run_takes_the_single_in_flight_slot() {
    assert!(holds_slot("abap_run", None));
    assert!(holds_slot("abap_run", Some(&Map::new())));
    assert!(!holds_slot("abap_read_table", None));
}

#[test]
fn the_policy_matrix_refuses_before_any_sap_activity() {
    let cases = [
        // The default: mcp_run is off.
        ("{}", args(), "policy mcp_run"),
        (
            r#"{"mcp_run":"off","mcp_run_allow":["ZCL_A"]}"#,
            args(),
            "policy mcp_run",
        ),
        // run: off wins over mcp_run: confirm.
        (
            r#"{"run":"off","mcp_run":"confirm","mcp_run_allow":["ZCL_A"]}"#,
            args(),
            "policy run",
        ),
        // The name is not on the list: empty list, other name, prefix of an entry.
        (r#"{"mcp_run":"confirm"}"#, args(), "not on mcp_run_allow"),
        (
            OPEN,
            json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_B"}),
            "not on mcp_run_allow",
        ),
        (
            OPEN,
            json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A2"}),
            "not on mcp_run_allow",
        ),
        // A snippet needs its own opt-in, which the allow-list does not replace.
        (
            OPEN,
            json!({"conn": "DEV_100", "type": "SNIPPET", "source": "WRITE 1."}),
            "policy mcp_run_snippet",
        ),
    ];

    for (policies, arguments, reason) in cases {
        let mut rig = rig(policies);
        let mut human = Human::typing("ZCL_A");
        let result = rig.call(&mut human, arguments);

        assert!(result.is_error, "{policies}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
        assert!(result.text.contains(reason), "{}: {}", reason, result.text);
        assert_eq!(rig.sap.opened, 0, "{policies}");
        assert!(human.forms.is_empty());
    }
}

#[test]
fn report_and_snippet_stop_as_not_implemented_after_their_gates() {
    let policies = r#"{"mcp_run":"confirm","mcp_run_allow":["ZREPORT"],"mcp_run_snippet":"on"}"#;

    for arguments in [
        json!({"conn": "DEV_100", "type": "PROG", "name": "zreport", "params": ["P_A=1"]}),
        json!({"conn": "DEV_100", "type": "SNIPPET", "source": "WRITE 1."}),
    ] {
        let mut rig = rig(policies);
        let result = rig.call(&mut Human::typing("x"), arguments);

        assert!(result.is_error);
        assert!(
            result.text.ends_with("is not implemented yet"),
            "{}",
            result.text
        );
        assert_eq!(rig.sap.opened, 0);
    }
}

#[test]
fn a_bad_argument_is_a_refusal_not_a_run() {
    for arguments in [
        json!({"conn": "DEV_100", "type": "BOGUS", "name": "ZCL_A"}),
        json!({"conn": "DEV_100", "type": "CLAS"}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "params": ["A=1"]}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "alv": "off"}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "timeout": 0}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "timeout": 3601}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "timeout": "5"}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "source": "WRITE 1."}),
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "simulate": "yes"}),
    ] {
        let mut rig = rig(OPEN);
        let result = rig.call(&mut Human::typing("ZCL_A"), arguments.clone());

        assert!(result.is_error, "{arguments}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
        assert_eq!(rig.sap.opened, 0, "{arguments}");
    }
}

#[test]
fn a_client_that_cannot_ask_gets_no_run_but_a_simulate_works() {
    let mut rig = rig(OPEN);
    let result = rig.call(&mut Human::blind(), args());

    assert!(result.is_error);
    assert!(
        result.text.contains("always needs a human confirmation"),
        "{}",
        result.text
    );
    assert_eq!(rig.sap.opened, 0);

    rig.sap.adt.push_back(reads());

    let mut simulate = args();

    simulate["simulate"] = json!(true);

    let result = rig.call(&mut Human::blind(), simulate);

    assert!(!result.is_error, "{}", result.text);
    assert!(
        result
            .text
            .starts_with("SIMULATED_OK run CLAS ZCL_A package ZPKG author DEVUSER")
    );
    assert!(rig.posts().is_empty());
}

#[test]
fn the_human_confirms_what_runs_and_only_then_the_class_posts_once() {
    let mut rig = rig(OPEN);

    rig.sap.adt.push_back(reads());
    rig.sap.adt.push_back(reads());
    rig.sap.adt.push_back(vec![ok("Hello from ZCL_A\n")]);

    let mut human = Human::typing("ZCL_A");
    let result = rig.call(&mut human, args());

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.text, "Hello from ZCL_A");
    assert_eq!(result.structured["outcome"], "REPLY");
    assert_eq!(result.structured["exit"], 0);
    assert_eq!(result.structured["output"], "Hello from ZCL_A");

    // The form named everything that runs, in full, and asked for the name.
    assert_eq!(human.forms.len(), 1);

    let message = &human.forms[0].message;

    assert!(
        message.starts_with("RUN CLAS on DEV_100 (SID DEV, client 100), backend adt"),
        "{message}"
    );
    assert!(message.contains("not a dry run"));
    assert!(message.contains("name: ZCL_A"));
    assert!(message.contains("package: ZPKG"));
    assert!(message.contains("author: DEVUSER"));
    assert!(message.contains("active source hash: "));
    assert!(message.contains("timeout: 600 s"));
    assert!(message.ends_with("Type ZCL_A to confirm."));

    // One POST, from a session of its own, with the default limit.
    let posts = rig.posts();

    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, 3);
    assert_eq!(posts[0].1.uri, "/sap/bc/adt/oo/classrun/ZCL_A");
    assert_eq!(posts[0].2, Some(Duration::from_secs(600)));
}

#[test]
fn mcp_write_does_not_waive_the_human_and_run_direct_counts_as_plan() {
    for policies in [
        r#"{"mcp_write":"token","run":"direct","mcp_run":"confirm","mcp_run_allow":["ZCL_A"]}"#,
        r#"{"mcp_write":"confirm","mcp_run":"confirm","mcp_run_allow":["zcl_a"]}"#,
    ] {
        let mut rig = rig(policies);

        rig.sap.adt.push_back(reads());

        let mut human = Human::declining();
        let result = rig.call(&mut human, args());

        assert!(result.is_error);
        assert!(
            result.text.contains("run not confirmed: declined"),
            "{}",
            result.text
        );
        assert_eq!(human.forms.len(), 1);
        assert!(rig.posts().is_empty());
    }
}

#[test]
fn a_wrong_answer_or_a_cancel_runs_nothing() {
    let mut rig = rig(OPEN);

    rig.sap.adt.push_back(reads());

    let result = rig.call(&mut Human::typing("zcl_b"), args());

    assert!(result.is_error);
    assert!(
        result
            .text
            .contains("run not confirmed: the typed text did not match")
    );
    assert!(rig.posts().is_empty());

    rig.sap.adt.push_back(reads());

    let mut human = Human::typing("ZCL_A");

    human.cancel.set();

    let result = rig.call(&mut human, args());

    assert!(result.is_error);
    assert!(result.text.starts_with("CANCELED: "), "{}", result.text);
    assert!(rig.posts().is_empty());
}

#[test]
fn a_run_is_bound_to_the_facts_the_human_saw() {
    let mut rig = rig(OPEN);
    let changed = format!("{SOURCE}* changed after the form\n");

    rig.sap.adt.push_back(reads());
    // The second read sees another source.
    rig.sap.adt.push_back(vec![
        ok(&preview(&[("CCCATEGORY", &[Some("C")])])),
        ok(CLASS_XML),
        ok(&changed),
        ok(&preview(&[("REFCLSNAME", &[Some("IF_OO_ADT_CLASSRUN")])])),
    ]);

    let result = rig.call(&mut Human::typing("ZCL_A"), args());

    assert!(result.is_error);
    assert!(
        result
            .text
            .contains("REJECTED: plan changed since simulate (source hash)"),
        "{}",
        result.text
    );
    assert!(rig.posts().is_empty());
}

#[test]
fn a_productive_client_refuses_the_run_before_any_form() {
    let mut rig = rig(OPEN);

    rig.sap
        .adt
        .push_back(vec![ok(&preview(&[("CCCATEGORY", &[Some("P")])]))]);

    let mut human = Human::typing("ZCL_A");
    let result = rig.call(&mut human, args());

    assert!(result.is_error);
    assert!(result.text.contains("productive"), "{}", result.text);
    assert!(human.forms.is_empty());
    assert!(rig.posts().is_empty());
}

#[test]
fn the_timeout_argument_is_the_limit_of_the_post() {
    let mut rig = rig(OPEN);

    rig.sap.adt.push_back(reads());
    rig.sap.adt.push_back(reads());
    rig.sap.adt.push_back(vec![ok("x")]);

    let mut arguments = args();

    arguments["timeout"] = json!(45);

    let mut human = Human::typing("ZCL_A");
    let result = rig.call(&mut human, arguments);

    assert!(!result.is_error, "{}", result.text);
    assert!(human.forms[0].message.contains("timeout: 45 s"));
    assert_eq!(rig.posts()[0].2, Some(Duration::from_secs(45)));
}

#[test]
fn a_failed_run_is_an_error_with_its_token() {
    for (reply, token, text) in [
        (
            Ok(Response::text(500, "<html>ABAP runtime error</html>")),
            "RUN_FAILED",
            "RUN_FAILED: runtime error; see ST22 (dumps)",
        ),
        (
            Ok(Response::text(504, "")),
            "RUN_UNKNOWN",
            "RUN_UNKNOWN: the reply was lost; the code may have run",
        ),
    ] {
        let mut rig = rig(OPEN);

        rig.sap.adt.push_back(reads());
        rig.sap.adt.push_back(reads());
        rig.sap.adt.push_back(vec![reply]);

        let result = rig.call(&mut Human::typing("ZCL_A"), args());

        assert!(result.is_error);
        assert!(result.text.contains(text), "{}", result.text);
        assert_eq!(result.structured["outcome"], token);
        assert_eq!(result.structured["exit"], 1);
        assert_eq!(rig.posts().len(), 1);
    }
}

#[test]
fn output_that_holds_a_configured_secret_is_withheld_but_the_run_is_reported() {
    let mut rig = rig(OPEN);

    rig.sap.adt.push_back(reads());
    rig.sap.adt.push_back(reads());
    rig.sap
        .adt
        .push_back(vec![ok(&format!("the password is {PASSWORD}"))]);

    let result = rig.call(&mut Human::typing("ZCL_A"), args());

    assert!(result.is_error);
    assert!(!result.text.contains(PASSWORD), "{}", result.text);
    assert!(result.text.contains("payload contains a configured secret"));
    assert!(result.text.contains("the run itself has happened"));
    assert!(!result.structured.to_string().contains(PASSWORD));
}

#[test]
fn the_output_of_one_call_is_cut_at_one_mebibyte() {
    let mut rig = rig(OPEN);
    let big = "ä".repeat(600_000);

    rig.sap.adt.push_back(reads());
    rig.sap.adt.push_back(reads());
    rig.sap.adt.push_back(vec![ok(&big)]);

    let result = rig.call(&mut Human::typing("ZCL_A"), args());

    assert!(!result.is_error);
    assert!(
        result
            .text
            .contains("W: output cut at 1048576 bytes; 151424 bytes left out"),
        "{}",
        &result.text[result.text.len() - 80..]
    );
    assert!(result.structured["output"].as_str().unwrap().len() <= 1 << 20);
}
