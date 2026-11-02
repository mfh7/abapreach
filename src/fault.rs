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
const UNKNOWN: &str = "unknown ABAPREACH_FAULT value";

/// The write whose reply is dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// The source `PUT`.
    Put,
    /// `POST /sap/bc/adt/activation`.
    Activate,
    /// The create `POST` of a function group, function module, class or interface.
    Create,
}

impl Target {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "PUT" => Some(Self::Put),
            "ACTIVATE" => Some(Self::Activate),
            "CREATE" => Some(Self::Create),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Put => "PUT",
            Self::Activate => "ACTIVATE",
            Self::Create => "CREATE",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// The request is sent and executed, then its reply is discarded.
    DropReply(Target),
    /// The read-only reconciliation GET that follows a create `POST` fails without being sent.
    FailReconcileRead,
}

impl Fault {
    fn parse(value: &str) -> Option<Self> {
        let (action, target) = value.split_once(':')?;

        match (action, target) {
            (DROP_REPLY, _) => Some(Self::DropReply(Target::parse(target)?)),
            (FAIL_READ, RECONCILE) => Some(Self::FailReconcileRead),
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
            Self::FailReconcileRead => None,
        }
    }

    fn label(self) -> String {
        match self {
            Self::DropReply(target) => format!("{DROP_REPLY}:{}", target.name()),
            Self::FailReconcileRead => format!("{FAIL_READ}:{RECONCILE}"),
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

#[cfg(test)]
mod tests {
    use super::Fault;

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
    fn every_other_value_is_refused() {
        for value in [
            "",
            "drop-reply",
            "drop-reply:",
            "drop-reply:put",
            "drop-reply:create",
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
