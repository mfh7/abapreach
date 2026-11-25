use abapreach::operation::dictionary::{
    BaseRow, CheckError, ClassRow, Dictionary, GeneratedViewRow,
};
use abapreach::operation::read_table::CLI_MAX_ROWS;
use abapreach::operation::sql::{BODY_MAX, ENTITIES_MAX, Query, Select, TEXT_MAX, validate_body};
use abapreach::policy::{DEFAULT_READ_TABLE_DENIED, Policies};

fn body(text: &str) -> String {
    Select::parse(text)
        .unwrap_or_else(|error| panic!("{text:?}: {error}"))
        .body()
}

fn refusal(text: &str) -> String {
    match Select::parse(text) {
        Ok(select) => panic!("{text:?} was accepted as {}", select.body()),
        Err(message) => message,
    }
}

fn refused_with(text: &str, fragment: &str) {
    let message = refusal(text);

    assert!(
        message.contains(fragment),
        "{text:?}: {message:?} lacks {fragment:?}"
    );
}

fn policies_denying(tables: &[&str]) -> Policies {
    Policies {
        read_table_denied: tables.iter().map(|table| (*table).to_owned()).collect(),
        ..Policies::default()
    }
}

fn query(text: &str) -> Result<Query, String> {
    Query::new(text, 10, CLI_MAX_ROWS, &Policies::default())
}

#[test]
fn a_plain_select_gets_its_canonical_form() {
    assert_eq!(body("select * from t001"), "SELECT * FROM T001");
    assert_eq!(
        body(
            "SELECT DISTINCT bukrs, butxt FROM t001 WHERE bukrs = '1000' ORDER BY bukrs DESCENDING"
        ),
        "SELECT DISTINCT BUKRS, BUTXT FROM T001 WHERE BUKRS = '1000' ORDER BY BUKRS DESCENDING"
    );
    assert_eq!(
        body("SELECT bukrs FROM t001 ORDER BY bukrs ASCENDING, butxt"),
        "SELECT BUKRS FROM T001 ORDER BY BUKRS ASCENDING, BUTXT"
    );
}

#[test]
fn namespace_entities_and_fields_are_allowed() {
    assert_eq!(
        body("select /ns/fld from /ns/tab as a"),
        "SELECT /NS/FLD FROM /NS/TAB AS A"
    );
}

#[test]
fn joins_aliases_and_qualified_columns_are_rendered_canonically() {
    assert_eq!(
        body(
            "select a~obj_name, b~dlvunit as unit from tadir as a join tdevc as b \
             on a~devclass = b~devclass where a~object = 'CLAS' and a~obj_name like 'CL_ABAP_TYPED%' \
             order by a~obj_name"
        ),
        "SELECT A~OBJ_NAME, B~DLVUNIT AS UNIT FROM TADIR AS A INNER JOIN TDEVC AS B \
         ON A~DEVCLASS = B~DEVCLASS WHERE A~OBJECT = 'CLAS' AND A~OBJ_NAME LIKE 'CL_ABAP_TYPED%' \
         ORDER BY A~OBJ_NAME"
    );
}

#[test]
fn every_join_spelling_has_one_canonical_form() {
    let on = "ON a~x = b~y";

    for (given, canonical) in [
        ("JOIN", "INNER JOIN"),
        ("INNER JOIN", "INNER JOIN"),
        ("LEFT JOIN", "LEFT OUTER JOIN"),
        ("LEFT OUTER JOIN", "LEFT OUTER JOIN"),
        ("left outer join", "LEFT OUTER JOIN"),
    ] {
        assert_eq!(
            body(&format!("SELECT * FROM t AS a {given} u AS b {on}")),
            format!("SELECT * FROM T AS A {canonical} U AS B ON A~X = B~Y"),
            "{given}"
        );
    }
}

#[test]
fn several_joins_and_several_on_pairs_are_accepted() {
    assert_eq!(
        body(
            "SELECT a~f FROM t AS a INNER JOIN u AS b ON a~k1 = b~k1 AND a~k2 = b~k2 \
             LEFT JOIN v AS c ON b~z = c~z"
        ),
        "SELECT A~F FROM T AS A INNER JOIN U AS B ON A~K1 = B~K1 AND A~K2 = B~K2 \
         LEFT OUTER JOIN V AS C ON B~Z = C~Z"
    );
}

#[test]
fn a_table_without_an_alias_qualifies_with_its_own_name() {
    assert_eq!(
        body("SELECT t001~bukrs FROM t001 INNER JOIN t001w ON t001~bukrs = t001w~werks"),
        "SELECT T001~BUKRS FROM T001 INNER JOIN T001W ON T001~BUKRS = T001W~WERKS"
    );
}

#[test]
fn a_table_may_join_itself_through_aliases() {
    let select = Select::parse("SELECT a~f FROM t AS a JOIN t AS b ON a~k = b~k").unwrap();

    assert_eq!(select.entities(), ["T"]);
}

