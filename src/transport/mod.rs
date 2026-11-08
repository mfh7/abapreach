mod admission;
pub mod http;
pub mod rfc;

pub use admission::{RunnerArm, runner_verdict};

use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    pub method: String,
    pub uri: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn get(uri: impl Into<String>) -> Self {
        Self::new("GET", uri, Vec::new())
    }

    pub fn post(uri: impl Into<String>, body: Vec<u8>) -> Self {
        Self::new("POST", uri, body)
    }

    pub fn put(uri: impl Into<String>, body: Vec<u8>) -> Self {
        Self::new("PUT", uri, body)
    }

    pub fn delete(uri: impl Into<String>) -> Self {
        Self::new("DELETE", uri, Vec::new())
    }

    pub fn new(method: impl Into<String>, uri: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: method.into(),
            uri: uri.into(),
            headers: BTreeMap::new(),
            body,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    pub fn text(status: u16, body: impl AsRef<str>) -> Self {
        Self {
            status,
            headers: BTreeMap::new(),
            body: body.as_ref().as_bytes().to_vec(),
        }
    }

    /// The authorization failure this reply reports. HTTP 401 refuses the logon; HTTP 403 keeps
    /// its operation-specific handling (including the CSRF token flow).
    pub fn authorization_failure(&self) -> Option<TransportError> {
        is_authorization_failure(self.status)
            .then(|| TransportError::Authorization(format!("HTTP {}", self.status)))
    }
}

/// Whether a reply carries the CSRF marker `x-csrf-token: Required`: the token flow refetches the
/// token once, so that reply is not a refused logon.
pub fn is_csrf_required(csrf_token: Option<&str>) -> bool {
    csrf_token.is_some_and(|value| value.eq_ignore_ascii_case("Required"))
}

/// Whether a reply refuses the logon itself. A refused logon is never retried, because every
/// attempt counts against the SAP user.
pub fn is_authorization_failure(status: u16) -> bool {
    status == 401
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TransportError {
    #[error("pre-send connectivity failure: {0}")]
    PreSendConnectivity(String),
    /// A read-only request that was sent but never answered: it has no side effect, so routing
    /// treats it as a connectivity failure rather than an uncertain write.
    #[error("no response to a read-only request: {0}")]
    NoResponse(String),
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("CSRF token required")]
    CsrfRequired,
    #[error("SADT request is not allowed: {0}")]
    RequestNotAllowed(String),
    #[error("authorization failed: {0}")]
    Authorization(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("post-send outcome uncertain: {0}")]
    PostSendUncertain(String),
    #[error("{0}")]
    Sent(Box<TransportError>),
}

impl TransportError {
    pub fn is_retryable_csrf(&self) -> bool {
        matches!(self, Self::CsrfRequired)
    }

    pub fn is_post_send_uncertain(&self) -> bool {
        matches!(self, Self::PostSendUncertain(_))
    }

    pub fn after_send(self) -> Self {
        if self.is_post_send_uncertain() {
            self
        } else {
            Self::Sent(Box::new(self))
        }
    }

    pub fn was_sent(&self) -> bool {
        matches!(self, Self::PostSendUncertain(_) | Self::Sent(_))
    }
}

pub trait SadtTransport {
    fn request(&mut self, request: Request) -> Result<Response, TransportError>;

    /// Ends the session; a failure is reported, never turned into a different verdict.
    fn close(&mut self) -> Result<(), TransportError>;

    /// Whether the backend keeps a stateful session; `None` when it cannot tell (RFC/SADT).
    fn stateful_supported(&self) -> Option<bool> {
        None
    }
}
