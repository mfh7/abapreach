//! `delete` of the DDIC types through the temporary runner class (M6 Task 23): the simulation that
//! writes the plan, the real delete that sends the plan hash to the run, the batch order, every
//! status of a row, the stop rules, the check from a new session, and the mix with an M4 row.
//! A fake SAP answers the classrun POST with scripted, nonce-bound output. No SAP.

mod common;

use abapreach::backend::BackendKind;
use abapreach::cli::{DeleteKind, DeleteTarget};
use abapreach::domain::Outcome;
use abapreach::operation::delete::{
    self, Context, DeleteBackend, DeleteReply, ObjectFacts, Plan, Reconcile, RowPlan,
};
use abapreach::operation::delete_ddic::{DdicDelete, DdicEnv, RunnerBatch};
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::payload::Kind;
use abapreach::operation::runner_reconcile::Clock;
use abapreach::operation::runner_template::{
    DTEL_TEMPLATE, TRAN_TEMPLATE, TypeTemplate, template_lines,
};
use abapreach::policy::{Confirmer, Policies};
use abapreach::transport::TransportError;
use common::runner_fake::{
    Evolution, Opened, PostReply, Shared, Tick, World, http_session, new_sap, rfc_session,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HASH_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const PREFIX: &str = "YTEST_ABAPREACH_RUN_";

#[derive(Clone, Copy, Debug)]
enum Backend {
    Http,
    Rfc,
}

const BACKENDS: [Backend; 2] = [Backend::Http, Backend::Rfc];

impl Backend {
    fn kind(self) -> BackendKind {
        match self {
            Self::Http => BackendKind::AdtHttp,
            Self::Rfc => BackendKind::RfcSadt,
        }
    }

    fn post(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Rfc => "sadt",
        }
    }
}

// --- scripted classrun output ----------------------------------------------------------------

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

/// A whole run in the classrun framing: BEGIN, the body, STATUS, END.
fn output(mode: &str, body: &[String], status: &str) -> String {
    let mut lines = vec![format!("ABAPREACH:{NONCE}:BEGIN:{mode}")];

    lines.extend(body.iter().map(|rest| format!("ABAPREACH:{NONCE}:{rest}")));
    lines.push(format!("ABAPREACH:{NONCE}:STATUS:{status}"));

    let count = lines.len();

    lines.push(format!("ABAPREACH:{NONCE}:END:{count}"));

    lines.iter().map(|line| format!("{line}\n\n")).collect()
}

fn fact(key: &str, value: &str) -> String {
    format!("FACT:{key}:{}", hex(value))
}

fn msg(level: &str, text: &str) -> String {
    format!("MSG:{level}:{}", hex(text))
}

/// The body of a successful `DELETE_SIM`: package, author, request, state, hash.
fn sim_body(hash: &str) -> Vec<String> {
    vec![
        fact("PACKAGE", "$TMP"),
        fact("AUTHOR", "DEVUSER"),
        fact("REQUEST", "-"),
        fact("STATE", "ACTIVE"),
        fact("HASH", hash),
    ]
}

fn sim_ok(hash: &str) -> String {
    output("DELETE_SIM", &sim_body(hash), "SIMULATED_OK")
}

fn sim_rejected(text: &str) -> String {
    output("DELETE_SIM", &[msg("E", text)], "REJECTED")
}

fn deleted() -> String {
    output("DELETE", &[], "DELETED")
}

fn delete_rejected(text: &str) -> String {
    output("DELETE", &[msg("E", text)], "REJECTED")
}

// --- the world and the clock -----------------------------------------------------------------

fn present() -> World {
    World {
        definition: true,
        directory: true,
        ..World::default()
    }
}

/// The object is there until a classrun POST ran, and gone afterwards.
fn gone_after_post() -> Evolution {
    Box::new(|tick: &Tick| {
        if tick.posts == 0 {
            present()
        } else {
            World::default()
        }
    })
}

fn constant(world: World) -> Evolution {
    Box::new(move |_| world)
}

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

struct Rig {
    sap: Shared,
    now: Rc<Cell<Duration>>,
}

fn rig(world: Evolution, replies: Vec<PostReply>) -> Rig {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let sap = new_sap(true, &now, world);

    sap.borrow_mut().post_replies = replies.into();

    Rig { sap, now }
}

fn opened(backend: Backend, sap: &Shared) -> Opened {
    match backend {
        Backend::Http => http_session(sap),
        Backend::Rfc => rfc_session(sap),
    }
}

fn env(backend: Backend) -> DdicEnv {
    DdicEnv {
        sid: "DEV".to_owned(),
        client: "100".to_owned(),
        conn: "dev".to_owned(),
        prefix: PREFIX.to_owned(),
        user: "DEVUSER".to_owned(),
        language: "EN".to_owned(),
        backend: backend.kind(),
        rfc_available: true,
    }
}

fn targets(texts: &[&str]) -> Vec<DeleteTarget> {
    texts
        .iter()
        .map(|text| text.parse().expect("target"))
        .collect()
}

struct No;

impl Confirmer for No {
    fn interactive(&self) -> bool {
        false
    }

    fn confirm(&self, _prompt: &str) -> bool {
        false
    }
}

struct Done {
    outcome: Outcome,
    plan_path: std::path::PathBuf,
    _directory: tempfile::TempDir,
}

/// Runs a batch of DDIC rows through the engine and the DDIC backend, like the CLI does.
fn batch(
    rig: &Rig,
    backend: Backend,
    texts: &[&str],
    tr: Option<&str>,
    simulate: bool,
    plan: Option<&Plan>,
    cancel: &CancelFlag,
) -> Done {
    let directory = tempfile::tempdir().unwrap();
    let plan_path = directory.path().join("plan.json");

    if let Some(plan) = plan {
        std::fs::write(&plan_path, serde_json::to_string(plan).unwrap()).unwrap();
    }

    let policies = Policies::default();
    let context = Context {
        conn: "dev",
        user: "DEVUSER",
        policies: &policies,
        confirmer: &No,
    };
    let sap = rig.sap.clone();
    let mut open = move || opened(backend, &sap);
    let mut ddic = DdicDelete::new(env(backend), &mut open, cancel.clone())
        .with_nonce(Box::new(|| NONCE.to_owned()))
        .with_clock(Box::new(FakeClock {
            now: rig.now.clone(),
        }));
    let mut outcome = delete::run_until(
        &mut ddic,
        &context,
        &targets(texts),
        tr,
        simulate,
        &plan_path,
        &mut || cancel.is_set(),
    );

    ddic.finish(&mut outcome);

    Done {
        outcome,
        plan_path,
        _directory: directory,
    }
}

fn simulate_batch(rig: &Rig, backend: Backend, texts: &[&str], tr: Option<&str>) -> Done {
    batch(rig, backend, texts, tr, true, None, &CancelFlag::new())
}

fn plan_of(done: &Done) -> Plan {
    delete::load_plan(&done.plan_path).expect("plan file")
}

fn delete_batch(
    rig: &Rig,
    backend: Backend,
    texts: &[&str],
    tr: Option<&str>,
    plan: &Plan,
) -> Done {
    batch(
        rig,
        backend,
        texts,
        tr,
        false,
        Some(plan),
        &CancelFlag::new(),
    )
}

// --- the payload that reached the class ------------------------------------------------------

fn decode_base64(text: &str) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;

    for character in text.bytes().filter(|byte| *byte != b'=') {
        let value = alphabet
            .iter()
            .position(|known| *known == character)
            .expect("base64 character") as u32;

        buffer = (buffer << 6) | value;
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }

    String::from_utf8(bytes).expect("UTF-8 payload")
}

/// The decoded payload document of the class a POST ran.
fn payload_of(source: &str) -> Vec<String> {
    let encoded: String = source
        .lines()
        .filter_map(|line| {
            line.strip_prefix("    lv_b64 = lv_b64 && '")?
                .strip_suffix("'.")
        })
        .collect();

    decode_base64(&encoded).lines().map(str::to_owned).collect()
}

fn posted(rig: &Rig) -> Vec<Vec<String>> {
    rig.sap
        .borrow()
        .posted
        .iter()
        .map(|s| payload_of(s))
        .collect()
}

fn value<'a>(payload: &'a [String], key: &str) -> Option<&'a str> {
    payload
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
}