#[test]
fn aggregates_group_by_and_having_are_accepted() {
    assert_eq!(
        body(
            "select bukrs, count(*) as cnt, sum( dmbtr ), count( distinct x ), min(a~b), max(a~b), avg(a~c) \
             from bsik as a group by bukrs, a~b having count(*) > 1 and sum(dmbtr) <= 5 order by bukrs"
        ),
        "SELECT BUKRS, COUNT( * ) AS CNT, SUM( DMBTR ), COUNT( DISTINCT X ), MIN( A~B ), MAX( A~B ), AVG( A~C ) \
         FROM BSIK AS A GROUP BY BUKRS, A~B HAVING COUNT( * ) > 1 AND SUM( DMBTR ) <= 5 ORDER BY BUKRS"
    );
}

#[test]
fn a_having_may_name_a_plain_column() {
    assert_eq!(
        body("SELECT a, COUNT(*) FROM t GROUP BY a HAVING a <> 'X'"),
        "SELECT A, COUNT( * ) FROM T GROUP BY A HAVING A <> 'X'"
    );
}

#[test]
fn the_where_grammar_of_read_table_applies_to_columns() {
    assert_eq!(
        body(
            "SELECT * FROM t AS a WHERE ( a~x = 1 OR a~y = 'it''s' ) AND NOT a~z IN ('p', 'q') \
             AND a~w BETWEEN -1 AND 25 AND a~v IS NOT NULL AND a~u LIKE 'A%'"
        ),
        "SELECT * FROM T AS A WHERE ( A~X = 1 OR A~Y = 'it''s' ) AND NOT A~Z IN ( 'p', 'q' ) \
         AND A~W BETWEEN -1 AND 25 AND A~V IS NOT NULL AND A~U LIKE 'A%'"
    );
}

#[test]
fn lines_tabs_and_runs_of_spaces_are_whitespace() {
    assert_eq!(
        body("SELECT\ta ,\r\n  b\n FROM   t\n\n WHERE a = 'x  y'"),
        "SELECT A, B FROM T WHERE A = 'x  y'"
    );
}

#[test]
fn a_star_that_is_not_at_a_line_start_is_no_comment() {
    assert_eq!(body("SELECT\n *\nFROM t"), "SELECT * FROM T");
}

#[test]
fn entities_lists_every_from_and_join_name_once() {
    let select = Select::parse(
        "SELECT * FROM t001 AS a JOIN t001w AS b ON a~x = b~y JOIN t001 AS c ON a~x = c~x",
    )
    .unwrap();

    assert_eq!(select.entities(), ["T001", "T001W"]);
}

#[test]
fn subqueries_unions_and_exists_are_refused() {
    refused_with(
        "SELECT * FROM t001 WHERE bukrs IN ( SELECT bukrs FROM t001w )",
        "subqueries",
    );
    refused_with("SELECT a FROM t UNION SELECT a FROM u", "UNION");
    refused_with("SELECT a FROM t UNION ALL SELECT a FROM u", "UNION");
    refused_with("SELECT a FROM t INTERSECT SELECT a FROM u", "UNION");
    refused_with("SELECT a FROM t EXCEPT SELECT a FROM u", "UNION");
    refused_with(
        "SELECT * FROM t001 WHERE EXISTS ( SELECT * FROM t001w )",
        "EXISTS",
    );
    refused_with("SELECT * FROM t001 WHERE NOT exists ( x )", "EXISTS");
}

#[test]
fn with_into_up_to_and_client_clauses_are_refused() {
    refused_with(
        "WITH +x AS ( SELECT * FROM t001 ) SELECT * FROM +x",
        "character '+'",
    );
    refused_with("WITH x AS ( SELECT * FROM t001 ) SELECT * FROM x", "WITH");
    refused_with("SELECT * FROM t001 INTO TABLE lt", "INTO");
    refused_with("SELECT * INTO TABLE lt FROM t001", "INTO");
    refused_with("SELECT * FROM t001 UP TO 5 ROWS", "UP TO");
    refused_with("SELECT * FROM t001 up   to 5 rows", "UP TO");
    refused_with(
        "SELECT * FROM t001 CLIENT SPECIFIED WHERE mandt = '100'",
        "CLIENT SPECIFIED",
    );
    refused_with("SELECT * FROM t001 USING CLIENT '100'", "USING CLIENT");
    refused_with("SELECT * FROM t001 USING client '100'", "USING CLIENT");
}

#[test]
fn host_variables_path_expressions_and_backquotes_are_refused() {
    refused_with(
        "SELECT * FROM t001 WHERE bukrs = @lv_bukrs",
        "host variables",
    );
    refused_with("SELECT @lv FROM t001", "host variables");
    refused_with("SELECT * FROM i_cds\\_assoc", "path expressions");
    refused_with("SELECT a\\_b-c FROM t", "path expressions");
    refused_with("SELECT * FROM t WHERE a = `x`", "backquote");
}

