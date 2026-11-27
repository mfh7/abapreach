//! `Sql::run` on a scripted transport: the dictionary check, the freestyle POST and the replies of
//! the three releases (HD1 750, FED 740, S4D 758, from `tests/fixtures/m9`).

mod common;

use abapreach::backend::Failure;
use abapreach::operation::dictionary::PreviewError;
use abapreach::operation::read_table::CLI_MAX_ROWS;
use abapreach::operation::sql::{self, FREESTYLE_URI, Query};
use abapreach::policy::Policies;
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, fixture_path_in, preview};
use std::fs;

const JOIN: &str = "SELECT TADIR~OBJ_NAME, TDEVC~DLVUNIT FROM TADIR INNER JOIN TDEVC ON TADIR~DEVCLASS = TDEVC~DEVCLASS WHERE TADIR~OBJECT = 'CLAS' AND TADIR~OBJ_NAME LIKE 'CL_ABAP_TYPED%'";

fn ok(xml: String) -> Result<Response, TransportError> {
    Ok(Response::text(200, xml))
}

fn fixture(name: &str) -> Result<Response, TransportError> {
    ok(fs::read_to_string(fixture_path_in("m9", name)).expect("read fixture"))
}

fn class_of(name: &str, class: &str, view_class: Option<&str>) -> Result<Response, TransportError> {
    ok(preview(&[
        ("TABNAME", &[Some(name)]),
        ("TABCLASS", &[Some(class)]),
        ("VIEWCLASS", &[view_class]),
    ]))
}

fn transparent(name: &str) -> Result<Response, TransportError> {
    class_of(name, "TRANSP", None)
}

fn database_view(name: &str) -> Result<Response, TransportError> {
    class_of(name, "VIEW", Some("D"))
}

fn no_cds() -> Result<Response, TransportError> {
    ok(preview(&[
        ("DDLNAME", &[]),
        ("OBJECTNAME", &[]),
        ("OBJECTTYPE", &[]),
    ]))
}

fn bases(view: &str, tables: &[&str]) -> Result<Response, TransportError> {
    let names: Vec<Option<&str>> = tables.iter().map(|_| Some(view)).collect();
    let tabs: Vec<Option<&str>> = tables.iter().map(|table| Some(*table)).collect();
    let pos: Vec<Option<&str>> = tables.iter().map(|_| Some("0001")).collect();

    ok(preview(&[
        ("VIEWNAME", &names),
        ("TABNAME", &tabs),
        ("TABPOS", &pos),
    ]))
}

fn query(text: &str, rows: u32) -> Query {
    Query::new(text, rows, CLI_MAX_ROWS, &Policies::default())
        .unwrap_or_else(|error| panic!("{text:?}: {error}"))
}

fn sent(transport: &ScriptedTransport, index: usize) -> (String, String) {
    let call = &transport.calls[index];

    (
        call.uri.clone(),
        String::from_utf8(call.body.clone()).unwrap(),
    )
}

fn freestyle_calls(transport: &ScriptedTransport) -> usize {
    transport
        .calls
        .iter()
        .filter(|call| call.uri.starts_with(FREESTYLE_URI))
        .count()
}

/// The statement is refused after `lookups` dictionary reads, and the freestyle POST never left.
#[track_caller]
fn assert_refused(
    replies: Vec<Result<Response, TransportError>>,
    text: &str,
    lookups: usize,
    fragments: &[&str],
) {
    let mut transport = ScriptedTransport::replies(replies);
    let error = sql::run(&mut transport, query(text, 5), &Policies::default()).unwrap_err();

    match error {
        PreviewError::Refused(message) => {
            assert!(message.ends_with("; no statement was sent"), "{message}");

            for fragment in fragments {
                assert!(message.contains(fragment), "{message:?} lacks {fragment:?}");
            }
        }
        other => panic!("{text:?}: {other:?}"),
    }

    assert_eq!(transport.calls.len(), lookups, "{text:?}");
    assert_eq!(freestyle_calls(&transport), 0, "{text:?}: freestyle sent");
    transport.assert_finished();
}

