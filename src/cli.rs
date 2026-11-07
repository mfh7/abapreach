use crate::extension::{ExtensionKind, ExtensionOptions, Registry, Verbs};
use clap::builder::{EnumValueParser, PossibleValue, PossibleValuesParser, TypedValueParser};
use clap::{Arg, ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
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
        #[arg(value_parser = standard_type::<ObjectKind>())]
        kind: TypeArg<ObjectKind>,
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
        #[arg(value_parser = standard_type::<ObjectKind>())]
        kind: TypeArg<ObjectKind>,
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
        #[arg(value_parser = standard_type::<CreateKind>())]
        kind: TypeArg<CreateKind>,
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

/// The object kinds `delete` accepts: the standard ones and the types of the registered extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteKind {
    Prog,
    Fugr,
    Func,
    Clas,
    Intf,
    Extension(ExtensionKind),
}

impl DeleteKind {
    const STANDARD: [(&'static str, Self); 5] = [
        ("PROG", Self::Prog),
        ("FUGR", Self::Fugr),
        ("FUNC", Self::Func),
        ("CLAS", Self::Clas),
        ("INTF", Self::Intf),
    ];

    /// The kind a token names: a standard one, or one of `extensions`.
    pub fn parse(token: &str, extensions: &[ExtensionKind]) -> Option<Self> {
        Self::STANDARD
            .iter()
            .find(|(known, _)| *known == token)
            .map(|(_, kind)| *kind)
            .or_else(|| {
                extensions
                    .iter()
                    .find(|kind| kind.token() == token)
                    .map(|kind| Self::Extension(*kind))
            })
    }
}

/// One `TYPE:NAME` argument of `delete`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteTarget {
    pub kind: DeleteKind,
    pub name: String,
}

impl DeleteTarget {
    /// `TYPE:NAME` for a standard type or one of `extensions`.
    pub fn parse(text: &str, extensions: &[ExtensionKind]) -> Result<Self, String> {
        let invalid = || {
            let types = DeleteKind::STANDARD
                .iter()
                .map(|(token, _)| *token)
                .chain(extensions.iter().map(|kind| kind.token()))
                .collect::<Vec<_>>()
                .join("|");

            format!("delete target must be TYPE:NAME ({types}): {text:?}")
        };
        let (kind, name) = text.split_once(':').ok_or_else(invalid)?;
        let kind = DeleteKind::parse(kind, extensions).ok_or_else(invalid)?;

        if name.is_empty() {
            return Err(invalid());
        }

        Ok(Self {
            kind,
            name: name.to_owned(),
        })
    }
}

impl FromStr for DeleteTarget {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::parse(text, &[])
    }
}

/// The type argument of `read`, `deploy` and `create`: a standard type, or one of the registered
/// extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeArg<K> {
    Standard(K),
    Extension(ExtensionKind),
}

impl<K: PartialEq> PartialEq<K> for TypeArg<K> {
    fn eq(&self, other: &K) -> bool {
        matches!(self, Self::Standard(kind) if kind == other)
    }
}

fn standard_type<K: ValueEnum + Clone + Send + Sync + 'static>()
-> impl TypedValueParser<Value = TypeArg<K>> {
    EnumValueParser::<K>::new().map(TypeArg::Standard)
}

/// The standard types plus `extensions`, for a registered extension. The help of a type shows in
/// the long `--help`.
fn extended_type<K: ValueEnum + Clone + Send + Sync + 'static>(
    extensions: Vec<ExtensionKind>,
) -> impl TypedValueParser<Value = TypeArg<K>> {
    let values = K::value_variants()
        .iter()
        .filter_map(ValueEnum::to_possible_value)
        .chain(extensions.iter().map(|kind| {
            let value = PossibleValue::new(kind.token());

            match kind.spec().help {
                "" => value,
                help => value.help(help),
            }
        }))
        .collect::<Vec<_>>();

    PossibleValuesParser::new(values).map(move |token| {
        K::from_str(&token, false).map_or_else(
            |_| {
                TypeArg::Extension(
                    *extensions
                        .iter()
                        .find(|kind| kind.token() == token)
                        .expect("the parser admits only the listed tokens"),
                )
            },
            TypeArg::Standard,
        )
    })
}

