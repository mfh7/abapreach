mod common;
mod replay;

use abapreach::backend::Failure;
use abapreach::domain::Status;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{CancelFlag, DeployInput, DeployProg, activation_status};
use abapreach::transport::rfc::{RfcPhase, classify_rfc_error};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use replay::{Replay, Step, fx, replay};

const FIXTURES: [&str; 3] = ["dev-750-rfc.json", "dev-750-http.json", "dv2-740-rfc.json"];
const FIXTURE: &str = "dev-750-rfc.json";
const PROGRAM: &str = "/sap/bc/adt/programs/programs/ytest_abapreach01";
const E_BODY: &[u8] = br#"<chkl:messages xmlns:chkl="x"><msg type="E" line="1"/></chkl:messages>"#;
const W_BODY: &[u8] = br#"<chkl:messages xmlns:chkl="x"><msg type="W" line="1"/></chkl:messages>"#;
const ROLLBACK: &str = "ROLLBACK: the inactive version belongs to you; GET without a version returns it; restore only by deploying the base source again on your order";

fn input(last_line: &str, simulate: bool, tr: Option<&str>) -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            last_line.to_owned(),
        ],
        base: None,
        simulate,
        tr: tr.map(str::to_owned),
    }
}

fn v2(tr: Option<&str>) -> DeployInput {
    input("WRITE / 'v2'.", false, tr)
}

fn lock_body(corrnr: &str, local: bool) -> Step {
    Step::Any(Ok(Response::text(
        200,
        format!(
            concat!(
                r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
                r#"<LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR>{}</CORRNR><IS_LOCAL>{}</IS_LOCAL>"#,
                r#"</DATA></asx:values></asx:abap>"#,
            ),
            corrnr,
            if local { "X" } else { "" }
        ),
    )))
}

fn ok() -> Step {
    Step::Any(Ok(Response::empty(200)))
}

/// The recorded deploy of v2 over v1 up to and including the PUT, with the locked re-read.
fn through_put() -> Vec<Step> {
    vec![fx(6), fx(7), fx(6), fx(8), fx(17)]
}

fn then(mut steps: Vec<Step>, rest: Vec<Step>) -> Vec<Step> {
    steps.extend(rest);
    steps
}

struct CancelOnPut {
    inner: Replay,
    cancel: CancelFlag,
}

impl SadtTransport for CancelOnPut {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if request.method == "PUT" {
            self.cancel.set();
        }

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }
}

#[test]
fn recorded_deploy_activates_and_reports_the_new_hash() {
    for name in FIXTURES {
        let steps = then(through_put(), vec![fx(18), fx(19), fx(20)]);
        let mut transport = replay(name, steps);
        let outcome = DeployProg::run(&mut transport, v2(None), None);
        let hash = source_hash(&v2(None).source);

        assert_eq!(outcome.status, Some(Status::Active), "{name}");
        assert_eq!((outcome.exit, outcome.stderr.as_str()), (0, ""), "{name}");
        assert!(
            outcome.stdout.ends_with(&format!("ACTIVE\nhash {hash}")),
            "{name}"
        );
        assert_eq!(transport.calls[4].method, "PUT");
        assert!(transport.calls[5].uri.contains("_action=UNLOCK"));
        assert!(transport.calls[6].uri.contains("activation"));
        assert_eq!(transport.closes, 1);
        transport.assert_finished();
    }
}

#[test]
fn transport_request_rules_apply_right_after_the_lock_in_simulate_and_deploy() {
    let cases = [
        (
            true,
            "",
            Some("DEVK000001"),
            "--tr is not allowed for a local ($TMP) object",
        ),
        (
            false,
            "",
            Some("DEVK000001"),
            "the lock returned no transport request",
        ),
        (false, "", None, "the lock returned no transport request"),
        (
            false,
            "DEVK000001",
            Some("DEVK000002"),
            "--tr DEVK000002 does not match",
        ),
    ];

    for (local, corrnr, tr, stop) in cases {
        for simulate in [true, false] {
            let steps = vec![fx(6), lock_body(corrnr, local), ok()];
            let mut transport = replay(FIXTURE, steps);
            let outcome =
                DeployProg::run(&mut transport, input("WRITE / 'v2'.", simulate, tr), None);

            assert_eq!(outcome.exit, 2, "{stop}");
            assert!(outcome.stderr.starts_with("STOP: "), "{}", outcome.stderr);
            assert!(outcome.stderr.contains(stop), "{}", outcome.stderr);
            assert_eq!(transport.calls.len(), 3);
            assert!(
                transport.calls[2]
                    .uri
                    .ends_with("_action=UNLOCK&lockHandle=h%2B1%2F%3D")
            );
            assert_eq!(transport.closes, 1);
            transport.assert_finished();
        }
    }
}

