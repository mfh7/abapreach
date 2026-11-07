//! The ADT side of delete: the read-only facts behind a plan, the single lock-and-DELETE
//! sequence, and the read from a new session that settles an unknown outcome.
//!
//! The lock handle is not bound to a session and SAP echoes it in its 423 text, so it never leaves
//! this module: every message built here is scrubbed of it. A DELETE is sent once; whatever it
//! answers is classified, never retried.

use crate::adt::{
    Caller, USAGE_REQUEST_XML, VersionEntry, group_members, group_nodestructure_uri, package_of,
    parse_callers, parse_versions, responsible_of, usage_references_uri,
};
use crate::backend::Failure;
use crate::cli::{DeleteKind, IncludeKind, ObjectKind};
use crate::hash::local_source_lines;
use crate::object::check_request;
use crate::operation::delete::{
    DeleteBackend, DeleteReply, InactiveFact, ObjectFacts, Reconcile, SourceFact, TrLock,
};
use crate::operation::deploy::{
    CancelFlag, LOCK_ACCEPT, LockInfo, VERSIONS_ACCEPT, encode_lock_handle, leaf_texts, parse_lock,
};
use crate::operation::include::{ProgTarget, ResolveError, locate_properties};
use crate::operation::read::resolve_group;
use crate::transport::{Request, Response, SadtTransport, TransportError};
use std::collections::HashMap;

const LOCK_MAY_REMAIN: &str = "a lock may remain on the object";
pub const NO_STATEFUL_SESSION: &str = "ADT HTTP stateful session unavailable (no sap-contextid)";
const HANDLE_MASK: &str = "<lock handle>";
const MESSAGE_LIMIT: usize = 200;
/// The class includes that have a source of their own, with the suffix of their include name.
const INCLUDES: [(IncludeKind, &str); 4] = [
    (IncludeKind::Ccdef, "CCDEF"),
    (IncludeKind::Ccimp, "CCIMP"),
    (IncludeKind::Ccmac, "CCMAC"),
    (IncludeKind::Ccau, "CCAU"),
];
/// The width of a class name inside its include names.
const CLASS_INCLUDE_WIDTH: usize = 30;

/// Opens a new session for the read-only look that follows a delete.
pub type Opener<'a> = dyn FnMut() -> Result<Box<dyn SadtTransport>, TransportError> + 'a;

pub struct AdtDelete<'a> {
    transport: &'a mut dyn SadtTransport,
    /// The logon user: another user's inactive version outranks the user's own.
    user: String,
    open: &'a mut Opener<'a>,
    /// The session that replaced `transport` after a delete; `None` while `transport` is in use.
    renewed: Option<Box<dyn SadtTransport>>,
    /// A delete ended the active stateful session: the next row opens a new one first.
    renew: bool,
    /// Why the new session could not be opened; the batch stops there and nothing is retried.
    open_failure: Option<String>,
    cancel: Option<CancelFlag>,
    /// The first read that did not leave the process, for the router's fallback.
    unsent: Option<String>,
    /// A delete request has started: nothing can fall back to another backend any more.
    wrote: bool,
    /// `W:` lines for the fresh sessions whose logoff failed.
    close_notes: Vec<String>,
    /// What each `PROG` name resolved to (program or include), by upper-case name. Set when the
    /// object is first resolved, so that lock, DELETE and reconciliation use the URI that answered.
    resolved: HashMap<String, ProgTarget>,
}

impl<'a> AdtDelete<'a> {
    pub fn new(transport: &'a mut dyn SadtTransport, user: &str, open: &'a mut Opener<'a>) -> Self {
        Self {
            transport,
            user: user.to_owned(),
            open,
            renewed: None,
            renew: false,
            open_failure: None,
            cancel: None,
            unsent: None,
            wrote: false,
            close_notes: Vec::new(),
            resolved: HashMap::new(),
        }
    }

