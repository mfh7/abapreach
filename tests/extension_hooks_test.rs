//! The extension point (hooks a to h) against a test extension that adds the type `XTEST`: grammar,
//! rules, mixed delete, MCP, doctor line, fault target, function-call connections, operations
//! outside the router and the version string. The extension is a fake: it names no custom
//! function module and talks to no SAP.

mod common;

use abapreach::cli::{DeleteKind, DeleteTarget};
use abapreach::config::{Config, ConfigPaths};
use abapreach::extension::{
    Backend, BackendKind, CancelFlag, Connection, CreateRequest, DeleteBackend, DeleteReply,
    DoctorProbe, Exit, Extension, ExtensionDelete, ExtensionKind, ExtensionOp, Factories,
    FunctionApi, FunctionInfo, InactiveFact, KindSpec, MixedDelete, Normalize, ObjectFacts,
    OptionSpec, Outcome, Plan, ReadRequest, Reconcile, Registry, RfcProbe, RunContext, SourceFact,
    Status, TrLock, TransportError, Verbs, adt_label, adt_pin, run_with,
};
use abapreach::mcp::{Answer, Form, NoTools, Server, Session, ToolHandler, ToolResult, serve};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{McpTools, Operation, Ticket, TokenGate, confirmation_summary_with};
use abapreach::operation::delete::{Context, DeleteStatus, execute, simulate};
use abapreach::policy::{Confirmer, Policies};
use abapreach::transport::{Response, SadtTransport};
use abapreach::{RfcLibraryIdentity, RfcLibraryInfo};
use common::ScriptedTransport;
use serde_json::{Map, Value, json};
use std::cell::{Cell as StdCell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

const LABEL: &str = "test (XTEST)";
const NAME: &str = "YTEST_XT1";

// ---- the test extension --------------------------------------------------------------------

static NOTES: [OptionSpec; 1] = [OptionSpec {
    id: "note",
    long: "note",
    value_name: "NOTE",
    help: "An XTEST create only: a note",
    mcp_description: "XTEST only: a note",
    normalize: Normalize::Upper,
    misuse: "--note only applies to XTEST",
}];

static XTEST: KindSpec = KindSpec {
    token: "XTEST",
    help: "A test type that runs over the extension only",
    verbs: Verbs::READ.or(Verbs::CREATE).or(Verbs::DELETE),
    deploy_refusal: Some("deploy XTEST is not supported"),
    max_name: 20,
    max_text: 36,
    create_options: &NOTES,
    delete_rank: 0,
    backend_label: LABEL,
    plan_backend_optional: true,
    is_self: Some(is_self),
    mcp_read_refusal: Some("abap_read_source does not read XTEST; use the CLI read XTEST"),
};

static KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&XTEST)];

fn is_self(name: &str) -> bool {
    name.starts_with("YSELF")
}

fn xtest() -> ExtensionKind {
    KINDS[0]
}

/// What the extension saw, shared with the test.
#[derive(Default)]
struct State {
    checks: Vec<String>,
    preflights: Vec<String>,
    runs: Vec<String>,
    pins: Vec<Option<Backend>>,
    cancel_set: Vec<bool>,
    begun: Vec<(Vec<String>, Option<String>)>,
    deleted: Vec<String>,
    inspected: Vec<String>,
    doctor: Vec<String>,
    describes: usize,
    /// `begin` fails: `Some(true)` as an unreachable backend, `Some(false)` as a contract error.
    begin_error: Option<bool>,
    halt_after_delete: bool,
    halted: bool,
}

type Shared = Rc<RefCell<State>>;

struct TestExtension {
    state: Shared,
}

impl Extension for TestExtension {
    fn name(&self) -> &'static str {
        "xtest"
    }

    fn kinds(&self) -> &'static [ExtensionKind] {
        &KINDS
    }

    fn check_read(&self, _: ExtensionKind, request: &ReadRequest<'_>) -> Result<(), String> {
        self.state
            .borrow_mut()
            .checks
            .push(format!("read {}", request.name));

        if request.name == "ZNOREAD" {
            return Err("XTEST rejects ZNOREAD".to_owned());
        }

        Ok(())
    }

    fn check_create(&self, _: ExtensionKind, request: &CreateRequest<'_>) -> Result<(), String> {
        self.state
            .borrow_mut()
            .checks
            .push(format!("create {}", request.name));

        if request.name == "ZREJECT" {
            return Err("XTEST rejects ZREJECT".to_owned());
        }

        Ok(())
    }

    fn preflight(
        &self,
        op: ExtensionOp,
        _: ExtensionKind,
        connection: &Connection,
        pin: Option<Backend>,
    ) -> Result<(), String> {
        self.state
            .borrow_mut()
            .preflights
            .push(format!("{} {pin:?}", op.verb()));

        if pin == Some(Backend::Adt) || connection.rfc.is_none() {
            return Err(format!(
                "{} XTEST needs the RFC path; connection {} has no RFC or is pinned to ADT",
                op.verb(),
                connection.id
            ));
        }

        Ok(())
    }

    fn read(&self, _: ExtensionKind, request: &ReadRequest<'_>, run: RunContext<'_>) -> Outcome {
        let mut state = self.state.borrow_mut();

        state.runs.push(format!("read {}", request.name));
        state.pins.push(run.pin);
        state.cancel_set.push(run.cancel.is_set());

        outcome(
            0,
            Some(Status::Active),
            format!("XTEST {}", request.name),
            "",
        )
    }

    fn create(
        &self,
        _: ExtensionKind,
        request: &CreateRequest<'_>,
        run: RunContext<'_>,
    ) -> Outcome {
        let note = request.options.get("note").cloned().unwrap_or_default();

        {
            let mut state = self.state.borrow_mut();

            state
                .runs
                .push(format!("create {} note={note}", request.name));
            state.pins.push(run.pin);
            state.cancel_set.push(run.cancel.is_set());
        }

        // The factories of the command, through the shared lend: a logon for the call, and a
        // fresh one for the read that settles a lost reply.
        let mut api = match run.factories.lend().function_api(run.connection) {
            Ok(api) => api,
            Err(error) => return outcome(1, None, String::new(), &format!("ERROR: {error}")),
        };

        let _ = api.describe_function("XTEST_FM");

        if request.text == "RECONCILE" {
            let _ = run.factories.lend().function_api_fresh(run.connection);
        }

        let _write = (!request.simulate).then(|| run.cancel.hold_write());
        let status = if request.simulate {
            Status::SimulatedOk
        } else {
            Status::Active
        };

        outcome(
            0,
            Some(status),
            format!("CREATE XTEST {}", request.name),
            "",
        )
    }

    fn delete_backend<'r>(
        &'r self,
        run: RunContext<'r>,
    ) -> Result<Box<dyn ExtensionDelete + 'r>, Outcome> {
        self.state.borrow_mut().pins.push(run.pin);

        Ok(Box::new(XDelete {
            state: self.state.clone(),
            cancel: run.cancel.clone(),
            wrote: false,
        }))
    }

    fn doctor_line(&self, probe: &mut DoctorProbe<'_>) -> Option<String> {
        let line = match (
            &probe.library_error,
            probe.rfc_passed,
            &mut probe.function_api,
        ) {
            (Some(error), _, _) => format!("XTEST: unavailable: {error}"),
            (None, false, _) => "XTEST: not run (RFC SADT probe failed)".to_owned(),
            (None, true, Some(Ok(api))) => {
                // The one logon of the ping: describing through it opens nothing new.
                let _ = api.describe_function("XTEST_FM");
                self.state.borrow_mut().describes += 1;

                "XTEST: installed".to_owned()
            }
            (None, true, Some(Err(error))) => format!("XTEST: unavailable: {error}"),
            (None, true, None) => "XTEST: not run".to_owned(),
        };

        self.state.borrow_mut().doctor.push(line.clone());

        Some(line)
    }

    fn fault_targets(&self) -> &'static [&'static str] {
        &["XTEST"]
    }
}

fn outcome(exit: i32, status: Option<Status>, stdout: String, stderr: &str) -> Outcome {
    Outcome {
        status,
        exit,
        stdout,
        stderr: stderr.to_owned(),
    }
}

struct XDelete {
    state: Shared,
    cancel: CancelFlag,
    wrote: bool,
}

impl DeleteBackend for XDelete {
    fn inspect(&mut self, _: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        self.state.borrow_mut().inspected.push(name.to_owned());

        if self.state.borrow().halted {
            return Err("cannot open a new session".to_owned());
        }

        Ok(Some(ObjectFacts {
            package: "$TMP".to_owned(),
            author: "DEVUSER".to_owned(),
            group: None,
            sources: vec![SourceFact::active("LIVE", &[name])],
            contents: Vec::new(),
            where_used: Vec::new(),
            inactive: InactiveFact::Absent,
            tr: TrLock::Local,
        }))
    }

    fn delete(&mut self, _: DeleteKind, name: &str, _: Option<&str>) -> DeleteReply {
        self.wrote = true;

        let _write = self.cancel.hold_write();
        let mut state = self.state.borrow_mut();

        state.deleted.push(name.to_owned());
        state.halted = state.halt_after_delete;

        DeleteReply::Deleted
    }

