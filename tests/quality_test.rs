//! `inactive`, `activate` and `aunit` over scripted transports: parsers, rules, request shapes and
//! outcomes. The reply shapes of the inactive list and of ABAP Unit are assumed (see the files
//! `tests/fixtures/m7/*-assumed.xml`); the live probe replaces them.

mod common;

use abapreach::domain::Status;
use abapreach::operation::activate::{
    ActivateInput, ActivateKind, ActivateTarget, MAX_OBJECTS, check_targets, run as activate_run,
};
use abapreach::operation::aunit::{
    AunitKind, AunitTarget, Scope, parse_run_result, run as aunit_run, run_configuration,
    run_request,
};
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::inactive::{
    InactiveEntry, inactive_author, inactive_request, inactive_uri, list, parse_inactive_objects,
    run as inactive_run,
};
use abapreach::operation::quality::{QualityError, check_client};
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, preview};

const INACTIVE: &str = include_str!("fixtures/m7/inactive-objects-assumed.xml");
const FLAT_740: &str = include_str!("fixtures/m3/fed-740-inactive-flat.xml");
const FAILED: &str = include_str!("fixtures/m7/aunit-result-failed-assumed.xml");
const CLEAN: &str = include_str!("fixtures/m7/aunit-result-clean-assumed.xml");
const NO_TESTS: &str = include_str!("fixtures/m7/aunit-result-no-tests-assumed.xml");
const ERROR_MESSAGE: &str = r#"<?xml version="1.0" encoding="utf-8"?><chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist"><msg objDescr="Program ZTEST_MAIN" type="E" line="1" href="/sap/bc/adt/programs/programs/ztest_main/source/main#start=3,8" forceSupported="true"><shortText><txt>Field "NO_SUCH_VAR" is unknown.</txt></shortText></msg></chkl:messages>"#;
const USER: &str = "DEVUSER";

type Reply = Result<Response, TransportError>;

fn ok(body: impl AsRef<str>) -> Reply {
    Ok(Response::text(200, body))
}

fn client(category: &str) -> Reply {
    ok(preview(&[("CCCATEGORY", &[Some(category)])]))
}

fn transport(replies: Vec<Reply>) -> ScriptedTransport {
    ScriptedTransport::replies(replies)
}

fn target(text: &str) -> ActivateTarget {
    text.parse().unwrap()
}

fn activation_input<'a>(
    targets: &'a [ActivateTarget],
    user: &'a str,
    simulate: bool,
) -> ActivateInput<'a> {
    ActivateInput {
        targets,
        client: "100",
        user,
        simulate,
    }
}

fn body(call: &abapreach::transport::Request) -> String {
    String::from_utf8(call.body.clone()).unwrap()
}

// ---- the inactive list ---------------------------------------------------------------------------

#[test]
fn the_inactive_list_keeps_objects_users_and_main_programs_and_drops_class_members() {
    let entries = parse_inactive_objects(INACTIVE).unwrap();
    let lines: Vec<String> = entries.iter().map(InactiveEntry::line).collect();

    assert_eq!(
        lines,
        [
            "PROG/P ZTEST_MAIN DEVUSER",
            "PROG/I ZTEST_INC DEVUSER main ZTEST_MAIN",
            "CLAS/OC ZCL_TEST DEVUSER",
            "INTF/OI ZIF_TEST DEVUSER",
            "FUGR/F ZFG_TEST DEVUSER",
            "FUGR/FF ZFM_TEST DEVUSER",
            "TABL/DT ZTEST_TAB DEVUSER",
            "PROG/P ZOTHER OTHERUSER",
        ]
    );
    // The transport reference of the first entry is no object.
    assert!(entries.iter().all(|entry| entry.kind != "RQ"));
}

#[test]
fn the_flat_740_feed_names_no_user() {
    let entries = parse_inactive_objects(FLAT_740).unwrap();

    assert_eq!(
        entries.iter().map(InactiveEntry::line).collect::<Vec<_>>(),
        [
            "FUGR/F YTEST_ABAPREACH_RCAG ?",
            "FUGR/FF YTEST_ABAPREACH_RCAF2 ?"
        ]
    );
}

#[test]
fn unknown_elements_are_ignored_and_a_broken_document_is_an_error() {
    let tolerant = r#"<x:root xmlns:x="a"><x:extra><x:deep/></x:extra><x:entry><x:object x:user="U"><x:note/><x:ref x:uri="/u" x:type="PROG/P" x:name=" ZA "/></x:object></x:entry></x:root>"#;
    let entries = parse_inactive_objects(tolerant).unwrap();

    assert_eq!(entries[0].name, "ZA");
    assert_eq!(entries[0].user.as_deref(), Some("U"));
    assert!(parse_inactive_objects("<a><b></a>").is_err());
    assert_eq!(parse_inactive_objects("").unwrap(), []);
}

#[test]
fn the_request_is_a_get_with_the_inactive_accept_and_all_adds_the_user_filter() {
    assert_eq!(
        inactive_uri(false),
        "/sap/bc/adt/activation/inactiveobjects"
    );
    assert_eq!(
        inactive_uri(true),
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=*"
    );

    let request = inactive_request(false);

    assert_eq!(request.method, "GET");
    assert!(request.body.is_empty());
    assert_eq!(
        request.headers["Accept"],
        "application/vnd.sap.adt.inactivectsobjects.v1+xml, application/xml;q=0.8"
    );
}

