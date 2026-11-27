//! Fail-closed classification of the entities a data preview reads.
//!
//! An entity (`read-table` table, `sql` `FROM` and `JOIN` name) may be read only when the ABAP
//! dictionary vouches for it:
//!
//! - `DD02L` (active rows) gives a table class `TRANSP`, `CLUSTER` or `POOL`; or
//! - `DD02L` gives a database view, `DDLDEPENDENCY` does not list it as a generated CDS SQL view,
//!   and `DD26S` names base tables that are non-empty and all allowed in turn.
//!
//! Everything else is refused: CDS entities (not in `DD02L`, so unknown), generated CDS SQL
//! views, views without `DD26S` rows, unknown names and other table classes. The deny list
//! applies to every entity and base table.
//!
//! [`check_entities`] is pure: it only reads the rows a [`Dictionary`] returns. The data preview
//! implementation is [`TransportDictionary`].

use crate::backend::Failure;
use crate::operation::read_table::{ReadTableError, TableData, identifier, lookup, rejected};
use crate::policy::Policies;
use crate::transport::SadtTransport;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::{self, Debug, Display};

/// The most dictionary lookups one check makes: a connection stays far below the RFC limit of 36
/// requests, so the read that follows the check still fits.
pub const MAX_LOOKUPS: usize = 24;
/// The `DD26S` rows one lookup asks for. A reply this long is cut, so it is refused.
pub const BASE_ROWS: u32 = 500;
/// Levels of views below an entity that are followed: entity, base, base of a base.
const MAX_DEPTH: usize = 2;
const CLASS_ROWS: u32 = 10;
const GENERATED_ROWS: u32 = 10;

/// One active `DD02L` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassRow {
    pub name: String,
    /// `TABCLASS`: `TRANSP`, `CLUSTER`, `POOL`, `VIEW`, ...
    pub table_class: String,
    /// `VIEWCLASS`: `D` for a database view; `None` when the reply holds no such column.
    pub view_class: Option<String>,
}

/// One `DDLDEPENDENCY` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedViewRow {
    pub object_name: String,
    pub object_type: String,
}

/// One `DD26S` row: a base table of a view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseRow {
    pub view: String,
    pub table: String,
}

/// The dictionary reads the check needs. Each call asks about one name, in upper case.
pub trait Dictionary {
    type Error;

    /// Active `DD02L` rows of `name`.
    fn class_rows(&mut self, name: &str) -> Result<Vec<ClassRow>, Self::Error>;

    /// `DDLDEPENDENCY` rows whose `OBJECTNAME` is `name`.
    fn generated_view_rows(&mut self, name: &str) -> Result<Vec<GeneratedViewRow>, Self::Error>;

    /// Active `DD26S` rows of the view `view`, at most `BASE_ROWS`.
    fn base_rows(&mut self, view: &str) -> Result<Vec<BaseRow>, Self::Error>;
}

/// Why a check stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckError<E> {
    /// The entity is refused; the text names it.
    Refused(String),
    /// The dictionary could not be read.
    Lookup(E),
}

impl<E: Display> Display for CheckError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(message) => formatter.write_str(message),
            Self::Lookup(error) => write!(formatter, "{error}"),
        }
    }
}

impl<E: Debug + Display> Error for CheckError<E> {}

/// Why a data preview read stopped after its input passed the grammar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewError {
    /// The dictionary check refused an entity (deny list, view over a denied table, CDS entity,
    /// generated CDS SQL view, view without base tables, unknown name). A STOP.
    Refused(String),
    /// A dictionary lookup could not vouch for an entity: fail closed.
    Rejected(String),
    /// The backend failed; `failure` is the routing class.
    Failed { failure: Failure, message: String },
}

impl From<ReadTableError> for PreviewError {
    fn from(error: ReadTableError) -> Self {
        match error {
            ReadTableError::Rejected(message) => Self::Rejected(message),
            ReadTableError::Failed { failure, message } => Self::Failed { failure, message },
        }
    }
}

impl PreviewError {
    /// The error of a dictionary check; `outcome` says what the refusal kept from happening.
    pub(crate) fn from_check(error: CheckError<ReadTableError>, outcome: &str) -> Self {
        match error {
            CheckError::Refused(message) => Self::Refused(format!("{message}; {outcome}")),
            CheckError::Lookup(error) => error.into(),
        }
    }
}

