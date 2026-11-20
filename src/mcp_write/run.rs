//! The MCP tool `abap_run`: runs a class (report and snippet follow) with the authorizations of
//! the logon user.
//!
//! Running code is not a write the connection's `mcp_write` decides: the tool has its own opt-in
//! (`mcp_run`), and it always asks the human, whatever `mcp_write` says. There is no plan token:
//! the call reads the facts first (the simulate of the command line), shows them with everything
//! that runs in a form, and runs only what the human confirmed, against the same facts. The full
//! content must fit the form; a summary that would be cut is refused. The call takes the single
//! in-flight slot and returns when the run returns or its timeout ends it.

use super::{McpTools, Settled, WithCancel, WriteGate, ask_form, flag, sanitize};
use crate::app::{Attempt, end_session, route};
use crate::backend::{BackendKind, OperationKind};
use crate::cli::{AlvMode, RunKind};
use crate::config::Connection;
use crate::mcp::{Session, ToolHandler, ToolResult};
use crate::mcp_read::{optional, required};
use crate::operation::run::{
    ClassFacts, DEFAULT_TIMEOUT_SECS, NO_RFC_TIMEOUT, NOT_A_DRY_RUN, RunResult, Verdict,
};
use crate::operation::run_plan::{RawArgs, RunArgs, RunPlan, mcp_gate};
use crate::operation::run_runner::{RunCall, RunSettings, unix_now};
use crate::operation::runner_route::random_nonce;
use serde_json::{Map, Value, json};

/// The most characters of the confirmation of a run: the same bound as every other confirmation,
/// but a run is refused when its summary would be cut.
pub const MAX_RUN_SUMMARY: usize = super::MAX_SUMMARY;

/// The most output bytes one MCP run returns.
pub const MCP_MAX_OUTPUT: usize = 1 << 20;

/// What the human types for a snippet: it has no name.
const SNIPPET_CONFIRM: &str = "RUN";

/// The longest line of a field of the summary; code and parameters are shown whole.
const FIELD_LINE: usize = 120;

impl<G: WriteGate> McpTools<'_, G> {
    pub(super) fn run(
        &mut self,
        arguments: &Map<String, Value>,
        session: &mut dyn Session,
    ) -> Result<ToolResult, String> {
        let mut connection = self.read.connection(arguments)?;

        connection.policies = connection.policies.for_mcp();

        let args = parse_arguments(arguments)?;

        mcp_gate(&connection.policies, &connection.id, &args)?;
        args.check_not_runner(&connection.runner_prefix)?;

        let simulate = flag(arguments, "simulate")?;
        let timeout = timeout_of(arguments)?;

        // The rules of a snippet run before anything is sent, like every other argument rule.
        if let Some(snippet) = &args.snippet {
            crate::operation::run_snippet::check(&snippet.text)?;
        }

        let nonce = random_nonce();
        let now_unix = unix_now();

        // Nobody to ask is a refusal before anything is sent; a simulate asks nobody.
        if !simulate && !session.can_elicit() {
            return Err(format!(
                "abap_run always needs a human confirmation and this client cannot answer one (it \
                 did not advertise elicitation, or its protocol is older than 2025-06-18); {} \
                 allows MCP runs only with it",
                connection.id
            ));
        }

        let cancel = session.cancel();
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let mut found = Found::default();
        let simulated = route(
            OperationKind::Run,
            None,
            false,
            &connection,
            &mut factories,
            |transport, selection, cancel, _| {
                if timeout.is_some() && selection.backend == BackendKind::RfcSadt {
                    return stop_attempt(NO_RFC_TIMEOUT);
                }

                let call = RunCall::new(&RunSettings {
                    connection: &connection,
                    args: &args,
                    backend: selection.backend,
                    timeout: timeout.unwrap_or(DEFAULT_TIMEOUT_SECS),
                    cancel,
                    max_output: MCP_MAX_OUTPUT as u64,
                    nonce: &nonce,
                    now_unix,
                });
                let result = call.simulate(transport);
                let note = end_session(transport);

                found.take(&result);

                attempt_of(result).noted(note)
            },
        );

        drop(factories);

        if simulate || found.plan.is_none() {
            let mut result = self.finish(
                &connection,
                &simulated,
                None,
                None,
                run_data(&args, None),
                false,
                None,
            );

            keep_runner_lines_in_notes(&mut result);

            return Ok(result);
        }

        let (Some(plan), Some(facts)) = (found.plan, found.facts) else {
            return Err("the simulate returned no plan".to_owned());
        };
        let backend = backend_of(&plan)?;
        let effective = timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let summary = run_summary(&connection, &args, &facts, backend, effective)?;
        let expected = confirmation_word(&args);
        let elicit = self.limits.elicit;
        let read = &mut self.read;

        ask_form(
            &connection,
            &summary,
            &expected,
            elicit,
            session,
            &mut |name, args| read.call(name, args),
        )
        .map_err(|message| message.replacen("write not confirmed", "run not confirmed", 1))?;

        if cancel.is_set() {
            return Err("CANCELED: the request was cancelled before anything was sent".to_owned());
        }

        // The run repeats what the human confirmed: the backend and the facts of the simulate.
        let _write = cancel.hold_write();
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let mut verdict = None;
        let exit = route(
            OperationKind::Run,
            Some(backend),
            false,
            &connection,
            &mut factories,
            |transport, selection, cancel, fresh| {
                let call = RunCall::new(&RunSettings {
                    connection: &connection,
                    args: &args,
                    backend: selection.backend,
                    timeout: effective,
                    cancel,
                    max_output: MCP_MAX_OUTPUT as u64,
                    nonce: &nonce,
                    now_unix,
                });
                let result = call.execute(transport, Some(&plan), fresh);
                let note = end_session(transport);

                verdict = result.verdict;

                attempt_of(result).noted(note)
            },
        );

        drop(factories);

        let mut exit = exit;
        let cut = cut_output(&mut exit.stdout);

        if let Some(note) = cut {
            exit.stderr = if exit.stderr.is_empty() {
                note
            } else {
                format!("{}\n{note}", exit.stderr)
            };
        }

        let mut result = self.finish(
            &connection,
            &exit,
            None,
            None,
            run_data(&args, verdict),
            false,
            Some(Settled::new("abap_run", &args.name, &cancel, session)),
        );

        keep_runner_lines_in_notes(&mut result);

        // The run has happened even when its output is withheld.
        if result.is_error && result.text.starts_with("REJECTED: payload contains") {
            result.text = format!(
                "{}; the run itself has happened, see its effects in SAP",
                result.text
            );
            result.structured["message"] = json!(result.text);
        }

        Ok(result)
    }
}

