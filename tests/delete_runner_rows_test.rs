//! The delete batch with rows that a backend plans and deletes inside one run of its own (M6 Task
//! 23: the DDIC rows). The engine must hand such a row to the backend instead of reading it, take
//! the plan hash from the backend, send the hash of the plan file to the delete, print what the
//! backend reports for the row, and stop the batch when the backend says so. The rows of the five
//! M4 kinds go the old way in the same batch.

use abapreach::cli::{DeleteKind, DeleteTarget};
use abapreach::operation::delete::{
    Context, DeleteBackend, DeleteReply, DeleteReport, DeleteStatus, InactiveFact, ObjectFacts,
    Plan, Reconcile, RowPlan, SourceFact, TrLock, execute, simulate,
};
use abapreach::operation::payload::Kind;
use abapreach::policy::{Confirmer, Policies};
use std::collections::BTreeMap;

struct No;

impl Confirmer for No {
    fn interactive(&self) -> bool {
        false
    }

    fn confirm(&self, _prompt: &str) -> bool {
        false
    }
}

#[derive(Default)]
struct Own {
    /// What `plan_row` answers per name; an unscripted row is ready with `HASH_<name>`.
    plans: BTreeMap<String, RowPlan>,
    replies: BTreeMap<String, DeleteReply>,
    reconcile: Option<Reconcile>,
    /// Names the engine read with `inspect`.
    inspected: Vec<String>,
    /// Names planned in a run, in order.
    planned: Vec<(String, Option<String>)>,
    /// `(name, tr, plan hash)` of every `delete_planned`.
    deleted_in_run: Vec<(String, Option<String>, String)>,
    /// Names of `delete` (M4 rows).
    deleted: Vec<String>,
    /// What the backend printed for the row it handled last.
    lines: Vec<String>,
    /// The backend asks to stop after this name.
    stop_after: Option<String>,
    stopped: bool,
}

impl DeleteBackend for Own {
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        assert!(
            !matches!(kind, DeleteKind::Runner(_)),
            "a row planned in a run is never read by the engine"
        );
        self.inspected.push(name.to_owned());

        Ok(Some(ObjectFacts {
            package: "$TMP".to_owned(),
            author: "DEVUSER".to_owned(),
            group: None,
            sources: vec![SourceFact::active("MAIN", &["REPORT."])],
            contents: Vec::new(),
            where_used: Vec::new(),
            inactive: InactiveFact::Absent,
            tr: TrLock::Local,
        }))
    }

    fn delete(&mut self, _kind: DeleteKind, name: &str, _tr: Option<&str>) -> DeleteReply {
        self.deleted.push(name.to_owned());

        DeleteReply::Deleted
    }

    fn reconcile(&mut self, _kind: DeleteKind, _name: &str) -> Reconcile {
        self.reconcile.clone().expect("reconcile was not scripted")
    }

    fn plans_in_run(&self, kind: DeleteKind) -> bool {
        matches!(kind, DeleteKind::Runner(_))
    }

    fn plan_row(&mut self, _kind: DeleteKind, name: &str, tr: Option<&str>) -> RowPlan {
        self.planned.push((name.to_owned(), tr.map(str::to_owned)));
        self.lines = vec![format!("RUNNER: {name}")];
        self.stopped = self.stop_after.as_deref() == Some(name);

        self.plans.get(name).cloned().unwrap_or(RowPlan::Ready {
            hash: format!("HASH_{name}"),
            tr: None,
            lines: vec![format!("DELETE row {name}")],
        })
    }

    fn delete_planned(
        &mut self,
        _kind: DeleteKind,
        name: &str,
        tr: Option<&str>,
        plan_hash: &str,
    ) -> DeleteReply {
        self.deleted_in_run
            .push((name.to_owned(), tr.map(str::to_owned), plan_hash.to_owned()));
        self.lines = vec![format!("RUNNER: {name}")];
        self.stopped = self.stop_after.as_deref() == Some(name);

        self.replies
            .get(name)
            .cloned()
            .unwrap_or(DeleteReply::Deleted)
    }

    fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }

    fn stops_batch(&self) -> bool {
        self.stopped
    }
}

fn targets(texts: &[&str]) -> Vec<DeleteTarget> {
    texts
        .iter()
        .map(|text| text.parse().expect("target"))
        .collect()
}

fn context(policies: &Policies) -> Context<'_> {
    Context {
        conn: "DEV_100",
        user: "DEVUSER",
        policies,
        confirmer: &No,
    }
}

fn sim(own: &mut Own, texts: &[&str], tr: Option<&str>) -> DeleteReport {
    let policies = Policies::default();

    simulate(own, &context(&policies), &targets(texts), tr).expect("simulate")
}

