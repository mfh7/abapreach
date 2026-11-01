mod common;
mod replay;

use abapreach::backend::Failure;
use abapreach::domain::Status;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{CancelFlag, DeployInput, DeployProg, InactiveRead, reconcile};
use abapreach::transport::{Response, TransportError};
use replay::{Replay, Step, fx, replay};

const FIXTURE: &str = "dev-750-rfc.json";
const RETRY: &str = "check with read before retrying";

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

fn lost() -> Result<Response, TransportError> {
    Err(TransportError::PostSendUncertain(
        "connection reset".to_owned(),
    ))
}

/// Every way the read-only inactive GET can fail, with the exact reason it reports.
fn inactive_failures() -> Vec<(Result<Response, TransportError>, String)> {
    let mut not_utf8 = Response::text(200, "");
    not_utf8.body = vec![0xff, 0xfe];
    let utf8 = String::from_utf8(not_utf8.body.clone()).unwrap_err();

    vec![
        (Ok(Response::empty(401)), "HTTP 401".to_owned()),
        (Ok(Response::empty(403)), "HTTP 403".to_owned()),
        (Ok(Response::empty(500)), "HTTP 500".to_owned()),
        (
            lost(),
            "post-send outcome uncertain: connection reset".to_owned(),
        ),
        (Ok(not_utf8), format!("not UTF-8: {utf8}")),
    ]
}

fn input(last_line: &str) -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            last_line.to_owned(),
        ],
        base: None,
        simulate: false,
        tr: None,
    }
}

/// v2 over the recorded v1: lock, locked re-read, check, then the recorded PUT of v2.
fn v2_until_put() -> Vec<Step> {
    vec![fx(6), fx(7), fx(6), fx(8)]
}

/// The invalid source over the recorded v2: the check is answered as valid so the PUT happens.
fn invalid_until_put() -> Vec<Step> {
    vec![fx(20), fx(21), fx(20), Step::Pair(12, 8)]
}

fn run(transport: &mut Replay, deploy: DeployInput) -> abapreach::operation::deploy::DeployResult {
    DeployProg::run_classified(transport, deploy, None, &CancelFlag::new())
}

fn steps(mut head: Vec<Step>, tail: Vec<Step>) -> Vec<Step> {
    head.extend(tail);
    head
}

fn count(transport: &Replay, needle: &str) -> usize {
    transport
        .calls
        .iter()
        .filter(|call| call.uri.contains(needle))
        .count()
}

#[test]
fn active_equal_to_the_file_is_reconciled_as_active_with_exit_1() {
    let (status, message) = reconcile(&lines("file"), &InactiveRead::Absent, "h", &lines("file"));

    assert_eq!(status, Status::Active);
    assert_eq!(
        message,
        format!("ACTIVE (reconciled): the active source equals the file; {RETRY}")
    );
}

#[test]
fn active_unchanged_with_the_file_inactive_means_the_put_arrived() {
    let live = source_hash(&lines("live"));
    let (status, message) = reconcile(
        &lines("live"),
        &InactiveRead::Read(lines("file")),
        &live,
        &lines("file"),
    );

    assert_eq!(status, Status::Inactive);
    assert_eq!(
        message,
        format!("INACTIVE: the PUT reached SAP but the activation did not; {RETRY}")
    );
}

#[test]
fn active_unchanged_without_a_usable_inactive_is_outcome_unknown() {
    let live = source_hash(&lines("live"));
    let unknown = format!("PARTIAL: outcome unknown; {RETRY}");
    let cases = [
        InactiveRead::Absent,
        InactiveRead::Read(lines("live")),
        InactiveRead::Read(lines("")),
        InactiveRead::Read(Vec::new()),
    ];

    for inactive in cases {
        let (status, message) = reconcile(&lines("live"), &inactive, &live, &lines("file"));

        assert_eq!(
            (status, message),
            (Status::Partial, unknown.clone()),
            "{inactive:?}"
        );
    }
}

