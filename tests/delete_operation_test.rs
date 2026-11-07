//! The shared delete operation: plan identity, guards, ordering and the batch state machine,
//! against a fake backend. The fake counts every call, so "no delete without a matching
//! simulation" and "no retry" are asserted, not assumed. Ported from `tools/test_rfc_deploy.py`.

use abapreach::cli::{DeleteKind, DeleteTarget};
use abapreach::operation::delete::{
    Context, DeleteBackend, DeleteReply, DeleteReport, DeleteStatus, InactiveFact, ObjectFacts,
    Plan, Reconcile, SourceFact, TrLock, execute, execute_until, run, simulate,
};
use abapreach::policy::{Confirmer, ForeignInactive, Policies};
use std::collections::{BTreeMap, BTreeSet};

struct Yes(bool);

impl Confirmer for Yes {
    fn interactive(&self) -> bool {
        true
    }

    fn confirm(&self, _prompt: &str) -> bool {
        self.0
    }
}

#[derive(Default)]
struct Fake {
    facts: BTreeMap<String, Option<ObjectFacts>>,
    replies: BTreeMap<String, DeleteReply>,
    reconcile: Option<Reconcile>,
    inspected: Vec<String>,
    deleted: Vec<(String, Option<String>)>,
    reconciled: Vec<String>,
    includes: BTreeSet<String>,
    include_asked: Vec<String>,
}

impl Fake {
    fn include(mut self, name: &str) -> Self {
        self.includes.insert(name.to_owned());

        self
    }

    fn with(mut self, name: &str, facts: ObjectFacts) -> Self {
        self.facts.insert(name.to_owned(), Some(facts));

        self
    }

    fn absent(mut self, name: &str) -> Self {
        self.facts.insert(name.to_owned(), None);

        self
    }

    fn reply(mut self, name: &str, reply: DeleteReply) -> Self {
        self.replies.insert(name.to_owned(), reply);

        self
    }

    fn names_deleted(&self) -> Vec<&str> {
        self.deleted.iter().map(|(name, _)| name.as_str()).collect()
    }
}

impl DeleteBackend for Fake {
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        self.inspected.push(name.to_owned());

        match self.facts.get(name) {
            Some(facts) => Ok(facts.clone()),
            None if kind == DeleteKind::Func => Ok(Some(func_facts("ZFG_OK"))),
            None => Ok(Some(facts_of(name))),
        }
    }

    fn delete(&mut self, _kind: DeleteKind, name: &str, tr: Option<&str>) -> DeleteReply {
        self.deleted.push((name.to_owned(), tr.map(str::to_owned)));

        self.replies
            .get(name)
            .cloned()
            .unwrap_or(DeleteReply::Deleted)
    }

    fn is_include(&mut self, _kind: DeleteKind, name: &str) -> bool {
        self.include_asked.push(name.to_owned());

        self.includes.contains(name)
    }

    fn reconcile(&mut self, _kind: DeleteKind, name: &str) -> Reconcile {
        self.reconciled.push(name.to_owned());

        self.reconcile.clone().expect("reconcile was not scripted")
    }
}

fn facts_of(name: &str) -> ObjectFacts {
    ObjectFacts {
        package: "$TMP".to_owned(),
        author: "DEVUSER".to_owned(),
        group: None,
        sources: vec![SourceFact::active("MAIN", &[&format!("REPORT {name}.")])],
        contents: Vec::new(),
        where_used: Vec::new(),
        inactive: InactiveFact::Absent,
        tr: TrLock::Local,
    }
}

fn func_facts(group: &str) -> ObjectFacts {
    ObjectFacts {
        group: Some(group.to_owned()),
        ..facts_of("FM")
    }
}

fn target(text: &str) -> DeleteTarget {
    text.parse().expect("target")
}

fn targets(texts: &[&str]) -> Vec<DeleteTarget> {
    texts.iter().map(|text| target(text)).collect()
}

fn policies(foreign: ForeignInactive) -> Policies {
    Policies {
        foreign_inactive: foreign,
        ..Policies::default()
    }
}

fn context<'a>(policies: &'a Policies, confirmer: &'a dyn Confirmer) -> Context<'a> {
    Context {
        conn: "DEV_100",
        user: "DEVUSER",
        policies,
        confirmer,
    }
}

fn sim(fake: &mut Fake, texts: &[&str], tr: Option<&str>) -> DeleteReport {
    let policies = Policies::default();

    simulate(fake, &context(&policies, &Yes(false)), &targets(texts), tr).expect("simulate")
}

fn exec(fake: &mut Fake, texts: &[&str], tr: Option<&str>, plan: &Plan) -> DeleteReport {
    let policies = Policies::default();

    execute(
        fake,
        &context(&policies, &Yes(false)),
        &targets(texts),
        tr,
        plan,
    )
    .expect("execute")
}

fn status_of(report: &DeleteReport, name: &str) -> DeleteStatus {
    report
        .results
        .iter()
        .find(|result| result.name == name)
        .unwrap_or_else(|| panic!("no result for {name}"))
        .status
}

fn reason_of(report: &DeleteReport, name: &str) -> String {
    report
        .results
        .iter()
        .find(|result| result.name == name)
        .and_then(|result| result.reason.clone())
        .unwrap_or_default()
}

fn plan_of(report: &DeleteReport) -> Plan {
    report.plan.clone().expect("plan")
}

// ---------- plan generation and identity ----------

#[test]
fn simulate_never_deletes_and_prints_plan_hash() {
    let mut fake = Fake::default();
    let report = sim(&mut fake, &["CLAS:ZCL_A", "PROG:ZP"], None);

    assert!(fake.deleted.is_empty());
    assert_eq!(report.outcome.exit, 0);
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 2 OK, 0 FAILED, 0 SKIPPED")
    );
    assert!(report.outcome.stdout.contains("== CLAS ZCL_A =="));
    assert!(report.outcome.stdout.contains("DELETE_SIM: SIMULATED_OK"));
    assert!(report.outcome.stdout.contains("plan hash "));
    assert!(report.all_simulated_ok());
}

#[test]
fn plan_line_is_the_python_line_with_tr_dash_for_local() {
    let mut fake = Fake::default();
    let report = sim(&mut fake, &["PROG:ZP"], None);

    assert!(
        report
            .outcome
            .stdout
            .contains("DELETE PROG ZP package $TMP author DEVUSER TR -")
    );
}

