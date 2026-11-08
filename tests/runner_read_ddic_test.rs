//! `read` of `DTEL`, `DOMA`, `TABL`, and `TTYP` over the temporary runner class (M6 Task 18),
//! driven through the command line against a fake SAP. The fake keeps the runner class state,
//! answers the data previews, and answers the classrun POST with a run bound to the nonce that the
//! generated payload carries, so the product code sees exactly what a real run would send. No SAP.
//!
//! The XML fixtures under `tests/fixtures/m6/` are in the layout that `rfc-deploy read` writes
//! (the envelope of `YCL_UTILS_DDIC_DEPLOY.serialize`: XML header, `abapGit` root, the asXML
//! document indented by one blank per level). They are written to that layout, not recorded from
//! a system; the live byte comparison against the Python tool is Task 30.
//!
//! Ports of the Python `read` cases (`tools/test_rfc_deploy.py`, `rfc_deploy.read_live`) and their
//! intentional differences:
//! - `test_read_subcommand_writes_file`: ported (`the_xml_is_written_...`), the output adds the
//!   `RUNNER:` lines.
//! - `test_read_refuses_overwrite_unless_force`: ported (`an_existing_file_...`).
//! - `read_live` status not ACTIVE is a `Stop` (exit 2): here `NOT_FOUND` and `REJECTED` are exit 1
//!   like every other status of an operation that reached SAP.
//! - `read_live` hash mismatch is a `Stop` (exit 2): here exit 1 and no file, because the client is
//!   not at fault.
//! - The Python tool opens one RFC call and has no `--lines`; here `--lines` is a `STOP`, a
//!   temporary class is created and removed, and the repository preflight runs first.

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
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

const CLASSRUN: &str = "/sap/bc/adt/oo/classrun/";
const PAYLOAD_PREFIX: &str = "    lv_b64 = lv_b64 && '";
const RUNNER_DONE: &str = "(temporary, $TMP, post via";
const WRONG_HASH: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const PREFLIGHT: &str =
    "RUNNER: preflight T000 CCCATEGORY=C CCCORACTIV=1 CCNOCLIIND=- TRNSPACE /0CUST/ EDITFLAG=X";

/// `(token, fixture, name in the fixture, hash of the normalized rows)`; the hash is the SHA-256
/// of the rows joined by line feeds, computed outside the product.
const FIXTURES: [(&str, &str, &str, &str); 4] = [
    (
        "DTEL",
        include_str!("fixtures/m6/read_dtel.xml"),
        "ZDTEL",
        "20039b6579a46ebe9fa56621124d686a39cc568aa00aa1545705cfb6faa2c9b3",
    ),
    (
        "DOMA",
        include_str!("fixtures/m6/read_doma.xml"),
        "ZDOMA",
        "7b09e24bb6adfd98d4edf3bf86de307756b0ac06dfc816c260ae9e9c66f2d259",
    ),
    (
        "TABL",
        include_str!("fixtures/m6/read_tabl.xml"),
        "ZTABL",
        "435a3bfe483aaf819b820f9936d105cd48176f40020ed12222c2fcedd517e5e5",
    ),
    (
        "TTYP",
        include_str!("fixtures/m6/read_ttyp.xml"),
        "ZTTYP",
        "e300360da6106e4e729430f9c6291c52187bf06acaf1c43880986345b6207e3d",
    ),
];

// ---- the runner output ------------------------------------------------------------------------

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

/// A whole run: BEGIN, the body, the status, and the closing count, as the classrun frames it.
fn run_output(nonce: &str, body: &[String], status: &str) -> String {
    let mut lines = vec!["BEGIN:READ".to_owned()];

    lines.extend(body.iter().cloned());
    lines.push(format!("STATUS:{status}"));

    let count = lines.len();

    lines.push(format!("END:{count}"));

    lines
        .iter()
        .map(|rest| format!("ABAPREACH:{nonce}:{rest}\n\n"))
        .collect()
}

fn data(text: &str) -> String {
    format!("LINE:{}", hex(text))
}

fn fact(key: &str, text: &str) -> String {
    format!("FACT:{key}:{}", hex(text))
}

