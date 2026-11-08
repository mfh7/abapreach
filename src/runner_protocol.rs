//! The output protocol of the temporary runner class and the strict parser for it.
//!
//! The class writes machine lines; Rust trusts nothing else. Grammar, one line each:
//!
//! ```text
//! ABAPREACH:<nonce>:BEGIN:<MODE>
//! ABAPREACH:<nonce>:MSG:<I|W|E>:<text>
//! ABAPREACH:<nonce>:LINE:<text>
//! ABAPREACH:<nonce>:FACT:<KEY>:<text>
//! ABAPREACH:<nonce>:PARTIAL_DELETE:<text>
//! ABAPREACH:<nonce>:OBJECT_STILL_EXISTS:<text>
//! ABAPREACH:<nonce>:STATUS:<TOKEN>
//! ABAPREACH:<nonce>:END:<count>
//! ```
//!
//! `<nonce>` is the 128-bit nonce of the run as 32 upper-case hex characters. Every `<text>` is
//! the UTF-8 bytes of the SAP text as upper-case hex, so a newline, a colon, or a forged marker
//! in SAP text cannot become a line. The order is `BEGIN`, any number of body lines, exactly one
//! `STATUS`, then `END`. `<count>` is the number of machine lines before `END`, `BEGIN` and
//! `STATUS` included. Lines that do not start with the nonce are ignored with a `W:` line; a line
//! that starts with the nonce but breaks the grammar makes the run uncertain. The classrun
//! framing (blank lines between lines, CR before LF) is not part of the grammar.
//!
//! A run that is not complete and well formed is never a success: the caller treats it as an
//! uncertain status and reconciles from a new session.

use std::fmt;

/// The nonce is 128 bits as upper-case hex.
pub const NONCE_LEN: usize = 32;
/// Output above this many bytes is not parsed. The measured runs stay far below it (2 MB in
/// 9-13 s is the proven upper end).
pub const MAX_OUTPUT_BYTES: usize = 8 << 20;
/// The longest scrubbed text; a longer one ends in `...`.
pub const TEXT_LIMIT: usize = 255;
/// Ignored lines listed one by one; the rest are counted in one closing `W:` line.
pub const MAX_WARNINGS: usize = 20;

const MARKER: &str = "ABAPREACH:";
const KEY_MAX: usize = 24;
const COUNT_DIGITS_MAX: usize = 9;
const CUT_MARK: &str = "...";

/// The 128-bit nonce that binds output lines to one run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nonce(String);

impl Nonce {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() == NONCE_LEN && text.bytes().all(is_upper_hex) {
            return Ok(Self(text.to_owned()));
        }

