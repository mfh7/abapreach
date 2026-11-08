//! `create` of `DTEL`, `DOMA`, `TABL`, and `TTYP` through the temporary runner class (M6 Task 21),
//! driven through the command line against a fake SAP. One run (mode `CREATE`) writes the
//! definition; the status is decided by the client: `CREATE: ACTIVE` needs the run to say so (with
//! the hash of the written definition) AND a new session to find the definition active, in the
//! package of the command, with no inactive version left. The fake keeps the runner class state,
//! answers the data previews, and answers each classrun POST with a run bound to the nonce that
//! the generated payload carries. No SAP.
//!
//! Transportable creation is supported: live U4 passed (`RS_CORR_INSERT` class `DICT`, no dialog),
//! so there is no UNSUPPORTED path and no test for one.
//!
//! Ports of the Python create cases and their intentional differences:
//! - `test_ddic_create_requires_file`, `test_ddic_create_rejects_source_options`: ported
//!   (`create_needs_a_file_and_every_rejected_flag_stops_before_any_connection`).
//! - `test_ddic_create_sends_xml_single_call`: ported as the payload checks; the XML travels inside
//!   the payload of a temporary class, not as an FM parameter.
//! - The status lines are the M3 tokens (`CREATE: ACTIVE` and the others); each create prints the
//!   `RUNNER:` line because it creates and deletes a temporary class in `$TMP`.

mod common;

use abapreach::backend::BackendKind;
use abapreach::config::Connection;
use abapreach::hash::{normalize, source_hash};
use abapreach::operation::create_ddic::{DdicCreate, create_with};
use abapreach::operation::deploy::CancelFlag;
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
const RETRY: &str = "check with read before retrying";

