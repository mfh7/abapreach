//! `aunit`: runs the ABAP Unit tests of a program, a class or a function group, or of every such
//! object of a package or a transport request, through `POST /sap/bc/adt/abapunit/testruns`.
//!
//! Only harmless tests run, of every duration. The request is the classic `application/xml` run
//! configuration. The reply is read tolerantly: unknown elements are ignored. Test code runs on the
//! server, so the command refuses a productive client like every command that runs code.

use crate::adt::{prog_uri, reference_text};
use crate::domain::{Outcome, Status};
use crate::hash::validate_name_within;
use crate::object::check_request;
use crate::operation::quality::{QualityError, check_client};
use crate::operation::read_table::{Query, ReadTableRequest, TableData, fetch};
use crate::policy::Policies;
use crate::transport::{Request, SadtTransport};
use clap::ValueEnum;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::fmt;

const TESTRUNS_URI: &str = "/sap/bc/adt/abapunit/testruns";
const GROUPS_URI: &str = "/sap/bc/adt/functions/groups/";
/// The existence read only needs the status. SAP_BASIS 758 answers 406 to `application/xml` on an
/// object resource, so the server picks its own representation.
const EXISTS_ACCEPT: &str = "*/*";
/// The most objects a `--package` or `--tr` batch runs.
pub const MAX_BATCH: usize = 100;
const TADIR_ROWS: u32 = 5000;
const TRDIR_CHUNK: usize = 30;
const TASK_LIMIT: usize = 50;
const TEXT_LIMIT: usize = 400;

/// The object types `aunit` runs, in the alphabetical order a batch is sorted by.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
pub enum AunitKind {
    #[value(name = "CLAS")]
    Clas,
    #[value(name = "FUGR")]
    Fugr,
    #[value(name = "PROG")]
    Prog,
}

impl AunitKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Prog => "PROG",
            Self::Clas => "CLAS",
            Self::Fugr => "FUGR",
        }
    }

    fn max_name(self) -> usize {
        match self {
            Self::Prog => 40,
            Self::Clas => 30,
            Self::Fugr => 26,
        }
    }
}

/// One object to test.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AunitTarget {
    pub kind: AunitKind,
    /// Upper case.
    pub name: String,
}

impl AunitTarget {
    /// The local rules: a plain Z or Y name of the type's length.
    pub fn new(kind: AunitKind, name: &str) -> Result<Self, String> {
        let name = name.to_ascii_uppercase();

        validate_name_within(&name, kind.max_name())?;

        if !name.starts_with(['Z', 'Y']) || name.contains('/') {
            return Err(format!("only plain Z and Y objects are tested: {name}"));
        }

        Ok(Self { kind, name })
    }

    /// The ADT URI of the object.
    pub fn uri(&self) -> String {
        match self.kind {
            AunitKind::Prog => prog_uri(&self.name),
            AunitKind::Clas => format!("/sap/bc/adt/oo/classes/{}", self.name.to_ascii_lowercase()),
            AunitKind::Fugr => format!("{GROUPS_URI}{}", self.name.to_ascii_lowercase()),
        }
    }
}

impl fmt::Display for AunitTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.kind.token(), self.name)
    }
}

/// What to run: one object, or every object of a package or a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    Object(AunitTarget),
    Package(String),
    Request(String),
}

impl Scope {
    /// The scope the arguments name, or the rule they break. Exactly one of the three forms.
    pub fn parse(
        object: Option<(AunitKind, &str)>,
        package: Option<&str>,
        request: Option<&str>,
    ) -> Result<Self, String> {
        match (object, package, request) {
            (Some((kind, name)), None, None) => Ok(Self::Object(AunitTarget::new(kind, name)?)),
            (None, Some(package), None) => {
                let package = package.to_ascii_uppercase();

                if package.is_empty()
                    || package.len() > 30
                    || !package
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_/$".contains(&byte))
                {
                    return Err(format!("invalid --package: {package}"));
                }

                Ok(Self::Package(package))
            }
            (None, None, Some(request)) => {
                check_request(request)?;

                Ok(Self::Request(request.to_ascii_uppercase()))
            }
            (None, None, None) => {
                Err("name an object (PROG|CLAS|FUGR NAME), --package or --tr".to_owned())
            }
            _ => Err("TYPE NAME, --package and --tr exclude each other".to_owned()),
        }
    }
}

