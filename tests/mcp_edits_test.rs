//! Deploy by `edits` + `base` through `abap_simulate` and `abap_deploy`: the argument rules, the
//! token binding, the checks on the result, the stored result hash and the confirmation form.
//! Fake client, fake transports, no SAP.

mod common;
mod mcprig;
mod replay;

use abapreach::edit::{Edit, edits_sha256};
use abapreach::hash::source_hash;
use abapreach::mcp_write::{Operation, Ticket, WriteGate};
use mcprig::*;
use replay::{Step, fx, no_inactive, replay};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FIXTURE: &str = "dev-750-rfc.json";
const TOKEN: &str = r#"{"mcp_write":"token"}"#;
const CONFIRM: &str = r#"{"mcp_write":"confirm"}"#;
const NAME: &str = "YTEST_ABAPREACH01";

/// The live source of the m1 fixture exchange 6, as `abap_read_source` shows it.
fn live_lines() -> Vec<String> {
    ["REPORT ytest_abapreach01.", "", "WRITE / 'v1'."]
        .map(str::to_owned)
        .to_vec()
}

fn base() -> String {
    source_hash(&live_lines())
}

/// The one edit that turns the fixture's v1 into the v2 of `PROGRAM`.
fn v2_edits() -> Value {
    json!([{"old": "'v1'", "new": "'v2'"}])
}

fn edit_arguments(edits: Value) -> Value {
    json!({"conn": "DEV_100", "type": "PROG", "name": NAME, "edits": edits, "base": base()})
}

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

fn execute_steps() -> Vec<Step> {
    vec![
        fx(6),
        no_inactive(),
        fx(7),
        fx(6),
        no_inactive(),
        fx(8),
        fx(17),
        fx(18),
        fx(19),
        fx(20),
    ]
}

/// What `abap_deploy` presents for `v2_edits`: the bound fields of a deploy by edits.
fn presented(edits: &[Edit]) -> Ticket {
    Ticket::new(
        Operation::Deploy,
        "DEV_100",
        BTreeMap::from([
            ("type".to_owned(), "PROG".to_owned()),
            ("name".to_owned(), NAME.to_owned()),
            ("base".to_owned(), base()),
            ("edits_sha256".to_owned(), edits_sha256(edits)),
            ("create_include".to_owned(), "false".to_owned()),
        ]),
    )
}

fn v2_edit_list() -> Vec<Edit> {
    vec![Edit {
        old: "'v1'".to_owned(),
        new: "'v2'".to_owned(),
    }]
}

// ---- the argument rules, refused before SAP is touched ---------------------------------------

