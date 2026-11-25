//! `sql`: the grammar of a read-only SELECT with joins.
//!
//! The ADT freestyle data preview runs its body as SAP reads it. So the user text is never sent.
//! It is parsed into a [`Select`], and the only thing that may reach SAP is the single-line
//! canonical rendering of that tree ([`Select::body`]). [`validate_body`] re-parses a body and
//! refuses it unless it equals its own canonical rendering. The freestyle transport admission
//! boundary will call it (wired in with the `sql` command), like
//! `read_table::validate_preview_body`.
//!
//! Accepted:
//!
//! ```text
//! SELECT [DISTINCT] item {, item} | *
//! FROM entity [AS alias]
//! {[INNER | LEFT [OUTER]] JOIN entity [AS alias] ON col = col {AND col = col}}
//! [WHERE condition] [GROUP BY col {, col}] [HAVING condition]
//! [ORDER BY col [ASCENDING | DESCENDING] {, ...}]
//! ```
//!
//! An item is `col [AS alias]` or `COUNT|SUM|MIN|MAX|AVG ( [DISTINCT] col )` or `COUNT( * )`, with
//! an optional `AS alias`. A `col` is `field` or `alias~field`. A condition is the `read-table`
//! condition grammar over columns (`HAVING` also over aggregates).
//!
//! Refused, nothing sent: subqueries, `UNION`, `EXISTS`, `WITH`, `INTO`, `UP TO`, host variables,
//! path expressions, CDS parameters, comments, backquote literals, parenthesized joins, `RIGHT`
//! and `CROSS` joins, `USING CLIENT` and `CLIENT SPECIFIED`.
//!
//! This module holds no dictionary rule: [`Query::verify`] runs the entities through
//! `dictionary::check_entities` and only a [`VerifiedQuery`] exposes the body.

use crate::operation::dictionary::{CheckError, Dictionary, check_entities};
use crate::operation::read_table::{
    Operand, Parser, RESERVED, Scan, Token, field_name, identifier, parse_condition, tokenize_with,
};
use crate::policy::Policies;
use std::fmt::{self, Display, Write};

const LABEL: &str = "sql";
/// The longest statement a caller may give.
pub const TEXT_MAX: usize = 4000;
/// The longest canonical body: every token may gain a space, so twice the input limit.
pub const BODY_MAX: usize = 2 * TEXT_MAX;
/// The most entities in `FROM` and `JOIN` together.
pub const ENTITIES_MAX: usize = 10;
/// Words that are SQL syntax of this grammar and never a table, alias or field.
const SQL_RESERVED: [&str; 10] = [
    "DISTINCT",
    "INNER",
    "LEFT",
    "OUTER",
    "RIGHT",
    "CROSS",
    "FULL",
    "ASCENDING",
    "DESCENDING",
    "WITH",
];

/// A column: `field`, or `alias~field`. Names are upper case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Column {
    pub qualifier: Option<String>,
    pub name: String,
}

