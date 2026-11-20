//! The arguments and the safety gates of `run`: what the command line and the MCP tool may ask
//! for, the name rule, the connection policy matrix, and the plan file that binds a run to the
//! simulate that preceded it. Pure logic; nothing here talks to SAP.

use crate::cli::{AlvColumns, AlvMode, RunKind};
use crate::operation::delete::sha256_hex;
use crate::operation::runner::{RUNNER_CLASS_REFUSED, is_runner_class};
use crate::policy::{McpRun, McpRunSnippet, Policies, RunPolicy};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Parameter names of a report selection screen: `[A-Z0-9_]{1,8}`.
const PARAM_NAME_MAX: usize = 8;
/// The most characters of a parameter value: the length of `RSPARAMS-LOW`.
pub const PARAM_VALUE_MAX: usize = 45;
const REPORT_NAME_MAX: usize = 40;
const CLASS_NAME_MAX: usize = 30;
const VARIANT_NAME_MAX: usize = 14;
/// The version of the plan file; a file of another version is refused.
const PLAN_VERSION: u32 = 1;

/// A snippet and its hash: the hash is what the plan binds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snippet {
    pub text: String,
    pub sha256: String,
}

impl Snippet {
    /// A snippet text; an empty one runs nothing and is refused.
    pub fn new(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Err("the snippet is empty".to_owned());
        }

        if text.contains('\0') {
            return Err("the snippet contains a NUL character".to_owned());
        }

        Ok(Self {
            text: text.to_owned(),
            sha256: sha256_hex(text),
        })
    }
}

/// The arguments of a `run` as given, before any rule is applied.
#[derive(Clone, Copy, Debug)]
pub struct RawArgs<'a> {
    pub kind: RunKind,
    pub target: &'a str,
    pub params: &'a [String],
    pub variant: Option<&'a str>,
    pub alv: Option<AlvMode>,
    pub alv_columns: Option<AlvColumns>,
    pub max_output: Option<u64>,
}

/// The arguments of a `run` after the local rules: names upper-cased, parameters sorted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunArgs {
    pub kind: RunKind,
    /// The class or report name, upper-case; `SNIPPET` for a snippet.
    pub name: String,
    pub params: Vec<(String, String)>,
    pub variant: Option<String>,
    /// `Some` for `PROG` only; the default is `on`.
    pub alv: Option<AlvMode>,
    pub alv_columns: Option<AlvColumns>,
    pub snippet: Option<Snippet>,
}

/// The label a snippet run carries in plans and messages.
pub const SNIPPET_LABEL: &str = "SNIPPET";

impl RunArgs {
    /// Applies the argument rules of `kind`. `snippet` is the text of the snippet file for a
    /// `SNIPPET` run. Every `Err` is the text of a `STOP`.
    pub fn parse(raw: &RawArgs<'_>, snippet: Option<&str>) -> Result<Self, String> {
        let only_prog = |given: bool, option: &str| {
            if given && raw.kind != RunKind::Prog {
                return Err(format!("{option} applies to PROG only"));
            }

            Ok(())
        };

        only_prog(!raw.params.is_empty(), "--param")?;
        only_prog(raw.variant.is_some(), "--variant")?;
        only_prog(raw.alv.is_some(), "--alv")?;
        only_prog(raw.alv_columns.is_some(), "--alv-columns")?;

        if raw.kind == RunKind::Clas && raw.max_output.is_some() {
            return Err(
                "--max-output applies to PROG and SNIPPET only: a class run prints the whole \
                 classrun reply"
                    .to_owned(),
            );
        }

        match raw.kind {
            RunKind::Clas => Ok(Self {
                kind: raw.kind,
                name: class_name(raw.target)?,
                params: Vec::new(),
                variant: None,
                alv: None,
                alv_columns: None,
                snippet: None,
            }),
            RunKind::Prog => {
                let alv = raw.alv.unwrap_or(AlvMode::On);

                if alv == AlvMode::Off && raw.alv_columns.is_some() {
                    return Err("--alv-columns needs --alv on".to_owned());
                }

                Ok(Self {
                    kind: raw.kind,
                    name: report_name(raw.target)?,
                    params: parameters(raw.params)?,
                    variant: raw.variant.map(variant_name).transpose()?,
                    alv: Some(alv),
                    alv_columns: (alv == AlvMode::On)
                        .then(|| raw.alv_columns.unwrap_or(AlvColumns::Visible)),
                    snippet: None,
                })
            }
            RunKind::Snippet => Ok(Self {
                kind: raw.kind,
                name: SNIPPET_LABEL.to_owned(),
                params: Vec::new(),
                variant: None,
                alv: None,
                alv_columns: None,
                snippet: Some(Snippet::new(
                    snippet.ok_or("a SNIPPET run needs a snippet file")?,
                )?),
            }),
        }
    }

