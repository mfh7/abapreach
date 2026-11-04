//! Bounded write execution through the MCP tools: the base and the function group a token
//! binds, the confirmation summary, the tool deadline and its safe points, cancellation before
//! the operation and the scrub of everything a write reports. Fake client, fake transports.

mod common;
mod mcprig;
mod replay;

use abapreach::config::Connection;
use abapreach::mcp_write::{
    Limits, MAX_SUMMARY, Operation, Ticket, TokenGate, WriteGate, confirmation_summary,
};
use abapreach::operation::deploy::Change;
use abapreach::transport::{Response, TransportError};
use mcprig::*;
use replay::{Step, fx, no_inactive, replay};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;

const FIXTURE: &str = "dev-750-rfc.json";
const TOKEN: &str = r#"{"mcp_write":"token"}"#;
const CONFIRM: &str = r#"{"mcp_write":"confirm"}"#;
const CLEAN: &str = r#"<chkrun:checkReport xmlns:chkrun="http://www.sap.com/adt/checkrun" chkrun:status="processed"/>"#;

fn simulate_steps() -> Vec<Step> {
    vec![
        fx(6),
        no_inactive(),
        fx(7),
        fx(6),
        no_inactive(),
        fx(8),
        fx(9),
    ]
}

fn token_session() -> FakeSession {
    FakeSession::new(false)
}

fn simulated_token(rig: &mut Rig, session: &mut FakeSession) -> String {
    rig.adt(replay(FIXTURE, simulate_steps()));

    token_of(&rig.call(session, "abap_simulate", deploy_arguments()))
}

// ---- the base and the group the token binds --------------------------------------------------

#[test]
fn a_live_source_that_moved_on_since_the_simulation_stops_the_deploy_before_the_lock() {
    let mut rig = rig(TOKEN);
    let mut session = token_session();
    let requests = log();
    let token = simulated_token(&mut rig, &mut session);

    rig.adt(recording(
        replay(
            FIXTURE,
            vec![Step::ReplyTo(
                6,
                Ok(Response::text(
                    200,
                    "REPORT ytest_abapreach01.\n\nWRITE / 'changed by someone else'.",
                )),
            )],
        ),
        &requests,
    ));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(result.is_error);
    assert!(result.text.contains("live changed"), "{}", result.text);
    assert!(!any(&requests, "_action=LOCK"));
    assert!(!any(&requests, "PUT "));
}

fn func_search() -> String {
    abapreach::adt::function_group_search_uri("YTEST_ABAPREACH_FM")
}

fn search_reply(group: &str) -> String {
    format!(
        concat!(
            r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/{}/fmodules/YTEST_ABAPREACH_FM" "#,
            r#"adtcore:type="FUGR/FF" adtcore:name="YTEST_ABAPREACH_FM"/></adtcore:objectReferences>"#,
        ),
        group
    )
}

#[test]
fn a_function_module_that_moved_to_another_group_stops_the_deploy() {
    let mut rig = rig(TOKEN);
    let mut session = token_session();
    let requests = log();
    let fm = "/sap/bc/adt/functions/groups/ytest_abapreach_fg/fmodules/ytest_abapreach_fm";
    let live = "FUNCTION YTEST_ABAPREACH_FM\r\n  IMPORTING\r\n    VALUE(IV_IN) TYPE I.\r\n\r\n\r\n\r\n  WRITE / 'v1'.\r\nENDFUNCTION.";
    let source = "FUNCTION ytest_abapreach_fm\n  IMPORTING\n    VALUE(iv_in) TYPE i.\n\n  WRITE / 'v2'.\nENDFUNCTION.";
    let arguments = json!({"conn": "DEV_100", "type": "FUNC", "name": "YTEST_ABAPREACH_FM",
                           "source": source});
    let feed = r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom"><atom:entry><atom:author><atom:name>user</atom:name></atom:author><atom:id>00000</atom:id></atom:entry></atom:feed>"#;

    rig.adt(
        Script::new(&requests)
            .get(&func_search(), 200, &search_reply("YTEST_ABAPREACH_FG"))
            .get(&format!("{fm}/source/main?version=active"), 200, live)
            .get(&format!("{fm}/source/main/versions"), 200, feed)
            .get(
                "/sap/bc/adt/activation/inactiveobjects",
                200,
                r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#,
            )
            .post(
                &format!("{fm}?_action=LOCK&accessMode=MODIFY"),
                200,
                &lock_xml(),
            )
            .get(&format!("{fm}/source/main?version=active"), 200, live)
            .get(&format!("{fm}/source/main/versions"), 200, feed)
            .post("/sap/bc/adt/checkruns?reporters=abapCheckRun", 200, CLEAN)
            .post(
                &format!("{fm}?_action=UNLOCK&lockHandle={ENCODED}"),
                200,
                "",
            ),
    );

    let token = token_of(&rig.call(&mut session, "abap_simulate", arguments.clone()));

    // The function group of the module changed between the simulation and the write.
    rig.adt(Script::new(&requests).get(&func_search(), 200, &search_reply("ZOTHER_GROUP")));

    let result = rig.call(&mut session, "abap_deploy", with_token(arguments, &token));

    assert!(result.is_error);
    assert!(
        result
            .text
            .to_ascii_uppercase()
            .contains("YTEST_ABAPREACH_FG")
            || result.text.to_ascii_uppercase().contains("ZOTHER_GROUP"),
        "{}",
        result.text
    );
    assert!(!any(&requests, "PUT "));
    assert_eq!(
        requests
            .borrow()
            .iter()
            .filter(|line| line.contains("_action=LOCK"))
            .count(),
        1,
        "only the simulation locked"
    );
}