#[test]
fn a_join_is_checked_then_sent_to_the_freestyle_preview_on_every_release() {
    for release in ["740", "750", "758"] {
        let mut transport = ScriptedTransport::replies(vec![
            transparent("TADIR"),
            transparent("TDEVC"),
            fixture(&format!("freestyle-join-{release}.xml")),
        ]);
        let data = sql::run(&mut transport, query(JOIN, 3), &Policies::default())
            .unwrap_or_else(|error| panic!("{release}: {error:?}"));

        assert_eq!(data.columns, ["OBJ_NAME", "DLVUNIT"], "{release}");
        assert_eq!(data.rows, [["CL_ABAP_TYPEDESCR", "SAP_BASIS"]], "{release}");
        assert_eq!(
            data.render(1 << 20),
            (
                "OBJ_NAME\tDLVUNIT\nCL_ABAP_TYPEDESCR\tSAP_BASIS".to_owned(),
                0
            )
        );
        assert_eq!(transport.calls.len(), 3, "{release}");

        let (uri, body) = sent(&transport, 2);

        assert_eq!(uri, format!("{FREESTYLE_URI}?rowNumber=3"), "{release}");
        assert_eq!(body, JOIN, "{release}");
        assert!(
            sent(&transport, 0).0.contains("ddicEntityName=DD02L")
                && sent(&transport, 1).0.contains("ddicEntityName=DD02L"),
            "{release}: the dictionary lookups come first"
        );
        transport.assert_finished();
    }
}

