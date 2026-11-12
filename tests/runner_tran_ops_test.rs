//! `read` and `create` of a `TRAN` through the temporary runner class (M6 Task 22) against the fake
//! SAP of the runner tests: output format, the status and exit of every branch of the contract,
//! the payload that reaches the class, the routing between the backends, and the reconciliation of
//! an uncertain create. Time is virtual; no SAP.

mod common;

use abapreach::backend::BackendKind;
use abapreach::config::Connection;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{CancelFlag, InterruptAction};
use abapreach::operation::payload::{Kind, Mode, Request as PayloadRequest, encode};
use abapreach::operation::runner_reconcile::Clock;
use abapreach::operation::runner_tran::{TranCreate, TranRead, create_with, read_with};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo};
use common::runner_fake::{
    Evolution, Opened, PostReply, Shared, World, http_session, new_sap, rfc_session,
};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const PREFIX: &str = "YTEST_ABAPREACH_RUN_";
const TCODE: &str = "YTEST_ABAPREACHT0001";
/// The report the fake data preview answers for `PGMNA`.
const REPORT: &str = "ZREPORT";
const TEXT: &str = "Demo transaction";
const PREFLIGHT: &str =
    "RUNNER: preflight T000 CCCATEGORY=C CCCORACTIV=1 CCNOCLIIND=- TRNSPACE /0CUST/ EDITFLAG=X";

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

/// A well formed run: BEGIN, the body, STATUS, END, in the classrun framing.
fn output(mode: &str, body: &[String], status: &str) -> String {
    let mut lines = vec![format!("ABAPREACH:{NONCE}:BEGIN:{mode}")];

    lines.extend(body.iter().map(|rest| format!("ABAPREACH:{NONCE}:{rest}")));
    lines.push(format!("ABAPREACH:{NONCE}:STATUS:{status}"));

    let count = lines.len();

    lines.push(format!("ABAPREACH:{NONCE}:END:{count}"));

    lines.iter().map(|line| format!("{line}\n\n")).collect()
}

fn msg(level: &str, text: &str) -> String {
    format!("MSG:{level}:{}", hex(text))
}

fn line(text: &str) -> String {
    format!("LINE:{}", hex(text))
}

/// The six lines of a live transaction.
fn live_lines() -> Vec<String> {
    vec![
        format!("TRAN {TCODE}"),
        format!("program={REPORT}"),
        "dynpro=1000".to_owned(),
        "cinfo=80".to_owned(),
        format!("text={TEXT}"),
        "package=$TMP".to_owned(),
    ]
}

fn read_ok() -> String {
    let body: Vec<String> = live_lines().iter().map(|text| line(text)).collect();

    output("READ", &body, "OK")
}

fn create_reply(mode: &str, messages: &[(&str, &str)], status: &str) -> PostReply {
    let body: Vec<String> = messages
        .iter()
        .map(|(level, text)| msg(level, text))
        .collect();

    PostReply::Output(output(mode, &body, status))
}

// --- the fake system and its clock -----------------------------------------------------------

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

fn present() -> World {
    World {
        definition: true,
        directory: true,
        ..World::default()
    }
}

fn constant(world: World) -> Evolution {
    Box::new(move |_| world)
}

struct Rig {
    sap: Shared,
    now: Rc<Cell<Duration>>,
}

fn rig(world: World) -> Rig {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let sap = new_sap(true, &now, constant(world));

    Rig { sap, now }
}

impl Rig {
    fn reply(&self, replies: Vec<PostReply>) {
        self.sap.borrow_mut().post_replies = replies.into();
    }

    fn posts(&self) -> usize {
        self.sap.borrow().posts
    }

    fn classes(&self) -> usize {
        self.sap.borrow().classes.len()
    }
}

/// Wraps a session and keeps the body of every PUT: the class source that the cycle writes.
struct Recorder {
    inner: Box<dyn SadtTransport>,
    puts: Rc<RefCell<Vec<String>>>,
}

impl SadtTransport for Recorder {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if request.method == "PUT" {
            self.puts
                .borrow_mut()
                .push(String::from_utf8_lossy(&request.body).into_owned());
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

/// The factories of the test: sessions of the fake SAP, or an ADT route that cannot connect.
struct Factory {
    sap: Shared,
    adt_fails: bool,
    adt_opened: usize,
    rfc_opened: usize,
    cancel: CancelFlag,
    puts: Rc<RefCell<Vec<String>>>,
}

impl Factory {
    fn new(rig: &Rig) -> Self {
        Self {
            sap: rig.sap.clone(),
            adt_fails: false,
            adt_opened: 0,
            rfc_opened: 0,
            cancel: CancelFlag::new(),
            puts: Rc::default(),
        }
    }
}

impl Factories for Factory {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened += 1;