#[test]
fn plan_hash_changes_with_package_author_tr_and_active_source() {
    let base = |mutate: &dyn Fn(&mut ObjectFacts)| {
        let mut facts = facts_of("ZP");
        mutate(&mut facts);
        let mut fake = Fake::default().with("ZP", facts);

        plan_of(&sim(&mut fake, &["PROG:ZP"], None)).objects[0]
            .2
            .clone()
    };
    let hash = base(&|_| {});

    assert_eq!(hash, base(&|_| {}), "hash is deterministic");
    assert_ne!(hash, base(&|facts| facts.package = "ZPKG".to_owned()));
    assert_ne!(hash, base(&|facts| facts.author = "OTHER".to_owned()));
    assert_ne!(
        hash,
        base(&|facts| facts.sources = vec![SourceFact::active("MAIN", &["REPORT zp.", "WRITE 1."])])
    );
}

#[test]
fn where_used_is_reported_but_not_hashed() {
    let mut plain = Fake::default();
    let mut used = Fake::default().with(
        "ZP",
        ObjectFacts {
            where_used: vec!["ZCALLER".to_owned()],
            ..facts_of("ZP")
        },
    );
    let first = sim(&mut plain, &["PROG:ZP"], None);
    let second = sim(&mut used, &["PROG:ZP"], None);

    assert!(second.outcome.stdout.contains("WHERE-USED: ZCALLER"));
    assert_eq!(plan_of(&first).objects, plan_of(&second).objects);
}

#[test]
fn source_markers_active_and_no_active_differ() {
    let absent = ObjectFacts {
        sources: vec![SourceFact::no_active("MAIN")],
        ..facts_of("ZP")
    };
    let mut with_source = Fake::default();
    let mut without = Fake::default().with("ZP", absent);
    let first = plan_of(&sim(&mut with_source, &["PROG:ZP"], None));
    let second = plan_of(&sim(&mut without, &["PROG:ZP"], None));

    assert_ne!(first.objects[0].2, second.objects[0].2);
}

#[test]
fn class_includes_are_hashed_and_part_of_the_source_set() {
    let with_includes = |lines: &[&str]| ObjectFacts {
        sources: vec![
            SourceFact::active("MAIN", &["CLASS zcl_a DEFINITION."]),
            SourceFact::active("CCIMP", lines),
        ],
        contents: vec!["INCLUDE ZCL_A=======CCIMP".to_owned()],
        ..facts_of("ZCL_A")
    };
    let hash = |lines: &[&str]| {
        let mut fake = Fake::default().with("ZCL_A", with_includes(lines));

        plan_of(&sim(&mut fake, &["CLAS:ZCL_A"], None)).objects[0]
            .2
            .clone()
    };

    assert_ne!(hash(&["a"]), hash(&["b"]));
}

#[test]
fn fugr_contents_are_hashed_unless_in_the_batch() {
    let fugr = ObjectFacts {
        contents: vec!["FUNC ZFM_A".to_owned()],
        sources: Vec::new(),
        ..facts_of("ZFG")
    };
    let hash = |texts: &[&str], contents: &[&str]| {
        let mut fake = Fake::default()
            .with(
                "ZFG",
                ObjectFacts {
                    contents: contents.iter().map(|text| (*text).to_owned()).collect(),
                    ..fugr.clone()
                },
            )
            .with("ZFM_A", func_facts("ZFG"));
        let report = sim(&mut fake, texts, None);

        plan_of(&report)
            .objects
            .iter()
            .find(|object| object.1 == "ZFG")
            .expect("fugr row")
            .2
            .clone()
    };
    let alone = hash(&["FUGR:ZFG"], &["FUNC ZFM_A"]);

    assert_ne!(alone, hash(&["FUGR:ZFG"], &["FUNC ZFM_A", "FUNC ZFM_B"]));
    // A member that is itself in the batch is not a "plan changed" when it is gone later.
    assert_eq!(
        hash(&["FUGR:ZFG", "FUNC:ZFM_A"], &["FUNC ZFM_A"]),
        hash(&["FUGR:ZFG", "FUNC:ZFM_A"], &[])
    );
    assert!(
        sim(&mut Fake::default(), &["FUGR:ZFG"], None)
            .outcome
            .stdout
            .contains("SUMMARY: 1 OK")
    );
}

#[test]
fn fugr_has_no_source_hash_line() {
    let facts = ObjectFacts {
        sources: Vec::new(),
        ..facts_of("ZFG")
    };
    let mut fake = Fake::default().with("ZFG", facts);

    assert!(sim(&mut fake, &["FUGR:ZFG"], None).all_simulated_ok());
}

#[test]
fn simulate_writes_only_simulated_ok_objects_into_the_plan() {
    let mut fake = Fake::default().with(
        "ZCL_B",
        ObjectFacts {
            tr: TrLock::Locked {
                request: "DEVK1".to_owned(),
                owner: "OTHER".to_owned(),
                own: false,
            },
            ..facts_of("ZCL_B")
        },
    );
    let report = sim(&mut fake, &["CLAS:ZCL_A", "CLAS:ZCL_B"], Some("devk1"));
    let plan = plan_of(&report);

    assert_eq!(report.outcome.exit, 1);
    assert_eq!(plan.conn, "DEV_100");
    assert_eq!(plan.tr.as_deref(), Some("DEVK1"));
    assert_eq!(plan.objects.len(), 1);
    assert_eq!(
        (plan.objects[0].0.as_str(), plan.objects[0].1.as_str()),
        ("CLAS", "ZCL_A")
    );
    assert!(!report.all_simulated_ok());
}

#[test]
fn simulate_states_active_and_absent_guards() {
    let report = sim(&mut Fake::default(), &["PROG:ZP"], None);
    let out = &report.outcome.stdout;

    for active in [
        "Z/Y scope",
        "self-guard",
        "lock request agreement at execute",
        "foreign_inactive",
        "plan hash",
    ] {
        assert!(out.contains(active), "missing active guard {active}: {out}");
    }

    for absent in [
        "system change option",
        "CCNOCLIIND",
        "SE06",
        "software component",
        "server-side self-guard",
    ] {
        assert!(out.contains(absent), "missing absent guard {absent}: {out}");
    }
}

// ---------- plan binding on real delete ----------

fn plan_for(texts: &[&str], tr: Option<&str>) -> Plan {
    plan_of(&sim(&mut Fake::default(), texts, tr))
}

