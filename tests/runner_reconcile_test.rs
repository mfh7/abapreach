//! Uncertain status and reconciliation of a runner operation (M6 Task 16): the outcome mapping, the
//! stable-state wait, the extra `READ` run, Ctrl-C, and the end-to-end path from a lost classrun
//! reply on both backends. Fake SAP only; time is virtual.

mod common;

use abapreach::backend::BackendKind;
use abapreach::domain::Status;
use abapreach::operation::deploy::{CancelFlag, InterruptAction};
use abapreach::operation::payload::Kind;
use abapreach::operation::runner::{
    CycleInput, PostRoute, Row, RunnerEnd, RunnerRun, run_cycle, run_rows,
};
use abapreach::operation::runner_reconcile::{
    Active, Cause, Clock, FIRST_WAIT, Flag, GIVE_UP, Intent, Presence, READ_INTERVAL, ReadFacts,
    ReadRun, Reconciled, Stable, Subject, Verdict, classify_create, classify_delete,
    classify_deploy, read_facts, reconcile, settle, stable_state, uncertainty,
};
use abapreach::runner_protocol::{Nonce, RunMode};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use common::runner_fake::{
    Evolution, OBJECT, Opened, PostReply, Shared, Tick, World, http_session, new_sap, rfc_session,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const PREFIX: &str = "YTEST_ABAPREACH_RUN_";
const RUNNER: &str = "YTEST_ABAPREACH_RUN_AB12CD34";
const ADVICE: &str = "check with read before retrying";
const FILE: &str = "FILEHASH";
const BASE: &str = "BASEHASH";

fn nonce() -> Nonce {
    Nonce::parse(NONCE).unwrap()
}

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

fn read_ok(hash: &str) -> String {
    output("READ", &[format!("FACT:HASH:{}", hex(hash))], "OK")
}

fn read_not_found() -> String {
    output("READ", &[], "NOT_FOUND")
}

// --- the clock -------------------------------------------------------------------------------

/// Virtual time: a wait only moves the clock.
struct FakeClock {
    now: Rc<Cell<Duration>>,
    waits: Vec<Duration>,
    /// Runs before the nth wait (1-based) decides.
    hook: Option<(usize, Box<dyn Fn()>)>,
}

impl FakeClock {
    fn new(now: &Rc<Cell<Duration>>) -> Self {
        Self {
            now: now.clone(),
            waits: Vec::new(),
            hook: None,
        }
    }
}

impl Clock for FakeClock {
    fn wait(&mut self, duration: Duration, abort: &dyn Fn() -> bool) -> bool {
        self.waits.push(duration);

        if let Some((nth, hook)) = &self.hook
            && *nth == self.waits.len()
        {
            hook();
        }

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

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

// --- the pure mapping ------------------------------------------------------------------------

fn hash(text: &str) -> Active {
    Active::Hash(text.to_owned())
}

#[test]
fn a_deploy_is_classified_by_the_active_hash_and_the_inactive_version() {
    let cases = [
        // (active, inactive, verdict)
        (hash(FILE), Flag::No, Verdict::ActiveReconciled),
        (hash(FILE), Flag::Yes, Verdict::ActiveReconciled),
        (
            hash(FILE),
            Flag::Unknown("x".to_owned()),
            Verdict::ActiveReconciled,
        ),
        (hash(BASE), Flag::Yes, Verdict::Inactive),
        (hash(BASE), Flag::No, Verdict::PartialUnknown),
        (
            hash(BASE),
            Flag::Unknown("preview failed".to_owned()),
            Verdict::Incomplete("the inactive version cannot be read (preview failed)".to_owned()),
        ),
        (hash("OTHER"), Flag::Yes, Verdict::PartialUnexpected),
        (hash("OTHER"), Flag::No, Verdict::PartialUnexpected),
        (Active::Missing, Flag::No, Verdict::PartialUnexpected),
        (Active::Missing, Flag::Yes, Verdict::PartialUnexpected),
    ];

    for (active, inactive, verdict) in cases {
        assert_eq!(
            classify_deploy(FILE, BASE, &active, &inactive),
            verdict,
            "{active:?} {inactive:?}"
        );
    }
}

#[test]
fn a_file_hash_that_equals_the_base_is_still_judged_active_first() {
    // The order of the rows: the file hash wins over the base.
    assert_eq!(
        classify_deploy(BASE, BASE, &hash(BASE), &Flag::Yes),
        Verdict::ActiveReconciled
    );
}

#[test]
fn the_hash_compare_ignores_case() {
    assert_eq!(
        classify_deploy("abc123", "def456", &hash("ABC123"), &Flag::No),
        Verdict::ActiveReconciled
    );
    assert_eq!(
        classify_deploy("abc123", "def456", &hash("DEF456"), &Flag::Yes),
        Verdict::Inactive
    );
}

#[test]
fn a_create_and_a_delete_are_classified_by_presence() {
    let unknown = Presence::Unknown("DD04L: preview failed".to_owned());

    assert_eq!(classify_create(&Presence::Present), Verdict::CreatePartial);
    assert_eq!(classify_create(&Presence::Absent), Verdict::CreateRejected);
    assert_eq!(
        classify_create(&unknown),
        Verdict::Incomplete("the object cannot be read (DD04L: preview failed)".to_owned())
    );
    assert_eq!(classify_delete(&Presence::Present), Verdict::DeletePartial);
    assert_eq!(
        classify_delete(&Presence::Absent),
        Verdict::DeletedReconciled
    );
    assert_eq!(
        classify_delete(&unknown),
        Verdict::Incomplete("the object cannot be read (DD04L: preview failed)".to_owned())
    );
}

#[test]
fn every_verdict_has_its_token_its_status_its_line_and_exit_1() {
    let cases = [
        (
            Verdict::ActiveReconciled,
            "ACTIVE (reconciled)",
            Status::Active,
            "ACTIVE (reconciled): the active definition equals the file",
        ),
        (
            Verdict::Inactive,
            "INACTIVE",
            Status::Inactive,
            "INACTIVE: the write reached SAP but the activation did not",
        ),
        (
            Verdict::PartialUnknown,
            "PARTIAL: outcome unknown",
            Status::Partial,
            "PARTIAL: outcome unknown; the active definition equals the base and there is no \
             inactive version",
        ),
        (
            Verdict::PartialUnexpected,
            "PARTIAL: unexpected state",
            Status::Partial,
            "PARTIAL: unexpected state; the active definition is neither the base nor the file",
        ),
        (
            Verdict::CreatePartial,
            "CREATE: PARTIAL",
            Status::Partial,
            "CREATE: PARTIAL; the object exists after an uncertain create",
        ),
        (
            Verdict::CreateRejected,
            "CREATE: REJECTED",
            Status::Rejected,
            "CREATE: REJECTED; the object does not exist after an uncertain create",
        ),
        (
            Verdict::DeletedReconciled,
            "DELETED (reconciled)",
            Status::Active,
            "DELETED (reconciled): the object is gone in a new session",
        ),
        (
            Verdict::DeletePartial,
            "PARTIAL",
            Status::Partial,
            "PARTIAL: the object still exists after an uncertain delete",
        ),
        (
            Verdict::Incomplete("no luck".to_owned()),
            "PARTIAL: reconciliation incomplete",
            Status::Partial,
            "PARTIAL: reconciliation incomplete; no luck",
        ),
    ];

    for (verdict, token, status, line) in cases {
        assert_eq!(verdict.token(), token);
        assert_eq!(verdict.status(), status);
        assert_eq!(verdict.line(), format!("{line}; {ADVICE}"), "{token}");
        assert_eq!(verdict.exit(), 1, "{token}");
    }
}

// --- what makes a status uncertain -----------------------------------------------------------

#[test]
fn a_lost_reply_a_timeout_and_a_dropped_connection_are_uncertain() {
    for message in [
        "reply lost",
        "HTTP timeout after request send",
        "connection reset by peer",
    ] {
        let end = RunnerEnd::PostUncertain(TransportError::PostSendUncertain(message.to_owned()));

        assert!(
            matches!(
                uncertainty(&end, &nonce(), RunMode::Deploy),
                Some(Cause::LostReply(text)) if text.contains(message)
            ),
            "{message}"
        );
    }
}

#[test]
fn a_5xx_and_a_broken_protocol_are_uncertain_and_a_whole_run_is_not() {
    let begin_only = format!("ABAPREACH:{NONCE}:BEGIN:DEPLOY\n\n");
    let broken = [
        Response::text(500, "<html>Application Server Error</html>"),
        Response::text(502, "<html>Bad Gateway</html>"),
        Response::text(504, ""),
        Response::text(200, begin_only),
        Response::text(200, "no runner output at all"),
    ];

    for response in broken {
        assert!(
            matches!(
                uncertainty(
                    &RunnerEnd::Posted(response.clone()),
                    &nonce(),
                    RunMode::Deploy
                ),
                Some(Cause::Broken(_))
            ),
            "{response:?}"
        );
    }

    let whole = output("DEPLOY", &[], "ACTIVE");

    assert_eq!(
        uncertainty(
            &RunnerEnd::Posted(Response::text(200, whole)),
            &nonce(),
            RunMode::Deploy
        ),
        None
    );
}

#[test]
fn a_refused_post_and_a_stage_before_the_post_are_not_uncertain() {
    let ends = [
        RunnerEnd::Posted(Response::text(400, "bad request")),
        RunnerEnd::Rejected("runner PUT failed: HTTP 500".to_owned()),
        RunnerEnd::Stop("x".to_owned()),
        RunnerEnd::Interrupted,
        RunnerEnd::NotStarted(TransportError::PreSendConnectivity("down".to_owned())),
    ];

    for end in ends {
        assert_eq!(
            uncertainty(&end, &nonce(), RunMode::Deploy),
            None,
            "{end:?}"
        );
    }
}

// --- the facts of the extra READ run ---------------------------------------------------------

#[test]
fn a_read_run_gives_the_hash_or_not_found_and_anything_else_is_an_error() {
    let post = |body: String| RunnerEnd::Posted(Response::text(200, body));

    assert_eq!(
        read_facts(&post(read_ok("ABCDEF")), &nonce()),
        Ok(ReadFacts::Found {
            hash: "ABCDEF".to_owned()
        })
    );
    assert_eq!(
        read_facts(&post(read_not_found()), &nonce()),
        Ok(ReadFacts::NotFound)
    );

    // OK without a HASH fact, a REJECTED run, a broken run, a 5xx, a lost reply, a stage failure.
    let errors = [
        post(output("READ", &[], "OK")),
        post(output("READ", &[], "REJECTED")),
        post(format!("ABAPREACH:{NONCE}:BEGIN:READ\n\n")),
        RunnerEnd::Posted(Response::text(500, "<html/>")),
        RunnerEnd::Posted(Response::text(403, "")),
        RunnerEnd::PostUncertain(TransportError::PostSendUncertain("lost".to_owned())),
        RunnerEnd::Rejected("runner PUT failed: HTTP 500".to_owned()),
        RunnerEnd::Stop("needs classrun".to_owned()),
        RunnerEnd::Interrupted,
        RunnerEnd::NotStarted(TransportError::PreSendConnectivity("down".to_owned())),
    ];

    for end in errors {
        assert!(read_facts(&end, &nonce()).is_err(), "{end:?}");
    }
}

// --- the stable-state wait -------------------------------------------------------------------

#[test]
fn two_identical_reads_end_the_wait_after_the_first_wait_and_one_interval() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock::new(&now);
    let mut reads = 0;
    let result = stable_state(&mut clock, &|| false, || {
        reads += 1;
        "state"
    });

    assert_eq!(result, Stable::Settled("state"));
    assert_eq!(reads, 2);
    assert_eq!(clock.waits, [FIRST_WAIT, READ_INTERVAL]);
    assert_eq!(
        (FIRST_WAIT, READ_INTERVAL, GIVE_UP),
        (secs(20), secs(10), secs(120))
    );
    assert_eq!(now.get(), secs(30));
}

#[test]
fn a_state_that_moves_is_read_again_until_two_reads_agree() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock::new(&now);
    let mut script = vec!["a", "b", "c", "c"].into_iter();
    let result = stable_state(&mut clock, &|| false, || script.next().unwrap());

    assert_eq!(result, Stable::Settled("c"));
    assert_eq!(
        clock.waits,
        [FIRST_WAIT, READ_INTERVAL, READ_INTERVAL, READ_INTERVAL]
    );
}

#[test]
fn a_state_that_never_settles_is_given_up_at_120_seconds() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock::new(&now);
    let mut reads = 0_u32;
    let result = stable_state(&mut clock, &|| false, || {
        reads += 1;
        reads
    });

    assert_eq!(result, Stable::GaveUp);
    // Reads at 20, 30, ..., 120 seconds.
    assert_eq!(reads, 11);
    assert_eq!(now.get(), secs(120));
    assert_eq!(clock.waits.len(), 11);
}