    fn reconcile(&mut self, _: DeleteKind, _: &str) -> Reconcile {
        Reconcile::Gone
    }

    fn halted(&self) -> bool {
        self.state.borrow().halted
    }

    fn begin(&mut self, rows: &[(DeleteKind, String)], tr: Option<&str>) -> Result<(), String> {
        let mut state = self.state.borrow_mut();

        state.begun.push((
            rows.iter()
                .map(|(kind, name)| format!("{kind} {name}"))
                .collect(),
            tr.map(str::to_owned),
        ));

        match state.begin_error {
            Some(_) => Err("the XTEST backend cannot start".to_owned()),
            None => Ok(()),
        }
    }

    fn begin_failure(&self, message: &str) -> (i32, String) {
        if self.state.borrow().begin_error == Some(true) {
            (1, format!("ERROR: {message}"))
        } else {
            (2, format!("STOP: {message}"))
        }
    }

    fn backend_label(&self, _: DeleteKind) -> Option<String> {
        Some(LABEL.to_owned())
    }
}

impl ExtensionDelete for XDelete {
    fn wrote(&self) -> bool {
        self.wrote
    }
}

fn registry() -> (Registry, Shared) {
    let state = Shared::default();

    (
        Registry::new().with(TestExtension {
            state: state.clone(),
        }),
        state,
    )
}

// ---- the factories and the environment -------------------------------------------------------

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    adt_opened: usize,
    rfc_opened: usize,
    function_opened: usize,
    fresh_opened: usize,
    pings: usize,
    probes: usize,
    library: Option<Result<RfcLibraryInfo, String>>,
    ping: Option<Result<(), TransportError>>,
    cancelled: bool,
    describes: Rc<StdCell<usize>>,
}

struct Api {
    describes: Rc<StdCell<usize>>,
}

impl FunctionApi for Api {
    fn describe_function(&mut self, _: &str) -> Result<Option<FunctionInfo>, TransportError> {
        self.describes.set(self.describes.get() + 1);

        Ok(None)
    }

    fn call_function(
        &mut self,
        _: &abapreach::extension::FunctionCall,
    ) -> Result<abapreach::extension::FunctionResult, TransportError> {
        panic!("the test extension never calls a function")
    }
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened += 1;
        self.adt.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no ADT connection scripted".to_owned(),
            ))
        })
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened += 1;
        self.rfc.pop_front().unwrap_or_else(|| {
            Err(TransportError::PreSendConnectivity(
                "no RFC connection scripted".to_owned(),
            ))
        })
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        self.library.clone().unwrap_or_else(library)
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        self.pings += 1;
        self.ping.clone().unwrap_or(Ok(()))
    }

    fn rfc_probe(&mut self, _: &Connection) -> RfcProbe {
        self.probes += 1;

        let ping = self.ping.clone().unwrap_or(Ok(()));
        let function = ping.is_ok().then(|| {
            Ok(Box::new(Api {
                describes: self.describes.clone(),
            }) as Box<dyn FunctionApi>)
        });

        RfcProbe { ping, function }
    }

    fn function_api(&mut self, _: &Connection) -> Result<Box<dyn FunctionApi>, TransportError> {
        self.function_opened += 1;

        Ok(Box::new(Api {
            describes: self.describes.clone(),
        }))
    }

    fn function_api_fresh(
        &mut self,
        _: &Connection,
    ) -> Result<Box<dyn FunctionApi>, TransportError> {
        self.fresh_opened += 1;

        Ok(Box::new(Api {
            describes: self.describes.clone(),
        }))
    }

    fn cancel(&self) -> CancelFlag {
        let flag = CancelFlag::new();

        if self.cancelled {
            flag.set();
        }

        flag
    }
}

impl Fake {
    fn opens(&self) -> (usize, usize, usize, usize) {
        (
            self.adt_opened,
            self.rfc_opened,
            self.function_opened,
            self.fresh_opened,
        )
    }
}

fn library() -> Result<RfcLibraryInfo, String> {
    Ok(RfcLibraryInfo {
        identity: RfcLibraryIdentity {
            path: "/opt/sap/nwrfcsdk/lib/libsapnwrfc.so".to_owned(),
            version: "7.6.2".to_owned(),
            architecture: "x86-64".to_owned(),
            family: "NW RFC SDK".to_owned(),
            source: "discovered".to_owned(),
            chars: "UTF-16".to_owned(),
        },
        notes: Vec::new(),
    })
}

fn write_config(directory: &Path, servers: &str) -> BTreeMap<String, String> {
    fs::write(directory.join("connections.json"), servers).unwrap();

    let mut env = BTreeMap::from([(
        "ABAP_CONNECTIONS".to_owned(),
        directory.join("connections.json").display().to_string(),
    )]);

    for id in ["DEV_100", "AOL_100", "QAS_100", "CNF_100"] {
        env.insert(format!("{id}_USER"), "user".to_owned());
        env.insert(format!("{id}_PASS"), "secret-pw".to_owned());
    }

    env
}

