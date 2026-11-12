//! The extension point: the whole stability surface another crate may build on.
//!
//! A crate registers exactly one [`Extension`] in a [`Registry`] and hands it to [`run_main`].
//! The extension can add object types to the `create`, `read` and `delete` grammar and to the MCP
//! write tools (hook a, c), run its own delete backend next to the ADT one inside a mixed batch
//! (hook b), add a `doctor` line (hook d) and a `drop-reply:<NAME>` fault target (hook e), open
//! generic function-module connections through the public factories (hook f), run operations
//! outside the router with the connection, the cancel flag and the `--backend` pin (hook g), and
//! set the version string (hook h).
//!
//! Everything an extension needs is re-exported flat from here. A change to a type that this
//! module does not re-export is not an extension point change; removing or retyping an item below
//! is. Without a registered extension every command behaves exactly as it does with this module
//! absent.

mod delete;

pub use crate::app::{Exit, Factories, RfcProbe};
pub use crate::backend::BackendKind;
pub use crate::cli::{Backend, DeleteKind, LineRange};
pub use crate::config::Connection;
pub use crate::domain::{Outcome, Status};
#[cfg(feature = "fault-injection")]
pub use crate::fault::{ENV, Fault, Target, delete_reply_dropped, note_delete_reply_dropped};
pub use crate::hash::{source_hash, validate_name_within};
pub use crate::object::DEFAULT_PACKAGE;
pub use crate::operation::delete::{
    DeleteBackend, DeleteReply, DeleteStatus, InactiveFact, ObjectFacts, Plan, Reconcile,
    SourceFact, TrLock,
};
pub use crate::operation::deploy::{CancelFlag, INTERRUPT_EXIT_CODE};
pub use crate::operation::read::{ReadError, emit_lines};
pub use crate::transport::TransportError;
pub use crate::transport::rfc::{
    Cell, CellKind, ColumnRead, Direction, FUNCTION_NOT_FOUND_KEY, FunctionApi, FunctionCall,
    FunctionInfo, FunctionResult, MAX_CALL_CHARS, MAX_CALL_PARAMETERS, MAX_CALL_ROWS,
    ParameterInfo, ParameterType, TableRead, TableWrite,
};
// `MixedDelete` and `adt_pin` are kept on purpose: the private delete tests drive a batch through
// them, and `RfcProbe` is the return type of `Extension::rfc_probe`.
pub use delete::{ExtensionDelete, MixedDelete, adt_label, adt_pin};

use crate::app::{Opened, RealFactories, RfcLibraryInfo};
use crate::operation::deploy::InterruptAction;
use crate::transport::http::end_open_sessions;
use crate::transport::rfc::after_trace_environment;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// The object types the standard grammar owns, among them the DDIC and transaction types built in
/// since 0.16.0; an extension may not reuse one.
const STANDARD_TOKENS: [&str; 10] = [
    "PROG", "FUGR", "FUNC", "CLAS", "INTF", "DTEL", "DOMA", "TABL", "TTYP", "TRAN",
];
/// The write targets of the standard fault grammar; an extension may not reuse one.
const STANDARD_FAULT_TARGETS: [&str; 5] = ["PUT", "ACTIVATE", "CREATE", "DELETE", "CLASSRUN"];
/// The ids and long flags of the standard create arguments and MCP properties; an extension
/// option may not reuse one.
const STANDARD_OPTION_NAMES: [&str; 16] = [
    "kind",
    "type",
    "name",
    "file",
    "text",
    "package",
    "fugr",
    "prog",
    "source",
    "subc",
    "tr",
    "simulate",
    "backend",
    "conn",
    "plan_token",
    "help",
];
const ONE_EXTENSION: &str = "only one extension per process";

/// Which grammars accept a type: `read`, `create` and the `delete` batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Verbs(u8);

impl Verbs {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1);
    pub const CREATE: Self = Self(2);
    pub const DELETE: Self = Self(4);

    /// Both sets together.
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// How the value of an extension option enters the plan token of an MCP write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Normalize {
    /// Upper-cased, like an object name.
    Upper,
    /// Bound as the client sent it.
    Verbatim,
}

/// One option of a type, as a CLI flag of `create` and as an MCP property.
#[derive(Debug)]
pub struct OptionSpec {
    /// The clap argument id, the MCP property key and the token-binding key.
    pub id: &'static str,
    /// The CLI long flag without dashes.
    pub long: &'static str,
    pub value_name: &'static str,
    /// The CLI help text.
    pub help: &'static str,
    /// The MCP property description.
    pub mcp_description: &'static str,
    pub normalize: Normalize,
    /// The STOP text when the option is given with a type whose `create_options` omit it.
    pub misuse: &'static str,
}

