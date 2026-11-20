//! The MCP tools `abap_activate_simulate`, `abap_activate` and `abap_aunit`.
//!
//! Activation is a write: it needs `mcp_write`, a plan token from a passed simulation and, under
//! `confirm`, the human. `abap_aunit` runs test code on the server, so it sits behind `mcp_run`
//! instead: it has no token, and the human always confirms, whatever `mcp_write` says. Both call
//! the operations of the command line; this module adapts arguments and results.

use super::{
    Deadline, McpTools, Operation, Seen, Settled, Ticket, WithCancel, WriteGate, ask, bind,
};
use crate::app::end_session;
use crate::backend::OperationKind;
use crate::config::Connection;
use crate::domain::{Outcome, Status};
use crate::mcp::{Session, ToolHandler, ToolResult};
use crate::mcp_read::{optional, required};
use crate::operation::activate::{self, ActivateInput, ActivateTarget};
use crate::operation::aunit::{self, AunitKind, AunitTarget, Scope};
use crate::operation::quality;
use crate::policy::{McpRun, RunPolicy};
use clap::ValueEnum;
use serde_json::{Map, Value, json};

impl<G: WriteGate> McpTools<'_, G> {
    /// The connection of `abap_aunit`: it must allow MCP runs and the command-line policy `run`
    /// must not be off either.
    fn run_connection(&self, arguments: &Map<String, Value>) -> Result<Connection, String> {
        let mut connection = self.read.connection(arguments)?;

        if connection.policies.mcp_run == McpRun::Off {
            return Err(format!(
                "MCP runs are off for {} (policy mcp_run)",
                connection.id
            ));
        }

        if connection.policies.run == RunPolicy::Off {
            return Err(format!("run is off for {} (policy run)", connection.id));
        }

        connection.policies = connection.policies.for_mcp();

        Ok(connection)
    }

    /// `abap_activate_simulate` and `abap_activate`.
    pub(super) fn activate(
        &mut self,
        arguments: &Map<String, Value>,
        execute: bool,
        session: &mut dyn Session,
    ) -> Result<ToolResult, String> {
        let mut deadline = Deadline::start(self.limits.call);
        let connection = self.writer(arguments)?;
        let targets = activate_targets(arguments)?;

        activate::check_targets(&targets)?;

        let mut tokens: Vec<String> = targets.iter().map(ActivateTarget::token).collect();

        tokens.sort();

        let ticket = Ticket::new(
            Operation::Activate,
            connection.id.clone(),
            bind([("objects", Some(tokens.join("\n")))]),
        );
        let stored = if execute {
            Some(self.authorize(&connection, arguments, &ticket, session, &mut deadline)?)
        } else {
            None
        };
        let cancel = session.cancel();

        Self::proceed(&deadline, &cancel, self.limits)?;

        let explicit = stored.as_ref().and_then(|stored| stored.backend);
        let mut seen = Seen::default();
        let _write = execute.then(|| cancel.hold_write());
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let exit = crate::app::route(
            OperationKind::Activate,
            explicit,
            false,
            &connection,
            &mut factories,
            |transport, selection, cancel, _| {
                seen.backend = Some(selection.backend);

                let input = ActivateInput {
                    targets: &targets,
                    client: &connection.client,
                    user: &connection.user,
                    simulate: !execute,
                };
                let result = activate::run(transport, &input, cancel);
                let attempt = quality::attempt(result, end_session(transport));

                seen.status = attempt.outcome.status;

                attempt
            },
        );

        drop(factories);

        let issue = (!execute && seen.status == Some(Status::SimulatedOk)).then_some(Ticket {
            backend: seen.backend,
            ..ticket
        });
        let label = tokens.join(" ");

        Ok(self.finish(
            &connection,
            &exit,
            seen.status,
            issue,
            json!({}),
            // The output is object names and SAP messages: scrub it instead of rejecting it.
            true,
            execute.then(|| Settled::new("abap_activate", &label, &cancel, session)),
        ))
    }

    /// `abap_aunit`: runs test code, so the human confirms every call and the connection must
    /// allow MCP runs.
    pub(super) fn aunit(
        &mut self,
        arguments: &Map<String, Value>,
        session: &mut dyn Session,
    ) -> Result<ToolResult, String> {
        let mut deadline = Deadline::start(self.limits.call);
        let connection = self.run_connection(arguments)?;
        let object = match optional(arguments, "type")? {
            Some(kind) => {
                let kind = AunitKind::from_str(kind, false)
                    .map_err(|_| "type must be one of PROG, CLAS, FUGR".to_owned())?;

                Some((kind, required(arguments, "name")?))
            }
            None => {
                if optional(arguments, "name")?.is_some() {
                    return Err("name needs a type".to_owned());
                }

                None
            }
        };
        let scope = Scope::parse(
            object,
            optional(arguments, "package")?,
            optional(arguments, "tr")?,
        )?;

        // The human must be asked, so a client that cannot answer is refused before any request.
        if !session.can_elicit() {
            return Err(format!(
                "abap_aunit always needs a human confirmation and this client cannot answer one \
                 (it did not advertise elicitation, or its protocol is older than 2025-06-18); \
                 mcp_run is {} for {}",
                connection.policies.mcp_run, connection.id
            ));
        }

        let cancel = session.cancel();
        let (targets, notes) = match &scope {
            // One named object needs no request to be known.
            Scope::Object(target) => (vec![target.clone()], Vec::new()),
            // The objects of a package or a request are read first, so that the human sees them.
            _ => {
                let mut resolved: Option<(Vec<AunitTarget>, Vec<String>)> = None;
                let mut factories = WithCancel {
                    inner: self.read.factories(),
                    cancel: cancel.clone(),
                    opened: None,
                };
                let exit = crate::app::route(
                    OperationKind::Aunit,
                    None,
                    false,
                    &connection,
                    &mut factories,
                    |transport, _, _, _| {
                        let result = aunit::resolve_scope(transport, &scope).map(|found| {
                            resolved = Some(found);

                            Outcome {
                                status: Some(Status::Active),
                                exit: 0,
                                stdout: String::new(),
                                stderr: String::new(),
                            }
                        });

                        quality::attempt(result, end_session(transport))
                    },
                );

                drop(factories);

                match resolved.filter(|_| exit.code == 0) {
                    Some(found) => found,
                    None => {
                        return Ok(self.finish(
                            &connection,
                            &exit,
                            None,
                            None,
                            json!({}),
                            true,
                            None,
                        ));
                    }
                }
            }
        };
        let tokens: Vec<String> = targets.iter().map(ToString::to_string).collect();
        let scope_text = match &scope {
            Scope::Object(_) => None,
            Scope::Package(package) => Some(format!("package {package}")),
            Scope::Request(request) => Some(format!("request {request}")),
        };
        let mut ticket = Ticket::new(
            Operation::Aunit,
            connection.id.clone(),
            bind([("objects", Some(tokens.join("\n"))), ("scope", scope_text)]),
        );

        ticket.listing = tokens.clone();

        run_allowed(&connection, &targets)?;

        let registry = self.registry;
        let elicit = self.limits.elicit;
        let read = &mut self.read;

        ask(
            &connection,
            &ticket,
            registry,
            elicit,
            session,
            &mut |name, args| read.call(name, args),
        )?;
        // The answer is in; what follows is work, and the deadline counts only work.
        deadline.restart();
        Self::proceed(&deadline, &cancel, self.limits)?;

        let mut seen = Seen::default();
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let batch = !matches!(scope, Scope::Object(_));
        let exit = crate::app::route(
            OperationKind::Aunit,
            None,
            false,
            &connection,
            &mut factories,
            |transport, _, _, _| {
                let result =
                    aunit::run_resolved(transport, &targets, &connection.client, batch, &notes);
                let attempt = quality::attempt(result, end_session(transport));

                seen.status = attempt.outcome.status;

                attempt
            },
        );

        drop(factories);

        let label = tokens.join(" ");

        Ok(self.finish(
            &connection,
            &exit,
            seen.status,
            None,
            json!({}),
            true,
            Some(Settled::new("abap_aunit", &label, &cancel, session)),
        ))
    }
}

/// The `objects` argument of the activation tools: a non-empty array of `TYPE/SUBTYPE:NAME`.
fn activate_targets(arguments: &Map<String, Value>) -> Result<Vec<ActivateTarget>, String> {
    let objects = arguments
        .get("objects")
        .and_then(Value::as_array)
        .filter(|objects| !objects.is_empty())
        .ok_or("objects must be a non-empty array of TYPE/SUBTYPE:NAME strings")?;

    objects
        .iter()
        .map(|object| {
            object
                .as_str()
                .ok_or_else(|| "objects must be strings of the form TYPE/SUBTYPE:NAME".to_owned())
                .and_then(str::parse)
        })
        .collect()
}

/// The seam of the `mcp_run_allow` list: every object of a call must be on it. Fails closed until
/// the list is wired: MCP `abap_aunit` is refused.
fn run_allowed(_connection: &Connection, targets: &[AunitTarget]) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }

    Err("STOP: no object is on mcp_run_allow for this connection".to_owned())
}