#[test]
fn both_source_and_edits_are_refused_before_sap() {
    let mut rig = rig(TOKEN);
    let mut arguments = edit_arguments(v2_edits());

    arguments["source"] = json!("REPORT x.");

    let result = rig.bare("abap_simulate", arguments);

    assert!(result.is_error);
    assert!(
        result.text.contains("exactly one of source or edits"),
        "{}",
        result.text
    );
    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn neither_source_nor_edits_is_refused_before_sap() {
    let mut rig = rig(TOKEN);
    let result = rig.bare(
        "abap_simulate",
        json!({"conn": "DEV_100", "type": "PROG", "name": NAME}),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("source or edits is required"),
        "{}",
        result.text
    );
    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn edits_without_a_base_are_refused_before_sap() {
    let mut rig = rig(TOKEN);
    let mut arguments = edit_arguments(v2_edits());

    arguments.as_object_mut().unwrap().remove("base");

    let result = rig.bare("abap_simulate", arguments);

    assert!(result.is_error);
    assert!(
        result.text.contains("base is required with edits"),
        "{}",
        result.text
    );
    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn a_base_with_a_full_source_is_refused() {
    let mut rig = rig(TOKEN);
    let mut arguments = deploy_arguments();

    arguments["base"] = json!(base());

    let result = rig.bare("abap_simulate", arguments);

    assert!(result.is_error);
    assert!(
        result.text.contains("base only applies to edits"),
        "{}",
        result.text
    );
    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn a_malformed_edit_is_refused_before_sap() {
    let mut rig = rig(TOKEN);

    for (edits, message) in [
        (json!([]), "non-empty array"),
        (json!([{"old": "a"}]), "edit 1: new must be a string"),
        (
            json!([{"old": "a", "new": "b", "extra": "c"}]),
            "edit 1: unknown key extra",
        ),
    ] {
        let result = rig.bare("abap_simulate", edit_arguments(edits));

        assert!(result.is_error);
        assert!(result.text.contains(message), "{}", result.text);
    }

    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn edits_are_refused_with_create_include() {
    let mut rig = rig(TOKEN);
    let result = rig.bare(
        "abap_simulate",
        json!({"conn": "DEV_100", "type": "CLAS", "name": "ZCL_A", "include": "CCIMP",
               "create_include": true, "edits": v2_edits(), "base": base()}),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("edits do not apply to create_include"),
        "{}",
        result.text
    );
    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn ddic_and_transaction_types_refuse_edits() {
    let mut rig = rig(TOKEN);

    for kind in ["DTEL", "DOMA", "TABL", "TTYP", "TRAN"] {
        for tool in ["abap_simulate", "abap_deploy"] {
            let mut arguments = json!({"conn": "DEV_100", "type": kind, "name": "YTEST",
                                       "edits": v2_edits(), "base": base()});

            if tool == "abap_deploy" {
                arguments["plan_token"] = json!("tok");
            }

            let result = rig.bare(tool, arguments);

            assert!(result.is_error, "{kind} {tool}");
            assert!(
                result.text.contains("does not apply to"),
                "{kind} {tool}: {}",
                result.text
            );
        }
    }

    assert_eq!(rig.opened(), (0, 0));
}

// ---- the simulation ---------------------------------------------------------------------------

#[test]
fn a_simulation_by_edits_shows_the_diff_and_binds_base_and_edits_not_the_source() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let result = rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits()));
    let token = token_of(&result);

    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("-WRITE / 'v1'."), "{}", result.text);
    assert!(result.text.contains("+WRITE / 'v2'."), "{}", result.text);

    // The ticket binds `base` and `edits_sha256` where a full source binds `source_sha256`.
    let mut wrong = presented(&v2_edit_list());

    wrong
        .fields
        .insert("source_sha256".to_owned(), "x".repeat(64));

    let mut gate = rig.gate.clone();

    assert!(gate.admit(&token, &wrong).is_err());

    let stored = gate.admit(&token, &presented(&v2_edit_list())).unwrap();

    assert_eq!(stored.base.as_deref(), Some(base().as_str()));
    assert!(!stored.fields.contains_key("source_sha256"));
    // The result hash is the hash of the full source the edits reach, kept outside the fields.
    assert_eq!(
        stored.result_hash,
        Some(source_hash(&[
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            "WRITE / 'v2'.".to_owned()
        ]))
    );
}

#[test]
fn a_stale_base_is_the_live_changed_stop_and_nothing_is_locked() {
    let mut rig = rig(TOKEN);
    let requests = log();
    let mut arguments = edit_arguments(v2_edits());

    arguments["base"] = json!("0".repeat(64));
    rig.adt(recording(replay(FIXTURE, vec![fx(6)]), &requests));

    let result = rig.bare("abap_simulate", arguments);

    assert!(result.is_error);
    assert!(result.text.contains("live changed"), "{}", result.text);
    assert!(!any(&requests, "_action=LOCK"));
    assert!(result.structured.get("plan_token").is_none());
}

#[test]
fn an_edit_that_is_not_found_stops_with_its_number_and_nothing_is_locked() {
    let mut rig = rig(TOKEN);
    let requests = log();

    rig.adt(recording(replay(FIXTURE, vec![fx(6)]), &requests));

    let result = rig.bare(
        "abap_simulate",
        edit_arguments(json!([
            {"old": "'v1'", "new": "'v2'"},
            {"old": "no such text", "new": "x"}
        ])),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("STOP: edit 2: old not found"),
        "{}",
        result.text
    );
    assert!(!any(&requests, "_action=LOCK"));
}

#[test]
fn an_old_that_matches_twice_stops_and_asks_for_context() {
    let mut rig = rig(TOKEN);

    rig.adt(replay(FIXTURE, vec![fx(6)]));

    // Both statements of the program end with a period.
    let result = rig.bare(
        "abap_simulate",
        edit_arguments(json!([{"old": ".", "new": "!"}])),
    );

    assert!(result.is_error);
    assert!(
        result
            .text
            .contains("STOP: edit 1: old matches 2 times; add context"),
        "{}",
        result.text
    );
}

#[test]
fn an_empty_old_stops_the_simulation() {
    let mut rig = rig(TOKEN);

    rig.adt(replay(FIXTURE, vec![fx(6)]));

    let result = rig.bare(
        "abap_simulate",
        edit_arguments(json!([{"old": "", "new": "x"}])),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("STOP: edit 1: old is empty"),
        "{}",
        result.text
    );
}

#[test]
fn a_line_over_255_after_the_edit_stops_before_the_lock() {
    let mut rig = rig(TOKEN);
    let requests = log();

    rig.adt(recording(replay(FIXTURE, vec![fx(6)]), &requests));

    let long = "x".repeat(300);
    let result = rig.bare(
        "abap_simulate",
        edit_arguments(json!([{"old": "'v1'", "new": long}])),
    );

    assert!(result.is_error);
    assert!(result.text.contains("length 309 > 255"), "{}", result.text);
    assert!(!any(&requests, "_action=LOCK"));
}

#[test]
fn an_edit_result_equal_to_live_is_no_changes_and_issues_no_token() {
    let mut rig = rig(TOKEN);

    rig.adt(replay(FIXTURE, vec![fx(6)]));

    let result = rig.bare(
        "abap_simulate",
        edit_arguments(json!([{"old": "'v1'", "new": "'v1'"}])),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("no changes"), "{}", result.text);
    assert!(result.structured.get("plan_token").is_none());
}

#[test]
fn trailing_blanks_and_cr_in_old_and_new_are_cut_before_matching() {
    let mut rig = rig(TOKEN);

    rig.adt(replay(FIXTURE, simulate_steps()));

    // CRLF and a trailing blank in `old` match the normalized live lines.
    let result = rig.bare(
        "abap_simulate",
        edit_arguments(json!([
            {"old": "REPORT ytest_abapreach01. \r\n\r\nWRITE / 'v1'.",
             "new": "REPORT ytest_abapreach01.  \r\n\r\nWRITE / 'v2'."}
        ])),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("+WRITE / 'v2'."), "{}", result.text);
}

#[test]
fn an_edit_that_breaks_the_function_format_is_refused_by_the_checks_on_the_result() {
    let mut rig = rig(TOKEN);
    let requests = log();
    let fm = "/sap/bc/adt/functions/groups/ytest_abapreach_fg/fmodules/ytest_abapreach_fm";
    let live = "FUNCTION YTEST_ABAPREACH_FM\r\n  IMPORTING\r\n    VALUE(IV_IN) TYPE I.\r\n\r\n  WRITE / 'v1'.\r\nENDFUNCTION.";
    let reply = concat!(
        r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
        r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/ytest_abapreach_fg/fmodules/ytest_abapreach_fm" "#,
        r#"adtcore:type="FUGR/FF" adtcore:name="YTEST_ABAPREACH_FM"/></adtcore:objectReferences>"#,
    );
    let lines: Vec<String> = live.split("\r\n").map(str::to_owned).collect();
    let base = source_hash(&lines);

    for (old, new, message) in [
        // The ADT format has no `*"` header.
        ("  WRITE / 'v1'.", "*\" header", "starts with *\""),
        // A source that does not open with FUNCTION has no readable signature.
        (
            "FUNCTION YTEST_ABAPREACH_FM",
            "FUNKTION YTEST_ABAPREACH_FM",
            "FUNC source signature unreadable",
        ),
    ] {
        rig.adt(
            Script::new(&requests)
                .get(
                    &abapreach::adt::function_group_search_uri("YTEST_ABAPREACH_FM"),
                    200,
                    reply,
                )
                .get(&format!("{fm}/source/main?version=active"), 200, live),
        );

        let result = rig.bare(
            "abap_simulate",
            json!({"conn": "DEV_100", "type": "FUNC", "name": "YTEST_ABAPREACH_FM",
                   "edits": [{"old": old, "new": new}], "base": base}),
        );

        assert!(result.is_error, "{message}: {}", result.text);
        assert!(result.text.contains(message), "{}", result.text);
        assert!(result.structured.get("plan_token").is_none());
    }

    assert!(!any(&requests, "_action=LOCK"));
}

// ---- the token binding ------------------------------------------------------------------------

#[test]
fn a_token_does_not_admit_other_edits_or_another_base() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));
    let opened = rig.opened();

    for arguments in [edit_arguments(json!([{"old": "'v1'", "new": "'v3'"}])), {
        let mut other = edit_arguments(v2_edits());

        other["base"] = json!("1".repeat(64));
        other
    }] {
        let result = rig.call(&mut session, "abap_deploy", with_token(arguments, &token));

        assert!(result.is_error);
        assert!(result.text.contains("arguments differ"), "{}", result.text);
    }

    // Nothing was sent and the refused calls did not spend the token.
    assert_eq!(rig.opened(), opened);
    assert!(
        rig.gate
            .clone()
            .admit(&token, &presented(&v2_edit_list()))
            .is_ok()
    );
}