    /// The name rule: Z and Y names run; anything else only when `allowed` (the CLI flag
    /// `--allow-standard`, or the exact name on `mcp_run_allow`). A snippet has no name.
    pub fn check_scope(&self, allowed: bool) -> Result<(), String> {
        if self.kind == RunKind::Snippet || allowed || self.name.starts_with(['Z', 'Y']) {
            return Ok(());
        }

        let what = match self.kind {
            RunKind::Clas => "class",
            RunKind::Prog | RunKind::Snippet => "report",
        };

        Err(format!(
            "{what} {} is outside the Z/Y scope; --allow-standard allows a standard {what}",
            self.name
        ))
    }

    /// A `CLAS` run refuses a temporary runner class of the connection (its `runner_prefix` or the
    /// default one): a leftover carries its own payload, which is not what `run CLAS` is for.
    pub fn check_not_runner(&self, runner_prefix: &str) -> Result<(), String> {
        if self.kind == RunKind::Clas && is_runner_class(&self.name, runner_prefix) {
            return Err(format!("{}: {RUNNER_CLASS_REFUSED}", self.name));
        }

        Ok(())
    }

    /// The identity the plan file binds: everything the arguments say, and the connection.
    pub fn plan(&self, conn: &str, client: &str, backend: &str, facts: PlanFacts) -> RunPlan {
        RunPlan {
            version: PLAN_VERSION,
            conn: conn.to_owned(),
            client: client.to_owned(),
            backend: backend.to_owned(),
            kind: self.kind.token().to_owned(),
            name: self.name.clone(),
            params: self.params.clone(),
            variant: self.variant.clone(),
            alv: self.alv.map(|alv| alv.token().to_owned()),
            alv_columns: self.alv_columns.map(|columns| columns.token().to_owned()),
            snippet_sha256: self.snippet.as_ref().map(|snippet| snippet.sha256.clone()),
            facts,
        }
    }
}

fn class_name(text: &str) -> Result<String, String> {
    let name = text.trim().to_ascii_uppercase();

    if name.starts_with('/') {
        return Err(format!(
            "a namespaced class name has no verified classrun path yet: {name}"
        ));
    }

    let valid = !name.is_empty()
        && name.len() <= CLASS_NAME_MAX
        && name.starts_with(|first: char| first.is_ascii_alphabetic())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');

    if valid {
        Ok(name)
    } else {
        Err(format!(
            "class name must be 1-{CLASS_NAME_MAX} characters of A-Z, 0-9 and _, starting with a \
             letter: {text:?}"
        ))
    }
}

fn report_name(text: &str) -> Result<String, String> {
    let name = text.trim().to_ascii_uppercase();
    let valid = !name.is_empty()
        && name.len() <= REPORT_NAME_MAX
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'/');

    if valid {
        Ok(name)
    } else {
        Err(format!(
            "report name must be 1-{REPORT_NAME_MAX} characters of A-Z, 0-9, _ and /: {text:?}"
        ))
    }
}

fn variant_name(text: &str) -> Result<String, String> {
    let name = text.trim().to_ascii_uppercase();
    let valid = !name.is_empty()
        && name.len() <= VARIANT_NAME_MAX
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'&' || byte == b'%'
        });

    if valid {
        Ok(name)
    } else {
        Err(format!(
            "--variant must be 1-{VARIANT_NAME_MAX} characters of A-Z, 0-9, _, & and %: {text:?}"
        ))
    }
}

