//! The check of a request named by `--tr` for an object that no open request holds: the same
//! rules the classrun runner applies, read from `E070` over the data preview. Scripted
//! transports only; no SAP.

mod common;

use abapreach::operation::transport_request::check_open_request;
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, preview};

const TR: &str = "HD1K917833";
const USER: &str = "HR_ABAP";

type Reply = Result<Response, TransportError>;

fn request_row(strkorr: &str, function: &str, status: &str, owner: &str) -> Reply {
    Ok(Response::text(
        200,
        preview(&[
            ("TRKORR", &[Some(TR)]),
            ("STRKORR", &[Some(strkorr).filter(|text| !text.is_empty())]),
            ("TRFUNCTION", &[Some(function)]),
            ("TRSTATUS", &[Some(status)]),
            ("AS4USER", &[Some(owner)]),
        ]),
    ))
}

fn no_request() -> Reply {
    let none: &[Option<&str>] = &[];

    Ok(Response::text(
        200,
        preview(&[
            ("TRKORR", none),
            ("STRKORR", none),
            ("TRFUNCTION", none),
            ("TRSTATUS", none),
            ("AS4USER", none),
        ]),
    ))
}

fn task(found: bool) -> Reply {
    let rows: &[Option<&str>] = if found { &[Some("HD1K917834")] } else { &[] };

    Ok(Response::text(200, preview(&[("TRKORR", rows)])))
}

fn check(replies: Vec<Reply>) -> (Result<(), String>, ScriptedTransport) {
    let mut transport = ScriptedTransport::replies(replies);
    let result = check_open_request(&mut transport, TR, USER);

    (result, transport)
}

fn bodies(transport: &ScriptedTransport) -> Vec<String> {
    transport
        .calls
        .iter()
        .map(|call| String::from_utf8_lossy(&call.body).into_owned())
        .collect()
}

#[test]
fn an_own_modifiable_workbench_request_passes_with_one_read() {
    let (result, transport) = check(vec![request_row("", "K", "D", USER)]);

    assert_eq!(result, Ok(()));
    assert_eq!(
        bodies(&transport),
        [format!(
            "SELECT TRKORR, STRKORR, TRFUNCTION, TRSTATUS, AS4USER FROM E070 WHERE TRKORR IN ( '{TR}' )"
        )]
    );
    assert!(transport.calls[0].uri.ends_with("ddicEntityName=E070"));
    transport.assert_finished();
}

#[test]
fn the_owner_is_compared_without_regard_to_case() {
    let (result, _) = check(vec![request_row("", "K", "D", "hr_abap")]);

    assert_eq!(result, Ok(()));
}

#[test]
fn a_request_of_another_user_passes_only_with_a_modifiable_task_of_the_user() {
    let (result, transport) = check(vec![request_row("", "K", "D", "OTHER"), task(true)]);

    assert_eq!(result, Ok(()));
    assert_eq!(
        bodies(&transport)[1],
        format!(
            "SELECT TRKORR FROM E070 WHERE STRKORR IN ( '{TR}' ) AND AS4USER IN ( '{USER}' ) AND TRSTATUS IN ( 'D' )"
        )
    );

    let (result, _) = check(vec![request_row("", "K", "D", "OTHER"), task(false)]);

    assert_eq!(
        result,
        Err(format!(
            "request {TR} is not owned by {USER} and holds no task of {USER}"
        ))
    );
}

#[test]
fn every_other_rule_refuses_and_names_itself() {
    let cases = [
        (no_request(), format!("request {TR} does not exist")),
        (
            request_row("HD1K917800", "K", "D", USER),
            format!("{TR} is not a workbench request (a task or another type is not accepted)"),
        ),
        (
            request_row("", "W", "D", USER),
            format!("{TR} is not a workbench request (a task or another type is not accepted)"),
        ),
        (
            request_row("", "K", "R", USER),
            format!("request {TR} is not modifiable (TRSTATUS=R)"),
        ),
        (
            request_row("", "K", "L", USER),
            format!("request {TR} is not modifiable (TRSTATUS=L)"),
        ),
    ];

    for (reply, expected) in cases {
        let (result, transport) = check(vec![reply]);

        assert_eq!(result, Err(expected));
        // A refusal after the first read needs no second one.
        assert_eq!(transport.calls.len(), 1);
    }
}

#[test]
fn a_row_of_another_request_is_not_trusted() {
    let other = Ok(Response::text(
        200,
        preview(&[
            ("TRKORR", &[Some("HD1K000001")]),
            ("STRKORR", &[None]),
            ("TRFUNCTION", &[Some("K")]),
            ("TRSTATUS", &[Some("D")]),
            ("AS4USER", &[Some(USER)]),
        ]),
    ));
    let (result, _) = check(vec![other]);

    assert_eq!(result, Err(format!("request {TR} does not exist")));
}

#[test]
fn a_read_that_fails_refuses_and_writes_nothing() {
    let (result, _) = check(vec![Ok(Response::text(500, ""))]);

    assert!(
        result
            .as_ref()
            .is_err_and(|text| text.starts_with(&format!("cannot read request {TR}: "))),
        "{result:?}"
    );

    let (result, _) = check(vec![
        request_row("", "K", "D", "OTHER"),
        Ok(Response::text(500, "")),
    ]);

    assert!(
        result
            .as_ref()
            .is_err_and(|text| text.starts_with(&format!("cannot read request {TR}: "))),
        "{result:?}"
    );
}

#[test]
fn a_request_or_user_that_could_carry_sql_sends_nothing() {
    for (request, user) in [
        ("HD1K9'", USER),
        ("", USER),
        ("HD1K 1", USER),
        (TR, "A'B"),
        (TR, ""),
        (TR, "A B"),
        (TR, "ABCDEFGHIJKLM"),
    ] {
        let mut transport = ScriptedTransport::replies(Vec::new());

        assert!(check_open_request(&mut transport, request, user).is_err());
        assert!(transport.calls.is_empty());
    }
}
