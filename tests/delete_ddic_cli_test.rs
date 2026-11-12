//! The `delete` command with DDIC rows through the whole application: the router, the runner
//! backend, the plan file, and the exit. A fake SAP answers the classrun POSTs. No SAP.

mod common;

use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::TransportError;
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use common::runner_fake::{Opened, Shared, World, http_session, new_sap, rfc_session};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

const NONCE_HEX: usize = 32;
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Sessions {
    sap: Shared,
    /// ADT sessions that open; later ones fail before anything is sent.
    adt_ok: usize,
    adt_calls: usize,
    rfc_calls: usize,
    cancel: CancelFlag,
    /// The failure of an ADT open past `adt_ok`; connectivity when unset.
    adt_error: Option<TransportError>,
}

impl Factories for Sessions {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_calls += 1;

        if self.adt_calls > self.adt_ok {
            return Err(self
                .adt_error
                .clone()
                .unwrap_or_else(|| TransportError::PreSendConnectivity("unreachable".to_owned())));
        }

        http_session(&self.sap)
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_calls += 1;

        rfc_session(&self.sap)
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("a delete probes no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("a delete pings nothing")
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02X}")).collect()
}

/// Output for the nonce the product drew: the fake reads it from the class source at POST time,
/// so the scripted reply is built by `answer`.
fn output(nonce: &str, mode: &str, body: &[String], status: &str) -> String {
    let mut lines = vec![format!("ABAPREACH:{nonce}:BEGIN:{mode}")];

    lines.extend(body.iter().map(|rest| format!("ABAPREACH:{nonce}:{rest}")));
    lines.push(format!("ABAPREACH:{nonce}:STATUS:{status}"));

    let count = lines.len();

    lines.push(format!("ABAPREACH:{nonce}:END:{count}"));

    lines.iter().map(|line| format!("{line}\n\n")).collect()
}

fn base64_decode(text: &str) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0);

    for character in text.bytes().filter(|byte| *byte != b'=') {
        buffer = (buffer << 6)
            | alphabet
                .iter()
                .position(|known| *known == character)
                .unwrap() as u32;
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }

    String::from_utf8(bytes).unwrap()
}

/// The nonce of the payload in a class source.
fn nonce_of(source: &str) -> String {
    let encoded: String = source
        .lines()
        .filter_map(|line| {
            line.strip_prefix("    lv_b64 = lv_b64 && '")?
                .strip_suffix("'.")
        })
        .collect();
    let nonce = base64_decode(&encoded)
        .lines()
        .find_map(|line| line.strip_prefix("NONCE=").map(str::to_owned))
        .expect("a nonce in the payload");

    assert_eq!(nonce.len(), NONCE_HEX);

    nonce
}