fn message(level: &str, text: &str) -> String {
    format!("MSG:{level}:{}", hex(text))
}

/// The rows the runner sends for a fixture: the normalized rows plus the debris that the client
/// rule removes (a trailing blank on a row, empty rows at the end).
fn rows_of(xml: &str) -> Vec<String> {
    xml.trim_end_matches('\n')
        .split('\n')
        .map(str::to_owned)
        .collect()
}

fn ok_run(nonce: &str, rows: &[String], hash: &str) -> String {
    let mut body: Vec<String> = rows.iter().map(|row| data(row)).collect();

    body.push(fact("HASH", hash));

    run_output(nonce, &body, "OK")
}

fn dirty(rows: &[String]) -> Vec<String> {
    let mut sent = rows.to_vec();

    sent[2].push_str("  ");
    sent.push(String::new());
    sent.push(String::new());

    sent
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

/// The decoded payload document of a generated class source, as `KEY` to `VALUE` (the XML rows
/// are not part of a `READ`).
fn payload_of(source: &str) -> BTreeMap<String, String> {
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

// ---- the fake system ----------------------------------------------------------------------------

type Script = Rc<dyn Fn(&str) -> PostReply>;

/// Wraps a session: when the classrun POST arrives it reads the nonce from the class source that
/// the product wrote and queues the scripted reply for it, then lets the fake SAP answer.
struct Responder {
    inner: Box<dyn SadtTransport>,
    sap: Shared,
    script: Script,
    /// The reply of the `TRNSPACE` preview, when a test wants another namespace state.
    namespace: Option<&'static str>,
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
            let nonce = payload_of(&source)
                .get("NONCE")
                .cloned()
                .unwrap_or_default();
            let reply = (self.script)(&nonce);

            self.sap.borrow_mut().post_replies.push_back(reply);
        }

        if let Some(flag) = self.namespace
            && request.uri.starts_with("/sap/bc/adt/datapreview/ddic")
            && String::from_utf8_lossy(&request.body).contains("FROM TRNSPACE")
        {
            return Ok(Response::text(200, preview(&[("EDITFLAG", &[Some(flag)])])));
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
    namespace: Option<&'static str>,
    /// The ADT connection cannot be opened.
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
            namespace: self.namespace,
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
        panic!("a read probes no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("a read pings nothing")
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
        r#""rfc":{"ashost":"example.com","sysnr":"02"}}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
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

const PRESENT: World = World {
    definition: true,
    directory: true,
    inactive: false,
    broken: false,
};

/// One scripted command: the system, the reply to the POST, and the command line.
struct Case {
    kind: &'static str,
    name: &'static str,
    conn: &'static str,
    extra: Vec<&'static str>,
    sap: Shared,
    script: Script,
    namespace: Option<&'static str>,
    adt_down: bool,
    cancel: CancelFlag,
    /// An existing file the command may not overwrite, when set.
    existing: Option<&'static str>,
}

struct Done {
    exit: Exit,
    sap: Shared,
    directory: tempfile::TempDir,
    out: PathBuf,
    adt_opens: usize,
    rfc_opens: usize,
}

impl Done {
    fn file(&self) -> Option<String> {
        fs::read_to_string(&self.out).ok()
    }

    /// The runner class names the command created.
    fn creates(&self) -> usize {
        self.sap
            .borrow()
            .log
            .iter()
            .filter(|(_, line)| line == "POST /sap/bc/adt/oo/classes")
            .count()
    }
}

impl Case {
    fn new(kind: &'static str, name: &'static str, script: Script) -> Self {
        Self {
            kind,
            name,
            conn: "DEV_100",
            extra: Vec::new(),
            sap: sap(true, PRESENT),
            script,
            namespace: None,
            adt_down: false,
            cancel: CancelFlag::new(),
            existing: None,
        }
    }

    fn go(self) -> Done {
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("out.xml");

        if let Some(text) = self.existing {
            fs::write(&out, text).unwrap();
        }

        let mut args: Vec<String> = ["abapreach", "read", self.kind, self.name]
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();

        args.push(out.display().to_string());
        args.extend(self.extra.iter().map(|argument| (*argument).to_owned()));
        args.extend(["--conn".to_owned(), self.conn.to_owned()]);

        let mut fake = Fake {
            sap: self.sap.clone(),
            script: self.script,
            cancel: self.cancel,
            namespace: self.namespace,
            adt_down: self.adt_down,
            adt_opens: 0,
            rfc_opens: 0,
        };
        let exit = run(args, env_in(directory.path()), &mut fake);

        Done {
            exit,
            sap: self.sap,
            directory,
            out,
            adt_opens: fake.adt_opens,
            rfc_opens: fake.rfc_opens,
        }
    }
}

fn reply(make: impl Fn(&str) -> String + 'static) -> Script {
    Rc::new(move |nonce| PostReply::Output(make(nonce)))
}

/// The scripted reply of a successful run of a fixture.
fn fixture_reply(xml: &'static str, hash: &'static str) -> Script {
    reply(move |nonce| ok_run(nonce, &dirty(&rows_of(xml)), hash))
}

#[track_caller]
fn assert_clean(done: &Done) {
    assert!(
        done.sap.borrow().classes.is_empty(),
        "a runner class was left: {:?}",
        done.sap.borrow().classes.keys().collect::<Vec<_>>()
    );
}

/// The `RUNNER:` line of the output, with the class name cut out and checked.
#[track_caller]
fn runner_line(stdout: &str, post: &str) -> String {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("RUNNER: YCL_ABAPREACH_RUN_"))
        .unwrap_or_else(|| panic!("no RUNNER line in {stdout:?}"));
    let rest = line.strip_prefix("RUNNER: YCL_ABAPREACH_RUN_").unwrap();
    let (suffix, tail) = rest.split_at(8);

    assert!(
        suffix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()),
        "{line}"
    );
    assert_eq!(
        tail,
        format!(" {RUNNER_DONE} {post}, removed after the run)"),
        "{line}"
    );

    format!("YCL_ABAPREACH_RUN_{suffix}")
}

// ---- a successful read ----------------------------------------------------------------------------

#[test]
fn the_xml_is_written_and_printed_for_each_type_on_adt_http() {
    for (kind, xml, name, hash) in FIXTURES {
        let done = Case::new(kind, name, fixture_reply(xml, hash)).go();

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert_eq!(done.exit.stderr, "", "{kind}");
        // The file is the Python layout after the client rule: no blank at the end of a row,
        // no empty row at the end, one line feed per row.
        assert_eq!(done.file().as_deref(), Some(xml), "{kind}");

        let rows = rows_of(xml).len();
        let runner = runner_line(&done.exit.stdout, "http");

        assert_eq!(
            done.exit.stdout,
            format!(
                "RUNNER: {runner} {RUNNER_DONE} http, removed after the run)\n{PREFLIGHT}\n\
                 {rows} lines -> {}\nhash {hash}",
                done.out.display()
            ),
            "{kind}"
        );
        assert_eq!(source_hash(&rows_of(xml)), hash, "{kind}");
        assert_eq!(done.creates(), 1, "{kind}");
        assert_eq!(done.sap.borrow().posts, 1, "{kind}");
        assert_clean(&done);
        assert_eq!((done.adt_opens > 0, done.rfc_opens), (true, 0), "{kind}");
    }
}

#[test]
fn the_xml_is_written_and_printed_for_each_type_on_rfc_sadt() {
    for (kind, xml, name, hash) in FIXTURES {
        let mut case = Case::new(kind, name, fixture_reply(xml, hash));

        case.extra = vec!["--backend", "rfc"];

        let done = case.go();

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert_eq!(done.file().as_deref(), Some(xml), "{kind}");
        runner_line(&done.exit.stdout, "sadt");
        assert!(
            done.exit.stdout.contains(&format!("hash {hash}")),
            "{kind}: {}",
            done.exit.stdout
        );
        assert_clean(&done);
        assert_eq!((done.adt_opens, done.rfc_opens > 0), (0, true), "{kind}");
    }
}

#[test]
fn an_rfc_only_connection_reads_on_rfc_without_a_flag() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.conn = "QAS_100";

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    runner_line(&done.exit.stdout, "sadt");
    assert_eq!((done.adt_opens, done.rfc_opens > 0), (0, true));
}

#[test]
fn a_lower_case_name_is_the_same_object_and_the_payload_is_a_read() {
    let (kind, xml, _, hash) = FIXTURES[0];
    let seen: Rc<RefCell<Vec<BTreeMap<String, String>>>> = Rc::default();
    let sap = sap(true, PRESENT);
    let script: Script = {
        let sap = sap.clone();
        let seen = seen.clone();

        Rc::new(move |nonce| {
            let source = sap.borrow().classes.values().next().cloned().unwrap();

            seen.borrow_mut().push(payload_of(&source));

            PostReply::Output(ok_run(nonce, &rows_of(xml), hash))
        })
    };
    let mut case = Case::new(kind, "zdtel", script);

    case.sap = sap;

    let done = case.go();
    let payloads = seen.borrow();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["MODE"], "READ");
    assert_eq!(payloads[0]["TYPE"], "DTEL");
    assert_eq!(payloads[0]["NAME"], "ZDTEL");
    assert_eq!(payloads[0]["NONCE"].len(), 32);
    // A read carries no value besides the object and the nonce.
    assert!(
        payloads[0]
            .keys()
            .all(|key| ["FORMAT", "NONCE", "MODE", "TYPE", "NAME"].contains(&key.as_str())),
        "{:?}",
        payloads[0]
    );
}