fn batch_of(payload: &[String]) -> Vec<&str> {
    payload
        .iter()
        .filter_map(|line| line.strip_prefix("BATCH="))
        .collect()
}

fn clean(rig: &Rig) {
    let sap = rig.sap.borrow();

    assert!(sap.classes.is_empty(), "every runner class is removed");
}

// --- tests -----------------------------------------------------------------------------------

#[test]
fn a_simulate_prints_the_plan_line_the_marker_and_what_the_run_reported() {
    for backend in BACKENDS {
        let body = {
            let mut body = sim_body(HASH_A);

            body.push(msg("I", "WHERE-USED TABL ZTABL (transparent)"));
            body.push(msg("I", "WHERE-USED ZPROGRAM"));
            body.push(msg("W", "Request DEVK900002 is ignored"));
            body.push(fact("GUARD", "Customer names only (Z or Y)"));
            body.push(fact("GUARD", "Rows of a transparent table"));
            output("DELETE_SIM", &body, "SIMULATED_OK")
        };
        let rig = rig(constant(present()), vec![PostReply::Output(body)]);
        let done = simulate_batch(&rig, backend, &["DTEL:zdtel"], None);
        let out = &done.outcome.stdout;

        assert_eq!(done.outcome.exit, 0, "{backend:?} {out}");
        assert_eq!(done.outcome.stderr, "", "{backend:?}");

        for expected in [
            "== DTEL ZDTEL ==",
            "RUNNER: preflight T000",
            "(temporary, $TMP, post via",
            "DELETE DTEL ZDTEL package $TMP author DEVUSER TR -",
            &format!("SOURCE XML {HASH_A}"),
            "WHERE-USED TABL ZTABL (transparent)",
            "WHERE-USED ZPROGRAM",
            "W: Request DEVK900002 is ignored",
            "guards active: Customer names only (Z or Y); Rows of a transparent table",
            &format!("plan hash {HASH_A}"),
            "DELETE_SIM: SIMULATED_OK",
            "SUMMARY: 1 OK, 0 FAILED, 0 SKIPPED",
        ] {
            assert!(out.contains(expected), "{backend:?} {expected} in {out}");
        }

        assert!(
            out.contains(&format!("post via {},", backend.post())),
            "{backend:?} {out}"
        );
        // The guards of the run replace the two header lines of the ADT route.
        assert!(!out.contains("guards not applied"), "{out}");

        let plan = plan_of(&done);

        assert_eq!(
            plan.objects,
            [("DTEL".to_owned(), "ZDTEL".to_owned(), HASH_A.to_owned())]
        );
        assert!(plan.backends.is_empty());

        let payloads = posted(&rig);

        assert_eq!(payloads.len(), 1);
        assert_eq!(value(&payloads[0], "MODE"), Some("DELETE_SIM"));
        assert_eq!(value(&payloads[0], "TYPE"), Some("DTEL"));
        assert_eq!(value(&payloads[0], "NAME"), Some("ZDTEL"));
        assert_eq!(value(&payloads[0], "PLAN"), None);
        assert_eq!(batch_of(&payloads[0]), ["DTEL:ZDTEL"]);
        clean(&rig);
    }
}

#[test]
fn a_definition_without_an_active_version_carries_the_marker() {
    let mut body = sim_body(HASH_B);

    body[3] = fact("STATE", "NO_ACTIVE");

    let rig = rig(
        constant(present()),
        vec![PostReply::Output(output(
            "DELETE_SIM",
            &body,
            "SIMULATED_OK",
        ))],
    );
    let done = simulate_batch(&rig, Backend::Rfc, &["DOMA:ZDOMA"], None);

    assert!(done.outcome.stdout.contains("SOURCE XML NO_ACTIVE"));
    assert!(
        !done
            .outcome
            .stdout
            .contains(&format!("SOURCE XML {HASH_B}"))
    );
    // The plan hash is still the hash the run reported for the (absent) definition.
    assert_eq!(plan_of(&done).objects[0].2, HASH_B);
}

#[test]
fn the_plan_hash_follows_the_definition_the_run_reports() {
    let first = rig(constant(present()), vec![PostReply::Output(sim_ok(HASH_A))]);
    let second = rig(constant(present()), vec![PostReply::Output(sim_ok(HASH_B))]);
    let one = simulate_batch(&first, Backend::Rfc, &["TTYP:ZT"], None);
    let two = simulate_batch(&second, Backend::Rfc, &["TTYP:ZT"], None);

    assert_eq!(plan_of(&one).objects[0].2, HASH_A);
    assert_eq!(plan_of(&two).objects[0].2, HASH_B);
}

#[test]
fn a_mixed_batch_runs_in_the_python_order_and_every_run_knows_the_whole_batch() {
    let rig = rig(
        constant(present()),
        vec![
            PostReply::Output(sim_ok(HASH_A)),
            PostReply::Output(sim_ok(HASH_B)),
            PostReply::Output(sim_ok(HASH_C)),
            PostReply::Output(sim_ok(HASH_A)),
        ],
    );
    let done = simulate_batch(
        &rig,
        Backend::Rfc,
        &["DOMA:ZDOMA", "DTEL:ZDTEL", "TABL:ZTABL", "TTYP:ZTTYP"],
        Some("devk900001"),
    );
    let payloads = posted(&rig);
    let order = ["TTYP:ZTTYP", "TABL:ZTABL", "DTEL:ZDTEL", "DOMA:ZDOMA"];

    assert_eq!(done.outcome.exit, 0, "{}", done.outcome.stdout);
    assert_eq!(payloads.len(), 4);

    for (payload, expected) in payloads.iter().zip(order) {
        let (kind, name) = expected.split_once(':').unwrap();

        assert_eq!(value(payload, "TYPE"), Some(kind));
        assert_eq!(value(payload, "NAME"), Some(name));
        assert_eq!(batch_of(payload), order, "the batch list is in run order");
        assert_eq!(value(payload, "TR"), Some("DEVK900001"));
    }

    assert_eq!(plan_of(&done).tr.as_deref(), Some("DEVK900001"));
    clean(&rig);
}

#[test]
fn rejected_and_missing_rows_let_the_next_row_run() {
    let rig = rig(
        constant(present()),
        vec![
            PostReply::Output(sim_rejected("Table ZTA contains 3 rows; delete rejected")),
            PostReply::Output(output(
                "DELETE_SIM",
                &[msg("E", "Object TABL ZTB does not exist")],
                "NOT_FOUND",
            )),
            PostReply::Output(sim_rejected(
                "TABL ZTC still in use: TABL ZTX; delete rejected",
            )),
            PostReply::Output(sim_rejected(
                "Batch order: delete users ZTE first before TABL ZTD",
            )),
            PostReply::Output(sim_ok(HASH_A)),
        ],
    );
    let done = simulate_batch(
        &rig,
        Backend::Http,
        &["TABL:ZTA", "TABL:ZTB", "TABL:ZTC", "TABL:ZTD", "TABL:ZTF"],
        None,
    );
    let out = &done.outcome.stdout;

    assert_eq!(done.outcome.exit, 1);
    assert_eq!(rig.sap.borrow().posts, 5);

    for expected in [
        "ERROR: Table ZTA contains 3 rows; delete rejected",
        "DELETE_SIM: NOT_FOUND",
        "ERROR: TABL ZTC still in use: TABL ZTX; delete rejected",
        "ERROR: Batch order: delete users ZTE first before TABL ZTD",
        "DELETE_SIM: SIMULATED_OK",
        "SUMMARY: 1 OK, 4 FAILED, 0 SKIPPED",
    ] {
        assert!(out.contains(expected), "{expected} in {out}");
    }

    // Every row, also a rejected one, shows the class it ran in.
    assert_eq!(out.matches("RUNNER: YTEST_ABAPREACH_RUN_").count(), 5);
    clean(&rig);
}

