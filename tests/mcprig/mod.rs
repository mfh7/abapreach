//! Shared rig of the Task 10 MCP tests: fake transports and factories, a fake session (the part
//! of the client a tool call can reach) and a gate with a clock the test moves.

#![allow(dead_code)]

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{Answer, Form, Session, ToolHandler, ToolResult};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{Limits, McpTools, TokenGate};
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::http::HttpTransport;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const PASSWORD: &str = "secret-pw";
pub const HOST: &str = "example.com";
pub const PROGRAM: &str = "REPORT ytest_abapreach01.\n\nWRITE / 'v2'.";
pub const PROG: &str = "/sap/bc/adt/programs/programs/zprog";
pub const HANDLE: &str = "a/b+c=";
pub const ENCODED: &str = "a%2Fb%2Bc%3D";

pub type Reply = Result<Response, TransportError>;
pub type Opened = Result<Box<dyn SadtTransport>, TransportError>;
pub type Log = Rc<RefCell<Vec<String>>>;
pub type Hook = Box<dyn FnMut(&Request)>;

pub fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

/// A scripted ADT: replies per method and exact URI, in order; an unscripted request is a
/// transport error. Every request is logged with its body.
pub struct Script {
    rules: Vec<(String, String, VecDeque<Reply>)>,
    log: Log,
    /// Runs before the reply is chosen: the place to cancel, close the input or sleep.
    hook: Option<Hook>,
}

impl Script {
    pub fn new(log: &Log) -> Self {
        Self {
            rules: Vec::new(),
            log: log.clone(),
            hook: None,
        }
    }

    pub fn on(mut self, method: &str, uri: &str, reply: Reply) -> Self {
        match self
            .rules
            .iter_mut()
            .find(|(rule_method, rule_uri, _)| rule_method == method && rule_uri == uri)
        {
            Some((_, _, replies)) => replies.push_back(reply),
            None => self
                .rules
                .push((method.to_owned(), uri.to_owned(), VecDeque::from([reply]))),
        }

        self
    }

    pub fn get(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("GET", uri, Ok(Response::text(status, body)))
    }

    pub fn post(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("POST", uri, Ok(Response::text(status, body)))
    }

    pub fn hook(mut self, hook: impl FnMut(&Request) + 'static) -> Self {
        self.hook = Some(Box::new(hook));

        self
    }
}

impl SadtTransport for Script {
    fn request(&mut self, request: Request) -> Reply {
        self.log.borrow_mut().push(format!(
            "{} {} {}",
            request.method,
            request.uri,
            String::from_utf8_lossy(&request.body)
        ));

        if let Some(hook) = self.hook.as_mut() {
            hook(&request);
        }

        self.rules
            .iter_mut()
            .find(|(method, uri, _)| *method == request.method && *uri == request.uri)
            .and_then(|(_, _, replies)| replies.pop_front())
            .unwrap_or_else(|| {
                Err(TransportError::RequestNotAllowed(format!(
                    "unscripted {} {}",
                    request.method, request.uri
                )))
            })
    }

    fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

/// Runs `hook` before every request of `inner`.
pub struct Hooked<T> {
    pub inner: T,
    pub hook: Hook,
}

impl<T: SadtTransport> SadtTransport for Hooked<T> {
    fn request(&mut self, request: Request) -> Reply {
        (self.hook)(&request);

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }
}

#[derive(Default)]
pub struct Fake {
    pub adt: VecDeque<Opened>,
    pub rfc: VecDeque<Opened>,
    pub adt_opened: Rc<Cell<usize>>,
    pub rfc_opened: Rc<Cell<usize>>,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened.set(self.adt_opened.get() + 1);
        self.adt.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no ADT transport scripted".to_owned(),
            ))
        })
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened.set(self.rfc_opened.get() + 1);
        self.rfc.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no RFC transport scripted".to_owned(),
            ))
        })
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        Err("no RFC library in a test".to_owned())
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        Ok(())
    }
}

/// What the human answers and what the server asked, in order.
pub struct FakeSession {
    pub can_elicit: bool,
    pub cancel: CancelFlag,
    pub answers: VecDeque<Answer>,
    pub forms: Vec<(Form, Duration)>,
    pub logs: Vec<String>,
    /// Runs while the elicitation is open.
    pub while_open: Option<Box<dyn FnMut()>>,
}

impl FakeSession {
    pub fn new(can_elicit: bool) -> Self {
        Self {
            can_elicit,
            cancel: CancelFlag::new(),
            answers: VecDeque::new(),
            forms: Vec::new(),
            logs: Vec::new(),
            while_open: None,
        }
    }

    /// A client that can elicit and types `text` into the form.
    pub fn typing(text: &str) -> Self {
        let mut session = Self::new(true);

        session.answers.push_back(Answer::Accepted(text.to_owned()));
        session
    }

    pub fn asked(&self) -> usize {
        self.forms.len()
    }
}

impl Session for FakeSession {
    fn can_elicit(&self) -> bool {
        self.can_elicit
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }

    fn elicit(
        &mut self,
        form: &Form,
        timeout: Duration,
        _read: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
    ) -> Answer {
        self.forms.push((form.clone(), timeout));

        if let Some(open) = self.while_open.as_mut() {
            open();
        }

        self.answers.pop_front().unwrap_or(Answer::Declined)
    }

    fn log(&mut self, line: &str) {
        self.logs.push(line.to_owned());
    }
}

/// A clock the test moves.
#[derive(Clone)]
pub struct Clock {
    start: Instant,
    offset: Rc<Cell<Duration>>,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            offset: Rc::new(Cell::new(Duration::ZERO)),
        }
    }

    pub fn advance(&self, by: Duration) {
        self.offset.set(self.offset.get() + by);
    }

    pub fn gate(&self) -> TokenGate {
        let clock = self.clone();

        TokenGate::with_clock(Rc::new(move || clock.start + clock.offset.get()))
    }
}

