//! The plan token lifecycle and the policies that gate a write: issue, expiry, single use and
//! every bound field; `confirm` and `token`; delete's unconditional confirmation. Fake client and
//! fake transports, no SAP. The token cases of `tools/test_abap_rfc_mcp.py` are ported here; the
//! differences from the Python server are listed in the final report of Task 10.

mod common;
mod mcprig;
mod replay;

use abapreach::backend::BackendKind;
use abapreach::mcp::Answer;
use abapreach::mcp_write::{Operation, TOKEN_LIFETIME, Ticket, TokenGate, WriteGate};
use mcprig::*;
use replay::{Step, fx, no_inactive, replay};
use serde_json::json;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

const FIXTURE: &str = "dev-750-rfc.json";
const TOKEN: &str = r#"{"mcp_write":"token"}"#;
const CONFIRM: &str = r#"{"mcp_write":"confirm"}"#;

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

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn ticket() -> Ticket {
    Ticket::new(
        Operation::Deploy,
        "DEV_100",
        fields(&[
            ("type", "PROG"),
            ("name", "YTEST"),
            ("source_sha256", "abc"),
            ("tr", "DEVK900001"),
            ("fugr", "ZFG"),
            ("include", "CCIMP"),
            ("create_include", "true"),
        ]),
    )
}

fn stored(gate: &mut TokenGate, clock: &Clock) -> (String, Ticket) {
    let _ = clock;
    let full = Ticket {
        backend: Some(BackendKind::AdtHttp),
        group: Some("ZFG".to_owned()),
        base: Some("f".repeat(64)),
        ..ticket()
    };
    let token = gate.issue(full.clone()).expect("a token");

    (token, full)
}

// ---- the gate on its own ---------------------------------------------------------------------

#[test]
fn a_token_is_32_hex_digits_and_never_repeats() {
    let mut gate = Clock::new().gate();
    let tokens: Vec<String> = (0..200)
        .map(|_| gate.issue(ticket()).expect("a token"))
        .collect();
    let mut unique = tokens.clone();

    unique.sort();
    unique.dedup();

    assert_eq!(unique.len(), tokens.len());
    assert!(tokens.iter().all(|token| {
        token.len() == 32
            && token
                .chars()
                .all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase())
    }));
}

#[test]
fn a_token_admits_once_and_returns_what_the_simulation_stored() {
    let clock = Clock::new();
    let mut gate = clock.gate();
    let (token, full) = stored(&mut gate, &clock);

    assert_eq!(gate.admit(&token, &ticket()), Ok(full));

    let again = gate.admit(&token, &ticket()).unwrap_err();

    assert!(again.contains("unknown"), "{again}");
}

#[test]
fn a_token_lives_fifteen_minutes() {
    assert_eq!(TOKEN_LIFETIME, Duration::from_secs(15 * 60));

    let clock = Clock::new();
    let mut gate = clock.gate();
    let (late, _) = stored(&mut gate, &clock);
    let (fresh, _) = stored(&mut gate, &clock);

    clock.advance(TOKEN_LIFETIME + Duration::from_secs(1));

    let error = gate.admit(&late, &ticket()).unwrap_err();

    assert!(error.contains("expired"), "{error}");
    // Expired is gone, not merely refused: the next look says unknown.
    assert!(
        gate.admit(&late, &ticket())
            .unwrap_err()
            .contains("unknown")
    );
    assert!(
        gate.admit(&fresh, &ticket())
            .unwrap_err()
            .contains("expired")
    );

    let (young, _) = stored(&mut gate, &clock);

    clock.advance(TOKEN_LIFETIME - Duration::from_secs(1));
    assert!(gate.admit(&young, &ticket()).is_ok());
}

#[test]
fn every_bound_field_must_match_and_a_mismatch_burns_nothing() {
    let clock = Clock::new();
    let mut gate = clock.gate();
    let (token, _) = stored(&mut gate, &clock);
    let mut mutations: Vec<(String, Ticket)> = vec![
        (
            "operation".to_owned(),
            Ticket {
                operation: Operation::Create,
                ..ticket()
            },
        ),
        (
            "conn".to_owned(),
            Ticket {
                conn: "QAS_100".to_owned(),
                ..ticket()
            },
        ),
    ];

    for key in ticket().fields.keys() {
        let mut changed = ticket();

        changed.fields.insert(key.clone(), "other".to_owned());
        mutations.push((format!("changed {key}"), changed));

        let mut missing = ticket();

        missing.fields.remove(key);
        mutations.push((format!("missing {key}"), missing));
    }

    let mut extra = ticket();

    extra.fields.insert("tr_extra".to_owned(), "x".to_owned());
    mutations.push(("extra field".to_owned(), extra));

    for (what, presented) in mutations {
        let error = gate.admit(&token, &presented).unwrap_err();

        assert!(error.contains("arguments differ"), "{what}: {error}");
    }

    // None of the attempts spent the token; the original arguments still redeem it.
    assert!(gate.admit(&token, &ticket()).is_ok());
}