/// The `NAME=VALUE` parameters, names upper-cased, sorted by name. A name repeats nowhere, and a
/// value is at most [`PARAM_VALUE_MAX`] characters without control characters.
fn parameters(given: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut parsed: Vec<(String, String)> = Vec::new();

    for text in given {
        let (name, value) = text
            .split_once('=')
            .ok_or_else(|| format!("--param must be NAME=VALUE: {text:?}"))?;
        let name = name.trim().to_ascii_uppercase();

        if name.is_empty()
            || name.len() > PARAM_NAME_MAX
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!(
                "parameter name must be 1-{PARAM_NAME_MAX} characters of A-Z, 0-9 and _: {name:?}"
            ));
        }

        if value.encode_utf16().count() > PARAM_VALUE_MAX {
            return Err(format!(
                "parameter {name}: the value has more than {PARAM_VALUE_MAX} characters"
            ));
        }

        if value.chars().any(char::is_control) {
            return Err(format!(
                "parameter {name}: the value has a control character"
            ));
        }

        if parsed.iter().any(|(known, _)| *known == name) {
            return Err(format!("duplicate parameter: {name}"));
        }

        parsed.push((name, value.to_owned()));
    }

    parsed.sort();

    Ok(parsed)
}

/// What the live system says and the plan binds: the active source hash, the package, and for a
/// report its `SUBC`. The author is shown, never bound.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanFacts {
    /// The active source hash; absent for a snippet, whose own hash is bound by the arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subc: Option<String>,
}

/// The plan file of a `run`: written by `--simulate`, read by the run that follows.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunPlan {
    pub version: u32,
    pub conn: String,
    pub client: String,
    /// The backend token the simulate ran on; the run is pinned to it.
    pub backend: String,
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alv: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alv_columns: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_sha256: Option<String>,
    pub facts: PlanFacts,
}

impl RunPlan {
    /// Reads the plan file of a run.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("plan file cannot be read: {}: {error}", path.display()))?;
        let plan: Self = serde_json::from_str(&text)
            .map_err(|error| format!("plan file cannot be read: {}: {error}", path.display()))?;

        if plan.version != PLAN_VERSION {
            return Err(format!(
                "plan file {} has version {}; this program writes version {PLAN_VERSION}; \
                 simulate again",
                path.display(),
                plan.version
            ));
        }

        Ok(plan)
    }

    /// Writes the plan file; an existing file is replaced.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;

        std::fs::write(path, json)
            .map_err(|error| format!("plan file cannot be written: {}: {error}", path.display()))
    }

    /// The first argument of the run that the plan does not bind to, or `None` when the plan is
    /// the plan of these arguments on this connection. `expected` is built from the arguments
    /// with the facts left out. A mismatch is a `STOP` before any connection.
    pub fn argument_mismatch(&self, expected: &Self) -> Option<&'static str> {
        [
            ("conn", self.conn == expected.conn),
            ("client", self.client == expected.client),
            ("type", self.kind == expected.kind),
            ("name", self.name == expected.name),
            ("--param", self.params == expected.params),
            ("--variant", self.variant == expected.variant),
            ("--alv", self.alv == expected.alv),
            ("--alv-columns", self.alv_columns == expected.alv_columns),
            ("snippet", self.snippet_sha256 == expected.snippet_sha256),
        ]
        .into_iter()
        .find_map(|(field, equal)| (!equal).then_some(field))
    }

    /// The first live fact that differs from the plan, or `None` when everything still matches.
    pub fn fact_mismatch(&self, live: &PlanFacts) -> Option<&'static str> {
        [
            ("source hash", self.facts.hash == live.hash),
            ("package", self.facts.package == live.package),
            ("SUBC", self.facts.subc == live.subc),
        ]
        .into_iter()
        .find_map(|(field, equal)| (!equal).then_some(field))
    }
}

/// The STOP text of an argument that the plan does not bind to.
pub fn plan_mismatch_text(field: &str) -> String {
    format!("plan does not match the arguments ({field}); simulate again")
}

/// The reason a real run is refused because the live facts moved on.
pub fn plan_changed_text(field: &str) -> String {
    format!("plan changed since simulate ({field}); simulate again")
}