impl Display for Column {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.qualifier {
            Some(qualifier) => write!(formatter, "{qualifier}~{}", self.name),
            None => formatter.write_str(&self.name),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Function {
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

impl Function {
    fn from_word(word: &str) -> Option<Self> {
        match word.to_ascii_uppercase().as_str() {
            "COUNT" => Some(Self::Count),
            "SUM" => Some(Self::Sum),
            "MIN" => Some(Self::Min),
            "MAX" => Some(Self::Max),
            "AVG" => Some(Self::Avg),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Count => "COUNT",
            Self::Sum => "SUM",
            Self::Min => "MIN",
            Self::Max => "MAX",
            Self::Avg => "AVG",
        }
    }
}

/// An aggregate call: `COUNT( * )` has no argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Aggregate {
    pub function: Function,
    pub distinct: bool,
    pub argument: Option<Column>,
}

impl Display for Aggregate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}( ", self.function.name())?;

        if self.distinct {
            formatter.write_str("DISTINCT ")?;
        }

        match &self.argument {
            Some(column) => write!(formatter, "{column} )"),
            None => formatter.write_str("* )"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expression {
    Column(Column),
    Aggregate(Aggregate),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    pub expression: Expression,
    pub alias: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Columns {
    /// `SELECT *`
    All,
    Items(Vec<Item>),
}

/// An entity in `FROM` or `JOIN`, with its optional alias.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
    pub entity: String,
    pub alias: Option<String>,
}

impl Source {
    /// The name columns qualify with: the alias, else the entity.
    fn handle(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.entity)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinKind {
    Inner,
    LeftOuter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Join {
    pub kind: JoinKind,
    pub source: Source,
    /// `left = right` pairs, joined by `AND`.
    pub on: Vec<(Column, Column)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderKey {
    pub column: Column,
    pub descending: Option<bool>,
}

/// A parsed statement. Only the parser builds one, so every `Select` is a statement that passed
/// the grammar. Conditions are held as their canonical text: they came from the shared condition
/// grammar, which writes only checked tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Select {
    distinct: bool,
    columns: Columns,
    from: Source,
    joins: Vec<Join>,
    condition: Option<String>,
    group_by: Vec<Column>,
    having: Option<String>,
    order_by: Vec<OrderKey>,
}

impl Select {
    /// Parses user text, or names the first rule it breaks.
    pub fn parse(text: &str) -> Result<Self, String> {
        parse_select(text, TEXT_MAX)
    }

    /// The canonical single-line rendering: the only text that may be sent to SAP.
    pub fn body(&self) -> String {
        self.to_string()
    }

    /// The entities of `FROM` and `JOIN`, each once, in statement order.
    pub fn entities(&self) -> Vec<String> {
        let mut entities: Vec<String> = Vec::new();

        for source in std::iter::once(&self.from).chain(self.joins.iter().map(|join| &join.source))
        {
            if !entities.contains(&source.entity) {
                entities.push(source.entity.clone());
            }
        }

        entities
    }
}

fn write_list<T: Display>(out: &mut String, items: &[T]) {
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }

        let _ = write!(out, "{item}");
    }
}

impl Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.entity)?;

        match &self.alias {
            Some(alias) => write!(formatter, " AS {alias}"),
            None => Ok(()),
        }
    }
}

impl Display for Item {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.expression {
            Expression::Column(column) => write!(formatter, "{column}")?,
            Expression::Aggregate(aggregate) => write!(formatter, "{aggregate}")?,
        }

        match &self.alias {
            Some(alias) => write!(formatter, " AS {alias}"),
            None => Ok(()),
        }
    }
}

impl Display for OrderKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.column)?;

        match self.descending {
            Some(true) => formatter.write_str(" DESCENDING"),
            Some(false) => formatter.write_str(" ASCENDING"),
            None => Ok(()),
        }
    }
}

impl Display for Select {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::from("SELECT ");

        if self.distinct {
            out.push_str("DISTINCT ");
        }

        match &self.columns {
            Columns::All => out.push('*'),
            Columns::Items(items) => write_list(&mut out, items),
        }

        let _ = write!(out, " FROM {}", self.from);

        for join in &self.joins {
            let kind = match join.kind {
                JoinKind::Inner => "INNER",
                JoinKind::LeftOuter => "LEFT OUTER",
            };
            let _ = write!(out, " {kind} JOIN {} ON ", join.source);

            for (index, (left, right)) in join.on.iter().enumerate() {
                if index > 0 {
                    out.push_str(" AND ");
                }

                let _ = write!(out, "{left} = {right}");
            }
        }

        if let Some(condition) = &self.condition {
            let _ = write!(out, " WHERE {condition}");
        }

        if !self.group_by.is_empty() {
            out.push_str(" GROUP BY ");
            write_list(&mut out, &self.group_by);
        }

        if let Some(condition) = &self.having {
            let _ = write!(out, " HAVING {condition}");
        }

        if !self.order_by.is_empty() {
            out.push_str(" ORDER BY ");
            write_list(&mut out, &self.order_by);
        }

        formatter.write_str(&out)
    }
}

