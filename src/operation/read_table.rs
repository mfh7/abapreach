//! `read-table`: rows of one DDIC table or database view through the ADT data preview.
//!
//! SAP executes the data preview body as written: a `FROM` in the body overrides
//! `ddicEntityName`, and JOINs and subqueries run. So the body is never user text. It is built
//! from tokens that passed the grammar below, `preview_request` re-checks every body against the
//! entity it names, and no request is sent for an input that fails any of it.

use crate::backend::Failure;
use crate::operation::deploy::leaf_texts;
use crate::policy::Policies;
use crate::transport::{Request, SadtTransport, TransportError};
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// The largest `--rows`: the data preview of the 740 release falls back to 100 rows above it.
pub const CLI_MAX_ROWS: u32 = 5000;
/// Rows an MCP call returns when it names none.
pub const MCP_DEFAULT_ROWS: u32 = 100;
/// The largest row count of an MCP call.
pub const MCP_MAX_ROWS: u32 = 1000;
/// Output beyond this many bytes is cut at a row boundary.
pub const MAX_OUTPUT_BYTES: usize = 1 << 20;
/// The width of one `RFC_READ_TABLE` OPTIONS line.
pub const WHERE_LINE_MAX: usize = 72;

const PREVIEW_URI: &str = "/sap/bc/adt/datapreview/ddic";
const CLASS_ROWS: u32 = 10;
const BASE_ROWS: u32 = 500;
const IDENTIFIER_MAX: usize = 30;
const LITERAL_MAX: usize = 255;
const WHERE_MAX: usize = 2000;
const DEPTH_MAX: usize = 16;
/// Words that are SQL syntax, never a field of a table.
const RESERVED: [&str; 20] = [
    "SELECT", "FROM", "JOIN", "EXISTS", "UNION", "WHERE", "AND", "OR", "NOT", "IN", "BETWEEN",
    "LIKE", "IS", "NULL", "INTO", "ON", "AS", "ORDER", "GROUP", "HAVING",
];

/// What the caller asked for, before any check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadTableRequest {
    pub table: String,
    pub fields: Vec<String>,
    pub where_clause: Option<String>,
    pub rows: u32,
}

/// A request that passed every local rule: the only way to reach the data preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    table: String,
    body: String,
    rows: u32,
}

impl Query {
    /// Validates the request and applies the direct deny list. Any `Err` is a `STOP` and no
    /// connection is opened.
    pub fn new(
        request: &ReadTableRequest,
        max_rows: u32,
        policies: &Policies,
    ) -> Result<Self, String> {
        let (table, fields, condition) = query_tokens(
            &request.table,
            &request.fields,
            request.where_clause.as_deref(),
        )?;

        if request.rows == 0 || request.rows > max_rows {
            return Err(format!("rows must be between 1 and {max_rows}"));
        }

        if policies.is_read_table_denied(&table) {
            return Err(format!("table {table} is on the read_table_denied list"));
        }

        let body = render_body(&table, &fields, condition.as_deref());

        Ok(Self {
            table,
            body,
            rows: request.rows,
        })
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }
}

/// The row count of an MCP call: 100 when absent, 1 to 1000 otherwise.
pub fn mcp_rows(requested: Option<u32>) -> Result<u32, String> {
    match requested {
        None => Ok(MCP_DEFAULT_ROWS),
        Some(rows) if (1..=MCP_MAX_ROWS).contains(&rows) => Ok(rows),
        Some(_) => Err(format!("rows must be between 1 and {MCP_MAX_ROWS}")),
    }
}

/// An upper-case DDIC identifier: `[A-Z0-9_]`, or `/NAMESPACE/NAME`.
fn identifier(what: &str, text: &str) -> Result<String, String> {
    let name = text.to_ascii_uppercase();
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    let valid = name.len() <= IDENTIFIER_MAX
        && match name.strip_prefix('/') {
            Some(rest) => rest
                .split_once('/')
                .is_some_and(|(space, local)| plain(space) && plain(local)),
            None => plain(&name),
        };

    if valid {
        Ok(name)
    } else {
        Err(format!(
            "{what} {text:?} is not a DDIC identifier (letters, digits, _ or /NAMESPACE/NAME, at most {IDENTIFIER_MAX} characters)"
        ))
    }
}

