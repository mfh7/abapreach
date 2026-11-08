//! The MCP write tools: simulate, deploy, create and delete over the shared operations. The tools
//! only adapt arguments and results; the checks, guards and routing are the ones the CLI runs.
//!
//! A write needs several things before SAP is touched: the connection allows MCP writes
//! (`mcp_write`), the arguments pass the local rules, the [`WriteGate`] admits the plan token of
//! a passed simulation, and, under `confirm` and always for a delete, the human types the name of
//! the object into a form. Nothing a client sends, annotations included, stands in for these.

use crate::app::{
    Attempt, Exit, Factories, Opened, RfcLibraryInfo, RfcProbe, end_session, exit_of_outcome,
    option_misuse, pin_of, route,
};
use crate::backend::{BackendKind, Failure, OperationKind, Selection};
use crate::cli::{CreateKind, DeleteKind, DeleteTarget, IncludeKind, ObjectKind, Subc, TypeArg};
use crate::config::{Connection, scrub};
use crate::domain::{Outcome, Status};
use crate::extension::{
    CreateRequest, ExtensionDelete, ExtensionKind, ExtensionOp, ExtensionOptions, MixedDelete,
    Normalize, Registry, RunContext, SharedFactories, Verbs, adt_label,
};
use crate::hash::local_source_lines;
use crate::mcp::{Answer, Form, NoSession, Session, ToolHandler, ToolResult};
use crate::mcp_read::{
    READ_TYPES, ReadTools, SECRET_REJECTED, diagnostic, holds_secret, optional, required,
    status_token,
};
use crate::object::{
    check_create, check_create_extension, check_deploy_options, check_deploy_source,
};
use crate::operation::create::{CreateInput, CreateProg};
use crate::operation::delete::{self, Context, DeleteBackend, DeleteReport, Plan, sha256_hex};
use crate::operation::delete_adt::{AdtDelete, NO_STATEFUL_SESSION};
use crate::operation::delete_ddic::{DdicDelete, DdicEnv, RunnerBatch};
use crate::operation::deploy::{
    CancelFlag, Change, DeployInput, DeployProg, DeployScope, NEW_INCLUDE_WARNING,
    known_lock_handles,
};
use crate::policy::{Confirmer, McpWrite};
use crate::transport::rfc::FunctionApi;
use crate::transport::{SadtTransport, TransportError};
use clap::ValueEnum;
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod runner;

/// How long a plan token stays valid.
pub const TOKEN_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// The most characters of a confirmation summary; the rest is cut with a marker.
pub const MAX_SUMMARY: usize = 6000;

/// The longest line, and the most excerpt and object lines, a summary shows.
const SUMMARY_LINE: usize = 120;
const SUMMARY_EXCERPT_LINES: usize = 30;
const SUMMARY_OBJECT_LINES: usize = 60;

/// The limits of a tool call. `call` is the deadline of the work itself; `elicit` is how long a
/// human may take to answer, which the deadline does not count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub call: Duration,
    pub elicit: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            call: Duration::from_secs(55),
            elicit: Duration::from_secs(300),
        }
    }
}

/// What a plan token is for: a token of one operation never admits another.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Deploy,
    Create,
    Delete,
}

/// What a simulation settled and a later write must repeat. `fields` holds every argument the
/// contract binds in its normalized form, so equal fields mean equal arguments; the rest is what
/// the simulation found: the backend it ran on, the delete plan, the function group it resolved,
/// the live hash it read and what the human is shown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ticket {
    pub operation: Operation,
    pub conn: String,
    pub fields: BTreeMap<String, String>,
    pub backend: Option<BackendKind>,
    pub plan: Option<Plan>,
    /// The function group a FUNC deploy resolved; the write must find the same one.
    pub group: Option<String>,
    /// The live hash the simulation read; the write refuses a source that moved on.
    pub base: Option<String>,
    /// The main program an include was resolved to; the write runs under the same one, so a
    /// main program that changed since the simulation stops it before the lock.
    pub main: Option<String>,
    /// The `W:` line of a new include's first simulation, shown to the human before the write.
    pub warning: Option<String>,
    /// The size and an excerpt of the change, for the confirmation.
    pub change: Option<Change>,
    /// A delete's objects, with the contents of a group, one line each.
    pub listing: Vec<String>,
    /// What run 1 of a DDIC deploy found; set at execution, never stored by a simulation.
    pub ddic: Option<DdicPreview>,
}

/// What run 1 of a DDIC deploy found, for the confirmation shown before run 2 writes: the
/// temporary class, the size and an excerpt of the diff, and the conversion verdicts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DdicPreview {
    pub runner: Option<String>,
    pub added: usize,
    pub removed: usize,
    /// The unified diff lines, header and context included.
    pub excerpt: Vec<String>,
    /// The `CONVERSION` lines of the verdict.
    pub conversion: Vec<String>,
}

impl DdicPreview {
    /// Reads the printed blocks of run 1: the diff block starts with its `--- live` header.
    pub fn of(runner: Option<&str>, printed: &[String]) -> Self {
        let mut preview = Self {
            runner: runner.map(str::to_owned),
            added: 0,
            removed: 0,
            excerpt: Vec::new(),
            conversion: Vec::new(),
        };

        for block in printed {
            if block.starts_with("--- live") {
                // The first two lines are the header, the rest are rows.
                for (index, line) in block.lines().enumerate() {
                    match line.chars().next() {
                        Some('+') if index > 1 => preview.added += 1,
                        Some('-') if index > 1 => preview.removed += 1,
                        _ => {}
                    }

                    preview.excerpt.push(line.to_owned());
                }
            } else {
                preview.conversion.extend(
                    block
                        .lines()
                        .filter(|line| line.starts_with("CONVERSION "))
                        .map(str::to_owned),
                );
            }
        }

        preview
    }
}

impl Ticket {
    /// The arguments of a call, with nothing a simulation adds.
    pub fn new(
        operation: Operation,
        conn: impl Into<String>,
        fields: BTreeMap<String, String>,
    ) -> Self {
        Self {
            operation,
            conn: conn.into(),
            fields,
            backend: None,
            plan: None,
            group: None,
            base: None,
            main: None,
            warning: None,
            change: None,
            listing: Vec::new(),
            ddic: None,
        }
    }
}

/// The seam between the tools and the plan token lifecycle.
pub trait WriteGate {
    /// Called after a simulation that passed: the token the client must present, if any.
    fn issue(&mut self, ticket: Ticket) -> Option<String>;

    /// Called before any SAP activity of a write. `presented` carries the arguments of the call
    /// (no backend, no plan); the answer is the ticket the simulation stored, or the reason the
    /// token does not admit this call. A token that admits is spent.
    fn admit(&mut self, token: &str, presented: &Ticket) -> Result<Ticket, String>;
}

type Clock = Rc<dyn Fn() -> Instant>;

#[derive(Default)]
struct GateState {
    stored: HashMap<String, (Ticket, Instant)>,
    issued: u64,
}

/// The plan tokens of one process: random, single use, valid for [`TOKEN_LIFETIME`]. A clone
/// shares the tokens of the original.
#[derive(Clone)]
pub struct TokenGate {
    state: Rc<RefCell<GateState>>,
    clock: Clock,
}