#[test]
fn anything_else_is_an_unexpected_state() {
    let live = source_hash(&lines("live"));
    let unexpected = format!("PARTIAL: unexpected state; {RETRY}");
    let (status, message) = reconcile(
        &lines("live"),
        &InactiveRead::Read(lines("other")),
        &live,
        &lines("file"),
    );

    assert_eq!((status, message), (Status::Partial, unexpected.clone()));

    let (status, message) = reconcile(
        &lines("other"),
        &InactiveRead::Absent,
        &live,
        &lines("file"),
    );

    assert_eq!((status, message), (Status::Partial, unexpected.clone()));

    let (status, message) = reconcile(
        &lines("other"),
        &InactiveRead::Read(lines("file")),
        &live,
        &lines("file"),
    );

    assert_eq!((status, message), (Status::Partial, unexpected));
}

#[test]
fn crlf_and_trailing_blanks_do_not_change_the_classification() {
    let live = source_hash(&lines("live"));
    let (status, _) = reconcile(
        &lines("file  \r\n\n"),
        &InactiveRead::Absent,
        &live,
        &lines("file"),
    );

    assert_eq!(status, Status::Active);
}

#[test]
fn an_unreadable_inactive_source_while_the_active_is_unchanged_is_reconciliation_incomplete() {
    let live = source_hash(&lines("live"));
    let (status, message) = reconcile(
        &lines("live"),
        &InactiveRead::Unreadable("HTTP 500".to_owned()),
        &live,
        &lines("file"),
    );

    assert_eq!(
        (status, message),
        (
            Status::Partial,
            format!(
                "PARTIAL: reconciliation incomplete, inactive source unreadable (HTTP 500); {RETRY}"
            )
        )
    );
}

#[test]
fn an_unreadable_inactive_source_with_the_file_active_says_the_inactive_state_is_unknown() {
    let reason = "not UTF-8: invalid utf-8 sequence of 1 bytes from index 0";
    let (status, message) = reconcile(
        &lines("file"),
        &InactiveRead::Unreadable(reason.to_owned()),
        "h",
        &lines("file"),
    );

    assert_eq!(status, Status::Active);
    assert!(message.starts_with("ACTIVE (reconciled), inactive state unknown"));
    assert!(message.contains(reason));
    assert!(message.ends_with(RETRY));
}

#[test]
fn a_lost_activation_with_the_file_active_is_active_reconciled_and_never_retried() {
    let tail = vec![
        fx(17),
        fx(18),
        Step::ReplyTo(19, lost()),
        fx(20),
        Step::Pair(26, 26),
    ];
    let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
    let result = run(&mut transport, input("WRITE / 'v2'."));

    assert_eq!(result.outcome.status, Some(Status::Active));
    assert_eq!(result.outcome.exit, 1);
    assert!(result.outcome.stdout.ends_with(&format!(
        "ACTIVE (reconciled): the active source equals the file; {RETRY}"
    )));
    assert_eq!(count(&transport, "version=inactive"), 1);
    assert!(!result.fallback_allowed);
    assert_eq!(count(&transport, "activation"), 1);
    assert_eq!(
        transport
            .calls
            .iter()
            .filter(|call| call.method == "PUT")
            .count(),
        1
    );
    assert_eq!(transport.closes, 1);
    transport.assert_finished();
}

#[test]
fn a_lost_activation_with_the_file_inactive_is_inactive() {
    let tail = vec![fx(22), fx(23), Step::ReplyTo(24, lost()), fx(25), fx(26)];
    let mut transport = replay(FIXTURE, steps(invalid_until_put(), tail));
    let result = run(&mut transport, input("WRITE / no_such_var."));

    assert_eq!(result.outcome.status, Some(Status::Inactive));
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .contains("INACTIVE: the PUT reached SAP")
    );
    assert!(result.outcome.stdout.ends_with(RETRY));
    transport.assert_finished();
}