#[test]
fn an_abort_during_the_wait_skips_the_rest() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock::new(&now);
    let aborted = Rc::new(Cell::new(false));
    let flag = aborted.clone();

    clock.hook = Some((2, Box::new(move || flag.set(true))));

    let mut reads = 0;
    let result = stable_state(&mut clock, &|| aborted.get(), || {
        reads += 1;
        reads
    });

    assert_eq!(result, Stable::Skipped);
    assert_eq!(reads, 1);
}

#[test]
fn an_abort_before_the_first_read_reads_nothing() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock::new(&now);
    let mut reads = 0;
    let result = stable_state(&mut clock, &|| true, || {
        reads += 1;
        reads
    });

    assert_eq!(result, Stable::Skipped);
    assert_eq!(reads, 0);
}

// --- reconcile against the fake SAP ----------------------------------------------------------

fn subject() -> Subject<'static> {
    Subject {
        kind: Kind::Dtel,
        name: OBJECT,
        runner: RUNNER,
    }
}

fn constant(world: World) -> Evolution {
    Box::new(move |_| world)
}

fn present() -> World {
    World {
        definition: true,
        directory: true,
        ..World::default()
    }
}

/// Reconciles with a READ run that answers the given facts; returns the result and the number
/// of READ runs started.
fn run_reconcile(
    intent: &Intent<'_>,
    sap: &Shared,
    clock: &mut FakeClock,
    cancel: &CancelFlag,
    read: Result<ReadFacts, String>,
) -> (Reconciled, usize) {
    let reads = Cell::new(0);
    let mut open = || rfc_session(sap);
    let mut read_run = |_: &CancelFlag| {
        reads.set(reads.get() + 1);

        ReadRun {
            facts: read.clone(),
            runner_line: None,
            leftover: None,
            leftover_text: None,
            notes: Vec::new(),
        }
    };
    let result = reconcile(intent, &subject(), &mut open, &mut read_run, clock, cancel);

    (result, reads.get())
}