#[test]
fn the_xml_is_data_and_reaches_the_file_only() {
    let (kind, _, name, _) = FIXTURES[0];
    let rows = vec![
        "<?xml version=\"1.0\"?>".to_owned(),
        "<A>ABAPREACH:FAKE:STATUS:OK $(touch x) `id`; rm -rf /</A>".to_owned(),
    ];
    let hash = source_hash(&rows);
    let done = Case::new(
        kind,
        name,
        reply({
            let rows = rows.clone();
            let hash = hash.clone();

            move |nonce| ok_run(nonce, &rows, &hash)
        }),
    )
    .go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(done.file().unwrap(), format!("{}\n{}\n", rows[0], rows[1]));
    // The rows are in the file; neither stream carries any of their text.
    for stream in [&done.exit.stdout, &done.exit.stderr] {
        assert!(
            !stream.contains("touch x") && !stream.contains("FAKE"),
            "{stream}"
        );
    }
}

#[test]
fn xml_with_a_control_character_is_not_written() {
    // The write side refuses such a row, so a file with one could not be deployed again.
    let (kind, _, name, _) = FIXTURES[0];
    let rows = vec![
        "<?xml version=\"1.0\"?>".to_owned(),
        "<A>\u{1b}[31mred</A>".to_owned(),
    ];
    let hash = source_hash(&rows);
    let done = Case::new(kind, name, reply(move |nonce| ok_run(nonce, &rows, &hash))).go();

    assert_eq!(done.exit.code, 1);
    assert!(done.file().is_none());
    assert!(
        done.exit.stderr.contains(
            "ERROR: read DTEL ZDTEL: XML row 2 has a control character; nothing was written"
        ),
        "{}",
        done.exit.stderr
    );
    assert!(!done.exit.stderr.contains('\u{1b}'));
    assert_clean(&done);
}

