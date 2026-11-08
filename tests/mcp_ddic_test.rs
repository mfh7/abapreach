//! The MCP tools for `DTEL`, `DOMA`, `TABL`, `TTYP`, and `TRAN` (M6 Task 26): schema, rejected
//! arguments, the plan token bindings, `mcp_write` (`off`, `confirm`, `token`) for writes and for
//! reads, the confirmation from run 1 of a deploy, the always-asked delete, the single in-flight
//! slot, the scrub, and the leftover class in the data of the call. A fake client and the fake SAP
//! of the runner tests answer; no SAP.

mod common;
mod mcprig;

use abapreach::config::Connection;
use abapreach::hash::{normalize, source_hash};
use abapreach::mcp::{Server, ToolHandler, ToolResult, serve_with, tool_list_for};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::McpTools;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use common::runner_fake::{Opened, Shared, Tick, World, http_session, new_sap, rfc_session};
use mcprig::{FakeSession, HOST, PASSWORD, Rig, rig, token_of};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TOKEN: &str = r#"{"mcp_write":"token"}"#;
const CONFIRM: &str = r#"{"mcp_write":"confirm"}"#;
const OFF: &str = r#"{"mcp_write":"off"}"#;
const LIVE: &str = include_str!("fixtures/m6/read_dtel.xml");
const PREFIX: &str = "YCL_ABAPREACH_RUN_";
const PAYLOAD_PREFIX: &str = "    lv_b64 = lv_b64 && '";
const TCODE: &str = "ZTRAN";
const REPORT: &str = "ZREPORT";
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// ---- the runner output ------------------------------------------------------------------------

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

fn run_output(nonce: &str, mode: &str, body: &[String], status: &str) -> String {
    let mut lines = vec![format!("BEGIN:{mode}")];

    lines.extend(body.iter().cloned());
    lines.push(format!("STATUS:{status}"));

    let count = lines.len();

    lines.push(format!("END:{count}"));

    lines
        .iter()
        .map(|rest| format!("ABAPREACH:{nonce}:{rest}\n\n"))
        .collect()
}

fn row(text: &str) -> String {
    format!("LINE:{}", hex(text))
}

fn fact(key: &str, text: &str) -> String {
    format!("FACT:{key}:{}", hex(text))
}

fn info(text: &str) -> String {
    format!("MSG:I:{}", hex(text))
}

fn error(text: &str) -> String {
    format!("MSG:E:{}", hex(text))
}

fn rows_of(xml: &str) -> Vec<String> {
    xml.trim_end_matches('\n')
        .split('\n')
        .map(str::to_owned)
        .collect()
}

fn changed() -> String {
    LIVE.replace("Test data element", "Changed data element")
}

fn file_hash(text: &str) -> String {
    source_hash(&normalize(rows_of(text)))
}

// ---- the payload the class carries --------------------------------------------------------------

fn base64_decode(text: &str) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0);

    for character in text.bytes().filter(|byte| *byte != b'=') {
        buffer = (buffer << 6)
            | alphabet
                .iter()
                .position(|known| *known == character)
                .unwrap() as u32;
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }

    String::from_utf8(bytes).unwrap()
}