#[test]
fn delete_with_matching_plan_runs_and_sends_the_resolved_tr() {
    let locked = ObjectFacts {
        package: "ZPKG".to_owned(),
        tr: TrLock::Unlocked,
        ..facts_of("ZCL_A")
    };
    let mut fake = Fake::default().with("ZCL_A", locked);
    let simulated = plan_of(&sim(&mut fake, &["CLAS:ZCL_A"], Some("DEVK1")));
    let report = exec(&mut fake, &["CLAS:ZCL_A"], Some("DEVK1"), &simulated);

    assert_eq!(report.outcome.exit, 0);
    assert_eq!(
        fake.deleted,
        vec![("ZCL_A".to_owned(), Some("DEVK1".to_owned()))]
    );
    assert!(report.outcome.stdout.contains("DELETE: DELETED"));
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 1 OK, 0 FAILED, 0 SKIPPED")
    );
}

#[test]
fn delete_rejects_an_object_whose_live_state_drifted_since_simulate() {
    let plan = plan_for(&["PROG:ZP"], None);
    let mut fake = Fake::default().with(
        "ZP",
        ObjectFacts {
            sources: vec![SourceFact::active("MAIN", &["REPORT zp.", "WRITE 9."])],
            ..facts_of("ZP")
        },
    );
    let report = exec(&mut fake, &["PROG:ZP"], None, &plan);

    assert!(fake.deleted.is_empty());
    assert_eq!(status_of(&report, "ZP"), DeleteStatus::Rejected);
    assert!(reason_of(&report, "ZP").contains("Plan changed since simulate"));
    assert_eq!(report.outcome.exit, 1);
}

#[test]
fn delete_with_a_plan_hash_edited_by_hand_is_rejected() {
    let mut plan = plan_for(&["PROG:ZP"], None);
    plan.objects[0].2 = "0".repeat(64);
    let mut fake = Fake::default();
    let report = exec(&mut fake, &["PROG:ZP"], None, &plan);

    assert!(fake.deleted.is_empty());
    assert_eq!(status_of(&report, "ZP"), DeleteStatus::Rejected);
}

#[test]
fn plan_must_match_conn_object_set_and_tr_exactly() {
    let plan = plan_for(&["FUNC:ZFM_X", "FUGR:ZFG_X"], None);
    let policies = Policies::default();
    let other_conn = Context {
        conn: "FED_100",
        ..context(&policies, &Yes(false))
    };
    let check = |ctx: &Context, texts: &[&str], tr: Option<&str>| {
        let mut fake = Fake::default();
        let result = execute(&mut fake, ctx, &targets(texts), tr, &plan);

        assert!(fake.deleted.is_empty() && fake.inspected.is_empty());

        result.expect_err("must STOP")
    };
    let same = context(&policies, &Yes(false));

    assert!(
        check(&same, &["FUNC:ZFM_X", "FUGR:ZFG_X", "CLAS:ZCL_A"], None)
            .contains("plan does not match")
    );
    assert!(check(&same, &["FUNC:ZFM_X"], None).contains("plan does not match"));
    assert!(
        check(&same, &["FUNC:ZFM_X", "FUGR:ZFG_X"], Some("DEVK1")).contains("plan does not match")
    );
    assert!(
        check(&other_conn, &["FUNC:ZFM_X", "FUGR:ZFG_X"], None).contains("plan does not match")
    );
}

#[test]
fn plan_matches_in_any_argument_order_and_runs_in_kind_order() {
    let plan = plan_for(&["FUGR:ZFG_X", "FUNC:ZFM_X"], None);
    let mut fake = Fake::default();
    let report = exec(&mut fake, &["FUNC:ZFM_X", "FUGR:ZFG_X"], None, &plan);

    assert_eq!(report.outcome.exit, 0);
    assert_eq!(fake.names_deleted(), ["ZFM_X", "ZFG_X"]);
}

#[test]
fn every_actual_delete_needs_a_simulated_row_with_that_exact_object() {
    let mut plan = plan_for(&["PROG:ZP"], None);
    plan.objects[0].1 = "ZQ".to_owned();
    let policies = Policies::default();
    let mut fake = Fake::default();
    let result = execute(
        &mut fake,
        &context(&policies, &Yes(false)),
        &targets(&["PROG:ZP"]),
        None,
        &plan,
    );

    assert!(result.is_err());
    assert!(fake.deleted.is_empty());
}

#[test]
fn plan_file_round_trips_and_a_bad_file_stops() {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("plan.json");
    let policies = Policies::default();
    let ctx = context(&policies, &Yes(false));
    let mut fake = Fake::default();
    let written = run(
        &mut fake,
        &ctx,
        &targets(&["CLAS:ZCL_A"]),
        None,
        true,
        &path,
    );

    assert_eq!(written.exit, 0, "{}", written.stderr);
    assert!(written.stdout.contains("plan -> "));

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("plan")).expect("json");

    assert_eq!(json["conn"], "DEV_100");
    assert!(json["tr"].is_null());
    assert_eq!(json["objects"][0][0], "CLAS");

    let deleted = run(
        &mut fake,
        &ctx,
        &targets(&["CLAS:ZCL_A"]),
        None,
        false,
        &path,
    );

    assert_eq!(deleted.exit, 0, "{}", deleted.stderr);
    assert_eq!(fake.names_deleted(), ["ZCL_A"]);

    let before = fake.deleted.len();
    let missing = run(
        &mut fake,
        &ctx,
        &targets(&["CLAS:ZCL_A"]),
        None,
        false,
        &directory.path().join("absent.json"),
    );

    assert_eq!(missing.exit, 2);
    assert!(missing.stderr.starts_with("STOP: plan file cannot be read"));
    assert_eq!(fake.deleted.len(), before);

    std::fs::write(&path, "not json").expect("write");

    let broken = run(
        &mut fake,
        &ctx,
        &targets(&["CLAS:ZCL_A"]),
        None,
        false,
        &path,
    );

    assert_eq!(broken.exit, 2);
    assert_eq!(fake.deleted.len(), before);
}

#[test]
fn simulate_does_not_write_a_plan_file_when_the_arguments_stop() {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("plan.json");
    let policies = Policies::default();
    let mut fake = Fake::default();
    let outcome = run(
        &mut fake,
        &context(&policies, &Yes(false)),
        &targets(&["CLAS:ZCL_A", "CLAS:zcl_a"]),
        None,
        true,
        &path,
    );

    assert_eq!(outcome.exit, 2);
    assert!(outcome.stderr.contains("duplicate object"));
    assert!(!path.exists());
    assert!(fake.inspected.is_empty());
}