        Err(format!(
            "the runner nonce must be {NONCE_LEN} upper-case hex characters"
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The template mode a run was started with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunMode {
    Read,
    Simulate,
    Deploy,
    CreateSim,
    Create,
    DeleteSim,
    Delete,
}

impl RunMode {
    pub fn token(self) -> &'static str {
        match self {
            Self::Read => "READ",
            Self::Simulate => "SIMULATE",
            Self::Deploy => "DEPLOY",
            Self::CreateSim => "CREATE_SIM",
            Self::Create => "CREATE",
            Self::DeleteSim => "DELETE_SIM",
            Self::Delete => "DELETE",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        [
            Self::Read,
            Self::Simulate,
            Self::Deploy,
            Self::CreateSim,
            Self::Create,
            Self::DeleteSim,
            Self::Delete,
        ]
        .into_iter()
        .find(|mode| mode.token() == token)
    }

    /// The statuses a mode may end in; anything else is a protocol error.
    fn accepts(self, status: RunStatus) -> bool {
        match self {
            Self::Read => matches!(
                status,
                RunStatus::Ok | RunStatus::NotFound | RunStatus::Rejected
            ),
            Self::Simulate => matches!(
                status,
                RunStatus::SimulatedOk | RunStatus::NoChanges | RunStatus::Rejected
            ),
            Self::CreateSim => matches!(status, RunStatus::SimulatedOk | RunStatus::Rejected),
            Self::Deploy | Self::Create => matches!(
                status,
                RunStatus::Active | RunStatus::Inactive | RunStatus::Rejected | RunStatus::Partial
            ),
            Self::DeleteSim => matches!(
                status,
                RunStatus::SimulatedOk | RunStatus::NotFound | RunStatus::Rejected
            ),
            Self::Delete => matches!(
                status,
                RunStatus::Deleted | RunStatus::NotFound | RunStatus::Rejected | RunStatus::Partial
            ),
        }
    }
}

/// What the run reports. It is evidence, not the operation status: Rust decides that.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Ok,
    SimulatedOk,
    NoChanges,
    Active,
    Inactive,
    Rejected,
    Partial,
    NotFound,
    Deleted,
}

impl RunStatus {
    pub fn token(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::SimulatedOk => "SIMULATED_OK",
            Self::NoChanges => "NO_CHANGES",
            Self::Active => "ACTIVE",
            Self::Inactive => "INACTIVE",
            Self::Rejected => "REJECTED",
            Self::Partial => "PARTIAL",
            Self::NotFound => "NOT_FOUND",
            Self::Deleted => "DELETED",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        [
            Self::Ok,
            Self::SimulatedOk,
            Self::NoChanges,
            Self::Active,
            Self::Inactive,
            Self::Rejected,
            Self::Partial,
            Self::NotFound,
            Self::Deleted,
        ]
        .into_iter()
        .find(|status| status.token() == token)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// A SAP message, already scrubbed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub level: Level,
    pub text: String,
}

/// A named value of the run (hash, verdict line, evidence); the value is scrubbed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fact {
    pub key: String,
    pub value: String,
}

/// A data row from SAP (the XML of `read`). It is data from a remote system: never run it, never
/// splice it into a summary or a command, and print it only where a file or a quoted block is
/// the destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedLine(String);

impl UntrustedLine {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A marker that makes the status `PARTIAL` and stops the batch; the detail is scrubbed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Stop {
    PartialDelete(String),
    ObjectStillExists(String),
}

/// One complete, well formed run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Report {
    pub mode: RunMode,
    pub status: RunStatus,
    pub messages: Vec<Message>,
    pub lines: Vec<UntrustedLine>,
    pub facts: Vec<Fact>,
    pub stops: Vec<Stop>,
    /// The batch must not continue with the next row.
    pub stop_batch: bool,
    /// `W: ...` lines for output that was ignored.
    pub warnings: Vec<String>,
}

/// Why a run is an uncertain status. Line numbers count the lines bound to the nonce, from 1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    TooLarge,
    MissingBegin,
    Truncated,
    MissingStatus,
    DuplicateStatus,
    DuplicateBegin,
    BadOrder,
    TrailingMarker,
    ModeMismatch,
    StatusNotForMode,
    MarkerNotForMode,
    Malformed(usize),
    BrokenEncoding(usize),
    CountMismatch { expected: usize, found: usize },
    ServerError(u16),
    RuntimeFailure(String),
}

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("runner output is larger than the limit"),
            Self::MissingBegin => {
                formatter.write_str("runner output has no start line for this run")
            }
            Self::Truncated => formatter.write_str("runner output ends before the closing line"),
            Self::MissingStatus => formatter.write_str("runner output has no status line"),
            Self::DuplicateStatus => {
                formatter.write_str("runner output has more than one status line")
            }
            Self::DuplicateBegin => {
                formatter.write_str("runner output has more than one start line")
            }
            Self::BadOrder => formatter.write_str("runner output lines are in an invalid order"),
            Self::TrailingMarker => {
                formatter.write_str("runner output has lines after the closing line")
            }
            Self::ModeMismatch => formatter.write_str("runner output belongs to another mode"),
            Self::StatusNotForMode => {
                formatter.write_str("runner status is not valid for this mode")
            }
            Self::MarkerNotForMode => {
                formatter.write_str("runner marker is not valid for this mode")
            }
            Self::Malformed(line) => write!(formatter, "runner output line {line} is malformed"),
            Self::BrokenEncoding(line) => {
                write!(formatter, "runner output line {line} has a broken encoding")
            }
            Self::CountMismatch { expected, found } => write!(
                formatter,
                "runner closing count is {found}, expected {expected}"
            ),
            Self::ServerError(status) => write!(formatter, "the classrun reply was HTTP {status}"),
            Self::RuntimeFailure(key) => {
                write!(formatter, "the runner stopped with runtime error {key}")
            }
        }
    }
}

/// The result of reading a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Parsed {
    Complete(Report),
    /// The outcome of the work in SAP is unknown: reconcile, never resend.
    Uncertain {
        reason: Reason,
        warnings: Vec<String>,
    },
    /// The server refused the POST (4xx, redirect): the class did not run.
    NotRun(u16),
}

/// Parses the body of a classrun reply that came back as a normal answer.
pub fn parse_output(nonce: &Nonce, mode: RunMode, body: &str) -> Parsed {
    if body.len() > MAX_OUTPUT_BYTES {
        return uncertain(Reason::TooLarge, Vec::new());
    }

    let mut run = Run::new(mode);

    for (index, raw) in body.split('\n').enumerate() {
        let text = raw.strip_suffix('\r').unwrap_or(raw);

        if text.is_empty() {
            continue;
        }

        let Some(rest) = bound(nonce, text) else {
            run.ignore(index + 1);

            continue;
        };

        if let Err(reason) = run.accept(rest) {
            return uncertain(reason, run.warnings.finish());
        }
    }

    run.finish()
}