        if self.adt_fails {
            return Err(TransportError::PreSendConnectivity("no route".to_owned()));
        }

        Ok(Box::new(Recorder {
            inner: http_session(&self.sap)?,
            puts: self.puts.clone(),
        }))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened += 1;

        Ok(Box::new(Recorder {
            inner: rfc_session(&self.sap)?,
            puts: self.puts.clone(),
        }))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        Err("no library in this test".to_owned())
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        Ok(())
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

fn connection() -> Connection {
    let mut connection = Connection::test_insecure();

    connection.runner_prefix = PREFIX.to_owned();

    connection
}

const HTTP: Option<BackendKind> = Some(BackendKind::AdtHttp);
const RFC: Option<BackendKind> = Some(BackendKind::RfcSadt);

fn spec<'a>(simulate: bool) -> TranCreate<'a> {
    TranCreate {
        name: TCODE,
        report: REPORT,
        text: Some(TEXT),
        package: None,
        tr: None,
        simulate,
    }
}

fn run_create(rig: &Rig, backend: Option<BackendKind>, create: &TranCreate<'_>) -> Exit {
    let mut factory = Factory::new(rig);

    run_create_with(rig, backend, create, &mut factory)
}

fn run_create_with(
    rig: &Rig,
    backend: Option<BackendKind>,
    create: &TranCreate<'_>,
    factory: &mut Factory,
) -> Exit {
    let mut nonce = || NONCE.to_owned();
    let mut clock = FakeClock {
        now: rig.now.clone(),
    };

    create_with(
        &connection(),
        backend,
        create,
        factory,
        &mut nonce,
        &mut clock,
    )
}

fn run_read(rig: &Rig, backend: Option<BackendKind>, out: &Path) -> Exit {
    let mut factory = Factory::new(rig);
    let mut nonce = || NONCE.to_owned();
    let mut clock = FakeClock {
        now: rig.now.clone(),
    };
    let read = TranRead { name: TCODE, out };

    read_with(
        &connection(),
        backend,
        &read,
        &mut factory,
        &mut nonce,
        &mut clock,
    )
}

/// The lines of a stream with the random runner name replaced.
fn shown(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| match line.strip_prefix("RUNNER: ") {
            Some(rest) if rest.starts_with(PREFIX) => {
                let (_, tail) = rest.split_once(' ').expect("a runner line has a tail");

                format!("RUNNER: <RUNNER> {tail}")
            }
            _ => line.to_owned(),
        })
        .collect()
}

fn runner_line(post: &str) -> String {
    format!("RUNNER: <RUNNER> (temporary, $TMP, post via {post}, removed after the run)")
}

const BACKENDS: [(Option<BackendKind>, &str, &str); 2] =
    [(HTTP, "http", "adt"), (RFC, "sadt", "rfc")];

// --- read -------------------------------------------------------------------------------------

#[test]
fn read_writes_the_six_lines_and_prints_the_hash_and_the_runner_line() {
    for (backend, post, _) in BACKENDS {
        let rig = rig(present());
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("tran.txt");

        rig.reply(vec![PostReply::Output(read_ok())]);

        let exit = run_read(&rig, backend, &out);

        assert_eq!(exit.code, 0, "{backend:?} {}", exit.stderr);
        assert_eq!(exit.stderr, "");
        assert_eq!(
            shown(&exit.stdout),
            vec![
                runner_line(post),
                PREFLIGHT.to_owned(),
                format!("6 lines -> {}", out.display()),
                format!("hash {}", source_hash(&live_lines())),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            format!("{}\n", live_lines().join("\n"))
        );
        assert_eq!(rig.posts(), 1);
        assert_eq!(rig.classes(), 0, "the runner class is removed");
    }
}

#[test]
fn read_reports_a_missing_or_refused_transaction_as_an_error_and_writes_nothing() {
    let cases = [
        ("NOT_FOUND", "Transaction YT does not exist (TSTC)"),
        ("REJECTED", "Client 100 is productive (CCCATEGORY=P)"),
    ];

    for (status, text) in cases {
        let rig = rig(present());
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("tran.txt");

        rig.reply(vec![create_reply("READ", &[("E", text)], status)]);

        let exit = run_read(&rig, HTTP, &out);

        assert_eq!(exit.code, 1, "{status}");
        assert_eq!(exit.stderr, format!("ERROR: {text}"), "{status}");
        assert_eq!(
            shown(&exit.stdout),
            vec![runner_line("http"), PREFLIGHT.to_owned()]
        );
        assert!(!out.exists(), "{status}");
        assert_eq!(rig.classes(), 0);
    }
}

#[test]
fn read_refuses_a_reply_that_is_not_the_six_lines() {
    let rig = rig(present());
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("tran.txt");
    let body = vec![line(&format!("TRAN {TCODE}")), line("program=ZREPORT")];

    rig.reply(vec![PostReply::Output(output("READ", &body, "OK"))]);

    let exit = run_read(&rig, HTTP, &out);

    assert_eq!(exit.code, 1);
    assert_eq!(
        exit.stderr,
        "ERROR: the runner returned 2 lines for the transaction; expected 6"
    );
    assert!(!out.exists());
}

#[test]
fn read_with_a_lost_reply_is_an_error_and_never_resends() {
    let rig = rig(present());
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("tran.txt");

    rig.reply(vec![PostReply::Lost]);

    let exit = run_read(&rig, HTTP, &out);

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.contains("W: the classrun status is uncertain"));
    assert!(
        exit.stderr
            .ends_with("ERROR: the read status is unknown; nothing was written to SAP"),
        "{}",
        exit.stderr
    );
    assert_eq!(rig.posts(), 1);
    assert_eq!(rig.classes(), 0);
    assert!(!out.exists());
}

// --- create: the success paths ----------------------------------------------------------------

#[test]
fn create_prints_active_after_the_run_and_the_new_session_agree() {
    for (backend, post, token) in BACKENDS {
        let rig = rig(present());

        rig.reply(vec![create_reply("CREATE", &[], "ACTIVE")]);

        let exit = run_create(&rig, backend, &spec(false));

        assert_eq!(exit.code, 0, "{}", exit.stderr);
        assert_eq!(exit.stderr, "");
        assert_eq!(
            shown(&exit.stdout),
            vec![
                format!("backend: {token}"),
                "reason: explicit --backend".to_owned(),
                runner_line(post),
                PREFLIGHT.to_owned(),
                "CREATE: ACTIVE".to_owned(),
                format!("TRAN {TCODE} created (program {REPORT}, package $TMP)"),
            ]
        );
        assert_eq!(rig.classes(), 0);
    }
}

#[test]
fn create_simulate_prints_the_guards_and_writes_no_evidence_preview() {
    let rig = rig(World::default());

    rig.reply(vec![create_reply("CREATE_SIM", &[], "SIMULATED_OK")]);

    let exit = run_create(&rig, HTTP, &spec(true));

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(
        shown(&exit.stdout),
        vec![
            "backend: adt".to_owned(),
            "reason: explicit --backend".to_owned(),
            runner_line("http"),
            PREFLIGHT.to_owned(),
            format!("WILL CREATE TRAN {TCODE} in package $TMP"),
            "GUARD Customer names only (Z or Y)".to_owned(),
            "GUARD System change option, client role, and customer namespace".to_owned(),
            "GUARD Request owner and state, and the package of the object".to_owned(),
            "GUARD A name not held in TSTC or TADIR, and an executable customer report".to_owned(),
            "CREATE_SIM: SIMULATED_OK".to_owned(),
            "validated; nothing was written".to_owned(),
        ]
    );
    // Only the two previews of the preflight: a simulate writes nothing to prove.
    assert_eq!(rig.sap.borrow().previews, 2);
    assert_eq!(rig.classes(), 0);
}

#[test]
fn create_simulate_names_the_request_of_a_transportable_package() {
    let rig = rig(World::default());
    let mut create = spec(true);

    create.package = Some("ZPKG");
    create.tr = Some("DEVK900001");
    rig.reply(vec![create_reply("CREATE_SIM", &[], "SIMULATED_OK")]);

    let exit = run_create(&rig, RFC, &create);

    assert!(
        shown(&exit.stdout).contains(&format!(
            "WILL CREATE TRAN {TCODE} in package ZPKG (request DEVK900001)"
        )),
        "{}",
        exit.stdout
    );
}

// --- create: the failure branches of the contract ---------------------------------------------

#[test]
fn create_passes_the_class_rejections_through_as_english_text() {
    let exists = format!("Object TRAN {TCODE} already exists (TSTC/TADIR)");
    let report =
        format!("A transaction needs exactly one existing executable report (SUBC 1): {REPORT}");
    let customer = format!("Report {REPORT} is not a customer report");
    let cases = [exists, report, customer];

    for simulate in [false, true] {
        for text in &cases {
            let rig = rig(World::default());
            let mode = if simulate { "CREATE_SIM" } else { "CREATE" };

            rig.reply(vec![create_reply(mode, &[("E", text)], "REJECTED")]);

            let exit = run_create(&rig, HTTP, &spec(simulate));
            let shown = shown(&exit.stdout);

            assert_eq!(exit.code, 1, "{text}");
            assert_eq!(shown[shown.len() - 2], "CREATE: REJECTED", "{text}");
            assert_eq!(shown[shown.len() - 1], *text);
            assert_eq!(rig.classes(), 0);
            // A rejection writes nothing: no evidence preview beyond the preflight.
            assert_eq!(rig.sap.borrow().previews, 2, "{text}");
        }
    }
}

#[test]
fn a_failed_insert_with_a_clean_undo_is_rejected_and_names_the_request_that_keeps_the_name() {
    let rig = rig(World::default());
    let failure = "RPY_TRANSACTION_INSERT failed (rc=4): name not allowed";
    let kept = format!("TRAN {TCODE} remains recorded in request DEVK900001");

    rig.reply(vec![create_reply(
        "CREATE",
        &[("E", failure), ("W", &kept)],
        "REJECTED",
    )]);

    let exit = run_create(&rig, HTTP, &spec(false));
    let shown = shown(&exit.stdout);

    assert_eq!(exit.code, 1);
    assert_eq!(shown[shown.len() - 2..], ["CREATE: REJECTED", failure]);
    assert_eq!(exit.stderr, format!("W: {kept}"));
}

#[test]
fn a_failed_insert_whose_undo_fails_is_partial_with_the_left_behind_text() {
    let rig = rig(World::default());
    let failure = "RPY_TRANSACTION_INSERT failed (rc=8): db access error";
    let left = format!("TADIR R3TR TRAN {TCODE} left behind without TSTC");

    rig.reply(vec![create_reply(
        "CREATE",
        &[("E", failure), ("E", &left)],
        "PARTIAL",
    )]);

    let exit = run_create(&rig, RFC, &spec(false));
    let shown = shown(&exit.stdout);

    assert_eq!(exit.code, 1);
    assert_eq!(shown[shown.len() - 2], "CREATE: PARTIAL");
    assert_eq!(shown[shown.len() - 1], format!("{failure}; {left}"));
    assert_eq!(rig.classes(), 0);
}

#[test]
fn an_incomplete_insert_is_inactive_and_exits_1() {
    let rig = rig(World::default());
    let text = format!("TRAN {TCODE} incomplete: TSTC-PGMNA= TADIR=$TMP");

    rig.reply(vec![create_reply("CREATE", &[("E", &text)], "INACTIVE")]);

    let exit = run_create(&rig, HTTP, &spec(false));
    let shown = shown(&exit.stdout);

    assert_eq!(exit.code, 1);
    assert_eq!(
        shown[shown.len() - 2..],
        ["CREATE: INACTIVE", text.as_str()]
    );
}

#[test]
fn an_active_run_that_the_new_session_does_not_confirm_is_partial() {
    let cases = [
        (
            "no definition row",
            World {
                definition: false,
                directory: true,
                ..World::default()
            },
            "definition TSTC has no row",
        ),
        (
            "no directory row",
            World {
                definition: true,
                directory: false,
                ..World::default()
            },
            "TADIR has no row",
        ),
    ];

    for (label, world, mismatch) in cases {
        let rig = rig(world);

        rig.reply(vec![create_reply("CREATE", &[], "ACTIVE")]);

        let exit = run_create(&rig, HTTP, &spec(false));
        let shown = shown(&exit.stdout);

        assert_eq!(exit.code, 1, "{label}");
        assert_eq!(shown[shown.len() - 2], "CREATE: PARTIAL", "{label}");
        assert_eq!(
            shown[shown.len() - 1],
            format!(
                "the run reported ACTIVE but a new session disagrees ({mismatch}); check with read before retrying"
            ),
            "{label}"
        );
    }
}

#[test]
fn the_new_session_compares_the_report() {
    let rig = rig(present());
    let mut create = spec(false);

    create.report = "ZOTHER";
    rig.reply(vec![create_reply("CREATE", &[], "ACTIVE")]);

    let exit = run_create(&rig, HTTP, &create);
    let shown = shown(&exit.stdout);

    assert_eq!(exit.code, 1);
    assert_eq!(shown[shown.len() - 2], "CREATE: PARTIAL");
    assert!(
        shown[shown.len() - 1].contains("TSTC PGMNA ZREPORT, expected ZOTHER"),
        "{shown:?}"
    );
}

#[test]
fn unreadable_evidence_proves_nothing_and_is_partial() {
    let rig = rig(World {
        broken: true,
        ..present()
    });

    rig.reply(vec![create_reply("CREATE", &[], "ACTIVE")]);

    let exit = run_create(&rig, HTTP, &spec(false));
    let shown = shown(&exit.stdout);

    assert_eq!(exit.code, 1);
    assert_eq!(shown[shown.len() - 2], "CREATE: PARTIAL");
    assert!(
        shown[shown.len() - 1]
            .starts_with("the run reported ACTIVE but a new session could not confirm it ("),
        "{shown:?}"
    );
}

// --- cleanup, signals, and the stage failures -------------------------------------------------

#[test]
fn a_class_that_stays_keeps_the_status_line_and_makes_the_exit_1() {
    let rig = rig(present());

    rig.reply(vec![create_reply("CREATE", &[], "ACTIVE")]);
    rig.sap.borrow_mut().failing_deletes = 2;

    let exit = run_create(&rig, HTTP, &spec(false));
    let shown = shown(&exit.stdout);

    assert_eq!(exit.code, 1);
    // The transaction itself is confirmed; only the class is left over.
    assert_eq!(shown[shown.len() - 2], "CREATE: ACTIVE");
    assert!(
        exit.stderr
            .contains("RUNNER_LEFTOVER: YTEST_ABAPREACH_RUN_")
    );
    assert!(exit.stderr.contains("abapreach delete --simulate CLAS:"));
    assert_eq!(rig.classes(), 1);
}

#[test]
fn a_signal_before_the_post_exits_130_without_a_run() {
    let rig = rig(present());
    let mut factory = Factory::new(&rig);

    factory.cancel.set();

    let exit = run_create_with(&rig, HTTP, &spec(false), &mut factory);

    assert_eq!(exit.code, 130);
    assert!(exit.stderr.contains("CANCELED"), "{}", exit.stderr);
    assert_eq!(rig.posts(), 0);
    assert_eq!(rig.classes(), 0);
}

#[test]
fn a_stop_of_the_cycle_is_exit_2() {
    let rig = rig(present());
    let mut factory = Factory::new(&rig);

    // A stateless HTTP session cannot hold the lock of the runner class.
    rig.sap.borrow_mut().stateful = false;

    let exit = run_create_with(&rig, HTTP, &spec(false), &mut factory);

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    assert_eq!(rig.posts(), 0);
}

// --- local rules before any connection --------------------------------------------------------

#[test]
fn a_value_the_payload_refuses_stops_before_any_connection() {
    let rig = rig(present());
    let cases = [
        (
            TranCreate {
                report: "SAPMXX",
                ..spec(false)
            },
            "write and delete accept only Z and Y names: SAPMXX",
        ),
        (
            TranCreate {
                text: None,
                ..spec(false)
            },
            "--text is required for this operation",
        ),
        (
            TranCreate {
                text: Some("a\nb"),
                ..spec(false)
            },
            "--text must be 1 to 36 printable characters",
        ),
    ];

    for (create, expected) in cases {
        let mut factory = Factory::new(&rig);
        let exit = run_create_with(&rig, HTTP, &create, &mut factory);

        assert_eq!(exit.code, 2);
        assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
        assert!(exit.stderr.contains(expected), "{}", exit.stderr);
        assert_eq!(factory.adt_opened + factory.rfc_opened, 0);
    }
}

#[test]
fn a_backend_the_connection_lacks_stops_before_any_connection() {
    let rig = rig(present());
    let mut connection = connection();

    connection.rfc = None;

    let mut factory = Factory::new(&rig);
    let mut nonce = || NONCE.to_owned();
    let mut clock = FakeClock {
        now: rig.now.clone(),
    };
    let exit = create_with(
        &connection,
        RFC,
        &spec(false),
        &mut factory,
        &mut nonce,
        &mut clock,
    );

    assert_eq!(exit.code, 2);
    assert_eq!(
        exit.stderr,
        "STOP: RFC SADT is not available for create TRAN"
    );
    assert_eq!(factory.adt_opened + factory.rfc_opened, 0);
}

#[test]
fn the_package_and_request_rules_are_the_ones_of_the_ddic_types() {
    use abapreach::runner_cli::{CreateOptions, check_create};

    let create = |package, tr| CreateOptions {
        text: Some(TEXT),
        package,
        tr,
        prog: Some(REPORT),
        ..CreateOptions::default()
    };

    assert!(check_create(Kind::Tran, TCODE, &create(None, None)).is_ok());
    assert!(check_create(Kind::Tran, TCODE, &create(Some("$TMP"), None)).is_ok());
    assert!(check_create(Kind::Tran, TCODE, &create(Some("ZPKG"), Some("DEVK900001"))).is_ok());
    assert_eq!(
        check_create(Kind::Tran, TCODE, &create(Some("ZPKG"), None)),
        Err("package ZPKG is transportable: --tr is required".to_owned())
    );
    assert_eq!(
        check_create(Kind::Tran, TCODE, &create(Some("$TMP"), Some("DEVK900001"))),
        Err("--tr is not valid for the local package $TMP".to_owned())
    );
}

#[test]
fn a_transaction_has_no_deploy() {
    let message = "TRAN has no deploy; use create or delete";

    assert_eq!(
        abapreach::runner_cli::check_deploy(Kind::Tran, TCODE, None, &Default::default()),
        Err(message.to_owned())
    );

    for mode in [Mode::Simulate, Mode::Deploy] {
        let request = PayloadRequest::new(mode, Kind::Tran, TCODE, NONCE);

        assert_eq!(encode(&request), Err(message.to_owned()));
    }
}

// --- the payload that reaches the class -------------------------------------------------------

fn base64_decode(text: &str) -> Vec<u8> {
    let value = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("byte {c} outside the base64 alphabet"),
        }
    };
    let mut out = Vec::new();