fn payload_of(source: &str) -> Vec<(String, String)> {
    let encoded: String = source
        .lines()
        .filter_map(|line| line.strip_prefix(PAYLOAD_PREFIX)?.strip_suffix("'."))
        .collect();

    base64_decode(&encoded)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn value(payload: &[(String, String)], key: &str) -> String {
    payload
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

/// What the runner class answers to the payload it carries.
fn answer(source: &str, secret: &Rc<RefCell<Option<String>>>) -> String {
    let payload = payload_of(source);
    let nonce = value(&payload, "NONCE");
    let tran = value(&payload, "TYPE") == "TRAN";

    match value(&payload, "MODE").as_str() {
        "READ" if tran => {
            let lines = [
                format!("TRAN {TCODE}"),
                format!("program={REPORT}"),
                "dynpro=1000".to_owned(),
                "cinfo=80".to_owned(),
                "text=Demo transaction".to_owned(),
                "package=$TMP".to_owned(),
            ];
            let body: Vec<String> = lines.iter().map(|text| row(text)).collect();

            run_output(&nonce, "READ", &body, "OK")
        }
        "READ" => {
            let mut body: Vec<String> = rows_of(LIVE).iter().map(|text| row(text)).collect();

            body.push(fact("HASH", &file_hash(LIVE)));

            run_output(&nonce, "READ", &body, "OK")
        }
        "SIMULATE" => {
            let mut rows = rows_of(LIVE);

            rows[2].push_str("  ");
            rows.push(String::new());

            let mut body = vec![fact("HASH", &file_hash(LIVE))];

            body.extend(rows.iter().map(|text| row(text)));
            body.push(info(
                "CONVERSION ZDTEL: action=ALT modeflag=F daction=ALT allowed=X rows=0",
            ));

            run_output(&nonce, "SIMULATE", &body, "SIMULATED_OK")
        }
        "DEPLOY" => {
            let body = vec![
                fact("HASH", &file_hash(LIVE)),
                fact("PACKAGE", "$TMP"),
                fact("RESULT_HASH", &file_hash(&changed())),
            ];

            run_output(&nonce, "DEPLOY", &body, "ACTIVE")
        }
        "CREATE_SIM" => match secret.borrow().as_deref() {
            Some(text) => run_output(&nonce, "CREATE_SIM", &[error(text)], "REJECTED"),
            None => run_output(&nonce, "CREATE_SIM", &[], "SIMULATED_OK"),
        },
        "CREATE" if tran => run_output(&nonce, "CREATE", &[], "ACTIVE"),
        "CREATE" => run_output(
            &nonce,
            "CREATE",
            &[fact("RESULT_HASH", &file_hash(LIVE))],
            "ACTIVE",
        ),
        "DELETE_SIM" => {
            let mut body = vec![
                fact("PACKAGE", "$TMP"),
                fact("AUTHOR", "DEVUSER"),
                fact("REQUEST", "-"),
            ];

            if tran {
                body.push(fact("PROGRAM", REPORT));
                body.push(fact("CINFO", "80"));
            }

            body.push(fact("HASH", HASH));

            run_output(&nonce, "DELETE_SIM", &body, "SIMULATED_OK")
        }
        "DELETE" => run_output(&nonce, "DELETE", &[], "DELETED"),
        other => panic!("unexpected mode {other}"),
    }
}

// ---- the fake system and the client --------------------------------------------------------------

/// Answers the package of a TADIR row as the test says; the fake SAP answers `$TMP`.
struct Retagged {
    inner: Box<dyn SadtTransport>,
    package: &'static str,
}

impl SadtTransport for Retagged {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        let tadir = request.uri.starts_with("/sap/bc/adt/datapreview/ddic")
            && String::from_utf8_lossy(&request.body).contains(" FROM TADIR");
        let mut response = self.inner.request(request)?;

        if tadir {
            let text = String::from_utf8_lossy(&response.body).replace("$TMP", self.package);

            response.body = text.into_bytes();
        }

        Ok(response)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.inner.stateful_supported()
    }
}

/// The sessions of the fake SAP, or an ADT route that cannot connect.
struct Sessions {
    sap: Shared,
    adt_down: bool,
    adt_opened: Rc<Cell<usize>>,
    rfc_opened: Rc<Cell<usize>>,
    /// The package of every TADIR row.
    package: &'static str,
}

impl Sessions {
    fn retag(&self, opened: Opened) -> Opened {
        Ok(Box::new(Retagged {
            inner: opened?,
            package: self.package,
        }))
    }
}

impl Factories for Sessions {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened.set(self.adt_opened.get() + 1);

        if self.adt_down {
            return Err(TransportError::PreSendConnectivity("no route".to_owned()));
        }

        self.retag(http_session(&self.sap))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened.set(self.rfc_opened.get() + 1);

        self.retag(rfc_session(&self.sap))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        Err("no library in a test".to_owned())
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        Ok(())
    }
}

/// The server's tools over one fake SAP.
struct Lab {
    rig: Rig,
    sessions: Sessions,
    /// A text the fake class answers a create simulate with as a rejection.
    secret: Rc<RefCell<Option<String>>>,
}

type Evolution = Box<dyn Fn(&Tick) -> World>;

const PRESENT: World = World {
    definition: true,
    directory: true,
    inactive: false,
    broken: false,
};

fn present() -> Evolution {
    Box::new(|_| PRESENT)
}

/// Absent until the second classrun POST (the create), present afterwards.
fn created_by_the_second_post() -> Evolution {
    Box::new(|tick| {
        if tick.posts >= 2 {
            PRESENT
        } else {
            World::default()
        }
    })
}

/// Present until the second classrun POST (the delete), gone afterwards.
fn deleted_by_the_second_post() -> Evolution {
    Box::new(|tick| {
        if tick.posts >= 2 {
            World::default()
        } else {
            PRESENT
        }
    })
}

fn lab(policies: &str, evolution: Evolution) -> Lab {
    let secret = Rc::new(RefCell::new(None));
    let sap = new_sap(true, &Rc::new(Cell::new(Duration::ZERO)), evolution);
    let reply = secret.clone();

    sap.borrow_mut().reply_with = Some(Box::new(move |source| answer(source, &reply)));

    Lab {
        rig: rig(policies),
        sessions: Sessions {
            sap,
            adt_down: false,
            adt_opened: Rc::default(),
            rfc_opened: Rc::default(),
            package: "$TMP",
        },
        secret,
    }
}

