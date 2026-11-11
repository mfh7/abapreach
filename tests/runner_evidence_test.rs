//! Evidence of a runner operation (M6 Task 15): fixed-table data previews from a new session, the
//! status rule, and the repository preflight. Fake transports only; no SAP.

mod common;

use abapreach::domain::Status;
use abapreach::operation::payload::Kind;
use abapreach::operation::runner_evidence::{
    Evidence, Expect, FIXED_TABLES, PREFLIGHT_PREVIEWS, PREVIEWS_MAX, Proof, confirm, judge,
    preflight, read_evidence,
};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use common::preview;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

const RUNNER: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const KINDS: [Kind; 5] = [Kind::Dtel, Kind::Doma, Kind::Tabl, Kind::Ttyp, Kind::Tran];

#[derive(Default)]
struct Log {
    requests: Vec<Request>,
    replies: VecDeque<Result<Response, TransportError>>,
    opened: usize,
    closed: usize,
}

type Shared = Rc<RefCell<Log>>;

struct Fake(Shared);

impl SadtTransport for Fake {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        let mut log = self.0.borrow_mut();

        log.requests.push(request);
        log.replies
            .pop_front()
            .expect("a preview without a scripted reply")
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.0.borrow_mut().closed += 1;

        Ok(())
    }
}

fn scripted(replies: Vec<Result<Response, TransportError>>) -> Shared {
    Rc::new(RefCell::new(Log {
        replies: replies.into(),
        ..Log::default()
    }))
}

fn reply(columns: &[(&str, &[Option<&str>])]) -> Result<Response, TransportError> {
    Ok(Response::text(200, preview(columns)))
}

/// An answer with a header and no row.
fn empty(columns: &[&str]) -> Result<Response, TransportError> {
    let none: &[Option<&str>] = &[];
    let columns: Vec<(&str, &[Option<&str>])> = columns.iter().map(|name| (*name, none)).collect();

    reply(&columns)
}

fn definition_columns(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Dtel => ("DD04L", "ROLLNAME"),
        Kind::Doma => ("DD01L", "DOMNAME"),
        Kind::Tabl => ("DD02L", "TABNAME"),
        Kind::Ttyp => ("DD40L", "TYPENAME"),
        Kind::Tran => ("TSTC", "TCODE"),
    }
}

fn definition_row(kind: Kind, name: &str) -> Result<Response, TransportError> {
    let (_, key) = definition_columns(kind);

    if kind == Kind::Tran {
        reply(&[(key, &[Some(name)]), ("PGMNA", &[Some("YREPORT")])])
    } else {
        reply(&[(key, &[Some(name)])])
    }
}

fn definition_absent(kind: Kind) -> Result<Response, TransportError> {
    empty(&[definition_columns(kind).1])
}

fn directory_row(name: &str, package: &str) -> Result<Response, TransportError> {
    reply(&[("OBJ_NAME", &[Some(name)]), ("DEVCLASS", &[Some(package)])])
}

fn directory_absent() -> Result<Response, TransportError> {
    empty(&["OBJ_NAME", "DEVCLASS", "DELFLAG"])
}

/// A TADIR row with its delete flag: `X` is a deletion recorded in a transport request.
fn directory_flagged(name: &str, package: &str, flag: &str) -> Result<Response, TransportError> {
    reply(&[
        ("OBJ_NAME", &[Some(name)]),
        ("DEVCLASS", &[Some(package)]),
        ("DELFLAG", &[Some(flag)]),
    ])
}

fn runner_row() -> Result<Response, TransportError> {
    reply(&[("CLSNAME", &[Some(RUNNER)])])
}

fn runner_absent() -> Result<Response, TransportError> {
    empty(&["CLSNAME"])
}

fn evidence(kind: Kind, name: &str, replies: Vec<Result<Response, TransportError>>) -> Evidence {
    let log = scripted(replies);
    let mut open = || {
        log.borrow_mut().opened += 1;

        Ok(Box::new(Fake(log.clone())) as Box<dyn SadtTransport>)
    };

    read_evidence(&mut open, kind, name, RUNNER)
}

fn present(package: Option<&'static str>, report: Option<&'static str>) -> Expect<'static> {
    Expect::Present { package, report }
}