/// Re-parses a freestyle body and requires its exact canonical rendering. The freestyle
/// transport admission boundary is to call this without a deny list: policy belongs to the operation layer, while
/// every admitted statement must still come from this grammar.
pub fn validate_body(body: &str) -> Result<(), String> {
    let select = parse_select(body, BODY_MAX)
        .map_err(|error| format!("freestyle body refused: {error}; refusing to send it"))?;

    if select.body() != body {
        return Err("freestyle body is not the canonical rendering of its own statement: refusing to send it".to_owned());
    }

    Ok(())
}

/// A statement that passed the grammar and the direct deny list. Its body is not exposed: it
/// still has to pass the dictionary check of its entities ([`Query::verify`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    entities: Vec<String>,
    body: String,
    rows: u32,
}

impl Query {
    /// Validates the text, the row count, the canonical body length and every `FROM`/`JOIN`
    /// entity against the deny list. Any `Err` is a `STOP` and no connection is opened.
    pub fn new(text: &str, rows: u32, max_rows: u32, policies: &Policies) -> Result<Self, String> {
        let select = Select::parse(text)?;

        if rows == 0 || rows > max_rows {
            return Err(format!("rows must be between 1 and {max_rows}"));
        }

        let entities = select.entities();

        for entity in &entities {
            if policies.is_read_table_denied(entity) {
                return Err(format!("table {entity} is on the read_table_denied list"));
            }
        }

        let body = select.body();

        if body.len() > BODY_MAX {
            return Err(error(&format!(
                "the canonical statement is longer than {BODY_MAX} characters"
            )));
        }

        Ok(Self {
            entities,
            body,
            rows,
        })
    }

    /// Every entity of `FROM` and `JOIN`, each once.
    pub fn entities(&self) -> &[String] {
        &self.entities
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// Runs the dictionary check over every entity. Only the result exposes the body to send.
    pub fn verify<D: Dictionary>(
        self,
        dictionary: &mut D,
        policies: &Policies,
    ) -> Result<VerifiedQuery, CheckError<D::Error>> {
        check_entities(dictionary, &self.entities, policies)?;

        Ok(VerifiedQuery {
            entities: self.entities,
            body: self.body,
            rows: self.rows,
        })
    }
}

/// A [`Query`] whose entities the dictionary vouched for: the only holder of the body that may be
/// sent to the freestyle data preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedQuery {
    entities: Vec<String>,
    body: String,
    rows: u32,
}

impl VerifiedQuery {
    pub fn entities(&self) -> &[String] {
        &self.entities
    }

    /// The canonical body.
    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }
}

fn error(message: &str) -> String {
    format!("{LABEL}: {message}")
}

fn unknown(chars: &[char], index: usize) -> Result<Scan, String> {
    match chars[index] {
        '\t' | '\n' | '\r' => Ok(Scan::Skip(1)),
        '~' => Ok(Scan::Emit(Token::Operator("~"), 1)),
        '*' => Ok(Scan::Emit(Token::Operator("*"), 1)),
        '@' => Err(error("host variables (@) are not supported")),
        '\\' => Err(error("path expressions (\\_assoc) are not supported")),
        '`' => Err(error(
            "backquote literals are not supported (use a quoted literal)",
        )),
        '"' => Err(error("comments are not supported")),
        '.' => Err(error(
            "the statement terminator . is not allowed (give the statement without a period)",
        )),
        other => Err(error(&format!("character {other:?} is not allowed"))),
    }
}

/// A table, alias or field name: a DDIC identifier that is no SQL keyword.
fn name(what: &str, text: &str) -> Result<String, String> {
    let name = identifier(what, text).map_err(|message| error(&message))?;

    if RESERVED.contains(&name.as_str()) || SQL_RESERVED.contains(&name.as_str()) {
        return Err(error(&format!("{what} {text:?} is an SQL keyword")));
    }

    Ok(name)
}

fn column_name(text: &str) -> Result<String, String> {
    let name = field_name(text).map_err(|message| error(&message))?;

    if SQL_RESERVED.contains(&name.as_str()) {
        return Err(error(&format!("field {text:?} is an SQL keyword")));
    }

    Ok(name)
}