/// The run configuration of one object: harmless tests only, every duration, no coverage.
pub fn run_configuration(uri: &str) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<aunit:runConfiguration xmlns:aunit="http://www.sap.com/adt/aunit">"#,
            r#"<external><coverage active="false"/></external>"#,
            r#"<options><uriType value="semantic"/>"#,
            r#"<testDeterminationStrategy sameProgram="true" assignedTests="false" appendAssignedTestsPreview="true"/>"#,
            r#"<testRiskLevels harmless="true" dangerous="false" critical="false"/>"#,
            r#"<testDurations short="true" medium="true" long="true"/></options>"#,
            r#"<adtcore:objectSets xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<objectSet kind="inclusive"><adtcore:objectReferences>"#,
            r#"<adtcore:objectReference adtcore:uri="{uri}"/>"#,
            r#"</adtcore:objectReferences></objectSet></adtcore:objectSets>"#,
            r#"</aunit:runConfiguration>"#,
        ),
        uri = quick_xml::escape::escape(uri)
    )
}

/// The test-run request of one object.
pub fn run_request(target: &AunitTarget) -> Request {
    let mut request = Request::post(TESTRUNS_URI, run_configuration(&target.uri()).into_bytes());

    request
        .headers
        .insert("Content-Type".to_owned(), "application/xml".to_owned());
    request
        .headers
        .insert("Accept".to_owned(), "application/xml".to_owned());

    request
}

/// One alert of a run: a failed assertion, an exception, a warning.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Alert {
    pub kind: String,
    pub severity: String,
    pub program: String,
    pub class: String,
    pub method: String,
    pub text: String,
}

impl Alert {
    /// `<kind> <level> <prog> <class>-><method>: <text>`; a class-level alert has no method and a
    /// program-level one no class.
    pub fn line(&self) -> String {
        let place = match (self.class.as_str(), self.method.as_str()) {
            ("", _) => "-".to_owned(),
            (class, "") => class.to_owned(),
            (class, method) => format!("{class}->{method}"),
        };
        let program = if self.program.is_empty() {
            "-"
        } else {
            &self.program
        };

        format!(
            "{} {} {program} {place}: {}",
            or_dash(&self.kind),
            or_dash(&self.severity),
            self.text
        )
    }
}

fn or_dash(text: &str) -> &str {
    if text.is_empty() { "-" } else { text }
}

/// What one run found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunResult {
    pub tests: usize,
    pub alerts: Vec<Alert>,
}

#[derive(Clone, Copy, PartialEq)]
enum Text {
    None,
    Title,
    Detail,
}

