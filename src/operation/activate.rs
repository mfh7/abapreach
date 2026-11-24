//! `activate`: activates inactive objects of the logon user, one `POST /sap/bc/adt/activation`
//! per object, and proves the result against the inactive list.
//!
//! The tokens are the ones `inactive` prints (`PROG/P:ZPROG`). Only own inactive Z/Y objects of
//! the types below are activated; DDIC stays on the DDIC deploy path. Every rule runs before the
//! first request that changes something, for a simulation and for the real run alike.

use crate::adt::{
    ActivationBody, classify_activation, format_messages, parse_object_refs, prog_uri,
};
use crate::cli::ObjectKind;
use crate::domain::{Outcome, Status};
use crate::hash::validate_name_within;
use crate::object::{context_uri, include_uri};
use crate::operation::deploy::{
    CancelFlag, VERSIONS_ACCEPT, activation_request_xml, activation_status, under,
    worklist_request_xml,
};
use crate::operation::inactive::{InactiveEntry, fetch_inactive, inactive_author};
use crate::operation::quality::{QualityError, check_client};
use crate::transport::{Request, Response, SadtTransport, TransportError};
use std::fmt;
use std::str::FromStr;

/// The types `activate` accepts, as `inactive` prints them.
pub const ACCEPTED: &str = "PROG/P, PROG/I, CLAS/OC, INTF/OI, FUGR/FF";
/// The most objects one call activates.
pub const MAX_OBJECTS: usize = 50;
const GROUPS_PREFIX: &str = "/sap/bc/adt/functions/groups/";

/// An activatable type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivateKind {
    Program,
    /// A program include, activated under its main program.
    Include,
    Class,
    Interface,
    FunctionModule,
}

impl ActivateKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Program => "PROG/P",
            Self::Include => "PROG/I",
            Self::Class => "CLAS/OC",
            Self::Interface => "INTF/OI",
            Self::FunctionModule => "FUGR/FF",
        }
    }

    fn parse(token: &str) -> Result<Self, String> {
        match token.to_ascii_uppercase().as_str() {
            "PROG/P" => Ok(Self::Program),
            "PROG/I" => Ok(Self::Include),
            "CLAS/OC" => Ok(Self::Class),
            "INTF/OI" => Ok(Self::Interface),
            "FUGR/FF" => Ok(Self::FunctionModule),
            "FUGR/F" => Err(
                "FUGR/F is refused: activate the function modules (FUGR/FF) of the group instead"
                    .to_owned(),
            ),
            _ => Err(format!(
                "type {token:?} is not activated here (accepted: {ACCEPTED}); DDIC objects go through the DDIC deploy"
            )),
        }
    }

    fn max_name(self) -> usize {
        match self {
            Self::Program | Self::Include => 40,
            Self::Class | Self::Interface | Self::FunctionModule => 30,
        }
    }
}

/// One `TYPE/SUBTYPE:NAME` argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivateTarget {
    pub kind: ActivateKind,
    /// Upper case.
    pub name: String,
}

impl ActivateTarget {
    pub fn token(&self) -> String {
        format!("{}:{}", self.kind.token(), self.name)
    }
}

impl FromStr for ActivateTarget {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let (kind, name) = text
            .split_once(':')
            .filter(|(_, name)| !name.is_empty())
            .ok_or_else(|| format!("activate target must be TYPE:NAME ({ACCEPTED}): {text:?}"))?;
        let kind = ActivateKind::parse(kind)?;
        let name = name.to_ascii_uppercase();

        validate_name_within(&name, kind.max_name())?;

        if name.contains('/') {
            return Err(format!("a namespaced name is not activated: {name}"));
        }

        if !name.starts_with(['Z', 'Y']) {
            return Err(format!("only Z and Y objects are activated: {name}"));
        }

        Ok(Self { kind, name })
    }
}

impl fmt::Display for ActivateTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.token())
    }
}