fn mismatch(proof: &Proof) -> &str {
    match proof {
        Proof::Mismatch(text) => text,
        other => panic!("expected a mismatch, got {other:?}"),
    }
}

// --- the previews ------------------------------------------------------------------------------

#[test]
fn every_kind_reads_its_definition_table_the_directory_and_the_runner_in_one_new_session() {
    let expected = [
        (
            Kind::Dtel,
            "SELECT ROLLNAME FROM DD04L WHERE ROLLNAME = 'YTEST_ABAPREACH_DT' AND AS4LOCAL = 'A'",
            "DD04L",
        ),
        (
            Kind::Doma,
            "SELECT DOMNAME FROM DD01L WHERE DOMNAME = 'YTEST_ABAPREACH_DT' AND AS4LOCAL = 'A'",
            "DD01L",
        ),
        (
            Kind::Tabl,
            "SELECT TABNAME FROM DD02L WHERE TABNAME = 'YTEST_ABAPREACH_DT' AND AS4LOCAL = 'A'",
            "DD02L",
        ),
        (
            Kind::Ttyp,
            "SELECT TYPENAME FROM DD40L WHERE TYPENAME = 'YTEST_ABAPREACH_DT' AND AS4LOCAL = 'A'",
            "DD40L",
        ),
        (
            Kind::Tran,
            "SELECT TCODE, PGMNA FROM TSTC WHERE TCODE = 'YTEST_ABAPREACH_DT'",
            "TSTC",
        ),
    ];

    for (kind, body, table) in expected {
        let log = scripted(vec![
            definition_row(kind, "YTEST_ABAPREACH_DT"),
            directory_row("YTEST_ABAPREACH_DT", "$TMP"),
            runner_absent(),
        ]);
        let mut open = || {
            log.borrow_mut().opened += 1;

            Ok(Box::new(Fake(log.clone())) as Box<dyn SadtTransport>)
        };
        let found = read_evidence(&mut open, kind, "YTEST_ABAPREACH_DT", RUNNER);
        let log = log.borrow();
        let bodies: Vec<String> = log
            .requests
            .iter()
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect();

        assert_eq!(
            bodies,
            [
                body.to_owned(),
                format!(
                    "SELECT OBJ_NAME, DEVCLASS, DELFLAG FROM TADIR WHERE PGMID = 'R3TR' AND OBJECT = '{}' AND OBJ_NAME = 'YTEST_ABAPREACH_DT'",
                    kind.token()
                ),
                format!("SELECT CLSNAME FROM SEOCLASS WHERE CLSNAME = '{RUNNER}'"),
            ],
            "{kind:?}"
        );
        assert!(
            log.requests[0]
                .uri
                .ends_with(&format!("ddicEntityName={table}"))
        );
        assert_eq!(
            (log.opened, log.closed),
            (1, 1),
            "{kind:?}: one new session"
        );
        assert_eq!(
            judge(&present(Some("$TMP"), None), kind, &found),
            Proof::Matches
        );
    }
}

#[test]
fn no_preview_names_a_table_outside_the_fixed_list_and_the_counts_stay_bounded() {
    assert_eq!(FIXED_TABLES.len(), 9);

    for kind in KINDS {
        let log = scripted(vec![
            definition_row(kind, "YTEST_ABAPREACH_DT"),
            directory_row("YTEST_ABAPREACH_DT", "$TMP"),
            runner_absent(),
        ]);
        let mut open = || Ok(Box::new(Fake(log.clone())) as Box<dyn SadtTransport>);

        read_evidence(&mut open, kind, "YTEST_ABAPREACH_DT", RUNNER);

        let log = log.borrow();

        assert!(
            log.requests.len() + PREFLIGHT_PREVIEWS <= PREVIEWS_MAX,
            "{kind:?}"
        );

        for request in &log.requests {
            let entity = request
                .uri
                .split("ddicEntityName=")
                .nth(1)
                .expect("an entity");

            assert!(FIXED_TABLES.contains(&entity), "{entity}");
        }
    }

    assert_eq!(PREVIEWS_MAX, 5);
    assert_eq!(PREFLIGHT_PREVIEWS, 2);
}