#[test]
fn a_token_of_edits_does_not_admit_a_full_source() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));
    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(result.is_error);
    assert!(result.text.contains("arguments differ"), "{}", result.text);
}

// ---- the execution ----------------------------------------------------------------------------

#[test]
fn the_deploy_by_edits_writes_the_full_source_the_edits_reach() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);
    let requests = log();

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));

    // The replay compares the PUT body with the recorded one: the v2 source of the full deploy.
    rig.adt(recording(replay(FIXTURE, execute_steps()), &requests));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(edit_arguments(v2_edits()), &token),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(any(&requests, "PUT "));
}

#[test]
fn an_execute_whose_result_hash_differs_from_the_simulation_stops_before_the_lock() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);
    let requests = log();

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));
    let mut gate = rig.gate.clone();
    let mut stored = gate.admit(&token, &presented(&v2_edit_list())).unwrap();

    // The simulation reached another result than these edits reach now.
    stored.result_hash = Some("2".repeat(64));

    let token = gate.issue(stored).unwrap();

    rig.adt(recording(replay(FIXTURE, vec![fx(6)]), &requests));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(edit_arguments(v2_edits()), &token),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("result changed since the simulation"),
        "{}",
        result.text
    );
    assert!(!any(&requests, "_action=LOCK"));
    assert!(!any(&requests, "PUT "));
}