#[test]
fn argument_validation_stops_before_any_backend_call() {
    let policies = Policies::default();
    let ctx = context(&policies, &Yes(false));
    let stop = |texts: &[&str], tr: Option<&str>| {
        let mut fake = Fake::default();
        let error = simulate(&mut fake, &ctx, &targets(texts), tr).expect_err("STOP");

        assert!(fake.inspected.is_empty());

        error
    };

    assert!(stop(&["CLAS:ZCL_A", "CLAS:zcl_a"], None).contains("duplicate"));
    assert!(stop(&["CLAS:ZCL A"], None).contains("object name"));
    assert!(stop(&[&format!("FUGR:Z{}", "A".repeat(26))], None).contains("26"));
    assert!(stop(&["CLAS:ZCL_A"], Some("DEV K1")).contains("invalid --tr"));
}

// ---------- scope and self-guard ----------

fn rejected_reason(texts: &[&str], fake: Fake, name: &str) -> String {
    let mut fake = fake;
    let report = sim(&mut fake, texts, None);

    assert_eq!(status_of(&report, name), DeleteStatus::Rejected, "{name}");
    assert_eq!(report.outcome.exit, 1);

    reason_of(&report, name)
}

#[test]
fn only_z_and_y_names_are_in_scope() {
    for name in ["ZP", "YP", "zp", "Z", "ZCL_A", "YCL_A"] {
        let report = sim(&mut Fake::default(), &[&format!("PROG:{name}")], None);

        assert!(report.all_simulated_ok(), "{name}");
    }

    for name in ["SAPMF02D", "RSABAPPROGRAM", "/ABC/ZP", "ABC"] {
        let reason = rejected_reason(&[&format!("PROG:{name}")], Fake::default(), name);

        assert!(reason.contains("Z/Y"), "{name}: {reason}");
    }
}

#[test]
fn out_of_scope_name_is_rejected_without_any_lookup() {
    let mut fake = Fake::default();
    let report = sim(&mut fake, &["PROG:SAPMF02D"], None);

    assert_eq!(status_of(&report, "SAPMF02D"), DeleteStatus::Rejected);
    assert!(fake.inspected.is_empty());
}

#[test]
fn func_needs_a_z_or_y_function_group() {
    let reason = rejected_reason(
        &["FUNC:ZFM_X"],
        Fake::default().with("ZFM_X", func_facts("SYST")),
        "ZFM_X",
    );

    assert!(reason.contains("function group"), "{reason}");

    let mut fake = Fake::default().with("ZFM_X", func_facts("ZFG_OK"));

    assert!(sim(&mut fake, &["FUNC:ZFM_X"], None).all_simulated_ok());

    let unknown = ObjectFacts {
        group: None,
        ..func_facts("")
    };

    assert!(
        rejected_reason(
            &["FUNC:ZFM_X"],
            Fake::default().with("ZFM_X", unknown),
            "ZFM_X"
        )
        .contains("function group")
    );
}

#[test]
fn self_guard_matches_the_check_delete_self_baseline() {
    // Names the ABAP baseline rejects, per kind (gc_self_fg, gc_self_class_cp, gc_self_ddic_cp,
    // SAPLYFG_UTILS, LYFG_UTILS*), plus the spec's YFM_UTILS_* FUNC.
    let rejected: [(&str, &str); 11] = [
        ("FUGR", "YFG_UTILS"),
        ("FUNC", "YFM_UTILS_SOURCE_DEPLOY"),
        ("FUNC", "YFM_UTILS_ANYTHING"),
        ("CLAS", "YCL_UTILS_SOURCE_DEPLOY"),
        ("CLAS", "YCL_UTILS_SOURCE_DEPLOY_X"),
        ("CLAS", "YCL_UTILS_DDIC_DEPLOY"),
        ("PROG", "SAPLYFG_UTILS"),
        ("PROG", "LYFG_UTILS01"),
        ("PROG", "LYFG_UTILSTOP"),
        ("PROG", "YCL_UTILS_SOURCE_DEPLOY_CP"),
        ("PROG", "YCL_UTILS_DDIC_DEPLOY_CCIMP"),
    ];

    for (kind, name) in rejected {
        let reason = rejected_reason(&[&format!("{kind}:{name}")], Fake::default(), name);

        assert!(reason.contains("Self-guard"), "{kind} {name}: {reason}");
    }

    let allowed: [(&str, &str); 6] = [
        ("FUGR", "YFG_UTILS_OTHER"),
        ("CLAS", "YCL_UTIL_OTHER"),
        ("CLAS", "YCL_UTILSX"),
        ("PROG", "ZLYFG_UTILS01"),
        ("PROG", "ZSAPLYFG_UTILS"),
        ("INTF", "YIF_ANY"),
    ];

    for (kind, name) in allowed {
        let report = sim(&mut Fake::default(), &[&format!("{kind}:{name}")], None);

        assert!(report.all_simulated_ok(), "{kind} {name}");
    }
}

#[test]
fn func_in_the_tool_function_group_is_rejected_even_with_another_name() {
    let reason = rejected_reason(
        &["FUNC:ZFM_PLAIN"],
        Fake::default().with("ZFM_PLAIN", func_facts("YFG_UTILS")),
        "ZFM_PLAIN",
    );

    assert!(reason.contains("Self-guard"), "{reason}");
}

#[test]
fn self_guard_is_not_waived_by_the_policy_or_a_plan() {
    let mut plan = plan_for(&["PROG:ZP"], None);
    plan.objects[0] = (
        "PROG".to_owned(),
        "SAPLYFG_UTILS".to_owned(),
        "x".to_owned(),
    );

    let policies = policies(ForeignInactive::Allow);
    let mut fake = Fake::default();
    let report = execute(
        &mut fake,
        &context(&policies, &Yes(true)),
        &targets(&["PROG:SAPLYFG_UTILS"]),
        None,
        &plan,
    )
    .expect("execute");

    assert!(fake.deleted.is_empty());
    assert_eq!(status_of(&report, "SAPLYFG_UTILS"), DeleteStatus::Rejected);
}

// ---------- transport request ownership ----------

fn locked(request: &str, owner: &str, own: bool) -> TrLock {
    TrLock::Locked {
        request: request.to_owned(),
        owner: owner.to_owned(),
        own,
    }
}

fn with_tr(name: &str, tr: TrLock) -> Fake {
    Fake::default().with(
        name,
        ObjectFacts {
            package: "ZPKG".to_owned(),
            tr,
            ..facts_of(name)
        },
    )
}

