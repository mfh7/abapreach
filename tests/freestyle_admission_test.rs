//! The request boundary of the freestyle data preview: exactly `POST freestyle?rowNumber=<n>`
//! with a canonical `sql` body, nothing else under `/datapreview`.

mod common;

use abapreach::operation::read_table::{CLI_MAX_ROWS, preview_request};
use abapreach::operation::sql::{self, FREESTYLE_URI, Query};
use abapreach::policy::Policies;
use abapreach::transport::{
    Request, Response, SadtTransport, TransportError,
    rfc::{FakeRfcApi, RfcTransport},
};
use common::{ScriptedTransport, preview};

const JOIN: &str = "SELECT TADIR~OBJ_NAME, TDEVC~DLVUNIT FROM TADIR INNER JOIN TDEVC ON TADIR~DEVCLASS = TDEVC~DEVCLASS WHERE TADIR~OBJECT = 'CLAS' AND TADIR~OBJ_NAME LIKE 'CL_ABAP_TYPED%'";

fn admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

/// The freestyle request the operation sends for `text`: the whole `sql::run` on a scripted
/// transport whose dictionary says every entity is a transparent table. `freestyle_request` is
/// private to the operation, so this is the only way to get one.
fn built(text: &str, rows: u32) -> Request {
    let query = Query::new(text, rows, CLI_MAX_ROWS, &Policies::default()).unwrap();
    let mut replies: Vec<Result<Response, TransportError>> = query
        .entities()
        .iter()
        .map(|entity| {
            Ok(Response::text(
                200,
                preview(&[
                    ("TABNAME", &[Some(entity.as_str())]),
                    ("TABCLASS", &[Some("TRANSP")]),
                    ("VIEWCLASS", &[None]),
                ]),
            ))
        })
        .collect();

    replies.push(Ok(Response::text(
        200,
        preview(&[("BUKRS", &[Some("1000")])]),
    )));

    let mut transport = ScriptedTransport::replies(replies);

    sql::run(&mut transport, query, &Policies::default()).unwrap();

    let request = transport.calls.pop().unwrap();

    assert!(request.uri.starts_with(FREESTYLE_URI), "{}", request.uri);

    request
}

fn post(uri: &str, body: &str, accept: Option<&str>, content_type: Option<&str>) -> Request {
    let mut request = Request::post(uri, body.as_bytes().to_vec());

    for (name, value) in [("Accept", accept), ("Content-Type", content_type)] {
        if let Some(value) = value {
            request.headers.insert(name.to_owned(), value.to_owned());
        }
    }

    request
}

fn valid(uri: &str, body: &str) -> Request {
    post(uri, body, Some("application/*"), Some("text/plain"))
}

fn freestyle(body: &str) -> Request {
    valid(&format!("{FREESTYLE_URI}?rowNumber=5"), body)
}

#[track_caller]
fn assert_refused(requests: &[Request]) {
    for request in requests {
        assert!(!admits(request), "must be refused: {request:?}");
    }
}

#[test]
fn the_request_the_operation_builds_is_admitted() {
    for (body, rows) in [
        ("SELECT * FROM T001", 1),
        ("select * from t001", 5000),
        (JOIN, 3),
        (
            "SELECT DISTINCT BUKRS FROM T001 ORDER BY BUKRS DESCENDING",
            100,
        ),
        (
            "SELECT WAERS, COUNT( * ) AS N FROM T001 GROUP BY WAERS HAVING COUNT( * ) > 1",
            10,
        ),
        ("SELECT * FROM /BIC/AFOO", 17),
    ] {
        let request = built(body, rows);

        assert!(admits(&request), "must be admitted: {request:?}");
    }
}

/// The deny list is a policy of the operation layer (`Query::new`, the dictionary check). The
/// boundary only judges the grammar and the canonical spelling, so a canonical statement on a
/// denied table IS admitted here. A hand-built request, because the operation never builds it.
/// If this ever flips, the deny list has moved: change it on purpose, not by accident.
#[test]
fn a_canonical_body_on_a_denied_table_is_admitted_because_the_deny_list_is_not_here() {
    assert!(admits(&freestyle("SELECT * FROM USR02")));
    assert!(Query::new("SELECT * FROM USR02", 5, CLI_MAX_ROWS, &Policies::default()).is_err());
}

#[test]
fn the_exact_shape_is_admitted() {
    assert!(admits(&freestyle(JOIN)));
}

#[test]
fn a_row_count_outside_one_to_5000_or_not_plain_digits_is_refused() {
    for rows in [
        "0",
        "5001",
        "05",
        "+5",
        "-1",
        "",
        "1e3",
        "99999999999",
        "5 ",
        "x",
    ] {
        assert_refused(&[valid(&format!("{FREESTYLE_URI}?rowNumber={rows}"), JOIN)]);
    }
}

#[test]
fn any_query_parameter_besides_the_row_number_is_refused() {
    for query in [
        "",
        "rowNumber=5&ddicEntityName=T001",
        "rowNumber=5&maxRows=5",
        "rowNumber=5&rowNumber=6",
        "rowNumber=5&",
        "rowNumber=5&x",
        "maxRows=5&rowNumber=5",
        "x=1&rowNumber=5",
        "ROWNUMBER=5",
        "rowNumber",
        "rownumber=5",
    ] {
        assert_refused(&[valid(&format!("{FREESTYLE_URI}?{query}"), JOIN)]);
    }

    assert_refused(&[valid(FREESTYLE_URI, JOIN)]);
}

