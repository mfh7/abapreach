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
}

impl Fault {
    fn parse(value: &str) -> Option<Self> {
        let (action, target) = value.split_once(':')?;

        match action {
            DROP_REPLY => Some(Self::DropReply(Target::parse(target)?)),
            _ => None,
        }
    }

    /// `Ok(None)` when the variable is unset, `Err` for a value this build does not know.
    pub fn from_lookup(value: Option<&str>) -> Result<Option<Self>, String> {
        match value {
            None => Ok(None),
            Some(value) => Self::parse(value)
                .map(Some)
                .ok_or_else(|| UNKNOWN.to_owned()),
        }
    }

    /// The fault the process environment selects. An unknown value has already stopped the
    /// command (`app::run`) before the first request, so it injects nothing here.
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(std::env::var(ENV).ok().as_deref())
            .ok()
            .flatten()
    }

    pub fn target(self) -> Target {
        match self {
            Self::DropReply(target) => target,
        }
    }

    /// The stderr line every command prints while the fault is active.
    pub fn warning(self) -> String {
        format!(
            "WARNING: fault injection active (drop-reply:{})",
            self.target().name()
        )
    }

    /// The uncertain outcome of a dropped reply, for a request the fault targets; `None` leaves
    /// every other request untouched.
    pub fn drop_reply(self, targeted: bool) -> Option<TransportError> {
        targeted.then(|| {
            TransportError::PostSendUncertain(format!(
                "injected fault: reply dropped after {}",
                self.target().name()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Fault;

    #[test]
    fn a_lookup_without_a_value_injects_nothing() {
        assert_eq!(Fault::from_lookup(None), Ok(None));
    }

    #[test]
    fn the_three_writes_of_the_rfc_path_are_known_values() {
        let cases = [
            ("drop-reply:PUT", "PUT"),
            ("drop-reply:ACTIVATE", "ACTIVATE"),
            ("drop-reply:CREATE", "CREATE"),
        ];

        for (value, name) in cases {
            let fault = Fault::from_lookup(Some(value)).unwrap().unwrap();

            assert_eq!(fault.target().name(), name);
            assert_eq!(
                fault.warning(),
                format!("WARNING: fault injection active ({value})")
            );
            assert_eq!(
                fault.drop_reply(true).unwrap().to_string(),
                format!("post-send outcome uncertain: injected fault: reply dropped after {name}")
            );
            assert_eq!(fault.drop_reply(false), None);
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
            "drop-reply:INCLUDE",
            "drop-reply:GET",
            "drop-reply:PUT ",
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