#[test]
fn a_real_delete_sends_the_plan_hash_and_the_request_and_proves_it_from_a_new_session() {
    for backend in BACKENDS {
        let rig = rig(
            gone_after_post(),
            vec![PostReply::Output(deleted()), PostReply::Output(deleted())],
        );
        let plan = Plan {
            conn: "dev".to_owned(),
            tr: Some("DEVK900001".to_owned()),
            objects: vec![
                ("TABL".to_owned(), "ZTABL".to_owned(), HASH_B.to_owned()),
                ("DTEL".to_owned(), "ZDTEL".to_owned(), HASH_A.to_owned()),
            ],
            backends: Vec::new(),
        };
        let done = delete_batch(
            &rig,
            backend,
            &["DTEL:ZDTEL", "TABL:ZTABL"],
            Some("DEVK900001"),
            &plan,
        );
        let out = &done.outcome.stdout;
        let payloads = posted(&rig);

        assert_eq!(done.outcome.exit, 0, "{backend:?} {out}");
        assert_eq!(done.outcome.stderr, "");
        assert!(out.contains("DELETE: DELETED"), "{out}");
        assert!(out.contains("SUMMARY: 2 OK, 0 FAILED, 0 SKIPPED"), "{out}");
        // One run per row: the delete itself, in the order TABL before DTEL.
        assert_eq!(payloads.len(), 2);
        assert_eq!(value(&payloads[0], "MODE"), Some("DELETE"));
        assert_eq!(value(&payloads[0], "NAME"), Some("ZTABL"));
        assert_eq!(value(&payloads[0], "PLAN"), Some(HASH_B));
        assert_eq!(value(&payloads[0], "TR"), Some("DEVK900001"));
        assert_eq!(value(&payloads[1], "NAME"), Some("ZDTEL"));
        assert_eq!(value(&payloads[1], "PLAN"), Some(HASH_A));
        assert_eq!(batch_of(&payloads[1]), ["TABL:ZTABL", "DTEL:ZDTEL"]);
        // The new-session check read the definition table, TADIR, and the runner class.
        assert!(rig.sap.borrow().count("datapreview") >= 6);
        clean(&rig);
    }
}

#[test]
fn a_plan_that_changed_since_the_simulate_is_rejected_by_the_run_and_the_batch_goes_on() {
    let rig = rig(
        gone_after_post(),
        vec![
            PostReply::Output(delete_rejected(
                "Plan changed since simulate; simulate again",
            )),
            PostReply::Output(deleted()),
        ],
    );
    let plan = Plan {
        conn: "dev".to_owned(),
        tr: None,
        objects: vec![
            ("TABL".to_owned(), "ZTA".to_owned(), HASH_A.to_owned()),
            ("TABL".to_owned(), "ZTB".to_owned(), HASH_B.to_owned()),
        ],
        backends: Vec::new(),
    };
    let done = delete_batch(&rig, Backend::Rfc, &["TABL:ZTA", "TABL:ZTB"], None, &plan);
    let out = &done.outcome.stdout;

    assert_eq!(done.outcome.exit, 1);
    assert!(
        out.contains("ERROR: Plan changed since simulate; simulate again"),
        "{out}"
    );
    assert!(out.contains("SUMMARY: 1 OK, 1 FAILED, 0 SKIPPED"), "{out}");
    assert_eq!(rig.sap.borrow().posts, 2);
}

#[test]
fn a_table_with_rows_is_rechecked_in_the_delete_run() {
    let rig = rig(
        constant(present()),
        vec![PostReply::Output(delete_rejected(
            "Table ZT contains 7 rows; delete rejected",
        ))],
    );
    let plan = Plan {
        conn: "dev".to_owned(),
        tr: None,
        objects: vec![("TABL".to_owned(), "ZT".to_owned(), HASH_A.to_owned())],
        backends: Vec::new(),
    };
    let done = delete_batch(&rig, Backend::Rfc, &["TABL:ZT"], None, &plan);

    assert_eq!(done.outcome.exit, 1);
    assert!(
        done.outcome
            .stdout
            .contains("ERROR: Table ZT contains 7 rows; delete rejected"),
        "{}",
        done.outcome.stdout
    );
}

#[test]
fn a_plan_that_does_not_match_the_arguments_stops_before_any_connection() {
    let rig = rig(constant(present()), Vec::new());
    let plan = Plan {
        conn: "dev".to_owned(),
        tr: None,
        objects: vec![("DTEL".to_owned(), "ZDTEL".to_owned(), HASH_A.to_owned())],
        backends: Vec::new(),
    };

    for (texts, tr) in [
        (vec!["DTEL:ZDTEL", "DOMA:ZDOMA"], None),
        (vec!["DOMA:ZDTEL"], None),
        (vec!["DTEL:ZDTEL"], Some("DEVK900001")),
    ] {
        let done = delete_batch(&rig, Backend::Rfc, &texts, tr, &plan);

        assert_eq!(done.outcome.exit, 2, "{texts:?}");
        assert_eq!(
            done.outcome.stderr,
            "STOP: plan does not match the arguments (conn/objects/--tr); simulate again"
        );
    }

    assert_eq!(rig.sap.borrow().sessions, 0, "nothing was opened");
}

#[test]
fn a_run_that_deleted_but_a_new_session_still_sees_the_object_is_partial_and_stops() {
    let cases = [
        (constant(present()), "the new session"),
        (
            constant(World {
                broken: true,
                ..World::default()
            }),
            "could not confirm",
        ),
    ];

    for (world, expected) in cases {
        let rig = rig(world, vec![PostReply::Output(deleted())]);
        let plan = Plan {
            conn: "dev".to_owned(),
            tr: None,
            objects: vec![
                ("TABL".to_owned(), "ZTA".to_owned(), HASH_A.to_owned()),
                ("DTEL".to_owned(), "ZTB".to_owned(), HASH_B.to_owned()),
            ],
            backends: Vec::new(),
        };
        let done = delete_batch(&rig, Backend::Rfc, &["TABL:ZTA", "DTEL:ZTB"], None, &plan);
        let out = &done.outcome.stdout;

        assert_eq!(done.outcome.exit, 1);
        assert!(out.contains("DELETE: PARTIAL"), "{out}");
        assert!(out.contains(expected), "{expected} in {out}");
        assert!(out.contains("SKIPPED DTEL ZTB"), "{out}");
        assert_eq!(rig.sap.borrow().posts, 1);
    }
}

#[test]
fn the_markers_of_a_partial_delete_stop_the_batch() {
    for marker in ["PARTIAL_DELETE:{}", "OBJECT_STILL_EXISTS:{}"] {
        let line = marker.replace("{}", &hex("TTYP ZA may have been deleted in part"));
        let rig = rig(
            constant(present()),
            vec![PostReply::Output(output("DELETE", &[line], "REJECTED"))],
        );
        let plan = Plan {
            conn: "dev".to_owned(),
            tr: None,
            objects: vec![
                ("TTYP".to_owned(), "ZA".to_owned(), HASH_A.to_owned()),
                ("DOMA".to_owned(), "ZB".to_owned(), HASH_B.to_owned()),
            ],
            backends: Vec::new(),
        };
        let done = delete_batch(&rig, Backend::Http, &["TTYP:ZA", "DOMA:ZB"], None, &plan);
        let out = &done.outcome.stdout;

        assert_eq!(done.outcome.exit, 1, "{marker}");
        assert!(out.contains("DELETE: PARTIAL"), "{marker} {out}");
        assert!(
            out.contains("TTYP ZA may have been deleted in part"),
            "{out}"
        );
        assert!(out.contains("SKIPPED DOMA ZB"), "{out}");
        assert_eq!(rig.sap.borrow().posts, 1);
    }
}

#[test]
fn a_leftover_class_keeps_the_row_status_exits_1_and_skips_the_rest() {
    for backend in BACKENDS {
        let rig = rig(
            gone_after_post(),
            vec![PostReply::Output(deleted()), PostReply::Output(deleted())],
        );

        rig.sap.borrow_mut().failing_deletes = 10;

        let plan = Plan {
            conn: "dev".to_owned(),
            tr: None,
            objects: vec![
                ("TABL".to_owned(), "ZTA".to_owned(), HASH_A.to_owned()),
                ("DTEL".to_owned(), "ZTB".to_owned(), HASH_B.to_owned()),
            ],
            backends: Vec::new(),
        };
        let done = delete_batch(&rig, backend, &["TABL:ZTA", "DTEL:ZTB"], None, &plan);
        let out = &done.outcome.stdout;
        let err = &done.outcome.stderr;

        assert_eq!(done.outcome.exit, 1, "{backend:?} {out} {err}");
        assert!(out.contains("DELETE: DELETED"), "{out}");
        assert!(out.contains("SKIPPED DTEL ZTB"), "{out}");
        assert!(
            err.contains("RUNNER_LEFTOVER: YTEST_ABAPREACH_RUN_"),
            "{err}"
        );
        assert!(err.contains("abapreach delete --simulate CLAS:"), "{err}");
        assert!(err.contains("--conn dev"), "{err}");
        assert_eq!(rig.sap.borrow().posts, 1);
    }
}

