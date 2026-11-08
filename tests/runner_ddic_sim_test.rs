//! `deploy --simulate` and `create --simulate` of `DTEL`, `DOMA`, `TABL`, and `TTYP` over the
//! temporary runner class (M6 Task 19), driven through the command line against a fake SAP. The
//! fake keeps the runner class state, answers the data previews, and answers the classrun POST
//! with a run bound to the nonce that the generated payload carries. No SAP.
//!
//! The run outputs are written to the shapes that the template emits (message texts of
//! `ddic_impl.abap`, the `CONVERSION` line of live run 12A: `action=ALT modeflag=F daction=ALT
//! allowed=X rows=0`); the live bytes are Task 12A evidence, not recorded here.
//!
//! Ports of the Python simulate cases (`tools/test_rfc_deploy.py`) and their intentional
//! differences:
//! - `test_ddic_create_requires_file`, `test_ddic_create_rejects_source_options`: ported
//!   (`create_needs_a_file_...`, `create_rejects_the_options_of_other_types`).
//! - `test_ddic_create_sends_xml_single_call`: ported as the payload checks of the create cases;
//!   the XML travels inside the payload of a temporary class, not as an FM parameter.
//! - The Python simulate prints `SIMULATE: SIMULATED_OK` after the diff; here the status is the
//!   bare token of the M1 Rust deploy (`SIMULATED_OK`), and the output starts with the `RUNNER:`
//!   line (the temporary class is a repository footprint that Python does not have).
//! - `WHERE-USED` prints `(transparent)` where Python prints `(transparan)`.
//! - `--base` that differs from the live hash is a STOP with exit 2, as in M1 (Python: `Stop`).

mod common;

use abapreach::config::Connection;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use common::preview;
use common::runner_fake::{PostReply, Shared, World, http_session, new_sap, rfc_session};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

type Opened = Result<Box<dyn SadtTransport>, TransportError>;
/// The decoded payload document of a class: every `KEY=VALUE` line in order.
type Payload = Vec<(String, String)>;
/// The payloads of the classes that were posted, in order.
type Seen = Rc<RefCell<Vec<Payload>>>;

const CLASSRUN: &str = "/sap/bc/adt/oo/classrun/";
const PAYLOAD_PREFIX: &str = "    lv_b64 = lv_b64 && '";
const PREFLIGHT: &str =
    "RUNNER: preflight T000 CCCATEGORY=C CCCORACTIV=1 CCNOCLIIND=- TRNSPACE /0CUST/ EDITFLAG=X";
const OTHER_BASE: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

const DEPLOY_GUARDS: [&str; 5] = [
    "Customer names only (Z or Y)",
    "System change option, client role, and customer namespace",
    "Request owner and state, and the package of the object",
    "Inactive version of anyone, and the base hash",
    "Conversion verdict of DD_DECIDE, also for dependent tables",
];
const CREATE_GUARDS: [&str; 4] = [
    "Customer names only (Z or Y)",
    "System change option, client role, and customer namespace",
    "Request owner and state, and the package of the object",
    "Conversion verdict of DD_DECIDE, also for dependent tables",
];

/// `(token, live definition, name in the definition, text in it, changed text)`.
const FIXTURES: [(&str, &str, &str, &str, &str); 4] = [
    (
        "DTEL",
        include_str!("fixtures/m6/read_dtel.xml"),
        "ZDTEL",
        "Test data element",
        "Changed data element",
    ),
    (
        "DOMA",
        include_str!("fixtures/m6/read_doma.xml"),
        "ZDOMA",
        "Test domain",
        "Changed domain",
    ),
    (
        "TABL",
        include_str!("fixtures/m6/read_tabl.xml"),
        "ZTABL",
        "Test table",
        "Changed table",
    ),
    (
        "TTYP",
        include_str!("fixtures/m6/read_ttyp.xml"),
        "ZTTYP",
        "Test table type",
        "Changed table type",
    ),
];

const TABL: usize = 2;

// ---- the runner output ------------------------------------------------------------------------

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

/// A whole run: BEGIN, the body, the status, and the closing count, as the classrun frames it.
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

fn live_hash(xml: &str) -> String {
    source_hash(&rows_of(xml))
}

/// What a `SIMULATE` run answers: the live rows (with the debris that the client rule removes),
/// the live hash, the lines of `extra`, and the status.
fn deploy_run(nonce: &str, xml: &str, extra: &[String], status: &str) -> String {
    let mut rows = rows_of(xml);

    rows[2].push_str("  ");
    rows.push(String::new());

    let mut body = vec![fact("HASH", &live_hash(xml))];

    body.extend(rows.iter().map(|text| row(text)));
    body.extend(extra.iter().cloned());

    run_output(nonce, "SIMULATE", &body, status)
}

fn guards(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| fact("GUARD", text)).collect()
}

/// The lines of a passing table change on an empty table: the live verdict of run 12A.
fn passing(name: &str) -> Vec<String> {
    let mut lines = vec![
        info(&format!(
            "CONVERSION {name}: action=ALT modeflag=F daction=ALT allowed=X rows=0"
        )),
        info("WHERE-USED: none"),
    ];

    lines.extend(guards(&DEPLOY_GUARDS));

    lines
}

fn rejected(extra: &[String], reason: &str) -> Vec<String> {
    let mut body = extra.to_vec();

    body.push(error(reason));

    body
}

// ---- the payload the class carries --------------------------------------------------------------

