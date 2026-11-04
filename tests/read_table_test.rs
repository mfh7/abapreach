mod common;

use abapreach::backend::Failure;
use abapreach::operation::read_table::{
    CLI_MAX_ROWS, MCP_DEFAULT_ROWS, MCP_MAX_ROWS, Query, ReadTable, ReadTableError,
    ReadTableRequest, TableData, mcp_rows, preview_request, split_where,
};
use abapreach::policy::Policies;
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, preview};

fn ok(xml: String) -> Result<Response, TransportError> {
    Ok(Response::text(200, xml))
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

fn request(
    table: &str,
    fields: &[&str],
    where_clause: Option<&str>,
    rows: u32,
) -> ReadTableRequest {
    ReadTableRequest {
        table: table.to_owned(),
        fields: fields.iter().map(|field| (*field).to_owned()).collect(),
        where_clause: where_clause.map(str::to_owned),
        rows,
    }
}

fn query(table: &str, fields: &[&str], where_clause: Option<&str>) -> Result<Query, String> {
    Query::new(
        &request(table, fields, where_clause, 10),
        CLI_MAX_ROWS,
        &Policies::default(),
    )
}

fn body_of(table: &str, fields: &[&str], where_clause: Option<&str>) -> String {
    query(table, fields, where_clause)
        .unwrap_or_else(|error| panic!("{where_clause:?}: {error}"))
        .body()
        .to_owned()
}

fn policies_denying(tables: &[&str]) -> Policies {
    Policies {
        read_table_denied: tables.iter().map(|table| (*table).to_owned()).collect(),
        ..Policies::default()
    }
}

fn sent(transport: &ScriptedTransport, index: usize) -> (String, String) {
    let call = &transport.calls[index];

    (
        call.uri.clone(),
        String::from_utf8(call.body.clone()).unwrap(),
    )
}

fn run(
    transport: &mut ScriptedTransport,
    request: &ReadTableRequest,
    policies: &Policies,
) -> Result<TableData, ReadTableError> {
    let query = Query::new(request, CLI_MAX_ROWS, policies).expect("valid query");

    ReadTable::run(transport, &query, policies)
}

#[test]
fn the_default_deny_list_stops_a_direct_table_in_any_case() {
    for table in [
        "USR02",
        "usr02",
        "Ush02",
        "USH02_ARC_TMP",
        "USRPWDHISTORY",
        "RFCDES",
        "RSECTAB",
        "RSECACTB",
        "SSF_PSE_D",
    ] {
        let error = query(table, &[], None).unwrap_err();

        assert!(error.contains("read_table_denied"), "{table}: {error}");
        assert!(error.contains(&table.to_ascii_uppercase()), "{error}");
    }

    assert!(query("T001", &[], None).is_ok());
}

#[test]
fn an_added_table_is_denied_and_a_removed_one_is_allowed() {
    let added = Policies {
        read_table_denied: vec!["USR02".to_owned(), "BSEG".to_owned()],
        ..Policies::default()
    };
    let removed = policies_denying(&["BSEG"]);
    let empty = policies_denying(&[]);
    let make = |policies: &Policies, table: &str| {
        Query::new(&request(table, &[], None, 10), CLI_MAX_ROWS, policies)
    };

    assert!(make(&added, "bseg").is_err() && make(&added, "USR02").is_err());
    assert!(make(&removed, "USR02").is_ok() && make(&removed, "BSEG").is_err());
    assert!(make(&empty, "USR02").is_ok());
}

#[test]
fn namespace_identifiers_are_allowed() {
    assert_eq!(
        body_of("/bic/afoo", &["/BIC/Field", "plain_1"], None),
        "SELECT /BIC/FIELD, PLAIN_1 FROM /BIC/AFOO"
    );
}

#[test]
fn bad_table_and_field_identifiers_are_rejected() {
    for table in [
        "",
        "T001 T002",
        "T001;",
        "T~001",
        "T-001",
        "T.001",
        "'T001'",
        "T001\n",
        "/X",
        "/X/",
        "//Y",
        "/X/Y/Z",
        "X/Y",
        "T001,T002",
        "T001 JOIN T002",
        "T@1",
        "A2345678901234567890123456789012",
    ] {
        assert!(query(table, &[], None).is_err(), "table {table:?}");
    }

    for field in [
        "",
        "*",
        "COUNT(*)",
        "BUKRS AS B",
        "T001~BUKRS",
        "BUKRS+1",
        "'X'",
        "A.B",
        "BUKRS BUTXT",
        "SELECT",
        "from",
        "(BUKRS)",
        "BUKRS\n",
        "@BUKRS",
    ] {
        assert!(query("T001", &[field], None).is_err(), "field {field:?}");
    }

    assert!(query("T001", &["BUKRS", ""], None).is_err());
}

#[test]
fn the_admitted_where_grammar_builds_a_canonical_body() {
    let cases = [
        ("bukrs = '1000'", "BUKRS = '1000'"),
        ("BUKRS<>'1000'", "BUKRS <> '1000'"),
        (
            "BUKRS >= '1000' and BUKRS <= '2000'",
            "BUKRS >= '1000' AND BUKRS <= '2000'",
        ),
        (
            "WAERS < 'EUR' OR WAERS > 'GBP'",
            "WAERS < 'EUR' OR WAERS > 'GBP'",
        ),
        ("MANDT = 0", "MANDT = 0"),
        ("AMOUNT = -12.50", "AMOUNT = -12.50"),
        ("BUTXT LIKE '%a%'", "BUTXT LIKE '%a%'"),
        ("NOT BUTXT LIKE '%a%'", "NOT BUTXT LIKE '%a%'"),
        ("BUKRS IN ('1000','2000')", "BUKRS IN ( '1000', '2000' )"),
        ("BUKRS in ( '1000' )", "BUKRS IN ( '1000' )"),
        ("AMOUNT BETWEEN 1 AND 5", "AMOUNT BETWEEN 1 AND 5"),
        ("ORT01 IS NULL", "ORT01 IS NULL"),
        ("ort01 is not null", "ORT01 IS NOT NULL"),
        (
            "NOT ( BUKRS = '1000' OR BUKRS = '2000' )",
            "NOT ( BUKRS = '1000' OR BUKRS = '2000' )",
        ),
        (
            "(BUKRS = '1000' AND (WAERS = 'EUR' OR WAERS = 'USD'))",
            "( BUKRS = '1000' AND ( WAERS = 'EUR' OR WAERS = 'USD' ) )",
        ),
        ("BUTXT = 'it''s'", "BUTXT = 'it''s'"),
        (
            "BUTXT = 'a \" b * c ~ d @ e'",
            "BUTXT = 'a \" b * c ~ d @ e'",
        ),
        ("BUTXT = ''", "BUTXT = ''"),
        ("/BIC/FLD = 'x'", "/BIC/FLD = 'x'"),
    ];

    for (given, canonical) in cases {
        assert_eq!(
            body_of("T001", &[], Some(given)),
            format!("SELECT * FROM T001 WHERE {canonical}"),
            "{given}"
        );
    }

    assert_eq!(
        body_of("T001", &["bukrs", "butxt"], None),
        "SELECT BUKRS, BUTXT FROM T001"
    );
}

#[test]
fn every_where_outside_the_grammar_stops_locally() {
    let long_literal = format!("BUTXT = '{}'", "x".repeat(256));
    let deep = format!("{}BUKRS = '1'{}", "(".repeat(40), ")".repeat(40));
    let rejected = [
        "",
        "   ",
        "SELECT * FROM T001",
        "BUKRS = '1000' UNION SELECT BNAME FROM USR02",
        "BUKRS = '1' AND EXISTS ( SELECT 1 FROM USR02 )",
        "EXISTS ( SELECT 1 FROM USR02 )",
        "BUKRS IN ( SELECT BNAME FROM USR02 )",
        "BUKRS = ( SELECT MAX( BNAME ) FROM USR02 )",
        "BUKRS = '1' ) OR ( 1 = 1",
        "BUKRS = '1' FROM USR02",
        "BUKRS = '1' INNER JOIN USR02 ON T001~BUKRS = USR02~BNAME",
        "BUKRS = '1' JOIN USR02",
        "T001~BUKRS = '1'",
        "USR02~BNAME = 'X'",
        "T002.LAND1 = 'DE'",
        "USR02-BNAME = 'X'",
        "@BUKRS = '1'",
        "BUKRS = @var",
        "BUKRS = '1' \" comment",
        "BUKRS = '1' * comment",
        "* BUKRS = '1'",
        "BUKRS = '1' -- c",
        "BUKRS = 'open",
        "BUKRS = 'a''",
        "BUKRS = 'a'b'",
        "BUKRS = \"1000\"",
        "BUKRS = `1000`",
        "BUKRS = BUTXT",
        "BUKRS = T002",
        "BUKRS + 1 = 5",
        "UPPER( BUKRS ) = 'X'",
        "BUKRS = '1'; DELETE FROM T001",
        "BUKRS = '1'\nAND WAERS = 'EUR'",
        "BUKRS = '1'\tAND WAERS = 'EUR'\r",
        "BUKRS = '1' AND",
        "AND BUKRS = '1'",
        "BUKRS = '1' AND AND WAERS = 'EUR'",
        "BUKRS = '1' OR",
        "BUKRS == '1'",
        "BUKRS != '1'",
        "BUKRS =< '1'",
        "BUKRS IN ()",
        "BUKRS IN ( '1' '2' )",
        "BUKRS IN ( '1', )",
        "BUKRS IN '1'",
        "BUKRS IN ( BUTXT )",
        "BUKRS BETWEEN 1",
        "BUKRS BETWEEN 1 OR 5",
        "BUKRS BETWEEN 1 AND",
        "BUKRS LIKE BUTXT",
        "BUKRS LIKE 5",
        "BUKRS IS",
        "BUKRS IS NOT",
        "BUKRS IS 1",
        "BUKRS IS NULL NULL",
        "BUKRS",
        "( )",
        "()",
        "( BUKRS = '1'",
        "SELECT = '1'",
        "FROM = '1'",
        "JOIN = '1'",
        "EXISTS = '1'",
        "NOT = '1'",
        "BUKRS = 1e5",
        "BUKRS = 0x10",
        "BUKRS = 1.",
        "BUKRS = .5",
        "BUKRS = --5",
        "BUKRS = '1' OR 1 = 1",
        "BUKRS = '1' ORDER BY BUKRS",
        "BUKRS = '1' UP TO 1 ROWS",
        "BUKRS = '1' INTO TABLE x",
        "BUKRS = 'a\u{7}b'",
        "BUKRS = 'a\nb'",
        "BUKRS = 'a\u{2028}b'",
        long_literal.as_str(),
        deep.as_str(),
    ];

    for given in rejected {
        assert!(
            query("T001", &[], Some(given)).is_err(),
            "must be rejected: {given:?}"
        );
    }
}

#[test]
fn a_rejected_input_makes_no_transport_call() {
    let transport = ScriptedTransport::replies(Vec::new());

    for given in ["BUKRS IN ( SELECT 1 FROM USR02 )", "BUKRS = 'open"] {
        assert!(query("T001", &[], Some(given)).is_err());
    }

    assert!(query("USR02", &[], None).is_err());
    assert!(transport.calls.is_empty());
    transport.assert_finished();
}

#[test]
fn rows_follow_the_cli_and_mcp_limits() {
    let make = |rows: u32, max: u32| {
        Query::new(&request("T001", &[], None, rows), max, &Policies::default())
    };

    assert_eq!(
        (CLI_MAX_ROWS, MCP_DEFAULT_ROWS, MCP_MAX_ROWS),
        (5000, 100, 1000)
    );
    assert!(make(1, CLI_MAX_ROWS).is_ok() && make(5000, CLI_MAX_ROWS).is_ok());
    assert!(make(0, CLI_MAX_ROWS).is_err() && make(5001, CLI_MAX_ROWS).is_err());
    assert!(make(1000, MCP_MAX_ROWS).is_ok() && make(1001, MCP_MAX_ROWS).is_err());
    assert_eq!(mcp_rows(None), Ok(100));
    assert_eq!(mcp_rows(Some(1)), Ok(1));
    assert_eq!(mcp_rows(Some(1000)), Ok(1000));
    assert!(mcp_rows(Some(0)).is_err() && mcp_rows(Some(1001)).is_err());
}

#[test]
fn the_request_body_must_read_the_entity_it_names() {
    let request = preview_request("T001", "SELECT * FROM T001 WHERE BUKRS = '1'", 7).unwrap();

    assert_eq!(request.method, "POST");
    assert_eq!(
        request.uri,
        "/sap/bc/adt/datapreview/ddic?rowNumber=7&ddicEntityName=T001"
    );
    assert_eq!(request.headers.get("Accept").unwrap(), "application/*");
    assert_eq!(request.headers.get("Content-Type").unwrap(), "text/plain");
    assert_eq!(request.body, b"SELECT * FROM T001 WHERE BUKRS = '1'");
    assert_eq!(
        preview_request("/BIC/AFOO", "SELECT * FROM /BIC/AFOO", 1)
            .unwrap()
            .uri,
        "/sap/bc/adt/datapreview/ddic?rowNumber=1&ddicEntityName=%2FBIC%2FAFOO"
    );
    assert!(preview_request("T001", "SELECT * FROM T001 WHERE X = ' FROM USR02'", 1).is_ok());
    assert!(preview_request("T001", "SELECT * FROM T001", 0).is_err());

    for (entity, body) in [
        ("T001", "SELECT * FROM USR02"),
        ("T001", "SELECT * FROM T001X"),
        ("T001", "SELECT * FROM T001 JOIN USR02"),
        (
            "T001",
            "SELECT * FROM T001 WHERE A IN ( SELECT B FROM USR02 )",
        ),
        ("T001", "SELECT FROM FROM T001"),
        ("T001", "WHERE X = 1"),
        ("T001", ""),
        ("T001", "SELECT * FROM T001\nWHERE X = 1"),
    ] {
        assert!(
            preview_request(entity, body, 1).is_err(),
            "{entity}: {body:?}"
        );
    }
}

#[test]
fn split_where_cuts_at_72_characters_and_never_inside_a_literal() {
    let where_clause = format!(
        "MANDT = '110' AND ZZNAMA = 'A B  C' AND FELD_PANJANG_SEKALI = {}ABC = 'Z'",
        "'X' AND ".repeat(14)
    );

    assert!(where_clause.len() >= 150);

    let lines = split_where(&where_clause).unwrap();

    assert!(lines.iter().all(|line| line.len() <= 72), "{lines:?}");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("'A B  C'"))
            .count(),
        1
    );
    assert_eq!(
        lines.join(" ").split_whitespace().collect::<Vec<_>>(),
        where_clause.split_whitespace().collect::<Vec<_>>()
    );
    assert!(split_where(&format!("A = '{}'", "x".repeat(78))).is_err());
    assert!(split_where("A = 'not closed").is_err());
    assert_eq!(
        split_where("A = 'it''s' AND B = 1").unwrap().join(" "),
        "A = 'it''s' AND B = 1"
    );
    assert!(split_where("A = 'x'").unwrap().len() == 1);
    assert!(split_where("").unwrap().is_empty());
}