fn exec(own: &mut Own, texts: &[&str], tr: Option<&str>, plan: &Plan) -> DeleteReport {
    let policies = Policies::default();

    execute(own, &context(&policies), &targets(texts), tr, plan).expect("execute")
}

fn status_of(report: &DeleteReport, name: &str) -> DeleteStatus {
    report
        .results
        .iter()
        .find(|result| result.name == name)
        .unwrap_or_else(|| panic!("no result for {name}"))
        .status
}

const ALL_TEN: [&str; 10] = [
    "DOMA:ZDOMA",
    "DTEL:ZDTEL",
    "TABL:ZTABL",
    "TTYP:ZTTYP",
    "FUGR:ZFUGR",
    "PROG:ZPROG",
    "INTF:ZINTF",
    "CLAS:ZCLAS",
    "FUNC:ZFUNC",
    "TRAN:ZTRAN",
];

#[test]
fn the_ten_ranks_order_a_batch_the_way_the_python_tool_does() {
    let ranks: Vec<(&str, u8)> = [
        ("TRAN", DeleteKind::Runner(Kind::Tran)),
        ("FUNC", DeleteKind::Func),
        ("CLAS", DeleteKind::Clas),
        ("INTF", DeleteKind::Intf),
        ("PROG", DeleteKind::Prog),
        ("FUGR", DeleteKind::Fugr),
        ("TTYP", DeleteKind::Runner(Kind::Ttyp)),
        ("TABL", DeleteKind::Runner(Kind::Tabl)),
        ("DTEL", DeleteKind::Runner(Kind::Dtel)),
        ("DOMA", DeleteKind::Runner(Kind::Doma)),
    ]
    .into_iter()
    .map(|(token, kind)| (token, kind.rank()))
    .collect();

    assert_eq!(
        ranks,
        [
            ("TRAN", 0),
            ("FUNC", 1),
            ("CLAS", 2),
            ("INTF", 3),
            ("PROG", 4),
            ("FUGR", 5),
            ("TTYP", 6),
            ("TABL", 7),
            ("DTEL", 8),
            ("DOMA", 9),
        ]
    );

    // A function module needs its group to exist, so the batch of this test uses the M4 fake only
    // for the order: the simulate prints the header of every row in execution order.
    let mut own = Own::default();
    let report = sim(&mut own, &ALL_TEN, None);
    let headers: Vec<&str> = report
        .outcome
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix("== ")?.strip_suffix(" =="))
        .collect();

    assert_eq!(
        headers,
        [
            "TRAN ZTRAN",
            "FUNC ZFUNC",
            "CLAS ZCLAS",
            "INTF ZINTF",
            "PROG ZPROG",
            "FUGR ZFUGR",
            "TTYP ZTTYP",
            "TABL ZTABL",
            "DTEL ZDTEL",
            "DOMA ZDOMA",
        ]
    );
}

#[test]
fn the_order_inside_one_type_is_the_order_given() {
    let mut own = Own::default();

    sim(
        &mut own,
        &["TABL:ZB", "TABL:ZA", "DTEL:ZD2", "DTEL:ZD1"],
        None,
    );

    let names: Vec<&str> = own.planned.iter().map(|(name, _)| name.as_str()).collect();

    assert_eq!(names, ["ZB", "ZA", "ZD2", "ZD1"]);
}

#[test]
fn a_simulate_takes_hash_and_lines_from_the_backend_and_never_reads_the_row() {
    let mut own = Own::default();
    let report = sim(&mut own, &["DTEL:ZD", "PROG:ZP"], Some("devk900001"));
    let plan = report.plan.expect("plan");

    // The M4 row was read; the DDIC row was planned in a run with the normalized --tr.
    assert_eq!(own.inspected, ["ZP"]);
    assert_eq!(
        own.planned,
        [("ZD".to_owned(), Some("DEVK900001".to_owned()))]
    );
    assert!(
        plan.objects
            .iter()
            .any(|object| *object == ("DTEL".into(), "ZD".into(), "HASH_ZD".into()))
    );
    assert!(plan.backends.is_empty());

    // The header, what the backend printed, its plan lines, the hash, the status.
    let out = report.outcome.stdout;
    let section = &out[out.find("== DTEL ZD ==").expect("the DTEL section")..];
    let at = |needle: &str| {
        section
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} in {section}"))
    };

    assert!(at("== DTEL ZD ==") < at("RUNNER: ZD"));
    assert!(at("RUNNER: ZD") < at("DELETE row ZD"));
    assert!(at("DELETE row ZD") < at("plan hash HASH_ZD"));
    assert!(at("plan hash HASH_ZD") < at("DELETE_SIM: SIMULATED_OK"));
}