#[test]
fn comments_are_refused_wherever_they_start() {
    refused_with("* comment\nSELECT * FROM t001", "comments");
    refused_with("SELECT * FROM t001\n* comment", "comments");
    refused_with("SELECT * FROM t001\r\n* comment", "comments");
    refused_with("SELECT * FROM t001 \" comment", "comments");
    refused_with("SELECT * FROM t001 WHERE a = 'x' \"y", "comments");
}

#[test]
fn a_quote_or_star_inside_a_literal_is_no_comment() {
    assert_eq!(
        body("SELECT * FROM t WHERE a = 'say \"hi\" * @ ~ \\ `'"),
        "SELECT * FROM T WHERE A = 'say \"hi\" * @ ~ \\ `'"
    );
}

#[test]
fn cds_parameters_are_refused() {
    refused_with("SELECT * FROM zcds( p = 1 )", "CDS parameters");
    refused_with(
        "SELECT * FROM t AS a JOIN zcds( p = 1 ) AS b ON a~x = b~x",
        "CDS parameters",
    );
    refused_with("SELECT * FROM zcds( p_x: 1 )", "character ':'");
    refused_with("SELECT * FROM zcds ( p = 1 )", "CDS parameters");
}

#[test]
fn parenthesized_joins_are_refused() {
    refused_with(
        "SELECT * FROM ( t AS a INNER JOIN u AS b ON a~x = b~x )",
        "parenthesized joins",
    );
    refused_with(
        "SELECT * FROM t AS a INNER JOIN ( u AS b INNER JOIN v AS c ON b~x = c~x ) ON a~x = b~x",
        "parenthesized joins",
    );
    refused_with(
        "SELECT * FROM t AS a JOIN u AS b ON ( a~x = b~x )",
        "parentheses in ON",
    );
}

#[test]
fn right_cross_and_full_joins_are_refused() {
    refused_with(
        "SELECT * FROM t AS a RIGHT JOIN u AS b ON a~x = b~x",
        "RIGHT JOIN",
    );
    refused_with(
        "SELECT * FROM t AS a RIGHT OUTER JOIN u AS b ON a~x = b~x",
        "RIGHT JOIN",
    );
    refused_with("SELECT * FROM t AS a CROSS JOIN u AS b", "CROSS JOIN");
    refused_with(
        "SELECT * FROM t AS a FULL OUTER JOIN u AS b ON a~x = b~x",
        "FULL JOIN",
    );
    refused_with(
        "SELECT * FROM t AS a LEFT RIGHT JOIN u AS b ON a~x = b~x",
        "expected JOIN",
    );
}

#[test]
fn only_field_to_field_equalities_join_tables() {
    refused_with("SELECT * FROM t AS a JOIN u AS b", "expected ON");
    refused_with(
        "SELECT * FROM t AS a JOIN u AS b ON a~x < b~x",
        "expected =",
    );
    refused_with(
        "SELECT * FROM t AS a JOIN u AS b ON a~x = 'X'",
        "expected a field name",
    );
    refused_with(
        "SELECT * FROM t AS a JOIN u AS b ON a~x = b~x OR a~y = b~y",
        "unexpected OR",
    );
    refused_with(
        "SELECT * FROM t AS a JOIN u AS b ON a~x = b~x AND a~y = 'X'",
        "expected a field name",
    );
    refused_with("SELECT * FROM t AS a JOIN u AS b ON a~x", "expected =");
}

#[test]
fn qualifiers_must_name_a_source_of_the_statement() {
    refused_with("SELECT x~f FROM t AS a", "X~ names no table");
    refused_with("SELECT t~f FROM t AS a", "T~ names no table");
    refused_with("SELECT * FROM t AS a WHERE z~f = 1", "Z~ names no table");
    refused_with(
        "SELECT * FROM t AS a JOIN u AS b ON a~x = z~x",
        "Z~ names no table",
    );
    refused_with("SELECT f FROM t AS a GROUP BY z~f", "Z~ names no table");
    refused_with("SELECT f FROM t AS a ORDER BY z~f", "Z~ names no table");
    refused_with(
        "SELECT COUNT( DISTINCT z~f ) FROM t AS a",
        "Z~ names no table",
    );
    refused_with(
        "SELECT f FROM t AS a GROUP BY f HAVING MAX( z~f ) > 1",
        "Z~ names no table",
    );
}

#[test]
fn every_source_needs_a_handle_of_its_own() {
    refused_with("SELECT * FROM t JOIN t ON t~x = t~y", "T is used twice");
    refused_with(
        "SELECT * FROM t AS a JOIN u AS a ON a~x = a~y",
        "A is used twice",
    );
}

#[test]
fn names_that_are_sql_keywords_are_refused() {
    refused_with("SELECT * FROM where", "SQL keyword");
    refused_with("SELECT * FROM t AS where", "SQL keyword");
    refused_with("SELECT * FROM t AS inner", "SQL keyword");
    refused_with("SELECT left FROM t", "SQL keyword");
    refused_with("SELECT * FROM t WHERE distinct = 1", "SQL keyword");
    refused_with("SELECT a~order FROM t AS a", "SQL keyword");
    refused_with("SELECT order~a FROM t", "SQL keyword");
}

