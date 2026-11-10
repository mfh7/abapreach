//! Guards for the native four-runner verification workflow. The workflow file is the artifact, so
//! the tests read it and check the parts a later edit could weaken: the trigger set (reusable plus
//! manual dispatch, never its own tag push, so release.yml is the only tag-time caller), the runner
//! matrix, the four cargo commands in every runner, and the absence of anything that could reach a
//! real system or hide a failing suite.

use std::path::PathBuf;

mod yaml_subset;
use yaml_subset::{Block, find, get, items, keys, lines, map, parse, scalar};

const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";

/// The four release targets, exactly as the milestone contract lists them.
const RUNNERS: [(&str, &str); 4] = [
    ("macos-15", "arm64"),
    ("windows-latest", "x64"),
    ("ubuntu-24.04", "x64"),
    ("ubuntu-24.04-arm", "arm64"),
];

const FORMATTED: &str = "cargo fmt --check";
const LINTED: &str = "cargo clippy --all-targets --all-features -- -D warnings";
const DEFAULT_LINTED: &str = "cargo clippy --all-targets -- -D warnings";
const FAULT_LINTED: &str = "cargo clippy --all-targets --features fault-injection -- -D warnings";
const DEFAULT_TESTS: &str = "cargo test";
const FAULT_TESTS: &str = "cargo test --features fault-injection";
const REQUIRED: [&str; 6] = [
    FORMATTED,
    DEFAULT_LINTED,
    FAULT_LINTED,
    LINTED,
    DEFAULT_TESTS,
    FAULT_TESTS,
];

/// Nothing here may reach a system: no secret, and no connection source.
const FORBIDDEN: [&str; 4] = [
    "secrets.",
    "ABAP_CONNECTIONS",
    "connections.local",
    "ABAPREACH_RFC_LIBRARY",
];

fn workflow_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(WORKFLOW_PATH);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[test]
fn ci_workflow_uses_the_pinned_toolchain_and_records_its_version() {
    let text = workflow_text();
    assert!(text.contains("rustup toolchain install 1.99.1"), "{text}");
    assert!(
        text.contains("rustc --version | tee -a \"$GITHUB_STEP_SUMMARY\""),
        "{text}"
    );
    assert!(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("rust-toolchain.toml")
            .exists()
    );
}

fn document() -> Block {
    parse(&workflow_text())
}

fn steps(document: &Block) -> &[Block] {
    items(get(get(get(document, "jobs"), "verify"), "steps"))
}

fn run_commands(document: &Block) -> Vec<String> {
    steps(document)
        .iter()
        .filter_map(|step| find(step, "run"))
        .flat_map(|script| lines(script).into_iter().map(str::to_owned))
        .collect()
}