fn field_name(text: &str) -> Result<String, String> {
    let name = identifier("field", text)?;

    if RESERVED.contains(&name.as_str()) {
        return Err(format!("field {text:?} is an SQL keyword"));
    }

    Ok(name)
}

/// Parse the tokens shared by normal `read-table` validation and the transport boundary. The
/// caller decides separately whether the table is denied by a policy.
fn query_tokens(
    table: &str,
    fields: &[String],
    where_clause: Option<&str>,
) -> Result<(String, Vec<String>, Option<String>), String> {
    let table = identifier("table", table)?;
    let fields = fields
        .iter()
        .map(|field| field_name(field.trim_matches(' ')))
        .collect::<Result<Vec<_>, _>>()?;
    let condition = where_clause.map(canonical_where).transpose()?;

    Ok((table, fields, condition))
}

fn render_body(table: &str, fields: &[String], condition: Option<&str>) -> String {
    let list = if fields.is_empty() {
        "*".to_owned()
    } else {
        fields.join(", ")
    };
    let mut body = format!("SELECT {list} FROM {table}");

    if let Some(condition) = condition {
        body.push_str(" WHERE ");
        body.push_str(condition);
    }

    body
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Word(String),
    Number(String),
    Literal(String),
    Operator(&'static str),
    Open,
    Close,
    Comma,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < chars.len() {
        let character = chars[index];

        match character {
            ' ' => index += 1,
            '(' => {
                tokens.push(Token::Open);
                index += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                index += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                index += 1;
            }
            '=' => {
                tokens.push(Token::Operator("="));
                index += 1;
            }
            '<' | '>' => {
                let next = chars.get(index + 1).copied();
                let (operator, width) = match (character, next) {
                    ('<', Some('>')) => ("<>", 2),
                    ('<', Some('=')) => ("<=", 2),
                    ('>', Some('=')) => (">=", 2),
                    ('<', _) => ("<", 1),
                    _ => (">", 1),
                };

                tokens.push(Token::Operator(operator));
                index += width;
            }
            '\'' => {
                let (value, next) = literal(&chars, index)?;

                tokens.push(Token::Literal(value));
                index = next;
            }
            '-' if chars.get(index + 1).is_some_and(char::is_ascii_digit) => {
                let (number, next) = number(&chars, index + 1);

                tokens.push(Token::Number(format!("-{number}")));
                index = next;
            }
            character
                if character.is_ascii_alphanumeric() || character == '_' || character == '/' =>
            {
                let end = chars[index..]
                    .iter()
                    .position(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '/'))
                    .map_or(chars.len(), |offset| index + offset);
                let word: String = chars[index..end].iter().collect();

                if word.bytes().all(|byte| byte.is_ascii_digit()) {
                    let (number, next) = number(&chars, index);

                    tokens.push(Token::Number(number));
                    index = next;
                } else {
                    tokens.push(Token::Word(word));
                    index = end;
                }
            }
            other => {
                return Err(format!(
                    "--where: character {other:?} is not allowed (only fields, comparison operators, AND/OR/NOT, parentheses and literals)"
                ));
            }
        }
    }

    Ok(tokens)
}

/// Digits with an optional fraction, starting at `start`; returns the text and the next index.
fn number(chars: &[char], start: usize) -> (String, usize) {
    let mut end = start;

    while chars.get(end).is_some_and(char::is_ascii_digit) {
        end += 1;
    }

    if chars.get(end) == Some(&'.') && chars.get(end + 1).is_some_and(char::is_ascii_digit) {
        end += 1;

        while chars.get(end).is_some_and(char::is_ascii_digit) {
            end += 1;
        }
    }

    (chars[start..end].iter().collect(), end)
}