#[test]
fn own_lock_request_is_used_and_a_different_tr_argument_is_ignored_with_a_warning() {
    let mut fake = with_tr("ZP", locked("DEVK2", "DEVUSER", true));
    let report = sim(&mut fake, &["PROG:ZP"], Some("DEVK1"));

    assert!(report.all_simulated_ok());
    assert!(report.outcome.stdout.contains("TR DEVK2"));
    assert!(report.outcome.stdout.contains("DEVK1 ignored"));
}

#[test]
fn foreign_lock_owner_is_rejected_but_a_task_of_the_user_counts_as_own() {
    let reason = rejected_reason(
        &["PROG:ZP"],
        with_tr("ZP", locked("DEVK3", "OTHER", false)),
        "ZP",
    );

    assert!(
        reason.contains("DEVK3") && reason.contains("OTHER"),
        "{reason}"
    );

    let mut fake = with_tr("ZP", locked("DEVK3", "OTHER", true));

    assert!(sim(&mut fake, &["PROG:ZP"], None).all_simulated_ok());
}

#[test]
fn two_locking_requests_are_rejected() {
    let reason = rejected_reason(
        &["PROG:ZP"],
        with_tr(
            "ZP",
            TrLock::Multiple(vec!["DEVK1".to_owned(), "DEVK2".to_owned()]),
        ),
        "ZP",
    );

    assert!(reason.contains("more than one request"), "{reason}");
}

#[test]
fn unlocked_transportable_object_defers_the_request_to_lock_and_binds_an_explicit_tr() {
    let mut fake = with_tr("ZP", TrLock::Unlocked);

    assert!(sim(&mut fake, &["PROG:ZP"], None).all_simulated_ok());
    assert!(sim(&mut fake, &["PROG:ZP"], Some("DEVK1")).all_simulated_ok());
}

#[test]
fn local_object_ignores_tr_with_a_warning_and_sends_none() {
    let mut fake = Fake::default();
    let simulated = sim(&mut fake, &["PROG:ZP"], Some("DEVK1"));

    assert!(simulated.outcome.stdout.contains("ignored"));

    let plan = plan_of(&simulated);
    let _ = exec(&mut fake, &["PROG:ZP"], Some("DEVK1"), &plan);

    assert_eq!(fake.deleted, vec![("ZP".to_owned(), None)]);
}

// ---------- foreign_inactive ----------

fn with_inactive(name: &str, inactive: InactiveFact) -> Fake {
    Fake::default().with(
        name,
        ObjectFacts {
            inactive,
            ..facts_of(name)
        },
    )
}

fn sim_with(fake: &mut Fake, policies: &Policies, confirmer: &dyn Confirmer) -> DeleteReport {
    simulate(
        fake,
        &context(policies, confirmer),
        &targets(&["PROG:ZP"]),
        None,
    )
    .expect("simulate")
}

#[test]
fn foreign_inactive_default_rejects_and_own_inactive_passes() {
    let reason = rejected_reason(
        &["PROG:ZP"],
        with_inactive("ZP", InactiveFact::Owned("OTHER".to_owned())),
        "ZP",
    );

    assert!(
        reason.contains("inactive version of another user"),
        "{reason}"
    );
    assert!(reason.contains("OTHER"));

    let mut own = with_inactive("ZP", InactiveFact::Owned("devuser".to_owned()));

    assert!(sim(&mut own, &["PROG:ZP"], None).all_simulated_ok());
}

#[test]
fn foreign_inactive_allow_continues_with_a_warning() {
    let mut fake = with_inactive("ZP", InactiveFact::Owned("OTHER".to_owned()));
    let report = sim_with(&mut fake, &policies(ForeignInactive::Allow), &Yes(false));

    assert!(report.all_simulated_ok());
    assert!(report.outcome.stdout.contains("policy allow"));
}

#[test]
fn foreign_inactive_unreadable_list_fails_closed_unless_allow() {
    let mut fake = with_inactive(
        "ZP",
        InactiveFact::Unknown("version list failed".to_owned()),
    );

    assert_eq!(
        status_of(&sim_with(&mut fake, &Policies::default(), &Yes(true)), "ZP"),
        DeleteStatus::Rejected
    );
    assert!(sim_with(&mut fake, &policies(ForeignInactive::Allow), &Yes(false)).all_simulated_ok());
}

#[test]
fn foreign_inactive_ask_follows_the_terminal_and_is_reject_under_mcp() {
    let ask = policies(ForeignInactive::Ask);
    let foreign = || with_inactive("ZP", InactiveFact::Owned("OTHER".to_owned()));

    assert!(sim_with(&mut foreign(), &ask, &Yes(true)).all_simulated_ok());
    assert_eq!(
        status_of(&sim_with(&mut foreign(), &ask, &Yes(false)), "ZP"),
        DeleteStatus::Rejected
    );
    // The MCP server loads `for_mcp()`: ask counts as reject even when the user would say yes.
    assert_eq!(
        status_of(&sim_with(&mut foreign(), &ask.for_mcp(), &Yes(true)), "ZP"),
        DeleteStatus::Rejected
    );
}

#[test]
fn foreign_inactive_is_checked_again_at_delete_time() {
    let plan = plan_for(&["PROG:ZP"], None);
    let mut fake = with_inactive("ZP", InactiveFact::Owned("OTHER".to_owned()));
    let report = exec(&mut fake, &["PROG:ZP"], None, &plan);

    assert!(fake.deleted.is_empty());
    assert_eq!(status_of(&report, "ZP"), DeleteStatus::Rejected);
}

// ---------- order and batch state machine ----------

#[test]
fn execution_order_is_func_clas_intf_prog_fugr_stable_within_a_kind() {
    let texts = [
        "FUGR:ZFG_X",
        "PROG:ZP2",
        "CLAS:ZCL_Z",
        "INTF:ZIF_X",
        "FUNC:ZFM_X",
        "PROG:ZP1",
        "CLAS:ZCL_A",
    ];
    let mut fake = Fake::default().with("ZFM_X", func_facts("ZFG_X"));
    let simulated = sim(&mut fake, &texts, None);

    assert_eq!(
        fake.inspected,
        ["ZFM_X", "ZCL_Z", "ZCL_A", "ZIF_X", "ZP2", "ZP1", "ZFG_X"]
    );

    let plan = plan_of(&simulated);
    let _ = exec(&mut fake, &texts, None, &plan);

    assert_eq!(
        fake.names_deleted(),
        ["ZFM_X", "ZCL_Z", "ZCL_A", "ZIF_X", "ZP2", "ZP1", "ZFG_X"]
    );
}

