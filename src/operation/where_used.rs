//! `where-used`: the objects that use one object.
//!
//! PROG, FUNC, FUGR, CLAS and INTF go through the ADT `usageReferences` search. A DDIC type
//! (DTEL, DOMA, TABL, TTYP) goes through it only when the discovery document of the release lists
//! the collection of the type; otherwise the standard cross-reference tables are read through the
//! internal data preview, the way `read-table` reads a table. The type and the name are checked
//! before any connection, and every data preview body is built from a validated identifier, so a
//! request that leaves the process is one of the exact shapes the RFC SADT admission knows.

use crate::adt::{
    USAGE_REQUEST_XML, UsingObject, discovery_collections, parse_usage_objects,
    usage_references_uri,
};
use crate::backend::Failure;
use crate::cli::{CreateKind, ObjectKind};
use crate::domain::Outcome;
use crate::operation::dictionary::{Dictionary, PreviewError, TransportDictionary, check_entities};
use crate::operation::read::resolve_group;
use crate::operation::read_table::{
    CLI_MAX_ROWS, Query as PreviewQuery, ReadTableRequest, TableData, fetch, identifier,
};
use crate::operation::search::TYPES;
use crate::operation::text::is_dropped;
use crate::policy::Policies;
use crate::transport::{Request, Response, SadtTransport, TransportError};
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::RangeInclusive;
use std::rc::Rc;

const DISCOVERY_URI: &str = "/sap/bc/adt/discovery";
/// Objects an unbounded `where-used` prints.
pub const DEFAULT_MAX: u32 = 200;
/// The `--max` a run may ask for.
pub const MAX_RESULTS: RangeInclusive<u32> = 1..=5000;
/// Names in one `IN` list. A name has up to 40 characters (a program name), so a quoted name with
/// its separator is 44 characters and a list of 40 is 1760: with the longest fixed part of a
/// condition (the `TADIR` one, about 110 characters) it stays below the 2000 characters a
/// condition may have.
const CHUNK: usize = 40;
/// The width the name of a class or interface pool is padded to in an include name.
const POOL_NAME_WIDTH: usize = 30;
/// The `TRDIR` reads that map includes to function groups; further includes stay programs.
const MAX_GROUP_READS: usize = 3;
/// The `WBCROSSI` reads that map includes to their master programs; further includes stay as
/// they are.
const MAX_MASTER_READS: usize = 3;
/// The largest where-used reply taken. `usageReferences` has no result limit (a structure such as
/// `BAPIRET2` answers 15 to 29 MB), and `--max` only cuts the printed list, so a reply beyond
/// this is refused instead of read.
///
/// The limit is checked after the read: the transports hold a whole reply in memory before the
/// operation sees it. A transport-wide limit before the read is a follow-up outside 0.18.
pub const MAX_REPLY_BYTES: usize = 64 << 20;
/// The requests one run may send over its connection: the discovery, the usage search or the
/// dictionary checks and reads of the cross-reference tables, all counted together. An RFC
/// connection stops at 36 requests.
pub const REQUEST_BUDGET: usize = 36;
const TABLES: &str = "/sap/bc/adt/ddic/tables";
const STRUCTURES: &str = "/sap/bc/adt/ddic/structures";
/// The `TADIR` reads that name the packages; further objects show `package=?`.
const MAX_PACKAGE_READS: usize = 6;
/// The rows of one package read: a name has one row per object type that carries it.
const PACKAGE_ROWS: u32 = 500;
/// The object types a cross-reference hit can have, and the `TADIR` types that hold them.
const PACKAGE_TYPES: &str = "'CLAS', 'DTEL', 'FUGR', 'INTF', 'PROG', 'TABL'";

/// The object types `where-used` knows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Prog,
    Func,
    Fugr,
    Clas,
    Intf,
    Dtel,
    Doma,
    Tabl,
    Ttyp,
}

const KINDS: [(&str, Kind); 9] = [
    ("PROG", Kind::Prog),
    ("FUNC", Kind::Func),
    ("FUGR", Kind::Fugr),
    ("CLAS", Kind::Clas),
    ("INTF", Kind::Intf),
    ("DTEL", Kind::Dtel),
    ("DOMA", Kind::Doma),
    ("TABL", Kind::Tabl),
    ("TTYP", Kind::Ttyp),
];