    /// Like [`Self::new`], while a write owns the interrupt scope.
    pub fn with_cancel(
        transport: &'a mut dyn SadtTransport,
        user: &str,
        open: &'a mut Opener<'a>,
        cancel: CancelFlag,
    ) -> Self {
        Self {
            transport,
            user: user.to_owned(),
            open,
            renewed: None,
            renew: false,
            open_failure: None,
            cancel: Some(cancel),
            unsent: None,
            wrote: false,
            close_notes: Vec::new(),
            resolved: HashMap::new(),
        }
    }

    /// The pre-send connectivity failure of a read when no write has started: the whole batch can
    /// then run again on another backend, because nothing was changed.
    /// A transport that keeps no stateful session cannot hold the lock, so the batch cannot run
    /// here at all, whether it simulates or deletes.
    pub fn unsent_failure(&self) -> Option<(Failure, String)> {
        if self.wrote {
            return None;
        }

        match &self.unsent {
            Some(message) => Some((Failure::PreSendConnectivity, message.clone())),
            None if self.active_ref().stateful_supported() == Some(false) => Some((
                Failure::CapabilityUnavailable,
                NO_STATEFUL_SESSION.to_owned(),
            )),
            None => None,
        }
    }

    /// Ends the session this backend opened after a delete, then returns the `W:` lines for the
    /// sessions whose logoff failed; they change no verdict.
    pub fn close_note(&mut self) -> Option<String> {
        if let Some(mut session) = self.renewed.take()
            && let Err(error) = session.close()
        {
            self.close_notes
                .push(format!("W: session close failed: {error}"));
        }

        (!self.close_notes.is_empty()).then(|| self.close_notes.join("\n"))
    }

    fn active_ref(&self) -> &dyn SadtTransport {
        match self.renewed.as_deref() {
            Some(session) => session,
            None => &*self.transport,
        }
    }

    /// The session the batch currently works in: the one the caller opened until a delete ends
    /// it, then the one opened for the following row.
    fn active(&mut self) -> &mut dyn SadtTransport {
        match self.renewed.as_deref_mut() {
            Some(session) => session,
            None => &mut *self.transport,
        }
    }

    /// Opens the session for the next row once a delete has ended the previous one. A failure is
    /// final: the batch stops and the open is not tried again.
    fn ensure_session(&mut self) -> Result<(), String> {
        if let Some(reason) = &self.open_failure {
            return Err(reason.clone());
        }

        if !self.renew {
            return Ok(());
        }

        match (self.open)() {
            Ok(session) => {
                self.renewed = Some(session);
                self.renew = false;

                Ok(())
            }
            Err(error) => {
                let reason = format!("cannot open a new session: {error}");

                self.open_failure = Some(reason.clone());

                Err(reason)
            }
        }
    }

    /// A DELETE answered 2xx: its stateful session is logged off and the next row gets a new one.
    /// SAP kills a session that reads an include whose program was just deleted.
    fn end_stateful_session(&mut self) {
        if self.active_ref().stateful_supported() != Some(true) {
            return;
        }

        if let Err(error) = self.active().close() {
            self.close_notes
                .push(format!("W: session close failed: {error}"));
        }

        self.renewed = None;
        self.renew = true;
    }

    fn get(&mut self, uri: &str, accept: &str) -> Result<Response, String> {
        let mut request = Request::get(uri);
        request
            .headers
            .insert("Accept".to_owned(), accept.to_owned());

        self.send(request)
    }

    fn send(&mut self, request: Request) -> Result<Response, String> {
        let response = self.active().request(request).map_err(|error| {
            if Failure::from(&error) == Failure::PreSendConnectivity {
                self.unsent.get_or_insert_with(|| error.to_string());
            }

            error.to_string()
        })?;

        match response.authorization_failure() {
            Some(error) => Err(error.to_string()),
            None => Ok(response),
        }
    }

