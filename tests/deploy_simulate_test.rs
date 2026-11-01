mod common;
mod replay;

use abapreach::backend::Failure;
use abapreach::domain::Status;
use abapreach::operation::deploy::{
    CancelFlag, DeployInput, DeployProg, checkrun_xml, format_messages,
};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use common::Fixture;
use replay::{Replay, Step, fx, replay};
use std::fs;

const FIXTURES: [&str; 3] = ["dev-750-rfc.json", "dev-750-http.json", "dv2-740-rfc.json"];
const FIXTURE: &str = "dev-750-rfc.json";
const VALID_CHECK_BODY: &str = concat!(
    r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun">"#,
    r#"<chkrun:checkReport chkrun:status="processed"><chkrun:checkMessageList>"#,
    r#"<chkrun:checkMessage chkrun:uri="x#start=5,1" chkrun:type="W" "#,
    r#"chkrun:shortText="careful"/></chkrun:checkMessageList></chkrun:checkReport>"#,
    r#"</chkrun:checkRunReports>"#,
);

fn input(last_line: &str) -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            last_line.to_owned(),
        ],
        base: None,
        simulate: true,
        tr: None,
    }
}

fn valid() -> DeployInput {
    input("WRITE / 'v2'.")
}

fn invalid() -> DeployInput {
    input("WRITE / no_such_var.")
}

struct CancelOnCheck {
    inner: Replay,
    cancel: CancelFlag,
}

impl SadtTransport for CancelOnCheck {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if request.uri.contains("checkruns") {
            self.cancel.set();
        }

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }
}

/// A transport whose session end always fails, with the failure the caller must report.
struct FailingClose {
    inner: Replay,
    failure: TransportError,
}

impl SadtTransport for FailingClose {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.inner.close();

        Err(self.failure.clone())
    }
}

#[test]
fn a_failed_session_end_after_the_unlock_is_only_a_warning() {
    let mut transport = FailingClose {
        inner: replay(FIXTURE, vec![fx(6), fx(7), fx(6), fx(8), fx(9)]),
        failure: TransportError::PostSendUncertain("gone".to_owned()),
    };
    let outcome = DeployProg::run(&mut transport, valid(), None);

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(outcome.exit, 0);
    assert!(outcome.stdout.ends_with("SIMULATED_OK"));
    assert_eq!(
        outcome.stderr,
        "W: session close failed: post-send outcome uncertain: gone (lock was released)"
    );
    assert_eq!(transport.inner.closes, 1);
    transport.inner.assert_finished();
}

#[test]
fn a_failed_unlock_keeps_its_stop_without_a_session_warning() {
    let mut transport = FailingClose {
        inner: replay(
            FIXTURE,
            vec![
                fx(6),
                fx(7),
                fx(6),
                fx(8),
                Step::ReplyTo(9, Ok(Response::empty(500))),
            ],
        ),
        failure: TransportError::PostSendUncertain("gone".to_owned()),
    };
    let outcome = DeployProg::run(&mut transport, valid(), None);

    assert_eq!(outcome.status, Some(Status::Partial));
    assert_eq!(outcome.exit, 1);
    assert!(
        outcome
            .stdout
            .ends_with("PARTIAL: the check passed but the lock was not released")
    );
    assert_eq!(
        outcome.stderr,
        "UNLOCK FAILED (HTTP 500); the lock on YTEST_ABAPREACH01 may remain"
    );
}