#[test]
fn a_live_source_that_moved_since_the_simulation_stops_the_execute_as_live_changed() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);
    let requests = log();

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));

    rig.adt(recording(
        replay(
            FIXTURE,
            vec![Step::ReplyTo(
                6,
                Ok(abapreach::transport::Response::text(
                    200,
                    "REPORT ytest_abapreach01.\n\nWRITE / 'moved'.",
                )),
            )],
        ),
        &requests,
    ));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(edit_arguments(v2_edits()), &token),
    );

    assert!(result.is_error);
    assert!(result.text.contains("live changed"), "{}", result.text);
    assert!(!any(&requests, "_action=LOCK"));
}

#[test]
fn the_confirmation_form_shows_the_diff_and_never_the_raw_edits() {
    let mut rig = rig(CONFIRM);
    let mut session = FakeSession::typing(NAME);
    let requests = log();

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));

    rig.adt(recording(replay(FIXTURE, execute_steps()), &requests));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(edit_arguments(v2_edits()), &token),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(session.asked(), 1);

    let form = &session.forms[0].0.message;

    assert!(form.contains("-WRITE / 'v1'."), "{form}");
    assert!(form.contains("+WRITE / 'v2'."), "{form}");
    for raw in ["edits", "\"old\"", "\"new\"", "edits_sha256"] {
        assert!(!form.contains(raw), "{raw} in {form}");
    }
}

#[test]
fn an_edits_execute_with_a_ticket_that_holds_no_result_hash_is_refused() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));
    let mut gate = rig.gate.clone();
    let mut stored = gate.admit(&token, &presented(&v2_edit_list())).unwrap();

    stored.result_hash = None;

    let token = gate.issue(stored).unwrap();
    let opened = rig.opened();
    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(edit_arguments(v2_edits()), &token),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("holds no edit result"),
        "{}",
        result.text
    );
    // Nothing was sent to SAP.
    assert_eq!(rig.opened(), opened);
}