#[test]
fn inactive_shows_only_own_entries_unless_all_and_counts_them() {
    let mut own = transport(vec![ok(INACTIVE)]);
    let outcome = inactive_run(&mut own, false, "devuser").unwrap();

    assert_eq!(outcome.exit, 0);
    assert!(outcome.stdout.ends_with("SUMMARY: 7 inactive objects"));
    assert!(!outcome.stdout.contains("ZOTHER"));
    assert_eq!(own.calls[0].uri, "/sap/bc/adt/activation/inactiveobjects");

    let mut all = transport(vec![ok(INACTIVE)]);
    let outcome = inactive_run(&mut all, true, USER).unwrap();

    assert!(outcome.stdout.contains("PROG/P ZOTHER OTHERUSER"));
    assert!(outcome.stdout.ends_with("SUMMARY: 8 inactive objects"));
    assert_eq!(
        all.calls[0].uri,
        "/sap/bc/adt/activation/inactiveobjects?USERNAME=*"
    );
}

#[test]
fn an_entry_without_a_user_stays_in_the_own_list() {
    let mut flat = transport(vec![ok(FLAT_740)]);

    assert_eq!(list(&mut flat, false, USER).unwrap().len(), 2);
}

#[test]
fn an_empty_reply_is_an_empty_list_and_a_refused_one_names_the_status() {
    let mut empty = transport(vec![Ok(Response::empty(200))]);

    assert_eq!(
        inactive_run(&mut empty, false, USER).unwrap().stdout,
        "SUMMARY: 0 inactive objects"
    );

    let mut refused = transport(vec![Ok(Response::empty(404))]);
    let error = inactive_run(&mut refused, true, USER).unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("HTTP 404") && message.contains("--all")),
        "{error:?}"
    );
}

#[test]
fn the_author_of_the_inactive_version_comes_from_the_version_feed() {
    let feed = r#"<atom:feed xmlns:atom="a"><atom:entry><atom:id>99999</atom:id><atom:author><atom:name>DEVUSER</atom:name></atom:author></atom:entry><atom:entry><atom:id>00002</atom:id><atom:author><atom:name>OTHER</atom:name></atom:author></atom:entry></atom:feed>"#;

    assert_eq!(inactive_author(feed).unwrap().as_deref(), Some("DEVUSER"));
    assert_eq!(
        inactive_author("<atom:feed xmlns:atom=\"a\"/>").unwrap(),
        None
    );
}

// ---- the client check ----------------------------------------------------------------------------

#[test]
fn a_productive_client_is_refused_before_anything_else() {
    let mut sap = transport(vec![client("P")]);
    let error = check_client(&mut sap, "100").unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("productive")),
        "{error:?}"
    );
    assert_eq!(sap.calls.len(), 1);
    assert_eq!(
        body(&sap.calls[0]),
        "SELECT CCCATEGORY FROM T000 WHERE MANDT = '100'"
    );
    assert!(check_client(&mut transport(vec![client("T")]), "100").is_ok());
}

#[test]
fn an_unreadable_or_missing_client_role_fails_closed() {
    let mut refused = transport(vec![Ok(Response::empty(403))]);

    assert!(
        matches!(check_client(&mut refused, "100"), Err(QualityError::Rejected(message)) if message.contains("cannot read")),
    );

    let mut missing = transport(vec![ok(preview(&[("CCCATEGORY", &[])]))]);

    assert!(
        matches!(check_client(&mut missing, "100"), Err(QualityError::Rejected(message)) if message.contains("not listed")),
    );
    assert!(matches!(
        check_client(&mut transport(vec![]), "10"),
        Err(QualityError::Rejected(_))
    ));
}

#[test]
fn a_lost_connection_stays_a_failure_that_routing_may_retry() {
    let mut lost = transport(vec![Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    ))]);

    assert!(matches!(
        check_client(&mut lost, "100"),
        Err(QualityError::Failed { .. })
    ));
}

// ---- activate: arguments -------------------------------------------------------------------------

#[test]
fn an_activate_target_is_a_type_and_a_z_or_y_name() {
    let parsed = target("prog/p:ztest_main");

    assert_eq!(parsed.kind, ActivateKind::Program);
    assert_eq!(parsed.token(), "PROG/P:ZTEST_MAIN");

    for (text, kind) in [
        ("PROG/I:ZINC", ActivateKind::Include),
        ("CLAS/OC:ZCL_A", ActivateKind::Class),
        ("INTF/OI:YIF_A", ActivateKind::Interface),
        ("FUGR/FF:ZFM", ActivateKind::FunctionModule),
    ] {
        assert_eq!(target(text).kind, kind, "{text}");
    }
}

#[test]
fn every_other_type_and_every_other_name_stops_with_the_reason() {
    let message = |text: &str| text.parse::<ActivateTarget>().unwrap_err();

    assert!(message("FUGR/F:ZFG").contains("FUGR/F is refused"));
    assert!(message("TABL/DT:ZTAB").contains("DDIC"));
    assert!(message("PROG:ZX").contains("not activated here"));
    assert!(message("PROG/P:SAPMSYST").contains("Z and Y"));
    assert!(message("PROG/P:/NS/ZPROG").contains("namespaced"));
    assert!(message("PROG/P:").contains("TYPE:NAME"));
    assert!(message("PROG/P").contains("TYPE:NAME"));
    assert!(message("CLAS/OC:ZCL_ABCDEFGHIJKLMNOPQRSTUVWXYZ1").contains("30"));
}