/// The `RUNNER:` lines (the name of the temporary class, the repository preflight) are for the
/// log, not for the reader of the output: they stay in `structured.notes` and leave the text.
/// Every other note (`RUNNER_LEFTOVER` with its cleanup commands, `W:` lines such as a cut output)
/// stays in the text.
fn keep_runner_lines_in_notes(result: &mut ToolResult) {
    let notes = result.structured["notes"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    if !notes.lines().any(is_runner_line) {
        return;
    }

    let kept = notes
        .lines()
        .filter(|line| !is_runner_line(line))
        .collect::<Vec<_>>()
        .join("\n");
    let output = result.structured["output"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    result.text = [output.as_str(), kept.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if result.structured.get("message").is_some() {
        result.structured["message"] = json!(result.text);
    }
}

fn is_runner_line(line: &str) -> bool {
    line.starts_with("RUNNER: ")
}

/// What the simulate left behind, taken out of the routing closure.
#[derive(Default)]
struct Found {
    plan: Option<RunPlan>,
    facts: Option<ClassFacts>,
}

impl Found {
    fn take(&mut self, result: &RunResult) {
        self.plan = result.plan.clone();
        self.facts = result.facts.clone();
    }
}

fn attempt_of(result: RunResult) -> Attempt {
    Attempt {
        outcome: result.outcome,
        failure: result.failure,
        fallback_allowed: result.fallback_allowed,
        detail: result.detail,
    }
}

fn stop_attempt(message: &str) -> Attempt {
    Attempt {
        outcome: crate::domain::Outcome {
            status: None,
            exit: 2,
            stdout: String::new(),
            stderr: format!("STOP: {message}"),
        },
        failure: None,
        fallback_allowed: false,
        detail: None,
    }
}

/// The arguments of the call after the local rules. MCP has no `--allow-standard`: the exact
/// names of `mcp_run_allow` decide, so the Z/Y rule is not applied here.
fn parse_arguments(arguments: &Map<String, Value>) -> Result<RunArgs, String> {
    let kind = match required(arguments, "type")? {
        "CLAS" => RunKind::Clas,
        "PROG" => RunKind::Prog,
        "SNIPPET" => RunKind::Snippet,
        _ => return Err("type must be one of CLAS, PROG, SNIPPET".to_owned()),
    };
    let params = match arguments.get("params") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "params must be strings of the form NAME=VALUE".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("params must be an array of NAME=VALUE strings".to_owned()),
    };
    let alv = match optional(arguments, "alv")? {
        None => None,
        Some("on") => Some(AlvMode::On),
        Some("off") => Some(AlvMode::Off),
        Some(_) => return Err("alv must be on or off".to_owned()),
    };
    let target = match kind {
        RunKind::Snippet => "snippet",
        RunKind::Clas | RunKind::Prog => required(arguments, "name")?,
    };

    if kind == RunKind::Snippet && optional(arguments, "name")?.is_some() {
        return Err("name does not apply to SNIPPET".to_owned());
    }

    if kind != RunKind::Snippet && optional(arguments, "source")?.is_some() {
        return Err("source applies to SNIPPET only".to_owned());
    }

    RunArgs::parse(
        &RawArgs {
            kind,
            target,
            params: &params,
            variant: optional(arguments, "variant")?,
            alv,
            alv_columns: None,
            max_output: None,
        },
        optional(arguments, "source")?,
    )
    .map_err(|message| {
        message
            .replace("--param", "params")
            .replace("--variant", "variant")
            .replace("--alv", "alv")
    })
}

/// The `timeout` argument: whole seconds, 1 to 3600.
fn timeout_of(arguments: &Map<String, Value>) -> Result<Option<u32>, String> {
    match arguments.get("timeout") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|seconds| (1..=3600).contains(seconds))
            .and_then(|seconds| u32::try_from(seconds).ok())
            .map(Some)
            .ok_or_else(|| "timeout must be a whole number of seconds from 1 to 3600".to_owned()),
    }
}