impl Lab {
    fn call(&mut self, session: &mut FakeSession, name: &str, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.rig.config(), self.rig.env.clone(), &mut self.sessions),
            self.rig.gate.clone(),
        );

        tools.call_in(name, arguments.as_object().unwrap(), session)
    }

    /// A call by a client that cannot be asked.
    fn plain(&mut self, name: &str, arguments: Value) -> ToolResult {
        self.call(&mut FakeSession::new(false), name, arguments)
    }

    fn posts(&self) -> usize {
        self.sessions.sap.borrow().posts
    }

    fn classes(&self) -> usize {
        self.sessions.sap.borrow().classes.len()
    }

    fn opened(&self) -> (usize, usize) {
        (
            self.sessions.adt_opened.get(),
            self.sessions.rfc_opened.get(),
        )
    }
}

fn dtel(source: &str) -> Value {
    json!({"conn": "DEV_100", "type": "DTEL", "name": "ZDTEL", "source": source})
}

fn with_token(mut arguments: Value, token: &str) -> Value {
    arguments["plan_token"] = json!(token);

    arguments
}

fn tran_create() -> Value {
    json!({"conn": "DEV_100", "type": "TRAN", "name": TCODE, "prog": REPORT,
           "text": "Demo transaction", "package": "ZPKG", "tr": "DEVK900001"})
}

fn dtel_create() -> Value {
    json!({"conn": "DEV_100", "type": "DTEL", "name": "ZDTEL", "source": LIVE})
}

fn delete(objects: &[&str]) -> Value {
    json!({"conn": "DEV_100", "objects": objects})
}

#[track_caller]
fn assert_ok(result: &ToolResult) {
    assert!(!result.is_error, "{}", result.text);
}

#[track_caller]
fn assert_error(result: &ToolResult, fragment: &str) {
    assert!(result.is_error, "{}", result.text);
    assert!(
        result.text.contains(fragment),
        "{fragment} in {}",
        result.text
    );
}

// ---- schema ---------------------------------------------------------------------------------------

#[test]
fn the_schemas_name_the_new_types_the_arguments_and_the_footprint() {
    let tools = tool_list_for(&["DEV_100".to_owned()]);
    let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap();
    let types = |name: &str| find(name)["inputSchema"]["properties"]["type"]["enum"].clone();
    let source = json!([
        "PROG", "FUNC", "CLAS", "INTF", "DTEL", "DOMA", "TABL", "TTYP", "TRAN"
    ]);
    let create = json!([
        "PROG", "FUGR", "FUNC", "CLAS", "INTF", "DTEL", "DOMA", "TABL", "TTYP", "TRAN"
    ]);

    for name in ["abap_read_source", "abap_simulate", "abap_deploy"] {
        assert_eq!(types(name), source, "{name}");
    }

    for name in ["abap_create_simulate", "abap_create"] {
        let properties = &find(name)["inputSchema"]["properties"];

        assert_eq!(types(name), create, "{name}");

        for key in ["prog", "text", "source"] {
            assert_eq!(properties[key]["type"], "string", "{name} {key}");
        }

        // `text` is optional for a DDIC type, so the schema cannot require it.
        let required = find(name)["inputSchema"]["required"].as_array().unwrap();

        assert!(!required.iter().any(|field| field == "text"), "{name}");
    }

    for name in ["abap_delete_simulate", "abap_delete"] {
        let description = find(name)["inputSchema"]["properties"]["objects"]["description"]
            .as_str()
            .unwrap();

        assert!(
            description.ends_with("PROG|FUGR|FUNC|CLAS|INTF|DTEL|DOMA|TABL|TTYP|TRAN"),
            "{name}: {description}"
        );
    }

    // Reads and simulations disclose the temporary class; only the read hint of the source read
    // changed.
    for name in [
        "abap_read_source",
        "abap_simulate",
        "abap_create_simulate",
        "abap_delete_simulate",
    ] {
        assert!(
            find(name)["description"]
                .as_str()
                .unwrap()
                .contains("temporary class in $TMP"),
            "{name}"
        );
    }

    for tool in &tools {
        let name = tool["name"].as_str().unwrap();

        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            name == "abap_read_table",
            "{name}"
        );
        assert_eq!(
            tool["annotations"]["destructiveHint"],
            name != "abap_read_source" && name != "abap_read_table",
            "{name}"
        );
    }
}

// ---- rejected arguments --------------------------------------------------------------------------