    /// The function modules of a group. The group metadata links none, so the member listing is
    /// read; a failed read fails the plan rather than leaving the members out of it.
    fn members(&mut self, group: &str) -> Result<Vec<String>, String> {
        let mut request = Request::post(group_nodestructure_uri(group), Vec::new());
        request
            .headers
            .insert("Accept".to_owned(), "application/*".to_owned());
        let response = self.send(request)?;

        if response.status != 200 {
            return Err(format!("member listing: HTTP {}", response.status));
        }

        let xml = String::from_utf8(response.body)
            .map_err(|error| format!("member listing is not UTF-8: {error}"))?;

        group_members(&xml).map_err(|error| format!("member listing: {error}"))
    }

    /// The ADT URI of the object, and for a function module its group, which the server resolves
    /// first; `None` when the module is unknown.
    fn locate(
        &mut self,
        kind: DeleteKind,
        name: &str,
    ) -> Result<Option<(String, Option<String>)>, String> {
        let object = match kind {
            // The URI that answered 200 at inspect. A PROG that was never inspected here has no
            // answer to follow; it is addressed as a program.
            // shortcut: unresolved means program, safe because every batch path inspects first.
            DeleteKind::Prog => {
                let uri = self.resolved.get(&name.to_ascii_uppercase()).map_or_else(
                    || ProgTarget::program(name).uri,
                    |target| target.uri.clone(),
                );

                return Ok(Some((uri, None)));
            }
            DeleteKind::Clas => ObjectKind::Clas,
            DeleteKind::Intf => ObjectKind::Intf,
            DeleteKind::Fugr => return Ok(Some((group_uri(name), None))),
            DeleteKind::Extension(kind) => {
                return Err(format!("{} is not an ADT object", kind.token()));
            }
            DeleteKind::Func => {
                let Some(group) = self.group_of(name)? else {
                    return Ok(None);
                };

                return Ok(Some((
                    ObjectKind::Func.object_uri(name, Some(&group))?,
                    Some(group),
                )));
            }
        };

        Ok(Some((object.object_uri(name, None)?, None)))
    }

    /// The function group of a module; `None` when the search knows no such module.
    fn group_of(&mut self, name: &str) -> Result<Option<String>, String> {
        match resolve_group(self.active(), name, None) {
            Ok(group) => Ok(Some(group)),
            Err(error) if error.message.contains("returned no reference") => Ok(None),
            Err(error) => {
                if error.failure == Failure::PreSendConnectivity {
                    self.unsent.get_or_insert_with(|| error.message.clone());
                }

                Err(error.message)
            }
        }
    }

    fn source(&mut self, uri: &str) -> Result<Option<Vec<String>>, String> {
        let response = self.get(&format!("{uri}?version=active"), "text/plain")?;

        match response.status {
            200 => String::from_utf8(response.body)
                .map(|text| Some(local_source_lines(&text)))
                .map_err(|error| format!("source is not UTF-8: {error}")),
            404 => Ok(None),
            status => Err(format!("HTTP {status}")),
        }
    }

    /// The owner of the inactive version over `feeds`: another user's version wins, an unreadable
    /// feed is unknown, and only then does the user's own version count.
    fn inactive(&mut self, feeds: &[String]) -> InactiveFact {
        let mut own = None;
        let mut unreadable = None;

        for feed in feeds {
            match self.feed_owner(feed) {
                Ok(Some(owner)) if !owner.eq_ignore_ascii_case(&self.user) => {
                    return InactiveFact::Owned(owner);
                }
                Ok(Some(owner)) => own = Some(owner),
                Ok(None) => {}
                Err(reason) => {
                    unreadable.get_or_insert(reason);
                }
            }
        }

        match (unreadable, own) {
            (Some(reason), _) => InactiveFact::Unknown(reason),
            (None, Some(owner)) => InactiveFact::Owned(owner),
            (None, None) => InactiveFact::Absent,
        }
    }