#[test]
fn malformed_statements_are_refused() {
    refused_with("", "empty");
    refused_with("   \n\t", "empty");
    refused_with("SELECT", "expected a field name");
    refused_with("SELECT *", "expected FROM");
    refused_with("SELECT * FROM", "expected a table or view name");
    refused_with("SELECT a, * FROM t", "expected a field name");
    refused_with("SELECT a b FROM t", "expected FROM");
    refused_with("FROM t SELECT a", "expected SELECT");
    refused_with("SELECT * FROM t WHERE", "expected a field name");
    refused_with("SELECT * FROM t GROUP a", "expected BY");
    refused_with("SELECT * FROM t ORDER a", "expected BY");
    refused_with("SELECT * FROM t ORDER BY", "expected a field name");
    refused_with("SELECT * FROM t FOR UPDATE", "unexpected FOR");
    refused_with("SELECT * FROM t WHERE a = 1 b", "unexpected b");
    refused_with("SELECT * FROM t AS", "expected an alias");
    refused_with("SELECT a AS FROM t", "SQL keyword");
    refused_with("SELECT a~ FROM t AS a", "field \"FROM\" is an SQL keyword");
    refused_with("SELECT a~* FROM t AS a", "expected a field name after A~");
    refused_with("SELECT * FROM t.", "terminator");
    refused_with("SELECT * FROM t WHERE a = 'x", "unterminated");
    refused_with("SELECT a+b FROM t", "character '+'");
    refused_with(
        "SELECT * FROM t WHERE a = 1 ORDER BY a DESCENDING ASCENDING",
        "unexpected ASCENDING",
    );
}

#[test]
fn aggregate_calls_are_checked() {
    refused_with("SELECT SUM( * ) FROM t", "expected a field name");
    refused_with("SELECT COUNT( DISTINCT * ) FROM t", "expected a field name");
    refused_with("SELECT COUNT( a FROM t", "expected ) to close COUNT");
    refused_with("SELECT COUNT( a, b ) FROM t", "expected ) to close COUNT");
    refused_with(
        "SELECT * FROM t WHERE COUNT( * ) > 1",
        "expected an operator after COUNT",
    );
    refused_with("SELECT * FROM t ORDER BY COUNT( * )", "unexpected (");
    assert_eq!(body("SELECT count FROM t"), "SELECT COUNT FROM T");
}

#[test]
fn statement_and_entity_counts_are_bounded() {
    let long = format!("SELECT * FROM t WHERE a = '{}'", "x".repeat(TEXT_MAX));

    refused_with(&long, "longer than");

    let mut text = "SELECT * FROM t0 AS a0".to_owned();

    for index in 1..=ENTITIES_MAX {
        text.push_str(&format!(" JOIN t{index} AS a{index} ON a0~k = a{index}~k"));
    }

    refused_with(&text, "more than 10 tables");

    text = "SELECT * FROM t0 AS a0".to_owned();

    for index in 1..ENTITIES_MAX {
        text.push_str(&format!(" JOIN t{index} AS a{index} ON a0~k = a{index}~k"));
    }

    assert_eq!(Select::parse(&text).unwrap().entities().len(), ENTITIES_MAX);
}

#[test]
fn a_literal_of_255_characters_is_the_longest() {
    assert!(Select::parse(&format!("SELECT * FROM t WHERE a = '{}'", "x".repeat(255))).is_ok());
    refused_with(
        &format!("SELECT * FROM t WHERE a = '{}'", "x".repeat(256)),
        "sql: a literal is longer than 255",
    );
    refused_with(
        "SELECT * FROM t WHERE a = 'x\ny'",
        "sql: a literal holds a control character",
    );
}

#[test]
fn a_condition_may_nest_to_16_levels_and_not_deeper() {
    let nested = |levels: usize| {
        format!(
            "SELECT * FROM t WHERE {}a = 1{}",
            "( ".repeat(levels),
            " )".repeat(levels)
        )
    };

    assert!(Select::parse(&nested(16)).is_ok());
    refused_with(&nested(17), "sql: nested too deeply");
}

#[test]
fn every_entity_goes_through_the_deny_list() {
    for denied in DEFAULT_READ_TABLE_DENIED {
        let lower = denied.to_ascii_lowercase();

        for text in [
            format!("SELECT * FROM {lower}"),
            format!("SELECT * FROM {lower} AS u"),
            format!("SELECT a~x FROM t AS a JOIN {lower} AS u ON a~k = u~k"),
            format!(
                "SELECT a~x FROM t AS a LEFT JOIN t2 AS b ON a~k = b~k INNER JOIN {denied} ON a~k = {denied}~k"
            ),
            format!("SELECT * FROM {denied} AS u JOIN t AS a ON a~k = u~k"),
        ] {
            let message = query(&text).expect_err(&text);

            assert_eq!(
                message,
                format!("table {denied} is on the read_table_denied list"),
                "{text}"
            );
        }
    }
}

