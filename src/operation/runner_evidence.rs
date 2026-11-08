//! Evidence of a runner operation: facts read from a NEW session through the ADT data preview of
//! a fixed list of tables, and the repository preflight that runs before the runner class exists.
//!
//! Every preview here names a table of [`FIXED_TABLES`] chosen by this module, with values that
//! passed a strict alphabet check. Nothing a user types reaches a table name, and the `read-table`
//! deny list is not involved. A run counts as confirmed only when its own proof AND this evidence
//! agree.

use crate::backend::Failure;
use crate::domain::Status;
use crate::operation::delete_adt::Opener;
use crate::operation::payload::Kind;
use crate::operation::read_table::{ReadTableError, fetch};
use crate::transport::{SadtTransport, TransportError};
use std::collections::BTreeMap;

/// The only tables a preview of this module reads, in the order of [`Table`].
pub const FIXED_TABLES: [&str; 9] = [
    "DD01L", "DD04L", "DD02L", "DD40L", "TADIR", "TSTC", "SEOCLASS", "T000", "TRNSPACE",
];
/// The most previews one operation sends, far below the 35 per connection of the RFC route.
pub const PREVIEWS_MAX: usize = 5;
/// Of those, the preflight sends at most this many; the evidence sends the rest.
pub const PREFLIGHT_PREVIEWS: usize = 2;

const NAME_MAX: usize = 30;

#[derive(Clone, Copy)]
enum Table {
    Dd01l,
    Dd04l,
    Dd02l,
    Dd40l,
    Tadir,
    Tstc,
    Seoclass,
    T000,
    Trnspace,
}

impl Table {
    fn name(self) -> &'static str {
        FIXED_TABLES[self as usize]
    }
}

/// One row of a preview: column name to trimmed value.
pub type Row = BTreeMap<String, String>;

/// What one preview saw: no row, a row, or why it could not tell.
pub type Probe = Result<Option<Row>, String>;

/// An upper-case DDIC name of at most 30 characters, as a literal for a preview condition.
fn literal(value: &str) -> Result<&str, String> {
    let valid = !value.is_empty()
        && value.len() <= NAME_MAX
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"_/".contains(&byte)
        });

    if valid {
        Ok(value)
    } else {
        Err(format!(
            "{value:?} is not an object name for an evidence preview"
        ))
    }
}

fn read(
    transport: &mut dyn SadtTransport,
    table: Table,
    fields: &str,
    condition: &str,
) -> Result<Option<Row>, ReadTableError> {
    let body = format!("SELECT {fields} FROM {} WHERE {condition}", table.name());
    let data = fetch(transport, table.name(), &body, 1)?;

    Ok(data.rows.first().map(|row| {
        data.columns
            .iter()
            .cloned()
            .zip(row.iter().map(|value| value.trim().to_owned()))
            .collect()
    }))
}

fn describe(table: Table, error: &ReadTableError) -> String {
    match error {
        ReadTableError::Rejected(message) | ReadTableError::Failed { message, .. } => {
            format!("{}: {message}", table.name())
        }
    }
}

/// A preview of the object `expected`: the row it returns must name that object, or the answer is
/// not trusted.
fn probe(
    transport: &mut dyn SadtTransport,
    table: Table,
    fields: &str,
    condition: &str,
    key: &str,
    expected: &str,
) -> Probe {
    match read(transport, table, fields, condition) {
        Err(error) => Err(describe(table, &error)),
        Ok(None) => Ok(None),
        Ok(Some(row))
            if row
                .get(key)
                .is_some_and(|got| got.eq_ignore_ascii_case(expected)) =>
        {
            Ok(Some(row))
        }
        Ok(Some(_)) => Err(format!("{} returned a different object", table.name())),
    }
}

/// The definition table, its key column, and the fields to read for a kind. `TRAN` has no
/// active/inactive column, and its row carries the report.
fn definition_of(kind: Kind) -> (Table, &'static str, &'static str) {
    match kind {
        Kind::Dtel => (Table::Dd04l, "ROLLNAME", "ROLLNAME"),
        Kind::Doma => (Table::Dd01l, "DOMNAME", "DOMNAME"),
        Kind::Tabl => (Table::Dd02l, "TABNAME", "TABNAME"),
        Kind::Ttyp => (Table::Dd40l, "TYPENAME", "TYPENAME"),
        Kind::Tran => (Table::Tstc, "TCODE", "TCODE, PGMNA"),
    }
}

/// What the new session saw for one object and for the runner class.
#[derive(Clone, Debug)]
pub struct Evidence {
    pub definition: Probe,
    pub directory: Probe,
    pub runner: Probe,
    /// `W:` lines for the stderr.
    pub notes: Vec<String>,
}

