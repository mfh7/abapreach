//! The shared delete operation: a simulation that writes a plan, and a real delete that only
//! accepts a plan whose identity still matches the live objects. The CLI and the MCP tools both
//! call it; the ADT requests behind it belong to a [`DeleteBackend`].
//!
//! Delete never retries and never rolls back: a lost reply is reconciled once from a new session,
//! and the batch stops on any outcome it cannot settle.

use crate::cli::{DeleteKind, DeleteTarget};
use crate::domain::Outcome;
use crate::hash::{source_hash, validate_name_within};
use crate::object::check_request;
use crate::policy::{Confirmer, Decision, ForeignInactive, InactiveVersion, Policies};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

const NO_ACTIVE: &str = "NO_ACTIVE";
const PLAN_CHANGED: &str = "Plan changed since simulate; simulate again";
const LOCK_MAY_REMAIN: &str = "W: a lock may remain on the object";
const GUARDS_ACTIVE: &str = "guards active: Z/Y scope, self-guard, lock request agreement at execute, \
request owner when the lock names it, foreign_inactive (policy {policy}), plan hash";
const GUARDS_ABSENT: &str = "guards not applied on the ADT route: system change option, \
client CCNOCLIIND, SE06/software component, server-side self-guard";

/// The `check_delete_self` patterns of `YCL_UTILS_SOURCE_DEPLOY`: `gc_self_fg`,
/// `gc_self_class_cp` and `gc_self_ddic_cp`.
const SELF_GROUP: &str = "YFG_UTILS";
const SELF_CLASS_PREFIXES: [&str; 2] = ["YCL_UTILS_SOURCE_DEPLOY", "YCL_UTILS_"];

impl DeleteKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Prog => "PROG",
            Self::Fugr => "FUGR",
            Self::Func => "FUNC",
            Self::Clas => "CLAS",
            Self::Intf => "INTF",
        }
    }

    /// The SAP name limit of the type.
    pub fn max_name(self) -> usize {
        match self {
            Self::Prog => 40,
            Self::Fugr => 26,
            Self::Func | Self::Clas | Self::Intf => 30,
        }
    }

    /// Execution order: a function module before its group, a class before the program it
    /// includes.
    fn rank(self) -> u8 {
        match self {
            Self::Func => 1,
            Self::Clas => 2,
            Self::Intf => 3,
            Self::Prog => 4,
            Self::Fugr => 5,
        }
    }
}

impl fmt::Display for DeleteKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.token())
    }
}

/// The active source of one part of an object, or the marker that it has none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFact {
    pub label: String,
    /// `None` is an object without an active version (`NO_ACTIVE`).
    pub active: Option<Vec<String>>,
}

impl SourceFact {
    pub fn active(label: &str, lines: &[&str]) -> Self {
        Self {
            label: label.to_owned(),
            active: Some(lines.iter().map(|line| (*line).to_owned()).collect()),
        }
    }

    pub fn no_active(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            active: None,
        }
    }

    fn marker(&self) -> String {
        let hash = self
            .active
            .as_deref()
            .map_or_else(|| NO_ACTIVE.to_owned(), source_hash);

        format!("SOURCE {} {hash}", self.label)
    }
}

/// Who holds the transport lock on an object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrLock {
    /// A local object: no request.
    Local,
    /// Transportable and not locked: the request comes from `--tr`.
    Unlocked,
    /// Locked in one request; `own` is true when the user owns the request or one of its tasks.
    Locked {
        request: String,
        owner: String,
        own: bool,
    },
    Multiple(Vec<String>),
}

/// The inactive version of an object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InactiveFact {
    Absent,
    Owned(String),
    /// The version list could not be read.
    Unknown(String),
}

/// What a backend read about one object, read-only. The plan identity is made of these facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectFacts {
    pub package: String,
    pub author: String,
    /// The function group of a FUNC.
    pub group: Option<String>,
    /// Active source per part: a class has its main source and every local include that exists;
    /// a function group has none.
    pub sources: Vec<SourceFact>,
    /// What goes with the object: `FUNC <name>` per function module of a group, `INCLUDE <name>`
    /// per local include of a class.
    pub contents: Vec<String>,
    /// Reported, never hashed.
    pub where_used: Vec<String>,
    pub inactive: InactiveFact,
    pub tr: TrLock,
}

