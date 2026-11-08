use crate::adt::{ActivationBody, classify_activation, parse_messages};
use crate::backend::{BackendKind, Failure};
use crate::cli::{CreateKind, Subc};
use crate::hash::{local_source_lines, source_hash};
use crate::object::PackageScope;
use crate::operation::create::{CreateInput, creation_body};
use crate::operation::delete_adt::Opener;
use crate::operation::deploy::{
    CancelFlag, INTERRUPT_EXIT_CODE, LOCK_ACCEPT, LockInfo, activation_request_xml,
    encode_lock_handle, known_lock_handles, leaf_texts, parse_lock,
};
use crate::transport::{
    Request, Response, RunnerArm, SadtTransport, TransportError, runner_verdict,
};
use std::cell::RefCell;

/// The product default; the live test harness selects its own prefix through `runner_prefix`.
pub const DEFAULT_RUNNER_PREFIX: &str = "YCL_ABAPREACH_RUN_";

const SUFFIX_LENGTH: usize = 8;
const MAX_PREFIX_LENGTH: usize = 22;
const SUFFIX_ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// The largest multiple of 36 that fits a byte: a byte at or above it is redrawn, so every
/// alphabet character is equally likely.
const BIAS_LIMIT: u8 = 252;

/// The `runner_prefix` rule: upper-case letters, digits, and underscore, starting with `Y` or
/// `Z`, at most 22 characters, so the name stays within the 30 characters of a class name.
pub fn validate_runner_prefix(prefix: &str) -> Result<(), String> {
    if !prefix.starts_with(['Y', 'Z']) {
        return Err("must start with Y or Z".to_owned());
    }
    if prefix.len() > MAX_PREFIX_LENGTH {
        return Err(format!("must be at most {MAX_PREFIX_LENGTH} characters"));
    }
    if !prefix
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("may contain only upper-case letters, digits, and underscore".to_owned());
    }

    Ok(())
}

/// A fresh runner class name: `prefix` plus eight `[0-9A-Z]` characters from the OS random source.
pub fn runner_name(prefix: &str) -> String {
    runner_name_with(prefix, &mut |buffer| {
        getrandom::fill(buffer).expect("the OS random source is available")
    })
}

/// [`runner_name`] over any byte source, so the mapping is tested on every platform.
pub fn runner_name_with(prefix: &str, fill: &mut dyn FnMut(&mut [u8])) -> String {
    let mut name = String::from(prefix);
    let mut drawn = 0;

    while drawn < SUFFIX_LENGTH {
        let mut bytes = [0u8; SUFFIX_LENGTH];

        fill(&mut bytes);
        for byte in bytes.into_iter().filter(|byte| *byte < BIAS_LIMIT) {
            if drawn < SUFFIX_LENGTH {
                name.push(char::from(SUFFIX_ALPHABET[usize::from(byte) % 36]));
                drawn += 1;
            }
        }
    }

    name
}

/// Why a runner class could not be created.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// Both names were taken (HTTP 409); nothing was written.
    #[error("runner class name collided twice; retry later")]
    TwoCollisions,
    #[error("{0}")]
    Failed(TransportError),
}

/// Creates the runner class under a fresh name. A 409 means the name exists and nothing was
/// written, so one new name is tried; a second 409 or any other failure ends it.
pub fn create_with_fresh_name(
    mut new_name: impl FnMut() -> String,
    mut create: impl FnMut(&str) -> Result<(), TransportError>,
) -> Result<String, CreateError> {
    for attempt in 0..2 {
        let name = new_name();

        match create(&name) {
            Ok(()) => return Ok(name),
            Err(TransportError::Http { status: 409, .. }) if attempt == 0 => {}
            Err(TransportError::Http { status: 409, .. }) => {
                return Err(CreateError::TwoCollisions);
            }
            Err(error) => return Err(CreateError::Failed(error)),
        }
    }

    unreachable!("the second attempt always returns")
}

/// The ADT HTTP side of the runner boundary. ADT HTTP has no admission layer, so this wraps the
/// transport and applies the same runner rules as the RFC-SADT admission: the classrun POST, and
/// the create of the armed runner class. Every other request passes unchanged.
pub struct RunnerGated<'a> {
    inner: &'a mut dyn SadtTransport,
}

impl<'a> RunnerGated<'a> {
    pub fn new(inner: &'a mut dyn SadtTransport) -> Self {
        Self { inner }
    }
}