#[test]
fn split_where_keeps_a_literal_with_spaces_whole_at_the_width_edge() {
    let literal = "'a  b  c'";
    let filler = "X".repeat(72 - 4 - 1);
    let text = format!("A = {filler} {literal}");
    let lines = split_where(&text).unwrap();

    assert!(lines.iter().all(|line| line.len() <= 72), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.ends_with(literal) || line == literal)
    );
}

#[test]
fn a_transparent_table_is_resolved_then_read() {
    let mut transport = ScriptedTransport::replies(vec![
        transparent("T001"),
        ok(preview(&[
            ("BUKRS", &[Some("1000"), Some("2000")]),
            ("BUTXT", &[Some("Alpha"), None]),
        ])),
    ]);
    let data = run(
        &mut transport,
        &request("t001", &["bukrs", "butxt"], Some("bukrs = '1000'"), 2),
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(transport.calls.len(), 2);
    assert_eq!(
        sent(&transport, 0),
        (
            "/sap/bc/adt/datapreview/ddic?rowNumber=10&ddicEntityName=DD02L".to_owned(),
            "SELECT TABNAME, TABCLASS, VIEWCLASS FROM DD02L WHERE TABNAME = 'T001' AND AS4LOCAL = 'A'"
                .to_owned()
        )
    );
    assert_eq!(
        sent(&transport, 1),
        (
            "/sap/bc/adt/datapreview/ddic?rowNumber=2&ddicEntityName=T001".to_owned(),
            "SELECT BUKRS, BUTXT FROM T001 WHERE BUKRS = '1000'".to_owned()
        )
    );
    assert_eq!(data.columns, ["BUKRS", "BUTXT"]);
    assert_eq!(
        data.rows,
        [["1000", "Alpha"], ["2000", ""]].map(|row| row.map(str::to_owned).to_vec())
    );
    assert_eq!(data.render(1 << 20).0, "BUKRS\tBUTXT\n1000\tAlpha\n2000\t");
    transport.assert_finished();
}

#[test]
fn a_750_reply_with_one_row_more_is_cut_to_the_requested_rows() {
    let mut transport = ScriptedTransport::replies(vec![
        transparent("T001"),
        ok(preview(&[("BUKRS", &[Some("1"), Some("2"), Some("3")])])),
    ]);
    let data = run(
        &mut transport,
        &request("T001", &[], None, 2),
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(data.rows.len(), 2);
    assert_eq!(data.rows[1], ["2"]);
}

#[test]
fn a_740_reply_with_exactly_the_requested_rows_is_kept() {
    let mut transport = ScriptedTransport::replies(vec![
        transparent("T001"),
        ok(preview(&[("BUKRS", &[Some("1"), Some("2")])])),
    ]);
    let data = run(
        &mut transport,
        &request("T001", &[], None, 2),
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(data.rows.len(), 2);
}

#[test]
fn zero_rows_print_only_the_header() {
    let mut transport = ScriptedTransport::replies(vec![
        transparent("T001"),
        ok(preview(&[("BUKRS", &[]), ("BUTXT", &[])])),
    ]);
    let data = run(
        &mut transport,
        &request("T001", &[], None, 5),
        &Policies::default(),
    )
    .unwrap();

    assert!(data.rows.is_empty());
    assert_eq!(data.render(1 << 20), ("BUKRS\tBUTXT".to_owned(), 0));
}

#[test]
fn values_never_break_the_tab_separated_layout() {
    let mut transport = ScriptedTransport::replies(vec![
        transparent("T001"),
        ok(preview(&[("TXT", &[Some("a\tb\nc & d <e>")])])),
    ]);
    let data = run(
        &mut transport,
        &request("T001", &[], None, 5),
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(data.rows[0], ["a\tb\nc & d <e>"]);
    assert_eq!(data.render(1 << 20).0, "TXT\na b c & d <e>");
}

#[test]
fn output_beyond_the_size_cap_is_cut_at_a_row_boundary() {
    let values: Vec<Option<&str>> = vec![Some("0123456789"); 50];
    let mut transport =
        ScriptedTransport::replies(vec![transparent("T001"), ok(preview(&[("V", &values)]))]);
    let data = run(
        &mut transport,
        &request("T001", &[], None, 100),
        &Policies::default(),
    )
    .unwrap();
    let (text, omitted) = data.render(2 + 11 * 4);

    assert_eq!(text, format!("V\n{}", ["0123456789"; 4].join("\n")));
    assert_eq!(omitted, 46);
    assert_eq!(data.render(1 << 20).1, 0);
}

#[test]
fn a_denied_base_table_of_a_view_fails_before_the_data_read() {
    let mut transport = ScriptedTransport::replies(vec![
        database_view("ZV_USERS"),
        bases("ZV_USERS", &["T000", "USR02"]),
    ]);
    let error = run(
        &mut transport,
        &request("ZV_USERS", &[], None, 5),
        &Policies::default(),
    )
    .unwrap_err();

    match error {
        ReadTableError::Rejected(message) => {
            assert!(
                message.contains("USR02") && message.contains("ZV_USERS"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }

    assert_eq!(transport.calls.len(), 2);
    assert!(
        sent(&transport, 1)
            .1
            .contains("FROM DD26S WHERE VIEWNAME = 'ZV_USERS'")
    );
    assert!(
        transport
            .calls
            .iter()
            .all(|call| !call.uri.contains("ddicEntityName=ZV_USERS")),
        "the view was read"
    );
    transport.assert_finished();
}

#[test]
fn the_deny_list_of_the_view_check_is_the_policy_in_force() {
    let relaxed = policies_denying(&[]);
    let mut transport = ScriptedTransport::replies(vec![
        database_view("ZV_USERS"),
        bases("ZV_USERS", &["USR02"]),
        ok(preview(&[("BNAME", &[Some("X")])])),
    ]);

    assert!(run(&mut transport, &request("ZV_USERS", &[], None, 5), &relaxed).is_ok());

    let added = policies_denying(&["T000"]);
    let mut transport = ScriptedTransport::replies(vec![
        database_view("SBC_CLIENT"),
        bases("SBC_CLIENT", &["t000"]),
    ]);

    assert!(matches!(
        run(&mut transport, &request("SBC_CLIENT", &[], None, 5), &added),
        Err(ReadTableError::Rejected(_))
    ));
}

#[test]
fn a_view_over_allowed_tables_is_read_after_two_lookups() {
    let mut transport = ScriptedTransport::replies(vec![
        database_view("SBC_CLIENT"),
        bases("SBC_CLIENT", &["T000"]),
        ok(preview(&[("MANDT", &[Some("100")])])),
    ]);
    let data = run(
        &mut transport,
        &request("SBC_CLIENT", &[], None, 5),
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(transport.calls.len(), 3);
    assert_eq!(
        sent(&transport, 1),
        (
            "/sap/bc/adt/datapreview/ddic?rowNumber=500&ddicEntityName=DD26S".to_owned(),
            "SELECT VIEWNAME, TABNAME, TABPOS FROM DD26S WHERE VIEWNAME = 'SBC_CLIENT' AND AS4LOCAL = 'A'"
                .to_owned()
        )
    );
    assert_eq!(
        sent(&transport, 2),
        (
            "/sap/bc/adt/datapreview/ddic?rowNumber=5&ddicEntityName=SBC_CLIENT".to_owned(),
            "SELECT * FROM SBC_CLIENT".to_owned()
        )
    );
    assert_eq!(data.rows, [["100"]]);
}

#[test]
fn pool_and_cluster_tables_are_read_like_transparent_ones() {
    for class in ["POOL", "CLUSTER", "TRANSP"] {
        let mut transport = ScriptedTransport::replies(vec![
            class_of("BC325V00", class, None),
            ok(preview(&[("A", &[Some("1")])])),
        ]);

        assert!(
            run(
                &mut transport,
                &request("BC325V00", &[], None, 5),
                &Policies::default()
            )
            .is_ok(),
            "{class}"
        );
    }
}

fn rejected(
    first: Result<Response, TransportError>,
    rest: Vec<Result<Response, TransportError>>,
    expected_calls: usize,
) -> String {
    let mut replies = vec![first];

    replies.extend(rest);

    let mut transport = ScriptedTransport::replies(replies);
    let error = run(
        &mut transport,
        &request("T001", &[], None, 5),
        &Policies::default(),
    )
    .expect_err("must be rejected");

    assert_eq!(transport.calls.len(), expected_calls, "{error:?}");

    match error {
        ReadTableError::Rejected(message) => message,
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unknown_name_is_rejected_without_a_read() {
    let empty = ok(preview(&[
        ("TABNAME", &[]),
        ("TABCLASS", &[]),
        ("VIEWCLASS", &[]),
    ]));

    assert!(rejected(empty, vec![], 1).contains("T001"));
}

#[test]
fn a_lookup_that_fails_or_cannot_be_read_is_rejected_fail_closed() {
    let message = rejected(Ok(Response::text(500, "boom")), vec![], 1);

    assert!(message.contains("HTTP 500"), "{message}");
    rejected(Ok(Response::text(400, "<exc:exception/>")), vec![], 1);
    rejected(ok("not xml <<".to_owned()), vec![], 1);
    rejected(ok(preview(&[("OTHER", &[Some("x")])])), vec![], 1);
    rejected(
        ok(preview(&[
            ("TABNAME", &[Some("OTHER")]),
            ("TABCLASS", &[Some("TRANSP")]),
            ("VIEWCLASS", &[None]),
        ])),
        vec![],
        1,
    );
    rejected(
        ok(preview(&[
            ("TABNAME", &[Some("T001"), Some("T001")]),
            ("TABCLASS", &[Some("TRANSP"), Some("VIEW")]),
            ("VIEWCLASS", &[None, Some("D")]),
        ])),
        vec![],
        1,
    );
}

#[test]
fn a_view_without_a_readable_base_list_is_rejected() {
    rejected(database_view("T001"), vec![bases("T001", &[])], 2);
    rejected(
        database_view("T001"),
        vec![Ok(Response::text(500, "boom"))],
        2,
    );
    rejected(database_view("T001"), vec![ok("<<".to_owned())], 2);
    rejected(
        database_view("T001"),
        vec![ok(preview(&[("VIEWNAME", &[Some("T001")])]))],
        2,
    );
}

#[test]
fn cds_and_other_non_database_views_are_rejected() {
    for view_class in [Some("V"), Some("P"), Some("H"), Some("C"), None] {
        let message = rejected(class_of("T001", "VIEW", view_class), vec![], 1);

        assert!(message.contains("database view"), "{message}");
    }
}

#[test]
fn objects_that_are_no_table_or_view_are_rejected() {
    for class in ["INTTAB", "APPEND", ""] {
        rejected(class_of("T001", class, None), vec![], 1);
    }
}

#[test]
fn a_failed_data_read_reports_the_sap_message() {
    let body = "<exc:exception xmlns:exc=\"http://www.sap.com/abapxml/types/communicationframework\">\
                <type id=\"ExceptionDataPreviewGeneral\"/><localizedMessage lang=\"EN\">Unknown column name \"NOPE\"</localizedMessage></exc:exception>";
    let mut transport =
        ScriptedTransport::replies(vec![transparent("T001"), Ok(Response::text(400, body))]);
    let error = run(
        &mut transport,
        &request("T001", &[], None, 5),
        &Policies::default(),
    )
    .unwrap_err();

    match error {
        ReadTableError::Failed { failure, message } => {
            assert_eq!(failure, Failure::Validation);
            assert!(message.contains("Unknown column name"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_refused_logon_and_a_lost_connection_keep_their_routing_class() {
    let failed = |replies: Vec<Result<Response, TransportError>>| {
        let mut transport = ScriptedTransport::replies(replies);

        run(
            &mut transport,
            &request("T001", &[], None, 5),
            &Policies::default(),
        )
        .unwrap_err()
    };

    match failed(vec![Ok(Response::empty(401))]) {
        ReadTableError::Failed { failure, .. } => assert_eq!(failure, Failure::Authorization),
        other => panic!("{other:?}"),
    }

    match failed(vec![Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    ))]) {
        ReadTableError::Failed { failure, .. } => {
            assert_eq!(failure, Failure::PreSendConnectivity);
        }
        other => panic!("{other:?}"),
    }

    match failed(vec![
        transparent("T001"),
        Err(TransportError::NoResponse("closed".to_owned())),
    ]) {
        ReadTableError::Failed { failure, .. } => {
            assert_eq!(failure, Failure::PreSendConnectivity);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reply_with_columns_of_unequal_length_is_an_error_not_a_guess() {
    let mut transport = ScriptedTransport::replies(vec![
        transparent("T001"),
        ok(preview(&[
            ("A", &[Some("1"), Some("2")]),
            ("B", &[Some("1")]),
        ])),
    ]);

    assert!(matches!(
        run(
            &mut transport,
            &request("T001", &[], None, 5),
            &Policies::default()
        ),
        Err(ReadTableError::Failed { .. })
    ));
}