    fn feed_owner(&mut self, feed: &str) -> Result<Option<String>, String> {
        let response = self
            .get(feed, VERSIONS_ACCEPT)
            .map_err(|error| format!("version list unavailable: {error}"))?;

        if response.status != 200 {
            return Err(format!(
                "version list unavailable: HTTP {}",
                response.status
            ));
        }

        let entries: Vec<VersionEntry> =
            parse_versions(&String::from_utf8_lossy(&response.body))
                .map_err(|error| format!("cannot parse the version list: {error}"))?;

        match entries.iter().find(|entry| entry.is_inactive()) {
            Some(entry) if entry.author.is_empty() => {
                Err("the version list names no owner of the inactive version".to_owned())
            }
            Some(entry) => Ok(Some(entry.author.clone())),
            None => Ok(None),
        }
    }

    /// The calling objects of `uri` as the usage search lists them.
    fn callers(&mut self, uri: &str) -> Result<Vec<Caller>, String> {
        let mut request = Request::post(
            usage_references_uri(uri),
            USAGE_REQUEST_XML.as_bytes().to_vec(),
        );

        for header in ["Content-Type", "Accept"] {
            request
                .headers
                .insert(header.to_owned(), "application/*".to_owned());
        }

        match self.send(request) {
            Ok(response) if response.status == 200 => {
                parse_callers(&String::from_utf8_lossy(&response.body))
            }
            Ok(response) => Err(format!("HTTP {}", response.status)),
            Err(error) => Err(error),
        }
    }

    /// Reports only: callers are never part of the plan identity, so a failure is a line, not an
    /// error.
    fn where_used(&mut self, uri: &str) -> Vec<String> {
        match self.callers(uri) {
            Ok(callers) => callers
                .iter()
                .map(|Caller { kind, name }| format!("{kind} {name}"))
                .collect(),
            Err(reason) => vec![format!("unavailable ({reason})")],
        }
    }

    /// Reports only: the programs that include the object, then the transactions that start it
    /// (`TRAN <tcode>`, from the same usage search). A failed lookup is the M3 warning line.
    fn prog_where_used(&mut self, uri: &str) -> Vec<String> {
        match self.callers(uri) {
            Ok(callers) => {
                let (transactions, others): (Vec<&Caller>, Vec<&Caller>) = callers
                    .iter()
                    .partition(|caller| caller.kind.split('/').next() == Some("TRAN"));

                others
                    .into_iter()
                    .map(|Caller { kind, name }| format!("{kind} {name}"))
                    .chain(
                        transactions
                            .into_iter()
                            .map(|Caller { name, .. }| format!("TRAN {name}")),
                    )
                    .collect()
            }
            Err(reason) => vec![format!("W: where-used unavailable: {reason}")],
        }
    }

