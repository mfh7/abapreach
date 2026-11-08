use crate::backend::{BackendKind, Failure, OperationKind, Router, Selection, SelectionReason};
use crate::cli::{Backend, Command, DeleteKind, DeleteTarget, ObjectKind, Subc, TypeArg};
use crate::config::{
    Config, ConfigPaths, Connection, LocalCredentialsPermission, local_credentials_permission,
    scrub, tls_warning,
};
use crate::domain::Outcome;
use crate::extension::{
    CreateRequest, ExtensionDelete, ExtensionKind, ExtensionOp, ExtensionOptions, MixedDelete,
    ReadRequest, Registry, RunContext, SharedFactories, adt_label, adt_pin,
};
use crate::hash::local_source_lines;
use crate::mcp::{Server, serve_with};
use crate::mcp_read::ReadTools;
use crate::mcp_write::{McpTools, TokenGate};
use crate::object::{
    check_create, check_create_extension, check_deploy_options, check_deploy_source, check_read,
    check_read_extension,
};
use crate::operation::create::{CreateInput, CreateProg};
use crate::operation::create_ddic::{self, DdicCreate};
use crate::operation::delete::{self, Context};
use crate::operation::delete_adt::{AdtDelete, NO_STATEFUL_SESSION};
use crate::operation::delete_ddic::{DdicDelete, DdicEnv, RunnerBatch};
use crate::operation::deploy::{CancelFlag, DeployInput, DeployProg, DeployScope};
use crate::operation::deploy_ddic::{self, DdicDeploy};
use crate::operation::doctor::Doctor;
use crate::operation::payload::Kind;
use crate::operation::read::{ReadProg, ReadTarget};
use crate::operation::read_ddic::{self, DdicRead};
use crate::operation::read_table::{
    CLI_MAX_ROWS, MAX_OUTPUT_BYTES, Query, ReadTable, ReadTableError, ReadTableRequest,
};
use crate::operation::runner_tran::{self, TranCreate, TranRead};
use crate::operation::simulate_ddic::{self, DdicSimulate, Simulation};
use crate::policy::Terminal;
use crate::runner_cli::{self, CreateOptions, SourceOptions};
use crate::transport::http::{HttpConfig, HttpTransport};
use crate::transport::rfc::{
    DynamicRfcApi, Env, FunctionApi, RfcApi, RfcLibrary, RfcTransport, connection_parameters,
};
use crate::transport::{SadtTransport, TransportError};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
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

/// What the doctor's RFC probe found: the ping result and, after a passed ping, the logon behind it.
pub struct RfcProbe {
    pub ping: Result<(), TransportError>,
    /// `Some` only after a passed ping; an `Err` keeps the open error of that logon.
    pub function: Option<Result<Box<dyn FunctionApi>, TransportError>>,
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

    /// The doctor's RFC probe for an extension: one ping and, only when it passed, the generic
    /// function-call layer on that same logon, so a doctor run never logs on twice. A failed ping
    /// opens nothing more. The default belongs to test doubles that count `rfc_ping` and
    /// `function_api` separately.
    fn rfc_probe(&mut self, connection: &Connection) -> RfcProbe {
        let ping = self.rfc_ping(connection);
        let function = ping.is_ok().then(|| self.function_api(connection));

        RfcProbe { ping, function }
    }

    /// An RFC logon for generic function-module calls. The default belongs to factories that carry
    /// no RFC (a test double); the real ones override.
    fn function_api(&mut self, _: &Connection) -> Result<Box<dyn FunctionApi>, TransportError> {
        Err(TransportError::PreSendConnectivity(
            "function calls are not available from these factories".to_owned(),
        ))
    }

    /// A NEW logon for the read-only reconciliation of an uncertain write.
    fn function_api_fresh(
        &mut self,
        connection: &Connection,
    ) -> Result<Box<dyn FunctionApi>, TransportError> {
        self.function_api(connection)
    }

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
        let parameters =
            connection_parameters(connection).map_err(TransportError::PreSendConnectivity)?;

        RfcLibrary::load(&Env::current())
            .and_then(|library| DynamicRfcApi::load(library, parameters))
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