#[test]
fn an_unknown_or_empty_token_admits_nothing() {
    let mut gate = Clock::new().gate();

    for token in ["", "deadbeef", &"0".repeat(32)] {
        assert!(
            gate.admit(token, &ticket())
                .unwrap_err()
                .contains("unknown")
        );
    }
}

#[test]
fn the_resolved_group_the_live_base_and_the_plan_travel_with_the_token() {
    let clock = Clock::new();
    let mut gate = clock.gate();
    let (token, full) = stored(&mut gate, &clock);
    let back = gate.admit(&token, &ticket()).unwrap();

    assert_eq!(back.group.as_deref(), Some("ZFG"));
    assert_eq!(back.base, full.base);
    assert_eq!(back.backend, Some(BackendKind::AdtHttp));
}

#[test]
fn a_clock_that_never_moves_never_expires_a_token() {
    let now = Instant::now();
    let mut gate = TokenGate::with_clock(Rc::new(move || now));
    let token = gate.issue(ticket()).unwrap();

    assert!(gate.admit(&token, &ticket()).is_ok());
}

// ---- tokens through the tools ----------------------------------------------------------------

#[test]
fn a_deploy_needs_the_token_of_its_simulation_and_the_token_is_single_use() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);
    let requests = log();

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));

    let simulated = rig.call(&mut session, "abap_simulate", deploy_arguments());

    assert!(!simulated.is_error, "{}", simulated.text);
    assert_eq!(simulated.structured["status"], "SIMULATED_OK");

    let token = token_of(&simulated);

    assert!(simulated.text.contains(&format!("plan_token {token}")));

    rig.adt(recording(replay(FIXTURE, execute_steps()), &requests));

    let deployed = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(!deployed.is_error, "{}", deployed.text);
    assert_eq!(deployed.structured["status"], "ACTIVE");
    assert!(any(&requests, "PUT "));
    assert_eq!(session.asked(), 0, "`token` asks no human");

    let opened = rig.opened();
    let again = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(again.is_error);
    assert!(again.text.contains("unknown"), "{}", again.text);
    assert_eq!(rig.opened(), opened);
}

#[test]
fn a_deploy_without_a_token_or_with_an_unknown_one_touches_nothing() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    for arguments in [
        deploy_arguments(),
        with_token(deploy_arguments(), "deadbeef"),
    ] {
        let result = rig.call(&mut session, "abap_deploy", arguments);

        assert!(result.is_error);
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
    }

    assert_eq!(rig.opened(), (0, 0));
}

#[test]
fn an_expired_token_runs_nothing() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));
    let opened = rig.opened();

    rig.clock.advance(TOKEN_LIFETIME + Duration::from_secs(1));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(result.is_error);
    assert!(result.text.contains("expired"), "{}", result.text);
    assert_eq!(rig.opened(), opened);
}

#[test]
fn a_token_for_other_arguments_runs_nothing_and_still_serves_the_original_call() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));
    let opened = rig.opened();

    for change in [
        json!({"source": format!("{PROGRAM}\nWRITE 3.")}),
        json!({"name": "YOTHER"}),
        json!({"type": "INTF"}),
        json!({"tr": "DEVK900002"}),
        json!({"include": "CCIMP", "type": "CLAS", "name": "ZCL_A"}),
        json!({"fugr": "ZFG", "type": "FUNC", "name": "ZFM"}),
    ] {
        let mut arguments = with_token(deploy_arguments(), &token);

        for (key, value) in change.as_object().unwrap() {
            arguments[key] = value.clone();
        }

        let result = rig.call(&mut session, "abap_deploy", arguments);

        assert!(result.is_error, "{change}");
    }

    assert_eq!(rig.opened(), opened);

    rig.adt(replay(FIXTURE, execute_steps()));

    let original = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(!original.is_error, "{}", original.text);
}

#[test]
fn a_deploy_token_never_admits_a_create_or_a_delete() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::typing("ZPROG");

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));
    let opened = rig.opened();
    let create = json!({"conn": "DEV_100", "type": "FUGR", "name": "ZTESTFG", "text": "T",
                        "package": "ZPKG", "plan_token": token});
    let delete = json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"], "plan_token": token});

    session.can_elicit = true;
    assert!(rig.call(&mut session, "abap_create", create).is_error);
    assert!(rig.call(&mut session, "abap_delete", delete).is_error);
    assert_eq!(rig.opened(), opened);
}