#[test]
fn rejected_and_not_found_continue_the_batch() {
    let mut fake = with_tr("ZCL_B", locked("DEVK3", "OTHER", false)).absent("ZCL_C");
    let report = sim(
        &mut fake,
        &["CLAS:ZCL_A", "CLAS:ZCL_B", "CLAS:ZCL_C", "CLAS:ZCL_D"],
        None,
    );

    assert_eq!(report.outcome.exit, 1);
    assert_eq!(status_of(&report, "ZCL_B"), DeleteStatus::Rejected);
    assert_eq!(status_of(&report, "ZCL_C"), DeleteStatus::NotFound);
    assert_eq!(status_of(&report, "ZCL_D"), DeleteStatus::SimulatedOk);
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 2 OK, 2 FAILED, 0 SKIPPED")
    );
    assert!(
        report
            .outcome
            .stdout
            .contains("FAILED CLAS ZCL_C: NOT_FOUND")
    );
    assert!(report.outcome.stdout.contains("DELETE_SIM: NOT_FOUND"));
    assert!(report.outcome.stdout.contains("DELETE_SIM: REJECTED"));
}

/// The `FAILED <kind> <name>` lines of a report, one per object that did not end in success.
fn failed_lines(report: &DeleteReport) -> Vec<&str> {
    report
        .outcome
        .stdout
        .lines()
        .filter(|line| line.starts_with("FAILED "))
        .collect()
}

#[test]
fn only_an_object_that_did_not_succeed_gets_a_failed_line() {
    let mut fake = Fake::default();

    // Every object succeeds: the summary says "0 FAILED" but no object is listed as failed.
    let simulated = sim(&mut fake, &["CLAS:ZCL_A", "PROG:ZP"], None);

    assert_eq!(simulated.outcome.exit, 0);
    assert!(failed_lines(&simulated).is_empty(), "{simulated:?}");

    let deleted = real_delete(&mut fake, &["CLAS:ZCL_A", "PROG:ZP"]);

    assert_eq!(deleted.outcome.exit, 0);
    assert!(failed_lines(&deleted).is_empty(), "{deleted:?}");

    let mut fake = Fake::default().reply("ZCL_B", DeleteReply::Rejected("SAP refused".to_owned()));
    let mixed = real_delete(&mut fake, &["CLAS:ZCL_A", "CLAS:ZCL_B"]);

    assert_eq!(failed_lines(&mixed), ["FAILED CLAS ZCL_B: SAP refused"]);
}

#[test]
fn a_member_of_a_group_counts_as_in_the_batch_only_with_its_own_kind_and_name() {
    let hash = |texts: &[&str], contents: &[&str]| {
        let mut fake = Fake::default().with(
            "ZFG",
            ObjectFacts {
                contents: contents.iter().map(|text| (*text).to_owned()).collect(),
                sources: Vec::new(),
                ..facts_of("ZFG")
            },
        );

        plan_of(&sim(&mut fake, texts, None))
            .objects
            .iter()
            .find(|object| object.1 == "ZFG")
            .expect("fugr row")
            .2
            .clone()
    };
    let content = ["FUNC ZFM_A"];

    // Another function module in the batch does not stand in for ZFM_A, nor does an object of
    // another kind with the same name: the group's member list stays part of the plan.
    for batch in [
        ["FUGR:ZFG", "FUNC:ZFM_B"],
        ["FUGR:ZFG", "PROG:ZFM_A"],
        ["FUGR:ZFG", "CLAS:ZFM_A"],
    ] {
        assert_ne!(hash(&batch, &content), hash(&batch, &[]), "{batch:?}");
    }

    // ZFM_A itself in the batch does.
    assert_eq!(
        hash(&["FUGR:ZFG", "FUNC:ZFM_A"], &content),
        hash(&["FUGR:ZFG", "FUNC:ZFM_A"], &[])
    );
}

fn real_delete(fake: &mut Fake, texts: &[&str]) -> DeleteReport {
    let plan = plan_for(texts, None);

    exec(fake, texts, None, &plan)
}

#[test]
fn real_delete_continues_after_rejected_and_not_found() {
    let mut fake = Fake::default()
        .reply("ZCL_B", DeleteReply::Rejected("SAP refused".to_owned()))
        .reply("ZCL_C", DeleteReply::NotFound);
    let report = real_delete(
        &mut fake,
        &["CLAS:ZCL_A", "CLAS:ZCL_B", "CLAS:ZCL_C", "CLAS:ZCL_D"],
    );

    assert_eq!(fake.names_deleted(), ["ZCL_A", "ZCL_B", "ZCL_C", "ZCL_D"]);
    assert_eq!(report.outcome.exit, 1);
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 2 OK, 2 FAILED, 0 SKIPPED")
    );
    assert!(
        report
            .outcome
            .stdout
            .contains("FAILED CLAS ZCL_B: SAP refused")
    );
    assert!(
        report
            .outcome
            .stdout
            .contains("FAILED CLAS ZCL_C: NOT_FOUND")
    );
}

#[test]
fn partial_stops_the_batch_and_skips_the_rest_without_rollback() {
    let mut fake = Fake::default().reply("ZCL_B", DeleteReply::Partial("members left".to_owned()));
    let report = real_delete(&mut fake, &["CLAS:ZCL_A", "CLAS:ZCL_B", "CLAS:ZCL_C"]);

    assert_eq!(
        fake.names_deleted(),
        ["ZCL_A", "ZCL_B"],
        "no rollback, no retry"
    );
    assert_eq!(status_of(&report, "ZCL_B"), DeleteStatus::Partial);
    assert_eq!(status_of(&report, "ZCL_C"), DeleteStatus::Skipped);
    assert!(report.outcome.stdout.contains("DELETE: PARTIAL"));
    assert!(report.outcome.stdout.contains("SKIPPED CLAS ZCL_C"));
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 1 OK, 1 FAILED, 1 SKIPPED")
    );
    assert_eq!(report.outcome.exit, 1);
}