#[test]
fn an_unchanged_active_with_no_evidence_of_the_put_is_outcome_unknown() {
    let missing = [
        Step::ReplyTo(26, Ok(Response::empty(404))),
        Step::Pair(26, 25),
    ];

    for inactive in missing {
        let tail = vec![fx(22), fx(23), Step::ReplyTo(24, lost()), fx(25), inactive];
        let mut transport = replay(FIXTURE, steps(invalid_until_put(), tail));
        let result = run(&mut transport, input("WRITE / no_such_var."));

        assert_eq!(result.outcome.status, Some(Status::Partial));
        assert_eq!(result.outcome.exit, 1);
        assert!(result.outcome.stdout.contains("PARTIAL: outcome unknown"));
        transport.assert_finished();
    }
}

#[test]
fn another_active_content_is_an_unexpected_state() {
    let other = Step::ReplyTo(25, Ok(Response::text(200, "REPORT other.\n")));
    let tail = vec![fx(22), fx(23), Step::ReplyTo(24, lost()), other, fx(26)];
    let mut transport = replay(FIXTURE, steps(invalid_until_put(), tail));
    let result = run(&mut transport, input("WRITE / no_such_var."));

    assert_eq!(result.outcome.status, Some(Status::Partial));
    assert_eq!(result.outcome.exit, 1);
    assert!(result.outcome.stdout.contains("PARTIAL: unexpected state"));
    transport.assert_finished();
}

#[test]
fn a_lost_put_unlocks_first_and_then_reconciles_by_content() {
    let inactive_is_file = Step::Pair(26, 20);
    let tail = vec![
        Step::ReplyTo(17, lost()),
        fx(18),
        Step::Pair(20, 10),
        inactive_is_file,
    ];
    let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
    let result = run(&mut transport, input("WRITE / 'v2'."));
    let order = transport.uris();

    assert_eq!(result.outcome.status, Some(Status::Inactive));
    assert_eq!(result.outcome.exit, 1);
    assert!(order[5].contains("_action=UNLOCK"));
    assert!(order[6].ends_with("version=active") && order[7].ends_with("version=inactive"));
    assert_eq!(count(&transport, "activation"), 0);
    assert_eq!(
        transport
            .calls
            .iter()
            .filter(|call| call.method == "PUT")
            .count(),
        1
    );
    assert!(!result.fallback_allowed);
    transport.assert_finished();
}

#[test]
fn a_lost_put_with_a_failed_unlock_still_reconciles_and_names_the_lock() {
    let tail = vec![
        Step::ReplyTo(17, lost()),
        Step::ReplyTo(18, lost()),
        Step::Pair(20, 10),
        Step::Pair(26, 5),
    ];
    let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
    let result = run(&mut transport, input("WRITE / 'v2'."));

    assert_eq!(result.outcome.status, Some(Status::Partial));
    assert_eq!(result.outcome.exit, 1);
    assert!(result.outcome.stdout.contains("PARTIAL: outcome unknown"));
    assert!(result.outcome.stderr.contains("UNLOCK FAILED"));
    assert!(
        result
            .outcome
            .stderr
            .contains("the lock on YTEST_ABAPREACH01 may remain")
    );
    transport.assert_finished();
}

#[test]
fn a_lost_read_back_after_activation_is_reconciled() {
    let tail = vec![
        fx(17),
        fx(18),
        fx(19),
        Step::ReplyTo(20, lost()),
        fx(20),
        fx(26),
    ];
    let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
    let result = run(&mut transport, input("WRITE / 'v2'."));

    assert_eq!(result.outcome.status, Some(Status::Active));
    assert_eq!(result.outcome.exit, 1);
    assert!(result.outcome.stdout.contains("ACTIVE (reconciled)"));
    transport.assert_finished();
}