#[test]
fn the_stale_base_text_of_the_edits_path_names_base_not_the_cli_flag() {
    let mut rig = rig(TOKEN);
    let mut arguments = edit_arguments(v2_edits());

    arguments["base"] = json!("0".repeat(64));
    rig.adt(replay(FIXTURE, vec![fx(6)]));

    let result = rig.bare("abap_simulate", arguments);

    assert!(result.text.contains("!= base 000"), "{}", result.text);
    assert!(!result.text.contains("--base"), "{}", result.text);
}

#[test]
fn a_null_edits_argument_is_absent() {
    let mut rig = rig(TOKEN);

    // Without a source it is the same refusal as no edits at all.
    let mut arguments = json!({"conn": "DEV_100", "type": "PROG", "name": NAME});

    arguments["edits"] = Value::Null;

    let result = rig.bare("abap_simulate", arguments);

    assert!(result.is_error);
    assert!(
        result.text.contains("source or edits is required"),
        "{}",
        result.text
    );

    // With a source it is the full-source path, token bound to `source_sha256`.
    let mut arguments = deploy_arguments();

    arguments["edits"] = Value::Null;
    rig.adt(replay(FIXTURE, simulate_steps()));

    let result = rig.bare("abap_simulate", arguments);

    assert!(!result.is_error, "{}", result.text);
    assert!(result.structured.get("plan_token").is_some());
}

#[test]
fn every_other_bound_field_still_binds_a_token_of_edits() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));
    let opened = rig.opened();

    // `name`, `type` and `tr` of the deploy differ from the simulation.
    for (key, value) in [
        ("name", json!("YTEST_ABAPREACH02")),
        ("type", json!("CLAS")),
        ("tr", json!("DEVK900002")),
    ] {
        let mut arguments = edit_arguments(v2_edits());

        arguments[key] = value;

        let result = rig.call(&mut session, "abap_deploy", with_token(arguments, &token));

        assert!(result.is_error, "{key}");
        assert!(
            result.text.contains("arguments differ"),
            "{key}: {}",
            result.text
        );
    }

    assert_eq!(rig.opened(), opened);

    // `include` is bound as well, and a bound field that is absent from the call differs too.
    let mut gate = rig.gate.clone();
    let mut other = presented(&v2_edit_list());

    other
        .fields
        .insert("include".to_owned(), "CCIMP".to_owned());

    assert!(
        gate.admit(&token, &other)
            .unwrap_err()
            .contains("arguments differ")
    );

    // The unchanged arguments still redeem the token: none of the refusals spent it.
    other.fields.remove("include");

    assert!(gate.admit(&token, &other).is_ok());
}

#[test]
fn a_ticket_without_a_result_hash_is_refused_before_the_confirmation_form() {
    let mut rig = rig(CONFIRM);
    let mut session = FakeSession::typing(NAME);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", edit_arguments(v2_edits())));
    let mut gate = rig.gate.clone();
    let mut stored = gate.admit(&token, &presented(&v2_edit_list())).unwrap();

    stored.result_hash = None;

    let token = gate.issue(stored).unwrap();
    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(edit_arguments(v2_edits()), &token),
    );

    assert!(result.is_error);
    assert!(
        result.text.contains("holds no edit result"),
        "{}",
        result.text
    );
    assert_eq!(
        session.asked(),
        0,
        "a human is never asked about a refused call"
    );
}

#[test]
fn a_base_that_is_not_64_lowercase_hex_is_refused_before_sap() {
    let mut rig = rig(TOKEN);

    for bad in [
        "abc".to_owned(),
        base().to_ascii_uppercase(),
        format!("{}g", &base()[..63]),
        format!("{}0", base()),
    ] {
        let mut arguments = edit_arguments(v2_edits());

        arguments["base"] = json!(bad);

        let result = rig.bare("abap_simulate", arguments);

        assert!(result.is_error, "{bad}");
        assert!(
            result
                .text
                .contains("base must be the 64-hex hash from abap_read_source"),
            "{bad}: {}",
            result.text
        );
    }

    assert_eq!(rig.opened(), (0, 0));
}

