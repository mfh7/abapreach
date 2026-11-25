//! The MCP read tools: `abap_read_source` and `abap_read_table` over the shared operations.
//! Every call opens its own connection, so a read never shares the session of a running write.
//! The result carries the same data in `content` text and in `structuredContent`; the status
//! comes from the operation's own state, never from formatted text.

mod features;

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
    MAX_OUTPUT_BYTES, MCP_MAX_ROWS, Query, ReadTable, ReadTableRequest, TableData, mcp_rows,
};
use clap::ValueEnum;
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::RangeInclusive;
use std::path::Path;

/// The refusal of a type no read tool knows.
pub(crate) const READ_TYPES: &str =
    "type must be one of PROG, FUNC, CLAS, INTF, DTEL, DOMA, TABL, TTYP, TRAN";

/// The refusal of a connection id that names no configured system and client. It does not quote
/// the id: an MCP client may have sent anything, a secret included, and a refusal comes back in
/// its context. The CLI names the id, since the user typed it.
pub(crate) const UNKNOWN_CONNECTION: &str =
    "unknown connection id; use <SID>_<client> of a configured system and client";

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
            if !self.config.is_configured(id) {
                return Err(UNKNOWN_CONNECTION.to_owned());
            }

            return self.config.resolve(id, &self.env);
        };
        let fresh = Config::load(reload.paths.clone(), &self.env)?;

        let (system_id, client) =
            Config::parse_id(id).map_err(|_| UNKNOWN_CONNECTION.to_owned())?;
        let key = format!("{system_id}_{client}").to_ascii_uppercase();
        let known = reload.known.get(&key);

        // An id of the start is a real one and keeps its own refusals; any other must name a
        // system and a client of the file now.
        if known.is_none() && !fresh.is_configured(id) {
            return Err(UNKNOWN_CONNECTION.to_owned());
        }

        let (mut connection, started) = match known {
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
        let rows = preview_rows(arguments)?;
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

        Ok(table_result(&connection, &data, &notes))
    }
}

impl ReadTools<'_> {
    fn inactive(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let all = boolean(arguments, "all")?;
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
            "abap_sql" => self.sql(arguments),
            "abap_search" => self.search(arguments),
            "abap_where_used" => self.where_used(arguments),
            "abap_dumps" => self.dumps(arguments),
            "abap_dump" => self.dump(arguments),
            "abap_ping" => self.ping(arguments),
            _ => {
                return ToolResult::error(format!(
                    "ERROR: {name} is not available in this build yet"
                ));
            }
        };

        self.seal(
            arguments,
            result.unwrap_or_else(|message| ToolResult::error(format!("STOP: {message}"))),
        )
    }
}

impl ReadTools<'_> {
    /// The one place every result of a read tool passes before it leaves the server, so a tool
    /// cannot forget it: an error (an argument error echoes what the client sent, a refusal
    /// what SAP said) goes through the scrub of the connection, in every case of its secrets, its
    /// hosts and its route, in the text and in the structured message. A result that carries data
    /// has passed the secret check of its payload already; here every string of it is scrubbed of
    /// the exact secrets once more, so that no field of the structure can carry one that the
    /// text check did not see. Hosts and cases are left alone there: source and rows are never
    /// rewritten.
    fn seal(&self, arguments: &Map<String, Value>, result: ToolResult) -> ToolResult {
        let Ok(connection) = self.connection(arguments) else {
            // No connection, so no secret to name; a lock handle never leaves the process.
            if !result.is_error {
                return result;
            }

            return ToolResult::error(scrub(result.text, &known_lock_handles()));
        };

        if result.is_error {
            return ToolResult::error(error_scrub(&connection, &result.text));
        }

        let secrets = connection.secrets();
        let mut result = result;

        result.text = scrub(result.text, &secrets);
        scrub_strings(&mut result.structured, &secrets);

        result
    }
}

/// An error text without any secret of the connection: the exact values and their lower and upper
/// case (a tool may fold what the client sent), its hosts and its route.
fn error_scrub(connection: &Connection, text: &str) -> String {
    let mut folded: Vec<String> = connection
        .secrets()
        .iter()
        .flat_map(|secret| [secret.to_ascii_lowercase(), secret.to_ascii_uppercase()])
        .collect();

    folded.sort_by_key(|secret| std::cmp::Reverse(secret.len()));

    scrub(diagnostic(connection, text), &folded)
}

/// Scrubs every string of a JSON value, the object keys excepted.
fn scrub_strings(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = scrub(std::mem::take(text), secrets),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| scrub_strings(item, secrets)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|field| scrub_strings(field, secrets)),
        _ => {}
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

/// The `rows` argument of a data preview tool: default 100, at most 1000.
pub(crate) fn preview_rows(arguments: &Map<String, Value>) -> Result<u32, String> {
    let rows = count(arguments, "rows", &(1..=MCP_MAX_ROWS))?;

    mcp_rows(rows)
}

/// An optional count argument: an integer within `range`.
pub(crate) fn count(
    arguments: &Map<String, Value>,
    key: &str,
    range: &RangeInclusive<u32>,
) -> Result<Option<u32>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .filter(|count| range.contains(count))
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "{key} must be between {} and {}",
                    range.start(),
                    range.end()
                )
            }),
    }
}

/// An optional flag argument: `false` when absent.
pub(crate) fn boolean(arguments: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

/// The result of a data preview read: the table as the CLI prints it, cut at the output limit, and
/// the same rows as structured data. Rows that hold a configured secret are not shown at all.
fn table_result(connection: &Connection, data: &TableData, notes: &str) -> ToolResult {
    let (mut text, omitted) = data.render(MAX_OUTPUT_BYTES);
    let shown = data.rows.len() - omitted;
    // The rendering may change a cell (a control character, a tab), so the cells are checked as
    // SAP sent them, rows past the output cut included.
    let raw_cells = data.columns.iter().chain(data.rows.iter().flatten());

    if holds_secret(connection, &text)
        || raw_cells.clone().any(|cell| holds_secret(connection, cell))
    {
        return ToolResult::error(SECRET_REJECTED.to_owned());
    }

    let cut = format!("W: output cut at {MAX_OUTPUT_BYTES} bytes; {omitted} rows left out");

    // A run that already carries the warning as a note does not say it twice.
    if omitted > 0 && !notes.contains(&cut) {
        text.push('\n');
        text.push_str(&cut);
    }

    let mut structured = json!({
        "columns": data.columns,
        "rows": &data.rows[..shown],
        "row_count": shown,
        "truncated": omitted > 0,
        "omitted": omitted,
    });

    add_notes(&mut text, &mut structured, notes);

    ToolResult {
        text,
        structured,
        is_error: false,
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