fn describe(token: &Token) -> String {
    match token {
        Token::Word(text) | Token::Number(text) => text.clone(),
        Token::Literal(_) => "a quoted literal".to_owned(),
        Token::Operator(operator) => (*operator).to_owned(),
        Token::Open => "(".to_owned(),
        Token::Close => ")".to_owned(),
        Token::Comma => ",".to_owned(),
    }
}

/// `field` or `alias~field` at `position`.
fn take_column(tokens: &[Token], position: &mut usize) -> Result<Column, String> {
    let Some(Token::Word(first)) = tokens.get(*position) else {
        return Err(error("expected a field name"));
    };

    if tokens.get(*position + 1) == Some(&Token::Operator("~")) {
        let qualifier = name("alias", first)?;

        let Some(Token::Word(field)) = tokens.get(*position + 2) else {
            return Err(error(&format!("expected a field name after {qualifier}~")));
        };
        let column = Column {
            qualifier: Some(qualifier),
            name: column_name(field)?,
        };

        *position += 3;

        return Ok(column);
    }

    let column = Column {
        qualifier: None,
        name: column_name(first)?,
    };

    *position += 1;

    Ok(column)
}

/// An aggregate call at `position`, or `None` when the tokens there are no call.
fn take_aggregate(tokens: &[Token], position: &mut usize) -> Result<Option<Aggregate>, String> {
    let Some(Token::Word(word)) = tokens.get(*position) else {
        return Ok(None);
    };
    let Some(function) = Function::from_word(word) else {
        return Ok(None);
    };

    if tokens.get(*position + 1) != Some(&Token::Open) {
        return Ok(None);
    }

    let mut next = *position + 2;
    let distinct = matches!(
        tokens.get(next),
        Some(Token::Word(word)) if word.eq_ignore_ascii_case("DISTINCT")
    );

    if distinct {
        next += 1;
    }

    let argument = if !distinct
        && function == Function::Count
        && tokens.get(next) == Some(&Token::Operator("*"))
    {
        next += 1;
        None
    } else {
        Some(take_column(tokens, &mut next)?)
    };

    if tokens.get(next) != Some(&Token::Close) {
        return Err(error(&format!(
            "expected ) to close {}( ... )",
            function.name()
        )));
    }

    *position = next + 1;

    Ok(Some(Aggregate {
        function,
        distinct,
        argument,
    }))
}

fn where_operand(parser: &mut Parser<'_>) -> Result<String, String> {
    let column = take_column(parser.tokens, &mut parser.position)?;

    parser.qualifiers.extend(column.qualifier.clone());

    Ok(column.to_string())
}

fn having_operand(parser: &mut Parser<'_>) -> Result<String, String> {
    match take_aggregate(parser.tokens, &mut parser.position)? {
        Some(aggregate) => {
            parser.qualifiers.extend(
                aggregate
                    .argument
                    .iter()
                    .filter_map(|column| column.qualifier.clone()),
            );

            Ok(aggregate.to_string())
        }
        None => where_operand(parser),
    }
}

/// The words and word pairs this grammar refuses wherever they stand, with the reason.
fn refuse_forbidden(tokens: &[Token]) -> Result<(), String> {
    let word = |index: usize| match tokens.get(index) {
        Some(Token::Word(text)) => text.to_ascii_uppercase(),
        _ => String::new(),
    };

    for index in 0..tokens.len() {
        let reason = match (word(index).as_str(), word(index + 1).as_str()) {
            ("SELECT", _) if index > 0 && word(0) == "SELECT" => {
                "subqueries (a second SELECT) are not supported"
            }
            ("UNION" | "INTERSECT" | "EXCEPT", _) => "UNION is not supported",
            ("EXISTS", _) => "EXISTS is not supported",
            ("WITH", _) => "WITH is not supported",
            ("INTO", _) => "INTO is not supported (the rows are returned as a table)",
            ("UP", "TO") => "UP TO is not supported (use --rows)",
            ("CLIENT", "SPECIFIED") => "CLIENT SPECIFIED is not supported",
            ("USING", "CLIENT") => "USING CLIENT is not supported",
            _ => continue,
        };

        return Err(error(reason));
    }

    Ok(())
}