fn base64_value(byte: u8) -> u32 {
    match byte {
        b'A'..=b'Z' => u32::from(byte - b'A'),
        b'a'..=b'z' => u32::from(byte - b'a') + 26,
        b'0'..=b'9' => u32::from(byte - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64: {byte}"),
    }
}

fn base64_decode(text: &str) -> String {
    let bytes = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::new();

    for chunk in bytes.chunks(4) {
        let mut bits = 0u32;

        for (index, byte) in chunk.iter().enumerate() {
            bits |= base64_value(*byte) << (18 - 6 * index);
        }

        for index in 0..chunk.len() - 1 {
            out.push((bits >> (16 - 8 * index)) as u8);
        }
    }

    String::from_utf8(out).unwrap()
}

/// The decoded payload document of a generated class source: every `KEY=VALUE` line in order.
fn payload_of(source: &str) -> Payload {
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

fn value(payload: &[(String, String)], key: &str) -> Option<String> {
    payload
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

fn xml_of(payload: &[(String, String)]) -> Vec<String> {
    payload
        .iter()
        .filter(|(key, _)| key == "XML")
        .map(|(_, value)| value.clone())
        .collect()
}

// ---- the fake system ----------------------------------------------------------------------------

type Script = Rc<dyn Fn(&str) -> PostReply>;

/// Wraps a session: when the classrun POST arrives it reads the nonce from the class source that
/// the product wrote and queues the scripted reply for it, then lets the fake SAP answer.
struct Responder {
    inner: Box<dyn SadtTransport>,
    sap: Shared,
    script: Script,
    /// The payloads of the classes that were posted, in order.
    seen: Seen,
}

impl SadtTransport for Responder {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if request.method == "POST"
            && let Some(name) = request.uri.strip_prefix(CLASSRUN)
        {
            let source = self
                .sap
                .borrow()
                .classes
                .get(&name.to_ascii_uppercase())
                .cloned()
                .unwrap_or_default();
            let payload = payload_of(&source);
            let nonce = value(&payload, "NONCE").unwrap_or_default();
            let reply = (self.script)(&nonce);

            self.seen.borrow_mut().push(payload);
            self.sap.borrow_mut().post_replies.push_back(reply);
        }

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.inner.stateful_supported()
    }
}

struct Fake {
    sap: Shared,
    script: Script,
    cancel: CancelFlag,
    seen: Seen,
    adt_down: bool,
    adt_opens: usize,
    rfc_opens: usize,
}

impl Fake {
    fn wrap(&self, inner: Opened) -> Opened {
        Ok(Box::new(Responder {
            inner: inner?,
            sap: self.sap.clone(),
            script: self.script.clone(),
            seen: self.seen.clone(),
        }))
    }
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opens += 1;

        if self.adt_down {
            return Err(TransportError::PreSendConnectivity("no route".to_owned()));
        }

        self.wrap(http_session(&self.sap))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opens += 1;

        self.wrap(rfc_session(&self.sap))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("a simulate probes no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("a simulate pings nothing")
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

/// A factory set that fails the test on any call: the command must stop before a connection.
struct NoTransport;

impl Factories for NoTransport {
    fn adt(&mut self, _: &Connection) -> Opened {
        panic!("this command opens no ADT connection")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        panic!("this command opens no RFC connection")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("no ping")
    }
}

fn env_in(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}},"#,
        r#"{"systemId":"QAS","clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"02"}},"#,
        r#"{"systemId":"ADO","host":"example.com","port":443,"clients":["100"]}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
        ("ADO_100_USER".to_owned(), "user".to_owned()),
        ("ADO_100_PASS".to_owned(), "secret-pw".to_owned()),
        ("QAS_100_USER".to_owned(), "user".to_owned()),
        ("QAS_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn sap(stateful: bool, world: World) -> Shared {
    new_sap(
        stateful,
        &Rc::new(Cell::new(Duration::ZERO)),
        Box::new(move |_| world),
    )
}

/// A deploy simulate finds the definition and its TADIR row, and leaves them.
const PRESENT: World = World {
    definition: true,
    directory: true,
    inactive: false,
    broken: false,
};

/// A create simulate finds nothing, and leaves nothing.
const ABSENT: World = World {
    definition: false,
    directory: false,
    inactive: false,
    broken: false,
};

/// One scripted command: the verb, the system, the reply to the POST, and the file.
struct Case {
    verb: &'static str,
    kind: &'static str,
    name: &'static str,
    conn: &'static str,
    extra: Vec<&'static str>,
    sap: Shared,
    script: Script,
    adt_down: bool,
    cancel: CancelFlag,
    /// The text of the XML file.
    file: String,
}

struct Done {
    exit: Exit,
    sap: Shared,
    seen: Seen,
    adt_opens: usize,
    rfc_opens: usize,
}

impl Done {
    /// The runner class names the command created.
    fn creates(&self) -> usize {
        self.sap
            .borrow()
            .log
            .iter()
            .filter(|(_, line)| line == "POST /sap/bc/adt/oo/classes")
            .count()
    }

    fn payload(&self) -> Payload {
        let seen = self.seen.borrow();

        assert_eq!(seen.len(), 1, "one run");

        seen[0].clone()
    }
}

impl Case {
    fn deploy(index: usize, script: Script) -> Self {
        let (kind, xml, name, text, changed) = FIXTURES[index];

        Self::new("deploy", kind, name, xml.replace(text, changed), script)
    }

    fn create(index: usize, script: Script) -> Self {
        let (kind, xml, name, ..) = FIXTURES[index];
        let mut case = Self::new("create", kind, name, xml.to_owned(), script);

        case.sap = sap(true, ABSENT);

        case
    }

    fn new(
        verb: &'static str,
        kind: &'static str,
        name: &'static str,
        file: String,
        script: Script,
    ) -> Self {
        Self {
            verb,
            kind,
            name,
            conn: "DEV_100",
            extra: Vec::new(),
            sap: sap(true, PRESENT),
            script,
            adt_down: false,
            cancel: CancelFlag::new(),
            file,
        }
    }

    fn args(&self, file: &Path) -> Vec<String> {
        let mut args: Vec<String> = ["abapreach", self.verb, "--simulate", self.kind, self.name]
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();

        args.push(file.display().to_string());
        args.extend(self.extra.iter().map(|argument| (*argument).to_owned()));
        args.extend(["--conn".to_owned(), self.conn.to_owned()]);

        args
    }

    fn go(self) -> Done {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("in.xml");

        fs::write(&file, &self.file).unwrap();

        let args = self.args(&file);
        let seen: Seen = Rc::default();
        let mut fake = Fake {
            sap: self.sap.clone(),
            script: self.script,
            cancel: self.cancel,
            seen: seen.clone(),
            adt_down: self.adt_down,
            adt_opens: 0,
            rfc_opens: 0,
        };
        let exit = run(args, env_in(directory.path()), &mut fake);

        Done {
            exit,
            sap: self.sap,
            seen,
            adt_opens: fake.adt_opens,
            rfc_opens: fake.rfc_opens,
        }
    }

    /// Runs the command with a factory set that fails on any connection and returns the exit.
    fn stops(self) -> Exit {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("in.xml");

        fs::write(&file, &self.file).unwrap();

        run(self.args(&file), env_in(directory.path()), &mut NoTransport)
    }
}

fn reply(make: impl Fn(&str) -> String + 'static) -> Script {
    Rc::new(move |nonce| PostReply::Output(make(nonce)))
}

/// The scripted reply of a deploy simulate against the live definition `xml`.
fn deploy_reply(xml: &'static str, extra: Vec<String>, status: &'static str) -> Script {
    reply(move |nonce| deploy_run(nonce, xml, &extra, status))
}

/// The scripted reply of a create simulate.
fn create_reply(extra: Vec<String>, status: &'static str) -> Script {
    reply(move |nonce| run_output(nonce, "CREATE_SIM", &extra, status))
}

#[track_caller]
fn assert_clean(done: &Done) {
    assert!(
        done.sap.borrow().classes.is_empty(),
        "a runner class was left: {:?}",
        done.sap.borrow().classes.keys().collect::<Vec<_>>()
    );
}

/// The `RUNNER:` line of the output, with the class name cut out.
#[track_caller]
fn runner_line(stdout: &str, post: &str) -> String {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("RUNNER: YCL_ABAPREACH_RUN_"))
        .unwrap_or_else(|| panic!("no RUNNER line in {stdout:?}"));
    let rest = line.strip_prefix("RUNNER: YCL_ABAPREACH_RUN_").unwrap();
    let (suffix, tail) = rest.split_at(8);

    assert_eq!(
        tail,
        format!(" (temporary, $TMP, post via {post}, removed after the run)"),
        "{line}"
    );

    format!("YCL_ABAPREACH_RUN_{suffix}")
}

fn lines_of(stdout: &str) -> Vec<&str> {
    stdout.lines().collect()
}

// ---- deploy --simulate: the output ------------------------------------------------------------------

#[test]
fn a_deploy_simulate_prints_the_diff_the_hash_the_verdict_the_guards_and_the_status() {
    let (kind, xml, name, text, changed) = FIXTURES[0];
    let done = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK")).go();
    let runner = runner_line(&done.exit.stdout, "http");
    let lines = lines_of(&done.exit.stdout);
    let diff_at = lines.iter().position(|line| *line == "--- live").unwrap();
    let hash_at = lines
        .iter()
        .position(|line| *line == format!("live hash {}", live_hash(xml)))
        .unwrap();

    assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
    assert_eq!(done.exit.stderr, "");
    assert_eq!(
        lines[..diff_at],
        [
            "backend: adt".to_owned(),
            "reason: default ADT".to_owned(),
            format!("RUNNER: {runner} (temporary, $TMP, post via http, removed after the run)"),
            PREFLIGHT.to_owned(),
        ]
    );
    assert_eq!(lines[diff_at + 1], "+++ file");
    assert!(
        lines[diff_at..hash_at].contains(&format!("-    <DDTEXT>{text}</DDTEXT>").as_str()),
        "{}",
        done.exit.stdout
    );
    assert!(
        lines[diff_at..hash_at].contains(&format!("+    <DDTEXT>{changed}</DDTEXT>").as_str()),
        "{}",
        done.exit.stdout
    );
    assert_eq!(
        lines[hash_at + 1..],
        [
            "CONVERSION ZDTEL: action=ALT modeflag=F daction=ALT allowed=X rows=0",
            "WHERE-USED: none",
            "GUARD Customer names only (Z or Y)",
            "GUARD System change option, client role, and customer namespace",
            "GUARD Request owner and state, and the package of the object",
            "GUARD Inactive version of anyone, and the base hash",
            "GUARD Conversion verdict of DD_DECIDE, also for dependent tables",
            "SIMULATED_OK",
        ]
    );
    assert_eq!(name, "ZDTEL");
}

#[test]
fn the_four_types_simulate_on_adt_http_and_on_rfc_sadt() {
    for (index, (kind, xml, name, ..)) in FIXTURES.iter().enumerate() {
        for backend in [None, Some("rfc")] {
            let mut case = Case::deploy(index, deploy_reply(xml, passing(name), "SIMULATED_OK"));

            if let Some(backend) = backend {
                case.extra = vec!["--backend", backend];
            }

            let done = case.go();
            let post = if backend.is_some() { "sadt" } else { "http" };

            assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
            assert!(
                done.exit.stdout.ends_with("\nSIMULATED_OK"),
                "{kind}: {}",
                done.exit.stdout
            );
            runner_line(&done.exit.stdout, post);
            assert_eq!(done.creates(), 1, "{kind}");
            assert_eq!(done.sap.borrow().posts, 1, "{kind}");
            assert_clean(&done);
            assert_eq!(
                (done.adt_opens > 0, done.rfc_opens > 0),
                (backend.is_none(), backend.is_some()),
                "{kind}"
            );
        }
    }
}

#[test]
fn the_payload_is_a_simulate_with_the_object_the_rows_and_no_base() {
    for (index, (kind, xml, name, ..)) in FIXTURES.iter().enumerate() {
        let mut case = Case::deploy(index, deploy_reply(xml, passing(name), "SIMULATED_OK"));

        case.extra = vec!["--tr", "hd1k917754"];

        let file = case.file.clone();
        let done = case.go();
        let payload = done.payload();

        assert_eq!(value(&payload, "MODE").as_deref(), Some("SIMULATE"));
        assert_eq!(value(&payload, "TYPE").as_deref(), Some(*kind));
        assert_eq!(value(&payload, "NAME").as_deref(), Some(*name));
        assert_eq!(value(&payload, "TR").as_deref(), Some("HD1K917754"));
        assert_eq!(value(&payload, "NONCE").unwrap().len(), 32);
        // The base is compared here: a run that got it would answer another text.
        assert_eq!(value(&payload, "BASE"), None, "{kind}");
        assert_eq!(xml_of(&payload), rows_of(&file), "{kind}");
        assert_eq!(
            value(&payload, "XMLROWS"),
            Some(rows_of(&file).len().to_string())
        );
    }
}

#[test]
fn a_file_equal_to_the_live_definition_is_no_changes_with_one_run_and_no_diff() {
    for (index, (kind, xml, ..)) in FIXTURES.iter().enumerate() {
        let extra = vec![info("No change: file equals active definition")];
        let mut case = Case::deploy(index, deploy_reply(xml, extra, "NO_CHANGES"));

        case.file = (*xml).to_owned();

        let done = case.go();

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert!(
            done.exit.stdout.ends_with("\nno changes"),
            "{kind}: {}",
            done.exit.stdout
        );
        assert!(!done.exit.stdout.contains("--- live"), "{kind}");
        assert!(!done.exit.stdout.contains("SIMULATED_OK"), "{kind}");
        assert_eq!(done.sap.borrow().posts, 1, "{kind}");
        assert_clean(&done);
    }
}

// ---- --base ---------------------------------------------------------------------------------------

#[test]
fn a_base_that_differs_from_the_live_hash_is_a_stop_after_the_one_run() {
    let (_, xml, ..) = FIXTURES[0];
    let mut case = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK"));

    case.extra = vec!["--base", OTHER_BASE];

    let done = case.go();

    assert_eq!(done.exit.code, 2);
    assert_eq!(
        done.exit.stderr,
        format!(
            "STOP: live changed: live hash {} != --base {OTHER_BASE}",
            live_hash(xml)
        )
    );
    assert!(!done.exit.stdout.contains("SIMULATED_OK"));
    assert!(!done.exit.stdout.contains("--- live"));
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(&done);
}

#[test]
fn a_base_that_equals_the_live_hash_simulates() {
    let (_, xml, ..) = FIXTURES[0];
    let hash: &'static str = Box::leak(live_hash(xml).into_boxed_str());
    let mut case = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK"));

    case.extra = vec!["--base", hash];

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(done.exit.stdout.ends_with("\nSIMULATED_OK"));
}

#[test]
fn a_base_that_is_no_hash_stops_before_any_connection() {
    let (_, xml, ..) = FIXTURES[0];
    let mut case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));

    case.extra = vec!["--base", "not-a-hash"];

    let exit = case.stops();

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr
            .starts_with("STOP: --base must be 64 hex digits"),
        "{}",
        exit.stderr
    );
}