    /// What a `PROG` name is: the cached answer is read again, otherwise `programs/programs`
    /// then `programs/includes`; the answering GET is returned with it.
    fn resolve_prog(&mut self, name: &str) -> Result<Option<(ProgTarget, Response)>, String> {
        let key = name.to_ascii_uppercase();

        if let Some(target) = self.resolved.get(&key).cloned() {
            let response = self.get(&target.uri, "application/*")?;

            return Ok(Some((target, response)));
        }

        match locate_properties(self.active(), name) {
            Ok(Some((target, response))) => {
                self.resolved.insert(key, target.clone());

                Ok(Some((target, response)))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(self.resolve_failure(error)),
        }
    }

    fn resolve_failure(&mut self, error: ResolveError) -> String {
        match error {
            ResolveError::Transport(error) => {
                if Failure::from(&error) == Failure::PreSendConnectivity {
                    self.unsent.get_or_insert_with(|| error.to_string());
                }

                error.to_string()
            }
            ResolveError::Status { status, .. } => format!("HTTP {status}"),
            ResolveError::Stop(message) => message,
        }
    }

    /// The package of a function module is the package of its group.
    fn package(
        &mut self,
        kind: DeleteKind,
        group: Option<&str>,
        xml: &str,
    ) -> Result<String, String> {
        let own = package_of(xml)?;

        match (own, group) {
            (Some(package), _) => Ok(package),
            (None, Some(group)) if kind == DeleteKind::Func => {
                let response = self.get(&group_uri(group), "application/*")?;

                if response.status != 200 {
                    return Err(format!(
                        "cannot read function group {group}: HTTP {}",
                        response.status
                    ));
                }

                package_of(&String::from_utf8_lossy(&response.body))?
                    .ok_or_else(|| format!("package of function group {group} is unknown"))
            }
            (None, _) => Err("package is unknown".to_owned()),
        }
    }

    /// Sends the one DELETE. The caller holds the lock handle only as long as this call.
    fn send_delete(
        &mut self,
        uri: &str,
        lock: &LockInfo,
        request: Option<&str>,
    ) -> Result<Response, TransportError> {
        let mut target = format!("{uri}?lockHandle={}", encode_lock_handle(&lock.handle));

        if let Some(request) = request {
            target.push_str(&format!("&corrNr={request}"));
        }

        self.active().request(Request::delete(target))
    }

    fn unlock(&mut self, uri: &str, lock: &LockInfo) -> bool {
        let request = Request::post(
            format!(
                "{uri}?_action=UNLOCK&lockHandle={}",
                encode_lock_handle(&lock.handle)
            ),
            Vec::new(),
        );

        matches!(
            self.active().request(request),
            Ok(response) if (200..300).contains(&response.status)
        )
    }

    /// Releases the lock after a refusal and words the reason; a failed release says so.
    fn refuse(&mut self, uri: &str, lock: &LockInfo, reason: String) -> DeleteReply {
        let reason = scrub(&reason, &lock.handle);

        if self.unlock(uri, lock) {
            DeleteReply::Rejected(reason)
        } else {
            DeleteReply::Rejected(format!("{reason}; {LOCK_MAY_REMAIN}"))
        }
    }

    /// 2xx is authoritative; the fresh-session read can only contradict it.
    fn verify(&mut self, uri: &str, name: &str) -> DeleteReply {
        match self.fresh_status(uri) {
            Ok(200) => DeleteReply::Partial(format!(
                "DELETE answered 2xx but {name} still exists in a new session"
            )),
            _ => DeleteReply::Deleted,
        }
    }

    /// The status of `GET <uri>` from a new, short-lived session.
    fn fresh_status(&mut self, uri: &str) -> Result<u16, String> {
        let mut session = (self.open)().map_err(|error| error.to_string())?;
        let mut request = Request::get(uri);
        request
            .headers
            .insert("Accept".to_owned(), "application/*".to_owned());
        let status = session
            .request(request)
            .map(|response| response.status)
            .map_err(|error| error.to_string());

        // A read holds no lock, so a failed end of the session is only a note.
        if let Err(error) = session.close() {
            self.close_notes
                .push(format!("W: session close failed: {error}"));
        }

        status
    }
}

impl DeleteBackend for AdtDelete<'_> {
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        self.ensure_session()?;

        let (uri, group, response) = if kind == DeleteKind::Prog {
            match self.resolve_prog(name)? {
                Some((target, response)) => (target.uri, None, response),
                None => return Ok(None),
            }
        } else {
            let Some((uri, group)) = self.locate(kind, name)? else {
                return Ok(None);
            };
            let response = self.get(&uri, "application/*")?;

            (uri, group, response)
        };

        match response.status {
            200 => {}
            404 => return Ok(None),
            status => return Err(format!("HTTP {status}")),
        }

        let xml = String::from_utf8(response.body)
            .map_err(|error| format!("metadata is not UTF-8: {error}"))?;
        let package = self.package(kind, group.as_deref(), &xml)?;
        let author = responsible_of(&xml)?.unwrap_or_default();
        let mut sources = Vec::new();
        let mut contents = Vec::new();
        let mut feeds = Vec::new();
        let mut where_used = Vec::new();