/// Reads a classrun POST reply by its HTTP status. A 5xx is a lost reply even when the body
/// holds a whole run: a request that ran into the proxy timeout is rolled back, so the text
/// cannot be believed.
pub fn parse_post_reply(nonce: &Nonce, mode: RunMode, status: u16, body: &str) -> Parsed {
    match status {
        200..=299 => parse_output(nonce, mode, body),
        500..=599 => uncertain(Reason::ServerError(status), Vec::new()),
        _ => Parsed::NotRun(status),
    }
}

/// A runtime failure over SADT (no response, only the error key): the work done before it may
/// persist, so the status is uncertain.
pub fn runtime_failure(key: &str) -> Parsed {
    uncertain(Reason::RuntimeFailure(scrub_text(key)), Vec::new())
}

/// Makes SAP text safe for a summary or a tool result: control characters are dropped, newlines
/// and tabs become one space, bidirectional controls are dropped, and the length is cut.
pub fn scrub_text(text: &str) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut pending_space = false;

    for character in text.chars() {
        if is_spacing(character) {
            pending_space = true;

            continue;
        }

        if character.is_control() || is_bidi(character) {
            continue;
        }

        if pending_space && !out.is_empty() {
            out.push(' ');
            count += 1;
        }

        pending_space = false;
        out.push(character);
        count += 1;

        if count > TEXT_LIMIT {
            break;
        }
    }

    if count <= TEXT_LIMIT {
        return out;
    }

    let mut cut: String = out.chars().take(TEXT_LIMIT - CUT_MARK.len()).collect();
    cut.push_str(CUT_MARK);

    cut
}

fn uncertain(reason: Reason, warnings: Vec<String>) -> Parsed {
    Parsed::Uncertain { reason, warnings }
}

fn is_upper_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)
}

fn is_spacing(character: char) -> bool {
    matches!(
        character,
        ' ' | '\n' | '\r' | '\t' | '\u{b}' | '\u{c}' | '\u{2028}' | '\u{2029}'
    )
}

fn is_bidi(character: char) -> bool {
    matches!(
        character,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// The part of a line after `ABAPREACH:<nonce>:`, or nothing when the line is not bound.
fn bound<'a>(nonce: &Nonce, text: &'a str) -> Option<&'a str> {
    text.strip_prefix(MARKER)?
        .strip_prefix(nonce.as_str())?
        .strip_prefix(':')
}

fn decode_hex(field: &str) -> Option<String> {
    let bytes = field.as_bytes();

    if !bytes.len().is_multiple_of(2) || !bytes.iter().copied().all(is_upper_hex) {
        return None;
    }

    let decoded: Vec<u8> = bytes
        .chunks(2)
        .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
        .collect();

    String::from_utf8(decoded).ok()
}

fn nibble(byte: u8) -> u8 {
    if byte.is_ascii_digit() {
        byte - b'0'
    } else {
        byte - b'A' + 10
    }
}