// ---- the conversion verdict -------------------------------------------------------------------------

#[test]
fn a_change_on_an_empty_table_is_allowed_and_prints_the_conversion_line() {
    let (_, xml, ..) = FIXTURES[TABL];
    let done = Case::deploy(TABL, deploy_reply(xml, passing("ZTABL"), "SIMULATED_OK")).go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(
        done.exit
            .stdout
            .contains("\nCONVERSION ZTABL: action=ALT modeflag=F daction=ALT allowed=X rows=0\n"),
        "{}",
        done.exit.stdout
    );
    assert!(done.exit.stdout.ends_with("\nSIMULATED_OK"));
}

/// A rejected simulate: the exit is 1, the verdict lines stay, the status line carries the
/// reason, and nothing claims a success.
#[track_caller]
fn assert_rejected(done: &Done, reason: &str) {
    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit.stdout.ends_with(&format!("\nREJECTED: {reason}")),
        "{}",
        done.exit.stdout
    );
    assert!(!done.exit.stdout.lines().any(|line| line == "SIMULATED_OK"));
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(done);
}

#[test]
fn a_table_with_rows_that_needs_se14_is_rejected_and_shows_the_verdict() {
    let (_, xml, ..) = FIXTURES[TABL];
    let reason = "Table ZTABL (3 rows) needs SE14 conversion; change rejected";
    let extra = rejected(
        &[info(
            "CONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed= rows=3",
        )],
        reason,
    );
    let done = Case::deploy(TABL, deploy_reply(xml, extra, "REJECTED")).go();

    assert_rejected(&done, reason);
    assert!(
        done.exit
            .stdout
            .contains("\nCONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed= rows=3\n"),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn a_class_change_and_a_buffered_key_change_are_rejected_with_the_reason_of_the_run() {
    let (_, xml, ..) = FIXTURES[TABL];

    for reason in [
        "TABCLASS ZTABL from/to TRANSP not supported (SE11/SE14)",
        "Key ZTABL changed on a buffered table: cannot be assessed without a write, rejected",
    ] {
        let done = Case::deploy(TABL, deploy_reply(xml, rejected(&[], reason), "REJECTED")).go();

        assert_rejected(&done, reason);
    }
}

#[test]
fn a_data_element_change_that_reaches_a_transparent_table_prints_the_where_used_and_the_verdict() {
    let (_, xml, ..) = FIXTURES[0];
    let mut extra = vec![
        info("WHERE-USED TABL ZTAB2 (transparent)"),
        info("CONVERSION ZTAB2: action=ALT modeflag=F daction=ALT allowed=X rows=0"),
    ];

    extra.extend(guards(&DEPLOY_GUARDS));

    let done = Case::deploy(0, deploy_reply(xml, extra, "SIMULATED_OK")).go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(
        done.exit
            .stdout
            .contains("\nWHERE-USED TABL ZTAB2 (transparent)\nCONVERSION ZTAB2: action=ALT"),
        "{}",
        done.exit.stdout
    );
    assert!(!done.exit.stdout.contains("transparan\n"));
}

#[test]
fn a_domain_change_that_needs_se14_on_a_dependent_table_is_rejected() {
    let (_, xml, ..) = FIXTURES[1];
    let reason = "Table ZTAB2 (5 rows) needs SE14 conversion (action=CNV modeflag=U daction=CNV)";
    let extra = rejected(&[info("WHERE-USED TABL ZTAB2 (transparent)")], reason);
    let done = Case::deploy(1, deploy_reply(xml, extra, "REJECTED")).go();

    assert_rejected(&done, reason);
    assert!(
        done.exit
            .stdout
            .contains("\nWHERE-USED TABL ZTAB2 (transparent)\n")
    );
}

#[test]
fn a_structure_change_used_by_a_transparent_table_is_rejected() {
    let (_, xml, ..) = FIXTURES[TABL];
    let reason = "Structure ZTABL is used by transparent table (ZTAB2): field change rejected";
    let done = Case::deploy(TABL, deploy_reply(xml, rejected(&[], reason), "REJECTED")).go();

    assert_rejected(&done, reason);
}

#[test]
fn an_unavailable_row_count_is_rejected_and_the_verdict_is_never_claimed() {
    let (_, xml, ..) = FIXTURES[TABL];
    let reason = "The row count of ZTABL is not available";
    let done = Case::deploy(TABL, deploy_reply(xml, rejected(&[], reason), "REJECTED")).go();

    assert_rejected(&done, reason);
    assert!(!done.exit.stdout.contains("CONVERSION"));
}

#[test]
fn a_simulated_ok_of_a_transparent_table_without_a_verdict_is_rejected_fail_closed() {
    let (_, xml, ..) = FIXTURES[TABL];
    let done = Case::deploy(
        TABL,
        deploy_reply(xml, guards(&DEPLOY_GUARDS), "SIMULATED_OK"),
    )
    .go();

    assert_rejected(
        &done,
        "the run reported SIMULATED_OK without a conversion verdict for table ZTABL; \
         the change is rejected",
    );
}

#[test]
fn a_simulated_ok_next_to_a_conversion_that_is_not_allowed_is_rejected_fail_closed() {
    let (_, xml, ..) = FIXTURES[TABL];

    // A blank flag as the template renders it, one or two blanks, and a line without the field.
    for line in [
        "CONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed= rows=3",
        "CONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed=  rows=3",
        "CONVERSION ZTABL: action=CNV modeflag=U daction=CNV rows=3",
        "CONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed=x rows=3",
    ] {
        let done = Case::deploy(TABL, deploy_reply(xml, vec![info(line)], "SIMULATED_OK")).go();

        assert_rejected(
            &done,
            "the run reported SIMULATED_OK next to a conversion that is not allowed \
             (CONVERSION ZTABL); the change is rejected",
        );
    }
}

// ---- the other rejections --------------------------------------------------------------------------

#[test]
fn a_version_that_waits_for_activation_is_rejected_not_inactive() {
    let (_, xml, ..) = FIXTURES[0];
    let reason =
        "DTEL ZDTEL has an inactive version or revision: activate or discard it in SE11 first";
    let done = Case::deploy(0, deploy_reply(xml, rejected(&[], reason), "REJECTED")).go();

    assert_rejected(&done, reason);
    assert!(!done.exit.stdout.contains("\nINACTIVE"));
}

#[test]
fn an_object_that_does_not_exist_is_rejected_without_a_diff() {
    let reason = "DTEL ZDTEL active version not found";
    let script = reply(move |nonce| run_output(nonce, "SIMULATE", &[error(reason)], "REJECTED"));
    let done = Case::deploy(0, script).go();

    assert_rejected(&done, reason);
    assert!(!done.exit.stdout.contains("--- live"));
}

#[test]
fn a_file_for_another_object_stops_before_any_connection() {
    let (_, xml, ..) = FIXTURES[0];
    let mut case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));

    case.file = case.file.replace("ZDTEL", "ZOTHER");

    let exit = case.stops();

    assert_eq!(exit.code, 2);
    assert_eq!(exit.stderr, "STOP: XML file is for ZOTHER, not DTEL ZDTEL");
}

