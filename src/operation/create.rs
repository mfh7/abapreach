//! Creating a function group, function module, class or interface. A create is source-free: it
//! yields an empty ADT skeleton, and a later deploy supplies the source. It is one POST that is
//! never repeated; an uncertain answer is settled by a read-only GET of the object.

use crate::adt::package_of;
use crate::backend::{Failure, Selection, SelectionReason};
use crate::cli::CreateKind;
use crate::config::Connection;
use crate::domain::{Outcome, Status};
use crate::object::{DEFAULT_PACKAGE, PackageScope};
use crate::operation::deploy::{DeployResult, leaf_texts};
use crate::operation::read::OBJECT_STRUCTURE;
use crate::transport::{Request, Response, SadtTransport, TransportError};
use quick_xml::escape::escape;
use std::fmt::Display;

const ANY: &str = "application/*";
const UNCONFIRMED: &str = "validation did not confirm the name";
const NO_STATEFUL_SESSION: &str = "ADT HTTP stateful session unavailable (no sap-contextid)";

#[derive(Clone, Debug)]
pub struct CreateInput {
    pub kind: CreateKind,
    pub name: String,
    pub text: String,
    /// Ignored for a FUNC, which takes the package of its function group.
    pub package: Option<String>,
    /// The function group of a FUNC.
    pub group: Option<String>,
    pub tr: Option<String>,
    pub simulate: bool,
}

/// What a validation request answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Validation {
    /// The name may be created; the text is a server note worth showing, such as INFO.
    Accepted(Option<String>),
    /// The name or text is refused, such as an existing name or the shared CLAS/INTF namespace.
    Rejected(String),
    /// The endpoint answered something the validation does not define.
    Failed(u16),
}

/// Reads the answer of a validation request. A function group or module answers 200 with a
/// `SEVERITY`, a class or interface answers 200 with `CHECK_RESULT` X or 400 `InvalidClifName`.
/// Anything that does not confirm the name is a rejection.
pub fn classify_validation(kind: CreateKind, status: u16, body: &str) -> Validation {
    let fields = leaf_texts(body).unwrap_or_default();
    let field = |name: &str| fields.get(name).map(|text| text.trim().to_owned());

    match (status, kind) {
        (400, _) => Validation::Rejected(
            field("localizedMessage")
                .or_else(|| field("message"))
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "validation refused the name: HTTP 400".to_owned()),
        ),
        (200, CreateKind::Fugr | CreateKind::Func) => {
            let text = field("SHORT_TEXT").filter(|text| !text.is_empty());

            match field("SEVERITY").as_deref() {
                Some("OK") => Validation::Accepted(None),
                Some("INFO" | "WARNING") => Validation::Accepted(text),
                Some("ERROR") => Validation::Rejected(
                    text.unwrap_or_else(|| "validation refused the name".to_owned()),
                ),
                other => Validation::Rejected(format!(
                    "{UNCONFIRMED} (SEVERITY {})",
                    other.unwrap_or("missing")
                )),
            }
        }
        (200, CreateKind::Clas | CreateKind::Intf) => match field("CHECK_RESULT").as_deref() {
            Some("X") => Validation::Accepted(None),
            _ => Validation::Rejected(UNCONFIRMED.to_owned()),
        },
        (status, _) => Validation::Failed(status),
    }
}

/// The body of the create POST. `responsible` and `language` come from the logon.
pub fn creation_body(
    input: &CreateInput,
    package: &str,
    responsible: &str,
    language: &str,
) -> Result<String, String> {
    let name = escape(input.name.to_ascii_uppercase());
    let description = escape(&input.text);
    let (element, namespace, object_type) = match input.kind {
        CreateKind::Fugr => (
            "group:abapFunctionGroup",
            r#"xmlns:group="http://www.sap.com/adt/functions/groups""#,
            "FUGR/F",
        ),
        CreateKind::Func => (
            "fmodule:abapFunctionModule",
            r#"xmlns:fmodule="http://www.sap.com/adt/functions/fmodules""#,
            "FUGR/FF",
        ),
        CreateKind::Clas => (
            "class:abapClass",
            r#"xmlns:class="http://www.sap.com/adt/oo/classes""#,
            "CLAS/OC",
        ),
        CreateKind::Intf => (
            "intf:abapInterface",
            r#"xmlns:intf="http://www.sap.com/adt/oo/interfaces""#,
            "INTF/OI",
        ),
    };
    let mut attributes = format!(
        r#"adtcore:description="{description}" adtcore:name="{name}" adtcore:type="{object_type}""#
    );
    let inner = if input.kind == CreateKind::Func {
        let group = input
            .group
            .as_deref()
            .ok_or("create FUNC needs its function group")?;
        let uri = CreateKind::Fugr.object_uri(group, None)?;

        format!(
            r#"<adtcore:containerRef adtcore:name="{}" adtcore:type="FUGR/F" adtcore:uri="{uri}"/>"#,
            escape(group.to_ascii_uppercase())
        )
    } else {
        attributes.push_str(&format!(
            r#" adtcore:language="{}" adtcore:masterLanguage="{}""#,
            escape(language),
            escape(language)
        ));

        if !responsible.is_empty() {
            attributes.push_str(&format!(
                r#" adtcore:responsible="{}""#,
                escape(responsible)
            ));
        }

        format!(
            r#"<adtcore:packageRef adtcore:name="{}"/>"#,
            escape(package.to_ascii_uppercase())
        )
    };

    Ok(format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><{element} {namespace} "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core" {attributes}>{inner}</{element}>"#,
        ),
        element = element,
        namespace = namespace,
        attributes = attributes,
        inner = inner
    ))
}