impl Kind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Prog => "PROG",
            Self::Func => "FUNC",
            Self::Fugr => "FUGR",
            Self::Clas => "CLAS",
            Self::Intf => "INTF",
            Self::Dtel => "DTEL",
            Self::Doma => "DOMA",
            Self::Tabl => "TABL",
            Self::Ttyp => "TTYP",
        }
    }

    fn max_name(self) -> usize {
        match self {
            Self::Prog => 40,
            _ => 30,
        }
    }

    /// The ADT collection of a DDIC type: the release offers where-used for the type only when its
    /// discovery document lists it. The other types are always served. A table type is a TABL
    /// either way; the run picks `tables` or `structures` by the table class.
    fn collection(self) -> Option<&'static str> {
        match self {
            Self::Dtel => Some("/sap/bc/adt/ddic/dataelements"),
            Self::Doma => Some("/sap/bc/adt/ddic/domains"),
            Self::Tabl => Some(TABLES),
            Self::Ttyp => Some("/sap/bc/adt/ddic/tabletypes"),
            _ => None,
        }
    }
}

/// The type tokens, in the order of [`KINDS`], for a message.
pub fn type_tokens() -> String {
    KINDS
        .iter()
        .map(|(token, _)| *token)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The where-used search of one run: a type and a name that passed every local rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    kind: Kind,
    name: String,
    max: u32,
}

impl Query {
    /// Validates the three inputs; any `Err` is a `STOP` and no connection is opened. An ADT type
    /// takes a plain name (letters, digits and `_`), because no other form has a verified ADT
    /// path; a DDIC type takes any DDIC identifier, a namespace included.
    pub fn new(type_token: &str, name: &str, max: u32) -> Result<Self, String> {
        if !MAX_RESULTS.contains(&max) {
            return Err(format!(
                "max must be between {} and {}",
                MAX_RESULTS.start(),
                MAX_RESULTS.end()
            ));
        }

        let kind = match type_token.to_ascii_uppercase().as_str() {
            "MSAG" => return Err("where-used of MSAG is not available in this version".to_owned()),
            "TRAN" => {
                return Err(
                    "where-used of TRAN is not supported: no ADT where-used path exists for a transaction"
                        .to_owned(),
                );
            }
            token => KINDS
                .iter()
                .find(|(known, _)| *known == token)
                .map(|(_, kind)| *kind)
                .ok_or_else(|| format!("unknown type {type_token} (known: {})", type_tokens()))?,
        };
        let name = name.to_ascii_uppercase();

        if kind.collection().is_some() {
            identifier("name", &name)?;
        } else {
            check_plain_name(kind, &name)?;
        }

        Ok(Self { kind, name, max })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn max(&self) -> u32 {
        self.max
    }

    /// The object URI the where-used request names. A FUNC needs its function group, and a DDIC
    /// type the collection the run chose.
    fn object_uri(&self, group: Option<&str>, collection: Option<&str>) -> Result<String, String> {
        match self.kind {
            Kind::Prog => ObjectKind::Prog.object_uri(&self.name, None),
            Kind::Func => ObjectKind::Func.object_uri(&self.name, group),
            Kind::Fugr => CreateKind::Fugr.object_uri(&self.name, None),
            Kind::Clas => ObjectKind::Clas.object_uri(&self.name, None),
            Kind::Intf => ObjectKind::Intf.object_uri(&self.name, None),
            Kind::Dtel | Kind::Doma | Kind::Tabl | Kind::Ttyp => {
                let collection = collection.ok_or("a DDIC type has a collection")?;

                Ok(format!("{collection}/{}", self.name.to_ascii_lowercase()))
            }
        }
    }
}

fn check_plain_name(kind: Kind, name: &str) -> Result<(), String> {
    let token = kind.token();

    if name.starts_with('/') {
        return Err(format!(
            "a namespaced {token} name has no verified where-used path yet: {name}"
        ));
    }

    if name.is_empty() || name.len() > kind.max_name() {
        return Err(format!(
            "a {token} name has 1 to {} characters",
            kind.max_name()
        ));
    }

    if let Some(bad) = name.chars().find(|character| {
        !(character.is_ascii_uppercase() || character.is_ascii_digit() || *character == '_')
    }) {
        return Err(format!(
            "character {bad:?} is not allowed in a {token} name (letters, digits and _ only)"
        ));
    }

    Ok(())
}

/// One printed line before its text: where a user was found.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Hit {
    kind: String,
    name: String,
    via: String,
    package: Option<String>,
}

