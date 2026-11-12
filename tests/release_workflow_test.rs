//! Guards for the tag release workflow and the scripts it runs. The workflow file is the artifact,
//! so the structure tests read it and check the parts a later edit could weaken: the triggers, the
//! least-privilege permissions, the four targets, the separation of the fault-injection artifact,
//! the inspection of the packaged executable, and the promotion checks. The script tests run the
//! shell logic against fixtures; they never touch GitHub, a registry or a system.

use std::path::PathBuf;

mod yaml_subset;
use yaml_subset::{Block, find, get, items, keys, map, parse, scalar, scalars};

const WORKFLOW_PATH: &str = ".github/workflows/release.yml";
const BARE_BUILD_JOB: &str = "build-bare";
const LINUX_BUILD_JOB: &str = "build-linux";
const CI_JOB: &str = "ci";

/// Target triple, runner label, archive extension, executable name.
const TARGETS: [(&str, &str, &str, &str); 4] = [
    ("aarch64-apple-darwin", "macos-15", "tar.gz", "abapreach"),
    (
        "x86_64-pc-windows-msvc",
        "windows-latest",
        "zip",
        "abapreach.exe",
    ),
    (
        "x86_64-unknown-linux-gnu",
        "ubuntu-24.04",
        "tar.gz",
        "abapreach",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "ubuntu-24.04-arm",
        "tar.gz",
        "abapreach",
    ),
];

/// Every archive holds the executable, LICENSE and THIRD-PARTY-NOTICES at its root.
const UNIX_PACKAGE: &str = "tar -czf \"dist/$ARCHIVE_FILE\" -C \"target/$TARGET/release\" \"$EXE\" -C \"$PWD\" LICENSE THIRD-PARTY-NOTICES";
const WINDOWS_PACKAGE: &str = "Compress-Archive -LiteralPath \"target\\\\$env:TARGET\\\\release\\\\$env:EXE\", \"LICENSE\", \"THIRD-PARTY-NOTICES\" -DestinationPath \"dist\\\\$env:ARCHIVE_FILE\"";

/// Nothing here may reach a system: no repository secret, and no connection source.
const FORBIDDEN: [&str; 5] = [
    "secrets.",
    "ABAP_CONNECTIONS",
    "connections.local",
    "ABAPREACH_RFC_LIBRARY",
    "continue-on-error",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workflow_text() -> String {
    let path = root().join(WORKFLOW_PATH);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn document() -> Block {
    parse(&workflow_text())
}

fn job<'a>(document: &'a Block, name: &str) -> &'a Block {
    get(get(document, "jobs"), name)
}

fn steps<'a>(document: &'a Block, name: &str) -> &'a [Block] {
    items(get(job(document, name), "steps"))
}

/// All run scripts of a job, one entry per step, steps in order.
fn run_scripts(document: &Block, name: &str) -> Vec<String> {
    steps(document, name)
        .iter()
        .filter_map(|step| find(step, "run"))
        .map(|script| scalar(script).to_owned())
        .collect()
}

fn step_index(document: &Block, name: &str, needle: &str) -> usize {
    steps(document, name)
        .iter()
        .position(|step| {
            [find(step, "run"), find(step, "name"), find(step, "uses")]
                .into_iter()
                .flatten()
                .any(|text| scalar(text).contains(needle))
        })
        .unwrap_or_else(|| panic!("job `{name}` has no step mentioning `{needle}`"))
}

fn job_text(document: &Block, name: &str) -> String {
    format!("{:?}", job(document, name))
}

#[test]
fn release_workflow_triggers_are_v_tags_and_a_dry_run_dispatch_only() {
    let document = document();
    let triggers = get(&document, "on");
    assert_eq!(keys(triggers), ["push", "workflow_dispatch"]);

    let push = get(triggers, "push");
    assert_eq!(keys(push), ["tags"]);
    assert_eq!(scalars(get(push, "tags")), ["v*"]);

    let inputs = get(get(triggers, "workflow_dispatch"), "inputs");
    assert_eq!(keys(inputs), ["rc_tag"]);
    assert_eq!(scalar(get(get(inputs, "rc_tag"), "required")), "true");
}

#[test]
fn release_workflow_grants_write_access_only_to_the_two_publish_jobs() {
    let document = document();
    let top = get(&document, "permissions");
    assert_eq!(keys(top), ["actions", "contents"]);
    assert_eq!(scalar(get(top, "actions")), "read");
    assert_eq!(scalar(get(top, "contents")), "read");

    for (name, _) in map(get(&document, "jobs")) {
        let writer = name == "publish-rc" || name == "publish-final";
        let permissions = find(job(&document, name), "permissions");
        match (writer, permissions) {
            (true, Some(permissions)) => {
                assert_eq!(keys(permissions), ["contents"], "job `{name}`");
                assert_eq!(
                    scalar(get(permissions, "contents")),
                    "write",
                    "job `{name}`"
                );
            }
            (true, None) => panic!("publish job `{name}` has no write permission"),
            (false, Some(permissions)) => {
                assert_eq!(
                    scalar(get(permissions, "contents")),
                    "read",
                    "job `{name}` must stay read-only"
                );
            }
            (false, None) => {}
        }
    }
    assert_eq!(
        keys(get(&document, "jobs")),
        [
            "plan",
            "ci",
            "build-linux",
            "build-bare",
            "publish-rc",
            "promote-verify",
            "publish-final"
        ]
    );
}

#[test]
fn release_workflow_calls_ci_with_read_only_contents_and_gates_the_rc_jobs_on_it() {
    let document = document();
    let ci = job(&document, CI_JOB);
    assert_eq!(scalar(get(ci, "uses")), "./.github/workflows/ci.yml");
    assert_eq!(scalar(get(ci, "if")), "needs.plan.outputs.mode == 'rc'");
    // A called workflow inherits the caller's permissions, so the job must narrow them.
    let permissions = get(ci, "permissions");
    assert_eq!(keys(permissions), ["contents"]);
    assert_eq!(scalar(get(permissions, "contents")), "read");
    assert_eq!(
        find(ci, "steps"),
        None,
        "a calling job has no steps of its own"
    );
    assert_eq!(find(ci, "secrets"), None, "no secret is passed to CI");

    for name in [BARE_BUILD_JOB, LINUX_BUILD_JOB, "publish-rc"] {
        let needs = scalars(get(job(&document, name), "needs"));
        assert!(
            needs.contains(&CI_JOB),
            "job `{name}` must need `ci`: {needs:?}"
        );
    }
}

