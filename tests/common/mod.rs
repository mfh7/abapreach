#![allow(dead_code)]

use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use serde::Deserialize;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct Fixture {
    pub system: String,
    pub release: String,
    pub backend: String,
    pub exchanges: Vec<FixtureExchange>,
}

#[derive(Deserialize)]
pub struct FixtureExchange {
    pub request: FixtureRequest,
    pub response: FixtureResponse,
}

#[derive(Deserialize)]
pub struct FixtureRequest {
    pub method: String,
    pub uri: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

#[derive(Deserialize)]
pub struct FixtureResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

pub struct ScriptedTransport {
    pub calls: Vec<Request>,
    pub closes: usize,
    expected: VecDeque<Request>,
    replies: VecDeque<Result<Response, TransportError>>,
}

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("m1")
        .join(name)
}

pub fn all(_: &FixtureExchange) -> bool {
    true
}

pub fn from_fixture(
    path: impl AsRef<Path>,
    filter: fn(&FixtureExchange) -> bool,
) -> ScriptedTransport {
    let text = fs::read_to_string(path).expect("read fixture");
    let fixture: Fixture = serde_json::from_str(&text).expect("parse fixture");
    let exchanges = fixture.exchanges.into_iter().filter(filter);
    let mut expected = VecDeque::new();
    let mut replies = VecDeque::new();

    for exchange in exchanges {
        expected.push_back(Request {
            method: exchange.request.method,
            uri: exchange.request.uri,
            headers: exchange.request.headers,
            body: exchange.request.body.into_bytes(),
        });
        replies.push_back(Ok(Response {
            status: exchange.response.status,
            headers: exchange.response.headers,
            body: exchange.response.body.into_bytes(),
        }));
    }

    ScriptedTransport {
        calls: Vec::new(),
        closes: 0,
        expected,
        replies,
    }
}

impl ScriptedTransport {
    pub fn replies(replies: Vec<Result<Response, TransportError>>) -> Self {
        Self {
            calls: Vec::new(),
            closes: 0,
            expected: VecDeque::new(),
            replies: replies.into(),
        }
    }

    pub fn assert_finished(&self) {
        assert!(self.expected.is_empty(), "unconsumed expected requests");
        assert!(self.replies.is_empty(), "unconsumed scripted replies");
    }
}

impl SadtTransport for ScriptedTransport {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if let Some(expected) = self.expected.pop_front() {
            assert_eq!(request, expected, "unexpected SADT request");
        }

        self.calls.push(request);
        self.replies
            .pop_front()
            .expect("request without a scripted response")
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes += 1;

        Ok(())
    }
}