#[test]
fn the_put_carries_the_encoded_handle_and_the_transport_request_of_the_lock() {
    for tr in [None, Some("devk000001")] {
        let steps = vec![
            fx(6),
            lock_body("DEVK000001", false),
            fx(6),
            fx(8),
            ok(),
            ok(),
            fx(19),
            fx(20),
        ];
        let mut transport = replay(FIXTURE, steps);
        let outcome = DeployProg::run(&mut transport, v2(tr), None);
        let put = &transport.calls[4];

        assert_eq!(outcome.status, Some(Status::Active));
        assert!(outcome.stdout.contains("\ntransport request DEVK000001\n"));
        assert_eq!(
            put.uri,
            format!("{PROGRAM}/source/main?lockHandle=h%2B1%2F%3D&corrNr=DEVK000001")
        );
        assert_eq!(put.headers["Content-Type"], "text/plain; charset=utf-8");
        assert_eq!(put.body, b"REPORT ytest_abapreach01.\n\nWRITE / 'v2'.\n");
    }

    let steps = vec![
        fx(6),
        lock_body("", true),
        fx(6),
        fx(8),
        ok(),
        ok(),
        fx(19),
        fx(20),
    ];
    let mut transport = replay(FIXTURE, steps);
    let outcome = DeployProg::run(&mut transport, v2(None), None);

    assert_eq!(outcome.status, Some(Status::Active));
    assert_eq!(
        transport.calls[4].uri,
        format!("{PROGRAM}/source/main?lockHandle=h%2B1%2F%3D")
    );
    assert!(!outcome.stdout.contains("transport request"));
}

#[test]
fn simulate_prints_the_transport_request_and_never_writes() {
    let steps = vec![fx(6), lock_body("DEVK000001", false), fx(6), fx(8), ok()];
    let mut transport = replay(FIXTURE, steps);
    let outcome = DeployProg::run(&mut transport, input("WRITE / 'v2'.", true, None), None);

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert!(outcome.stdout.contains("transport request DEVK000001"));
    assert!(transport.calls.iter().all(|call| call.method != "PUT"));
}

#[test]
fn recorded_activation_errors_are_inactive_without_a_read_back() {
    let invalid = input("WRITE / no_such_var.", false, None);
    let steps = vec![
        fx(20),
        fx(21),
        fx(20),
        Step::Pair(12, 8),
        fx(22),
        fx(23),
        fx(24),
    ];
    let mut transport = replay(FIXTURE, steps);
    let outcome = DeployProg::run(&mut transport, invalid, None);

    assert_eq!(outcome.status, Some(Status::Inactive));
    assert_eq!(outcome.exit, 1);
    assert!(
        outcome
            .stdout
            .contains("E row 3: Field \"NO_SUCH_VAR\" is unknown.")
    );
    assert!(
        outcome
            .stdout
            .contains("INACTIVE: activation reported errors")
    );
    assert!(outcome.stdout.ends_with(ROLLBACK));
    assert!(transport.calls.last().unwrap().uri.contains("activation"));
    transport.assert_finished();
}

#[test]
fn a_non_2xx_activation_is_reconciled_when_the_active_source_matches() {
    let steps = then(
        through_put(),
        vec![fx(18), Step::ReplyTo(19, Ok(Response::empty(500))), fx(20)],
    );
    let mut transport = replay(FIXTURE, steps);
    let result = DeployProg::run_classified(&mut transport, v2(None), None, &CancelFlag::new());

    assert_eq!(result.outcome.status, Some(Status::Active));
    assert_eq!(result.outcome.exit, 1);
    assert!(result.outcome.stdout.contains("ACTIVE (reconciled)"));
    assert_eq!(result.failure, Some(Failure::Validation));
    assert!(
        transport
            .calls
            .last()
            .unwrap()
            .uri
            .contains("version=active")
    );
    transport.assert_finished();
}