#[test]
fn a_transaction_a_foreign_name_and_a_bad_request_stop_before_any_connection() {
    let (_, xml, ..) = FIXTURES[0];

    let mut case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));

    case.kind = "TRAN";

    assert_eq!(
        case.stops().stderr,
        "STOP: TRAN has no deploy; use create or delete"
    );

    let mut case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));

    case.name = "MANDT";

    assert_eq!(
        case.stops().stderr,
        "STOP: write and delete accept only Z and Y names: MANDT"
    );

    let mut case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));

    case.extra = vec!["--tr", "hd1;k917754"];

    assert_eq!(case.stops().code, 2);
}

// ---- no state is left -------------------------------------------------------------------------------

#[test]
fn a_simulate_is_confirmed_by_a_new_session_and_leaves_no_class() {
    let (_, xml, ..) = FIXTURES[0];
    let done = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK")).go();
    let sap = done.sap.borrow();
    let creates: Vec<usize> = sap
        .log
        .iter()
        .filter(|(_, line)| line == "POST /sap/bc/adt/oo/classes")
        .map(|(session, _)| *session)
        .collect();
    let previews: Vec<usize> = sap
        .log
        .iter()
        .filter(|(_, line)| line.starts_with("POST /sap/bc/adt/datapreview/ddic"))
        .map(|(session, _)| *session)
        .collect();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(creates.len(), 1);
    // The two preflight previews run in the session of the class; the three of the evidence run
    // in another one.
    assert_eq!(previews.len(), 5, "{previews:?}");
    assert_eq!(previews[0], creates[0]);
    assert!(previews[2..].iter().all(|session| *session != creates[0]));
    assert!(sap.classes.is_empty());
}

