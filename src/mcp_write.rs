//! The MCP write tools: simulate, deploy, create and delete over the shared operations. The tools
//! only adapt arguments and results; the checks, guards and routing are the ones the CLI runs.
//!
//! A write needs three things before SAP is touched: the connection allows MCP writes
//! (`mcp_write`), the arguments pass the local rules, and the [`WriteGate`] admits the call. The
//! gate is the seam for the plan token lifecycle; nothing a client sends, annotations included,
//! stands in for it.

use crate::app::{Attempt, Exit, route};
use crate::backend::{BackendKind, Failure, OperationKind};
use crate::cli::{CreateKind, DeleteTarget, IncludeKind, ObjectKind};
use crate::config::Connection;
use crate::domain::{Outcome, Status};
use crate::hash::local_source_lines;
use crate::mcp::{ToolHandler, ToolResult};
use crate::mcp_read::{
    ReadTools, SECRET_REJECTED, diagnostic, holds_secret, optional, required, status_token,
};
use crate::object::{check_create, check_deploy_options, check_deploy_source};
use crate::operation::create::{CreateInput, CreateProg};
use crate::operation::delete::{self, Context, Plan, sha256_hex};
use crate::operation::delete_adt::{AdtDelete, NO_STATEFUL_SESSION};
use crate::operation::deploy::{DeployInput, DeployProg, DeployScope};
use crate::policy::{Confirmer, McpWrite};
use clap::ValueEnum;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// What a plan token is for: a token of one operation never admits another.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Deploy,
    Create,
    Delete,
}

/// What a simulation settled and a later write must repeat. `fields` holds every argument the
/// contract binds in its normalized form, so equal fields mean equal arguments; `backend` is the
/// backend that simulated, `plan` the delete plan.
///
/// The token lifecycle also binds the resolved function group and the live base hash of a
/// deploy; the operation layer does not report either yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ticket {
    pub operation: Operation,
    pub conn: String,
    pub fields: BTreeMap<String, String>,
    pub backend: Option<BackendKind>,
    pub plan: Option<Plan>,
}

/// The seam between the tools and the plan token lifecycle.
pub trait WriteGate {
    /// Called after a simulation that passed: the token the client must present, if any.
    fn issue(&mut self, ticket: Ticket) -> Option<String>;

    /// Called before any SAP activity of a write. `presented` carries the arguments of the call
    /// (no backend, no plan); the answer is the ticket the simulation stored, or the reason the
    /// token does not admit this call.
    fn admit(&mut self, token: &str, presented: &Ticket) -> Result<Ticket, String>;
}

/// The gate until the token lifecycle exists: it issues nothing and admits nothing.
pub struct ClosedGate;

impl WriteGate for ClosedGate {
    fn issue(&mut self, _: Ticket) -> Option<String> {
        None
    }

    fn admit(&mut self, _: &str, _: &Ticket) -> Result<Ticket, String> {
        Err("a write needs a plan_token and this build issues none yet".to_owned())
    }
}

/// MCP has no terminal: a prompt is never shown and never answered.
struct NoTerminal;

impl Confirmer for NoTerminal {
    fn interactive(&self) -> bool {
        false
    }

    fn confirm(&self, _: &str) -> bool {
        false
    }
}

/// The read tools plus the write tools.
pub struct McpTools<'a, G> {
    read: ReadTools<'a>,
    gate: G,
}

impl<'a, G: WriteGate> McpTools<'a, G> {
    pub fn new(read: ReadTools<'a>, gate: G) -> Self {
        Self { read, gate }
    }

    /// The connection of a write tool: it must allow MCP writes, and runs with the MCP policies.
    fn writer(&self, arguments: &Map<String, Value>) -> Result<Connection, String> {
        let mut connection = self.read.connection(arguments)?;

        if connection.policies.mcp_write == McpWrite::Off {
            return Err(format!(
                "MCP writes are off for {} (policy mcp_write)",
                connection.id
            ));
        }

        connection.policies = connection.policies.for_mcp();

        Ok(connection)
    }