#[test]
fn uncertain_reply_is_reconciled_once_and_never_retried() {
    let mut fake = Fake::default().reply("ZCL_B", DeleteReply::Uncertain("timeout".to_owned()));
    fake.reconcile = Some(Reconcile::Gone);

    let report = real_delete(&mut fake, &["CLAS:ZCL_A", "CLAS:ZCL_B", "CLAS:ZCL_C"]);

    assert_eq!(
        fake.names_deleted(),
        ["ZCL_A", "ZCL_B"],
        "no retry of ZCL_B"
    );
    assert_eq!(fake.reconciled, ["ZCL_B"]);
    assert_eq!(status_of(&report, "ZCL_B"), DeleteStatus::DeletedReconciled);
    assert_eq!(status_of(&report, "ZCL_C"), DeleteStatus::Skipped);
    assert!(
        report
            .outcome
            .stdout
            .contains("DELETE: DELETED (reconciled)")
    );
    assert!(report.outcome.stdout.contains("check before retrying"));
    assert!(report.outcome.stdout.contains("lock may remain"));
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 1 OK, 1 FAILED, 1 SKIPPED")
    );
    assert_eq!(report.outcome.exit, 1);
}

#[test]
fn uncertain_reply_with_object_still_there_or_failed_reconcile_is_partial() {
    for reconcile in [
        Reconcile::StillExists,
        Reconcile::Failed("GET failed".to_owned()),
    ] {
        let mut fake = Fake::default().reply("ZCL_A", DeleteReply::Uncertain("drop".to_owned()));
        fake.reconcile = Some(reconcile);

        let report = real_delete(&mut fake, &["CLAS:ZCL_A", "CLAS:ZCL_B"]);

        assert_eq!(fake.names_deleted(), ["ZCL_A"]);
        assert_eq!(status_of(&report, "ZCL_A"), DeleteStatus::Partial);
        assert_eq!(status_of(&report, "ZCL_B"), DeleteStatus::Skipped);
        assert!(report.outcome.stdout.contains("lock may remain"));
        assert_eq!(report.outcome.exit, 1);
    }
}

#[test]
fn simulate_never_calls_delete_or_reconcile() {
    let mut fake = Fake::default().reply("ZCL_A", DeleteReply::Uncertain("x".to_owned()));
    let _ = sim(&mut fake, &["CLAS:ZCL_A", "PROG:ZP"], None);

    assert!(fake.deleted.is_empty());
    assert!(fake.reconciled.is_empty());
}

#[test]
fn absent_object_at_delete_time_is_not_found_without_a_delete_call() {
    let plan = plan_for(&["CLAS:ZCL_A", "CLAS:ZCL_B"], None);
    let mut fake = Fake::default().absent("ZCL_A");
    let report = exec(&mut fake, &["CLAS:ZCL_A", "CLAS:ZCL_B"], None, &plan);

    assert_eq!(fake.names_deleted(), ["ZCL_B"]);
    assert_eq!(status_of(&report, "ZCL_A"), DeleteStatus::NotFound);
    assert_eq!(report.outcome.exit, 1);
}

// ---------- Task 10: safe points and contents ----------

#[test]
fn a_simulation_reports_the_contents_of_a_group_for_the_confirmation() {
    let mut fake = Fake::default().with(
        "ZFG",
        ObjectFacts {
            contents: vec!["FUNC ZF1".to_owned(), "FUNC ZF2".to_owned()],
            ..facts_of("ZFG")
        },
    );
    let report = sim(&mut fake, &["FUGR:ZFG", "PROG:ZP"], None);
    let contents = |name: &str| {
        report
            .results
            .iter()
            .find(|result| result.name == name)
            .unwrap()
            .contents
            .clone()
    };

    assert_eq!(contents("ZFG"), ["FUNC ZF1", "FUNC ZF2"]);
    assert!(contents("ZP").is_empty());
}

#[test]
fn execute_until_skips_the_objects_from_the_first_stop_and_never_interrupts_a_request() {
    let mut fake = Fake::default();
    let plan = plan_of(&sim(&mut fake, &["PROG:ZP1", "PROG:ZP2", "PROG:ZP3"], None));
    let policies = Policies::default();
    let mut asked = 0;
    // The stop answer comes before the second object, after the first was deleted.
    let report = execute_until(
        &mut fake,
        &context(&policies, &Yes(false)),
        &targets(&["PROG:ZP1", "PROG:ZP2", "PROG:ZP3"]),
        None,
        &plan,
        &mut || {
            asked += 1;

            asked > 2
        },
    )
    .expect("execute");

    assert_eq!(fake.names_deleted(), ["ZP1"]);
    assert_eq!(status_of(&report, "ZP1"), DeleteStatus::Deleted);
    assert_eq!(status_of(&report, "ZP2"), DeleteStatus::Skipped);
    assert_eq!(status_of(&report, "ZP3"), DeleteStatus::Skipped);
    assert_eq!(report.outcome.exit, 1);
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 1 OK, 0 FAILED, 2 SKIPPED"),
        "{}",
        report.outcome.stdout
    );
    assert!(report.outcome.stdout.contains("W: stopped before PROG ZP2"));
}

#[test]
fn execute_until_with_a_stop_before_the_first_object_deletes_nothing() {
    let mut fake = Fake::default();
    let plan = plan_of(&sim(&mut fake, &["PROG:ZP1"], None));
    let policies = Policies::default();
    let report = execute_until(
        &mut fake,
        &context(&policies, &Yes(false)),
        &targets(&["PROG:ZP1"]),
        None,
        &plan,
        &mut || true,
    )
    .expect("execute");

    assert!(fake.deleted.is_empty());
    assert_eq!(status_of(&report, "ZP1"), DeleteStatus::Skipped);
}

#[test]
fn execute_until_stops_after_the_inspection_before_the_first_lock() {
    let mut fake = Fake::default();
    let plan = plan_of(&sim(&mut fake, &["PROG:ZP1", "PROG:ZP2"], None));
    let policies = Policies::default();
    let mut checks = 0;
    let report = execute_until(
        &mut fake,
        &context(&policies, &Yes(false)),
        &targets(&["PROG:ZP1", "PROG:ZP2"]),
        None,
        &plan,
        &mut || {
            checks += 1;
            checks > 1
        },
    )
    .expect("execute");

    assert!(fake.deleted.is_empty());
    assert_eq!(status_of(&report, "ZP1"), DeleteStatus::Skipped);
    assert_eq!(status_of(&report, "ZP2"), DeleteStatus::Skipped);
    assert!(report.outcome.stdout.contains("W: stopped before PROG ZP1"));
}

#[test]
fn adt_simulation_without_a_tr_leaves_the_request_to_the_later_lock() {
    let mut fake = with_tr("ZP", TrLock::Unlocked);

    let report = sim(&mut fake, &["PROG:ZP"], None);

    assert!(report.all_simulated_ok(), "{}", report.outcome.stdout);
    assert!(
        report.outcome.stdout.contains("TR -"),
        "{}",
        report.outcome.stdout
    );
    assert!(
        report
            .outcome
            .stdout
            .contains("TR: decided by the lock at execute"),
        "{}",
        report.outcome.stdout
    );
}

