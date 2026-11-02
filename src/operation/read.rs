//! Reading the active source of one object: PROG, FUNC, CLAS (main or one local include) and
//! INTF. The read is descriptor-based: the object contract supplies the source URI and the
//! parent identity, so every kind follows the same overwrite, normalization, hash and
//! classification rules.

use crate::adt::{function_group_of, function_group_search_uri};
use crate::backend::Failure;
use crate::cli::{IncludeKind, LineRange, ObjectKind};
use crate::config::{Connection, tls_warning};
use crate::domain::{Outcome, Status};
use crate::hash::{normalize, source_hash};
use crate::object::check_group_match;
use crate::transport::{Request, Response, SadtTransport, TransportError};
use std::fs;
use std::path::Path;

const SEARCH_ACCEPT: &str = "application/*";

/// The class-state probe of a freshly created class: its source and metadata answer HTTP 500
/// until the first PUT, while `objectstructure` answers 200 in that state.
const OBJECT_STRUCTURE: &str = "objectstructure?version=active&withShortDescriptions=true";

/// A failed read with the routing class that decides whether a fallback is allowed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadError {
    pub failure: Failure,
    pub message: String,
    /// A local rule rejected the command: exit 2 with `STOP:`, not a failed read.
    pub stop: bool,
}

impl ReadError {
    fn validation(message: String) -> Self {
        Self {
            failure: Failure::Validation,
            message,
            stop: false,
        }
    }

    /// A rule of the object contract that only the server resolution can break, such as a
    /// `--fugr` that names another group than the module's.
    fn stop(message: String) -> Self {
        Self {
            stop: true,
            ..Self::validation(message)
        }
    }

    fn from_transport(error: &TransportError) -> Self {
        Self {
            failure: Failure::from(error),
            message: error.to_string(),
            stop: false,
        }
    }
}

/// What to read: the kind, its name, the function group of a FUNC, the class include, and the
/// optional line interval.
#[derive(Clone, Copy, Debug)]
pub struct ReadTarget<'a> {
    pub kind: ObjectKind,
    pub name: &'a str,
    pub group: Option<&'a str>,
    pub include: Option<IncludeKind>,
    pub lines: Option<LineRange>,
}

impl<'a> ReadTarget<'a> {
    pub fn new(kind: ObjectKind, name: &'a str) -> Self {
        Self {
            kind,
            name,
            group: None,
            include: None,
            lines: None,
        }
    }

    pub fn group(mut self, group: Option<&'a str>) -> Self {
        self.group = group;

        self
    }

    pub fn include(mut self, include: Option<IncludeKind>) -> Self {
        self.include = include;

        self
    }

    pub fn lines(mut self, lines: Option<LineRange>) -> Self {
        self.lines = lines;

        self
    }
}

/// `A-B`, one-based and inclusive, of the whole source. An interval past the end returns the
/// remainder, which may be empty: a partial file is never a deployable source.
pub fn project_lines(lines: &[String], range: Option<LineRange>) -> Vec<String> {
    let Some(range) = range else {
        return lines.to_vec();
    };
    let start = range.start - 1;

    if start >= lines.len() {
        return Vec::new();
    }

    let end = range.end.min(lines.len());

    lines[start..end].to_vec()
}

pub struct ReadProg;

impl ReadProg {
    /// A PROG read, with the message-only result the M1 callers use.
    pub fn run(
        transport: &mut dyn SadtTransport,
        name: &str,
        output: &Path,
        force: bool,
        connection: Option<&Connection>,
    ) -> Result<Outcome, String> {
        let target = ReadTarget::new(ObjectKind::Prog, name);

        Self::run_target_classified(transport, target, output, force, connection)
            .map_err(|error| error.message)
    }

    /// A PROG read with the routing class of a failure.
    pub fn run_classified(
        transport: &mut dyn SadtTransport,
        name: &str,
        output: &Path,
        force: bool,
        connection: Option<&Connection>,
    ) -> Result<Outcome, ReadError> {
        let target = ReadTarget::new(ObjectKind::Prog, name);

        Self::run_target_classified(transport, target, output, force, connection)
    }

    pub fn run_target(
        transport: &mut dyn SadtTransport,
        target: ReadTarget<'_>,
        output: &Path,
        force: bool,
        connection: Option<&Connection>,
    ) -> Result<Outcome, String> {
        Self::run_target_classified(transport, target, output, force, connection)
            .map_err(|error| error.message)
    }

    pub fn run_target_classified(
        transport: &mut dyn SadtTransport,
        target: ReadTarget<'_>,
        output: &Path,
        force: bool,
        connection: Option<&Connection>,
    ) -> Result<Outcome, ReadError> {
        // A partial file must never be deployed as the full source.
        let to_stdout = target.lines.is_some();

        if to_stdout && output != Path::new("-") {
            return Err(ReadError::validation(
                "--lines requires OUT to be '-' (stdout)".to_owned(),
            ));
        }

        if !to_stdout && output.exists() && !force {
            return Err(ReadError::validation(format!(
                "{} already exists; use --force to overwrite",
                output.display()
            )));
        }

        let name = target.name;
        let group = match target.kind {
            ObjectKind::Func => Some(resolve_group(transport, name, target.group)?),
            _ => None,
        };
        let uri = target
            .kind
            .source_uri(name, group.as_deref(), target.include)
            .map_err(ReadError::validation)?;
        let response = read_source(transport, &uri, target.kind, name, target.include)?;
        let lines = normalize_of(response, target.kind, name, target.include)?;

        let projected = project_lines(&lines, target.lines);

        if !to_stdout {
            write_source(output, &projected)?;
        }

        let mut report = match target.lines {
            Some(range) => format!(
                "{} lines -> - (lines {}-{})",
                projected.len(),
                range.start,
                range.end
            ),
            None => format!("{} lines -> {}", projected.len(), output.display()),
        };

        report.push_str(&format!("\nhash {}", source_hash(&lines)));

        // The partial source is printed instead of written: a projected file could be deployed
        // as if it were the whole source.
        let mut stdout = if to_stdout {
            let mut block = projected.join("\n");

            if !block.is_empty() {
                block.push('\n');
            }

            report.insert_str(0, &block);
            report
        } else {
            report
        };

        if let Some(warning) = connection.and_then(tls_warning) {
            stdout = format!("{warning}\n{stdout}");
        }

        Ok(Outcome {
            status: Some(Status::Active),
            exit: 0,
            stdout,
            stderr: String::new(),
        })
    }
}

