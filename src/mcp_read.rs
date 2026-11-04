//! The MCP read tools: `abap_read_source` and `abap_read_table` over the shared operations.
//! Every call opens its own connection, so a read never shares the session of a running write.
//! The result carries the same data in `content` text and in `structuredContent`; the status
//! comes from the operation's own state, never from formatted text.

use crate::app::{Attempt, Factories, route};
use crate::backend::OperationKind;
use crate::cli::{IncludeKind, LineRange, ObjectKind};
use crate::config::{Config, Connection, scrub};
use crate::domain::{Outcome, Status};
use crate::hash::source_hash;
use crate::mcp::{ToolHandler, ToolResult};
use crate::object::check_read;
use crate::operation::read::{ReadProg, ReadTarget, project_lines};
use crate::operation::read_table::{
    MAX_OUTPUT_BYTES, MCP_MAX_ROWS, Query, ReadTable, ReadTableError, ReadTableRequest, mcp_rows,
};
use clap::ValueEnum;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) const SECRET_REJECTED: &str = "REJECTED: payload contains a configured secret";

pub struct ReadTools<'a> {
    config: Config,
    env: BTreeMap<String, String>,
    factories: &'a mut dyn Factories,
}

impl<'a> ReadTools<'a> {
    pub fn new(
        config: Config,
        env: BTreeMap<String, String>,
        factories: &'a mut dyn Factories,
    ) -> Self {
        Self {
            config,
            env,
            factories,
        }
    }

    pub(crate) fn connection(&self, arguments: &Map<String, Value>) -> Result<Connection, String> {
        let id = required(arguments, "conn")?;

        self.config.resolve(id, &self.env)
    }

    pub(crate) fn factories(&mut self) -> &mut dyn Factories {
        &mut *self.factories
    }

    fn read_source(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let kind = required(arguments, "type")?;
        let kind = ObjectKind::from_str(kind, false)
            .map_err(|_| "type must be one of PROG, FUNC, CLAS, INTF".to_owned())?;
        let name = required(arguments, "name")?;
        let include = match optional(arguments, "include")? {
            Some(include) => Some(
                IncludeKind::from_str(include, false)
                    .map_err(|_| "include must be one of CCIMP, CCDEF, CCMAC, CCAU".to_owned())?,
            ),
            None => None,
        };
        let fugr = optional(arguments, "fugr")?;
        let range = match optional(arguments, "lines")? {
            Some(lines) => Some(lines.parse::<LineRange>()?),
            None => None,
        };

        check_read(kind, name, Path::new("-"), include, fugr, range)?;

        let connection = self.connection(arguments)?;
        let target = ReadTarget::new(kind, name)
            .group(fugr)
            .include(include)
            .lines(range);
        let mut found = None;
        let exit = route(
            OperationKind::Read(kind),
            None,
            false,
            &connection,
            &mut *self.factories,
            |transport, _, _, _| {
                let read = ReadProg::fetch_lines(transport, target);

                // A read holds no lock, so a failed session end changes nothing here.
                let _ = transport.close();

                match read {
                    Ok(lines) => {
                        found = Some(lines);

                        done()
                    }
                    Err(error) if error.stop => Attempt {
                        outcome: Outcome {
                            status: None,
                            exit: 2,
                            stdout: String::new(),
                            stderr: format!("STOP: {}", error.message),
                        },
                        failure: None,
                        fallback_allowed: false,
                        detail: None,
                    },
                    Err(error) => Attempt::failed(error.message, error.failure),
                }
            },
        );
        let notes = diagnostic(&connection, &exit.stderr);

        let Some(lines) = found.filter(|_| exit.code == 0) else {
            return Ok(ToolResult::error(notes));
        };
        let shown = project_lines(&lines, range);
        let payload = shown.join("\n");

        if holds_secret(&connection, &payload) {
            return Ok(ToolResult::error(SECRET_REJECTED.to_owned()));
        }

        let block = if payload.is_empty() {
            String::new()
        } else {
            format!("{payload}\n")
        };
        let (mut text, mut structured) = match range {
            Some(range) => (
                format!(
                    "{block}{} lines (lines {}-{}, partial excerpt)",
                    shown.len(),
                    range.start,
                    range.end
                ),
                json!({
                    "status": status_token(Status::Active),
                    "partial": true,
                    "excerpt": payload,
                    "lines": shown.len(),
                    "range": format!("{}-{}", range.start, range.end),
                }),
            ),
            None => {
                let hash = source_hash(&lines);

                (
                    format!("{block}{} lines\nhash {hash}", shown.len()),
                    json!({
                        "status": status_token(Status::Active),
                        "partial": false,
                        "source": payload,
                        "lines": shown.len(),
                        "hash": hash,
                    }),
                )
            }
        };

        add_notes(&mut text, &mut structured, &notes);

        Ok(ToolResult {
            text,
            structured,
            is_error: false,
        })
    }