#[test]
fn release_workflow_stays_offline_from_sap_and_uses_only_the_default_token() {
    let text = workflow_text();
    for forbidden in FORBIDDEN {
        assert!(
            !text.contains(forbidden),
            "the workflow references `{forbidden}`"
        );
    }
    assert!(text.contains("github.token"));
}

#[test]
fn release_workflow_builds_the_four_targets_with_the_release_environments() {
    let document = document();
    let bare = job(&document, BARE_BUILD_JOB);
    assert_eq!(scalar(get(bare, "runs-on")), "${{ matrix.runner.label }}");
    assert_eq!(
        find(bare, "container"),
        None,
        "macOS and Windows must build on their bare hosted runners"
    );

    let linux = job(&document, LINUX_BUILD_JOB);
    assert_eq!(scalar(get(linux, "runs-on")), "${{ matrix.runner.label }}");
    assert_eq!(
        scalar(get(linux, "container")),
        "${{ matrix.runner.image }}"
    );

    let bare_runners = items(get(get(get(bare, "strategy"), "matrix"), "runner"));
    assert_eq!(bare_runners.len(), 2);
    for (entry, (target, label, extension, executable)) in bare_runners.iter().zip(TARGETS) {
        assert_eq!(scalar(get(entry, "target")), target);
        assert_eq!(scalar(get(entry, "label")), label);
        assert_eq!(scalar(get(entry, "ext")), extension);
        assert_eq!(scalar(get(entry, "exe")), executable);
        assert_eq!(find(entry, "image"), None, "{target} must stay bare");

        let flags = find(entry, "rustflags").map(scalar);
        if target.contains("windows") {
            assert_eq!(flags, Some("-C target-feature=+crt-static"));
        } else {
            assert_eq!(flags, None, "{target} must not carry static CRT flags");
        }
    }

    let linux_runners = items(get(get(get(linux, "strategy"), "matrix"), "runner"));
    assert_eq!(linux_runners.len(), 2);
    for (entry, &(target, label, extension, executable)) in
        linux_runners.iter().zip(TARGETS[2..].iter())
    {
        assert_eq!(scalar(get(entry, "target")), target);
        assert_eq!(scalar(get(entry, "label")), label);
        assert_eq!(scalar(get(entry, "ext")), extension);
        assert_eq!(scalar(get(entry, "exe")), executable);
        let arch = target.split('-').next().unwrap();
        assert_eq!(
            find(entry, "image").map(scalar),
            Some(format!("quay.io/pypa/manylinux_2_28_{arch}").as_str())
        );
        assert_eq!(
            find(entry, "rustflags"),
            None,
            "{target} must not carry CRT flags"
        );
    }
}

#[test]
fn release_workflow_builds_an_ordinary_and_a_separate_fault_injection_variant() {
    let document = document();
    for name in [BARE_BUILD_JOB, LINUX_BUILD_JOB] {
        let matrix = get(get(job(&document, name), "strategy"), "matrix");
        let variants = items(get(matrix, "variant"));
        assert_eq!(variants.len(), 2, "{name}");

        let ordinary = &variants[0];
        assert_eq!(scalar(get(ordinary, "artifact")), "release");
        assert_eq!(scalar(get(ordinary, "flags")), "");
        assert_eq!(scalar(get(ordinary, "suffix")), "");

        let fault = &variants[1];
        assert_eq!(scalar(get(fault, "artifact")), "fault-injection");
        assert_eq!(scalar(get(fault, "flags")), "--features fault-injection");
        assert_eq!(scalar(get(fault, "suffix")), "-fault-injection");

        let scripts = run_scripts(&document, name);
        assert!(
            scripts.iter().any(|script| script
                .contains("cargo build --release --locked --target \"$TARGET\" $FLAGS")),
            "{name}: {scripts:?}"
        );
    }
    assert!(
        !workflow_text().contains("--all-features"),
        "a release build must never turn on every feature"
    );

    // Archive names carry the variant suffix, so the two variants can never share a file name.
    let text = workflow_text();
    assert!(text.contains(
        "ARCHIVE_FILE: abapreach-${{ needs.plan.outputs.version }}-${{ matrix.runner.target }}${{ matrix.variant.suffix }}.${{ matrix.runner.ext }}"
    ));
}