impl TokenGate {
    pub fn new() -> Self {
        Self::with_clock(Rc::new(Instant::now))
    }

    /// A gate that reads time from `clock`, so a test can move it.
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            state: Rc::default(),
            clock,
        }
    }
}

impl Default for TokenGate {
    fn default() -> Self {
        Self::new()
    }
}

/// 128 bits from the keys the standard library draws from the operating system.
/// shortcut: no `getrandom`; a token only has to be unguessable for the lifetime of one
/// process, upgrade when it ever has to resist an attacker who can read process memory.
fn random_token(issued: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());

    (0u8..2)
        .map(|round| {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();

            hasher.write_u64(issued);
            hasher.write_u8(round);
            hasher.write_u128(nanos);

            format!("{:016x}", hasher.finish())
        })
        .collect()
}

impl WriteGate for TokenGate {
    fn issue(&mut self, ticket: Ticket) -> Option<String> {
        let now = (self.clock)();
        let mut state = self.state.borrow_mut();

        state
            .stored
            .retain(|_, (_, issued)| now.saturating_duration_since(*issued) <= TOKEN_LIFETIME);
        state.issued += 1;

        let token = random_token(state.issued);

        state.stored.insert(token.clone(), (ticket, now));

        Some(token)
    }

    fn admit(&mut self, token: &str, presented: &Ticket) -> Result<Ticket, String> {
        let now = (self.clock)();
        let mut state = self.state.borrow_mut();
        let Some((stored, issued)) = state.stored.get(token) else {
            return Err("plan_token is unknown or already used; simulate again".to_owned());
        };

        if now.saturating_duration_since(*issued) > TOKEN_LIFETIME {
            state.stored.remove(token);

            return Err("plan_token expired after 15 minutes; simulate again".to_owned());
        }

        // Another call's arguments neither redeem nor spend the token.
        if stored.operation != presented.operation
            || stored.conn != presented.conn
            || stored.fields != presented.fields
        {
            return Err("plan_token does not match this call (arguments differ)".to_owned());
        }

        Ok(state
            .stored
            .remove(token)
            .map(|(ticket, _)| ticket)
            .unwrap())
    }
}

/// MCP has no terminal: a prompt is never shown and never answered.
struct NoTerminal;

impl Confirmer for NoTerminal {
    fn interactive(&self) -> bool {
        false
    }

    fn confirm(&self, _: &str) -> bool {
        false
    }
}

/// The factories of a call with the cancel flag of that call: the operation sees the flag the
/// client's cancellation sets.
struct WithCancel<'a> {
    inner: &'a mut dyn Factories,
    cancel: CancelFlag,
    /// The backend of the last connection the operation asked for: the one it ended on.
    opened: Option<BackendKind>,
}

impl Factories for WithCancel<'_> {
    fn adt(&mut self, connection: &Connection) -> Opened {
        self.opened = Some(BackendKind::AdtHttp);
        self.inner.adt(connection)
    }

    fn rfc(&mut self, connection: &Connection) -> Opened {
        self.opened = Some(BackendKind::RfcSadt);
        self.inner.rfc(connection)
    }

    fn rfc_library(&mut self, connection: &Connection) -> Result<RfcLibraryInfo, String> {
        self.inner.rfc_library(connection)
    }

    fn rfc_ping(&mut self, connection: &Connection) -> Result<(), TransportError> {
        self.inner.rfc_ping(connection)
    }

    fn rfc_probe(&mut self, connection: &Connection) -> RfcProbe {
        self.inner.rfc_probe(connection)
    }

    fn function_api(
        &mut self,
        connection: &Connection,
    ) -> Result<Box<dyn FunctionApi>, TransportError> {
        self.inner.function_api(connection)
    }

    fn function_api_fresh(
        &mut self,
        connection: &Connection,
    ) -> Result<Box<dyn FunctionApi>, TransportError> {
        self.inner.function_api_fresh(connection)
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

/// The deadline of one tool call. It is only looked at where stopping is safe: before the
/// operation starts and between the objects of a delete batch.
struct Deadline {
    limit: Duration,
    started: Instant,
}

impl Deadline {
    fn start(limit: Duration) -> Self {
        Self {
            limit,
            started: Instant::now(),
        }
    }

    fn restart(&mut self) {
        self.started = Instant::now();
    }

    fn passed(&self) -> bool {
        self.started.elapsed() >= self.limit
    }
}

/// The read tools plus the write tools.
pub struct McpTools<'a, G> {
    read: ReadTools<'a>,
    gate: G,
    limits: Limits,
    /// `W:` lines about writes the client gave up on, shown with the next tool result.
    notices: Vec<String>,
    /// The extension whose types the write tools accept; none without a registry.
    registry: Option<&'a Registry>,
}

impl<'a, G: WriteGate> McpTools<'a, G> {
    pub fn new(read: ReadTools<'a>, gate: G) -> Self {
        Self {
            read,
            gate,
            limits: Limits::default(),
            notices: Vec::new(),
            registry: None,
        }
    }

    /// Adds the types and options of a registered extension to the write tools.
    #[must_use]
    pub fn with_registry(mut self, registry: &'a Registry) -> Self {
        self.registry = Some(registry);
        self.read = self.read.with_registry(registry);

        self
    }

    /// The extension types that accept a verb.
    fn extension_kinds(&self, verb: Verbs) -> Vec<ExtensionKind> {
        self.registry
            .into_iter()
            .flat_map(Registry::kinds)
            .filter(|kind| kind.spec().verbs.contains(verb))
            .collect()
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;

        self
    }

    /// The connection of a write tool: it must allow MCP writes, and runs with the MCP policies.
    fn writer(&self, arguments: &Map<String, Value>) -> Result<Connection, String> {
        let mut connection = self.read.connection(arguments)?;

        if connection.policies.mcp_write == McpWrite::Off {
            return Err(format!(
                "MCP writes are off for {} (policy mcp_write)",
                connection.id
            ));
        }

        connection.policies = connection.policies.for_mcp();

        Ok(connection)
    }

    /// The token and the client capability an executing call needs: the presented token is spent
    /// here, and whether the human must still be asked is returned with the stored ticket.
    fn admit(
        &mut self,
        connection: &Connection,
        arguments: &Map<String, Value>,
        presented: &Ticket,
        session: &dyn Session,
    ) -> Result<(Ticket, bool), String> {
        let delete = presented.operation == Operation::Delete;
        let confirm = delete || connection.policies.mcp_write == McpWrite::Confirm;
        let token = required(arguments, "plan_token")?;
        let stored = self.gate.admit(token, presented)?;

        // A matching presented token is terminal, including an incapable or old client. Missing
        // or mismatched tokens fail in `admit` without touching any other ticket.
        if confirm && !session.can_elicit() {
            let blind = "this client cannot answer a confirmation (it did not advertise \
                         elicitation, or its protocol is older than 2025-06-18)";

            return Err(if delete {
                format!("abap_delete always needs a human confirmation and {blind}")
            } else {
                format!("mcp_write is confirm for {} and {blind}", connection.id)
            });
        }

        Ok((stored, confirm))
    }

