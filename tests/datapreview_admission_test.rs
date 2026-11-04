//! The request boundary for the read-table data preview: exactly the evidenced POST, nothing
//! else under `/datapreview`.

use abapreach::operation::read_table::preview_request;
use abapreach::transport::{
    Request, SadtTransport, TransportError,
    rfc::{FakeRfcApi, RfcTransport},
};

const BASE: &str = "/sap/bc/adt/datapreview/ddic";
const BODY: &str = "SELECT BUKRS, BUTXT FROM T001 WHERE BUKRS <> '0001'";

fn admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

fn preview(uri: &str, body: &str, accept: Option<&str>, content_type: Option<&str>) -> Request {
    let mut request = Request::post(uri, body.as_bytes().to_vec());

    for (name, value) in [("Accept", accept), ("Content-Type", content_type)] {
        if let Some(value) = value {
            request.headers.insert(name.to_owned(), value.to_owned());
        }
    }

    request
}

fn valid(uri: &str, body: &str) -> Request {
    preview(uri, body, Some("application/*"), Some("text/plain"))
}

#[track_caller]
fn assert_refused(requests: &[Request]) {
    for request in requests {
        assert!(!admits(request), "must be refused: {request:?}");
    }
}

#[test]
fn the_request_the_operation_builds_is_admitted() {
    for (entity, body, rows) in [
        ("T001", BODY, 1),
        ("T001", "SELECT * FROM T001", 5000),
        ("/BIC/AFOO", "SELECT * FROM /BIC/AFOO", 17),
        ("DD02L", "SELECT * FROM DD02L WHERE TABNAME = 'T001'", 1),
    ] {
        let request = preview_request(entity, body, rows).unwrap();

        assert!(admits(&request), "must be admitted: {request:?}");
    }
}

#[test]
fn the_exact_shape_is_admitted() {
    assert!(admits(&valid(
        &format!("{BASE}?rowNumber=2&ddicEntityName=T001"),
        BODY
    )));
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
        assert_refused(&[valid(
            &format!("{BASE}?rowNumber={rows}&ddicEntityName=T001"),
            BODY,
        )]);
    }
}

#[test]
fn an_entity_that_is_not_a_validated_name_is_refused() {
    for entity in [
        "",
        "T001%20X",
        "T001%2",
        "T001+X",
        "T001~X",
        "T001;X",
        "T001%00",
        "%2e%2e",
        "%2FNS",
        "NS%2F",
        "%2F%2FX",
        "%2FNS%2FA%2FB",
        "/BIC/AFOO",
        "%2fbic%2fafoo",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ_01234567",
        "%2FABCDEFGHIJKLMNO%2FPQRSTUVWXYZ_012",
    ] {
        assert_refused(&[valid(
            &format!("{BASE}?rowNumber=1&ddicEntityName={entity}"),
            &format!("SELECT * FROM {entity}"),
        )]);
    }
}

#[test]
fn a_query_other_than_the_two_keys_in_order_is_refused() {
    let entity = "ddicEntityName=T001";

    assert_refused(&[
        valid(BASE, BODY),
        valid(&format!("{BASE}?"), BODY),
        valid(&format!("{BASE}?rowNumber=1"), BODY),
        valid(&format!("{BASE}?{entity}"), BODY),
        valid(&format!("{BASE}?{entity}&rowNumber=1"), BODY),
        valid(&format!("{BASE}?rowNumber=1&{entity}&x=1"), BODY),
        valid(&format!("{BASE}?rowNumber=1&rowNumber=2&{entity}"), BODY),
        valid(&format!("{BASE}?rowNumber=1&{entity}&{entity}"), BODY),
        valid(&format!("{BASE}?rowNumber=1&{entity}&"), BODY),
        valid(
            &format!("{BASE}?rowNumber=1&ddicEntityName=T001&sql=x"),
            BODY,
        ),
        valid(&format!("{BASE}?rowNumber=1&{entity}#frag"), BODY),
        valid(&format!("{BASE}?rowNumber=1&ddicentityname=T001"), BODY),
    ]);
}

#[test]
fn nothing_else_under_datapreview_opens() {
    let query = "rowNumber=1&ddicEntityName=T001";

    assert_refused(&[
        valid(&format!("/sap/bc/adt/datapreview/freestyle?{query}"), BODY),
        valid("/sap/bc/adt/datapreview/freestyle", BODY),
        valid(&format!("/sap/bc/adt/datapreview?{query}"), BODY),
        valid(&format!("/sap/bc/adt/datapreview/?{query}"), BODY),
        valid(&format!("{BASE}/?{query}"), BODY),
        valid(&format!("{BASE}/x?{query}"), BODY),
        valid(&format!("{BASE}x?{query}"), BODY),
        valid(
            &format!("/sap/bc/adt/datapreview/ddic/../freestyle?{query}"),
            BODY,
        ),
        valid(&format!("/sap/bc/adt/datapreview/DDIC?{query}"), BODY),
        valid(&format!("//sap/bc/adt/datapreview/ddic?{query}"), BODY),
        valid(&format!("/sap/bc/adt/datapreview/vm?{query}"), BODY),
    ]);
}

#[test]
fn only_a_post_is_admitted() {
    let uri = format!("{BASE}?rowNumber=1&ddicEntityName=T001");

    for method in ["GET", "PUT", "DELETE", "post", "PATCH", "HEAD"] {
        let mut request = valid(&uri, BODY);

        request.method = method.to_owned();
        assert_refused(&[request]);
    }
}

#[test]
fn the_headers_are_exact() {
    let uri = format!("{BASE}?rowNumber=1&ddicEntityName=T001");

    assert_refused(&[
        preview(&uri, BODY, Some("application/*"), None),
        preview(&uri, BODY, None, Some("text/plain")),
        preview(&uri, BODY, None, None),
        preview(&uri, BODY, Some("application/xml"), Some("text/plain")),
        preview(&uri, BODY, Some("*/*"), Some("text/plain")),
        preview(&uri, BODY, Some("application/*"), Some("application/xml")),
        preview(
            &uri,
            BODY,
            Some("application/*"),
            Some("text/plain; charset=utf-8"),
        ),
        preview(&uri, BODY, Some("application/*"), Some("application/*")),
    ]);
}

#[test]
fn the_body_is_one_select_line_that_reads_the_entity() {
    let uri = |entity: &str| format!("{BASE}?rowNumber=1&ddicEntityName={entity}");

    assert_refused(&[
        valid(&uri("T001"), ""),
        valid(&uri("T001"), "DELETE FROM T001"),
        valid(&uri("T001"), "select * from T001"),
        valid(&uri("T001"), "SELECT * FROM T001\nSELECT * FROM USR02"),
        valid(&uri("T001"), "SELECT * FROM T001\r\n"),
        valid(&uri("T001"), "SELECT * FROM T001\n"),
        valid(&uri("T001"), "SELECT *"),
        valid(&uri("T001"), "SELECT * FROM USR02"),
        valid(&uri("T001"), "SELECT * FROM T001X"),
        valid(&uri("T001"), "SELECT * FROM T001; SELECT * FROM USR02"),
        valid(&uri("T001"), "SELECT * FROM T001,USR02"),
        valid(&uri("T001"), "SELECT * FROM T001 JOIN USR02"),
        valid(&uri("%2FBIC%2FAFOO"), "SELECT * FROM /BIC/AFOOX"),
    ]);

    let mut not_utf8 = valid(&uri("T001"), "");

    not_utf8.body = b"SELECT * FROM T001 WHERE X = '\xff'".to_vec();
    assert_refused(&[not_utf8]);
}