#[test]
fn the_deny_list_in_force_decides_and_an_allowed_join_passes() {
    let text = "SELECT a~x FROM t AS a JOIN usr02 AS u ON a~k = u~k";

    assert!(Query::new(text, 10, CLI_MAX_ROWS, &Policies::default()).is_err());
    assert!(Query::new(text, 10, CLI_MAX_ROWS, &policies_denying(&[])).is_ok());
    assert!(Query::new(text, 10, CLI_MAX_ROWS, &policies_denying(&["T"])).is_err());
    assert!(Query::new(text, 10, CLI_MAX_ROWS, &policies_denying(&["u", "usr02"])).is_err());

    let query = Query::new(
        "SELECT * FROM tadir AS a JOIN tdevc AS b ON a~devclass = b~devclass",
        7,
        CLI_MAX_ROWS,
        &Policies::default(),
    )
    .unwrap();

    assert_eq!(query.entities(), ["TADIR", "TDEVC"]);
    assert_eq!(query.rows(), 7);

    let verified = query.verify(&mut Tables, &Policies::default()).unwrap();

    assert_eq!(verified.entities(), ["TADIR", "TDEVC"]);
    assert_eq!(
        verified.body(),
        "SELECT * FROM TADIR AS A INNER JOIN TDEVC AS B ON A~DEVCLASS = B~DEVCLASS"
    );
    assert_eq!(verified.rows(), 7);
}

/// A dictionary where every name is a transparent table, except `GONE`, which is unknown.
struct Tables;

impl Dictionary for Tables {
    type Error = String;

    fn class_rows(&mut self, name: &str) -> Result<Vec<ClassRow>, String> {
        Ok(if name == "GONE" {
            Vec::new()
        } else {
            vec![ClassRow {
                name: name.to_owned(),
                table_class: "TRANSP".to_owned(),
                view_class: None,
            }]
        })
    }

    fn generated_view_rows(&mut self, _: &str) -> Result<Vec<GeneratedViewRow>, String> {
        Ok(Vec::new())
    }

