//! `run PROG` and `run SNIPPET` through the whole application against the fake SAP of the runner
//! tests: the facts and the plan, the payload and the class that reach the system, the outcome of
//! every reply of the classrun `POST`, the cleanup after each of them, the line of a snippet error,
//! the timeout of the `POST`, the second Ctrl-C, and the MCP tool. No network, no SAP.

mod common;

use abapreach::backend::BackendKind;
use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::hash::{local_source_lines, source_hash};
use abapreach::mcp::{Answer, Form, Session, ToolHandler, ToolResult};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{McpTools, TokenGate};
use abapreach::operation::deploy::{CancelFlag, InterruptAction};
use abapreach::operation::run_runner::{RUNNER_WAIT_TEXT, expiry, unix_now, utc_stamp};
use abapreach::operation::runner_template::{OutType, generate_snippet};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use common::preview;
use common::runner_fake::{PostReply, Shared, World, http_session, new_sap, rfc_session};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

const SOURCE: &str = "REPORT zreport.\nPARAMETERS p_a TYPE c LENGTH 5.\nWRITE p_a.\n";
const OTHER_SOURCE: &str = "REPORT zreport.\nWRITE 'changed'.\n";
const PROGRAM_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><program:abapProgram xmlns:program="http://www.sap.com/adt/programs/programs" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:responsible="DEVUSER" adtcore:version="active" adtcore:name="ZREPORT"><adtcore:packageRef adtcore:name="ZPKG"/></program:abapProgram>"#;
const WAIT: &str = RUNNER_WAIT_TEXT;
/// The snippet of most tests: two statements, three lines.
const SNIPPET: &str = "DATA(lv_x) = 1.\nprint( |x = { lv_x }| ).\nprint( `done` ).\n";

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

// ---- the fake SAP: the runner fake plus the program and the activation messages -----------------

#[derive(Default)]
struct Front {
    /// The source of the report the facts read; `None` is a report that does not exist.
    source: Option<String>,
    subc: String,
    category: String,
    /// The activation of the first runner class answers this error: `(row, text)`.
    activation_error: Option<(u32, String)>,
    /// `(uri, limit)` of every request sent with a per-request limit.
    limits: Vec<(String, Duration)>,
    /// How many Ctrl-C arrive while the classrun `POST` is in flight, and what they were told.
    interrupts: usize,
    told: Vec<InterruptAction>,
    leftover: Option<String>,
    cancel: CancelFlag,
}

type Shell = Rc<RefCell<Front>>;

struct FrontSession {
    inner: Box<dyn SadtTransport>,
    front: Shell,
}

impl FrontSession {
    fn answer(&mut self, request: &Request) -> Option<Response> {
        let mut front = self.front.borrow_mut();
        let (path, _) = request
            .uri
            .split_once('?')
            .unwrap_or((request.uri.as_str(), ""));
        let body = String::from_utf8_lossy(&request.body).into_owned();

        if request.method == "GET" && path.starts_with("/sap/bc/adt/programs/programs/") {
            return Some(match &front.source {
                Some(source) if path.ends_with("/source/main") => Response::text(200, source),
                Some(_) => Response::text(200, PROGRAM_XML),
                None => Response::text(404, ""),
            });
        }

        if request.method == "POST" && path == "/sap/bc/adt/datapreview/ddic" {
            if body.contains("FROM TRDIR") {
                return Some(Response::text(
                    200,
                    preview(&[("SUBC", &[Some(front.subc.as_str())])]),
                ));
            }

            if body.contains("FROM T000") && body.contains("SELECT CCCATEGORY FROM") {
                return Some(Response::text(
                    200,
                    preview(&[("CCCATEGORY", &[Some(front.category.as_str())])]),
                ));
            }
        }

        if request.method == "POST"
            && path == "/sap/bc/adt/activation"
            && let Some((row, text)) = front.activation_error.take()
        {
            return Some(Response::text(
                200,
                format!(
                    r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="{row}"><shortText><txt>{text}</txt></shortText></msg></chkl:messages>"#
                ),
            ));
        }

        if request.method == "POST" && path.starts_with("/sap/bc/adt/oo/classrun/") {
            front.leftover = front.cancel.run_leftover();

            for _ in 0..front.interrupts {
                let told = front.cancel.on_interrupt();

                front.told.push(told);
            }
        }

        None
    }
}

impl SadtTransport for FrontSession {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        match self.answer(&request) {
            Some(response) => Ok(response),
            None => self.inner.request(request),
        }
    }

    fn request_within(
        &mut self,
        request: Request,
        limit: Duration,
    ) -> Result<Response, TransportError> {
        self.front
            .borrow_mut()
            .limits
            .push((request.uri.clone(), limit));

        match self.answer(&request) {
            Some(response) => Ok(response),
            None => self.inner.request_within(request, limit),
        }
    }

    fn supports_request_timeout(&self) -> bool {
        self.inner.supports_request_timeout()
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.inner.stateful_supported()
    }
}

struct Factory {
    sap: Shared,
    front: Shell,
    adt_opened: usize,
    rfc_opened: usize,
    cancel: CancelFlag,
}

impl Factories for Factory {
    fn adt(&mut self, _: &Connection) -> Result<Box<dyn SadtTransport>, TransportError> {
        self.adt_opened += 1;

        Ok(Box::new(FrontSession {
            inner: http_session(&self.sap)?,
            front: self.front.clone(),
        }))
    }

