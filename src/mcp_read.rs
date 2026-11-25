//! The MCP read tools: `abap_read_source` and `abap_read_table` over the shared operations.
//! Every call opens its own connection, so a read never shares the session of a running write.
//! The result carries the same data in `content` text and in `structuredContent`; the status
//! comes from the operation's own state, never from formatted text.

use crate::app::{Attempt, Factories, end_session, read_attempt, route};
use crate::backend::OperationKind;
use crate::cli::{IncludeKind, LineRange, ObjectKind};
use crate::config::{Config, ConfigPaths, Connection, McpPolicies, scrub};
use crate::domain::{Outcome, Status};
use crate::extension::Registry;
use crate::hash::source_hash;
use crate::mcp::{ToolHandler, ToolResult, is_runner_type};
use crate::object::check_read;
use crate::operation::deploy::known_lock_handles;
use crate::operation::inactive::{self, InactiveEntry};
use crate::operation::quality;
use crate::operation::read::{ReadProg, ReadTarget, project_lines};
use crate::operation::read_table::{
    MAX_OUTPUT_BYTES, MCP_MAX_ROWS, Query, ReadTable, ReadTableRequest, mcp_rows,
};
use clap::ValueEnum;
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

/// The refusal of a type no read tool knows.
pub(crate) const READ_TYPES: &str =
    "type must be one of PROG, FUNC, CLAS, INTF, DTEL, DOMA, TABL, TTYP, TRAN";

/// The stderr line of a call that found other policies in the file than the server started with.
const POLICIES_CHANGED: &str =
    "W: policies changed in connections.json; restart the MCP server to apply them";

/// The STOP of a call whose connection the file now points elsewhere.
fn changed(id: &str) -> String {
    format!(
        "connection {id} changed in connections.json since the MCP server started; restart the \
         MCP server to use it"
    )
}

/// Whether two resolutions of one connection reach the same system with the same credentials:
/// everything but the policies, which the server freezes on their own.
fn same_target(now: &Connection, start: &Connection) -> bool {
    let mut now = now.clone();

    now.policies = start.policies.clone();

    now == *start
}

pub(crate) const SECRET_REJECTED: &str = "REJECTED: payload contains a configured secret";

/// What a server that re-reads its connection files keeps from its start.
struct Reload {
    paths: ConfigPaths,
    /// The policies of every server as the start loaded them; a later edit never changes them.
    start: McpPolicies,
    /// The connections of the start; a later edit of the file cannot move one to another target.
    known: BTreeMap<String, Result<Connection, String>>,
}

pub struct ReadTools<'a> {
    config: Config,
    reload: Option<Reload>,
    /// The stderr lines of the calls so far; `take_warnings` hands them to the session log.
    warnings: RefCell<Vec<String>>,
    env: BTreeMap<String, String>,
    factories: &'a mut dyn Factories,
    /// The extension types the tools refuse by name; none without a registry.
    registry: Option<&'a Registry>,
}

impl<'a> ReadTools<'a> {
    pub fn new(
        config: Config,
        env: BTreeMap<String, String>,
        factories: &'a mut dyn Factories,
    ) -> Self {
        Self {
            config,
            reload: None,
            warnings: RefCell::default(),
            env,
            factories,
            registry: None,
        }
    }

    /// Re-reads the connection files on every call to resolve the connection, so a connection
    /// added while the server runs resolves. The policies stay the ones `config` holds now: an
    /// edit of the file during a session cannot loosen them. A server that was absent at start
    /// runs with the strictest policies (`mcp_write` off) until the next start.
    #[must_use]
    pub fn with_reload(mut self, paths: ConfigPaths) -> Self {
        self.reload = Some(Reload {
            paths,
            start: self.config.mcp_policies(),
            known: self.config.start_connections(&self.env),
        });

        self
    }

    /// The stderr lines collected since the last call of this method. One tool call may resolve
    /// its connection twice, so a repeated line counts once.
    pub fn take_warnings(&self) -> Vec<String> {
        let mut lines = std::mem::take(&mut *self.warnings.borrow_mut());

        lines.dedup();

        lines
    }

    /// Lets `abap_read_source` name the refusal of an extension type.
    #[must_use]
    pub fn with_registry(mut self, registry: &'a Registry) -> Self {
        self.registry = Some(registry);

        self
    }

    pub(crate) fn connection(&self, arguments: &Map<String, Value>) -> Result<Connection, String> {
        let id = required(arguments, "conn")?;
        let Some(reload) = &self.reload else {
            return self.config.resolve(id, &self.env);
        };
        let fresh = Config::load(reload.paths.clone(), &self.env)?;
        let (system_id, client) = Config::parse_id(id)?;
        let key = format!("{system_id}_{client}").to_ascii_uppercase();
        let (mut connection, started) = match reload.known.get(&key) {
            // A connection of the start keeps its start target and credentials: the file may
            // add connections, never redirect one that is in use.
            Some(Ok(start)) => match fresh.resolve(id, &self.env) {
                Ok(now) if same_target(&now, start) => (start.clone(), true),
                _ => return Err(changed(&key)),
            },
            Some(Err(reason)) => return Err(reason.clone()),
            None => (fresh.resolve(id, &self.env)?, false),
        };
        // The policies of a connection of the start are the frozen ones of its server; any other
        // connection, a new server, a new client of a known server or a second entry with the
        // same system id, gets the strictest set the start allows: no MCP write, and a deny list
        // that is the start list plus the built-in tables.
        let fresh_policies = fresh
            .resolve(id, &self.env)
            .map_or_else(|_| connection.policies.clone(), |now| now.policies);
        let frozen = if started {
            reload.start.get(&connection.system_id)
        } else {
            None
        }
        .unwrap_or_else(|| reload.start.absent())
        .clone();

        if fresh_policies.for_mcp() != frozen {
            self.warnings.borrow_mut().push(POLICIES_CHANGED.to_owned());
        }

        connection.policies = frozen;

        Ok(connection)
    }