const CHECK_ERROR: &str = r#"<chkrun:checkReport xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkMessageList><chkrun:checkMessage chkrun:type="E" chkrun:uri="/x#start=2,0" chkrun:shortText="bad"/></chkrun:checkMessageList></chkrun:checkReport>"#;

#[test]
fn a_syntax_check_error_on_the_edit_result_reports_it_and_issues_no_token() {
    let mut rig = rig(TOKEN);
    let requests = log();
    let intf = "/sap/bc/adt/oo/interfaces/ytest_abapreach_if";
    let source = format!("{intf}/source/main");
    let feed_uri = format!("{source}/versions");
    let feed = r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom"><atom:entry><atom:author><atom:name>user</atom:name></atom:author><atom:id>00000</atom:id></atom:entry></atom:feed>"#;
    let live = "INTERFACE ytest_abapreach_if PUBLIC.\nENDINTERFACE.";
    let base = source_hash(&live.split('\n').map(str::to_owned).collect::<Vec<_>>());

    rig.adt(
        Script::new(&requests)
            .get(&format!("{source}?version=active"), 200, live)
            .get(&feed_uri, 200, feed)
            .get(&feed_uri, 200, feed)
            .post(
                &format!("{intf}?_action=LOCK&accessMode=MODIFY"),
                200,
                &lock_xml(),
            )
            .get(&format!("{source}?version=active"), 200, live)
            .post(
                "/sap/bc/adt/checkruns?reporters=abapCheckRun",
                200,
                CHECK_ERROR,
            )
            .post(
                &format!("{intf}?_action=UNLOCK&lockHandle={ENCODED}"),
                200,
                "",
            ),
    );

    let result = rig.bare(
        "abap_simulate",
        json!({"conn": "DEV_100", "type": "INTF", "name": "YTEST_ABAPREACH_IF", "base": base,
               "edits": [{"old": "PUBLIC.\n", "new": "PUBLIC.\n  METHODS 1bad.\n"}]}),
    );

    // The edit applied (the diff shows it), the check failed, nothing was written or issued.
    assert!(result.is_error, "{}", result.text);
    assert!(result.text.contains("+  METHODS 1bad."), "{}", result.text);
    assert!(result.text.contains("bad"), "{}", result.text);
    assert!(result.structured.get("plan_token").is_none());
    assert!(!any(&requests, "PUT "));
}

/// A ticket as the simulation of a deploy by edits would store it, with extra bound fields.
fn edit_ticket(kind: &str, extra: &[(&str, &str)]) -> Ticket {
    let mut ticket = presented(&v2_edit_list());

    ticket.fields.insert("type".to_owned(), kind.to_owned());
    for (key, value) in extra {
        ticket.fields.insert((*key).to_owned(), (*value).to_owned());
    }

    Ticket {
        base: Some(base()),
        result_hash: Some("9".repeat(64)),
        ..ticket
    }
}

#[test]
fn fugr_and_main_bind_a_token_of_edits() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);
    let mut gate = rig.gate.clone();
    let mut call = |kind: &str, name: &str, bound: &[(&str, &str)], sent: &[(&str, &str)]| {
        let token = gate.issue(edit_ticket(kind, bound)).unwrap();
        let mut arguments = edit_arguments(v2_edits());

        arguments["type"] = json!(kind);
        arguments["name"] = json!(name);
        for (key, value) in sent {
            arguments[*key] = json!(value);
        }

        rig.call(&mut session, "abap_deploy", with_token(arguments, &token))
    };

    for (kind, name, key, bound, other) in [
        ("FUNC", "YTEST_ABAPREACH_FM", "fugr", "ZFG1", "ZFG2"),
        ("PROG", NAME, "main", "ZMAIN1", "ZMAIN2"),
    ] {
        let named = ("name", name.to_ascii_uppercase());
        let bound_fields = [(key, bound), (named.0, named.1.as_str())];

        // Another value, and no value, differ from the bound one.
        for sent in [vec![(key, other)], vec![]] {
            let result = call(kind, name, &bound_fields, &sent);

            assert!(result.is_error, "{key} {sent:?}");
            assert!(
                result.text.contains("arguments differ"),
                "{key} {sent:?}: {}",
                result.text
            );
        }
    }

    assert_eq!(rig.opened(), (0, 0));
}