pub struct Rig {
    pub fake: Fake,
    pub env: BTreeMap<String, String>,
    pub gate: TokenGate,
    pub clock: Clock,
    pub limits: Limits,
    _directory: tempfile::TempDir,
}

/// One server, DEV client 100, with ADT and RFC and the given `policies` JSON.
pub fn rig(policies: &str) -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");
    let config = format!(
        r#"{{"servers":[
            {{"systemId":"DEV","host":"{HOST}","port":443,"clients":["100"],"rfc":{{"ashost":"{HOST}","sysnr":"00","saprouter":"/H/router.example.net/S/3299/H/"}},"adt":{{"insecure":true}},"policies":{policies}}}
        ]}}"#
    );

    std::fs::write(&path, config).unwrap();

    let mut env = BTreeMap::from([("ABAP_CONNECTIONS".to_owned(), path.display().to_string())]);

    env.insert("DEV_100_USER".to_owned(), "user".to_owned());
    env.insert("DEV_100_PASS".to_owned(), PASSWORD.to_owned());

    let clock = Clock::new();

    Rig {
        fake: Fake::default(),
        env,
        gate: clock.gate(),
        clock,
        limits: Limits::default(),
        _directory: directory,
    }
}

impl Rig {
    pub fn config(&self) -> Config {
        Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap()
    }

    pub fn adt(&mut self, transport: impl SadtTransport + 'static) {
        self.fake.adt.push_back(Ok(Box::new(transport)));
    }

    pub fn rfc(&mut self, transport: impl SadtTransport + 'static) {
        self.fake.rfc.push_back(Ok(Box::new(transport)));
    }

    pub fn tools(&mut self) -> McpTools<'_, TokenGate> {
        McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.fake),
            self.gate.clone(),
        )
        .with_limits(self.limits)
    }

    pub fn call(&mut self, session: &mut FakeSession, name: &str, arguments: Value) -> ToolResult {
        self.tools()
            .call_in(name, arguments.as_object().unwrap(), session)
    }

    /// A call without any client behind it: no elicitation, no cancellation.
    pub fn bare(&mut self, name: &str, arguments: Value) -> ToolResult {
        self.tools().call(name, arguments.as_object().unwrap())
    }

    pub fn opened(&self) -> (usize, usize) {
        (self.fake.adt_opened.get(), self.fake.rfc_opened.get())
    }
}

pub fn deploy_arguments() -> Value {
    json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST_ABAPREACH01", "source": PROGRAM})
}

pub fn with_token(mut arguments: Value, token: &str) -> Value {
    arguments["plan_token"] = json!(token);

    arguments
}

pub fn token_of(result: &ToolResult) -> String {
    result.structured["plan_token"]
        .as_str()
        .unwrap_or_else(|| panic!("no plan_token in {}", result.text))
        .to_owned()
}

pub fn lock_xml() -> String {
    format!(
        "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA>\
         <LOCK_HANDLE>{HANDLE}</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>\
         </DATA></asx:values></asx:abap>"
    )
}

pub fn metadata(responsible: &str) -> String {
    format!(
        "<x:object xmlns:x=\"urn:x\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
         adtcore:responsible=\"{responsible}\"><adtcore:packageRef adtcore:name=\"$TMP\"/>\
         </x:object>"
    )
}

/// The read-only requests of a delete of `PROG ZPROG`.
pub fn prog_reads(requests: &Log, responsible: &str) -> Script {
    Script::new(requests)
        .get(PROG, 200, &metadata(responsible))
        .get(
            &format!("{PROG}/source/main?version=active"),
            200,
            "REPORT zprog.",
        )
        .get(
            &format!("{PROG}/source/main/versions"),
            200,
            "<feed xmlns=\"http://www.w3.org/2005/Atom\"/>",
        )
        .post(
            &abapreach::adt::usage_references_uri(PROG),
            200,
            "<usageReferences:usageReferenceResult xmlns:usageReferences=\"u\"/>",
        )
}

/// Logs every request of `inner` as `METHOD uri` into `log` and lets it answer.
pub fn recording<T: SadtTransport>(inner: T, log: &Log) -> Hooked<T> {
    let log = log.clone();

    Hooked {
        inner,
        hook: Box::new(move |request| {
            log.borrow_mut()
                .push(format!("{} {}", request.method, request.uri));
        }),
    }
}

pub fn any(log: &Log, needle: &str) -> bool {
    log.borrow().iter().any(|line| line.contains(needle))
}

pub const LOGOFF_URI: &str = "/sap/public/bc/icf/logoff";

/// Gives every reply of `inner` the context cookie of a stateful ADT session.
pub struct Cookied<T>(pub T);

impl<T: SadtTransport> SadtTransport for Cookied<T> {
    fn request(&mut self, request: Request) -> Reply {
        self.0.request(request).map(|mut response| {
            response.headers.insert(
                "Set-Cookie".to_owned(),
                "SAP_SESSIONID_DEV_100=s1; path=/; sap-contextid=ctx".to_owned(),
            );

            response
        })
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.0.close()
    }
}

/// A real `HttpTransport` whose network is `inner`, so the logoff it sends reaches `inner`.
pub fn http(inner: impl SadtTransport + 'static) -> HttpTransport {
    HttpTransport::wrapping(Cookied(inner))
}

/// How many logoff requests the log holds.
pub fn logoffs(log: &Log) -> usize {
    log.borrow()
        .iter()
        .filter(|line| line.starts_with(&format!("GET {LOGOFF_URI}")))
        .count()
}