    for quad in text.as_bytes().chunks(4) {
        let pad = quad.iter().filter(|&&c| c == b'=').count();
        let mut acc = 0u32;

        for &c in &quad[..4 - pad] {
            acc = (acc << 6) | value(c);
        }

        acc <<= 6 * pad as u32;
        out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8][..3 - pad]);
    }

    out
}

/// The `KEY=VALUE` lines of the payload document inside the class source the POST ran.
fn payload_of(source: &str) -> Vec<String> {
    let encoded: String = source
        .lines()
        .filter_map(|line| {
            line.strip_prefix("    lv_b64 = lv_b64 && '")?
                .strip_suffix("'.")
        })
        .collect();

    String::from_utf8(base64_decode(&encoded))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Runs `create` and returns the payload of the class that the (first) POST ran.
fn payload_of_run(create: &TranCreate<'_>) -> Vec<String> {
    let rig = rig(present());
    let mut factory = Factory::new(&rig);

    rig.reply(vec![create_reply(
        if create.simulate {
            "CREATE_SIM"
        } else {
            "CREATE"
        },
        &[],
        if create.simulate {
            "SIMULATED_OK"
        } else {
            "ACTIVE"
        },
    )]);

    run_create_with(&rig, HTTP, create, &mut factory);

    let source = factory
        .puts
        .borrow()
        .first()
        .cloned()
        .expect("a class source");

    payload_of(&source)
}

#[test]
fn the_payload_carries_exactly_the_fields_of_the_mode() {
    let payload = payload_of_run(&spec(false));

    assert_eq!(
        payload,
        [
            "FORMAT=1".to_owned(),
            format!("NONCE={NONCE}"),
            "MODE=CREATE".to_owned(),
            "TYPE=TRAN".to_owned(),
            format!("NAME={TCODE}"),
            format!("REPORT={REPORT}"),
            format!("TEXT={TEXT}"),
        ]
    );
    // The package of a simulate and the request of a transportable package travel too.
    let mut create = spec(true);

    create.package = Some("zpkg");
    create.tr = Some("devk900001");

    let payload = payload_of_run(&create);

    assert!(
        payload.contains(&"MODE=CREATE_SIM".to_owned()),
        "{payload:?}"
    );
    assert!(payload.contains(&"PACKAGE=ZPKG".to_owned()), "{payload:?}");
    assert!(payload.contains(&"TR=DEVK900001".to_owned()), "{payload:?}");
    assert!(!payload.iter().any(|entry| entry.starts_with("PLAN=")));
}

// --- routing ----------------------------------------------------------------------------------

#[test]
fn without_a_pin_adt_comes_first_and_rfc_takes_over_when_nothing_was_sent() {
    let rig = rig(present());
    let mut factory = Factory::new(&rig);

    factory.adt_fails = true;
    rig.reply(vec![create_reply("CREATE", &[], "ACTIVE")]);

    let exit = run_create_with(&rig, None, &spec(false), &mut factory);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!((factory.adt_opened, factory.rfc_opened > 0), (1, true));
    assert!(
        exit.stdout
            .starts_with("backend: rfc\nreason: fallback to RFC: "),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stderr.contains(
            "NOTE: ADT HTTP failed before anything was written; falling back to RFC SADT"
        ),
        "{}",
        exit.stderr
    );
}

#[test]
fn a_pinned_backend_never_falls_back() {
    let rig = rig(present());
    let mut factory = Factory::new(&rig);

    factory.adt_fails = true;

    let exit = run_create_with(&rig, HTTP, &spec(false), &mut factory);

    assert_eq!(exit.code, 1);
    assert!(exit.stderr.contains("ERROR: "), "{}", exit.stderr);
    assert_eq!(factory.rfc_opened, 0);
}

#[test]
fn a_connection_without_an_adt_endpoint_runs_on_rfc() {
    let rig = rig(present());
    let mut connection = connection();

    connection.adt = None;

    let mut factory = Factory::new(&rig);
    let mut nonce = || NONCE.to_owned();
    let mut clock = FakeClock {
        now: rig.now.clone(),
    };

    rig.reply(vec![create_reply("CREATE", &[], "ACTIVE")]);

    let exit = create_with(
        &connection,
        None,
        &spec(false),
        &mut factory,
        &mut nonce,
        &mut clock,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(factory.adt_opened, 0);
    assert!(exit.stdout.contains("reason: RFC-only connection"));
}

// --- an uncertain create ----------------------------------------------------------------------

#[test]
fn a_lost_create_reply_is_settled_from_the_evidence_and_never_resent() {
    let cases = [
        (
            present(),
            "CREATE: PARTIAL; the object exists after an uncertain create; check with read before retrying",
        ),
        (
            World::default(),
            "CREATE: REJECTED; the object does not exist after an uncertain create; check with read before retrying",
        ),
    ];

    for (backend, _, _) in BACKENDS {
        for (world, expected) in cases {
            let rig = rig(world);

            rig.reply(vec![PostReply::Lost]);

            let exit = run_create(&rig, backend, &spec(false));

            assert_eq!(exit.code, 1, "{backend:?}");
            assert!(
                shown(&exit.stdout).contains(&expected.to_owned()),
                "{backend:?} {}",
                exit.stdout
            );
            assert!(exit.stderr.contains("W: the classrun status is uncertain"));
            // The create POST is never resent, and the previews were enough: no READ run.
            assert_eq!(rig.posts(), 1, "{backend:?}");
            assert_eq!(rig.classes(), 0);
        }
    }
}

#[test]
fn a_5xx_create_reply_is_uncertain_too() {
    let rig = rig(present());

    rig.reply(vec![PostReply::Status(502, String::new())]);

    let exit = run_create(&rig, HTTP, &spec(false));

    assert_eq!(exit.code, 1);
    assert!(
        shown(&exit.stdout).contains(
            &"CREATE: PARTIAL; the object exists after an uncertain create; check with read before retrying"
                .to_owned()
        ),
        "{}",
        exit.stdout
    );
    assert_eq!(rig.posts(), 1);
}

#[test]
fn unreadable_previews_make_one_extra_read_run_that_decides() {
    let cases = [
        (
            PostReply::Output(read_ok()),
            "CREATE: PARTIAL; the object exists after an uncertain create; check with read before retrying",
        ),
        (
            create_reply(
                "READ",
                &[("E", "Transaction X does not exist (TSTC)")],
                "NOT_FOUND",
            ),
            "CREATE: REJECTED; the object does not exist after an uncertain create; check with read before retrying",
        ),
    ];

    for (reply, expected) in cases {
        let rig = rig(World {
            broken: true,
            ..World::default()
        });

        rig.reply(vec![PostReply::Lost, reply]);

        let exit = run_create(&rig, HTTP, &spec(false));
        let lines = shown(&exit.stdout);

        assert_eq!(exit.code, 1);
        assert!(lines.contains(&expected.to_owned()), "{lines:?}");
        // The create and the one READ run; each has its own RUNNER line.
        assert_eq!(rig.posts(), 2);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("RUNNER: <RUNNER> "))
                .count(),
            2,
            "{lines:?}"
        );
        assert_eq!(rig.classes(), 0);
    }
}

