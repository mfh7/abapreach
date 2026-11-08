//! `deploy` of `DTEL`, `DOMA`, `TABL`, and `TTYP` as two runs of the temporary runner class (M6
//! Task 20), driven through the command line against a fake SAP. Run 1 (`SIMULATE`) returns the
//! live rows, the live hash, and the verdict; the CLI prints the diff and the live hash and
//! compares `--base`; run 2 (`DEPLOY`) carries the live hash as the base and writes. The fake keeps
//! the runner class state, answers the data previews, and answers each classrun POST with a run
//! bound to the nonce that the generated payload carries. No SAP.
//!
//! Ports of the Python deploy cases (`tools/test_rfc_deploy.py`) and their intentional
//! differences:
//! - `test_ddic_deploy_warns_when_active_differs_from_file` and
//!   `test_ddic_deploy_no_warning_when_active_equals_file`: ported
//!   (`an_active_definition_that_differs_from_the_file_warns_on_stderr`,
//!   `an_active_definition_equal_to_the_file_has_no_warning`). The warning is a `W:` line on the
//!   stderr; Python prints it on the stdout.
//! - Python prints `DEPLOY: ACTIVE hash <h>`; here the status is the bare token of the M1 deploy
//!   (`ACTIVE`), and each of the two runs prints its own `RUNNER:` line because each creates and
//!   deletes a temporary class in `$TMP` (two runs mean two classes; intentional difference).
//! - A `--base` that differs from the live hash is a STOP with exit 2 after the first run, as in
//!   M1 (Python: `Stop`).

mod common;

use abapreach::backend::BackendKind;
use abapreach::config::Connection;
use abapreach::hash::{normalize, source_hash};
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::deploy_ddic::{DdicDeploy, deploy_with};
use abapreach::operation::payload::Kind;
use abapreach::operation::runner_reconcile::Clock;
use abapreach::operation::runner_route::random_nonce;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
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
const BASE_CHANGED: &str = "Base changed since the diff was shown; run deploy again";