    /// Redeems the `plan_token` of an executing call.
    fn admit(&mut self, arguments: &Map<String, Value>, ticket: &Ticket) -> Result<Ticket, String> {
        let token = required(arguments, "plan_token")?;

        self.gate.admit(token, ticket)
    }

    fn deploy(
        &mut self,
        arguments: &Map<String, Value>,
        execute: bool,
    ) -> Result<ToolResult, String> {
        let connection = self.writer(arguments)?;
        let kind = parse::<ObjectKind>(
            required(arguments, "type")?,
            "type must be one of PROG, FUNC, CLAS, INTF",
        )?;
        let name = required(arguments, "name")?;
        let source = required(arguments, "source")?;
        let tr = optional(arguments, "tr")?;
        let include = match optional(arguments, "include")? {
            Some(include) => Some(parse::<IncludeKind>(
                include,
                "include must be one of CCIMP, CCDEF, CCMAC, CCAU",
            )?),
            None => None,
        };
        let fugr = optional(arguments, "fugr")?;
        let create_include = flag(arguments, "create_include")?;

        check_deploy_options(kind, name, include, create_include, fugr)?;

        let lines = local_source_lines(source);

        check_deploy_source(kind, name, &lines)?;

        let mut fields = bind([
            ("type", Some(kind.token().to_owned())),
            ("name", Some(name.to_ascii_uppercase())),
            ("source_sha256", Some(sha256_hex(source))),
            ("tr", tr.map(str::to_ascii_uppercase)),
            ("include", include.map(|include| include.token().to_owned())),
            ("fugr", fugr.map(str::to_ascii_uppercase)),
        ]);

        fields.insert("create_include".to_owned(), create_include.to_string());

        let ticket = Ticket {
            operation: Operation::Deploy,
            conn: connection.id.clone(),
            fields,
            backend: None,
            plan: None,
        };
        let explicit = self.explicit(arguments, &ticket, execute)?;
        let scope = DeployScope::new(kind)
            .group(fugr)
            .include(include)
            .create_include(create_include);
        let input = DeployInput {
            name: name.to_owned(),
            source: lines,
            base: None,
            simulate: !execute,
            tr: tr.map(str::to_owned),
        };
        let mut selected = None;
        let mut status = None;
        let exit = route(
            OperationKind::Deploy(kind),
            explicit,
            false,
            &connection,
            self.read.factories(),
            |transport, selection, cancel, _| {
                selected = Some(selection.backend);

                let result = DeployProg::run_scoped(
                    transport,
                    input.clone(),
                    scope.clone(),
                    Some(selection),
                    Some(&connection),
                    cancel,
                );

                status = result.outcome.status;

                Attempt {
                    outcome: result.outcome,
                    failure: result.failure,
                    fallback_allowed: result.fallback_allowed,
                    detail: result.detail,
                }
            },
        );
        let issue = (!execute && status == Some(Status::SimulatedOk)).then_some(Ticket {
            backend: selected,
            ..ticket
        });

        Ok(self.finish(&connection, &exit, status, issue, json!({})))
    }