/// Checks every entity (upper-case names) against the deny list and the dictionary.
pub fn check_entities<D: Dictionary>(
    dictionary: &mut D,
    entities: &[String],
    policies: &Policies,
) -> Result<(), CheckError<D::Error>> {
    let mut check = Check {
        dictionary,
        policies,
        done: HashSet::new(),
        active: Vec::new(),
        lookups: 0,
    };

    for entity in entities {
        check.entity(entity, None, 0)?;
    }

    Ok(())
}

struct Check<'a, D: Dictionary> {
    dictionary: &'a mut D,
    policies: &'a Policies,
    /// Entities that passed.
    done: HashSet<String>,
    /// The views whose base tables are being checked.
    active: Vec<String>,
    lookups: usize,
}

fn refused<E>(message: String) -> CheckError<E> {
    CheckError::Refused(message)
}

impl<D: Dictionary> Check<'_, D> {
    /// Counts one lookup against the budget.
    fn spend(&mut self) -> Result<(), CheckError<D::Error>> {
        self.lookups += 1;

        if self.lookups > MAX_LOOKUPS {
            return Err(refused(format!(
                "the dictionary check needs more than {MAX_LOOKUPS} lookups"
            )));
        }

        Ok(())
    }

    /// `parent` is the view that named `name` as a base table.
    fn entity(
        &mut self,
        name: &str,
        parent: Option<&str>,
        depth: usize,
    ) -> Result<(), CheckError<D::Error>> {
        if self.policies.is_read_table_denied(name) {
            return Err(refused(match parent {
                None => format!("table {name} is on the read_table_denied list"),
                Some(view) => format!("view {view} reads table {name} (read_table_denied)"),
            }));
        }

        if self.active.iter().any(|view| view == name) {
            return Err(refused(format!(
                "view {name} reads itself through its base tables"
            )));
        }

        if self.done.contains(name) {
            return Ok(());
        }

        self.spend()?;

        let rows = self
            .dictionary
            .class_rows(name)
            .map_err(CheckError::Lookup)?;
        let [row] = rows.as_slice() else {
            return Err(refused(if rows.is_empty() {
                format!("{name} is not in the ABAP dictionary as an active table or view")
            } else {
                format!("{name} resolves to more than one dictionary entry")
            }));
        };

        if row.name != name {
            return Err(refused(format!(
                "the dictionary lookup did not return {name}"
            )));
        }

        let outcome = match row.table_class.as_str() {
            "TRANSP" | "POOL" | "CLUSTER" => Ok(()),
            "VIEW" if row.view_class.as_deref() == Some("D") => self.view(name, depth),
            "VIEW" => Err(refused(format!(
                "{name} is not a database view (CDS/DDLS entities and other view types are not supported)"
            ))),
            other => Err(refused(format!(
                "{name} is neither a table nor a database view (table category {})",
                if other.is_empty() { "missing" } else { other }
            ))),
        };

        if outcome.is_ok() {
            self.done.insert(name.to_owned());
        }

        outcome
    }

    fn view(&mut self, view: &str, depth: usize) -> Result<(), CheckError<D::Error>> {
        if depth >= MAX_DEPTH {
            return Err(refused(format!(
                "view {view} is nested too deeply to check its base tables"
            )));
        }

        self.active.push(view.to_owned());

        let outcome = self.bases_of(view, depth);

        self.active.pop();

        outcome
    }

    fn bases_of(&mut self, view: &str, depth: usize) -> Result<(), CheckError<D::Error>> {
        self.spend()?;

        let generated = self
            .dictionary
            .generated_view_rows(view)
            .map_err(CheckError::Lookup)?;

        // The lookup asks for this view only, so any row refuses it, whatever it names or its
        // type: a reply the lookup did not expect never relaxes the check.
        if !generated.is_empty() {
            return Err(refused(format!(
                "{view} is a generated CDS SQL view (CDS entities are not supported)"
            )));
        }

        self.spend()?;

        let bases = self
            .dictionary
            .base_rows(view)
            .map_err(CheckError::Lookup)?;

        if bases.is_empty() || bases.len() >= BASE_ROWS as usize {
            return Err(refused(format!(
                "the base tables of {view} are missing or too many to check"
            )));
        }

        if bases
            .iter()
            .any(|row| row.view != view || row.table.is_empty())
        {
            return Err(refused(format!("the base tables of {view} are unreadable")));
        }

        // A cell that is no DDIC identifier never reaches the body of the next lookup.
        let tables = bases
            .iter()
            .map(|row| {
                identifier("base table", &row.table).map_err(|_| {
                    refused(format!(
                        "view {view} names a base table that is no DDIC identifier"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        for table in &tables {
            self.entity(table, Some(view), depth + 1)?;
        }

        Ok(())
    }
}

/// The dictionary read through data previews of a connection.
pub struct TransportDictionary<'a> {
    transport: &'a mut dyn SadtTransport,
}

impl<'a> TransportDictionary<'a> {
    pub fn new(transport: &'a mut dyn SadtTransport) -> Self {
        Self { transport }
    }

    fn rows(
        &mut self,
        entity: &str,
        body: &str,
        rows: u32,
        subject: &str,
    ) -> Result<TableData, ReadTableError> {
        lookup(&mut *self.transport, entity, body, rows, subject)
    }
}

/// The trimmed cell of `data` in column `name`, per row; `None` when the column is missing.
fn column_cells(data: &TableData, name: &str) -> Option<Vec<String>> {
    let index = data.column(name)?;

    Some(
        data.rows
            .iter()
            .map(|row| row[index].trim().to_owned())
            .collect(),
    )
}

fn unreadable(what: &str) -> ReadTableError {
    rejected(format!("cannot read {what}"))
}

impl Dictionary for TransportDictionary<'_> {
    type Error = ReadTableError;

    fn class_rows(&mut self, name: &str) -> Result<Vec<ClassRow>, ReadTableError> {
        let body = format!(
            "SELECT TABNAME, TABCLASS, VIEWCLASS FROM DD02L WHERE TABNAME = '{name}' AND AS4LOCAL = 'A'"
        );
        let data = self.rows("DD02L", &body, CLASS_ROWS, name)?;
        let (Some(names), Some(classes)) = (
            column_cells(&data, "TABNAME"),
            column_cells(&data, "TABCLASS"),
        ) else {
            return Err(unreadable(&format!("the dictionary entry of {name}")));
        };
        let view_classes = column_cells(&data, "VIEWCLASS");

        Ok(names
            .into_iter()
            .zip(classes)
            .enumerate()
            .map(|(index, (name, table_class))| ClassRow {
                name,
                table_class,
                view_class: view_classes.as_ref().map(|cells| cells[index].clone()),
            })
            .collect())
    }

    fn generated_view_rows(&mut self, name: &str) -> Result<Vec<GeneratedViewRow>, ReadTableError> {
        let body = format!(
            "SELECT DDLNAME, OBJECTNAME, OBJECTTYPE FROM DDLDEPENDENCY WHERE OBJECTNAME = '{name}' AND OBJECTTYPE = 'VIEW'"
        );
        let data = self.rows("DDLDEPENDENCY", &body, GENERATED_ROWS, name)?;
        let (Some(names), Some(types)) = (
            column_cells(&data, "OBJECTNAME"),
            column_cells(&data, "OBJECTTYPE"),
        ) else {
            return Err(unreadable(&format!("the CDS dependency list of {name}")));
        };

        Ok(names
            .into_iter()
            .zip(types)
            .map(|(object_name, object_type)| GeneratedViewRow {
                object_name,
                object_type,
            })
            .collect())
    }

    fn base_rows(&mut self, view: &str) -> Result<Vec<BaseRow>, ReadTableError> {
        let body = format!(
            "SELECT VIEWNAME, TABNAME, TABPOS FROM DD26S WHERE VIEWNAME = '{view}' AND AS4LOCAL = 'A'"
        );
        let data = self.rows("DD26S", &body, BASE_ROWS, view)?;
        let (Some(views), Some(tables)) = (
            column_cells(&data, "VIEWNAME"),
            column_cells(&data, "TABNAME"),
        ) else {
            return Err(unreadable(&format!("the base tables of {view}")));
        };

        Ok(views
            .into_iter()
            .zip(tables)
            .map(|(view, table)| BaseRow { view, table })
            .collect())
    }
}