fn env(directory: &Path, rfc: bool) -> BTreeMap<String, String> {
    let path = directory.join("connections.json");
    let config = if rfc {
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"rfc":{"ashost":"example.com","sysnr":"00"},"runner_prefix":"YTEST_ABAPREACH_RUN_"}]}"#
    } else {
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"runner_prefix":"YTEST_ABAPREACH_RUN_"}]}"#
    };

    fs::write(&path, config).unwrap();

    BTreeMap::from([
        ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn sap_with(gone_after_post: bool) -> Shared {
    let now = Rc::new(Cell::new(Duration::ZERO));
    new_sap(
        true,
        &now,
        Box::new(move |tick| {
            if gone_after_post && tick.posts > 0 {
                World::default()
            } else {
                World {
                    definition: true,
                    directory: true,
                    ..World::default()
                }
            }
        }),
    )
}

fn run_delete(sessions: &mut Sessions, directory: &Path, rfc: bool, args: &[&str]) -> Exit {
    let mut full = vec!["abapreach".to_owned(), "delete".to_owned()];

    full.extend(args.iter().map(|argument| (*argument).to_owned()));
    full.extend(["--conn".to_owned(), "DEV_100".to_owned()]);

    run(full, env(directory, rfc), sessions)
}

/// A fake SAP that answers a transaction or DDIC `DELETE_SIM` and `DELETE` like the class would.
fn answering_tran(sap: &Shared) {
    sap.borrow_mut().reply_with = Some(Box::new(|source: &str| {
        let nonce = nonce_of(source);

        if payload_mode(source) == "DELETE_SIM" {
            let mut body = vec![
                format!("FACT:PACKAGE:{}", hex("$TMP")),
                format!("FACT:AUTHOR:{}", hex("DEVUSER")),
            ];

            if payload_type(source) == "TRAN" {
                body.push(format!("FACT:PROGRAM:{}", hex("ZREPORT")));
                body.push(format!("FACT:CINFO:{}", hex("80")));
            }

            body.push(format!("FACT:REQUEST:{}", hex("-")));
            body.push(format!("FACT:HASH:{}", hex(HASH)));

            output(&nonce, "DELETE_SIM", &body, "SIMULATED_OK")
        } else {
            output(&nonce, "DELETE", &[], "DELETED")
        }
    }));
}

#[test]
fn a_tran_batch_runs_through_the_router_the_plan_file_and_one_backend() {
    // An ADT route that opens, and one that cannot be reached before anything is sent (RFC then
    // runs the whole operation).
    for (adt_ok, rfc) in [(usize::MAX, false), (0, true)] {
        let directory = tempfile::tempdir().unwrap();
        let plan = directory.path().join("plan.json");
        let plan_arg = plan.display().to_string();
        let sap = sap_with(true);

        answering_tran(&sap);

        let mut sessions = Sessions {
            sap: sap.clone(),
            adt_ok,
            adt_calls: 0,
            rfc_calls: 0,
            cancel: CancelFlag::new(),
            adt_error: None,
        };
        let args = [
            "--simulate",
            "DTEL:zdtel",
            "TRAN:ztran",
            "--plan",
            &plan_arg,
        ];
        let simulated = run_delete(&mut sessions, directory.path(), rfc, &args);

        assert_eq!(
            simulated.code, 0,
            "{rfc} {} {}",
            simulated.stdout, simulated.stderr
        );

        for expected in [
            "== TRAN ZTRAN ==",
            "DELETE TRAN ZTRAN package $TMP author DEVUSER TR -",
            "PROGRAM ZREPORT",
            "CINFO 80",
            &format!("SOURCE LINES {HASH}"),
        ] {
            assert!(
                simulated.stdout.contains(expected),
                "{expected} in {}",
                simulated.stdout
            );
        }

        // TRAN runs before DTEL.
        let first = simulated.stdout.find("== TRAN ZTRAN ==").unwrap();
        let second = simulated.stdout.find("== DTEL ZDTEL ==").unwrap();

        assert!(first < second, "{}", simulated.stdout);

        // The plan file keeps its format: connection, request, and rows; no backend list.
        let text = fs::read_to_string(&plan).unwrap();

        assert!(
            text.contains("ZTRAN") && !text.contains("backends"),
            "{text}"
        );

        let real = run_delete(
            &mut sessions,
            directory.path(),
            rfc,
            &["TRAN:ztran", "DTEL:zdtel", "--plan", &plan_arg],
        );

        assert_eq!(real.code, 0, "{rfc} {} {}", real.stdout, real.stderr);
        assert!(real.stdout.contains("SUMMARY: 2 OK, 0 FAILED, 0 SKIPPED"));
        assert_eq!(sap.borrow().posts, 4);
        assert!(sap.borrow().classes.is_empty());

        // The operation stays on the backend it began with: simulate and delete, the TRAN row and
        // the DDIC row. The ADT route that cannot open is tried once per command and never again.
        if rfc {
            assert_eq!(sessions.adt_calls, 2, "one failed open per command");
            assert!(sessions.rfc_calls >= 4, "{}", sessions.rfc_calls);
        } else {
            assert!(sessions.adt_calls >= 4, "{}", sessions.adt_calls);
            assert_eq!(sessions.rfc_calls, 0);
        }
    }
}

#[test]
fn a_tran_row_outside_the_customer_scope_stops_before_a_connection() {
    let directory = tempfile::tempdir().unwrap();
    let mut sessions = Sessions {
        sap: sap_with(false),
        adt_ok: 0,
        adt_calls: 0,
        rfc_calls: 0,
        cancel: CancelFlag::new(),
        adt_error: None,
    };
    let plan = directory.path().join("plan.json");
    let exit = run_delete(
        &mut sessions,
        directory.path(),
        false,
        &["TRAN:SE38", "--plan", &plan.display().to_string()],
    );

    assert_eq!(exit.code, 2);
    assert_eq!(
        exit.stderr,
        "STOP: write and delete accept only Z and Y names: SE38"
    );
    assert_eq!(sessions.adt_calls + sessions.rfc_calls, 0);
}

#[test]
fn a_ddic_row_with_a_name_outside_the_customer_scope_stops_before_a_connection() {
    let directory = tempfile::tempdir().unwrap();
    let mut sessions = Sessions {
        sap: sap_with(false),
        adt_ok: 0,
        adt_calls: 0,
        rfc_calls: 0,
        cancel: CancelFlag::new(),
        adt_error: None,
    };
    let plan = directory.path().join("plan.json");
    let exit = run_delete(
        &mut sessions,
        directory.path(),
        false,
        &["DTEL:ADTEL", "--plan", &plan.display().to_string()],
    );

    assert_eq!(exit.code, 2);
    assert_eq!(
        exit.stderr,
        "STOP: write and delete accept only Z and Y names: ADTEL"
    );
    assert_eq!(sessions.adt_calls + sessions.rfc_calls, 0);
}

#[test]
fn a_simulate_then_a_delete_run_through_the_router_and_the_plan_file() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let plan_arg = plan.display().to_string();
    let sap = sap_with(true);
    let hash = HASH.to_owned();

    // The fake needs the nonce of each posted class, which it only learns at POST time.
    sap.borrow_mut().reply_with = Some(Box::new(move |source: &str| {
        let nonce = nonce_of(source);
        let payload = payload_mode(source);

        if payload == "DELETE_SIM" {
            output(
                &nonce,
                "DELETE_SIM",
                &[
                    format!("FACT:PACKAGE:{}", hex("$TMP")),
                    format!("FACT:AUTHOR:{}", hex("DEVUSER")),
                    format!("FACT:REQUEST:{}", hex("-")),
                    format!("FACT:STATE:{}", hex("ACTIVE")),
                    format!("FACT:HASH:{}", hex(&hash)),
                ],
                "SIMULATED_OK",
            )
        } else {
            output(&nonce, "DELETE", &[], "DELETED")
        }
    }));

    let mut sessions = Sessions {
        sap: sap.clone(),
        adt_ok: usize::MAX,
        adt_calls: 0,
        rfc_calls: 0,
        cancel: CancelFlag::new(),
        adt_error: None,
    };
    let simulated = run_delete(
        &mut sessions,
        directory.path(),
        false,
        &["--simulate", "DTEL:zdtel", "--plan", &plan_arg],
    );

    assert_eq!(
        simulated.code, 0,
        "{} {}",
        simulated.stdout, simulated.stderr
    );
    assert!(simulated.stdout.contains("DELETE_SIM: SIMULATED_OK"));
    assert!(plan.exists());
    assert!(fs::read_to_string(&plan).unwrap().contains(HASH));

    let real = run_delete(
        &mut sessions,
        directory.path(),
        false,
        &["DTEL:zdtel", "--plan", &plan_arg],
    );

    assert_eq!(real.code, 0, "{} {}", real.stdout, real.stderr);
    assert!(real.stdout.contains("DELETE: DELETED"), "{}", real.stdout);
    assert_eq!(sap.borrow().posts, 2);
    assert!(sap.borrow().classes.is_empty());
}

fn payload_type(source: &str) -> String {
    payload_value(source, "TYPE=")
}

fn payload_mode(source: &str) -> String {
    payload_value(source, "MODE=")
}

fn payload_value(source: &str, key: &str) -> String {
    let encoded: String = source
        .lines()
        .filter_map(|line| {
            line.strip_prefix("    lv_b64 = lv_b64 && '")?
                .strip_suffix("'.")
        })
        .collect();

    base64_decode(&encoded)
        .lines()
        .find_map(|line| line.strip_prefix(key).map(str::to_owned))
        .expect("a key in the payload")
}

#[test]
fn a_first_run_that_never_left_the_process_falls_back_to_rfc() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let sap = sap_with(true);
    let hash = HASH.to_owned();

    sap.borrow_mut().reply_with = Some(Box::new(move |source: &str| {
        output(
            &nonce_of(source),
            "DELETE_SIM",
            &[format!("FACT:HASH:{}", hex(&hash))],
            "SIMULATED_OK",
        )
    }));

    // One ADT session opens (the router's), the next one (the runner cycle) cannot be reached.
    let mut sessions = Sessions {
        sap: sap.clone(),
        adt_ok: 1,
        adt_calls: 0,
        rfc_calls: 0,
        cancel: CancelFlag::new(),
        adt_error: None,
    };
    let exit = run_delete(
        &mut sessions,
        directory.path(),
        true,
        &[
            "--simulate",
            "TTYP:ZTT",
            "--plan",
            &plan.display().to_string(),
        ],
    );

    assert_eq!(exit.code, 0, "{} {}", exit.stdout, exit.stderr);
    assert!(
        exit.stderr.contains("falling back to RFC SADT"),
        "{}",
        exit.stderr
    );
    assert!(sessions.rfc_calls >= 2);
    assert_eq!(sap.borrow().posts, 1);
}