    pub(crate) fn factories(&mut self) -> &mut dyn Factories {
        &mut *self.factories
    }

    fn read_source(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let kind = required(arguments, "type")?;
        let token = kind;

        // A DDIC or transaction read needs the policy and the slot of the write tools.
        if is_runner_type(token) {
            return Err(format!(
                "reading {token} needs the write-capable MCP tools of the server (a temporary class)"
            ));
        }

        let kind = ObjectKind::from_str(token, false).map_err(|_| {
            self.registry
                .and_then(|registry| registry.kind(token))
                .and_then(|kind| kind.spec().mcp_read_refusal)
                .unwrap_or(READ_TYPES)
                .to_owned()
        })?;

        let name = required(arguments, "name")?;
        let include = match optional(arguments, "include")? {
            Some(include) => Some(
                IncludeKind::from_str(include, false)
                    .map_err(|_| "include must be one of CCIMP, CCDEF, CCMAC, CCAU".to_owned())?,
            ),
            None => None,
        };
        let fugr = optional(arguments, "fugr")?;
        let main = optional(arguments, "main")?;
        let range = match optional(arguments, "lines")? {
            Some(lines) => Some(lines.parse::<LineRange>()?),
            None => None,
        };

        check_read(kind, name, Path::new("-"), include, fugr, range, main)?;

        let connection = self.connection(arguments)?;
        let target = ReadTarget::new(kind, name)
            .group(fugr)
            .include(include)
            .lines(range)
            .main(main);
        let mut found = None;
        let exit = route(
            OperationKind::Read(kind),
            None,
            false,
            &connection,
            &mut *self.factories,
            |transport, _, _, _| {
                let read = ReadProg::fetch_lines(transport, target);

                // A read holds no lock, so a failed session end is only a note.
                let note = end_session(transport);
                let attempt = match read {
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
                };

                attempt.noted(note)
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
            Some(range) => {
                let mut text = format!(
                    "{block}{} lines (lines {}-{}, partial excerpt)",
                    shown.len(),
                    range.start,
                    range.end
                );
                let mut structured = json!({
                    "status": status_token(Status::Active),
                    "partial": true,
                    "excerpt": payload,
                    "lines": shown.len(),
                    "range": format!("{}-{}", range.start, range.end),
                });

                // The hash of the whole source lets a client send edits without a full read. It
                // is a hash of text the excerpt does not show, so it must not act as an oracle
                // for a configured secret elsewhere in the source: no hash then.
                if !holds_secret(&connection, &lines.join("\n")) {
                    let hash = source_hash(&lines);

                    write!(text, "\nhash {hash} (whole source)").expect("writing to a String");
                    structured["hash"] = json!(hash);
                }

                (text, structured)
            }
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

                // A read holds no lock, so a failed session end is only a note.
                let note = end_session(transport);
                let attempt = match read {
                    Ok(data) => {
                        found = Some(data);

                        done()
                    }
                    Err(error) => read_attempt(Err(error)),
                };

                attempt.noted(note)
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

impl ReadTools<'_> {
    fn inactive(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let all = match arguments.get("all") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(all)) => *all,
            Some(_) => return Err("all must be a boolean".to_owned()),
        };
        let connection = self.connection(arguments)?;
        let mut found = None;
        let exit = route(
            OperationKind::Inactive,
            None,
            false,
            &connection,
            &mut *self.factories,
            |transport, _, _, _| {
                let listed = inactive::list(transport, all, &connection.user).map(|entries| {
                    found = Some(entries);

                    Outcome {
                        status: Some(Status::Active),
                        exit: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    }
                });

                // A read holds no lock, so a failed session end is only a note.
                quality::attempt(listed, end_session(transport))
            },
        );
        let notes = diagnostic(&connection, &exit.stderr);

        let Some(entries) = found.filter(|_| exit.code == 0) else {
            return Ok(ToolResult::error(notes));
        };
        let mut text = entries
            .iter()
            .map(InactiveEntry::line)
            .collect::<Vec<_>>()
            .join("\n");

        if holds_secret(&connection, &text) {
            return Ok(ToolResult::error(SECRET_REJECTED.to_owned()));
        }

        if !text.is_empty() {
            text.push('\n');
        }

        text.push_str(&format!("SUMMARY: {} inactive objects", entries.len()));

        let objects: Vec<Value> = entries
            .iter()
            .map(|entry| {
                json!({
                    "type": entry.kind,
                    "name": entry.name,
                    "user": entry.user,
                    "main": entry.main_program(),
                })
            })
            .collect();
        let mut structured = json!({"objects": objects, "count": entries.len()});

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
            "abap_inactive" => self.inactive(arguments),
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
    // reqwest prints the URL host lowercased: hosts and route parts match in both spellings. The
    // password stays exact.
    let lowered: Vec<String> = secrets
        .iter()
        .filter(|secret| **secret != connection.password)
        .map(|secret| secret.to_ascii_lowercase())
        .collect();

    secrets.extend(lowered);
    // A lock handle is not bound to the session: it never leaves the process.
    secrets.extend(known_lock_handles());
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));

    scrub(text, &secrets)
}

pub(crate) fn holds_secret(connection: &Connection, payload: &str) -> bool {
    connection.holds_secret(payload)
}

fn add_notes(text: &mut String, structured: &mut Value, notes: &str) {
    if notes.is_empty() {
        return;
    }

    text.push('\n');
    text.push_str(notes);
    structured["notes"] = json!(notes);
}