pub struct CreateProg;

impl CreateProg {
    /// Runs one create. The result carries the routing class like a deploy: a backend switch is
    /// possible only while the POST has not been sent.
    pub fn run(
        transport: &mut dyn SadtTransport,
        input: CreateInput,
        selection: Option<&Selection>,
        connection: Option<&Connection>,
    ) -> DeployResult {
        let mut run = Run {
            transport,
            input,
            pinned: selection
                .is_some_and(|selection| selection.reason == SelectionReason::ExplicitBackend),
            stdout: selection
                .map(|selection| {
                    vec![
                        format!("backend: {}", selection.backend.token()),
                        format!("reason: {}", selection.reason),
                    ]
                })
                .unwrap_or_default(),
            notes: Vec::new(),
            user: connection
                .map(|connection| connection.user.clone())
                .unwrap_or_default(),
            language: connection
                .map(|connection| connection.lang.clone())
                .unwrap_or_else(|| "EN".to_owned()),
            write_sent: false,
        };
        let mut end = match run.flow() {
            Ok(end) | Err(end) => end,
        };

        if let Err(error) = run.transport.close() {
            end.warn(format!("W: session close failed: {error}"));
        }

        for note in run.notes.into_iter().rev() {
            end.warn_first(note);
        }

        run.stdout.extend(end.stdout);

        DeployResult {
            outcome: Outcome {
                status: end.status,
                exit: end.exit,
                stdout: run.stdout.join("\n"),
                stderr: end.stderr.unwrap_or_default(),
            },
            failure: end.failure,
            fallback_allowed: end.fallback_allowed,
            detail: end.detail,
        }
    }
}

/// How the run ends.
struct End {
    status: Option<Status>,
    exit: i32,
    stdout: Vec<String>,
    stderr: Option<String>,
    failure: Option<Failure>,
    fallback_allowed: bool,
    detail: Option<String>,
}

impl End {
    fn new(status: Option<Status>, exit: i32) -> Self {
        Self {
            status,
            exit,
            stdout: Vec::new(),
            stderr: None,
            failure: None,
            fallback_allowed: false,
            detail: None,
        }
    }

    /// The token line first, then the reason; the exit follows the status.
    fn with(status: Status, token: &str, reason: impl Into<String>) -> Self {
        Self {
            stdout: vec![token.to_owned(), reason.into()],
            ..Self::new(Some(status), status.exit())
        }
    }

    fn rejected(reason: impl Into<String>) -> Self {
        Self::with(Status::Rejected, "CREATE: REJECTED", reason)
    }

    fn partial(reason: impl Into<String>) -> Self {
        Self::with(Status::Partial, "CREATE: PARTIAL", reason)
    }

    fn stop(message: impl Display) -> Self {
        Self {
            stderr: Some(format!("STOP: {message}")),
            ..Self::new(None, 2)
        }
    }

    fn error(error: &TransportError, before_write: bool) -> Self {
        let failure = Failure::from(error);

        Self {
            stderr: Some(format!("ERROR: {error}")),
            failure: Some(failure),
            fallback_allowed: before_write && failure == Failure::PreSendConnectivity,
            detail: Some(error.to_string()),
            ..Self::new(None, 1)
        }
    }

    /// A status the create does not define: exit 1, no fallback, no reconciliation.
    fn http(what: &str, code: u16) -> Self {
        let error = TransportError::Http {
            status: code,
            message: String::new(),
        };

        Self {
            stderr: Some(format!("ERROR: {what}: HTTP {code}")),
            failure: Some(Failure::from(&error)),
            detail: Some(format!("{what}: HTTP {code}")),
            ..Self::new(None, 1)
        }
    }