#[test]
fn a_batch_has_one_to_fifty_distinct_objects() {
    assert!(check_targets(&[]).is_err());
    assert!(check_targets(&[target("PROG/P:ZA"), target("PROG/P:ZA")]).is_err());

    let many: Vec<ActivateTarget> = (0..=MAX_OBJECTS)
        .map(|index| target(&format!("PROG/P:ZP{index}")))
        .collect();

    assert!(check_targets(&many).unwrap_err().contains("at most"));
    assert!(check_targets(&many[..MAX_OBJECTS]).is_ok());
}

// ---- activate: the run ---------------------------------------------------------------------------

fn without(entry: &str) -> String {
    let marker = format!("name=\"{entry}\"");

    INACTIVE
        .split("<ioc:entry>")
        .filter(|chunk| !chunk.contains(&marker))
        .collect::<Vec<_>>()
        .join("<ioc:entry>")
}

#[test]
fn a_simulation_checks_every_rule_and_sends_no_activation() {
    let targets = [target("PROG/P:ZTEST_MAIN"), target("CLAS/OC:ZCL_TEST")];
    let mut sap = transport(vec![client("T"), ok(INACTIVE)]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, true),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(outcome.exit, 0);
    assert_eq!(
        outcome.stdout,
        "SIMULATED_OK PROG/P:ZTEST_MAIN user DEVUSER\n\
         SIMULATED_OK CLAS/OC:ZCL_TEST user DEVUSER\n\
         SUMMARY: 2 SIMULATED_OK"
    );
    assert!(
        sap.calls
            .iter()
            .all(|call| call.method != "POST" || call.uri.contains("datapreview"))
    );
}

#[test]
fn a_program_is_activated_with_the_pinned_request_and_proved_by_the_list() {
    let targets = [target("PROG/P:ZTEST_MAIN")];
    let mut sap = transport(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        ok(without("ZTEST_MAIN")),
    ]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.status, Some(Status::Active));
    assert_eq!(outcome.exit, 0);
    assert_eq!(
        outcome.stdout,
        "ACTIVE PROG/P:ZTEST_MAIN\nSUMMARY: 1 ACTIVE, 0 INACTIVE"
    );

    let post = &sap.calls[2];

    assert_eq!(post.method, "POST");
    assert_eq!(
        post.uri,
        "/sap/bc/adt/activation?method=activate&preauditRequested=true"
    );
    assert_eq!(post.headers["Content-Type"], "application/xml");
    assert_eq!(post.headers["Accept"], "application/xml");
    assert_eq!(
        body(post),
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/programs/programs/ztest_main" adtcore:name="ZTEST_MAIN"/>"#,
            r#"</adtcore:objectReferences>"#,
        )
    );
    assert_eq!(sap.calls[3].method, "GET");
}

#[test]
fn an_include_is_activated_under_its_main_program() {
    let targets = [target("PROG/I:ZTEST_INC")];
    let mut sap = transport(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        ok(without("ZTEST_INC")),
    ]);

    activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert!(
        body(&sap.calls[2]).contains(
            r#"adtcore:uri="/sap/bc/adt/programs/includes/ztest_inc?context=%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fztest_main" adtcore:name="ZTEST_INC""#
        ),
        "{}",
        body(&sap.calls[2])
    );
}

#[test]
fn a_function_module_and_an_interface_use_their_own_uris() {
    let targets = [target("FUGR/FF:ZFM_TEST"), target("INTF/OI:ZIF_TEST")];
    let mut sap = transport(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        Ok(Response::empty(200)),
        ok(without("ZFM_TEST")),
    ]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert!(body(&sap.calls[2]).contains(
        r#"adtcore:uri="/sap/bc/adt/functions/groups/zfg_test/fmodules/zfm_test" adtcore:name="ZFM_TEST""#
    ));
    assert!(
        body(&sap.calls[3]).contains(
            r#"adtcore:uri="/sap/bc/adt/oo/interfaces/zif_test" adtcore:name="ZIF_TEST""#
        )
    );
    // The interface is still listed afterwards, so it is not proven active.
    assert_eq!(outcome.exit, 1);
    assert!(outcome.stdout.contains("ACTIVE FUGR/FF:ZFM_TEST"));
    assert!(
        outcome.stdout.contains(
            "INACTIVE INTF/OI:ZIF_TEST: activation reported no error but the object is still inactive"
        ),
        "{}",
        outcome.stdout
    );
}

#[test]
fn a_class_worklist_is_resent_once_without_the_preaudit() {
    let worklist = r#"<ioc:inactiveObjects xmlns:ioc="i" xmlns:adtcore="a"><ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_test" adtcore:type="CLAS/OC" adtcore:name="ZCL_TEST"/></ioc:object></ioc:entry><ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_test/includes/testclasses" adtcore:type="CLAS/OCN/testclasses" adtcore:name="ZCL_TEST" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_test"/></ioc:object></ioc:entry></ioc:inactiveObjects>"#;
    let targets = [target("CLAS/OC:ZCL_TEST")];
    let mut sap = transport(vec![
        client("T"),
        ok(INACTIVE),
        ok(worklist),
        Ok(Response::empty(200)),
        ok(without("ZCL_TEST")),
    ]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.exit, 0, "{}", outcome.stdout);
    assert!(sap.calls[2].uri.ends_with("preauditRequested=true"));
    assert!(sap.calls[3].uri.ends_with("preauditRequested=false"));
    assert!(body(&sap.calls[3]).contains(
        r#"adtcore:uri="/sap/bc/adt/oo/classes/zcl_test/includes/testclasses" adtcore:type="CLAS/OCN/testclasses""#
    ));
}