    /// Everything an executing call needs before SAP is touched: the capability to ask, the
    /// token, and where the policy or the tool demands it the human's typed answer. The form is
    /// shown before the lock, so the base check under the lock catches a stale confirmation.
    fn authorize(
        &mut self,
        connection: &Connection,
        arguments: &Map<String, Value>,
        presented: &Ticket,
        session: &mut dyn Session,
        deadline: &mut Deadline,
    ) -> Result<Ticket, String> {
        let (stored, confirm) = self.admit(connection, arguments, presented, session)?;

        if !confirm {
            return Ok(stored);
        }

        let read = &mut self.read;
        let answer = ask(
            connection,
            &stored,
            self.registry,
            self.limits.elicit,
            session,
            &mut |name, args| read.call(name, args),
        );

        // The answer is in; what follows is work, and the deadline counts only work.
        deadline.restart();

        answer.map(|()| stored)
    }

    /// The last look before anything is sent: a passed deadline or a cancellation ends the call.
    fn proceed(deadline: &Deadline, cancel: &CancelFlag, limits: Limits) -> Result<(), String> {
        if cancel.is_set() {
            return Err("CANCELED: the request was cancelled before anything was sent".to_owned());
        }

        if deadline.passed() {
            return Err(format!(
                "the time limit of {} s passed before anything was sent; nothing was changed",
                limits.call.as_secs()
            ));
        }

        Ok(())
    }

    fn deploy(
        &mut self,
        arguments: &Map<String, Value>,
        execute: bool,
        session: &mut dyn Session,
    ) -> Result<ToolResult, String> {
        let mut deadline = Deadline::start(self.limits.call);
        let connection = self.writer(arguments)?;

        if let Some(kind) = runner::runner_type_of(arguments) {
            return self.runner_deploy(connection, kind, arguments, execute, session, deadline);
        }

        let kind = parse::<ObjectKind>(required(arguments, "type")?, READ_TYPES)?;
        let name = required(arguments, "name")?;
        let source = required(arguments, "source")?;
        let tr = optional(arguments, "tr")?;
        let include = match optional(arguments, "include")? {
            Some(include) => Some(parse::<IncludeKind>(
                include,
                "include must be one of CCIMP, CCDEF, CCMAC, CCAU",
            )?),
            None => None,
        };
        let fugr = optional(arguments, "fugr")?;
        let create_include = flag(arguments, "create_include")?;
        let main = optional(arguments, "main")?;

        check_deploy_options(kind, name, include, create_include, fugr, main)?;

        let lines = local_source_lines(source);

        check_deploy_source(kind, name, &lines)?;

        let mut fields = bind([
            ("type", Some(kind.token().to_owned())),
            ("name", Some(name.to_ascii_uppercase())),
            ("source_sha256", Some(sha256_hex(source))),
            ("tr", tr.map(str::to_ascii_uppercase)),
            ("include", include.map(|include| include.token().to_owned())),
            ("fugr", fugr.map(str::to_ascii_uppercase)),
            ("main", main.map(str::to_ascii_uppercase)),
        ]);

        fields.insert("create_include".to_owned(), create_include.to_string());

        let ticket = Ticket::new(Operation::Deploy, connection.id.clone(), fields);
        let stored = if execute {
            Some(self.authorize(&connection, arguments, &ticket, session, &mut deadline)?)
        } else {
            None
        };
        let cancel = session.cancel();

        Self::proceed(&deadline, &cancel, self.limits)?;

        // The write repeats what the simulation saw: its backend, its base and its group.
        let group = stored
            .as_ref()
            .and_then(|stored| stored.group.clone())
            .or_else(|| fugr.map(str::to_owned));
        // The execute resolves the main program as the simulation did (a client `main` is checked
        // again, a listed one is not) and stops before the lock when it is no longer the stored one.
        let expected_main = stored.as_ref().and_then(|stored| stored.main.as_deref());
        let scope = DeployScope::new(kind)
            .group(group.as_deref())
            .include(include)
            .create_include(create_include)
            .main(main)
            .expected_main(expected_main)
            .cancel_before_lock(true);
        let input = DeployInput {
            name: name.to_owned(),
            source: lines,
            base: stored.as_ref().and_then(|stored| stored.base.clone()),
            simulate: !execute,
            tr: tr.map(str::to_owned),
        };
        let explicit = stored.as_ref().and_then(|stored| stored.backend);
        let mut seen = Seen::default();
        let _write = execute.then(|| cancel.hold_write());
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let exit = route(
            OperationKind::Deploy(kind),
            explicit,
            false,
            &connection,
            &mut factories,
            |transport, selection, cancel, _| {
                seen.backend = Some(selection.backend);

                let result = DeployProg::run_scoped(
                    transport,
                    input.clone(),
                    scope.clone(),
                    Some(selection),
                    Some(&connection),
                    cancel,
                );

                seen.status = result.outcome.status;
                seen.group = result.group.clone();
                seen.base = result.base.clone();
                seen.change = result.change.clone();
                seen.main = result.main.clone();
                seen.warning = result
                    .outcome
                    .stderr
                    .lines()
                    .find(|line| line.starts_with(NEW_INCLUDE_WARNING))
                    .map(str::to_owned);

                Attempt {
                    outcome: result.outcome,
                    failure: result.failure,
                    fallback_allowed: result.fallback_allowed,
                    detail: result.detail,
                }
            },
        );

        drop(factories);

        let issue = (!execute && seen.status == Some(Status::SimulatedOk)).then(|| Ticket {
            backend: seen.backend,
            group: seen.group.clone(),
            base: seen.base.clone(),
            main: seen.main.clone(),
            warning: seen.warning.clone(),
            change: seen.change.clone(),
            ..ticket
        });

        Ok(self.finish(
            &connection,
            &exit,
            seen.status,
            issue,
            json!({}),
            false,
            execute.then(|| Settled::new("abap_deploy", name, &cancel, session)),
        ))
    }