#[test]
fn release_workflow_inspects_the_executable_extracted_from_the_packaged_archive() {
    let document = document();
    for build in [BARE_BUILD_JOB, LINUX_BUILD_JOB] {
        let package = step_index(&document, build, "Package");
        let checksum = step_index(&document, build, "sha256.sh");
        let extract = step_index(&document, build, "Extract");
        let upload = step_index(&document, build, "actions/upload-artifact");

        assert!(
            package < checksum && checksum < extract,
            "{build}: package, hash, extract"
        );

        let steps = steps(&document, build);
        let extraction = steps
            .iter()
            .filter(|step| find(step, "name").is_some_and(|name| scalar(name).contains("Extract")))
            .map(|step| scalar(get(step, "run")))
            .collect::<Vec<_>>();
        assert_eq!(
            extraction.len(),
            if build == LINUX_BUILD_JOB { 1 } else { 2 },
            "{build}"
        );
        for script in extraction {
            assert!(
                script.contains("dist/") || script.contains("dist\\"),
                "{script}"
            );
            assert!(script.contains("inspect"), "{script}");
        }

        let inspected = steps
            .iter()
            .filter(|step| {
                find(step, "run").is_some_and(|run| scalar(run).contains("inspect/$EXE"))
            })
            .collect::<Vec<_>>();
        assert!(
            !inspected.is_empty(),
            "{build} has no packaged-binary inspection"
        );
        for step in inspected {
            let script = scalar(get(step, "run"));
            assert!(!script.contains("target/"), "{script}");
            assert!(!script.contains("target\\"), "{script}");
        }

        if build == LINUX_BUILD_JOB {
            assert!(!job_text(&document, build).contains("RUSTFLAGS"), "{build}");
            assert!(!job_text(&document, build).contains("Windows"), "{build}");
            let glibc = step_index(&document, build, "check-glibc.sh");
            assert!(
                extract < glibc && glibc < upload,
                "{build}: inspect before upload"
            );
            let script = scalar(get(&steps[glibc], "run"));
            assert!(
                script.contains(
                    "objdump -T \"inspect/$EXE\" | bash .github/scripts/check-glibc.sh 2.28"
                ),
                "{script}"
            );
            assert_eq!(scalar(get(&steps[glibc], "if")), "runner.os == 'Linux'");
        } else {
            let imports = step_index(&document, build, "check-imports.sh");
            assert!(
                extract < imports && imports < upload,
                "{build}: inspect before upload"
            );
            let script = scalar(get(&steps[imports], "run"));
            assert!(
                script.contains(
                    "--coff-imports \"inspect/$EXE\" | bash .github/scripts/check-imports.sh"
                ),
                "{script}"
            );
            assert_eq!(scalar(get(&steps[imports], "if")), "runner.os == 'Windows'");
        }
    }
}

#[test]
fn release_workflow_checks_the_extracted_executable_for_the_custom_function_module() {
    let document = document();

    for build in [BARE_BUILD_JOB, LINUX_BUILD_JOB] {
        let extract = step_index(&document, build, "Extract");
        let check = step_index(&document, build, "check-no-custom-module.sh");
        let upload = step_index(&document, build, "actions/upload-artifact");
        let script = scalar(get(&steps(&document, build)[check], "run"));

        assert!(
            extract < check && check < upload,
            "{build}: check before upload"
        );
        assert_eq!(
            script, "bash .github/scripts/check-no-custom-module.sh \"inspect/$EXE\"",
            "{build}"
        );
    }

    // The needle must stay out of the workflow file itself (the public boundary test).
    assert!(!workflow_text().contains(concat!("YFM_UTILS_", "SOURCE_DEPLOY")));
}

#[cfg(unix)]
#[test]
fn custom_module_check_rejects_a_binary_that_names_a_private_name_and_accepts_a_clean_one() {
    let directory = tempfile::tempdir().unwrap();
    let clean = directory.path().join("clean");
    let needles = [
        concat!("YFM_UTILS_", "SOURCE_DEPLOY"),
        concat!("experimental", "-ext"),
        concat!("abapreach", "-ext"),
    ];

    std::fs::write(
        &clean,
        b"\x7fELF\0YFM_UTILS_OTHER\0SOURCE_DEPLOY\0experimental\0ext\0abapreach\0",
    )
    .unwrap();

    let outcome = scripts::run(
        "check-no-custom-module.sh",
        &[clean.to_str().unwrap()],
        "",
        None,
    );
    assert!(outcome.success, "{}", outcome.text);

    for needle in needles {
        let tainted = directory.path().join("tainted");

        std::fs::write(&tainted, format!("\x7fELF\0\0{needle}\0\0").as_bytes()).unwrap();

        let outcome = scripts::run(
            "check-no-custom-module.sh",
            &[tainted.to_str().unwrap()],
            "",
            None,
        );
        assert!(!outcome.success, "{needle}: {}", outcome.text);
        assert!(outcome.text.contains("::error::"), "{}", outcome.text);
    }

    let missing = directory.path().join("missing");
    let outcome = scripts::run(
        "check-no-custom-module.sh",
        &[missing.to_str().unwrap()],
        "",
        None,
    );
    assert!(!outcome.success, "{}", outcome.text);
}

#[test]
fn release_workflow_rejects_a_package_version_that_differs_from_the_tag() {
    // The rc build must report the tag's base version: a Cargo.toml mismatch stops the plan step.
    let scripts = run_scripts(&document(), "plan").join("\n");
    for needle in [
        "cargo_version=\"$(sed -n 's/^version = \"\\(.*\\)\"$/\\1/p' Cargo.toml | head -n 1)\"",
        "if [ \"$cargo_version\" != \"$base_version\" ]; then",
        "::error::Cargo.toml says $cargo_version but tag $tag releases $base_version",
    ] {
        assert!(scripts.contains(needle), "missing `{needle}`: {scripts}");
    }
}

#[test]
fn release_workflow_promotion_requires_a_successful_release_run_of_the_rc_tag() {
    let document = document();
    // CI now runs inside the rc's release run, so neither job polls ci.yml any more: publish-rc
    // is gated by `needs: ci`, and promotion checks the conclusion of the rc's release run.
    for name in ["publish-rc", "promote-verify"] {
        let scripts = run_scripts(&document, name).join("\n");
        assert!(!scripts.contains("ci.yml"), "{name}: {scripts}");
    }

    let scripts = run_scripts(&document, "promote-verify").join("\n");
    for needle in [
        "gh run list",
        "--workflow release.yml",
        "--event push",
        "--branch \"$RC_TAG\"",
        "--commit \"$RC_SHA\"",
        "--json conclusion",
        "grep -qx success",
    ] {
        assert!(scripts.contains(needle), "missing `{needle}`: {scripts}");
    }

    // The check needs the rc tag and commit, so it follows the lookup step.
    let lookup = step_index(&document, "promote-verify", "gh release view");
    let check = step_index(&document, "promote-verify", "gh run list");
    let download = step_index(&document, "promote-verify", "gh release download");
    assert!(
        lookup < check && check < download,
        "lookup, run check, download"
    );
}

#[test]
fn release_workflow_packages_and_hashes_every_archive() {
    let text = workflow_text();
    assert!(text.contains(UNIX_PACKAGE));
    assert!(text.contains("Compress-Archive"));
    assert!(text.contains("Expand-Archive"));
    assert!(text.contains("> \"dist/$ARCHIVE_FILE.sha256\""));
    assert!(text.contains("persist-credentials: false"));
}