// ---- the confirmation summary ----------------------------------------------------------------

fn connection() -> Connection {
    Connection::test_insecure()
}

fn deploy_ticket(change: Change) -> Ticket {
    Ticket {
        change: Some(change),
        backend: Some(abapreach::backend::BackendKind::AdtHttp),
        ..Ticket::new(
            Operation::Deploy,
            "DEV_100",
            BTreeMap::from([
                ("type".to_owned(), "PROG".to_owned()),
                ("name".to_owned(), "ZP".to_owned()),
            ]),
        )
    }
}

#[test]
fn an_excerpt_deploy_is_shown_as_a_large_deletion() {
    let mut rig = rig(CONFIRM);
    // The human declines: only the form matters here.
    let mut session = FakeSession::new(true);
    let excerpt_only = "REPORT ytest_abapreach01.";

    // A deploy built from a partial excerpt of the three-line program.
    rig.adt(replay(
        FIXTURE,
        vec![
            fx(6),
            no_inactive(),
            Step::Any(Ok(Response::text(200, lock_xml()))),
            fx(6),
            no_inactive(),
            Step::Any(Ok(Response::text(200, CLEAN))),
            Step::Any(Ok(Response::empty(200))),
        ],
    ));

    let mut arguments = deploy_arguments();

    arguments["source"] = json!(excerpt_only);

    let token = token_of(&rig.call(&mut session, "abap_simulate", arguments.clone()));

    rig.call(&mut session, "abap_deploy", with_token(arguments, &token));

    let message = &session.forms[0].0.message;

    assert!(message.contains("+0 -2"), "{message}");
    assert!(message.contains("removes 2 of 3 live lines"), "{message}");
}

/// Every Unicode format character (Cf) the summary must drop, as inclusive ranges.
const INVISIBLE: [(u32, u32); 21] = [
    (0x00ad, 0x00ad),
    (0x0600, 0x0605),
    (0x061c, 0x061c),
    (0x06dd, 0x06dd),
    (0x070f, 0x070f),
    (0x180e, 0x180e),
    (0x200b, 0x200f),
    (0x2028, 0x2029),
    (0x202a, 0x202e),
    (0x2060, 0x2064),
    (0x2066, 0x206f),
    (0xfeff, 0xfeff),
    (0xfff9, 0xfffb),
    (0x110bd, 0x110bd),
    (0x110cd, 0x110cd),
    (0x13430, 0x1343f),
    (0x1bca0, 0x1bca3),
    (0x1d173, 0x1d17a),
    (0xe0001, 0xe0001),
    (0xe0020, 0xe007f),
    (0x0890, 0x0891),
];

#[test]
fn the_summary_drops_every_format_character_at_each_range_boundary() {
    for (first, last) in INVISIBLE {
        for code in [first, last, first + (last - first) / 2] {
            let hidden = char::from_u32(code).unwrap();
            let change = Change {
                added: 1,
                removed: 1,
                live_lines: 2,
                excerpt: vec![format!("+a{hidden}b")],
                omitted: 0,
            };
            let summary = confirmation_summary(&connection(), &deploy_ticket(change));

            assert!(summary.contains("+ab"), "U+{code:04X} kept: {summary:?}");
        }
    }
}