#[test]
fn an_authorization_failure_at_the_runner_start_does_not_fall_back_to_rfc() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let sap = sap_with(true);

    answering_tran(&sap);

    // The router's ADT session opens; the runner cycle's open is refused for authorization.
    let mut sessions = Sessions {
        sap: sap.clone(),
        adt_ok: 1,
        adt_calls: 0,
        rfc_calls: 0,
        cancel: CancelFlag::new(),
        adt_error: Some(TransportError::Authorization("HTTP 401".to_owned())),
    };
    let exit = run_delete(
        &mut sessions,
        directory.path(),
        true,
        &[
            "--simulate",
            "TTYP:ZTT",
            "--plan",
            &plan.display().to_string(),
        ],
    );

    assert_ne!(exit.code, 0, "{} {}", exit.stdout, exit.stderr);
    assert!(!exit.stderr.contains("falling back"), "{}", exit.stderr);
    assert_eq!(
        sessions.rfc_calls, 0,
        "no backend switch after authorization"
    );
    assert_eq!(sap.borrow().posts, 0);
    assert!(exit.stdout.contains("authorization"), "{}", exit.stdout);
}

#[test]
fn a_stateless_adt_session_runs_the_tran_batch_over_rfc() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let sap = sap_with(false);

    answering_tran(&sap);
    // No context cookie: the ADT session of this release keeps no state.
    sap.borrow_mut().stateful = false;

    let mut sessions = Sessions {
        sap: sap.clone(),
        adt_ok: usize::MAX,
        adt_calls: 0,
        rfc_calls: 0,
        cancel: CancelFlag::new(),
        adt_error: None,
    };
    let exit = run_delete(
        &mut sessions,
        directory.path(),
        true,
        &[
            "--simulate",
            "TRAN:ZTRAN",
            "--plan",
            &plan.display().to_string(),
        ],
    );

    assert_eq!(exit.code, 0, "{} {}", exit.stdout, exit.stderr);
    assert!(
        exit.stdout.contains("DELETE_SIM: SIMULATED_OK"),
        "{}",
        exit.stdout
    );
    assert!(
        exit.stderr.contains("falling back to RFC SADT") && !exit.stderr.contains("STOP:"),
        "{}",
        exit.stderr
    );
    assert!(sessions.rfc_calls >= 1);
    assert_eq!(sap.borrow().posts, 1);
}

#[test]
fn a_stateless_adt_session_without_rfc_keeps_the_stop() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory.path().join("plan.json");
    let sap = sap_with(false);

    answering_tran(&sap);
    sap.borrow_mut().stateful = false;

    let mut sessions = Sessions {
        sap: sap.clone(),
        adt_ok: usize::MAX,
        adt_calls: 0,
        rfc_calls: 0,
        cancel: CancelFlag::new(),
        adt_error: None,
    };
    let exit = run_delete(
        &mut sessions,
        directory.path(),
        false,
        &[
            "--simulate",
            "TRAN:ZTRAN",
            "--plan",
            &plan.display().to_string(),
        ],
    );

    assert_eq!(exit.code, 2, "{} {}", exit.stdout, exit.stderr);
    assert_eq!(sessions.rfc_calls, 0);
    assert_eq!(sap.borrow().posts, 0);
}