    fn rfc(&mut self, _: &Connection) -> Result<Box<dyn SadtTransport>, TransportError> {
        self.rfc_opened += 1;

        Ok(Box::new(FrontSession {
            inner: rfc_session(&self.sap)?,
            front: self.front.clone(),
        }))
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        Err("no library in this test".to_owned())
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        Ok(())
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

struct Rig {
    sap: Shared,
    factory: Factory,
    directory: tempfile::TempDir,
}

fn rig() -> Rig {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let sap = new_sap(
        true,
        &now,
        Box::new(|_| World {
            definition: true,
            directory: true,
            ..World::default()
        }),
    );
    let cancel = CancelFlag::new();
    let front = Rc::new(RefCell::new(Front {
        source: Some(SOURCE.to_owned()),
        subc: "1".to_owned(),
        category: "C".to_owned(),
        cancel: cancel.clone(),
        ..Front::default()
    }));

    Rig {
        factory: Factory {
            sap: sap.clone(),
            front,
            adt_opened: 0,
            rfc_opened: 0,
            cancel,
        },
        sap,
        directory: tempfile::tempdir().unwrap(),
    }
}

fn env(directory: &Path, policies: &str, rfc: bool) -> BTreeMap<String, String> {
    let path = directory.join("connections.json");
    let rfc_block = if rfc {
        r#","rfc":{"ashost":"example.com","sysnr":"00"}"#
    } else {
        ""
    };
    let policies = if policies.is_empty() {
        String::new()
    } else {
        format!(r#","policies":{policies}"#)
    };

    fs::write(
        &path,
        format!(
            r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"runner_prefix":"YTEST_ABAPREACH_RUN_"{rfc_block}{policies}}}]}}"#
        ),
    )
    .unwrap();

    BTreeMap::from([
        ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

impl Rig {
    fn plan(&self) -> String {
        self.directory
            .path()
            .join("plan.json")
            .display()
            .to_string()
    }

    fn snippet_file(&self, text: &str) -> String {
        let path = self.directory.path().join("snippet.abap");

        fs::write(&path, text).unwrap();

        path.display().to_string()
    }

    fn run(&mut self, policies: &str, rfc: bool, args: &[&str]) -> Exit {
        let mut full = vec!["abapreach".to_owned(), "run".to_owned()];

        full.extend(args.iter().map(|arg| (*arg).to_owned()));
        full.extend(["--conn".to_owned(), "DEV_100".to_owned()]);

        run(
            full,
            env(self.directory.path(), policies, rfc),
            &mut self.factory,
        )
    }

    fn front(&self) -> std::cell::RefMut<'_, Front> {
        self.factory.front.borrow_mut()
    }

    /// The reply of every classrun `POST` that has no scripted one: the protocol lines for the
    /// nonce in the payload of the class that runs.
    fn answer(&self, mode: &'static str, body: Vec<String>, status: &'static str) {
        self.sap.borrow_mut().reply_with = Some(Box::new(move |source| {
            let nonce = payload(source)
                .into_iter()
                .find_map(|line| line.strip_prefix("NONCE=").map(str::to_owned))
                .expect("a nonce in the payload");

            output(&nonce, mode, &body, status)
        }));
    }

    fn classes(&self) -> usize {
        self.sap.borrow().classes.len()
    }

    fn posts(&self) -> usize {
        self.sap.borrow().posts
    }

    fn payload(&self) -> Vec<String> {
        payload(self.sap.borrow().posted.last().expect("a class that ran"))
    }
}

fn base64_decode(text: &str) -> Vec<u8> {
    let value = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("byte {c} outside the base64 alphabet"),
        }
    };
    let mut out = Vec::new();

    for quad in text.as_bytes().chunks(4) {
        let pad = quad.iter().filter(|&&c| c == b'=').count();
        let mut acc = 0u32;

        for &c in &quad[..4 - pad] {
            acc = (acc << 6) | value(c);
        }

        acc <<= 6 * pad as u32;
        out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8][..3 - pad]);
    }

    out
}