#[test]
fn a_read_sends_at_most_the_preview_budget() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let done = Case::new(kind, name, fixture_reply(xml, hash)).go();

    // Two for the preflight and three for the evidence.
    assert_eq!(done.sap.borrow().previews, 5);
}

// ---- the file rules ---------------------------------------------------------------------------------

#[test]
fn an_existing_file_is_kept_without_force_and_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.xml");

    fs::write(&out, "keep me").unwrap();

    let args = [
        "abapreach",
        "read",
        "DTEL",
        "ZD",
        out.to_str().unwrap(),
        "--conn",
        "DEV_100",
    ]
    .map(str::to_owned)
    .to_vec();
    let exit = run(args, env_in(directory.path()), &mut NoTransport);

    assert_eq!(exit.code, 2);
    assert_eq!(
        exit.stderr,
        format!(
            "STOP: {} already exists; use --force to overwrite",
            out.display()
        )
    );
    assert_eq!(fs::read_to_string(&out).unwrap(), "keep me");
}

#[test]
fn force_overwrites_an_existing_file() {
    let (kind, xml, name, hash) = FIXTURES[1];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.existing = Some("old content");
    case.extra = vec!["--force"];

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(done.file().as_deref(), Some(xml));
}

#[test]
fn lines_is_a_stop_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let args = [
        "abapreach",
        "read",
        "TABL",
        "T000",
        "out.xml",
        "--lines",
        "1-5",
        "--conn",
        "DEV_100",
    ]
    .map(str::to_owned)
    .to_vec();
    let exit = run(args, env_in(directory.path()), &mut NoTransport);

    assert_eq!(exit.code, 2);
    assert_eq!(exit.stderr, "STOP: --lines does not apply to TABL");
    assert_eq!(exit.stdout, "");
}

