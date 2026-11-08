//! The backend choice of a DDIC or `TRAN` runner command, shared by `read`, simulate, and create.
//!
//! One attempt runs on the backend the connection allows. A second backend is tried only when
//! the first one failed before anything was sent and `--backend` does not pin it; the fallback is
//! announced in the output and never taken after a write was sent.

use crate::app::{Exit, Factories, Opened};
use crate::backend::{BackendKind, Failure, Selection, SelectionReason};
use crate::config::Connection;
use crate::operation::deploy::CancelFlag;
use std::cell::RefCell;

/// What one attempt on one backend produced.
pub struct Routed {
    pub code: i32,
    pub stdout: String,
    /// Blocks of the stderr, in order; a block may hold several lines.
    pub stderr: Vec<String>,
    /// Set when nothing was sent and the failure allows the other backend.
    pub fallback: Option<(Failure, String)>,
}

/// A fresh 128-bit nonce as 32 upper-case hex digits from the OS random source.
pub fn random_nonce() -> String {
    let mut bytes = [0u8; 16];

    getrandom::fill(&mut bytes).expect("the OS random source is available");

    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

/// Runs `attempt` on the first backend and, when nothing was sent and the failure allows it, on
/// RFC. `label` names the command in the STOP of a backend that is not configured.
pub fn route(
    label: &str,
    connection: &Connection,
    explicit: Option<BackendKind>,
    factories: &mut dyn Factories,
    mut attempt: impl FnMut(&Selection, &dyn Fn() -> Opened, &CancelFlag) -> Routed,
) -> Exit {
    let cancel = factories.cancel();
    let mut backend = match explicit {
        Some(backend) => backend,
        None if connection.adt.is_some() => BackendKind::AdtHttp,
        None => BackendKind::RfcSadt,
    };
    let mut reason = match explicit {
        Some(_) => SelectionReason::ExplicitBackend,
        None if connection.adt.is_some() => SelectionReason::DefaultAdt,
        None => SelectionReason::RfcOnly,
    };
    let mut earlier: Vec<String> = Vec::new();

    loop {
        let available = match backend {
            BackendKind::AdtHttp => connection.adt.is_some(),
            BackendKind::RfcSadt => connection.rfc.is_some(),
        };

        if !available {
            return Exit {
                code: 2,
                stdout: String::new(),
                stderr: format!("STOP: {backend} is not available for {label}"),
            };
        }

        let selection = Selection::new(backend, reason.clone());
        let shared = RefCell::new(&mut *factories);
        let open = || match backend {
            BackendKind::AdtHttp => shared.borrow_mut().adt(connection),
            BackendKind::RfcSadt => shared.borrow_mut().rfc(connection),
        };
        let done = attempt(&selection, &open, &cancel);
        let retry = done.fallback.as_ref().filter(|_| {
            explicit.is_none() && backend == BackendKind::AdtHttp && connection.rfc.is_some()
        });

        let Some((failure, text)) = retry else {
            return exit_of(earlier, done);
        };

        reason = SelectionReason::Fallback(format!("{failure}: {text}"));
        earlier.extend(done.stderr);
        earlier.push(format!(
            "NOTE: {backend} failed before anything was written; falling back to RFC SADT"
        ));
        backend = BackendKind::RfcSadt;
    }
}

fn exit_of(mut stderr: Vec<String>, done: Routed) -> Exit {
    stderr.extend(done.stderr);
    stderr.retain(|line| !line.is_empty());

    // An abandoned backend breaks the command only when the one that replaced it failed too.
    if done.code == 0 {
        stderr = stderr
            .into_iter()
            .map(|block| match block.strip_prefix("ERROR: ") {
                Some(rest) => format!("NOTE: {rest}"),
                None => block,
            })
            .collect();
    }

    Exit {
        code: done.code,
        stdout: done.stdout,
        stderr: stderr.join("\n"),
    }
}