/// The alerts and the number of test methods of an ABAP Unit reply. An alert of severity
/// `noAlert` is no alert.
pub fn parse_run_result(xml: &str) -> Result<RunResult, String> {
    let mut reader = Reader::from_str(xml);
    let mut result = RunResult::default();
    let (mut program, mut class, mut method) = (String::new(), String::new(), String::new());
    let mut open: Option<(Alert, Vec<String>)> = None;
    let mut text = Text::None;
    let mut buffer = String::new();

    loop {
        let event = reader.read_event().map_err(|error| error.to_string())?;
        let (element, is_empty) = match &event {
            Event::Start(element) => (Some(element), false),
            Event::Empty(element) => (Some(element), true),
            _ => (None, false),
        };

        if let Some(element) = element {
            let attribute = |wanted: &str| -> Result<Option<String>, String> {
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|error| error.to_string())?;

                    if attribute.key.local_name().into_inner() == wanted {
                        return attribute
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .map(|value| Some(value.into_owned()))
                            .map_err(|error| error.to_string());
                    }
                }

                Ok(None)
            };

            match element.local_name().into_inner() {
                "program" if !is_empty => program = attribute("name")?.unwrap_or_default(),
                "testClass" if !is_empty => class = attribute("name")?.unwrap_or_default(),
                "testMethod" => {
                    result.tests += 1;

                    if !is_empty {
                        method = attribute("name")?.unwrap_or_default();
                    }
                }
                "alert" => {
                    let alert = Alert {
                        kind: attribute("kind")?.unwrap_or_default(),
                        severity: attribute("severity")?.unwrap_or_default(),
                        program: program.clone(),
                        class: class.clone(),
                        method: method.clone(),
                        text: String::new(),
                    };

                    if is_empty {
                        result.alerts.push(alert);
                    } else {
                        open = Some((alert, Vec::new()));
                    }
                }
                "title" if open.is_some() && !is_empty => {
                    text = Text::Title;
                    buffer.clear();
                }
                "detail" if open.is_some() => {
                    if let Some(value) = attribute("text")? {
                        if let Some((_, details)) = open.as_mut() {
                            details.push(value);
                        }
                    } else if !is_empty {
                        text = Text::Detail;
                        buffer.clear();
                    }
                }
                _ => {}
            }
        }

        match event {
            Event::Text(chunk) if text != Text::None => {
                buffer.push_str(&chunk.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::CData(chunk) if text != Text::None => {
                buffer.push_str(&chunk.into_inner());
            }
            Event::GeneralRef(reference) if text != Text::None => {
                buffer.push_str(&reference_text(&reference)?);
            }
            Event::End(element) => match element.local_name().into_inner() {
                "title" if text == Text::Title => {
                    if let Some((alert, _)) = open.as_mut() {
                        alert.text = std::mem::take(&mut buffer);
                    }

                    text = Text::None;
                }
                "detail" if text == Text::Detail => {
                    if let Some((_, details)) = open.as_mut() {
                        details.push(std::mem::take(&mut buffer));
                    }

                    text = Text::None;
                }
                "alert" => {
                    if let Some((mut alert, details)) = open.take() {
                        let detail = details
                            .iter()
                            .map(|detail| detail.trim())
                            .filter(|detail| !detail.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");

                        alert.text = clean(&format!("{} {detail}", alert.text));
                        result.alerts.push(alert);
                    }
                }
                "testMethod" => method.clear(),
                "testClass" => class.clear(),
                "program" => program.clear(),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    result.alerts.retain(|alert| alert.severity != "noAlert");

    for alert in &mut result.alerts {
        alert.text = clean(&alert.text);
    }

    Ok(result)
}

/// One line of printable text, cut at [`TEXT_LIMIT`] characters.
fn clean(text: &str) -> String {
    let single: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .collect();

    if single.chars().count() <= TEXT_LIMIT {
        return single;
    }

    let mut cut: String = single.chars().take(TEXT_LIMIT).collect();

    cut.push_str("...");
    cut
}

/// The one read that shows whether the ADT HTTP session keeps state, before anything is run. The
/// status of the reply does not matter here; a failed request or a refused logon does.
pub fn probe_session(
    transport: &mut dyn SadtTransport,
    target: &AunitTarget,
) -> Result<(), QualityError> {
    let mut exists = Request::get(target.uri());

    exists
        .headers
        .insert("Accept".to_owned(), EXISTS_ACCEPT.to_owned());

    let response = transport
        .request(exists)
        .map_err(|error| QualityError::from_transport(&error))?;

    match response.authorization_failure() {
        Some(error) => Err(QualityError::from_transport(&error)),
        None => Ok(()),
    }
}

/// The outcome of one object.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ObjectRun {
    lines: Vec<String>,
    passed: bool,
    /// The reason of a failure, for the batch summary.
    reason: String,
}

/// The existence read and the run of one object. A failure of the request itself is an error;
/// an object that does not exist, or alerts, are a result.
fn run_object(
    transport: &mut dyn SadtTransport,
    target: &AunitTarget,
) -> Result<ObjectRun, QualityError> {
    let mut exists = Request::get(target.uri());

    exists
        .headers
        .insert("Accept".to_owned(), EXISTS_ACCEPT.to_owned());

    let response = transport
        .request(exists)
        .map_err(|error| QualityError::from_transport(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(QualityError::from_transport(&error));
    }

    // A stateless session may answer 404 for an object that exists: the read proves nothing.
    if response.status == 404 && transport.stateful_supported() == Some(false) {
        return Err(QualityError::rejected(format!(
            "{target} was not found, but this ADT HTTP session is stateless and cannot show \
             that it does not exist; use --backend rfc"
        )));
    }

    if response.status == 404 {
        let reason = format!("{target} does not exist");

        return Ok(ObjectRun {
            lines: vec![format!("REJECTED: {reason}")],
            passed: false,
            reason,
        });
    }

    if response.status != 200 {
        return Err(QualityError::rejected(format!(
            "reading {target} answered HTTP {}",
            response.status
        )));
    }

    let response = transport
        .request(run_request(target))
        .map_err(|error| QualityError::from_transport(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(QualityError::from_transport(&error));
    }

    if !(200..300).contains(&response.status) {
        let reason = format!(
            "the ABAP Unit run of {target} answered HTTP {}",
            response.status
        );

        return Ok(ObjectRun {
            lines: vec![format!("REJECTED: {reason}")],
            passed: false,
            reason,
        });
    }

    let result = parse_run_result(&String::from_utf8_lossy(&response.body)).map_err(|message| {
        QualityError::rejected(format!(
            "cannot read the ABAP Unit reply of {target}: {message}"
        ))
    })?;
    let mut lines: Vec<String> = result.alerts.iter().map(Alert::line).collect();

    if result.tests == 0 {
        lines.push(format!("W: no test method ran for {target}"));
    }

    lines.push(format!(
        "TESTS: {} methods, {} alerts",
        result.tests,
        result.alerts.len()
    ));

    let passed = result.alerts.is_empty();
    let reason = result
        .alerts
        .first()
        .map_or_else(|| "REJECTED".to_owned(), Alert::line);

    Ok(ObjectRun {
        lines,
        passed,
        reason,
    })
}

/// The objects of a `--package`: the Z and Y programs, classes and function groups of TADIR.
fn package_objects(
    transport: &mut dyn SadtTransport,
    package: &str,
) -> Result<Vec<AunitTarget>, QualityError> {
    let condition = format!(
        "PGMID = 'R3TR' AND DEVCLASS = '{package}' AND OBJECT IN ('PROG', 'CLAS', 'FUGR') AND DELFLAG = ' '"
    );
    let data = preview(
        transport,
        "TADIR",
        &["OBJECT", "OBJ_NAME"],
        &condition,
        TADIR_ROWS,
    )?;
    let pairs = columns(&data, &["OBJECT", "OBJ_NAME"])?;

    Ok(pairs
        .into_iter()
        .filter_map(|pair| target_of(&pair[0], &pair[1]))
        .collect())
}

/// The objects of a `--tr`: the request and its tasks, `R3TR` programs, classes and function
/// groups, and the parts of a class (`LIMU`), which stand for the class.
fn request_objects(
    transport: &mut dyn SadtTransport,
    request: &str,
) -> Result<(Vec<AunitTarget>, usize), QualityError> {
    let tasks = preview(
        transport,
        "E070",
        &["TRKORR"],
        &format!("TRKORR = '{request}' OR STRKORR = '{request}'"),
        TASK_LIMIT as u32 + 1,
    )?;
    let numbers: Vec<String> = columns(&tasks, &["TRKORR"])?
        .into_iter()
        .map(|row| row[0].clone())
        .collect();

    if !numbers.iter().any(|number| number == request) {
        return Err(QualityError::rejected(format!(
            "transport request {request} does not exist"
        )));
    }

    if numbers.len() > TASK_LIMIT {
        return Err(QualityError::rejected(format!(
            "transport request {request} has more than {TASK_LIMIT} tasks"
        )));
    }

    let list = numbers
        .iter()
        .map(|number| format!("'{number}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = preview(
        transport,
        "E071",
        &["PGMID", "OBJECT", "OBJ_NAME"],
        &format!("TRKORR IN ({list})"),
        TADIR_ROWS,
    )?;
    let mut targets = Vec::new();
    let mut unmapped = 0;

    for row in columns(&rows, &["PGMID", "OBJECT", "OBJ_NAME"])? {
        match (row[0].as_str(), row[1].as_str()) {
            ("R3TR", object) => targets.extend(target_of(object, &row[2])),
            ("LIMU", "METH" | "CPUB" | "CPRO" | "CPRI" | "CLSD" | "CINC" | "CDEF" | "CIMP") => {
                let class: String = row[2]
                    .chars()
                    .take(30)
                    .take_while(|character| *character != '=' && *character != ' ')
                    .collect();

                if let Some(target) = target_of("CLAS", &class) {
                    targets.push(target);
                }
            }
            ("LIMU", _) => unmapped += 1,
            _ => {}
        }
    }

    Ok((targets, unmapped))
}

fn target_of(object: &str, name: &str) -> Option<AunitTarget> {
    let kind = match object {
        "PROG" => AunitKind::Prog,
        "CLAS" => AunitKind::Clas,
        "FUGR" => AunitKind::Fugr,
        _ => return None,
    };

    AunitTarget::new(kind, name.trim()).ok()
}

fn preview(
    transport: &mut dyn SadtTransport,
    table: &str,
    fields: &[&str],
    condition: &str,
    rows: u32,
) -> Result<TableData, QualityError> {
    let request = ReadTableRequest {
        table: table.to_owned(),
        fields: fields.iter().map(|field| (*field).to_owned()).collect(),
        where_clause: Some(condition.to_owned()),
        rows,
    };
    let query =
        Query::new(&request, TADIR_ROWS, &Policies::default()).map_err(QualityError::rejected)?;

    fetch(transport, table, query.body(), rows).map_err(QualityError::from)
}

/// The named columns of every row, trimmed.
fn columns(data: &TableData, names: &[&str]) -> Result<Vec<Vec<String>>, QualityError> {
    let positions: Option<Vec<usize>> = names
        .iter()
        .map(|name| data.columns.iter().position(|column| column == name))
        .collect();
    let positions = positions.ok_or_else(|| {
        QualityError::rejected(format!("the preview lacks the column {}", names.join(", ")))
    })?;

    Ok(data
        .rows
        .iter()
        .map(|row| {
            positions
                .iter()
                .map(|index| row[*index].trim().to_owned())
                .collect()
        })
        .collect())
}

/// The programs that are includes: they cannot be tested on their own.
fn includes_among(
    transport: &mut dyn SadtTransport,
    targets: &[AunitTarget],
) -> Result<Vec<String>, QualityError> {
    let names: Vec<&str> = targets
        .iter()
        .filter(|target| target.kind == AunitKind::Prog)
        .map(|target| target.name.as_str())
        .collect();
    let mut includes = Vec::new();

    for chunk in names.chunks(TRDIR_CHUNK) {
        let list = chunk
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let data = preview(
            transport,
            "TRDIR",
            &["NAME", "SUBC"],
            &format!("NAME IN ({list})"),
            TRDIR_CHUNK as u32 + 1,
        )?;

        for row in columns(&data, &["NAME", "SUBC"])? {
            if row[1] == "I" {
                includes.push(row[0].clone());
            }
        }
    }

    Ok(includes)
}

/// The objects a scope names, sorted by type and name, with the `W:` notes of what was left out.
/// A `--package` or `--tr` batch is read here, so that a caller can see (and approve) the objects
/// before anything runs.
pub fn resolve_scope(
    transport: &mut dyn SadtTransport,
    scope: &Scope,
) -> Result<(Vec<AunitTarget>, Vec<String>), QualityError> {
    let mut notes = Vec::new();
    let mut targets = match scope {
        Scope::Object(target) => return Ok((vec![target.clone()], notes)),
        Scope::Package(package) => package_objects(transport, package)?,
        Scope::Request(request) => {
            let (targets, unmapped) = request_objects(transport, request)?;

            if unmapped > 0 {
                notes.push(format!(
                    "W: {unmapped} request entries (LIMU REPS, FUNC and others) are not mapped to an object; list the object with R3TR in the request or run it by name"
                ));
            }

            targets
        }
    };

    targets.sort();
    targets.dedup();

    let includes = includes_among(transport, &targets)?;

    targets.retain(|target| target.kind != AunitKind::Prog || !includes.contains(&target.name));

    if targets.len() > MAX_BATCH {
        return Err(QualityError::rejected(format!(
            "{} objects found; a batch runs at most {MAX_BATCH}: narrow the package or request",
            targets.len()
        )));
    }

    Ok((targets, notes))
}

/// Runs the tests of `targets`. One object prints its alerts; a batch prints a block per object
/// and a summary. An exception on one object counts as failed and the batch goes on.
pub fn run_targets(
    transport: &mut dyn SadtTransport,
    targets: &[AunitTarget],
    batch: bool,
    notes: &[String],
) -> Result<Outcome, QualityError> {
    let mut lines: Vec<String> = notes.to_vec();
    let mut failed: Vec<(&AunitTarget, String)> = Vec::new();

    if targets.is_empty() {
        lines.push("W: no PROG, CLAS or FUGR object (Z/Y) to test".to_owned());
        lines.push("SUMMARY: 0 OK, 0 FAILED".to_owned());

        return Ok(Outcome {
            status: Some(Status::Rejected),
            exit: 1,
            stdout: lines.join("\n"),
            stderr: String::new(),
        });
    }

    for target in targets {
        if batch {
            lines.push(format!("== {target} =="));
        }

        let run = match run_object(transport, target) {
            Ok(run) => run,
            Err(error) if !batch => return Err(error),
            Err(error) => ObjectRun {
                lines: Vec::new(),
                passed: false,
                reason: crate::operation::quality::describe(&error),
            },
        };

        lines.extend(run.lines);
        lines.push(format!(
            "AUNIT: {}",
            if run.passed { "ACTIVE" } else { "REJECTED" }
        ));

        if !run.passed {
            failed.push((target, run.reason));
        }
    }

    if batch {
        lines.push(format!(
            "SUMMARY: {} OK, {} FAILED",
            targets.len() - failed.len(),
            failed.len()
        ));

        for (target, reason) in &failed {
            lines.push(format!("FAILED {target}: {reason}"));
        }
    }

    let passed = failed.is_empty();

    Ok(Outcome {
        status: Some(if passed {
            Status::Active
        } else {
            Status::Rejected
        }),
        exit: i32::from(!passed),
        stdout: lines.join("\n"),
        stderr: String::new(),
    })
}

/// The runs of objects that were already resolved (and, for MCP, approved): the
/// productive-client check, then the tests.
pub fn run_resolved(
    transport: &mut dyn SadtTransport,
    targets: &[AunitTarget],
    client: &str,
    batch: bool,
    notes: &[String],
) -> Result<Outcome, QualityError> {
    check_client(transport, client)?;

    run_targets(transport, targets, batch, notes)
}

/// The `aunit` command: the productive-client check, the objects, the runs.
pub fn run(
    transport: &mut dyn SadtTransport,
    scope: &Scope,
    client: &str,
) -> Result<Outcome, QualityError> {
    check_client(transport, client)?;

    let (targets, notes) = resolve_scope(transport, scope)?;

    run_targets(
        transport,
        &targets,
        !matches!(scope, Scope::Object(_)),
        &notes,
    )
}