fn backend_of(plan: &RunPlan) -> Result<BackendKind, String> {
    match plan.backend.as_str() {
        "adt" => Ok(BackendKind::AdtHttp),
        "rfc" => Ok(BackendKind::RfcSadt),
        other => Err(format!("the simulate named an unknown backend {other:?}")),
    }
}

/// What the human types: the name of the class or report, or a fixed word for a snippet.
fn confirmation_word(args: &RunArgs) -> String {
    match args.kind {
        RunKind::Snippet => SNIPPET_CONFIRM.to_owned(),
        RunKind::Clas | RunKind::Prog => args.name.clone(),
    }
}

/// What the human confirms: where, what, the facts the run is bound to, every parameter, and the
/// whole snippet. The text is never cut: a summary over the bound is refused, so nothing runs that
/// the human did not see in full.
pub(super) fn run_summary(
    connection: &Connection,
    args: &RunArgs,
    facts: &ClassFacts,
    backend: BackendKind,
    timeout: u32,
) -> Result<String, String> {
    let mut lines = vec![format!(
        "RUN {} on {} (SID {}, client {}), backend {}",
        args.kind.token(),
        sanitize(&connection.id, FIELD_LINE),
        sanitize(&connection.system_id, FIELD_LINE),
        sanitize(&connection.client, FIELD_LINE),
        backend.token(),
    )];

    lines.push(NOT_A_DRY_RUN.trim_start_matches("W: ").to_owned());

    if args.kind != RunKind::Snippet {
        lines.push(format!("name: {}", sanitize(&args.name, FIELD_LINE)));
        lines.push(format!("package: {}", sanitize(&facts.package, FIELD_LINE)));

        if !facts.author.is_empty() {
            lines.push(format!("author: {}", sanitize(&facts.author, FIELD_LINE)));
        }

        lines.push(format!("active source hash: {}", facts.hash));
    }

    // A value the form cannot show as it is is not confirmed: an invisible character would
    // reach the report unseen. A variant is ASCII already (`RunArgs::parse`).
    for (name, value) in &args.params {
        if sanitize(value, usize::MAX) != *value {
            return Err(format!(
                "parameter {name}: the value has control or invisible characters, which the \
                 confirmation form cannot show; remove them"
            ));
        }

        lines.push(format!(
            "parameter {name} = {}",
            sanitize(value, FIELD_LINE)
        ));
    }

    if let Some(variant) = &args.variant {
        lines.push(format!("variant: {}", sanitize(variant, FIELD_LINE)));
    }

    if let Some(alv) = args.alv {
        lines.push(format!("alv capture: {}", alv.token()));
    }

    if let Some(snippet) = &args.snippet {
        lines.push("snippet:".to_owned());

        for line in snippet.text.lines() {
            let shown = sanitize(line, usize::MAX);

            // What the form cannot show is not confirmed: an invisible character could hide
            // code from the human.
            if shown != line {
                return Err(
                    "the snippet has control or invisible characters, which the confirmation \
                     form cannot show; remove them"
                        .to_owned(),
                );
            }

            lines.push(shown);
        }
    }

    if backend == BackendKind::AdtHttp {
        lines.push(format!("timeout: {timeout} s"));
    }

    let summary = lines.join("\n");
    let length = summary.chars().count();

    if length > MAX_RUN_SUMMARY {
        return Err(format!(
            "the confirmation of this run has {length} characters; the form shows at most \
             {MAX_RUN_SUMMARY} and a run is never confirmed from a cut summary"
        ));
    }

    Ok(summary)
}