// ---- what the run answers instead of the XML --------------------------------------------------------

#[test]
fn not_found_is_exit_1_with_the_sap_message_and_no_file() {
    let done = Case::new(
        "DTEL",
        "ZMISSING",
        reply(|nonce| {
            run_output(
                nonce,
                &[message("E", "DTEL ZMISSING active version not found")],
                "NOT_FOUND",
            )
        }),
    )
    .go();

    assert_eq!(done.exit.code, 1);
    assert!(done.file().is_none());
    assert!(
        done.exit
            .stderr
            .contains("read DTEL ZMISSING: NOT_FOUND (DTEL ZMISSING active version not found)"),
        "{}",
        done.exit.stderr
    );
    assert_clean(&done);
}

#[test]
fn a_serialization_failure_is_rejected_and_never_not_found() {
    // What the template answers when the XML transformation raises: the error message, then
    // REJECTED (task 18 defect: it used to end with NOT_FOUND).
    let done = Case::new(
        "TABL",
        "TSTCT",
        reply(|nonce| {
            run_output(
                nonce,
                &[message(
                    "E",
                    "XML TABL TSTCT failed: Error occurred during serialization",
                )],
                "REJECTED",
            )
        }),
    )
    .go();

    assert_eq!(done.exit.code, 1);
    assert!(done.file().is_none());
    assert!(
        done.exit
            .stderr
            .contains("REJECTED: XML TABL TSTCT failed: Error occurred during serialization"),
        "{}",
        done.exit.stderr
    );
    assert!(
        !done.exit.stderr.contains("NOT_FOUND"),
        "{}",
        done.exit.stderr
    );
    assert_clean(&done);
}

#[test]
fn the_messages_of_a_run_are_printed_with_their_level() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let done = Case::new(
        kind,
        name,
        reply(move |nonce| {
            let mut body = vec![
                message("W", "check this"),
                message("I", "for your information"),
            ];

            body.extend(rows_of(xml).iter().map(|row| data(row)));
            body.push(fact("HASH", hash));

            run_output(nonce, &body, "OK")
        }),
    )
    .go();

    assert_eq!(done.exit.code, 0);
    assert_eq!(done.exit.stderr, "W: check this");
    assert!(
        done.exit.stdout.contains("\nfor your information\n"),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn a_hash_that_differs_from_the_runner_hash_writes_nothing() {
    let (kind, xml, name, _) = FIXTURES[0];
    let done = Case::new(kind, name, fixture_reply(xml, WRONG_HASH)).go();

    assert_eq!(done.exit.code, 1);
    assert!(done.file().is_none());
    assert!(
        done.exit.stderr.contains(
            "ERROR: read DTEL ZDTEL: the client hash differs from the runner hash \
             (normalization rules out of sync)"
        ),
        "{}",
        done.exit.stderr
    );
    assert_clean(&done);
}

#[test]
fn a_run_without_rows_or_without_a_hash_writes_nothing() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let without_hash = Case::new(
        kind,
        name,
        reply(move |nonce| {
            let body: Vec<String> = rows_of(xml).iter().map(|row| data(row)).collect();

            run_output(nonce, &body, "OK")
        }),
    )
    .go();
    let without_rows = Case::new(
        kind,
        name,
        reply(move |nonce| run_output(nonce, &[fact("HASH", hash)], "OK")),
    )
    .go();

    for done in [without_hash, without_rows] {
        assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
        assert!(done.file().is_none());
        assert_clean(&done);
    }
}