    fn create(
        &mut self,
        arguments: &Map<String, Value>,
        execute: bool,
        session: &mut dyn Session,
    ) -> Result<ToolResult, String> {
        let mut deadline = Deadline::start(self.limits.call);
        let connection = self.writer(arguments)?;

        if let Some(kind) = runner::runner_type_of(arguments) {
            return self.runner_create(connection, kind, arguments, execute, session, deadline);
        }

        let registry = self.registry;
        let empty = Registry::new();
        let registry = registry.unwrap_or(&empty);
        let extension_kinds = self.extension_kinds(Verbs::CREATE);
        let type_text = required(arguments, "type")?;
        let kind = match extension_kinds
            .iter()
            .find(|kind| kind.token() == type_text)
        {
            Some(kind) => TypeArg::Extension(*kind),
            None => TypeArg::Standard(parse::<CreateKind>(
                type_text,
                &format!(
                    "type must be one of PROG, FUGR, FUNC, CLAS, INTF, DTEL, DOMA, TABL, TTYP, TRAN{}",
                    extension_kinds
                        .iter()
                        .map(|kind| format!(", {}", kind.token()))
                        .collect::<String>()
                ),
            )?),
        };
        let name = required(arguments, "name")?;
        let text = required(arguments, "text")?;
        let package = optional(arguments, "package")?;
        let fugr = optional(arguments, "fugr")?;
        let tr = optional(arguments, "tr")?;
        let subc = match optional(arguments, "subc")? {
            Some(subc) => Some(parse::<Subc>(subc, "subc must be one of 1, I")?),
            None => None,
        };
        let mut options = ExtensionOptions::new();

        for option in extension_kinds
            .iter()
            .flat_map(|kind| kind.spec().create_options)
        {
            if let Some(value) = optional(arguments, option.id)? {
                options.insert(option.id, value.to_owned());
            }
        }

        // The MCP create has no FILE: it creates the object and writes no source. The local rules
        // are the ones of the CLI, in the same order.
        let request = CreateRequest {
            name,
            text,
            package,
            tr,
            simulate: !execute,
            options: &options,
        };

        match kind {
            TypeArg::Standard(kind) => check_create(
                kind,
                name,
                text,
                package,
                fugr,
                tr,
                subc,
                option_misuse(None, &options, registry),
                None,
            )?,
            // A DDIC or transaction type took the runner branch above.
            TypeArg::Runner(kind) => unreachable!("{} is handled by the runner", kind.token()),
            TypeArg::Extension(kind) => {
                let extension = registry.extension().ok_or("no extension is registered")?;

                check_create_extension(
                    kind.spec(),
                    name,
                    text,
                    package,
                    fugr,
                    tr,
                    subc,
                    option_misuse(Some(kind), &options, registry),
                    &|| extension.check_create(kind, &request),
                    None,
                )?;
                // A refusal needs no connection, so it comes before the ticket and the human.
                extension.preflight(ExtensionOp::Create, kind, &connection, None)?;
            }
        }

        let subc = (kind == CreateKind::Prog).then(|| subc.unwrap_or(Subc::Program));
        let mut fields = bind([
            (
                "type",
                Some(match kind {
                    TypeArg::Standard(kind) => kind.token().to_owned(),
                    TypeArg::Runner(kind) => kind.token().to_owned(),
                    TypeArg::Extension(kind) => kind.token().to_owned(),
                }),
            ),
            ("name", Some(name.to_ascii_uppercase())),
            ("text", Some(text.to_owned())),
            ("package", package.map(str::to_ascii_uppercase)),
            ("tr", tr.map(str::to_ascii_uppercase)),
            ("fugr", fugr.map(str::to_ascii_uppercase)),
            (
                "subc",
                subc.and_then(|subc| subc.to_possible_value())
                    .map(|value| value.get_name().to_owned()),
            ),
        ]);

        // An absent option adds no key, so a token without an extension option is unchanged.
        for option in extension_kinds
            .iter()
            .flat_map(|kind| kind.spec().create_options)
        {
            if let Some(value) = options.get(option.id) {
                fields.insert(
                    option.id.to_owned(),
                    match option.normalize {
                        Normalize::Upper => value.to_ascii_uppercase(),
                        Normalize::Verbatim => value.clone(),
                    },
                );
            }
        }

        let ticket = Ticket::new(Operation::Create, connection.id.clone(), fields);
        let stored = if execute {
            Some(self.authorize(&connection, arguments, &ticket, session, &mut deadline)?)
        } else {
            None
        };
        let cancel = session.cancel();

        Self::proceed(&deadline, &cancel, self.limits)?;

        let explicit = stored.as_ref().and_then(|stored| stored.backend);
        let mut seen = Seen::default();
        let _write = execute.then(|| cancel.hold_write());
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let exit = match kind {
            TypeArg::Runner(kind) => unreachable!("{} is handled by the runner", kind.token()),
            TypeArg::Extension(kind) => {
                // Over the extension only, never routed; the token's backend is its pin.
                let extension = registry.extension().ok_or("no extension is registered")?;
                let shared = SharedFactories::new(&mut factories);
                let run = RunContext {
                    connection: &connection,
                    cancel: &cancel,
                    pin: explicit.map(pin_of),
                    factories: &shared,
                };
                let outcome = extension.create(kind, &request, run);

                seen.status = outcome.status;

                exit_of_outcome(outcome)
            }
            TypeArg::Standard(kind) => {
                let input = CreateInput {
                    kind,
                    name: name.to_owned(),
                    text: text.to_owned(),
                    package: package.map(str::to_owned),
                    group: fugr.map(str::to_owned),
                    tr: tr.map(str::to_owned),
                    simulate: !execute,
                    subc: subc.unwrap_or(Subc::Program),
                    source: None,
                };

                route(
                    OperationKind::Create(kind),
                    explicit,
                    false,
                    &connection,
                    &mut factories,
                    |transport, selection, cancel, _| {
                        seen.backend = Some(selection.backend);

                        let result = CreateProg::run_with_cancel(
                            transport,
                            input.clone(),
                            Some(selection),
                            Some(&connection),
                            cancel,
                        );

                        seen.status = result.outcome.status;

                        Attempt {
                            outcome: result.outcome,
                            failure: result.failure,
                            fallback_allowed: result.fallback_allowed,
                            detail: result.detail,
                        }
                    },
                )
            }
        };

        drop(factories);

        let issue = (!execute && seen.status == Some(Status::SimulatedOk)).then_some(Ticket {
            backend: seen.backend,
            ..ticket
        });

        Ok(self.finish(
            &connection,
            &exit,
            seen.status,
            issue,
            json!({}),
            false,
            execute.then(|| Settled::new("abap_create", name, &cancel, session)),
        ))
    }