    /// Nothing was written, so another backend may still create the object.
    fn stateful_unavailable() -> Self {
        Self {
            stderr: Some(format!("ERROR: {NO_STATEFUL_SESSION}")),
            failure: Some(Failure::CapabilityUnavailable),
            fallback_allowed: true,
            detail: Some(NO_STATEFUL_SESSION.to_owned()),
            ..Self::new(None, 1)
        }
    }

    fn warn(&mut self, note: String) {
        self.stderr = Some(match self.stderr.take() {
            Some(existing) => format!("{existing}\n{note}"),
            None => note,
        });
    }

    fn warn_first(&mut self, note: String) {
        self.stderr = Some(match self.stderr.take() {
            Some(existing) => format!("{note}\n{existing}"),
            None => note,
        });
    }
}

struct Run<'a> {
    transport: &'a mut dyn SadtTransport,
    input: CreateInput,
    /// `--backend` named the backend, so no other one may take over.
    pinned: bool,
    stdout: Vec<String>,
    /// `W:` lines that come before the verdict on stderr.
    notes: Vec<String>,
    user: String,
    language: String,
    /// The create POST has been sent: no fallback is possible any more.
    write_sent: bool,
}

impl Run<'_> {
    fn flow(&mut self) -> Result<End, End> {
        let package = self.package()?;
        let scope = PackageScope::of(&package, self.input.tr.as_deref()).map_err(End::stop)?;

        self.validate(&package)?;

        // The server accepts a create over a session that cannot keep state, and leaves
        // an object that nothing can lock or delete afterwards; nothing is sent before this.
        if self.transport.stateful_supported() == Some(false) {
            return Err(if self.pinned {
                End::stop(
                    "ADT HTTP on this system keeps no stateful session (no sap-contextid), so a \
                     create there leaves objects that cannot be changed or deleted; use \
                     --backend rfc",
                )
            } else {
                End::stateful_unavailable()
            });
        }

        if self.input.simulate {
            self.stdout.push(format!(
                "WILL CREATE {} {} in package {package}{}",
                self.input.kind.token(),
                self.input.name.to_ascii_uppercase(),
                match &scope {
                    PackageScope::Local => String::new(),
                    PackageScope::Transportable { request } => format!(" (request {request})"),
                }
            ));

            return Ok(End::with(
                Status::SimulatedOk,
                "CREATE_SIM: SIMULATED_OK",
                "validated; nothing was written",
            ));
        }

        self.post(&scope, &package)
    }

    /// The package the object lands in: the one asked for, or for a FUNC the package of its
    /// function group, which also decides the transport rules.
    fn package(&mut self) -> Result<String, End> {
        if self.input.kind != CreateKind::Func {
            return Ok(self
                .input
                .package
                .clone()
                .unwrap_or_else(|| DEFAULT_PACKAGE.to_owned()));
        }

        let group = self
            .input
            .group
            .clone()
            .ok_or_else(|| End::stop("create FUNC needs its function group"))?;
        let uri = CreateKind::Fugr
            .object_uri(&group, None)
            .map_err(End::stop)?;
        let response = self.read(uri)?;

        match response.status {
            200 => {
                let xml = String::from_utf8_lossy(&response.body);

                match package_of(&xml) {
                    Ok(Some(package)) => Ok(package),
                    _ => Err(End::rejected(format!(
                        "the package of function group {group} cannot be read"
                    ))),
                }
            }
            404 => Err(End::rejected(format!(
                "function group {group} does not exist"
            ))),
            status => Err(End::http("read function group", status)),
        }
    }

    fn validate(&mut self, package: &str) -> Result<(), End> {
        let scope = match self.input.kind {
            CreateKind::Func => self.input.group.clone().unwrap_or_default(),
            _ => package.to_owned(),
        };
        let uri = self
            .input
            .kind
            .validation_uri(&self.input.name, &self.input.text, &scope);
        let mut request = Request::post(uri, Vec::new());
        request.headers.insert("Accept".to_owned(), ANY.to_owned());
        let response = self.call(request)?;
        let body = String::from_utf8_lossy(&response.body);

        match classify_validation(self.input.kind, response.status, &body) {
            Validation::Accepted(note) => {
                if let Some(note) = note {
                    self.notes.push(format!("W: validation: {note}"));
                }

                Ok(())
            }
            Validation::Rejected(reason) => Err(End::rejected(reason)),
            Validation::Failed(status) => Err(End::http("create validation failed", status)),
        }
    }

    /// The only write of the run. It is sent once; any answer that does not settle the outcome
    /// is read back, never sent again.
    fn post(&mut self, scope: &PackageScope, package: &str) -> Result<End, End> {
        let uri = self
            .input
            .kind
            .creation_request_uri(self.input.group.as_deref(), scope)
            .map_err(End::stop)?;
        let body =
            creation_body(&self.input, package, &self.user, &self.language).map_err(End::stop)?;
        let mut request = Request::post(uri, body.into_bytes());
        request
            .headers
            .insert("Content-Type".to_owned(), ANY.to_owned());

        let response = match self.transport.request(request) {
            Ok(response) => response,
            // Sent, but the reply never settled it: read back, never send again.
            Err(error) if error.was_sent() => {
                self.write_sent = true;

                return Ok(self.reconcile("reply was lost"));
            }
            Err(error) => return Err(End::error(&error, true)),
        };

        self.write_sent = true;

        if let Some(error) = response.authorization_failure() {
            return Err(End::error(&error, false));
        }

        match response.status {
            200..=299 => Ok(End::with(
                Status::Active,
                "CREATE: ACTIVE",
                format!(
                    "{} {} created (empty skeleton; deploy supplies the source)",
                    self.input.kind.token(),
                    self.input.name.to_ascii_uppercase()
                ),
            )),
            // The class create without a request answered 500 and still formed the object.
            500..=599 => Ok(self.reconcile(&format!("answered HTTP {}", response.status))),
            status => {
                let body = String::from_utf8_lossy(&response.body);
                let detail = leaf_texts(&body)
                    .ok()
                    .and_then(|fields| {
                        fields
                            .get("localizedMessage")
                            .or_else(|| fields.get("message"))
                            .map(|text| text.trim().to_owned())
                    })
                    .filter(|text| !text.is_empty());

                Err(End::rejected(match detail {
                    Some(detail) => format!("HTTP {status}: {detail}"),
                    None => format!("HTTP {status}"),
                }))
            }
        }
    }

    /// A read-only GET settles an uncertain create: the object is there (`PARTIAL`), not there
    /// (`REJECTED`, a new run is safe), or unreadable (`PARTIAL`). There is no second POST.
    fn reconcile(&mut self, cause: &str) -> End {
        let kind = self.input.kind;
        let name = self.input.name.to_ascii_uppercase();
        let uri = match kind.object_uri(&self.input.name, self.input.group.as_deref()) {
            // A fresh class answers 500 for its metadata and source; its structure answers 200.
            Ok(uri) if kind == CreateKind::Clas => format!("{uri}/{OBJECT_STRUCTURE}"),
            Ok(uri) => uri,
            Err(message) => {
                return End::partial(format!(
                    "the create POST {cause} and the object cannot be read back ({message}); \
                     {CLEANUP}"
                ));
            }
        };
        let mut request = Request::get(uri);
        request.headers.insert("Accept".to_owned(), ANY.to_owned());

        match self.transport.request(request) {
            Ok(response) if response.authorization_failure().is_none() => match response.status {
                200 => End::partial(format!(
                    "the create POST {cause} and {} {name} exists; the create is not repeated; \
                     {CLEANUP}",
                    kind.token()
                )),
                404 => End::rejected(format!(
                    "the create POST {cause} and {} {name} does not exist; nothing was created, \
                     the run can be repeated",
                    kind.token()
                )),
                status => End::partial(format!(
                    "the create POST {cause} and the object cannot be read back (HTTP {status}); \
                     {CLEANUP}"
                )),
            },
            Ok(_) => End::partial(format!(
                "the create POST {cause} and the object cannot be read back (logon refused); \
                 {CLEANUP}"
            )),
            Err(error) => End::partial(format!(
                "the create POST {cause} and the object cannot be read back ({error}); {CLEANUP}"
            )),
        }
    }

    fn read(&mut self, uri: String) -> Result<Response, End> {
        let mut request = Request::get(uri);
        request.headers.insert("Accept".to_owned(), ANY.to_owned());

        self.call(request)
    }

    /// A read-only request. A logon that was refused ends the run whatever the request was.
    fn call(&mut self, request: Request) -> Result<Response, End> {
        let before_write = !self.write_sent;
        let response = self
            .transport
            .request(request)
            .map_err(|error| End::error(&error, before_write))?;

        match response.authorization_failure() {
            Some(error) => Err(End::error(&error, false)),
            None => Ok(response),
        }
    }
}

const CLEANUP: &str = "check whether the object exists and remove it manually if it is unwanted";
