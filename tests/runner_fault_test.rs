//! The fault targets of the runner (M6 Task 16): `drop-reply:CLASSRUN` and the runner stages
//! `CREATE`, `PUT`, `ACTIVATE`, and `DELETE` of the temporary class. The request runs in the fake
//! SAP and only its reply is dropped. Both backends (ADT HTTP and RFC-SADT) and both POST
//! transports. Only built with the feature.
#![cfg(feature = "fault-injection")]

mod common;

use abapreach::backend::BackendKind;
use abapreach::fault::{self, Fault, Target};
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::runner::{CycleInput, PostRoute, RunnerEnd, RunnerRun, run_cycle};
use abapreach::transport::rfc::{
    RfcApi, RfcEndpointRequest, RfcEndpointResponse, RfcTransport, SADT_REST_RFC_ENDPOINT,
};
use abapreach::transport::{Request, SadtTransport, TransportError};
use common::runner_fake::{Opened, Session, Shared, World, http_session, new_sap, session};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

const PREFIX: &str = "YTEST_ABAPREACH_RUN_";

fn faults(value: &str) -> Vec<Fault> {
    Fault::from_lookup(Some(value)).unwrap()
}

#[test]
fn the_classrun_target_is_a_known_fault_value() {
    let list = faults("drop-reply:CLASSRUN");
    let fault = list[0];

    assert_eq!(list.len(), 1);
    assert_eq!(fault.target(), Some(Target::Classrun));
    assert_eq!(fault.target().unwrap().name(), "CLASSRUN");
    assert_eq!(
        Fault::warning(&list).unwrap(),
        "WARNING: fault injection active (drop-reply:CLASSRUN)"
    );
    assert_eq!(
        fault.drop_reply(true).unwrap().to_string(),
        "post-send outcome uncertain: injected fault: reply dropped after CLASSRUN"
    );
    assert_eq!(fault.drop_reply(false), None);
    assert_eq!(
        faults("drop-reply:CLASSRUN,drop-reply:DELETE").len(),
        2,
        "it combines with the others"
    );

    for value in [
        "drop-reply:classrun",
        "drop-reply:CLASSRUN ",
        "drop-reply:CLASSRUNS",
        "drop-reply:RUN",
        "fail-read:CLASSRUN",
    ] {
        assert_eq!(
            Fault::from_lookup(Some(value)),
            Err("unknown ABAPREACH_FAULT value".to_owned()),
            "{value:?}"
        );
    }
}

#[test]
fn a_scoped_fault_replaces_the_environment_on_this_thread_only() {
    assert!(Fault::from_env().is_empty());

    {
        let _scope = fault::scoped(faults("drop-reply:PUT"));

        assert_eq!(Fault::from_env(), faults("drop-reply:PUT"));

        let other = std::thread::spawn(|| Fault::from_env().is_empty())
            .join()
            .unwrap();

        assert!(other, "another thread sees no scope");
    }

    assert!(Fault::from_env().is_empty());
}

// --- the RFC transport ------------------------------------------------------------------------

/// The RFC endpoint over the fake SAP: the SADT request reaches the same state as an HTTP one.
struct SapApi(Session);

