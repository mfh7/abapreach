use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "abapreach")]
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Backend {
    Adt,
    Rfc,
}
