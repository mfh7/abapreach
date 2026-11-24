//! What `inactive`, `activate` and `aunit` share: the failure type of an operation that runs
//! before or without a write, the productive-client check, and the mapping to a routed attempt.

use crate::app::Attempt;
use crate::backend::Failure;
use crate::domain::{Outcome, Status};
use crate::operation::read_table::{ReadTableError, fetch};
use crate::transport::{SadtTransport, TransportError};

/// Why an operation of this family stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QualityError {
    /// A rule or a fact refused the operation before it changed or ran anything.
    Rejected(String),
    /// The backend failed; `failure` is the routing class.
    Failed { failure: Failure, message: String },
}

impl QualityError {
    pub fn from_transport(error: &TransportError) -> Self {
        Self::Failed {
            failure: Failure::from(error),
            message: error.to_string(),
        }
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(message.into())
    }
}

impl From<ReadTableError> for QualityError {
    fn from(error: ReadTableError) -> Self {
        match error {
            ReadTableError::Rejected(message) => Self::Rejected(message),
            ReadTableError::Failed { failure, message } => Self::Failed { failure, message },
        }
    }
}

/// The routed attempt of a finished operation: a rejection is its own outcome, a backend failure
/// may fall back to the other backend when nothing was sent.
pub(crate) fn attempt(result: Result<Outcome, QualityError>, note: Option<String>) -> Attempt {
    let attempt = match result {
        Ok(outcome) => Attempt {
            outcome,
            failure: None,
            fallback_allowed: false,
            detail: None,
        },
        Err(QualityError::Rejected(message)) => Attempt {
            outcome: Outcome {
                status: Some(Status::Rejected),
                exit: 1,
                stdout: String::new(),
                stderr: format!("REJECTED: {message}"),
            },
            failure: None,
            fallback_allowed: false,
            detail: None,
        },
        Err(QualityError::Failed { failure, message }) => Attempt::failed(message, failure),
    };

    attempt.noted(note)
}

/// Why a command leaves ADT HTTP before it reads anything that matters.
pub const STATELESS_HTTP: &str = "ADT HTTP session is stateless on this release";

/// The SELECT of the client role: one preview of `T000`.
pub fn client_query(client: &str) -> String {
    format!("SELECT CCCATEGORY FROM T000 WHERE MANDT = '{client}'")
}

/// Refuses a productive client: `T000-CCCATEGORY = 'P'`. A client that `T000` does not list, or
/// a role that cannot be read, is refused too: the check fails closed. A lost connection or a
/// refused logon stay failures that routing understands. The one copy of the rule: `run`,
/// `activate` and `aunit` all call it.
pub fn check_client(transport: &mut dyn SadtTransport, client: &str) -> Result<(), QualityError> {
    if client.len() != 3 || !client.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(QualityError::rejected(format!(
            "client {client:?} is not three digits"
        )));
    }

    let data = match fetch(transport, "T000", &client_query(client), 1) {
        Ok(data) => data,
        Err(ReadTableError::Failed {
            failure: failure @ (Failure::PreSendConnectivity | Failure::Authorization),
            message,
        }) => return Err(QualityError::Failed { failure, message }),
        Err(ReadTableError::Failed { message, .. } | ReadTableError::Rejected(message)) => {
            return Err(QualityError::rejected(format!(
                "cannot read the client role of {client} from T000 ({message}); nothing was sent"
            )));
        }
    };
    let category = data
        .columns
        .iter()
        .position(|column| column == "CCCATEGORY")
        .and_then(|index| data.rows.first().map(|row| row[index].trim().to_owned()));

    match category.as_deref() {
        Some("P") => Err(QualityError::rejected(format!(
            "client {client} is productive (T000 CCCATEGORY P); nothing was sent"
        ))),
        Some(_) => Ok(()),
        None => Err(QualityError::rejected(format!(
            "client {client} is not listed in T000; nothing was sent"
        ))),
    }
}

/// The text of a `Failed` message for a `STOP`-free line.
pub fn describe(error: &QualityError) -> String {
    match error {
        QualityError::Rejected(message) | QualityError::Failed { message, .. } => message.clone(),
    }
}