const DEPLOY_GUARDS: [&str; 5] = [
    "Customer names only (Z or Y)",
    "System change option, client role, and customer namespace",
    "Request owner and state, and the package of the object",
    "Inactive version of anyone, and the base hash",
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

fn warning(text: &str) -> String {
    format!("MSG:W:{}", hex(text))
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

/// The hash of a file as the product computes it: the rows after the client rule.
fn file_hash(text: &str) -> String {
    source_hash(&normalize(rows_of(text)))
}

/// What a `SIMULATE` run answers: the live rows (with the debris that the client rule removes),
/// the live hash, the lines of `extra`, and the status.
fn simulate_run(nonce: &str, xml: &str, extra: &[String], status: &str) -> String {
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

/// The lines of a passing change on an empty table: the live verdict of run 12A.
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

/// What a `DEPLOY` run answers: the live hash it re-read, the package, the lines of `extra`, and
/// the status. `result` is the hash of the definition after the write.
fn deploy_run(
    nonce: &str,
    live: &str,
    package: &str,
    result: Option<&str>,
    extra: &[String],
    status: &str,
) -> String {
    let mut body = vec![fact("HASH", live), fact("PACKAGE", package)];

    body.extend(extra.iter().cloned());

    if let Some(result) = result {
        body.push(fact("RESULT_HASH", result));
    }

    run_output(nonce, "DEPLOY", &body, status)
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

/// The reply of a classrun POST, from the payload it runs.
type Script = Rc<dyn Fn(&Payload) -> PostReply>;

/// One reply per mode of the two-run deploy.
struct Replies {
    simulate: Rc<dyn Fn(&str) -> PostReply>,
    deploy: Rc<dyn Fn(&str) -> PostReply>,
    read: Rc<dyn Fn(&str) -> PostReply>,
}

impl Replies {
    fn script(self) -> Script {
        Rc::new(move |payload| {
            let nonce = value(payload, "NONCE").unwrap_or_default();

            match value(payload, "MODE").as_deref() {
                Some("SIMULATE") => (self.simulate)(&nonce),
                Some("DEPLOY") => (self.deploy)(&nonce),
                Some("READ") => (self.read)(&nonce),
                other => panic!("unexpected mode {other:?}"),
            }
        })
    }
}

/// Wraps a session: when the classrun POST arrives it reads the payload from the class source that
/// the product wrote and queues the scripted reply for it, then lets the fake SAP answer. The
/// package of the TADIR row is rewritten to `package`, because the fake answers `$TMP`.
struct Responder {
    inner: Box<dyn SadtTransport>,
    sap: Shared,
    script: Script,
    /// The payloads of the classes that were posted, in order.
    seen: Seen,
    package: &'static str,
    /// From this classrun POST on (1-based), every delete of a class fails.
    fail_deletes_at_post: Option<usize>,
}

impl SadtTransport for Responder {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        let tadir = request.uri.starts_with("/sap/bc/adt/datapreview/ddic")
            && String::from_utf8_lossy(&request.body).contains(" FROM TADIR");

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
            let reply = (self.script)(&payload);

            self.seen.borrow_mut().push(payload);
            self.sap.borrow_mut().post_replies.push_back(reply);

            if self.fail_deletes_at_post == Some(self.seen.borrow().len()) {
                self.sap.borrow_mut().failing_deletes = 100;
            }
        }

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

struct Fake {
    sap: Shared,
    script: Script,
    cancel: CancelFlag,
    seen: Seen,
    /// From this ADT open on (1-based), every open fails with a connectivity error.
    adt_down_from: Option<usize>,
    adt_opens: usize,
    rfc_opens: usize,
    package: &'static str,
    fail_deletes_at_post: Option<usize>,
}

impl Fake {
    fn wrap(&self, inner: Opened) -> Opened {
        Ok(Box::new(Responder {
            inner: inner?,
            sap: self.sap.clone(),
            script: self.script.clone(),
            seen: self.seen.clone(),
            package: self.package,
            fail_deletes_at_post: self.fail_deletes_at_post,
        }))
    }
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opens += 1;

        if self
            .adt_down_from
            .is_some_and(|from| self.adt_opens >= from)
        {
            return Err(TransportError::PreSendConnectivity("no route".to_owned()));
        }

        self.wrap(http_session(&self.sap))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opens += 1;

        self.wrap(rfc_session(&self.sap))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("a deploy probes no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("a deploy pings nothing")
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

/// A clock that moves only when the reconciliation waits.
struct FakeClock {
    now: Rc<Cell<Duration>>,
}

impl Clock for FakeClock {
    fn wait(&mut self, duration: Duration, abort: &dyn Fn() -> bool) -> bool {
        if abort() {
            return false;
        }

        self.now.set(self.now.get() + duration);

        true
    }

    fn elapsed(&self) -> Duration {
        self.now.get()
    }
}

fn env_in(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

/// The world of the previews: the object as it is before the deploy run, and `after` once the
/// second POST (the `DEPLOY` run) has run.
fn sap(after: World) -> Shared {
    new_sap(
        true,
        &Rc::new(Cell::new(Duration::ZERO)),
        Box::new(move |tick| if tick.posts >= 2 { after } else { PRESENT }),
    )
}

/// The object with its TADIR row and no inactive version.
const PRESENT: World = World {
    definition: true,
    directory: true,
    inactive: false,
    broken: false,
};

/// One scripted command: the system, the replies to the POSTs, and the file.
struct Case {
    kind: &'static str,
    name: &'static str,
    extra: Vec<&'static str>,
    sap: Shared,
    script: Script,
    adt_down_from: Option<usize>,
    cancel: CancelFlag,
    package: &'static str,
    fail_deletes_at_post: Option<usize>,
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

    /// The payload of the `index`-th classrun POST.
    fn payload(&self, index: usize) -> Payload {
        self.seen.borrow()[index].clone()
    }

    fn modes(&self) -> Vec<String> {
        self.seen
            .borrow()
            .iter()
            .map(|payload| value(payload, "MODE").unwrap_or_default())
            .collect()
    }
}

impl Case {
    /// The deploy of fixture `index` with its changed text. The live definition is the fixture.
    fn deploy(index: usize, replies: Replies) -> Self {
        let (kind, xml, name, text, changed) = FIXTURES[index];

        Self {
            kind,
            name,
            extra: Vec::new(),
            sap: sap(PRESENT),
            script: replies.script(),
            adt_down_from: None,
            cancel: CancelFlag::new(),
            package: "$TMP",
            fail_deletes_at_post: None,
            file: xml.replace(text, changed),
        }
    }

    fn args(&self, file: &Path) -> Vec<String> {
        let mut args: Vec<String> = ["abapreach", "deploy", self.kind, self.name]
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();

        args.push(file.display().to_string());
        args.extend(self.extra.iter().map(|argument| (*argument).to_owned()));
        args.extend(["--conn".to_owned(), "DEV_100".to_owned()]);

        args
    }

    fn fake(&self, seen: &Seen) -> Fake {
        Fake {
            sap: self.sap.clone(),
            script: self.script.clone(),
            cancel: self.cancel.clone(),
            seen: seen.clone(),
            adt_down_from: self.adt_down_from,
            adt_opens: 0,
            rfc_opens: 0,
            package: self.package,
            fail_deletes_at_post: self.fail_deletes_at_post,
        }
    }

    fn go(self) -> Done {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("in.xml");

        fs::write(&file, &self.file).unwrap();

        let args = self.args(&file);
        let seen: Seen = Rc::default();
        let mut fake = self.fake(&seen);
        let exit = run(args, env_in(directory.path()), &mut fake);

        Done {
            exit,
            sap: self.sap,
            seen,
            adt_opens: fake.adt_opens,
            rfc_opens: fake.rfc_opens,
        }
    }

    /// Runs the deploy through the library entry with a virtual clock, so that a reconciliation
    /// does not wait.
    fn go_virtual(self, base: Option<&str>, tr: Option<&str>) -> Done {
        let seen: Seen = Rc::default();
        let mut fake = self.fake(&seen);
        let request = DdicDeploy {
            kind: match self.kind {
                "DTEL" => Kind::Dtel,
                "DOMA" => Kind::Doma,
                "TABL" => Kind::Tabl,
                _ => Kind::Ttyp,
            },
            name: self.name,
            xml: &self.file,
            tr,
            base,
        };
        let mut clock = FakeClock {
            now: Rc::new(Cell::new(Duration::ZERO)),
        };
        let exit = deploy_with(
            &Connection::test_insecure(),
            Some(BackendKind::AdtHttp),
            &request,
            &mut fake,
            &mut random_nonce,
            &mut clock,
        );

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

fn reply(make: impl Fn(&str) -> String + 'static) -> Rc<dyn Fn(&str) -> PostReply> {
    Rc::new(move |nonce| PostReply::Output(make(nonce)))
}

/// A `READ` run of the extra reconciliation: found with the hash, or not found.
fn read_reply(found: Option<String>) -> Rc<dyn Fn(&str) -> PostReply> {
    reply(move |nonce| match &found {
        Some(hash) => run_output(nonce, "READ", &[fact("HASH", hash)], "OK"),
        None => run_output(nonce, "READ", &[], "NOT_FOUND"),
    })
}

/// A passing run 1 against the live definition `xml`, and the given reply to run 2.
fn replies(
    xml: &'static str,
    name: &'static str,
    deploy: Rc<dyn Fn(&str) -> PostReply>,
) -> Replies {
    Replies {
        simulate: reply(move |nonce| simulate_run(nonce, xml, &passing(name), "SIMULATED_OK")),
        deploy,
        read: read_reply(None),
    }
}

/// The `DEPLOY` run that wrote the file and proved it: the result hash is the file hash.
fn written(file: String, package: &'static str, live: String) -> Rc<dyn Fn(&str) -> PostReply> {
    reply(move |nonce| {
        deploy_run(
            nonce,
            &live,
            package,
            Some(&file_hash(&file)),
            &[],
            "ACTIVE",
        )
    })
}

/// A case whose both runs pass: fixture `index`, live definition the fixture, result the file.
fn passing_case(index: usize) -> Case {
    let (_, xml, name, text, changed) = FIXTURES[index];
    let file = xml.replace(text, changed);

    Case::deploy(
        index,
        replies(xml, name, written(file, "$TMP", live_hash(xml))),
    )
}

#[track_caller]
fn assert_clean(done: &Done) {
    assert!(
        done.sap.borrow().classes.is_empty(),
        "a runner class was left: {:?}",
        done.sap.borrow().classes.keys().collect::<Vec<_>>()
    );
}

/// The `RUNNER:` lines of the output as `(class name, post transport)`.
fn runner_lines(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix("RUNNER: YCL_ABAPREACH_RUN_"))
        .map(|rest| {
            let (suffix, tail) = rest.split_at(8);
            let post = tail
                .strip_prefix(" (temporary, $TMP, post via ")
                .and_then(|tail| tail.strip_suffix(", removed after the run)"))
                .unwrap_or_else(|| panic!("a RUNNER line with a strange tail: {rest}"));

            (format!("YCL_ABAPREACH_RUN_{suffix}"), post.to_owned())
        })
        .collect()
}

fn lines_of(stdout: &str) -> Vec<&str> {
    stdout.lines().collect()
}

// ---- the two runs and the diff before the write ---------------------------------------------------------

#[test]
fn a_deploy_prints_the_diff_and_the_live_hash_then_writes_in_a_second_run() {
    let (kind, xml, _, text, changed) = FIXTURES[0];
    let done = passing_case(0).go();
    let runners = runner_lines(&done.exit.stdout);
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
            format!(
                "RUNNER: {} (temporary, $TMP, post via http, removed after the run)",
                runners[0].0
            ),
            PREFLIGHT.to_owned(),
        ]
    );
    assert!(lines[diff_at..hash_at].contains(&format!("-    <DDTEXT>{text}</DDTEXT>").as_str()));
    assert!(lines[diff_at..hash_at].contains(&format!("+    <DDTEXT>{changed}</DDTEXT>").as_str()));
    assert_eq!(
        lines[hash_at + 1..],
        [
            "CONVERSION ZDTEL: action=ALT modeflag=F daction=ALT allowed=X rows=0".to_owned(),
            "WHERE-USED: none".to_owned(),
            "GUARD Customer names only (Z or Y)".to_owned(),
            "GUARD System change option, client role, and customer namespace".to_owned(),
            "GUARD Request owner and state, and the package of the object".to_owned(),
            "GUARD Inactive version of anyone, and the base hash".to_owned(),
            "GUARD Conversion verdict of DD_DECIDE, also for dependent tables".to_owned(),
            format!(
                "RUNNER: {} (temporary, $TMP, post via http, removed after the run)",
                runners[1].0
            ),
            "ACTIVE".to_owned(),
        ]
    );
    // Two runs mean two temporary classes with two names, both removed.
    assert_eq!(runners.len(), 2);
    assert_ne!(runners[0].0, runners[1].0);
    assert_eq!(done.modes(), ["SIMULATE", "DEPLOY"]);
    assert_eq!(done.creates(), 2);
    assert_clean(&done);
}

#[test]
fn run_2_carries_the_live_hash_as_the_base_when_no_base_was_given() {
    for (index, (kind, xml, ..)) in FIXTURES.iter().enumerate() {
        let case = passing_case(index);
        let file = case.file.clone();
        let done = case.go();
        let first = done.payload(0);
        let second = done.payload(1);

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert_eq!(value(&first, "MODE").as_deref(), Some("SIMULATE"));
        // Run 1 compares `--base` itself: it never sends one.
        assert_eq!(value(&first, "BASE"), None, "{kind}");
        assert_eq!(value(&second, "MODE").as_deref(), Some("DEPLOY"));
        assert_eq!(value(&second, "TYPE").as_deref(), Some(*kind));
        assert_eq!(value(&second, "BASE"), Some(live_hash(xml)), "{kind}");
        assert_eq!(xml_of(&second), rows_of(&file), "{kind}");
        assert_eq!(xml_of(&first), rows_of(&file), "{kind}");
        assert_ne!(value(&first, "NONCE"), value(&second, "NONCE"));
    }
}

#[test]
fn a_base_that_equals_the_live_hash_is_carried_to_run_2() {
    let (_, xml, ..) = FIXTURES[0];
    let hash: &'static str = Box::leak(live_hash(xml).into_boxed_str());
    let mut case = passing_case(0);

    case.extra = vec!["--base", hash];

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(value(&done.payload(1), "BASE").as_deref(), Some(hash));
    assert!(done.exit.stdout.ends_with("\nACTIVE"));
}

#[test]
fn the_four_types_deploy_on_adt_http_and_on_rfc_sadt() {
    for index in 0..FIXTURES.len() {
        for backend in [None, Some("rfc")] {
            let mut case = passing_case(index);
            let kind = case.kind;

            if let Some(backend) = backend {
                case.extra = vec!["--backend", backend];
            }

            let done = case.go();
            let post = if backend.is_some() { "sadt" } else { "http" };

            assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
            assert!(done.exit.stdout.ends_with("\nACTIVE"), "{kind}");
            assert!(
                runner_lines(&done.exit.stdout)
                    .iter()
                    .all(|(_, via)| via == post),
                "{kind}"
            );
            assert_eq!(done.creates(), 2, "{kind}");
            assert_eq!(done.sap.borrow().posts, 2, "{kind}");
            assert_clean(&done);
            assert_eq!(
                (done.adt_opens > 0, done.rfc_opens > 0),
                (backend.is_none(), backend.is_some()),
                "{kind}"
            );
        }
    }
}

// ---- --base and no changes: one run only --------------------------------------------------------------

#[test]
fn a_base_that_differs_from_the_live_hash_is_a_stop_after_the_first_run() {
    let (_, xml, ..) = FIXTURES[0];
    let mut case = passing_case(0);

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
    assert!(!done.exit.stdout.contains("--- live"));
    assert!(!done.exit.stdout.contains("ACTIVE"));
    assert_eq!(done.modes(), ["SIMULATE"]);
    assert_eq!(done.creates(), 1);
    assert_clean(&done);
}

#[test]
fn a_file_equal_to_the_live_definition_is_no_changes_with_one_run_and_no_diff() {
    for (index, (kind, xml, name, ..)) in FIXTURES.iter().enumerate() {
        let extra = vec![info("No change: file equals active definition")];
        let mut case = Case::deploy(
            index,
            Replies {
                simulate: reply(move |nonce| simulate_run(nonce, xml, &extra, "NO_CHANGES")),
                deploy: reply(|_| panic!("no changes needs no second run")),
                read: read_reply(None),
            },
        );

        case.file = (*xml).to_owned();

        let done = case.go();

        assert_eq!(done.exit.code, 0, "{kind} {name}: {}", done.exit.stderr);
        assert!(done.exit.stdout.ends_with("\nno changes"), "{kind}");
        assert!(!done.exit.stdout.contains("--- live"), "{kind}");
        assert!(!done.exit.stdout.contains("ACTIVE"), "{kind}");
        assert_eq!(done.modes(), ["SIMULATE"], "{kind}");
        assert_eq!(done.creates(), 1, "{kind}");
        assert_clean(&done);
    }
}

#[test]
fn a_base_that_is_no_hash_and_a_file_for_another_object_stop_before_any_connection() {
    let mut case = passing_case(0);

    case.extra = vec!["--base", "not-a-hash"];

    let exit = case.stops();

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr
            .starts_with("STOP: --base must be 64 hex digits"),
        "{}",
        exit.stderr
    );

    let mut case = passing_case(0);

    case.file = case.file.replace("ZDTEL", "ZOTHER");

    let exit = case.stops();

    assert_eq!(exit.code, 2);
    assert_eq!(exit.stderr, "STOP: XML file is for ZOTHER, not DTEL ZDTEL");
}

#[test]
fn a_transaction_and_a_foreign_name_stop_before_any_connection() {
    let mut case = passing_case(0);

    case.kind = "TRAN";

    assert_eq!(
        case.stops().stderr,
        "STOP: TRAN has no deploy; use create or delete"
    );

    let mut case = passing_case(0);

    case.name = "MANDT";

    assert_eq!(
        case.stops().stderr,
        "STOP: write and delete accept only Z and Y names: MANDT"
    );
}

// ---- the verdict, in run 1 and again in run 2 ----------------------------------------------------------

#[test]
fn a_rejection_of_run_1_ends_the_command_with_one_run_and_no_write() {
    let (_, xml, name, ..) = FIXTURES[TABL];
    let reason = "Table ZTABL (3 rows) needs SE14 conversion; change rejected";
    let extra = rejected(
        &[info(
            "CONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed= rows=3",
        )],
        reason,
    );
    let case = Case::deploy(
        TABL,
        Replies {
            simulate: reply(move |nonce| simulate_run(nonce, xml, &extra, "REJECTED")),
            deploy: reply(|_| panic!("a rejected verdict needs no second run")),
            read: read_reply(None),
        },
    );
    let done = case.go();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit.stdout.ends_with(&format!("\nREJECTED: {reason}")),
        "{}",
        done.exit.stdout
    );
    assert!(done.exit.stdout.contains("\nCONVERSION ZTABL: action=CNV"));
    assert_eq!(done.modes(), ["SIMULATE"]);
    assert_eq!(done.creates(), 1);
    assert_clean(&done);
    assert_eq!(name, "ZTABL");
}

#[test]
fn a_simulated_ok_without_a_verdict_for_a_transparent_table_is_rejected_before_run_2() {
    let (_, xml, ..) = FIXTURES[TABL];
    let case = Case::deploy(
        TABL,
        Replies {
            simulate: reply(move |nonce| {
                simulate_run(nonce, xml, &guards(&DEPLOY_GUARDS), "SIMULATED_OK")
            }),
            deploy: reply(|_| panic!("a missing verdict needs no second run")),
            read: read_reply(None),
        },
    );
    let done = case.go();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit.stdout.ends_with(
            "\nREJECTED: the run reported SIMULATED_OK without a conversion verdict for table \
             ZTABL; the change is rejected"
        ),
        "{}",
        done.exit.stdout
    );
    assert_eq!(done.modes(), ["SIMULATE"]);
}

#[test]
fn the_verdict_runs_again_in_run_2_and_its_rejection_is_shown() {
    let (_, xml, name, text, changed) = FIXTURES[TABL];
    let file = xml.replace(text, changed);
    let reason = "Table ZTABL (5 rows) needs SE14 conversion; change rejected";
    let extra = rejected(
        &[info(
            "CONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed= rows=5",
        )],
        reason,
    );
    let live = live_hash(xml);
    let done = Case::deploy(
        TABL,
        replies(
            xml,
            name,
            reply(move |nonce| deploy_run(nonce, &live, "$TMP", None, &extra, "REJECTED")),
        ),
    )
    .go();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit.stdout.ends_with(&format!(
            "\nCONVERSION ZTABL: action=CNV modeflag=U daction=CNV allowed= rows=5\n\
             REJECTED: {reason}"
        )),
        "{}",
        done.exit.stdout
    );
    assert!(!done.exit.stdout.contains("ACTIVE"));
    assert_eq!(done.modes(), ["SIMULATE", "DEPLOY"]);
    // Run 2 got the file again, so that the class can run the verdict on it.
    assert_eq!(xml_of(&done.payload(1)), rows_of(&file));
    assert_clean(&done);
}

#[test]
fn a_base_that_changed_between_the_runs_is_a_stop_with_exit_2() {
    let (_, xml, name, ..) = FIXTURES[0];
    let live = live_hash(xml);
    let done = Case::deploy(
        0,
        replies(
            xml,
            name,
            reply(move |nonce| {
                deploy_run(
                    nonce,
                    &live,
                    "$TMP",
                    None,
                    &[error(BASE_CHANGED)],
                    "REJECTED",
                )
            }),
        ),
    )
    .go();

    assert_eq!(done.exit.code, 2, "{}", done.exit.stdout);
    assert!(
        done.exit.stderr.contains(&format!("STOP: {BASE_CHANGED}")),
        "{}",
        done.exit.stderr
    );
    assert!(!done.exit.stdout.contains("ACTIVE"));
    assert!(!done.exit.stdout.contains("REJECTED"));
    assert_eq!(done.modes(), ["SIMULATE", "DEPLOY"]);
    assert_clean(&done);
}

// ---- the statuses of run 2 ---------------------------------------------------------------------------

#[test]
fn an_activation_failure_is_inactive_without_a_rollback() {
    let (_, xml, name, ..) = FIXTURES[0];
    let live = live_hash(xml);
    let done = Case::deploy(
        0,
        replies(
            xml,
            name,
            reply(move |nonce| {
                deploy_run(
                    nonce,
                    &live,
                    "$TMP",
                    None,
                    &[error("Activation of DTEL ZDTEL failed")],
                    "INACTIVE",
                )
            }),
        ),
    )
    .go();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit.stdout.ends_with(
            "\nINACTIVE: Activation of DTEL ZDTEL failed; the inactive version stays, deploy \
             again from the read file"
        ),
        "{}",
        done.exit.stdout
    );
    assert!(!done.exit.stdout.contains("ACTIVE\n"));
    assert_eq!(done.modes(), ["SIMULATE", "DEPLOY"]);
    assert_clean(&done);
}

#[test]
fn a_write_that_was_refused_is_rejected_and_a_partial_run_is_partial() {
    let (_, xml, name, ..) = FIXTURES[0];

    for (status, reason, line) in [
        ("REJECTED", "Request HD1K900001 is released", "REJECTED: "),
        ("PARTIAL", "The lock was not released", "PARTIAL: "),
    ] {
        let live = live_hash(xml);
        let done = Case::deploy(
            0,
            replies(
                xml,
                name,
                reply(move |nonce| {
                    deploy_run(nonce, &live, "$TMP", None, &[error(reason)], status)
                }),
            ),
        )
        .go();

        assert_eq!(done.exit.code, 1, "{status}: {}", done.exit.stderr);
        assert!(
            done.exit.stdout.ends_with(&format!("\n{line}{reason}")),
            "{status}: {}",
            done.exit.stdout
        );
        assert_clean(&done);
    }
}

#[test]
fn a_run_that_reports_active_without_a_result_hash_is_partial() {
    let (_, xml, name, ..) = FIXTURES[0];
    let live = live_hash(xml);
    let done = Case::deploy(
        0,
        replies(
            xml,
            name,
            reply(move |nonce| deploy_run(nonce, &live, "$TMP", None, &[], "ACTIVE")),
        ),
    )
    .go();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit
            .stdout
            .ends_with("\nPARTIAL: the run reported ACTIVE without a result hash; check with read before retrying"),
        "{}",
        done.exit.stdout
    );
}