/// The command line grammar of the process: the derived one, widened by the types and options of
/// the registered extension. Without an extension it is the derived grammar unchanged.
pub fn command(registry: &Registry) -> clap::Command {
    let mut command = Cli::command().version(registry.version_text());
    let kinds: Vec<ExtensionKind> = registry.kinds().collect();

    if kinds.is_empty() {
        return command;
    }

    let with = |keep: &dyn Fn(&ExtensionKind) -> bool| -> Vec<ExtensionKind> {
        kinds.iter().filter(|kind| keep(kind)).copied().collect()
    };
    let reads = with(&|kind| kind.spec().verbs.contains(Verbs::READ));
    let deploys = with(&|kind| kind.spec().deploy_refusal.is_some());
    let creates = with(&|kind| kind.spec().verbs.contains(Verbs::CREATE));
    let deletes = with(&|kind| kind.spec().verbs.contains(Verbs::DELETE));

    // `mut_arg` re-adds the argument at the end of the list, which would move it behind the other
    // positionals: the explicit indices keep the order of the derived grammar.
    let positional = |names: [&'static str; 3]| {
        move |sub: clap::Command| {
            names.into_iter().zip(1..).fold(sub, |sub, (name, index)| {
                sub.mut_arg(name, |arg| arg.index(index))
            })
        }
    };

    command = command
        .mut_subcommand("read", positional(["kind", "name", "out"]))
        .mut_subcommand("read", |sub| {
            sub.mut_arg("kind", |arg| {
                arg.value_parser(extended_type::<ObjectKind>(reads))
            })
        })
        .mut_subcommand("deploy", positional(["kind", "name", "file"]))
        .mut_subcommand("deploy", |sub| {
            sub.mut_arg("kind", |arg| {
                arg.value_parser(extended_type::<ObjectKind>(deploys))
            })
        })
        .mut_subcommand("create", positional(["kind", "name", "file"]))
        .mut_subcommand("create", |sub| {
            sub.mut_arg("kind", |arg| {
                arg.value_parser(extended_type::<CreateKind>(creates))
            })
        })
        .mut_subcommand("delete", |sub| {
            sub.mut_arg("targets", |arg| {
                arg.value_parser(move |text: &str| DeleteTarget::parse(text, &deletes))
            })
        });

    let mut seen: Vec<&'static str> = Vec::new();

    for kind in &kinds {
        for option in kind.spec().create_options {
            if seen.contains(&option.id) {
                continue;
            }

            seen.push(option.id);
            command = command.mut_subcommand("create", |sub| {
                sub.arg(
                    Arg::new(option.id)
                        .long(option.long)
                        .value_name(option.value_name)
                        .help(option.help),
                )
            });
        }
    }

    command
}

/// Parses the arguments against [`command`]; the second value holds the extension options given.
pub fn parse(
    args: Vec<String>,
    registry: &Registry,
) -> Result<(Cli, ExtensionOptions), clap::Error> {
    let mut command = command(registry);
    let mut matches = command.try_get_matches_from_mut(args)?;
    let options = extension_options(&matches, registry);
    let cli =
        Cli::from_arg_matches_mut(&mut matches).map_err(|error| error.format(&mut command))?;

    Ok((cli, options))
}

fn extension_options(matches: &ArgMatches, registry: &Registry) -> ExtensionOptions {
    let Some(create) = matches.subcommand_matches("create") else {
        return ExtensionOptions::new();
    };

    registry
        .kinds()
        .flat_map(|kind| kind.spec().create_options)
        .filter_map(|option| {
            create
                .try_get_one::<String>(option.id)
                .ok()
                .flatten()
                .map(|value| (option.id, value.clone()))
        })
        .collect()
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