#[test]
fn arguments_a_new_type_does_not_take_are_rejected_before_anything_is_sent() {
    let cases = [
        (
            "abap_read_source",
            json!({"type": "DTEL", "lines": "1-2"}),
            "lines does not apply to DTEL",
        ),
        (
            "abap_read_source",
            json!({"type": "TRAN", "include": "CCIMP"}),
            "include does not apply to TRAN",
        ),
        (
            "abap_read_source",
            json!({"type": "DOMA", "fugr": "ZFG"}),
            "fugr does not apply to DOMA",
        ),
        (
            "abap_read_source",
            json!({"type": "TTYP", "main": "ZMAIN"}),
            "main does not apply to TTYP",
        ),
        (
            "abap_simulate",
            json!({"type": "DTEL", "source": LIVE, "include": "CCIMP"}),
            "include does not apply to DTEL",
        ),
        (
            "abap_simulate",
            json!({"type": "TABL", "source": LIVE, "create_include": true}),
            "create_include does not apply to TABL",
        ),
        (
            "abap_simulate",
            json!({"type": "DTEL", "source": LIVE, "fugr": "ZFG"}),
            "fugr does not apply to DTEL",
        ),
        (
            "abap_simulate",
            json!({"type": "DTEL", "source": LIVE, "main": "ZM"}),
            "main does not apply to DTEL",
        ),
        (
            "abap_deploy",
            json!({"type": "TRAN", "source": "x", "plan_token": "t"}),
            "TRAN has no deploy; use create or delete",
        ),
        (
            "abap_simulate",
            json!({"type": "TRAN", "source": "x"}),
            "TRAN has no deploy; use create or delete",
        ),
        (
            "abap_create_simulate",
            json!({"type": "DTEL", "source": LIVE, "subc": "I"}),
            "--subc only applies to PROG",
        ),
        (
            "abap_create_simulate",
            json!({"type": "DTEL", "source": LIVE, "fugr": "ZFG"}),
            "--fugr is only valid for FUNC",
        ),
        (
            "abap_create_simulate",
            json!({"type": "DTEL", "source": LIVE, "prog": REPORT}),
            "prog only applies to TRAN",
        ),
        (
            "abap_create_simulate",
            json!({"type": "DTEL"}),
            "source (the XML) is required for DTEL",
        ),
        (
            "abap_create_simulate",
            json!({"type": "TRAN", "text": "t"}),
            "prog is required for TRAN",
        ),
        (
            "abap_create_simulate",
            json!({"type": "TRAN", "prog": REPORT}),
            "text is required for TRAN",
        ),
        (
            "abap_create_simulate",
            json!({"type": "TRAN", "prog": REPORT, "text": "t", "source": LIVE}),
            "source does not apply to TRAN",
        ),
        (
            "abap_create_simulate",
            json!({"type": "TRAN", "prog": REPORT, "text": "t", "subc": "I"}),
            "--subc only applies to PROG",
        ),
        (
            "abap_delete_simulate",
            json!({"objects": ["DTEL:ADTEL"]}),
            "write and delete accept only Z and Y names: ADTEL",
        ),
        (
            "abap_delete_simulate",
            json!({"objects": ["DTEL:ZA", "DOMA:ZB"]}),
            "at most 1 DTEL, DOMA, TABL, TTYP or TRAN object",
        ),
    ];

    for (tool, mut arguments, fragment) in cases {
        let mut lab = lab(TOKEN, present());

        arguments["conn"] = json!("DEV_100");

        if !arguments.as_object().unwrap().contains_key("name") && tool != "abap_delete_simulate" {
            arguments["name"] = json!("ZDTEL");
        }

        let result = lab.plain(tool, arguments.clone());

        assert_error(&result, fragment);
        assert_eq!(
            (lab.opened(), lab.posts(), lab.classes()),
            ((0, 0), 0, 0),
            "{tool} {arguments}"
        );
    }
}

#[test]
fn a_file_the_adapter_refuses_stops_a_write_before_the_token_is_looked_at() {
    let mut lab = lab(TOKEN, present());
    let doctype = LIVE.replace("<abapGit", "<!DOCTYPE x [<!ENTITY y \"z\">]><abapGit");
    let other = LIVE.replace("ZDTEL", "ZOTHER");

    for (source, fragment) in [
        (
            doctype.as_str(),
            "XML file must not declare a DTD or entity",
        ),
        (other.as_str(), "XML file is for ZOTHER, not DTEL ZDTEL"),
    ] {
        let result = lab.plain(
            "abap_deploy",
            with_token(dtel(source), "not-a-token-at-all"),
        );

        assert_error(&result, fragment);
        assert!(
            !result.text.contains("plan_token"),
            "the file is refused before the token: {}",
            result.text
        );
    }

    assert_eq!((lab.opened(), lab.posts(), lab.classes()), ((0, 0), 0, 0));
}

// ---- mcp_write and the read ------------------------------------------------------------------------