fn setup(world: Evolution) -> (Shared, Rc<Cell<Duration>>) {
    let now = Rc::new(Cell::new(Duration::ZERO));

    (new_sap(true, &now, world), now)
}

#[test]
fn a_create_is_settled_from_the_evidence_alone() {
    let cases = [
        (present(), Verdict::CreatePartial),
        (
            World {
                definition: false,
                directory: true,
                ..World::default()
            },
            Verdict::CreatePartial,
        ),
        (
            World {
                definition: true,
                directory: false,
                ..World::default()
            },
            Verdict::CreatePartial,
        ),
        (World::default(), Verdict::CreateRejected),
    ];

    for (world, verdict) in cases {
        let (sap, now) = setup(constant(world));
        let mut clock = FakeClock::new(&now);
        let (result, reads) = run_reconcile(
            &Intent::Create,
            &sap,
            &mut clock,
            &CancelFlag::new(),
            Err("x".into()),
        );

        assert_eq!(result.verdict, verdict, "{world:?}");
        assert_eq!(reads, 0, "evidence is enough");
        assert!(!result.skipped);
        assert_eq!(clock.waits, [FIRST_WAIT, READ_INTERVAL]);
    }
}

#[test]
fn a_delete_is_settled_from_the_evidence_alone() {
    let cases = [
        (World::default(), Verdict::DeletedReconciled),
        (present(), Verdict::DeletePartial),
        (
            World {
                directory: true,
                ..World::default()
            },
            Verdict::DeletePartial,
        ),
    ];

    for (world, verdict) in cases {
        let (sap, now) = setup(constant(world));
        let mut clock = FakeClock::new(&now);
        let (result, reads) = run_reconcile(
            &Intent::Delete,
            &sap,
            &mut clock,
            &CancelFlag::new(),
            Err("x".into()),
        );

        assert_eq!(result.verdict, verdict, "{world:?}");
        assert_eq!(reads, 0);
    }
}

