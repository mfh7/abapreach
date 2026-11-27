//! `ping`: the system and the application server instance that answered, and the time of the one
//! call that asked. `doctor` prints the instance of the same reading with its host masked.
//!
//! Over ADT HTTP the facts come from the Atom feed of `/system/information`; over RFC from the
//! typed `RFC_SYSTEM_INFO` call of the RFC transport. A release without the feed (7.40 answers
//! 404) has no instance to show: that is a reading with an unknown instance, not a failure.

use crate::adt::{SystemInformation, parse_system_information};
use crate::backend::{BackendKind, Failure};
use crate::config::scrub;
use crate::domain::Outcome;
use crate::operation::quality::QualityError;
use crate::operation::text::squeeze;
use crate::transport::rfc::SystemInfo;
use crate::transport::{Request, SadtTransport, TransportError};
use std::time::{Duration, Instant};

pub const SYSTEM_INFORMATION_URI: &str = "/sap/bc/adt/system/information";
/// The only `Accept` the request carries; the RFC SADT admission checks for the same value.
pub const SYSTEM_INFORMATION_ACCEPT: &str = "application/atom+xml;type=feed";
/// Why the instance is unknown on a release that has no `/system/information`.
pub const NO_ENDPOINT: &str = "no /system/information on this release";
/// The reasons an instance is unknown. They are fixed texts: no error text, host or reply content
/// ever reaches an instance line.
pub const REQUEST_FAILED: &str = "request failed";
pub const NO_ANSWER: &str = "no answer";
pub const NOT_AUTHORIZED: &str = "not authorized";
pub const UNREADABLE_REPLY: &str = "unreadable reply";
/// The longest value a line prints; the server decides the text, so it is cut.
const VALUE_MAX: usize = 64;

/// The GET of the feed, as admission accepts it.
pub fn request() -> Request {
    let mut request = Request::get(SYSTEM_INFORMATION_URI);

    request
        .headers
        .insert("Accept".to_owned(), SYSTEM_INFORMATION_ACCEPT.to_owned());

    request
}

/// What one reading knows about the system. A field the backend does not report is `None` and
/// has no line. The host address and the database server are never kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemFacts {
    /// The instance name, or why it is unknown.
    pub instance: Result<String, String>,
    pub system_id: Option<String>,
    pub release: Option<String>,
    pub kernel: Option<String>,
    pub database: Option<String>,
    pub os: Option<String>,
}

/// A value as printable text. The connection secrets go first, then the control and format
/// characters are dropped and the whitespace is squeezed, then the secrets go again (dropping a
/// character can join the halves of one), and only then the value is cut at [`VALUE_MAX`], so the
/// cut never leaves part of a secret. `None` when nothing is left.
fn clean(text: &str, secrets: &[String]) -> Option<String> {
    let squeezed = scrub(squeeze(&scrub(text, secrets)), secrets);
    let cut: String = squeezed.chars().take(VALUE_MAX).collect();

    (!cut.is_empty()).then_some(cut)
}

/// The fixed reason of a failed call; never the text of the error.
fn reason_of(error: &TransportError) -> String {
    match error {
        TransportError::Authorization(_) => NOT_AUTHORIZED.to_owned(),
        TransportError::Http { status, .. } => format!("HTTP {status}"),
        TransportError::NoResponse(_) | TransportError::PostSendUncertain(_) => {
            NO_ANSWER.to_owned()
        }
        TransportError::Sent(inner) => reason_of(inner),
        TransportError::PreSendConnectivity(_)
        | TransportError::CsrfRequired
        | TransportError::RequestNotAllowed(_)
        | TransportError::Protocol(_) => REQUEST_FAILED.to_owned(),
    }
}

/// A reading that failed: the usual failure of its class, and the fixed reason an instance line
/// of `doctor` gives for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadFailure {
    pub error: QualityError,
    pub reason: String,
}

impl ReadFailure {
    fn transport(error: &TransportError) -> Self {
        Self {
            error: QualityError::from_transport(error),
            reason: reason_of(error),
        }
    }
}

/// An instance name `<host>_<SID>_<NN>` split into its parts: a host (it may hold underscores), three
/// upper-case letters or digits, and a two-digit instance number. A name of another shape has no
/// parts.
fn split_instance(instance: &str) -> Option<(&str, &str, &str)> {
    let (rest, number) = instance.rsplit_once('_')?;
    let (host, system) = rest.rsplit_once('_')?;

    (!host.is_empty()
        && number.len() == 2
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && system.len() == 3
        && system
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()))
    .then_some((host, system, number))
}

/// The system id inside an instance name, when it has the `<host>_<SID>_<NN>` shape.
fn system_id_of(instance: &str) -> Option<String> {
    split_instance(instance).map(|(_, system, _)| system.to_owned())
}

impl SystemFacts {
    /// Nothing is known but why the instance is not.
    fn unknown(reason: &str) -> Self {
        Self {
            instance: Err(reason.to_owned()),
            system_id: None,
            release: None,
            kernel: None,
            database: None,
            os: None,
        }
    }

    /// The facts of the ADT feed. It has no SAP release, and its `SAPSystemID` is a platform
    /// number, so the system id is read from the instance name.
    fn from_feed(info: SystemInformation, secrets: &[String]) -> Self {
        let clean = |text: &str| clean(text, secrets);
        let instance = info.application_server.as_deref().and_then(clean);
        let system_id = instance.as_deref().and_then(system_id_of);

        Self {
            instance: instance.ok_or_else(|| UNREADABLE_REPLY.to_owned()),
            system_id,
            release: None,
            kernel: info.kernel_release.as_deref().and_then(clean),
            database: info.database_system.as_deref().and_then(clean),
            os: info.os_name.as_deref().and_then(clean),
        }
    }