#[test]
fn a_lost_reply_of_the_delete_is_settled_once_from_a_new_session_and_never_resent() {
    for backend in BACKENDS {
        for gone in [true, false] {
            let (world, expected) = if gone {
                (gone_after_post(), "DELETED (reconciled)")
            } else {
                (constant(present()), "DELETE: PARTIAL")
            };
            let rig = rig(world, vec![PostReply::Lost]);
            let plan = Plan {
                conn: "dev".to_owned(),
                tr: None,
                objects: vec![
                    ("TABL".to_owned(), "ZTA".to_owned(), HASH_A.to_owned()),
                    ("DTEL".to_owned(), "ZTB".to_owned(), HASH_B.to_owned()),
                ],
                backends: Vec::new(),
            };
            let done = delete_batch(&rig, backend, &["TABL:ZTA", "DTEL:ZTB"], None, &plan);
            let out = &done.outcome.stdout;

            assert_eq!(done.outcome.exit, 1, "{backend:?} {out}");
            assert!(out.contains(expected), "{expected} in {out}");
            assert!(out.contains("SKIPPED DTEL ZTB"), "{out}");
            assert_eq!(rig.sap.borrow().posts, 1, "the POST is never resent");
            assert!(!done.outcome.stderr.contains("RUNNER_LEFTOVER"));
            clean(&rig);
        }
    }
}

#[test]
fn a_missing_classrun_handler_is_a_stop_with_exit_2() {
    let rig = rig(
        constant(present()),
        vec![PostReply::Status(404, String::new())],
    );
    let done = simulate_batch(&rig, Backend::Rfc, &["DTEL:ZD", "DOMA:ZO"], None);

    assert_eq!(done.outcome.exit, 2);
    assert!(
        done.outcome
            .stderr
            .contains("STOP: delete DTEL needs ADT classrun; not available on DEV"),
        "{}",
        done.outcome.stderr
    );
    assert!(done.outcome.stdout.contains("SKIPPED DOMA ZO"));
    assert_eq!(rig.sap.borrow().posts, 1);
    clean(&rig);
}

#[test]
fn a_signal_before_the_run_exits_130_and_runs_nothing() {
    let rig = rig(constant(present()), Vec::new());
    let cancel = CancelFlag::new();

    cancel.set();

    let done = batch(&rig, Backend::Rfc, &["DTEL:ZD"], None, true, None, &cancel);

    assert_eq!(done.outcome.exit, 130);
    assert_eq!(rig.sap.borrow().posts, 0);
    assert!(rig.sap.borrow().classes.is_empty());
}

#[test]
fn a_name_the_payload_refuses_stops_before_any_connection() {
    let rig = rig(constant(present()), Vec::new());
    let done = simulate_batch(&rig, Backend::Rfc, &["DTEL:/ABC/ZDTEL"], None);

    assert_eq!(done.outcome.exit, 2);
    assert!(
        done.outcome
            .stderr
            .starts_with("STOP: write and delete accept only Z and Y names"),
        "{}",
        done.outcome.stderr
    );
    assert_eq!(rig.sap.borrow().sessions, 0);
}

#[test]
fn nothing_sent_before_the_first_row_lets_the_router_try_the_other_backend() {
    let sap = rig(constant(present()), Vec::new()).sap;
    let cancel = CancelFlag::new();
    let policies = Policies::default();
    let context = Context {
        conn: "dev",
        user: "DEVUSER",
        policies: &policies,
        confirmer: &No,
    };
    let mut open = || -> Opened {
        let _ = &sap;

        Err(TransportError::PreSendConnectivity(
            "unreachable".to_owned(),
        ))
    };
    let mut ddic = DdicDelete::new(env(Backend::Http), &mut open, cancel.clone())
        .with_nonce(Box::new(|| NONCE.to_owned()));
    let directory = tempfile::tempdir().unwrap();
    let plan_path = directory.path().join("plan.json");
    let outcome = delete::run(
        &mut ddic,
        &context,
        &targets(&["DTEL:ZD"]),
        None,
        true,
        &plan_path,
    );
    let (failure, message) = ddic.unsent_failure().expect("nothing was sent");

    assert_eq!(outcome.exit, 1);
    assert_eq!(failure, abapreach::backend::Failure::PreSendConnectivity);
    assert!(message.contains("unreachable"), "{message}");
}

// --- the mix with the M4 kinds ---------------------------------------------------------------

#[derive(Default)]
struct Adt {
    inspected: Vec<String>,
    deleted: Vec<String>,
}

impl DeleteBackend for Adt {
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        assert!(
            !matches!(kind, DeleteKind::Runner(_)),
            "a DDIC row never goes to the ADT backend"
        );
        self.inspected.push(name.to_owned());

        Ok(Some(ObjectFacts {
            package: "$TMP".to_owned(),
            author: "DEVUSER".to_owned(),
            group: None,
            sources: vec![abapreach::operation::delete::SourceFact::active(
                "MAIN",
                &["REPORT."],
            )],
            contents: Vec::new(),
            where_used: Vec::new(),
            inactive: abapreach::operation::delete::InactiveFact::Absent,
            tr: abapreach::operation::delete::TrLock::Local,
        }))
    }

    fn delete(&mut self, kind: DeleteKind, name: &str, _tr: Option<&str>) -> DeleteReply {
        assert!(!matches!(kind, DeleteKind::Runner(_)));
        self.deleted.push(name.to_owned());

        DeleteReply::Deleted
    }

    fn reconcile(&mut self, _kind: DeleteKind, _name: &str) -> Reconcile {
        Reconcile::Gone
    }
}

#[test]
fn a_batch_of_an_m4_row_and_ddic_rows_sends_each_row_to_its_own_backend() {
    let rig = rig(
        gone_after_post(),
        vec![
            PostReply::Output(sim_ok(HASH_A)),
            PostReply::Output(sim_ok(HASH_B)),
            PostReply::Output(deleted()),
            PostReply::Output(deleted()),
        ],
    );
    let policies = Policies::default();
    let context = Context {
        conn: "dev",
        user: "DEVUSER",
        policies: &policies,
        confirmer: &No,
    };
    let texts = ["DTEL:ZDTEL", "PROG:ZPROG", "TABL:ZTABL"];
    let directory = tempfile::tempdir().unwrap();
    let plan_path = directory.path().join("plan.json");
    let cancel = CancelFlag::new();
    let mut adt = Adt::default();
    let sap = rig.sap.clone();
    let mut open = move || opened(Backend::Rfc, &sap);
    let mut ddic = DdicDelete::new(env(Backend::Rfc), &mut open, cancel.clone())
        .with_nonce(Box::new(|| NONCE.to_owned()))
        .with_clock(Box::new(FakeClock {
            now: rig.now.clone(),
        }));
    let mut mixed = RunnerBatch::new(&mut adt, &mut ddic);
    let simulated = delete::run(
        &mut mixed,
        &context,
        &targets(&texts),
        None,
        true,
        &plan_path,
    );

    assert_eq!(simulated.exit, 0, "{}", simulated.stdout);
    assert!(simulated.stdout.contains("guards active"));

    let plan = delete::load_plan(&plan_path).unwrap();
    let order: Vec<&str> = plan.objects.iter().map(|o| o.1.as_str()).collect();

    // PROG first (rank 4), then TABL (7), then DTEL (8).
    assert_eq!(order, ["ZPROG", "ZTABL", "ZDTEL"]);

    let real = delete::run(
        &mut mixed,
        &context,
        &targets(&texts),
        None,
        false,
        &plan_path,
    );

    assert_eq!(real.exit, 0, "{}", real.stdout);
    assert_eq!(adt.inspected, ["ZPROG", "ZPROG"]);
    assert_eq!(adt.deleted, ["ZPROG"]);
    assert_eq!(rig.sap.borrow().posts, 4);
}