#[test]
fn release_workflow_packages_the_license_files_beside_the_executable_in_every_archive() {
    let document = document();
    for build in [BARE_BUILD_JOB, LINUX_BUILD_JOB] {
        let packaging = steps(&document, build)
            .iter()
            .filter(|step| find(step, "name").is_some_and(|name| scalar(name).contains("Package")))
            .collect::<Vec<_>>();
        assert_eq!(
            packaging.len(),
            if build == LINUX_BUILD_JOB { 1 } else { 2 },
            "{build}"
        );
        for step in packaging {
            let script = scalar(get(step, "run"));
            let command = if find(step, "shell").is_some() {
                WINDOWS_PACKAGE
            } else {
                UNIX_PACKAGE
            };
            assert!(script.contains(command), "{build}: {script}");
        }
    }

    // The set check compares the two text files with the tag commit's, so each job that runs it
    // starts from a checkout of that commit.
    for name in ["publish-rc", "promote-verify", "publish-final"] {
        let first = &steps(&document, name)[0];
        assert!(
            scalar(get(first, "uses")).starts_with("actions/checkout@"),
            "{name}"
        );
    }
}

#[test]
fn release_workflow_attaches_only_ordinary_archives_to_a_prerelease() {
    let document = document();
    let steps = steps(&document, "publish-rc");

    let download = &steps[step_index(&document, "publish-rc", "actions/download-artifact")];
    let with = get(download, "with");
    assert_eq!(scalar(get(with, "pattern")), "release-*");
    assert_eq!(scalar(get(with, "merge-multiple")), "true");

    let verify = step_index(&document, "publish-rc", "verify-dist.sh");
    let create = step_index(&document, "publish-rc", "gh release create");
    assert!(verify < create, "verify the set before publishing it");

    let verify_script = scalar(get(&steps[verify], "run"));
    assert!(
        verify_script.contains("verify-dist.sh dist \"$VERSION\""),
        "{verify_script}"
    );

    let create_script = scalar(get(&steps[create], "run"));
    assert!(create_script.contains("--prerelease"), "{create_script}");
    assert!(create_script.contains("--verify-tag"), "{create_script}");
    assert!(create_script.contains("dist/*"), "{create_script}");

    // The publish job never names the fault artifact, so it cannot reach a Release.
    let text = job_text(&document, "publish-rc");
    assert!(!text.contains("fault-injection"), "{text}");
    assert!(!text.contains("cargo"), "{text}");
}

#[test]
fn release_workflow_runs_the_promotion_only_for_a_final_tag_or_a_dry_run() {
    let document = document();
    for name in [BARE_BUILD_JOB, LINUX_BUILD_JOB] {
        assert_eq!(
            scalar(get(job(&document, name), "if")),
            "needs.plan.outputs.mode == 'rc'"
        );
    }
    assert_eq!(
        scalar(get(job(&document, "publish-rc"), "if")),
        "needs.plan.outputs.mode == 'rc'"
    );
    assert_eq!(
        scalar(get(job(&document, "promote-verify"), "if")),
        "needs.plan.outputs.mode != 'rc'"
    );
    // The dry run stops after verification: only a real final tag reaches the publish job.
    assert_eq!(
        scalar(get(job(&document, "publish-final"), "if")),
        "needs.plan.outputs.mode == 'promote'"
    );

    let plan = run_scripts(&document, "plan").join("\n");
    assert!(plan.contains("mode=dry-run"), "{plan}");
    assert!(plan.contains("mode=rc"), "{plan}");
    assert!(plan.contains("mode=promote"), "{plan}");
    assert!(plan.contains("Cargo.toml"), "{plan}");
}

#[test]
fn release_workflow_promotion_checks_prerelease_diff_and_binary_identity_without_a_rebuild() {
    let document = document();
    let name = "promote-verify";
    let text = job_text(&document, name);
    assert!(
        !text.contains("cargo"),
        "promotion must not rebuild: {text}"
    );

    let all = run_scripts(&document, name).join("\n");
    for needle in [
        "git merge-base --is-ancestor",
        "gh release view",
        "isPrerelease",
        "gh release download",
        "check-doc-only.sh",
        "promote-archives.sh",
    ] {
        assert!(all.contains(needle), "missing `{needle}`: {all}");
    }
    assert!(
        all.contains("v${BASE_VERSION}-rc.*"),
        "the lookup must list the rc tags of the same base version: {all}"
    );

    let lookup = step_index(&document, name, "gh release view");
    let diff = step_index(&document, name, "check-doc-only.sh");
    let download = step_index(&document, name, "gh release download");
    let promote = step_index(&document, name, "promote-archives.sh");
    assert!(
        lookup < diff && diff < download && download < promote,
        "lookup, diff, download, promote"
    );

    let checkout = &steps(&document, name)[0];
    assert_eq!(scalar(get(get(checkout, "with"), "fetch-depth")), "0");

    let diff_script = scalar(get(&steps(&document, name)[diff], "run"));
    assert!(
        diff_script.contains("check-doc-only.sh \"$RC_SHA\" \"$GITHUB_SHA\""),
        "{diff_script}"
    );

    // The publish job re-verifies the promoted set and attaches it as a normal release.
    let publish = run_scripts(&document, "publish-final").join("\n");
    assert!(
        publish.contains("verify-dist.sh dist \"$VERSION\""),
        "{publish}"
    );
    assert!(publish.contains("gh release create"), "{publish}");
    assert!(!publish.contains("--prerelease"), "{publish}");
    assert!(!job_text(&document, "publish-final").contains("fault-injection"));
}

#[test]
fn release_workflow_takes_expression_values_through_the_environment() {
    // A `${{ }}` value pasted into a script is shell injection; every run script must read the
    // context through `env:` instead.
    let document = document();
    for (name, _) in map(get(&document, "jobs")) {
        // The `ci` job calls a workflow and has no steps.
        if find(job(&document, name), "steps").is_none() {
            continue;
        }
        for script in run_scripts(&document, name) {
            assert!(
                !script.contains("${{"),
                "job `{name}` pastes an expression into a script: {script}"
            );
        }
    }
}