/// The unescaped content of the string literal that opens at `start`, and the next index.
fn literal(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut value = String::new();
    let mut index = start + 1;

    while let Some(&character) = chars.get(index) {
        if character == '\'' {
            if chars.get(index + 1) == Some(&'\'') {
                value.push('\'');
                index += 2;

                continue;
            }

            if value.chars().count() > LITERAL_MAX {
                return Err(format!(
                    "--where: a literal is longer than {LITERAL_MAX} characters"
                ));
            }

            return Ok((value, index + 1));
        }

        if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            return Err("--where: a literal holds a control character".to_owned());
        }

        value.push(character);
        index += 1;
    }

    Err("--where: unterminated quoted literal".to_owned())
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    out: String,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(found)) if found.eq_ignore_ascii_case(word))
    }

    fn take_keyword(&mut self, word: &str) -> bool {
        if self.keyword(word) {
            self.position += 1;
            self.put(word);

            return true;
        }

        false
    }

    fn put(&mut self, text: &str) {
        if !self.out.is_empty() && text != "," {
            self.out.push(' ');
        }

        self.out.push_str(text);
    }

    fn expect_keyword(&mut self, word: &str) -> Result<(), String> {
        if self.take_keyword(word) {
            Ok(())
        } else {
            Err(format!("--where: expected {word}"))
        }
    }

    fn expect(&mut self, token: &Token, text: &str) -> Result<(), String> {
        if self.peek() == Some(token) {
            self.position += 1;
            self.put(text);

            Ok(())
        } else {
            Err(format!("--where: expected {text}"))
        }
    }

    fn expression(&mut self, depth: usize) -> Result<(), String> {
        if depth > DEPTH_MAX {
            return Err("--where: nested too deeply".to_owned());
        }

        self.conjunction(depth)?;

        while self.take_keyword("OR") {
            self.conjunction(depth)?;
        }

        Ok(())
    }

    fn conjunction(&mut self, depth: usize) -> Result<(), String> {
        self.negation(depth)?;

        while self.take_keyword("AND") {
            self.negation(depth)?;
        }

        Ok(())
    }

    fn negation(&mut self, depth: usize) -> Result<(), String> {
        if depth > DEPTH_MAX {
            return Err("--where: nested too deeply".to_owned());
        }

        if self.take_keyword("NOT") {
            return self.negation(depth + 1);
        }

        if self.peek() == Some(&Token::Open) {
            self.position += 1;
            self.put("(");
            self.expression(depth + 1)?;

            return self.expect(&Token::Close, ")");
        }

        self.predicate()
    }

    fn predicate(&mut self) -> Result<(), String> {
        let Some(Token::Word(word)) = self.peek().cloned() else {
            return Err("--where: expected a field name".to_owned());
        };
        let field = field_name(&word).map_err(|error| format!("--where: {error}"))?;

        self.position += 1;
        self.put(&field);

        match self.peek().cloned() {
            Some(Token::Operator(operator)) => {
                self.position += 1;
                self.put(operator);
                self.value(false)
            }
            Some(Token::Word(_)) if self.take_keyword("LIKE") => self.value(true),
            Some(Token::Word(_)) if self.take_keyword("IN") => {
                self.expect(&Token::Open, "(")?;
                self.value(false)?;

                while self.peek() == Some(&Token::Comma) {
                    self.position += 1;
                    self.put(",");
                    self.value(false)?;
                }

                self.expect(&Token::Close, ")")
            }
            Some(Token::Word(_)) if self.take_keyword("BETWEEN") => {
                self.value(false)?;
                self.expect_keyword("AND")?;
                self.value(false)
            }
            Some(Token::Word(_)) if self.take_keyword("IS") => {
                self.take_keyword("NOT");
                self.expect_keyword("NULL")
            }
            _ => Err(format!("--where: expected an operator after {field}")),
        }
    }

    /// A literal operand; a string only when `text_only`.
    fn value(&mut self, text_only: bool) -> Result<(), String> {
        match self.peek().cloned() {
            Some(Token::Literal(value)) => {
                self.position += 1;
                self.put(&format!("'{}'", value.replace('\'', "''")));

                Ok(())
            }
            Some(Token::Number(number)) if !text_only => {
                self.position += 1;
                self.put(&number);

                Ok(())
            }
            _ => Err(if text_only {
                "--where: expected a quoted literal".to_owned()
            } else {
                "--where: expected a literal (a quoted string or a number)".to_owned()
            }),
        }
    }
}

/// The condition re-written from its tokens, or the first rule it breaks.
fn canonical_where(text: &str) -> Result<String, String> {
    if text.len() > WHERE_MAX {
        return Err(format!("--where is longer than {WHERE_MAX} characters"));
    }

    let tokens = tokenize(text)?;

    if tokens.is_empty() {
        return Err("--where is empty".to_owned());
    }

    let mut parser = Parser {
        tokens,
        position: 0,
        out: String::new(),
    };

    parser.expression(0)?;

    if parser.position != parser.tokens.len() {
        return Err("--where: unexpected text after the condition".to_owned());
    }

    Ok(parser.out)
}