/// Cuts `stdout` at [`MCP_MAX_OUTPUT`] bytes on a character boundary; the note says so.
fn cut_output(stdout: &mut String) -> Option<String> {
    if stdout.len() <= MCP_MAX_OUTPUT {
        return None;
    }

    let mut end = MCP_MAX_OUTPUT;

    while !stdout.is_char_boundary(end) {
        end -= 1;
    }

    let omitted = stdout.len() - end;

    stdout.truncate(end);

    Some(format!(
        "W: output cut at {MCP_MAX_OUTPUT} bytes; {omitted} bytes left out"
    ))
}

/// The structured data of a result: what ran, and how the `POST` ended.
fn run_data(args: &RunArgs, verdict: Option<Verdict>) -> Value {
    json!({
        "type": args.kind.token(),
        "name": args.name,
        "outcome": verdict.map(Verdict::token),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::run_plan::{RawArgs, RunArgs};

    fn result_with(output: &str, notes: &str, failed: bool) -> ToolResult {
        let text = [output, notes]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let mut structured = json!({"output": output, "notes": notes});

        if failed {
            structured["message"] = json!(text);
        }

        ToolResult {
            text,
            structured,
            is_error: failed,
        }
    }

    #[test]
    fn runner_lines_leave_the_text_and_stay_in_the_notes() {
        let notes = "RUNNER: YCL_ABAPREACH_RUN_AB12CD34 (temporary, $TMP, post via adt, removed after the run)\nRUNNER: preflight T000 CCCATEGORY=T\nW: output cut at 1048576 bytes; 2 bytes left out";
        let mut result = result_with("hello", notes, false);

        keep_runner_lines_in_notes(&mut result);

        assert_eq!(
            result.text,
            "hello\nW: output cut at 1048576 bytes; 2 bytes left out"
        );
        assert_eq!(result.structured["notes"], notes);
        assert_eq!(result.structured["output"], "hello");
    }

    #[test]
    fn a_leftover_and_its_cleanup_commands_stay_in_the_text() {
        let notes = "RUNNER: YCL_ABAPREACH_RUN_AB12CD34 (temporary, $TMP, post via adt, removed after the run)\nRUNNER_LEFTOVER: YCL_ABAPREACH_RUN_AB12CD34\nabapreach delete CLAS:YCL_ABAPREACH_RUN_AB12CD34 --plan <FILE> --conn DEV_100";
        let mut result = result_with("", notes, true);

        keep_runner_lines_in_notes(&mut result);

        assert!(!result.text.contains("RUNNER: "), "{}", result.text);
        assert!(
            result.text.starts_with("RUNNER_LEFTOVER: "),
            "{}",
            result.text
        );
        assert!(result.text.contains("abapreach delete CLAS:"));
        assert_eq!(result.structured["message"], json!(result.text));
        assert_eq!(result.structured["notes"], notes);
    }

    #[test]
    fn a_result_without_runner_lines_is_untouched() {
        let mut result = result_with("hello", "W: something", false);
        let before = result.text.clone();

        keep_runner_lines_in_notes(&mut result);

        assert_eq!(result.text, before);
    }

    fn facts() -> ClassFacts {
        ClassFacts {
            package: "ZPKG".to_owned(),
            author: "DEVUSER".to_owned(),
            hash: "abc".to_owned(),
            subc: None,
        }
    }

    #[test]
    fn a_parameter_the_form_cannot_show_is_refused() {
        let connection = Connection::test_insecure();
        let prog = |params: &[String], variant: Option<&str>| {
            RunArgs::parse(
                &RawArgs {
                    kind: RunKind::Prog,
                    target: "ZREPORT",
                    params,
                    variant,
                    alv: None,
                    alv_columns: None,
                    max_output: None,
                },
                None,
            )
            .unwrap()
        };
        let hidden = ["P_A=A\u{200b}B".to_owned()];
        let error = run_summary(
            &connection,
            &prog(&hidden, None),
            &facts(),
            BackendKind::AdtHttp,
            600,
        )
        .unwrap_err();

        assert!(error.starts_with("parameter P_A:"), "{error}");
        assert!(error.contains("control or invisible characters"), "{error}");

        assert!(
            run_summary(
                &connection,
                &prog(&["P_A=AB".to_owned()], Some("V1")),
                &facts(),
                BackendKind::AdtHttp,
                600,
            )
            .is_ok()
        );
    }

    fn snippet(text: &str) -> RunArgs {
        RunArgs::parse(
            &RawArgs {
                kind: RunKind::Snippet,
                target: "snippet",
                params: &[],
                variant: None,
                alv: None,
                alv_columns: None,
                max_output: None,
            },
            Some(text),
        )
        .unwrap()
    }

    #[test]
    fn the_whole_snippet_and_every_parameter_are_in_the_summary() {
        let connection = Connection::test_insecure();
        let args = snippet("WRITE 'one'.\nWRITE 'two'.");
        let summary = run_summary(&connection, &args, &facts(), BackendKind::AdtHttp, 600).unwrap();

        assert!(
            summary.contains("snippet:\nWRITE 'one'.\nWRITE 'two'."),
            "{summary}"
        );
        assert!(summary.contains("timeout: 600 s"));

        let report = RunArgs::parse(
            &RawArgs {
                kind: RunKind::Prog,
                target: "zreport",
                params: &["P_A=1".to_owned(), "P_B=x y".to_owned()],
                variant: Some("v1"),
                alv: None,
                alv_columns: None,
                max_output: None,
            },
            None,
        )
        .unwrap();
        let summary =
            run_summary(&connection, &report, &facts(), BackendKind::RfcSadt, 600).unwrap();

        assert!(summary.contains("parameter P_A = 1\nparameter P_B = x y\nvariant: V1"));
        // RFC has no limit to show.
        assert!(!summary.contains("timeout:"));
    }

    #[test]
    fn a_summary_that_would_be_cut_is_refused_and_one_at_the_bound_is_not() {
        let connection = Connection::test_insecure();
        let head = run_summary(
            &connection,
            &snippet("x"),
            &facts(),
            BackendKind::AdtHttp,
            600,
        )
        .unwrap()
        .chars()
        .count();
        // Room for the code: the lines of the summary except the one character of code.
        let room = MAX_RUN_SUMMARY - (head - 1);
        let fits = run_summary(
            &connection,
            &snippet(&"y".repeat(room)),
            &facts(),
            BackendKind::AdtHttp,
            600,
        )
        .unwrap();

        assert_eq!(fits.chars().count(), MAX_RUN_SUMMARY);

        let error = run_summary(
            &connection,
            &snippet(&"y".repeat(room + 1)),
            &facts(),
            BackendKind::AdtHttp,
            600,
        )
        .unwrap_err();

        assert!(
            error.contains("never confirmed from a cut summary"),
            "{error}"
        );
    }

    #[test]
    fn a_snippet_with_characters_the_form_cannot_show_is_refused() {
        let connection = Connection::test_insecure();

        for text in [
            "WRITE 1.\u{202e}DELETE.",
            "WRITE 1.\u{200b}",
            "WRITE\t1.",
            "A\u{7}B",
        ] {
            let error = run_summary(
                &connection,
                &snippet(text),
                &facts(),
                BackendKind::AdtHttp,
                600,
            )
            .unwrap_err();

            assert!(
                error.contains("control or invisible characters"),
                "{text:?}: {error}"
            );
        }

        // A CRLF file is shown line by line.
        assert!(
            run_summary(
                &connection,
                &snippet("WRITE 1.\r\nWRITE 2.\r\n"),
                &facts(),
                BackendKind::AdtHttp,
                600
            )
            .is_ok()
        );
    }

    #[test]
    fn the_output_cut_keeps_characters_whole() {
        let mut short = "abc".to_owned();

        assert_eq!(cut_output(&mut short), None);

        // A two-byte character straddles the bound: the cut moves back to its start.
        let mut text = format!("{}ä", "a".repeat(MCP_MAX_OUTPUT - 1));
        let note = cut_output(&mut text).unwrap();

        assert_eq!(text.len(), MCP_MAX_OUTPUT - 1);
        assert_eq!(note, "W: output cut at 1048576 bytes; 2 bytes left out");
    }
}