#[cfg(unix)]
mod scripts {
    use super::root;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    pub struct Outcome {
        pub success: bool,
        pub text: String,
    }

    /// Runs `bash <script> <args>` with a bounded wait; a hang is a failure, not a stall.
    pub fn run(script: &str, args: &[&str], stdin: &str, cwd: Option<&Path>) -> Outcome {
        let mut command = Command::new("bash");
        command
            .arg(root().join(".github/scripts").join(script))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().expect("bash starts");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("{script} did not finish within 60 seconds");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Outcome {
            success: output.status.success(),
            text,
        }
    }
}

#[cfg(unix)]
fn objdump_with(versions: &[&str]) -> String {
    let mut text = String::from("DYNAMIC SYMBOL TABLE:\n");
    for version in versions {
        text.push_str(&format!(
            "0000000000000000      DF *UND*\t0000000000000000 ({version}) symbol\n"
        ));
    }
    text
}

#[cfg(unix)]
#[test]
fn glibc_check_accepts_the_baseline_and_older_versions() {
    let input = objdump_with(&["GLIBC_2.2.5", "GLIBC_2.17", "GLIBC_2.28", "GLIBC_2.9"]);
    let outcome = scripts::run("check-glibc.sh", &["2.28"], &input, None);
    assert!(outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("highest GLIBC symbol version: 2.28"));
}

#[cfg(unix)]
#[test]
fn glibc_check_rejects_a_newer_version_using_version_order_not_text_order() {
    // 2.34 sorts before 2.9 and 2.100 before 2.28 as text; each must still count as newer.
    for newest in ["GLIBC_2.29", "GLIBC_2.34", "GLIBC_2.100", "GLIBC_3.0"] {
        let input = objdump_with(&[newest, "GLIBC_2.17"]);
        let outcome = scripts::run("check-glibc.sh", &["2.28"], &input, None);
        assert!(!outcome.success, "{newest} must fail: {}", outcome.text);
        assert!(outcome.text.contains("::error::"), "{}", outcome.text);
    }
}

#[cfg(unix)]
#[test]
fn glibc_check_fails_when_the_input_has_no_glibc_symbol() {
    for input in [
        String::new(),
        objdump_with(&["GLIBCXX_3.4", "GLIBC_PRIVATE"]),
    ] {
        let outcome = scripts::run("check-glibc.sh", &["2.28"], &input, None);
        assert!(!outcome.success, "{}", outcome.text);
        assert!(
            outcome.text.contains("no GLIBC_ symbol"),
            "{}",
            outcome.text
        );
    }
}

#[cfg(unix)]
fn coff_imports(dlls: &[&str], newline: &str) -> String {
    let mut text = String::new();
    for dll in dlls {
        text.push_str(&format!(
            "Import {{{newline}  Name: {dll}{newline}  ImportLookupTableRVA: 0x1000{newline}  Symbol: ReadFile (1){newline}}}{newline}"
        ));
    }
    text
}

#[cfg(unix)]
#[test]
fn imports_check_accepts_a_static_crt_import_table() {
    let input = coff_imports(
        &[
            "KERNEL32.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "bcrypt.dll",
        ],
        "\n",
    );
    let outcome = scripts::run("check-imports.sh", &[], &input, None);
    assert!(outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("KERNEL32.dll"), "{}", outcome.text);
}

#[cfg(unix)]
#[test]
fn imports_check_rejects_the_dynamic_vc_runtime_in_any_case_and_with_crlf() {
    for (dll, newline) in [
        ("VCRUNTIME140.dll", "\n"),
        ("vcruntime140_1.dll", "\r\n"),
        ("MSVCP140.dll", "\n"),
        ("msvcp140_2.DLL", "\r\n"),
    ] {
        let input = coff_imports(&["KERNEL32.dll", dll], newline);
        let outcome = scripts::run("check-imports.sh", &[], &input, None);
        assert!(!outcome.success, "{dll} must fail: {}", outcome.text);
        assert!(outcome.text.contains("::error::"), "{}", outcome.text);
        assert!(
            outcome.text.to_lowercase().contains(&dll.to_lowercase()),
            "the error names the DLL: {}",
            outcome.text
        );
    }
}

#[cfg(unix)]
#[test]
fn imports_check_fails_on_an_empty_import_table() {
    let outcome = scripts::run("check-imports.sh", &[], "", None);
    assert!(!outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("no imported DLL"), "{}", outcome.text);
}

#[cfg(unix)]
mod repository {
    use std::path::Path;
    use std::process::Command;

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    pub fn write(dir: &Path, path: &str, text: &str) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    /// A repository with a base commit holding README.md, docs/guide.md and src/main.rs.
    pub fn base() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "README.md", "one\n");
        write(dir.path(), "docs/guide.md", "one\n");
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        (dir, base)
    }

    pub fn commit(dir: &Path, message: &str) -> String {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }
}

#[cfg(unix)]
#[test]
fn doc_only_check_accepts_readme_and_docs_changes_including_a_deletion() {
    let (dir, base) = repository::base();
    repository::write(dir.path(), "README.md", "two\n");
    repository::write(dir.path(), "docs/new/deep/page.md", "new\n");
    std::fs::remove_file(dir.path().join("docs/guide.md")).unwrap();
    let head = repository::commit(dir.path(), "docs");

    let outcome = scripts::run("check-doc-only.sh", &[&base, &head], "", Some(dir.path()));
    assert!(outcome.success, "{}", outcome.text);
    assert!(
        outcome.text.contains("3 changed files, all documentation"),
        "{}",
        outcome.text
    );
}

#[cfg(unix)]
#[test]
fn doc_only_check_accepts_the_same_commit() {
    let (dir, base) = repository::base();
    let outcome = scripts::run("check-doc-only.sh", &[&base, &base], "", Some(dir.path()));
    assert!(outcome.success, "{}", outcome.text);
    assert!(
        outcome.text.contains("no changed files"),
        "{}",
        outcome.text
    );
}