#[test]
fn the_guard_header_is_printed_only_when_a_row_runs_over_adt() {
    let mut own = Own::default();
    let only_runner = sim(&mut own, &["DTEL:ZD", "TABL:ZT"], None).outcome.stdout;

    assert!(!only_runner.contains("guards active"), "{only_runner}");
    assert!(!only_runner.contains("guards not applied"), "{only_runner}");

    let mixed = sim(&mut Own::default(), &["DTEL:ZD", "PROG:ZP"], None)
        .outcome
        .stdout;

    assert!(mixed.contains("guards active"), "{mixed}");
}

#[test]
fn a_rejected_or_missing_row_lets_the_next_row_run_and_keeps_its_own_lines() {
    let mut own = Own::default();

    own.plans.insert(
        "ZA".to_owned(),
        RowPlan::Rejected("Table ZA contains 3 rows; delete rejected".to_owned()),
    );
    own.plans.insert("ZB".to_owned(), RowPlan::NotFound);

    let report = sim(&mut own, &["TABL:ZA", "TABL:ZB", "TABL:ZC"], None);

    assert_eq!(status_of(&report, "ZA"), DeleteStatus::Rejected);
    assert_eq!(status_of(&report, "ZB"), DeleteStatus::NotFound);
    assert_eq!(status_of(&report, "ZC"), DeleteStatus::SimulatedOk);
    assert_eq!(report.outcome.exit, 1);
    assert!(
        report
            .outcome
            .stdout
            .contains("ERROR: Table ZA contains 3 rows; delete rejected")
    );
    // The rejected row still shows what the run printed.
    assert!(report.outcome.stdout.contains("RUNNER: ZA"));
    assert_eq!(own.planned.len(), 3);
}

#[test]
fn a_backend_that_stops_the_batch_skips_the_rest_of_a_simulate() {
    let mut own = Own {
        stop_after: Some("ZA".to_owned()),
        ..Own::default()
    };
    let report = sim(&mut own, &["TTYP:ZA", "TABL:ZB", "DTEL:ZC"], None);

    assert_eq!(status_of(&report, "ZA"), DeleteStatus::SimulatedOk);
    assert_eq!(status_of(&report, "ZB"), DeleteStatus::Skipped);
    assert_eq!(status_of(&report, "ZC"), DeleteStatus::Skipped);
    assert_eq!(own.planned.len(), 1);
    assert!(
        report
            .outcome
            .stdout
            .contains("SUMMARY: 1 OK, 0 FAILED, 2 SKIPPED")
    );
    assert_eq!(report.outcome.exit, 1);
}

#[test]
fn a_real_delete_sends_the_hash_of_the_plan_file_and_never_plans_the_row_again() {
    let mut own = Own::default();
    let plan = sim(
        &mut own,
        &["TABL:ZT", "DTEL:ZD", "PROG:ZP"],
        Some("DEVK900001"),
    )
    .plan
    .expect("plan");
    let mut own = Own::default();
    let report = exec(
        &mut own,
        &["DTEL:ZD", "PROG:ZP", "TABL:ZT"],
        Some("DEVK900001"),
        &plan,
    );

    assert!(
        own.planned.is_empty(),
        "the delete is the only run of a row"
    );
    assert_eq!(
        own.deleted_in_run,
        [
            (
                "ZT".to_owned(),
                Some("DEVK900001".to_owned()),
                "HASH_ZT".to_owned()
            ),
            (
                "ZD".to_owned(),
                Some("DEVK900001".to_owned()),
                "HASH_ZD".to_owned()
            ),
        ]
    );
    assert_eq!(own.deleted, ["ZP"]);
    assert_eq!(report.outcome.exit, 0);
    assert!(report.outcome.stdout.contains("RUNNER: ZT"));
    assert!(report.outcome.stdout.contains("DELETE: DELETED"));
}

#[test]
fn the_plan_file_must_still_match_the_object_set_the_connection_and_the_request() {
    let policies = Policies::default();
    let plan = sim(
        &mut Own::default(),
        &["DTEL:ZD", "TABL:ZT"],
        Some("DEVK900001"),
    )
    .plan
    .expect("plan");
    let cases = [
        (vec!["DTEL:ZD"], Some("DEVK900001")),
        (vec!["DTEL:ZD", "TABL:ZT", "DOMA:ZX"], Some("DEVK900001")),
        (vec!["DTEL:ZD", "TABL:ZOTHER"], Some("DEVK900001")),
        (vec!["DTEL:ZD", "TABL:ZT"], Some("DEVK900002")),
        (vec!["DTEL:ZD", "TABL:ZT"], None),
    ];

    for (texts, tr) in cases {
        let mut own = Own::default();
        let error = execute(&mut own, &context(&policies), &targets(&texts), tr, &plan)
            .expect_err("a changed argument set");

        assert_eq!(
            error,
            "plan does not match the arguments (conn/objects/--tr); simulate again"
        );
        assert!(own.deleted_in_run.is_empty(), "{texts:?} {tr:?}");
    }

    let mut other = plan.clone();

    other.conn = "OTHER_100".to_owned();

    assert!(
        execute(
            &mut Own::default(),
            &context(&policies),
            &targets(&["DTEL:ZD", "TABL:ZT"]),
            Some("DEVK900001"),
            &other
        )
        .is_err()
    );
}