// ---- $TMP, transportable objects, and --tr --------------------------------------------------------

#[test]
fn a_local_object_deploys_without_a_request_and_keeps_its_package() {
    let done = passing_case(0).go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(value(&done.payload(0), "TR"), None);
    assert_eq!(value(&done.payload(1), "TR"), None);
    // The package comes from the TADIR row of the object, never from the command.
    assert_eq!(value(&done.payload(1), "PACKAGE"), None);
    assert!(done.exit.stdout.ends_with("\nACTIVE"));
}

#[test]
fn a_transportable_object_keeps_its_tadir_package_and_carries_the_request_through_both_runs() {
    let (_, xml, name, text, changed) = FIXTURES[0];
    let file = xml.replace(text, changed);
    let live = live_hash(xml);
    let mut case = Case::deploy(0, replies(xml, name, written(file, "ZPKG", live)));

    case.package = "ZPKG";
    case.extra = vec!["--tr", "hd1k917754"];

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(done.exit.stdout.ends_with("\nACTIVE"));
    assert_eq!(value(&done.payload(0), "TR").as_deref(), Some("HD1K917754"));
    assert_eq!(value(&done.payload(1), "TR").as_deref(), Some("HD1K917754"));
    assert_eq!(value(&done.payload(1), "PACKAGE"), None);
}

