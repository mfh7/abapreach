use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Parser)]
#[command(name = "abapreach", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Read {
        kind: ObjectKind,
        name: String,
        out: PathBuf,

        #[arg(long)]
        force: bool,

        #[arg(long)]
        include: Option<IncludeKind>,

        #[arg(long)]
        fugr: Option<String>,

        #[arg(long)]
        lines: Option<LineRange>,

        /// The main program of a `PROG` include, as `INCLUDE <NAME>.` names it.
        #[arg(long)]
        main: Option<String>,

        #[arg(long)]
        backend: Option<Backend>,

        #[arg(long)]
        conn: String,
    },
    Deploy {
        kind: ObjectKind,
        name: String,
        file: PathBuf,

        #[arg(long)]
        simulate: bool,

        #[arg(long)]
        base: Option<String>,

        #[arg(long)]
        tr: Option<String>,

        #[arg(long)]
        backend: Option<Backend>,

        #[arg(long)]
        include: Option<IncludeKind>,

        #[arg(long)]
        create_include: bool,

        #[arg(long)]
        fugr: Option<String>,

        /// The main program of a `PROG` include, as `INCLUDE <NAME>.` names it.
        #[arg(long)]
        main: Option<String>,

        #[arg(long)]
        conn: String,
    },
    Create {
        kind: CreateKind,
        name: String,

        /// A `PROG` create only: the source a simulate deploy checks after `CREATE: ACTIVE`.
        file: Option<PathBuf>,

        #[arg(long)]
        text: String,

        #[arg(long)]
        package: Option<String>,

        #[arg(long)]
        fugr: Option<String>,

        /// A `PROG` create only: `1` for a program (the default) or `I` for an include.
        #[arg(long)]
        subc: Option<Subc>,

        #[arg(long)]
        tr: Option<String>,

        #[arg(long)]
        simulate: bool,

        #[arg(long)]
        backend: Option<Backend>,

        #[arg(long)]
        conn: String,
    },
    /// Reads rows of one table through the ADT data preview.
    ReadTable {
        table: String,

        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,

        #[arg(long = "where")]
        where_clause: Option<String>,

        /// 1 to 5000; the data preview of the 740 release stops there.
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=5000))]
        rows: u32,

        #[arg(long)]
        conn: String,
    },
    /// Deletes objects from a plan written by `--simulate`.
    Delete {
        #[arg(required = true, value_name = "TYPE:NAME")]
        targets: Vec<DeleteTarget>,

        #[arg(long)]
        simulate: bool,

        #[arg(long)]
        tr: Option<String>,

        #[arg(long)]
        plan: PathBuf,

        #[arg(long)]
        conn: String,
    },
    /// Serves the MCP tools on stdio.
    Mcp,
    Doctor {
        #[arg(long)]
        conn: String,
    },
}

/// The object kinds `delete` accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum DeleteKind {
    #[value(name = "PROG")]
    Prog,
    #[value(name = "FUGR")]
    Fugr,
    #[value(name = "FUNC")]
    Func,
    #[value(name = "CLAS")]
    Clas,
    #[value(name = "INTF")]
    Intf,
}

/// One `TYPE:NAME` argument of `delete`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteTarget {
    pub kind: DeleteKind,
    pub name: String,
}

impl FromStr for DeleteTarget {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let invalid =
            || format!("delete target must be TYPE:NAME (PROG|FUGR|FUNC|CLAS|INTF): {text:?}");
        let (kind, name) = text.split_once(':').ok_or_else(invalid)?;
        let kind = DeleteKind::from_str(kind, false).map_err(|_| invalid())?;

        if name.is_empty() {
            return Err(invalid());
        }

        Ok(Self {
            kind,
            name: name.to_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ObjectKind {
    #[value(name = "PROG")]
    Prog,
    #[value(name = "FUNC")]
    Func,
    #[value(name = "CLAS")]
    Clas,
    #[value(name = "INTF")]
    Intf,
}

/// The object kinds `create` accepts; DDIC, `MSAG`, `TEXT`, `DDLS` and `ENHO` are out of scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CreateKind {
    #[value(name = "PROG")]
    Prog,
    #[value(name = "FUGR")]
    Fugr,
    #[value(name = "FUNC")]
    Func,
    #[value(name = "CLAS")]
    Clas,
    #[value(name = "INTF")]
    Intf,
}

/// `--subc` of a `PROG` create: a program (`1`, the default) or an include (`I`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Subc {
    #[value(name = "1")]
    Program,
    #[value(name = "I")]
    Include,
}

/// A local include of a class: `CCIMP`, `CCDEF`, `CCMAC` or `CCAU`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum IncludeKind {
    #[value(name = "CCIMP")]
    Ccimp,
    #[value(name = "CCDEF")]
    Ccdef,
    #[value(name = "CCMAC")]
    Ccmac,
    #[value(name = "CCAU")]
    Ccau,
}

/// `--lines A-B`: one-based, inclusive, `A <= B`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

impl FromStr for LineRange {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let invalid = || format!("--lines must be A-B (one-based, inclusive, A <= B): {text:?}");
        let (start, end) = text.split_once('-').ok_or_else(invalid)?;
        let number = |digits: &str| {
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }

            digits.parse::<usize>().map_err(|_| invalid())
        };
        let (start, end) = (number(start)?, number(end)?);

        if start == 0 || start > end {
            return Err(invalid());
        }

        Ok(Self { start, end })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Backend {
    Adt,
    Rfc,
}