#[test]
fn a_token_of_another_connection_is_refused() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));
    let mut ticket = ticket();

    ticket.conn = "QAS_100".to_owned();

    // The gate binds the connection; the tool cannot even name another one that may write.
    let mut gate = rig.gate.clone();

    assert!(
        gate.admit(&token, &ticket)
            .unwrap_err()
            .contains("arguments differ")
    );
}

// ---- policies and the human ------------------------------------------------------------------

#[test]
fn confirm_without_an_elicitation_capability_refuses_naming_the_policy_and_keeps_the_token() {
    let mut rig = rig(CONFIRM);
    let mut blind = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut blind, "abap_simulate", deploy_arguments()));
    let opened = rig.opened();
    let refused = rig.call(
        &mut blind,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(refused.is_error);
    assert!(refused.text.starts_with("STOP: "), "{}", refused.text);
    assert!(refused.text.contains("mcp_write"), "{}", refused.text);
    assert!(refused.text.contains("elicitation"), "{}", refused.text);
    assert_eq!(blind.asked(), 0);
    assert_eq!(rig.opened(), opened);

    // The refusal was not the human's decline: a client that can ask still redeems the token.
    rig.adt(replay(FIXTURE, execute_steps()));

    let mut able = FakeSession::typing("YTEST_ABAPREACH01");
    let result = rig.call(
        &mut able,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(!result.is_error, "{}", result.text);
}

#[test]
fn confirm_runs_the_deploy_after_the_typed_object_name() {
    let mut rig = rig(CONFIRM);
    let mut session = FakeSession::typing("YTEST_ABAPREACH01");
    let requests = log();

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));

    assert_eq!(session.asked(), 0, "a simulation asks nobody");

    rig.adt(recording(replay(FIXTURE, execute_steps()), &requests));

    let result = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(session.asked(), 1);
    assert!(any(&requests, "PUT "));
}

#[test]
fn the_form_has_one_required_string_field_and_a_bounded_sanitized_summary() {
    let mut rig = rig(CONFIRM);
    let mut session = FakeSession::typing("YTEST_ABAPREACH01");

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));

    rig.adt(replay(FIXTURE, execute_steps()));
    rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    let (form, timeout) = &session.forms[0];
    let message = &form.message;

    assert_eq!(*timeout, Duration::from_secs(300));
    assert!(message.contains("DEV_100"), "{message}");
    assert!(message.contains("SID DEV"), "{message}");
    assert!(message.contains("client 100"), "{message}");
    assert!(message.contains("backend adt"), "{message}");
    assert!(message.contains("+1 -1"), "{message}");
    assert!(message.contains("-WRITE / 'v1'."), "{message}");
    assert!(message.contains("+WRITE / 'v2'."), "{message}");
    assert!(message.contains("YTEST_ABAPREACH01"), "{message}");
    assert!(!message.contains(PASSWORD));
    assert!(form.label.contains("YTEST_ABAPREACH01"), "{}", form.label);
    assert!(
        message
            .chars()
            .all(|character| character == '\n' || !character.is_control()),
        "{message:?}"
    );
}

#[test]
fn anything_but_the_exact_typed_name_is_a_decline_that_burns_the_token() {
    for answer in [
        Answer::Declined,
        Answer::Cancelled,
        Answer::TimedOut,
        Answer::Accepted(String::new()),
        Answer::Accepted("ytest_abapreach01".to_owned()),
        Answer::Accepted("YTEST_ABAPREACH0".to_owned()),
        Answer::Accepted("DELETE 1".to_owned()),
        Answer::Accepted("yes".to_owned()),
    ] {
        let mut rig = rig(CONFIRM);
        let mut session = FakeSession::typing("YTEST_ABAPREACH01");

        rig.adt(replay(FIXTURE, simulate_steps()));

        let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));
        let opened = rig.opened();

        session.answers = [answer.clone()].into();

        let result = rig.call(
            &mut session,
            "abap_deploy",
            with_token(deploy_arguments(), &token),
        );

        assert!(result.is_error, "{answer:?}");
        assert!(result.text.starts_with("STOP: "), "{}", result.text);
        assert!(result.text.contains("not confirmed"), "{}", result.text);
        assert_eq!(rig.opened(), opened, "{answer:?}: nothing was opened");

        // Declined means burnt: the same token does not get a second chance.
        let second = rig.call(
            &mut session,
            "abap_deploy",
            with_token(deploy_arguments(), &token),
        );

        assert!(
            second.text.contains("unknown"),
            "{answer:?}: {}",
            second.text
        );
        assert_eq!(session.asked(), 1);
    }
}