#[test]
fn a_worklist_beyond_the_class_is_not_resent() {
    let worklist = r#"<ioc:inactiveObjects xmlns:ioc="i" xmlns:adtcore="a"><ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/programs/programs/zother" adtcore:type="PROG/P" adtcore:name="ZOTHER"/></ioc:object></ioc:entry></ioc:inactiveObjects>"#;
    let targets = [target("CLAS/OC:ZCL_TEST")];
    let mut sap = transport(vec![client("T"), ok(INACTIVE), ok(worklist), ok(INACTIVE)]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.exit, 1);
    assert!(
        outcome
            .stdout
            .contains("outside the class, nothing was resent")
    );
    assert_eq!(
        sap.calls
            .iter()
            .filter(|call| call.uri.contains("activation?"))
            .count(),
        1
    );
}

#[test]
fn a_worklist_for_a_program_activates_nothing() {
    let worklist = r#"<ioc:inactiveObjects xmlns:ioc="i"/>"#;
    let targets = [target("PROG/P:ZTEST_MAIN")];
    let mut sap = transport(vec![client("T"), ok(INACTIVE), ok(worklist), ok(INACTIVE)]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.exit, 1);
    assert!(
        outcome
            .stdout
            .contains("answered a worklist and activated nothing")
    );
}

#[test]
fn an_error_message_makes_the_object_inactive_with_the_message_printed() {
    let targets = [target("PROG/P:ZTEST_MAIN")];
    let mut sap = transport(vec![
        client("T"),
        ok(INACTIVE),
        ok(ERROR_MESSAGE),
        ok(INACTIVE),
    ]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.status, Some(Status::Inactive));
    assert_eq!(outcome.exit, 1);
    assert_eq!(
        outcome.stdout,
        "E row 3: Field \"NO_SUCH_VAR\" is unknown.\n\
         INACTIVE PROG/P:ZTEST_MAIN: activation reported errors\n\
         SUMMARY: 0 ACTIVE, 1 INACTIVE"
    );
}

#[test]
fn an_http_error_and_a_lost_reply_leave_the_object_inactive() {
    let targets = [target("PROG/P:ZTEST_MAIN")];
    let mut refused = transport(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(500)),
        ok(INACTIVE),
    ]);
    let outcome = activate_run(
        &mut refused,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert!(outcome.stdout.contains("activation failed with HTTP 500"));

    let mut lost = transport(vec![
        client("T"),
        ok(INACTIVE),
        Err(TransportError::PostSendUncertain("reset".to_owned())),
        ok(INACTIVE),
    ]);
    let outcome = activate_run(
        &mut lost,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.exit, 1);
    assert!(outcome.stdout.contains("sent and its reply is unknown"));
}

#[test]
fn an_unreadable_proof_list_is_not_a_proof() {
    let targets = [target("PROG/P:ZTEST_MAIN")];
    let mut sap = transport(vec![
        client("T"),
        ok(INACTIVE),
        Ok(Response::empty(200)),
        Ok(Response::empty(500)),
    ]);
    let outcome = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.exit, 1);
    assert!(outcome.stdout.contains("cannot be read to prove it"));
}

#[test]
fn a_foreign_missing_or_ownerless_object_rejects_the_batch_before_any_activation() {
    let foreign = [target("PROG/P:ZOTHER")];
    let mut sap = transport(vec![client("T"), ok(INACTIVE)]);
    let error = activate_run(
        &mut sap,
        &activation_input(&foreign, USER, false),
        &CancelFlag::new(),
    )
    .unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("belongs to OTHERUSER") && message.contains("nothing was activated")),
        "{error:?}"
    );
    assert_eq!(sap.calls.len(), 2);

    let mixed = [target("PROG/P:ZTEST_MAIN"), target("PROG/P:ZNOT_LISTED")];
    let mut sap = transport(vec![client("T"), ok(INACTIVE)]);
    let error = activate_run(
        &mut sap,
        &activation_input(&mixed, USER, false),
        &CancelFlag::new(),
    )
    .unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("ZNOT_LISTED is not in the inactive list")),
        "{error:?}"
    );
    assert_eq!(
        sap.calls.len(),
        2,
        "the good object was not activated either"
    );
}