impl Hit {
    fn new(kind: &str, name: String, via: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            name,
            via: via.to_owned(),
            package: None,
        }
    }
}

/// How the users of a run are found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    /// The ADT search; a DDIC type names the collection the run chose.
    Adt(Option<&'static str>),
    CrossReference,
}

/// What the ADT search answered.
enum Usage {
    Hits(Vec<Hit>),
    /// A DDIC type the system listed but does not serve: HTTP 404 or 500.
    Unavailable(u16),
}

/// The `where-used` command: one line per using object, then `WHERE-USED: <n>`. The lines are
/// sorted and each is printed once; `--max` cuts the list after sorting, with a warning.
pub fn run(
    transport: &mut dyn SadtTransport,
    query: &Query,
    policies: &Policies,
) -> Result<Outcome, PreviewError> {
    run_within(transport, query, policies, REQUEST_BUDGET)
}

/// The requests a run has sent and may still send: one count for the whole run, whichever step
/// sends them.
#[derive(Clone)]
struct Budget {
    used: Rc<Cell<usize>>,
    exceeded: Rc<Cell<bool>>,
    limit: usize,
}

impl Budget {
    fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used.get())
    }
}

/// The transport of a run: it counts every request and refuses the one past the budget, so a
/// step cannot spend what the others need.
struct Counted<'a> {
    inner: &'a mut dyn SadtTransport,
    budget: Budget,
}

impl SadtTransport for Counted<'_> {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if self.budget.remaining() == 0 {
            self.budget.exceeded.set(true);

            return Err(TransportError::Protocol(
                "the request budget of the run is spent".to_owned(),
            ));
        }

        self.budget.used.set(self.budget.used.get() + 1);

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }
}

/// [`run`] with another request budget. A run that needs a request past it is rejected, whatever
/// step asked, and prints nothing.
pub fn run_within(
    transport: &mut dyn SadtTransport,
    query: &Query,
    policies: &Policies,
    limit: usize,
) -> Result<Outcome, PreviewError> {
    let budget = Budget {
        used: Rc::new(Cell::new(0)),
        exceeded: Rc::new(Cell::new(false)),
        limit,
    };
    let mut counted = Counted {
        inner: transport,
        budget: budget.clone(),
    };
    let result = run_counted(&mut counted, query, policies, &budget);

    if budget.exceeded.get() {
        return Err(PreviewError::Rejected(format!(
            "where-used needs more than {limit} requests on one connection (the discovery, and the dictionary checks and reads of the cross-reference tables); nothing is printed, because the list would be incomplete"
        )));
    }

    result
}

fn run_counted(
    transport: &mut dyn SadtTransport,
    query: &Query,
    policies: &Policies,
    budget: &Budget,
) -> Result<Outcome, PreviewError> {
    let mut warnings = Vec::new();
    let mut route = route_of(transport, query, &mut warnings)?;
    let mut hits = None;

    if let Route::Adt(collection) = route {
        match adt_hits(transport, query, collection)? {
            Usage::Hits(found) => hits = Some(found),
            Usage::Unavailable(status) => {
                warnings.push(format!(
                    "W: the ADT where-used for {} answered HTTP {status}; using cross-reference tables",
                    query.kind.token()
                ));
                route = Route::CrossReference;
            }
        }
    }

    let hits = match hits {
        Some(hits) => hits,
        None => {
            let mut tables = Tables::new(transport, policies, &mut warnings, budget.clone());

            cross_reference_hits(&mut tables, query)?
        }
    };
    let mut hits = merged(hits);
    let found = hits.len();
    let max = usize::try_from(query.max).unwrap_or(usize::MAX);

    if found > max {
        hits.truncate(max);
        warnings.push(format!(
            "W: {found} objects found; the first {} are shown (--max)",
            query.max
        ));
    }

    if route == Route::CrossReference {
        let failure = {
            let mut tables = Tables::new(transport, policies, &mut warnings, budget.clone());

            fill_packages(&mut tables, &mut hits).err()
        };

        if let Some(error) = failure {
            warnings.push(format!(
                "W: the packages could not be read ({}); package=?",
                clean(&reason(&error))
            ));
        }
    }

    let mut lines: Vec<String> = hits.iter().map(line).collect();

    lines.push(format!("WHERE-USED: {}", hits.len()));

    Ok(Outcome {
        status: None,
        exit: 0,
        stdout: lines.join("\n"),
        stderr: warnings.join("\n"),
    })
}