#[test]
fn a_deploy_simulate_whose_definition_has_vanished_is_partial_not_simulated_ok() {
    let (_, xml, ..) = FIXTURES[0];
    let mut case = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK"));

    case.sap = sap(true, ABSENT);

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit.stdout.contains(
            "\nPARTIAL: the run reported SIMULATED_OK but a new session disagrees (definition DD04L \
             has no row; TADIR has no row); check with read before retrying"
        ),
        "{}",
        done.exit.stdout
    );
    assert!(!done.exit.stdout.ends_with("SIMULATED_OK"));
}

#[test]
fn a_preview_that_cannot_be_read_is_a_note_and_the_simulate_stands() {
    let (_, xml, ..) = FIXTURES[0];
    let mut case = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK"));

    case.sap = sap(
        true,
        World {
            broken: true,
            ..PRESENT
        },
    );

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(done.exit.stdout.ends_with("\nSIMULATED_OK"));
    assert!(
        done.exit
            .stderr
            .contains("W: the new session could not confirm the simulate"),
        "{}",
        done.exit.stderr
    );
}

// ---- uncertain status, cleanup, signals --------------------------------------------------------------

#[test]
fn a_lost_reply_and_a_broken_run_are_uncertain_and_never_resent() {
    let (_, xml, ..) = FIXTURES[0];

    for script in [
        Rc::new(|_: &str| PostReply::Lost) as Script,
        Rc::new(|_: &str| PostReply::Status(500, "boom".to_owned())),
        reply(|nonce| format!("ABAPREACH:{nonce}:BEGIN:SIMULATE\n")),
    ] {
        let done = Case::deploy(0, script).go();

        assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
        assert!(
            done.exit
                .stderr
                .contains("the simulate status is uncertain"),
            "{}",
            done.exit.stderr
        );
        assert!(
            done.exit.stderr.contains("nothing was written"),
            "{}",
            done.exit.stderr
        );
        assert!(!done.exit.stdout.contains("SIMULATED_OK"));
        assert_eq!(done.sap.borrow().posts, 1);
        assert_clean(&done);
    }

    assert_eq!(live_hash(xml).len(), 64);
}