/// What the delete request itself answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeleteReply {
    Deleted,
    NotFound,
    Rejected(String),
    /// SAP reports a partly executed delete, such as a function group with missing members.
    Partial(String),
    /// The request was sent and no answer is known: timeout, dropped reply, disconnect.
    Uncertain(String),
}

/// What the read-only look from a new session found after an uncertain delete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reconcile {
    Gone,
    StillExists,
    Failed(String),
}

/// The ADT side of delete. Task 6 provides the real implementation.
pub trait DeleteBackend {
    /// `Ok(None)` is an object that does not exist.
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String>;

    /// One delete request, never repeated by the caller.
    fn delete(&mut self, kind: DeleteKind, name: &str, tr: Option<&str>) -> DeleteReply;

    /// A read-only look from a new session.
    fn reconcile(&mut self, kind: DeleteKind, name: &str) -> Reconcile;

    /// Whether the `PROG` is an include (`subc I`), which runs after the programs of the batch:
    /// SAP refuses to delete an include that a program still references. A backend that cannot
    /// tell, or whose read fails (the failure surfaces when the object is inspected), answers no.
    fn is_include(&mut self, _kind: DeleteKind, _name: &str) -> bool {
        false
    }

    /// The backend cannot go on (no session for the next row): the batch stops after the current
    /// row and the rest is `SKIPPED`.
    fn halted(&self) -> bool {
        false
    }

    /// The batch is about to start: its rows in execution order (before the include order of
    /// `PROG`) and the normalized `--tr`. An `Err` is a STOP before the first object. A backend
    /// with a batch-wide state takes it here.
    fn begin(&mut self, _rows: &[(DeleteKind, String)], _tr: Option<&str>) -> Result<(), String> {
        Ok(())
    }

    /// The exit code and the line for an `Err` that `begin` returned. A `STOP` (exit 2) unless the
    /// backend could not reach SAP, which is a failed request (exit 1) as for create and read.
    fn begin_failure(&self, message: &str) -> (i32, String) {
        (2, format!("STOP: {message}"))
    }

    /// The backend that handles the kind. A batch that mixes backends prints it per object and
    /// records it in the plan; a backend that does not name itself answers `None`.
    fn backend_label(&self, _kind: DeleteKind) -> Option<String> {
        None
    }
}

/// The simulated plan: the Python plan file, `{conn, tr, objects: [[kind, name, hash]]}`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Plan {
    pub conn: String,
    pub tr: Option<String>,
    pub objects: Vec<(String, String, String)>,
    /// `[kind, name, backend]` per object, only for a batch that mixes backends: the delete runs
    /// each object on the one it was simulated on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backends: Vec<(String, String, String)>,
}

impl Plan {
    /// The backend recorded for the object.
    pub fn backend_of(&self, kind: DeleteKind, name: &str) -> Option<&str> {
        self.backends
            .iter()
            .find(|(recorded, known, _)| recorded == kind.token() && known == name)
            .map(|(_, _, backend)| backend.as_str())
    }
}