/// The CLI gate of the `run` policy: whether `run` may start, and whether it needs a plan file.
/// `simulate` writes the plan under `plan`, a real run needs it under `plan`; `direct` needs it
/// for neither; `off` refuses both.
pub fn cli_gate(
    policies: &Policies,
    conn: &str,
    simulate: bool,
    plan: Option<&Path>,
) -> Result<(), String> {
    match policies.run {
        RunPolicy::Off => Err(format!("run is off for {conn} (policy run)")),
        RunPolicy::Direct => Ok(()),
        RunPolicy::Plan if plan.is_some() => Ok(()),
        RunPolicy::Plan if simulate => Err(format!(
            "--simulate needs --plan <FILE> to write the plan for {conn} (policy run is plan)"
        )),
        RunPolicy::Plan => Err(format!(
            "run needs --plan <FILE> from run --simulate for {conn} (policy run is plan)"
        )),
    }
}

/// The MCP gate: `run: off` refuses first, whatever `mcp_run` says; then `mcp_run`, the snippet
/// opt-in, and the exact `TYPE:NAME` on `mcp_run_allow`. MCP ignores `run: direct`.
pub fn mcp_gate(policies: &Policies, conn: &str, args: &RunArgs) -> Result<(), String> {
    if args.kind == RunKind::Snippet {
        mcp_switches(policies, conn)?;

        return match policies.mcp_run_snippet {
            McpRunSnippet::On => Ok(()),
            McpRunSnippet::Off => Err(format!(
                "MCP snippet runs are off for {conn} (policy mcp_run_snippet)"
            )),
        };
    }

    mcp_object_gate(policies, conn, args.kind.token(), &args.name)
}

/// The MCP gate of one named object of `object_type` (`CLAS`, `PROG` or `FUGR`): the switches of
/// [`mcp_gate`], then the exact `TYPE:NAME` on `mcp_run_allow`. `abap_aunit` uses it for every
/// object of a call.
pub fn mcp_object_gate(
    policies: &Policies,
    conn: &str,
    object_type: &str,
    name: &str,
) -> Result<(), String> {
    mcp_switches(policies, conn)?;

    if policies.is_run_allowed(object_type, name) {
        return Ok(());
    }

    Err(format!(
        "{object_type}:{} is not on mcp_run_allow of {conn}",
        name.trim().to_ascii_uppercase()
    ))
}