#[test]
fn a_name_that_could_carry_sql_sends_no_preview() {
    for name in ["A'B", "A B", "a", "A;B", "", &"A".repeat(31), "A\nB"] {
        let log = scripted(Vec::new());
        let mut open = || Ok(Box::new(Fake(log.clone())) as Box<dyn SadtTransport>);
        let found = read_evidence(&mut open, Kind::Dtel, name, RUNNER);

        assert!(log.borrow().requests.is_empty(), "{name:?}");
        assert!(
            found.definition.is_err() && found.directory.is_err(),
            "{name:?}"
        );
        assert!(matches!(
            judge(&Expect::Absent, Kind::Dtel, &found),
            Proof::Unreadable(_)
        ));
    }
}

// --- the verdicts ------------------------------------------------------------------------------

#[test]
fn a_write_is_confirmed_when_the_new_session_agrees() {
    for kind in KINDS {
        let found = evidence(
            kind,
            "YTEST_ABAPREACH_DT",
            vec![
                definition_row(kind, "YTEST_ABAPREACH_DT"),
                directory_row("YTEST_ABAPREACH_DT", "$TMP"),
                runner_absent(),
            ],
        );
        let proof = judge(&present(Some("$TMP"), None), kind, &found);

        assert_eq!(proof, Proof::Matches, "{kind:?}");
        assert_eq!(confirm(true, &proof), Status::Active);
        // The run itself did not report a good proof: the new session cannot make it ACTIVE.
        assert_eq!(confirm(false, &proof), Status::Partial);
    }
}

#[test]
fn a_missing_row_a_wrong_package_or_a_wrong_report_is_a_mismatch() {
    let cases = [
        (
            "definition missing",
            vec![
                definition_absent(Kind::Tabl),
                directory_row("YT", "$TMP"),
                runner_absent(),
            ],
            Kind::Tabl,
            present(Some("$TMP"), None),
            "DD02L",
        ),
        (
            "directory missing",
            vec![
                definition_row(Kind::Doma, "YT"),
                directory_absent(),
                runner_absent(),
            ],
            Kind::Doma,
            present(None, None),
            "TADIR",
        ),
        (
            "package differs",
            vec![
                definition_row(Kind::Dtel, "YT"),
                directory_row("YT", "ZOTHER"),
                runner_absent(),
            ],
            Kind::Dtel,
            present(Some("$TMP"), None),
            "package ZOTHER",
        ),
        (
            "report differs",
            vec![
                definition_row(Kind::Tran, "YT"),
                directory_row("YT", "$TMP"),
                runner_absent(),
            ],
            Kind::Tran,
            present(Some("$TMP"), Some("YOTHER")),
            "PGMNA YREPORT",
        ),
        (
            "runner class stays",
            vec![
                definition_row(Kind::Ttyp, "YT"),
                directory_row("YT", "$TMP"),
                runner_row(),
            ],
            Kind::Ttyp,
            present(None, None),
            RUNNER,
        ),
    ];

    for (label, replies, kind, expect, text) in cases {
        let found = evidence(kind, "YT", replies);
        let proof = judge(&expect, kind, &found);

        assert!(mismatch(&proof).contains(text), "{label}: {proof:?}");
        assert_eq!(confirm(true, &proof), Status::Partial, "{label}");
        assert_eq!(Status::Partial.exit(), 1);
    }
}

#[test]
fn a_report_that_matches_confirms_a_transaction() {
    let found = evidence(
        Kind::Tran,
        "YT",
        vec![
            definition_row(Kind::Tran, "YT"),
            directory_row("YT", "$TMP"),
            runner_absent(),
        ],
    );

    assert_eq!(
        judge(&present(Some("$TMP"), Some("YREPORT")), Kind::Tran, &found),
        Proof::Matches
    );
}