/// The local rules of a batch, before any connection: one to [`MAX_OBJECTS`] objects, none twice.
pub fn check_targets(targets: &[ActivateTarget]) -> Result<(), String> {
    if targets.is_empty() {
        return Err("name at least one object (TYPE:NAME)".to_owned());
    }

    if targets.len() > MAX_OBJECTS {
        return Err(format!("at most {MAX_OBJECTS} objects per call"));
    }

    for (index, target) in targets.iter().enumerate() {
        if targets[..index].contains(target) {
            return Err(format!("{target} is named twice"));
        }
    }

    Ok(())
}

/// What a target resolved to: the URI the activation request names.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Resolved {
    target: ActivateTarget,
    uri: String,
}

/// What `run` needs besides the targets.
pub struct ActivateInput<'a> {
    pub targets: &'a [ActivateTarget],
    pub client: &'a str,
    pub user: &'a str,
    pub simulate: bool,
}

fn activation_post(preaudit: bool, body: String) -> Request {
    let mut request = Request::post(
        format!("/sap/bc/adt/activation?method=activate&preauditRequested={preaudit}"),
        body.into_bytes(),
    );

    request
        .headers
        .insert("Content-Type".to_owned(), "application/xml".to_owned());
    request
        .headers
        .insert("Accept".to_owned(), "application/xml".to_owned());

    request
}

/// The function group and module of an `FUGR/FF` entry, from its URI.
fn function_module_uri(entry: &InactiveEntry, name: &str) -> Option<String> {
    let path = entry.uri.split(['?', '#']).next().unwrap_or_default();
    let rest = path.strip_prefix(GROUPS_PREFIX)?;
    let (group, module) = rest.split_once("/fmodules/")?;
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };

    (plain(group) && plain(module) && module.eq_ignore_ascii_case(name)).then(|| path.to_owned())
}

/// The version feed of an object, which names the author of its inactive version.
fn feed_uri(resolved: &Resolved) -> Option<String> {
    let name = &resolved.target.name;

    match resolved.target.kind {
        ActivateKind::Program => ObjectKind::Prog.versions_uri(name, None, None).ok(),
        ActivateKind::Class => ObjectKind::Clas.versions_uri(name, None, None).ok(),
        ActivateKind::Interface => ObjectKind::Intf.versions_uri(name, None, None).ok(),
        ActivateKind::Include => Some(format!("{}/source/main/versions", include_uri(name))),
        ActivateKind::FunctionModule => {
            let path = resolved.uri.strip_prefix(GROUPS_PREFIX)?;
            let (group, _) = path.split_once("/fmodules/")?;

            ObjectKind::Func.versions_uri(name, Some(group), None).ok()
        }
    }
}

