use crate::backend::{BackendKind, Failure, OperationKind, Router, Selection, SelectionReason};
use crate::cli::{Backend, Cli, Command};
use crate::config::{
    Config, ConfigPaths, Connection, LocalCredentialsPermission, local_credentials_permission,
    scrub, tls_warning,
};
use crate::domain::Outcome;
use crate::hash::local_source_lines;
use crate::object::{check_create, check_deploy_options, check_deploy_source, check_read};
use crate::operation::create::{CreateInput, CreateProg};
use crate::operation::delete::{self, Context};
use crate::operation::delete_adt::{AdtDelete, NO_STATEFUL_SESSION};
use crate::operation::deploy::{CancelFlag, DeployInput, DeployProg, DeployScope};
use crate::operation::doctor::Doctor;
use crate::operation::read::{ReadProg, ReadTarget};
use crate::policy::Terminal;
use crate::transport::http::{HttpConfig, HttpTransport};
use crate::transport::rfc::{
    DynamicRfcApi, Env, RfcApi, RfcLibrary, RfcTransport, connection_parameters,
};
use crate::transport::{SadtTransport, TransportError};
use clap::Parser;
use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// What the process prints and returns; `main` only writes it out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub type Opened = Result<Box<dyn SadtTransport>, TransportError>;

/// The RFC library a probe found: its identity and the discovery notes behind the choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RfcLibraryInfo {
    pub identity: RfcLibraryIdentity,
    /// Discovery decisions the doctor prints, such as a SAP GUI location it skipped.
    pub notes: Vec<String>,
}

/// The doctor-facing library identity: path, version, architecture, family, and source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RfcLibraryIdentity {
    pub path: String,
    pub version: String,
    pub architecture: String,
    pub family: String,
    pub source: String,
    /// The character size the library reads: `UTF-16` or `single-byte`.
    pub chars: String,
}

/// Opens the transports; tests replace it so no network is touched. A failed open must be a
/// pre-send failure (nothing was sent yet).
pub trait Factories {
    fn adt(&mut self, connection: &Connection) -> Opened;

    fn rfc(&mut self, connection: &Connection) -> Opened;

    /// Loads the RFC library without opening a connection; used by the doctor probe.
    fn rfc_library(&mut self, connection: &Connection) -> Result<RfcLibraryInfo, String>;

    /// One `RfcPing`: the only logon the doctor probe makes on RFC.
    fn rfc_ping(&mut self, connection: &Connection) -> Result<(), TransportError>;

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

pub struct RealFactories {
    cancel: CancelFlag,
}

impl RealFactories {
    pub fn new(cancel: CancelFlag) -> Self {
        Self { cancel }
    }

    fn rfc_api(connection: &Connection) -> Result<DynamicRfcApi, TransportError> {
        RfcLibrary::load(&Env::current())
            .and_then(|library| DynamicRfcApi::load(library, connection_parameters(connection)))
            .map_err(TransportError::PreSendConnectivity)
    }
}

impl Factories for RealFactories {
    fn adt(&mut self, connection: &Connection) -> Opened {
        let adt = connection.adt.as_ref().ok_or_else(|| {
            TransportError::PreSendConnectivity(format!(
                "ADT not configured for connection {}",
                connection.id
            ))
        })?;
        let transport = HttpTransport::connect(HttpConfig {
            base_url: format!("{}://{}:{}", adt.scheme, adt.host, adt.port),
            client: connection.client.clone(),
            username: connection.user.clone(),
            password: connection.password.clone(),
            ca_file: adt.ca_file.clone(),
            insecure: adt.insecure,
            timeout: HTTP_TIMEOUT,
        })?;

        Ok(Box::new(transport))
    }

    fn rfc(&mut self, connection: &Connection) -> Opened {
        let api = Self::rfc_api(connection)?;

        Ok(Box::new(RfcTransport::new(api)))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        let env = Env::current();
        let library = RfcLibrary::load(&env)?;
        let identity = library.identity()?;
        let mut notes = env.notes().to_vec();

        notes.extend(library.notes().iter().cloned());
        notes.push(library.trace_target().label());

        Ok(RfcLibraryInfo {
            identity: RfcLibraryIdentity {
                path: identity.path().display().to_string(),
                version: identity.version().to_owned(),
                architecture: identity.arch().label().to_owned(),
                family: identity.family().label().to_owned(),
                source: identity.source().label().to_owned(),
                chars: identity.width().label().to_owned(),
            },
            notes,
        })
    }