        match kind {
            DeleteKind::Extension(kind) => {
                return Err(format!("{} is not an ADT object", kind.token()));
            }
            DeleteKind::Fugr => {
                contents = self
                    .members(name)?
                    .into_iter()
                    .map(|member| format!("FUNC {member}"))
                    .collect();
            }
            DeleteKind::Clas => {
                sources.push(self.part(&uri, "/source/main", "MAIN")?);
                feeds.push(format!("{uri}/includes/main/versions"));

                for (include, suffix) in INCLUDES {
                    let part = format!("/includes/{}", include.segment());

                    if let Some(lines) = self.source(&format!("{uri}{part}"))? {
                        sources.push(SourceFact {
                            label: suffix.to_owned(),
                            active: Some(lines),
                        });
                        contents.push(format!(
                            "INCLUDE {:=<CLASS_INCLUDE_WIDTH$}{suffix}",
                            name.to_ascii_uppercase()
                        ));
                        feeds.push(format!("{uri}{part}/versions"));
                    }
                }
            }
            DeleteKind::Prog | DeleteKind::Intf | DeleteKind::Func => {
                sources.push(self.part(&uri, "/source/main", "MAIN")?);
                feeds.push(format!("{uri}/source/main/versions"));
            }
        }

        match kind {
            DeleteKind::Func => where_used = self.where_used(&uri),
            DeleteKind::Prog => where_used = self.prog_where_used(&uri),
            _ => {}
        }

        let inactive = if feeds.is_empty() {
            InactiveFact::Absent
        } else {
            self.inactive(&feeds)
        };
        let tr = if package.starts_with('$') {
            TrLock::Local
        } else {
            // The lock of the delete names the request and its owner; `transport_request` refuses
            // a request of another user after the lock.
            TrLock::Unlocked
        };

        Ok(Some(ObjectFacts {
            package,
            author,
            group,
            sources,
            contents,
            where_used,
            inactive,
            tr,
        }))
    }

    fn delete(&mut self, kind: DeleteKind, name: &str, tr: Option<&str>) -> DeleteReply {
        // The server would answer the LOCK and then refuse the DELETE (423), so the capability is
        // checked before the first request that locks.
        if let Err(reason) = self.ensure_session() {
            return DeleteReply::Rejected(reason);
        }

        if self.active_ref().stateful_supported() == Some(false) {
            return DeleteReply::Rejected(NO_STATEFUL_SESSION.to_owned());
        }

        let uri = match self.locate(kind, name) {
            Ok(Some((uri, _))) => uri,
            Ok(None) => return DeleteReply::NotFound,
            Err(reason) => return DeleteReply::Rejected(reason),
        };
        self.wrote = true;
        // A successful LOCK may already have side effects, so interruption is deferred until the
        // handle is consumed or explicitly released.
        let cancel = self.cancel.clone();
        let _lock_scope = cancel.as_ref().map(CancelFlag::hold_lock);
        let lock = match self.lock(&uri) {
            Ok(lock) => lock,
            Err(reply) => return reply,
        };
        if cancel.as_ref().is_some_and(CancelFlag::is_set) {
            return self.refuse(&uri, &lock, "CANCELED: stopped after the lock".to_owned());
        }
        let request = match transport_request(&lock, tr, &self.user) {
            Ok(request) => request,
            Err(reason) => return self.refuse(&uri, &lock, reason),
        };

        match self.send_delete(&uri, &lock, request.as_deref()) {
            Ok(response) if (200..300).contains(&response.status) => {
                self.end_stateful_session();

                self.verify(&uri, name)
            }
            Ok(response) if response.status == 404 => DeleteReply::NotFound,
            Ok(response) if response.status >= 500 => {
                DeleteReply::Uncertain(scrub(&failure("DELETE failed", &response), &lock.handle))
            }
            Ok(response) => {
                let reason = failure("DELETE failed", &response);

                self.refuse(&uri, &lock, reason)
            }
            Err(error) if error.was_sent() => {
                DeleteReply::Uncertain(scrub(&error.to_string(), &lock.handle))
            }
            Err(error) => {
                let reason = format!("DELETE not sent: {error}");

                self.refuse(&uri, &lock, reason)
            }
        }
    }

    fn is_include(&mut self, kind: DeleteKind, name: &str) -> bool {
        if kind != DeleteKind::Prog || self.ensure_session().is_err() {
            return false;
        }

        if let Some(target) = self.resolved.get(&name.to_ascii_uppercase()) {
            return target.is_include();
        }

        matches!(self.resolve_prog(name), Ok(Some((target, _))) if target.is_include())
    }

    fn halted(&self) -> bool {
        self.open_failure.is_some()
    }

    fn reconcile(&mut self, kind: DeleteKind, name: &str) -> Reconcile {
        let uri = match self.locate(kind, name) {
            Ok(Some((uri, _))) => uri,
            Ok(None) => return Reconcile::Gone,
            Err(reason) => return Reconcile::Failed(reason),
        };

        match self.fresh_status(&uri) {
            Ok(404) => Reconcile::Gone,
            Ok(200) => Reconcile::StillExists,
            Ok(status) => Reconcile::Failed(format!("HTTP {status}")),
            Err(reason) => Reconcile::Failed(reason),
        }
    }
}