/// The owner of an entry that names none: the author of the inactive version in its feed.
fn owner_from_feed(
    transport: &mut dyn SadtTransport,
    resolved: &Resolved,
) -> Result<Option<String>, QualityError> {
    let Some(uri) = feed_uri(resolved) else {
        return Ok(None);
    };
    let mut request = Request::get(uri);

    request
        .headers
        .insert("Accept".to_owned(), VERSIONS_ACCEPT.to_owned());

    let response = transport
        .request(request)
        .map_err(|error| QualityError::from_transport(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(QualityError::from_transport(&error));
    }

    if response.status != 200 {
        return Ok(None);
    }

    Ok(inactive_author(&String::from_utf8_lossy(&response.body)).unwrap_or(None))
}

/// Checks every target against the inactive list and the rules; any problem rejects the batch.
fn resolve(
    transport: &mut dyn SadtTransport,
    targets: &[ActivateTarget],
    user: &str,
) -> Result<Vec<(Resolved, String)>, QualityError> {
    let entries = fetch_inactive(transport, false)?;
    let mut resolved = Vec::new();
    let mut problems = Vec::new();

    for target in targets {
        let entry = entries.iter().find(|entry| {
            entry.kind.eq_ignore_ascii_case(target.kind.token())
                && entry.name.eq_ignore_ascii_case(&target.name)
        });
        let Some(entry) = entry else {
            problems.push(format!("{target} is not in the inactive list"));

            continue;
        };

        if entry.deleted {
            problems.push(format!(
                "{target} is marked deleted in the inactive list; activating it would complete the \
                 deletion"
            ));

            continue;
        }

        let uri = match target.kind {
            ActivateKind::Program => Ok(prog_uri(&target.name)),
            ActivateKind::Class => ObjectKind::Clas.object_uri(&target.name, None),
            ActivateKind::Interface => ObjectKind::Intf.object_uri(&target.name, None),
            ActivateKind::Include => entry
                .main_program()
                .map(|main| context_uri(&target.name, &main))
                .ok_or_else(|| "its main program is unknown".to_owned()),
            ActivateKind::FunctionModule => function_module_uri(entry, &target.name)
                .ok_or_else(|| "its function module URI is not recognised".to_owned()),
        };
        let uri = match uri {
            Ok(uri) => uri,
            Err(reason) => {
                problems.push(format!("{target}: {reason}"));

                continue;
            }
        };
        let candidate = Resolved {
            target: target.clone(),
            uri,
        };
        let owner = match &entry.user {
            Some(owner) => Some(owner.clone()),
            None => owner_from_feed(transport, &candidate)?,
        };

        match owner {
            Some(owner) if owner.eq_ignore_ascii_case(user) => resolved.push((candidate, owner)),
            Some(owner) => problems.push(format!(
                "{target} belongs to {owner}, not to {user}; only own objects are activated"
            )),
            None => problems.push(format!(
                "{target}: the owner of the inactive version is unknown; only own objects are activated"
            )),
        }
    }

    if problems.is_empty() {
        Ok(resolved)
    } else {
        Err(QualityError::rejected(format!(
            "{}; nothing was activated",
            problems.join("; ")
        )))
    }
}

/// What one activation left: the messages SAP printed and, when it failed, why.
struct Activation {
    messages: Vec<String>,
    problem: Option<String>,
}

impl Activation {
    fn failed(messages: Vec<String>, problem: impl Into<String>) -> Self {
        Self {
            messages,
            problem: Some(problem.into()),
        }
    }
}

/// The reply of an activation POST as a verdict or `None` when only the proof can tell.
fn judge(response: &Response) -> Option<Activation> {
    let text = String::from_utf8_lossy(&response.body);
    let messages = format_messages(&text);

    if !(200..300).contains(&response.status) {
        return Some(Activation::failed(
            messages,
            format!("activation failed with HTTP {}", response.status),
        ));
    }

    activation_status(response.status, &response.body)
        .map(|_| Activation::failed(messages, "activation reported errors"))
}

fn post(
    transport: &mut dyn SadtTransport,
    preaudit: bool,
    body: String,
) -> Result<Response, Activation> {
    transport
        .request(activation_post(preaudit, body))
        .map_err(|error: TransportError| {
            Activation::failed(
                Vec::new(),
                if error.was_sent() {
                    format!("the activation was sent and its reply is unknown ({error})")
                } else {
                    format!("the activation was not sent ({error})")
                },
            )
        })
}

fn activate_one(transport: &mut dyn SadtTransport, resolved: &Resolved) -> Activation {
    let name = &resolved.target.name;
    let first = match post(transport, true, activation_request_xml(name, &resolved.uri)) {
        Ok(response) => response,
        Err(failed) => return failed,
    };
    let text = String::from_utf8_lossy(&first.body).into_owned();
    let is_worklist = (200..300).contains(&first.status)
        && classify_activation(&text) == ActivationBody::Worklist;

    if !is_worklist {
        return judge(&first).unwrap_or(Activation {
            messages: format_messages(&text),
            problem: None,
        });
    }

    // A transportable class answers a worklist and activates nothing: the second request sends
    // every object of the list, none outside the class, without the pre-audit.
    if resolved.target.kind != ActivateKind::Class {
        return Activation::failed(
            Vec::new(),
            "the activation answered a worklist and activated nothing",
        );
    }

    let references = match parse_object_refs(&text) {
        Ok(references) if !references.is_empty() => references,
        Ok(_) => {
            return Activation::failed(Vec::new(), "the activation worklist lists no object");
        }
        Err(error) => {
            return Activation::failed(
                Vec::new(),
                format!("cannot read the activation worklist: {error}"),
            );
        }
    };

    if let Some(outside) = references.iter().find(|object| {
        !under(
            &resolved.uri,
            object.uri.split(['?', '#']).next().unwrap_or(""),
        )
    }) {
        return Activation::failed(
            Vec::new(),
            format!(
                "the activation worklist lists an object outside the class, nothing was resent: {}",
                outside.uri
            ),
        );
    }

    let second = match post(transport, false, worklist_request_xml(&references)) {
        Ok(response) => response,
        Err(failed) => return failed,
    };
    let second_text = String::from_utf8_lossy(&second.body).into_owned();

    if (200..300).contains(&second.status)
        && classify_activation(&second_text) == ActivationBody::Worklist
    {
        return Activation::failed(
            Vec::new(),
            "the class was not activated after the worklist was resent",
        );
    }

    judge(&second).unwrap_or(Activation {
        messages: format_messages(&second_text),
        problem: None,
    })
}

/// The `activate` command and its simulation.
pub fn run(
    transport: &mut dyn SadtTransport,
    input: &ActivateInput<'_>,
    cancel: &CancelFlag,
) -> Result<Outcome, QualityError> {
    check_client(transport, input.client)?;

    let resolved = resolve(transport, input.targets, input.user)?;

    if input.simulate {
        let mut lines: Vec<String> = resolved
            .iter()
            .map(|(item, owner)| format!("SIMULATED_OK {} user {owner}", item.target))
            .collect();

        lines.push(format!("SUMMARY: {} SIMULATED_OK", resolved.len()));

        return Ok(Outcome {
            status: Some(Status::SimulatedOk),
            exit: 0,
            stdout: lines.join("\n"),
            stderr: String::new(),
        });
    }

    let mut results: Vec<(&Resolved, Activation)> = Vec::new();

    {
        let _write = cancel.hold_write();

        for (item, _) in &resolved {
            if cancel.is_set() {
                results.push((
                    item,
                    Activation::failed(Vec::new(), "not attempted: interrupted"),
                ));

                continue;
            }

            results.push((item, activate_one(transport, item)));
        }
    }

    let proof = fetch_inactive(transport, false);
    let mut lines = Vec::new();
    let mut active = 0;

    for (item, mut activation) in results {
        let listed = proof.as_ref().map(|entries| {
            entries.iter().any(|entry| {
                entry.kind.eq_ignore_ascii_case(item.target.kind.token())
                    && entry.name.eq_ignore_ascii_case(&item.target.name)
            })
        });

        if activation.problem.is_none() {
            match &listed {
                Ok(false) => {}
                Ok(true) => {
                    activation.problem = Some(
                        "activation reported no error but the object is still inactive".to_owned(),
                    );
                }
                Err(error) => {
                    activation.problem = Some(format!(
                        "activated, but the inactive list cannot be read to prove it ({})",
                        crate::operation::quality::describe(error)
                    ));
                }
            }
        }

        lines.extend(activation.messages);

        match activation.problem {
            None => {
                active += 1;
                lines.push(format!("ACTIVE {}", item.target));
            }
            Some(problem) => lines.push(format!("INACTIVE {}: {problem}", item.target)),
        }
    }

    let failed = resolved.len() - active;

    lines.push(format!("SUMMARY: {active} ACTIVE, {failed} INACTIVE"));

    let interrupted = cancel.is_set();

    Ok(Outcome {
        status: Some(if failed == 0 {
            Status::Active
        } else {
            Status::Inactive
        }),
        exit: match (failed, interrupted) {
            (_, true) => 130,
            (0, false) => 0,
            _ => 1,
        },
        stdout: lines.join("\n"),
        stderr: String::new(),
    })
}