#[test]
fn the_runner_backend_alone_does_not_declare_other_kinds_as_its_own() {
    let rig = rig(constant(present()), Vec::new());
    let cancel = CancelFlag::new();
    let sap = rig.sap.clone();
    let mut open = move || opened(Backend::Rfc, &sap);
    let ddic = DdicDelete::new(env(Backend::Rfc), &mut open, cancel);

    for kind in [
        DeleteKind::Prog,
        DeleteKind::Fugr,
        DeleteKind::Func,
        DeleteKind::Clas,
        DeleteKind::Intf,
    ] {
        assert!(!ddic.plans_in_run(kind), "{kind}");
    }

    for text in ["DTEL:Z", "DOMA:Z", "TABL:Z", "TTYP:Z", "TRAN:Z"] {
        let target: DeleteTarget = text.parse().unwrap();

        assert!(ddic.plans_in_run(target.kind), "{text}");
    }
}

#[test]
fn the_plan_shows_rows_request_and_messages_and_keeps_the_hash_as_the_run_gave_it() {
    let mut body = sim_body(HASH_A);

    body[2] = fact("REQUEST", "DEVK900005");
    body.push(fact("ROWS", "0"));
    body.push(msg(
        "W",
        "Request DEVK900001 is ignored: the object is locked in DEVK900005",
    ));
    body.push(msg("I", "WHERE-USED: none"));

    let rig = rig(
        constant(present()),
        vec![PostReply::Output(output(
            "DELETE_SIM",
            &body,
            "SIMULATED_OK",
        ))],
    );
    let done = simulate_batch(&rig, Backend::Rfc, &["TABL:ZT"], Some("DEVK900001"));
    let out = &done.outcome.stdout;

    for expected in [
        "DELETE TABL ZT package $TMP author DEVUSER TR DEVK900005",
        "ROWS 0",
        "W: Request DEVK900001 is ignored: the object is locked in DEVK900005",
        "WHERE-USED: none",
    ] {
        assert!(out.contains(expected), "{expected} in {out}");
    }

    // The request of the file is the one the user gave; the run decides the real one at the delete.
    assert_eq!(plan_of(&done).tr.as_deref(), Some("DEVK900001"));
}

#[test]
fn an_upper_case_hash_is_stored_in_lower_case() {
    let upper = HASH_A.to_ascii_uppercase();
    let rig = rig(constant(present()), vec![PostReply::Output(sim_ok(&upper))]);
    let done = simulate_batch(&rig, Backend::Rfc, &["DOMA:ZD"], None);

    assert_eq!(plan_of(&done).objects[0].2, HASH_A);
}

#[test]
fn a_simulation_without_a_usable_plan_hash_is_rejected() {
    let missing = {
        let mut body = sim_body(HASH_A);

        body.pop();

        output("DELETE_SIM", &body, "SIMULATED_OK")
    };
    let cases = [
        (missing, "the runner reported no plan hash"),
        (
            sim_ok("not-a-hash"),
            "the runner reported a malformed plan hash",
        ),
        (
            output("DELETE_SIM", &[], "REJECTED"),
            "the runner answered REJECTED",
        ),
    ];

    for (reply, expected) in cases {
        let rig = rig(constant(present()), vec![PostReply::Output(reply)]);
        let done = simulate_batch(&rig, Backend::Rfc, &["DTEL:ZD"], None);

        assert_eq!(done.outcome.exit, 1);
        assert!(
            done.outcome.stdout.contains(&format!("ERROR: {expected}")),
            "{expected} in {}",
            done.outcome.stdout
        );
        assert!(
            plan_of(&done).objects.is_empty(),
            "no object of a row that is not ready"
        );
    }
}

#[test]
fn a_run_that_answers_not_found_or_a_bare_partial_is_mapped_without_a_proof() {
    let plan = Plan {
        conn: "dev".to_owned(),
        tr: None,
        objects: vec![
            ("TABL".to_owned(), "ZTA".to_owned(), HASH_A.to_owned()),
            ("TABL".to_owned(), "ZTB".to_owned(), HASH_B.to_owned()),
        ],
        backends: Vec::new(),
    };
    let rig = rig(
        constant(present()),
        vec![
            PostReply::Output(output("DELETE", &[msg("E", "gone")], "NOT_FOUND")),
            PostReply::Output(output(
                "DELETE",
                &[msg("E", "RS_DD_DELETE_OBJ failed")],
                "PARTIAL",
            )),
        ],
    );
    let done = delete_batch(&rig, Backend::Rfc, &["TABL:ZTA", "TABL:ZTB"], None, &plan);
    let out = &done.outcome.stdout;

    assert!(out.contains("DELETE: NOT_FOUND"), "{out}");
    assert!(out.contains("DELETE: PARTIAL"), "{out}");
    assert!(
        out.contains("status unknown: RS_DD_DELETE_OBJ failed"),
        "{out}"
    );
    assert_eq!(done.outcome.exit, 1);
}

#[test]
fn a_leftover_after_a_clean_delete_still_exits_1() {
    let rig = rig(gone_after_post(), vec![PostReply::Output(deleted())]);

    rig.sap.borrow_mut().failing_deletes = 10;

    let plan = Plan {
        conn: "dev".to_owned(),
        tr: None,
        objects: vec![("DTEL".to_owned(), "ZD".to_owned(), HASH_A.to_owned())],
        backends: Vec::new(),
    };
    let done = delete_batch(&rig, Backend::Rfc, &["DTEL:ZD"], None, &plan);

    assert!(
        done.outcome
            .stdout
            .contains("SUMMARY: 1 OK, 0 FAILED, 0 SKIPPED")
    );
    assert_eq!(
        done.outcome.exit, 1,
        "the leftover class is never a clean success"
    );
    assert!(done.outcome.stderr.contains("RUNNER_LEFTOVER: "));
}

#[test]
fn the_exit_code_rules_rank_a_signal_above_a_leftover_above_a_stop() {
    use abapreach::operation::delete_ddic::exit_override;

    assert_eq!(exit_override(false, false, false), None);
    assert_eq!(exit_override(true, true, true), Some(130));
    assert_eq!(exit_override(false, true, true), Some(1));
    assert_eq!(exit_override(false, false, true), Some(2));
    assert_eq!(exit_override(true, false, false), Some(130));
    assert_eq!(exit_override(false, true, false), Some(1));
}

#[test]
fn a_ddic_row_is_never_read_or_deleted_without_its_plan_hash() {
    let rig = rig(constant(present()), Vec::new());
    let cancel = CancelFlag::new();
    let sap = rig.sap.clone();
    let mut open = move || opened(Backend::Rfc, &sap);
    let mut ddic = DdicDelete::new(env(Backend::Rfc), &mut open, cancel);
    let kind: DeleteKind = "DTEL:Z".parse::<DeleteTarget>().unwrap().kind;

    assert!(ddic.inspect(kind, "ZD").is_err());
    assert!(matches!(
        ddic.delete(kind, "ZD", None),
        DeleteReply::Rejected(_)
    ));
    assert_eq!(rig.sap.borrow().sessions, 0);
}

// --- TRAN rows (M6 Task 24) ------------------------------------------------------------------

/// The body of a successful `DELETE_SIM` of a transaction: package, author, program, cinfo,
/// request, and the hash of the six live lines. A transaction has no `STATE` fact.
fn tran_sim_body(hash: &str) -> Vec<String> {
    vec![
        fact("PACKAGE", "$TMP"),
        fact("AUTHOR", "DEVUSER"),
        fact("PROGRAM", "ZREPORT"),
        fact("CINFO", "80"),
        fact("REQUEST", "-"),
        fact("HASH", hash),
    ]
}

fn tran_sim_ok(hash: &str) -> String {
    output("DELETE_SIM", &tran_sim_body(hash), "SIMULATED_OK")
}

fn plan_of_rows(tr: Option<&str>, rows: &[(&str, &str, &str)]) -> Plan {
    Plan {
        conn: "dev".to_owned(),
        tr: tr.map(str::to_owned),
        objects: rows
            .iter()
            .map(|(kind, name, hash)| ((*kind).to_owned(), (*name).to_owned(), (*hash).to_owned()))
            .collect(),
        backends: Vec::new(),
    }
}