/// `DEV` has ADT and RFC and allows MCP writes by token; `CNF` asks a human; `AOL` is ADT only;
/// `QAS` is RFC only.
fn env(directory: &Path) -> BTreeMap<String, String> {
    let rfc = r#""rfc":{"ashost":"example.com","sysnr":"00"}"#;
    let adt = r#""adt":{"insecure":true}"#;
    let entry = |id: &str, parts: &str, mode: &str| {
        format!(
            r#"{{"systemId":"{id}","host":"example.com","port":443,"clients":["100"],{parts},"policies":{{"mcp_write":"{mode}"}}}}"#
        )
    };

    let qas = format!(r#"{{"systemId":"QAS","clients":["100"],{rfc}}}"#);

    write_config(
        directory,
        &format!(
            r#"{{"servers":[{},{},{},{}]}}"#,
            entry("DEV", &format!("{rfc},{adt}"), "token"),
            entry("CNF", &format!("{rfc},{adt}"), "confirm"),
            entry("AOL", adt, "token"),
            qas,
        ),
    )
}

fn cli(args: &[&str], directory: &Path, fake: &mut Fake, registry: &Registry) -> Exit {
    let mut full = vec!["abapreach".to_owned()];

    full.extend(args.iter().map(|arg| (*arg).to_owned()));

    run_with(full, env(directory), fake, registry)
}

/// The insecure-TLS warning every command on the test connections prints first.
fn without_warning(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|line| !line.starts_with("WARNING: connection"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn stops(args: &[&str], connection: &str, registry: &Registry) -> (String, Fake) {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let mut full = args.to_vec();

    full.extend(["--conn", connection]);

    let exit = cli(&full, directory.path(), &mut fake, registry);

    assert_eq!(exit.code, 2, "{args:?}: {}", exit.stderr);
    assert!(
        without_warning(&exit.stdout).is_empty(),
        "{args:?}: {}",
        exit.stdout
    );
    assert!(
        exit.stderr.starts_with("STOP: "),
        "{args:?}: {}",
        exit.stderr
    );

    (exit.stderr, fake)
}

// ---- hook a: kinds, grammar, options, local rules --------------------------------------------

#[test]
fn the_help_of_every_widened_command_names_the_type_and_the_option() {
    let (registry, _) = registry();
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    for command in ["read", "deploy", "create"] {
        let exit = cli(&[command, "--help"], directory.path(), &mut fake, &registry);

        assert_eq!(exit.code, 0, "{command}");
        assert!(exit.stdout.contains("XTEST"), "{command}: {}", exit.stdout);
    }

    let create = cli(
        &["create", "--help"],
        directory.path(),
        &mut fake,
        &registry,
    );

    assert!(
        create
            .stdout
            .contains("--note <NOTE>\n          An XTEST create only: a note"),
        "{}",
        create.stdout
    );

    let read = cli(&["read", "--help"], directory.path(), &mut fake, &registry);

    assert!(
        read.stdout
            .contains("XTEST: A test type that runs over the extension only"),
        "{}",
        read.stdout
    );
    assert_eq!(fake.opens(), (0, 0, 0, 0));
}

#[test]
fn the_unknown_type_texts_list_the_extension_type() {
    let (registry, _) = registry();
    let (read, _) = stops(&["read", "BOGUS", "x", "out"], "dev_100", &registry);
    let (create, _) = stops(
        &["create", "BOGUS", "ZX", "--text", "t"],
        "dev_100",
        &registry,
    );
    let (delete, _) = stops(&["delete", "BOGUS:ZX", "--plan", "p"], "dev_100", &registry);

    assert!(
        read.contains("[possible values: PROG, FUNC, CLAS, INTF, XTEST]"),
        "{read}"
    );
    assert!(
        create.contains("[possible values: PROG, FUGR, FUNC, CLAS, INTF, XTEST]"),
        "{create}"
    );
    assert!(
        delete.contains("delete target must be TYPE:NAME (PROG|FUGR|FUNC|CLAS|INTF|XTEST)"),
        "{delete}"
    );
}

#[test]
fn without_an_extension_the_type_is_an_unknown_type() {
    let plain = Registry::new();
    let (read, _) = stops(&["read", "XTEST", "x", "out"], "dev_100", &plain);
    let (delete, _) = stops(&["delete", "XTEST:ZX", "--plan", "p"], "dev_100", &plain);
    let (option, _) = stops(
        &["create", "PROG", "ZX", "--text", "t", "--note", "n"],
        "dev_100",
        &plain,
    );

    assert!(read.contains("invalid value 'XTEST'"), "{read}");
    assert!(
        read.contains("[possible values: PROG, FUNC, CLAS, INTF]"),
        "{read}"
    );
    assert!(delete.contains("(PROG|FUGR|FUNC|CLAS|INTF)"), "{delete}");
    assert!(option.contains("unexpected argument '--note'"), "{option}");
}

#[test]
fn deploy_of_the_type_parses_and_stops_with_its_refusal() {
    let (registry, state) = registry();
    let (message, fake) = stops(&["deploy", "XTEST", NAME, "f.abap"], "dev_100", &registry);

    assert_eq!(message, "STOP: deploy XTEST is not supported");
    assert_eq!(fake.opens(), (0, 0, 0, 0));
    assert!(state.borrow().preflights.is_empty());
}

#[test]
fn the_local_rules_stop_before_any_connection_in_the_standard_order() {
    let (registry, state) = registry();
    let long_name = "Z".repeat(21);
    let long_text = "t".repeat(37);
    let cases = [
        // name limit, then Z/Y, then text limit
        (
            format!("create XTEST {long_name} --text {long_text}"),
            "object name > 20 characters",
        ),
        (
            format!("create XTEST ANAME --text {long_text}"),
            "a created name must start with Z or Y: ANAME",
        ),
        (
            format!("create XTEST ZNAME --text {long_text}"),
            "--text is 37 characters; XTEST allows 36",
        ),
        // --fugr, then --subc, then the extension's own rule, then FILE
        (
            "create XTEST ZREJECT --text t --fugr ZFG --subc I".to_owned(),
            "--fugr is only valid for FUNC",
        ),
        (
            "create XTEST ZREJECT --text t --subc I".to_owned(),
            "--subc only applies to PROG",
        ),
        (
            "create XTEST ZREJECT f.abap --text t".to_owned(),
            "XTEST rejects ZREJECT",
        ),
        (
            "create XTEST ZNAME f.abap --text t".to_owned(),
            "create XTEST does not accept FILE",
        ),
        // FILE, then the package scope
        (
            "create XTEST ZNAME f.abap --text t --package ZPKG".to_owned(),
            "create XTEST does not accept FILE",
        ),
        (
            "create XTEST ZNAME --text t --package ZPKG".to_owned(),
            "package ZPKG is transportable: --tr is required",
        ),
        // read: the common rules, then the extension's
        (
            "read XTEST ZNOREAD o.abap --main ZM".to_owned(),
            "--main is only valid for PROG",
        ),
        (
            "read XTEST ZNOREAD o.abap".to_owned(),
            "XTEST rejects ZNOREAD",
        ),
        (
            "read XTEST ZNAME o.abap --lines 1-2".to_owned(),
            "--lines requires OUT to be '-' (stdout)",
        ),
    ];

    for (command, expected) in cases {
        let args: Vec<&str> = command.split(' ').collect();
        let (message, fake) = stops(&args, "dev_100", &registry);

        assert!(
            message.contains(expected),
            "{args:?}: {message} (expected {expected:?})"
        );
        assert_eq!(fake.opens(), (0, 0, 0, 0), "{args:?}");
    }

    // A local rule refuses before the preflight is asked.
    assert!(state.borrow().preflights.is_empty());
}

#[test]
fn an_option_of_the_type_given_with_a_standard_type_is_refused_before_any_connection() {
    let (registry, state) = registry();
    let (message, fake) = stops(
        &["create", "PROG", "ZPX", "--text", "t", "--note", "n"],
        "dev_100",
        &registry,
    );

    assert_eq!(message, "STOP: --note only applies to XTEST");
    assert_eq!(fake.opens(), (0, 0, 0, 0));

    // The option rule comes before the FILE rule.
    let (message, _) = stops(
        &[
            "create", "PROG", "ZPX", "f.abap", "--text", "t", "--note", "n",
        ],
        "dev_100",
        &registry,
    );

    assert_eq!(message, "STOP: --note only applies to XTEST");
    assert!(state.borrow().runs.is_empty());
}

// ---- hook g: read and create outside the router ----------------------------------------------

#[test]
fn read_and_create_run_the_extension_with_the_pin_and_the_cancel_flag_and_never_fall_back() {
    let (registry, state) = registry();
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let mut fake = Fake::default();
    let read = cli(
        &[
            "read",
            "XTEST",
            NAME,
            out.to_str().unwrap(),
            "--conn",
            "dev_100",
            "--backend",
            "rfc",
        ],
        directory.path(),
        &mut fake,
        &registry,
    );

    assert_eq!(
        (read.code, without_warning(&read.stdout)),
        (0, format!("XTEST {NAME}"))
    );

    let created = cli(
        &[
            "create",
            "XTEST",
            NAME,
            "--text",
            "t",
            "--note",
            "n1",
            "--simulate",
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
        &registry,
    );

    assert_eq!(created.code, 0, "{}", created.stderr);
    assert!(created.stdout.contains(&format!("CREATE XTEST {NAME}")));
    assert_eq!(
        state.borrow().runs,
        [format!("read {NAME}"), format!("create {NAME} note=n1")]
    );
    assert_eq!(state.borrow().pins, [Some(Backend::Rfc), None]);
    // Never an ADT or RFC SADT session, and the factories gave one function logon for the create.
    assert_eq!(fake.opens(), (0, 0, 1, 0));

    // The flag is the one of the factories.
    fake.cancelled = true;
    cli(
        &["read", "XTEST", NAME, "-", "--conn", "dev_100"],
        directory.path(),
        &mut fake,
        &registry,
    );
    assert_eq!(state.borrow().cancel_set, [false, false, true]);
}

#[test]
fn a_connection_without_rfc_or_a_pin_to_adt_is_refused_before_any_connection() {
    let (registry, state) = registry();
    let (adt_only, fake) = stops(&["read", "XTEST", NAME, "-"], "aol_100", &registry);
    let (pinned, _) = stops(
        &["read", "XTEST", NAME, "-", "--backend", "adt"],
        "dev_100",
        &registry,
    );
    let (create, _) = stops(
        &["create", "XTEST", NAME, "--text", "t", "--backend", "adt"],
        "dev_100",
        &registry,
    );

    assert!(
        adt_only.contains("read XTEST needs the RFC path; connection AOL_100"),
        "{adt_only}"
    );
    assert!(pinned.contains("read XTEST needs the RFC path"), "{pinned}");
    assert!(
        create.contains("create XTEST needs the RFC path"),
        "{create}"
    );
    assert_eq!(fake.opens(), (0, 0, 0, 0));
    assert_eq!(
        state.borrow().preflights,
        ["read None", "read Some(Adt)", "create Some(Adt)"]
    );
    assert!(state.borrow().runs.is_empty());
}

#[test]
fn an_existing_output_file_stops_a_read_before_the_extension_runs() {
    let (registry, state) = registry();
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");

    fs::write(&out, "x").unwrap();

    let mut fake = Fake::default();
    let exit = cli(
        &[
            "read",
            "XTEST",
            NAME,
            out.to_str().unwrap(),
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
        &registry,
    );

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr
            .contains("already exists; use --force to overwrite"),
        "{}",
        exit.stderr
    );
    assert!(state.borrow().runs.is_empty());
}

#[test]
fn a_function_logon_and_a_fresh_one_come_from_the_public_factories() {
    let (registry, _) = registry();
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();

    cli(
        &[
            "create",
            "XTEST",
            NAME,
            "--text",
            "RECONCILE",
            "--conn",
            "dev_100",
        ],
        directory.path(),
        &mut fake,
        &registry,
    );

    assert_eq!(fake.opens(), (0, 0, 1, 1));
    assert_eq!(fake.describes.get(), 1);
}

// ---- hook b: the mixed delete batch ----------------------------------------------------------

struct No;

impl Confirmer for No {
    fn interactive(&self) -> bool {
        false
    }

    fn confirm(&self, _: &str) -> bool {
        false
    }
}

/// The ADT side of a batch: it knows every object and records what it does.
#[derive(Default)]
struct Adt {
    inspected: Vec<String>,
    deleted: Vec<String>,
    begun: Vec<(Vec<String>, Option<String>)>,
    includes: Vec<String>,
    halt_after_delete: bool,
    halted: bool,
}

impl DeleteBackend for Adt {
    fn halted(&self) -> bool {
        self.halted
    }

    fn is_include(&mut self, _: DeleteKind, name: &str) -> bool {
        self.includes.iter().any(|include| include == name)
    }

    fn inspect(&mut self, _: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        self.inspected.push(name.to_owned());

        if self.halted {
            return Err("cannot open a new session".to_owned());
        }

        Ok(Some(ObjectFacts {
            package: "$TMP".to_owned(),
            author: "DEVUSER".to_owned(),
            group: None,
            sources: vec![SourceFact::active("MAIN", &[&format!("REPORT {name}.")])],
            contents: Vec::new(),
            where_used: Vec::new(),
            inactive: InactiveFact::Absent,
            tr: TrLock::Local,
        }))
    }

    fn delete(&mut self, _: DeleteKind, name: &str, _: Option<&str>) -> DeleteReply {
        self.deleted.push(name.to_owned());
        self.halted = self.halt_after_delete;

        DeleteReply::Deleted
    }

    fn reconcile(&mut self, _: DeleteKind, _: &str) -> Reconcile {
        Reconcile::Failed("not asked".to_owned())
    }

    fn begin(&mut self, rows: &[(DeleteKind, String)], tr: Option<&str>) -> Result<(), String> {
        self.begun.push((
            rows.iter()
                .map(|(kind, name)| format!("{kind} {name}"))
                .collect(),
            tr.map(str::to_owned),
        ));

        Ok(())
    }
}

fn targets(texts: &[&str]) -> Vec<DeleteTarget> {
    texts
        .iter()
        .map(|text| DeleteTarget::parse(text, &KINDS).unwrap())
        .collect()
}

struct Rig {
    state: Shared,
    adt: Adt,
}

impl Rig {
    fn new() -> Self {
        Self {
            state: Shared::default(),
            adt: Adt::default(),
        }
    }

    fn run<R>(&mut self, label: &'static str, batch: impl FnOnce(&mut MixedDelete) -> R) -> R {
        let mut backend = XDelete {
            state: self.state.clone(),
            cancel: CancelFlag::new(),
            wrote: false,
        };
        let mut mixed = MixedDelete::new(&mut self.adt, label, &mut backend);

        batch(&mut mixed)
    }

    fn simulate(
        &mut self,
        rows: &[DeleteTarget],
        tr: Option<&str>,
        label: &'static str,
    ) -> Result<abapreach::operation::delete::DeleteReport, String> {
        let policies = Policies::default();
        let context = Context {
            conn: "DEV_100",
            user: "DEVUSER",
            policies: &policies,
            confirmer: &No,
        };

        self.run(label, |backend| simulate(backend, &context, rows, tr))
    }

    fn execute(
        &mut self,
        rows: &[DeleteTarget],
        tr: Option<&str>,
        plan: &Plan,
        label: &'static str,
    ) -> Result<abapreach::operation::delete::DeleteReport, String> {
        let policies = Policies::default();
        let context = Context {
            conn: "DEV_100",
            user: "DEVUSER",
            policies: &policies,
            confirmer: &No,
        };

        self.run(label, |backend| execute(backend, &context, rows, tr, plan))
    }
}

fn status_of(report: &abapreach::operation::delete::DeleteReport, name: &str) -> DeleteStatus {
    report
        .results
        .iter()
        .find(|result| result.name == name)
        .unwrap_or_else(|| panic!("no result for {name}"))
        .status
}

#[test]
fn the_type_ranks_first_and_every_backend_is_told_the_whole_batch_and_the_tr() {
    let mut rig = Rig::new();
    let rows = targets(&["PROG:ZP", &format!("XTEST:{NAME}"), "CLAS:ZCL_A"]);

    rig.simulate(&rows, Some("hd1k900001"), "adt").unwrap();

    let expected = (
        vec![
            format!("XTEST {NAME}"),
            "CLAS ZCL_A".to_owned(),
            "PROG ZP".to_owned(),
        ],
        Some("HD1K900001".to_owned()),
    );

    assert_eq!(rig.adt.begun, std::slice::from_ref(&expected));
    assert_eq!(rig.state.borrow().begun, [expected]);
}

#[test]
fn a_mixed_simulation_prints_and_records_the_backend_per_object() {
    let mut rig = Rig::new();
    let rows = targets(&[&format!("XTEST:{NAME}"), "PROG:ZP"]);
    let report = rig.simulate(&rows, None, "adt").unwrap();
    let plan = report.plan.clone().unwrap();
    let stdout = &report.outcome.stdout;

    assert!(
        stdout.contains(&format!("== XTEST {NAME} ==\nbackend: {LABEL}\n")),
        "{stdout}"
    );
    assert!(stdout.contains("== PROG ZP ==\nbackend: adt\n"), "{stdout}");

    let json = serde_json::to_value(&plan).unwrap();

    assert_eq!(json["backends"][0], json!(["XTEST", NAME, LABEL]));
    assert_eq!(json["backends"][1], json!(["PROG", "ZP", "adt"]));
    assert_eq!(adt_pin(&plan), Some(BackendKind::AdtHttp));
}

#[test]
fn a_batch_without_the_type_records_and_prints_no_backend_and_keeps_the_plan_file() {
    let mut rig = Rig::new();
    let report = rig
        .simulate(&targets(&["PROG:ZP", "CLAS:ZCL_A"]), None, "adt")
        .unwrap();
    let plan = report.plan.unwrap();
    let json = serde_json::to_value(&plan).unwrap();

    assert!(!report.outcome.stdout.contains("backend:"));
    assert!(plan.backends.is_empty());
    assert!(json.get("backends").is_none(), "{json}");
}

#[test]
fn a_plan_that_records_no_backend_binds_a_mixed_batch_but_not_a_batch_of_the_type_alone() {
    let rows = targets(&[&format!("XTEST:{NAME}"), "PROG:ZP"]);
    let mut rig = Rig::new();
    let mut plan = rig.simulate(&rows, None, "adt").unwrap().plan.unwrap();

    plan.backends.clear();

    let error = rig.execute(&rows, None, &plan, "adt").unwrap_err();

    assert_eq!(
        error,
        "plan backend for PROG ZP is not recorded, this run would use adt; simulate again"
    );
    assert!(rig.adt.deleted.is_empty());

    // `plan_backend_optional`: a plan of another tool names no backend for the type alone.
    let rows = targets(&[&format!("XTEST:{NAME}")]);
    let mut rig = Rig::new();
    let mut plan = rig.simulate(&rows, None, "adt").unwrap().plan.unwrap();

    plan.backends.clear();

    let report = rig.execute(&rows, None, &plan, "adt").unwrap();

    assert_eq!(report.outcome.exit, 0, "{}", report.outcome.stdout);
}

#[test]
fn a_mixed_delete_runs_each_object_on_the_backend_its_plan_names() {
    let rows = targets(&[&format!("XTEST:{NAME}"), "PROG:ZP"]);
    let mut rig = Rig::new();
    let plan = rig.simulate(&rows, None, "adt").unwrap().plan.unwrap();
    let error = rig.execute(&rows, None, &plan, "rfc (SADT)").unwrap_err();

    assert_eq!(
        error,
        "plan backend for PROG ZP is adt, this run would use rfc (SADT); simulate again"
    );
    assert!(rig.adt.deleted.is_empty());
    assert!(rig.state.borrow().deleted.is_empty());

    let report = rig.execute(&rows, None, &plan, "adt").unwrap();

    assert_eq!(report.outcome.exit, 0, "{}", report.outcome.stdout);
    assert_eq!(rig.adt.deleted, ["ZP"]);
    assert_eq!(rig.state.borrow().deleted, [NAME]);

    // A plan that names another backend for the type itself stops as well.
    let mut other = plan.clone();

    other.backends[0].2 = "adt".to_owned();

    let error = rig.execute(&rows, None, &other, "adt").unwrap_err();

    assert_eq!(
        error,
        format!("plan backend for XTEST {NAME} is adt, this run would use {LABEL}; simulate again")
    );
}

#[test]
fn a_halted_side_stops_a_mixed_batch_and_skips_the_rest() {
    let rows = targets(&[&format!("XTEST:{NAME}"), "PROG:ZP1", "PROG:ZP2", "PROG:ZP3"]);
    let mut rig = Rig::new();
    let plan = rig.simulate(&rows, None, "adt").unwrap().plan.unwrap();

    rig.adt.halt_after_delete = true;

    let report = rig.execute(&rows, None, &plan, "adt").unwrap();
    let out = &report.outcome.stdout;

    assert_eq!(status_of(&report, NAME), DeleteStatus::Deleted, "{out}");
    assert_eq!(status_of(&report, "ZP1"), DeleteStatus::Deleted, "{out}");
    assert_eq!(status_of(&report, "ZP2"), DeleteStatus::Rejected, "{out}");
    assert_eq!(status_of(&report, "ZP3"), DeleteStatus::Skipped, "{out}");

    // The extension side halts too: the rest is skipped there as well.
    let mut rig = Rig::new();
    let rows = targets(&[&format!("XTEST:{NAME}"), "XTEST:YTEST_XT2", "PROG:ZP"]);
    let plan = rig.simulate(&rows, None, "adt").unwrap().plan.unwrap();

    rig.state.borrow_mut().halt_after_delete = true;

    let report = rig.execute(&rows, None, &plan, "adt").unwrap();

    assert_eq!(status_of(&report, NAME), DeleteStatus::Deleted);
    assert_eq!(status_of(&report, "YTEST_XT2"), DeleteStatus::Rejected);
    assert_eq!(status_of(&report, "ZP"), DeleteStatus::Skipped);
    assert!(rig.adt.deleted.is_empty());
}

#[test]
fn the_self_guard_is_decided_per_type_and_a_type_without_one_is_never_self() {
    let mut rig = Rig::new();
    let report = rig
        .simulate(
            &targets(&["XTEST:YSELF_1", &format!("XTEST:{NAME}")]),
            None,
            "adt",
        )
        .unwrap();

    assert_eq!(status_of(&report, "YSELF_1"), DeleteStatus::Rejected);
    assert_eq!(status_of(&report, NAME), DeleteStatus::SimulatedOk);
    assert!(
        report
            .outcome
            .stdout
            .contains("Self-guard: XTEST YSELF_1 belongs to this deploy tool; delete rejected"),
        "{}",
        report.outcome.stdout
    );
}

#[test]
fn the_labels_of_the_two_adt_backends_round_trip_to_the_pin() {
    for backend in [BackendKind::AdtHttp, BackendKind::RfcSadt] {
        let plan = Plan {
            conn: "DEV_100".to_owned(),
            tr: None,
            objects: Vec::new(),
            backends: vec![
                ("XTEST".to_owned(), NAME.to_owned(), LABEL.to_owned()),
                (
                    "PROG".to_owned(),
                    "ZP".to_owned(),
                    adt_label(backend).to_owned(),
                ),
            ],
        };

        assert_eq!(adt_pin(&plan), Some(backend));
    }

    let only = Plan {
        conn: "DEV_100".to_owned(),
        tr: None,
        objects: Vec::new(),
        backends: vec![("XTEST".to_owned(), NAME.to_owned(), LABEL.to_owned())],
    };

    assert_eq!(adt_pin(&only), None);
}

#[test]
fn a_mixed_batch_asks_the_adt_side_which_program_is_an_include_and_runs_it_last() {
    let mut rig = Rig::new();

    rig.adt.includes = vec!["ZINC".to_owned()];

    let rows = targets(&["PROG:ZINC", "PROG:ZMAIN", &format!("XTEST:{NAME}")]);
    let report = rig.simulate(&rows, None, "adt").unwrap();
    let stdout = &report.outcome.stdout;

    assert!(stdout.find("== PROG ZMAIN ==").unwrap() < stdout.find("== PROG ZINC ==").unwrap());
}

#[test]
fn the_adt_backend_refuses_the_type_with_no_extension_wording() {
    use abapreach::operation::delete_adt::AdtDelete;

    let mut transport = ScriptedTransport::replies(Vec::new());
    let mut open = || -> Opened { Err(TransportError::PreSendConnectivity("no".to_owned())) };
    let mut adt = AdtDelete::new(&mut transport, "DEVUSER", &mut open);
    let kind = DeleteKind::Extension(xtest());

    assert_eq!(
        adt.inspect(kind, NAME),
        Err("XTEST is not an ADT object".to_owned())
    );
    assert!(transport.calls.is_empty());
}

// ---- hook b, g: through the command line ------------------------------------------------------

fn source(text: &str) -> Result<Response, TransportError> {
    Ok(Response::text(200, text))
}

fn scripted(replies: Vec<Result<Response, TransportError>>) -> Opened {
    Ok(Box::new(ScriptedTransport::replies(replies)))
}

/// What the ADT delete reads about a program before it deletes it.
fn prog_facts() -> Vec<Result<Response, TransportError>> {
    vec![
        source(
            "<x xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:responsible=\"OWNER\">\
             <adtcore:packageRef adtcore:name=\"$TMP\"/></x>",
        ),
        source("REPORT ztest_del."),
        source("<usageReferences:usageReferenceResult xmlns:usageReferences=\"u\"/>"),
        source("<feed xmlns=\"http://www.w3.org/2005/Atom\"/>"),
    ]
}

const LOCK: &str = "<DATA><LOCK_HANDLE>a/b+c=</LOCK_HANDLE><CORRNR></CORRNR>\
    <IS_LOCAL>X</IS_LOCAL></DATA>";

fn mixed(
    directory: &Path,
    plan: &Path,
    simulate: bool,
    fake: &mut Fake,
    registry: &Registry,
) -> Exit {
    let mut args = vec![
        "delete",
        "PROG:ZTEST_DEL",
        "XTEST:YTEST_XT1",
        "--plan",
        plan.to_str().unwrap(),
        "--conn",
        "dev_100",
    ];

    if simulate {
        args.push("--simulate");
    }

    cli(&args, directory, fake, registry)
}

fn plan_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_command_line_runs_a_mixed_batch_on_both_backends_and_pins_the_delete_to_the_plan() {
    let (registry, state) = registry();
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(prog_facts()));

    let simulated = mixed(directory.path(), &plan, true, &mut fake, &registry);

    assert_eq!(
        simulated.code, 0,
        "{} {}",
        simulated.stdout, simulated.stderr
    );
    assert!(
        simulated
            .stdout
            .contains(&format!("== XTEST {NAME} ==\nbackend: {LABEL}\n")),
        "{}",
        simulated.stdout
    );
    assert!(
        simulated
            .stdout
            .contains("== PROG ZTEST_DEL ==\nbackend: adt\n")
    );
    assert_eq!(
        plan_json(&plan)["backends"],
        json!([["XTEST", NAME, LABEL], ["PROG", "ZTEST_DEL", "adt"]])
    );

    // The delete: ADT reads, locks and deletes the program, a fresh session proves it gone.
    let mut replies = prog_facts();

    replies.push(Ok(Response::text(200, LOCK)));
    replies.push(Ok(Response::empty(200)));
    fake.adt.push_back(scripted(replies));
    fake.adt
        .push_back(scripted(vec![Ok(Response::text(404, "<exc/>"))]));

    let deleted = mixed(directory.path(), &plan, false, &mut fake, &registry);

    assert_eq!(deleted.code, 0, "{} {}", deleted.stdout, deleted.stderr);
    assert!(
        deleted
            .stdout
            .contains("SUMMARY: 2 OK, 0 FAILED, 0 SKIPPED"),
        "{}",
        deleted.stdout
    );
    assert_eq!(fake.rfc_opened, 0);
    assert_eq!(state.borrow().deleted, [NAME]);
    assert_eq!(
        state.borrow().begun.last().unwrap().0,
        [format!("XTEST {NAME}"), "PROG ZTEST_DEL".to_owned()]
    );
}

#[test]
fn the_simulation_falls_back_to_rfc_sadt_and_the_delete_stays_there() {
    let (registry, _) = registry();
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let mut fake = Fake::default();

    // ADT does not open at the simulation: nothing was sent, so RFC SADT may run the whole batch.
    fake.adt.push_back(Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    )));
    fake.rfc.push_back(scripted(prog_facts()));

    let simulated = mixed(directory.path(), &plan, true, &mut fake, &registry);

    assert_eq!(
        simulated.code, 0,
        "{} {}",
        simulated.stdout, simulated.stderr
    );
    assert_eq!(
        plan_json(&plan)["backends"][1],
        json!(["PROG", "ZTEST_DEL", "rfc (SADT)"])
    );

    // The delete is pinned to RFC SADT: ADT is not even tried.
    let adt_before = fake.adt_opened;
    let mut replies = prog_facts();

    replies.push(Ok(Response::text(200, LOCK)));
    replies.push(Ok(Response::empty(200)));
    fake.rfc.push_back(scripted(replies));
    fake.rfc
        .push_back(scripted(vec![Ok(Response::text(404, "<exc/>"))]));

    let deleted = mixed(directory.path(), &plan, false, &mut fake, &registry);

    assert_eq!(deleted.code, 0, "{} {}", deleted.stdout, deleted.stderr);
    assert_eq!(fake.adt_opened, adt_before);
}

#[test]
fn a_program_left_after_the_extension_delete_is_rejected_and_nothing_falls_back() {
    let (registry, state) = registry();
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(prog_facts()));

    let simulated = mixed(directory.path(), &plan, true, &mut fake, &registry);

    assert_eq!(simulated.code, 0, "{}", simulated.stderr);

    // The ADT session opens but its first read never leaves the process: the extension row was
    // deleted before, so the program is rejected and nothing is repeated on another backend.
    fake.adt
        .push_back(scripted(vec![Err(TransportError::PreSendConnectivity(
            "no route".to_owned(),
        ))]));
    fake.rfc.push_back(scripted(prog_facts()));

    let deleted = mixed(directory.path(), &plan, false, &mut fake, &registry);

    assert_eq!(deleted.code, 1, "{} {}", deleted.stdout, deleted.stderr);
    assert!(
        deleted.stdout.contains("DELETE: DELETED"),
        "{}",
        deleted.stdout
    );
    assert!(
        deleted.stdout.contains("FAILED PROG ZTEST_DEL"),
        "{}",
        deleted.stdout
    );
    assert_eq!(fake.rfc_opened, 0);
    assert_eq!(state.borrow().deleted, [NAME]);
}