    fn base_rows(&mut self, _: &str) -> Result<Vec<BaseRow>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn only_a_query_that_passed_the_dictionary_check_yields_a_body() {
    let query = Query::new(
        "SELECT * FROM t001 AS a JOIN gone AS b ON a~x = b~x",
        10,
        CLI_MAX_ROWS,
        &Policies::default(),
    )
    .unwrap();

    match query.verify(&mut Tables, &Policies::default()) {
        Err(CheckError::Refused(message)) => {
            assert!(message.contains("GONE is not in"), "{message}")
        }
        other => panic!("{other:?}"),
    }

    let denied = Query::new("SELECT * FROM t001", 10, CLI_MAX_ROWS, &Policies::default()).unwrap();

    assert!(
        denied
            .verify(&mut Tables, &policies_denying(&["T001"]))
            .is_err()
    );
}

#[test]
fn an_operator_the_comparison_grammar_does_not_list_is_refused() {
    for text in [
        "SELECT * FROM t AS a WHERE a~x * 1",
        "SELECT * FROM t WHERE a * 1",
        "SELECT * FROM t WHERE a ~ 'x'",
        "SELECT * FROM t WHERE a ~ b",
        "SELECT a FROM t GROUP BY a HAVING COUNT( * ) * 1",
        "SELECT a FROM t GROUP BY a HAVING a * 1 > 1",
    ] {
        let message = refusal(text);

        assert!(
            query(text).is_err() && message.starts_with("sql: "),
            "{text}: {message}"
        );
    }

    for body in [
        "SELECT * FROM T WHERE A * 1",
        "SELECT * FROM T WHERE A ~ 'x'",
        "SELECT A FROM T GROUP BY A HAVING COUNT( * ) * 1",
    ] {
        assert!(validate_body(body).is_err(), "{body}");
    }

    refused_with(
        "SELECT * FROM t WHERE a * 1",
        "expected an operator after A",
    );
    refused_with(
        "SELECT a FROM t GROUP BY a HAVING COUNT( * ) * 1",
        "expected an operator after COUNT( * )",
    );

    for operator in ["=", "<>", "<", "<=", ">", ">="] {
        assert_eq!(
            body(&format!("SELECT * FROM t WHERE a {operator} 1")),
            format!("SELECT * FROM T WHERE A {operator} 1")
        );
    }
}

#[test]
fn a_decimal_number_needs_a_quoted_literal_in_sql() {
    for text in [
        "SELECT * FROM t WHERE a = 1.5",
        "SELECT * FROM t WHERE a BETWEEN 1 AND 2.5",
        "SELECT * FROM t WHERE a IN ( 1, 2.5 )",
        "SELECT a FROM t GROUP BY a HAVING SUM( a ) > 0.5",
    ] {
        refused_with(text, "a decimal number must be a quoted literal");
        assert!(query(text).is_err(), "{text}");
    }

    refused_with(
        "SELECT * FROM t WHERE a = -1.5",
        "a decimal number must be a quoted literal",
    );
    assert_eq!(
        body("SELECT * FROM t WHERE a = '1.5' AND b = -3 AND c = 12"),
        "SELECT * FROM T WHERE A = '1.5' AND B = -3 AND C = 12"
    );
    assert!(validate_body("SELECT * FROM T WHERE A = 1.5").is_err());
}

#[test]
fn the_worst_case_expansion_stays_within_the_body_limit() {
    // ` or(a=1)` grows from 8 to 13 characters in canonical form: the densest growth there is.
    let mut text = String::from("SELECT * FROM t WHERE a=1");

    while text.len() + 8 <= TEXT_MAX {
        text.push_str(" or(a=1)");
    }

    assert!(text.len() <= TEXT_MAX);

    let query = Query::new(&text, 10, CLI_MAX_ROWS, &Policies::default()).unwrap();
    let verified = query.verify(&mut Tables, &Policies::default()).unwrap();

    assert!(verified.body().len() > TEXT_MAX * 16 / 10);
    assert!(
        verified.body().len() <= BODY_MAX,
        "{}",
        verified.body().len()
    );
    assert_eq!(validate_body(verified.body()), Ok(()));
}

#[test]
fn a_denied_table_inside_a_literal_or_a_field_is_not_an_entity() {
    let query = query("SELECT usr02 FROM t001 WHERE bukrs = 'USR02'").unwrap();

    assert_eq!(query.entities(), ["T001"]);
}

#[test]
fn rows_follow_the_limit_of_the_caller() {
    let text = "SELECT * FROM t001";

    assert!(Query::new(text, 0, 5000, &Policies::default()).is_err());
    assert!(Query::new(text, 1, 5000, &Policies::default()).is_ok());
    assert!(Query::new(text, 1000, 1000, &Policies::default()).is_ok());
    assert!(Query::new(text, 1001, 1000, &Policies::default()).is_err());
    assert_eq!(
        Query::new(text, 0, 5000, &Policies::default()).unwrap_err(),
        "rows must be between 1 and 5000"
    );
}

#[test]
fn a_refused_statement_makes_no_query() {
    assert!(query("SELECT * FROM t001 INTO TABLE x").is_err());
    assert!(query("").is_err());
}

#[test]
fn validate_accepts_a_canonical_body_and_refuses_user_text() {
    let canonical = "SELECT A~X, COUNT( * ) AS N FROM T AS A LEFT OUTER JOIN U AS B ON A~K = B~K \
                     WHERE A~X = 'a' GROUP BY A~X HAVING COUNT( * ) > 1 ORDER BY A~X DESCENDING";

    assert_eq!(validate_body(canonical), Ok(()));
    assert_eq!(Select::parse(canonical).unwrap().body(), canonical);

    for text in [
        "select a~x from t as a",
        "SELECT * FROM T ",
        " SELECT * FROM T",
        "SELECT  * FROM T",
        "SELECT *\nFROM T",
        "SELECT *\tFROM T",
        "SELECT A,B FROM T",
        "SELECT A , B FROM T",
        "SELECT * FROM T JOIN U ON T~X = U~X",
        "SELECT * FROM T LEFT JOIN U ON T~X = U~X",
        "SELECT * FROM T WHERE A=1",
        "SELECT * FROM T WHERE (A = 1)",
        "SELECT * FROM T WHERE A IN ('x','y')",
        "SELECT COUNT(*) FROM T",
        "SELECT * FROM T ORDER BY A DESCENDING ",
    ] {
        assert!(
            Select::parse(text).is_ok() && validate_body(text).is_err(),
            "{text:?}: accepted by the grammar but must not be a valid body"
        );
    }

    assert!(validate_body("SELECT * FROM T WHERE A = 'x'").is_ok());
    assert!(validate_body("SELECT * FROM T WHERE A = 'x' ").is_err());
}

#[test]
fn validate_refuses_every_form_the_grammar_refuses() {
    for text in [
        "SELECT * FROM T WHERE A IN ( SELECT A FROM U )",
        "SELECT A FROM T UNION SELECT A FROM U",
        "SELECT * FROM T INTO TABLE X",
        "SELECT * FROM T UP TO 5 ROWS",
        "SELECT * FROM T WHERE A = @X",
        "SELECT * FROM T\n* comment",
        "SELECT * FROM T \" comment",
        "SELECT * FROM T WHERE A = `x`",
        "SELECT * FROM ( T AS A INNER JOIN U AS B ON A~X = B~X )",
        "SELECT * FROM T AS A RIGHT OUTER JOIN U AS B ON A~X = B~X",
        "SELECT * FROM T AS A CROSS JOIN U AS B",
        "SELECT * FROM T USING CLIENT '100'",
        "SELECT * FROM T CLIENT SPECIFIED",
        "SELECT * FROM T( P = 1 )",
        "",
        "SELECT * FROM T.",
    ] {
        assert!(validate_body(text).is_err(), "{text:?}");
    }
}

#[test]
fn validate_allows_a_body_up_to_the_body_limit_only() {
    // Many short predicates reach the body limit without meeting the literal limit.
    let predicate = "A = 'xxxxxxxxxxxxxxxxxxxx'";
    let mut text = format!("SELECT * FROM T WHERE {predicate}");

    while text.len() + 5 + predicate.len() <= BODY_MAX {
        text.push_str(" AND ");
        text.push_str(predicate);
    }

    assert!(text.len() <= BODY_MAX);
    assert_eq!(validate_body(&text), Ok(()), "{}", text.len());

    text.push_str(" AND ");
    text.push_str(predicate);

    assert!(text.len() > BODY_MAX);
    assert!(validate_body(&text).is_err());
}

/// A small deterministic generator: the tests need variety, not randomness.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

/// A random statement as tokens; literals stay one token.
fn generate(rng: &mut Rng) -> Vec<String> {
    let tables = ["tadir", "tdevc", "t001", "/ns/tab", "t001w"];
    let aliases = ["a", "b", "c", "d"];
    let fields = ["f1", "obj_name", "/ns/fld", "devclass"];
    let literals = ["'X'", "'it''s ok'", "'a b'", "'%'", "''", "'1000'"];
    let sources = 1 + rng.below(3);
    let mut handles: Vec<String> = Vec::new();
    let mut from: Vec<Vec<String>> = Vec::new();

    for index in 0..sources {
        let table = tables[index];
        let aliased = rng.chance(2);
        let mut source = if index == 0 {
            vec!["FROM".to_owned()]
        } else {
            let join = match rng.below(4) {
                0 => vec!["INNER", "JOIN"],
                1 => vec!["LEFT", "JOIN"],
                2 => vec!["LEFT", "OUTER", "JOIN"],
                _ => vec!["JOIN"],
            };

            join.into_iter().map(str::to_owned).collect()
        };

        source.push(table.to_owned());

        if aliased {
            source.push("AS".to_owned());
            source.push(aliases[index].to_owned());
            handles.push(aliases[index].to_owned());
        } else {
            handles.push(table.to_owned());
        }

        if index > 0 {
            source.push("ON".to_owned());

            for pair in 0..1 + rng.below(2) {
                if pair > 0 {
                    source.push("AND".to_owned());
                }

                let left = &handles[rng.below(index)];
                let right = &handles[index];

                source.push(format!("{left}~{}", rng.pick(&fields)));
                source.push("=".to_owned());
                source.push(format!("{right}~{}", rng.pick(&fields)));
            }
        }

        from.push(source);
    }

    let column = |rng: &mut Rng| -> String {
        if rng.chance(2) {
            format!(
                "{}~{}",
                handles[rng.below(handles.len())],
                rng.pick(&fields)
            )
        } else {
            rng.pick(&fields).to_owned()
        }
    };
    let mut tokens = vec!["SELECT".to_owned()];

    if rng.chance(3) {
        tokens.push("DISTINCT".to_owned());
    }

    let mut grouped = false;

    if rng.chance(4) {
        tokens.push("*".to_owned());
    } else {
        for index in 0..1 + rng.below(3) {
            if index > 0 {
                tokens.push(",".to_owned());
            }

            if rng.chance(3) {
                let function = rng.pick(&["COUNT", "SUM", "MIN", "MAX", "AVG"]);

                grouped = true;
                tokens.push(function.to_owned());
                tokens.push("(".to_owned());

                if function == "COUNT" && rng.chance(2) {
                    tokens.push("*".to_owned());
                } else {
                    if rng.chance(3) {
                        tokens.push("DISTINCT".to_owned());
                    }

                    tokens.push(column(rng));
                }

                tokens.push(")".to_owned());
            } else {
                tokens.push(column(rng));
            }

            if rng.chance(3) {
                tokens.push("AS".to_owned());
                tokens.push(format!("r{index}"));
            }
        }
    }

    for source in from {
        tokens.extend(source);
    }

    let predicate = |rng: &mut Rng, column: String| -> Vec<String> {
        match rng.below(6) {
            0 => vec![column, "=".to_owned(), rng.pick(&literals).to_owned()],
            1 => vec![
                column,
                "<>".to_owned(),
                rng.pick(&["1", "-2", "35"]).to_owned(),
            ],
            2 => vec![column, "LIKE".to_owned(), rng.pick(&literals).to_owned()],
            3 => vec![
                column,
                "IN".to_owned(),
                "(".to_owned(),
                rng.pick(&literals).to_owned(),
                ",".to_owned(),
                rng.pick(&literals).to_owned(),
                ")".to_owned(),
            ],
            4 => vec![
                column,
                "BETWEEN".to_owned(),
                "1".to_owned(),
                "AND".to_owned(),
                "9".to_owned(),
            ],
            _ => vec![column, "IS".to_owned(), "NOT".to_owned(), "NULL".to_owned()],
        }
    };
    let condition = |rng: &mut Rng, handles_column: &dyn Fn(&mut Rng) -> String| -> Vec<String> {
        let mut out = Vec::new();

        for index in 0..1 + rng.below(3) {
            if index > 0 {
                out.push(rng.pick(&["AND", "OR"]).to_owned());
            }

            let negate = rng.chance(4);
            let group = rng.chance(4);

            if negate {
                out.push("NOT".to_owned());
            }

            if group {
                out.push("(".to_owned());
            }

            let left = handles_column(rng);

            out.extend(predicate(rng, left));

            if group {
                out.push(")".to_owned());
            }
        }

        out
    };

    if rng.chance(2) {
        tokens.push("WHERE".to_owned());
        tokens.extend(condition(rng, &|rng| column(rng)));
    }

    if grouped || rng.chance(4) {
        tokens.push("GROUP".to_owned());
        tokens.push("BY".to_owned());

        for index in 0..1 + rng.below(2) {
            if index > 0 {
                tokens.push(",".to_owned());
            }

            tokens.push(column(rng));
        }

        if rng.chance(2) {
            tokens.push("HAVING".to_owned());
            tokens.push(rng.pick(&["COUNT", "SUM"]).to_owned());
            tokens.push("(".to_owned());
            tokens.push(column(rng));
            tokens.push(")".to_owned());
            tokens.push(">".to_owned());
            tokens.push("1".to_owned());
        }
    }

    if rng.chance(3) {
        tokens.push("ORDER".to_owned());
        tokens.push("BY".to_owned());

        for index in 0..1 + rng.below(2) {
            if index > 0 {
                tokens.push(",".to_owned());
            }

            tokens.push(column(rng));

            match rng.below(3) {
                0 => tokens.push("ASCENDING".to_owned()),
                1 => tokens.push("DESCENDING".to_owned()),
                _ => {}
            }
        }
    }

    tokens
}

/// The tokens as user text: random case outside literals and random whitespace between tokens.
fn spell(rng: &mut Rng, tokens: &[String]) -> String {
    let mut text = String::new();

    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            // A star at a line start is a comment.
            let glue = if token == "*" {
                " "
            } else {
                rng.pick(&[" ", " ", "  ", "\n", "\t", " \n  ", "\r\n"])
            };

            if !(token == "," || token == ")") || rng.chance(2) {
                text.push_str(glue);
            }
        }

