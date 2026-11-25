//! The MCP read tools of the 0.18.0 commands: `abap_sql`, `abap_search`, `abap_where_used`,
//! `abap_dumps` and `abap_dump`, and `abap_ping`. Each one runs the operation of its CLI command, so every check
//! (the grammar, `read_table_denied`, the dump owner rule, the admission of the transport) is the
//! one of the CLI and nothing is decided here. The text is what the CLI prints; the structured
//! result carries its fields. `abap_ping` prints the instance name in clear, like `ping`.
//!
//! The tools expose no `backend` argument, like the other MCP read tools: the router picks it.

use super::{
    ReadTools, SECRET_REJECTED, add_notes, boolean, count, diagnostic, holds_secret, optional,
    preview_rows, required, table_result,
};
use crate::app::{Exit, run_dump, run_dumps, run_ping, run_search, run_sql, run_where_used};
use crate::config::Connection;
use crate::mcp::ToolResult;
use crate::operation::dumps::{self, Dump, DumpQuery, DumpRow, ListOptions, ListQuery};
use crate::operation::ping::Pinged;
use crate::operation::read_table::MCP_MAX_ROWS;
use crate::operation::search::{self, Found, Query as SearchQuery};
use crate::operation::sql::Query as SqlQuery;
use crate::operation::where_used::{self, Listing, Query as WhereUsedQuery};
use serde_json::{Map, Value, json};

impl ReadTools<'_> {
    pub(super) fn sql(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let text = required(arguments, "query")?;
        let rows = preview_rows(arguments)?;
        let connection = self.connection(arguments)?;
        let query = SqlQuery::new(text, rows, MCP_MAX_ROWS, &connection.policies)?;
        let mut found = None;
        let exit = run_sql(&connection, &mut *self.factories, &query, &mut found);
        let notes = diagnostic(&connection, &exit.stderr);

        match found.filter(|_| exit.code == 0) {
            Some(data) => Ok(table_result(&connection, &data, &notes)),
            None => Ok(ToolResult::error(notes)),
        }
    }

    pub(super) fn search(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let pattern = required(arguments, "pattern")?;
        let object_type = optional(arguments, "type")?;
        let max = count(arguments, "max", &search::MAX_RESULTS)?.unwrap_or(search::DEFAULT_MAX);
        let connection = self.connection(arguments)?;
        let query = SearchQuery::new(pattern, object_type, max)?;
        let mut found = None;
        let exit = run_search(&connection, &mut *self.factories, &query, &mut found);

        Ok(typed_result(&connection, exit, found, search_view))
    }

    pub(super) fn where_used(
        &mut self,
        arguments: &Map<String, Value>,
    ) -> Result<ToolResult, String> {
        let object_type = required(arguments, "type")?;
        let name = required(arguments, "name")?;
        let max =
            count(arguments, "max", &where_used::MAX_RESULTS)?.unwrap_or(where_used::DEFAULT_MAX);
        let connection = self.connection(arguments)?;
        let query = WhereUsedQuery::new(object_type, name, max)?;
        let mut found = None;
        let exit = run_where_used(&connection, &mut *self.factories, &query, &mut found);

        Ok(typed_result(&connection, exit, found, where_used_view))
    }

    pub(super) fn dumps(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let date = optional(arguments, "date")?;
        let user = optional(arguments, "user")?;
        let all = boolean(arguments, "all")?;
        let rows = count(arguments, "rows", &dumps::ROWS)?.unwrap_or(dumps::DEFAULT_ROWS);
        let connection = self.connection(arguments)?;
        let options = ListOptions {
            date,
            user,
            all,
            rows,
        };
        let query = ListQuery::new(
            &options,
            &connection.user,
            &dumps::today(),
            &connection.policies,
        )?;
        let mut found = None;
        let exit = run_dumps(&connection, None, &mut *self.factories, &query, &mut found);

        Ok(typed_result(&connection, exit, found, dumps_view))
    }

    pub(super) fn dump(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let id = required(arguments, "id")?;
        let all = boolean(arguments, "all")?;
        let connection = self.connection(arguments)?;
        let query = DumpQuery::new(id, all, &connection.user, &connection.policies)?;
        let mut found = None;
        let exit = run_dump(&connection, None, &mut *self.factories, &query, &mut found);

        Ok(typed_result(&connection, exit, found, dump_view))
    }

    pub(super) fn ping(&mut self, arguments: &Map<String, Value>) -> Result<ToolResult, String> {
        let connection = self.connection(arguments)?;
        let mut found = None;
        let exit = run_ping(&connection, None, &mut *self.factories, &mut found);

        Ok(typed_result(&connection, exit, found, ping_view))
    }
}