/// The lines of the payload document inside a runner class source.
fn payload(source: &str) -> Vec<String> {
    let literal: String = source
        .lines()
        .filter_map(|line| {
            line.strip_prefix("    lv_b64 = lv_b64 && '")
                .and_then(|rest| rest.strip_suffix("'."))
        })
        .collect();

    String::from_utf8(base64_decode(&literal))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn output(nonce: &str, mode: &str, body: &[String], status: &str) -> String {
    let line = |rest: &str| format!("ABAPREACH:{nonce}:{rest}");
    let mut lines = vec![line(&format!("BEGIN:{mode}"))];

    lines.extend(body.iter().map(|rest| line(rest)));

    if !status.is_empty() {
        lines.push(line(&format!("STATUS:{status}")));
    }

    let count = lines.len();

    lines.push(line(&format!("END:{count}")));

    lines.iter().map(|text| format!("{text}\n\n")).collect()
}

fn list(text: &str) -> String {
    format!("LINE:{}", hex(text))
}

fn msg(level: &str, text: &str) -> String {
    format!("MSG:{level}:{}", hex(text))
}

fn col(visible: bool, name: &str, text: &str) -> String {
    format!("ALVCOL:{}:{}:{}", u8::from(visible), hex(name), hex(text))
}

fn row(cells: &[&str]) -> String {
    format!(
        "ALVROW:{}",
        cells
            .iter()
            .map(|cell| hex(cell))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// The simulate of a report with parameters, and the real run bound to its plan.
fn simulated_report(rig: &mut Rig) -> Exit {
    let plan = rig.plan();

    rig.run(
        "",
        false,
        &[
            "PROG",
            "zreport",
            "--param",
            "p_a=7",
            "--simulate",
            "--plan",
            &plan,
        ],
    )
}

#[test]
fn the_payload_expires_after_the_timeout_plus_a_grace_over_http_and_a_fixed_span_over_rfc() {
    assert_eq!(utc_stamp(0), "19700101000000");
    assert_eq!(utc_stamp(1_700_000_000), "20231114221320");
    // A leap day, and the first second of a year.
    assert_eq!(utc_stamp(951_782_400), "20000229000000");
    assert_eq!(utc_stamp(1_767_225_600), "20260101000000");
    assert_eq!(utc_stamp(1_798_761_599), "20261231235959");

    let start = 1_700_000_000;

    // 600 s timeout and 20 minutes of grace: 30 minutes.
    assert_eq!(
        expiry(start, BackendKind::AdtHttp, 600),
        utc_stamp(start + 1800)
    );
    assert_eq!(
        expiry(start, BackendKind::AdtHttp, 3600),
        utc_stamp(start + 3600 + 1200)
    );
    // RFC has no timeout: 75 minutes.
    assert_eq!(
        expiry(start, BackendKind::RfcSadt, 600),
        utc_stamp(start + 75 * 60)
    );
}

// ---- run PROG ---------------------------------------------------------------------------------------

#[test]
fn a_report_simulates_then_runs_and_prints_its_grid_and_its_list() {
    let mut rig = rig();
    let simulated = simulated_report(&mut rig);

    assert_eq!(simulated.code, 0, "{}", simulated.stderr);
    assert!(
        simulated
            .stdout
            .starts_with("SIMULATED_OK run PROG ZREPORT package ZPKG author DEVUSER\n"),
        "{}",
        simulated.stdout
    );

    let hash = source_hash(&local_source_lines(SOURCE));

    for line in [
        format!("HASH {hash}"),
        "SUBC 1".to_owned(),
        "PARAM P_A=7".to_owned(),
        "ALV on columns visible".to_owned(),
        "CLIENT 100 (not productive)".to_owned(),
        "BACKEND adt".to_owned(),
        "OUTPUT LIMIT 6291456 bytes".to_owned(),
    ] {
        assert!(
            simulated.stdout.lines().any(|candidate| candidate == line),
            "{line}\n{}",
            simulated.stdout
        );
    }

    // The simulate runs nothing and creates nothing.
    assert_eq!(rig.posts(), 0);
    assert_eq!(rig.classes(), 0);
    assert!(
        rig.sap
            .borrow()
            .log
            .iter()
            .all(|(_, request)| !request.starts_with("POST /sap/bc/adt/oo/classes")),
        "no class was created"
    );

    rig.answer(
        "RUN_PROG",
        vec![
            col(true, "CARRID", "Airline"),
            col(false, "MANDT", "Client"),
            col(true, "PRICE", "Price"),
            row(&["LH", "100", "12.50"]),
            list("a list line"),
            msg("W", "output cut after 2 lines"),
        ],
        "RAN",
    );

    let plan = rig.plan();
    let ran = rig.run(
        "",
        false,
        &["PROG", "zreport", "--param", "p_a=7", "--plan", &plan],
    );

    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, "Airline\tPrice\nLH\t12.50\na list line");
    assert!(
        ran.stderr.contains("RUNNER: YTEST_ABAPREACH_RUN_"),
        "{}",
        ran.stderr
    );
    assert!(
        ran.stderr.contains("W: output cut after 2 lines"),
        "{}",
        ran.stderr
    );
    for token in [
        "RUN_FAILED",
        "RUN_UNKNOWN",
        "REJECTED",
        "RUNNER_LEFTOVER",
        "ERROR",
    ] {
        assert!(!ran.stderr.contains(token), "{token}: {}", ran.stderr);
    }

    // One POST, never repeated, and no class is left.
    assert_eq!(rig.posts(), 1);
    assert_eq!(rig.classes(), 0);

    let document = rig.payload();
    let nonce = document
        .iter()
        .find_map(|line| line.strip_prefix("NONCE="))
        .unwrap();

    assert_eq!(nonce.len(), 32);
    assert!(document.contains(&"MODE=RUN_PROG".to_owned()));
    assert!(document.contains(&"USER=USER".to_owned()));
    assert!(document.contains(&"REPORT=ZREPORT".to_owned()));
    assert!(document.contains(&format!("BASE={hash}")));
    assert!(document.contains(&"PARAM=P_A=7".to_owned()));
    assert!(document.contains(&"MAXBYTES=6291456".to_owned()));
    assert!(document.contains(&"ALV=on".to_owned()));
    assert!(document.contains(&"ALVCOLS=visible".to_owned()));
    assert!(!document.iter().any(|line| line.starts_with("VARIANT=")));

    let expires = document
        .iter()
        .find_map(|line| line.strip_prefix("EXPIRES="))
        .unwrap();

    assert_eq!(expires.len(), 14);
    assert!(expires.bytes().all(|byte| byte.is_ascii_digit()));

    // The class that ran holds the generated SUBMIT and no user value outside the payload.
    let source = rig.sap.borrow().posted.last().unwrap().clone();

    assert!(source.contains(
        "SUBMIT (mv_report) WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN."
    ));
    assert!(
        !source.contains("zreport"),
        "the report name stands in the payload only"
    );
    assert!(!source.to_ascii_lowercase().contains("p_a=7"));

    // Over ADT HTTP the POST has its own limit: 600 s by default.
    let limits = rig.front().limits.clone();

    assert!(
        limits.iter().any(|(uri, limit)| uri.contains("/oo/classrun/") && *limit == Duration::from_secs(600)),
        "{limits:?}"
    );
}

#[test]
fn the_timeout_is_per_request_on_http_and_the_all_columns_choice_reaches_the_class() {
    let mut rig = rig();
    let plan = rig.plan();

    rig.run(
        "",
        false,
        &[
            "PROG",
            "zreport",
            "--alv-columns",
            "all",
            "--simulate",
            "--plan",
            &plan,
        ],
    );
    rig.answer("RUN_PROG", vec![], "RAN");

    let exit = rig.run(
        "",
        false,
        &[
            "PROG",
            "zreport",
            "--alv-columns",
            "all",
            "--timeout",
            "45",
            "--max-output",
            "5000",
            "--plan",
            &plan,
        ],
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);

    let limits = rig.front().limits.clone();

    assert!(
        limits
            .iter()
            .any(|(uri, limit)| uri.contains("/oo/classrun/") && *limit == Duration::from_secs(45)),
        "{limits:?}"
    );
    assert!(rig.payload().contains(&"ALVCOLS=all".to_owned()));
    assert!(rig.payload().contains(&"MAXBYTES=5000".to_owned()));

    // The expiry follows the timeout: the grace is 20 minutes over ADT HTTP.
    let expires = rig
        .payload()
        .iter()
        .find_map(|line| line.strip_prefix("EXPIRES=").map(str::to_owned))
        .unwrap();
    let now = utc_stamp(unix_now());

    assert!(expires > now, "{expires} {now}");
    assert!(expires < utc_stamp(unix_now() + 45 + 1200 + 5), "{expires}");

    // `--alv off` keeps the list only and takes no columns.
    rig.answer("RUN_PROG", vec![list("x")], "RAN");

    let plan_off = rig.directory.path().join("off.json").display().to_string();
    let off = ["PROG", "zreport", "--alv", "off"];
    let mut simulate = off.to_vec();

    simulate.extend(["--simulate", "--plan", &plan_off]);
    assert_eq!(rig.run("", false, &simulate).code, 0);

    let mut execute = off.to_vec();

    execute.extend(["--plan", &plan_off]);
    assert_eq!(rig.run("", false, &execute).stdout, "x");
    assert!(rig.payload().contains(&"ALV=off".to_owned()));
}

#[test]
fn a_plan_binds_the_report_its_source_its_type_its_package_and_every_option() {
    let mut rig = rig();

    assert_eq!(simulated_report(&mut rig).code, 0);

    let plan = rig.plan();

    // Arguments that differ from the simulate stop before any connection.
    for (extra, field) in [
        (vec!["--param", "p_a=8"], "--param"),
        (vec!["--param", "p_a=7", "--variant", "v1"], "--variant"),
        (vec!["--param", "p_a=7", "--alv", "off"], "--alv"),
        (
            vec!["--param", "p_a=7", "--alv-columns", "all"],
            "--alv-columns",
        ),
        (vec![], "--param"),
    ] {
        let mut args = vec!["PROG", "zreport"];

        args.extend(extra);
        args.extend(["--plan", &plan]);

        let opened = rig.factory.adt_opened;
        let exit = rig.run("", false, &args);

        assert_eq!(exit.code, 2, "{field}: {}", exit.stderr);
        assert_eq!(
            exit.stderr,
            format!("STOP: plan does not match the arguments ({field}); simulate again")
        );
        assert_eq!(rig.factory.adt_opened, opened, "no connection for {field}");
    }

    let other = rig.run(
        "",
        false,
        &["PROG", "zother", "--param", "p_a=7", "--plan", &plan],
    );

    assert!(other.stderr.contains("(name)"), "{}", other.stderr);

    // The source changed after the simulate: refused with nothing created.
    rig.front().source = Some(OTHER_SOURCE.to_owned());

    let changed = rig.run(
        "",
        false,
        &["PROG", "zreport", "--param", "p_a=7", "--plan", &plan],
    );

    assert_eq!(changed.code, 1);
    assert_eq!(
        changed.stderr,
        "REJECTED: plan changed since simulate (source hash); simulate again"
    );
    assert_eq!(rig.posts(), 0);
    assert!(
        rig.sap
            .borrow()
            .log
            .iter()
            .all(|(_, request)| !request.starts_with("POST /sap/bc/adt/oo/classes")),
        "no class was created"
    );

    // The type of the report changed: a module pool is not bound to the plan of a report.
    rig.front().source = Some(SOURCE.to_owned());
    rig.front().subc = "M".to_owned();

    let pool = rig.run(
        "",
        false,
        &["PROG", "zreport", "--param", "p_a=7", "--plan", &plan],
    );

    assert_eq!(pool.code, 1);
    assert!(
        pool.stderr.contains("not an executable program (SUBC=M)"),
        "{}",
        pool.stderr
    );
}

#[test]
fn the_facts_that_refuse_a_run_come_before_anything_is_created() {
    let mut rig = rig();
    let plan = rig.plan();

    // A report that does not exist, a standard report, a productive client.
    rig.front().source = None;

    let missing = rig.run(
        "",
        false,
        &["PROG", "zreport", "--simulate", "--plan", &plan],
    );

    assert_eq!(missing.code, 1);
    assert_eq!(missing.stderr, "REJECTED: report ZREPORT does not exist");

    rig.front().source = Some(SOURCE.to_owned());
    rig.front().category = "P".to_owned();

    let productive = rig.run(
        "",
        false,
        &["PROG", "zreport", "--simulate", "--plan", &plan],
    );

    assert_eq!(productive.code, 1);
    assert!(
        productive.stderr.contains("is productive"),
        "{}",
        productive.stderr
    );
    assert_eq!(rig.classes(), 0);
    assert_eq!(rig.posts(), 0);

    let standard = rig.run(
        "",
        false,
        &["PROG", "rsparam", "--simulate", "--plan", &plan],
    );

    assert_eq!(standard.code, 2);
    assert!(
        standard.stderr.contains("outside the Z/Y scope"),
        "{}",
        standard.stderr
    );
}

/// The outcome of one reply of the classrun `POST`, and what stays true after each of them.
#[test]
fn every_reply_of_the_post_maps_to_its_token_exit_code_and_a_clean_system() {
    let cases: Vec<(&str, PostReply, i32, Vec<&str>, &str)> = vec![
        (
            "a report that ran",
            PostReply::Output(String::new()),
            0,
            vec![],
            "line one",
        ),
        (
            "a runtime error body",
            PostReply::Status(
                500,
                "<html>ABAP runtime error DYNPRO_SEND_IN_BACKGROUND; see ST22</html>".to_owned(),
            ),
            1,
            vec!["RUN_FAILED: runtime error; see ST22 (dumps)"],
            "",
        ),
        (
            "an ICM timeout",
            PostReply::Status(
                503,
                "ICM timeout while waiting for the application".to_owned(),
            ),
            1,
            vec![
                "RUN_UNKNOWN: the reply was lost; the code may have run",
                "HTTP 503",
            ],
            "",
        ),
        (
            "a 500 that is not a runtime error",
            PostReply::Status(500, "internal server error".to_owned()),
            1,
            vec!["RUN_UNKNOWN: the reply was lost; the code may have run"],
            "",
        ),
        (
            "a lost reply",
            PostReply::Lost,
            1,
            vec!["RUN_UNKNOWN: the reply was lost; the code may have run"],
            "",
        ),
        (
            "a 200 without a status line",
            PostReply::Output(
                "ABAPREACH:00000000000000000000000000000000:BEGIN:RUN_PROG\n".to_owned(),
            ),
            1,
            vec!["RUN_UNKNOWN: the reply was lost; the code may have run"],
            "",
        ),
        (
            "a refused POST",
            PostReply::Status(405, "method not allowed".to_owned()),
            1,
            vec!["REJECTED: the classrun POST was refused: HTTP 405; the code did not run"],
            "",
        ),
        (
            "no classrun",
            PostReply::Status(404, String::new()),
            2,
            vec!["STOP: run PROG needs ADT classrun; not available on DEV"],
            "",
        ),
    ];

    for (label, reply, exit_code, expected, stdout) in cases {
        let mut rig = rig();

        simulated_report(&mut rig);

        if label == "a report that ran" {
            rig.answer("RUN_PROG", vec![list("line one")], "RAN");
        } else {
            rig.sap.borrow_mut().post_replies = vec![reply].into();
        }

        let plan = rig.plan();
        let exit = rig.run(
            "",
            false,
            &["PROG", "zreport", "--param", "p_a=7", "--plan", &plan],
        );

        assert_eq!(exit.code, exit_code, "{label}: {}", exit.stderr);
        assert_eq!(exit.stdout, stdout, "{label}");

        for text in expected {
            assert!(exit.stderr.contains(text), "{label}: {}", exit.stderr);
        }

        // The POST ran once and is never sent again; the runner class is always removed.
        assert_eq!(rig.posts(), 1, "{label}");
        assert_eq!(rig.classes(), 0, "{label}");
        assert!(!exit.stderr.contains("RUNNER_LEFTOVER"), "{label}");
    }
}

#[test]
fn the_class_can_report_a_refusal_or_a_failure_and_its_lines_still_print() {
    /// What the class sends and what the user sees of it.
    struct Case {
        body: Vec<String>,
        status: &'static str,
        token: &'static str,
        stdout: &'static str,
    }

    let cases = [
        Case {
            body: vec![msg("E", "report changed since the simulate")],
            status: "REJECTED",
            token: "REJECTED: report changed since the simulate",
            stdout: "",
        },
        Case {
            body: vec![msg(
                "E",
                "parameter P_X is not on the selection screen of report ZREPORT",
            )],
            status: "REJECTED",
            token: "REJECTED: parameter P_X is not on the selection screen of report ZREPORT",
            stdout: "",
        },
        Case {
            body: vec![
                list("before"),
                msg("E", "exception CX_SY_ZERODIVISION: Division by zero"),
            ],
            status: "RUN_FAILED",
            token: "RUN_FAILED: exception CX_SY_ZERODIVISION: Division by zero",
            stdout: "before",
        },
    ];

    for case in cases {
        let mut rig = rig();

        simulated_report(&mut rig);
        rig.answer("RUN_PROG", case.body, case.status);

        let plan = rig.plan();
        let exit = rig.run(
            "",
            false,
            &["PROG", "zreport", "--param", "p_a=7", "--plan", &plan],
        );

        assert_eq!(exit.code, 1, "{}", exit.stderr);
        assert!(exit.stderr.contains(case.token), "{}", exit.stderr);
        assert_eq!(exit.stdout, case.stdout);
        assert_eq!(rig.classes(), 0);
    }
}

#[test]
fn over_rfc_the_post_has_no_limit_and_its_failures_have_their_own_tokens() {
    let cases = [
        (
            PostReply::Lost,
            "RUN_UNKNOWN: the reply was lost; the code may have run",
        ),
        (
            PostReply::Error(
                TransportError::Protocol("call failed (RFC code 3)".to_owned()).after_send(),
            ),
            "RUN_FAILED: runtime error; see ST22 (dumps)",
        ),
        // Any other ABAP exception of the call is not a runtime error.
        (
            PostReply::Error(
                TransportError::Protocol("call failed (RFC code 5)".to_owned()).after_send(),
            ),
            "RUN_UNKNOWN: the reply was lost; the code may have run",
        ),
    ];

    for (reply, token) in cases {
        let mut rig = rig();
        let plan = rig.plan();

        assert_eq!(
            rig.run(
                "",
                true,
                &[
                    "PROG",
                    "zreport",
                    "--backend",
                    "rfc",
                    "--simulate",
                    "--plan",
                    &plan
                ]
            )
            .code,
            0
        );

        rig.sap.borrow_mut().post_replies = vec![reply].into();

        let exit = rig.run(
            "",
            true,
            &["PROG", "zreport", "--backend", "rfc", "--plan", &plan],
        );

        assert_eq!(exit.code, 1, "{}", exit.stderr);
        assert!(exit.stderr.contains(token), "{}", exit.stderr);
        assert_eq!(rig.posts(), 1);
        assert_eq!(rig.classes(), 0);
        // RFC has no call timeout: nothing was sent with a limit.
        assert!(rig.front().limits.is_empty(), "{:?}", rig.front().limits);
        assert!(
            rig.payload()
                .iter()
                .any(|line| line.starts_with("EXPIRES="))
        );
    }

    // The expiry over RFC is 75 minutes after the start, not the timeout plus a grace.
    let mut rig = rig();
    let plan = rig.plan();

    rig.run(
        "",
        true,
        &[
            "PROG",
            "zreport",
            "--backend",
            "rfc",
            "--simulate",
            "--plan",
            &plan,
        ],
    );
    rig.answer("RUN_PROG", vec![], "RAN");
    rig.run(
        "",
        true,
        &["PROG", "zreport", "--backend", "rfc", "--plan", &plan],
    );

    let expires = rig
        .payload()
        .iter()
        .find_map(|line| line.strip_prefix("EXPIRES=").map(str::to_owned))
        .unwrap();
    let start = unix_now();

    assert!(expires >= utc_stamp(start + 75 * 60 - 60), "{expires}");
    assert!(expires <= utc_stamp(start + 75 * 60 + 5), "{expires}");

    let timeout = rig.run(
        "",
        true,
        &[
            "PROG",
            "zreport",
            "--backend",
            "rfc",
            "--timeout",
            "30",
            "--simulate",
            "--plan",
            &plan,
        ],
    );

    assert_eq!(timeout.code, 2);
    assert!(
        timeout
            .stderr
            .contains("--timeout is not supported over RFC"),
        "{}",
        timeout.stderr
    );
}

#[test]
fn the_first_ctrl_c_waits_and_the_second_leaves_the_class_with_its_cleanup_commands() {
    let mut rig = rig();

    simulated_report(&mut rig);
    rig.front().interrupts = 2;
    rig.answer("RUN_PROG", vec![list("late")], "RAN");

    let plan = rig.plan();
    let exit = rig.run(
        "",
        false,
        &["PROG", "zreport", "--param", "p_a=7", "--plan", &plan],
    );
    let told = rig.front().told.clone();
    let leftover = rig
        .front()
        .leftover
        .clone()
        .expect("a leftover text while the POST runs");

    // The handler of the process prints the first text and ends the process on the second.
    assert_eq!(
        told,
        [InterruptAction::Waiting(WAIT), InterruptAction::ExitNow]
    );
    assert!(WAIT.contains("Ctrl-C again to leave the runner class behind"));
    assert!(
        leftover.starts_with("RUNNER_LEFTOVER: YTEST_ABAPREACH_RUN_"),
        "{leftover}"
    );
    assert!(
        leftover.contains("abapreach delete --simulate CLAS:YTEST_ABAPREACH_RUN_"),
        "{leftover}"
    );
    assert!(leftover.contains("--conn DEV_100"), "{leftover}");
    // In this test the process does not end: the run is over, the leftover window is closed, and
    // the exit of an interrupted run is 130.
    assert_eq!(exit.code, 130, "{}", exit.stderr);
    assert!(rig.factory.cancel.run_leftover().is_none());
}

// ---- run SNIPPET ------------------------------------------------------------------------------------

#[test]
fn a_snippet_shows_its_code_in_the_simulate_and_prints_through_its_own_method() {
    let mut rig = rig();
    let file = rig.snippet_file(SNIPPET);
    let plan = rig.plan();
    let simulated = rig.run(
        "",
        false,
        &["SNIPPET", &file, "--simulate", "--plan", &plan],
    );

    assert_eq!(simulated.code, 0, "{}", simulated.stderr);
    assert!(
        simulated
            .stdout
            .starts_with("SIMULATED_OK run SNIPPET 3 lines\n"),
        "{}",
        simulated.stdout
    );
    assert!(
        simulated.stdout.contains(
            "SNIPPET:\n  DATA(lv_x) = 1.\n  print( |x = { lv_x }| ).\n  print( `done` ).\n"
        ),
        "{}",
        simulated.stdout
    );
    assert_eq!(rig.posts(), 0);

    rig.answer("RUN_SNIPPET", vec![list("x = 1"), list("done")], "RAN");

    let exit = rig.run("", false, &["SNIPPET", &file, "--plan", &plan]);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(exit.stdout, "x = 1\ndone");
    assert_eq!(rig.posts(), 1);
    assert_eq!(rig.classes(), 0);

    let source = rig.sap.borrow().posted.last().unwrap().clone();
    let document = rig.payload();

    // The snippet is code in the class, the payload holds no code and no report.
    assert!(
        source.contains("DATA(lv_x) = 1.\n    print( |x = { lv_x }| )")
            || source.contains("DATA(lv_x) = 1.\nprint( |x = { lv_x }| ).")
    );
    assert!(document.contains(&"MODE=RUN_SNIPPET".to_owned()));
    assert!(
        !document
            .iter()
            .any(|line| line.starts_with("REPORT=") || line.starts_with("BASE="))
    );
    assert!(document.iter().any(|line| line.starts_with("EXPIRES=")));
    assert!(document.contains(&"USER=USER".to_owned()));

    // A changed snippet is a different plan; the check is before any connection.
    let other = rig.snippet_file("print( `other` ).\n");
    let opened = rig.factory.adt_opened;
    let mismatch = rig.run("", false, &["SNIPPET", &other, "--plan", &plan]);

    assert_eq!(mismatch.code, 2);
    assert_eq!(
        mismatch.stderr,
        "STOP: plan does not match the arguments (snippet); simulate again"
    );
    assert_eq!(rig.factory.adt_opened, opened);
}

#[test]
fn a_snippet_that_could_leave_its_method_stops_before_any_connection() {
    let mut rig = rig();
    let plan = rig.plan();

    for text in [
        "x = 1.\nENDMETHOD.\n",
        "a = 1. INCLUDE: zfoo, zbar.\n",
        "x = 1.\nCLASS lcl DEFINITION.\n",
        "x = 1\n",
    ] {
        let file = rig.snippet_file(text);
        let exit = rig.run(
            "",
            false,
            &["SNIPPET", &file, "--simulate", "--plan", &plan],
        );

        assert_eq!(exit.code, 2, "{text:?}: {}", exit.stderr);
        assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    }

    assert_eq!(rig.factory.adt_opened, 0);
}

#[test]
fn a_syntax_error_in_the_snippet_names_its_own_line_and_nothing_runs() {
    let mut rig = rig();
    let file = rig.snippet_file(SNIPPET);
    let plan = rig.plan();

    assert_eq!(
        rig.run(
            "",
            false,
            &["SNIPPET", &file, "--simulate", "--plan", &plan]
        )
        .code,
        0
    );

    let made = generate_snippet(
        "YTEST_ABAPREACH_RUN_AAAAAAAA",
        &["QUJD".to_owned()],
        SNIPPET,
        OutType::Internal,
    )
    .unwrap();

    // The error stands on the second line of the snippet.
    rig.front().activation_error = Some((
        u32::try_from(made.first_line + 1).unwrap(),
        "\"LV_Y\" is not defined".replace('"', "&quot;"),
    ));

    let exit = rig.run("", false, &["SNIPPET", &file, "--plan", &plan]);

    assert_eq!(exit.code, 2, "{}", exit.stderr);
    assert!(
        exit.stderr
            .contains("SNIPPET_ERROR: line 2: \"LV_Y\" is not defined"),
        "{}",
        exit.stderr
    );
    assert!(
        !exit.stderr.contains("STOP: SNIPPET_ERROR"),
        "{}",
        exit.stderr
    );
    assert!(
        !exit.stderr.contains("please report"),
        "not a bug of the product: {}",
        exit.stderr
    );
    assert!(
        !exit.stderr.to_ascii_lowercase().contains("classrun"),
        "not read as needing classrun: {}",
        exit.stderr
    );
    // Nothing ran, and the cleanup ran.
    assert_eq!(rig.posts(), 0);
    assert_eq!(rig.classes(), 0);

    // An error outside the snippet is a bug of the template, never the user's.
    rig.front().activation_error = Some((3, "frame broken".to_owned()));

    let frame = rig.run("", false, &["SNIPPET", &file, "--plan", &plan]);

    assert_eq!(frame.code, 1, "{}", frame.stderr);
    assert!(
        frame.stderr.contains("please report this as a bug"),
        "{}",
        frame.stderr
    );
    assert!(!frame.stderr.contains("SNIPPET_ERROR"), "{}", frame.stderr);
    assert_eq!(rig.classes(), 0);
}

#[test]
fn a_snippet_exception_is_run_failed_with_the_text_of_the_exception() {
    let mut rig = rig();
    let file = rig.snippet_file(SNIPPET);
    let plan = rig.plan();

    rig.run(
        "",
        false,
        &["SNIPPET", &file, "--simulate", "--plan", &plan],
    );
    rig.answer(
        "RUN_SNIPPET",
        vec![
            list("x = 1"),
            msg("E", "exception CX_SY_ZERODIVIDE: Division by zero"),
        ],
        "RUN_FAILED",
    );

    let exit = rig.run("", false, &["SNIPPET", &file, "--plan", &plan]);

    assert_eq!(exit.code, 1);
    assert_eq!(exit.stdout, "x = 1");
    assert!(
        exit.stderr
            .contains("RUN_FAILED: exception CX_SY_ZERODIVIDE: Division by zero"),
        "{}",
        exit.stderr
    );
    assert!(exit.stderr.contains("rolled back"), "{}", exit.stderr);
    assert_eq!(rig.classes(), 0);
}

#[test]
fn a_direct_policy_runs_without_a_plan_and_off_refuses() {
    let mut rig = rig();
    let file = rig.snippet_file(SNIPPET);

    rig.answer("RUN_SNIPPET", vec![list("hello")], "RAN");

    let direct = rig.run(r#"{"run":"direct"}"#, false, &["SNIPPET", &file]);

    assert_eq!(direct.code, 0, "{}", direct.stderr);
    assert_eq!(direct.stdout, "hello");

    let off = rig.run(r#"{"run":"off"}"#, false, &["SNIPPET", &file]);

    assert_eq!(off.code, 2);
    assert!(off.stderr.contains("policy run"), "{}", off.stderr);

    // Standard names stay refused for a snippet: it has no name to allow.
    let flag = rig.run(
        r#"{"run":"direct"}"#,
        false,
        &["SNIPPET", &file, "--allow-standard"],
    );

    assert_eq!(flag.code, 2);
}

// ---- the MCP tool ---------------------------------------------------------------------------------------

struct Human {
    typed: Option<String>,
    forms: Vec<Form>,
    cancel: CancelFlag,
}

impl Session for Human {
    fn can_elicit(&self) -> bool {
        true
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }

    fn elicit(
        &mut self,
        form: &Form,
        _: Duration,
        _: &mut dyn FnMut(&str, &Map<String, Value>) -> ToolResult,
    ) -> Answer {
        self.forms.push(form.clone());

        match &self.typed {
            Some(text) => Answer::Accepted(text.clone()),
            None => Answer::Declined,
        }
    }

    fn log(&mut self, _: &str) {}
}

fn mcp(rig: &mut Rig, policies: &str, human: &mut Human, arguments: Value) -> ToolResult {
    let env = env(rig.directory.path(), policies, false);
    let config = Config::load(ConfigPaths::from_env(&env).unwrap(), &env).unwrap();
    let mut tools = McpTools::new(
        ReadTools::new(config, env.clone(), &mut rig.factory),
        TokenGate::new(),
    );

    tools.call_in("abap_run", arguments.as_object().unwrap(), human)
}

const MCP_POLICIES: &str =
    r#"{"mcp_run":"confirm","mcp_run_allow":["PROG:ZREPORT"],"mcp_run_snippet":"on"}"#;

#[test]
fn mcp_runs_a_report_after_the_human_saw_every_parameter_and_cuts_at_one_mib() {
    let mut rig = rig();

    rig.answer("RUN_PROG", vec![list("from the report")], "RAN");

    let mut human = Human {
        typed: Some("ZREPORT".to_owned()),
        forms: Vec::new(),
        cancel: rig.factory.cancel.clone(),
    };
    let result = mcp(
        &mut rig,
        MCP_POLICIES,
        &mut human,
        json!({"conn": "DEV_100", "type": "PROG", "name": "zreport", "params": ["P_A=7"], "variant": "v1"}),
    );

    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("from the report"), "{}", result.text);
    assert_eq!(human.forms.len(), 1);

    let form = format!("{:?}", human.forms[0]);

    for text in [
        "RUN PROG",
        "name: ZREPORT",
        "package: ZPKG",
        "parameter P_A = 7",
        "variant: V1",
        "alv capture: on",
        "NOT a dry run",
    ] {
        assert!(
            form.contains(text)
                || form
                    .to_ascii_lowercase()
                    .contains(&text.to_ascii_lowercase()),
            "{text}\n{form}"
        );
    }

    // MCP output is cut by the runner at 1 MiB, and `--alv-columns` is not an MCP argument.
    let document = rig.payload();

    assert!(
        document.contains(&"MAXBYTES=1048576".to_owned()),
        "{document:?}"
    );
    assert!(document.contains(&"ALVCOLS=visible".to_owned()));
    assert!(document.contains(&"VARIANT=V1".to_owned()));
    assert_eq!(rig.classes(), 0);
}

#[test]
fn mcp_runs_a_snippet_only_with_its_opt_in_and_shows_all_of_it() {
    let mut rig = rig();

    rig.answer("RUN_SNIPPET", vec![list("hello")], "RAN");

    let mut human = Human {
        typed: Some("RUN".to_owned()),
        forms: Vec::new(),
        cancel: rig.factory.cancel.clone(),
    };
    let arguments = json!({"conn": "DEV_100", "type": "SNIPPET", "source": SNIPPET});

    // The default refuses a snippet; the opt-in lets it through.
    let refused = mcp(
        &mut rig,
        r#"{"mcp_run":"confirm","mcp_run_allow":["PROG:ZREPORT"]}"#,
        &mut human,
        arguments.clone(),
    );

    assert!(refused.is_error);
    assert!(refused.text.contains("mcp_run_snippet"), "{}", refused.text);
    assert_eq!(rig.factory.adt_opened, 0);

    let result = mcp(&mut rig, MCP_POLICIES, &mut human, arguments);

    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("hello"), "{}", result.text);

    let form = format!("{:?}", human.forms[0]);

    assert!(
        form.contains("DATA(lv_x) = 1.") && form.contains("print( `done` )."),
        "{form}"
    );

    // A snippet that could leave its method never reaches the form.
    let mut human = Human {
        typed: Some("RUN".to_owned()),
        forms: Vec::new(),
        cancel: CancelFlag::new(),
    };
    let bad = mcp(
        &mut rig,
        MCP_POLICIES,
        &mut human,
        json!({"conn": "DEV_100", "type": "SNIPPET", "source": "x = 1. ENDMETHOD."}),
    );

    assert!(bad.is_error);
    assert!(bad.text.contains("ENDMETHOD"), "{}", bad.text);
    assert!(human.forms.is_empty());
}

#[test]
fn mcp_refuses_a_report_that_is_not_on_the_list_and_a_declined_form_runs_nothing() {
    let mut rig = rig();
    let mut human = Human {
        typed: None,
        forms: Vec::new(),
        cancel: rig.factory.cancel.clone(),
    };
    let other = mcp(
        &mut rig,
        MCP_POLICIES,
        &mut human,
        json!({"conn": "DEV_100", "type": "PROG", "name": "zother"}),
    );

    assert!(other.is_error);
    assert!(
        other.text.contains("not on mcp_run_allow"),
        "{}",
        other.text
    );
    assert_eq!(rig.factory.adt_opened, 0);

    let declined = mcp(
        &mut rig,
        MCP_POLICIES,
        &mut human,
        json!({"conn": "DEV_100", "type": "PROG", "name": "zreport"}),
    );

    assert!(declined.is_error);
    assert!(
        declined.text.contains("run not confirmed"),
        "{}",
        declined.text
    );
    assert_eq!(rig.posts(), 0);
    assert_eq!(rig.classes(), 0);
}