/// Parse a data-preview body with the same grammar as `Query` and require its exact canonical
/// rendering. The transport admission boundary uses this without a deny list: policy belongs to
/// the operation layer, while all admitted SQL-like syntax must still come from these tokens.
pub fn validate_preview_body(entity: &str, body: &str) -> Result<(), String> {
    let mismatch = || format!("data preview body does not read {entity}: refusing to send it");
    let rest = body.strip_prefix("SELECT ").ok_or_else(mismatch)?;
    let (list, tail) = rest.split_once(" FROM ").ok_or_else(mismatch)?;
    let (table, where_clause) = match tail.split_once(" WHERE ") {
        Some((table, condition)) => (table, Some(condition)),
        None => (tail, None),
    };
    let fields: Vec<String> = if list == "*" {
        Vec::new()
    } else {
        list.split(", ").map(str::to_owned).collect()
    };
    let (entity, fields, condition) =
        query_tokens(entity, &fields, where_clause).map_err(|_| mismatch())?;

    if table != entity || render_body(&entity, &fields, condition.as_deref()) != body {
        return Err(mismatch());
    }

    Ok(())
}

/// The data preview POST for `body`, checked against `entity`.
pub fn preview_request(entity: &str, body: &str, rows: u32) -> Result<Request, String> {
    if rows == 0 {
        return Err("data preview needs at least one row".to_owned());
    }

    validate_preview_body(entity, body)?;

    let mut request = Request::post(
        format!(
            "{PREVIEW_URI}?rowNumber={rows}&ddicEntityName={}",
            entity.replace('/', "%2F")
        ),
        body.as_bytes().to_vec(),
    );

    request
        .headers
        .insert("Accept".to_owned(), "application/*".to_owned());
    request
        .headers
        .insert("Content-Type".to_owned(), "text/plain".to_owned());

    Ok(request)
}

/// Splits a condition into lines of at most 72 characters for the `RFC_READ_TABLE` fallback;
/// a quoted literal is never cut.
pub fn split_where(text: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut gap = "";
    let mut index = 0;

    while index < chars.len() {
        let start = index;

        if chars[index].is_whitespace() {
            while chars.get(index).is_some_and(|c| c.is_whitespace()) {
                index += 1;
            }

            gap = " ";

            continue;
        }

        if chars[index] == '\'' {
            let mut close = index + 1;

            loop {
                match chars.get(close) {
                    None => return Err(format!("--where: unterminated quoted literal: {text}")),
                    Some('\'') if chars.get(close + 1) == Some(&'\'') => close += 2,
                    Some('\'') => break,
                    Some(_) => close += 1,
                }
            }

            index = close + 1;
        } else {
            while chars
                .get(index)
                .is_some_and(|c| !c.is_whitespace() && *c != '\'')
            {
                index += 1;
            }
        }

        let token: String = chars[start..index].iter().collect();
        let width = token.chars().count();

        if width > WHERE_LINE_MAX {
            let head: String = token.chars().take(30).collect();

            return Err(format!(
                "--where: token {width} characters > {WHERE_LINE_MAX}: {head}..."
            ));
        }

        if !current.is_empty() && current.chars().count() + gap.len() + width <= WHERE_LINE_MAX {
            current.push_str(gap);
            current.push_str(&token);
        } else {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }

            current = token;
        }

        gap = "";
    }

    if !current.is_empty() {
        lines.push(current);
    }

    Ok(lines)
}

/// The rows of one data preview: column names and row-major values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableData {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl TableData {
    /// The tab-separated table, cut at a row boundary once it would pass `max_bytes`, and the
    /// number of rows left out. Tabs and line breaks inside a value become spaces.
    pub fn render(&self, max_bytes: usize) -> (String, usize) {
        let clean = |value: &str| value.replace(['\t', '\r', '\n'], " ");
        let mut text = self
            .columns
            .iter()
            .map(|column| clean(column))
            .collect::<Vec<_>>()
            .join("\t");

        for (shown, row) in self.rows.iter().enumerate() {
            let line = row
                .iter()
                .map(|value| clean(value))
                .collect::<Vec<_>>()
                .join("\t");

            if text.len() + 1 + line.len() > max_bytes {
                return (text, self.rows.len() - shown);
            }

            text.push('\n');
            text.push_str(&line);
        }

        (text, 0)
    }

    fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column == name)
    }
}