#[test]
fn recorded_valid_check_leaves_the_object_alone_on_every_release() {
    for name in FIXTURES {
        let mut transport = replay(name, vec![fx(6), fx(7), fx(6), fx(8), fx(9)]);
        let outcome = DeployProg::run(&mut transport, valid(), None);

        assert_eq!(outcome.status, Some(Status::SimulatedOk), "{name}");
        assert_eq!((outcome.exit, outcome.stderr.as_str()), (0, ""), "{name}");
        assert!(outcome.stdout.ends_with("SIMULATED_OK"), "{name}");
        assert!(transport.calls.iter().all(|call| call.method != "PUT"));
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
}

#[test]
fn recorded_invalid_check_is_rejected_with_the_message_and_unlocks() {
    for name in FIXTURES {
        let mut transport = replay(name, vec![fx(10), fx(11), fx(10), fx(12), fx(13)]);
        let outcome = DeployProg::run(&mut transport, invalid(), None);

        assert_eq!(outcome.status, Some(Status::Rejected), "{name}");
        assert_eq!(outcome.exit, 1, "{name}");
        assert!(
            outcome
                .stdout
                .contains("E row 3: Field \"NO_SUCH_VAR\" is unknown."),
            "{name}: {}",
            outcome.stdout
        );
        assert!(outcome.stdout.ends_with("REJECTED: the check found errors"));
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
}

#[test]
fn warnings_are_printed_and_do_not_reject() {
    let steps = vec![
        fx(6),
        fx(7),
        fx(6),
        Step::ReplyTo(8, Ok(Response::text(200, VALID_CHECK_BODY))),
        fx(9),
    ];
    let mut transport = replay(FIXTURE, steps);
    let outcome = DeployProg::run(&mut transport, valid(), None);

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert!(outcome.stdout.contains("W row 5: careful"));
    assert!(outcome.stdout.ends_with("SIMULATED_OK"));
}

#[test]
fn an_unavailable_or_unreadable_check_stops_and_never_saves() {
    let cases = [
        (
            Ok(Response::empty(404)),
            "STOP: the check without save is unavailable: HTTP 404",
        ),
        (
            Ok(Response::text(200, "<a><b></a>")),
            "STOP: cannot read the check result",
        ),
    ];

    for (reply, stop) in cases {
        let mut transport = replay(
            FIXTURE,
            vec![fx(6), fx(7), fx(6), Step::ReplyTo(8, reply), fx(9)],
        );
        let outcome = DeployProg::run(&mut transport, valid(), None);

        assert_eq!(outcome.exit, 2);
        assert!(outcome.stderr.starts_with(stop), "{}", outcome.stderr);
        assert!(transport.calls.iter().all(|call| call.method != "PUT"));
        assert!(
            transport
                .calls
                .last()
                .unwrap()
                .uri
                .contains("_action=UNLOCK")
        );
        assert_eq!(transport.closes, 1);
    }
}

#[test]
fn a_lost_check_response_is_classified_and_still_unlocks() {
    let lost = Err(TransportError::PostSendUncertain("reset".to_owned()));
    let steps = vec![fx(6), fx(7), fx(6), Step::ReplyTo(8, lost), fx(9)];
    let mut transport = replay(FIXTURE, steps);
    let result = DeployProg::run_classified(&mut transport, valid(), None, &CancelFlag::new());

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.failure, Some(Failure::PostSendUncertain));
    assert!(!result.fallback_allowed);
    assert!(
        transport
            .calls
            .last()
            .unwrap()
            .uri
            .contains("_action=UNLOCK")
    );
}

#[test]
fn cancel_during_the_check_unlocks_without_a_write() {
    let mut transport = CancelOnCheck {
        inner: replay(FIXTURE, vec![fx(6), fx(7), fx(6), fx(8), fx(9)]),
        cancel: CancelFlag::new(),
    };
    let cancel = transport.cancel.clone();
    let outcome = DeployProg::run_with_cancel(&mut transport, valid(), &cancel);

    assert_eq!(outcome.exit, 1);
    assert!(outcome.stderr.starts_with("CANCELED"));
    assert!(
        transport
            .inner
            .calls
            .iter()
            .all(|call| call.method != "PUT")
    );
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
fn simulate_with_a_failed_unlock_is_partial() {
    let steps = vec![
        fx(6),
        fx(7),
        fx(6),
        fx(8),
        Step::ReplyTo(9, Ok(Response::empty(500))),
    ];
    let mut transport = replay(FIXTURE, steps);
    let outcome = DeployProg::run(&mut transport, valid(), None);

    assert_eq!(outcome.status, Some(Status::Partial));
    assert_eq!(outcome.exit, 1);
    assert!(outcome.stderr.contains("UNLOCK FAILED (HTTP 500)"));
}

#[test]
fn checkrun_request_matches_the_recording_and_pads_base64() {
    let text = fs::read_to_string(common::fixture_path(FIXTURE)).unwrap();
    let fixture: Fixture = serde_json::from_str(&text).unwrap();
    let lines = valid().source;

    assert_eq!(
        checkrun_xml("YTEST_ABAPREACH01", &lines),
        fixture.exchanges[8].request.body
    );

    let content = |source: &str| {
        let xml = checkrun_xml("Z", &[source.to_owned()]);
        let start = xml.find("<chkrun:content>").unwrap() + "<chkrun:content>".len();

        xml[start..xml.find("</chkrun:content>").unwrap()].to_owned()
    };

    assert_eq!(content("a"), "YQo=");
    assert_eq!(content("ab"), "YWIK");
    assert_eq!(content("abc"), "YWJjCg==");
}

#[test]
fn format_messages_is_reused_from_the_adt_module() {
    let xml =
        r#"<chkrun:checkMessage chkrun:type="E" chkrun:uri="x#start=3,8" chkrun:shortText="bad"/>"#;

    assert_eq!(format_messages(xml), vec!["E row 3: bad"]);
}
