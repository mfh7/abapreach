mod common;
mod replay;

use abapreach::backend::Failure;
use abapreach::config::Connection;
use abapreach::domain::Status;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{CancelFlag, DeployInput, DeployProg, encode_lock_handle};
use abapreach::transport::{Response, TransportError};
use replay::{Step, fx, no_inactive, replay};

const FIXTURE: &str = "dev-750-rfc.json";
const V1_HASH: &str = "63886692bf652fa419147f746d6b7760d6c991606fa9603d8ade3caf08ac7c64";

fn input(source: &[&str], base: Option<&str>) -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: source.iter().map(|line| (*line).to_owned()).collect(),
        base: base.map(str::to_owned),
        simulate: true,
        tr: None,
    }
}

fn v1(base: Option<&str>) -> DeployInput {
    input(&["REPORT ytest_abapreach01.", "", "WRITE / 'v1'."], base)
}

fn v2() -> DeployInput {
    input(&["REPORT ytest_abapreach01.", "", "WRITE / 'v2'."], None)
}

fn changed_reread() -> Step {
    Step::ReplyTo(6, Ok(Response::text(200, "REPORT changed.\n")))
}

#[test]
fn no_change_replays_fixture_v1_and_includes_tls_warning() {
    let mut transport = replay(FIXTURE, vec![fx(6)]);
    let outcome = DeployProg::run(&mut transport, v1(None), Some(&Connection::test_insecure()));

    assert_eq!(source_hash(&v1(None).source), V1_HASH);
    assert_eq!(outcome.exit, 0);
    assert_eq!(outcome.status, None);
    assert_eq!(
        outcome.stdout,
        "WARNING: connection DEV_100 uses insecure TLS or HTTP.\nno changes"
    );
    assert_eq!(transport.calls.len(), 1);
    assert_eq!(transport.closes, 1);
    transport.assert_finished();
}

#[test]
fn initial_base_mismatch_stops_before_lock() {
    let mut transport = replay(FIXTURE, vec![fx(6)]);
    let outcome = DeployProg::run(&mut transport, v1(Some("00")), None);

    assert_eq!(outcome.exit, 2);
    assert!(outcome.stderr.starts_with("STOP: live changed:"));
    assert!(outcome.stderr.contains(V1_HASH));
    assert!(outcome.stderr.contains("--base 00"));
    assert_eq!(transport.calls.len(), 1);
    transport.assert_finished();

    let mut transport = replay(FIXTURE, vec![fx(6)]);
    let outcome = DeployProg::run(&mut transport, v1(Some(V1_HASH)), None);

    assert_eq!((outcome.exit, outcome.stdout.as_str()), (0, "no changes"));
}

#[test]
fn diff_and_live_hash_precede_locked_reread_and_unlock() {
    let mut transport = replay(
        FIXTURE,
        vec![fx(6), no_inactive(), fx(7), changed_reread(), fx(9)],
    );
    let outcome = DeployProg::run(&mut transport, v2(), None);
    let position = |needle: &str| outcome.stdout.find(needle).unwrap();

    assert_eq!(outcome.exit, 2);
    assert!(
        outcome
            .stderr
            .starts_with("STOP: live changed since initial read")
    );
    assert!(outcome.stdout.starts_with("--- live\n+++ file\n@@"));
    assert!(position("@@") < position("live hash"));
    assert!(outcome.stdout.contains("-WRITE / 'v1'.\n+WRITE / 'v2'."));
    assert!(outcome.stdout.ends_with(&format!("live hash {V1_HASH}")));
    assert_eq!(
        transport
            .uris()
            .iter()
            .map(|call| call.split('?').next().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "GET /sap/bc/adt/programs/programs/ytest_abapreach01/source/main",
            "GET /sap/bc/adt/programs/programs/ytest_abapreach01/source/main/versions",
            "POST /sap/bc/adt/programs/programs/ytest_abapreach01",
            "GET /sap/bc/adt/programs/programs/ytest_abapreach01/source/main",
            "POST /sap/bc/adt/programs/programs/ytest_abapreach01",
        ]
    );
    assert_eq!(transport.closes, 1);
    transport.assert_finished();
}

#[test]
fn cancel_after_lock_unlocks_and_closes() {
    let mut transport = replay(FIXTURE, vec![fx(6), no_inactive(), fx(7), fx(9)]);
    let cancel = CancelFlag::new();

    cancel.set();

    let outcome = DeployProg::run_with_cancel(&mut transport, v2(), &cancel);

    assert_eq!(outcome.exit, 1);
    assert!(outcome.stderr.starts_with("CANCELED"));
    assert!(
        transport
            .calls
            .last()
            .unwrap()
            .uri
            .contains("_action=UNLOCK")
    );
    assert_eq!(transport.closes, 1);
    transport.assert_finished();
}

#[test]
fn invalid_input_stops_before_any_request() {
    let long = "x".repeat(256);
    let mut transport = replay(FIXTURE, vec![]);
    let outcome = DeployProg::run(&mut transport, input(&[&long], None), None);

    assert_eq!(outcome.exit, 2);
    assert!(outcome.stderr.starts_with("STOP: line 1 length 256"));
    assert!(transport.calls.is_empty());
}