/// Why a read stopped after its checks passed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadTableError {
    /// The dictionary lookup refused the table or could not vouch for it: nothing was read.
    Rejected(String),
    /// The backend failed; `failure` is the routing class.
    Failed { failure: Failure, message: String },
}

impl ReadTableError {
    fn failed(failure: Failure, message: String) -> Self {
        Self::Failed { failure, message }
    }

    fn from_transport(error: &TransportError) -> Self {
        Self::failed(Failure::from(error), error.to_string())
    }
}

pub struct ReadTable;

impl ReadTable {
    /// Resolves the table through DD02L (and DD26S for a view), applies the deny list to every
    /// base table, then reads. At most three previews: a connection never nears the RFC limit of
    /// 36 per connection.
    pub fn run(
        transport: &mut dyn SadtTransport,
        query: &Query,
        policies: &Policies,
    ) -> Result<TableData, ReadTableError> {
        resolve(transport, query.table(), policies)?;

        let mut data = fetch(transport, query.table(), query.body(), query.rows())?;

        // The 750 release answers one row more than asked.
        data.rows.truncate(query.rows() as usize);

        Ok(data)
    }
}

fn resolve(
    transport: &mut dyn SadtTransport,
    table: &str,
    policies: &Policies,
) -> Result<(), ReadTableError> {
    let body = format!(
        "SELECT TABNAME, TABCLASS, VIEWCLASS FROM DD02L WHERE TABNAME = '{table}' AND AS4LOCAL = 'A'"
    );
    let class = lookup(transport, "DD02L", &body, CLASS_ROWS, table)?;
    let cell = |data: &TableData, name: &str, row: usize| {
        data.column(name)
            .map(|index| data.rows[row][index].trim().to_owned())
    };

    match class.rows.len() {
        0 => {
            return Err(rejected(format!(
                "{table} is not in the ABAP dictionary as an active table or view"
            )));
        }
        1 => {}
        _ => {
            return Err(rejected(format!(
                "{table} resolves to more than one dictionary entry"
            )));
        }
    }

    if cell(&class, "TABNAME", 0).as_deref() != Some(table) {
        return Err(rejected(format!(
            "the dictionary lookup did not return {table}"
        )));
    }

    match cell(&class, "TABCLASS", 0).as_deref() {
        Some("TRANSP" | "POOL" | "CLUSTER") => Ok(()),
        Some("VIEW") => {
            if cell(&class, "VIEWCLASS", 0).as_deref() != Some("D") {
                return Err(rejected(format!(
                    "{table} is not a database view (CDS/DDLS entities and other view types are not supported)"
                )));
            }

            check_view(transport, table, policies)
        }
        other => Err(rejected(format!(
            "{table} is neither a table nor a database view (table category {})",
            other.unwrap_or("missing")
        ))),
    }
}

fn check_view(
    transport: &mut dyn SadtTransport,
    view: &str,
    policies: &Policies,
) -> Result<(), ReadTableError> {
    let body = format!(
        "SELECT VIEWNAME, TABNAME, TABPOS FROM DD26S WHERE VIEWNAME = '{view}' AND AS4LOCAL = 'A'"
    );
    let bases = lookup(transport, "DD26S", &body, BASE_ROWS, view)?;
    let (Some(name), Some(base)) = (bases.column("VIEWNAME"), bases.column("TABNAME")) else {
        return Err(rejected(format!(
            "the base tables of {view} are unreadable"
        )));
    };

    if bases.rows.is_empty() || bases.rows.len() >= BASE_ROWS as usize {
        return Err(rejected(format!(
            "the base tables of {view} are missing or too many to check"
        )));
    }

    for row in &bases.rows {
        let table = row[base].trim();

        if row[name].trim() != view || table.is_empty() {
            return Err(rejected(format!(
                "the base tables of {view} are unreadable"
            )));
        }

        if policies.is_read_table_denied(table) {
            return Err(rejected(format!(
                "view {view} reads table {} (read_table_denied)",
                table.to_ascii_uppercase()
            )));
        }
    }

    Ok(())
}

fn rejected(message: String) -> ReadTableError {
    ReadTableError::Rejected(format!("{message}; nothing was read"))
}