#[test]
fn a_failed_inactive_read_while_the_active_is_unchanged_is_reconciliation_incomplete() {
    for (reply, reason) in inactive_failures() {
        let tail = vec![
            fx(22),
            fx(23),
            Step::ReplyTo(24, lost()),
            fx(25),
            Step::ReplyTo(26, reply),
        ];
        let mut transport = replay(FIXTURE, steps(invalid_until_put(), tail));
        let result = run(&mut transport, input("WRITE / no_such_var."));

        assert_eq!(result.outcome.status, Some(Status::Partial), "{reason}");
        assert_eq!(result.outcome.exit, 1, "{reason}");
        assert!(
            result.outcome.stdout.contains(&format!(
                "PARTIAL: reconciliation incomplete, inactive source unreadable ({reason})"
            )),
            "{reason}: {}",
            result.outcome.stdout
        );
        assert!(result.outcome.stdout.ends_with(RETRY), "{reason}");
        assert_eq!(count(&transport, "activation"), 1, "{reason}");
        transport.assert_finished();
    }
}

#[test]
fn a_failed_inactive_read_with_the_file_active_is_active_reconciled_and_unknown() {
    for (reply, reason) in inactive_failures() {
        let tail = vec![
            fx(17),
            fx(18),
            Step::ReplyTo(19, lost()),
            fx(20),
            Step::ReplyTo(26, reply),
        ];
        let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
        let result = run(&mut transport, input("WRITE / 'v2'."));

        assert_eq!(result.outcome.status, Some(Status::Active), "{reason}");
        assert_eq!(result.outcome.exit, 1, "{reason}");
        assert!(
            result
                .outcome
                .stdout
                .contains("ACTIVE (reconciled), inactive state unknown"),
            "{reason}: {}",
            result.outcome.stdout
        );
        assert!(result.outcome.stdout.contains(&reason), "{reason}");
        assert!(result.outcome.stdout.ends_with(RETRY), "{reason}");
        transport.assert_finished();
    }
}

#[test]
fn an_unreadable_active_source_is_outcome_unknown_not_a_guess() {
    let tail = vec![
        fx(17),
        fx(18),
        Step::ReplyTo(19, lost()),
        Step::ReplyTo(20, lost()),
    ];
    let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
    let result = run(&mut transport, input("WRITE / 'v2'."));

    assert_eq!(result.outcome.status, Some(Status::Partial));
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .contains("PARTIAL: outcome unknown, the active source cannot be read")
    );
    assert!(result.outcome.stdout.ends_with(RETRY));
    transport.assert_finished();
}

#[test]
fn an_activation_that_was_not_sent_is_inactive_without_reconciliation() {
    let unsent = Err(TransportError::PreSendConnectivity("no route".to_owned()));
    let tail = vec![fx(17), fx(18), Step::ReplyTo(19, unsent)];
    let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
    let result = run(&mut transport, input("WRITE / 'v2'."));

    assert_eq!(result.outcome.status, Some(Status::Inactive));
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .contains("INACTIVE: the activation was not sent")
    );
    assert_eq!(result.failure, Some(Failure::PreSendConnectivity));
    assert!(!result.fallback_allowed);
    transport.assert_finished();
}

#[test]
fn only_a_post_send_uncertain_read_back_is_reconciled_other_failures_are_unknown() {
    let refused = || Ok(Response::empty(500));
    let unsent = || Err(TransportError::PreSendConnectivity("no route".to_owned()));

    for reply in [refused(), unsent()] {
        let tail = vec![fx(17), fx(18), fx(19), Step::ReplyTo(20, reply)];
        let mut transport = replay(FIXTURE, steps(v2_until_put(), tail));
        let result = run(&mut transport, input("WRITE / 'v2'."));

        assert_eq!(result.outcome.status, Some(Status::Partial));
        assert_eq!(result.outcome.exit, 1);
        assert!(
            result
                .outcome
                .stdout
                .contains("PARTIAL: outcome unknown, the active source cannot be read")
        );
        assert!(result.outcome.stdout.ends_with(RETRY));
        assert_eq!(count(&transport, "version=inactive"), 0);
        transport.assert_finished();
    }
}