#[test]
fn a_tran_simulate_prints_package_author_tr_program_and_cinfo() {
    for backend in BACKENDS {
        let rig = rig(
            constant(present()),
            vec![PostReply::Output(tran_sim_ok(HASH_A))],
        );
        let done = simulate_batch(&rig, backend, &["TRAN:ztran"], None);
        let out = &done.outcome.stdout;

        assert_eq!(done.outcome.exit, 0, "{backend:?} {out}");

        for expected in [
            "== TRAN ZTRAN ==",
            "DELETE TRAN ZTRAN package $TMP author DEVUSER TR -",
            "PROGRAM ZREPORT",
            "CINFO 80",
            &format!("SOURCE LINES {HASH_A}"),
            &format!("plan hash {HASH_A}"),
            "DELETE_SIM: SIMULATED_OK",
            "SUMMARY: 1 OK, 0 FAILED, 0 SKIPPED",
        ] {
            assert!(out.contains(expected), "{backend:?} {expected} in {out}");
        }

        // A transaction is not a DDIC XML: no XML marker, no row count.
        assert!(!out.contains("SOURCE XML"), "{out}");
        assert!(!out.contains("ROWS"), "{out}");

        let payloads = posted(&rig);

        assert_eq!(payloads.len(), 1);
        assert_eq!(value(&payloads[0], "MODE"), Some("DELETE_SIM"));
        assert_eq!(value(&payloads[0], "TYPE"), Some("TRAN"));
        assert_eq!(value(&payloads[0], "NAME"), Some("ZTRAN"));
        assert_eq!(batch_of(&payloads[0]), ["TRAN:ZTRAN"]);
        clean(&rig);
    }
}

#[test]
fn the_tran_plan_hash_is_the_hash_the_run_gave_for_the_live_definition() {
    let first = rig(
        constant(present()),
        vec![PostReply::Output(tran_sim_ok(HASH_A))],
    );
    let second = rig(
        constant(present()),
        vec![PostReply::Output(tran_sim_ok(&HASH_B.to_ascii_uppercase()))],
    );
    let one = simulate_batch(&first, Backend::Rfc, &["TRAN:ZT"], None);
    let two = simulate_batch(&second, Backend::Rfc, &["TRAN:ZT"], None);

    assert_eq!(plan_of(&one).objects[0].2, HASH_A);
    // An upper-case hash is stored in lower case, like every other plan hash.
    assert_eq!(plan_of(&two).objects[0].2, HASH_B);

    for bad in ["", "abc", &"g".repeat(64)] {
        let mut body = tran_sim_body(HASH_A);

        body[5] = fact("HASH", bad);

        let rig = rig(
            constant(present()),
            vec![PostReply::Output(output(
                "DELETE_SIM",
                &body,
                "SIMULATED_OK",
            ))],
        );
        let done = simulate_batch(&rig, Backend::Rfc, &["TRAN:ZT"], None);

        assert_eq!(done.outcome.exit, 1, "{bad:?}");
        assert!(
            done.outcome.stdout.contains("ERROR: the runner reported"),
            "{}",
            done.outcome.stdout
        );
        assert!(!done.plan_path.exists() || plan_of(&done).objects.is_empty());
    }

    let mut body = tran_sim_body(HASH_A);

    body.remove(5);

    let rig = rig(
        constant(present()),
        vec![PostReply::Output(output(
            "DELETE_SIM",
            &body,
            "SIMULATED_OK",
        ))],
    );
    let done = simulate_batch(&rig, Backend::Rfc, &["TRAN:ZT"], None);

    assert!(
        done.outcome
            .stdout
            .contains("ERROR: the runner reported no plan hash")
    );
}

#[test]
fn a_tran_row_runs_first_in_a_mixed_batch_on_the_one_backend_of_the_operation() {
    for backend in BACKENDS {
        let rig = rig(
            gone_after_post(),
            vec![
                PostReply::Output(tran_sim_ok(HASH_A)),
                PostReply::Output(sim_ok(HASH_B)),
                PostReply::Output(deleted()),
                PostReply::Output(deleted()),
            ],
        );
        let policies = Policies::default();
        let context = Context {
            conn: "dev",
            user: "DEVUSER",
            policies: &policies,
            confirmer: &No,
        };
        let texts = ["DTEL:ZDTEL", "PROG:ZPROG", "CLAS:ZCL_X", "TRAN:ZTRAN"];
        let directory = tempfile::tempdir().unwrap();
        let plan_path = directory.path().join("plan.json");
        let cancel = CancelFlag::new();
        let mut adt = Adt::default();
        let sap = rig.sap.clone();
        let mut open = move || opened(backend, &sap);
        let mut ddic = DdicDelete::new(env(backend), &mut open, cancel.clone())
            .with_nonce(Box::new(|| NONCE.to_owned()))
            .with_clock(Box::new(FakeClock {
                now: rig.now.clone(),
            }));
        let mut mixed = RunnerBatch::new(&mut adt, &mut ddic);
        let simulated = delete::run(
            &mut mixed,
            &context,
            &targets(&texts),
            None,
            true,
            &plan_path,
        );

        assert_eq!(simulated.exit, 0, "{backend:?} {}", simulated.stdout);

        let plan = delete::load_plan(&plan_path).unwrap();
        let order: Vec<&str> = plan.objects.iter().map(|o| o.1.as_str()).collect();

        // The Python order: TRAN, then CLAS, PROG, and the DDIC rows last.
        assert_eq!(order, ["ZTRAN", "ZCL_X", "ZPROG", "ZDTEL"], "{backend:?}");
        // The plan file keeps its format: no per-object backend list.
        assert!(plan.backends.is_empty());
        assert!(
            !std::fs::read_to_string(&plan_path)
                .unwrap()
                .contains("backends")
        );

        let real = delete::run(
            &mut mixed,
            &context,
            &targets(&texts),
            None,
            false,
            &plan_path,
        );

        assert_eq!(real.exit, 0, "{backend:?} {}", real.stdout);
        assert!(real.stdout.contains("SUMMARY: 4 OK, 0 FAILED, 0 SKIPPED"));
        assert_eq!(adt.deleted, ["ZCL_X", "ZPROG"]);

        // Every run of the operation, simulate and delete, posted on the one transport.
        let other = match backend {
            Backend::Http => "sadt",
            Backend::Rfc => "http",
        };

        for out in [&simulated.stdout, &real.stdout] {
            assert!(out.contains(&format!("post via {},", backend.post())));
            assert!(!out.contains(&format!("post via {other},")), "{out}");
        }

        let payloads = posted(&rig);
        // The batch list is the whole batch, ADT rows included, in run order.
        let tokens = ["TRAN:ZTRAN", "CLAS:ZCL_X", "PROG:ZPROG", "DTEL:ZDTEL"];

        assert_eq!(payloads.len(), 4);

        for payload in &payloads {
            assert_eq!(batch_of(payload), tokens, "the batch list is in run order");
        }

        assert_eq!(value(&payloads[0], "NAME"), Some("ZTRAN"));
        assert_eq!(value(&payloads[2], "MODE"), Some("DELETE"));
        assert_eq!(value(&payloads[2], "NAME"), Some("ZTRAN"));
        assert_eq!(value(&payloads[2], "PLAN"), Some(HASH_A));
        clean(&rig);
    }
}

#[test]
fn a_tran_plan_that_changed_since_the_simulate_is_rejected_and_the_batch_goes_on() {
    let rig = rig(
        gone_after_post(),
        vec![
            PostReply::Output(delete_rejected(
                "Plan changed since simulate; simulate again",
            )),
            PostReply::Output(deleted()),
        ],
    );
    let plan = plan_of_rows(
        None,
        &[("TRAN", "ZTRAN", HASH_A), ("DTEL", "ZDTEL", HASH_B)],
    );
    let done = delete_batch(
        &rig,
        Backend::Rfc,
        &["TRAN:ZTRAN", "DTEL:ZDTEL"],
        None,
        &plan,
    );
    let out = &done.outcome.stdout;

    assert_eq!(done.outcome.exit, 1, "{out}");
    assert!(
        out.contains("ERROR: Plan changed since simulate; simulate again"),
        "{out}"
    );
    assert!(out.contains("SUMMARY: 1 OK, 1 FAILED, 0 SKIPPED"), "{out}");
    assert_eq!(rig.sap.borrow().posts, 2);
}

#[test]
fn a_tran_plan_that_does_not_match_the_arguments_stops_before_any_connection() {
    let rig = rig(constant(present()), Vec::new());
    let plan = plan_of_rows(None, &[("TRAN", "ZTRAN", HASH_A)]);

    for (texts, tr) in [
        (vec!["TRAN:ZTRAN", "DTEL:ZDTEL"], None),
        (vec!["TRAN:ZOTHER"], None),
        (vec!["DTEL:ZTRAN"], None),
        (vec!["TRAN:ZTRAN"], Some("DEVK900001")),
    ] {
        let done = delete_batch(&rig, Backend::Http, &texts, tr, &plan);

        assert_eq!(done.outcome.exit, 2, "{texts:?}");
        assert_eq!(
            done.outcome.stderr,
            "STOP: plan does not match the arguments (conn/objects/--tr); simulate again"
        );
    }

    assert_eq!(rig.sap.borrow().sessions, 0, "nothing was opened");
}