#[test]
fn a_ddic_or_transaction_read_is_refused_under_off_with_the_reason_and_opens_nothing() {
    for kind in ["DTEL", "DOMA", "TABL", "TTYP", "TRAN"] {
        let mut lab = lab(OFF, present());
        let result = lab.plain(
            "abap_read_source",
            json!({"conn": "DEV_100", "type": kind, "name": "ZOBJ"}),
        );

        assert!(result.is_error, "{kind}");
        assert_eq!(
            result.text,
            format!("STOP: reading {kind} needs a temporary class; mcp_write is off for DEV_100")
        );
        assert_eq!((lab.opened(), lab.posts(), lab.classes()), ((0, 0), 0, 0));
    }
}

#[test]
fn a_read_runs_under_confirm_and_token_without_a_question_or_a_token() {
    for policies in [CONFIRM, TOKEN] {
        let mut lab = lab(policies, present());
        let mut session = FakeSession::new(true);
        let result = lab.call(
            &mut session,
            "abap_read_source",
            json!({"conn": "DEV_100", "type": "DTEL", "name": "zdtel"}),
        );

        assert_ok(&result);
        assert_eq!(session.asked(), 0, "{policies}");
        assert_eq!(result.structured["status"], "ACTIVE");
        assert_eq!(result.structured["partial"], false);
        assert_eq!(result.structured["source"], rows_of(LIVE).join("\n"));
        assert_eq!(result.structured["hash"], file_hash(LIVE));
        assert!(result.text.contains(&format!("hash {}", file_hash(LIVE))));
        assert!(
            result.text.contains("RUNNER: YCL_ABAPREACH_RUN_"),
            "{}",
            result.text
        );
        assert!(
            !result.text.contains("abapreach-read"),
            "no scratch path: {}",
            result.text
        );
        assert_eq!((lab.posts(), lab.classes()), (1, 0));

        let tran = lab.call(
            &mut session,
            "abap_read_source",
            json!({"conn": "DEV_100", "type": "TRAN", "name": TCODE}),
        );

        assert_ok(&tran);
        assert_eq!(tran.structured["lines"], 6);
        assert!(
            tran.structured["source"]
                .as_str()
                .unwrap()
                .starts_with("TRAN ZTRAN\nprogram=ZREPORT\n"),
            "{}",
            tran.text
        );
        assert_eq!((lab.posts(), lab.classes()), (2, 0));
        assert_eq!(session.asked(), 0);
    }
}

#[test]
fn a_write_under_off_is_unavailable_for_the_new_types_too() {
    let mut lab = lab(OFF, present());

    for (tool, arguments) in [
        ("abap_simulate", dtel(LIVE)),
        ("abap_create_simulate", tran_create()),
        ("abap_delete_simulate", delete(&["DTEL:ZDTEL"])),
    ] {
        let result = lab.plain(tool, arguments);

        assert_error(&result, "MCP writes are off for DEV_100");
    }

    assert_eq!((lab.opened(), lab.posts()), ((0, 0), 0));
}

// ---- deploy: tokens, the confirmation from run 1, the file hash ----------------------------------------

#[test]
fn a_ddic_deploy_needs_the_token_of_its_file_and_runs_two_classes() {
    let mut lab = lab(TOKEN, present());
    let file = changed();
    let simulated = lab.plain("abap_simulate", dtel(&file));

    assert_ok(&simulated);
    assert_eq!(simulated.structured["status"], "SIMULATED_OK");
    assert!(
        simulated.text.contains("SIMULATED_OK"),
        "{}",
        simulated.text
    );
    assert!(simulated.text.contains("RUNNER: YCL_ABAPREACH_RUN_"));

    let token = token_of(&simulated);

    assert_eq!((lab.posts(), lab.classes()), (1, 0));

    // Another file hash is not this plan: nothing runs and the token is not burnt.
    let wrong = lab.plain(
        "abap_deploy",
        with_token(dtel(&LIVE.replace("Test data element", "Other")), &token),
    );

    assert_error(&wrong, "does not match");
    assert_eq!(lab.posts(), 1);

    let deployed = lab.plain("abap_deploy", with_token(dtel(&file), &token));

    assert_ok(&deployed);
    assert_eq!(deployed.structured["status"], "ACTIVE");
    // Run 1 and run 2: two temporary classes, both removed.
    assert_eq!((lab.posts(), lab.classes()), (3, 0));

    let again = lab.plain("abap_deploy", with_token(dtel(&file), &token));

    assert_error(&again, "unknown or already used");
    assert_eq!(lab.posts(), 3);
}