/// The result of a run that answered with typed data: an error with the diagnostic of a refusal
/// or a failure (STOP exit 2, REJECTED or ERROR exit 1), else the text and the structured result
/// that `view` makes from the same data. The text goes through the configured-secret check
/// first, the notes through the scrub.
fn typed_result<T>(
    connection: &Connection,
    exit: Exit,
    found: Option<T>,
    view: impl FnOnce(&T) -> (String, Value),
) -> ToolResult {
    let notes = diagnostic(connection, &exit.stderr);
    let Some(found) = found.filter(|_| exit.code == 0) else {
        return ToolResult::error(notes);
    };
    let (mut text, mut structured) = view(&found);

    if holds_secret(connection, &text) {
        return ToolResult::error(SECRET_REJECTED.to_owned());
    }

    add_notes(&mut text, &mut structured, &notes);

    ToolResult {
        text,
        structured,
        is_error: false,
    }
}

/// A field the SAP reply may lack: `null` for none.
fn nullable(value: &Option<String>) -> Value {
    value.as_ref().map_or(Value::Null, |value| json!(value))
}

fn search_view(found: &Found) -> (String, Value) {
    let objects: Vec<Value> = found
        .entries
        .iter()
        .map(|entry| {
            json!({
                "type": entry.kind,
                "name": entry.name,
                "package": nullable(&entry.package),
                "description": entry.description,
            })
        })
        .collect();

    (
        found.text(),
        json!({"count": objects.len(), "objects": objects}),
    )
}

fn where_used_view(found: &Listing) -> (String, Value) {
    let used_by: Vec<Value> = found
        .hits
        .iter()
        .map(|hit| {
            json!({
                "type": hit.kind,
                "name": hit.name,
                "package": nullable(&hit.package),
                "via": hit.via,
            })
        })
        .collect();

    (
        found.text(),
        json!({"count": used_by.len(), "used_by": used_by}),
    )
}

fn dumps_view(found: &dumps::Listing) -> (String, Value) {
    let entry = |row: &DumpRow| {
        json!({
            "date": nullable(&row.date),
            "time": nullable(&row.time),
            "user": nullable(&row.user),
            "client": nullable(&row.client),
            "error": nullable(&row.error),
            "program": nullable(&row.program),
            "include": nullable(&row.include),
            "line": nullable(&row.line),
            "id": nullable(&row.id),
        })
    };
    let dumps: Vec<Value> = found.rows.iter().map(entry).collect();

    (
        found.text.clone(),
        json!({"count": dumps.len(), "dumps": dumps}),
    )
}

fn dump_view(dump: &Dump) -> (String, Value) {
    (
        dump.text(),
        json!({
            "error": nullable(&dump.error),
            "program": nullable(&dump.program),
            "include": nullable(&dump.include),
            "line": nullable(&dump.line),
            "user": dump.user,
            "client": nullable(&dump.client),
            "short_text": dump.short_text,
            "source": dump.source,
        }),
    )
}

/// The fields `ping` prints. `instance` is the name as SAP reports it, or `null` with the fixed
/// reason in `instance_unknown`; a field the backend does not report is `null`.
fn ping_view(pinged: &Pinged) -> (String, Value) {
    let facts = &pinged.reading.facts;
    let (instance, unknown) = match &facts.instance {
        Ok(name) => (json!(name), Value::Null),
        Err(reason) => (Value::Null, json!(reason)),
    };

    (
        pinged.text(),
        json!({
            "backend": pinged.backend.to_string(),
            "instance": instance,
            "instance_unknown": unknown,
            "system_id": nullable(&facts.system_id),
            "release": nullable(&facts.release),
            "kernel": nullable(&facts.kernel),
            "database": nullable(&facts.database),
            "os": nullable(&facts.os),
            "round_trip_ms": u64::try_from(pinged.reading.round_trip.as_millis()).unwrap_or(u64::MAX),
            "includes_logon": pinged.reading.includes_logon,
        }),
    )
}