impl Evidence {
    fn unreadable(reason: &str) -> Self {
        let probe = || Err(reason.to_owned());

        Self {
            definition: probe(),
            directory: probe(),
            runner: probe(),
            notes: Vec::new(),
        }
    }
}

/// Reads the facts of `name` (definition table, TADIR row) and of the runner class from a new
/// session, three previews, and closes that session again. Nothing is sent when a name fails the
/// alphabet check or the session cannot be opened.
pub fn read_evidence(open: &mut Opener<'_>, kind: Kind, name: &str, runner: &str) -> Evidence {
    let (name, runner) = match (literal(name), literal(runner)) {
        (Ok(name), Ok(runner)) => (name, runner),
        (Err(reason), _) | (_, Err(reason)) => return Evidence::unreadable(&reason),
    };
    let mut session = match open() {
        Ok(session) => session,
        Err(error) => {
            return Evidence::unreadable(&format!("cannot open a new session: {error}"));
        }
    };
    let (table, key, fields) = definition_of(kind);
    let active = if kind == Kind::Tran {
        ""
    } else {
        " AND AS4LOCAL = 'A'"
    };
    let definition = probe(
        &mut *session,
        table,
        fields,
        &format!("{key} = '{name}'{active}"),
        key,
        name,
    );
    let directory = probe(
        &mut *session,
        Table::Tadir,
        "OBJ_NAME, DEVCLASS",
        &format!(
            "PGMID = 'R3TR' AND OBJECT = '{}' AND OBJ_NAME = '{name}'",
            kind.token()
        ),
        "OBJ_NAME",
        name,
    );
    let runner = probe(
        &mut *session,
        Table::Seoclass,
        "CLSNAME",
        &format!("CLSNAME = '{runner}'"),
        "CLSNAME",
        runner,
    );
    let mut notes = Vec::new();

    if let Err(error) = session.close() {
        notes.push(format!("W: session close failed: {error}"));
    }

    Evidence {
        definition,
        directory,
        runner,
        notes,
    }
}

/// Whether an inactive version of `name` exists (`AS4LOCAL = 'N'`), read from a new session that
/// is closed again. `TRAN` has no inactive version. One preview.
pub fn read_inactive(open: &mut Opener<'_>, kind: Kind, name: &str) -> Probe {
    let name = literal(name)?;

    if kind == Kind::Tran {
        return Ok(None);
    }

    let mut session = open().map_err(|error| format!("cannot open a new session: {error}"))?;
    let (table, key, _) = definition_of(kind);
    let found = probe(
        &mut *session,
        table,
        key,
        &format!("{key} = '{name}' AND AS4LOCAL = 'N'"),
        key,
        name,
    );

    // A failed close cannot change what the preview saw.
    let _ = session.close();

    found
}

/// What a finished run should have left behind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expect<'a> {
    /// The object exists and is active; the package and the `TRAN` report are compared when given.
    Present {
        package: Option<&'a str>,
        report: Option<&'a str>,
    },
    /// The object is gone from the definition table and from TADIR.
    Absent,
}

/// The verdict of the new session on one run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Proof {
    Matches,
    /// A fact differs from the expectation; the text names it.
    Mismatch(String),
    /// A preview could not be read: nothing is proven.
    Unreadable(String),
}