#[test]
fn a_deploy_stays_on_the_backend_of_its_plan_and_never_falls_back() {
    let mut lab = lab(TOKEN, present());
    let file = changed();
    let token = token_of(&lab.plain("abap_simulate", dtel(&file)));

    // The simulation ran on ADT; the write finds ADT unreachable and must not move to RFC.
    lab.sessions.adt_down = true;

    let (adt_before, rfc_before) = lab.opened();
    let result = lab.plain("abap_deploy", with_token(dtel(&file), &token));
    let (adt_after, rfc_after) = lab.opened();

    assert!(result.is_error, "{}", result.text);
    assert!(adt_after > adt_before);
    assert_eq!(
        rfc_after, rfc_before,
        "no RFC session for a plan made on ADT"
    );
    assert_eq!(lab.posts(), 1);
}

#[test]
fn under_confirm_the_summary_comes_from_run_1_before_run_2_writes() {
    let mut lab = lab(CONFIRM, present());
    let file = changed();
    let token = token_of(&lab.plain("abap_simulate", dtel(&file)));
    let posts_when_asked = Rc::new(Cell::new(usize::MAX));
    let sap = lab.sessions.sap.clone();
    let seen = posts_when_asked.clone();
    let mut session = FakeSession::typing("ZDTEL");

    session.while_open = Some(Box::new(move || seen.set(sap.borrow().posts)));

    let deployed = lab.call(&mut session, "abap_deploy", with_token(dtel(&file), &token));

    assert_ok(&deployed);
    // The simulation and run 1 have posted; run 2 has not.
    assert_eq!(posts_when_asked.get(), 2);
    assert_eq!(lab.posts(), 3);
    assert_eq!(session.asked(), 1);

    let message = &session.forms[0].0.message;

    for fragment in [
        "DEPLOY on DEV_100 (SID DEV, client 100), backend adt",
        "type: DTEL",
        "name: ZDTEL",
        "runner class: YCL_ABAPREACH_RUN_",
        "changed XML rows: +1 -1",
        "diff excerpt:",
        "--- live",
        "+++ file",
        "+    <DDTEXT>Changed data element</DDTEXT>",
        "CONVERSION ZDTEL: action=ALT",
        "a temporary class is created in $TMP",
        "Type ZDTEL to confirm.",
    ] {
        assert!(message.contains(fragment), "{fragment} in {message}");
    }

    assert!(!message.contains(PASSWORD));
}

#[test]
fn a_declined_ddic_deploy_never_starts_run_2() {
    let mut lab = lab(CONFIRM, present());
    let file = changed();
    let token = token_of(&lab.plain("abap_simulate", dtel(&file)));
    let mut session = FakeSession::new(true);
    let result = lab.call(&mut session, "abap_deploy", with_token(dtel(&file), &token));

    assert_eq!(result.text, "STOP: write not confirmed: declined");
    // The simulation and run 1 only, and no class is left.
    assert_eq!((lab.posts(), lab.classes()), (2, 0));
}

#[test]
fn a_client_that_cannot_answer_burns_the_token_and_runs_nothing_more() {
    let mut lab = lab(CONFIRM, present());
    let file = changed();
    let token = token_of(&lab.plain("abap_simulate", dtel(&file)));
    let result = lab.plain("abap_deploy", with_token(dtel(&file), &token));

    assert_error(
        &result,
        "mcp_write is confirm for DEV_100 and this client cannot answer",
    );
    assert_eq!(lab.posts(), 1);
}

// ---- create: the bindings ----------------------------------------------------------------------------------

#[test]
fn a_create_token_binds_package_prog_text_and_the_file() {
    let mut lab = lab(TOKEN, created_by_the_second_post());

    lab.sessions.package = "ZPKG";

    let token = token_of(&lab.plain("abap_create_simulate", tran_create()));
    let mut changes: Vec<(&str, &str)> = vec![
        ("package", "ZOTHER"),
        ("prog", "ZOTHERREPORT"),
        ("text", "Another text"),
        ("tr", "DEVK900002"),
        ("name", "ZTRAN2"),
    ];

    changes.reverse();

    for (key, other) in changes {
        let mut arguments = tran_create();

        arguments[key] = json!(other);

        let result = lab.plain("abap_create", with_token(arguments, &token));

        assert_error(&result, "does not match");
    }

    // Nothing ran, and the original arguments still redeem the token.
    assert_eq!(lab.posts(), 1);

    let created = lab.plain("abap_create", with_token(tran_create(), &token));

    assert_ok(&created);
    assert_eq!(created.structured["status"], "ACTIVE");
    assert!(created.text.contains("CREATE: ACTIVE"), "{}", created.text);
    assert_eq!((lab.posts(), lab.classes()), (2, 0));

    // A DDIC create binds the hash of the file.
    let mut lab = self::lab(TOKEN, created_by_the_second_post());
    let token = token_of(&lab.plain("abap_create_simulate", dtel_create()));
    let mut other = dtel_create();

    other["source"] = json!(changed());

    assert_error(
        &lab.plain("abap_create", with_token(other, &token)),
        "does not match",
    );
    assert_eq!(lab.posts(), 1);
    assert_ok(&lab.plain("abap_create", with_token(dtel_create(), &token)));
    assert_eq!((lab.posts(), lab.classes()), (2, 0));
}