#[test]
fn the_summary_is_bounded_and_strips_control_characters_from_sap_strings() {
    let hostile = format!(
        "-line\u{1b}[31m red\r\nsecond\u{0}\u{202a}\u{202e}\u{2066}\u{2069}\u{200b}\u{200f}\u{feff}\u{2028}\u{2029}{}",
        "x".repeat(2000)
    );
    let change = Change {
        added: 500,
        removed: 400,
        live_lines: 900,
        excerpt: std::iter::repeat_n(hostile.clone(), 200).collect(),
        omitted: 1234,
    };
    let summary = confirmation_summary(&connection(), &deploy_ticket(change));

    assert!(
        summary.chars().count() <= MAX_SUMMARY + 80,
        "{}",
        summary.len()
    );
    assert!(
        summary.chars().all(|character| {
            character == '\n'
                || (!character.is_control()
                    && !matches!(
                        character,
                        '\u{200b}'..='\u{200f}'
                            | '\u{202a}'..='\u{202e}'
                            | '\u{2060}'..='\u{2069}'
                            | '\u{feff}'
                            | '\u{2028}'
                            | '\u{2029}'
                    ))
        }),
        "{summary:?}"
    );
    // The 1234 the excerpt left out and the 170 of its lines the summary does not show.
    assert!(summary.contains("1404 more diff lines"), "{summary}");
    assert!(summary.contains("truncated"), "{summary}");
}

#[test]
fn a_delete_summary_lists_every_object_and_the_contents_of_a_group() {
    let mut ticket = Ticket::new(
        Operation::Delete,
        "DEV_100",
        BTreeMap::from([("objects".to_owned(), "FUGR:ZFG\nPROG:ZP".to_owned())]),
    );

    ticket.listing = vec![
        "FUGR:ZFG".to_owned(),
        "  CONTENT FUNC ZF1".to_owned(),
        "  CONTENT FUNC ZF2".to_owned(),
        "PROG:ZP".to_owned(),
    ];

    let summary = confirmation_summary(&connection(), &ticket);

    for needle in [
        "DEV_100",
        "SID DEV",
        "client 100",
        "FUGR:ZFG",
        "CONTENT FUNC ZF2",
        "PROG:ZP",
    ] {
        assert!(summary.contains(needle), "{needle} in {summary}");
    }
}

// ---- deadline and cancellation ---------------------------------------------------------------