#[test]
fn a_delete_whose_state_moves_waits_until_two_reads_agree() {
    // Present until 25 s, gone afterwards: the reads at 20 s and 30 s differ; 30 s and 40 s agree.
    let (sap, now) = setup(Box::new(|tick: &Tick| {
        if tick.elapsed < secs(25) {
            present()
        } else {
            World::default()
        }
    }));
    let mut clock = FakeClock::new(&now);
    let (result, _) = run_reconcile(
        &Intent::Delete,
        &sap,
        &mut clock,
        &CancelFlag::new(),
        Err("x".into()),
    );

    assert_eq!(result.verdict, Verdict::DeletedReconciled);
    assert_eq!(clock.waits, [FIRST_WAIT, READ_INTERVAL, READ_INTERVAL]);
    assert_eq!(now.get(), secs(40));
}

#[test]
fn a_state_that_never_settles_is_a_partial_after_120_seconds() {
    // The row flips every 10 s, so no two reads in a row agree.
    let (sap, now) = setup(Box::new(|tick: &Tick| World {
        definition: (tick.elapsed.as_secs() / 10).is_multiple_of(2),
        ..World::default()
    }));
    let mut clock = FakeClock::new(&now);
    let (result, reads) = run_reconcile(
        &Intent::Delete,
        &sap,
        &mut clock,
        &CancelFlag::new(),
        Err("x".into()),
    );

    assert!(
        matches!(&result.verdict, Verdict::Incomplete(reason) if reason.contains("120")),
        "{:?}",
        result.verdict
    );
    assert_eq!(result.verdict.status(), Status::Partial);
    assert_eq!(reads, 0);
    assert_eq!(now.get(), secs(120));
}

#[test]
fn a_deploy_reads_the_active_hash_with_one_extra_read_run() {
    let cases = [
        (FILE, true, Verdict::ActiveReconciled),
        (FILE, false, Verdict::ActiveReconciled),
        (BASE, true, Verdict::Inactive),
        (BASE, false, Verdict::PartialUnknown),
        ("OTHER", true, Verdict::PartialUnexpected),
        ("OTHER", false, Verdict::PartialUnexpected),
    ];

    for (hash, inactive, verdict) in cases {
        let (sap, now) = setup(constant(World {
            definition: true,
            directory: true,
            inactive,
            ..World::default()
        }));
        let mut clock = FakeClock::new(&now);
        let intent = Intent::Deploy {
            file_hash: FILE,
            base_hash: BASE,
        };
        let (result, reads) = run_reconcile(
            &intent,
            &sap,
            &mut clock,
            &CancelFlag::new(),
            Ok(ReadFacts::Found {
                hash: hash.to_owned(),
            }),
        );

        assert_eq!(result.verdict, verdict, "{hash} {inactive}");
        assert_eq!(reads, 1, "exactly one extra READ run");
        assert!(result.read_run.is_some());
    }
}

#[test]
fn a_deploy_whose_definition_is_gone_is_unexpected_without_a_read_run() {
    let (sap, now) = setup(constant(World::default()));
    let mut clock = FakeClock::new(&now);
    let intent = Intent::Deploy {
        file_hash: FILE,
        base_hash: BASE,
    };
    let (result, reads) = run_reconcile(
        &intent,
        &sap,
        &mut clock,
        &CancelFlag::new(),
        Err("unused".to_owned()),
    );

    assert_eq!(result.verdict, Verdict::PartialUnexpected);
    assert_eq!(reads, 0);
}

#[test]
fn a_deploy_whose_read_run_says_not_found_is_unexpected() {
    let (sap, now) = setup(constant(present()));
    let mut clock = FakeClock::new(&now);
    let intent = Intent::Deploy {
        file_hash: FILE,
        base_hash: BASE,
    };
    let (result, reads) = run_reconcile(
        &intent,
        &sap,
        &mut clock,
        &CancelFlag::new(),
        Ok(ReadFacts::NotFound),
    );

    assert_eq!(result.verdict, Verdict::PartialUnexpected);
    assert_eq!(reads, 1);
}

#[test]
fn a_deploy_with_an_unreadable_inactive_version_is_judged_by_the_hash() {
    // The preview of the inactive row fails, the active definition is readable.
    let (sap, now) = setup(constant(present()));
    let mut clock = FakeClock::new(&now);
    let intent = Intent::Deploy {
        file_hash: FILE,
        base_hash: BASE,
    };

    sap.borrow_mut().inactive_fails = true;

    let (result, _) = run_reconcile(
        &intent,
        &sap,
        &mut clock,
        &CancelFlag::new(),
        Ok(ReadFacts::Found {
            hash: FILE.to_owned(),
        }),
    );

    assert_eq!(result.verdict, Verdict::ActiveReconciled);

    let (sap, now) = setup(constant(present()));
    let mut clock = FakeClock::new(&now);

    sap.borrow_mut().inactive_fails = true;

    let (result, _) = run_reconcile(
        &intent,
        &sap,
        &mut clock,
        &CancelFlag::new(),
        Ok(ReadFacts::Found {
            hash: BASE.to_owned(),
        }),
    );

    assert!(
        matches!(result.verdict, Verdict::Incomplete(_)),
        "{:?}",
        result.verdict
    );
}

