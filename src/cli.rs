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

        #[arg(long)]
        conn: String,
    },
    Create {
        kind: CreateKind,
        name: String,

        #[arg(long)]
        text: String,

        #[arg(long)]
        package: Option<String>,

        #[arg(long)]
        fugr: Option<String>,

        #[arg(long)]
        tr: Option<String>,

        #[arg(long)]
        simulate: bool,

        #[arg(long)]
        backend: Option<Backend>,

        #[arg(long)]
        conn: String,
    },
    Doctor {
        #[arg(long)]
        conn: String,
    },
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

/// The object kinds `create` accepts; PROG is not among them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CreateKind {
    #[value(name = "FUGR")]
    Fugr,
    #[value(name = "FUNC")]
    Func,
    #[value(name = "CLAS")]
    Clas,
    #[value(name = "INTF")]
    Intf,
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