    fn create(
        &mut self,
        arguments: &Map<String, Value>,
        execute: bool,
    ) -> Result<ToolResult, String> {
        let connection = self.writer(arguments)?;
        let kind = parse::<CreateKind>(
            required(arguments, "type")?,
            "type must be one of FUGR, FUNC, CLAS, INTF",
        )?;
        let name = required(arguments, "name")?;
        let text = required(arguments, "text")?;
        let package = optional(arguments, "package")?;
        let fugr = optional(arguments, "fugr")?;
        let tr = optional(arguments, "tr")?;

        check_create(kind, name, text, package, fugr, tr)?;

        let ticket = Ticket {
            operation: Operation::Create,
            conn: connection.id.clone(),
            fields: bind([
                ("type", Some(kind.token().to_owned())),
                ("name", Some(name.to_ascii_uppercase())),
                ("text", Some(text.to_owned())),
                ("package", package.map(str::to_ascii_uppercase)),
                ("tr", tr.map(str::to_ascii_uppercase)),
                ("fugr", fugr.map(str::to_ascii_uppercase)),
            ]),
            backend: None,
            plan: None,
        };
        let explicit = self.explicit(arguments, &ticket, execute)?;
        let input = CreateInput {
            kind,
            name: name.to_owned(),
            text: text.to_owned(),
            package: package.map(str::to_owned),
            group: fugr.map(str::to_owned),
            tr: tr.map(str::to_owned),
            simulate: !execute,
        };
        let mut selected = None;
        let mut status = None;
        let exit = route(
            OperationKind::Create(kind),
            explicit,
            false,
            &connection,
            self.read.factories(),
            |transport, selection, cancel, _| {
                selected = Some(selection.backend);

                let result = CreateProg::run_with_cancel(
                    transport,
                    input.clone(),
                    Some(selection),
                    Some(&connection),
                    cancel,
                );

                status = result.outcome.status;

                Attempt {
                    outcome: result.outcome,
                    failure: result.failure,
                    fallback_allowed: result.fallback_allowed,
                    detail: result.detail,
                }
            },
        );
        let issue = (!execute && status == Some(Status::SimulatedOk)).then_some(Ticket {
            backend: selected,
            ..ticket
        });

        Ok(self.finish(&connection, &exit, status, issue, json!({})))
    }

    fn delete(
        &mut self,
        arguments: &Map<String, Value>,
        execute: bool,
    ) -> Result<ToolResult, String> {
        let connection = self.writer(arguments)?;
        let targets = targets(arguments)?;
        let tr = optional(arguments, "tr")?;
        // The local rules of the batch run before a connection is opened.
        let (rows, normalized_tr) = delete::prepare(&targets, tr)?;
        let mut objects: Vec<String> = rows
            .iter()
            .map(|(kind, name)| format!("{}:{name}", kind.token()))
            .collect();

        objects.sort();

        let ticket = Ticket {
            operation: Operation::Delete,
            conn: connection.id.clone(),
            fields: bind([("objects", Some(objects.join("\n"))), ("tr", normalized_tr)]),
            backend: None,
            plan: None,
        };
        let admitted = if execute {
            Some(self.admit(arguments, &ticket)?)
        } else {
            None
        };
        let plan = match &admitted {
            Some(stored) => Some(
                stored
                    .plan
                    .clone()
                    .ok_or("the plan_token carries no delete plan")?,
            ),
            None => None,
        };
        let explicit = admitted.and_then(|stored| stored.backend);
        let mut selected = None;
        let mut report = None;
        let exit = route(
            OperationKind::Delete,
            explicit,
            false,
            &connection,
            self.read.factories(),
            |transport, selection, _, fresh| {
                selected = Some(selection.backend);

                // The server would answer the LOCK and then refuse the DELETE, so nothing is sent
                // and the router may try RFC.
                if transport.stateful_supported() == Some(false) {
                    let _ = transport.close();

                    return Attempt {
                        fallback_allowed: true,
                        ..Attempt::failed(
                            NO_STATEFUL_SESSION.to_owned(),
                            Failure::CapabilityUnavailable,
                        )
                    };
                }

                let context = Context {
                    conn: &connection.id,
                    user: &connection.user,
                    policies: &connection.policies,
                    confirmer: &NoTerminal,
                };
                let result = {
                    let mut backend = AdtDelete::new(&mut *transport, &connection.user, fresh);

                    match &plan {
                        Some(plan) => delete::execute(&mut backend, &context, &targets, tr, plan),
                        None => delete::simulate(&mut backend, &context, &targets, tr),
                    }
                };

                // The batch is over and the lock, if any, is reported by the outcome.
                let _ = transport.close();

                match result {
                    Ok(done) => {
                        let outcome = done.outcome.clone();

                        report = Some(done);

                        Attempt {
                            outcome,
                            failure: None,
                            fallback_allowed: false,
                            detail: None,
                        }
                    }
                    Err(message) => Attempt {
                        outcome: Outcome {
                            status: None,
                            exit: 2,
                            stdout: String::new(),
                            stderr: format!("STOP: {message}"),
                        },
                        failure: None,
                        fallback_allowed: false,
                        detail: None,
                    },
                }
            },
        );
        let results: Vec<Value> = report.as_ref().map_or_else(Vec::new, |report| {
            report
                .results
                .iter()
                .map(|result| {
                    json!({
                        "type": result.kind.token(),
                        "name": result.name,
                        "status": result.status.token(),
                        "reason": result.reason.as_deref().map(|reason| diagnostic(&connection, reason)),
                    })
                })
                .collect()
        });
        let issue = report
            .as_ref()
            .filter(|report| !execute && report.all_simulated_ok())
            .map(|report| Ticket {
                backend: selected,
                plan: report.plan.clone(),
                ..ticket
            });

        Ok(self.finish(&connection, &exit, None, issue, json!({"objects": results})))
    }