#[test]
fn an_entry_without_a_user_is_proved_own_by_its_version_feed() {
    let flat = r#"<adtcore:objectReferences xmlns:adtcore="a"><adtcore:objectReference adtcore:uri="/sap/bc/adt/programs/programs/zflat" adtcore:type="PROG/P" adtcore:name="ZFLAT"/></adtcore:objectReferences>"#;
    let feed = |author: &str| {
        format!(
            r#"<atom:feed xmlns:atom="a"><atom:entry><atom:id>99999</atom:id><atom:author><atom:name>{author}</atom:name></atom:author></atom:entry></atom:feed>"#
        )
    };
    let targets = [target("PROG/P:ZFLAT")];
    let mut own = transport(vec![
        client("T"),
        ok(flat),
        ok(feed(USER)),
        Ok(Response::empty(200)),
        ok("<x/>"),
    ]);
    let outcome = activate_run(
        &mut own,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap();

    assert_eq!(outcome.exit, 0, "{}", outcome.stdout);
    assert_eq!(
        own.calls[2].uri,
        "/sap/bc/adt/programs/programs/zflat/source/main/versions"
    );
    assert_eq!(
        own.calls[2].headers["Accept"],
        "application/atom+xml;type=feed"
    );

    let mut foreign = transport(vec![client("T"), ok(flat), ok(feed("OTHER"))]);
    let error = activate_run(
        &mut foreign,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("belongs to OTHER"))
    );

    let mut unknown = transport(vec![client("T"), ok(flat), Ok(Response::empty(404))]);
    let error = activate_run(
        &mut unknown,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("owner of the inactive version is unknown"))
    );
}

#[test]
fn the_person_responsible_of_a_flat_entry_is_not_the_owner_of_the_inactive_version() {
    // `adtcore:responsible` names the person responsible for the object, which says nothing about
    // who wrote the inactive version: the owner comes from the version feed alone.
    let flat = r#"<adtcore:objectReferences xmlns:adtcore="a"><adtcore:objectReference adtcore:uri="/sap/bc/adt/programs/programs/zflat" adtcore:type="PROG/P" adtcore:name="ZFLAT" adtcore:responsible="DEVUSER"/></adtcore:objectReferences>"#;
    let entries = parse_inactive_objects(flat).unwrap();

    assert_eq!(entries[0].user, None);
    assert_eq!(entries[0].line(), "PROG/P ZFLAT ?");

    let feed = |author: &str| {
        format!(
            r#"<atom:feed xmlns:atom="a"><atom:entry><atom:id>99999</atom:id><atom:author><atom:name>{author}</atom:name></atom:author></atom:entry></atom:feed>"#
        )
    };
    let targets = [target("PROG/P:ZFLAT")];
    let mut sap = transport(vec![client("T"), ok(flat), ok(feed("OTHER"))]);
    let error = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("belongs to OTHER, not to DEVUSER")),
        "{error:?}"
    );
    // The version feed was read, and nothing was activated.
    assert_eq!(sap.calls.len(), 3);
    assert_eq!(
        sap.calls[2].uri,
        "/sap/bc/adt/programs/programs/zflat/source/main/versions"
    );
}

#[test]
fn an_inactive_deletion_is_listed_with_a_mark_and_never_activated() {
    let list_xml = r#"<ioc:inactiveObjects xmlns:ioc="i" xmlns:adtcore="a"><ioc:entry><ioc:object ioc:user="DEVUSER" ioc:deleted="true"><ioc:ref adtcore:uri="/sap/bc/adt/programs/programs/zgone" adtcore:type="PROG/P" adtcore:name="ZGONE"/></ioc:object></ioc:entry><ioc:entry><ioc:object ioc:user="DEVUSER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/programs/programs/zkept" adtcore:type="PROG/P" adtcore:name="ZKEPT"/></ioc:object></ioc:entry></ioc:inactiveObjects>"#;
    let entries = parse_inactive_objects(list_xml).unwrap();

    assert_eq!(
        entries.iter().map(InactiveEntry::line).collect::<Vec<_>>(),
        ["PROG/P ZGONE DEVUSER deleted", "PROG/P ZKEPT DEVUSER"]
    );

    let mut sap = transport(vec![ok(list_xml)]);
    let outcome = inactive_run(&mut sap, false, USER).unwrap();

    assert!(outcome.stdout.contains("PROG/P ZGONE DEVUSER deleted"));

    // `activate` refuses it with the reason and activates nothing of the batch.
    let targets = [target("PROG/P:ZKEPT"), target("PROG/P:ZGONE")];
    let mut sap = transport(vec![client("T"), ok(list_xml)]);
    let error = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("ZGONE is marked deleted") && message.contains("complete the deletion") && message.contains("nothing was activated")),
        "{error:?}"
    );
    assert_eq!(sap.calls.len(), 2);
}

#[test]
fn the_function_group_entry_itself_is_never_a_target_of_the_flat_feed() {
    // FUGR/F is refused when the argument is parsed; the flat feed lists it only for `inactive`.
    assert!(
        "FUGR/F:YTEST_ABAPREACH_RCAG"
            .parse::<ActivateTarget>()
            .is_err()
    );
}

#[test]
fn a_productive_client_activates_nothing() {
    let targets = [target("PROG/P:ZTEST_MAIN")];
    let mut sap = transport(vec![client("P")]);
    let error = activate_run(
        &mut sap,
        &activation_input(&targets, USER, false),
        &CancelFlag::new(),
    )
    .unwrap_err();

    assert!(matches!(&error, QualityError::Rejected(message) if message.contains("productive")));
    assert_eq!(sap.calls.len(), 1);
}