#[cfg(unix)]
#[test]
fn doc_only_check_rejects_a_source_workflow_or_look_alike_path_and_names_it() {
    for path in [
        "src/main.rs",
        ".github/workflows/release.yml",
        "Cargo.toml",
        "docs.md",
        "README.md.bak",
        "tests/docs/page.md",
        "docsx/page.md",
    ] {
        let (dir, base) = repository::base();
        repository::write(dir.path(), "docs/guide.md", "two\n");
        repository::write(dir.path(), path, "changed\n");
        let head = repository::commit(dir.path(), "mixed");

        let outcome = scripts::run("check-doc-only.sh", &[&base, &head], "", Some(dir.path()));
        assert!(!outcome.success, "{path} must fail: {}", outcome.text);
        assert!(outcome.text.contains(path), "{path}: {}", outcome.text);
        assert!(outcome.text.contains("::error::"), "{}", outcome.text);
    }
}

#[cfg(unix)]
#[test]
fn doc_only_check_sees_a_rename_out_of_docs_as_a_source_change() {
    let (dir, base) = repository::base();
    std::fs::rename(
        dir.path().join("docs/guide.md"),
        dir.path().join("src/guide.md"),
    )
    .unwrap();
    let head = repository::commit(dir.path(), "move");

    let outcome = scripts::run("check-doc-only.sh", &[&base, &head], "", Some(dir.path()));
    assert!(!outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("src/guide.md"), "{}", outcome.text);
}

#[cfg(unix)]
#[test]
fn doc_only_check_sees_a_source_file_moved_into_docs_by_its_old_path() {
    let (dir, base) = repository::base();
    std::fs::rename(
        dir.path().join("src/main.rs"),
        dir.path().join("docs/main.md"),
    )
    .unwrap();
    let head = repository::commit(dir.path(), "move");

    let outcome = scripts::run("check-doc-only.sh", &[&base, &head], "", Some(dir.path()));
    assert!(!outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("src/main.rs"), "{}", outcome.text);
}

#[cfg(unix)]
#[test]
fn doc_only_check_fails_on_an_unknown_commit() {
    let (dir, base) = repository::base();
    let outcome = scripts::run(
        "check-doc-only.sh",
        &[&base, "0123456789abcdef0123456789abcdef01234567"],
        "",
        Some(dir.path()),
    );
    assert!(!outcome.success, "{}", outcome.text);
}

#[cfg(unix)]
mod dist {
    use super::{root, scripts};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub const TARGETS: [(&str, &str); 4] = [
        ("aarch64-apple-darwin", "tar.gz"),
        ("x86_64-pc-windows-msvc", "zip"),
        ("x86_64-unknown-linux-gnu", "tar.gz"),
        ("aarch64-unknown-linux-gnu", "tar.gz"),
    ];

    pub fn sha256(path: &Path) -> String {
        scripts::run("sha256.sh", &[path.to_str().unwrap()], "", None)
            .text
            .trim()
            .to_owned()
    }

    pub const LICENSE_FILES: [&str; 2] = ["LICENSE", "THIRD-PARTY-NOTICES"];

    pub fn executable(extension: &str) -> &'static str {
        if extension == "zip" {
            "abapreach.exe"
        } else {
            "abapreach"
        }
    }

    /// The three entries a release archive for `target` holds: the executable with target-specific
    /// bytes (so a swapped archive is detectable), and the two text files as the repository has them.
    pub fn release_entries(target: usize) -> Vec<(String, Vec<u8>)> {
        let (name, extension) = TARGETS[target];
        let mut entries = vec![(
            executable(extension).to_owned(),
            format!("binary for {name}\n").into_bytes(),
        )];
        for file in LICENSE_FILES {
            entries.push((file.to_owned(), std::fs::read(root().join(file)).unwrap()));
        }
        entries
    }

    /// Writes (or replaces) the archive of `target` for `version` holding exactly `entries`, and
    /// its SHA256 file.
    pub fn write_archive(dir: &Path, version: &str, target: usize, entries: &[(String, Vec<u8>)]) {
        std::fs::create_dir_all(dir).unwrap();
        let work = tempfile::tempdir().unwrap();
        for (name, bytes) in entries {
            std::fs::write(work.path().join(name), bytes).unwrap();
        }
        let names = entries.iter().map(|(name, _)| name.as_str());
        let archive = path(dir, version, target);
        let _ = std::fs::remove_file(&archive);
        let status = if TARGETS[target].1 == "zip" {
            Command::new("zip")
                .current_dir(work.path())
                .args(["-q", "-X"])
                .arg(&archive)
                .args(names)
                .status()
        } else {
            Command::new("tar")
                .arg("-C")
                .arg(work.path())
                .arg("-czf")
                .arg(&archive)
                .args(names)
                .status()
        };
        assert!(status.expect("archiver runs").success());
        let name = archive.file_name().unwrap().to_str().unwrap().to_owned();
        std::fs::write(
            dir.join(format!("{name}.sha256")),
            format!("{}  {name}\n", sha256(&archive)),
        )
        .unwrap();
    }

    /// A release set for `version`: four archives that each hold the executable, LICENSE and
    /// THIRD-PARTY-NOTICES, plus their SHA256 files.
    pub fn release_set(dir: &Path, version: &str) {
        for target in 0..TARGETS.len() {
            write_archive(dir, version, target, &release_entries(target));
        }
    }

    pub fn path(dir: &Path, version: &str, target: usize) -> PathBuf {
        let (name, extension) = TARGETS[target];
        dir.join(format!("abapreach-{version}-{name}.{extension}"))
    }
}

#[cfg(unix)]
#[test]
fn sha256_script_hashes_a_file_and_standard_input() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("abc");
    std::fs::write(&file, "abc").unwrap();
    let known = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(dist::sha256(&file), known);
    let outcome = scripts::run("sha256.sh", &[], "abc", None);
    assert_eq!(outcome.text.trim(), known);
}