struct Cursor {
    tokens: Vec<Token>,
    position: usize,
}

impl Cursor {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(found)) if found.eq_ignore_ascii_case(word))
    }

    fn take_keyword(&mut self, word: &str) -> bool {
        let found = self.keyword(word);

        if found {
            self.position += 1;
        }

        found
    }

    fn expect_keyword(&mut self, word: &str) -> Result<(), String> {
        if self.take_keyword(word) {
            Ok(())
        } else {
            Err(error(&format!("expected {word}")))
        }
    }

    fn take(&mut self, token: &Token) -> bool {
        let found = self.peek() == Some(token);

        if found {
            self.position += 1;
        }

        found
    }

    fn column(&mut self) -> Result<Column, String> {
        take_column(&self.tokens, &mut self.position)
    }

    fn alias(&mut self) -> Result<Option<String>, String> {
        if !self.take_keyword("AS") {
            return Ok(None);
        }

        let Some(Token::Word(alias)) = self.peek().cloned() else {
            return Err(error("expected an alias after AS"));
        };

        self.position += 1;

        name("alias", &alias).map(Some)
    }

    fn columns(&mut self) -> Result<Vec<Column>, String> {
        let mut columns = vec![self.column()?];

        while self.take(&Token::Comma) {
            columns.push(self.column()?);
        }

        Ok(columns)
    }

    fn condition(&mut self, operand: Operand) -> Result<(String, Vec<String>), String> {
        let condition = parse_condition(&self.tokens, self.position, LABEL, operand, false)?;

        self.position = condition.next;

        Ok((condition.text, condition.qualifiers))
    }

    fn item(&mut self) -> Result<Item, String> {
        let expression = match take_aggregate(&self.tokens, &mut self.position)? {
            Some(aggregate) => Expression::Aggregate(aggregate),
            None => Expression::Column(self.column()?),
        };

        Ok(Item {
            expression,
            alias: self.alias()?,
        })
    }

    fn source(&mut self) -> Result<Source, String> {
        if self.peek() == Some(&Token::Open) {
            return Err(error(
                "parenthesized joins are not supported (write the joins without parentheses)",
            ));
        }

        let Some(Token::Word(entity)) = self.peek().cloned() else {
            return Err(error("expected a table or view name"));
        };

        self.position += 1;

        let entity = name("table", &entity)?;

        if self.peek() == Some(&Token::Open) {
            return Err(error(&format!(
                "CDS parameters ( ... ) after {entity} are not supported"
            )));
        }

        Ok(Source {
            entity,
            alias: self.alias()?,
        })
    }

    fn join(&mut self) -> Result<Option<Join>, String> {
        let kind = if self.take_keyword("INNER") {
            self.expect_keyword("JOIN")?;
            JoinKind::Inner
        } else if self.take_keyword("LEFT") {
            self.take_keyword("OUTER");
            self.expect_keyword("JOIN")?;
            JoinKind::LeftOuter
        } else if self.take_keyword("JOIN") {
            JoinKind::Inner
        } else {
            for word in ["RIGHT", "CROSS", "FULL"] {
                if self.keyword(word) {
                    return Err(error(&format!(
                        "{word} JOIN is not supported (INNER and LEFT OUTER only)"
                    )));
                }
            }

            return Ok(None);
        };
        let source = self.source()?;

        self.expect_keyword("ON")?;

        if self.peek() == Some(&Token::Open) {
            return Err(error("parentheses in ON are not supported"));
        }

        let mut on = Vec::new();

        loop {
            let left = self.column()?;

            if !self.take(&Token::Operator("=")) {
                return Err(error("expected = in an ON condition (field = field)"));
            }

            on.push((left, self.column()?));

            if !self.take_keyword("AND") {
                break;
            }
        }

        Ok(Some(Join { kind, source, on }))
    }
}