#[test]
fn a_missing_transaction_is_not_found_in_both_modes_and_the_next_row_runs() {
    let missing = |mode: &str| {
        PostReply::Output(output(
            mode,
            &[msg(
                "E",
                "Object TRAN ZTRAN does not exist (TADIR R3TR TRAN)",
            )],
            "NOT_FOUND",
        ))
    };
    let rig = rig(
        constant(present()),
        vec![missing("DELETE_SIM"), PostReply::Output(sim_ok(HASH_B))],
    );
    let simulated = simulate_batch(&rig, Backend::Rfc, &["TRAN:ZTRAN", "DTEL:ZDTEL"], None);
    let out = &simulated.outcome.stdout;

    assert_eq!(simulated.outcome.exit, 1, "{out}");
    assert!(out.contains("DELETE_SIM: NOT_FOUND"), "{out}");
    assert!(out.contains("DELETE_SIM: SIMULATED_OK"), "{out}");
    assert!(out.contains("SUMMARY: 1 OK, 1 FAILED, 0 SKIPPED"), "{out}");
    // Only the existing row is in the plan.
    assert_eq!(plan_of(&simulated).objects.len(), 1);

    let rig = rig_after_not_found(missing("DELETE"));
    let plan = plan_of_rows(
        None,
        &[("TRAN", "ZTRAN", HASH_A), ("DTEL", "ZDTEL", HASH_B)],
    );
    let done = delete_batch(
        &rig,
        Backend::Http,
        &["TRAN:ZTRAN", "DTEL:ZDTEL"],
        None,
        &plan,
    );

    assert!(
        done.outcome.stdout.contains("DELETE: NOT_FOUND"),
        "{}",
        done.outcome.stdout
    );
    assert!(
        done.outcome.stdout.contains("DELETE: DELETED"),
        "{}",
        done.outcome.stdout
    );
    assert_eq!(rig.sap.borrow().posts, 2);
}

fn rig_after_not_found(first: PostReply) -> Rig {
    rig(gone_after_post(), vec![first, PostReply::Output(deleted())])
}

#[test]
fn the_markers_of_a_partial_tran_delete_stop_the_batch() {
    for marker in ["PARTIAL_DELETE:{}", "OBJECT_STILL_EXISTS:{}"] {
        let line = marker.replace(
            "{}",
            &hex("Object TRAN ZTRAN still exists after the delete"),
        );
        let rig = rig(
            constant(present()),
            vec![PostReply::Output(output("DELETE", &[line], "PARTIAL"))],
        );
        let plan = plan_of_rows(
            None,
            &[("TRAN", "ZTRAN", HASH_A), ("DTEL", "ZDTEL", HASH_B)],
        );
        let done = delete_batch(
            &rig,
            Backend::Http,
            &["TRAN:ZTRAN", "DTEL:ZDTEL"],
            None,
            &plan,
        );
        let out = &done.outcome.stdout;

        assert_eq!(done.outcome.exit, 1, "{marker}");
        assert!(out.contains("DELETE: PARTIAL"), "{marker} {out}");
        assert!(
            out.contains("Object TRAN ZTRAN still exists after the delete"),
            "{out}"
        );
        assert!(out.contains("SKIPPED DTEL ZDTEL"), "{out}");
        assert_eq!(rig.sap.borrow().posts, 1);
    }
}

#[test]
fn a_tran_delete_is_proven_by_a_new_session_and_a_lost_reply_is_settled_once() {
    // The run says DELETED and the TSTC row is still there: PARTIAL, and the batch stops.
    let first = rig(constant(present()), vec![PostReply::Output(deleted())]);
    let plan = plan_of_rows(
        None,
        &[("TRAN", "ZTRAN", HASH_A), ("DTEL", "ZDTEL", HASH_B)],
    );
    let done = delete_batch(
        &first,
        Backend::Rfc,
        &["TRAN:ZTRAN", "DTEL:ZDTEL"],
        None,
        &plan,
    );

    assert_eq!(done.outcome.exit, 1);
    assert!(done.outcome.stdout.contains("DELETE: PARTIAL"));
    assert!(done.outcome.stdout.contains("SKIPPED DTEL ZDTEL"));

    for backend in BACKENDS {
        for gone in [true, false] {
            let (world, expected) = if gone {
                (gone_after_post(), "DELETED (reconciled)")
            } else {
                (constant(present()), "DELETE: PARTIAL")
            };
            let rig = rig(world, vec![PostReply::Lost]);
            let done = delete_batch(&rig, backend, &["TRAN:ZTRAN", "DTEL:ZDTEL"], None, &plan);
            let out = &done.outcome.stdout;

            assert_eq!(done.outcome.exit, 1, "{backend:?} {out}");
            assert!(out.contains(expected), "{expected} in {out}");
            assert!(out.contains("SKIPPED DTEL ZDTEL"), "{out}");
            assert_eq!(rig.sap.borrow().posts, 1, "the POST is never resent");
            clean(&rig);
        }
    }
}

#[test]
fn the_request_of_a_tran_delete_is_settled_by_the_run_and_shown_in_the_plan() {
    // A transaction locked in the user's own request: the run reports it in the plan line.
    let mut body = tran_sim_body(HASH_A);

    body[4] = fact("REQUEST", "DEVK900005");
    body.push(msg(
        "W",
        "Request DEVK900001 is ignored: the object is locked in DEVK900005",
    ));

    let own = rig(
        gone_after_post(),
        vec![
            PostReply::Output(output("DELETE_SIM", &body, "SIMULATED_OK")),
            PostReply::Output(deleted()),
        ],
    );
    let simulated = simulate_batch(&own, Backend::Rfc, &["TRAN:ZTRAN"], Some("DEVK900001"));
    let out = &simulated.outcome.stdout;

    assert!(
        out.contains("DELETE TRAN ZTRAN package $TMP author DEVUSER TR DEVK900005"),
        "{out}"
    );
    assert!(
        out.contains("W: Request DEVK900001 is ignored: the object is locked in DEVK900005"),
        "{out}"
    );

    let plan = plan_of(&simulated);
    let real = delete_batch(
        &own,
        Backend::Rfc,
        &["TRAN:ZTRAN"],
        Some("DEVK900001"),
        &plan,
    );
    let payloads = posted(&own);

    assert_eq!(real.outcome.exit, 0, "{}", real.outcome.stdout);
    // Both runs get the request of the command; the run settles the lock itself.
    assert_eq!(value(&payloads[0], "TR"), Some("DEVK900001"));
    assert_eq!(value(&payloads[1], "TR"), Some("DEVK900001"));
    assert_eq!(value(&payloads[1], "PLAN"), Some(HASH_A));

    // Another user's request, or an unlocked transportable object without a request, is the
    // run's REJECTED and the text is shown as the run gave it.
    for text in [
        "Object TRAN ZTRAN is locked in request DEVK900009 of OTHERUSER",
        "Transportable package ZPKG needs --tr",
    ] {
        let refused = rig(
            constant(present()),
            vec![PostReply::Output(sim_rejected(text))],
        );
        let done = simulate_batch(&refused, Backend::Http, &["TRAN:ZTRAN"], None);

        assert_eq!(done.outcome.exit, 1);
        assert!(
            done.outcome.stdout.contains(&format!("ERROR: {text}")),
            "{}",
            done.outcome.stdout
        );
    }
}

#[test]
fn the_tran_delete_order_stays_before_every_other_kind() {
    // Port of the Python order case: a transaction goes before its program.
    let kinds: Vec<DeleteKind> = [
        "PROG:ZP", "FUGR:ZF", "FUNC:ZF1", "CLAS:ZC", "INTF:ZI", "TTYP:ZT", "TABL:ZB", "DTEL:ZD",
        "DOMA:ZM",
    ]
    .iter()
    .map(|text| text.parse::<DeleteTarget>().unwrap().kind)
    .collect();
    let tran = "TRAN:ZT".parse::<DeleteTarget>().unwrap().kind;

    assert!(kinds.iter().all(|kind| tran.rank() < kind.rank()));
}