fn describe_row(row: &Row) -> String {
    row.iter()
        .map(|(column, value)| format!("{column} {value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Compares the evidence with the expectation. The runner class must be gone in both cases. A
/// definite difference outranks an unreadable preview.
pub fn judge(expect: &Expect<'_>, kind: Kind, evidence: &Evidence) -> Proof {
    let mut wrong = Vec::new();
    let mut unread = Vec::new();
    let present = matches!(expect, Expect::Present { .. });
    let (table, _, _) = definition_of(kind);
    let mut seen = |label: String, probe: &Probe, want: bool| match probe {
        Err(error) => {
            unread.push(format!("{label}: {error}"));

            None
        }
        Ok(None) => {
            if want {
                wrong.push(format!("{label} has no row"));
            }

            None
        }
        Ok(Some(row)) => {
            if !want {
                wrong.push(format!("{label} still has a row ({})", describe_row(row)));
            }

            Some(row.clone())
        }
    };
    let definition = seen(
        format!("definition {}", table.name()),
        &evidence.definition,
        present,
    );
    let directory = seen("TADIR".to_owned(), &evidence.directory, present);

    seen("SEOCLASS".to_owned(), &evidence.runner, false);

    if let Expect::Present { package, report } = expect {
        let differs = |want: &str, got: Option<&String>| {
            got.is_some_and(|got| !got.eq_ignore_ascii_case(want))
        };

        if let (Some(want), Some(row)) = (package, &directory)
            && differs(want, row.get("DEVCLASS"))
        {
            wrong.push(format!(
                "TADIR package {}, expected {want}",
                row["DEVCLASS"]
            ));
        }

        if let (Some(want), Some(row)) = (report, &definition)
            && differs(want, row.get("PGMNA"))
        {
            wrong.push(format!("TSTC PGMNA {}, expected {want}", row["PGMNA"]));
        }
    }

    if !wrong.is_empty() {
        Proof::Mismatch(wrong.join("; "))
    } else if !unread.is_empty() {
        Proof::Unreadable(unread.join("; "))
    } else {
        Proof::Matches
    }
}

/// The status rule: `Active` (the claim holds; a delete prints it as deleted) only when the run
/// reported success with an OK in-run proof AND the new session agrees; otherwise `Partial`.
pub fn confirm(run_reported_ok: bool, proof: &Proof) -> Status {
    if run_reported_ok && *proof == Proof::Matches {
        Status::Active
    } else {
        Status::Partial
    }
}

/// The ABAP interface a classrun handler implements; `doctor` reads it from the repository table
/// because the 7.40 discovery document is empty.
const CLASSRUN_INTERFACE: &str = "IF_OO_ADT_CLASSRUN";

/// Whether the classrun interface exists, from one preview of `SEOCLASS`. `Err` is the reason it
/// could not be read.
pub fn classrun_interface(transport: &mut dyn SadtTransport) -> Result<bool, String> {
    probe(
        transport,
        Table::Seoclass,
        "CLSNAME",
        &format!("CLSNAME = '{CLASSRUN_INTERFACE}'"),
        "CLSNAME",
        CLASSRUN_INTERFACE,
    )
    .map(|row| row.is_some())
}

/// What the repository preflight found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preflight {
    /// The `RUNNER: preflight ...` line.
    pub line: String,
    /// Set when repository changes are not allowed; the text after `(` in the STOP.
    pub refusal: Option<String>,
    /// `W:` lines for sources that could not be read.
    pub notes: Vec<String>,
}

/// One preflight source. A lost connection or a refused logon stops everything; any other failure
/// leaves the source unknown with a warning, because the preflight blocks only on facts that are
/// read and unambiguous.
fn read_soft(
    transport: &mut dyn SadtTransport,
    table: Table,
    fields: &str,
    condition: &str,
    notes: &mut Vec<String>,
) -> Result<Option<Row>, TransportError> {
    match read(transport, table, fields, condition) {
        Ok(row) => Ok(row),
        Err(ReadTableError::Failed {
            failure: Failure::Authorization,
            message,
        }) => Err(TransportError::Authorization(message)),
        Err(ReadTableError::Failed {
            failure: Failure::PreSendConnectivity,
            message,
        }) => Err(TransportError::PreSendConnectivity(message)),
        Err(error) => {
            notes.push(format!(
                "W: repository preflight could not read {}",
                describe(table, &error)
            ));

            Ok(None)
        }
    }
}

/// Reads `T000` (client role) and the customer namespace `/0CUST/` (`TRNSPACE`), two previews.
/// Only a customer namespace whose `EDITFLAG` is not `X` refuses; the client settings are printed.
pub fn preflight(
    transport: &mut dyn SadtTransport,
    client: &str,
) -> Result<Preflight, TransportError> {
    if client.len() != 3 || !client.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TransportError::Protocol(format!(
            "client {client:?} is not three digits"
        )));
    }

    let mut notes = Vec::new();
    let t000 = read_soft(
        transport,
        Table::T000,
        "CCCATEGORY, CCCORACTIV, CCNOCLIIND",
        &format!("MANDT = '{client}'"),
        &mut notes,
    )?;
    let space = read_soft(
        transport,
        Table::Trnspace,
        "EDITFLAG",
        "NAMESPACE = '/0CUST/'",
        &mut notes,
    )?;
    let value = |row: &Option<Row>, column: &str| match row.as_ref().and_then(|row| row.get(column))
    {
        Some(text) if text.is_empty() => "-".to_owned(),
        Some(text) => text.clone(),
        None => "?".to_owned(),
    };
    let flag = value(&space, "EDITFLAG");
    let refusal = (space.is_some() && flag != "X")
        .then(|| format!("customer namespace /0CUST/ is not modifiable; TRNSPACE EDITFLAG {flag}"));

    Ok(Preflight {
        line: format!(
            "RUNNER: preflight T000 CCCATEGORY={} CCCORACTIV={} CCNOCLIIND={} TRNSPACE /0CUST/ EDITFLAG={flag}",
            value(&t000, "CCCATEGORY"),
            value(&t000, "CCCORACTIV"),
            value(&t000, "CCNOCLIIND"),
        ),
        refusal,
        notes,
    })
}