#[test]
fn a_delete_pinned_to_adt_does_not_fall_back_when_adt_does_not_open() {
    let (registry, state) = registry();
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let mut fake = Fake::default();

    fake.adt.push_back(scripted(prog_facts()));
    mixed(directory.path(), &plan, true, &mut fake, &registry);

    // Nothing opens for the delete: the batch must not move to RFC SADT.
    fake.adt.push_back(Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    )));
    fake.rfc.push_back(scripted(prog_facts()));

    let deleted = mixed(directory.path(), &plan, false, &mut fake, &registry);

    assert_eq!(deleted.code, 1, "{} {}", deleted.stdout, deleted.stderr);
    assert_eq!(fake.rfc_opened, 0);
    assert!(state.borrow().deleted.is_empty());
}

#[test]
fn a_mixed_plan_without_a_recorded_backend_stops_before_the_first_object() {
    let (registry, state) = registry();
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");

    fs::write(
        &plan,
        format!(
            r#"{{"conn":"DEV_100","tr":null,"objects":[["XTEST","{NAME}","h"],["PROG","ZTEST_DEL","h"]]}}"#
        ),
    )
    .unwrap();

    let mut fake = Fake::default();

    fake.adt.push_back(scripted(Vec::new()));

    let exit = mixed(directory.path(), &plan, false, &mut fake, &registry);

    assert_eq!(exit.code, 2, "{} {}", exit.stdout, exit.stderr);
    assert!(
        exit.stderr
            .contains("plan backend for PROG ZTEST_DEL is not recorded"),
        "{}",
        exit.stderr
    );
    assert!(state.borrow().deleted.is_empty());
}

