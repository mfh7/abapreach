//! The runner fault targets exist only in `fault-injection` builds (M6 Task 16): the normal binary
//! ignores `ABAPREACH_FAULT`, and every fault site of the runner is behind the feature gate.

use std::process::Command;

fn version(fault: &str) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_abapreach"))
        .arg("--version")
        .env("ABAPREACH_FAULT", fault)
        .output()
        .expect("run the binary");

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[cfg(not(feature = "fault-injection"))]
#[test]
fn the_normal_binary_ignores_every_runner_fault() {
    for fault in [
        "drop-reply:CLASSRUN",
        "drop-reply:CREATE",
        "drop-reply:PUT",
        "drop-reply:ACTIVATE",
        "drop-reply:DELETE",
    ] {
        let (success, stdout, stderr) = version(fault);

        assert!(success, "{fault}: {stderr}");
        assert!(stdout.starts_with("abapreach "), "{fault}: {stdout}");
        assert!(!stderr.contains("fault injection"), "{fault}: {stderr}");
    }
}

#[cfg(feature = "fault-injection")]
#[test]
fn the_fault_binary_announces_a_runner_fault_and_refuses_an_unknown_one() {
    let (_, _, stderr) = version("drop-reply:CLASSRUN");

    assert!(
        stderr.contains("WARNING: fault injection active (drop-reply:CLASSRUN)"),
        "{stderr}"
    );

    let (success, _, stderr) = version("drop-reply:CLASSRUNS");

    assert!(!success);
    assert!(
        stderr.contains("STOP: unknown ABAPREACH_FAULT value"),
        "{stderr}"
    );
}

/// Whether the first line above `needle` that is not a comment or blank is the feature gate.
fn gated(source: &str, needle: &str) -> bool {
    let source = source.replace("\r\n", "\n");
    let lines: Vec<&str> = source.lines().collect();
    let hits: Vec<usize> = (0..lines.len())
        .filter(|index| lines[*index].contains(needle))
        .collect();

    assert!(!hits.is_empty(), "{needle} not found");

    hits.into_iter().all(|hit| {
        lines[..hit]
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with("//"))
            .is_some_and(|line| line.trim() == "#[cfg(feature = \"fault-injection\")]")
    })
}

#[test]
fn every_runner_fault_site_is_behind_the_feature_gate() {
    let runner = include_str!("../src/operation/runner.rs");
    let admission = include_str!("../src/transport/admission.rs");
    let rfc = include_str!("../src/transport/rfc.rs");

    for needle in [
        "faults: Vec<crate::fault::Fault>,",
        "faults: crate::fault::Fault::from_env(),",
        "let dropped = self.faults.iter()",
        "if let Some(error) = dropped {",
    ] {
        assert!(gated(runner, needle), "{needle}");
    }

    assert!(gated(admission, "pub fn is_classrun_post("));
    assert!(gated(rfc, "pub(crate) fn is_fault_target("));
}
