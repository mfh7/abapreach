use crate::backend::{BackendKind, Failure, OperationKind, Router, Selection, SelectionReason};
use crate::cli::{Backend, Cli, Command};
use crate::config::{
    Config, ConfigPaths, Connection, LocalCredentialsPermission, local_credentials_permission,
    scrub, tls_warning,
};
use crate::domain::Outcome;
use crate::hash::{local_source_lines, validate_name, validate_prog};
use crate::operation::deploy::{CancelFlag, DeployInput, DeployProg};
use crate::operation::doctor::Doctor;
use crate::operation::read::ReadProg;
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
            Ok(fault) => fault.map(crate::fault::Fault::warning),
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
    let (connection, local_credentials) = match resolve(connection_id(&cli.command), &env) {
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

    let secrets = connection.secrets();

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

fn connection_id(command: &Command) -> &str {
    match command {
        Command::Read { conn, .. } | Command::Deploy { conn, .. } | Command::Doctor { conn } => {
            conn
        }
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
        Command::Read {
            name, out, force, ..
        } => {
            if let Err(message) = validate_name(&name) {
                return stop(message);
            }

            if out.exists() && !force {
                return stop(format!(
                    "{} already exists; use --force to overwrite",
                    out.display()
                ));
            }

            route(
                OperationKind::ReadProg,
                None,
                false,
                connection,
                factories,
                |transport, _, _| {
                    let read = ReadProg::run_classified(transport, &name, &out, force, None);

                    // A read holds no lock, so a failed session end changes nothing here.
                    let _ = transport.close();

                    match read {
                        Ok(outcome) => Attempt {
                            outcome,
                            failure: None,
                            fallback_allowed: false,
                            detail: None,
                        },
                        Err(error) => Attempt::failed(error.message, error.failure),
                    }
                },
            )
        }
        Command::Deploy {
            name,
            file,
            simulate,
            base,
            tr,
            backend,
            ..
        } => {
            let text = match fs::read_to_string(&file) {
                Ok(text) => text,
                Err(error) => return stop(format!("cannot read {}: {error}", file.display())),
            };
            let source = local_source_lines(&text);

            if let Err(message) = validate_prog(&name, &source) {
                return stop(message);
            }

            let base_given = base.is_some();
            let input = DeployInput {
                name,
                source,
                base,
                simulate,
                tr,
            };

            route(
                OperationKind::DeployProg,
                backend.map(backend_kind),
                base_given,
                connection,
                factories,
                |transport, selection, cancel| {
                    let result = DeployProg::run_selected(
                        transport,
                        input.clone(),
                        selection,
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
    mut attempt: impl FnMut(&mut dyn SadtTransport, &Selection, &CancelFlag) -> Attempt,
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
            Ok(mut transport) => attempt(&mut *transport, &selection, &cancel),
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