    fn delete(
        &mut self,
        arguments: &Map<String, Value>,
        execute: bool,
        session: &mut dyn Session,
    ) -> Result<ToolResult, String> {
        let mut deadline = Deadline::start(self.limits.call);
        let connection = self.writer(arguments)?;
        let extension_kinds = self.extension_kinds(Verbs::DELETE);
        let targets = targets(arguments, &extension_kinds)?;
        let tr = optional(arguments, "tr")?;
        let registry = self.registry;
        let empty = Registry::new();
        let registry = registry.unwrap_or(&empty);
        let mut batch_kinds: Vec<ExtensionKind> = Vec::new();
        let runner_rows = runner::check_delete_rows(&targets, tr)?;

        for target in &targets {
            if let DeleteKind::Extension(kind) = target.kind
                && !batch_kinds.contains(&kind)
            {
                batch_kinds.push(kind);
            }
        }

        // A batch with an extension type is refused before any connection and before the token is
        // looked at: its rows never run over ADT.
        for kind in &batch_kinds {
            let extension = registry.extension().ok_or("no extension is registered")?;

            extension.preflight(ExtensionOp::Delete, *kind, &connection, None)?;
        }

        // The local rules of the batch run before a connection is opened.
        let (rows, normalized_tr) = delete::prepare(&targets, tr)?;
        let mut objects: Vec<String> = rows
            .iter()
            .map(|(kind, name)| format!("{}:{name}", kind.token()))
            .collect();

        objects.sort();

        let ticket = Ticket::new(
            Operation::Delete,
            connection.id.clone(),
            bind([("objects", Some(objects.join("\n"))), ("tr", normalized_tr)]),
        );
        let stored = if execute {
            Some(self.authorize(&connection, arguments, &ticket, session, &mut deadline)?)
        } else {
            None
        };
        let cancel = session.cancel();

        Self::proceed(&deadline, &cancel, self.limits)?;

        let plan = match &stored {
            Some(stored) => Some(
                stored
                    .plan
                    .clone()
                    .ok_or("the plan_token carries no delete plan")?,
            ),
            None => None,
        };
        let explicit = stored.and_then(|stored| stored.backend);
        let _write = execute.then(|| cancel.hold_write());
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let mut call = DeleteCall {
            connection: &connection,
            targets: &targets,
            tr,
            plan: plan.as_ref(),
            deadline: &deadline,
            runner: runner_rows > 0,
            selected: None,
            report: None,
        };
        let exit = if batch_kinds.is_empty() {
            route(
                OperationKind::Delete,
                explicit,
                false,
                &connection,
                &mut factories,
                |transport, selection, cancel, fresh| {
                    call.attempt(transport, selection, cancel, fresh, None)
                },
            )
        } else {
            // The extension rows run on its backend and the other objects over the backend the
            // plan names: an execute is pinned to it, so nothing falls back between simulate and
            // delete.
            let extension = registry.extension().ok_or("no extension is registered")?;
            let shared = SharedFactories::new(&mut factories);
            let run = RunContext {
                connection: &connection,
                cancel: &cancel,
                pin: explicit.map(pin_of),
                factories: &shared,
            };

            match extension.delete_backend(run) {
                Err(outcome) => exit_of_outcome(outcome),
                Ok(mut backend) => {
                    if targets
                        .iter()
                        .all(|target| matches!(target.kind, DeleteKind::Extension(_)))
                    {
                        call.extension_only(&mut *backend, &cancel)
                    } else {
                        route(
                            OperationKind::Delete,
                            explicit,
                            false,
                            &connection,
                            &mut shared.lend(),
                            |transport, selection, cancel, fresh| {
                                call.attempt(
                                    transport,
                                    selection,
                                    cancel,
                                    fresh,
                                    Some(&mut *backend),
                                )
                            },
                        )
                    }
                }
            }
        };
        let DeleteCall {
            selected, report, ..
        } = call;

        drop(factories);

        let results: Vec<Value> = report.as_ref().map_or_else(Vec::new, |report| {
            report
                .results
                .iter()
                .map(|result| {
                    json!({
                        "type": result.kind.token(),
                        "name": result.name,
                        "status": result.status.token(),
                        "reason": result.reason.as_deref().map(|reason| diagnostic(&connection, reason)),
                    })
                })
                .collect()
        });
        let issue = report
            .as_ref()
            .filter(|report| !execute && report.all_simulated_ok())
            .map(|report| Ticket {
                backend: selected,
                plan: report.plan.clone(),
                listing: listing_of(report),
                ..ticket
            });
        let label = objects.join(" ");

        Ok(self.finish(
            &connection,
            &exit,
            None,
            issue,
            json!({"objects": results}),
            // Delete output is diagnostics, never a source or table payload, and its error
            // reasons echo the transport text: scrub it instead of rejecting it.
            true,
            execute.then(|| Settled::new("abap_delete", &label, &cancel, session)),
        ))
    }

    /// The result of an operation: complete text, the same data in structured form, the status
    /// from the operation's own state, and a token when a passed simulation earns one. A write
    /// that has run is logged, and noted for the next result when the client had given up on it.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        connection: &Connection,
        exit: &Exit,
        status: Option<Status>,
        issue: Option<Ticket>,
        mut structured: Value,
        diagnostics: bool,
        settled: Option<Settled<'_>>,
    ) -> ToolResult {
        // Every write here repeats the backend of a plan_token, not of a CLI flag.
        let notes = diagnostic(
            connection,
            &exit.stderr.replace(
                "--backend pins the backend",
                "the plan_token pins the backend",
            ),
        );
        let leaked = !diagnostics && holds_secret(connection, &exit.stdout);
        let output = if leaked {
            SECRET_REJECTED.to_owned()
        } else if diagnostics {
            diagnostic(connection, exit.stdout.trim_end())
        } else {
            scrub(exit.stdout.trim_end(), &known_lock_handles())
        };
        let mut text = [output.as_str(), notes.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let failed = exit.code != 0 || leaked;

        // A temporary class that stays is named in the data of the call that left it.
        if let Some(leftover) = exit
            .stderr
            .lines()
            .find_map(|line| line.strip_prefix("RUNNER_LEFTOVER: "))
        {
            structured["runner_leftover"] = json!(diagnostic(connection, leftover.trim()));
        }

        structured["status"] = json!(status.map(status_token));
        structured["exit"] = json!(exit.code);
        structured["output"] = json!(output);
        structured["notes"] = json!(notes);

        if failed {
            structured["message"] = json!(text);
        } else if let Some(token) = issue.and_then(|ticket| self.gate.issue(ticket)) {
            text.push_str(&format!("\nplan_token {token}"));
            structured["plan_token"] = json!(token);
        }

        if let Some(settled) = settled {
            let state = status.map_or("-", status_token);

            settled.session.log(&format!(
                "abapreach-mcp: {} exit {} status {state}",
                settled.tool, exit.code
            ));

            if settled.cancel.is_set() {
                self.notices.push(format!(
                    "W: {} {} was cancelled while it ran and ended with exit {} status {state}; \
                     check the object before retrying",
                    settled.tool, settled.what, exit.code
                ));
            }
        }

        ToolResult {
            text,
            structured,
            is_error: failed,
        }
    }

    /// Adds `notices`, the `W:` lines of earlier calls, to a result.
    fn with_notices(mut result: ToolResult, notices: Vec<String>) -> ToolResult {
        if notices.is_empty() {
            return result;
        }

        result.text.push('\n');
        result.text.push_str(&notices.join("\n"));

        if result.structured.get("message").is_some() {
            result.structured["message"] = json!(result.text);
        }

        result.structured["warnings"] = json!(notices);

        result
    }
}

/// Shows the human the summary of `ticket` and takes the typed name of the object: `Ok` only when
/// it matches. A summary that holds a configured secret is refused without asking.
fn ask(
    connection: &Connection,
    ticket: &Ticket,
    registry: Option<&Registry>,
    timeout: Duration,
    session: &mut dyn Session,
    read: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
) -> Result<(), String> {
    let summary = confirmation_summary_with(connection, ticket, registry);

    if holds_secret(connection, &summary) {
        return Err(SECRET_REJECTED.to_owned());
    }

    let expected = typed_text(ticket);
    let form = Form {
        message: format!("{summary}\nType {expected} to confirm."),
        label: format!("Type {expected} to confirm"),
    };

    match session.elicit(&form, timeout, read) {
        Answer::Accepted(typed) if typed.trim() == expected => Ok(()),
        Answer::Accepted(_) => Err("write not confirmed: the typed text did not match".to_owned()),
        Answer::Declined => Err("write not confirmed: declined".to_owned()),
        Answer::Cancelled => Err("write not confirmed: cancelled".to_owned()),
        Answer::TimedOut => Err(format!(
            "write not confirmed: timed out after {} s",
            timeout.as_secs()
        )),
    }
}