#[test]
fn a_refused_post_is_rejected_and_nothing_ran() {
    let done = Case::deploy(
        0,
        Rc::new(|_| PostReply::Status(403, "no authorization".to_owned())),
    )
    .go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .contains("\nREJECTED: runner POST failed: HTTP 403"),
        "{}",
        done.exit.stdout
    );
    assert_clean(&done);
}

#[test]
fn a_class_that_stays_is_exit_1_although_the_simulate_succeeded() {
    let (_, xml, ..) = FIXTURES[0];
    let case = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK"));

    case.sap.borrow_mut().failing_deletes = 100;

    let done = case.go();
    let left: Vec<String> = done.sap.borrow().classes.keys().cloned().collect();

    assert_eq!(left.len(), 1);
    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit.stdout.ends_with("\nSIMULATED_OK"),
        "{}",
        done.exit.stdout
    );
    assert!(
        done.exit.stderr.contains(&format!(
            "RUNNER_LEFTOVER: {0}\nabapreach delete --simulate CLAS:{0} --plan <FILE> --conn \
             DEV_100\nabapreach delete CLAS:{0} --plan <FILE> --conn DEV_100",
            left[0]
        )),
        "{}",
        done.exit.stderr
    );
}

#[test]
fn ctrl_c_before_and_after_the_post_exits_130() {
    let (_, xml, ..) = FIXTURES[0];
    let case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));

    case.cancel.set();

    let done = case.go();

    assert_eq!(done.exit.code, 130);
    assert_eq!(done.creates(), 0);
    assert_eq!(done.adt_opens + done.rfc_opens, 0);

    let case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));
    let cancel = case.cancel.clone();

    case.sap.borrow_mut().on_post = Some(Box::new(move || cancel.set()));

    let done = case.go();

    assert_eq!(done.exit.code, 130);
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(&done);
}

#[test]
fn a_repository_that_forbids_a_class_stops_before_any_class_exists() {
    let (_, xml, ..) = FIXTURES[0];
    let case = Case::deploy(0, deploy_reply(xml, Vec::new(), "SIMULATED_OK"));
    let sap = case.sap.clone();
    // The namespace preview answers EDITFLAG N through a wrapped fake.
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("in.xml");

    fs::write(&file, &case.file).unwrap();

    struct Closed(Shared, CancelFlag);

    struct ClosedSession(Box<dyn SadtTransport>);

    impl SadtTransport for ClosedSession {
        fn request(&mut self, request: Request) -> Result<Response, TransportError> {
            if request.uri.starts_with("/sap/bc/adt/datapreview/ddic")
                && String::from_utf8_lossy(&request.body).contains("FROM TRNSPACE")
            {
                return Ok(Response::text(200, preview(&[("EDITFLAG", &[Some("N")])])));
            }

            self.0.request(request)
        }

        fn close(&mut self) -> Result<(), TransportError> {
            self.0.close()
        }

        fn stateful_supported(&self) -> Option<bool> {
            self.0.stateful_supported()
        }
    }

    impl Factories for Closed {
        fn adt(&mut self, _: &Connection) -> Opened {
            Ok(Box::new(ClosedSession(http_session(&self.0)?)))
        }

        fn rfc(&mut self, _: &Connection) -> Opened {
            panic!("no RFC")
        }

        fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
            panic!("no library")
        }

        fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
            panic!("no ping")
        }

        fn cancel(&self) -> CancelFlag {
            self.1.clone()
        }
    }

    let exit = run(
        case.args(&file),
        env_in(directory.path()),
        &mut Closed(sap.clone(), CancelFlag::new()),
    );

    assert_eq!(exit.code, 2);
    assert_eq!(
        exit.stderr,
        "STOP: deploy DTEL needs a temporary class; repository changes are not allowed on \
         DEV/100 (customer namespace /0CUST/ is not modifiable; TRNSPACE EDITFLAG N)"
    );
    assert_eq!(sap.borrow().count("POST /sap/bc/adt/oo/classes"), 0);
    assert_eq!(sap.borrow().posts, 0);
}