/// The static data of one extension type. Plain data, so a type token needs no registry argument.
#[derive(Debug)]
pub struct KindSpec {
    /// The upper-case type token of the CLI and MCP grammar.
    pub token: &'static str,
    /// The help text of the type in the long `--help`; empty for none.
    pub help: &'static str,
    pub verbs: Verbs,
    /// `Some(text)`: `deploy <TOKEN>` parses and then stops with this text.
    pub deploy_refusal: Option<&'static str>,
    /// The name limit of read, create and delete.
    pub max_name: usize,
    /// The `--text` limit of create.
    pub max_text: usize,
    pub create_options: &'static [OptionSpec],
    /// The delete order on the scale of the standard types (FUNC 1, CLAS 2, INTF 3, PROG 4, FUGR 5).
    pub delete_rank: u8,
    /// The backend printed per object, stored in the plan and shown in the MCP confirmation.
    pub backend_label: &'static str,
    /// Whether a plan without recorded backends (from another tool) is accepted for this type.
    pub plan_backend_optional: bool,
    /// The self-guard of the type; `None` is never self.
    pub is_self: Option<fn(&str) -> bool>,
    /// The text of `abap_read_source` for this type; `None` is the generic unknown-type text.
    pub mcp_read_refusal: Option<&'static str>,
}

/// An extension type: a handle on its static [`KindSpec`], equal by token.
#[derive(Clone, Copy)]
pub struct ExtensionKind(&'static KindSpec);

impl ExtensionKind {
    pub const fn new(spec: &'static KindSpec) -> Self {
        Self(spec)
    }

    pub fn spec(self) -> &'static KindSpec {
        self.0
    }

    pub fn token(self) -> &'static str {
        self.0.token
    }
}

impl PartialEq for ExtensionKind {
    fn eq(&self, other: &Self) -> bool {
        self.0.token == other.0.token
    }
}

impl Eq for ExtensionKind {}

impl Hash for ExtensionKind {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.token.hash(state);
    }
}

impl std::fmt::Debug for ExtensionKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ExtensionKind")
            .field(&self.0.token)
            .finish()
    }
}

/// The options of an extension type that a command gave, by [`OptionSpec::id`].
pub type ExtensionOptions = BTreeMap<&'static str, String>;

pub struct ReadRequest<'a> {
    pub name: &'a str,
    pub out: &'a Path,
    pub force: bool,
    pub lines: Option<LineRange>,
    pub options: &'a ExtensionOptions,
}

pub struct CreateRequest<'a> {
    pub name: &'a str,
    pub text: &'a str,
    pub package: Option<&'a str>,
    pub tr: Option<&'a str>,
    pub simulate: bool,
    pub options: &'a ExtensionOptions,
}

/// The operation a [`Extension::preflight`] guards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionOp {
    Read,
    Create,
    Delete,
}

impl ExtensionOp {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Delete => "delete",
        }
    }
}

/// The factories of one command, lent to the router (ADT rows) and to the extension (function-module
/// rows) at the same time: each call borrows for its own duration only.
pub struct SharedFactories<'f>(RefCell<&'f mut dyn Factories>);

impl<'f> SharedFactories<'f> {
    pub fn new(factories: &'f mut dyn Factories) -> Self {
        Self(RefCell::new(factories))
    }

    pub fn lend(&self) -> impl Factories + '_ {
        Lend(&self.0)
    }
}

struct Lend<'a, 'b>(&'a RefCell<&'b mut dyn Factories>);

impl Factories for Lend<'_, '_> {
    fn adt(&mut self, connection: &Connection) -> Opened {
        self.0.borrow_mut().adt(connection)
    }

    fn rfc(&mut self, connection: &Connection) -> Opened {
        self.0.borrow_mut().rfc(connection)
    }

    fn rfc_library(&mut self, connection: &Connection) -> Result<RfcLibraryInfo, String> {
        self.0.borrow_mut().rfc_library(connection)
    }

    fn rfc_ping(&mut self, connection: &Connection) -> Result<(), TransportError> {
        self.0.borrow_mut().rfc_ping(connection)
    }

    fn rfc_probe(&mut self, connection: &Connection) -> RfcProbe {
        self.0.borrow_mut().rfc_probe(connection)
    }

    fn function_api(
        &mut self,
        connection: &Connection,
    ) -> Result<Box<dyn FunctionApi>, TransportError> {
        self.0.borrow_mut().function_api(connection)
    }

    fn function_api_fresh(
        &mut self,
        connection: &Connection,
    ) -> Result<Box<dyn FunctionApi>, TransportError> {
        self.0.borrow_mut().function_api_fresh(connection)
    }

