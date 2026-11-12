//! Live-only entry of the M6 template activation gate (plan Task 12A): one template mode through
//! the real runner cycle on a real connection. It is ignored, so `cargo test` never contacts SAP,
//! and it is reachable from nowhere else: neither the CLI nor the MCP server calls it.
//!
//! Run by hand, for example:
//! `ABAPREACH_LIVE_CONN=DEV_100 ABAPREACH_LIVE_BACKEND=rfc ABAPREACH_LIVE_TYPE=DTEL
//!  ABAPREACH_LIVE_MODE=READ ABAPREACH_LIVE_NAME=YTEST_ABAPREACH_E1
//!  cargo test --test runner_live_test -- --ignored --nocapture`
//! `ABAPREACH_LIVE_XML` (a file), `_PACKAGE`, `_TR`, `_BASE`, `_PLAN`, `_REPORT`, and `_TEXT`
//! carry the other payload values of a mode.

use abapreach::backend::BackendKind;
use abapreach::config::{Config, ConfigPaths};
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::payload::{self, Kind, Mode};
use abapreach::operation::runner::{CycleInput, PostRoute, RunnerEnd, run_cycle};
use abapreach::operation::runner_template::{TEMPLATES, generate};
use abapreach::runner_protocol::{Nonce, Parsed, RunMode, parse_post_reply};
use abapreach::{Factories, RealFactories};
use std::collections::BTreeMap;

/// The only runner prefix a live test may use (decision 30).
const TEST_PREFIX: &str = "YTEST_ABAPREACH_RUN_";

fn var(env: &BTreeMap<String, String>, key: &str) -> Option<String> {
    env.get(&format!("ABAPREACH_LIVE_{key}")).cloned()
}

fn modes(token: &str) -> (Mode, RunMode) {
    match token {
        "READ" => (Mode::Read, RunMode::Read),
        "SIMULATE" => (Mode::Simulate, RunMode::Simulate),
        "DEPLOY" => (Mode::Deploy, RunMode::Deploy),
        "CREATE_SIM" => (Mode::CreateSim, RunMode::CreateSim),
        "CREATE" => (Mode::Create, RunMode::Create),
        "DELETE_SIM" => (Mode::DeleteSim, RunMode::DeleteSim),
        "DELETE" => (Mode::Delete, RunMode::Delete),
        other => panic!("unknown mode {other}"),
    }
}

fn kind(token: &str) -> Kind {
    [Kind::Dtel, Kind::Doma, Kind::Tabl, Kind::Ttyp, Kind::Tran]
        .into_iter()
        .find(|kind| kind.token() == token)
        .unwrap_or_else(|| panic!("unknown type {token}"))
}

#[test]
#[ignore = "live only: contacts a real SAP system"]
fn one_template_mode_through_the_real_runner_cycle() {
    let env: BTreeMap<String, String> = std::env::vars()
        .filter(|(key, _)| key.starts_with("ABAPREACH_LIVE_") || key == "ABAP_CONNECTIONS")
        .collect();
    let required = |key: &str| var(&env, key).unwrap_or_else(|| panic!("ABAPREACH_LIVE_{key}"));
    let (mode, run_mode) = modes(&required("MODE"));
    let kind = kind(&required("TYPE"));
    let name = required("NAME");
    let backend = match required("BACKEND").as_str() {
        "adt" => BackendKind::AdtHttp,
        "rfc" => BackendKind::RfcSadt,
        other => panic!("unknown backend {other}"),
    };
    let config = Config::load(ConfigPaths::from_env(&env).expect("paths"), &env).expect("config");
    let connection = config.resolve(&required("CONN"), &env).expect("connection");

    // The harness asserts the effective prefix before the first cell; so does this entry.
    assert_eq!(connection.runner_prefix, TEST_PREFIX, "runner_prefix");

    let xml = var(&env, "XML").map(|path| std::fs::read_to_string(path).expect("XML file"));
    let nonce_text: String = {
        let mut bytes = [0u8; 16];

        getrandom::fill(&mut bytes).expect("random");
        bytes.iter().map(|byte| format!("{byte:02X}")).collect()
    };
    let nonce = Nonce::parse(&nonce_text).expect("nonce");
    let (package, tr, base, plan, report, text) = (
        var(&env, "PACKAGE"),
        var(&env, "TR"),
        var(&env, "BASE"),
        var(&env, "PLAN"),
        var(&env, "REPORT"),
        var(&env, "TEXT"),
    );
    let mut request = payload::Request::new(mode, kind, &name, &nonce_text);

    request.package = package.as_deref();
    request.tr = tr.as_deref();
    request.base = base.as_deref();
    request.plan = plan.as_deref();
    request.report = report.as_deref();
    request.text = text.as_deref();
    request.xml = xml.as_deref();

    let payload_lines = payload::encode(&request).expect("payload");
    let template = TEMPLATES
        .iter()
        .find(|template| template.name == kind.token())
        .unwrap_or_else(|| panic!("no template for {}", kind.token()));
    let build = |class: &str| generate(template, class, &payload_lines);
    let operation = format!("live {} {}", mode_token(&required("MODE")), kind.token());
    let input = CycleInput {
        operation: &operation,
        sid: &connection.system_id,
        client: &connection.client,
        conn: &connection.id,
        prefix: &connection.runner_prefix,
        user: &connection.user,
        language: &connection.lang,
        backend,
        rfc_available: connection.rfc.is_some(),
        build: &build,
    };
    let cancel = CancelFlag::new();
    let mut factories = RealFactories::new(cancel.clone());
    let mut open = || match backend {
        BackendKind::AdtHttp => factories.adt(&connection),
        BackendKind::RfcSadt => factories.rfc(&connection),
    };
    let run = run_cycle(&input, &mut open, PostRoute::Cycle, &cancel);

    println!("{}", run.runner_line().unwrap_or_default());

    for note in &run.notes {
        println!("{note}");
    }

    if let Some(text) = run.leftover_text() {
        panic!("{text}");
    }

    let RunnerEnd::Posted(response) = &run.end else {
        panic!("the cycle did not reach a reply: {:?}", run.end);
    };
    let body = String::from_utf8_lossy(&response.body);

    println!("{body}");

    assert!(
        matches!(
            parse_post_reply(&nonce, run_mode, response.status, &body),
            Parsed::Complete(_)
        ),
        "exactly one valid nonce-bound status line is required"
    );
}

fn mode_token(token: &str) -> String {
    token.to_ascii_lowercase()
}