#[test]
fn a_batch_of_the_type_alone_needs_no_other_backend() {
    let (registry, state) = registry();
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let mut fake = Fake::default();
    let args = |extra: &[&'static str]| {
        let mut args = vec![
            "delete",
            "XTEST:ytest_xt1",
            "--plan",
            plan.to_str().unwrap(),
            "--conn",
            "dev_100",
        ];

        args.extend(extra);
        args.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    let env_map = env(directory.path());
    let simulated = run_with(
        [vec!["abapreach".to_owned()], args(&["--simulate"])].concat(),
        env_map.clone(),
        &mut fake,
        &registry,
    );

    assert_eq!(simulated.code, 0, "{}", simulated.stderr);
    assert!(
        simulated.stdout.contains(&format!("backend: {LABEL}\n")),
        "{}",
        simulated.stdout
    );

    let deleted = run_with(
        [vec!["abapreach".to_owned()], args(&[])].concat(),
        env_map,
        &mut fake,
        &registry,
    );

    assert_eq!(deleted.code, 0, "{} {}", deleted.stdout, deleted.stderr);
    assert_eq!(state.borrow().deleted, [NAME]);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
}

#[test]
fn a_backend_that_cannot_start_stops_before_the_first_object_with_its_own_exit_code() {
    for (unreached, code, prefix) in [(true, 1, "ERROR: "), (false, 2, "STOP: ")] {
        let (registry, state) = registry();
        let directory = tempfile::tempdir().unwrap();
        let plan = directory.path().join("plan.json");
        let mut fake = Fake::default();

        state.borrow_mut().begin_error = Some(unreached);

        let exit = cli(
            &[
                "delete",
                "XTEST:YTEST_XT1",
                "--simulate",
                "--plan",
                plan.to_str().unwrap(),
                "--conn",
                "dev_100",
            ],
            directory.path(),
            &mut fake,
            &registry,
        );

        assert_eq!(exit.code, code, "{} {}", exit.stdout, exit.stderr);
        assert_eq!(
            exit.stderr,
            format!("{prefix}the XTEST backend cannot start")
        );
        assert!(state.borrow().inspected.is_empty());
    }
}

#[test]
fn a_batch_with_the_type_on_a_connection_without_rfc_stops_before_the_first_object() {
    let (registry, state) = registry();
    let (message, fake) = stops(
        &[
            "delete",
            "PROG:ZP",
            "XTEST:YTEST_XT1",
            "--simulate",
            "--plan",
            "p.json",
        ],
        "aol_100",
        &registry,
    );

    assert!(
        message.contains("delete XTEST needs the RFC path; connection AOL_100"),
        "{message}"
    );
    assert_eq!(fake.opens(), (0, 0, 0, 0));
    assert_eq!(state.borrow().preflights, ["delete None"]);
    assert!(state.borrow().begun.is_empty());
}

// ---- hook c: the MCP tools -------------------------------------------------------------------

struct Rig2 {
    directory: tempfile::TempDir,
    env: BTreeMap<String, String>,
    registry: Registry,
    state: Shared,
    fake: Fake,
    gate: TokenGate,
}

fn rig() -> Rig2 {
    let directory = tempfile::tempdir().unwrap();
    let env = env(directory.path());
    let (registry, state) = registry();

    Rig2 {
        directory,
        env,
        registry,
        state,
        fake: Fake::default(),
        gate: TokenGate::new(),
    }
}

/// A client that types `text` into every form.
struct Typing(&'static str);

impl Session for Typing {
    fn can_elicit(&self) -> bool {
        true
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }

    fn elicit(
        &mut self,
        _: &Form,
        _: Duration,
        _: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
    ) -> Answer {
        Answer::Accepted(self.0.to_owned())
    }

    fn log(&mut self, _: &str) {}
}

impl Rig2 {
    fn config(&self) -> Config {
        Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap()
    }

    fn call(&mut self, name: &str, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.fake),
            self.gate.clone(),
        )
        .with_registry(&self.registry);

        tools.call(name, arguments.as_object().unwrap())
    }

    fn call_as(&mut self, session: &mut dyn Session, name: &str, arguments: Value) -> ToolResult {
        let mut tools = McpTools::new(
            ReadTools::new(self.config(), self.env.clone(), &mut self.fake),
            self.gate.clone(),
        )
        .with_registry(&self.registry);

        tools.call_in(name, arguments.as_object().unwrap(), session)
    }

    fn cli(&mut self, args: &[&str]) -> Exit {
        let mut all = vec!["abapreach".to_owned()];

        all.extend(args.iter().map(|arg| (*arg).to_owned()));
        run_with(all, self.env.clone(), &mut self.fake, &self.registry)
    }

    fn directory(&self) -> &Path {
        self.directory.path()
    }
}