    fn cancel(&self) -> CancelFlag {
        self.0.borrow().cancel()
    }
}

/// What one extension operation receives: the connection, the cancel flag (hold its write window
/// during the one write), the `--backend` pin and the factories.
#[derive(Clone, Copy)]
pub struct RunContext<'r> {
    pub connection: &'r Connection,
    pub cancel: &'r CancelFlag,
    /// `--backend` on the CLI; the backend the token was minted on for an MCP execute.
    pub pin: Option<Backend>,
    pub factories: &'r SharedFactories<'r>,
}

/// What the `doctor` line of an extension is built from.
pub struct DoctorProbe<'a> {
    pub connection: &'a Connection,
    /// The RFC library text when the library was not found.
    pub library_error: Option<&'a str>,
    /// Whether the RFC SADT probe passed.
    pub rfc_passed: bool,
    /// The logon of the passed ping, borrowed; `Err` is its open error; `None` when the ping failed
    /// or did not run.
    pub function_api: Option<Result<&'a mut dyn FunctionApi, &'a TransportError>>,
}

/// An extension: object types and the operations behind them.
pub trait Extension {
    /// For diagnostics.
    fn name(&self) -> &'static str;

    fn kinds(&self) -> &'static [ExtensionKind];

    /// Hook a: the local rules of `read`, before any connection. `Err` is a STOP.
    fn check_read(&self, _: ExtensionKind, _: &ReadRequest<'_>) -> Result<(), String> {
        Ok(())
    }

    /// Hook a: the local rules of `create`, after the common ones and before the FILE rules.
    fn check_create(&self, _: ExtensionKind, _: &CreateRequest<'_>) -> Result<(), String> {
        Ok(())
    }

    /// Hook g: a refusal before any connection (a connection without RFC, a `--backend adt` pin).
    /// Called for `read`, `create` and, per extension type in the batch, `delete`. `Err` is a STOP.
    fn preflight(
        &self,
        op: ExtensionOp,
        kind: ExtensionKind,
        connection: &Connection,
        pin: Option<Backend>,
    ) -> Result<(), String>;

    /// Hook g: one `read`, on the extension's own connection, never routed and never falling back.
    fn read(&self, kind: ExtensionKind, request: &ReadRequest<'_>, run: RunContext<'_>) -> Outcome;

    /// Hook g: one `create`, like [`Extension::read`].
    fn create(
        &self,
        kind: ExtensionKind,
        request: &CreateRequest<'_>,
        run: RunContext<'_>,
    ) -> Outcome;

    /// Hooks b and g: the delete backend of the extension's rows. `Err(outcome)` is a backend
    /// that could not open (STOP exit 2, or a failed request exit 1).
    fn delete_backend<'r>(
        &'r self,
        run: RunContext<'r>,
    ) -> Result<Box<dyn ExtensionDelete + 'r>, Outcome>;

    /// Hook d: the line after `RFC SADT probe` and before the `capability` lines. It never decides
    /// the exit code.
    fn doctor_line(&self, _: &mut DoctorProbe<'_>) -> Option<String> {
        None
    }

    /// Hook e: the `drop-reply:<NAME>` targets. Read only in `fault-injection` builds.
    fn fault_targets(&self) -> &'static [&'static str] {
        &[]
    }
}

