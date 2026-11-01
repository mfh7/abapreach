mod common;
mod replay;

use abapreach::domain::Status;
use abapreach::operation::deploy::{
    CancelFlag, DeployInput, DeployProg, INTERRUPT_EXIT_CODE, InterruptAction,
};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use replay::{Replay, fx, no_inactive, replay};

const FIXTURE: &str = "dev-750-rfc.json";

fn changed() -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            "WRITE / 'v2'.".to_owned(),
        ],
        base: None,
        simulate: true,
        tr: None,
    }
}

/// Records whether the lock is held at every request and, at one chosen request, behaves like a
/// SIGINT arriving while the deploy runs.
struct Watch {
    inner: Replay,
    cancel: CancelFlag,
    interrupt_at: Option<usize>,
    /// `(request, lock held before it)` for every request the deploy sent.
    watched: Vec<(String, bool)>,
    interrupts: Vec<InterruptAction>,
    closed_with_lock_held: bool,
}

impl Watch {
    fn new(inner: Replay, cancel: &CancelFlag, interrupt_at: Option<usize>) -> Self {
        Self {
            inner,
            cancel: cancel.clone(),
            interrupt_at,
            watched: Vec::new(),
            interrupts: Vec::new(),
            closed_with_lock_held: false,
        }
    }

    fn held(&self) -> Vec<bool> {
        self.watched.iter().map(|(_, held)| *held).collect()
    }
}

impl SadtTransport for Watch {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        let index = self.watched.len();
        self.watched.push((
            format!("{} {}", request.method, request.uri),
            self.cancel.lock_held(),
        ));

        if self.interrupt_at == Some(index) {
            self.interrupts.push(self.cancel.on_interrupt());
        }

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closed_with_lock_held = self.cancel.lock_held();
        self.inner.close()
    }
}

#[test]
fn ctrl_c_exits_at_once_without_a_lock_and_is_deferred_inside_one() {
    assert_eq!(INTERRUPT_EXIT_CODE, 130);

    let cancel = CancelFlag::new();

    assert_eq!(cancel.on_interrupt(), InterruptAction::ExitNow);
    assert!(!cancel.is_set());
    assert!(!cancel.lock_held());

    let scope = cancel.hold_lock();

    assert!(cancel.lock_held());
    assert_eq!(cancel.on_interrupt(), InterruptAction::Deferred);
    assert!(cancel.is_set());

    drop(scope);

    assert!(!cancel.lock_held());
    assert_eq!(cancel.on_interrupt(), InterruptAction::ExitNow);
}

#[test]
fn the_lock_is_held_from_the_lock_request_until_the_unlock() {
    let cancel = CancelFlag::new();
    let mut transport = Watch::new(
        replay(
            FIXTURE,
            vec![
                fx(6),
                no_inactive(),
                fx(7),
                fx(6),
                no_inactive(),
                fx(8),
                fx(9),
            ],
        ),
        &cancel,
        None,
    );

    let outcome = DeployProg::run_with_cancel(&mut transport, changed(), &cancel);

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(outcome.exit, 0);
    assert_eq!(
        transport.held(),
        vec![false, false, true, true, true, true, true]
    );
    assert!(!transport.closed_with_lock_held);
    assert!(!cancel.lock_held());
    assert!(transport.interrupts.is_empty());
}

#[test]
fn an_interrupt_during_the_lock_request_is_deferred_and_unlocks() {
    let cancel = CancelFlag::new();
    let mut transport = Watch::new(
        replay(FIXTURE, vec![fx(6), no_inactive(), fx(7), fx(9)]),
        &cancel,
        Some(2),
    );

    let outcome = DeployProg::run_with_cancel(&mut transport, changed(), &cancel);

    assert_eq!(transport.interrupts, vec![InterruptAction::Deferred]);
    assert_eq!(outcome.exit, INTERRUPT_EXIT_CODE);
    assert!(outcome.stderr.starts_with("CANCELED"), "{}", outcome.stderr);
    assert!(
        transport
            .watched
            .last()
            .unwrap()
            .0
            .contains("_action=UNLOCK")
    );
    assert_eq!(transport.inner.closes, 1);
    assert!(!cancel.lock_held());
    transport.inner.assert_finished();
}

#[test]
fn a_deferred_interrupt_still_unlocks_and_never_reports_success() {
    let cancel = CancelFlag::new();
    // The checkrun is the request during which the interrupt arrives.
    let mut transport = Watch::new(
        replay(
            FIXTURE,
            vec![
                fx(6),
                no_inactive(),
                fx(7),
                fx(6),
                no_inactive(),
                fx(8),
                fx(9),
            ],
        ),
        &cancel,
        Some(4),
    );

    let outcome = DeployProg::run_with_cancel(&mut transport, changed(), &cancel);

    assert_eq!(transport.interrupts, vec![InterruptAction::Deferred]);
    assert!(cancel.is_set());
    assert_ne!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(outcome.exit, INTERRUPT_EXIT_CODE);
    assert!(outcome.stderr.starts_with("CANCELED"), "{}", outcome.stderr);
    assert!(
        transport
            .watched
            .last()
            .unwrap()
            .0
            .contains("_action=UNLOCK")
    );
    assert_eq!(transport.inner.closes, 1);
    assert!(!cancel.lock_held());
    transport.inner.assert_finished();
}