fn create_arguments() -> Value {
    json!({"conn": "DEV_100", "type": "XTEST", "name": NAME, "text": "probe", "note": "n1"})
}

fn token_of(result: &ToolResult) -> String {
    result.structured["plan_token"]
        .as_str()
        .unwrap_or_else(|| panic!("no plan_token: {}", result.text))
        .to_owned()
}

fn with_token(mut arguments: Value, token: &str) -> Value {
    arguments["plan_token"] = json!(token);

    arguments
}

#[test]
fn the_tool_list_offers_the_type_and_the_option_where_the_hook_says() {
    let kinds = KINDS.to_vec();
    let tools = abapreach::mcp::tool_list_with(&["DEV_100".to_owned()], &kinds);
    let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap();

    for name in ["abap_create_simulate", "abap_create"] {
        let properties = &find(name)["inputSchema"]["properties"];

        assert_eq!(
            properties["type"]["enum"],
            json!(["PROG", "FUGR", "FUNC", "CLAS", "INTF", "XTEST"]),
            "{name}"
        );
        assert_eq!(properties["note"]["type"], "string", "{name}");
        assert_eq!(
            properties["note"]["description"], "XTEST only: a note",
            "{name}"
        );
    }

    for name in ["abap_delete_simulate", "abap_delete"] {
        let description = find(name)["inputSchema"]["properties"]["objects"]["description"]
            .as_str()
            .unwrap();

        assert!(
            description.ends_with("PROG|FUGR|FUNC|CLAS|INTF|XTEST"),
            "{description}"
        );
    }

    // A read or deploy never lists the type.
    for name in ["abap_read_source", "abap_simulate", "abap_deploy"] {
        assert_eq!(
            find(name)["inputSchema"]["properties"]["type"]["enum"],
            json!(["PROG", "FUNC", "CLAS", "INTF"]),
            "{name}"
        );
    }

    // No registered type: the schema is the standard one.
    let plain = abapreach::mcp::tool_list_for(&["DEV_100".to_owned()]);
    let again = abapreach::mcp::tool_list_with(&["DEV_100".to_owned()], &[]);

    assert_eq!(plain, again);
}

#[test]
fn the_unknown_type_texts_of_the_tools_list_the_extension_type() {
    let mut rig = rig();
    let created = rig.call(
        "abap_create_simulate",
        json!({"conn": "DEV_100", "type": "BOGUS", "name": NAME, "text": "t"}),
    );
    let deleted = rig.call(
        "abap_delete_simulate",
        json!({"conn": "DEV_100", "objects": ["BOGUS:ZX"]}),
    );
    let read = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "XTEST", "name": NAME}),
    );
    let unknown = rig.call(
        "abap_read_source",
        json!({"conn": "DEV_100", "type": "BOGUS", "name": NAME}),
    );

    assert_eq!(
        created.text,
        "STOP: type must be one of PROG, FUGR, FUNC, CLAS, INTF, XTEST"
    );
    assert!(
        deleted.text.contains("(PROG|FUGR|FUNC|CLAS|INTF|XTEST)"),
        "{}",
        deleted.text
    );
    assert_eq!(
        read.text,
        "STOP: abap_read_source does not read XTEST; use the CLI read XTEST"
    );
    assert_eq!(
        unknown.text,
        "STOP: type must be one of PROG, FUNC, CLAS, INTF"
    );
    assert_eq!(rig.fake.opens(), (0, 0, 0, 0));
}