#[test]
fn unreadable_previews_fall_back_to_one_read_run_for_a_create_and_a_delete() {
    let broken = World {
        broken: true,
        ..World::default()
    };
    let cases = [
        (
            &Intent::Create,
            ReadFacts::NotFound,
            Verdict::CreateRejected,
        ),
        (
            &Intent::Create,
            ReadFacts::Found {
                hash: FILE.to_owned(),
            },
            Verdict::CreatePartial,
        ),
        (
            &Intent::Delete,
            ReadFacts::NotFound,
            Verdict::DeletedReconciled,
        ),
        (
            &Intent::Delete,
            ReadFacts::Found {
                hash: FILE.to_owned(),
            },
            Verdict::DeletePartial,
        ),
    ];

    for (intent, facts, verdict) in cases {
        let (sap, now) = setup(constant(broken));
        let mut clock = FakeClock::new(&now);
        let (result, reads) =
            run_reconcile(intent, &sap, &mut clock, &CancelFlag::new(), Ok(facts));

        assert_eq!(result.verdict, verdict);
        assert_eq!(reads, 1);
        // The unreadable snapshot is identical twice, so the wait ends after 30 s.
        assert_eq!(now.get(), secs(30));
    }
}

#[test]
fn a_reconciliation_that_cannot_read_anything_is_a_partial() {
    let broken = World {
        broken: true,
        ..World::default()
    };

    for intent in [&Intent::Create, &Intent::Delete] {
        let (sap, now) = setup(constant(broken));
        let mut clock = FakeClock::new(&now);
        let (result, reads) = run_reconcile(
            intent,
            &sap,
            &mut clock,
            &CancelFlag::new(),
            Err("the READ run was rejected".to_owned()),
        );

        assert!(
            matches!(&result.verdict, Verdict::Incomplete(reason) if reason.contains("READ")),
            "{:?}",
            result.verdict
        );
        assert_eq!(reads, 1, "at most one extra READ run");
        assert_eq!(result.verdict.status(), Status::Partial);
        assert_eq!(result.verdict.exit(), 1);
    }
}

#[test]
fn a_session_that_cannot_open_is_a_partial_without_a_read_run() {
    let (_, now) = setup(constant(present()));
    let mut clock = FakeClock::new(&now);
    let reads = Cell::new(0);
    let mut open =
        || -> Opened { Err(TransportError::PreSendConnectivity("no session".to_owned())) };
    let mut read_run = |_: &CancelFlag| {
        reads.set(reads.get() + 1);

        ReadRun {
            facts: Err("unused".to_owned()),
            runner_line: None,
            leftover: None,
            leftover_text: None,
            notes: Vec::new(),
        }
    };
    let result = reconcile(
        &Intent::Delete,
        &subject(),
        &mut open,
        &mut read_run,
        &mut clock,
        &CancelFlag::new(),
    );

    assert!(
        matches!(result.verdict, Verdict::Incomplete(_)),
        "{:?}",
        result.verdict
    );
    assert_eq!(reads.get(), 1, "the READ run is the last resort");
}

// --- Ctrl-C ----------------------------------------------------------------------------------

#[test]
fn a_first_ctrl_c_is_deferred_and_reconciliation_still_finishes() {
    let (sap, now) = setup(constant(World::default()));
    let cancel = CancelFlag::new();
    let signal = cancel.clone();
    let mut clock = FakeClock::new(&now);

    clock.hook = Some((
        1,
        Box::new(move || {
            // The process must stay alive while the reconciliation runs.
            assert_eq!(signal.on_interrupt(), InterruptAction::Deferred);
        }),
    ));

    let (result, _) = run_reconcile(&Intent::Delete, &sap, &mut clock, &cancel, Err("x".into()));

    assert_eq!(result.verdict, Verdict::DeletedReconciled);
    assert!(!result.skipped);
    assert!(cancel.is_set());
    assert_eq!(result.exit_code(cancel.is_set()), 130);
    assert_eq!(result.exit_code(false), 1);
}

#[test]
fn the_ctrl_c_is_exit_now_again_once_the_reconciliation_is_over() {
    let (sap, now) = setup(constant(World::default()));
    let cancel = CancelFlag::new();
    let mut clock = FakeClock::new(&now);

    run_reconcile(&Intent::Delete, &sap, &mut clock, &cancel, Err("x".into()));

    assert_eq!(cancel.on_interrupt(), InterruptAction::ExitNow);
}

#[test]
fn a_second_ctrl_c_during_the_wait_skips_the_rest() {
    let (sap, now) = setup(constant(present()));
    let cancel = CancelFlag::new();
    let signal = cancel.clone();
    let mut clock = FakeClock::new(&now);

    clock.hook = Some((
        2,
        Box::new(move || {
            assert_eq!(signal.on_interrupt(), InterruptAction::Deferred);
            assert_eq!(signal.on_interrupt(), InterruptAction::Deferred);
        }),
    ));

    let intent = Intent::Deploy {
        file_hash: FILE,
        base_hash: BASE,
    };
    let (result, reads) = run_reconcile(&intent, &sap, &mut clock, &cancel, Err("x".into()));

    assert!(result.skipped);
    assert!(
        matches!(&result.verdict, Verdict::Incomplete(reason) if reason.contains("interrupt")),
        "{:?}",
        result.verdict
    );
    assert_eq!(result.verdict.status(), Status::Partial);
    assert_eq!(result.exit_code(true), 130);
    assert_eq!(reads, 0, "no READ run after the second Ctrl-C");
}