#[test]
fn a_connection_that_cannot_be_opened_falls_back_to_rfc_before_anything_is_written() {
    let (_, xml, ..) = FIXTURES[0];
    let mut case = Case::deploy(0, deploy_reply(xml, passing("ZDTEL"), "SIMULATED_OK"));

    case.adt_down = true;

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    runner_line(&done.exit.stdout, "sadt");
    assert!(
        done.exit.stdout.contains("reason: fallback to RFC"),
        "{}",
        done.exit.stdout
    );
    assert!(
        done.exit.stderr.contains(
            "NOTE: ADT HTTP failed before anything was written; falling back to RFC SADT"
        ),
        "{}",
        done.exit.stderr
    );
}

// ---- create --simulate ------------------------------------------------------------------------------

fn create_lines(kind: &str, name: &str, package: &str) -> Vec<String> {
    let mut lines = vec![
        info(&format!("CREATE {kind} {name} package {package} TR -")),
        fact("PACKAGE", package),
    ];

    lines.extend(guards(&CREATE_GUARDS));

    lines
}

#[test]
fn a_create_simulate_prints_the_plan_the_guards_and_the_status_for_each_type() {
    for (index, (kind, _, name, ..)) in FIXTURES.iter().enumerate() {
        let done = Case::create(
            index,
            create_reply(create_lines(kind, name, "$TMP"), "SIMULATED_OK"),
        )
        .go();
        let runner = runner_line(&done.exit.stdout, "http");

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert_eq!(done.exit.stderr, "", "{kind}");
        assert_eq!(
            done.exit.stdout,
            format!(
                "backend: adt\nreason: default ADT\n\
                 RUNNER: {runner} (temporary, $TMP, post via http, removed after the run)\n\
                 {PREFLIGHT}\n\
                 CREATE {kind} {name} package $TMP TR -\n\
                 WILL CREATE {kind} {name} in package $TMP\n\
                 GUARD Customer names only (Z or Y)\n\
                 GUARD System change option, client role, and customer namespace\n\
                 GUARD Request owner and state, and the package of the object\n\
                 GUARD Conversion verdict of DD_DECIDE, also for dependent tables\n\
                 CREATE_SIM: SIMULATED_OK\n\
                 validated; nothing was written"
            ),
            "{kind}"
        );
        assert_eq!(done.creates(), 1, "{kind}");
        assert_eq!(done.sap.borrow().posts, 1, "{kind}");
        assert_clean(&done);
    }
}

#[test]
fn the_create_payload_carries_the_rows_the_package_the_request_and_the_text() {
    for (index, (kind, xml, name, text, _)) in FIXTURES.iter().enumerate() {
        let mut case = Case::create(
            index,
            create_reply(create_lines(kind, name, "ZABAPTEST"), "SIMULATED_OK"),
        );

        case.extra = vec![
            "--package",
            "zabaptest",
            "--tr",
            "hd1k917754",
            "--text",
            text,
        ];

        let done = case.go();
        let payload = done.payload();

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert_eq!(value(&payload, "MODE").as_deref(), Some("CREATE_SIM"));
        assert_eq!(value(&payload, "TYPE").as_deref(), Some(*kind));
        assert_eq!(value(&payload, "PACKAGE").as_deref(), Some("ZABAPTEST"));
        assert_eq!(value(&payload, "TR").as_deref(), Some("HD1K917754"));
        assert_eq!(value(&payload, "BASE"), None);
        assert_eq!(xml_of(&payload), rows_of(xml), "{kind}");
        assert!(
            done.exit.stdout.contains(&format!(
                "WILL CREATE {kind} {name} in package ZABAPTEST (request HD1K917754)"
            )),
            "{kind}: {}",
            done.exit.stdout
        );
    }
}

#[test]
fn a_name_that_exists_is_rejected_by_the_run() {
    let reason = "DTEL ZDTEL already exists";
    let done = Case::create(0, create_reply(rejected(&[], reason), "REJECTED")).go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .ends_with(&format!("\nCREATE: REJECTED\n{reason}")),
        "{}",
        done.exit.stdout
    );
    assert!(!done.exit.stdout.contains("SIMULATED_OK"));
    assert_clean(&done);
}

#[test]
fn a_transparent_name_over_16_characters_is_rejected_by_the_run() {
    let reason = "Transparent table name ZTABLE_NAME_TOO_LONG > 16 characters";
    let done = Case::create(TABL, create_reply(rejected(&[], reason), "REJECTED")).go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .ends_with(&format!("\nCREATE: REJECTED\n{reason}")),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn a_create_simulate_that_left_the_object_behind_is_partial() {
    let (kind, _, name, ..) = FIXTURES[0];
    let mut case = Case::create(
        0,
        create_reply(create_lines(kind, name, "$TMP"), "SIMULATED_OK"),
    );

    case.sap = sap(true, PRESENT);

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit.stdout.contains(
            "\nCREATE: PARTIAL\nthe run reported SIMULATED_OK but a new session disagrees \
             (definition DD04L still has a row (ROLLNAME ZDTEL); TADIR still has a row"
        ),
        "{}",
        done.exit.stdout
    );
    assert!(!done.exit.stdout.contains("CREATE_SIM: SIMULATED_OK"));
}

#[test]
fn a_create_simulate_with_a_lost_reply_is_uncertain_and_nothing_is_reconciled() {
    let done = Case::create(0, Rc::new(|_| PostReply::Lost)).go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stderr
            .contains("the simulate status is uncertain"),
        "{}",
        done.exit.stderr
    );
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(&done);
}