fn parse_select(text: &str, limit: usize) -> Result<Select, String> {
    if text.len() > limit {
        return Err(error(&format!(
            "the statement is longer than {limit} characters"
        )));
    }

    if text.split(['\n', '\r']).any(|line| line.starts_with('*')) {
        return Err(error("comments are not supported (a line starting with *)"));
    }

    let tokens = tokenize_with(text, LABEL, unknown)?;

    if tokens.is_empty() {
        return Err(error("the statement is empty"));
    }

    refuse_forbidden(&tokens)?;

    let mut cursor = Cursor {
        tokens,
        position: 0,
    };
    let mut qualifiers: Vec<String> = Vec::new();

    cursor.expect_keyword("SELECT")?;

    let distinct = cursor.take_keyword("DISTINCT");
    let columns = if cursor.take(&Token::Operator("*")) {
        Columns::All
    } else {
        let mut items = vec![cursor.item()?];

        while cursor.take(&Token::Comma) {
            items.push(cursor.item()?);
        }

        for item in &items {
            match &item.expression {
                Expression::Column(column) => qualifiers.extend(column.qualifier.clone()),
                Expression::Aggregate(aggregate) => qualifiers.extend(
                    aggregate
                        .argument
                        .iter()
                        .filter_map(|column| column.qualifier.clone()),
                ),
            }
        }

        Columns::Items(items)
    };

    cursor.expect_keyword("FROM")?;

    let from = cursor.source()?;
    let mut joins = Vec::new();

    while let Some(join) = cursor.join()? {
        joins.push(join);

        if joins.len() + 1 > ENTITIES_MAX {
            return Err(error(&format!(
                "more than {ENTITIES_MAX} tables and views in one statement"
            )));
        }
    }

    for join in &joins {
        for (left, right) in &join.on {
            qualifiers.extend(left.qualifier.clone());
            qualifiers.extend(right.qualifier.clone());
        }
    }

    let condition = if cursor.take_keyword("WHERE") {
        let (text, used) = cursor.condition(where_operand)?;

        qualifiers.extend(used);

        Some(text)
    } else {
        None
    };
    let group_by = if cursor.take_keyword("GROUP") {
        cursor.expect_keyword("BY")?;
        cursor.columns()?
    } else {
        Vec::new()
    };
    let having = if cursor.take_keyword("HAVING") {
        let (text, used) = cursor.condition(having_operand)?;

        qualifiers.extend(used);

        Some(text)
    } else {
        None
    };
    let mut order_by = Vec::new();

    if cursor.take_keyword("ORDER") {
        cursor.expect_keyword("BY")?;

        loop {
            let column = cursor.column()?;
            let descending = if cursor.take_keyword("DESCENDING") {
                Some(true)
            } else if cursor.take_keyword("ASCENDING") {
                Some(false)
            } else {
                None
            };

            order_by.push(OrderKey { column, descending });

            if !cursor.take(&Token::Comma) {
                break;
            }
        }
    }

    if let Some(token) = cursor.peek() {
        return Err(error(&format!(
            "unexpected {} in the statement",
            describe(token)
        )));
    }

    qualifiers.extend(
        group_by
            .iter()
            .filter_map(|column| column.qualifier.clone()),
    );
    qualifiers.extend(
        order_by
            .iter()
            .filter_map(|key| key.column.qualifier.clone()),
    );

    let select = Select {
        distinct,
        columns,
        from,
        joins,
        condition,
        group_by,
        having,
        order_by,
    };

    check_handles(&select, &qualifiers)?;

    Ok(select)
}

/// Every source has its own handle, and every qualifier names one.
fn check_handles(select: &Select, qualifiers: &[String]) -> Result<(), String> {
    let mut handles: Vec<&str> = Vec::new();

    for source in std::iter::once(&select.from).chain(select.joins.iter().map(|join| &join.source))
    {
        if handles.contains(&source.handle()) {
            return Err(error(&format!(
                "{} is used twice (give each table or view its own alias with AS)",
                source.handle()
            )));
        }

        handles.push(source.handle());
    }

    for qualifier in qualifiers {
        if !handles.contains(&qualifier.as_str()) {
            return Err(error(&format!(
                "{qualifier}~ names no table, view or alias of this statement"
            )));
        }
    }

    Ok(())
}