#[test]
fn the_local_rules_of_the_tool_are_the_rules_of_the_cli_in_the_same_order() {
    let long_name = "Z".repeat(21);
    let long_text = "t".repeat(37);
    let cases = [
        (
            json!({"type": "XTEST", "name": long_name, "text": long_text}),
            format!("create XTEST {long_name} --text {long_text}"),
        ),
        (
            json!({"type": "XTEST", "name": "ZREJECT", "text": "t", "fugr": "ZFG", "subc": "I"}),
            "create XTEST ZREJECT --text t --fugr ZFG --subc I".to_owned(),
        ),
        (
            json!({"type": "XTEST", "name": "ZREJECT", "text": "t"}),
            "create XTEST ZREJECT --text t".to_owned(),
        ),
        (
            json!({"type": "PROG", "name": "ZPX", "text": "t", "note": "n"}),
            "create PROG ZPX --text t --note n".to_owned(),
        ),
        (
            json!({"type": "XTEST", "name": "ZNAME", "text": "t", "package": "ZPKG"}),
            "create XTEST ZNAME --text t --package ZPKG".to_owned(),
        ),
    ];

    for (mut arguments, command) in cases {
        let mut rig = rig();
        let mut full: Vec<&str> = command.split(' ').collect();

        full.extend(["--conn", "dev_100"]);

        let cli = rig.cli(&full);

        arguments["conn"] = json!("DEV_100");

        for tool in ["abap_create_simulate", "abap_create"] {
            let mut arguments = arguments.clone();

            if tool == "abap_create" {
                arguments["plan_token"] = json!("tok");
            }

            let result = rig.call(tool, arguments);

            assert_eq!(cli.code, 2, "{}", cli.stderr);
            assert!(result.is_error, "{tool}");
            assert_eq!(result.text, cli.stderr, "{tool}");
        }

        assert_eq!(rig.fake.opens(), (0, 0, 0, 0));
    }
}

#[test]
fn a_refusal_of_the_type_comes_before_the_token_and_any_connection() {
    let mut rig = rig();
    let adt_only = json!({"conn": "AOL_100", "type": "XTEST", "name": NAME, "text": "t"});
    let simulated = rig.call("abap_create_simulate", adt_only.clone());
    let executed = rig.call("abap_create", with_token(adt_only, "tok"));
    let deleted = rig.call(
        "abap_delete",
        json!({"conn": "AOL_100", "objects": [format!("XTEST:{NAME}")], "plan_token": "tok"}),
    );

    for result in [&simulated, &executed, &deleted] {
        assert!(result.is_error);
        assert!(
            result
                .text
                .contains("needs the RFC path; connection AOL_100"),
            "{}",
            result.text
        );
    }

    assert_eq!(rig.fake.opens(), (0, 0, 0, 0));
    // The token never entered into it: the preflight ran with no pin.
    assert_eq!(
        rig.state.borrow().preflights,
        ["create None", "create None", "delete None"]
    );
}

#[test]
fn the_token_binds_the_option_and_the_execute_runs_the_extension_outside_the_router() {
    let mut rig = rig();
    let simulated = rig.call("abap_create_simulate", create_arguments());

    assert!(!simulated.is_error, "{}", simulated.text);
    assert_eq!(rig.state.borrow().runs, [format!("create {NAME} note=n1")]);

    let token = token_of(&simulated);
    let mut other = create_arguments();

    other["note"] = json!("n2");

    let refused = rig.call("abap_create", with_token(other, &token));

    assert!(refused.is_error);
    assert!(
        refused.text.contains("plan_token does not match"),
        "{}",
        refused.text
    );

    // A lower-case value binds as its upper-case form, like a name.
    let mut lower = create_arguments();

    lower["note"] = json!("N1");

    let created = rig.call("abap_create", with_token(lower, &token));

    assert!(!created.is_error, "{}", created.text);
    assert_eq!(created.structured["status"], "ACTIVE");
    assert_eq!(rig.state.borrow().runs.len(), 2);
    // Never an ADT or RFC SADT session; the function logons came from the factories.
    assert_eq!((rig.fake.adt_opened, rig.fake.rfc_opened), (0, 0));
    assert_eq!(rig.fake.function_opened, 2);

    // A token minted without the option does not fit an execute that adds one.
    let mut plain = create_arguments();

    plain.as_object_mut().unwrap().remove("note");

    let simulated = rig.call("abap_create_simulate", plain.clone());
    let mut with_note = plain;

    with_note["note"] = json!("n1");

    let refused = rig.call("abap_create", with_token(with_note, &token_of(&simulated)));

    assert!(
        refused.text.contains("plan_token does not match"),
        "{}",
        refused.text
    );
}

#[test]
fn the_confirmation_names_the_label_of_the_extension_and_lists_its_option() {
    let (registry, _) = registry();
    let rig = rig();
    let connection = rig.config().resolve("DEV_100", &rig.env).unwrap();
    let ticket = Ticket::new(
        Operation::Create,
        "DEV_100",
        BTreeMap::from([
            ("type".to_owned(), "XTEST".to_owned()),
            ("name".to_owned(), NAME.to_owned()),
            ("note".to_owned(), "N1".to_owned()),
            ("text".to_owned(), "probe".to_owned()),
        ]),
    );
    let summary = confirmation_summary_with(&connection, &ticket, Some(&registry));

    assert!(summary.contains(&format!("backend {LABEL}")), "{summary}");
    assert!(
        summary.find("name:").unwrap() < summary.find("note: N1").unwrap(),
        "{summary}"
    );
    assert!(
        summary.find("note: N1").unwrap() < summary.find("text:").unwrap(),
        "{summary}"
    );

    // The same ticket without the registry does not know the label.
    let plain = confirmation_summary_with(&connection, &ticket, None);

    assert!(plain.contains("backend unknown"), "{plain}");
}

#[test]
fn a_delete_of_the_type_is_simulated_and_deleted_with_the_typed_confirmation() {
    let mut rig = rig();
    let arguments = json!({"conn": "DEV_100", "objects": [format!("XTEST:{NAME}")]});
    let simulated = rig.call("abap_delete_simulate", arguments.clone());

    assert!(!simulated.is_error, "{}", simulated.text);
    assert!(
        simulated.text.contains(&format!("backend: {LABEL}")),
        "{}",
        simulated.text
    );

    let done = rig.call_as(
        &mut Typing(NAME),
        "abap_delete",
        with_token(arguments, &token_of(&simulated)),
    );

    assert!(!done.is_error, "{}", done.text);
    assert_eq!(done.structured["objects"][0]["status"], "DELETED");
    assert_eq!(rig.state.borrow().deleted, [NAME]);
    assert_eq!((rig.fake.adt_opened, rig.fake.rfc_opened), (0, 0));
    let _ = rig.directory();
}

// ---- hook d: the doctor line -----------------------------------------------------------------

fn doctor(registry: &Registry, fake: &mut Fake) -> Exit {
    let directory = tempfile::tempdir().unwrap();

    cli(
        &["doctor", "--conn", "qas_100"],
        directory.path(),
        fake,
        registry,
    )
}

#[test]
fn the_doctor_line_comes_after_the_sadt_probe_and_before_the_capability_lines_on_one_logon() {
    let (registry, state) = registry();
    let mut fake = Fake::default();
    let exit = doctor(&registry, &mut fake);
    let lines: Vec<&str> = exit.stdout.lines().collect();
    let position = |needle: &str| {
        lines
            .iter()
            .position(|line| line.starts_with(needle))
            .unwrap()
    };

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert_eq!(lines[position("RFC SADT probe") + 1], "XTEST: installed");
    assert!(position("XTEST: installed") < position("capability read PROG"));
    // One logon: the probe's own, no ping and no second function logon; the extension used it.
    assert_eq!((fake.pings, fake.probes, fake.function_opened), (0, 1, 0));
    assert_eq!(fake.describes.get(), 1);
    assert_eq!(state.borrow().describes, 1);
}

#[test]
fn the_doctor_line_reports_a_failed_ping_and_a_missing_library_and_never_changes_the_exit() {
    let (registry, state) = registry();
    let mut fake = Fake {
        ping: Some(Err(TransportError::PreSendConnectivity(
            "no route".to_owned(),
        ))),
        ..Fake::default()
    };
    let failed = doctor(&registry, &mut fake);

    assert_eq!(failed.code, 1, "{}", failed.stdout);
    assert!(
        failed
            .stdout
            .contains("XTEST: not run (RFC SADT probe failed)"),
        "{}",
        failed.stdout
    );
    assert_eq!(fake.function_opened, 0);

    let mut fake = Fake {
        library: Some(Err("RFC library not found".to_owned())),
        ..Fake::default()
    };
    let missing = doctor(&registry, &mut fake);

    assert!(
        missing
            .stdout
            .contains("XTEST: unavailable: RFC library not found"),
        "{}",
        missing.stdout
    );
    assert_eq!((fake.pings, fake.probes), (0, 0));
    assert_eq!(state.borrow().doctor.len(), 2);
}

