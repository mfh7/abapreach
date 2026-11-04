//! The ADT side of delete: the read-only facts behind a plan, the single lock-and-DELETE
//! sequence, and the read from a new session that settles an unknown outcome.
//!
//! The lock handle is not bound to a session and SAP echoes it in its 423 text, so it never leaves
//! this module: every message built here is scrubbed of it. A DELETE is sent once; whatever it
//! answers is classified, never retried.

use crate::adt::{
    Caller, USAGE_REQUEST_XML, VersionEntry, group_members, package_of, parse_callers,
    parse_versions, responsible_of, usage_references_uri,
};
use crate::cli::{DeleteKind, IncludeKind, ObjectKind};
use crate::hash::local_source_lines;
use crate::object::check_request;
use crate::operation::delete::{
    DeleteBackend, DeleteReply, InactiveFact, ObjectFacts, Reconcile, SourceFact, TrLock,
};
use crate::operation::deploy::{
    LOCK_ACCEPT, LockInfo, VERSIONS_ACCEPT, encode_lock_handle, leaf_texts, parse_lock,
};
use crate::operation::read::resolve_group;
use crate::transport::{Request, Response, SadtTransport, TransportError};

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
}

impl<'a> AdtDelete<'a> {
    pub fn new(transport: &'a mut dyn SadtTransport, user: &str, open: &'a mut Opener<'a>) -> Self {
        Self {
            transport,
            user: user.to_owned(),
            open,
        }
    }

    fn get(&mut self, uri: &str, accept: &str) -> Result<Response, String> {
        let mut request = Request::get(uri);
        request
            .headers
            .insert("Accept".to_owned(), accept.to_owned());

        self.send(request)
    }

    fn send(&mut self, request: Request) -> Result<Response, String> {
        let response = self
            .transport
            .request(request)
            .map_err(|error| error.to_string())?;

        match response.authorization_failure() {
            Some(error) => Err(error.to_string()),
            None => Ok(response),
        }
    }

    /// The ADT URI of the object, and for a function module its group, which the server resolves
    /// first; `None` when the module is unknown.
    fn locate(
        &mut self,
        kind: DeleteKind,
        name: &str,
    ) -> Result<Option<(String, Option<String>)>, String> {
        let object = match kind {
            DeleteKind::Prog => ObjectKind::Prog,
            DeleteKind::Clas => ObjectKind::Clas,
            DeleteKind::Intf => ObjectKind::Intf,
            DeleteKind::Fugr => return Ok(Some((group_uri(name), None))),
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
        match resolve_group(&mut *self.transport, name, None) {
            Ok(group) => Ok(Some(group)),
            Err(error) if error.message.contains("returned no reference") => Ok(None),
            Err(error) => Err(error.message),
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

    /// Reports only: callers are never part of the plan identity, so a failure is a line, not an
    /// error.
    fn where_used(&mut self, uri: &str) -> Vec<String> {
        let mut request = Request::post(
            usage_references_uri(uri),
            USAGE_REQUEST_XML.as_bytes().to_vec(),
        );

        for header in ["Content-Type", "Accept"] {
            request
                .headers
                .insert(header.to_owned(), "application/*".to_owned());
        }

        let callers = match self.send(request) {
            Ok(response) if response.status == 200 => {
                parse_callers(&String::from_utf8_lossy(&response.body))
            }
            Ok(response) => Err(format!("HTTP {}", response.status)),
            Err(error) => Err(error),
        };

        match callers {
            Ok(callers) => callers
                .iter()
                .map(|Caller { kind, name }| format!("{kind} {name}"))
                .collect(),
            Err(reason) => vec![format!("unavailable ({reason})")],
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

        self.transport.request(Request::delete(target))
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
            self.transport.request(request),
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

        // A read holds no lock, so a failed end of the session changes nothing.
        let _ = session.close();

        status
    }
}

impl DeleteBackend for AdtDelete<'_> {
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        let Some((uri, group)) = self.locate(kind, name)? else {
            return Ok(None);
        };
        let response = self.get(&uri, "application/*")?;

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
            DeleteKind::Fugr => {
                contents = group_members(&xml)?
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

        if kind == DeleteKind::Func {
            where_used = self.where_used(&uri);
        }

        let inactive = if feeds.is_empty() {
            InactiveFact::Absent
        } else {
            self.inactive(&feeds)
        };
        let tr = if package.starts_with('$') {
            TrLock::Local
        } else {
            // The lock of the delete names the request; SAP refuses a request of another user.
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
        if self.transport.stateful_supported() == Some(false) {
            return DeleteReply::Rejected(NO_STATEFUL_SESSION.to_owned());
        }

        let uri = match self.locate(kind, name) {
            Ok(Some((uri, _))) => uri,
            Ok(None) => return DeleteReply::NotFound,
            Err(reason) => return DeleteReply::Rejected(reason),
        };
        let lock = match self.lock(&uri) {
            Ok(lock) => lock,
            Err(reply) => return reply,
        };
        let request = match transport_request(&lock, tr) {
            Ok(request) => request,
            Err(reason) => return self.refuse(&uri, &lock, reason),
        };

        match self.send_delete(&uri, &lock, request.as_deref()) {
            Ok(response) if (200..300).contains(&response.status) => self.verify(&uri, name),
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
        let response = self.transport.request(request).map_err(|error| {
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
/// which the plan's request must equal.
fn transport_request(lock: &LockInfo, wanted: Option<&str>) -> Result<Option<String>, String> {
    let corrnr = lock.corrnr.as_str();

    match (lock.is_local, wanted) {
        (true, Some(_)) => Err("--tr is not allowed for a local object".to_owned()),
        (true, None) => Ok(None),
        (false, _) if corrnr.is_empty() => {
            Err("the object is not local but the lock returned no transport request".to_owned())
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
        Some(message) => format!(
            "{what}: HTTP {}: {}",
            response.status,
            message.chars().take(MESSAGE_LIMIT).collect::<String>()
        ),
        None => format!("{what}: HTTP {}", response.status),
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
    use super::{LockInfo, scrub, transport_request};

    fn lock(local: bool, corrnr: &str) -> LockInfo {
        LockInfo {
            handle: "h".to_owned(),
            corrnr: corrnr.to_owned(),
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
    fn the_request_comes_from_the_lock() {
        assert_eq!(transport_request(&lock(true, ""), None), Ok(None));
        assert_eq!(
            transport_request(&lock(false, "DEVK900001"), Some("devk900001")),
            Ok(Some("DEVK900001".to_owned()))
        );
        assert!(transport_request(&lock(false, "DEVK 1"), None).is_err());
    }
}