impl SadtTransport for RunnerGated<'_> {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if runner_verdict(&request) == Some(false) {
            return Err(TransportError::RequestNotAllowed(format!(
                "{} {}",
                request.method, request.uri
            )));
        }

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.inner.stateful_supported()
    }
}

const RUNNER_DESCRIPTION: &str = "ABAPReach temporary runner";
const CLASSRUN: &str = "/sap/bc/adt/oo/classrun";
const DISCOVERY: &str = "/sap/bc/adt/discovery";
const AUTHORIZATION_HINT: &str =
    "the temporary runner class needs CLAS create/activate in $TMP and ADT classrun access";
const STATELESS: &str = "ADT HTTP session is stateless on this release; the temporary runner class \
                         needs a stateful session (use the RFC path)";
const TEMPLATE_BUG: &str =
    "W: the generated runner class did not activate; please report this as a bug";
const MASK: &str = "<lock handle>";
/// The most of a SAP message that reaches a diagnostic.
const MESSAGE_LIMIT: usize = 200;

/// The transport that carries the classrun POST, as printed on the `RUNNER:` line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostTransport {
    Sadt,
    Http,
}

impl PostTransport {
    pub fn token(self) -> &'static str {
        match self {
            Self::Sadt => "sadt",
            Self::Http => "http",
        }
    }
}

/// The transport of the POST for a release: ADT HTTP posts over HTTP; RFC-SADT posts over SADT
/// when the POST passed there on this release (fact U2) and over HTTP when it did not.
pub fn post_transport(backend: BackendKind, sadt_post_works: bool) -> PostTransport {
    match backend {
        BackendKind::RfcSadt if sadt_post_works => PostTransport::Sadt,
        BackendKind::AdtHttp | BackendKind::RfcSadt => PostTransport::Http,
    }
}

/// Where the classrun POST goes. The class cycle never leaves its backend; only the POST may.
pub enum PostRoute<'a> {
    /// The session of the class cycle.
    Cycle,
    /// A new ADT HTTP session opened for the POST alone and logged off right after it.
    Http(&'a mut Opener<'a>),
}

/// What one row asks of the cycle.
pub struct CycleInput<'a> {
    /// `<op> <TYPE>`, such as `deploy DTEL`, for the STOP texts.
    pub operation: &'a str,
    pub sid: &'a str,
    /// The connection id of the cleanup commands.
    pub conn: &'a str,
    pub prefix: &'a str,
    pub user: &'a str,
    pub language: &'a str,
    /// The backend of the class cycle.
    pub backend: BackendKind,
    /// Whether the connection has an RFC route, which decides the text of a stateless HTTP stop.
    pub rfc_available: bool,
    /// The full class source for a runner name.
    pub build: &'a dyn Fn(&str) -> Result<String, String>,
}

/// How a row ended, before the cleanup is taken into account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunnerEnd {
    /// The POST was answered. The status and the body are for the protocol parser.
    Posted(Response),
    /// The POST was sent and its reply is lost or broken: reconcile, never resend.
    PostUncertain(TransportError),
    /// A stage before the POST failed (`runner <stage> failed: <SAP text>`); exit 1.
    Rejected(String),
    /// A condition that stops the command (exit 2), with the text after `STOP: `.
    Stop(String),
    /// A signal arrived before the POST; nothing ran (exit 130).
    Interrupted,
    /// Nothing was sent: the open or the first request failed before any write, so another
    /// backend may still take the row.
    NotStarted(TransportError),
}

/// The outcome of one row's cycle.
#[derive(Clone, Debug)]
pub struct RunnerRun {
    /// `None` when the row ended before a name was generated.
    pub name: Option<String>,
    pub post: PostTransport,
    pub end: RunnerEnd,
    /// The class that could not be removed or proven gone.
    pub leftover: Option<String>,
    /// `W:` lines for the stderr, before the verdict.
    pub notes: Vec<String>,
    /// A signal or a closed input arrived during the row.
    pub interrupted: bool,
    conn: String,
}

impl RunnerRun {
    /// The `RUNNER:` line of every DDIC and `TRAN` output.
    pub fn runner_line(&self) -> Option<String> {
        self.name.as_ref().map(|name| {
            format!(
                "RUNNER: {name} (temporary, $TMP, post via {}, removed after the run)",
                self.post.token()
            )
        })
    }