    /// The backend a call is pinned to: a simulation is free, an executing call runs where the
    /// simulation ran, and only after the gate admitted its token.
    fn explicit(
        &mut self,
        arguments: &Map<String, Value>,
        ticket: &Ticket,
        execute: bool,
    ) -> Result<Option<BackendKind>, String> {
        if !execute {
            return Ok(None);
        }

        Ok(self.admit(arguments, ticket)?.backend)
    }

    /// The result of an operation: complete text, the same data in structured form, the status
    /// from the operation's own state, and a token when a passed simulation earns one.
    fn finish(
        &mut self,
        connection: &Connection,
        exit: &Exit,
        status: Option<Status>,
        issue: Option<Ticket>,
        mut structured: Value,
    ) -> ToolResult {
        let notes = diagnostic(connection, &exit.stderr);
        let leaked = holds_secret(connection, &exit.stdout);
        let output = if leaked {
            SECRET_REJECTED.to_owned()
        } else {
            exit.stdout.trim_end().to_owned()
        };
        let mut text = [output.as_str(), notes.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let failed = exit.code != 0 || leaked;

        structured["status"] = json!(status.map(status_token));
        structured["exit"] = json!(exit.code);
        structured["output"] = json!(output);
        structured["notes"] = json!(notes);

        if failed {
            structured["message"] = json!(text);
        } else if let Some(token) = issue.and_then(|ticket| self.gate.issue(ticket)) {
            text.push_str(&format!("\nplan_token {token}"));
            structured["plan_token"] = json!(token);
        }

        ToolResult {
            text,
            structured,
            is_error: failed,
        }
    }
}

impl<G: WriteGate> ToolHandler for McpTools<'_, G> {
    fn call(&mut self, name: &str, arguments: &Map<String, Value>) -> ToolResult {
        let result = match name {
            "abap_simulate" => self.deploy(arguments, false),
            "abap_deploy" => self.deploy(arguments, true),
            "abap_create_simulate" => self.create(arguments, false),
            "abap_create" => self.create(arguments, true),
            "abap_delete_simulate" => self.delete(arguments, false),
            "abap_delete" => self.delete(arguments, true),
            _ => return self.read.call(name, arguments),
        };

        result.unwrap_or_else(|message| ToolResult::error(format!("STOP: {message}")))
    }
}

fn parse<T: ValueEnum>(text: &str, message: &str) -> Result<T, String> {
    T::from_str(text, false).map_err(|_| message.to_owned())
}

fn flag(arguments: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

/// The `objects` argument: a non-empty array of `TYPE:NAME` strings.
fn targets(arguments: &Map<String, Value>) -> Result<Vec<DeleteTarget>, String> {
    let objects = arguments
        .get("objects")
        .and_then(Value::as_array)
        .filter(|objects| !objects.is_empty())
        .ok_or("objects must be a non-empty array of TYPE:NAME strings")?;

    objects
        .iter()
        .map(|object| {
            object
                .as_str()
                .ok_or_else(|| "objects must be strings of the form TYPE:NAME".to_owned())
                .and_then(str::parse)
        })
        .collect()
}

/// The bound fields that are present.
fn bind<const N: usize>(fields: [(&str, Option<String>); N]) -> BTreeMap<String, String> {
    fields
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
        .collect()
}