    /// The facts of `RFC_SYSTEM_INFO`. It reports no operating system here.
    fn from_rfc(info: &SystemInfo, secrets: &[String]) -> Self {
        let clean = |text: &str| clean(text, secrets);

        Self {
            instance: clean(&info.instance).ok_or_else(|| UNREADABLE_REPLY.to_owned()),
            system_id: clean(&info.system_id),
            release: clean(&info.release),
            kernel: clean(&info.kernel),
            database: clean(&info.database),
            os: None,
        }
    }

    /// The instance line `doctor` prints: the host part of the name is always `***`, so the line is
    /// `instance ***_<SID>_<NN>`. A name that does not have that shape is `instance ***`, never the
    /// raw value. An unknown instance keeps its reason.
    pub fn masked_instance_line(&self) -> String {
        match &self.instance {
            Ok(name) => match split_instance(name) {
                Some((_, system, number)) => format!("instance ***_{system}_{number}"),
                None => "instance ***".to_owned(),
            },
            Err(reason) => format!("instance unknown ({reason})"),
        }
    }

    /// The instance line `ping` prints: the name exactly as SAP reports it (cleaned for a line).
    pub fn instance_line(&self) -> String {
        match &self.instance {
            Ok(name) => format!("instance {name}"),
            Err(reason) => format!("instance unknown ({reason})"),
        }
    }

    /// The lines of `ping`: the instance, then every known field.
    fn lines(&self) -> Vec<String> {
        let fields = [
            ("system id", &self.system_id),
            ("release", &self.release),
            ("kernel", &self.kernel),
            ("database", &self.database),
            ("os", &self.os),
        ];

        std::iter::once(self.instance_line())
            .chain(
                fields
                    .into_iter()
                    .filter_map(|(label, value)| value.as_ref().map(|v| format!("{label} {v}"))),
            )
            .collect()
    }
}

/// A reading with the backend that answered it: what `ping` prints and what the MCP tool returns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pinged {
    pub backend: BackendKind,
    pub reading: Reading,
}

impl Pinged {
    /// The text of `ping`.
    pub fn text(&self) -> String {
        self.reading.outcome(self.backend).stdout
    }
}

/// One answered reading: the facts and the time of the call that fetched them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reading {
    pub facts: SystemFacts,
    pub round_trip: Duration,
    /// Whether `round_trip` also holds the logon of the session. Over ADT HTTP the GET is the
    /// first request, so the TLS connect and the authentication cannot be timed apart from it.
    /// Over RFC the transport times the invoke alone, after the logon and the function
    /// description.
    pub includes_logon: bool,
}

impl Reading {
    /// The `ping` output: the backend, the facts and `round trip <ms> ms`.
    pub fn outcome(&self, backend: BackendKind) -> Outcome {
        let mut lines = vec![format!("backend {backend}")];

        lines.extend(self.facts.lines());
        lines.push(format!(
            "round trip {} ms{}",
            self.round_trip.as_millis(),
            if self.includes_logon {
                " (includes logon)"
            } else {
                ""
            }
        ));

        Outcome {
            status: None,
            exit: 0,
            stdout: lines.join("\n"),
            stderr: String::new(),
        }
    }
}

/// One reading on `transport`, which is a session of `backend`: a GET of the feed over ADT HTTP,
/// the typed `RFC_SYSTEM_INFO` call over RFC. Every printed value is scrubbed of `secrets` (see
/// [`clean`]). A failed call is the usual failure of its class.
pub fn read(
    transport: &mut dyn SadtTransport,
    backend: BackendKind,
    secrets: &[String],
) -> Result<Reading, ReadFailure> {
    let started = Instant::now();

    match backend {
        BackendKind::AdtHttp => {
            let response = transport.request(request());
            let round_trip = started.elapsed();
            let response = response.map_err(|error| ReadFailure::transport(&error))?;

            if let Some(error) = response.authorization_failure() {
                return Err(ReadFailure::transport(&error));
            }

            let facts = match response.status {
                200 => {
                    let feed = String::from_utf8_lossy(&response.body);
                    // The parser's message may quote the reply, so it is not kept.
                    let info = parse_system_information(&feed).map_err(|_| ReadFailure {
                        error: QualityError::rejected("cannot read the system information reply"),
                        reason: UNREADABLE_REPLY.to_owned(),
                    })?;

                    SystemFacts::from_feed(info, secrets)
                }
                404 => SystemFacts::unknown(NO_ENDPOINT),
                // The release does not serve the feed as Atom: no system information, no failure.
                406 => SystemFacts::unknown("HTTP 406"),
                status => {
                    let error = TransportError::Http {
                        status,
                        message: String::new(),
                    };

                    return Err(ReadFailure {
                        error: QualityError::Failed {
                            failure: Failure::from(&error),
                            message: format!("the system information answered HTTP {status}"),
                        },
                        reason: reason_of(&error),
                    });
                }
            };

            Ok(Reading {
                facts,
                round_trip,
                includes_logon: true,
            })
        }
        BackendKind::RfcSadt => {
            let info = transport.system_info();
            let outer = started.elapsed();
            let info = info.map_err(|error| ReadFailure::transport(&error))?;
            // A transport that cannot time the invoke alone gives the whole call, logon included.
            let invoke = info.invoke_time;

            Ok(Reading {
                facts: SystemFacts::from_rfc(&info, secrets),
                round_trip: invoke.unwrap_or(outer),
                includes_logon: invoke.is_none(),
            })
        }
    }
}