#[test]
fn a_non_2xx_activation_is_inactive_when_the_active_source_differs() {
    let steps = then(
        through_put(),
        vec![
            fx(18),
            Step::ReplyTo(19, Ok(Response::empty(409))),
            Step::Pair(20, 10),
        ],
    );
    let mut transport = replay(FIXTURE, steps);
    let result = DeployProg::run_classified(&mut transport, v2(None), None, &CancelFlag::new());

    assert_eq!(result.outcome.status, Some(Status::Inactive));
    assert_eq!(result.outcome.exit, 1);
    assert!(result.outcome.stdout.ends_with(ROLLBACK));
    assert_eq!(result.failure, Some(Failure::Conflict));
    assert!(
        transport
            .calls
            .last()
            .unwrap()
            .uri
            .contains("version=active")
    );
    transport.assert_finished();
}

#[test]
fn activation_without_errors_is_checked_against_the_active_content() {
    let stale = Step::Pair(20, 10);
    let mut transport = replay(FIXTURE, then(through_put(), vec![fx(18), fx(19), stale]));
    let outcome = DeployProg::run(&mut transport, v2(None), None);

    assert_eq!(outcome.status, Some(Status::Inactive));
    assert_eq!(outcome.exit, 1);
    assert!(
        outcome
            .stdout
            .contains("INACTIVE: activation reported no error but the active source differs")
    );
    assert!(outcome.stdout.ends_with(ROLLBACK));
}

#[test]
fn activation_status_maps_the_answer_alone() {
    assert_eq!(activation_status(200, E_BODY), Some(Status::Inactive));
    assert_eq!(activation_status(409, b""), Some(Status::Rejected));
    assert_eq!(activation_status(199, b""), Some(Status::Rejected));
    assert_eq!(activation_status(300, b""), Some(Status::Rejected));
    assert_eq!(activation_status(200, b""), None);
    assert_eq!(activation_status(204, W_BODY), None);
    assert_eq!(activation_status(200, b"<a><b></a>"), None);
}

#[test]
fn a_rejected_or_failed_put_unlocks_and_never_activates() {
    let rejected = then(
        vec![fx(6), fx(7), fx(6), fx(8)],
        vec![Step::ReplyTo(17, Ok(Response::empty(403))), fx(18)],
    );
    let mut transport = replay(FIXTURE, rejected);
    let outcome = DeployProg::run(&mut transport, v2(None), None);

    assert_eq!(outcome.status, Some(Status::Rejected));
    assert_eq!(outcome.exit, 1);
    assert!(outcome.stdout.ends_with("REJECTED: PUT failed: HTTP 403"));
    assert!(
        transport
            .calls
            .iter()
            .all(|call| !call.uri.contains("activation"))
    );
    transport.assert_finished();

    let unsent = Err(TransportError::PreSendConnectivity("no route".to_owned()));
    let steps = vec![
        fx(6),
        fx(7),
        fx(6),
        fx(8),
        Step::ReplyTo(17, unsent),
        fx(18),
    ];
    let mut transport = replay(FIXTURE, steps);
    let result = DeployProg::run_classified(&mut transport, v2(None), None, &CancelFlag::new());

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.failure, Some(Failure::PreSendConnectivity));
    assert!(!result.fallback_allowed);
    transport.assert_finished();
}

#[test]
fn rfc_invoke_failures_during_put_reconcile_before_retrying() {
    for (code, message) in [
        (3, "SYSTEM_FAILURE"),
        (4, "ABAP message"),
        (5, "NO_SUCH_ENTRY"),
        (29, "not authorized"),
    ] {
        let error = classify_rfc_error(RfcPhase::Invoke, code, message);
        let steps = vec![
            fx(6),
            fx(7),
            fx(6),
            fx(8),
            Step::ReplyTo(17, Err(error)),
            fx(18),
            fx(20),
            fx(26),
        ];
        let mut transport = replay(FIXTURE, steps);
        let outcome = DeployProg::run(&mut transport, v2(None), None);

        assert_eq!((outcome.status, outcome.exit), (Some(Status::Active), 1));
        assert!(outcome.stdout.ends_with("check with read before retrying"));
        assert!(
            transport
                .calls
                .iter()
                .all(|call| !call.uri.contains("activation"))
        );
        transport.assert_finished();
    }
}

