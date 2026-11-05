//! Test-only fault injection, compiled in only with the `fault-injection` feature.
//!
//! ADT HTTP loses a reply through a local proxy; the RFC path has no proxy, so this feature drops
//! the reply of one chosen write after the RFC call has run and SAP has executed it. What is left
//! is a sent write with an unknown outcome, which is what reconciliation is built for. Never
//! enabled in a release build and never turned on by `test-support`.

use crate::transport::TransportError;

/// The variable that selects the fault; unset means no fault.
pub const ENV: &str = "ABAPREACH_FAULT";

const DROP_REPLY: &str = "drop-reply";
const FAIL_READ: &str = "fail-read";
const RECONCILE: &str = "RECONCILE";
const HOLD_LOCK: &str = "hold-lock";
/// Upper bound of a `hold-lock` pause, so a forgotten variable cannot stall a run for long.
const HOLD_LOCK_MAX_MS: u64 = 60_000;
const UNKNOWN: &str = "unknown ABAPREACH_FAULT value";

thread_local! {
    static DELETE_REPLY_DROPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Remembers that the reply of a delete was dropped. A delete reconciles from a new session, whose
/// transport knows nothing of the drop, so the fault state is kept per thread: one operation runs
/// on one thread, and parallel tests do not see each other's drops.
pub fn note_delete_reply_dropped() {
    DELETE_REPLY_DROPPED.with(|dropped| dropped.set(true));
}

/// Whether a delete reply was dropped on this thread: the reads that follow reconcile it.
pub fn delete_reply_dropped() -> bool {
    DELETE_REPLY_DROPPED.with(std::cell::Cell::get)
}

/// The write whose reply is dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// The source `PUT`.
    Put,
    /// `POST /sap/bc/adt/activation`.
    Activate,
    /// The create `POST` of a function group, function module, class or interface.
    Create,
    /// The `DELETE` of an object under its lock.
    Delete,
}

impl Target {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "PUT" => Some(Self::Put),
            "ACTIVATE" => Some(Self::Activate),
            "CREATE" => Some(Self::Create),
            "DELETE" => Some(Self::Delete),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Put => "PUT",
            Self::Activate => "ACTIVATE",
            Self::Create => "CREATE",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// The request is sent and executed, then its reply is discarded.
    DropReply(Target),
    /// The read-only reconciliation GET that follows a create `POST` or a delete whose reply was
    /// dropped fails without being sent.
    FailReconcileRead,
    /// The deploy pauses this many milliseconds right after its LOCK succeeded.
    HoldLock(u64),
}

impl Fault {
    fn parse(value: &str) -> Option<Self> {
        let (action, target) = value.split_once(':')?;

        match (action, target) {
            (DROP_REPLY, _) => Some(Self::DropReply(Target::parse(target)?)),
            (FAIL_READ, RECONCILE) => Some(Self::FailReconcileRead),
            (HOLD_LOCK, ms) if !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()) => ms
                .parse()
                .ok()
                .filter(|ms| (1..=HOLD_LOCK_MAX_MS).contains(ms))
                .map(Self::HoldLock),
            _ => None,
        }
    }

    /// The faults the variable selects: empty when it is unset, otherwise a comma-separated list
    /// (`drop-reply:CREATE,fail-read:RECONCILE`). `Err` for any item this build does not know.
    pub fn from_lookup(value: Option<&str>) -> Result<Vec<Self>, String> {
        value.map_or_else(
            || Ok(Vec::new()),
            |value| {
                value
                    .split(',')
                    .map(|item| Self::parse(item).ok_or_else(|| UNKNOWN.to_owned()))
                    .collect()
            },
        )
    }

    /// The faults the process environment selects. An unknown value has already stopped the
    /// command (`app::run`) before the first request, so it injects nothing here.
    pub fn from_env() -> Vec<Self> {
        Self::from_lookup(std::env::var(ENV).ok().as_deref()).unwrap_or_default()
    }

    /// The write a drop-reply fault targets; `None` for a fault that drops no reply.
    pub fn target(self) -> Option<Target> {
        match self {
            Self::DropReply(target) => Some(target),
            Self::FailReconcileRead | Self::HoldLock(_) => None,
        }
    }

    fn label(self) -> String {
        match self {
            Self::DropReply(target) => format!("{DROP_REPLY}:{}", target.name()),
            Self::FailReconcileRead => format!("{FAIL_READ}:{RECONCILE}"),
            Self::HoldLock(ms) => format!("{HOLD_LOCK}:{ms}"),
        }
    }

    /// The stderr line every command prints while the faults are active; `None` without any.
    pub fn warning(faults: &[Self]) -> Option<String> {
        (!faults.is_empty()).then(|| {
            let labels: Vec<String> = faults.iter().map(|fault| fault.label()).collect();

            format!("WARNING: fault injection active ({})", labels.join(","))
        })
    }

    /// The uncertain outcome of a dropped reply, for a request the fault targets; `None` leaves
    /// every other request untouched.
    pub fn drop_reply(self, targeted: bool) -> Option<TransportError> {
        let target = self.target()?;

        targeted.then(|| {
            TransportError::PostSendUncertain(format!(
                "injected fault: reply dropped after {}",
                target.name()
            ))
        })
    }

    /// The failure of the reconciliation read, for a read the fault targets.
    pub fn fail_read(self, targeted: bool) -> Option<TransportError> {
        (self == Self::FailReconcileRead && targeted).then(|| {
            TransportError::NoResponse("injected fault: the reconciliation read failed".to_owned())
        })
    }
}