#[test]
fn an_interrupt_between_objects_leaves_the_rest_not_attempted() {
    let cancel = CancelFlag::new();

    cancel.set();

    let targets = [target("PROG/P:ZTEST_MAIN")];
    let mut sap = transport(vec![client("T"), ok(INACTIVE), ok(INACTIVE)]);
    let outcome =
        activate_run(&mut sap, &activation_input(&targets, USER, false), &cancel).unwrap();

    assert_eq!(outcome.exit, 130);
    assert!(outcome.stdout.contains("not attempted: interrupted"));
    assert!(
        sap.calls
            .iter()
            .all(|call| !call.uri.contains("activation?"))
    );
}

// ---- aunit: the request and the reply ------------------------------------------------------------

#[test]
fn the_run_configuration_is_pinned() {
    let target = AunitTarget::new(AunitKind::Clas, "zcl_test").unwrap();

    assert_eq!(
        run_configuration(&target.uri()),
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<aunit:runConfiguration xmlns:aunit="http://www.sap.com/adt/aunit">"#,
            r#"<external><coverage active="false"/></external>"#,
            r#"<options><uriType value="semantic"/>"#,
            r#"<testDeterminationStrategy sameProgram="true" assignedTests="false" appendAssignedTestsPreview="true"/>"#,
            r#"<testRiskLevels harmless="true" dangerous="false" critical="false"/>"#,
            r#"<testDurations short="true" medium="true" long="true"/></options>"#,
            r#"<adtcore:objectSets xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<objectSet kind="inclusive"><adtcore:objectReferences>"#,
            r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/oo/classes/zcl_test"/>"#,
            r#"</adtcore:objectReferences></objectSet></adtcore:objectSets>"#,
            r#"</aunit:runConfiguration>"#,
        )
    );

    let request = run_request(&target);

    assert_eq!(request.method, "POST");
    assert_eq!(request.uri, "/sap/bc/adt/abapunit/testruns");
    assert_eq!(request.headers["Content-Type"], "application/xml");
    assert_eq!(request.headers["Accept"], "application/xml");
}

#[test]
fn the_object_uri_follows_the_type() {
    let uri = |kind, name| AunitTarget::new(kind, name).unwrap().uri();

    assert_eq!(
        uri(AunitKind::Prog, "ZTEST"),
        "/sap/bc/adt/programs/programs/ztest"
    );
    assert_eq!(
        uri(AunitKind::Clas, "ZCL_A"),
        "/sap/bc/adt/oo/classes/zcl_a"
    );
    assert_eq!(
        uri(AunitKind::Fugr, "ZFG"),
        "/sap/bc/adt/functions/groups/zfg"
    );
}

#[test]
fn only_plain_z_and_y_objects_of_the_right_length_are_tested() {
    assert!(AunitTarget::new(AunitKind::Prog, "SAPMSYST").is_err());
    assert!(AunitTarget::new(AunitKind::Prog, "/NS/ZPROG").is_err());
    assert!(AunitTarget::new(AunitKind::Fugr, "ZFG_ABCDEFGHIJKLMNOPQRSTUVW").is_err());
    assert!(AunitTarget::new(AunitKind::Clas, "ZCL a").is_err());
    assert_eq!(
        AunitTarget::new(AunitKind::Clas, "ycl_x").unwrap().name,
        "YCL_X"
    );
}

#[test]
fn the_scope_is_exactly_one_of_an_object_a_package_and_a_request() {
    assert!(matches!(
        Scope::parse(Some((AunitKind::Prog, "ZP")), None, None),
        Ok(Scope::Object(_))
    ));
    assert_eq!(
        Scope::parse(None, Some("$tmp"), None).unwrap(),
        Scope::Package("$TMP".to_owned())
    );
    assert_eq!(
        Scope::parse(None, None, Some("devk900001")).unwrap(),
        Scope::Request("DEVK900001".to_owned())
    );
    assert!(Scope::parse(None, None, None).is_err());
    assert!(Scope::parse(Some((AunitKind::Prog, "ZP")), Some("ZPKG"), None).is_err());
    assert!(Scope::parse(None, Some("ZPKG"), Some("DEVK900001")).is_err());
    assert!(Scope::parse(None, Some("Z PKG"), None).is_err());
    assert!(Scope::parse(None, Some("ZPKG'"), None).is_err());
    assert!(Scope::parse(None, None, Some("DEV K1")).is_err());
}

#[test]
fn a_failed_run_lists_every_alert_with_its_place_and_drops_no_alert_entries() {
    let result = parse_run_result(FAILED).unwrap();
    let lines: Vec<String> = result.alerts.iter().map(|alert| alert.line()).collect();

    assert_eq!(result.tests, 3);
    assert_eq!(
        lines,
        [
            "failedAssertion critical ZCL_TEST LTCL_TEST->DIVIDE: Critical Assertion Error: 'Divide by two' Expected [2] Actual [3]",
            "exception fatal ZCL_TEST LTCL_TEST->RAISE: Runtime Error CX_SY_ZERODIVIDE & friends",
            "warning tolerable ZCL_TEST LTCL_TEST: Test class without assertion",
        ]
    );
}

#[test]
fn a_clean_run_has_tests_and_no_alerts_and_an_empty_reply_has_neither() {
    let clean = parse_run_result(CLEAN).unwrap();

    assert_eq!((clean.tests, clean.alerts.len()), (2, 0));

    let none = parse_run_result(NO_TESTS).unwrap();

    assert_eq!((none.tests, none.alerts.len()), (0, 0));
    assert!(parse_run_result("<a><b></a>").is_err());
}