#[test]
fn without_an_extension_the_doctor_pings_and_prints_no_extension_line() {
    let plain = Registry::new();
    let mut fake = Fake::default();
    let exit = doctor(&plain, &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stdout);
    assert!(!exit.stdout.contains("XTEST"));
    assert_eq!((fake.pings, fake.probes, fake.function_opened), (1, 0, 0));
}

// ---- hook e: the fault target ----------------------------------------------------------------

#[cfg(feature = "fault-injection")]
mod fault {
    use super::*;
    use abapreach::extension::{ENV, Fault, Target};
    use abapreach::transport::Request;
    use abapreach::transport::rfc::{FakeRfcApi, RfcTransport};

    fn with_fault(registry: &Registry, value: &str) -> Exit {
        let directory = tempfile::tempdir().unwrap();
        let mut fake = Fake::default();
        let mut env = env(directory.path());

        env.insert(ENV.to_owned(), value.to_owned());

        run_with(
            [
                "abapreach",
                "create",
                "XTEST",
                NAME,
                "--text",
                "t",
                "--simulate",
                "--conn",
                "dev_100",
            ]
            .map(str::to_owned)
            .to_vec(),
            env,
            &mut fake,
            registry,
        )
    }

    #[test]
    fn the_extension_target_is_accepted_and_without_the_extension_it_is_unknown() {
        let (registry, _) = registry();
        let accepted = with_fault(&registry, "drop-reply:XTEST");

        assert_eq!(accepted.code, 0, "{}", accepted.stderr);
        assert!(
            accepted
                .stderr
                .contains("WARNING: fault injection active (drop-reply:XTEST)"),
            "{}",
            accepted.stderr
        );

        for value in [
            "drop-reply:XTEST",
            "drop-reply:XTEST,fail-read:RECONCILE",
            "drop-reply:OTHER",
        ] {
            let plain = with_fault(&Registry::new(), value);

            assert_eq!(plain.code, 2, "{value}");
            assert_eq!(
                plain.stderr, "STOP: unknown ABAPREACH_FAULT value",
                "{value}"
            );
        }

        // A name the extension did not register is unknown even with the extension.
        assert_eq!(with_fault(&registry, "drop-reply:OTHER").code, 2);
    }

    #[test]
    fn the_lenient_environment_parse_keeps_the_other_faults_and_no_transport_matches_the_name() {
        let faults = Fault::from_lookup_lenient(Some("drop-reply:XTEST,fail-read:RECONCILE"));

        assert_eq!(faults.len(), 2);
        assert_eq!(faults[0].target(), Some(Target::Named("XTEST")));
        assert!(faults[1].fail_read(true).is_some());

        // The strict parse of the up-front check is a different path.
        assert!(Fault::from_lookup(Some("drop-reply:XTEST")).is_err());
        assert!(Fault::from_lookup_with(Some("drop-reply:XTEST"), &["XTEST"]).is_ok());

        let mut transport = RfcTransport::with_fault(FakeRfcApi::ok(), Some(faults[0]));

        assert!(
            transport
                .request(Request::get("/sap/bc/adt/discovery"))
                .is_ok()
        );
    }
}

// ---- hook h: the entry point and the version -------------------------------------------------

#[test]
fn the_version_is_the_crate_version_unless_the_registry_sets_one() {
    let directory = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let plain = cli(
        &["--version"],
        directory.path(),
        &mut fake,
        &Registry::new(),
    );
    let (registry, _) = registry();
    let custom = cli(
        &["--version"],
        directory.path(),
        &mut fake,
        &Registry::new().version("0.15.0-ext.1"),
    );
    let with_extension = cli(&["--version"], directory.path(), &mut fake, &registry);

    assert_eq!(
        plain.stdout,
        format!("abapreach {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(custom.stdout, "abapreach 0.15.0-ext.1");
    assert_eq!(with_extension.stdout, plain.stdout);
    assert_eq!(Registry::new().version_text(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn the_server_reports_the_version_of_the_registry() {
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}},
    });
    let report = |server: Server<NoTools>| {
        let mut server = server;
        let output = Capture::default();
        let mut log = Vec::new();

        serve(
            &mut server,
            std::io::Cursor::new(format!("{initialize}\n").into_bytes()),
            output.clone(),
            &mut log,
        )
        .unwrap();

        let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        let frame: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();

        frame["result"]["serverInfo"]["version"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    assert_eq!(
        report(Server::new(NoTools, false)),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        report(
            Server::new(NoTools, false)
                .with_version(Registry::new().version("0.15.0-ext.1").version_text())
        ),
        "0.15.0-ext.1"
    );
}

#[derive(Clone, Default)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);

        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_registry_holds_the_kinds_and_the_fault_targets_of_its_one_extension() {
    let (registry, _) = registry();

    assert_eq!(registry.kind("XTEST"), Some(xtest()));
    assert_eq!(registry.kind("xtest"), None);
    assert_eq!(registry.kind("PROG"), None);
    assert_eq!(registry.kinds().count(), 1);
    assert_eq!(registry.fault_targets(), ["XTEST"]);
    assert_eq!(Registry::new().kinds().count(), 0);
    assert_eq!(Registry::new().extension().map(Extension::name), None);
}

#[test]
#[should_panic(expected = "only one extension per process")]
fn a_second_extension_is_refused_at_start_up() {
    let _ = registry().0.with(TestExtension {
        state: Shared::default(),
    });
}

/// An extension whose only variable part is one kind spec and fault target list.
struct Colliding {
    kinds: &'static [ExtensionKind],
    faults: &'static [&'static str],
}

impl Extension for Colliding {
    fn name(&self) -> &'static str {
        "colliding"
    }

    fn kinds(&self) -> &'static [ExtensionKind] {
        self.kinds
    }

    fn preflight(
        &self,
        _: ExtensionOp,
        _: ExtensionKind,
        _: &Connection,
        _: Option<Backend>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn read(&self, _: ExtensionKind, _: &ReadRequest<'_>, _: RunContext<'_>) -> Outcome {
        unreachable!()
    }

    fn create(&self, _: ExtensionKind, _: &CreateRequest<'_>, _: RunContext<'_>) -> Outcome {
        unreachable!()
    }

    fn delete_backend<'r>(
        &'r self,
        _: RunContext<'r>,
    ) -> Result<Box<dyn ExtensionDelete + 'r>, Outcome> {
        unreachable!()
    }

    fn fault_targets(&self) -> &'static [&'static str] {
        self.faults
    }
}

const fn spec(token: &'static str, label: &'static str) -> KindSpec {
    KindSpec {
        token,
        help: "",
        verbs: Verbs::READ,
        deploy_refusal: None,
        max_name: 20,
        max_text: 36,
        create_options: &[],
        delete_rank: 0,
        backend_label: label,
        plan_backend_optional: false,
        is_self: None,
        mcp_read_refusal: None,
    }
}

static STANDARD_TOKEN: KindSpec = spec("PROG", "x");
static ADT_LABEL: KindSpec = spec("YKIND", "adt");
static SADT_LABEL: KindSpec = spec("YKIND", "rfc (SADT)");
static FINE: KindSpec = spec("YKIND", "y (YKIND)");
static STANDARD_TOKEN_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&STANDARD_TOKEN)];
static ADT_LABEL_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&ADT_LABEL)];
static SADT_LABEL_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&SADT_LABEL)];
static TWICE_KINDS: [ExtensionKind; 2] = [ExtensionKind::new(&FINE), ExtensionKind::new(&FINE)];
static FINE_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&FINE)];

#[test]
#[should_panic(expected = "extension type PROG is a standard type")]
fn a_type_token_that_is_a_standard_token_is_refused() {
    let _ = Registry::new().with(Colliding {
        kinds: &STANDARD_TOKEN_KINDS,
        faults: &[],
    });
}

#[test]
#[should_panic(expected = "extension type YKIND is registered twice")]
fn a_type_token_that_repeats_is_refused() {
    let _ = Registry::new().with(Colliding {
        kinds: &TWICE_KINDS,
        faults: &[],
    });
}

#[test]
#[should_panic(expected = "uses the label of an ADT backend")]
fn the_label_of_the_adt_backend_is_refused() {
    let _ = Registry::new().with(Colliding {
        kinds: &ADT_LABEL_KINDS,
        faults: &[],
    });
}

#[test]
#[should_panic(expected = "uses the label of an ADT backend")]
fn the_label_of_the_sadt_backend_is_refused() {
    let _ = Registry::new().with(Colliding {
        kinds: &SADT_LABEL_KINDS,
        faults: &[],
    });
}

#[test]
#[should_panic(expected = "fault target PUT is a standard target")]
fn a_standard_fault_target_is_refused() {
    let _ = Registry::new().with(Colliding {
        kinds: &FINE_KINDS,
        faults: &["PUT"],
    });
}