/// `run: off` refuses first, whatever `mcp_run` says; then `mcp_run`.
pub fn mcp_switches(policies: &Policies, conn: &str) -> Result<(), String> {
    if policies.run == RunPolicy::Off {
        return Err(format!("run is off for {conn} (policy run)"));
    }

    if policies.mcp_run == McpRun::Off {
        return Err(format!("MCP runs are off for {conn} (policy mcp_run)"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: RunKind, target: &str) -> RawArgs<'_> {
        RawArgs {
            kind,
            target,
            params: &[],
            variant: None,
            alv: None,
            alv_columns: None,
            max_output: None,
        }
    }

    fn policies(run: RunPolicy, mcp: McpRun) -> Policies {
        Policies {
            run,
            mcp_run: mcp,
            mcp_run_allow: vec!["CLAS:ZCL_OK".to_owned()],
            ..Policies::default()
        }
    }

    #[test]
    fn a_class_name_is_upper_cased_and_checked() {
        let args = RunArgs::parse(&raw(RunKind::Clas, "zcl_demo"), None).unwrap();

        assert_eq!(args.name, "ZCL_DEMO");
        assert!(RunArgs::parse(&raw(RunKind::Clas, "zcl demo"), None).is_err());
        assert!(RunArgs::parse(&raw(RunKind::Clas, ""), None).is_err());
        assert!(RunArgs::parse(&raw(RunKind::Clas, &"Z".repeat(31)), None).is_err());
        assert!(RunArgs::parse(&raw(RunKind::Clas, "1ZCL"), None).is_err());
        assert!(
            RunArgs::parse(&raw(RunKind::Clas, "/NS/ZCL"), None)
                .unwrap_err()
                .contains("namespaced")
        );
    }

    #[test]
    fn options_of_the_report_are_refused_on_a_class() {
        let params = ["A=1".to_owned()];
        let cases = [
            (
                RawArgs {
                    params: &params,
                    ..raw(RunKind::Clas, "ZCL")
                },
                "--param",
            ),
            (
                RawArgs {
                    variant: Some("V"),
                    ..raw(RunKind::Clas, "ZCL")
                },
                "--variant",
            ),
            (
                RawArgs {
                    alv: Some(AlvMode::On),
                    ..raw(RunKind::Clas, "ZCL")
                },
                "--alv",
            ),
            (
                RawArgs {
                    alv_columns: Some(AlvColumns::All),
                    ..raw(RunKind::Clas, "ZCL")
                },
                "--alv-columns",
            ),
            (
                RawArgs {
                    max_output: Some(10),
                    ..raw(RunKind::Clas, "ZCL")
                },
                "--max-output",
            ),
        ];

        for (arguments, option) in cases {
            let error = RunArgs::parse(&arguments, None).unwrap_err();

            assert!(error.starts_with(option), "{error}");
        }
    }

    #[test]
    fn report_parameters_are_checked_sorted_and_unique() {
        let params = ["p_b=two".to_owned(), "P_A=one value".to_owned()];
        let args = RunArgs::parse(
            &RawArgs {
                params: &params,
                variant: Some("sap&test"),
                ..raw(RunKind::Prog, "zreport")
            },
            None,
        )
        .unwrap();

        assert_eq!(args.name, "ZREPORT");
        assert_eq!(
            args.params,
            [
                ("P_A".to_owned(), "one value".to_owned()),
                ("P_B".to_owned(), "two".to_owned())
            ]
        );
        assert_eq!(args.variant.as_deref(), Some("SAP&TEST"));
        assert_eq!(args.alv, Some(AlvMode::On));
        assert_eq!(args.alv_columns, Some(AlvColumns::Visible));
    }

    #[test]
    fn a_parameter_that_breaks_a_rule_is_refused() {
        let check = |text: &str| {
            RunArgs::parse(
                &RawArgs {
                    params: &[text.to_owned()],
                    ..raw(RunKind::Prog, "ZREPORT")
                },
                None,
            )
            .unwrap_err()
        };

        assert!(check("NOVALUE").contains("NAME=VALUE"));
        assert!(check("TOOLONGNAME=1").contains("parameter name"));
        assert!(check("=1").contains("parameter name"));
        assert!(check("A-B=1").contains("parameter name"));
        assert!(check(&format!("A={}", "x".repeat(46))).contains("more than 45"));
        // RSPARAMS-LOW counts UTF-16 units: 44 + a surrogate pair is 46.
        assert!(check(&format!("A={}\u{1f600}", "x".repeat(44))).contains("more than 45"));
        assert!(
            RunArgs::parse(
                &RawArgs {
                    params: &[format!("A={}\u{1f600}", "x".repeat(43))],
                    ..raw(RunKind::Prog, "ZREPORT")
                },
                None,
            )
            .is_ok()
        );
        assert!(check("A=x\ny").contains("control character"));

        let twice = ["A=1".to_owned(), "a=2".to_owned()];
        let error = RunArgs::parse(
            &RawArgs {
                params: &twice,
                ..raw(RunKind::Prog, "ZREPORT")
            },
            None,
        )
        .unwrap_err();

        assert_eq!(error, "duplicate parameter: A");
        assert!(RunArgs::parse(&raw(RunKind::Prog, "ZREPORT"), None).is_ok());
        assert!(
            RunArgs::parse(
                &RawArgs {
                    params: &[format!("A={}", "x".repeat(45))],
                    ..raw(RunKind::Prog, "ZREPORT")
                },
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn a_variant_and_a_report_name_are_checked() {
        let variant = |text: &'static str| {
            RunArgs::parse(
                &RawArgs {
                    variant: Some(text),
                    ..raw(RunKind::Prog, "ZREPORT")
                },
                None,
            )
        };

        assert!(variant("ABCDEFGHIJKLMNO").is_err());
        assert!(variant("A B").is_err());
        assert!(variant("").is_err());
        assert!(variant("%_VAR&1").is_ok());
        assert!(RunArgs::parse(&raw(RunKind::Prog, "ZREPORT;DROP"), None).is_err());
        assert!(RunArgs::parse(&raw(RunKind::Prog, &"Z".repeat(41)), None).is_err());
        assert!(RunArgs::parse(&raw(RunKind::Prog, "/NS/ZREPORT"), None).is_ok());
    }

    #[test]
    fn alv_off_and_alv_columns_conflict_and_the_columns_default_to_visible() {
        let conflict = RunArgs::parse(
            &RawArgs {
                alv: Some(AlvMode::Off),
                alv_columns: Some(AlvColumns::All),
                ..raw(RunKind::Prog, "ZREPORT")
            },
            None,
        );
        let off = RunArgs::parse(
            &RawArgs {
                alv: Some(AlvMode::Off),
                ..raw(RunKind::Prog, "ZREPORT")
            },
            None,
        )
        .unwrap();
        let all = RunArgs::parse(
            &RawArgs {
                alv_columns: Some(AlvColumns::All),
                ..raw(RunKind::Prog, "ZREPORT")
            },
            None,
        )
        .unwrap();

        assert_eq!(conflict.unwrap_err(), "--alv-columns needs --alv on");
        assert_eq!((off.alv, off.alv_columns), (Some(AlvMode::Off), None));
        assert_eq!(all.alv_columns, Some(AlvColumns::All));
    }

    #[test]
    fn a_snippet_needs_text_and_takes_no_report_option() {
        let args = RunArgs::parse(&raw(RunKind::Snippet, "a.abap"), Some("WRITE 1.")).unwrap();

        assert_eq!(args.name, SNIPPET_LABEL);
        assert_eq!(
            args.snippet.as_ref().map(|snippet| snippet.sha256.len()),
            Some(64)
        );
        assert!(RunArgs::parse(&raw(RunKind::Snippet, "a.abap"), None).is_err());
        assert!(RunArgs::parse(&raw(RunKind::Snippet, "a.abap"), Some("  \n")).is_err());
        assert!(
            RunArgs::parse(
                &RawArgs {
                    variant: Some("V"),
                    ..raw(RunKind::Snippet, "a.abap")
                },
                Some("WRITE 1.")
            )
            .is_err()
        );
    }

    #[test]
    fn the_name_rule_admits_z_and_y_and_the_explicit_exception() {
        let class = |name: &str| RunArgs::parse(&raw(RunKind::Clas, name), None).unwrap();

        assert!(class("zcl_a").check_scope(false).is_ok());
        assert!(class("ycl_a").check_scope(false).is_ok());

        let refused = class("cl_demo_output").check_scope(false).unwrap_err();

        assert!(refused.contains("outside the Z/Y scope"), "{refused}");
        assert!(class("cl_demo_output").check_scope(true).is_ok());

        let report = RunArgs::parse(&raw(RunKind::Prog, "rsparam"), None).unwrap();

        assert!(report.check_scope(false).unwrap_err().contains("report"));
        assert!(report.check_scope(true).is_ok());

        let snippet = RunArgs::parse(&raw(RunKind::Snippet, "f"), Some("WRITE 1.")).unwrap();

        assert!(snippet.check_scope(false).is_ok());
    }

    #[test]
    fn a_class_run_refuses_the_runner_prefix_of_the_connection_and_the_default() {
        let class = |name: &str| RunArgs::parse(&raw(RunKind::Clas, name), None).unwrap();

        assert!(class("zcl_a").check_not_runner("ZCL_RUNX_").is_ok());
        assert!(class("zcl_run_a").check_not_runner("ZCL_RUNX_").is_ok());
        // Only the generated shape (prefix plus eight characters) is a runner class.
        assert!(
            class("zcl_runx_helper")
                .check_not_runner("ZCL_RUNX_")
                .is_ok()
        );
        assert!(
            class("ycl_abapreach_run_x")
                .check_not_runner("ZCL_RUNX_")
                .is_ok()
        );

        for name in ["zcl_runx_ab12cd34", "ycl_abapreach_run_ab12cd34"] {
            let refused = class(name).check_not_runner("ZCL_RUNX_").unwrap_err();

            assert!(
                refused.contains("a temporary runner class is not run; delete it (doctor)"),
                "{refused}"
            );
        }

        // A report is not a class: its name is not judged here.
        let report = RunArgs::parse(&raw(RunKind::Prog, "zcl_runx_a"), None).unwrap();

        assert!(report.check_not_runner("ZCL_RUNX_").is_ok());
    }

    #[test]
    fn the_object_gate_matches_the_type_and_the_name() {
        let open = Policies {
            mcp_run_allow: vec!["CLAS:ZX".to_owned(), "FUGR:ZG".to_owned()],
            ..policies(RunPolicy::Plan, McpRun::Confirm)
        };

        assert!(mcp_object_gate(&open, "DEV_100", "CLAS", "zx").is_ok());
        assert!(mcp_object_gate(&open, "DEV_100", "FUGR", "ZG").is_ok());

        let refused = mcp_object_gate(&open, "DEV_100", "PROG", "ZX").unwrap_err();

        assert_eq!(refused, "PROG:ZX is not on mcp_run_allow of DEV_100");
        assert!(mcp_object_gate(&open, "DEV_100", "CLAS", "ZG").is_err());
        assert!(
            mcp_object_gate(
                &policies(RunPolicy::Off, McpRun::Confirm),
                "D",
                "CLAS",
                "ZX"
            )
            .unwrap_err()
            .contains("policy run")
        );
    }

    #[test]
    fn the_cli_gate_follows_the_run_policy() {
        let file = Path::new("plan.json");
        let plan = |run| policies(run, McpRun::Off);

        assert!(cli_gate(&plan(RunPolicy::Off), "DEV_100", true, Some(file)).is_err());
        assert!(cli_gate(&plan(RunPolicy::Off), "DEV_100", false, Some(file)).is_err());
        assert!(cli_gate(&plan(RunPolicy::Plan), "DEV_100", true, Some(file)).is_ok());
        assert!(cli_gate(&plan(RunPolicy::Plan), "DEV_100", false, Some(file)).is_ok());
        assert!(
            cli_gate(&plan(RunPolicy::Plan), "DEV_100", false, None)
                .unwrap_err()
                .contains("needs --plan")
        );
        assert!(
            cli_gate(&plan(RunPolicy::Plan), "DEV_100", true, None)
                .unwrap_err()
                .contains("--simulate needs --plan")
        );
        assert!(cli_gate(&plan(RunPolicy::Direct), "DEV_100", false, None).is_ok());
        assert!(cli_gate(&plan(RunPolicy::Direct), "DEV_100", true, None).is_ok());
        assert_eq!(
            cli_gate(&plan(RunPolicy::Off), "DEV_100", false, None).unwrap_err(),
            "run is off for DEV_100 (policy run)"
        );
    }

    #[test]
    fn the_mcp_gate_is_the_matrix_of_the_four_keys() {
        let class = |name: &str| RunArgs::parse(&raw(RunKind::Clas, name), None).unwrap();
        let snippet = RunArgs::parse(&raw(RunKind::Snippet, "f"), Some("WRITE 1.")).unwrap();

        // `run: off` wins over `mcp_run: confirm`.
        let off = policies(RunPolicy::Off, McpRun::Confirm);

        assert!(
            mcp_gate(&off, "DEV_100", &class("zcl_ok"))
                .unwrap_err()
                .contains("policy run")
        );

        // `mcp_run: off` refuses whatever `run` says.
        for run in [RunPolicy::Plan, RunPolicy::Direct] {
            let closed = policies(run, McpRun::Off);

            assert!(
                mcp_gate(&closed, "DEV_100", &class("zcl_ok"))
                    .unwrap_err()
                    .contains("policy mcp_run")
            );
        }

        // `direct` counts as `plan` for MCP: the gate admits both the same way.
        for run in [RunPolicy::Plan, RunPolicy::Direct] {
            let open = policies(run, McpRun::Confirm);

            assert!(mcp_gate(&open, "DEV_100", &class("zcl_ok")).is_ok());
            assert!(mcp_gate(&open, "DEV_100", &class("ZCL_OK")).is_ok());
            assert!(
                mcp_gate(&open, "DEV_100", &class("zcl_other"))
                    .unwrap_err()
                    .contains("not on mcp_run_allow")
            );
            // Names match exactly, not by prefix.
            assert!(mcp_gate(&open, "DEV_100", &class("zcl_ok2")).is_err());
            assert!(
                mcp_gate(&open, "DEV_100", &snippet)
                    .unwrap_err()
                    .contains("policy mcp_run_snippet")
            );
        }

        let with_snippet = Policies {
            mcp_run_snippet: McpRunSnippet::On,
            ..policies(RunPolicy::Plan, McpRun::Confirm)
        };

        assert!(mcp_gate(&with_snippet, "DEV_100", &snippet).is_ok());
        // The snippet opt-in does not open an unlisted name.
        assert!(mcp_gate(&with_snippet, "DEV_100", &class("zcl_other")).is_err());
    }

    #[test]
    fn the_defaults_run_by_plan_and_keep_mcp_closed() {
        let defaults = Policies::default();

        assert_eq!(defaults.run, RunPolicy::Plan);
        assert_eq!(defaults.mcp_run, McpRun::Off);
        assert_eq!(defaults.mcp_run_snippet, McpRunSnippet::Off);
        assert!(defaults.mcp_run_allow.is_empty());
        assert_eq!(
            Policies {
                run: RunPolicy::Direct,
                ..Policies::default()
            }
            .for_mcp()
            .run,
            RunPolicy::Plan
        );
    }

    #[test]
    fn a_plan_binds_the_arguments_and_the_live_facts() {
        let args = RunArgs::parse(&raw(RunKind::Clas, "zcl_a"), None).unwrap();
        let facts = PlanFacts {
            hash: Some("abc".to_owned()),
            package: Some("ZPKG".to_owned()),
            subc: None,
        };
        let plan = args.plan("DEV_100", "100", "adt", facts.clone());

        assert_eq!(
            plan.argument_mismatch(&args.plan("DEV_100", "100", "adt", PlanFacts::default())),
            None
        );
        assert_eq!(plan.fact_mismatch(&facts), None);

        let other = |change: &dyn Fn(&mut RunPlan)| {
            let mut expected = args.plan("DEV_100", "100", "adt", PlanFacts::default());

            change(&mut expected);

            plan.argument_mismatch(&expected)
        };

        assert_eq!(
            other(&|plan| plan.conn = "DEV_200".to_owned()),
            Some("conn")
        );
        assert_eq!(
            other(&|plan| plan.client = "200".to_owned()),
            Some("client")
        );
        assert_eq!(other(&|plan| plan.kind = "PROG".to_owned()), Some("type"));
        assert_eq!(other(&|plan| plan.name = "ZCL_B".to_owned()), Some("name"));
        assert_eq!(
            other(&|plan| plan.params = vec![("A".to_owned(), "1".to_owned())]),
            Some("--param")
        );
        assert_eq!(
            other(&|plan| plan.variant = Some("V".to_owned())),
            Some("--variant")
        );
        assert_eq!(
            other(&|plan| plan.alv = Some("on".to_owned())),
            Some("--alv")
        );
        assert_eq!(
            other(&|plan| plan.alv_columns = Some("all".to_owned())),
            Some("--alv-columns")
        );
        assert_eq!(
            other(&|plan| plan.snippet_sha256 = Some("x".to_owned())),
            Some("snippet")
        );

        for (field, change) in [
            (
                "source hash",
                PlanFacts {
                    hash: Some("changed".to_owned()),
                    ..facts.clone()
                },
            ),
            (
                "package",
                PlanFacts {
                    package: Some("$TMP".to_owned()),
                    ..facts.clone()
                },
            ),
            (
                "SUBC",
                PlanFacts {
                    subc: Some("I".to_owned()),
                    ..facts.clone()
                },
            ),
        ] {
            assert_eq!(plan.fact_mismatch(&change), Some(field));
        }
    }

    #[test]
    fn a_plan_file_round_trips_and_refuses_another_version() {
        let directory =
            std::env::temp_dir().join(format!("abapreach-run-plan-{}", std::process::id()));

        std::fs::create_dir_all(&directory).unwrap();

        let path = directory.join("plan.json");
        let args = RunArgs::parse(&raw(RunKind::Clas, "zcl_a"), None).unwrap();
        let plan = args.plan(
            "DEV_100",
            "100",
            "rfc",
            PlanFacts {
                hash: Some("abc".to_owned()),
                package: Some("ZPKG".to_owned()),
                subc: None,
            },
        );

        plan.save(&path).unwrap();
        assert_eq!(RunPlan::load(&path).unwrap(), plan);

        let text = std::fs::read_to_string(&path).unwrap();

        std::fs::write(&path, text.replace("\"version\": 1", "\"version\": 2")).unwrap();
        assert!(RunPlan::load(&path).unwrap_err().contains("version 2"));

        std::fs::write(&path, "not json").unwrap();
        assert!(
            RunPlan::load(&path)
                .unwrap_err()
                .starts_with("plan file cannot be read")
        );
        assert!(
            RunPlan::load(&directory.join("missing.json"))
                .unwrap_err()
                .starts_with("plan file cannot be read")
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }
}
