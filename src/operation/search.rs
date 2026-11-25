//! `search`: objects by name pattern through the ADT `quickSearch`.
//!
//! `*` is the only wildcard and a pattern without one names exactly one object. The pattern and
//! the type are checked before any connection, and the same pattern rule is the one the RFC SADT
//! admission applies to the query, so a request that leaves the process always carries a pattern
//! this module would have built.

use crate::adt::{ObjectRef, parse_search_refs};
use crate::backend::Failure;
use crate::domain::Outcome;
use crate::operation::quality::QualityError;
use crate::operation::text::squeeze;
use crate::transport::{Request, SadtTransport, TransportError};
use std::ops::RangeInclusive;

pub const SEARCH_URI: &str = "/sap/bc/adt/repository/informationsystem/search";
/// Results an unbounded `search` asks for.
pub const DEFAULT_MAX: u32 = 50;
/// The `maxResults` a search may ask for.
pub const MAX_RESULTS: RangeInclusive<u32> = 1..=1000;
/// The longest object name is a program name: 40 characters, a namespace included.
const PATTERN_MAX: usize = 40;
/// A namespace is at most 10 characters between its slashes.
const NAMESPACE_MAX: usize = 10;

/// The types `--type` knows, each with the ADT `objectType` that filters for it. `TABL` is the
/// transparent table; the type of every other token is its only ADT type.
pub const TYPES: [(&str, &str); 12] = [
    ("PROG", "PROG/P"),
    ("FUNC", "FUGR/FF"),
    ("FUGR", "FUGR/F"),
    ("CLAS", "CLAS/OC"),
    ("INTF", "INTF/OI"),
    ("DTEL", "DTEL/DE"),
    ("DOMA", "DOMA/DD"),
    ("TABL", "TABL/DT"),
    ("TTYP", "TTYP/DA"),
    ("TRAN", "TRAN/T"),
    ("DDLS", "DDLS/DF"),
    ("MSAG", "MSAG/N"),
];

/// The type tokens, in the order of [`TYPES`], for a message.
pub fn type_tokens() -> String {
    TYPES
        .iter()
        .map(|(token, _)| *token)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The ADT `objectType` of a `--type` token, in any case.
fn object_type_of(token: &str) -> Result<&'static str, String> {
    TYPES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(token))
        .map(|(_, adt)| *adt)
        .ok_or_else(|| format!("unknown type {token} (known: {})", type_tokens()))
}

/// The `objectType` query value: the ADT type with its slash encoded.
fn encoded(object_type: &str) -> String {
    object_type.replace('/', "%2F")
}

/// Whether `value` is the encoded `objectType` of one of [`TYPES`]; the one rule the RFC SADT
/// admission applies to that parameter.
pub fn is_encoded_type(value: &str) -> bool {
    TYPES.iter().any(|(_, adt)| encoded(adt) == value)
}

/// The pattern in its canonical form (upper case), or why it is not one: upper-case letters,
/// digits, `_` and `*`, at most 40 characters, and either no `/` or a namespace `/NS/` at the
/// start followed by a name. Nothing else is a pattern, so nothing needs encoding in a query.
pub fn validate_pattern(text: &str) -> Result<String, String> {
    let pattern = text.to_ascii_uppercase();

    if pattern.is_empty() {
        return Err("the pattern is empty".to_owned());
    }

    if pattern.len() > PATTERN_MAX {
        return Err(format!(
            "the pattern is longer than {PATTERN_MAX} characters"
        ));
    }

    if let Some(bad) = pattern
        .chars()
        .find(|character| !is_name_character(*character) && *character != '*' && *character != '/')
    {
        return Err(format!(
            "character {bad:?} is not allowed in a pattern (letters, digits, _ and * only)"
        ));
    }

    let slashes = pattern.matches('/').count();

    if slashes == 0 {
        return Ok(pattern);
    }

    let namespaced = pattern
        .strip_prefix('/')
        .and_then(|rest| rest.split_once('/'));

    match namespaced {
        Some((namespace, name))
            if slashes == 2
                && !namespace.is_empty()
                && namespace.len() <= NAMESPACE_MAX
                && namespace.chars().all(is_name_character)
                && !name.is_empty() =>
        {
            Ok(pattern)
        }
        _ => Err("a / is only allowed in a namespace at the start, as /NS/NAME".to_owned()),
    }
}

fn is_name_character(character: char) -> bool {
    character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
}

/// The search of one run: a pattern that passed [`validate_pattern`], the type filter, and the
/// number of results.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    pattern: String,
    object_type: Option<&'static str>,
    max: u32,
}