/// What a finished write needs to report itself.
struct Settled<'a> {
    tool: &'static str,
    what: String,
    cancel: &'a CancelFlag,
    session: &'a mut dyn Session,
}

impl<'a> Settled<'a> {
    fn new(
        tool: &'static str,
        what: &str,
        cancel: &'a CancelFlag,
        session: &'a mut dyn Session,
    ) -> Self {
        Self {
            tool,
            what: what.to_owned(),
            cancel,
            session,
        }
    }
}

/// What a simulation found, taken out of the routing closure.
#[derive(Default)]
struct Seen {
    backend: Option<BackendKind>,
    status: Option<Status>,
    group: Option<String>,
    base: Option<String>,
    main: Option<String>,
    warning: Option<String>,
    change: Option<Change>,
}

/// One delete call: the batch, the plan of an execute, and what the attempts on the routed
/// backend leave behind.
struct DeleteCall<'a> {
    connection: &'a Connection,
    targets: &'a [DeleteTarget],
    tr: Option<&'a str>,
    plan: Option<&'a Plan>,
    deadline: &'a Deadline,
    /// The batch has a DDIC or transaction row: those rows run in their own temporary class.
    runner: bool,
    selected: Option<BackendKind>,
    report: Option<DeleteReport>,
}

impl DeleteCall<'_> {
    /// The simulation, or with a plan the execution that checks it. Between two objects a
    /// cancellation or the deadline ends the batch; a request in flight is never interrupted.
    fn run(
        &self,
        backend: &mut dyn DeleteBackend,
        cancel: &CancelFlag,
    ) -> Result<DeleteReport, String> {
        let context = Context {
            conn: &self.connection.id,
            user: &self.connection.user,
            policies: &self.connection.policies,
            confirmer: &NoTerminal,
        };

        match self.plan {
            Some(plan) => {
                delete::execute_until(backend, &context, self.targets, self.tr, plan, &mut || {
                    cancel.is_set() || self.deadline.passed()
                })
            }
            None => delete::simulate(backend, &context, self.targets, self.tr),
        }
    }

    /// A batch of extension rows only: no ADT or SADT session is opened.
    fn extension_only(&mut self, extension: &mut dyn ExtensionDelete, cancel: &CancelFlag) -> Exit {
        match self.run(extension, cancel) {
            Ok(done) => {
                let exit = Exit {
                    code: done.outcome.exit,
                    stdout: done.outcome.stdout.clone(),
                    stderr: done.outcome.stderr.clone(),
                };

                self.report = Some(done);

                exit
            }
            Err(message) => {
                let (code, stderr) = extension.begin_failure(&message);

                Exit {
                    code,
                    stdout: String::new(),
                    stderr,
                }
            }
        }
    }

    fn attempt(
        &mut self,
        transport: &mut dyn SadtTransport,
        selection: &Selection,
        cancel: &CancelFlag,
        fresh: &mut dyn FnMut() -> Opened,
        mut extension: Option<&mut dyn ExtensionDelete>,
    ) -> Attempt {
        self.selected = Some(selection.backend);

        // The server would answer the LOCK and then refuse the DELETE, so nothing is sent
        // and the router may try RFC.
        if transport.stateful_supported() == Some(false) {
            let note = end_session(transport);

            return Attempt {
                fallback_allowed: true,
                ..Attempt::failed(
                    NO_STATEFUL_SESSION.to_owned(),
                    Failure::CapabilityUnavailable,
                )
            }
            .noted(note);
        }

        if self.runner {
            return self.attempt_runner(transport, selection, cancel, fresh);
        }

        let (result, unsent, fresh_note) = {
            let mut backend = AdtDelete::with_cancel(
                &mut *transport,
                &self.connection.user,
                fresh,
                cancel.clone(),
            );
            let result = match extension.as_deref_mut() {
                Some(extension) => {
                    let mut mixed =
                        MixedDelete::new(&mut backend, adt_label(selection.backend), extension);

                    self.run(&mut mixed, cancel)
                        .map_err(|message| mixed.begin_failure(&message))
                }
                None => self
                    .run(&mut backend, cancel)
                    .map_err(|message| backend.begin_failure(&message)),
            };

            (result, backend.unsent_failure(), backend.close_note())
        };

        // The batch is over and the lock, if any, is reported by the outcome; a failed
        // session end is only a note.
        let note = end_session(transport);

        // A read that never left the process and no delete request sent: another backend
        // may run the whole batch, unless the call was cancelled or ran out of time. A
        // token pins its backend, so the router refuses there; the report stays for the
        // per-object results. An extension delete that was sent keeps its report too.
        let sent = extension
            .as_ref()
            .is_some_and(|extension| extension.wrote());
        let fallback = unsent.filter(|_| !sent && !cancel.is_set() && !self.deadline.passed());

        self.conclude(result, fallback, fresh_note, note)
    }

    /// A batch with a DDIC or transaction row: those rows run in their own temporary class and the
    /// others over the ADT backend the router picked, as the command line runs them.
    fn attempt_runner(
        &mut self,
        transport: &mut dyn SadtTransport,
        selection: &Selection,
        cancel: &CancelFlag,
        fresh: &mut dyn FnMut() -> Opened,
    ) -> Attempt {
        // Both sides open their new sessions from the one router closure.
        let shared = RefCell::new(fresh);
        let mut open_adt = || {
            let mut guard = shared.borrow_mut();

            (**guard)()
        };
        let mut open_runner = || {
            let mut guard = shared.borrow_mut();

            (**guard)()
        };
        let (result, unsent, fresh_note) = {
            let mut adt = AdtDelete::with_cancel(
                &mut *transport,
                &self.connection.user,
                &mut open_adt,
                cancel.clone(),
            );
            let mut runner = DdicDelete::new(
                DdicEnv::of(self.connection, selection.backend),
                &mut open_runner,
                cancel.clone(),
            );
            let result = {
                let mut mixed = RunnerBatch::new(&mut adt, &mut runner);

                self.run(&mut mixed, cancel)
                    .map_err(|message| mixed.begin_failure(&message))
            };
            // The notes, the STOP, and the leftover class belong to the outcome of the batch.
            let result = result.map(|mut done| {
                runner.finish(&mut done.outcome);

                done
            });

            (
                result,
                runner.unsent_failure().or_else(|| adt.unsent_failure()),
                adt.close_note(),
            )
        };
        let note = end_session(transport);
        let fallback = unsent.filter(|_| !cancel.is_set() && !self.deadline.passed());

        self.conclude(result, fallback, fresh_note, note)
    }

    /// The attempt that a finished batch makes: its report and outcome, or the fallback to the
    /// other backend when nothing was sent.
    fn conclude(
        &mut self,
        result: Result<DeleteReport, (i32, String)>,
        fallback: Option<(Failure, String)>,
        fresh_note: Option<String>,
        note: Option<String>,
    ) -> Attempt {
        match result {
            Ok(done) => {
                let outcome = done.outcome.clone();

                self.report = Some(done);

                if let Some((failure, message)) = fallback {
                    // A simulation that moves on must not leave its results behind: if the
                    // next backend cannot even open, they would read as SIMULATED_OK.
                    // Only an execute pinned to this backend keeps them.
                    if self.plan.is_none() {
                        self.report = None;
                    }

                    return Attempt {
                        fallback_allowed: true,
                        ..Attempt::failed(message, failure)
                    }
                    .noted(fresh_note)
                    .noted(note);
                }

                Attempt {
                    outcome,
                    failure: None,
                    fallback_allowed: false,
                    detail: None,
                }
                .noted(fresh_note)
                .noted(note)
            }
            Err((exit, stderr)) => {
                self.report = None;

                if let Some((failure, message)) = fallback {
                    return Attempt {
                        fallback_allowed: true,
                        ..Attempt::failed(message, failure)
                    }
                    .noted(fresh_note)
                    .noted(note);
                }

                Attempt {
                    outcome: Outcome {
                        status: None,
                        exit,
                        stdout: String::new(),
                        stderr,
                    },
                    failure: None,
                    fallback_allowed: false,
                    detail: None,
                }
                .noted(fresh_note)
                .noted(note)
            }
        }
    }
}

