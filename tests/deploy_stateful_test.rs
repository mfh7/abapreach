mod common;

use abapreach::backend::{BackendKind, Failure, Selection, SelectionReason};
use abapreach::domain::Status;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{CancelFlag, DeployInput, DeployProg};
use abapreach::transport::http::HttpTransport;
use abapreach::transport::{Response, SadtTransport};

const LIVE: &str = "REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v1'.";
const NO_STATEFUL_SESSION: &str = "ADT HTTP stateful session unavailable (no sap-contextid)";
const STOP: &str = concat!(
    "STOP: ADT HTTP on this system keeps no stateful session (no sap-contextid); ",
    "use --backend rfc",
);
const LOCK_BODY: &str = concat!(
    r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
    r#"<LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>"#,
    r#"</DATA></asx:values></asx:abap>"#,
);

fn input(base: Option<String>) -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            "WRITE / 'v2'.".to_owned(),
        ],
        base,
        simulate: true,
        tr: None,
    }
}

fn live() -> Response {
    Response::text(200, LIVE)
}

/// The same live source, with the cookie that keeps a stateful ADT session alive.
fn live_with_context() -> Response {
    let mut response = live();
    response.headers.insert(
        "Set-Cookie".to_owned(),
        "SAP_SESSIONID_DEV_100=s1; path=/; sap-contextid=ctx".to_owned(),
    );

    response
}

fn live_hash() -> String {
    source_hash(&LIVE.split('\n').map(str::to_owned).collect::<Vec<_>>())
}

fn selection(reason: SelectionReason) -> Selection {
    Selection::new(BackendKind::AdtHttp, reason)
}

fn deployment(replies: Vec<Response>) -> HttpTransport {
    HttpTransport::for_test(replies.into_iter().map(Ok).collect())
}

fn locks(transport: &HttpTransport) -> bool {
    transport
        .recorded()
        .iter()
        .any(|call| call.uri.contains("_action=LOCK"))
}

#[test]
fn a_session_with_the_context_cookie_locks_and_simulates() {
    let mut transport = deployment(vec![
        live_with_context(),
        Response::text(200, LOCK_BODY),
        live_with_context(),
        Response::empty(200),
        Response::empty(200),
        Response::empty(200),
    ]);
    let outcome = DeployProg::run(&mut transport, input(None), None);

    assert_eq!(transport.stateful_supported(), Some(true));
    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(outcome.exit, 0);
    assert!(locks(&transport));
    assert_eq!(transport.recorded().len(), 6);
}

#[test]
fn a_session_without_the_context_cookie_reports_a_missing_capability_before_the_lock() {
    let mut transport = deployment(vec![live(), Response::empty(200)]);
    let result = DeployProg::run_selected(
        &mut transport,
        input(None),
        &selection(SelectionReason::DefaultAdt),
        None,
        &CancelFlag::new(),
    );

    assert_eq!(transport.stateful_supported(), Some(false));
    assert_eq!(result.failure, Some(Failure::CapabilityUnavailable));
    assert!(result.fallback_allowed);
    assert_eq!(result.detail.as_deref(), Some(NO_STATEFUL_SESSION));
    assert_eq!(result.outcome.status, None);
    assert_eq!(result.outcome.exit, 1);
    assert_eq!(
        result.outcome.stderr,
        format!("ERROR: {NO_STATEFUL_SESSION}")
    );
    assert!(result.outcome.stdout.contains("live hash"));
    assert!(!locks(&transport), "the LOCK must not be sent");
}

#[test]
fn base_or_an_explicit_adt_backend_stops_instead_of_falling_back() {
    let cases = [
        (Some(live_hash()), SelectionReason::DefaultAdt, "--base"),
        (None, SelectionReason::ExplicitBackend, "--backend"),
    ];

    for (base, reason, pin) in cases {
        let mut transport = deployment(vec![live(), Response::empty(200)]);
        let outcome = DeployProg::run_selected(
            &mut transport,
            input(base),
            &selection(reason),
            None,
            &CancelFlag::new(),
        )
        .outcome;

        assert_eq!(outcome.exit, 2, "{pin}");
        assert_eq!(outcome.stderr, STOP, "{pin}");
        assert!(!locks(&transport), "{pin}");
    }
}