#[test]
fn a_second_ctrl_c_before_the_read_run_starts_no_run() {
    // The interrupt arrives while the second snapshot is read; the state is settled, but the
    // READ run does not start.
    let cancel = CancelFlag::new();
    let signal = cancel.clone();
    let _held = cancel.hold_write();
    let (sap, now) = setup(Box::new(move |tick: &Tick| {
        if tick.previews == 5 {
            signal.on_interrupt();
            signal.on_interrupt();
        }

        present()
    }));
    let mut clock = FakeClock::new(&now);
    let intent = Intent::Deploy {
        file_hash: FILE,
        base_hash: BASE,
    };
    let (result, reads) = run_reconcile(
        &intent,
        &sap,
        &mut clock,
        &cancel,
        Ok(ReadFacts::Found {
            hash: FILE.to_owned(),
        }),
    );

    assert!(result.skipped, "{:?}", result.verdict);
    assert_eq!(reads, 0);
}

#[test]
fn the_second_interrupt_is_counted_per_flag_and_shared_by_clones() {
    let cancel = CancelFlag::new();
    let clone = cancel.clone();
    let _held = cancel.hold_write();

    assert!(!cancel.is_second_interrupt());
    assert_eq!(cancel.on_interrupt(), InterruptAction::Deferred);
    assert!(!clone.is_second_interrupt());
    assert_eq!(clone.on_interrupt(), InterruptAction::Deferred);
    assert!(cancel.is_second_interrupt());
    assert!(!CancelFlag::new().is_second_interrupt());
}

// --- end to end: a lost classrun reply on both backends --------------------------------------

#[derive(Clone, Copy, Debug)]
enum Backend {
    Http,
    Rfc,
}

fn build(name: &str) -> Result<String, String> {
    Ok(format!("CLASS {name} DEFINITION.\nENDCLASS.\n"))
}