#[cfg(unix)]
#[test]
fn verify_dist_accepts_exactly_the_four_archives_with_matching_checksums() {
    let dir = tempfile::tempdir().unwrap();
    dist::release_set(dir.path(), "0.12.0-rc.1");
    let outcome = scripts::run(
        "verify-dist.sh",
        &[dir.path().to_str().unwrap(), "0.12.0-rc.1"],
        "",
        None,
    );
    assert!(outcome.success, "{}", outcome.text);
}

#[cfg(unix)]
#[test]
fn verify_dist_rejects_a_fault_archive_a_missing_target_and_a_wrong_version() {
    let verify = |dir: &std::path::Path, version: &str| {
        scripts::run(
            "verify-dist.sh",
            &[dir.to_str().unwrap(), version],
            "",
            None,
        )
    };

    let dir = tempfile::tempdir().unwrap();
    dist::release_set(dir.path(), "0.12.0-rc.1");
    std::fs::write(
        dir.path()
            .join("abapreach-0.12.0-rc.1-x86_64-unknown-linux-gnu-fault-injection.tar.gz"),
        "fault",
    )
    .unwrap();
    let outcome = verify(dir.path(), "0.12.0-rc.1");
    assert!(!outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("fault-injection"), "{}", outcome.text);

    let dir = tempfile::tempdir().unwrap();
    dist::release_set(dir.path(), "0.12.0-rc.1");
    std::fs::remove_file(dist::path(dir.path(), "0.12.0-rc.1", 3)).unwrap();
    let outcome = verify(dir.path(), "0.12.0-rc.1");
    assert!(!outcome.success, "{}", outcome.text);
    assert!(
        outcome.text.contains("aarch64-unknown-linux-gnu"),
        "{}",
        outcome.text
    );

    let dir = tempfile::tempdir().unwrap();
    dist::release_set(dir.path(), "0.12.0-rc.1");
    let outcome = verify(dir.path(), "0.12.0");
    assert!(!outcome.success, "{}", outcome.text);
}

#[cfg(unix)]
#[test]
fn verify_dist_rejects_a_tampered_archive() {
    let dir = tempfile::tempdir().unwrap();
    dist::release_set(dir.path(), "0.12.0-rc.1");
    let archive = dist::path(dir.path(), "0.12.0-rc.1", 0);
    let mut bytes = std::fs::read(&archive).unwrap();
    bytes.push(0);
    std::fs::write(&archive, bytes).unwrap();

    let outcome = scripts::run(
        "verify-dist.sh",
        &[dir.path().to_str().unwrap(), "0.12.0-rc.1"],
        "",
        None,
    );
    assert!(!outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("SHA256 mismatch"), "{}", outcome.text);
}

#[cfg(unix)]
fn verify(dir: &std::path::Path, version: &str) -> scripts::Outcome {
    scripts::run(
        "verify-dist.sh",
        &[dir.to_str().unwrap(), version],
        "",
        None,
    )
}