#[test]
fn a_passed_deadline_sends_nothing_for_any_write_tool() {
    let mut rig = rig(TOKEN);
    let mut session = token_session();

    rig.limits = Limits {
        call: Duration::ZERO,
        ..Limits::default()
    };

    for (tool, arguments) in [
        ("abap_simulate", deploy_arguments()),
        (
            "abap_create_simulate",
            json!({"conn": "DEV_100", "type": "FUGR", "name": "ZTESTFG", "text": "T",
                   "package": "$TMP"}),
        ),
        (
            "abap_delete_simulate",
            json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]}),
        ),
    ] {
        let result = rig.call(&mut session, tool, arguments);

        assert!(result.is_error, "{tool}");
        assert!(
            result.text.contains("time limit"),
            "{tool}: {}",
            result.text
        );
    }

    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn a_delete_batch_stops_between_objects_at_the_deadline() {
    let mut rig = rig(TOKEN);
    let requests = log();
    let other = "/sap/bc/adt/programs/programs/zother";
    let reads = |script: Script| {
        script
            .get(other, 200, &metadata("OWNER"))
            .get(
                &format!("{other}/source/main?version=active"),
                200,
                "REPORT zother.",
            )
            .get(
                &format!("{other}/source/main/versions"),
                200,
                "<feed xmlns=\"http://www.w3.org/2005/Atom\"/>",
            )
    };
    let delete = json!({"conn": "DEV_100", "objects": ["PROG:ZPROG", "PROG:ZOTHER"]});
    let mut session = FakeSession::typing("DELETE 2");

    rig.adt(reads(prog_reads(&requests, "OWNER")));

    let token = token_of(&rig.call(&mut session, "abap_delete_simulate", delete.clone()));

    rig.limits = Limits {
        call: Duration::from_millis(250),
        ..Limits::default()
    };
    // The first delete outlasts the deadline; the request in flight is never interrupted.
    rig.adt(reads(
        prog_reads(&requests, "OWNER")
            .post(
                &format!("{PROG}?_action=LOCK&accessMode=MODIFY"),
                200,
                &lock_xml(),
            )
            .on(
                "DELETE",
                &format!("{PROG}?lockHandle={ENCODED}"),
                Ok(Response::empty(200)),
            )
            .hook(|request| {
                if request.method == "DELETE" {
                    std::thread::sleep(Duration::from_millis(400));
                }
            }),
    ));
    rig.adt(Script::new(&requests).get(PROG, 404, ""));

    let result = rig.call(&mut session, "abap_delete", with_token(delete, &token));
    let statuses: Vec<(String, String)> = result.structured["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| {
            (
                object["name"].as_str().unwrap().to_owned(),
                object["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();

    assert_eq!(
        statuses,
        [
            ("ZPROG".to_owned(), "DELETED".to_owned()),
            ("ZOTHER".to_owned(), "SKIPPED".to_owned())
        ],
        "{}",
        result.text
    );
    assert!(result.is_error, "a skipped object makes the batch exit 1");
}

#[test]
fn a_cancel_before_the_operation_starts_sends_nothing() {
    let mut rig = rig(TOKEN);
    let mut session = token_session();
    let token = simulated_token(&mut rig, &mut session);
    let opened = rig.opened();

    session.cancel.set();

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(result.is_error);
    assert!(result.text.contains("CANCELED"), "{}", result.text);
    assert_eq!(rig.opened(), opened);
}

// ---- scrub -----------------------------------------------------------------------------------

#[test]
fn an_error_that_echoes_the_password_the_host_and_the_route_reaches_nobody() {
    let mut rig = rig(TOKEN);
    let mut session = token_session();
    let leak =
        format!("cannot reach {HOST} via /H/router.example.net/S/3299/H/ as user with {PASSWORD}");

    rig.fake
        .adt
        .push_back(Err(TransportError::PreSendConnectivity(leak.clone())));
    rig.fake
        .rfc
        .push_back(Err(TransportError::PreSendConnectivity(leak)));

    let result = rig.call(&mut session, "abap_simulate", deploy_arguments());

    assert!(result.is_error);

    let mut everything = vec![result.text.clone(), result.structured.to_string()];

    everything.extend(session.logs.clone());

    for text in everything {
        for secret in [PASSWORD, HOST, "router.example.net", "/H/router"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
}

#[test]
fn the_lock_handle_never_leaves_the_process_not_even_inside_a_failure_of_the_unlock() {
    let mut rig = rig(TOKEN);
    let mut session = token_session();
    let token = simulated_token(&mut rig, &mut session);
    let echo = |status| {
        Step::Any(Err(TransportError::Http {
            status,
            message: format!("held by another session, handle {HANDLE} / {ENCODED}"),
        }))
    };

    rig.adt(replay(
        FIXTURE,
        vec![
            fx(6),
            no_inactive(),
            Step::Any(Ok(Response::text(200, lock_xml()))),
            fx(6),
            no_inactive(),
            fx(8),
            Step::Any(Ok(Response::empty(200))),
            echo(423),
        ],
    ));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );
    let all = format!("{}{}", result.text, result.structured);

    assert!(
        all.contains("UNLOCK FAILED"),
        "the failure is still reported: {all}"
    );
    assert!(!all.contains(HANDLE), "{all}");
    assert!(!all.contains(ENCODED), "{all}");
    assert!(session.logs.iter().all(|line| !line.contains(HANDLE)));
}

#[test]
fn a_simulation_whose_diff_holds_a_configured_secret_is_rejected_and_issues_no_token() {
    let mut rig = rig(TOKEN);
    let mut session = token_session();
    let mut arguments = deploy_arguments();

    arguments["source"] = json!(format!("{PROGRAM}\nWRITE / '{PASSWORD}'."));
    rig.adt(replay(
        FIXTURE,
        vec![
            fx(6),
            no_inactive(),
            Step::Any(Ok(Response::text(200, lock_xml()))),
            fx(6),
            no_inactive(),
            Step::Any(Ok(Response::text(200, CLEAN))),
            Step::Any(Ok(Response::empty(200))),
        ],
    ));

    let result = rig.call(&mut session, "abap_simulate", arguments);

    assert!(result.is_error);
    assert!(result.text.contains("payload contains a configured secret"));
    assert!(!result.text.contains(PASSWORD));
    assert!(!result.structured.to_string().contains(PASSWORD));
    assert!(result.structured.get("plan_token").is_none());
}

#[test]
fn a_gate_built_without_a_clock_uses_the_real_one() {
    let mut gate = TokenGate::new();
    let ticket = Ticket::new(Operation::Delete, "DEV_100", BTreeMap::new());
    let token = gate.issue(ticket.clone()).unwrap();

    assert!(gate.admit(&token, &ticket).is_ok());
}
