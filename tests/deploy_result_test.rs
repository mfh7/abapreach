//! What a deploy run reports for the plan token and the confirmation: the live base hash, the
//! resolved function group and the size of the change; the opt-in stop before the lock; and the
//! lock handles the process has seen.

mod common;
mod replay;

use abapreach::cli::ObjectKind;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{
    CancelFlag, DeployInput, DeployProg, DeployScope, EXCERPT_LINES, known_lock_handles,
};
use replay::{fx, no_inactive, replay};

const FIXTURE: &str = "dev-750-rfc.json";

fn input(source: &[&str], simulate: bool) -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: source.iter().map(|line| (*line).to_owned()).collect(),
        base: None,
        simulate,
        tr: None,
    }
}

const V2: [&str; 3] = ["REPORT ytest_abapreach01.", "", "WRITE / 'v2'."];

fn simulate_steps() -> Vec<replay::Step> {
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

#[test]
fn a_simulation_reports_the_live_hash_and_the_size_of_the_change() {
    let mut transport = replay(FIXTURE, simulate_steps());
    let result =
        DeployProg::run_classified(&mut transport, input(&V2, true), None, &CancelFlag::new());
    let change = result.change.expect("a change was computed");
    let live = ["REPORT ytest_abapreach01.", "", "WRITE / 'v1'."].map(str::to_owned);

    assert_eq!(result.base, Some(source_hash(&live)));
    assert_eq!((change.added, change.removed, change.live_lines), (1, 1, 3));
    assert!(change.excerpt.iter().any(|line| line == "-WRITE / 'v1'."));
    assert!(change.excerpt.iter().any(|line| line == "+WRITE / 'v2'."));
    assert_eq!(change.omitted, 0);
    assert_eq!(result.group, None);
}

#[test]
fn a_run_stopped_before_any_change_reports_the_hash_and_no_change() {
    let mut transport = replay(FIXTURE, vec![fx(6)]);
    let live = ["REPORT ytest_abapreach01.", "", "WRITE / 'v1'."];
    let result =
        DeployProg::run_classified(&mut transport, input(&live, true), None, &CancelFlag::new());

    assert_eq!(result.outcome.stdout, "no changes");
    assert!(result.base.is_some());
    assert!(result.change.is_none());
}

#[test]
fn the_excerpt_is_cut_at_the_limit_with_the_omitted_count() {
    let long: Vec<String> = (0..200)
        .map(|number| format!("WRITE / '{number}'."))
        .collect();
    let live = long.join("\n");
    let mut changed = long.clone();

    for line in changed.iter_mut().step_by(10) {
        line.push_str(" \"x");
    }

    let change = abapreach::operation::deploy::summarize_change(&live, &changed.join("\n"));

    assert_eq!(change.excerpt.len(), EXCERPT_LINES);
    assert!(change.omitted > 0);
    assert_eq!(change.live_lines, 200);
}

#[test]
fn only_a_scope_that_asks_for_it_stops_before_the_lock_on_a_set_flag() {
    let cancel = CancelFlag::new();

    cancel.set();

    // The default keeps the CLI contract: the flag is looked at once the lock is held.
    let mut cli = replay(FIXTURE, vec![fx(6), no_inactive(), fx(7), fx(9)]);
    let lock_taken = DeployProg::run_scoped(
        &mut cli,
        input(&V2, false),
        DeployScope::new(ObjectKind::Prog),
        None,
        None,
        &cancel,
    );

    assert_eq!(lock_taken.outcome.exit, 130);
    assert!(
        lock_taken
            .outcome
            .stderr
            .contains("while the lock was held")
    );

    let mut mcp = replay(FIXTURE, vec![fx(6), no_inactive()]);
    let early = DeployProg::run_scoped(
        &mut mcp,
        input(&V2, false),
        DeployScope::new(ObjectKind::Prog).cancel_before_lock(true),
        None,
        None,
        &cancel,
    );

    assert_eq!(early.outcome.exit, 130);
    assert!(early.outcome.stderr.contains("before the lock was taken"));
    assert!(!mcp.uris().iter().any(|uri| uri.contains("_action=LOCK")));
    mcp.assert_finished();
}

#[test]
fn a_lock_handle_is_remembered_in_both_spellings() {
    let mut transport = replay(FIXTURE, simulate_steps());

    DeployProg::run_classified(&mut transport, input(&V2, true), None, &CancelFlag::new());

    let known = known_lock_handles();

    assert!(!known.is_empty());
    // Longest first, so a scrub never leaves the tail of a longer spelling behind.
    assert!(known.windows(2).all(|pair| pair[0].len() >= pair[1].len()));
}

#[test]
fn the_cancel_flag_tells_a_shutdown_from_a_cancelled_request() {
    let flag = CancelFlag::new();

    flag.set();
    assert!(flag.is_set() && !flag.is_shutdown());

    flag.reset();
    assert!(!flag.is_set());

    flag.request_shutdown();
    flag.reset();
    assert!(flag.is_shutdown());
}