#[test]
fn adt_simulation_does_not_claim_a_transport_ownership_guard() {
    let mut fake = Fake::default();

    let report = sim(&mut fake, &["PROG:ZP"], None);

    assert!(!report.outcome.stdout.contains("TR ownership"));
}

// ---------- program includes ----------

fn position(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {text:?}"))
}

#[test]
fn a_program_is_deleted_before_an_include_in_a_mixed_batch() {
    let mut fake = Fake::default().include("ZINC");
    let report = sim(&mut fake, &["PROG:ZINC", "PROG:ZPROG"], None);
    let out = &report.outcome.stdout;

    assert!(position(out, "== PROG ZPROG ==") < position(out, "== PROG ZINC =="));
    assert_eq!(
        plan_of(&report)
            .objects
            .iter()
            .map(|(_, name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["ZPROG", "ZINC"]
    );

    let plan = plan_of(&report);
    let real = exec(&mut fake, &["PROG:ZINC", "PROG:ZPROG"], None, &plan);

    assert_eq!(real.outcome.exit, 0);
    assert_eq!(fake.names_deleted(), ["ZPROG", "ZINC"]);
}

#[test]
fn includes_follow_the_programs_but_the_kind_order_is_kept() {
    let mut fake = Fake::default().include("ZINC");
    let report = sim(
        &mut fake,
        &["PROG:ZINC", "PROG:ZPROG", "CLAS:ZCL_A", "FUGR:ZFG"],
        None,
    );
    let owned: Vec<String> = plan_of(&report)
        .objects
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect();

    assert_eq!(owned, ["ZCL_A", "ZPROG", "ZINC", "ZFG"]);
}

#[test]
fn two_programs_keep_their_given_order() {
    let mut fake = Fake::default();
    let report = sim(&mut fake, &["PROG:ZB", "PROG:ZA"], None);

    assert_eq!(
        plan_of(&report)
            .objects
            .iter()
            .map(|(_, name, _)| name.clone())
            .collect::<Vec<_>>(),
        ["ZB", "ZA"]
    );
}

#[test]
fn a_single_program_asks_nothing_about_includes() {
    let mut fake = Fake::default();

    sim(&mut fake, &["PROG:ZPROG", "CLAS:ZCL_A"], None);

    assert!(fake.include_asked.is_empty());
}

#[test]
fn a_referenced_include_is_rejected_and_the_batch_continues() {
    let referenced = "DELETE failed: HTTP 403: Program ZINC is referenced in other programs";
    let mut fake = Fake::default()
        .include("ZINC")
        .reply("ZINC", DeleteReply::Rejected(referenced.to_owned()));
    let texts = ["PROG:ZINC", "PROG:ZPROG", "CLAS:ZCL_A"];
    let plan = plan_of(&sim(&mut fake, &texts, None));
    let report = exec(&mut fake, &texts, None, &plan);

    assert_eq!(status_of(&report, "ZINC"), DeleteStatus::Rejected);
    assert_eq!(reason_of(&report, "ZINC"), referenced);
    assert_eq!(status_of(&report, "ZPROG"), DeleteStatus::Deleted);
    assert_eq!(status_of(&report, "ZCL_A"), DeleteStatus::Deleted);
    assert_eq!(report.outcome.exit, 1);
    assert_eq!(fake.names_deleted(), ["ZCL_A", "ZPROG", "ZINC"]);
}

#[test]
fn an_include_source_change_changes_the_hash_and_rejects_the_old_plan() {
    let include = |lines: &[&str]| ObjectFacts {
        sources: vec![SourceFact::active("MAIN", lines)],
        ..facts_of("ZINC")
    };
    let mut before = Fake::default().with("ZINC", include(&["WRITE 1."]));
    let plan = plan_of(&sim(&mut before, &["PROG:ZINC"], None));
    let mut after = Fake::default().with("ZINC", include(&["WRITE 1.", "WRITE 2."]));
    let changed = plan_of(&sim(&mut after, &["PROG:ZINC"], None));

    assert_ne!(plan.objects[0].2, changed.objects[0].2);

    let report = exec(&mut after, &["PROG:ZINC"], None, &plan);

    assert_eq!(status_of(&report, "ZINC"), DeleteStatus::Rejected);
    assert_eq!(
        reason_of(&report, "ZINC"),
        "Plan changed since simulate; simulate again"
    );
    assert!(after.deleted.is_empty());
}

#[test]
fn an_include_without_an_active_source_is_no_active() {
    let absent = ObjectFacts {
        sources: vec![SourceFact::no_active("MAIN")],
        ..facts_of("ZINC")
    };
    let mut plain = Fake::default();
    let mut never_active = Fake::default().with("ZINC", absent);
    let first = plan_of(&sim(&mut plain, &["PROG:ZINC"], None));
    let report = sim(&mut never_active, &["PROG:ZINC"], None);

    assert_ne!(first.objects[0].2, plan_of(&report).objects[0].2);
    assert!(report.outcome.stdout.contains("SOURCE MAIN NO_ACTIVE"));
}

#[test]
fn where_used_lines_name_programs_transactions_and_warnings() {
    let used = |entries: &[&str]| {
        let mut fake = Fake::default().with(
            "ZP",
            ObjectFacts {
                where_used: entries.iter().map(|entry| (*entry).to_owned()).collect(),
                ..facts_of("ZP")
            },
        );

        sim(&mut fake, &["PROG:ZP"], None)
    };
    let report = used(&["PROG/P ZCALLER", "TRAN ZTRAN"]);
    let out = &report.outcome.stdout;

    assert!(out.contains("\nWHERE-USED: PROG/P ZCALLER\n"));
    assert!(out.contains("\nWHERE-USED TRAN ZTRAN\n"));
    assert!(!out.contains("WHERE-USED: TRAN"));

    let failed = used(&["W: where-used unavailable: HTTP 500"]);

    assert!(
        failed
            .outcome
            .stdout
            .contains("\nW: where-used unavailable: HTTP 500\n")
    );
    assert!(!failed.outcome.stdout.contains("WHERE-USED:"));
    assert_eq!(failed.outcome.exit, 0, "a failed lookup never blocks");
    assert_eq!(
        plan_of(&used(&[])).objects,
        plan_of(&used(&["TRAN ZTRAN", "PROG/P ZCALLER"])).objects,
        "where-used is never hashed"
    );
}