impl AdtDelete<'_> {
    /// One labelled source: the active text, or the marker that the part has none.
    fn part(&mut self, uri: &str, tail: &str, label: &str) -> Result<SourceFact, String> {
        Ok(SourceFact {
            label: label.to_owned(),
            active: self.source(&format!("{uri}{tail}"))?,
        })
    }

    fn lock(&mut self, uri: &str) -> Result<LockInfo, DeleteReply> {
        let mut request =
            Request::post(format!("{uri}?_action=LOCK&accessMode=MODIFY"), Vec::new());
        request
            .headers
            .insert("Accept".to_owned(), LOCK_ACCEPT.to_owned());
        let response = self.active().request(request).map_err(|error| {
            if error.was_sent() {
                DeleteReply::Rejected(format!(
                    "lock request unanswered: {error}; {LOCK_MAY_REMAIN}"
                ))
            } else {
                DeleteReply::Rejected(format!("lock not sent: {error}"))
            }
        })?;

        if let Some(error) = response.authorization_failure() {
            return Err(DeleteReply::Rejected(error.to_string()));
        }

        if !(200..300).contains(&response.status) {
            return Err(DeleteReply::Rejected(failure("lock failed", &response)));
        }

        parse_lock(&String::from_utf8_lossy(&response.body)).map_err(|message| {
            DeleteReply::Rejected(format!(
                "unreadable lock response: {message}; {LOCK_MAY_REMAIN}"
            ))
        })
    }
}

fn group_uri(name: &str) -> String {
    format!("/sap/bc/adt/functions/groups/{}", name.to_ascii_lowercase())
}

/// The request the DELETE carries: none for a local object, otherwise the request of the lock,
/// which the plan's request must equal and which must not belong to another user. An owner the
/// lock does not name cannot be checked here; SAP then decides at the DELETE.
fn transport_request(
    lock: &LockInfo,
    wanted: Option<&str>,
    user: &str,
) -> Result<Option<String>, String> {
    let corrnr = lock.corrnr.as_str();

    match (lock.is_local, wanted) {
        (true, Some(_)) => Err("--tr is not allowed for a local object".to_owned()),
        (true, None) => Ok(None),
        (false, _) if corrnr.is_empty() => {
            Err("the object is not local but the lock returned no transport request".to_owned())
        }
        (false, _) if !lock.corruser.is_empty() && !lock.corruser.eq_ignore_ascii_case(user) => {
            Err(format!(
                "transport request {corrnr} of the lock belongs to another user"
            ))
        }
        (false, Some(tr)) if !tr.eq_ignore_ascii_case(corrnr) => Err(format!(
            "--tr {tr} does not match the transport request {corrnr} of the lock"
        )),
        (false, _) => {
            check_request(corrnr)
                .map_err(|_| format!("the lock returned an invalid transport request {corrnr}"))?;

            Ok(Some(corrnr.to_owned()))
        }
    }
}