    fn rfc_probe(&mut self, connection: &Connection) -> RfcProbe {
        let mut api = match Self::rfc_api(connection) {
            Ok(api) => api,
            Err(error) => {
                return RfcProbe {
                    ping: Err(error),
                    function: None,
                };
            }
        };
        let ping = api.ping();

        if ping.is_err() {
            api.close();

            return RfcProbe {
                ping,
                function: None,
            };
        }

        // The probe keeps the pinged logon; dropping it logs off.
        RfcProbe {
            ping,
            function: Some(Ok(Box::new(api))),
        }
    }

    fn function_api(
        &mut self,
        connection: &Connection,
    ) -> Result<Box<dyn FunctionApi>, TransportError> {
        Ok(Box::new(Self::rfc_api(connection)?))
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
    run_registered(args, env, factories, &Registry::new())
}

/// [`run`] with the registry of the process; `extension::run_with` is the public name.
pub(crate) fn run_registered(
    args: Vec<String>,
    env: BTreeMap<String, String>,
    factories: &mut dyn Factories,
    registry: &Registry,
) -> Exit {
    // Test-only: an unknown value stops the command before any connection is opened.
    #[cfg(feature = "fault-injection")]
    let warning = match crate::fault::Fault::from_lookup_with(
        env.get(crate::fault::ENV).map(String::as_str),
        &registry.fault_targets(),
    ) {
        Ok(faults) => crate::fault::Fault::warning(&faults),
        Err(message) => return stop(message),
    };
    #[cfg(not(feature = "fault-injection"))]
    let warning: Option<String> = None;
    let (cli, options) = match crate::cli::parse(args, registry) {
        Ok(parsed) => parsed,
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
        return with_warning(mcp(env, factories, registry), warning.as_deref());
    };
    let (connection, local_credentials) = match resolve(conn, &env) {
        Ok(resolved) => resolved,
        Err(message) => return with_warning(stop(message), warning.as_deref()),
    };
    let exit = with_warning(
        execute(
            cli.command,
            &connection,
            local_credentials,
            factories,
            registry,
            &options,
        ),
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

pub(crate) fn stop(message: impl std::fmt::Display) -> Exit {
    Exit {
        code: 2,
        stdout: String::new(),
        stderr: format!("STOP: {message}"),
    }
}

/// The STOP of the rule a DDIC or transaction command broke; a command that passed its rules ran
/// before this is reached.
fn runner_exit(checked: Result<(), String>) -> Exit {
    stop(checked.expect_err("a DDIC or transaction command that passed its rules ran above"))
}

/// The text of the XML FILE of a DDIC command.
fn read_xml(file: &Path) -> Result<String, Exit> {
    fs::read_to_string(file)
        .map_err(|error| stop(format!("cannot read {}: {error}", file.display())))
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

/// `mcp` loads every policy once, then serves stdio until EOF; an invalid policy stops the start
/// with exit 2 before anything is written to stdout.
fn mcp(env: BTreeMap<String, String>, factories: &mut dyn Factories, registry: &Registry) -> Exit {
    let loaded = ConfigPaths::from_env(&env).and_then(|paths| Config::load(paths, &env));

    match loaded {
        Ok(config) => {
            let writable = config.mcp_writable();
            // The flag a signal handler sets: it ends the server once a write has settled.
            let cancel = factories.cancel();
            let tools = McpTools::new(ReadTools::new(config, env, factories), TokenGate::new())
                .with_registry(registry);
            let mut server = Server::new(tools, false)
                .with_writable(writable)
                .with_kinds(registry.kinds().collect())
                .with_version(registry.version_text());
            // Not the locked handles: the reader thread owns stdin, and stdout is shared.
            let served = serve_with(
                &mut server,
                std::io::BufReader::new(std::io::stdin()),
                std::io::stdout(),
                std::io::stderr().lock(),
                cancel,
            );

            match served {
                Ok(()) => Exit {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                Err(error) => stop(format!("MCP stdio failed: {error}")),
            }
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
pub(crate) struct Attempt {
    pub(crate) outcome: Outcome,
    pub(crate) failure: Option<Failure>,
    pub(crate) fallback_allowed: bool,
    /// The failure message, quoted as the reason when the next backend is a fallback.
    pub(crate) detail: Option<String>,
}

impl Attempt {
    fn from_error(error: &TransportError) -> Self {
        Self::failed(error.to_string(), Failure::from(error))
    }

    /// Adds the `W:` note of a session end that failed; the verdict stays as it was.
    pub(crate) fn noted(mut self, note: Option<String>) -> Self {
        append_note(&mut self.outcome.stderr, note);

        self
    }

    pub(crate) fn failed(message: String, failure: Failure) -> Self {
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

/// Ends the session of an operation whose lock, if any, is reported elsewhere. A failed logoff is
/// best effort: it is only this `W:` note, never another status or exit code.
pub(crate) fn end_session(transport: &mut dyn SadtTransport) -> Option<String> {
    transport
        .close()
        .err()
        .map(|error| format!("W: session close failed: {error}"))
}

pub(crate) fn append_note(stderr: &mut String, note: Option<String>) {
    if let Some(note) = note {
        if !stderr.is_empty() {
            stderr.push('\n');
        }

        stderr.push_str(&note);
    }
}

fn execute(
    command: Command,
    connection: &Connection,
    local_credentials: LocalCredentialsPermission,
    factories: &mut dyn Factories,
    registry: &Registry,
    options: &ExtensionOptions,
) -> Exit {
    // A batch with an extension type is refused by the extension before the first object, and
    // before any connection; its rows never run over ADT.
    if let Command::Delete {
        targets,
        simulate,
        tr,
        plan,
        ..
    } = &command
        && let Some(refusal) = extension_delete_refusal(targets, registry, connection)
    {
        return match refusal {
            Ok(()) => delete_with_extension(
                targets,
                *simulate,
                tr.as_deref(),
                plan,
                connection,
                factories,
                registry,
            ),
            Err(message) => stop(message),
        };
    }

    // A DDIC or transaction row is checked here before any connection.
    if let Command::Delete { targets, tr, .. } = &command
        && runner_cli::first_kind(targets).is_some()
        && let Err(message) = runner_cli::check_delete(targets, tr.as_deref())
    {
        return stop(message);
    }

    match command {
        Command::Doctor { .. } => {
            let report = Doctor::probe_with(connection, local_credentials, factories, registry);

            Exit {
                code: report.exit(),
                stdout: report.render(),
                stderr: String::new(),
            }
        }
        Command::ReadTable {
            table,
            fields,
            where_clause,
            rows,
            ..
        } => {
            let request = ReadTableRequest {
                table,
                fields,
                where_clause,
                rows,
            };
            let query = match Query::new(&request, CLI_MAX_ROWS, &connection.policies) {
                Ok(query) => query,
                Err(message) => return stop(message),
            };

            route(
                OperationKind::ReadTable,
                None,
                false,
                connection,
                factories,
                |transport, _, _, _| {
                    let read = ReadTable::run(transport, &query, &connection.policies);

                    // A read holds no lock, so a failed session end is only a note.
                    let note = end_session(transport);
                    let attempt = match read {
                        Ok(data) => {
                            let (stdout, omitted) = data.render(MAX_OUTPUT_BYTES);
                            let stderr = if omitted == 0 {
                                String::new()
                            } else {
                                format!(
                                    "W: output cut at {MAX_OUTPUT_BYTES} bytes; {omitted} rows left out"
                                )
                            };

                            Attempt {
                                outcome: Outcome {
                                    status: None,
                                    exit: 0,
                                    stdout,
                                    stderr,
                                },
                                failure: None,
                                fallback_allowed: false,
                                detail: None,
                            }
                        }
                        Err(ReadTableError::Rejected(message)) => Attempt {
                            outcome: Outcome {
                                status: None,
                                exit: 1,
                                stdout: String::new(),
                                stderr: format!("REJECTED: {message}"),
                            },
                            failure: None,
                            fallback_allowed: false,
                            detail: None,
                        },
                        Err(ReadTableError::Failed { failure, message }) => {
                            Attempt::failed(message, failure)
                        }
                    };

                    attempt.noted(note)
                },
            )
        }
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
            |transport, selection, cancel, fresh| {
                if runner_cli::first_kind(&targets).is_some() {
                    return delete_runner_attempt(
                        transport,
                        selection,
                        cancel,
                        fresh,
                        connection,
                        &targets,
                        tr.as_deref(),
                        simulate,
                        &plan,
                    );
                }

                delete_attempt(
                    transport,
                    selection,
                    cancel,
                    fresh,
                    connection,
                    &targets,
                    tr.as_deref(),
                    simulate,
                    &plan,
                    None,
                )
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
            main,
            backend,
            ..
        } => {
            let kind = match kind {
                TypeArg::Standard(kind) => kind,
                TypeArg::Runner(kind) => {
                    let options = SourceOptions {
                        include,
                        fugr: fugr.as_deref(),
                        main: main.as_deref(),
                        create_include: false,
                    };

                    let checked = runner_cli::check_read(kind, &name, &out, force, lines, &options);

                    if kind == Kind::Tran && checked.is_ok() {
                        let request = TranRead {
                            name: &name,
                            out: &out,
                        };

                        return runner_tran::read(
                            connection,
                            backend.map(backend_kind),
                            &request,
                            factories,
                        );
                    }

                    if kind != Kind::Tran && checked.is_ok() {
                        let request = DdicRead {
                            kind,
                            name: &name,
                            out: &out,
                            force,
                        };

                        return read_ddic::read(
                            connection,
                            backend.map(backend_kind),
                            &request,
                            factories,
                        );
                    }

                    return runner_exit(checked);
                }
                TypeArg::Extension(kind) => {
                    let request = ReadRequest {
                        name: &name,
                        out: &out,
                        force,
                        lines,
                        options,
                    };

                    return read_with_extension(
                        kind,
                        &request,
                        (include, fugr.as_deref(), main.as_deref()),
                        backend,
                        connection,
                        factories,
                        registry,
                    );
                }
            };

            if let Err(message) = check_read(
                kind,
                &name,
                &out,
                include,
                fugr.as_deref(),
                lines,
                main.as_deref(),
            ) {
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
                .lines(lines)
                .main(main.as_deref());

            route(
                OperationKind::Read(kind),
                backend.map(backend_kind),
                false,
                connection,
                factories,
                |transport, _, _, _| {
                    let read =
                        ReadProg::run_target_classified(transport, target, &out, force, None);

                    // A read holds no lock, so a failed session end is only a note.
                    let note = end_session(transport);
                    let attempt = match read {
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
                    };

                    attempt.noted(note)
                },
            )
        }
        Command::Create {
            kind,
            name,
            file,
            text,
            package,
            fugr,
            prog,
            subc,
            tr,
            simulate,
            backend,
            ..
        } => {
            if prog.is_some() && !matches!(kind, TypeArg::Runner(Kind::Tran)) {
                return stop(runner_cli::PROG_RULE);
            }

            let kind = match kind {
                TypeArg::Standard(kind) => kind,
                TypeArg::Runner(kind) => {
                    let options = CreateOptions {
                        text: text.as_deref(),
                        package: package.as_deref(),
                        tr: tr.as_deref(),
                        fugr: fugr.as_deref(),
                        subc,
                        prog: prog.as_deref(),
                        file: file.as_deref(),
                    };

                    let checked = runner_cli::check_create(kind, &name, &options);

                    // `check_create` makes `--prog` mandatory for a TRAN.
                    if let (Kind::Tran, Ok(()), Some(report)) = (kind, &checked, prog.as_deref()) {
                        let request = TranCreate {
                            name: &name,
                            report,
                            text: text.as_deref(),
                            package: package.as_deref(),
                            tr: tr.as_deref(),
                            simulate,
                        };

                        return runner_tran::create(
                            connection,
                            backend.map(backend_kind),
                            &request,
                            factories,
                        );
                    }

                    // `check_create` makes FILE mandatory for a DDIC type.
                    if let (Ok(()), Some(file)) = (&checked, file.as_deref()) {
                        let xml = match read_xml(file) {
                            Ok(xml) => xml,
                            Err(exit) => return exit,
                        };

                        if !simulate {
                            let request = DdicCreate {
                                kind,
                                name: &name,
                                xml: &xml,
                                package: package.as_deref(),
                                text: text.as_deref(),
                                tr: tr.as_deref(),
                            };

                            return create_ddic::create(
                                connection,
                                backend.map(backend_kind),
                                &request,
                                factories,
                            );
                        }

                        let request = DdicSimulate {
                            kind,
                            name: &name,
                            xml: &xml,
                            tr: tr.as_deref(),
                            simulation: Simulation::Create {
                                package: package.as_deref(),
                                text: text.as_deref(),
                            },
                        };

                        return simulate_ddic::simulate(
                            connection,
                            backend.map(backend_kind),
                            &request,
                            factories,
                        );
                    }

                    return runner_exit(checked);
                }
                TypeArg::Extension(kind) => {
                    let text = text.unwrap_or_default();
                    let request = CreateRequest {
                        name: &name,
                        text: &text,
                        package: package.as_deref(),
                        tr: tr.as_deref(),
                        simulate,
                        options,
                    };

                    return create_with_extension(
                        kind,
                        &request,
                        (fugr.as_deref(), subc, file.as_deref()),
                        backend,
                        connection,
                        factories,
                        registry,
                    );
                }
            };

            let text = text.unwrap_or_default();

            if let Err(message) = check_create(
                kind,
                &name,
                &text,
                package.as_deref(),
                fugr.as_deref(),
                tr.as_deref(),
                subc,
                option_misuse(None, options, registry),
                file.as_deref(),
            ) {
                return stop(message);
            }

            // The FILE of a `PROG` create is read and checked before any connection, like a deploy
            // source; it is never written.
            let source = match file {
                Some(file) => {
                    let text = match fs::read_to_string(&file) {
                        Ok(text) => text,
                        Err(error) => {
                            return stop(format!("cannot read {}: {error}", file.display()));
                        }
                    };
                    let source = local_source_lines(&text);

                    if let Err(message) = check_deploy_source(ObjectKind::Prog, &name, &source) {
                        return stop(message);
                    }

                    Some(source)
                }
                None => None,
            };
            let input = CreateInput {
                kind,
                name,
                text,
                package,
                group: fugr,
                tr,
                simulate,
                subc: subc.unwrap_or(Subc::Program),
                source,
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
            main,
            ..
        } => {
            let kind = match kind {
                TypeArg::Standard(kind) => kind,
                TypeArg::Runner(kind) => {
                    let options = SourceOptions {
                        include,
                        fugr: fugr.as_deref(),
                        main: main.as_deref(),
                        create_include,
                    };

                    let checked = runner_cli::check_deploy(kind, &name, tr.as_deref(), &options);

                    if simulate && checked.is_ok() {
                        let xml = match read_xml(&file) {
                            Ok(xml) => xml,
                            Err(exit) => return exit,
                        };
                        let request = DdicSimulate {
                            kind,
                            name: &name,
                            xml: &xml,
                            tr: tr.as_deref(),
                            simulation: Simulation::Deploy {
                                base: base.as_deref(),
                            },
                        };

                        return simulate_ddic::simulate(
                            connection,
                            backend.map(backend_kind),
                            &request,
                            factories,
                        );
                    }

                    if checked.is_ok() {
                        let xml = match read_xml(&file) {
                            Ok(xml) => xml,
                            Err(exit) => return exit,
                        };
                        let request = DdicDeploy {
                            kind,
                            name: &name,
                            xml: &xml,
                            tr: tr.as_deref(),
                            base: base.as_deref(),
                        };

                        return deploy_ddic::deploy(
                            connection,
                            backend.map(backend_kind),
                            &request,
                            factories,
                        );
                    }

                    return runner_exit(checked);
                }
                // `deploy` parses an extension type only so that it can say why it stops.
                TypeArg::Extension(kind) => {
                    return stop(
                        kind.spec()
                            .deploy_refusal
                            .unwrap_or("deploy is not supported"),
                    );
                }
            };

            if let Err(message) = check_deploy_options(
                kind,
                &name,
                include,
                create_include,
                fugr.as_deref(),
                main.as_deref(),
            ) {
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
                .create_include(create_include)
                .main(main.as_deref());
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

/// The `--backend` pin that names a backend, for an extension that reads the pin of a token.
pub(crate) fn pin_of(backend: BackendKind) -> Backend {
    match backend {
        BackendKind::AdtHttp => Backend::Adt,
        BackendKind::RfcSadt => Backend::Rfc,
    }
}

fn backend_kind(backend: Backend) -> BackendKind {
    match backend {
        Backend::Adt => BackendKind::AdtHttp,
        Backend::Rfc => BackendKind::RfcSadt,
    }
}

/// One attempt of a delete batch on the transport the router opened. `extension` is the backend
/// of the extension rows of a mixed batch: its rows run on it, the rest over `transport`.
#[allow(clippy::too_many_arguments)]
fn delete_attempt(
    transport: &mut dyn SadtTransport,
    selection: &Selection,
    cancel: &CancelFlag,
    fresh: &mut dyn FnMut() -> Opened,
    connection: &Connection,
    targets: &[DeleteTarget],
    tr: Option<&str>,
    simulate: bool,
    plan: &Path,
    mut extension: Option<&mut dyn ExtensionDelete>,
) -> Attempt {
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

    let context = Context {
        conn: &connection.id,
        user: &connection.user,
        policies: &connection.policies,
        confirmer: &Terminal,
    };
    let (mut outcome, unsent, fresh_note) = {
        let mut backend =
            AdtDelete::with_cancel(&mut *transport, &connection.user, fresh, cancel.clone());
        let outcome = match extension.as_deref_mut() {
            Some(extension) => {
                let mut mixed =
                    MixedDelete::new(&mut backend, adt_label(selection.backend), extension);

                delete::run_until(
                    &mut mixed,
                    &context,
                    targets,
                    tr,
                    simulate,
                    plan,
                    &mut || cancel.is_set(),
                )
            }
            None => delete::run_until(
                &mut backend,
                &context,
                targets,
                tr,
                simulate,
                plan,
                &mut || cancel.is_set(),
            ),
        };

        (outcome, backend.unsent_failure(), backend.close_note())
    };
    if !simulate && cancel.is_set() {
        outcome.exit = 130;
    }

    // The batch is over and the lock, if any, is reported by the outcome; a failed
    // session end is only a note.
    let note = end_session(transport);

    // A read that never left the process and no delete request sent: the plan file of a batch
    // without an extension row pins no backend, so RFC may run the whole batch, unless Ctrl-C came
    // first. A batch with one is pinned by its plan, so the router refuses the fallback; an
    // extension delete that was sent still keeps its report, which the refused fallback would drop.
    let sent = extension
        .as_ref()
        .is_some_and(|extension| extension.wrote());

    if let Some((failure, message)) = unsent.filter(|_| !sent) {
        if !cancel.is_set() {
            return Attempt {
                fallback_allowed: true,
                ..Attempt::failed(message, failure)
            }
            .noted(fresh_note)
            .noted(note);
        }

        outcome.exit = 130;
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

/// One attempt of a delete batch with DDIC rows: those rows run in their own temporary class, the
/// other rows over the ADT backend the router picked.
#[allow(clippy::too_many_arguments)]
fn delete_runner_attempt(
    transport: &mut dyn SadtTransport,
    selection: &Selection,
    cancel: &CancelFlag,
    fresh: &mut dyn FnMut() -> Opened,
    connection: &Connection,
    targets: &[DeleteTarget],
    tr: Option<&str>,
    simulate: bool,
    plan: &Path,
) -> Attempt {
    // The ADT side needs a stateful session for its own rows and the runner cycle needs one on
    // ADT HTTP; a release without one runs the batch over RFC.
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

    let context = Context {
        conn: &connection.id,
        user: &connection.user,
        policies: &connection.policies,
        confirmer: &Terminal,
    };
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
    let (mut outcome, unsent, fresh_note) = {
        let mut adt = AdtDelete::with_cancel(
            &mut *transport,
            &connection.user,
            &mut open_adt,
            cancel.clone(),
        );
        let mut runner = DdicDelete::new(
            DdicEnv::of(connection, selection.backend),
            &mut open_runner,
            cancel.clone(),
        );
        let mut outcome = {
            let mut mixed = RunnerBatch::new(&mut adt, &mut runner);

            delete::run_until(
                &mut mixed,
                &context,
                targets,
                tr,
                simulate,
                plan,
                &mut || cancel.is_set(),
            )
        };

        runner.finish(&mut outcome);

        (
            outcome,
            runner.unsent_failure().or_else(|| adt.unsent_failure()),
            adt.close_note(),
        )
    };
    let note = end_session(transport);

    if let Some((failure, message)) = unsent {
        if !cancel.is_set() {
            return Attempt {
                fallback_allowed: true,
                ..Attempt::failed(message, failure)
            }
            .noted(fresh_note)
            .noted(note);
        }

        outcome.exit = 130;
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

pub(crate) fn exit_of_outcome(outcome: Outcome) -> Exit {
    Exit {
        code: outcome.exit,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
    }
}

/// The extension types of a delete batch, once each.
fn extension_kinds(targets: &[DeleteTarget]) -> Vec<ExtensionKind> {
    let mut kinds: Vec<ExtensionKind> = Vec::new();

    for target in targets {
        if let DeleteKind::Extension(kind) = target.kind
            && !kinds.contains(&kind)
        {
            kinds.push(kind);
        }
    }

    kinds
}

/// `None`: no extension row. `Some(Ok(()))`: every extension type of the batch passed its
/// preflight, so the batch goes to the extension path. `Some(Err(message))`: a STOP.
fn extension_delete_refusal(
    targets: &[DeleteTarget],
    registry: &Registry,
    connection: &Connection,
) -> Option<Result<(), String>> {
    let kinds = extension_kinds(targets);

    if kinds.is_empty() {
        return None;
    }

    let Some(extension) = registry.extension() else {
        return Some(Err("no extension is registered for this type".to_owned()));
    };

    Some(
        kinds
            .into_iter()
            .try_for_each(|kind| extension.preflight(ExtensionOp::Delete, kind, connection, None)),
    )
}

/// The text of the extension option a command gave that the type does not declare: for a standard
/// type (`None`) every extension option, for an extension type the options of other types.
pub(crate) fn option_misuse(
    kind: Option<ExtensionKind>,
    options: &ExtensionOptions,
    registry: &Registry,
) -> Option<&'static str> {
    let declared = kind.map_or(&[][..], |kind| kind.spec().create_options);

    options.keys().find_map(|id| {
        if declared.iter().any(|option| option.id == *id) {
            return None;
        }

        registry
            .kinds()
            .flat_map(|kind| kind.spec().create_options)
            .find(|option| option.id == *id)
            .map(|option| option.misuse)
    })
}

/// `read` of an extension type: the common local rules, the extension's own, the refusals, the
/// overwrite rule, then the one read outside the router.
fn read_with_extension(
    kind: ExtensionKind,
    request: &ReadRequest<'_>,
    (include, fugr, main): (Option<crate::cli::IncludeKind>, Option<&str>, Option<&str>),
    backend: Option<Backend>,
    connection: &Connection,
    factories: &mut dyn Factories,
    registry: &Registry,
) -> Exit {
    let Some(extension) = registry.extension() else {
        return stop("no extension is registered for this type");
    };

    if let Err(message) = check_read_extension(
        kind.spec(),
        request.name,
        request.out,
        include,
        fugr,
        request.lines,
        main,
    )
    .and_then(|()| extension.check_read(kind, request))
    .and_then(|()| extension.preflight(ExtensionOp::Read, kind, connection, backend))
    {
        return stop(message);
    }

    // A local overwrite rule: it stops before the connection, like every other option rule.
    if request.lines.is_none() && request.out.exists() && !request.force {
        return stop(format!(
            "{} already exists; use --force to overwrite",
            request.out.display()
        ));
    }

    let cancel = factories.cancel();
    let shared = SharedFactories::new(factories);
    let run = RunContext {
        connection,
        cancel: &cancel,
        pin: backend,
        factories: &shared,
    };

    exit_of_outcome(extension.read(kind, request, run))
}

/// `create` of an extension type, like [`read_with_extension`].
fn create_with_extension(
    kind: ExtensionKind,
    request: &CreateRequest<'_>,
    (fugr, subc, file): (Option<&str>, Option<Subc>, Option<&Path>),
    backend: Option<Backend>,
    connection: &Connection,
    factories: &mut dyn Factories,
    registry: &Registry,
) -> Exit {
    let Some(extension) = registry.extension() else {
        return stop("no extension is registered for this type");
    };

    if let Err(message) = check_create_extension(
        kind.spec(),
        request.name,
        request.text,
        request.package,
        fugr,
        request.tr,
        subc,
        option_misuse(Some(kind), request.options, registry),
        &|| extension.check_create(kind, request),
        file,
    )
    .and_then(|()| extension.preflight(ExtensionOp::Create, kind, connection, backend))
    {
        return stop(message);
    }

    let cancel = factories.cancel();
    let shared = SharedFactories::new(factories);
    let run = RunContext {
        connection,
        cancel: &cancel,
        pin: backend,
        factories: &shared,
    };

    exit_of_outcome(extension.create(kind, request, run))
}

/// A delete batch with an extension type: its rows run on the extension's backend, the other
/// objects over the ADT backend the router picks, and the plan records the backend of every
/// object. A real delete is pinned to the backend its plan names, so nothing falls back between
/// simulate and delete.
fn delete_with_extension(
    targets: &[DeleteTarget],
    simulate: bool,
    tr: Option<&str>,
    plan: &Path,
    connection: &Connection,
    factories: &mut dyn Factories,
    registry: &Registry,
) -> Exit {
    let Some(extension) = registry.extension() else {
        return stop("no extension is registered for this type");
    };

    // The local rules of the batch run before any connection is opened.
    if let Err(message) = delete::prepare(targets, tr) {
        return stop(message);
    }

    let cancel = factories.cancel();
    let shared = SharedFactories::new(factories);
    let run = RunContext {
        connection,
        cancel: &cancel,
        pin: None,
        factories: &shared,
    };
    let mut backend = match extension.delete_backend(run) {
        Ok(backend) => backend,
        Err(outcome) => return exit_of_outcome(outcome),
    };

    if targets
        .iter()
        .all(|target| matches!(target.kind, DeleteKind::Extension(_)))
    {
        let context = Context {
            conn: &connection.id,
            user: &connection.user,
            policies: &connection.policies,
            confirmer: &Terminal,
        };
        let mut outcome = delete::run_until(
            &mut *backend,
            &context,
            targets,
            tr,
            simulate,
            plan,
            &mut || cancel.is_set(),
        );

        if !simulate && cancel.is_set() {
            outcome.exit = 130;
        }

        return exit_of_outcome(outcome);
    }

    // shortcut: a plan that names no backend leaves the router free; the batch then stops at the
    // plan check, before its first object, when the backend it picks is not the recorded one.
    let pin = if simulate {
        None
    } else {
        delete::load_plan(plan).ok().as_ref().and_then(adt_pin)
    };

    route(
        OperationKind::Delete,
        pin,
        false,
        connection,
        &mut shared.lend(),
        |transport, selection, cancel, fresh| {
            delete_attempt(
                transport,
                selection,
                cancel,
                fresh,
                connection,
                targets,
                tr,
                simulate,
                plan,
                Some(&mut *backend),
            )
        },
    )
}

/// Runs the operation on the backend the router picks. A second backend is tried only when the
/// first one failed before it sent a write request and neither `--backend` nor `--base` pins it.
pub(crate) fn route(
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
    let router = Router::new(connection.adt.is_some(), connection.rfc.is_some());
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