#[cfg(unix)]
#[test]
fn verify_dist_rejects_an_archive_that_does_not_hold_exactly_the_three_entries() {
    // (case, entries kept by name, extra entry)
    let cases: [(&str, &[&str], Option<&str>); 5] = [
        ("only the executable", &["EXE"], None),
        ("no LICENSE", &["EXE", "THIRD-PARTY-NOTICES"], None),
        ("no THIRD-PARTY-NOTICES", &["EXE", "LICENSE"], None),
        (
            "an extra file",
            &["EXE", "LICENSE", "THIRD-PARTY-NOTICES"],
            Some("README.md"),
        ),
        (
            "no executable",
            &["LICENSE", "THIRD-PARTY-NOTICES"],
            Some("other"),
        ),
    ];
    for target in 0..dist::TARGETS.len() {
        for (case, kept, extra) in cases {
            let dir = tempfile::tempdir().unwrap();
            dist::release_set(dir.path(), "0.12.0-rc.1");
            let exe = dist::executable(dist::TARGETS[target].1);
            let mut entries = dist::release_entries(target)
                .into_iter()
                .filter(|(name, _)| {
                    kept.contains(&name.as_str()) || (name.as_str() == exe && kept.contains(&"EXE"))
                })
                .collect::<Vec<_>>();
            if let Some(extra) = extra {
                entries.push((extra.to_owned(), b"extra\n".to_vec()));
            }
            dist::write_archive(dir.path(), "0.12.0-rc.1", target, &entries);

            let outcome = verify(dir.path(), "0.12.0-rc.1");
            let archive = dist::TARGETS[target].0;
            assert!(!outcome.success, "{archive}, {case}: {}", outcome.text);
            assert!(outcome.text.contains("::error::"), "{}", outcome.text);
            assert!(
                outcome.text.contains(archive),
                "{archive}, {case}: {}",
                outcome.text
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn verify_dist_rejects_an_executable_named_for_another_target() {
    // The Windows archive must hold abapreach.exe, the others abapreach.
    let dir = tempfile::tempdir().unwrap();
    dist::release_set(dir.path(), "0.12.0-rc.1");
    let entries = dist::release_entries(1)
        .into_iter()
        .map(|(name, bytes)| (name.replace("abapreach.exe", "abapreach"), bytes))
        .collect::<Vec<_>>();
    dist::write_archive(dir.path(), "0.12.0-rc.1", 1, &entries);

    let outcome = verify(dir.path(), "0.12.0-rc.1");
    assert!(!outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("abapreach.exe"), "{}", outcome.text);
}

#[cfg(unix)]
#[test]
fn verify_dist_rejects_license_files_that_differ_from_the_checked_out_commit() {
    for file in dist::LICENSE_FILES {
        for target in [0, 1] {
            let dir = tempfile::tempdir().unwrap();
            dist::release_set(dir.path(), "0.12.0-rc.1");
            let entries = dist::release_entries(target)
                .into_iter()
                .map(|(name, mut bytes)| {
                    if name == file {
                        bytes.extend_from_slice(b"changed\n");
                    }
                    (name, bytes)
                })
                .collect::<Vec<_>>();
            dist::write_archive(dir.path(), "0.12.0-rc.1", target, &entries);

            let outcome = verify(dir.path(), "0.12.0-rc.1");
            assert!(!outcome.success, "{file}, {target}: {}", outcome.text);
            assert!(outcome.text.contains("::error::"), "{}", outcome.text);
            assert!(outcome.text.contains(file), "{file}: {}", outcome.text);
            assert!(
                outcome.text.contains(dist::TARGETS[target].0),
                "{}",
                outcome.text
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn promotion_refuses_an_rc_archive_with_the_wrong_entries_or_license_files() {
    // An old-layout archive (executable only) and an archive whose LICENSE differs from the tag
    // commit's both carry a valid checksum, so only the content checks can stop them.
    for case in ["executable only", "changed LICENSE"] {
        let work = tempfile::tempdir().unwrap();
        let rc = work.path().join("rc");
        let out = work.path().join("promoted");
        dist::release_set(&rc, "0.12.0-rc.2");
        let entries = dist::release_entries(2)
            .into_iter()
            .filter(|(name, _)| case != "executable only" || name == "abapreach")
            .map(|(name, mut bytes)| {
                if name == "LICENSE" {
                    bytes.extend_from_slice(b"changed\n");
                }
                (name, bytes)
            })
            .collect::<Vec<_>>();
        dist::write_archive(&rc, "0.12.0-rc.2", 2, &entries);

        let outcome = scripts::run(
            "promote-archives.sh",
            &[
                rc.to_str().unwrap(),
                "0.12.0-rc.2",
                out.to_str().unwrap(),
                "0.12.0",
            ],
            "",
            None,
        );
        assert!(!outcome.success, "{case}: {}", outcome.text);
        assert!(
            !out.exists() || std::fs::read_dir(&out).unwrap().next().is_none(),
            "{case}: nothing is promoted"
        );
    }
}

#[cfg(unix)]
#[test]
fn promotion_renames_the_rc_archives_and_keeps_every_binary_hash() {
    let work = tempfile::tempdir().unwrap();
    let rc = work.path().join("rc");
    let out = work.path().join("promoted");
    dist::release_set(&rc, "0.12.0-rc.2");

    let outcome = scripts::run(
        "promote-archives.sh",
        &[
            rc.to_str().unwrap(),
            "0.12.0-rc.2",
            out.to_str().unwrap(),
            "0.12.0",
        ],
        "",
        None,
    );
    assert!(outcome.success, "{}", outcome.text);

    for (index, (target, _)) in dist::TARGETS.iter().enumerate() {
        let before = dist::path(&rc, "0.12.0-rc.2", index);
        let after = dist::path(&out, "0.12.0", index);
        assert_eq!(
            dist::sha256(&before),
            dist::sha256(&after),
            "{target}: the archive bytes are the tested bytes"
        );
        let recorded = std::fs::read_to_string(format!("{}.sha256", after.display())).unwrap();
        let name = after.file_name().unwrap().to_str().unwrap();
        assert_eq!(recorded, format!("{}  {name}\n", dist::sha256(&after)));
        assert!(
            outcome
                .text
                .contains(&format!("{target}: binary SHA256 identical")),
            "{}",
            outcome.text
        );
    }
    let mut names = std::fs::read_dir(&out)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names.len(), 8, "{names:?}");
    assert!(
        names
            .iter()
            .all(|name| name.starts_with("abapreach-0.12.0-"))
    );
}

#[cfg(unix)]
#[test]
fn promotion_refuses_an_rc_set_whose_archive_does_not_match_its_checksum() {
    let work = tempfile::tempdir().unwrap();
    let rc = work.path().join("rc");
    let out = work.path().join("promoted");
    dist::release_set(&rc, "0.12.0-rc.2");
    let archive = dist::path(&rc, "0.12.0-rc.2", 1);
    let mut bytes = std::fs::read(&archive).unwrap();
    bytes.push(0);
    std::fs::write(&archive, bytes).unwrap();

    let outcome = scripts::run(
        "promote-archives.sh",
        &[
            rc.to_str().unwrap(),
            "0.12.0-rc.2",
            out.to_str().unwrap(),
            "0.12.0",
        ],
        "",
        None,
    );
    assert!(!outcome.success, "{}", outcome.text);
    assert!(
        !out.exists() || std::fs::read_dir(&out).unwrap().next().is_none(),
        "nothing is promoted from a bad rc set"
    );
}

#[cfg(unix)]
#[test]
fn promotion_refuses_to_overwrite_an_existing_output_directory_content() {
    let work = tempfile::tempdir().unwrap();
    let rc = work.path().join("rc");
    let out = work.path().join("promoted");
    dist::release_set(&rc, "0.12.0-rc.2");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("leftover"), "x").unwrap();

    let outcome = scripts::run(
        "promote-archives.sh",
        &[
            rc.to_str().unwrap(),
            "0.12.0-rc.2",
            out.to_str().unwrap(),
            "0.12.0",
        ],
        "",
        None,
    );
    assert!(!outcome.success, "{}", outcome.text);
    assert!(outcome.text.contains("not empty"), "{}", outcome.text);
}

#[test]
fn the_changelog_keeps_the_unreleased_0_16_0_section_above_the_released_0_15_0_section() {
    let text = std::fs::read_to_string(root().join("CHANGELOG.md")).unwrap();
    let next = text
        .find("## [Unreleased]")
        .expect("the Unreleased heading");
    let released = text
        .find("## [0.15.0] - 2026-11-07")
        .expect("the 0.15.0 heading");

    assert!(next < released);
}

#[test]
fn the_changelog_keeps_the_released_0_14_0_section_above_its_entries() {
    let text = std::fs::read_to_string(root().join("CHANGELOG.md")).unwrap();
    let heading = text
        .find("## [0.14.0] - 2026-11-04")
        .expect("the 0.14.0 heading");
    let entry = text.find("- `read-table` command").expect("a 0.14.0 entry");
    let current = text.find("## [0.15.0]").expect("the 0.15.0 heading");

    assert!(current < heading && heading < entry);
}