#[test]
fn a_create_stays_on_the_backend_of_its_plan() {
    let mut lab = lab(TOKEN, created_by_the_second_post());
    let token = token_of(&lab.plain("abap_create_simulate", tran_create()));

    lab.sessions.adt_down = true;

    let (_, rfc_before) = lab.opened();
    let result = lab.plain("abap_create", with_token(tran_create(), &token));

    assert!(result.is_error, "{}", result.text);
    assert_eq!(lab.opened().1, rfc_before, "no fallback to RFC");
    assert_eq!(lab.posts(), 1);
}

#[test]
fn a_transaction_create_under_confirm_asks_with_what_it_will_create() {
    let mut lab = lab(CONFIRM, created_by_the_second_post());

    lab.sessions.package = "ZPKG";

    let token = token_of(&lab.plain("abap_create_simulate", tran_create()));
    let mut session = FakeSession::typing(TCODE);
    let created = lab.call(
        &mut session,
        "abap_create",
        with_token(tran_create(), &token),
    );

    assert_ok(&created);

    let message = &session.forms[0].0.message;

    for fragment in [
        "CREATE on DEV_100 (SID DEV, client 100), backend adt",
        "type: TRAN",
        "prog: ZREPORT",
        "package: ZPKG",
        "tr: DEVK900001",
        "text: Demo transaction",
        "a temporary class is created in $TMP",
    ] {
        assert!(message.contains(fragment), "{fragment} in {message}");
    }
}

// ---- delete ----------------------------------------------------------------------------------------------------

#[test]
fn a_ddic_delete_is_always_asked_even_under_token() {
    let mut lab = lab(TOKEN, deleted_by_the_second_post());
    let simulated = lab.plain("abap_delete_simulate", delete(&["DTEL:zdtel"]));

    assert_ok(&simulated);

    let token = token_of(&simulated);
    let blind = lab.plain("abap_delete", with_token(delete(&["DTEL:zdtel"]), &token));

    assert_error(&blind, "abap_delete always needs a human confirmation");
    assert_eq!(lab.posts(), 1, "the burnt token ran nothing");

    let token = token_of(&lab.plain("abap_delete_simulate", delete(&["DTEL:zdtel"])));
    let mut session = FakeSession::typing("ZDTEL");
    let deleted = lab.call(
        &mut session,
        "abap_delete",
        with_token(delete(&["DTEL:zdtel"]), &token),
    );

    assert_ok(&deleted);
    assert_eq!(session.asked(), 1);
    assert!(session.forms[0].0.message.contains("DELETE (permanent)"));
    assert!(session.forms[0].0.message.contains("DTEL:ZDTEL"));
    assert!(
        session.forms[0]
            .0
            .message
            .contains("a temporary class is created in $TMP")
    );
    assert!(deleted.text.contains("DELETED"), "{}", deleted.text);
    assert_eq!(lab.classes(), 0);
}

// ---- scrub and leftover -------------------------------------------------------------------------------------

#[test]
fn sap_text_is_scrubbed_in_the_result() {
    let mut lab = lab(TOKEN, created_by_the_second_post());

    *lab.secret.borrow_mut() = Some(format!("bad {PASSWORD} on {HOST} \u{202e} done"));

    let result = lab.plain("abap_create_simulate", tran_create());
    let everything = format!("{} {}", result.text, result.structured);

    assert!(result.is_error, "{everything}");
    // The message came through, and without the secret, the host, or the invisible control.
    assert!(everything.contains("bad "), "{everything}");
    assert!(!everything.contains('\u{202e}'), "{everything}");
    assert!(!everything.contains(PASSWORD), "{everything}");
    assert!(!everything.contains(HOST), "{everything}");
    assert!(result.structured.get("plan_token").is_none());
}

#[test]
fn a_class_that_stays_is_named_in_the_data_of_the_call_that_left_it() {
    let mut lab = lab(TOKEN, created_by_the_second_post());

    lab.sessions.sap.borrow_mut().failing_deletes = 100;

    let result = lab.plain("abap_create_simulate", tran_create());
    let leftover = result.structured["runner_leftover"]
        .as_str()
        .unwrap_or_default();

    assert!(result.is_error, "{}", result.text);
    assert!(leftover.starts_with(PREFIX), "{}", result.structured);
    assert!(
        result
            .text
            .contains(&format!("abapreach delete --simulate CLAS:{leftover}")),
        "{}",
        result.text
    );
    assert!(result.structured.get("plan_token").is_none());
}