#[test]
fn only_the_canonical_rendering_is_sent_never_the_text_the_caller_gave() {
    let mut transport =
        ScriptedTransport::replies(vec![transparent("T001"), fixture("freestyle-join-750.xml")]);

    sql::run(
        &mut transport,
        query(
            "select\n  bukrs ,\tbutxt\nfrom t001\nwhere bukrs = '1000'",
            7,
        ),
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(
        sent(&transport, 1),
        (
            format!("{FREESTYLE_URI}?rowNumber=7"),
            "SELECT BUKRS, BUTXT FROM T001 WHERE BUKRS = '1000'".to_owned()
        )
    );

    let request = &transport.calls[1];

    assert_eq!(request.method, "POST");
    assert_eq!(
        request.headers.get("Accept").map(String::as_str),
        Some("application/*")
    );
    assert_eq!(
        request.headers.get("Content-Type").map(String::as_str),
        Some("text/plain")
    );
}

#[test]
fn a_reply_with_one_row_more_than_asked_is_cut_to_the_requested_rows() {
    let rows: Vec<Option<&str>> = vec![Some("1"), Some("2"), Some("3")];
    let mut transport =
        ScriptedTransport::replies(vec![transparent("T001"), ok(preview(&[("BUKRS", &rows)]))]);
    let data = sql::run(
        &mut transport,
        query("SELECT BUKRS FROM T001", 2),
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(data.rows.len(), 2);
}

#[test]
fn a_denied_table_stops_before_any_connection_is_used() {
    for text in [
        "SELECT * FROM USR02",
        "SELECT * FROM T001 AS A INNER JOIN usr02 AS B ON A~BUKRS = B~BNAME",
        "SELECT * FROM T001 LEFT OUTER JOIN RFCDES ON T001~BUKRS = RFCDES~RFCDEST",
    ] {
        assert!(
            Query::new(text, 5, CLI_MAX_ROWS, &Policies::default()).is_err(),
            "{text}"
        );
    }
}

#[test]
fn a_view_over_a_denied_table_is_refused_and_nothing_is_sent() {
    assert_refused(
        vec![
            database_view("ZV_USERS"),
            no_cds(),
            bases("ZV_USERS", &["T000", "USR02"]),
            transparent("T000"),
        ],
        "SELECT * FROM ZV_USERS",
        4,
        &["USR02", "ZV_USERS"],
    );
}

#[test]
fn a_denied_table_in_a_join_behind_a_view_is_refused_and_nothing_is_sent() {
    assert_refused(
        vec![
            transparent("T001"),
            database_view("ZV_USERS"),
            no_cds(),
            bases("ZV_USERS", &["USR02"]),
        ],
        "SELECT * FROM T001 INNER JOIN ZV_USERS ON T001~BUKRS = ZV_USERS~BNAME",
        4,
        &["USR02"],
    );
}

#[test]
fn a_generated_cds_sql_view_is_refused_and_nothing_is_sent() {
    assert_refused(
        vec![
            fixture("dd02l-cds-sql-view-758.xml"),
            fixture("ddldependency-cds-sql-view-758.xml"),
        ],
        "SELECT * FROM IFICOUNTRY",
        2,
        &["IFICOUNTRY", "generated CDS SQL view"],
    );
}

#[test]
fn a_cds_name_is_refused_as_unknown_and_nothing_is_sent() {
    assert_refused(
        vec![fixture("dd02l-cds-ddl-name-758.xml")],
        "SELECT * FROM I_COUNTRY",
        1,
        &["I_COUNTRY", "not in the ABAP dictionary"],
    );
}

#[test]
fn an_unknown_name_is_refused_and_nothing_is_sent() {
    assert_refused(
        vec![
            transparent("T001"),
            ok(preview(&[
                ("TABNAME", &[]),
                ("TABCLASS", &[]),
                ("VIEWCLASS", &[]),
            ])),
        ],
        "SELECT * FROM T001 INNER JOIN ZNOPE ON T001~BUKRS = ZNOPE~BUKRS",
        2,
        &["ZNOPE", "not in the ABAP dictionary"],
    );
}

#[test]
fn a_view_without_base_tables_is_refused_and_nothing_is_sent() {
    assert_refused(
        vec![database_view("ZV_EMPTY"), no_cds(), bases("ZV_EMPTY", &[])],
        "SELECT * FROM ZV_EMPTY",
        3,
        &["ZV_EMPTY", "base tables"],
    );
}

#[test]
fn a_dictionary_lookup_that_cannot_answer_fails_closed_without_a_freestyle_post() {
    let mut transport = ScriptedTransport::replies(vec![Ok(Response::text(500, "boom"))]);
    let error = sql::run(
        &mut transport,
        query("SELECT * FROM T001", 5),
        &Policies::default(),
    )
    .unwrap_err();

    match error {
        PreviewError::Rejected(message) => {
            assert!(
                message.contains("T001") && message.ends_with("nothing was read"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }

    assert_eq!(freestyle_calls(&transport), 0);
}

#[test]
fn a_refused_logon_and_a_lost_connection_keep_their_routing_class() {
    for (error, expected) in [
        (
            TransportError::PreSendConnectivity("no route".to_owned()),
            Failure::PreSendConnectivity,
        ),
        (
            TransportError::Authorization("logon refused".to_owned()),
            Failure::Authorization,
        ),
    ] {
        let mut transport = ScriptedTransport::replies(vec![Err(error)]);
        let result = sql::run(
            &mut transport,
            query("SELECT * FROM T001", 5),
            &Policies::default(),
        );

        match result {
            Err(PreviewError::Failed { failure, .. }) => assert_eq!(failure, expected),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_failed_freestyle_read_reports_the_sap_message() {
    let mut transport = ScriptedTransport::replies(vec![
        transparent("T001"),
        Ok(Response::text(
            400,
            "<exc:exception><localizedMessage>Unknown column name \"NOPE\"</localizedMessage></exc:exception>",
        )),
    ]);
    let error = sql::run(
        &mut transport,
        query("SELECT NOPE FROM T001", 5),
        &Policies::default(),
    )
    .unwrap_err();

    match error {
        PreviewError::Failed { failure, message } => {
            assert_eq!(failure, Failure::Validation);
            assert!(message.contains("Unknown column name"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}