fn valid_key(key: &str) -> bool {
    let mut bytes = key.bytes();

    key.len() <= KEY_MAX
        && bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn parse_count(field: &str) -> Option<usize> {
    let plain = field.bytes().all(|byte| byte.is_ascii_digit())
        && !field.is_empty()
        && field.len() <= COUNT_DIGITS_MAX
        && (field == "0" || !field.starts_with('0'));

    plain.then(|| field.parse().ok()).flatten()
}

/// What a bound line says, before the order is checked.
enum Line {
    Begin(RunMode),
    Msg(Level, String),
    Data(String),
    Fact(String, String),
    Stop(Stop),
    Status(RunStatus),
    End(usize),
}

fn read_line(rest: &str, number: usize) -> Result<Line, Reason> {
    let malformed = || Reason::Malformed(number);
    let text = |field: &str| decode_hex(field).ok_or(Reason::BrokenEncoding(number));
    let fields: Vec<&str> = rest.split(':').collect();

    match fields.as_slice() {
        ["BEGIN", mode] => RunMode::from_token(mode)
            .map(Line::Begin)
            .ok_or_else(malformed),
        ["MSG", level, field] => {
            let level = match *level {
                "I" => Level::Info,
                "W" => Level::Warning,
                "E" => Level::Error,
                _ => return Err(malformed()),
            };

            Ok(Line::Msg(level, scrub_text(&text(field)?)))
        }
        ["LINE", field] => Ok(Line::Data(text(field)?)),
        ["FACT", key, field] if valid_key(key) => {
            Ok(Line::Fact((*key).to_owned(), scrub_text(&text(field)?)))
        }
        ["PARTIAL_DELETE", field] => Ok(Line::Stop(Stop::PartialDelete(scrub_text(&text(field)?)))),
        ["OBJECT_STILL_EXISTS", field] => Ok(Line::Stop(Stop::ObjectStillExists(scrub_text(
            &text(field)?,
        )))),
        ["STATUS", token] => RunStatus::from_token(token)
            .map(Line::Status)
            .ok_or_else(malformed),
        ["END", count] => parse_count(count).map(Line::End).ok_or_else(malformed),
        _ => Err(malformed()),
    }
}

/// Ignored-line notes: the first ones one by one, the rest as a count.
struct Warnings {
    listed: Vec<String>,
    skipped: usize,
}

impl Warnings {
    fn push(&mut self, line: usize) {
        if self.listed.len() < MAX_WARNINGS {
            self.listed.push(format!(
                "W: ignored output line {line} (not bound to this run)"
            ));
        } else {
            self.skipped += 1;
        }
    }

    fn finish(&mut self) -> Vec<String> {
        let mut all = std::mem::take(&mut self.listed);

        if self.skipped > 0 {
            all.push(format!(
                "W: {} further ignored output lines not listed",
                self.skipped
            ));
        }

        all
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum State {
    Start,
    Body,
    Statused,
    Ended,
}

struct Run {
    mode: RunMode,
    state: State,
    count: usize,
    status: Option<RunStatus>,
    messages: Vec<Message>,
    lines: Vec<UntrustedLine>,
    facts: Vec<Fact>,
    stops: Vec<Stop>,
    warnings: Warnings,
}

impl Run {
    fn new(mode: RunMode) -> Self {
        Self {
            mode,
            state: State::Start,
            count: 0,
            status: None,
            messages: Vec::new(),
            lines: Vec::new(),
            facts: Vec::new(),
            stops: Vec::new(),
            warnings: Warnings {
                listed: Vec::new(),
                skipped: 0,
            },
        }
    }

    fn ignore(&mut self, line: usize) {
        self.warnings.push(line);
    }

    fn accept(&mut self, rest: &str) -> Result<(), Reason> {
        if self.state == State::Ended {
            return Err(Reason::TrailingMarker);
        }

        let number = self.count + 1;
        let line = read_line(rest, number)?;

        match line {
            Line::Begin(mode) => match self.state {
                State::Start if mode == self.mode => self.state = State::Body,
                State::Start => return Err(Reason::ModeMismatch),
                _ => return Err(Reason::DuplicateBegin),
            },
            Line::End(found) => {
                match self.state {
                    State::Start => return Err(Reason::BadOrder),
                    State::Body => return Err(Reason::MissingStatus),
                    _ => {}
                }

                if found != self.count {
                    return Err(Reason::CountMismatch {
                        expected: self.count,
                        found,
                    });
                }

                self.state = State::Ended;

                return Ok(());
            }
            Line::Status(status) => match self.state {
                State::Start => return Err(Reason::BadOrder),
                State::Statused => return Err(Reason::DuplicateStatus),
                _ if !self.mode.accepts(status) => return Err(Reason::StatusNotForMode),
                _ => {
                    self.status = Some(status);
                    self.state = State::Statused;
                }
            },
            body => {
                if self.state != State::Body {
                    return Err(Reason::BadOrder);
                }

                self.add(body)?;
            }
        }

        self.count = number;

        Ok(())
    }

    fn add(&mut self, line: Line) -> Result<(), Reason> {
        match line {
            Line::Msg(level, text) => self.messages.push(Message { level, text }),
            Line::Data(text) => self.lines.push(UntrustedLine(text)),
            Line::Fact(key, value) => self.facts.push(Fact { key, value }),
            Line::Stop(_) if self.mode != RunMode::Delete => return Err(Reason::MarkerNotForMode),
            Line::Stop(stop) => self.stops.push(stop),
            Line::Begin(_) | Line::Status(_) | Line::End(_) => return Err(Reason::BadOrder),
        }

        Ok(())
    }

    fn finish(mut self) -> Parsed {
        let Some(reported) = self.status else {
            let reason = if self.state == State::Start {
                Reason::MissingBegin
            } else {
                Reason::Truncated
            };

            return uncertain(reason, self.warnings.finish());
        };

        if self.state != State::Ended {
            return uncertain(Reason::Truncated, self.warnings.finish());
        }

        let status = if self.stops.is_empty() {
            reported
        } else {
            RunStatus::Partial
        };

        Parsed::Complete(Report {
            mode: self.mode,
            status,
            messages: self.messages,
            lines: self.lines,
            facts: self.facts,
            stops: self.stops,
            stop_batch: status == RunStatus::Partial,
            warnings: self.warnings.finish(),
        })
    }
}