#[test]
fn an_alert_text_is_one_bounded_printable_line() {
    let long = "x".repeat(900);
    let xml = format!(
        r#"<runResult><program name="P"><testClasses><testClass name="C"><alerts><alert kind="k" severity="s"><title>{long}
line two</title></alert></alerts></testClass></testClasses></program></runResult>"#
    );
    let text = &parse_run_result(&xml).unwrap().alerts[0].text;

    assert!(text.ends_with("..."));
    assert!(text.chars().count() <= 403);
    assert!(!text.contains('\n'));
}

// ---- aunit: the run ------------------------------------------------------------------------------

fn object_scope(kind: AunitKind, name: &str) -> Scope {
    Scope::Object(AunitTarget::new(kind, name).unwrap())
}

#[test]
fn a_class_with_passing_tests_exits_0_and_sends_the_pinned_requests() {
    let mut sap = transport(vec![client("T"), ok("<class/>"), ok(CLEAN)]);
    let outcome = aunit_run(&mut sap, &object_scope(AunitKind::Clas, "ZCL_TEST"), "100").unwrap();

    assert_eq!(outcome.status, Some(Status::Active));
    assert_eq!(outcome.exit, 0);
    assert_eq!(outcome.stdout, "TESTS: 2 methods, 0 alerts\nAUNIT: ACTIVE");
    assert_eq!(sap.calls[1].method, "GET");
    assert_eq!(sap.calls[1].uri, "/sap/bc/adt/oo/classes/zcl_test");
    // SAP_BASIS 758 answers 406 to `application/xml` on the object resource.
    assert_eq!(sap.calls[1].headers["Accept"], "*/*");
    assert_eq!(sap.calls[2].uri, "/sap/bc/adt/abapunit/testruns");
    assert!(body(&sap.calls[2]).contains(r#"adtcore:uri="/sap/bc/adt/oo/classes/zcl_test""#));
    assert!(
        body(&sap.calls[2])
            .contains(r#"<testRiskLevels harmless="true" dangerous="false" critical="false"/>"#)
    );
}

#[test]
fn alerts_make_the_run_fail_with_exit_1() {
    let mut sap = transport(vec![client("T"), ok("<class/>"), ok(FAILED)]);
    let outcome = aunit_run(&mut sap, &object_scope(AunitKind::Clas, "ZCL_TEST"), "100").unwrap();

    assert_eq!(outcome.status, Some(Status::Rejected));
    assert_eq!(outcome.exit, 1);
    assert!(
        outcome
            .stdout
            .ends_with("TESTS: 3 methods, 3 alerts\nAUNIT: REJECTED")
    );
    assert!(
        outcome
            .stdout
            .starts_with("failedAssertion critical ZCL_TEST LTCL_TEST->DIVIDE:")
    );
}

#[test]
fn a_run_without_test_methods_warns_and_passes() {
    let mut sap = transport(vec![client("T"), ok("<prog/>"), ok(NO_TESTS)]);
    let outcome = aunit_run(&mut sap, &object_scope(AunitKind::Prog, "ZTEST"), "100").unwrap();

    assert_eq!(outcome.exit, 0);
    assert!(
        outcome
            .stdout
            .starts_with("W: no test method ran for PROG ZTEST")
    );
}

#[test]
fn an_object_that_does_not_exist_is_rejected_and_runs_nothing() {
    let mut sap = transport(vec![client("T"), Ok(Response::empty(404))]);
    let outcome = aunit_run(&mut sap, &object_scope(AunitKind::Fugr, "ZFG"), "100").unwrap();

    assert_eq!(outcome.exit, 1);
    assert!(
        outcome
            .stdout
            .starts_with("REJECTED: FUGR ZFG does not exist")
    );
    assert_eq!(sap.calls.len(), 2);
    assert_eq!(sap.calls[1].uri, "/sap/bc/adt/functions/groups/zfg");
}

#[test]
fn a_productive_client_runs_no_test() {
    let mut sap = transport(vec![client("P")]);
    let error = aunit_run(&mut sap, &object_scope(AunitKind::Prog, "ZTEST"), "100").unwrap_err();

    assert!(matches!(&error, QualityError::Rejected(message) if message.contains("productive")));
    assert_eq!(sap.calls.len(), 1);
}

#[test]
fn a_package_batch_reads_tadir_skips_includes_and_prints_a_summary() {
    let tadir = ok(preview(&[
        (
            "OBJECT",
            &[
                Some("CLAS"),
                Some("PROG"),
                Some("PROG"),
                Some("DEVC"),
                Some("PROG"),
            ],
        ),
        (
            "OBJ_NAME",
            &[
                Some("ZCL_TEST"),
                Some("ZTEST_MAIN"),
                Some("ZTEST_INC"),
                Some("ZPKG"),
                Some("SAPX"),
            ],
        ),
    ]));
    let trdir = ok(preview(&[
        ("NAME", &[Some("ZTEST_INC"), Some("ZTEST_MAIN")]),
        ("SUBC", &[Some("I"), Some("1")]),
    ]));
    let mut sap = transport(vec![
        client("T"),
        tadir,
        trdir,
        // PROG ZTEST_MAIN passes, CLAS ZCL_TEST fails (sorted by type and name: CLAS first).
        ok("<class/>"),
        ok(FAILED),
        ok("<prog/>"),
        ok(CLEAN),
    ]);
    let outcome = aunit_run(&mut sap, &Scope::Package("ZPKG".to_owned()), "100").unwrap();

    assert_eq!(
        body(&sap.calls[1]),
        "SELECT OBJECT, OBJ_NAME FROM TADIR WHERE PGMID = 'R3TR' AND DEVCLASS = 'ZPKG' AND OBJECT IN ( 'PROG', 'CLAS', 'FUGR' ) AND DELFLAG = ' '"
    );
    assert_eq!(
        body(&sap.calls[2]),
        "SELECT NAME, SUBC FROM TRDIR WHERE NAME IN ( 'ZTEST_INC', 'ZTEST_MAIN' )"
    );
    assert_eq!(outcome.exit, 1);
    assert!(outcome.stdout.contains("== CLAS ZCL_TEST =="));
    assert!(outcome.stdout.contains("== PROG ZTEST_MAIN =="));
    assert!(!outcome.stdout.contains("ZTEST_INC"));
    assert!(outcome.stdout.contains("SUMMARY: 1 OK, 1 FAILED"));
    assert!(
        outcome
            .stdout
            .contains("FAILED CLAS ZCL_TEST: failedAssertion critical"),
        "{}",
        outcome.stdout
    );
}

#[test]
fn an_exception_on_one_object_of_a_batch_counts_as_failed_and_the_batch_goes_on() {
    let tadir = ok(preview(&[
        ("OBJECT", &[Some("CLAS"), Some("CLAS")]),
        ("OBJ_NAME", &[Some("ZCL_A"), Some("ZCL_B")]),
    ]));
    let mut sap = transport(vec![
        client("T"),
        tadir,
        ok("<class/>"),
        Err(TransportError::PostSendUncertain("reset".to_owned())),
        ok("<class/>"),
        ok(CLEAN),
    ]);
    let outcome = aunit_run(&mut sap, &Scope::Package("ZPKG".to_owned()), "100").unwrap();

    assert_eq!(outcome.exit, 1);
    assert!(outcome.stdout.contains("SUMMARY: 1 OK, 1 FAILED"));
    assert!(outcome.stdout.contains("FAILED CLAS ZCL_A"));
}

#[test]
fn an_empty_batch_is_not_a_pass() {
    let tadir = ok(preview(&[("OBJECT", &[]), ("OBJ_NAME", &[])]));
    let mut sap = transport(vec![client("T"), tadir]);
    let outcome = aunit_run(&mut sap, &Scope::Package("ZPKG".to_owned()), "100").unwrap();

    assert_eq!(outcome.exit, 1);
    assert!(outcome.stdout.contains("SUMMARY: 0 OK, 0 FAILED"));
}

#[test]
fn a_request_batch_maps_class_parts_and_reports_what_it_cannot_map() {
    let tasks = ok(preview(&[(
        "TRKORR",
        &[Some("DEVK900001"), Some("DEVK900002")],
    )]));
    let rows = ok(preview(&[
        (
            "PGMID",
            &[
                Some("R3TR"),
                Some("LIMU"),
                Some("LIMU"),
                Some("LIMU"),
                Some("R3TR"),
            ],
        ),
        (
            "OBJECT",
            &[
                Some("PROG"),
                Some("METH"),
                Some("CPUB"),
                Some("REPS"),
                Some("TABL"),
            ],
        ),
        (
            "OBJ_NAME",
            &[
                Some("ZTEST_MAIN"),
                Some("ZCL_TEST                      RUN"),
                Some("ZCL_TEST"),
                Some("ZPROG_X"),
                Some("ZTAB"),
            ],
        ),
    ]));
    let trdir = ok(preview(&[
        ("NAME", &[Some("ZTEST_MAIN")]),
        ("SUBC", &[Some("1")]),
    ]));
    let mut sap = transport(vec![
        client("T"),
        tasks,
        rows,
        trdir,
        ok("<class/>"),
        ok(CLEAN),
        ok("<prog/>"),
        ok(CLEAN),
    ]);
    let outcome = aunit_run(&mut sap, &Scope::Request("DEVK900001".to_owned()), "100").unwrap();

    assert_eq!(
        body(&sap.calls[1]),
        "SELECT TRKORR FROM E070 WHERE TRKORR = 'DEVK900001' OR STRKORR = 'DEVK900001'"
    );
    assert_eq!(
        body(&sap.calls[2]),
        "SELECT PGMID, OBJECT, OBJ_NAME FROM E071 WHERE TRKORR IN ( 'DEVK900001', 'DEVK900002' )"
    );
    assert_eq!(outcome.exit, 0, "{}", outcome.stdout);
    assert!(outcome.stdout.starts_with("W: 1 request entries"));
    assert!(outcome.stdout.contains("== CLAS ZCL_TEST =="));
    assert!(outcome.stdout.contains("== PROG ZTEST_MAIN =="));
    assert!(outcome.stdout.contains("SUMMARY: 2 OK, 0 FAILED"));
}

#[test]
fn an_unknown_request_is_rejected() {
    let tasks = ok(preview(&[("TRKORR", &[])]));
    let mut sap = transport(vec![client("T"), tasks]);
    let error = aunit_run(&mut sap, &Scope::Request("DEVK900009".to_owned()), "100").unwrap_err();

    assert!(
        matches!(&error, QualityError::Rejected(message) if message.contains("does not exist"))
    );
}
