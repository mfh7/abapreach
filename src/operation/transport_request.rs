//! The request a `--tr` names for an object that no open request holds yet.
//!
//! The ADT lock answers `CORRNR` only when a modifiable request already holds the object. When it
//! is empty and `--tr` is given, the write under the lock carries `corrNr=<request>` and records
//! the object in that request. Before any write, this module reads the request from `E070` over
//! the data preview (read-only) and applies the rules the classrun runner applies to its `--tr`:
//! a workbench request, not a task, modifiable, owned by the logon user or holding a modifiable
//! task of that user. Any failure refuses, so nothing is written.

use crate::operation::read_table::{ReadTableError, fetch};
use crate::transport::SadtTransport;
use std::collections::BTreeMap;

const E070: &str = "E070";
/// A user name is at most 12 characters.
const USER_MAX: usize = 12;
/// A request number is at most 20 characters.
const REQUEST_MAX: usize = 20;

/// The columns of one `E070` row, trimmed.
type Row = BTreeMap<String, String>;

/// The first row of one preview, or `None` for an empty answer.
fn first_row(transport: &mut dyn SadtTransport, body: &str) -> Result<Option<Row>, String> {
    let data = fetch(transport, E070, body, 1).map_err(|error| match error {
        ReadTableError::Rejected(message) | ReadTableError::Failed { message, .. } => message,
    })?;

    Ok(data.rows.first().map(|row| {
        data.columns
            .iter()
            .cloned()
            .zip(row.iter().map(|value| value.trim().to_owned()))
            .collect()
    }))
}

/// A value of a preview condition: upper-case letters, digits, `_` and `/` only.
fn literal(value: &str, longest: usize) -> Option<String> {
    let value = value.to_ascii_uppercase();
    let valid = !value.is_empty()
        && value.len() <= longest
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"_/".contains(&byte)
        });

    valid.then_some(value)
}

/// What `E070` says about the request, as far as the rules need it.
#[derive(Debug, Eq, PartialEq)]
struct RequestFacts<'a> {
    strkorr: &'a str,
    function: &'a str,
    status: &'a str,
    owner: &'a str,
}

/// What the rules decide before the task lookup.
#[derive(Debug, Eq, PartialEq)]
enum Verdict {
    /// The request qualifies on its own.
    Accepted,
    /// The request is another user's: it qualifies only through a modifiable task of the user.
    NeedsTask,
    Refused(String),
}

/// The pure rules on one `E070` row of the request `tr`.
fn judge(tr: &str, user: &str, facts: Option<&RequestFacts<'_>>) -> Verdict {
    let Some(facts) = facts else {
        return Verdict::Refused(format!("request {tr} does not exist"));
    };

    if !facts.strkorr.is_empty() || facts.function != "K" {
        return Verdict::Refused(format!(
            "{tr} is not a workbench request (a task or another type is not accepted)"
        ));
    }

    if facts.status != "D" {
        return Verdict::Refused(format!(
            "request {tr} is not modifiable (TRSTATUS={})",
            facts.status
        ));
    }

    if facts.owner.eq_ignore_ascii_case(user) {
        Verdict::Accepted
    } else {
        Verdict::NeedsTask
    }
}

/// Checks that `tr` may take a new object for `user`. `Err` is the text of the STOP; nothing has
/// been written when it is returned. Two previews at most, both read-only.
pub fn check_open_request(
    transport: &mut dyn SadtTransport,
    tr: &str,
    user: &str,
) -> Result<(), String> {
    let (Some(tr), Some(user)) = (literal(tr, REQUEST_MAX), literal(user, USER_MAX)) else {
        return Err(format!(
            "request {tr} cannot be checked for the logon user {user:?}"
        ));
    };
    let unreadable = |reason: String| format!("cannot read request {tr}: {reason}");
    let request = first_row(
        transport,
        &format!(
            "SELECT TRKORR, STRKORR, TRFUNCTION, TRSTATUS, AS4USER FROM {E070} WHERE TRKORR IN ( '{tr}' )"
        ),
    )
    .map_err(unreadable)?
    // A row of another request is not trusted.
    .filter(|row| row.get("TRKORR").is_some_and(|got| got.eq_ignore_ascii_case(&tr)));
    let value = |name: &str| {
        request
            .as_ref()
            .and_then(|row| row.get(name))
            .map_or("", String::as_str)
    };
    let facts = request.as_ref().map(|_| RequestFacts {
        strkorr: value("STRKORR"),
        function: value("TRFUNCTION"),
        status: value("TRSTATUS"),
        owner: value("AS4USER"),
    });

    match judge(&tr, &user, facts.as_ref()) {
        Verdict::Accepted => Ok(()),
        Verdict::Refused(reason) => Err(reason),
        Verdict::NeedsTask => {
            let task = first_row(
                transport,
                &format!(
                    "SELECT TRKORR FROM {E070} WHERE STRKORR IN ( '{tr}' ) AND AS4USER IN ( '{user}' ) AND TRSTATUS IN ( 'D' )"
                ),
            )
            .map_err(unreadable)?;

            if task.is_some() {
                Ok(())
            } else {
                Err(format!(
                    "request {tr} is not owned by {user} and holds no task of {user}"
                ))
            }
        }
    }
}