#[test]
fn a_delete_is_confirmed_only_when_every_row_and_the_runner_are_gone() {
    for kind in KINDS {
        let gone = evidence(
            kind,
            "YT",
            vec![definition_absent(kind), directory_absent(), runner_absent()],
        );

        assert_eq!(
            judge(&Expect::Absent, kind, &gone),
            Proof::Matches,
            "{kind:?}"
        );

        let left = [
            (
                vec![
                    definition_row(kind, "YT"),
                    directory_absent(),
                    runner_absent(),
                ],
                "definition",
            ),
            (
                vec![
                    definition_absent(kind),
                    directory_row("YT", "$TMP"),
                    runner_absent(),
                ],
                "TADIR",
            ),
            (
                vec![definition_absent(kind), directory_absent(), runner_row()],
                RUNNER,
            ),
        ];

        for (replies, text) in left {
            let found = evidence(kind, "YT", replies);
            let proof = judge(&Expect::Absent, kind, &found);

            assert!(mismatch(&proof).contains(text), "{kind:?}: {proof:?}");
            assert_eq!(confirm(true, &proof), Status::Partial);
        }
    }
}

#[test]
fn a_tadir_row_flagged_deleted_with_no_definition_counts_as_deleted() {
    for kind in KINDS {
        let flagged = evidence(
            kind,
            "YT",
            vec![
                definition_absent(kind),
                directory_flagged("YT", "ZPKG", "X"),
                runner_absent(),
            ],
        );

        assert_eq!(
            judge(&Expect::Absent, kind, &flagged),
            Proof::Matches,
            "{kind:?}"
        );

        // A blank flag is a live entry; a definition that is still there is never deleted.
        let live = evidence(
            kind,
            "YT",
            vec![
                definition_absent(kind),
                directory_flagged("YT", "ZPKG", ""),
                runner_absent(),
            ],
        );
        let proof = judge(&Expect::Absent, kind, &live);

        assert!(
            mismatch(&proof).contains("TADIR still has a row"),
            "{kind:?}: {proof:?}"
        );

        let kept = evidence(
            kind,
            "YT",
            vec![
                definition_row(kind, "YT"),
                directory_flagged("YT", "ZPKG", "X"),
                runner_absent(),
            ],
        );
        let proof = judge(&Expect::Absent, kind, &kept);

        assert!(
            mismatch(&proof).contains("definition"),
            "{kind:?}: {proof:?}"
        );
    }
}

#[test]
fn a_reply_for_another_object_is_a_mismatch_not_a_match() {
    let found = evidence(
        Kind::Dtel,
        "YT",
        vec![
            definition_row(Kind::Dtel, "YOTHER"),
            directory_row("YT", "$TMP"),
            runner_absent(),
        ],
    );

    assert!(found.definition.is_err());
    assert!(matches!(
        judge(&present(None, None), Kind::Dtel, &found),
        Proof::Unreadable(_)
    ));
}

#[test]
fn an_unreadable_preview_or_a_failed_open_never_confirms() {
    let failed = evidence(
        Kind::Doma,
        "YT",
        vec![
            Ok(Response::text(500, "")),
            directory_row("YT", "$TMP"),
            runner_absent(),
        ],
    );
    let proof = judge(&present(None, None), Kind::Doma, &failed);

    assert!(matches!(proof, Proof::Unreadable(_)), "{proof:?}");
    assert_eq!(confirm(true, &proof), Status::Partial);

    let mut open = || -> Result<Box<dyn SadtTransport>, TransportError> {
        Err(TransportError::PreSendConnectivity("down".to_owned()))
    };
    let found = read_evidence(&mut open, Kind::Doma, "YT", RUNNER);

    assert!(found.definition.is_err() && found.directory.is_err() && found.runner.is_err());
    assert!(matches!(
        judge(&Expect::Absent, Kind::Doma, &found),
        Proof::Unreadable(_)
    ));
}

#[test]
fn a_definite_mismatch_outranks_an_unreadable_probe() {
    let found = evidence(
        Kind::Doma,
        "YT",
        vec![
            Ok(Response::text(500, "")),
            directory_row("YT", "$TMP"),
            runner_row(),
        ],
    );

    assert!(matches!(
        judge(&present(None, None), Kind::Doma, &found),
        Proof::Mismatch(_)
    ));
}

// --- the repository preflight --------------------------------------------------------------------

fn t000(category: &str, activation: &str, no_client: &str) -> Result<Response, TransportError> {
    let none = (!no_client.is_empty()).then_some(no_client);

    reply(&[
        ("CCCATEGORY", &[Some(category)]),
        ("CCCORACTIV", &[Some(activation)]),
        ("CCNOCLIIND", &[none]),
    ])
}