/// Reads the plan file of a real delete.
pub fn load_plan(path: &Path) -> Result<Plan, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("plan file cannot be read: {}: {error}", path.display()))
        .and_then(|text| {
            serde_json::from_str::<Plan>(&text)
                .map_err(|error| format!("plan file cannot be read: {}: {error}", path.display()))
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteStatus {
    SimulatedOk,
    Deleted,
    /// The reply was lost, but a new session no longer finds the object. Still exit 1.
    DeletedReconciled,
    NotFound,
    Rejected,
    Skipped,
    Partial,
}

impl DeleteStatus {
    pub fn token(self) -> &'static str {
        match self {
            Self::SimulatedOk => "SIMULATED_OK",
            Self::Deleted => "DELETED",
            Self::DeletedReconciled => "DELETED (reconciled)",
            Self::NotFound => "NOT_FOUND",
            Self::Rejected => "REJECTED",
            Self::Skipped => "SKIPPED",
            Self::Partial => "PARTIAL",
        }
    }

    fn is_ok(self) -> bool {
        matches!(self, Self::SimulatedOk | Self::Deleted)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectResult {
    pub kind: DeleteKind,
    pub name: String,
    pub status: DeleteStatus,
    pub hash: Option<String>,
    pub reason: Option<String>,
    /// What goes with the object: `FUNC <name>` per function module of a group, `INCLUDE <name>`
    /// per local include of a class. Set by a simulation.
    pub contents: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DeleteReport {
    pub results: Vec<ObjectResult>,
    /// Set by a simulation: the objects that are `SIMULATED_OK`.
    pub plan: Option<Plan>,
    pub outcome: Outcome,
}

impl DeleteReport {
    /// The condition for an MCP token: every object, not just most, is `SIMULATED_OK`.
    pub fn all_simulated_ok(&self) -> bool {
        !self.results.is_empty()
            && self
                .results
                .iter()
                .all(|result| result.status == DeleteStatus::SimulatedOk)
    }
}

pub struct Context<'a> {
    pub conn: &'a str,
    pub user: &'a str,
    pub policies: &'a Policies,
    pub confirmer: &'a dyn Confirmer,
}

type Row = (DeleteKind, String);

/// Validates the arguments, upper-cases names and orders them for execution. Every `Err` is the
/// text of a `STOP`.
pub(crate) fn prepare(
    targets: &[DeleteTarget],
    tr: Option<&str>,
) -> Result<(Vec<Row>, Option<String>), String> {
    let mut rows: Vec<Row> = Vec::new();

    for target in targets {
        validate_name_within(&target.name, target.kind.max_name())?;

        let name = target.name.to_ascii_uppercase();

        if rows
            .iter()
            .any(|(kind, known)| *kind == target.kind && *known == name)
        {
            return Err(format!("duplicate object: {}:{name}", target.kind));
        }

        rows.push((target.kind, name));
    }

    if let Some(request) = tr {
        check_request(request)?;
    }

    rows.sort_by_key(|(kind, _)| kind.rank());

    Ok((rows, tr.map(str::to_ascii_uppercase)))
}

/// The `check_delete_self` baseline plus the spec's `YFM_UTILS_*` function modules. `group` is
/// the function group of a FUNC, once known.
fn is_self(kind: DeleteKind, name: &str, group: Option<&str>) -> bool {
    let class = || {
        SELF_CLASS_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
    };

    match kind {
        DeleteKind::Fugr => name == SELF_GROUP,
        DeleteKind::Func => {
            name.starts_with("YFM_UTILS_") || group.is_some_and(|group| group == SELF_GROUP)
        }
        DeleteKind::Clas => class(),
        DeleteKind::Prog => name == "SAPLYFG_UTILS" || name.starts_with("LYFG_UTILS") || class(),
        DeleteKind::Intf => false,
    }
}

/// Within `PROG` a program (subc 1) runs before an include. The order is only asked for when the
/// batch holds two `PROG` rows, so a single `PROG` costs no extra read.
fn programs_before_includes(backend: &mut dyn DeleteBackend, rows: &mut [Row]) {
    if rows
        .iter()
        .filter(|(kind, _)| *kind == DeleteKind::Prog)
        .count()
        < 2
    {
        return;
    }

    let mut keyed: Vec<(u8, bool, Row)> = rows
        .iter()
        .map(|(kind, name)| {
            let include = *kind == DeleteKind::Prog && backend.is_include(*kind, name);

            (kind.rank(), include, (*kind, name.clone()))
        })
        .collect();

    keyed.sort_by_key(|(rank, include, _)| (*rank, *include));

    for (slot, (_, _, row)) in rows.iter_mut().zip(keyed) {
        *slot = row;
    }
}

/// One reported user of the object. A transaction is `TRAN <tcode>` (the line the RFC path
/// prints as well) and a `W:` entry is already a warning line; anything else is a calling object.
fn where_used_line(entry: &str) -> String {
    if entry.starts_with("W: ") {
        entry.to_owned()
    } else if entry.starts_with("TRAN ") {
        format!("WHERE-USED {entry}")
    } else {
        format!("WHERE-USED: {entry}")
    }
}

fn in_scope(name: &str) -> bool {
    name.starts_with(['Z', 'Y'])
}

enum Eval {
    Rejected(String),
    NotFound,
    Ready(Ready),
}

struct Ready {
    lines: Vec<String>,
    hash: String,
    tr: Option<String>,
    contents: Vec<String>,
}

fn self_rejection(kind: DeleteKind, name: &str) -> String {
    format!("Self-guard: {kind} {name} belongs to this deploy tool; delete rejected")
}

fn inactive_decision(context: &Context, name: &str, inactive: &InactiveFact) -> Decision {
    let state = match inactive {
        InactiveFact::Absent => InactiveVersion::Absent,
        InactiveFact::Owned(owner) => InactiveVersion::Owned { owner },
        InactiveFact::Unknown(reason) => InactiveVersion::Unknown { reason },
    };

    // The shared guard prompts with the size of a deploy; a delete has its own question.
    if let (ForeignInactive::Ask, InactiveFact::Owned(owner)) =
        (context.policies.foreign_inactive, inactive)
        && !owner.eq_ignore_ascii_case(context.user)
    {
        let exists = format!("inactive version of another user exists: {owner}");

        if !context.confirmer.interactive() {
            return Decision::Refuse(format!("{exists} (policy ask needs a terminal)"));
        }

        let prompt = format!(
            "{name}: inactive version of {owner} would be deleted with the object. Delete? [y/N] "
        );

        return if context.confirmer.confirm(&prompt) {
            Decision::Continue(Some(format!(
                "W: deleting the inactive version of {owner} (confirmed at the prompt)"
            )))
        } else {
            Decision::Refuse(format!("{exists} (declined at the prompt)"))
        };
    }

    context
        .policies
        .inactive_version(state, context.user, None, context.confirmer)
}

/// The transport request the delete uses, or why there is none.
fn resolve_tr(
    lock: &TrLock,
    tr: Option<&str>,
    lines: &mut Vec<String>,
) -> Result<Option<String>, String> {
    match lock {
        TrLock::Local => {
            if let Some(request) = tr {
                lines.push(format!("W: local object; --tr {request} ignored"));
            }

            Ok(None)
        }
        // ADT simulation is read-only, so the owning request only becomes known from the
        // execute-time LOCK reply. An explicit request is checked against that reply later.
        TrLock::Unlocked => {
            if tr.is_none() {
                lines.push(
                    "TR: decided by the lock at execute; simulation cannot see the owning request"
                        .to_owned(),
                );
            }

            Ok(tr.map(str::to_owned))
        }
        TrLock::Locked {
            request,
            owner,
            own,
        } => {
            if !own {
                return Err(format!(
                    "object locked in {request} owned by {owner}; delete into a request of \
                     another user is not supported"
                ));
            }

            if let Some(given) = tr
                && given != request
            {
                lines.push(format!(
                    "W: --tr {given} ignored: object locked in {request}"
                ));
            }

            Ok(Some(request.clone()))
        }
        TrLock::Multiple(requests) => Err(format!(
            "object locked in more than one request: {}",
            requests.join(" ")
        )),
    }
}

fn evaluate(
    backend: &mut dyn DeleteBackend,
    context: &Context,
    row: &Row,
    tr: Option<&str>,
    batch: &[Row],
) -> Eval {
    let (kind, name) = (row.0, row.1.as_str());

    // The tool's own SAPL/LYFG programs are outside Z/Y too, but the self-guard names them.
    if is_self(kind, name, None) {
        return Eval::Rejected(self_rejection(kind, name));
    }

    if !in_scope(name) {
        return Eval::Rejected(format!("{kind} {name} is outside the Z/Y scope"));
    }

    let facts = match backend.inspect(kind, name) {
        Ok(Some(facts)) => facts,
        Ok(None) => return Eval::NotFound,
        Err(error) => return Eval::Rejected(format!("cannot read {kind} {name}: {error}")),
    };

    if kind == DeleteKind::Func {
        let Some(group) = facts.group.as_deref().filter(|group| !group.is_empty()) else {
            return Eval::Rejected(format!("function group of {name} is unknown"));
        };
        let group = group.to_ascii_uppercase();

        if !in_scope(&group) {
            return Eval::Rejected(format!(
                "function group {group} of {name} is outside the Z/Y scope"
            ));
        }

        if is_self(kind, name, Some(&group)) {
            return Eval::Rejected(self_rejection(kind, name));
        }
    }

    let mut lines = Vec::new();

    match inactive_decision(context, name, &facts.inactive) {
        Decision::Refuse(reason) => return Eval::Rejected(reason),
        Decision::Continue(warning) => lines.extend(warning),
    }

    let tr = match resolve_tr(&facts.tr, tr, &mut lines) {
        Ok(tr) => tr,
        Err(reason) => return Eval::Rejected(reason),
    };
    let object = format!(
        "DELETE {kind} {name} package {} author {} TR {}",
        facts.package,
        facts.author,
        tr.as_deref().unwrap_or("-")
    );
    let mut hashed = vec![object.clone()];
    let mut shown = vec![object];

    for content in &facts.contents {
        shown.push(format!("CONTENT {content}"));

        // A member deleted in the same batch is not a changed plan when it is gone later.
        let in_batch = content
            .split_once(' ')
            .is_some_and(|(content_kind, content_name)| {
                batch
                    .iter()
                    .any(|(kind, name)| kind.token() == content_kind && name == content_name)
            });

        if !in_batch {
            hashed.push(format!("CONTENT {content}"));
        }
    }

    for source in &facts.sources {
        hashed.push(source.marker());
        shown.push(source.marker());
    }

    shown.extend(facts.where_used.iter().map(|entry| where_used_line(entry)));
    lines.extend(shown);

    Eval::Ready(Ready {
        lines,
        hash: sha256_hex(&hashed.join("\n")),
        tr,
        contents: facts.contents.clone(),
    })
}

pub(crate) fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct Batch {
    mode: &'static str,
    results: Vec<ObjectResult>,
    out: Vec<String>,
    stopped: bool,
}

impl Batch {
    fn new(mode: &'static str) -> Self {
        Self {
            mode,
            results: Vec::new(),
            out: Vec::new(),
            stopped: false,
        }
    }

    fn start(&mut self, row: &Row) {
        self.out.push(format!("== {} {} ==", row.0, row.1));
    }

    fn finish(
        &mut self,
        row: &Row,
        status: DeleteStatus,
        hash: Option<String>,
        reason: Option<String>,
    ) {
        if let (DeleteStatus::SimulatedOk, Some(hash)) = (status, &hash) {
            self.out.push(format!("plan hash {hash}"));
        }

        self.out.push(format!("{}: {}", self.mode, status.token()));
        self.results.push(ObjectResult {
            kind: row.0,
            name: row.1.clone(),
            status,
            hash,
            reason,
            contents: Vec::new(),
        });
    }

    fn fail(&mut self, row: &Row, status: DeleteStatus, reason: String) {
        if status == DeleteStatus::Rejected {
            self.out.push(format!("ERROR: {reason}"));
        }

        self.finish(row, status, None, Some(reason));
    }

    fn skip_rest(&mut self, rows: &[Row]) {
        for (kind, name) in rows {
            self.results.push(ObjectResult {
                kind: *kind,
                name: name.clone(),
                status: DeleteStatus::Skipped,
                hash: None,
                reason: None,
                contents: Vec::new(),
            });
        }
    }

    fn report(mut self, plan: Option<Plan>) -> DeleteReport {
        let ok = self
            .results
            .iter()
            .filter(|result| result.status.is_ok())
            .count();
        let skipped = self
            .results
            .iter()
            .filter(|result| result.status == DeleteStatus::Skipped)
            .count();
        let failed = self.results.len() - ok - skipped;

        self.out.push(format!(
            "SUMMARY: {ok} OK, {failed} FAILED, {skipped} SKIPPED"
        ));

        for result in &self.results {
            match result.status {
                DeleteStatus::Skipped => {
                    self.out
                        .push(format!("SKIPPED {} {}", result.kind, result.name));
                }
                status if !status.is_ok() => self.out.push(format!(
                    "FAILED {} {}: {}",
                    result.kind,
                    result.name,
                    result.reason.as_deref().unwrap_or(status.token())
                )),
                _ => {}
            }
        }

        DeleteReport {
            outcome: Outcome {
                status: None,
                exit: i32::from(ok != self.results.len()),
                stdout: format!("{}\n", self.out.join("\n")),
                stderr: String::new(),
            },
            results: self.results,
            plan,
        }
    }
}

/// Writes nothing to SAP: reads every object, applies the guards and computes the plan hash.
/// `Err` is the text of a `STOP`.
pub fn simulate(
    backend: &mut dyn DeleteBackend,
    context: &Context,
    targets: &[DeleteTarget],
    tr: Option<&str>,
) -> Result<DeleteReport, String> {
    let (mut rows, tr) = prepare(targets, tr)?;

    backend.begin(&rows, tr.as_deref())?;
    programs_before_includes(backend, &mut rows);

    let mut batch = Batch::new("DELETE_SIM");
    let mut plan = Plan {
        conn: context.conn.to_owned(),
        tr: tr.clone(),
        objects: Vec::new(),
        backends: Vec::new(),
    };

    batch
        .out
        .push(GUARDS_ACTIVE.replace("{policy}", context.policies.foreign_inactive.token()));
    batch.out.push(GUARDS_ABSENT.to_owned());

    for row in &rows {
        batch.start(row);

        match evaluate(backend, context, row, tr.as_deref(), &rows) {
            Eval::Rejected(reason) => batch.fail(row, DeleteStatus::Rejected, reason),
            Eval::NotFound => batch.finish(row, DeleteStatus::NotFound, None, None),
            Eval::Ready(ready) => {
                batch.out.extend(ready.lines);
                plan.objects
                    .push((row.0.token().to_owned(), row.1.clone(), ready.hash.clone()));

                batch.finish(row, DeleteStatus::SimulatedOk, Some(ready.hash), None);

                if let Some(result) = batch.results.last_mut() {
                    result.contents = ready.contents;
                }
            }
        }
    }

    Ok(batch.report(Some(plan)))
}

/// Deletes the objects of `plan`, one request each, in execution order. Every object is read
/// again first; one whose plan hash changed is `REJECTED`. `Err` is the text of a `STOP`.
pub fn execute(
    backend: &mut dyn DeleteBackend,
    context: &Context,
    targets: &[DeleteTarget],
    tr: Option<&str>,
    plan: &Plan,
) -> Result<DeleteReport, String> {
    execute_until(backend, context, targets, tr, plan, &mut || false)
}

/// Like [`execute`], but `stop` is asked before each object: once it says yes, that object and
/// the rest are `SKIPPED`. A delete request in flight is never interrupted.
pub fn execute_until(
    backend: &mut dyn DeleteBackend,
    context: &Context,
    targets: &[DeleteTarget],
    tr: Option<&str>,
    plan: &Plan,
    stop: &mut dyn FnMut() -> bool,
) -> Result<DeleteReport, String> {
    let (mut rows, tr) = prepare(targets, tr)?;
    let wanted: BTreeSet<(&str, &str)> = rows
        .iter()
        .map(|(kind, name)| (kind.token(), name.as_str()))
        .collect();
    let planned: BTreeSet<(&str, &str)> = plan
        .objects
        .iter()
        .map(|(kind, name, _)| (kind.as_str(), name.as_str()))
        .collect();

    if plan.conn != context.conn
        || plan.tr != tr
        || planned != wanted
        || planned.len() != plan.objects.len()
    {
        return Err(
            "plan does not match the arguments (conn/objects/--tr); simulate again".to_owned(),
        );
    }

    backend.begin(&rows, tr.as_deref())?;
    programs_before_includes(backend, &mut rows);

    let mut batch = Batch::new("DELETE");

    for (index, row) in rows.iter().enumerate() {
        if stop() {
            batch.out.push(format!(
                "W: stopped before {} {}: the time limit or a cancellation; the rest is SKIPPED",
                row.0, row.1
            ));
            batch.skip_rest(&rows[index..]);
            break;
        }

        batch.start(row);

        let ready = match evaluate(backend, context, row, tr.as_deref(), &rows) {
            Eval::Rejected(reason) => {
                batch.fail(row, DeleteStatus::Rejected, reason);

                if backend.halted() {
                    batch.skip_rest(&rows[index + 1..]);
                    break;
                }

                continue;
            }
            Eval::NotFound => {
                batch.finish(row, DeleteStatus::NotFound, None, None);
                continue;
            }
            Eval::Ready(ready) => ready,
        };
        let planned_hash = plan
            .objects
            .iter()
            .find(|(kind, name, _)| kind == row.0.token() && *name == row.1)
            .map(|(_, _, hash)| hash.as_str());

        if planned_hash != Some(ready.hash.as_str()) {
            batch.fail(row, DeleteStatus::Rejected, PLAN_CHANGED.to_owned());
            continue;
        }

        batch.out.extend(ready.lines);

        if stop() {
            batch.out.push(format!(
                "W: stopped before {} {}: the time limit or a cancellation; the rest is SKIPPED",
                row.0, row.1
            ));
            batch.skip_rest(&rows[index..]);
            break;
        }

        match backend.delete(row.0, &row.1, ready.tr.as_deref()) {
            DeleteReply::Deleted => batch.finish(row, DeleteStatus::Deleted, None, None),
            DeleteReply::NotFound => batch.finish(row, DeleteStatus::NotFound, None, None),
            DeleteReply::Rejected(reason) => batch.fail(row, DeleteStatus::Rejected, reason),
            DeleteReply::Partial(reason) => {
                batch.finish(
                    row,
                    DeleteStatus::Partial,
                    None,
                    Some(format!("status unknown: {reason}")),
                );
                batch.stopped = true;
            }
            DeleteReply::Uncertain(reason) => {
                settle_uncertain(backend, &mut batch, row, &reason);
                batch.stopped = true;
            }
        }

        if batch.stopped {
            batch.skip_rest(&rows[index + 1..]);
            break;
        }
    }

    Ok(batch.report(None))
}

/// One read-only look from a new session; the delete request is never sent again.
fn settle_uncertain(backend: &mut dyn DeleteBackend, batch: &mut Batch, row: &Row, reason: &str) {
    let (status, detail) = match backend.reconcile(row.0, &row.1) {
        Reconcile::Gone => (
            DeleteStatus::DeletedReconciled,
            format!(
                "reply lost ({reason}); a new session no longer finds the object: check before retrying"
            ),
        ),
        Reconcile::StillExists => (
            DeleteStatus::Partial,
            format!("status unknown: reply lost ({reason}); the object still exists"),
        ),
        Reconcile::Failed(failure) => (
            DeleteStatus::Partial,
            format!("status unknown: reply lost ({reason}); the check failed: {failure}"),
        ),
    };

    batch.out.push(format!("W: {detail}"));
    batch.out.push(LOCK_MAY_REMAIN.to_owned());
    batch.finish(row, status, None, Some(detail));
}

/// The CLI shape: `--simulate` writes the plan to `plan_path`, a real delete reads it.
pub fn run(
    backend: &mut dyn DeleteBackend,
    context: &Context,
    targets: &[DeleteTarget],
    tr: Option<&str>,
    simulate_only: bool,
    plan_path: &Path,
) -> Outcome {
    run_until(
        backend,
        context,
        targets,
        tr,
        simulate_only,
        plan_path,
        &mut || false,
    )
}

/// Like [`run`]; a real delete asks `stop` before each object, as [`execute_until`] does.
pub fn run_until(
    backend: &mut dyn DeleteBackend,
    context: &Context,
    targets: &[DeleteTarget],
    tr: Option<&str>,
    simulate_only: bool,
    plan_path: &Path,
    stop: &mut dyn FnMut() -> bool,
) -> Outcome {
    let result = if simulate_only {
        simulate(backend, context, targets, tr).and_then(|mut report| {
            if let Some(plan) = &report.plan {
                let json = serde_json::to_string_pretty(plan).map_err(|error| error.to_string())?;

                std::fs::write(plan_path, json).map_err(|error| {
                    format!(
                        "plan file cannot be written: {}: {error}",
                        plan_path.display()
                    )
                })?;
                report
                    .outcome
                    .stdout
                    .push_str(&format!("plan -> {}\n", plan_path.display()));
            }

            Ok(report)
        })
    } else {
        load_plan(plan_path)
            .and_then(|plan| execute_until(backend, context, targets, tr, &plan, stop))
    };

    match result {
        Ok(report) => report.outcome,
        Err(message) => {
            let (exit, stderr) = backend.begin_failure(&message);

            Outcome {
                status: None,
                exit,
                stdout: String::new(),
                stderr,
            }
        }
    }
}