/// `<what>: HTTP <status>`, with SAP's own message when the body has one. Callers scrub it.
fn failure(what: &str, response: &Response) -> String {
    let message = leaf_texts(&String::from_utf8_lossy(&response.body))
        .ok()
        .and_then(|texts| texts.get("localizedMessage").cloned())
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty());

    match message {
        Some(message) => format!("{what}: HTTP {}: {}", response.status, cut(&message)),
        None => format!("{what}: HTTP {}", response.status),
    }
}

/// Shown instead of a message that has no word boundary to cut at.
const MESSAGE_CUT: &str = "(message cut)";

/// At most `MESSAGE_LIMIT` characters, cut at a word boundary: the caller scrubs afterwards, and
/// a secret split by the cut would survive as a prefix. The message has single spaces only; one
/// without a boundary in its first characters is replaced by a marker.
fn cut(message: &str) -> String {
    if message.chars().count() <= MESSAGE_LIMIT {
        return message.to_owned();
    }

    let head: String = message.chars().take(MESSAGE_LIMIT).collect();
    let on_boundary = head.ends_with(' ') || message.chars().nth(MESSAGE_LIMIT) == Some(' ');

    match head.rsplit_once(' ') {
        Some((words, _)) if !on_boundary => words.to_owned(),
        _ if on_boundary => head.trim_end().to_owned(),
        _ => MESSAGE_CUT.to_owned(),
    }
}

/// Removes the lock handle, plain and percent-encoded, from a diagnostic.
fn scrub(text: &str, handle: &str) -> String {
    if handle.is_empty() {
        return text.to_owned();
    }

    text.replace(&encode_lock_handle(handle), HANDLE_MASK)
        .replace(handle, HANDLE_MASK)
}

#[cfg(test)]
mod tests {
    use super::{LockInfo, MESSAGE_CUT, MESSAGE_LIMIT, cut, scrub, transport_request};

    fn lock(local: bool, corrnr: &str) -> LockInfo {
        LockInfo {
            handle: "h".to_owned(),
            corrnr: corrnr.to_owned(),
            corruser: String::new(),
            is_local: local,
        }
    }

    #[test]
    fn the_handle_is_masked_in_both_spellings() {
        assert_eq!(
            scrub("x a/b+c= y a%2Fb%2Bc%3D", "a/b+c="),
            "x <lock handle> y <lock handle>"
        );
        assert_eq!(scrub("untouched", ""), "untouched");
    }

    #[test]
    fn a_long_message_is_cut_at_a_word_boundary() {
        let word = "x".repeat(9);
        let long = vec![word.as_str(); 30].join(" ");
        let secret_cut = format!("{} router.example.net", "y".repeat(MESSAGE_LIMIT - 11));

        assert_eq!(cut("short"), "short");
        assert_eq!(cut(&long), vec![word.as_str(); 20].join(" "));
        // The cut falls inside the host: the partial word is dropped with the rest.
        assert_eq!(cut(&secret_cut), "y".repeat(MESSAGE_LIMIT - 11));
        assert!(!cut(&secret_cut).contains("router"));
        // No boundary at all: nothing of the message can be kept safely.
        assert_eq!(cut(&"z".repeat(MESSAGE_LIMIT + 5)), MESSAGE_CUT);
    }

    #[test]
    fn the_request_comes_from_the_lock() {
        assert_eq!(transport_request(&lock(true, ""), None, "U"), Ok(None));
        assert_eq!(
            transport_request(&lock(false, "DEVK900001"), Some("devk900001"), "U"),
            Ok(Some("DEVK900001".to_owned()))
        );
        assert!(transport_request(&lock(false, "DEVK 1"), None, "U").is_err());
    }
}