#[test]
fn a_create_is_confirmed_like_a_deploy_under_confirm() {
    let mut rig = rig(CONFIRM);
    let mut session = FakeSession::new(true);
    let requests = log();
    let create = json!({"conn": "DEV_100", "type": "FUGR", "name": "ZTESTFG", "text": "T",
                        "package": "ZPKG"});

    rig.adt(Script::new(&requests));

    let simulated = rig.call(&mut session, "abap_create_simulate", create.clone());

    // The script knows no request, so the simulation fails and no token exists.
    assert!(simulated.is_error);
    assert!(simulated.structured.get("plan_token").is_none());

    let result = rig.call(&mut session, "abap_create", with_token(create, "deadbeef"));

    assert!(result.is_error);
    assert_eq!(session.asked(), 0, "an unknown token asks nobody");
}

#[test]
fn delete_always_asks_even_when_the_policy_is_token_and_refuses_a_blind_client() {
    for policy in [TOKEN, CONFIRM] {
        let mut rig = rig(policy);
        let requests = log();
        let delete = json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]});
        let mut blind = FakeSession::new(false);

        rig.adt(prog_reads(&requests, "OWNER"));

        let simulated = rig.call(&mut blind, "abap_delete_simulate", delete.clone());
        let token = token_of(&simulated);
        let refused = rig.call(
            &mut blind,
            "abap_delete",
            with_token(delete.clone(), &token),
        );

        assert!(refused.is_error, "{policy}");
        assert!(refused.text.contains("abap_delete"), "{}", refused.text);
        assert!(refused.text.contains("elicitation"), "{}", refused.text);
        assert!(!any(&requests, "DELETE "), "{policy}");

        // Refused before the token was spent: a client that can answer still deletes.
        rig.adt(
            prog_reads(&requests, "OWNER")
                .post(
                    &format!("{PROG}?_action=LOCK&accessMode=MODIFY"),
                    200,
                    &lock_xml(),
                )
                .on(
                    "DELETE",
                    &format!("{PROG}?lockHandle={ENCODED}"),
                    Ok(abapreach::transport::Response::empty(200)),
                ),
        );
        rig.adt(Script::new(&requests).get(PROG, 404, ""));

        let mut able = FakeSession::typing("ZPROG");
        let deleted = rig.call(&mut able, "abap_delete", with_token(delete, &token));

        assert!(!deleted.is_error, "{policy}: {}", deleted.text);
        assert_eq!(able.asked(), 1, "{policy}");
        assert_eq!(deleted.structured["objects"][0]["status"], "DELETED");
    }
}

#[test]
fn a_delete_batch_is_confirmed_by_typing_delete_and_the_count() {
    let mut rig = rig(TOKEN);
    let requests = log();
    let delete = json!({"conn": "DEV_100", "objects": ["PROG:ZPROG", "PROG:ZOTHER"]});
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

    rig.adt(reads(prog_reads(&requests, "OWNER")));

    let mut session = FakeSession::new(true);
    let token = token_of(&rig.call(&mut session, "abap_delete_simulate", delete.clone()));

    rig.adt(reads(prog_reads(&requests, "OWNER")));
    session
        .answers
        .push_back(Answer::Accepted("ZPROG".to_owned()));

    let wrong = rig.call(&mut session, "abap_delete", with_token(delete, &token));

    assert!(
        wrong.is_error,
        "the object name does not confirm a batch of two"
    );
    assert!(
        session.forms[0].0.label.contains("DELETE 2"),
        "{}",
        session.forms[0].0.label
    );
    assert!(session.forms[0].0.message.contains("PROG:ZPROG"));
    assert!(session.forms[0].0.message.contains("PROG:ZOTHER"));
    assert!(!any(&requests, "DELETE "));
}

#[test]
fn a_mixed_simulation_issues_no_delete_token_and_nothing_can_delete_afterwards() {
    let mut rig = rig(TOKEN);
    let requests = log();
    let mut session = FakeSession::typing("ZPROG");
    let delete = json!({"conn": "DEV_100", "objects": ["PROG:ZPROG", "PROG:ZMISSING"]});

    rig.adt(prog_reads(&requests, "OWNER").get("/sap/bc/adt/programs/programs/zmissing", 404, ""));

    let simulated = rig.call(&mut session, "abap_delete_simulate", delete.clone());

    assert!(
        simulated.structured.get("plan_token").is_none(),
        "{}",
        simulated.text
    );
    assert!(!simulated.text.contains("plan_token"));

    let attempt = rig.call(&mut session, "abap_delete", with_token(delete, "deadbeef"));

    assert!(attempt.is_error);
    assert!(!any(&requests, "DELETE "));
    assert_eq!(session.asked(), 0);
}

