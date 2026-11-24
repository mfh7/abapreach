//! The `run` command: the argument rules, the policy gate, the plan binding, and the routing of
//! one `run CLAS` to a backend. The rules run before any connection is opened.

use crate::app::{Attempt, Exit, Factories, end_session, route, stop};
use crate::backend::{BackendKind, OperationKind};
use crate::cli::{AlvColumns, AlvMode, Backend, RunKind};
use crate::config::Connection;
use crate::operation::run::{DEFAULT_TIMEOUT_SECS, NO_RFC_TIMEOUT};
use crate::operation::run_plan::{
    PlanFacts, RawArgs, RunArgs, RunPlan, cli_gate, plan_mismatch_text,
};
use crate::operation::run_runner::{DEFAULT_MAX_OUTPUT, RunCall, RunSettings, unix_now};
use crate::operation::runner_route::random_nonce;
use std::fs;
use std::path::Path;

/// The arguments of one `run` as the command line gave them.
pub struct RunCommand<'a> {
    pub kind: RunKind,
    pub target: &'a str,
    pub simulate: bool,
    pub plan: Option<&'a Path>,
    pub params: &'a [String],
    pub variant: Option<&'a str>,
    pub alv: Option<AlvMode>,
    pub alv_columns: Option<AlvColumns>,
    pub allow_standard: bool,
    pub timeout: Option<u32>,
    pub max_output: Option<u64>,
    pub backend: Option<Backend>,
}

/// Runs `command`; every rule that can refuse does so before the first connection.
pub fn execute(
    command: &RunCommand<'_>,
    connection: &Connection,
    factories: &mut dyn Factories,
) -> Exit {
    let snippet = match command.kind {
        RunKind::Snippet => match fs::read_to_string(command.target) {
            Ok(text) => Some(text),
            Err(error) => return stop(format!("cannot read {}: {error}", command.target)),
        },
        RunKind::Clas | RunKind::Prog => None,
    };
    let args = match RunArgs::parse(
        &RawArgs {
            kind: command.kind,
            target: command.target,
            params: command.params,
            variant: command.variant,
            alv: command.alv,
            alv_columns: command.alv_columns,
            max_output: command.max_output,
        },
        snippet.as_deref(),
    ) {
        Ok(args) => args,
        Err(message) => return stop(message),
    };

    if command.allow_standard && args.kind == RunKind::Snippet {
        return stop("--allow-standard applies to CLAS and PROG only");
    }

    if let Err(message) = args.check_scope(command.allow_standard) {
        return stop(message);
    }

    if let Err(message) = args.check_not_runner(&connection.runner_prefix) {
        return stop(message);
    }

    if let Err(message) = cli_gate(
        &connection.policies,
        &connection.id,
        command.simulate,
        command.plan,
    ) {
        return stop(message);
    }

    // The RFC library has no call timeout: the refusal comes before a connection exists.
    if command.timeout.is_some()
        && (command.backend == Some(Backend::Rfc) || connection.adt.is_none())
    {
        return stop(NO_RFC_TIMEOUT);
    }

    let expected = match bound_plan(command, &args, connection) {
        Ok(expected) => expected,
        Err(message) => return stop(message),
    };
    let pinned = match pinned_backend(command.backend, expected.as_ref()) {
        Ok(pinned) => pinned,
        Err(message) => return stop(message),
    };

    // The rules of a snippet run before any connection, like every other argument rule.
    if let Some(snippet) = &args.snippet
        && let Err(message) = crate::operation::run_snippet::check(&snippet.text)
    {
        return stop(message);
    }

    let timeout = command.timeout;
    let nonce = random_nonce();
    let now_unix = unix_now();

    route(
        OperationKind::Run,
        pinned,
        false,
        connection,
        factories,
        |transport, selection, cancel, fresh| {
            if timeout.is_some() && selection.backend == BackendKind::RfcSadt {
                return Attempt {
                    outcome: crate::domain::Outcome {
                        status: None,
                        exit: 2,
                        stdout: String::new(),
                        stderr: format!("STOP: {NO_RFC_TIMEOUT}"),
                    },
                    failure: None,
                    fallback_allowed: false,
                    detail: None,
                };
            }

            let call = RunCall::new(&RunSettings {
                connection,
                args: &args,
                backend: selection.backend,
                timeout: timeout.unwrap_or(DEFAULT_TIMEOUT_SECS),
                cancel,
                max_output: command.max_output.unwrap_or(DEFAULT_MAX_OUTPUT),
                nonce: &nonce,
                now_unix,
            });
            let mut result = if command.simulate {
                call.simulate(transport)
            } else {
                call.execute(transport, expected.as_ref(), fresh)
            };

            if let (Some(plan), Some(path)) = (&result.plan, command.plan) {
                match plan.save(path) {
                    Ok(()) => result
                        .outcome
                        .stdout
                        .push_str(&format!("\nplan -> {}", path.display())),
                    Err(message) => {
                        // No plan was written, so the simulate did not pass: no SIMULATED_OK line
                        // and no passed status next to exit 1.
                        result.outcome.exit = 1;
                        result.outcome.status = None;
                        result.outcome.stdout.clear();
                        result.outcome.stderr = format!("ERROR: {message}");
                    }
                }
            }

            // A run that was sent has ended its read session already; this only reports a failure.
            let note = end_session(transport);

            Attempt {
                outcome: result.outcome,
                failure: result.failure,
                fallback_allowed: result.fallback_allowed,
                detail: result.detail,
            }
            .noted(note)
        },
    )
}

/// The plan of a real run, checked against the arguments: `None` for a simulate and for a direct
/// run without a file. `Err` is the text of a `STOP`.
fn bound_plan(
    command: &RunCommand<'_>,
    args: &RunArgs,
    connection: &Connection,
) -> Result<Option<RunPlan>, String> {
    let Some(path) = command.plan.filter(|_| !command.simulate) else {
        return Ok(None);
    };
    let plan = RunPlan::load(path)?;
    let wanted = args.plan(
        &connection.id,
        &connection.client,
        &plan.backend,
        PlanFacts::default(),
    );

    match plan.argument_mismatch(&wanted) {
        Some(field) => Err(plan_mismatch_text(field)),
        None => Ok(Some(plan)),
    }
}

/// The backend a run is pinned to: the one its plan names, which `--backend` may not contradict.
fn pinned_backend(
    flag: Option<Backend>,
    plan: Option<&RunPlan>,
) -> Result<Option<BackendKind>, String> {
    let flagged = flag.map(|backend| match backend {
        Backend::Adt => BackendKind::AdtHttp,
        Backend::Rfc => BackendKind::RfcSadt,
    });
    let Some(plan) = plan else {
        return Ok(flagged);
    };
    let planned = match plan.backend.as_str() {
        "adt" => BackendKind::AdtHttp,
        "rfc" => BackendKind::RfcSadt,
        other => {
            return Err(format!(
                "plan file names the backend {other:?}; simulate again"
            ));
        }
    };

    match flagged {
        Some(flagged) if flagged != planned => Err(plan_mismatch_text("backend")),
        _ => Ok(Some(planned)),
    }
}