/// The first Ctrl-C that arrives in the extra `READ` run of the reconciliation is deferred, so the
/// verdict is still printed, and it ends the command with 130 (the run itself ended without it).
#[test]
fn a_ctrl_c_during_the_reconciliation_exits_130_and_still_prints_the_verdict() {
    let rig = rig(World {
        broken: true,
        ..World::default()
    });
    let mut factory = Factory::new(&rig);
    let signal = factory.cancel.clone();
    let mut posts = 0;

    rig.reply(vec![PostReply::Lost, PostReply::Output(read_ok())]);
    rig.sap.borrow_mut().on_post = Some(Box::new(move || {
        posts += 1;

        // The second classrun POST is the extra READ run of the reconciliation.
        if posts == 2 {
            assert_eq!(signal.on_interrupt(), InterruptAction::Deferred);
        }
    }));

    let exit = run_create_with(&rig, HTTP, &spec(false), &mut factory);
    let lines = shown(&exit.stdout);

    assert_eq!(exit.code, 130, "{}", exit.stderr);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("CREATE: PARTIAL; ")),
        "{lines:?}"
    );
    assert_eq!(rig.posts(), 2);
}

#[test]
fn a_read_run_that_fails_leaves_the_create_incomplete() {
    let rig = rig(World {
        broken: true,
        ..World::default()
    });

    rig.reply(vec![PostReply::Lost, PostReply::Lost]);

    let exit = run_create(&rig, HTTP, &spec(false));

    assert_eq!(exit.code, 1);
    assert!(
        exit.stdout.contains("PARTIAL: reconciliation incomplete"),
        "{}",
        exit.stdout
    );
    assert_eq!(rig.posts(), 2, "no third POST");
}

