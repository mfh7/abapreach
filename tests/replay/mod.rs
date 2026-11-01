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
}

/// Expect and answer with the same fixture exchange.
pub fn fx(index: usize) -> Step {
    Step::Pair(index, index)
}

pub struct Replay {
    pub calls: Vec<Request>,
    pub closes: usize,
    fixture: Fixture,
    steps: VecDeque<Step>,
}

pub fn replay(fixture: &str, steps: Vec<Step>) -> Replay {
    let text = fs::read_to_string(fixture_path(fixture)).unwrap();

    Replay {
        calls: Vec::new(),
        closes: 0,
        fixture: serde_json::from_str(&text).unwrap(),
        steps: steps.into(),
    }
}

impl Replay {
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
        }
    }

    fn close(&mut self) {
        self.closes += 1;
    }
}