// ---- uncertain status -------------------------------------------------------------------------------

#[test]
fn a_lost_reply_a_5xx_and_a_broken_run_are_uncertain_and_never_resent() {
    let scripts: [(&str, Script); 3] = [
        ("lost reply", Rc::new(|_| PostReply::Lost)),
        (
            "HTTP 500",
            Rc::new(|nonce| PostReply::Status(500, ok_run(nonce, &[], "x"))),
        ),
        (
            "truncated",
            reply(|nonce| format!("ABAPREACH:{nonce}:BEGIN:READ\n\n")),
        ),
    ];

    for (label, script) in scripts {
        let done = Case::new("DTEL", "ZDTEL", script).go();

        assert_eq!(done.exit.code, 1, "{label}: {}", done.exit.stderr);
        assert!(done.file().is_none(), "{label}");
        assert!(
            done.exit
                .stderr
                .contains("ERROR: read DTEL ZDTEL: the runner status is uncertain ("),
            "{label}: {}",
            done.exit.stderr
        );
        assert!(
            done.exit
                .stderr
                .contains("nothing was written; run the read again"),
            "{label}: {}",
            done.exit.stderr
        );
        // A read has nothing to reconcile and is never resent.
        assert_eq!(done.sap.borrow().posts, 1, "{label}");
        assert_eq!(done.creates(), 1, "{label}");
        assert_clean(&done);
    }
}

#[test]
fn a_refused_post_is_rejected_and_a_missing_handler_is_a_stop() {
    let refused = Case::new(
        "DTEL",
        "ZDTEL",
        Rc::new(|_| PostReply::Status(403, String::new())),
    )
    .go();
    let missing = Case::new(
        "DTEL",
        "ZDTEL",
        Rc::new(|_| PostReply::Status(404, String::new())),
    )
    .go();

    assert_eq!(refused.exit.code, 1);
    assert!(
        refused.exit.stderr.contains(
            "REJECTED: runner POST failed: HTTP 403; the temporary runner class needs CLAS \
             create/activate in $TMP and ADT classrun access"
        ),
        "{}",
        refused.exit.stderr
    );
    assert_eq!(missing.exit.code, 2);
    assert_eq!(
        missing.exit.stderr,
        "STOP: read DTEL needs ADT classrun; not available on DEV"
    );

    for done in [refused, missing] {
        assert!(done.file().is_none());
        assert_clean(&done);
    }
}

// ---- evidence and preflight -------------------------------------------------------------------------

#[test]
fn a_definition_that_the_new_session_does_not_see_is_partial() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.sap = sap(
        true,
        World {
            definition: false,
            ..PRESENT
        },
    );

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(done.file().is_none());
    assert!(
        done.exit.stderr.contains(
            "PARTIAL: read DTEL ZDTEL: the runner read the definition but a new session does \
             not confirm it (definition DD04L has no row); nothing was written"
        ),
        "{}",
        done.exit.stderr
    );
    assert_clean(&done);
}

#[test]
fn evidence_that_cannot_be_read_is_a_note_and_the_read_stands() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.sap = sap(
        true,
        World {
            broken: true,
            ..PRESENT
        },
    );

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(done.file().as_deref(), Some(xml));
    assert!(
        done.exit
            .stderr
            .contains("W: the new session could not confirm the definition ("),
        "{}",
        done.exit.stderr
    );
}