/// The function group of a FUNC: `--fugr` when it was supplied, otherwise the server resolution.
/// A supplied group that differs from the resolved one is a `STOP`, never a fallback.
pub(crate) fn resolve_group(
    transport: &mut dyn SadtTransport,
    name: &str,
    supplied: Option<&str>,
) -> Result<String, ReadError> {
    let mut request = Request::get(function_group_search_uri(name));
    request
        .headers
        .insert("Accept".to_owned(), SEARCH_ACCEPT.to_owned());
    let response = transport
        .request(request)
        .map_err(|error| ReadError::from_transport(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(ReadError::from_transport(&error));
    }

    if response.status != 200 {
        return Err(ReadError::validation(format!(
            "cannot resolve the function group of {name}: HTTP {}",
            response.status
        )));
    }

    let xml = String::from_utf8(response.body)
        .map_err(|error| ReadError::validation(format!("search response is not UTF-8: {error}")))?;
    let resolved = function_group_of(&xml, name)
        .map_err(|error| {
            ReadError::validation(format!("cannot parse the search response: {error}"))
        })?
        .ok_or_else(|| {
            ReadError::validation(format!(
                "cannot resolve the function group of {name}: the search returned no reference"
            ))
        })?;

    check_group_match(supplied, &resolved).map_err(ReadError::stop)?;

    Ok(resolved)
}

/// `GET <source>?version=active`. A fresh class answers 500 for its source and metadata; that is
/// an unreadable created state, not an absent object.
fn read_source(
    transport: &mut dyn SadtTransport,
    uri: &str,
    kind: ObjectKind,
    name: &str,
    include: Option<IncludeKind>,
) -> Result<Response, ReadError> {
    let mut request = Request::get(format!("{uri}?version=active"));
    request
        .headers
        .insert("Accept".to_owned(), "text/plain".to_owned());
    let response = transport
        .request(request)
        .map_err(|error| ReadError::from_transport(&error))?;

    // The logon was refused, not the read: report it the way the RFC backend does, never as a
    // stop and never with another attempt.
    if let Some(error) = response.authorization_failure() {
        return Err(ReadError::from_transport(&error));
    }

    if response.status == 200 {
        return Ok(response);
    }

    if kind == ObjectKind::Clas && response.status == 500 {
        return Err(unreadable_class(transport, name, include, response.status));
    }

    let error = TransportError::Http {
        status: response.status,
        message: String::new(),
    };

    Err(ReadError {
        failure: Failure::from(&error),
        message: format!(
            "read {} {name}{}: HTTP {}",
            kind.token(),
            include_suffix(include),
            response.status
        ),
        stop: false,
    })
}

/// A class whose source cannot be read: probe its structure, which answers 200 for a class that
/// exists but has no readable source yet. The probe is read-only and decides nothing else.
fn unreadable_class(
    transport: &mut dyn SadtTransport,
    name: &str,
    include: Option<IncludeKind>,
    status: u16,
) -> ReadError {
    let unreadable = format!(
        "read CLAS {name}{}: source unavailable (HTTP {status})",
        include_suffix(include)
    );
    let Ok(uri) = ObjectKind::Clas.object_uri(name, None) else {
        return ReadError::validation(unreadable);
    };
    let outcome = transport.request(Request::get(format!("{uri}/{OBJECT_STRUCTURE}")));

    match outcome {
        Err(error) => ReadError::from_transport(&error),
        Ok(response) => match response.authorization_failure() {
            Some(error) => ReadError::from_transport(&error),
            None if response.status == 200 => ReadError::validation(format!(
                "{unreadable}; the class exists but has no readable source yet (structure read: 200)"
            )),
            None => ReadError::validation(format!(
                "read CLAS {name}: object not found (source {status}, structure {})",
                response.status
            )),
        },
    }
}

fn include_suffix(include: Option<IncludeKind>) -> String {
    include.map_or_else(String::new, |include| format!(" {}", include.token()))
}

fn normalize_of(
    response: Response,
    kind: ObjectKind,
    name: &str,
    include: Option<IncludeKind>,
) -> Result<Vec<String>, ReadError> {
    let source = String::from_utf8(response.body).map_err(|error| {
        ReadError::validation(format!(
            "read {} {name}{}: response is not UTF-8: {error}",
            kind.token(),
            include_suffix(include)
        ))
    })?;

    Ok(normalize(source.split('\n').map(str::to_owned).collect()))
}

fn write_source(output: &Path, lines: &[String]) -> Result<(), ReadError> {
    let mut written = lines.join("\n");

    if !written.is_empty() {
        written.push('\n');
    }

    fs::write(output, written).map_err(|error| {
        ReadError::validation(format!("cannot write {}: {error}", output.display()))
    })
}