// --- cargo-mutants triage (M6 Task 33): rules that no test pinned ----------------------------

#[test]
fn a_cancelled_batch_exits_130_even_when_no_run_saw_the_signal() {
    let cancel = CancelFlag::new();

    cancel.set();

    let mut open = || -> Opened { Err(TransportError::PreSendConnectivity("unused".to_owned())) };
    let mut ddic = DdicDelete::new(env(Backend::Http), &mut open, cancel);
    let mut outcome = Outcome {
        status: None,
        exit: 0,
        stdout: String::new(),
        stderr: String::new(),
    };

    ddic.finish(&mut outcome);

    assert_eq!(outcome.exit, 130);
}

#[test]
fn a_batch_that_ran_a_row_never_offers_the_other_backend_for_a_later_stop() {
    let rig = rig(constant(present()), vec![PostReply::Output(sim_ok(HASH_A))]);
    let policies = Policies::default();
    let context = Context {
        conn: "dev",
        user: "DEVUSER",
        policies: &policies,
        confirmer: &No,
    };
    let sap = rig.sap.clone();
    // The session of the second row keeps no state: it is the probe of that row that stops.
    let mut open = move || {
        if sap.borrow().posts > 0 {
            sap.borrow_mut().stateful = false;
        }

        opened(Backend::Http, &sap)
    };
    let mut ddic = DdicDelete::new(env(Backend::Http), &mut open, CancelFlag::new())
        .with_nonce(Box::new(|| NONCE.to_owned()))
        .with_clock(Box::new(FakeClock {
            now: rig.now.clone(),
        }));
    let directory = tempfile::tempdir().unwrap();
    let mut outcome = delete::run(
        &mut ddic,
        &context,
        &targets(&["DTEL:ZD1", "DTEL:ZD2"]),
        None,
        true,
        &directory.path().join("plan.json"),
    );

    ddic.finish(&mut outcome);

    assert_eq!(rig.sap.borrow().posts, 1, "the first row ran");
    assert!(
        outcome
            .stderr
            .starts_with("STOP: ADT HTTP session is stateless"),
        "{}",
        outcome.stderr
    );
    // Rerunning the whole batch on RFC would run the first row twice.
    assert_eq!(ddic.unsent_failure(), None);
}

/// The class source without its payload lines, the class name replaced by the template's
/// placeholder.
fn constant_lines(source: &str) -> Vec<String> {
    let name = source
        .lines()
        .find_map(|line| line.strip_prefix("CLASS ")?.strip_suffix(" DEFINITION"))
        .expect("the class definition line");

    source
        .lines()
        .filter(|line| !line.starts_with("    lv_b64 = lv_b64 && '"))
        .map(|line| line.replace(name, "<NAME>"))
        .collect()
}

#[test]
fn each_row_runs_the_template_of_its_own_type() {
    let cases: [(&str, &TypeTemplate); 2] =
        [("DTEL:ZD", &DTEL_TEMPLATE), ("TRAN:ZT", &TRAN_TEMPLATE)];

    for (text, template) in cases {
        let rig = rig(constant(present()), vec![PostReply::Output(sim_ok(HASH_A))]);

        simulate_batch(&rig, Backend::Rfc, &[text], None);

        let source = rig.sap.borrow().posted[0].clone();

        assert_eq!(constant_lines(&source), template_lines(template), "{text}");
    }
}

#[test]
fn a_planned_row_carries_the_request_the_run_reports() {
    for (request, expected) in [("DEVK900005", Some("DEVK900005")), ("-", None)] {
        let mut body = sim_body(HASH_A);

        body[2] = fact("REQUEST", request);

        let rig = rig(
            constant(present()),
            vec![PostReply::Output(output(
                "DELETE_SIM",
                &body,
                "SIMULATED_OK",
            ))],
        );
        let sap = rig.sap.clone();
        let mut open = move || opened(Backend::Rfc, &sap);
        let mut ddic = DdicDelete::new(env(Backend::Rfc), &mut open, CancelFlag::new())
            .with_nonce(Box::new(|| NONCE.to_owned()))
            .with_clock(Box::new(FakeClock {
                now: rig.now.clone(),
            }));
        let kind = DeleteKind::Runner(Kind::Dtel);

        ddic.begin(&[(kind, "ZD".to_owned())], None).unwrap();

        match ddic.plan_row(kind, "ZD", None) {
            RowPlan::Ready { tr, .. } => assert_eq!(tr.as_deref(), expected, "{request}"),
            other => panic!("{request}: {other:?}"),
        }
    }
}

// --- the mixed batch hands each call to the side that owns it --------------------------------

/// A backend with scripted answers, tagged so that a test can tell which side answered.
#[derive(Default)]
struct Side {
    tag: &'static str,
    code: i32,
    include: bool,
    halted: bool,
    plans: bool,
    stops: bool,
}

impl DeleteBackend for Side {
    fn inspect(&mut self, _kind: DeleteKind, _name: &str) -> Result<Option<ObjectFacts>, String> {
        Ok(None)
    }

    fn delete(&mut self, _kind: DeleteKind, _name: &str, _tr: Option<&str>) -> DeleteReply {
        DeleteReply::Deleted
    }

    fn reconcile(&mut self, _kind: DeleteKind, _name: &str) -> Reconcile {
        Reconcile::Gone
    }

    fn is_include(&mut self, _kind: DeleteKind, name: &str) -> bool {
        self.include && name == "ZINCL"
    }

    fn begin_failure(&self, message: &str) -> (i32, String) {
        (self.code, format!("{}: {message}", self.tag))
    }

    fn halted(&self) -> bool {
        self.halted
    }

    fn plans_in_run(&self, _kind: DeleteKind) -> bool {
        self.plans
    }

    fn stops_batch(&self) -> bool {
        self.stops
    }
}

#[test]
fn a_mixed_batch_asks_the_adt_side_whether_a_program_is_an_include() {
    let mut adt = Side {
        include: true,
        ..Side::default()
    };
    let mut runner = Side::default();
    let mut mixed = RunnerBatch::new(&mut adt, &mut runner);

    assert!(mixed.is_include(DeleteKind::Prog, "ZINCL"));
    assert!(!mixed.is_include(DeleteKind::Prog, "ZMAIN"));
}

#[test]
fn a_mixed_batch_reports_a_failed_begin_the_way_the_runner_side_does() {
    let mut adt = Side {
        tag: "adt",
        code: 8,
        ..Side::default()
    };
    let mut runner = Side {
        tag: "runner",
        code: 7,
        ..Side::default()
    };
    let mixed = RunnerBatch::new(&mut adt, &mut runner);

    assert_eq!(mixed.begin_failure("boom"), (7, "runner: boom".to_owned()));
}

#[test]
fn a_mixed_batch_is_halted_when_either_side_is() {
    for (adt_halted, runner_halted) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let mut adt = Side {
            halted: adt_halted,
            ..Side::default()
        };
        let mut runner = Side {
            halted: runner_halted,
            ..Side::default()
        };
        let mixed = RunnerBatch::new(&mut adt, &mut runner);

        assert_eq!(mixed.halted(), adt_halted || runner_halted);
    }
}

#[test]
fn a_mixed_batch_plans_in_a_run_only_a_runner_kind_the_runner_side_accepts() {
    let dtel = DeleteKind::Runner(Kind::Dtel);

    for runner_plans in [false, true] {
        let mut adt = Side {
            plans: true,
            ..Side::default()
        };
        let mut runner = Side {
            plans: runner_plans,
            ..Side::default()
        };
        let mixed = RunnerBatch::new(&mut adt, &mut runner);

        assert_eq!(mixed.plans_in_run(dtel), runner_plans);
        assert!(!mixed.plans_in_run(DeleteKind::Prog));
    }
}

#[test]
fn a_mixed_batch_stops_when_the_runner_side_says_so() {
    for (adt_stops, runner_stops) in [(false, false), (false, true), (true, false)] {
        let mut adt = Side {
            stops: adt_stops,
            ..Side::default()
        };
        let mut runner = Side {
            stops: runner_stops,
            ..Side::default()
        };
        let mixed = RunnerBatch::new(&mut adt, &mut runner);

        assert_eq!(mixed.stops_batch(), runner_stops);
    }
}