impl Query {
    /// Validates the three inputs; any `Err` is a `STOP` and no connection is opened.
    pub fn new(pattern: &str, type_token: Option<&str>, max: u32) -> Result<Self, String> {
        if !MAX_RESULTS.contains(&max) {
            return Err(format!(
                "max must be between {} and {}",
                MAX_RESULTS.start(),
                MAX_RESULTS.end()
            ));
        }

        Ok(Self {
            pattern: validate_pattern(pattern)?,
            object_type: type_token.map(object_type_of).transpose()?,
            max,
        })
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn max(&self) -> u32 {
        self.max
    }

    /// `search?operation=quickSearch&query=<pattern>&maxResults=<max>[&objectType=<type>]`, the
    /// one shape the RFC SADT admission lets through for this command.
    pub fn uri(&self) -> String {
        let mut uri = format!(
            "{SEARCH_URI}?operation=quickSearch&query={}&maxResults={}",
            self.pattern, self.max
        );

        if let Some(object_type) = self.object_type {
            uri.push_str("&objectType=");
            uri.push_str(&encoded(object_type));
        }

        uri
    }
}

/// Whether `name` matches `pattern`: `*` is any run of characters (also none), every other
/// character stands for itself, and a pattern without `*` is the whole name. Both are upper case.
fn pattern_matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == name;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, middle @ .., last] = parts.as_slice() else {
        return false;
    };

    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };

    for part in middle {
        match rest.find(part) {
            Some(index) => rest = &rest[index + part.len()..],
            None => return false,
        }
    }

    rest.ends_with(last)
}

/// One found object, the fields as the printed line shows them: whitespace inside a field (the
/// padded name of an authorization default, a wrapped description) is one space, so a line stays a
/// line, and every other dropped character (see `text::is_dropped`) is removed. A name may therefore
/// hold a space; the line is for reading and the fields are for programs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub kind: String,
    pub name: String,
    /// `None` where the reply names no package.
    pub package: Option<String>,
    pub description: String,
}

impl Entry {
    fn of(object: &ObjectRef) -> Self {
        Self {
            kind: squeeze(&object.kind),
            name: squeeze(&object.name),
            package: Some(squeeze(object.package.as_deref().unwrap_or_default()))
                .filter(|package| !package.is_empty()),
            description: squeeze(object.description.as_deref().unwrap_or_default()),
        }
    }

    /// `<TYPE> <NAME> package=<P> <description>`; `?` stands for a package the reply lacks.
    pub fn line(&self) -> String {
        format!(
            "{} {} package={} {}",
            self.kind,
            self.name,
            self.package.as_deref().unwrap_or("?"),
            self.description
        )
        .trim_end()
        .to_owned()
    }
}

/// What a `search` found: the objects and the warnings. The printed text and any other view are
/// made from these entries, so they cannot disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Found {
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

impl Found {
    /// One line per object, then `SEARCH: <n> objects`.
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = self.entries.iter().map(Entry::line).collect();

        lines.push(match self.entries.len() {
            1 => "SEARCH: 1 object".to_owned(),
            count => format!("SEARCH: {count} objects"),
        });

        lines.join("\n")
    }

    /// The outcome the command prints.
    pub fn outcome(&self) -> Outcome {
        Outcome {
            status: None,
            exit: 0,
            stdout: self.text(),
            stderr: self.warnings.join("\n"),
        }
    }
}

/// The `search` command: the objects found. A release that ignores the type filter answers
/// objects of other types; they are dropped here and the run says so.
pub fn list(transport: &mut dyn SadtTransport, query: &Query) -> Result<Found, QualityError> {
    let response = transport
        .request(Request::get(query.uri()))
        .map_err(|error| QualityError::from_transport(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(QualityError::from_transport(&error));
    }

    if response.status != 200 {
        let error = TransportError::Http {
            status: response.status,
            message: String::new(),
        };
        let hint = if response.status == 404 {
            ": this system does not offer the object search on this route"
        } else {
            ""
        };

        return Err(QualityError::Failed {
            failure: Failure::from(&error),
            message: format!("the object search answered HTTP {}{hint}", response.status),
        });
    }

    let mut objects =
        parse_search_refs(&String::from_utf8_lossy(&response.body)).map_err(|message| {
            QualityError::rejected(format!("cannot read the search reply: {message}"))
        })?;

    objects.retain(|object| !object.kind.trim().is_empty() && !object.name.trim().is_empty());

    let mut warnings = Vec::new();
    let mut left_out = |objects: &mut Vec<ObjectRef>, keep: &dyn Fn(&ObjectRef) -> bool, what| {
        let before = objects.len();

        objects.retain(|object| keep(object));

        if objects.len() < before {
            warnings.push(format!(
                "W: the reply held {what}; {} left out (the result may hold fewer than --max objects)",
                before - objects.len()
            ));
        }
    };

    if let Some(wanted) = query.object_type {
        left_out(
            &mut objects,
            &|object| object.kind == wanted,
            "objects of other types",
        );
    }

    left_out(
        &mut objects,
        &|object| pattern_matches(&query.pattern, &object.name.trim().to_ascii_uppercase()),
        "objects whose name does not match the pattern",
    );

    objects.truncate(query.max as usize);

    let before = objects.len();
    let mut entries: Vec<Entry> = objects.iter().map(Entry::of).collect();

    entries.retain(|entry| !entry.kind.is_empty() && !entry.name.is_empty());

    if entries.len() < before {
        let left_out = before - entries.len();

        warnings.push(format!(
            "W: {left_out} {} without a usable type or name {} left out",
            if left_out == 1 { "object" } else { "objects" },
            if left_out == 1 { "was" } else { "were" }
        ));
    }

    Ok(Found { entries, warnings })
}

/// The `search` command as the CLI prints it.
pub fn run(transport: &mut dyn SadtTransport, query: &Query) -> Result<Outcome, QualityError> {
    list(transport, query).map(|found| found.outcome())
}