#[test]
fn a_repository_that_forbids_a_class_stops_before_any_class_exists() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.namespace = Some("N");

    let done = case.go();

    assert_eq!(done.exit.code, 2);
    assert_eq!(
        done.exit.stderr,
        "STOP: read DTEL needs a temporary class; repository changes are not allowed on \
         DEV/100 (customer namespace /0CUST/ is not modifiable; TRNSPACE EDITFLAG N)"
    );
    assert_eq!(done.creates(), 0);
    assert_eq!(done.sap.borrow().posts, 0);
    assert!(done.file().is_none());
    assert!(
        done.exit
            .stdout
            .starts_with("RUNNER: preflight T000 CCCATEGORY=C"),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn an_adt_session_without_state_stops_with_the_one_text_when_adt_is_pinned() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.sap = sap(false, PRESENT);
    case.extra = vec!["--backend", "adt"];

    let done = case.go();

    assert_eq!(done.exit.code, 2);
    assert_eq!(
        done.exit.stderr,
        "STOP: ADT HTTP session is stateless on this release; the temporary runner class needs \
         a stateful session (use the RFC path)"
    );
    assert_eq!(done.creates(), 0);
}

#[test]
fn an_adt_session_without_state_reads_over_rfc_when_nothing_is_pinned() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.sap = sap(false, PRESENT);

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(done.file().as_deref(), Some(xml));
    runner_line(&done.exit.stdout, "sadt");
    assert_eq!(
        done.exit.stderr,
        "NOTE: ADT HTTP is stateless on this release; the RFC path is used"
    );
    assert_clean(&done);
}

// ---- cleanup and signals ----------------------------------------------------------------------------

#[test]
fn a_class_that_stays_is_exit_1_although_the_read_succeeded() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let case = Case::new(kind, name, fixture_reply(xml, hash));

    case.sap.borrow_mut().failing_deletes = 100;

    let done = case.go();
    let left: Vec<String> = done.sap.borrow().classes.keys().cloned().collect();

    assert_eq!(left.len(), 1);
    assert_eq!(done.exit.code, 1);
    // The read itself stands: the file is there and the output is the read output.
    assert_eq!(done.file().as_deref(), Some(xml));
    assert!(
        done.exit
            .stdout
            .contains(&format!("\n{} lines -> ", rows_of(xml).len())),
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
fn ctrl_c_before_the_run_deletes_nothing_and_exits_130() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let case = Case::new(kind, name, fixture_reply(xml, hash));

    case.cancel.set();

    let done = case.go();

    assert_eq!(done.exit.code, 130);
    assert_eq!(done.creates(), 0);
    assert!(done.file().is_none());
    assert_eq!(done.adt_opens + done.rfc_opens, 0);
    assert!(
        done.exit
            .stderr
            .contains("CANCELED: interrupted before the classrun POST; nothing ran"),
        "{}",
        done.exit.stderr
    );
}

#[test]
fn ctrl_c_after_the_post_exits_130_and_removes_the_class() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let case = Case::new(kind, name, fixture_reply(xml, hash));
    let cancel = case.cancel.clone();

    case.sap.borrow_mut().on_post = Some(Box::new(move || cancel.set()));

    let done = case.go();

    assert_eq!(done.exit.code, 130);
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(&done);
}

// ---- routing ----------------------------------------------------------------------------------------

#[test]
fn a_connection_that_cannot_be_opened_falls_back_to_rfc_before_anything_is_written() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.adt_down = true;

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(done.file().as_deref(), Some(xml));
    runner_line(&done.exit.stdout, "sadt");
    assert!(
        done.exit.stderr.contains(
            "NOTE: ADT HTTP failed before anything was written; falling back to RFC SADT"
        ),
        "{}",
        done.exit.stderr
    );
}

#[test]
fn a_pinned_backend_does_not_fall_back() {
    let (kind, xml, name, hash) = FIXTURES[0];
    let mut case = Case::new(kind, name, fixture_reply(xml, hash));

    case.adt_down = true;
    case.extra = vec!["--backend", "adt"];

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert_eq!(done.rfc_opens, 0);
    assert!(done.exit.stderr.contains("ERROR: "), "{}", done.exit.stderr);
    assert!(done.file().is_none());
}

#[test]
fn the_directory_of_the_output_is_not_touched_by_a_failed_read() {
    let done = Case::new("DTEL", "ZMISSING", Rc::new(|_| PostReply::Lost)).go();
    let entries: Vec<_> = fs::read_dir(done.directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();

    assert_eq!(entries, ["connections.json"]);
}