/// The hits sorted, each (type, name, table) once; of repeats that differ only in the package, the
/// one that names a package is kept.
///
/// The fields are cleaned first, because the printed text is what makes two hits the same: `ZA`
/// and `Z` + a zero-width space + `A` print one line and count once. A hit left without a name is
/// dropped.
fn merged(hits: Vec<Hit>) -> Vec<Hit> {
    let mut hits: Vec<Hit> = hits
        .into_iter()
        .map(|hit| Hit {
            kind: clean(&hit.kind),
            name: clean(&hit.name),
            via: clean(&hit.via),
            package: hit
                .package
                .map(|package| clean(&package))
                .filter(|package| !package.is_empty()),
        })
        .filter(|hit| !hit.name.is_empty())
        .collect();

    hits.sort();

    let mut kept: Vec<Hit> = Vec::new();

    for hit in hits {
        match kept.last_mut() {
            Some(last) if last.kind == hit.kind && last.name == hit.name && last.via == hit.via => {
                if last.package.is_none() {
                    last.package = hit.package;
                }
            }
            _ => kept.push(hit),
        }
    }

    kept
}

/// A field of a printed line keeps neither a control character nor a Unicode format character,
/// and whitespace inside it becomes a single space, so a line stays a line.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_whitespace() || !is_dropped(*character))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn reason(error: &PreviewError) -> String {
    match error {
        PreviewError::Refused(message) | PreviewError::Rejected(message) => message.clone(),
        PreviewError::Failed { message, .. } => message.clone(),
    }
}

/// `USED-BY <TYPE> <NAME> package=<P> via=<ADT|table>`.
fn line(hit: &Hit) -> String {
    let package = clean(hit.package.as_deref().unwrap_or_default());
    let package = if package.is_empty() { "?" } else { &package };

    format!(
        "USED-BY {} {} package={package} via={}",
        clean(&hit.kind),
        clean(&hit.name),
        clean(&hit.via)
    )
}

fn failed(error: &TransportError) -> PreviewError {
    PreviewError::Failed {
        failure: Failure::from(error),
        message: error.to_string(),
    }
}

/// ADT for the types it always serves; for a DDIC type, ADT when the discovery document lists the
/// collection, and the cross-reference tables otherwise. A TABL takes `tables` for a transparent
/// table and `structures` for a structure, by one dictionary lookup. A namespaced DDIC name has no
/// verified ADT path, so it takes the tables too.
fn route_of(
    transport: &mut dyn SadtTransport,
    query: &Query,
    warnings: &mut Vec<String>,
) -> Result<Route, PreviewError> {
    let Some(first) = query.kind.collection() else {
        return Ok(Route::Adt(None));
    };
    let token = query.kind.token();

    if query.name.starts_with('/') {
        warnings.push(format!(
            "W: no ADT where-used path for a namespaced {token} name; using cross-reference tables"
        ));

        return Ok(Route::CrossReference);
    }

    let listed = discovery_hrefs(transport)?;
    let is_listed = |collection: &str| listed.iter().any(|href| href == collection);
    let collection = if query.kind == Kind::Tabl {
        if is_listed(TABLES) || is_listed(STRUCTURES) {
            table_collection(transport, &query.name)?.filter(|collection| is_listed(collection))
        } else {
            None
        }
    } else {
        Some(first).filter(|collection| is_listed(collection))
    };

    if collection.is_none() {
        warnings.push(format!(
            "W: no ADT where-used for {token} on this release; using cross-reference tables"
        ));
    }

    Ok(collection.map_or(Route::CrossReference, |collection| {
        Route::Adt(Some(collection))
    }))
}