    fn rfc_ping(&mut self, connection: &Connection) -> Result<(), TransportError> {
        let mut api = Self::rfc_api(connection)?;
        let result = api.ping();

        api.close();
        result
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

pub fn run(
    args: Vec<String>,
    env: BTreeMap<String, String>,
    factories: &mut dyn Factories,
) -> Exit {
    // Test-only: an unknown value stops the command before any connection is opened.
    #[cfg(feature = "fault-injection")]
    let warning =
        match crate::fault::Fault::from_lookup(env.get(crate::fault::ENV).map(String::as_str)) {
            Ok(faults) => crate::fault::Fault::warning(&faults),
            Err(message) => return stop(message),
        };
    #[cfg(not(feature = "fault-injection"))]
    let warning: Option<String> = None;
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) if error.use_stderr() => {
            return with_warning(
                stop(error.render().to_string().trim_end()),
                warning.as_deref(),
            );
        }
        Err(error) => {
            return with_warning(
                Exit {
                    code: error.exit_code(),
                    stdout: error.render().to_string().trim_end().to_owned(),
                    stderr: String::new(),
                },
                warning.as_deref(),
            );
        }
    };
    let is_doctor = matches!(cli.command, Command::Doctor { .. });
    let Some(conn) = connection_id(&cli.command) else {
        return with_warning(mcp(&env), warning.as_deref());
    };
    let (connection, local_credentials) = match resolve(conn, &env) {
        Ok(resolved) => resolved,
        Err(message) => return with_warning(stop(message), warning.as_deref()),
    };
    let exit = with_warning(
        execute(cli.command, &connection, local_credentials, factories),
        warning.as_deref(),
    );
    let mut stdout = exit.stdout;
    let stderr = exit.stderr;

    if let Some(warning) = tls_warning(&connection)
        && !stdout.lines().any(|line| line == warning)
    {
        stdout = if stdout.is_empty() {
            warning
        } else {
            format!("{warning}\n{stdout}")
        };
    }

    let mut secrets = connection.secrets();

    if is_doctor {
        secrets.extend(connection.hosts());
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    }

    Exit {
        code: exit.code,
        stdout: scrub(stdout, &secrets),
        stderr: scrub(stderr, &secrets),
    }
}

fn with_warning(mut exit: Exit, warning: Option<&str>) -> Exit {
    if let Some(warning) = warning
        && !exit.stderr.lines().any(|line| line == warning)
    {
        exit.stderr = if exit.stderr.is_empty() {
            warning.to_owned()
        } else {
            format!("{warning}\n{}", exit.stderr)
        };
    }

    exit
}

fn stop(message: impl std::fmt::Display) -> Exit {
    Exit {
        code: 2,
        stdout: String::new(),
        stderr: format!("STOP: {message}"),
    }
}

fn connection_id(command: &Command) -> Option<&str> {
    match command {
        Command::Read { conn, .. }
        | Command::Deploy { conn, .. }
        | Command::Create { conn, .. }
        | Command::ReadTable { conn, .. }
        | Command::Delete { conn, .. }
        | Command::Doctor { conn } => Some(conn),
        Command::Mcp => None,
    }
}

/// `mcp` loads every policy once; an invalid policy stops the start with exit 2.
fn mcp(env: &BTreeMap<String, String>) -> Exit {
    let loaded = ConfigPaths::from_env(env).and_then(|paths| Config::load(paths, env));

    match loaded {
        // shortcut: the stdio server is a later task; the policy snapshot is only validated here.
        Ok(config) => {
            let _ = config.mcp_policies();

            stop("the MCP server is not available in this build yet")
        }
        Err(message) => stop(message),
    }
}

fn resolve(
    connection_id: &str,
    env: &BTreeMap<String, String>,
) -> Result<(Connection, LocalCredentialsPermission), String> {
    let paths = ConfigPaths::from_env(env)?;
    // Read-only and metadata-only; the doctor line reports it, no command changes it.
    let local_credentials = local_credentials_permission(&paths.local);
    let connection = Config::load(paths, env)?.resolve(connection_id, env)?;

    Ok((connection, local_credentials))
}

/// One finished try on one backend: what to print and whether a switch is still allowed.
struct Attempt {
    outcome: Outcome,
    failure: Option<Failure>,
    fallback_allowed: bool,
    /// The failure message, quoted as the reason when the next backend is a fallback.
    detail: Option<String>,
}

impl Attempt {
    fn from_error(error: &TransportError) -> Self {
        Self::failed(error.to_string(), Failure::from(error))
    }