        if token.starts_with('\'') || !rng.chance(2) {
            text.push_str(token);
        } else {
            text.push_str(&token.to_ascii_lowercase());
        }
    }

    text
}

/// Edits that keep the text parseable or not, but never leave it equal to the canonical body.
fn disturb(rng: &mut Rng, body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut at = rng.below(chars.len());

    // Never edit inside a literal: that changes the value, which is a different statement.
    let outside = |upto: usize| chars[..upto].iter().filter(|c| **c == '\'').count() % 2 == 0;

    while !outside(at) {
        at = rng.below(chars.len());
    }

    let mut edited: Vec<char> = chars.clone();

    match rng.below(5) {
        0 => edited.insert(at, ' '),
        1 => edited.insert(at, '\n'),
        2 => {
            edited[at] = if edited[at].is_ascii_uppercase() {
                edited[at].to_ascii_lowercase()
            } else {
                edited[at]
            };

            edited.push(' ');
        }
        3 => edited.insert(0, ' '),
        _ => edited.push('\t'),
    }

    edited.into_iter().collect()
}

#[test]
fn generated_statements_round_trip_to_a_fixpoint() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut seen = std::collections::HashSet::new();

    for _ in 0..3000 {
        let tokens = generate(&mut rng);
        let text = spell(&mut rng, &tokens);
        let select = Select::parse(&text).unwrap_or_else(|error| panic!("{text:?}: {error}"));
        let canonical = select.body();

        assert!(
            !canonical.contains(['\n', '\r', '\t']) && !canonical.contains("  "),
            "{canonical:?}"
        );
        assert_eq!(validate_body(&canonical), Ok(()), "{canonical}");

        let again = Select::parse(&canonical).unwrap();

        assert_eq!(again, select, "{canonical}");
        assert_eq!(again.body(), canonical, "{text:?}");
        seen.insert(canonical);
    }

    assert!(seen.len() > 1000, "only {} distinct statements", seen.len());
}

#[test]
fn spelling_never_changes_the_canonical_body() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);

    for _ in 0..500 {
        let tokens = generate(&mut rng);
        let first = Select::parse(&spell(&mut rng, &tokens)).unwrap().body();
        let second = Select::parse(&spell(&mut rng, &tokens)).unwrap().body();

        assert_eq!(first, second);
    }
}

#[test]
fn validate_refuses_any_body_that_is_not_its_own_canonical_rendering() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);

    for _ in 0..1500 {
        let tokens = generate(&mut rng);
        let canonical = Select::parse(&spell(&mut rng, &tokens)).unwrap().body();
        let mutated = disturb(&mut rng, &canonical);

        if mutated == canonical {
            continue;
        }

        assert!(
            validate_body(&mutated).is_err(),
            "{mutated:?} passed although {canonical:?} is canonical"
        );
    }
}