#[test]
fn rfc_invoke_failures_during_activation_reconcile_before_retrying() {
    for (code, message) in [
        (3, "SYSTEM_FAILURE"),
        (4, "ABAP message"),
        (5, "NO_SUCH_ENTRY"),
        (29, "not authorized"),
    ] {
        let error = classify_rfc_error(RfcPhase::Invoke, code, message);
        let steps = then(
            through_put(),
            vec![fx(18), Step::ReplyTo(19, Err(error)), fx(20), fx(26)],
        );
        let mut transport = replay(FIXTURE, steps);
        let outcome = DeployProg::run(&mut transport, v2(None), None);

        assert_eq!((outcome.status, outcome.exit), (Some(Status::Active), 1));
        assert!(outcome.stdout.ends_with("check with read before retrying"));
        assert!(
            transport
                .calls
                .iter()
                .any(|call| call.uri.contains("activation"))
        );
        transport.assert_finished();
    }
}

#[test]
fn a_failed_unlock_after_the_put_stops_before_activation() {
    for reply in [
        Ok(Response::empty(500)),
        Err(TransportError::PostSendUncertain("reset".to_owned())),
    ] {
        let steps = then(through_put(), vec![Step::ReplyTo(18, reply)]);
        let mut transport = replay(FIXTURE, steps);
        let outcome = DeployProg::run(&mut transport, v2(None), None);

        assert_eq!(outcome.status, Some(Status::Partial));
        assert_eq!(outcome.exit, 1);
        assert!(outcome.stderr.contains("UNLOCK FAILED"));
        assert!(
            outcome
                .stderr
                .contains("the lock on YTEST_ABAPREACH01 may remain")
        );
        assert!(
            transport
                .calls
                .iter()
                .all(|call| !call.uri.contains("activation"))
        );
        assert_eq!(transport.closes, 1);
        transport.assert_finished();
    }
}

#[test]
fn cancel_after_the_put_unlocks_and_does_not_activate() {
    let steps = then(through_put(), vec![fx(18)]);
    let mut transport = CancelOnPut {
        inner: replay(FIXTURE, steps),
        cancel: CancelFlag::new(),
    };
    let cancel = transport.cancel.clone();
    let outcome = DeployProg::run_with_cancel(&mut transport, v2(None), &cancel);

    assert_eq!(outcome.status, Some(Status::Partial));
    assert_eq!(outcome.exit, 1);
    assert!(outcome.stderr.starts_with("CANCELED"));
    assert!(
        transport
            .inner
            .calls
            .last()
            .unwrap()
            .uri
            .contains("_action=UNLOCK")
    );
    assert_eq!(transport.inner.closes, 1);
    transport.inner.assert_finished();
}

#[test]
fn the_lock_response_is_read_with_entities_and_pretty_printing() {
    let pretty = Step::Any(Ok(Response::text(
        200,
        concat!(
            "<asx:abap xmlns:asx=\"x\">\n <asx:values>\n  <DATA>\n",
            "   <LOCK_HANDLE>a&amp;b</LOCK_HANDLE>\n   <CORRNR>DEVK000001</CORRNR>\n",
            "   <IS_LOCAL></IS_LOCAL>\n  </DATA>\n </asx:values>\n</asx:abap>\n",
        ),
    )));
    let steps = vec![fx(6), pretty, fx(6), fx(8), ok(), ok(), fx(19), fx(20)];
    let mut transport = replay(FIXTURE, steps);
    let outcome = DeployProg::run(&mut transport, v2(None), None);

    assert_eq!(outcome.status, Some(Status::Active));
    assert_eq!(
        transport.calls[4].uri,
        format!("{PROGRAM}/source/main?lockHandle=a%26b&corrNr=DEVK000001")
    );
}