/// Runs right after a successful LOCK when `hold-lock:<ms>` is set: prints a marker on stderr,
/// then waits until the time is up or Ctrl-C set the flag. The cancel checks that follow decide.
pub fn hold_lock(cancel: &crate::operation::deploy::CancelFlag) {
    let Some(ms) = Fault::from_env().into_iter().find_map(|fault| match fault {
        Fault::HoldLock(ms) => Some(ms),
        _ => None,
    }) else {
        return;
    };

    eprintln!("FAULT: lock held, pausing {ms} ms");
    pause(ms, cancel);
}

fn pause(ms: u64, cancel: &crate::operation::deploy::CancelFlag) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);

    while !cancel.is_set() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::{Fault, pause};
    use crate::operation::deploy::CancelFlag;
    use std::time::{Duration, Instant};

    #[test]
    fn hold_lock_combines_with_the_other_faults() {
        assert_eq!(
            Fault::from_lookup(Some("drop-reply:CREATE,hold-lock:1500")),
            Ok(vec![
                Fault::DropReply(super::Target::parse("CREATE").unwrap()),
                Fault::HoldLock(1500)
            ])
        );
        assert_eq!(
            Fault::from_lookup(Some("hold-lock:60000")),
            Ok(vec![Fault::HoldLock(60_000)])
        );
    }

    #[test]
    fn the_pause_ends_early_on_cancel_and_otherwise_runs_its_time() {
        let cancel = CancelFlag::new();
        let started = Instant::now();
        pause(150, &cancel);
        assert!(started.elapsed() >= Duration::from_millis(150));

        let cancel = CancelFlag::new();
        let signal = cancel.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            signal.set();
        });
        let started = Instant::now();
        pause(20_000, &cancel);
        thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_lookup_without_a_value_injects_nothing() {
        assert_eq!(Fault::from_lookup(None), Ok(Vec::new()));
        assert_eq!(Fault::warning(&[]), None);
    }

    #[test]
    fn the_three_writes_of_the_rfc_path_are_known_values() {
        let cases = [
            ("drop-reply:PUT", "PUT"),
            ("drop-reply:ACTIVATE", "ACTIVATE"),
            ("drop-reply:CREATE", "CREATE"),
            ("drop-reply:DELETE", "DELETE"),
        ];

        for (value, name) in cases {
            let faults = Fault::from_lookup(Some(value)).unwrap();
            let fault = faults[0];

            assert_eq!(faults.len(), 1);
            assert_eq!(fault.target().unwrap().name(), name);
            assert_eq!(
                Fault::warning(&faults).unwrap(),
                format!("WARNING: fault injection active ({value})")
            );
            assert_eq!(
                fault.drop_reply(true).unwrap().to_string(),
                format!("post-send outcome uncertain: injected fault: reply dropped after {name}")
            );
            assert_eq!(fault.drop_reply(false), None);
            assert_eq!(fault.fail_read(true), None);
        }
    }

    #[test]
    fn the_reconciliation_read_failure_is_a_known_value_that_drops_no_reply() {
        let faults = Fault::from_lookup(Some("fail-read:RECONCILE")).unwrap();
        let fault = faults[0];

        assert_eq!(fault, Fault::FailReconcileRead);
        assert_eq!(fault.target(), None);
        assert_eq!(fault.drop_reply(true), None);
        assert_eq!(fault.fail_read(false), None);
        assert_eq!(
            fault.fail_read(true).unwrap().to_string(),
            "no response to a read-only request: injected fault: the reconciliation read failed"
        );
        assert_eq!(
            Fault::warning(&faults).unwrap(),
            "WARNING: fault injection active (fail-read:RECONCILE)"
        );
    }

    #[test]
    fn faults_combine_as_a_comma_separated_list() {
        let faults = Fault::from_lookup(Some("drop-reply:CREATE,fail-read:RECONCILE")).unwrap();

        assert_eq!(faults.len(), 2);
        assert_eq!(
            Fault::warning(&faults).unwrap(),
            "WARNING: fault injection active (drop-reply:CREATE,fail-read:RECONCILE)"
        );
    }

    #[test]
    fn hold_lock_takes_a_bounded_millisecond_count_and_drops_no_reply() {
        let faults = Fault::from_lookup(Some("hold-lock:5000")).unwrap();

        assert_eq!(faults, vec![Fault::HoldLock(5000)]);
        assert_eq!(faults[0].target(), None);
        assert_eq!(faults[0].drop_reply(true), None);
        assert_eq!(faults[0].fail_read(true), None);
        assert_eq!(
            Fault::warning(&faults).unwrap(),
            "WARNING: fault injection active (hold-lock:5000)"
        );

        for value in [
            "hold-lock",
            "hold-lock:",
            "hold-lock:0",
            "hold-lock:60001",
            "hold-lock:+5",
            "hold-lock:5s",
        ] {
            assert_eq!(
                Fault::from_lookup(Some(value)),
                Err("unknown ABAPREACH_FAULT value".to_owned()),
                "{value:?}"
            );
        }
    }

    #[test]
    fn every_other_value_is_refused() {
        for value in [
            "",
            "drop-reply",
            "drop-reply:",
            "drop-reply:put",
            "drop-reply:create",
            "drop-reply:delete",
            "drop-reply:INCLUDE",
            "drop-reply:GET",
            "drop-reply:RECONCILE",
            "drop-reply:PUT ",
            "drop-reply:PUT,",
            ",drop-reply:PUT",
            "drop-reply:PUT,drop-reply:nope",
            "drop-reply:PUT drop-reply:CREATE",
            "fail-read",
            "fail-read:",
            "fail-read:reconcile",
            "fail-read:CREATE",
            "fail-read:RECONCILE ",
            "drop:PUT",
            "POST",
        ] {
            assert_eq!(
                Fault::from_lookup(Some(value)),
                Err("unknown ABAPREACH_FAULT value".to_owned()),
                "{value:?}"
            );
        }
    }
}
