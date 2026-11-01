pub mod http;
pub mod rfc;

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
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TransportError {
    #[error("pre-send connectivity failure: {0}")]
    PreSendConnectivity(String),
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
}

impl TransportError {
    pub fn is_retryable_csrf(&self) -> bool {
        matches!(self, Self::CsrfRequired)
    }

    pub fn is_post_send_uncertain(&self) -> bool {
        matches!(self, Self::PostSendUncertain(_))
    }
}

pub trait SadtTransport {
    fn request(&mut self, request: Request) -> Result<Response, TransportError>;

    fn close(&mut self);
}