#[test]
fn failure_class_and_fallback_permission_follow_the_failing_request() {
    let run = |steps: Vec<Step>| {
        let mut transport = replay(FIXTURE, steps);

        DeployProg::run_classified(&mut transport, v2(), None, &CancelFlag::new())
    };
    let pre_send = || Err(TransportError::PreSendConnectivity("no route".to_owned()));

    let first = run(vec![Step::Any(pre_send())]);

    assert_eq!(first.failure, Some(Failure::PreSendConnectivity));
    assert!(first.fallback_allowed);
    assert_eq!(first.outcome.exit, 1);
    assert!(
        first
            .outcome
            .stderr
            .starts_with("ERROR: pre-send connectivity failure")
    );

    let later = run(vec![fx(6), no_inactive(), Step::Any(pre_send())]);

    assert_eq!(later.failure, Some(Failure::PreSendConnectivity));
    assert!(!later.fallback_allowed);

    let uncertain = run(vec![Step::Any(Err(TransportError::PostSendUncertain(
        "x".to_owned(),
    )))]);

    assert_eq!(uncertain.failure, Some(Failure::PostSendUncertain));
    assert!(!uncertain.fallback_allowed);

    let denied = run(vec![Step::Any(Ok(Response::empty(401)))]);

    assert_eq!(denied.failure, Some(Failure::Authorization));
    assert!(!denied.fallback_allowed);
    assert_eq!(denied.outcome.exit, 2);
    assert!(
        denied
            .outcome
            .stderr
            .starts_with("STOP: cannot read live source: HTTP 401")
    );
}

#[test]
fn rejected_lock_is_reported_without_unlock() {
    let mut transport = replay(
        FIXTURE,
        vec![fx(6), no_inactive(), Step::Any(Ok(Response::empty(423)))],
    );
    let result = DeployProg::run_classified(&mut transport, v2(), None, &CancelFlag::new());

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .ends_with("REJECTED: lock failed: HTTP 423")
    );
    assert_eq!(result.failure, Some(Failure::Conflict));
    assert_eq!(transport.calls.len(), 3);

    let mut transport = replay(
        FIXTURE,
        vec![
            fx(6),
            no_inactive(),
            Step::Any(Ok(Response::text(200, "<DATA/>"))),
        ],
    );
    let outcome = DeployProg::run(&mut transport, v2(), None);

    assert_eq!(outcome.exit, 2);
    assert!(
        outcome
            .stderr
            .starts_with("STOP: unreadable lock response: no LOCK_HANDLE")
    );
    assert!(
        outcome
            .stderr
            .contains("the lock on YTEST_ABAPREACH01 may remain")
    );
    assert_eq!(transport.calls.len(), 3);
}

#[test]
fn an_unreadable_lock_reply_never_claims_the_lock_was_released() {
    let mut transport = replay(
        FIXTURE,
        vec![
            fx(6),
            no_inactive(),
            Step::Any(Ok(Response::text(200, "<DATA/>"))),
        ],
    )
    .failing_close(TransportError::PostSendUncertain("gone".to_owned()));
    let outcome = DeployProg::run(&mut transport, v2(), None);

    // The lock exists and its handle is unknown, so it cannot be released: no claim about it.
    assert_eq!(outcome.exit, 2);
    assert!(
        outcome
            .stderr
            .contains("the lock on YTEST_ABAPREACH01 may remain (the lock handle is unknown)")
    );
    assert!(
        !outcome.stderr.contains("lock was released"),
        "{}",
        outcome.stderr
    );
    assert_eq!(transport.closes, 1);
}

#[test]
fn failed_or_uncertain_unlock_forces_exit_1() {
    let failures = [
        (Ok(Response::empty(500)), "UNLOCK FAILED (HTTP 500)"),
        (
            Err(TransportError::PostSendUncertain("reset".to_owned())),
            "UNLOCK FAILED (post-send outcome uncertain: reset)",
        ),
    ];

    for (reply, note) in failures {
        let mut transport = replay(
            FIXTURE,
            vec![
                fx(6),
                no_inactive(),
                fx(7),
                changed_reread(),
                Step::ReplyTo(9, reply),
            ],
        );
        let outcome = DeployProg::run(&mut transport, v2(), None);

        assert_eq!(outcome.exit, 1);
        assert!(
            outcome
                .stderr
                .starts_with("STOP: live changed since initial read")
        );
        assert!(outcome.stderr.contains(note), "{}", outcome.stderr);
        assert!(
            outcome
                .stderr
                .contains("the lock on YTEST_ABAPREACH01 may remain")
        );
        assert_eq!(transport.closes, 1);
    }
}

#[test]
fn lock_handle_is_percent_encoded() {
    assert_eq!(encode_lock_handle("a+b/c="), "a%2Bb%2Fc%3D");
    assert_eq!(encode_lock_handle("AZaz09-_.~"), "AZaz09-_.~");
    assert_eq!(
        encode_lock_handle("qdbcyR6xoaD+xAqwYue9KQ4XHbM="),
        "qdbcyR6xoaD%2BxAqwYue9KQ4XHbM%3D"
    );
}