/// A dictionary lookup: only a lost connection or a refused logon stay failures that routing
/// understands; anything else the lookup cannot answer is a rejection (fail closed).
fn lookup(
    transport: &mut dyn SadtTransport,
    entity: &str,
    body: &str,
    rows: u32,
    subject: &str,
) -> Result<TableData, ReadTableError> {
    match fetch(transport, entity, body, rows) {
        Err(ReadTableError::Failed {
            failure: failure @ (Failure::PreSendConnectivity | Failure::Authorization),
            message,
        }) => Err(ReadTableError::failed(failure, message)),
        Err(ReadTableError::Failed { message, .. }) => Err(rejected(format!(
            "cannot resolve {subject} in the ABAP dictionary ({message})"
        ))),
        other => other,
    }
}

fn fetch(
    transport: &mut dyn SadtTransport,
    entity: &str,
    body: &str,
    rows: u32,
) -> Result<TableData, ReadTableError> {
    let request = preview_request(entity, body, rows)
        .map_err(|message| ReadTableError::failed(Failure::Validation, message))?;
    let response = transport
        .request(request)
        .map_err(|error| ReadTableError::from_transport(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(ReadTableError::from_transport(&error));
    }

    let text = String::from_utf8_lossy(&response.body);

    if response.status != 200 {
        let detail = leaf_texts(&text)
            .ok()
            .and_then(|fields| fields.get("localizedMessage").cloned())
            .filter(|message| !message.trim().is_empty());
        let message = match detail {
            Some(detail) => format!("HTTP {}: {}", response.status, detail.trim()),
            None => format!("HTTP {}", response.status),
        };
        let error = TransportError::Http {
            status: response.status,
            message: String::new(),
        };

        return Err(ReadTableError::failed(Failure::from(&error), message));
    }

    parse_preview(&text).map_err(|message| {
        ReadTableError::failed(
            Failure::Validation,
            format!("cannot read the data preview reply: {message}"),
        )
    })
}

/// The column-major `dataPreview:tableData` reply as rows.
pub fn parse_preview(xml: &str) -> Result<TableData, String> {
    let mut reader = Reader::from_str(xml);
    let mut names: Vec<String> = Vec::new();
    let mut columns: Vec<Vec<String>> = Vec::new();
    let mut value: Option<String> = None;
    let mut root = false;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) => match element.local_name().into_inner() {
                "tableData" => root = true,
                "columns" => {
                    names.push(String::new());
                    columns.push(Vec::new());
                }
                "metadata" => set_name(&element, &mut names)?,
                "data" => value = Some(String::new()),
                _ => {}
            },
            Event::Empty(element) => match element.local_name().into_inner() {
                "metadata" => set_name(&element, &mut names)?,
                "data" => columns
                    .last_mut()
                    .ok_or("a value outside a column")?
                    .push(String::new()),
                _ => {}
            },
            Event::Text(text) => {
                if let Some(value) = value.as_mut() {
                    value.push_str(&text.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(value) = value.as_mut() {
                    let resolved = match reference
                        .resolve_char_ref()
                        .map_err(|error| error.to_string())?
                    {
                        Some(character) => character.to_string(),
                        None => resolve_predefined_entity(&reference)
                            .ok_or_else(|| format!("unknown entity &{};", &*reference))?
                            .to_owned(),
                    };

                    value.push_str(&resolved);
                }
            }
            Event::End(element) if element.local_name().into_inner() == "data" => {
                let done = value.take().unwrap_or_default();

                columns
                    .last_mut()
                    .ok_or("a value outside a column")?
                    .push(done);
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !root || columns.is_empty() || names.iter().any(String::is_empty) {
        return Err("no table columns".to_owned());
    }

    let height = columns[0].len();

    if columns.iter().any(|column| column.len() != height) {
        return Err("columns have different lengths".to_owned());
    }

    let rows = (0..height)
        .map(|row| columns.iter().map(|column| column[row].clone()).collect())
        .collect();

    Ok(TableData {
        columns: names,
        rows,
    })
}

fn set_name(element: &BytesStart<'_>, names: &mut [String]) -> Result<(), String> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;

        if attribute.key.local_name().into_inner() == "name" {
            let value = attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|error| error.to_string())?
                .into_owned();

            *names.last_mut().ok_or("a column name outside a column")? = value;
        }
    }

    Ok(())
}