#[test]
fn create_needs_a_file_and_rejects_the_options_of_other_types() {
    let (kind, _, name, ..) = FIXTURES[0];

    for (extra, message) in [
        (
            vec!["--prog", "ZREPORT"],
            "STOP: create TRAN needs --prog <report>; --prog only applies to TRAN",
        ),
        (vec!["--subc", "1"], "STOP: --subc only applies to PROG"),
        (vec!["--fugr", "ZFG"], "STOP: --fugr is only valid for FUNC"),
        (
            vec!["--tr", "HD1K917754"],
            "STOP: --tr is not valid for the local package $TMP",
        ),
        (
            vec!["--package", "ZABAPTEST"],
            "STOP: package ZABAPTEST is transportable: --tr is required",
        ),
    ] {
        let mut case = Case::create(
            0,
            create_reply(create_lines(kind, name, "$TMP"), "SIMULATED_OK"),
        );

        case.extra = extra;

        let exit = case.stops();

        assert_eq!(exit.code, 2, "{message}");
        assert_eq!(exit.stderr, message);
    }
}

#[test]
fn a_text_that_differs_from_the_xml_and_a_file_for_another_object_stop_before_any_connection() {
    let (kind, _, name, ..) = FIXTURES[0];
    let mut case = Case::create(
        0,
        create_reply(create_lines(kind, name, "$TMP"), "SIMULATED_OK"),
    );

    case.extra = vec!["--text", "Another text"];

    assert_eq!(
        case.stops().stderr,
        "STOP: --text differs from the description in the XML file"
    );

    let mut case = Case::create(
        0,
        create_reply(create_lines(kind, name, "$TMP"), "SIMULATED_OK"),
    );

    case.file = case.file.replace("ZDTEL", "ZOTHER");

    assert_eq!(
        case.stops().stderr,
        "STOP: XML file is for ZOTHER, not DTEL ZDTEL"
    );
}

#[test]
fn a_name_outside_the_customer_scope_stops_before_any_connection() {
    let (kind, _, name, ..) = FIXTURES[0];
    let mut case = Case::create(
        0,
        create_reply(create_lines(kind, name, "$TMP"), "SIMULATED_OK"),
    );

    case.name = "MANDT";

    assert_eq!(
        case.stops().stderr,
        "STOP: a created name must start with Z or Y: MANDT"
    );
}

#[test]
fn a_create_simulate_without_a_file_stops_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let args = [
        "abapreach",
        "create",
        "--simulate",
        "DTEL",
        "ZDTEL",
        "--conn",
        "DEV_100",
    ]
    .map(str::to_owned);
    let exit = run(args.to_vec(), env_in(directory.path()), &mut NoTransport);

    assert_eq!(exit.code, 2);
    assert_eq!(exit.stderr, "STOP: create DTEL needs FILE (XML)");
}

#[test]
fn a_file_that_cannot_be_read_stops_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.xml");
    let args: Vec<String> = [
        "abapreach",
        "deploy",
        "--simulate",
        "DTEL",
        "ZDTEL",
        missing.to_str().unwrap(),
        "--conn",
        "DEV_100",
    ]
    .map(str::to_owned)
    .to_vec();
    let exit = run(args, env_in(directory.path()), &mut NoTransport);

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr
            .starts_with(&format!("STOP: cannot read {}", missing.display())),
        "{}",
        exit.stderr
    );
}

/// A dropped reply of the classrun POST (the fault feature) is an uncertain status for a
/// simulate too: nothing is resent and nothing needs settling.
#[cfg(feature = "fault-injection")]
#[test]
fn a_dropped_classrun_reply_is_uncertain_and_the_simulate_is_not_resent() {
    use abapreach::fault::{self, Fault};

    let _scope = fault::scoped(Fault::from_lookup(Some("drop-reply:CLASSRUN")).unwrap());
    let (_, xml, name, ..) = FIXTURES[0];
    let done = Case::deploy(0, deploy_reply(xml, passing(name), "SIMULATED_OK")).go();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit
            .stderr
            .contains("the simulate status is uncertain"),
        "{}",
        done.exit.stderr
    );
    assert!(!done.exit.stdout.lines().any(|line| line == "SIMULATED_OK"));
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(&done);
}

// ---- a release whose ADT session keeps no state -------------------------------------------------------

const STATELESS_STOP: &str = "STOP: ADT HTTP session is stateless on this release; the temporary \
                              runner class needs a stateful session (use the RFC path)";

fn stateless(index: usize) -> Case {
    let (_, xml, name, ..) = FIXTURES[index];
    let mut case = Case::deploy(index, deploy_reply(xml, passing(name), "SIMULATED_OK"));

    case.sap = sap(false, PRESENT);

    case
}

#[test]
fn an_unpinned_stateless_adt_session_takes_the_rfc_path_with_one_note() {
    let done = stateless(0).go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(
        done.exit.stderr,
        "NOTE: ADT HTTP is stateless on this release; the RFC path is used"
    );
    assert!(
        done.exit.stdout.contains(
            "reason: fallback to RFC: capability unavailable: ADT HTTP session is stateless on \
             this release"
        ),
        "{}",
        done.exit.stdout
    );
    runner_line(&done.exit.stdout, "sadt");
    assert!(done.exit.stdout.ends_with("\nSIMULATED_OK"));
    // The probe ran on ADT; the one class and the one POST ran on RFC.
    assert!(done.adt_opens > 0 && done.rfc_opens > 0);
    assert_eq!(done.creates(), 1);
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(&done);
}

#[test]
fn a_pinned_adt_backend_keeps_the_stateless_stop() {
    let mut case = stateless(0);

    case.extra = vec!["--backend", "adt"];

    let done = case.go();

    assert_eq!(done.exit.code, 2);
    assert_eq!(done.exit.stderr, STATELESS_STOP);
    assert_eq!(done.rfc_opens, 0);
    assert_eq!(done.creates(), 0);
}

#[test]
fn a_connection_without_rfc_keeps_a_stop_when_the_adt_session_is_stateless() {
    let mut case = stateless(0);

    case.conn = "ADO_100";

    let done = case.go();

    assert_eq!(done.exit.code, 2);
    assert_eq!(
        done.exit.stderr,
        "STOP: deploy DTEL needs a stateful ADT session or the RFC path"
    );
    assert_eq!(done.rfc_opens, 0);
    assert_eq!(done.creates(), 0);
}