#[test]
fn read_refuses_a_line_that_could_forge_another_one() {
    let rig = rig(present());
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("tran.txt");
    let mut lines = live_lines();

    lines[4] = "text=a\nprogram=ZEVIL".to_owned();

    let body: Vec<String> = lines.iter().map(|text| line(text)).collect();

    rig.reply(vec![PostReply::Output(output("READ", &body, "OK"))]);

    let exit = run_read(&rig, HTTP, &out);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stderr
            .ends_with("ERROR: the runner returned a line with a control character"),
        "{}",
        exit.stderr
    );
    assert!(!out.exists());
}

// --- the command line -------------------------------------------------------------------------

/// Factories whose every connection fails before anything is sent.
#[derive(Default)]
struct Unreachable {
    opened: usize,
}

impl Factories for Unreachable {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.opened += 1;

        Err(TransportError::PreSendConnectivity("no route".to_owned()))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.opened += 1;

        Err(TransportError::PreSendConnectivity("no route".to_owned()))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        Err("no library in this test".to_owned())
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        Ok(())
    }
}

#[test]
fn every_valid_tran_form_reaches_the_connection_instead_of_a_stop() {
    let name20 = "Z".repeat(20);
    let text36 = "t".repeat(36);
    let forms: Vec<Vec<&str>> = vec![
        vec!["read", "TRAN", "ZTRAN", "out.txt"],
        vec!["read", "TRAN", &name20, "-", "--force", "--backend", "rfc"],
        vec![
            "create",
            "TRAN",
            "ZTRAN",
            "--prog",
            "ZREPORT",
            "--text",
            "A transaction",
        ],
        vec![
            "create",
            "--simulate",
            "TRAN",
            &name20,
            "--prog",
            "ZREPORT",
            "--text",
            &text36,
            "--package",
            "ZPKG",
            "--tr",
            "DEVK900001",
            "--backend",
            "adt",
        ],
        vec![
            "create",
            "TRAN",
            "ZTRAN",
            "--prog",
            "ZREPORT",
            "--text",
            "T",
            "--backend",
            "rfc",
        ],
    ];
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"},"adt":{"insecure":true}}]}"#,
    );

    for form in forms {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connections.json");

        std::fs::write(&path, config).unwrap();

        let env = std::collections::BTreeMap::from([
            ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
            ("DEV_100_USER".to_owned(), "user".to_owned()),
            ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
        ]);
        let mut args = vec!["abapreach".to_owned()];
        let mut factory = Unreachable::default();

        args.extend(form.iter().map(|argument| (*argument).to_owned()));
        args.extend(["--conn".to_owned(), "DEV_100".to_owned()]);

        let exit = abapreach::run(args, env, &mut factory);

        assert_eq!(exit.code, 1, "{form:?}: {}", exit.stderr);
        assert!(exit.stderr.contains("ERROR: "), "{form:?}: {}", exit.stderr);
        assert!(!exit.stderr.contains("STOP: "), "{form:?}: {}", exit.stderr);
        assert!(factory.opened > 0, "{form:?}");
    }
}