impl RfcApi for SapApi {
    fn open(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    fn invoke(
        &mut self,
        request: RfcEndpointRequest,
    ) -> Result<RfcEndpointResponse, TransportError> {
        assert_eq!(request.function, SADT_REST_RFC_ENDPOINT);

        let response = self.0.request(Request {
            method: request.method,
            uri: request.uri,
            headers: request.headers,
            body: request.body,
        })?;

        Ok(RfcEndpointResponse {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }

    fn close(&mut self) {}
}

#[derive(Clone, Copy, Debug)]
enum Backend {
    Http,
    Rfc,
}

const BACKENDS: [Backend; 2] = [Backend::Http, Backend::Rfc];

fn opener(backend: Backend, sap: &Shared) -> Opened {
    match backend {
        Backend::Http => http_session(sap),
        Backend::Rfc => Ok(Box::new(RfcTransport::new(SapApi(session(sap, false))))),
    }
}

fn build(name: &str) -> Result<String, String> {
    Ok(format!("CLASS {name} DEFINITION.\nENDCLASS.\n"))
}

fn input(backend: Backend) -> CycleInput<'static> {
    CycleInput {
        operation: "deploy DTEL",
        sid: "DEV",
        client: "100",
        conn: "dev",
        prefix: PREFIX,
        user: "DEVUSER",
        language: "EN",
        backend: match backend {
            Backend::Http => BackendKind::AdtHttp,
            Backend::Rfc => BackendKind::RfcSadt,
        },
        rfc_available: true,
        build: &build,
    }
}

fn sap() -> Shared {
    new_sap(
        true,
        &Rc::new(Cell::new(Duration::ZERO)),
        Box::new(|_| World::default()),
    )
}

/// One cycle with the faults active.
fn faulty_cycle(backend: Backend, sap: &Shared, value: &str) -> RunnerRun {
    let _scope = fault::scoped(if value.is_empty() {
        Vec::new()
    } else {
        faults(value)
    });
    let mut open = || opener(backend, sap);

    run_cycle(
        &input(backend),
        &mut open,
        PostRoute::Cycle,
        &CancelFlag::new(),
    )
}

#[track_caller]
fn assert_clean(sap: &Shared, run: &RunnerRun) {
    assert!(sap.borrow().classes.is_empty(), "class left: {:?}", run.end);
    assert!(run.leftover.is_none(), "{:?}", run.leftover);
}

fn rejected(run: &RunnerRun) -> &str {
    match &run.end {
        RunnerEnd::Rejected(text) => text,
        other => panic!("expected REJECTED, got {other:?}"),
    }
}

#[test]
fn without_a_fault_the_cycle_posts_and_cleans_up_on_both_backends() {
    for backend in BACKENDS {
        let sap = sap();
        let run = faulty_cycle(backend, &sap, "");

        assert!(
            matches!(run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        assert_eq!(sap.borrow().posts, 1);
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_dropped_classrun_reply_is_uncertain_and_never_resent() {
    for backend in BACKENDS {
        let sap = sap();
        let run = faulty_cycle(backend, &sap, "drop-reply:CLASSRUN");

        assert!(
            matches!(&run.end, RunnerEnd::PostUncertain(error) if error.to_string().contains("reply dropped after CLASSRUN")),
            "{backend:?}: {:?}",
            run.end
        );
        assert!(run.uncertain);
        // The run executed in SAP (the fake counts it) and is not repeated.
        assert_eq!(sap.borrow().posts, 1, "{backend:?}");
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_dropped_classrun_reply_over_the_http_post_of_an_sadt_cycle() {
    // The class cycle runs over SADT; only the POST goes over HTTP in its own session.
    let sap = sap();
    let _scope = fault::scoped(faults("drop-reply:CLASSRUN"));
    let mut open = || opener(Backend::Rfc, &sap);
    let mut open_post = || http_session(&sap);
    let run = run_cycle(
        &input(Backend::Rfc),
        &mut open,
        PostRoute::Http(&mut open_post),
        &CancelFlag::new(),
    );

    assert!(
        matches!(&run.end, RunnerEnd::PostUncertain(_)),
        "{:?}",
        run.end
    );
    assert_eq!(sap.borrow().posts, 1);
    assert_clean(&sap, &run);
}

#[test]
fn the_runner_stages_are_dropped_after_they_ran_and_the_class_is_removed() {
    // (fault value, expected stage text, whether the POST is reached)
    let cases = [
        ("drop-reply:CREATE", "runner create failed"),
        ("drop-reply:PUT", "runner PUT failed"),
        ("drop-reply:ACTIVATE", "runner activate failed"),
    ];

    for backend in BACKENDS {
        for (value, stage) in cases {
            let sap = sap();
            let run = faulty_cycle(backend, &sap, value);

            assert!(
                rejected(&run).starts_with(stage),
                "{backend:?} {value}: {:?}",
                run.end
            );
            assert!(
                rejected(&run).contains("reply dropped"),
                "{backend:?} {value}: {:?}",
                run.end
            );
            assert_eq!(sap.borrow().posts, 0, "{backend:?} {value}: no POST");
            // The stage ran once in SAP and is not repeated.
            let marker = match value {
                "drop-reply:CREATE" => "POST /sap/bc/adt/oo/classes",
                "drop-reply:PUT" => "PUT /sap/bc/adt/oo/classes/",
                _ => "POST /sap/bc/adt/activation",
            };
            let times = sap
                .borrow()
                .log
                .iter()
                .filter(|(_, line)| {
                    line.starts_with(marker)
                        && !line.contains("_action=")
                        && (value != "drop-reply:CREATE" || line == marker)
                })
                .count();

            assert_eq!(times, 1, "{backend:?} {value}");
            assert_clean(&sap, &run);
        }
    }
}

#[test]
fn a_dropped_delete_reply_of_the_runner_class_is_settled_by_the_new_session() {
    for backend in BACKENDS {
        let sap = sap();
        let run = faulty_cycle(backend, &sap, "drop-reply:DELETE");

        assert!(
            matches!(run.end, RunnerEnd::Posted(_)),
            "{backend:?}: {:?}",
            run.end
        );
        // The delete ran; its reply was lost; the proof from a new session finds the class gone.
        assert_eq!(sap.borrow().count("DELETE /sap/bc/adt/oo/classes/"), 1);
        assert!(
            run.notes
                .iter()
                .any(|note| note.contains("reply dropped after DELETE")),
            "{:?}",
            run.notes
        );
        assert_clean(&sap, &run);
    }
}

#[test]
fn a_fault_of_one_target_leaves_the_other_stages_alone() {
    for backend in BACKENDS {
        let sap = sap();
        let run = faulty_cycle(backend, &sap, "drop-reply:CLASSRUN");
        let state = sap.borrow();

        // The create, PUT, and activation replies came back: the cycle got as far as the POST.
        assert_eq!(state.count("PUT /sap/bc/adt/oo/classes/"), 1);
        assert_eq!(state.count("POST /sap/bc/adt/activation"), 1);
        assert!(matches!(run.end, RunnerEnd::PostUncertain(_)));
    }
}