fn listing_of(report: &delete::DeleteReport) -> Vec<String> {
    report
        .results
        .iter()
        .flat_map(|result| {
            std::iter::once(format!("{}:{}", result.kind.token(), result.name)).chain(
                result
                    .contents
                    .iter()
                    .map(|content| format!("  CONTENT {content}")),
            )
        })
        .collect()
}

/// Whether the operation of a ticket runs a temporary class: its type, or a row of its batch, is
/// a DDIC or transaction type.
fn uses_runner(ticket: &Ticket) -> bool {
    let rows = ticket.fields.get("objects").map_or("", String::as_str);

    ticket
        .fields
        .get("type")
        .map(String::as_str)
        .into_iter()
        .chain(rows.lines().filter_map(|row| row.split(':').next()))
        .any(crate::mcp::is_runner_type)
}

/// The lines of the run-1 preview of a DDIC deploy: the class, the size of the change, a bounded
/// diff excerpt, and the conversion verdicts.
fn ddic_lines(preview: &DdicPreview) -> Vec<String> {
    let mut lines = Vec::new();

    if let Some(runner) = &preview.runner {
        lines.push(format!("runner class: {}", sanitize(runner, SUMMARY_LINE)));
    }

    lines.push(format!(
        "changed XML rows: +{} -{}",
        preview.added, preview.removed
    ));
    lines.push("diff excerpt:".to_owned());
    lines.extend(
        preview
            .excerpt
            .iter()
            .take(SUMMARY_EXCERPT_LINES)
            .map(|line| sanitize(line, SUMMARY_LINE)),
    );

    if preview.excerpt.len() > SUMMARY_EXCERPT_LINES {
        lines.push(format!(
            "... {} more diff lines omitted (excerpt truncated)",
            preview.excerpt.len() - SUMMARY_EXCERPT_LINES
        ));
    }

    lines.extend(
        preview
            .conversion
            .iter()
            .take(SUMMARY_OBJECT_LINES)
            .map(|line| sanitize(line, SUMMARY_LINE)),
    );

    lines
}

/// What the human types: the name of the object, or `DELETE <n>` for a batch of `n`.
fn typed_text(ticket: &Ticket) -> String {
    if ticket.operation != Operation::Delete {
        return ticket.fields.get("name").cloned().unwrap_or_default();
    }

    let objects: Vec<&str> = ticket
        .fields
        .get("objects")
        .map_or_else(Vec::new, |objects| objects.lines().collect());

    match objects.as_slice() {
        [one] => one
            .split_once(':')
            .map_or(*one, |(_, name)| name)
            .to_owned(),
        many => format!("DELETE {}", many.len()),
    }
}

/// Unicode format characters (category Cf) plus the line and paragraph separators: invisible, yet
/// able to reorder or hide text in what the human confirms.
const INVISIBLE: &[(char, char)] = &[
    ('\u{00ad}', '\u{00ad}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061c}', '\u{061c}'),
    ('\u{06dd}', '\u{06dd}'),
    ('\u{070f}', '\u{070f}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08e2}', '\u{08e2}'),
    ('\u{180e}', '\u{180e}'),
    ('\u{200b}', '\u{200f}'),
    ('\u{2028}', '\u{202e}'),
    ('\u{2060}', '\u{206f}'),
    ('\u{feff}', '\u{feff}'),
    ('\u{fff9}', '\u{fffb}'),
    ('\u{110bd}', '\u{110bd}'),
    ('\u{110cd}', '\u{110cd}'),
    ('\u{13430}', '\u{1343f}'),
    ('\u{1bca0}', '\u{1bca3}'),
    ('\u{1d173}', '\u{1d17a}'),
    ('\u{e0001}', '\u{e0001}'),
    ('\u{e0020}', '\u{e007f}'),
];

/// Control and format characters and newlines stripped from a string SAP or the client supplied,
/// then cut.
fn sanitize(text: &str, limit: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|character| {
            !character.is_control()
                && !INVISIBLE
                    .iter()
                    .any(|(first, last)| (*first..=*last).contains(character))
        })
        .collect();

    if clean.chars().count() <= limit {
        return clean;
    }

    let mut cut: String = clean.chars().take(limit).collect();

    cut.push_str("...");
    cut
}

/// The backend a confirmation names: the one the simulation ran on, or the label of an extension
/// for what has no ADT path (an extension create, a batch of extension rows).
fn backend_text(ticket: &Ticket, registry: Option<&Registry>) -> String {
    if let Some(backend) = ticket.backend {
        return backend.token().to_owned();
    }

    let kinds = || registry.into_iter().flat_map(Registry::kinds);
    let created = ticket.fields.get("type").and_then(|token| {
        kinds()
            .find(|kind| kind.token() == token)
            .map(|kind| kind.spec().backend_label)
    });
    let only = ticket.plan.as_ref().and_then(|plan| {
        let (_, _, first) = plan.backends.first()?;

        plan.backends
            .iter()
            .all(|(_, _, label)| label == first)
            .then(|| kinds().find(|kind| kind.spec().backend_label == first))
            .flatten()
            .map(|kind| kind.spec().backend_label)
    });

    created.or(only).unwrap_or("unknown").to_owned()
}

/// What the human sees before a write: where, what, how big, and a bounded excerpt. Every string
/// that came from SAP or the client is sanitized, and the whole is bounded.
pub fn confirmation_summary(connection: &Connection, ticket: &Ticket) -> String {
    confirmation_summary_with(connection, ticket, None)
}