#[test]
fn a_rejection_of_the_delete_run_lets_the_next_row_run() {
    let mut own = Own::default();
    let plan = sim(&mut own, &["TABL:ZA", "TABL:ZB"], None)
        .plan
        .expect("plan");
    let mut own = Own::default();

    own.replies.insert(
        "ZA".to_owned(),
        DeleteReply::Rejected("Plan changed since simulate; simulate again".to_owned()),
    );

    let report = exec(&mut own, &["TABL:ZA", "TABL:ZB"], None, &plan);

    assert_eq!(status_of(&report, "ZA"), DeleteStatus::Rejected);
    assert_eq!(status_of(&report, "ZB"), DeleteStatus::Deleted);
    assert!(
        report
            .outcome
            .stdout
            .contains("ERROR: Plan changed since simulate; simulate again")
    );
    assert_eq!(report.outcome.exit, 1);
}

#[test]
fn a_partial_or_a_stop_of_the_backend_skips_the_rest_of_a_real_delete() {
    let plan = sim(
        &mut Own::default(),
        &["TTYP:ZA", "TABL:ZB", "DTEL:ZC"],
        None,
    )
    .plan
    .expect("plan");
    let all = ["TTYP:ZA", "TABL:ZB", "DTEL:ZC"];

    let mut partial = Own::default();

    partial.replies.insert(
        "ZA".to_owned(),
        DeleteReply::Partial("OBJECT_STILL_EXISTS: TTYP ZA".to_owned()),
    );

    let report = exec(&mut partial, &all, None, &plan);

    assert_eq!(status_of(&report, "ZA"), DeleteStatus::Partial);
    assert_eq!(status_of(&report, "ZB"), DeleteStatus::Skipped);
    assert_eq!(status_of(&report, "ZC"), DeleteStatus::Skipped);
    assert_eq!(partial.deleted_in_run.len(), 1);

    // A leftover class: the row is DELETED, the backend stops the batch.
    let mut leftover = Own {
        stop_after: Some("ZA".to_owned()),
        ..Own::default()
    };
    let report = exec(&mut leftover, &all, None, &plan);

    assert_eq!(status_of(&report, "ZA"), DeleteStatus::Deleted);
    assert_eq!(status_of(&report, "ZB"), DeleteStatus::Skipped);
    assert_eq!(status_of(&report, "ZC"), DeleteStatus::Skipped);
    assert_eq!(leftover.deleted_in_run.len(), 1);
    assert_eq!(report.outcome.exit, 1);
}

#[test]
fn an_uncertain_delete_run_is_reconciled_once_and_stops_the_batch() {
    let plan = sim(&mut Own::default(), &["TABL:ZA", "DTEL:ZB"], None)
        .plan
        .expect("plan");
    let mut own = Own {
        reconcile: Some(Reconcile::Gone),
        ..Own::default()
    };

    own.replies.insert(
        "ZA".to_owned(),
        DeleteReply::Uncertain("reply lost".to_owned()),
    );

    let report = exec(&mut own, &["TABL:ZA", "DTEL:ZB"], None, &plan);

    assert_eq!(status_of(&report, "ZA"), DeleteStatus::DeletedReconciled);
    assert_eq!(status_of(&report, "ZB"), DeleteStatus::Skipped);
    assert_eq!(own.deleted_in_run.len(), 1, "the delete is never resent");
    assert_eq!(report.outcome.exit, 1);
}

#[test]
fn a_row_outside_the_customer_scope_is_refused_before_a_run() {
    let plan = Plan {
        conn: "DEV_100".to_owned(),
        tr: None,
        objects: vec![("DTEL".to_owned(), "SFLIGHT".to_owned(), "HASH".to_owned())],
        backends: Vec::new(),
    };
    let mut own = Own::default();
    let report = exec(&mut own, &["DTEL:SFLIGHT"], None, &plan);

    assert_eq!(status_of(&report, "SFLIGHT"), DeleteStatus::Rejected);
    assert!(own.deleted_in_run.is_empty());
}