#[test]
fn a_package_that_differs_from_the_report_of_the_run_is_partial() {
    let (_, xml, name, text, changed) = FIXTURES[0];
    let file = xml.replace(text, changed);
    let live = live_hash(xml);
    // The run reports ZPKG, a new session finds the TADIR row in $TMP.
    let done = Case::deploy(0, replies(xml, name, written(file, "ZPKG", live))).go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit.stdout.contains(
            "\nPARTIAL: the run reported ACTIVE but a new session disagrees (TADIR package $TMP, \
             expected ZPKG); check with read before retrying"
        ),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn a_transportable_object_without_a_request_is_rejected_by_run_1() {
    let (_, xml, ..) = FIXTURES[0];
    let reason = "Package ZPKG is transportable: --tr is required";
    let case = Case::deploy(
        0,
        Replies {
            simulate: reply(move |nonce| {
                simulate_run(nonce, xml, &rejected(&[], reason), "REJECTED")
            }),
            deploy: reply(|_| panic!("a rejection needs no second run")),
            read: read_reply(None),
        },
    );
    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(done.exit.stdout.ends_with(&format!("\nREJECTED: {reason}")));
    assert_eq!(done.modes(), ["SIMULATE"]);
}

#[test]
fn a_request_that_is_no_request_number_stops_before_any_connection() {
    let mut case = passing_case(0);

    case.extra = vec!["--tr", "hd1;k917754"];

    assert_eq!(case.stops().code, 2);
}

// ---- the new session decides ACTIVE ---------------------------------------------------------------------

#[test]
fn an_active_that_a_new_session_cannot_find_is_partial() {
    let mut case = passing_case(0);

    case.sap = sap(World {
        definition: false,
        directory: false,
        ..PRESENT
    });

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit.stdout.ends_with(
            "\nPARTIAL: the run reported ACTIVE but a new session disagrees (definition DD04L has \
             no row; TADIR has no row); check with read before retrying"
        ),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn an_active_that_leaves_an_inactive_version_is_partial() {
    let mut case = passing_case(0);

    case.sap = sap(World {
        inactive: true,
        ..PRESENT
    });

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .contains("\nPARTIAL: the run reported ACTIVE but a new session disagrees"),
        "{}",
        done.exit.stdout
    );
    assert!(
        done.exit.stdout.contains("inactive version"),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn a_preview_that_cannot_be_read_does_not_prove_active() {
    let mut case = passing_case(0);

    case.sap = sap(World {
        broken: true,
        ..PRESENT
    });

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .contains("\nPARTIAL: the run reported ACTIVE but a new session could not confirm it"),
        "{}",
        done.exit.stdout
    );
}

// ---- stdout and stderr ----------------------------------------------------------------------------------

#[test]
fn an_active_definition_that_differs_from_the_file_warns_on_stderr() {
    let (_, xml, name, ..) = FIXTURES[0];
    let live = live_hash(xml);
    let done = Case::deploy(
        0,
        replies(
            xml,
            name,
            reply(move |nonce| deploy_run(nonce, &live, "$TMP", Some(OTHER_BASE), &[], "ACTIVE")),
        ),
    )
    .go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(done.exit.stdout.ends_with("\nACTIVE"));
    assert_eq!(
        done.exit.stderr,
        "W: active definition differs from the file (SAP canonical); run read again before the \
         next edit"
    );
    assert!(!done.exit.stdout.contains("W:"));
}

#[test]
fn an_active_definition_equal_to_the_file_has_no_warning() {
    let done = passing_case(0).go();

    assert_eq!(done.exit.code, 0);
    assert_eq!(done.exit.stderr, "");
}

#[test]
fn warnings_of_both_runs_go_to_stderr_and_the_diff_and_status_to_stdout() {
    let (_, xml, name, text, changed) = FIXTURES[0];
    let file = xml.replace(text, changed);
    let live = live_hash(xml);
    let result = file_hash(&file);
    let mut case = Case::deploy(
        0,
        Replies {
            simulate: reply(move |nonce| {
                let mut extra = passing(name);

                extra.push(warning("first run warning"));

                simulate_run(nonce, xml, &extra, "SIMULATED_OK")
            }),
            deploy: reply(move |nonce| {
                deploy_run(
                    nonce,
                    &live,
                    "$TMP",
                    Some(&result),
                    &[warning("second run warning")],
                    "ACTIVE",
                )
            }),
            read: read_reply(None),
        },
    );

    case.file = file;

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(
        done.exit.stderr,
        "W: first run warning\nW: second run warning"
    );
    assert!(!done.exit.stdout.contains("warning"));
    assert!(done.exit.stdout.contains("--- live"));
    assert!(done.exit.stdout.ends_with("\nACTIVE"));
}

// ---- a class that stays, signals, routing ------------------------------------------------------------------

#[test]
fn a_class_that_stays_after_run_1_ends_the_command_before_any_write() {
    let case = passing_case(0);

    case.sap.borrow_mut().failing_deletes = 100;

    let done = case.go();
    let left: Vec<String> = done.sap.borrow().classes.keys().cloned().collect();

    assert_eq!(left.len(), 1);
    assert_eq!(done.exit.code, 1);
    assert_eq!(done.modes(), ["SIMULATE"]);
    assert!(!done.exit.stdout.contains("ACTIVE"));
    assert!(
        done.exit.stdout.contains("live hash"),
        "{}",
        done.exit.stdout
    );
    assert!(
        done.exit.stderr.contains(&format!(
            "ERROR: deploy DTEL ZDTEL: the temporary class of the diff run stays; nothing was \
             written\nRUNNER_LEFTOVER: {0}\nabapreach delete --simulate CLAS:{0} --plan <FILE> \
             --conn DEV_100\nabapreach delete CLAS:{0} --plan <FILE> --conn DEV_100",
            left[0]
        )),
        "{}",
        done.exit.stderr
    );
}

#[test]
fn a_class_that_stays_after_run_2_is_exit_1_although_the_write_succeeded() {
    let mut case = passing_case(0);

    case.fail_deletes_at_post = Some(2);

    let done = case.go();
    let left: Vec<String> = done.sap.borrow().classes.keys().cloned().collect();

    assert_eq!(left.len(), 1);
    assert_eq!(done.exit.code, 1);
    assert_eq!(done.modes(), ["SIMULATE", "DEPLOY"]);
    assert!(
        done.exit.stdout.ends_with("\nACTIVE"),
        "{}",
        done.exit.stdout
    );
    assert!(
        done.exit
            .stderr
            .contains(&format!("RUNNER_LEFTOVER: {}", left[0])),
        "{}",
        done.exit.stderr
    );
}

#[test]
fn ctrl_c_before_any_run_exits_130_and_opens_nothing() {
    let case = passing_case(0);

    case.cancel.set();

    let done = case.go();

    assert_eq!(done.exit.code, 130);
    assert_eq!(done.creates(), 0);
    assert_eq!(done.adt_opens + done.rfc_opens, 0);
}

#[test]
fn ctrl_c_during_run_1_exits_130_and_starts_no_write() {
    let case = passing_case(0);
    let cancel = case.cancel.clone();

    case.sap.borrow_mut().on_post = Some(Box::new(move || cancel.set()));

    let done = case.go();

    assert_eq!(done.exit.code, 130);
    assert_eq!(done.modes(), ["SIMULATE"]);
    assert!(!done.exit.stdout.contains("ACTIVE"));
    assert_clean(&done);
}

#[test]
fn a_connection_that_cannot_be_opened_falls_back_to_rfc_before_anything_is_written() {
    let mut case = passing_case(0);

    case.adt_down_from = Some(1);

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(
        runner_lines(&done.exit.stdout)
            .iter()
            .all(|(_, via)| via == "sadt")
    );
    assert!(done.exit.stdout.contains("reason: fallback to RFC"));
    assert!(
        done.exit.stderr.contains(
            "NOTE: ADT HTTP failed before anything was written; falling back to RFC SADT"
        )
    );
    assert!(done.exit.stdout.ends_with("\nACTIVE"));
}

#[test]
fn a_run_2_that_cannot_be_opened_reports_the_failure_and_never_reruns_run_1_on_rfc() {
    let mut case = passing_case(0);

    // Open 1 is run 1, open 2 its cleanup proof, open 3 run 2.
    case.adt_down_from = Some(3);

    let done = case.go();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert_eq!(done.modes(), ["SIMULATE"]);
    assert_eq!(done.rfc_opens, 0);
    assert_eq!(done.exit.stdout.matches("backend:").count(), 1);
    assert!(
        !done.exit.stderr.contains("falling back"),
        "{}",
        done.exit.stderr
    );
    assert!(done.exit.stderr.contains("ERROR:"), "{}", done.exit.stderr);
    assert_clean(&done);
}

// ---- an uncertain status after run 2 (Task 16) ----------------------------------------------------------

/// A case whose run 2 loses its reply, with the world after the POST and the answer of the extra
/// `READ` run.
fn uncertain(after: World, read: Option<String>, lose: Rc<dyn Fn(&str) -> PostReply>) -> Case {
    let (_, xml, name, ..) = FIXTURES[0];
    let mut case = Case::deploy(
        0,
        Replies {
            simulate: reply(move |nonce| simulate_run(nonce, xml, &passing(name), "SIMULATED_OK")),
            deploy: lose,
            read: read_reply(read),
        },
    );

    case.sap = sap(after);

    case
}

fn lost() -> Rc<dyn Fn(&str) -> PostReply> {
    Rc::new(|_| PostReply::Lost)
}

#[test]
fn a_lost_reply_of_run_2_is_reconciled_from_a_new_session_and_never_resent() {
    let (_, xml, _, text, changed) = FIXTURES[0];
    let file = xml.replace(text, changed);
    let cases = [
        // Active hash = file hash.
        (PRESENT, Some(file_hash(&file)), "ACTIVE (reconciled)"),
        // Active = base with an inactive version.
        (
            World {
                inactive: true,
                ..PRESENT
            },
            Some(live_hash(xml)),
            "INACTIVE",
        ),
        // Active = base without an inactive version.
        (PRESENT, Some(live_hash(xml)), "PARTIAL: outcome unknown"),
        // Neither.
        (
            PRESENT,
            Some(OTHER_BASE.to_owned()),
            "PARTIAL: unexpected state",
        ),
    ];

    for (after, read, token) in cases {
        let done = uncertain(after, read, lost()).go_virtual(None, None);

        assert_eq!(done.exit.code, 1, "{token}: {}", done.exit.stderr);
        assert!(
            done.exit.stdout.contains(&format!("\n{token}")),
            "{token}: {}",
            done.exit.stdout
        );
        assert!(
            done.exit.stdout.contains("check with read before retrying"),
            "{token}: {}",
            done.exit.stdout
        );
        // Run 1, run 2, and one extra READ run; the DEPLOY run is never resent.
        assert_eq!(done.modes(), ["SIMULATE", "DEPLOY", "READ"], "{token}");
        assert_clean(&done);
    }
}

#[test]
fn a_broken_run_2_and_a_5xx_are_uncertain_too() {
    let (_, xml, ..) = FIXTURES[0];

    for lose in [
        Rc::new(|_: &str| PostReply::Status(500, "boom".to_owned()))
            as Rc<dyn Fn(&str) -> PostReply>,
        reply(|nonce| format!("ABAPREACH:{nonce}:BEGIN:DEPLOY\n")),
    ] {
        let done = uncertain(PRESENT, Some(live_hash(xml)), lose).go_virtual(None, None);

        assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
        assert!(
            done.exit.stdout.contains("\nPARTIAL: outcome unknown"),
            "{}",
            done.exit.stdout
        );
        assert!(
            done.exit
                .stderr
                .contains("the classrun status is uncertain")
        );
        assert_eq!(
            done.modes().iter().filter(|mode| *mode == "DEPLOY").count(),
            1
        );
    }
}

#[test]
fn a_virtual_deploy_carries_the_given_base_and_request() {
    let (_, xml, name, text, changed) = FIXTURES[0];
    let file = xml.replace(text, changed);
    let live = live_hash(xml);
    let hash = live.clone();
    let case = Case::deploy(0, replies(xml, name, written(file, "$TMP", live)));
    let done = case.go_virtual(Some(&hash), Some("hd1k917754"));

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert_eq!(value(&done.payload(1), "BASE"), Some(hash));
    assert_eq!(value(&done.payload(1), "TR").as_deref(), Some("HD1K917754"));
}
