#![allow(dead_code)]

use crate::common::{Fixture, FixtureRequest, fixture_path};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use std::collections::VecDeque;
use std::fs;

pub enum Step {
    /// Expect the request of fixture exchange `.0` and answer with the response of `.1`.
    Pair(usize, usize),
    /// Expect the request of fixture exchange `.0` and answer with a scripted result.
    ReplyTo(usize, Result<Response, TransportError>),
    /// Accept any request and answer with a scripted result.
    Any(Result<Response, TransportError>),
    /// Expect the version-list GET of the `foreign_inactive` guard and answer with it.
    Versions(Result<Response, TransportError>),
}

/// The program every m1 fixture deploys.
pub const PROGRAM_URI: &str = "/sap/bc/adt/programs/programs/ytest_abapreach01";

/// The source version list of the `foreign_inactive` guard.
pub fn versions_uri() -> String {
    format!("{PROGRAM_URI}/source/main/versions")
}

/// The version list of a program with an inactive version of another user and one active version
/// of `DEVUSER`, as `GET <source>/versions` returns it.
pub const FOREIGN_INACTIVE_FEED: &str = concat!(
    r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom">"#,
    r#"<atom:entry><atom:author><atom:name>OTHERUSER</atom:name></atom:author>"#,
    r#"<atom:id>99999</atom:id></atom:entry>"#,
    r#"<atom:entry><atom:author><atom:name>DEVUSER</atom:name></atom:author>"#,
    r#"<atom:id>00001</atom:id></atom:entry></atom:feed>"#,
);

/// The version list of a program without an inactive version.
pub const NO_INACTIVE_FEED: &str = concat!(
    r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom">"#,
    r#"<atom:entry><atom:author><atom:name>DEVUSER</atom:name></atom:author>"#,
    r#"<atom:id>00001</atom:id></atom:entry></atom:feed>"#,
);

/// The version list of a program whose inactive version is the one `DEVUSER` wrote.
pub const OWN_INACTIVE_FEED: &str = concat!(
    r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom">"#,
    r#"<atom:entry><atom:author><atom:name>DEVUSER</atom:name></atom:author>"#,
    r#"<atom:id>99999</atom:id></atom:entry>"#,
    r#"<atom:entry><atom:author><atom:name>DEVUSER</atom:name></atom:author>"#,
    r#"<atom:id>00001</atom:id></atom:entry></atom:feed>"#,
);

/// The version list answer of a program without an inactive version.
pub fn no_inactive() -> Step {
    feed(NO_INACTIVE_FEED)
}

pub fn feed(body: &str) -> Step {
    Step::Versions(Ok(Response::text(200, body)))
}

/// Expect and answer with the same fixture exchange.
pub fn fx(index: usize) -> Step {
    Step::Pair(index, index)
}

pub struct Replay {
    pub calls: Vec<Request>,
    pub closes: usize,
    /// `Some` when the session end is scripted to fail with this error.
    close_failure: Option<TransportError>,
    fixture: Fixture,
    steps: VecDeque<Step>,
}

pub fn replay(fixture: &str, steps: Vec<Step>) -> Replay {
    let text = fs::read_to_string(fixture_path(fixture)).unwrap();

    Replay {
        calls: Vec::new(),
        closes: 0,
        close_failure: None,
        fixture: serde_json::from_str(&text).unwrap(),
        steps: steps.into(),
    }
}

impl Replay {
    pub fn failing_close(mut self, error: TransportError) -> Self {
        self.close_failure = Some(error);
        self
    }
    pub fn assert_finished(&self) {
        assert!(
            self.steps.is_empty(),
            "{} scripted steps were not used",
            self.steps.len()
        );
    }

    pub fn uris(&self) -> Vec<String> {
        self.calls
            .iter()
            .map(|call| format!("{} {}", call.method, call.uri))
            .collect()
    }

    fn expected(&self, index: usize) -> Request {
        let FixtureRequest {
            method,
            uri,
            headers,
            body,
        } = &self.fixture.exchanges[index].request;

        Request {
            method: method.clone(),
            uri: uri.clone(),
            headers: headers.clone(),
            body: body.clone().into_bytes(),
        }
    }

    fn recorded(&self, index: usize) -> Response {
        let response = &self.fixture.exchanges[index].response;

        Response {
            status: response.status,
            headers: response.headers.clone(),
            body: response.body.clone().into_bytes(),
        }
    }
}

impl SadtTransport for Replay {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        let step = self
            .steps
            .pop_front()
            .unwrap_or_else(|| panic!("unscripted request: {request:?}"));
        self.calls.push(request.clone());

        match step {
            Step::Pair(expected, answer) => {
                assert_eq!(request, self.expected(expected), "exchange {expected}");
                Ok(self.recorded(answer))
            }
            Step::ReplyTo(expected, result) => {
                assert_eq!(request, self.expected(expected), "exchange {expected}");
                result
            }
            Step::Any(result) => result,
            Step::Versions(result) => {
                assert_eq!(request.method, "GET", "version list method");
                assert_eq!(request.uri, versions_uri(), "version list uri");
                assert_eq!(
                    request.headers.get("Accept").map(String::as_str),
                    Some("application/atom+xml;type=feed")
                );
                result
            }
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes += 1;

        match &self.close_failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}