fn namespace(flag: Option<&str>) -> Result<Response, TransportError> {
    reply(&[("EDITFLAG", &[flag])])
}

fn run_preflight(
    replies: Vec<Result<Response, TransportError>>,
) -> (
    Result<abapreach::operation::runner_evidence::Preflight, TransportError>,
    Shared,
) {
    let log = scripted(replies);
    let mut fake = Fake(log.clone());

    (preflight(&mut fake, "100"), log)
}

#[test]
fn the_preflight_reads_exactly_two_fixed_tables_and_prints_what_it_found() {
    let (result, log) = run_preflight(vec![t000("C", "1", ""), namespace(Some("X"))]);
    let found = result.expect("a preflight");
    let log = log.borrow();
    let bodies: Vec<String> = log
        .requests
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect();

    assert_eq!(
        bodies,
        [
            "SELECT CCCATEGORY, CCCORACTIV, CCNOCLIIND FROM T000 WHERE MANDT = '100'",
            "SELECT EDITFLAG FROM TRNSPACE WHERE NAMESPACE = '/0CUST/'",
        ]
    );
    assert_eq!(log.requests.len(), PREFLIGHT_PREVIEWS);
    assert!(found.refusal.is_none() && found.notes.is_empty());
    assert_eq!(
        found.line,
        "RUNNER: preflight T000 CCCATEGORY=C CCCORACTIV=1 CCNOCLIIND=- TRNSPACE /0CUST/ EDITFLAG=X"
    );
}

#[test]
fn only_a_customer_namespace_that_is_not_modifiable_refuses() {
    for flag in [None, Some(" "), Some("N")] {
        let (result, _) = run_preflight(vec![t000("C", "1", ""), namespace(flag)]);
        let found = result.expect("a preflight");
        let reason = found.refusal.expect("a refusal");

        assert!(
            reason.contains("/0CUST/") && reason.contains("EDITFLAG"),
            "{reason}"
        );
    }

    // The client settings are printed and never decide: every value passes the namespace rule.
    for (category, activation, no_client) in [("P", "0", "3"), ("T", "2", "2"), ("C", "1", "1")] {
        let (result, _) = run_preflight(vec![
            t000(category, activation, no_client),
            namespace(Some("X")),
        ]);

        assert!(result.expect("a preflight").refusal.is_none());
    }
}

#[test]
fn a_missing_namespace_row_or_an_unreadable_source_does_not_block() {
    // T000 only: the namespace table has no such row.
    let (result, _) = run_preflight(vec![t000("C", "1", ""), empty(&["EDITFLAG"])]);
    let found = result.expect("a preflight");

    assert!(found.refusal.is_none());
    assert!(found.line.contains("EDITFLAG=?"), "{}", found.line);

    // Both sources refused: no block, a W: line per source, and the line shows unknowns.
    let (result, log) = run_preflight(vec![
        Ok(Response::text(403, "")),
        Ok(Response::text(500, "")),
    ]);
    let found = result.expect("a preflight");

    assert!(found.refusal.is_none());
    assert_eq!(found.notes.len(), 2);
    assert!(found.notes.iter().all(|note| note.starts_with("W: ")));
    assert!(found.line.contains("CCCATEGORY=?"), "{}", found.line);
    assert_eq!(log.borrow().requests.len(), 2);
}

#[test]
fn a_lost_connection_or_a_refused_logon_stops_the_preflight() {
    let (result, _) = run_preflight(vec![Err(TransportError::PreSendConnectivity(
        "down".to_owned(),
    ))]);

    assert!(matches!(
        result,
        Err(TransportError::PreSendConnectivity(_))
    ));

    let (result, _) = run_preflight(vec![Ok(Response::text(401, ""))]);

    assert!(matches!(result, Err(TransportError::Authorization(_))));
}

#[test]
fn a_client_that_is_not_three_digits_sends_no_preview() {
    for client in ["", "1", "1000", "10O", "100'"] {
        let log = scripted(Vec::new());
        let mut fake = Fake(log.clone());

        assert!(preflight(&mut fake, client).is_err(), "{client:?}");
        assert!(log.borrow().requests.is_empty());
    }
}