/// `(token, definition, name in the definition, text in it)`.
const FIXTURES: [(&str, &str, &str, &str); 4] = [
    (
        "DTEL",
        include_str!("fixtures/m6/read_dtel.xml"),
        "ZDTEL",
        "Test data element",
    ),
    (
        "DOMA",
        include_str!("fixtures/m6/read_doma.xml"),
        "ZDOMA",
        "Test domain",
    ),
    (
        "TABL",
        include_str!("fixtures/m6/read_tabl.xml"),
        "ZTABL",
        "Test table",
    ),
    (
        "TTYP",
        include_str!("fixtures/m6/read_ttyp.xml"),
        "ZTTYP",
        "Test table type",
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

/// The hash of a file as the product computes it: the rows after the client rule.
fn file_hash(text: &str) -> String {
    source_hash(&normalize(rows_of(text)))
}

/// What a `CREATE` run answers: the lines of `extra`, the hash of the written definition when
/// `result` is given, and the status.
fn create_run(nonce: &str, extra: &[String], result: Option<&str>, status: &str) -> String {
    let mut body = extra.to_vec();

    if let Some(result) = result {
        body.push(fact("RESULT_HASH", result));
    }

    run_output(nonce, "CREATE", &body, status)
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
    adt_down: bool,
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

/// The world of the previews: nothing before the `CREATE` run, `after` once it has run.
fn sap(after: World) -> Shared {
    new_sap(
        true,
        &Rc::new(Cell::new(Duration::ZERO)),
        Box::new(move |tick| if tick.posts >= 1 { after } else { ABSENT }),
    )
}

/// The object with its TADIR row and no inactive version.
const PRESENT: World = World {
    definition: true,
    directory: true,
    inactive: false,
    broken: false,
};

/// Neither a definition nor a TADIR row.
const ABSENT: World = World {
    definition: false,
    directory: false,
    inactive: false,
    broken: false,
};

/// One reply per mode that a create can send.
struct Replies {
    create: Rc<dyn Fn(&str) -> PostReply>,
    read: Rc<dyn Fn(&str) -> PostReply>,
}

impl Replies {
    fn script(self) -> Script {
        Rc::new(move |payload| {
            let nonce = value(payload, "NONCE").unwrap_or_default();

            match value(payload, "MODE").as_deref() {
                Some("CREATE") => (self.create)(&nonce),
                Some("READ") => (self.read)(&nonce),
                other => panic!("unexpected mode {other:?}"),
            }
        })
    }
}

/// One scripted command: the system, the replies to the POSTs, and the file.
struct Case {
    index: usize,
    kind: &'static str,
    name: &'static str,
    extra: Vec<&'static str>,
    sap: Shared,
    script: Script,
    adt_down: bool,
    cancel: CancelFlag,
    /// The package that the TADIR row shows.
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
    /// The runner classes the command created.
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
    /// The create of fixture `index`; the definition in the file is the fixture.
    fn create(index: usize, replies: Replies) -> Self {
        let (kind, xml, name, _) = FIXTURES[index];

        Self {
            index,
            kind,
            name,
            extra: Vec::new(),
            sap: sap(PRESENT),
            script: replies.script(),
            adt_down: false,
            cancel: CancelFlag::new(),
            package: "$TMP",
            fail_deletes_at_post: None,
            file: xml.to_owned(),
        }
    }

    fn args(&self, file: &Path) -> Vec<String> {
        let mut args: Vec<String> = ["abapreach", "create", self.kind, self.name]
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
            adt_down: self.adt_down,
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

    /// Runs the create through the library entry with a virtual clock, so that a reconciliation
    /// does not wait.
    fn go_virtual(self) -> Done {
        let seen: Seen = Rc::default();
        let mut fake = self.fake(&seen);
        let request = DdicCreate {
            kind: [Kind::Dtel, Kind::Doma, Kind::Tabl, Kind::Ttyp][self.index],
            name: self.name,
            xml: &self.file,
            package: None,
            text: None,
            tr: None,
        };
        let mut clock = FakeClock {
            now: Rc::new(Cell::new(Duration::ZERO)),
        };
        let exit = create_with(
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
fn read_reply(found: bool) -> Rc<dyn Fn(&str) -> PostReply> {
    reply(move |nonce| {
        if found {
            run_output(nonce, "READ", &[fact("HASH", &"a".repeat(64))], "OK")
        } else {
            run_output(nonce, "READ", &[], "NOT_FOUND")
        }
    })
}

fn plan(index: usize, package: &str, request: &str) -> String {
    let (kind, _, name, _) = FIXTURES[index];

    info(&format!(
        "CREATE {kind} {name} package {package} TR {request}"
    ))
}

/// A `CREATE` run that wrote the file and proved it: the result hash is the file hash.
fn written(index: usize, package: &'static str, request: &'static str) -> Replies {
    let file = FIXTURES[index].1;

    Replies {
        create: reply(move |nonce| {
            create_run(
                nonce,
                &[plan(index, package, request)],
                Some(&file_hash(file)),
                "ACTIVE",
            )
        }),
        read: read_reply(false),
    }
}

/// A `CREATE` run that ends with `status` after the lines of `extra`.
fn ended(extra: Vec<String>, result: Option<String>, status: &'static str) -> Replies {
    Replies {
        create: reply(move |nonce| create_run(nonce, &extra, result.as_deref(), status)),
        read: read_reply(false),
    }
}

fn passing_case(index: usize) -> Case {
    Case::create(index, written(index, "$TMP", "-"))
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

// ---- a create in $TMP and in a transportable package ---------------------------------------------------

#[test]
fn a_create_in_tmp_runs_one_class_and_is_active_for_each_type() {
    for (index, (kind, _, name, _)) in FIXTURES.iter().enumerate() {
        let done = passing_case(index).go();
        let runners = runner_lines(&done.exit.stdout);

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert_eq!(done.exit.stderr, "", "{kind}");
        assert_eq!(
            done.exit.stdout,
            format!(
                "backend: adt\nreason: default ADT\n\
                 RUNNER: {} (temporary, $TMP, post via http, removed after the run)\n\
                 {PREFLIGHT}\n\
                 CREATE {kind} {name} package $TMP TR -\n\
                 CREATE: ACTIVE\n\
                 {kind} {name} created in package $TMP",
                runners[0].0
            ),
            "{kind}"
        );
        assert_eq!(runners.len(), 1, "{kind}");
        assert_eq!(done.modes(), ["CREATE"], "{kind}");
        assert_eq!(done.creates(), 1, "{kind}");
        assert_clean(&done);
    }
}

#[test]
fn the_create_payload_carries_the_rows_and_no_package_request_or_base_for_tmp() {
    for (index, (kind, xml, ..)) in FIXTURES.iter().enumerate() {
        let done = passing_case(index).go();
        let payload = done.payload(0);

        assert_eq!(value(&payload, "MODE").as_deref(), Some("CREATE"));
        assert_eq!(value(&payload, "TYPE").as_deref(), Some(*kind));
        assert_eq!(value(&payload, "PACKAGE"), None, "{kind}");
        assert_eq!(value(&payload, "TR"), None, "{kind}");
        assert_eq!(value(&payload, "BASE"), None, "{kind}");
        assert_eq!(xml_of(&payload), rows_of(xml), "{kind}");
    }
}

#[test]
fn the_four_types_create_on_adt_http_and_on_rfc_sadt() {
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
            assert!(
                done.exit.stdout.contains("\nCREATE: ACTIVE\n"),
                "{kind}: {}",
                done.exit.stdout
            );
            assert!(
                runner_lines(&done.exit.stdout)
                    .iter()
                    .all(|(_, via)| via == post),
                "{kind}"
            );
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
fn a_transportable_create_carries_the_package_and_the_request_and_accepts_the_text() {
    for (index, (kind, _, name, text)) in FIXTURES.iter().enumerate() {
        let mut case = Case::create(index, written(index, "ZABAPTEST", "HD1K917754"));

        case.package = "ZABAPTEST";
        case.extra = vec![
            "--package",
            "zabaptest",
            "--tr",
            "hd1k917754",
            "--text",
            text,
        ];

        let done = case.go();
        let payload = done.payload(0);

        assert_eq!(done.exit.code, 0, "{kind}: {}", done.exit.stderr);
        assert_eq!(value(&payload, "PACKAGE").as_deref(), Some("ZABAPTEST"));
        assert_eq!(value(&payload, "TR").as_deref(), Some("HD1K917754"));
        // A DDIC text only has to agree with the XML (checked before the connection): the
        // description travels inside the XML rows.
        assert_eq!(value(&payload, "TEXT"), None, "{kind}");
        assert!(
            done.exit.stdout.ends_with(&format!(
                "\nCREATE: ACTIVE\n{kind} {name} created in package ZABAPTEST (request HD1K917754)"
            )),
            "{kind}: {}",
            done.exit.stdout
        );
        assert_clean(&done);
    }
}

// ---- the rules that stop before any connection -----------------------------------------------------------

#[test]
fn create_needs_a_file_and_every_rejected_flag_stops_before_any_connection() {
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
        (
            vec!["--package", "ZABAPTEST", "--tr", "hd1;k917754"],
            "STOP: invalid --tr: hd1;k917754",
        ),
        (
            vec!["--text", "Another text"],
            "STOP: --text differs from the description in the XML file",
        ),
    ] {
        let mut case = passing_case(0);

        case.extra = extra;

        let exit = case.stops();

        assert_eq!(exit.code, 2, "{message}");
        assert_eq!(exit.stderr, message);
        assert_eq!(exit.stdout, "");
    }
}

#[test]
fn a_file_for_another_object_and_a_name_outside_the_customer_scope_stop_before_any_connection() {
    let mut case = passing_case(0);

    case.file = case.file.replace("ZDTEL", "ZOTHER");

    assert_eq!(
        case.stops().stderr,
        "STOP: XML file is for ZOTHER, not DTEL ZDTEL"
    );

    let mut case = passing_case(0);

    case.name = "MANDT";

    assert_eq!(
        case.stops().stderr,
        "STOP: a created name must start with Z or Y: MANDT"
    );
}

#[test]
fn a_create_without_a_file_and_a_file_that_cannot_be_read_stop_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.xml");
    let without: Vec<String> = ["abapreach", "create", "DTEL", "ZDTEL", "--conn", "DEV_100"]
        .map(str::to_owned)
        .to_vec();
    let unreadable: Vec<String> = [
        "abapreach",
        "create",
        "DTEL",
        "ZDTEL",
        missing.to_str().unwrap(),
        "--conn",
        "DEV_100",
    ]
    .map(str::to_owned)
    .to_vec();
    let exit = run(without, env_in(directory.path()), &mut NoTransport);

    assert_eq!(exit.code, 2);
    assert_eq!(exit.stderr, "STOP: create DTEL needs FILE (XML)");

    let exit = run(unreadable, env_in(directory.path()), &mut NoTransport);

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr
            .starts_with(&format!("STOP: cannot read {}", missing.display())),
        "{}",
        exit.stderr
    );
}

// ---- the statuses of the run -----------------------------------------------------------------------------

#[test]
fn a_name_that_exists_and_a_transparent_name_over_16_characters_are_rejected_by_the_run() {
    for (index, reason) in [
        (0, "DTEL ZDTEL already exists"),
        (
            TABL,
            "Transparent table name ZTABLE_NAME_TOO_LONG > 16 characters",
        ),
    ] {
        let mut case = Case::create(index, ended(vec![error(reason)], None, "REJECTED"));

        case.sap = sap(ABSENT);

        let done = case.go();

        assert_eq!(done.exit.code, 1, "{reason}");
        assert!(
            done.exit
                .stdout
                .ends_with(&format!("\nCREATE: REJECTED\n{reason}")),
            "{}",
            done.exit.stdout
        );
        assert_eq!(done.sap.borrow().posts, 1);
        assert_clean(&done);
    }
}

#[test]
fn an_activation_failure_is_create_inactive_with_the_reason() {
    let mut case = Case::create(
        0,
        ended(
            vec![
                plan(0, "$TMP", "-"),
                warning("Activation warning"),
                error("Activation of DTEL ZDTEL failed"),
            ],
            None,
            "INACTIVE",
        ),
    );

    case.sap = sap(ABSENT);

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .ends_with("\nCREATE: INACTIVE\nActivation of DTEL ZDTEL failed"),
        "{}",
        done.exit.stdout
    );
    assert!(done.exit.stderr.contains("W: Activation warning"));
    assert!(!done.exit.stdout.contains("CREATE: ACTIVE"));
    assert_clean(&done);
}

#[test]
fn a_run_without_a_reason_gets_the_default_reason_of_its_status() {
    for (status, token, text) in [
        (
            "INACTIVE",
            "CREATE: INACTIVE",
            "the definition was saved but not activated",
        ),
        ("PARTIAL", "CREATE: PARTIAL", "the create ended partially"),
        ("REJECTED", "CREATE: REJECTED", "the create was rejected"),
    ] {
        let done = Case::create(0, ended(Vec::new(), None, status)).go();

        assert_eq!(done.exit.code, 1, "{status}");
        assert!(
            done.exit.stdout.ends_with(&format!("\n{token}\n{text}")),
            "{status}: {}",
            done.exit.stdout
        );
    }
}

#[test]
fn a_partial_run_is_partial_with_its_reason() {
    let done = Case::create(
        0,
        ended(
            vec![error("The request could not be written")],
            None,
            "PARTIAL",
        ),
    )
    .go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .ends_with("\nCREATE: PARTIAL\nThe request could not be written"),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn a_run_that_reports_active_without_a_result_hash_is_partial() {
    let done = Case::create(0, ended(vec![plan(0, "$TMP", "-")], None, "ACTIVE")).go();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit.stdout.ends_with(&format!(
            "\nCREATE: PARTIAL\nthe run reported ACTIVE without a result hash; {RETRY}"
        )),
        "{}",
        done.exit.stdout
    );
}

// ---- the new session decides ACTIVE ------------------------------------------------------------------------

/// The text of the `CREATE: PARTIAL` that a disagreeing new session gives.
fn disagreement(stdout: &str) -> &str {
    stdout
        .split_once("\nCREATE: PARTIAL\nthe run reported ACTIVE but ")
        .unwrap_or_else(|| panic!("no disagreement in {stdout:?}"))
        .1
}

#[test]
fn an_active_that_a_new_session_cannot_find_is_partial() {
    let mut case = passing_case(0);

    case.sap = sap(ABSENT);

    let done = case.go();
    let why = disagreement(&done.exit.stdout);

    assert_eq!(done.exit.code, 1);
    assert!(
        why.starts_with("a new session disagrees (definition DD04L has no row; TADIR has no row)"),
        "{why}"
    );
    assert!(why.ends_with(&format!("; {RETRY}")), "{why}");
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
        disagreement(&done.exit.stdout)
            .starts_with("a new session disagrees (an inactive version exists)"),
        "{}",
        done.exit.stdout
    );
}

#[test]
fn a_package_that_differs_from_the_command_is_partial() {
    let mut case = passing_case(0);

    // The new session finds the TADIR row in ZPKG while the command asked for $TMP.
    case.package = "ZPKG";

    let done = case.go();

    assert_eq!(done.exit.code, 1);
    assert!(
        disagreement(&done.exit.stdout)
            .starts_with("a new session disagrees (TADIR package ZPKG, expected $TMP)"),
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
        disagreement(&done.exit.stdout).starts_with("a new session could not confirm it ("),
        "{}",
        done.exit.stdout
    );
}

// ---- a class that stays, Ctrl-C, and the fallback --------------------------------------------------------------

#[test]
fn a_class_that_stays_is_exit_1_although_the_create_succeeded() {
    let mut case = passing_case(0);

    case.fail_deletes_at_post = Some(1);

    let done = case.go();
    let left: Vec<String> = done.sap.borrow().classes.keys().cloned().collect();

    assert_eq!(left.len(), 1);
    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit.stdout.contains("\nCREATE: ACTIVE\n"),
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
fn a_connection_that_cannot_be_opened_falls_back_to_rfc_before_anything_is_written() {
    let mut case = passing_case(0);

    case.adt_down = true;

    let done = case.go();

    assert_eq!(done.exit.code, 0, "{}", done.exit.stderr);
    assert!(
        runner_lines(&done.exit.stdout)
            .iter()
            .all(|(_, via)| via == "sadt")
    );
    assert!(done.exit.stdout.contains("reason: fallback to RFC"));
    assert!(done.exit.stdout.contains("\nCREATE: ACTIVE\n"));
    assert_eq!(done.sap.borrow().posts, 1);
}

// ---- an uncertain create (Task 16) ---------------------------------------------------------------------------

/// A case whose `CREATE` run loses its reply, with the world after the POST and the answer of the
/// extra `READ` run.
fn uncertain(after: World, found: bool, lose: Rc<dyn Fn(&str) -> PostReply>) -> Case {
    let mut case = Case::create(
        0,
        Replies {
            create: lose,
            read: read_reply(found),
        },
    );

    case.sap = sap(after);

    case
}

fn lost() -> Rc<dyn Fn(&str) -> PostReply> {
    Rc::new(|_| PostReply::Lost)
}

#[test]
fn a_lost_create_reply_is_settled_from_the_evidence_and_never_resent() {
    for (after, line) in [
        (
            PRESENT,
            "CREATE: PARTIAL; the object exists after an uncertain create; check with read \
             before retrying",
        ),
        (
            ABSENT,
            "CREATE: REJECTED; the object does not exist after an uncertain create; check with \
             read before retrying",
        ),
    ] {
        let done = uncertain(after, false, lost()).go_virtual();

        assert_eq!(done.exit.code, 1, "{line}: {}", done.exit.stderr);
        assert!(
            done.exit.stdout.ends_with(line),
            "{line}: {}",
            done.exit.stdout
        );
        assert!(
            done.exit
                .stderr
                .contains("the classrun status is uncertain"),
            "{}",
            done.exit.stderr
        );
        // The previews are enough: no extra READ run, and the create is never resent.
        assert_eq!(done.modes(), ["CREATE"], "{line}");
        assert_clean(&done);
    }
}

#[test]
fn a_5xx_and_a_broken_reply_are_uncertain_too() {
    for lose in [
        Rc::new(|_: &str| PostReply::Status(500, "boom".to_owned()))
            as Rc<dyn Fn(&str) -> PostReply>,
        reply(|nonce| format!("ABAPREACH:{nonce}:BEGIN:CREATE\n")),
    ] {
        let done = uncertain(PRESENT, false, lose).go_virtual();

        assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
        assert!(
            done.exit
                .stdout
                .contains("\nCREATE: PARTIAL; the object exists"),
            "{}",
            done.exit.stdout
        );
        assert_eq!(done.modes(), ["CREATE"]);
    }
}

#[test]
fn previews_that_cannot_be_read_are_settled_by_one_extra_read_run() {
    let broken = World {
        broken: true,
        ..PRESENT
    };

    for (found, token) in [(true, "CREATE: PARTIAL;"), (false, "CREATE: REJECTED;")] {
        let done = uncertain(broken, found, lost()).go_virtual();

        assert_eq!(done.exit.code, 1);
        assert!(
            done.exit.stdout.contains(&format!("\n{token} ")),
            "{token}: {}",
            done.exit.stdout
        );
        assert_eq!(done.modes(), ["CREATE", "READ"], "{token}");
        assert_clean(&done);
    }
}

#[test]
fn a_reconciliation_that_cannot_settle_is_partial_and_incomplete() {
    let broken = World {
        broken: true,
        ..PRESENT
    };
    let mut case = uncertain(broken, false, lost());

    case.script = Replies {
        create: lost(),
        read: lost(),
    }
    .script();

    let done = case.go_virtual();

    assert_eq!(done.exit.code, 1);
    assert!(
        done.exit
            .stdout
            .contains("PARTIAL: reconciliation incomplete"),
        "{}",
        done.exit.stdout
    );
    assert_eq!(done.modes(), ["CREATE", "READ"]);
}

/// A dropped reply of the classrun POST (the fault feature) is an uncertain status: the create is
/// settled from a new session and never resent.
#[cfg(feature = "fault-injection")]
#[test]
fn a_dropped_classrun_reply_of_a_create_is_reconciled_and_not_resent() {
    use abapreach::fault::{self, Fault};

    let _scope = fault::scoped(Fault::from_lookup(Some("drop-reply:CLASSRUN")).unwrap());
    let done = passing_case(0).go_virtual();

    assert_eq!(done.exit.code, 1, "{}", done.exit.stderr);
    assert!(
        done.exit
            .stdout
            .contains("\nCREATE: PARTIAL; the object exists"),
        "{}",
        done.exit.stdout
    );
    assert_eq!(done.modes(), ["CREATE"]);
    assert_eq!(done.sap.borrow().posts, 1);
    assert_clean(&done);
}