// ---- the single in-flight slot -----------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);

        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The lines of a client, then a wait until `eof` is set: a closed input would shut a write down.
struct Lines {
    bytes: std::io::Cursor<Vec<u8>>,
    eof: Arc<AtomicBool>,
}

impl std::io::Read for Lines {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let count = self.bytes.read(out)?;

        if count > 0 {
            return Ok(count);
        }

        let end = Instant::now() + Duration::from_secs(10);

        while !self.eof.load(Ordering::SeqCst) && Instant::now() < end {
            thread::sleep(Duration::from_millis(5));
        }

        Ok(0)
    }
}

/// Holds the first DDIC read until released; answers everything else at once.
struct Holding {
    release: Arc<AtomicBool>,
    held: Arc<AtomicUsize>,
}

impl ToolHandler for Holding {
    fn call(&mut self, name: &str, arguments: &Map<String, Value>) -> ToolResult {
        let ddic = arguments.get("type").and_then(Value::as_str) == Some("DTEL");

        if name == "abap_read_source" && ddic && self.held.fetch_add(1, Ordering::SeqCst) == 0 {
            let end = Instant::now() + Duration::from_secs(10);

            while !self.release.load(Ordering::SeqCst) && Instant::now() < end {
                thread::sleep(Duration::from_millis(5));
            }
        }

        ToolResult {
            text: format!("ran {name}"),
            structured: json!({}),
            is_error: false,
        }
    }
}

#[test]
fn a_ddic_read_takes_the_single_slot_a_source_read_does_not() {
    let call = |id: u64, name: &str, kind: &str| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": name, "arguments": {"conn": "DEV_100", "type": kind, "name": "ZX"}}})
    };
    let input = [
        call(2, "abap_read_source", "DTEL"),
        call(3, "abap_read_source", "DOMA"),
        call(4, "abap_read_source", "TRAN"),
        call(5, "abap_simulate", "DTEL"),
        call(6, "abap_read_source", "PROG"),
    ]
    .map(|line| format!("{line}\n"))
    .concat();
    let release = Arc::new(AtomicBool::new(false));
    let eof = Arc::new(AtomicBool::new(false));
    let held = Arc::new(AtomicUsize::new(0));
    let output = Sink::default();
    let handler = Holding {
        release: release.clone(),
        held: held.clone(),
    };
    let mut server = Server::new(handler, true).with_writable(vec!["DEV_100".to_owned()]);
    let seen = output.clone();
    let end_of_input = eof.clone();

    // Releases the held read once the three refusals are out, ends the input once all five
    // requests are answered; both bounded.
    let watcher = thread::spawn(move || {
        let wait = |enough: &dyn Fn(&str) -> bool| {
            let end = Instant::now() + Duration::from_secs(8);

            while Instant::now() < end {
                let text = String::from_utf8_lossy(&seen.0.lock().unwrap()).into_owned();

                if enough(&text) {
                    break;
                }

                thread::sleep(Duration::from_millis(5));
            }
        };

        wait(&|text| text.matches("busy").count() >= 3);
        release.store(true, Ordering::SeqCst);
        wait(&|text| text.lines().count() >= 5);
        end_of_input.store(true, Ordering::SeqCst);
    });

    serve_with(
        &mut server,
        std::io::BufReader::new(Lines {
            bytes: std::io::Cursor::new(input.into_bytes()),
            eof,
        }),
        output.clone(),
        Vec::new(),
        CancelFlag::new(),
    )
    .unwrap();
    watcher.join().unwrap();

    let frames: Vec<Value> = String::from_utf8(output.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let reply = |id: u64| {
        frames
            .iter()
            .find(|frame| frame["id"] == id)
            .unwrap_or_else(|| panic!("no reply to {id}"))
            .clone()
    };
    let text = |id: u64| {
        reply(id)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // The first DDIC read ran; the next DDIC reads and the write are refused while it holds the slot.
    assert_eq!(text(2), "ran abap_read_source");

    for id in [3, 4, 5] {
        assert_eq!(reply(id)["result"]["isError"], true, "{id}");
        assert!(text(id).contains("busy"), "{id}: {}", text(id));
    }

    // A read of source is not slot-bound: it was served after the slot was free again.
    assert_eq!(text(6), "ran abap_read_source");
    assert_eq!(held.load(Ordering::SeqCst), 1);
}

#[test]
fn the_tools_name_no_new_write_tool() {
    let tools = tool_list_for(&["DEV_100".to_owned()]);
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();

    assert_eq!(
        names,
        [
            "abap_read_source",
            "abap_read_table",
            "abap_simulate",
            "abap_deploy",
            "abap_create_simulate",
            "abap_create",
            "abap_delete_simulate",
            "abap_delete"
        ]
    );
}