    fn read_table(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let table = required(arguments, "table")?;
        let fields = match optional(arguments, "fields")? {
            Some(fields) if !fields.trim().is_empty() => {
                fields.split(',').map(str::to_owned).collect()
            }
            _ => Vec::new(),
        };
        let where_clause = optional(arguments, "where")?.map(str::to_owned);
        let rows = match arguments.get("rows") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|rows| u32::try_from(rows).ok())
                    .ok_or_else(|| format!("rows must be between 1 and {MCP_MAX_ROWS}"))?,
            ),
        };
        let rows = mcp_rows(rows)?;
        let connection = self.connection(arguments)?;
        let request = ReadTableRequest {
            table: table.to_owned(),
            fields,
            where_clause,
            rows,
        };
        let query = Query::new(&request, MCP_MAX_ROWS, &connection.policies)?;
        let mut found = None;
        let exit = route(
            OperationKind::ReadTable,
            None,
            false,
            &connection,
            &mut *self.factories,
            |transport, _, _, _| {
                let read = ReadTable::run(transport, &query, &connection.policies);

                // A read holds no lock, so a failed session end changes nothing here.
                let _ = transport.close();

                match read {
                    Ok(data) => {
                        found = Some(data);

                        done()
                    }
                    Err(ReadTableError::Rejected(message)) => Attempt {
                        outcome: Outcome {
                            status: None,
                            exit: 1,
                            stdout: String::new(),
                            stderr: format!("REJECTED: {message}"),
                        },
                        failure: None,
                        fallback_allowed: false,
                        detail: None,
                    },
                    Err(ReadTableError::Failed { failure, message }) => {
                        Attempt::failed(message, failure)
                    }
                }
            },
        );
        let notes = diagnostic(&connection, &exit.stderr);

        let Some(data) = found.filter(|_| exit.code == 0) else {
            return Ok(ToolResult::error(notes));
        };
        let (mut text, omitted) = data.render(MAX_OUTPUT_BYTES);
        let shown = data.rows.len() - omitted;

        if holds_secret(&connection, &text) {
            return Ok(ToolResult::error(SECRET_REJECTED.to_owned()));
        }

        if omitted > 0 {
            text.push_str(&format!(
                "\nW: output cut at {MAX_OUTPUT_BYTES} bytes; {omitted} rows left out"
            ));
        }

        let mut structured = json!({
            "columns": data.columns,
            "rows": &data.rows[..shown],
            "row_count": shown,
            "truncated": omitted > 0,
            "omitted": omitted,
        });

        add_notes(&mut text, &mut structured, &notes);

        Ok(ToolResult {
            text,
            structured,
            is_error: false,
        })
    }
}

impl ToolHandler for ReadTools<'_> {
    fn call(&mut self, name: &str, arguments: &Map<String, Value>) -> ToolResult {
        let result = match name {
            "abap_read_source" => self.read_source(arguments),
            "abap_read_table" => self.read_table(arguments),
            _ => {
                return ToolResult::error(format!(
                    "ERROR: {name} is not available in this build yet"
                ));
            }
        };

        result.unwrap_or_else(|message| ToolResult::error(format!("STOP: {message}")))
    }
}

fn done() -> Attempt {
    Attempt {
        outcome: Outcome {
            status: Some(Status::Active),
            exit: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
        failure: None,
        fallback_allowed: false,
        detail: None,
    }
}

pub(crate) fn status_token(status: Status) -> &'static str {
    match status {
        Status::Active => "ACTIVE",
        Status::SimulatedOk => "SIMULATED_OK",
        Status::Inactive => "INACTIVE",
        Status::Rejected => "REJECTED",
        Status::Partial => "PARTIAL",
    }
}

pub(crate) fn required<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    optional(arguments, key)?.ok_or_else(|| format!("{key} is required"))
}

pub(crate) fn optional<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// Error and note text goes through the scrub, including the hosts and the route; payload never.
pub(crate) fn diagnostic(connection: &Connection, text: &str) -> String {
    let mut secrets = connection.secrets();

    secrets.extend(connection.hosts());
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));

    scrub(text, &secrets)
}

pub(crate) fn holds_secret(connection: &Connection, payload: &str) -> bool {
    connection
        .secrets()
        .iter()
        .any(|secret| !secret.is_empty() && payload.contains(secret.as_str()))
}

fn add_notes(text: &mut String, structured: &mut Value, notes: &str) {
    if notes.is_empty() {
        return;
    }

    text.push('\n');
    text.push_str(notes);
    structured["notes"] = json!(notes);
}