fn input(backend: Backend, operation: &'static str) -> CycleInput<'static> {
    CycleInput {
        operation,
        sid: "DEV",
        client: "100",
        conn: "dev",
        prefix: PREFIX,
        user: "DEVUSER",
        language: "EN",
        backend: match backend {
            Backend::Http => BackendKind::AdtHttp,
            Backend::Rfc => BackendKind::RfcSadt,
        },
        rfc_available: true,
        build: &build,
    }
}

fn opener(backend: Backend, sap: &Shared) -> Opened {
    match backend {
        Backend::Http => http_session(sap),
        Backend::Rfc => rfc_session(sap),
    }
}

/// One cycle that posts a classrun whose reply is `reply`.
fn cycle(
    backend: Backend,
    sap: &Shared,
    operation: &'static str,
    cancel: &CancelFlag,
) -> RunnerRun {
    let mut open = || opener(backend, sap);

    run_cycle(
        &input(backend, operation),
        &mut open,
        PostRoute::Cycle,
        cancel,
    )
}

/// The whole path: the cycle, `settle`, then `reconcile` with a real READ cycle.
fn end_to_end(
    backend: Backend,
    sap: &Shared,
    now: &Rc<Cell<Duration>>,
    intent: &Intent<'_>,
    mode: RunMode,
    cancel: &CancelFlag,
) -> (RunnerRun, Option<Cause>, Reconciled) {
    let mut run = cycle(backend, sap, "deploy DTEL", cancel);
    let cause = settle(&mut run, &nonce(), mode);
    let name = run.name.clone().expect("a runner name");
    let mut open = || opener(backend, sap);
    let mut read_run = |fresh: &CancelFlag| {
        let mut open_cycle = || opener(backend, sap);
        let read = run_cycle(
            &input(backend, "read DTEL"),
            &mut open_cycle,
            PostRoute::Cycle,
            fresh,
        );

        ReadRun::from_run(&read, &nonce())
    };
    let mut clock = FakeClock::new(now);
    let subject = Subject {
        kind: Kind::Dtel,
        name: OBJECT,
        runner: &name,
    };
    let result = reconcile(
        intent,
        &subject,
        &mut open,
        &mut read_run,
        &mut clock,
        cancel,
    );

    (run, cause, result)
}

const BACKENDS: [Backend; 2] = [Backend::Http, Backend::Rfc];

#[test]
fn a_lost_reply_of_a_deploy_is_reconciled_on_both_backends() {
    let cases = [
        (FILE, true, Verdict::ActiveReconciled),
        (BASE, true, Verdict::Inactive),
        (BASE, false, Verdict::PartialUnknown),
        ("OTHER", false, Verdict::PartialUnexpected),
    ];

    for backend in BACKENDS {
        for (hash, inactive, verdict) in cases.clone() {
            let (sap, now) = setup(constant(World {
                definition: true,
                directory: true,
                inactive,
                ..World::default()
            }));

            sap.borrow_mut().post_replies =
                [PostReply::Lost, PostReply::Output(read_ok(hash))].into();

            let intent = Intent::Deploy {
                file_hash: FILE,
                base_hash: BASE,
            };
            let (run, cause, result) = end_to_end(
                backend,
                &sap,
                &now,
                &intent,
                RunMode::Deploy,
                &CancelFlag::new(),
            );
            let sap = sap.borrow();

            assert!(matches!(cause, Some(Cause::LostReply(_))), "{backend:?}");
            assert!(matches!(run.end, RunnerEnd::PostUncertain(_)));
            assert_eq!(result.verdict, verdict, "{backend:?} {hash} {inactive}");
            assert_eq!(result.verdict.exit(), 1);
            // The deploy POST is never resent: one POST for the deploy, one for the READ run.
            assert_eq!(sap.posts, 2, "{backend:?}");
            assert_eq!(
                sap.log
                    .iter()
                    .filter(|(_, line)| line.starts_with("POST /sap/bc/adt/oo/classrun/"))
                    .map(|(_, line)| line.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                2,
                "two different classes, each posted once"
            );
            assert!(sap.classes.is_empty(), "every runner class is removed");
            assert!(run.leftover.is_none());
            assert!(result.read_run.as_ref().unwrap().leftover.is_none());
        }
    }
}

#[test]
fn a_lost_reply_of_a_create_is_reconciled_on_both_backends() {
    for backend in BACKENDS {
        for (world, verdict) in [
            (present(), Verdict::CreatePartial),
            (World::default(), Verdict::CreateRejected),
        ] {
            let (sap, now) = setup(constant(world));

            sap.borrow_mut().post_replies = [PostReply::Lost].into();

            let (_, cause, result) = end_to_end(
                backend,
                &sap,
                &now,
                &Intent::Create,
                RunMode::Create,
                &CancelFlag::new(),
            );

            assert!(cause.is_some());
            assert_eq!(result.verdict, verdict, "{backend:?}");
            assert_eq!(
                sap.borrow().posts,
                1,
                "no extra READ run: the evidence is enough"
            );
            assert!(result.read_run.is_none());
            assert!(sap.borrow().classes.is_empty());
        }
    }
}

#[test]
fn a_lost_reply_of_a_delete_is_reconciled_on_both_backends() {
    for backend in BACKENDS {
        for (world, verdict) in [
            (World::default(), Verdict::DeletedReconciled),
            (present(), Verdict::DeletePartial),
        ] {
            let (sap, now) = setup(constant(world));

            sap.borrow_mut().post_replies = [PostReply::Lost].into();

            let (_, _, result) = end_to_end(
                backend,
                &sap,
                &now,
                &Intent::Delete,
                RunMode::Delete,
                &CancelFlag::new(),
            );

            assert_eq!(result.verdict, verdict, "{backend:?}");
            assert_eq!(sap.borrow().posts, 1);
        }
    }
}

#[test]
fn a_5xx_and_a_broken_protocol_are_reconciled_like_a_lost_reply() {
    let replies = [
        PostReply::Status(500, "<html>Application Server Error</html>".to_owned()),
        PostReply::Status(502, "<html>Bad Gateway</html>".to_owned()),
        PostReply::Output(format!("ABAPREACH:{NONCE}:BEGIN:DELETE\n\n")),
    ];

    for backend in BACKENDS {
        for reply in replies.clone() {
            let (sap, now) = setup(constant(World::default()));

            sap.borrow_mut().post_replies = [reply.clone()].into();

            let (run, cause, result) = end_to_end(
                backend,
                &sap,
                &now,
                &Intent::Delete,
                RunMode::Delete,
                &CancelFlag::new(),
            );

            assert!(matches!(cause, Some(Cause::Broken(_))), "{reply:?}");
            assert!(run.uncertain, "the batch must stop");
            assert_eq!(result.verdict, Verdict::DeletedReconciled);
            assert_eq!(sap.borrow().posts, 1, "never resent");
        }
    }
}

#[test]
fn a_whole_run_is_not_reconciled() {
    let (sap, _) = setup(constant(present()));

    sap.borrow_mut().post_replies = [PostReply::Output(output("DELETE", &[], "DELETED"))].into();

    let mut run = cycle(Backend::Http, &sap, "delete DTEL", &CancelFlag::new());

    assert_eq!(settle(&mut run, &nonce(), RunMode::Delete), None);
    assert!(!run.uncertain);
}

#[test]
fn a_ctrl_c_during_the_post_reconciles_first_and_exits_130_with_the_status() {
    for backend in BACKENDS {
        let (sap, now) = setup(constant(World::default()));
        let cancel = CancelFlag::new();

        sap.borrow_mut().post_replies = [PostReply::Lost].into();

        // The first Ctrl-C arrives while the POST is in flight: the process must stay alive.
        let signal = cancel.clone();

        sap.borrow_mut().on_post = Some(Box::new(move || {
            assert_eq!(signal.on_interrupt(), InterruptAction::Deferred);
        }));

        let mut run = cycle(backend, &sap, "delete DTEL", &cancel);

        assert!(run.interrupted);
        assert!(settle(&mut run, &nonce(), RunMode::Delete).is_some());

        let name = run.name.clone().unwrap();
        let mut open = || opener(backend, &sap);
        let mut read_run = |_: &CancelFlag| -> ReadRun { panic!("evidence is enough") };
        let mut clock = FakeClock::new(&now);
        let subject = Subject {
            kind: Kind::Dtel,
            name: OBJECT,
            runner: &name,
        };
        let result = reconcile(
            &Intent::Delete,
            &subject,
            &mut open,
            &mut read_run,
            &mut clock,
            &cancel,
        );

        assert_eq!(result.verdict, Verdict::DeletedReconciled, "{backend:?}");
        assert!(!result.skipped);
        assert_eq!(result.exit_code(run.interrupted), 130);
        assert!(sap.borrow().classes.is_empty());
    }
}

#[test]
fn an_uncertain_row_stops_the_batch_and_the_rest_are_skipped() {
    for backend in BACKENDS {
        let (sap, _) = setup(constant(present()));

        sap.borrow_mut().post_replies = [PostReply::Lost].into();

        let rows = run_rows(&[1, 2, 3], |_| {
            let mut run = cycle(backend, &sap, "delete DTEL", &CancelFlag::new());

            settle(&mut run, &nonce(), RunMode::Delete);

            run
        });

        assert!(matches!(rows[0], Row::Ran(_)), "{backend:?}");
        assert!(matches!(rows[1], Row::Skipped));
        assert!(matches!(rows[2], Row::Skipped));
        assert_eq!(sap.borrow().posts, 1, "the later rows never ran");
        assert!(sap.borrow().classes.is_empty());
    }
}

#[test]
fn a_row_that_answered_is_not_uncertain_and_does_not_stop_the_batch() {
    let (sap, _) = setup(constant(present()));
    let whole = || PostReply::Output(output("DELETE", &[], "DELETED"));

    sap.borrow_mut().post_replies = [whole(), whole()].into();

    let rows = run_rows(&[1, 2], |_| {
        let mut run = cycle(Backend::Http, &sap, "delete DTEL", &CancelFlag::new());

        assert_eq!(settle(&mut run, &nonce(), RunMode::Delete), None);

        run
    });

    assert!(rows.iter().all(|row| matches!(row, Row::Ran(_))));
    assert_eq!(sap.borrow().posts, 2);
}

#[test]
fn a_leftover_of_the_read_run_is_carried_to_the_result() {
    let (sap, now) = setup(constant(present()));

    sap.borrow_mut().post_replies = [PostReply::Lost, PostReply::Output(read_ok(FILE))].into();
    // Both deletes of the READ class fail; the first class is removed by its own cycle.
    let intent = Intent::Deploy {
        file_hash: FILE,
        base_hash: BASE,
    };
    let mut run = cycle(Backend::Rfc, &sap, "deploy DTEL", &CancelFlag::new());

    settle(&mut run, &nonce(), RunMode::Deploy);
    sap.borrow_mut().failing_deletes = 2;

    let name = run.name.clone().unwrap();
    let mut open = || opener(Backend::Rfc, &sap);
    let mut read_run = |fresh: &CancelFlag| {
        let mut open_cycle = || opener(Backend::Rfc, &sap);
        let read = run_cycle(
            &input(Backend::Rfc, "read DTEL"),
            &mut open_cycle,
            PostRoute::Cycle,
            fresh,
        );

        ReadRun::from_run(&read, &nonce())
    };
    let mut clock = FakeClock::new(&now);
    let subject = Subject {
        kind: Kind::Dtel,
        name: OBJECT,
        runner: &name,
    };
    let result = reconcile(
        &intent,
        &subject,
        &mut open,
        &mut read_run,
        &mut clock,
        &CancelFlag::new(),
    );
    let read = result.read_run.expect("the READ run ran");

    assert_eq!(result.verdict, Verdict::ActiveReconciled);
    assert!(read.leftover.is_some());
    assert!(read.leftover_text.unwrap().starts_with("RUNNER_LEFTOVER: "));
    assert!(read.runner_line.unwrap().starts_with("RUNNER: "));
}

#[test]
fn the_system_clock_waits_for_real_and_stops_early_on_abort() {
    use abapreach::operation::runner_reconcile::SystemClock;

    let mut clock = SystemClock::new();
    let started = std::time::Instant::now();

    assert!(clock.wait(Duration::from_millis(60), &|| false));
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert!(clock.elapsed() >= Duration::from_millis(60));

    let started = std::time::Instant::now();

    assert!(!clock.wait(Duration::from_secs(30), &|| true));
    assert!(started.elapsed() < Duration::from_secs(5));
}

// --- cargo-mutants triage (M6 Task 33): a note that no test pinned ---------------------------

/// A session whose close fails after it was used.
struct BadClose(Box<dyn SadtTransport>);

impl SadtTransport for BadClose {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.0.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.0.close();

        Err(TransportError::Protocol("close refused".to_owned()))
    }
}

#[test]
fn a_session_close_failure_is_noted_once_however_many_reads_see_it() {
    let (sap, now) = setup(constant(World::default()));
    let mut clock = FakeClock::new(&now);
    let mut open = || -> Opened {
        let session = rfc_session(&sap)?;

        Ok(Box::new(BadClose(session)))
    };
    let mut read_run = |_: &CancelFlag| -> ReadRun { panic!("a delete needs no READ run") };
    let result = reconcile(
        &Intent::Delete,
        &subject(),
        &mut open,
        &mut read_run,
        &mut clock,
        &CancelFlag::new(),
    );

    assert_eq!(result.verdict, Verdict::DeletedReconciled);
    // The evidence was read at least twice to settle, and every read closed a failing session.
    assert!(clock.waits.len() >= 2, "{:?}", clock.waits);
    assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
    assert!(
        result.notes[0].starts_with("W: session close failed:"),
        "{:?}",
        result.notes
    );
}