    fn failed(message: String, failure: Failure) -> Self {
        Self {
            outcome: Outcome {
                status: None,
                exit: 1,
                stdout: String::new(),
                stderr: format!("ERROR: {message}"),
            },
            failure: Some(failure),
            fallback_allowed: failure == Failure::PreSendConnectivity,
            detail: Some(message),
        }
    }
}

fn execute(
    command: Command,
    connection: &Connection,
    local_credentials: LocalCredentialsPermission,
    factories: &mut dyn Factories,
) -> Exit {
    match command {
        Command::Doctor { .. } => {
            let report = Doctor::probe(connection, local_credentials, factories);

            Exit {
                code: report.exit(),
                stdout: report.render(),
                stderr: String::new(),
            }
        }
        Command::ReadTable { .. } => stop("read-table is not available in this build yet"),
        Command::Delete {
            targets,
            simulate,
            tr,
            plan,
            ..
        } => route(
            OperationKind::Delete,
            None,
            false,
            connection,
            factories,
            |transport, _, _, fresh| {
                // The server would answer the LOCK and then refuse the DELETE, so nothing is sent
                // and the router may try RFC.
                if transport.stateful_supported() == Some(false) {
                    let _ = transport.close();

                    return Attempt {
                        fallback_allowed: true,
                        ..Attempt::failed(
                            NO_STATEFUL_SESSION.to_owned(),
                            Failure::CapabilityUnavailable,
                        )
                    };
                }

                let context = Context {
                    conn: &connection.id,
                    user: &connection.user,
                    policies: &connection.policies,
                    confirmer: &Terminal,
                };
                let outcome = {
                    let mut backend = AdtDelete::new(&mut *transport, &connection.user, fresh);

                    delete::run(
                        &mut backend,
                        &context,
                        &targets,
                        tr.as_deref(),
                        simulate,
                        &plan,
                    )
                };

                // The batch is over and the lock, if any, is reported by the outcome.
                let _ = transport.close();

                Attempt {
                    outcome,
                    failure: None,
                    fallback_allowed: false,
                    detail: None,
                }
            },
        ),
        Command::Mcp => stop("the MCP server needs no connection"),
        Command::Read {
            kind,
            name,
            out,
            force,
            include,
            fugr,
            lines,
            ..
        } => {
            if let Err(message) = check_read(kind, &name, &out, include, fugr.as_deref(), lines) {
                return stop(message);
            }

            // A local overwrite rule: it stops before the connection, like every other option rule.
            if lines.is_none() && out.exists() && !force {
                return stop(format!(
                    "{} already exists; use --force to overwrite",
                    out.display()
                ));
            }

            let target = ReadTarget::new(kind, &name)
                .group(fugr.as_deref())
                .include(include)
                .lines(lines);

            route(
                OperationKind::Read(kind),
                None,
                false,
                connection,
                factories,
                |transport, _, _, _| {
                    let read =
                        ReadProg::run_target_classified(transport, target, &out, force, None);

                    // A read holds no lock, so a failed session end changes nothing here.
                    let _ = transport.close();

                    match read {
                        Ok(outcome) => Attempt {
                            outcome,
                            failure: None,
                            fallback_allowed: false,
                            detail: None,
                        },
                        Err(error) if error.stop => Attempt {
                            outcome: Outcome {
                                status: None,
                                exit: 2,
                                stdout: String::new(),
                                stderr: format!("STOP: {}", error.message),
                            },
                            failure: None,
                            fallback_allowed: false,
                            detail: None,
                        },
                        Err(error) => Attempt::failed(error.message, error.failure),
                    }
                },
            )
        }
        Command::Create {
            kind,
            name,
            text,
            package,
            fugr,
            tr,
            simulate,
            backend,
            ..
        } => {
            if let Err(message) = check_create(
                kind,
                &name,
                &text,
                package.as_deref(),
                fugr.as_deref(),
                tr.as_deref(),
            ) {
                return stop(message);
            }

            let input = CreateInput {
                kind,
                name,
                text,
                package,
                group: fugr,
                tr,
                simulate,
            };

            route(
                OperationKind::Create(kind),
                backend.map(backend_kind),
                false,
                connection,
                factories,
                |transport, selection, cancel, _| {
                    let result = CreateProg::run_with_cancel(
                        transport,
                        input.clone(),
                        Some(selection),
                        Some(connection),
                        cancel,
                    );

                    Attempt {
                        outcome: result.outcome,
                        failure: result.failure,
                        fallback_allowed: result.fallback_allowed,
                        detail: result.detail,
                    }
                },
            )
        }
        Command::Deploy {
            kind,
            name,
            file,
            simulate,
            base,
            tr,
            backend,
            include,
            create_include,
            fugr,
            ..
        } => {
            if let Err(message) =
                check_deploy_options(kind, &name, include, create_include, fugr.as_deref())
            {
                return stop(message);
            }

            let text = match fs::read_to_string(&file) {
                Ok(text) => text,
                Err(error) => return stop(format!("cannot read {}: {error}", file.display())),
            };
            let source = local_source_lines(&text);

            if let Err(message) = check_deploy_source(kind, &name, &source) {
                return stop(message);
            }

            let base_given = base.is_some();
            let scope = DeployScope::new(kind)
                .group(fugr.as_deref())
                .include(include)
                .create_include(create_include);
            let input = DeployInput {
                name,
                source,
                base,
                simulate,
                tr,
            };

            route(
                OperationKind::Deploy(kind),
                backend.map(backend_kind),
                base_given,
                connection,
                factories,
                |transport, selection, cancel, _| {
                    let result = DeployProg::run_scoped(
                        transport,
                        input.clone(),
                        scope.clone(),
                        Some(selection),
                        Some(connection),
                        cancel,
                    );

                    Attempt {
                        outcome: result.outcome,
                        failure: result.failure,
                        fallback_allowed: result.fallback_allowed,
                        detail: result.detail,
                    }
                },
            )
        }
    }
}

fn backend_kind(backend: Backend) -> BackendKind {
    match backend {
        Backend::Adt => BackendKind::AdtHttp,
        Backend::Rfc => BackendKind::RfcSadt,
    }
}

/// Runs the operation on the backend the router picks. A second backend is tried only when the
/// first one failed before it sent a write request and neither `--backend` nor `--base` pins it.
fn route(
    operation: OperationKind,
    explicit: Option<BackendKind>,
    base_given: bool,
    connection: &Connection,
    factories: &mut dyn Factories,
    mut attempt: impl FnMut(
        &mut dyn SadtTransport,
        &Selection,
        &CancelFlag,
        &mut dyn FnMut() -> Opened,
    ) -> Attempt,
) -> Exit {
    let router = Router::new(connection.adt.is_some(), true);
    let cancel = factories.cancel();
    let mut failure = Failure::Initial;
    let mut earlier: Vec<String> = Vec::new();
    // Held back until the router agrees to the fallback, so a pinned backend never claims one.
    let mut fallback_note: Option<String> = None;
    let mut reason = match explicit {
        Some(_) => SelectionReason::ExplicitBackend,
        None if connection.adt.is_some() => SelectionReason::DefaultAdt,
        None => SelectionReason::RfcOnly,
    };

    loop {
        let backend = match router.select(operation, explicit, base_given, failure) {
            Ok(backend) => backend,
            Err(message) if earlier.is_empty() => return stop(message),
            Err(message) => {
                earlier.push(format!("NOTE: {message}"));

                return exit_of(
                    earlier,
                    Outcome {
                        status: None,
                        exit: 1,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                );
            }
        };
        if let Some(note) = fallback_note.take() {
            earlier.push(note);
        }
        let selection = Selection::new(backend, reason);
        let opened = match backend {
            BackendKind::AdtHttp => factories.adt(connection),
            BackendKind::RfcSadt => factories.rfc(connection),
        };
        let done = match opened {
            Ok(mut transport) => {
                // A new session of the same backend, for the read that settles a lost reply.
                let mut fresh = || match backend {
                    BackendKind::AdtHttp => factories.adt(connection),
                    BackendKind::RfcSadt => factories.rfc(connection),
                };

                attempt(&mut *transport, &selection, &cancel, &mut fresh)
            }
            Err(error) => Attempt::from_error(&error),
        };

        if backend == BackendKind::RfcSadt || !done.fallback_allowed {
            return exit_of(earlier, done.outcome);
        }

        failure = done.failure.unwrap_or(Failure::Initial);
        reason = SelectionReason::Fallback(done.detail.unwrap_or_else(|| failure.to_string()));
        earlier.push(done.outcome.stderr);
        fallback_note = Some(format!(
            "NOTE: {backend} failed before anything was written; falling back to RFC SADT"
        ));
    }
}

fn exit_of(mut stderr: Vec<String>, outcome: Outcome) -> Exit {
    stderr.push(outcome.stderr);
    stderr.retain(|line| !line.is_empty());

    // A backend that was abandoned before it sent anything broke the run only when the backend
    // that replaced it failed too; a fallback that worked reports its cause as a note.
    if outcome.exit == 0 {
        stderr = stderr.into_iter().map(as_note).collect();
    }

    Exit {
        code: outcome.exit,
        stdout: outcome.stdout,
        stderr: stderr.join("\n"),
    }
}

fn as_note(block: String) -> String {
    match block.strip_prefix("ERROR: ") {
        Some(rest) => format!("NOTE: {rest}"),
        None => block,
    }
}