    /// `RUNNER_LEFTOVER` and the two exact cleanup commands, for the stderr.
    pub fn leftover_text(&self) -> Option<String> {
        self.leftover.as_ref().map(|name| {
            format!(
                "RUNNER_LEFTOVER: {name}\n\
                 abapreach delete --simulate CLAS:{name} --plan <FILE> --conn {conn}\n\
                 abapreach delete CLAS:{name} --plan <FILE> --conn {conn}",
                conn = self.conn
            )
        })
    }

    /// Whether the later rows of a batch are skipped: a class that stays, a signal, or a stop.
    pub fn halts_batch(&self) -> bool {
        self.leftover.is_some() || self.interrupted || matches!(self.end, RunnerEnd::Stop(_))
    }

    /// The exit code of the row. `posted_exit` is the code the operation derived from a POST
    /// that was answered or lost. A signal wins, then a leftover (exit 1 on any operation).
    pub fn exit_code(&self, posted_exit: i32) -> i32 {
        if self.interrupted {
            return INTERRUPT_EXIT_CODE;
        }

        if self.leftover.is_some() {
            return 1;
        }

        match self.end {
            RunnerEnd::Posted(_) | RunnerEnd::PostUncertain(_) => posted_exit,
            RunnerEnd::Rejected(_) | RunnerEnd::NotStarted(_) => 1,
            RunnerEnd::Stop(_) => 2,
            RunnerEnd::Interrupted => INTERRUPT_EXIT_CODE,
        }
    }
}

/// A row of a batch: it ran, or an earlier row stopped the batch.
#[derive(Clone, Debug)]
pub enum Row {
    Ran(RunnerRun),
    Skipped,
}

/// Runs the rows in order; the first row that halts the batch makes every later row `Skipped`.
pub fn run_rows<T>(rows: &[T], mut run: impl FnMut(&T) -> RunnerRun) -> Vec<Row> {
    let mut halted = false;

    rows.iter()
        .map(|row| {
            if halted {
                return Row::Skipped;
            }

            let run = run(row);

            halted = run.halts_batch();

            Row::Ran(run)
        })
        .collect()
}

/// What is known about the class on the server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    /// Never sent, or refused before it could exist.
    Absent,
    /// The create was sent and its outcome is not settled.
    Maybe,
    Present,
}

/// Runs the whole cycle for one row: create the class in `$TMP`, write the source, activate,
/// check the active source, POST classrun, delete the class, and prove from a new session that
/// it is gone. `open` yields a session of the cycle backend; the cycle opens its own session and
/// a new one for each proof, so every row works in a fresh session. Every path after the class
/// exists tries to delete it, and a class that stays is reported as the leftover of the run.
pub fn run_cycle(
    input: &CycleInput<'_>,
    open: &mut Opener<'_>,
    post: PostRoute<'_>,
    cancel: &CancelFlag,
) -> RunnerRun {
    let post_via = match post {
        PostRoute::Http(_) => PostTransport::Http,
        PostRoute::Cycle => post_transport(input.backend, true),
    };
    let mut run = RunnerRun {
        name: None,
        post: post_via,
        end: RunnerEnd::Interrupted,
        leftover: None,
        notes: Vec::new(),
        interrupted: false,
        conn: input.conn.to_owned(),
    };

    if cancel.is_set() {
        run.interrupted = true;

        return run;
    }

    let session = match open() {
        Ok(session) => session,
        Err(error) => {
            run.end = RunnerEnd::NotStarted(error);

            return run;
        }
    };
    // From here a signal must not end the process: the class has to be removed first.
    let _write = cancel.hold_write();
    let mut cycle = Cycle {
        input,
        cancel,
        session,
        name: None,
        class: Class::Absent,
        held: None,
        arm: None,
        notes: Vec::new(),
        fatal: None,
        source: None,
        expected: None,
    };
    let end = match cycle.steps(post) {
        Ok(end) | Err(end) => end,
    };
    let leftover = cycle.cleanup(open);

    if let Err(error) = cycle.session.close() {
        cycle
            .notes
            .push(format!("W: session close failed: {error}"));
    }

    run.name = cycle.name.take();
    run.end = end;
    run.leftover = leftover.then(|| run.name.clone()).flatten();
    run.notes = std::mem::take(&mut cycle.notes);
    run.interrupted = cancel.is_set();

    run
}