/// The ADT collection of a table by its table class: `tables` for a transparent, pool or cluster
/// table, `structures` for a structure, and none for anything else.
fn table_collection(
    transport: &mut dyn SadtTransport,
    name: &str,
) -> Result<Option<&'static str>, PreviewError> {
    let rows = TransportDictionary::new(&mut *transport).class_rows(name)?;

    Ok(match rows.as_slice() {
        [row] => match row.table_class.as_str() {
            "TRANSP" | "POOL" | "CLUSTER" => Some(TABLES),
            "INTTAB" => Some(STRUCTURES),
            _ => None,
        },
        _ => None,
    })
}

/// The collections the discovery document lists, without a trailing slash. A reply other than
/// 200 (a release that serves no discovery on this route) lists nothing.
fn discovery_hrefs(transport: &mut dyn SadtTransport) -> Result<Vec<String>, PreviewError> {
    // No headers, like the `doctor` probe: the admission accepts this one request only.
    let response = transport
        .request(Request::get(DISCOVERY_URI))
        .map_err(|error| failed(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(failed(&error));
    }

    if response.status != 200 {
        return Ok(Vec::new());
    }

    let hrefs =
        discovery_collections(&String::from_utf8_lossy(&response.body)).map_err(|message| {
            PreviewError::Rejected(format!(
                "cannot read the ADT discovery: {}",
                clean(&message)
            ))
        })?;

    Ok(hrefs
        .iter()
        .map(|href| href.trim_end_matches('/').to_owned())
        .collect())
}

/// The users from the ADT `usageReferences` search. The request is a POST on the query, with the
/// exact request body of the where-used search.
fn adt_hits(
    transport: &mut dyn SadtTransport,
    query: &Query,
    collection: Option<&str>,
) -> Result<Usage, PreviewError> {
    let group = match query.kind {
        Kind::Func => Some(
            resolve_group(transport, &query.name, None).map_err(|error| {
                if error.stop {
                    PreviewError::Refused(error.message)
                } else {
                    PreviewError::Failed {
                        failure: error.failure,
                        message: error.message,
                    }
                }
            })?,
        ),
        _ => None,
    };
    let uri = query
        .object_uri(group.as_deref(), collection)
        .map_err(PreviewError::Refused)?;
    let mut request = Request::post(
        usage_references_uri(&uri),
        USAGE_REQUEST_XML.as_bytes().to_vec(),
    );

    for header in ["Content-Type", "Accept"] {
        request
            .headers
            .insert(header.to_owned(), "application/*".to_owned());
    }

    let response = transport.request(request).map_err(|error| failed(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(failed(&error));
    }

    if response.status != 200 {
        let status = response.status;

        if collection.is_some() && matches!(status, 404 | 500) {
            return Ok(Usage::Unavailable(status));
        }

        let error = TransportError::Http {
            status,
            message: String::new(),
        };
        let hint = if status == 404 {
            ": the system knows no such object or offers no where-used for it"
        } else {
            ""
        };

        return Err(PreviewError::Failed {
            failure: Failure::from(&error),
            message: format!("the where-used search answered HTTP {status}{hint}"),
        });
    }

    if response.body.len() > MAX_REPLY_BYTES {
        return Err(PreviewError::Rejected(format!(
            "the where-used reply is {} bytes, over the limit of {MAX_REPLY_BYTES} bytes; --max cannot help, because the system sends the whole list before it is cut",
            response.body.len()
        )));
    }

    let objects =
        parse_usage_objects(&String::from_utf8_lossy(&response.body)).map_err(|message| {
            PreviewError::Rejected(format!(
                "cannot read the where-used reply: {}",
                clean(&message)
            ))
        })?;

    Ok(Usage::Hits(objects.into_iter().map(adt_hit).collect()))
}

fn adt_hit(object: UsingObject) -> Hit {
    Hit {
        kind: token_of(&object.kind),
        name: object.name.trim().to_owned(),
        via: "ADT".to_owned(),
        package: object.package,
    }
}

/// The token of an ADT object type; a type with no token (`PROG/I`, `PINF/KI`) stays as ADT
/// names it.
fn token_of(adt_type: &str) -> String {
    if adt_type == "TABL/DS" {
        return "TABL".to_owned();
    }

    TYPES
        .iter()
        .find(|(_, adt)| *adt == adt_type)
        .map_or_else(|| adt_type.to_owned(), |(token, _)| (*token).to_owned())
}

/// The data preview reads of the cross-reference fallback. A table is checked against the
/// dictionary once, the first time it is read.
struct Tables<'a> {
    transport: &'a mut dyn SadtTransport,
    policies: &'a Policies,
    warnings: &'a mut Vec<String>,
    budget: Budget,
    checked: HashSet<String>,
}

impl<'a> Tables<'a> {
    fn new(
        transport: &'a mut dyn SadtTransport,
        policies: &'a Policies,
        warnings: &'a mut Vec<String>,
        budget: Budget,
    ) -> Self {
        Self {
            transport,
            policies,
            warnings,
            budget,
            checked: HashSet::new(),
        }
    }

    /// The rows of `table` that meet `condition`, which names only validated identifiers and
    /// literals the caller built. The body goes through the grammar of `read-table`.
    fn read(
        &mut self,
        table: &str,
        fields: &[&str],
        condition: &str,
        rows: u32,
    ) -> Result<TableData, PreviewError> {
        let request = ReadTableRequest {
            table: table.to_owned(),
            fields: fields.iter().map(|field| (*field).to_owned()).collect(),
            where_clause: Some(condition.to_owned()),
            rows,
        };
        let query = PreviewQuery::new(&request, CLI_MAX_ROWS, self.policies)
            .map_err(PreviewError::Refused)?;

        if !self.checked.contains(table) {
            check_entities(
                &mut TransportDictionary::new(&mut *self.transport),
                &[query.table().to_owned()],
                self.policies,
            )
            .map_err(|error| PreviewError::from_check(error, "nothing was read"))?;
            self.checked.insert(table.to_owned());
        }

        let mut data = fetch(&mut *self.transport, query.table(), query.body(), rows)?;

        // The 750 release answers one row more than asked.
        data.rows.truncate(rows as usize);

        if data.rows.len() >= rows as usize && rows == CLI_MAX_ROWS {
            self.warnings.push(format!(
                "W: {table} returned {rows} rows, the limit of one read; the list may be incomplete"
            ));
        }

        Ok(data)
    }

    /// The trimmed, non-empty values of one column.
    fn values(
        &mut self,
        table: &str,
        column: &str,
        condition: &str,
        rows: u32,
    ) -> Result<Vec<String>, PreviewError> {
        let data = self.read(table, &[column], condition, rows)?;
        let index = data.column(column).ok_or_else(|| {
            PreviewError::Rejected(format!("the {table} reply has no {column} column"))
        })?;

        Ok(data
            .rows
            .iter()
            .filter_map(|row| row.get(index))
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect())
    }
}

/// The users of a DDIC type from the standard cross-reference tables: `WBCROSSGT` with `OTYPE =
/// 'TY'`; for a data element also the fields of `DD03L` that name it, and for a domain also the
/// data elements of `DD04L` that name it, both active.
fn cross_reference_hits(tables: &mut Tables<'_>, query: &Query) -> Result<Vec<Hit>, PreviewError> {
    let name = &query.name;
    let includes = tables.values(
        "WBCROSSGT",
        "INCLUDE",
        &format!("OTYPE = 'TY' AND NAME = '{name}'"),
        CLI_MAX_ROWS,
    )?;
    let mut hits = main_objects(tables, includes)?;

    match query.kind {
        Kind::Dtel => hits.extend(
            tables
                .values(
                    "DD03L",
                    "TABNAME",
                    &format!("ROLLNAME = '{name}' AND AS4LOCAL = 'A'"),
                    CLI_MAX_ROWS,
                )?
                .into_iter()
                .map(|table| Hit::new("TABL", table, "DD03L")),
        ),
        Kind::Doma => hits.extend(
            tables
                .values(
                    "DD04L",
                    "ROLLNAME",
                    &format!("DOMNAME = '{name}' AND AS4LOCAL = 'A'"),
                    CLI_MAX_ROWS,
                )?
                .into_iter()
                .map(|element| Hit::new("DTEL", element, "DD04L")),
        ),
        _ => {}
    }

    Ok(hits)
}

/// The main object of each include: a class or interface pool include is its class or interface,
/// an include `L<FG>xxx` whose `SAPL<FG>` is in `TRDIR` is its function group, and any other
/// include is the master program `WBCROSSI` names for it, or itself when it names none.
fn main_objects(tables: &mut Tables<'_>, includes: Vec<String>) -> Result<Vec<Hit>, PreviewError> {
    let mut hits = Vec::new();
    let mut others = Vec::new();

    for include in includes {
        match pool_owner(&include) {
            Some((kind, name)) => hits.push(Hit::new(kind, name, "WBCROSSGT")),
            None => others.push(include),
        }
    }

    let candidates: BTreeSet<String> = others
        .iter()
        .filter_map(|include| group_of(include))
        .map(|(program, _)| program)
        .collect();
    let candidates: Vec<String> = candidates.into_iter().collect();
    let mut found: HashSet<String> = HashSet::new();

    for (index, chunk) in candidates.chunks(CHUNK).enumerate() {
        if index == MAX_GROUP_READS {
            tables.warnings.push(format!(
                "W: {} function group candidates were not looked up; their includes show as programs",
                candidates.len() - index * CHUNK
            ));

            break;
        }

        found.extend(tables.values(
            "TRDIR",
            "NAME",
            &format!("NAME IN ({})", in_list(chunk)),
            PACKAGE_ROWS,
        )?);
    }

    let mut programs = Vec::new();

    for include in others {
        match group_of(&include) {
            Some((program, group)) if found.contains(&program) => {
                hits.push(Hit::new("FUGR", group, "WBCROSSGT"));
            }
            _ => programs.push(include),
        }
    }

    let masters = masters_of(tables, &programs)?;

    for include in programs {
        match masters.get(&include) {
            Some(masters) => {
                for master in masters {
                    hits.push(match pool_owner(master) {
                        Some((kind, name)) => Hit::new(kind, name, "WBCROSSGT"),
                        None => Hit::new("PROG", master.clone(), "WBCROSSGT"),
                    });
                }
            }
            None => hits.push(Hit::new("PROG", include, "WBCROSSGT")),
        }
    }

    Ok(hits)
}

/// The master programs of includes, from `WBCROSSI`: one read per `CHUNK` names, at most
/// `MAX_MASTER_READS`. An include the reads do not name has no entry.
fn masters_of(
    tables: &mut Tables<'_>,
    includes: &[String],
) -> Result<HashMap<String, BTreeSet<String>>, PreviewError> {
    let names: BTreeSet<String> = includes
        .iter()
        .filter(|include| is_plain_name(include))
        .cloned()
        .collect();
    let names: Vec<String> = names.into_iter().collect();
    let mut masters: HashMap<String, BTreeSet<String>> = HashMap::new();

    for (index, chunk) in names.chunks(CHUNK).enumerate() {
        if index == MAX_MASTER_READS {
            tables.warnings.push(format!(
                "W: the master programs of {} includes were not looked up; they show as themselves",
                names.len() - index * CHUNK
            ));

            break;
        }

        let data = tables.read(
            "WBCROSSI",
            &["INCLUDE", "MASTER"],
            &format!("INCLUDE IN ({})", in_list(chunk)),
            CLI_MAX_ROWS,
        )?;
        let (Some(include), Some(master)) = (data.column("INCLUDE"), data.column("MASTER")) else {
            return Err(PreviewError::Rejected(
                "the WBCROSSI reply lacks INCLUDE or MASTER".to_owned(),
            ));
        };

        for row in &data.rows {
            let (Some(include), Some(master)) = (row.get(include), row.get(master)) else {
                continue;
            };
            let master = master.trim();

            if !master.is_empty() {
                masters
                    .entry(include.trim().to_owned())
                    .or_default()
                    .insert(master.to_owned());
            }
        }
    }

    Ok(masters)
}

/// The include suffixes of a class pool: the pool, its sections, the local classes and the
/// methods (`CM` and three characters).
const CLASS_SUFFIXES: [&str; 10] = [
    "CP", "CU", "CO", "CI", "CT", "CS", "CCDEF", "CCIMP", "CCMAC", "CCAU",
];
/// The include suffixes of an interface pool.
const INTERFACE_SUFFIXES: [&str; 2] = ["IP", "IU"];

/// The class or interface of a pool include. The name is the pool name padded with `=` to exactly
/// 30 characters, followed by a known pool suffix; anything else is not a pool include, so
/// `ZREPORT=BAD` stays a program. A pool whose name has all 30 characters has no padding, and its
/// includes are not recognized: they fall through to a program, as in the reference tool.
fn pool_owner(include: &str) -> Option<(&'static str, String)> {
    let head = include.get(..POOL_NAME_WIDTH)?;
    let suffix = &include[POOL_NAME_WIDTH..];
    let name = head.trim_end_matches('=');

    if name.len() == head.len() || !is_plain_name(name) {
        return None;
    }

    if INTERFACE_SUFFIXES.contains(&suffix) {
        return Some(("INTF", name.to_owned()));
    }

    let is_method = suffix.strip_prefix("CM").is_some_and(|index| {
        index.len() == 3
            && index
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
    });

    (CLASS_SUFFIXES.contains(&suffix) || is_method).then(|| ("CLAS", name.to_owned()))
}

/// The function pool program and the function group an include `L<FG>xxx` (or `/NS/L<FG>xxx`)
/// would belong to: `SAPL<FG>` and `<FG>`. Only a candidate: `TRDIR` decides. Only a plain name
/// is a candidate, which also keeps the slice below on character boundaries.
fn group_of(include: &str) -> Option<(String, String)> {
    if !is_plain_name(include) {
        return None;
    }

    let (space, local) = match include.strip_prefix('/') {
        Some(rest) => {
            let (space, local) = rest.split_once('/')?;

            (format!("/{space}/"), local)
        }
        None => (String::new(), include),
    };
    let group = local
        .strip_prefix('L')
        .filter(|group| group.len() > 3)
        .map(|group| &group[..group.len() - 3])?;
    let candidate = (format!("{space}SAPL{group}"), format!("{space}{group}"));

    (is_plain_name(&candidate.0) && is_plain_name(&candidate.1)).then_some(candidate)
}

/// A name that can sit in a literal of a read: upper-case letters, digits, `_` and a namespace.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.chars().all(|character| {
            character.is_ascii_uppercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '/'
        })
}