/// The index of the step whose run script holds a line containing `needle`.
fn step_of(document: &Block, needle: &str) -> usize {
    steps(document)
        .iter()
        .position(|step| {
            find(step, "run")
                .map(|script| lines(script).iter().any(|line| line.contains(needle)))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no step runs a command containing `{needle}`"))
}

fn step_with_id(document: &Block, id: &str) -> usize {
    steps(document)
        .iter()
        .position(|step| find(step, "id").map(scalar) == Some(id))
        .unwrap_or_else(|| panic!("no step has id `{id}`"))
}

#[test]
fn ci_workflow_has_one_job_on_the_four_target_runners() {
    let document = document();
    assert_eq!(keys(get(&document, "jobs")), ["verify"]);

    let job = get(get(&document, "jobs"), "verify");
    assert_eq!(scalar(get(job, "runs-on")), "${{ matrix.runner.label }}");

    let matrix = get(get(job, "strategy"), "matrix");
    let runners: Vec<(&str, &str)> = items(get(matrix, "runner"))
        .iter()
        .map(|entry| (scalar(get(entry, "label")), scalar(get(entry, "arch"))))
        .collect();
    assert_eq!(runners, RUNNERS);
}

#[test]
fn ci_workflow_is_reusable_and_manually_dispatchable_with_no_trigger_of_its_own() {
    let document = document();
    let triggers = get(&document, "on");
    // No `push`: release.yml calls this workflow for an rc tag, and a second tag-push run would
    // run CI twice and race the release run.
    assert_eq!(keys(triggers), ["workflow_call", "workflow_dispatch"]);
    assert_eq!(map(get(triggers, "workflow_call")), &[]);
    // A manual dispatch has no filter, so it stays usable for the rc gate from the tagged commit.
    assert_eq!(map(get(triggers, "workflow_dispatch")), &[]);
}

#[test]
fn ci_workflow_runs_formatting_linting_and_both_feature_suites() {
    let commands = run_commands(&document());
    for required in REQUIRED {
        assert!(
            commands.iter().any(|command| command == required),
            "`{required}` is missing; the workflow runs: {commands:?}"
        );
    }
}

#[test]
fn ci_workflow_never_skips_or_softens_a_required_command() {
    let document = document();
    for step in steps(&document) {
        let Some(script) = find(step, "run") else {
            continue;
        };
        let commands = lines(script);
        assert!(
            !keys(step).contains(&"continue-on-error"),
            "a step sets continue-on-error: {step:?}"
        );
        if !commands.iter().any(|command| REQUIRED.contains(command)) {
            continue;
        }
        assert!(
            !keys(step).contains(&"if"),
            "a required command is conditional, so a runner could skip it: {step:?}"
        );
        for command in commands {
            for forbidden in ["|| true", "|| exit 0", "--skip", "--exclude"] {
                assert!(
                    !command.contains(forbidden),
                    "`{command}` softens a failure with `{forbidden}`"
                );
            }
        }
    }
}

#[test]
fn ci_workflow_stays_offline_from_sap() {
    let text = workflow_text();
    for forbidden in FORBIDDEN {
        assert!(
            !text.contains(forbidden),
            "the workflow references `{forbidden}`, so a job could reach a system"
        );
    }

    let document = document();
    assert_eq!(keys(get(&document, "permissions")), ["contents"]);
    assert_eq!(
        scalar(get(get(&document, "permissions"), "contents")),
        "read"
    );
}

#[test]
fn ci_workflow_rejects_a_runner_of_the_wrong_architecture_before_building() {
    let document = document();
    let steps = steps(&document);
    let first = &steps[0];
    assert_eq!(scalar(get(first, "id")), "runner_is_available");
    assert!(
        find(first, "name").is_some(),
        "a failing gate needs a name in the UI"
    );

    let script = scalar(get(first, "run"));
    assert!(script.contains("matrix.runner.arch"), "{script}");
    assert!(script.contains("::error::"), "{script}");
    assert!(script.contains("exit 1"), "{script}");

    assert!(
        step_with_id(&document, "runner_is_available") < step_of(&document, FORMATTED),
        "the architecture check must come before the build steps"
    );
}

#[test]
fn ci_workflow_installs_nasm_before_the_windows_build() {
    let document = document();
    let steps_of_workflow = steps(&document);
    let nasm = step_of(&document, "nasm");
    assert!(
        nasm < step_of(&document, FORMATTED),
        "the NASM install must come before a step that builds aws-lc-sys"
    );
    assert_eq!(
        scalar(get(&steps_of_workflow[nasm], "if")),
        "runner.os == 'Windows'"
    );
}

#[test]
fn ci_workflow_confirms_the_fault_injection_suite_is_not_empty() {
    let document = document();
    let guard = step_of(&document, "--test fault_injection_test");
    assert!(
        guard > step_of(&document, FAULT_TESTS),
        "the empty-suite check must follow the fault-injection run"
    );

    let script = scalar(get(&steps(&document)[guard], "run"));
    assert!(script.contains("--list"), "{script}");
    assert!(script.contains("-lt 1"), "{script}");
    assert!(script.contains("exit 1"), "{script}");
}

#[test]
fn ci_workflow_checks_the_third_party_notices_are_current_on_the_ubuntu_x64_runner_only() {
    let document = document();
    let check = step_of(&document, "cargo install cargo-about");
    let step = &steps(&document)[check];

    // One runner is enough to prove the committed file, and its output must not depend on the OS.
    assert_eq!(
        scalar(get(step, "if")),
        "matrix.runner.label == 'ubuntu-24.04'"
    );
    assert!(
        check > step_of(&document, "rustup toolchain install"),
        "cargo must be installed before it builds cargo-about"
    );

    let script = scalar(get(step, "run"));
    for needle in [
        "cargo install cargo-about --locked --version 0.9.3 --features cli",
        "cargo about generate about-text.hbs",
        "diff -u THIRD-PARTY-NOTICES",
        "::error::",
        "exit 1",
    ] {
        assert!(script.contains(needle), "missing `{needle}`: {script}");
    }
    for forbidden in ["|| true", "|| exit 0"] {
        assert!(!script.contains(forbidden), "{script}");
    }
}