struct Cycle<'a> {
    input: &'a CycleInput<'a>,
    cancel: &'a CancelFlag,
    session: Box<dyn SadtTransport>,
    name: Option<String>,
    class: Class,
    /// The lock handle that is still held on the class.
    held: Option<LockInfo>,
    arm: Option<RunnerArm>,
    notes: Vec<String>,
    /// An end decided while the name loop ran.
    fatal: Option<RunnerEnd>,
    /// The class source for the current name, and the hash the active source must have.
    source: Option<String>,
    expected: Option<String>,
}

impl Cycle<'_> {
    fn steps(&mut self, post: PostRoute<'_>) -> Result<RunnerEnd, RunnerEnd> {
        self.preflight()?;
        self.checkpoint()?;
        self.create()?;
        self.checkpoint()?;

        let lock = self.lock_class()?;

        self.held = Some(lock);
        self.write_source()?;
        self.checkpoint()?;
        self.unlock()?;
        self.checkpoint()?;
        self.activate()?;
        self.checkpoint()?;
        self.verify_source()?;
        self.checkpoint()?;

        Ok(self.post(post))
    }

    fn checkpoint(&self) -> Result<(), RunnerEnd> {
        if self.cancel.is_set() {
            return Err(RunnerEnd::Interrupted);
        }

        Ok(())
    }

    /// Requests go through the ADT HTTP gate on that backend; RFC-SADT has its own admission.
    fn send(&mut self, request: Request) -> Result<Response, TransportError> {
        if self.input.backend == BackendKind::AdtHttp {
            RunnerGated::new(&mut *self.session).request(request)
        } else {
            self.session.request(request)
        }
    }

    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or_default()
    }

    fn object_uri(&self) -> String {
        format!(
            "/sap/bc/adt/oo/classes/{}",
            self.name().to_ascii_lowercase()
        )
    }

    /// An ADT HTTP session must be stateful, and the first stateful reply shows whether it is:
    /// a read of the discovery document, before anything is written. RFC-SADT keeps its state
    /// in the connection and needs no probe.
    fn preflight(&mut self) -> Result<(), RunnerEnd> {
        if self.input.backend != BackendKind::AdtHttp {
            return Ok(());
        }

        let mut request = Request::get(DISCOVERY);

        request
            .headers
            .insert("Accept".to_owned(), "application/*".to_owned());

        let response = self.send(request).map_err(RunnerEnd::NotStarted)?;

        if let Some(error) = response.authorization_failure() {
            return Err(RunnerEnd::NotStarted(error));
        }

        match response.status {
            403 => return Err(RunnerEnd::Rejected(self.refused("preflight", &response))),
            200..=299 => {}
            _ => return Err(RunnerEnd::Rejected(self.failed("preflight", &response))),
        }

        if self.session.stateful_supported() == Some(false) {
            return Err(RunnerEnd::Stop(if self.input.rfc_available {
                STATELESS.to_owned()
            } else {
                format!(
                    "{} needs a stateful ADT session or the RFC path",
                    self.input.operation
                )
            }));
        }

        Ok(())
    }

    fn needs_classrun(&self) -> String {
        format!(
            "{} needs ADT classrun; not available on {}",
            self.input.operation, self.input.sid
        )
    }

    /// Arms the admission for each generated name and creates the class; a taken name is
    /// replaced once.
    fn create(&mut self) -> Result<(), RunnerEnd> {
        let armed = RefCell::new(None::<RunnerArm>);
        let refused = RefCell::new(None::<String>);
        let prefix = self.input.prefix;
        let result = create_with_fresh_name(
            || {
                armed.borrow_mut().take();

                let name = runner_name(prefix);

                match RunnerArm::arm(&name) {
                    Ok(arm) => *armed.borrow_mut() = Some(arm),
                    Err(message) => *refused.borrow_mut() = Some(message),
                }

                name
            },
            |name| {
                if let Some(message) = refused.borrow_mut().take() {
                    self.fatal = Some(RunnerEnd::Stop(format!("runner admission: {message}")));

                    return Err(TransportError::Protocol(message));
                }

                self.send_create(name)
            },
        );

        self.arm = armed.into_inner();

        if let Some(end) = self.fatal.take() {
            return Err(end);
        }

        match result {
            Ok(_) => Ok(()),
            Err(CreateError::TwoCollisions) => {
                Err(RunnerEnd::Stop(CreateError::TwoCollisions.to_string()))
            }
            Err(CreateError::Failed(error)) => Err(self.create_failure(&error)),
        }
    }

    /// One create POST. A 409 leaves nothing; a 4xx leaves nothing; a 5xx or a lost reply may.
    fn send_create(&mut self, name: &str) -> Result<(), TransportError> {
        self.name = Some(name.to_owned());

        let source = match (self.input.build)(name) {
            Ok(source) => source,
            Err(message) => {
                self.fatal = Some(RunnerEnd::Stop(message.clone()));

                return Err(TransportError::Protocol(message));
            }
        };

        self.expected = Some(source_hash(&local_source_lines(&source)));
        self.source = Some(source);

        let create = CreateInput {
            kind: CreateKind::Clas,
            name: name.to_owned(),
            text: RUNNER_DESCRIPTION.to_owned(),
            package: Some("$TMP".to_owned()),
            group: None,
            tr: None,
            simulate: false,
            subc: Subc::Program,
            source: None,
        };
        let body = match creation_body(&create, "$TMP", self.input.user, self.input.language) {
            Ok(body) => body,
            Err(message) => {
                self.fatal = Some(RunnerEnd::Stop(message.clone()));

                return Err(TransportError::Protocol(message));
            }
        };
        let uri = CreateKind::Clas
            .creation_request_uri(None, &PackageScope::Local)
            .map_err(TransportError::Protocol)?;
        let mut request = Request::post(uri, body.into_bytes());

        request
            .headers
            .insert("Content-Type".to_owned(), "application/*".to_owned());
        self.class = Class::Maybe;

        match self.send(request) {
            Ok(response) if ok(&response) => {
                self.class = Class::Present;

                Ok(())
            }
            Ok(response) => {
                if response.status < 500 {
                    self.class = Class::Absent;
                }

                Err(TransportError::Http {
                    status: response.status,
                    message: self.message_of(&response),
                })
            }
            Err(error) => {
                if !error.was_sent() {
                    self.class = Class::Absent;
                }

                Err(error)
            }
        }
    }

    fn create_failure(&mut self, error: &TransportError) -> RunnerEnd {
        if matches!(error, TransportError::Http { status: 403, .. })
            || Failure::from(error) == Failure::Authorization
        {
            return RunnerEnd::Rejected(self.with_hint("create", error));
        }

        if self.class == Class::Absent && !matches!(error, TransportError::Http { .. }) {
            return RunnerEnd::NotStarted(error.clone());
        }

        RunnerEnd::Rejected(self.stage_text("create", error))
    }

    fn lock_class(&mut self) -> Result<LockInfo, RunnerEnd> {
        let mut request = Request::post(
            format!("{}?_action=LOCK&accessMode=MODIFY", self.object_uri()),
            Vec::new(),
        );

        request
            .headers
            .insert("Accept".to_owned(), LOCK_ACCEPT.to_owned());

        match self.send(request) {
            Ok(response) if ok(&response) => parse_lock(&String::from_utf8_lossy(&response.body))
                .map_err(|message| {
                    RunnerEnd::Rejected(format!(
                        "runner lock failed: unreadable lock response: {message}"
                    ))
                }),
            Ok(response) => Err(self.reject("lock", &response)),
            Err(error) => Err(self.reject_error("lock", &error)),
        }
    }

    fn write_source(&mut self) -> Result<(), RunnerEnd> {
        let handle = self
            .held
            .as_ref()
            .map(|lock| encode_lock_handle(&lock.handle))
            .unwrap_or_default();
        let source = self.source.clone().unwrap_or_default();
        let mut request = Request::put(
            format!("{}/source/main?lockHandle={handle}", self.object_uri()),
            source.into_bytes(),
        );

        request.headers.insert(
            "Content-Type".to_owned(),
            "text/plain; charset=utf-8".to_owned(),
        );

        match self.send(request) {
            Ok(response) if ok(&response) => Ok(()),
            Ok(response) => Err(self.reject("PUT", &response)),
            Err(error) => Err(self.reject_error("PUT", &error)),
        }
    }

    fn unlock(&mut self) -> Result<(), RunnerEnd> {
        let Some(lock) = self.held.as_ref() else {
            return Ok(());
        };
        let request = Request::post(
            format!(
                "{}?_action=UNLOCK&lockHandle={}",
                self.object_uri(),
                encode_lock_handle(&lock.handle)
            ),
            Vec::new(),
        );

        match self.send(request) {
            Ok(response) if ok(&response) => {
                self.held = None;

                Ok(())
            }
            Ok(response) => Err(self.reject("unlock", &response)),
            Err(error) => Err(self.reject_error("unlock", &error)),
        }
    }

    fn activate(&mut self) -> Result<(), RunnerEnd> {
        let mut request = Request::post(
            "/sap/bc/adt/activation?method=activate&preauditRequested=true",
            activation_request_xml(self.name(), &self.object_uri()).into_bytes(),
        );

        request
            .headers
            .insert("Content-Type".to_owned(), "application/xml".to_owned());
        request
            .headers
            .insert("Accept".to_owned(), "application/xml".to_owned());

        let response = match self.send(request) {
            Ok(response) if ok(&response) => response,
            Ok(response) => return Err(self.reject("activate", &response)),
            Err(error) => return Err(self.reject_error("activate", &error)),
        };
        let text = String::from_utf8_lossy(&response.body).into_owned();

        if classify_activation(&text) == ActivationBody::Worklist {
            return Err(RunnerEnd::Rejected(
                "runner activate failed: the server answered a worklist and activated nothing"
                    .to_owned(),
            ));
        }

        let messages = parse_messages(&text).unwrap_or_default();

        if messages
            .iter()
            .any(|message| message.is_error() && mentions_interface(&message.text))
        {
            return Err(RunnerEnd::Stop(self.needs_classrun()));
        }

        match messages.iter().find(|message| message.is_error()) {
            Some(message) => {
                self.notes.push(TEMPLATE_BUG.to_owned());

                Err(RunnerEnd::Rejected(format!(
                    "runner activate failed: {}",
                    cut(&self.mask(&message.render()))
                )))
            }
            None => Ok(()),
        }
    }

    /// The active source must be the generated one: this closes a class that changed between
    /// the unlock and the POST, and a foreign class of the same name.
    fn verify_source(&mut self) -> Result<(), RunnerEnd> {
        let mut request = Request::get(format!("{}/source/main?version=active", self.object_uri()));

        request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());

        let response = match self.send(request) {
            Ok(response) if ok(&response) => response,
            Ok(response) => return Err(self.reject("source check", &response)),
            Err(error) => return Err(self.reject_error("source check", &error)),
        };
        let text = String::from_utf8_lossy(&response.body);

        if Some(source_hash(&local_source_lines(&text))) == self.expected {
            return Ok(());
        }

        Err(RunnerEnd::Rejected(
            "runner source check failed: the active class source differs from the generated source"
                .to_owned(),
        ))
    }

    /// The single classrun POST. It is never repeated, on any transport.
    fn post(&mut self, route: PostRoute<'_>) -> RunnerEnd {
        let mut request = Request::post(format!("{CLASSRUN}/{}", self.name()), Vec::new());

        request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());

        let result = match route {
            PostRoute::Cycle => self.send(request),
            PostRoute::Http(open) => match open() {
                Ok(mut session) => {
                    let result = RunnerGated::new(&mut *session).request(request);

                    if let Err(error) = session.close() {
                        self.notes.push(format!("W: session close failed: {error}"));
                    }

                    result
                }
                Err(error) => {
                    return RunnerEnd::Rejected(format!(
                        "runner POST failed: cannot open the POST session: {error}"
                    ));
                }
            },
        };

        match result {
            Ok(response) if response.status == 404 => RunnerEnd::Stop(self.needs_classrun()),
            Ok(response)
                if response.status == 403 || response.authorization_failure().is_some() =>
            {
                RunnerEnd::Rejected(self.refused("POST", &response))
            }
            Ok(response) => RunnerEnd::Posted(response),
            Err(error) if Failure::from(&error) == Failure::Authorization => {
                RunnerEnd::Rejected(self.with_hint("POST", &error))
            }
            Err(error) if error.was_sent() => RunnerEnd::PostUncertain(error),
            Err(error) => RunnerEnd::Rejected(self.stage_text("POST", &error)),
        }
    }

    /// Removes the class and proves from a new session that it is gone; `true` when it stays.
    fn cleanup(&mut self, open: &mut Opener<'_>) -> bool {
        if self.class == Class::Maybe {
            match self.exists(open) {
                Ok(false) => self.class = Class::Absent,
                Ok(true) => self.class = Class::Present,
                Err(_) => return true,
            }
        }

        if self.class == Class::Absent {
            return false;
        }

        if !self.delete_class() {
            return true;
        }

        !matches!(self.exists(open), Ok(false))
    }

    /// Sends the DELETE; `false` when it could not be sent at all (no lock). A sent DELETE is
    /// judged by the proof, whatever it answered.
    fn delete_class(&mut self) -> bool {
        let lock = match self.held.take() {
            Some(lock) => lock,
            None => match self.lock_class() {
                Ok(lock) => lock,
                Err(_) => return false,
            },
        };
        let request = Request::delete(format!(
            "{}?lockHandle={}",
            self.object_uri(),
            encode_lock_handle(&lock.handle)
        ));

        if matches!(self.send(request), Ok(response) if ok(&response)) {
            self.end_stateful_session();
        }

        true
    }

    /// A DELETE answered 2xx ends the stateful session with its logoff.
    fn end_stateful_session(&mut self) {
        if self.session.stateful_supported() != Some(true) {
            return;
        }

        if let Err(error) = self.session.close() {
            self.notes.push(format!("W: session close failed: {error}"));
        }
    }

    /// Whether the class exists, read from a new session that is closed again.
    fn exists(&mut self, open: &mut Opener<'_>) -> Result<bool, String> {
        let mut fresh = open().map_err(|error| error.to_string())?;
        let mut request = Request::get(self.object_uri());

        request
            .headers
            .insert("Accept".to_owned(), "application/*".to_owned());

        let result = fresh.request(request);

        if let Err(error) = fresh.close() {
            self.notes.push(format!("W: session close failed: {error}"));
        }

        match result {
            Ok(response) if response.status == 200 => Ok(true),
            Ok(response) if response.status == 404 => Ok(false),
            Ok(response) => Err(format!("HTTP {}", response.status)),
            Err(error) => Err(error.to_string()),
        }
    }

    fn reject(&self, stage: &str, response: &Response) -> RunnerEnd {
        if response.status == 403 || response.authorization_failure().is_some() {
            return RunnerEnd::Rejected(self.refused(stage, response));
        }

        RunnerEnd::Rejected(self.failed(stage, response))
    }

    fn reject_error(&self, stage: &str, error: &TransportError) -> RunnerEnd {
        if Failure::from(error) == Failure::Authorization {
            return RunnerEnd::Rejected(self.with_hint(stage, error));
        }

        RunnerEnd::Rejected(self.stage_text(stage, error))
    }

    fn failed(&self, stage: &str, response: &Response) -> String {
        self.stage_text(
            stage,
            &TransportError::Http {
                status: response.status,
                message: self.message_of(response),
            },
        )
    }

    fn refused(&self, stage: &str, response: &Response) -> String {
        format!("{}; {AUTHORIZATION_HINT}", self.failed(stage, response))
    }

    fn with_hint(&self, stage: &str, error: &TransportError) -> String {
        format!("{}; {AUTHORIZATION_HINT}", self.stage_text(stage, error))
    }

    fn stage_text(&self, stage: &str, error: &TransportError) -> String {
        let text = match error {
            TransportError::Http { status, message } if message.is_empty() => {
                format!("HTTP {status}")
            }
            other => other.to_string(),
        };

        format!("runner {stage} failed: {}", self.mask(&text))
    }

    /// The SAP message of an error body, on one line and cut.
    fn message_of(&self, response: &Response) -> String {
        let texts = leaf_texts(&String::from_utf8_lossy(&response.body)).unwrap_or_default();
        let message = ["localizedMessage", "message"]
            .iter()
            .find_map(|key| texts.get(*key))
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();

        cut(&self.mask(&message))
    }

    /// Removes every lock handle seen so far, plain and encoded, from a diagnostic.
    fn mask(&self, text: &str) -> String {
        known_lock_handles()
            .into_iter()
            .fold(text.to_owned(), |text, handle| text.replace(&handle, MASK))
    }
}

/// An activation error about the classrun interface itself: the system does not have it.
fn mentions_interface(text: &str) -> bool {
    text.to_ascii_uppercase().contains("IF_OO_ADT_CLASSRUN")
}

fn ok(response: &Response) -> bool {
    (200..300).contains(&response.status)
}

fn cut(text: &str) -> String {
    text.chars().take(MESSAGE_LIMIT).collect()
}