fn in_list(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The package of each hit, from `TADIR`: one read per `CHUNK` names, at most `MAX_PACKAGE_READS`.
/// A hit the reads do not name keeps no package and prints `package=?`.
fn fill_packages(tables: &mut Tables<'_>, hits: &mut [Hit]) -> Result<(), PreviewError> {
    let names: BTreeSet<String> = hits
        .iter()
        .filter(|hit| is_plain_name(&hit.name))
        .map(|hit| hit.name.clone())
        .collect();
    let names: Vec<String> = names.into_iter().collect();
    let mut packages: HashMap<(String, String), String> = HashMap::new();

    for (index, chunk) in names.chunks(CHUNK).enumerate() {
        if index == MAX_PACKAGE_READS {
            tables.warnings.push(format!(
                "W: the packages of {} objects were not looked up (package=?)",
                names.len() - index * CHUNK
            ));

            break;
        }

        // The packages are the last step and the list is complete without them, so they take
        // what the budget has left instead of ending the run.
        if tables.budget.remaining() < 1 + usize::from(!tables.checked.contains("TADIR")) {
            tables.warnings.push(format!(
                "W: the packages of {} objects were not looked up: the request limit of one connection is reached (package=?)",
                names.len() - index * CHUNK
            ));

            break;
        }

        let condition = format!(
            "PGMID = 'R3TR' AND OBJECT IN ({PACKAGE_TYPES}) AND OBJ_NAME IN ({})",
            in_list(chunk)
        );
        let data = tables.read(
            "TADIR",
            &["OBJECT", "OBJ_NAME", "DEVCLASS"],
            &condition,
            PACKAGE_ROWS,
        )?;
        let (Some(object), Some(name), Some(package)) = (
            data.column("OBJECT"),
            data.column("OBJ_NAME"),
            data.column("DEVCLASS"),
        ) else {
            return Err(PreviewError::Rejected(
                "the TADIR reply lacks OBJECT, OBJ_NAME or DEVCLASS".to_owned(),
            ));
        };

        for row in &data.rows {
            let (Some(object), Some(name), Some(package)) =
                (row.get(object), row.get(name), row.get(package))
            else {
                continue;
            };

            packages.insert(
                (object.trim().to_owned(), name.trim().to_owned()),
                package.trim().to_owned(),
            );
        }
    }

    for hit in hits {
        hit.package = packages
            .get(&(hit.kind.clone(), hit.name.clone()))
            .cloned()
            .filter(|package| !package.is_empty());
    }

    Ok(())
}