/// [`confirmation_summary`] for a server whose registry adds types and options.
pub fn confirmation_summary_with(
    connection: &Connection,
    ticket: &Ticket,
    registry: Option<&Registry>,
) -> String {
    let what = match ticket.operation {
        Operation::Deploy => "DEPLOY",
        Operation::Create => "CREATE",
        Operation::Delete => "DELETE (permanent)",
    };
    let backend = backend_text(ticket, registry);
    let mut lines = vec![format!(
        "{what} on {} (SID {}, client {}), backend {backend}",
        sanitize(&connection.id, SUMMARY_LINE),
        sanitize(&connection.system_id, SUMMARY_LINE),
        sanitize(&connection.client, SUMMARY_LINE),
    )];

    if ticket.operation == Operation::Delete {
        let named: Vec<String> = if ticket.listing.is_empty() {
            ticket
                .fields
                .get("objects")
                .map_or_else(Vec::new, |objects| {
                    objects.lines().map(str::to_owned).collect()
                })
        } else {
            ticket.listing.clone()
        };

        lines.push(format!(
            "objects ({}):",
            named.iter().filter(|line| !line.starts_with(' ')).count()
        ));
        lines.extend(
            named
                .iter()
                .take(SUMMARY_OBJECT_LINES)
                .map(|line| sanitize(line, SUMMARY_LINE)),
        );

        if named.len() > SUMMARY_OBJECT_LINES {
            lines.push(format!(
                "... {} more lines omitted",
                named.len() - SUMMARY_OBJECT_LINES
            ));
        }

        if let Some(tr) = ticket.fields.get("tr") {
            lines.push(format!("tr: {}", sanitize(tr, SUMMARY_LINE)));
        }

        // A mixed batch runs each object on the backend it was simulated on.
        if let Some(plan) = ticket.plan.as_ref().filter(|_| ticket.backend.is_some()) {
            lines.extend(plan.backends.iter().take(SUMMARY_OBJECT_LINES).map(
                |(kind, name, label)| {
                    sanitize(&format!("backend {kind}:{name} {label}"), SUMMARY_LINE)
                },
            ));
        }
    } else {
        let options = registry
            .into_iter()
            .flat_map(Registry::kinds)
            .flat_map(|kind| kind.spec().create_options)
            .map(|option| option.id);
        let keys = ["type", "name", "prog", "subc"]
            .into_iter()
            .chain(options)
            .chain(["include", "fugr", "package", "tr", "text"]);

        for key in keys {
            if let Some(value) = ticket.fields.get(key) {
                lines.push(format!("{key}: {}", sanitize(value, SUMMARY_LINE)));
            }
        }

        if let Some(main) = ticket.fields.get("main").or(ticket.main.as_ref()) {
            lines.push(format!("main: {}", sanitize(main, SUMMARY_LINE)));
        }
    }

    if uses_runner(ticket) {
        lines.push(
            "footprint: a temporary class is created in $TMP and removed after the run".to_owned(),
        );
    }

    if let Some(preview) = &ticket.ddic {
        lines.extend(ddic_lines(preview));
    }

    if let Some(warning) = &ticket.warning {
        lines.push(sanitize(warning, SUMMARY_LINE));
    }

    if let Some(change) = &ticket.change {
        lines.push(format!(
            "changes: +{} -{} lines",
            change.added, change.removed
        ));
        lines.push(format!(
            "removes {} of {} live lines",
            change.removed, change.live_lines
        ));
        lines.push("diff excerpt:".to_owned());
        lines.extend(
            change
                .excerpt
                .iter()
                .take(SUMMARY_EXCERPT_LINES)
                .map(|line| sanitize(line, SUMMARY_LINE)),
        );

        let left_out = change.omitted + change.excerpt.len().saturating_sub(SUMMARY_EXCERPT_LINES);

        if left_out > 0 {
            lines.push(format!(
                "... {left_out} more diff lines omitted (excerpt truncated)"
            ));
        }
    }

    let summary = lines.join("\n");

    if summary.chars().count() <= MAX_SUMMARY {
        return summary;
    }

    let mut cut: String = summary.chars().take(MAX_SUMMARY).collect();

    cut.push_str("\n... summary truncated");
    cut
}

impl<G: WriteGate> ToolHandler for McpTools<'_, G> {
    fn call(&mut self, name: &str, arguments: &Map<String, Value>) -> ToolResult {
        self.call_in(name, arguments, &mut NoSession::new())
    }

    fn call_in(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
        session: &mut dyn Session,
    ) -> ToolResult {
        // What earlier calls left is told now; what this call leaves waits for the next one.
        let carried = std::mem::take(&mut self.notices);
        let result = match name {
            "abap_simulate" => self.deploy(arguments, false, session),
            "abap_deploy" => self.deploy(arguments, true, session),
            "abap_create_simulate" => self.create(arguments, false, session),
            "abap_create" => self.create(arguments, true, session),
            "abap_delete_simulate" => self.delete(arguments, false, session),
            "abap_delete" => self.delete(arguments, true, session),
            "abap_read_source" if runner::runner_type_of(arguments).is_some() => {
                self.runner_read(arguments, session)
            }
            _ => {
                let result = self.read.call(name, arguments);

                return Self::with_notices(result, carried);
            }
        };
        let result = result.unwrap_or_else(|message| {
            // What SAP or the route said never reaches the client, not even in a refusal.
            let message = match self.read.connection(arguments) {
                Ok(connection) => diagnostic(&connection, &message),
                Err(_) => scrub(message, &known_lock_handles()),
            };

            if message.starts_with("CANCELED: ") || message.starts_with("REJECTED: ") {
                ToolResult::error(message)
            } else {
                ToolResult::error(format!("STOP: {message}"))
            }
        });

        Self::with_notices(result, carried)
    }
}

fn parse<T: ValueEnum>(text: &str, message: &str) -> Result<T, String> {
    T::from_str(text, false).map_err(|_| message.to_owned())
}

fn flag(arguments: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

/// The `objects` argument: a non-empty array of `TYPE:NAME` strings.
fn targets(
    arguments: &Map<String, Value>,
    extensions: &[ExtensionKind],
) -> Result<Vec<DeleteTarget>, String> {
    let objects = arguments
        .get("objects")
        .and_then(Value::as_array)
        .filter(|objects| !objects.is_empty())
        .ok_or("objects must be a non-empty array of TYPE:NAME strings")?;

    objects
        .iter()
        .map(|object| {
            object
                .as_str()
                .ok_or_else(|| "objects must be strings of the form TYPE:NAME".to_owned())
                .and_then(|text| DeleteTarget::parse_cli(text, extensions))
        })
        .collect()
}

/// The bound fields that are present.
fn bind<const N: usize>(fields: [(&str, Option<String>); N]) -> BTreeMap<String, String> {
    fields
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Pinging;

    impl Factories for Pinging {
        fn adt(&mut self, _: &Connection) -> Opened {
            unreachable!("only the ping is under test")
        }

        fn rfc(&mut self, _: &Connection) -> Opened {
            unreachable!("only the ping is under test")
        }

        fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
            unreachable!("only the ping is under test")
        }

        fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
            Err(TransportError::PreSendConnectivity("ping".to_owned()))
        }
    }

    #[test]
    fn a_terminal_that_does_not_exist_is_never_interactive_and_never_confirms() {
        assert!(!NoTerminal.interactive());
        assert!(!NoTerminal.confirm("Delete? [y/N] "));
    }

    #[test]
    fn the_factories_of_a_call_pass_a_ping_on_to_the_ones_they_wrap() {
        let mut inner = Pinging;
        let mut factories = WithCancel {
            inner: &mut inner,
            cancel: CancelFlag::new(),
            opened: None,
        };

        assert_eq!(
            factories.rfc_ping(&Connection::test_insecure()),
            Err(TransportError::PreSendConnectivity("ping".to_owned()))
        );
    }
}