// ---- the remaining Python token cases --------------------------------------------------------

#[test]
fn a_simulation_without_changes_is_no_error_and_issues_no_token() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);
    let mut arguments = deploy_arguments();

    // The live source of the fixture, so there is nothing to deploy.
    arguments["source"] = json!("REPORT ytest_abapreach01.\n\nWRITE / 'v1'.");
    rig.adt(replay(FIXTURE, vec![fx(6)]));

    let result = rig.call(&mut session, "abap_simulate", arguments);

    assert!(!result.is_error, "{}", result.text);
    assert!(result.structured.get("plan_token").is_none());
    assert!(result.text.contains("no changes"), "{}", result.text);
}

#[test]
fn a_token_is_spent_by_the_call_even_when_the_write_then_fails() {
    let mut rig = rig(TOKEN);
    let mut session = FakeSession::new(false);

    rig.adt(replay(FIXTURE, simulate_steps()));

    let token = token_of(&rig.call(&mut session, "abap_simulate", deploy_arguments()));

    // The live source moved on: the write fails before the lock.
    rig.adt(replay(
        FIXTURE,
        vec![Step::ReplyTo(
            6,
            Ok(abapreach::transport::Response::text(200, "REPORT other.")),
        )],
    ));

    let failed = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );
    let again = rig.call(
        &mut session,
        "abap_deploy",
        with_token(deploy_arguments(), &token),
    );

    assert!(failed.is_error && again.is_error);
    assert!(again.text.contains("unknown"), "{}", again.text);
}

#[test]
fn a_delete_token_binds_the_objects_and_the_request_but_not_their_order() {
    let mut rig = rig(TOKEN);
    let requests = log();
    let mut session = FakeSession::typing("DELETE 2");
    let delete = json!({"conn": "DEV_100", "objects": ["PROG:ZPROG", "PROG:ZOTHER"]});
    let other = "/sap/bc/adt/programs/programs/zother";

    rig.adt(
        prog_reads(&requests, "OWNER")
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
            ),
    );

    let token = token_of(&rig.call(&mut session, "abap_delete_simulate", delete));
    let opened = rig.opened();

    for changed in [
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG"]}),
        json!({"conn": "DEV_100", "objects": ["PROG:ZPROG", "PROG:ZOTHER"], "tr": "DEVK900001"}),
    ] {
        let result = rig.call(&mut session, "abap_delete", with_token(changed, &token));

        assert!(result.is_error);
        assert!(result.text.contains("arguments differ"), "{}", result.text);
    }

    assert_eq!(rig.opened(), opened);
    assert_eq!(
        session.asked(),
        0,
        "a token that does not match asks nobody"
    );

    // Python's plan file depended on the order of the arguments; the ticket does not, because
    // the batch runs in a fixed order anyway. The reordered call gets as far as the human.
    session.answers = [Answer::Declined].into();

    let reordered = rig.call(
        &mut session,
        "abap_delete",
        with_token(
            json!({"conn": "DEV_100", "objects": ["PROG:ZOTHER", "PROG:ZPROG"]}),
            &token,
        ),
    );

    assert!(
        reordered.text.contains("not confirmed"),
        "{}",
        reordered.text
    );
    assert_eq!(session.asked(), 1);
}

#[test]
fn a_create_asks_the_human_under_confirm_with_the_name_to_type() {
    let mut rig = rig(CONFIRM);
    let mut session = FakeSession::new(true);
    let create = json!({"conn": "DEV_100", "type": "FUGR", "name": "ZTESTFG", "text": "T",
                        "package": "$TMP"});
    let ticket = Ticket::new(
        Operation::Create,
        "DEV_100",
        fields(&[
            ("type", "FUGR"),
            ("name", "ZTESTFG"),
            ("text", "T"),
            ("package", "$TMP"),
        ]),
    );
    let token = rig.gate.clone().issue(ticket).unwrap();
    let result = rig.call(&mut session, "abap_create", with_token(create, &token));
    let (form, _) = &session.forms[0];

    assert!(result.text.contains("not confirmed"), "{}", result.text);
    assert!(
        form.message.starts_with("CREATE on DEV_100"),
        "{}",
        form.message
    );
    assert!(form.label.contains("ZTESTFG"), "{}", form.label);
    assert_eq!(rig.opened(), (0, 0));
}