#[test]
fn a_body_that_is_not_the_canonical_rendering_is_refused() {
    for body in [
        "",
        "select * from t001",
        "SELECT  *  FROM T001",
        "SELECT * FROM T001 ",
        " SELECT * FROM T001",
        "SELECT * FROM T001\n",
        "SELECT *\nFROM T001",
        "SELECT\t*\tFROM T001",
        "SELECT * FROM T001 WHERE BUKRS = '1000' \"comment",
        "SELECT * FROM T001 WHERE BUKRS = `1000`",
        "SELECT * FROM T001 UP TO 5 ROWS",
        "SELECT * FROM T001 INTO TABLE @DATA(LT_X)",
        "SELECT * FROM T001; SELECT * FROM T000",
        "SELECT * FROM T001 UNION SELECT * FROM T000",
        "SELECT BUKRS FROM T001 WHERE BUKRS IN ( SELECT BUKRS FROM T000 )",
        "SELECT * FROM T001 WHERE BUKRS = '1000''",
        "SELECT T001~BUKRS FROM T001 INNER JOIN T000 ON T001~BUKRS = T000~MANDT ",
        "SELECT * FROM T001 RIGHT OUTER JOIN T000 ON T001~BUKRS = T000~MANDT",
        "SELECT * FROM T001 CLIENT SPECIFIED",
    ] {
        assert_refused(&[freestyle(body)]);
    }
}

#[test]
fn a_body_that_looks_denied_but_is_not_canonical_is_refused() {
    // The deny list belongs to the operation layer; the boundary still never lets the caller's
    // own spelling of a sensitive read through.
    for body in [
        "select * from usr02",
        "SELECT * FROM USR02 ",
        "SELECT * FROM  USR02",
        "SELECT * FROM USR02\n",
        "SELECT * FROM T001 AS A INNER JOIN usr02 AS B ON A~BUKRS = B~BNAME",
        "SELECT BNAME FROM USR02 \"",
        "SELECT * FROM USR02 UP TO 1 ROWS",
        "*\nSELECT * FROM USR02",
        "SELECT * FROM T001 WHERE BUKRS IN ( SELECT BNAME FROM USR02 )",
    ] {
        assert_refused(&[freestyle(body)]);
    }
}

#[test]
fn another_path_is_refused_even_with_a_valid_body() {
    for path in [
        "/sap/bc/adt/datapreview",
        "/sap/bc/adt/datapreview/",
        "/sap/bc/adt/datapreview/freestyle/",
        "/sap/bc/adt/datapreview/freestyle/x",
        "/sap/bc/adt/datapreview/freestyle;x",
        "/sap/bc/adt/datapreview/freestyl",
        "/sap/bc/adt/datapreview/Freestyle",
        "/sap/bc/adt/datapreview/FREESTYLE",
        "/sap/bc/adt/datapreview/free%73tyle",
        "/sap/bc/adt/datapreview/../datapreview/freestyle",
        "//sap/bc/adt/datapreview/freestyle",
        "/sap/bc/adt/datapreview/amdp",
        "/sap/bc/adt/datapreview/cds",
        "/sap/bc/adt/datapreview/ddic/freestyle",
        "/sap/bc/adt/datapreview/freestyle/ddic",
        "/sap/bc/adt/freestyle",
        "/sap/bc/adt/repository/datapreview/freestyle",
        "/sap/bc/adt/discovery",
    ] {
        assert_refused(&[valid(&format!("{path}?rowNumber=5"), JOIN)]);
    }

    for suffix in ["#x", "?rowNumber=5#x"] {
        assert_refused(&[valid(&format!("{FREESTYLE_URI}{suffix}"), JOIN)]);
    }
}

#[test]
fn the_two_preview_shapes_do_not_admit_each_others_requests() {
    // A freestyle body on the DDIC path, and a DDIC query on the freestyle path.
    assert_refused(&[
        valid(
            "/sap/bc/adt/datapreview/ddic?rowNumber=5&ddicEntityName=TADIR",
            JOIN,
        ),
        valid(
            &format!("{FREESTYLE_URI}?rowNumber=5&ddicEntityName=T001"),
            "SELECT * FROM T001",
        ),
    ]);

    let ddic = preview_request("T001", "SELECT * FROM T001", 5).unwrap();

    assert!(admits(&ddic));
}

#[test]
fn the_method_and_the_headers_must_match() {
    let uri = format!("{FREESTYLE_URI}?rowNumber=5");

    assert_refused(&[
        post(&uri, JOIN, None, Some("text/plain")),
        post(&uri, JOIN, Some("application/*"), None),
        post(&uri, JOIN, Some("text/html"), Some("text/plain")),
        post(&uri, JOIN, Some("application/*"), Some("text/xml")),
        post(
            &uri,
            JOIN,
            Some("application/*"),
            Some("text/plain; charset=utf-8"),
        ),
        post(&uri, JOIN, Some("*/*"), Some("text/plain")),
        Request::get(&uri),
    ]);

    let mut put = valid(&uri, JOIN);

    put.method = "PUT".to_owned();

    let mut delete = valid(&uri, JOIN);

    delete.method = "DELETE".to_owned();

    assert_refused(&[put, delete]);
}

#[test]
fn a_body_that_is_not_utf_8_is_refused() {
    let mut body = JOIN.as_bytes().to_vec();

    body.push(0xFF);

    let mut request = freestyle(JOIN);

    request.body = body;

    assert_refused(&[request]);
}