/// The extensions of the process: none for the public binary, one for a binary that adds types.
pub struct Registry {
    extension: Option<Box<dyn Extension>>,
    version: &'static str,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// No extension: the public binary.
    pub fn new() -> Self {
        Self {
            extension: None,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// Registers the one extension.
    ///
    /// # Panics
    ///
    /// A programmer error at start-up: a second extension, a type token that is a standard token
    /// or repeats, a backend label that is an ADT label, an option id or long flag that is a
    /// standard create argument (or that two options share with different meanings), or a fault
    /// target that is a standard one.
    #[must_use]
    pub fn with(mut self, extension: impl Extension + 'static) -> Self {
        assert!(self.extension.is_none(), "{ONE_EXTENSION}");

        let kinds = extension.kinds();

        for (index, kind) in kinds.iter().enumerate() {
            let token = kind.token();

            assert!(
                !STANDARD_TOKENS.contains(&token),
                "extension type {token} is a standard type"
            );
            assert!(
                kinds[..index]
                    .iter()
                    .all(|earlier| earlier.token() != token),
                "extension type {token} is registered twice"
            );
            assert!(
                [BackendKind::AdtHttp, BackendKind::RfcSadt]
                    .into_iter()
                    .all(|backend| adt_label(backend) != kind.spec().backend_label),
                "extension type {token} uses the label of an ADT backend"
            );
        }

        // The same option id on several types is one option; its long flag must then agree.
        let options: Vec<&OptionSpec> = kinds
            .iter()
            .flat_map(|kind| kind.spec().create_options)
            .collect();

        for (index, option) in options.iter().enumerate() {
            for name in [option.id, option.long] {
                assert!(
                    !STANDARD_OPTION_NAMES.contains(&name),
                    "extension option {name} is a standard create argument"
                );
            }

            for earlier in &options[..index] {
                assert!(
                    (earlier.id == option.id) == (earlier.long == option.long),
                    "extension options {} and {} share an id or a long flag",
                    earlier.id,
                    option.id
                );
            }
        }

        for target in extension.fault_targets() {
            assert!(
                !STANDARD_FAULT_TARGETS.contains(target),
                "fault target {target} is a standard target"
            );
        }

        self.extension = Some(Box::new(extension));

        self
    }

    /// The text of `--version` and of the MCP `serverInfo`; the default is the crate version.
    #[must_use]
    pub fn version(mut self, text: impl Into<String>) -> Self {
        // One leak per registry, at start-up: the text lives as long as the process.
        self.version = Box::leak(text.into().into_boxed_str());

        self
    }

    pub fn version_text(&self) -> &'static str {
        self.version
    }

    pub fn extension(&self) -> Option<&dyn Extension> {
        self.extension.as_deref()
    }

    /// The type of the registered extension for a token (case-sensitive, upper case).
    pub fn kind(&self, token: &str) -> Option<ExtensionKind> {
        self.kinds().find(|kind| kind.token() == token)
    }

    pub fn kinds(&self) -> impl Iterator<Item = ExtensionKind> + '_ {
        self.extension
            .iter()
            .flat_map(|extension| extension.kinds().iter().copied())
    }

    pub fn fault_targets(&self) -> Vec<&'static str> {
        self.extension
            .iter()
            .flat_map(|extension| extension.fault_targets().iter().copied())
            .collect()
    }
}

/// `app::run` with a registry: the CLI and `mcp`.
pub fn run_with(
    args: Vec<String>,
    env: BTreeMap<String, String>,
    factories: &mut dyn Factories,
    registry: &Registry,
) -> Exit {
    crate::app::run_registered(args, env, factories, registry)
}

/// The whole process: trace environment, interrupt handler, environment map, run, print, exit.
/// The public `main` is `run_main(Registry::new())`.
pub fn run_main(registry: Registry) -> ! {
    let cancel = CancelFlag::new();
    let on_interrupt = cancel.clone();

    // A held lock must be released before the process ends, so the deploy gets the flag. Without
    // a lock there is nothing to clean up and an interrupt keeps its usual meaning: exit 130.
    // `termination` makes SIGTERM and SIGHUP (a dropped ssh session) take the same path on Unix.
    // On Windows the handler also runs for CTRL_BREAK, but the OS ends the process for
    // CTRL_CLOSE, CTRL_LOGOFF and CTRL_SHUTDOWN as soon as the handler returns, so a held lock may
    // stay locked there.
    // The trace environment goes first: `ctrlc` starts a thread, after which `set_var` is unsound.
    after_trace_environment(|| {
        let _ = ctrlc::set_handler(move || {
            if on_interrupt.on_interrupt() == InterruptAction::ExitNow {
                // No lock is held, but an open ADT session still keeps its context on SAP.
                end_open_sessions();
                std::process::exit(INTERRUPT_EXIT_CODE);
            }
        });
    });

    let env = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let exit = run_with(
        std::env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        env,
        &mut RealFactories::new(cancel),
        &registry,
    );

    if !exit.stdout.is_empty() {
        println!("{}", exit.stdout);
    }

    if !exit.stderr.is_empty() {
        eprintln!("{}", exit.stderr);
    }

    std::process::exit(exit.code);
}

/// The `doctor` line of the registered extension, if any. It lives here so that
/// `operation/doctor.rs` never names the function-call layer.
pub(crate) fn doctor_line(
    registry: &Registry,
    connection: &Connection,
    library_error: Option<&str>,
    rfc_passed: bool,
    probe: Option<Result<Box<dyn FunctionApi>, TransportError>>,
) -> Option<String> {
    let extension = registry.extension()?;
    let mut probe = probe;
    let mut doctor = DoctorProbe {
        connection,
        library_error,
        rfc_passed,
        function_api: match probe.as_mut() {
            Some(Ok(api)) => Some(Ok(&mut **api)),
            Some(Err(error)) => Some(Err(&*error)),
            None => None,
        },
    };

    extension.doctor_line(&mut doctor)
}
