//! The request boundary of the RFC/SADT tunnel: the one function that decides whether a request
//! may leave the process. It admits exactly the request shapes the commands send (M1 programs and
//! the M3 function module, class and interface operations) and nothing broader; every shape is
//! parsed rather than prefix-matched, so a traversal, a wrong object-kind path or an extra query
//! parameter is refused. Pure logic with no platform dependency.

use super::Request;
use crate::adt::{
    ActivationBody, ObjectRef, USAGE_REQUEST_XML, classify_activation, parse_object_refs,
};
use crate::operation::read_table::validate_preview_body;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

const DISCOVERY: &str = "/sap/bc/adt/discovery";
const INACTIVE_OBJECTS: &str = "/sap/bc/adt/activation/inactiveobjects";
const CHECKRUNS: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const ACTIVATION: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=";
const SEARCH: &str = "/sap/bc/adt/repository/informationsystem/search";
const USAGE_REFERENCES: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";
const NODE_STRUCTURE: &str = "/sap/bc/adt/repository/nodestructure";
const FUNCTION_VALIDATION: &str = "/sap/bc/adt/functions/validation";
const OBJECT_VALIDATION: &str = "/sap/bc/adt/oo/validation/objectname";
const GROUPS: &str = "/sap/bc/adt/functions/groups";
const CLASSES: &str = "/sap/bc/adt/oo/classes";
const INTERFACES: &str = "/sap/bc/adt/oo/interfaces";
const PROGRAMS: &str = "/sap/bc/adt/programs/programs";
const PROGRAM_INCLUDES: &str = "/sap/bc/adt/programs/includes";
const PROGRAM_VALIDATION: &str = "/sap/bc/adt/programs/validation";
/// The only `context` an include request carries: the encoded URI prefix of a main program.
const CONTEXT_PREFIX: &str = "context=%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2F";
const DATA_PREVIEW: &str = "/sap/bc/adt/datapreview/ddic";
/// The row limit of the 740 data preview.
const MAX_PREVIEW_ROWS: u32 = 5000;
/// A DDIC identifier is at most 30 characters, `/NAMESPACE/` included.
const MAX_ENTITY: usize = 30;
const OBJECT_STRUCTURE: &str = "version=active&withShortDescriptions=true";
const SEARCH_TAIL: &str = "&maxResults=10&objectType=FUGR%2FFF";
const LOCK: &str = "_action=LOCK&accessMode=MODIFY";
const UNLOCK: &str = "_action=UNLOCK&lockHandle=";
/// TRKORR is a CHAR 20 field.
const MAX_REQUEST: usize = 20;
/// The class includes that have a source of their own; `main` only exists as a version feed.
const INCLUDES: [&str; 4] = ["definitions", "implementations", "macros", "testclasses"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Prog,
    Func,
    Clas,
    Intf,
    /// A program include; its activation always names a main program.
    Incl,
    /// A function group: readable as metadata, locked only for a delete, never written.
    Group,
}

/// What a path below an object names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Part {
    Object,
    Source,
    SourceVersions,
    Includes,
    IncludeSource,
    IncludeVersions,
    Structure,
    /// `GET <include>/mainprograms`, the master programs of an include.
    MainPrograms,
}

struct Path {
    kind: Kind,
    part: Part,
}

/// The state the boundary keeps between activation calls. A resend is only legal for the exact
/// worklist returned by the RFC endpoint after a class's first activation request.
#[derive(Debug, Default)]
pub struct Admission {
    awaiting_first_response: bool,
    expected_worklist: Option<Vec<ObjectRef>>,
}

impl Admission {
    /// Whether the request may be sent. A class first step waits for its RFC response before a
    /// resend can be admitted.
    pub fn admits(&mut self, request: &Request) -> bool {
        let uri = request.uri.as_str();

        if uri.contains('#') {
            return false;
        }

        let (path, query) = split_query(uri);

        match request.method.as_str() {
            "GET" => is_read(uri, path, query),
            "DELETE" => request.body.is_empty() && is_delete(path, query),
            "PUT" => is_put(path, query) && has_content_type(request, "text/plain; charset=utf-8"),
            "POST" => {
                if let Some(preaudit) = uri.strip_prefix(ACTIVATION) {
                    return has_content_type(request, "application/xml")
                        && self.admits_activation(preaudit, &request.body);
                }

                is_post(request, uri, path, query)
            }
            _ => false,
        }
    }

    fn admits_activation(&mut self, preaudit: &str, body: &[u8]) -> bool {
        match preaudit {
            "true" => self.first_step(body),
            "false" => self.resend(body),
            _ => false,
        }
    }

    /// The first step names one object. An empty body is the M1 request and activates nothing.
    fn first_step(&mut self, body: &[u8]) -> bool {
        self.clear();

        if body.is_empty() {
            return true;
        }

        let Some(references) = references(body) else {
            return false;
        };
        let [reference] = references.as_slice() else {
            return false;
        };
        let Some(parsed) = activation_target(reference) else {
            return false;
        };

        if parsed.part != Part::Object || parsed.kind == Kind::Group {
            return false;
        }

        self.awaiting_first_response = parsed.kind == Kind::Clas;

        true
    }

    /// The sole resend consumes the worklist even when it does not match, so a rejected request
    /// cannot be followed by another attempt.
    fn resend(&mut self, body: &[u8]) -> bool {
        let expected = self.expected_worklist.take();
        self.awaiting_first_response = false;

        matches!(
            (expected, references(body)),
            (Some(expected), Some(actual)) if actual == expected
        )
    }

    /// Records the first activation response the RFC transport actually received. Anything other
    /// than a non-empty worklist makes a resend unavailable.
    pub(crate) fn received_response(&mut self, status: u16, body: &[u8]) {
        if !self.awaiting_first_response {
            return;
        }

        self.awaiting_first_response = false;
        self.expected_worklist = ((200..300).contains(&status)
            && classify_activation(&String::from_utf8_lossy(body)) == ActivationBody::Worklist)
            .then(|| parse_object_refs(&String::from_utf8_lossy(body)).ok())
            .flatten()
            .filter(|entries| !entries.is_empty());
    }

    /// No error or terminal path may leave a resend capability behind.
    pub(crate) fn clear(&mut self) {
        self.awaiting_first_response = false;
        self.expected_worklist = None;
    }
}

/// Whether `request` is the create `POST` of a function group, function module, class or
/// interface, with or without its `corrNr`.
#[cfg(feature = "fault-injection")]
pub fn is_create_post(request: &Request) -> bool {
    let (path, query) = split_query(&request.uri);

    request.method == "POST" && !request.uri.contains('#') && is_create(path, query)
}

/// Whether `request` is a source `PUT` under a lock.
#[cfg(feature = "fault-injection")]
pub fn is_put_request(request: &Request) -> bool {
    let (path, query) = split_query(&request.uri);

    request.method == "PUT" && !request.uri.contains('#') && is_put(path, query)
}

/// Whether `request` is a `DELETE` of an object under its lock.
#[cfg(feature = "fault-injection")]
pub fn is_delete_request(request: &Request) -> bool {
    let (path, query) = split_query(&request.uri);

    request.method == "DELETE" && !request.uri.contains('#') && is_delete(path, query)
}

/// The object an activation reference names. An include is only ever activated with its main
/// program as context, so its reference carries `?context=` and nothing else; every other kind
/// has no query and no `context` anywhere in its reference.
fn activation_target(reference: &ObjectRef) -> Option<Path> {
    if reference
        .parent_uri
        .as_deref()
        .is_some_and(has_query_marker)
    {
        return None;
    }

    match split_query(&reference.uri) {
        (path, Some(query)) => {
            let parsed = parse_path(path)?;

            (parsed.kind == Kind::Incl
                && parsed.part == Part::Object
                && reference.parent_uri.is_none()
                && is_context(query))
            .then_some(parsed)
        }
        (path, None) => parse_path(path).filter(|parsed| parsed.kind != Kind::Incl),
    }
}

/// `context=<encoded /sap/bc/adt/programs/programs/<name>>`, the only query of a body URI.
fn is_context(query: &str) -> bool {
    query
        .strip_prefix(CONTEXT_PREFIX)
        .is_some_and(|name| is_name(name, false))
}

/// Whether `text` holds a query or fragment delimiter, raw or percent-encoded. An object name
/// that merely contains the word "context" is not a marker.
fn has_query_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();

    ["?", "#", "%3f", "%23"]
        .iter()
        .any(|mark| lower.contains(mark))
}

fn mentions_context(text: &str) -> bool {
    text.to_ascii_lowercase().contains("context")
}

/// A checkrun body names objects by URI. Its `checkObject` of an include may carry the main
/// program as `?context=`; no other URI of the body may carry a query, a fragment or a context,
/// whichever element holds it. A body without any such URI is judged as before.
fn is_checkrun_body(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return !mentions_context(&String::from_utf8_lossy(body));
    };

    if body.is_empty() {
        return true;
    }

    let mut reader = Reader::from_str(text);

    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                let is_check_object = element.local_name().into_inner() == "checkObject";

                if !attributes_are_clean(&element, is_check_object) {
                    return false;
                }
            }
            Ok(Event::Eof) => return true,
            Ok(_) => {}
            Err(_) => return !mentions_context(text),
        }
    }
}

/// Whether no `uri` attribute of the element carries a query, fragment or context except the
/// contextual include URI of a `checkObject`.
fn attributes_are_clean(element: &BytesStart<'_>, is_check_object: bool) -> bool {
    for attribute in element.attributes() {
        let Ok(attribute) = attribute else {
            return false;
        };
        let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0) else {
            return false;
        };
        let is_uri = attribute.key.local_name().into_inner() == "uri";

        if !has_query_marker(&value) {
            continue;
        }

        if !(is_uri && is_check_object && is_contextual_include(&value)) {
            return false;
        }
    }

    true
}

/// `<include>?context=<main program>`, the URI of an include checked or activated with context.
fn is_contextual_include(uri: &str) -> bool {
    let (path, query) = split_query(uri);

    parse_path(path).is_some_and(|parsed| parsed.kind == Kind::Incl && parsed.part == Part::Object)
        && query.is_some_and(is_context)
}

/// The create body of a program or include names the endpoint's own object type and, for an
/// include, no container: the parent is bound later. The other collections are not inspected.
fn is_create_body(path: &str, body: &[u8]) -> bool {
    let (root, object_type) = match path {
        PROGRAMS => ("abapProgram", "PROG/P"),
        PROGRAM_INCLUDES => ("abapInclude", "PROG/I"),
        _ => return true,
    };
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };
    let mut reader = Reader::from_str(text);
    let mut root_seen = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                if root_seen {
                    if element.local_name().into_inner() == "containerRef" {
                        return false;
                    }

                    continue;
                }

                root_seen = true;

                if element.local_name().into_inner() != root || !has_type(&element, object_type) {
                    return false;
                }
            }
            Ok(Event::Eof) => return root_seen,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn has_type(element: &BytesStart<'_>, expected: &str) -> bool {
    element.attributes().flatten().any(|attribute| {
        attribute.key.local_name().into_inner() == "type"
            && attribute
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .is_ok_and(|value| value == expected)
    })
}

/// `objname=<n>&packagename=<p>&description=<t>&objtype=PROG/P|PROG/I`, the order and the raw
/// object type the recorded exchanges used; the answer is informational and never a guard.
fn is_program_validation(query: &str) -> bool {
    let mut parts = query.split('&');
    let (Some(name), Some(package), Some(text), Some(object_type), None) = (
        parts.next().and_then(|part| part.strip_prefix("objname=")),
        parts
            .next()
            .and_then(|part| part.strip_prefix("packagename=")),
        parts
            .next()
            .and_then(|part| part.strip_prefix("description=")),
        parts.next().and_then(|part| part.strip_prefix("objtype=")),
        parts.next(),
    ) else {
        return false;
    };

    !name.is_empty()
        && is_encoded(name)
        && !package.is_empty()
        && is_encoded(package)
        && is_encoded(text)
        && matches!(object_type, "PROG/P" | "PROG/I")
}

fn split_query(uri: &str) -> (&str, Option<&str>) {
    match uri.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (uri, None),
    }
}

fn is_read(uri: &str, path: &str, query: Option<&str>) -> bool {
    if uri == DISCOVERY || uri == INACTIVE_OBJECTS {
        return true;
    }

    if path == SEARCH {
        return query.is_some_and(is_group_search);
    }

    let Some(parsed) = parse_path(path) else {
        return false;
    };

    match (parsed.part, query) {
        (Part::Source | Part::IncludeSource, Some("version=active" | "version=inactive")) => true,
        (Part::SourceVersions | Part::IncludeVersions | Part::MainPrograms, None) => true,
        (Part::Structure, Some(OBJECT_STRUCTURE)) => true,
        // The metadata the create reconciliation and the group package lookup read, and the
        // read that proves a delete (404 from a fresh session).
        (Part::Object, None) => true,
        _ => false,
    }
}

/// `search?operation=quickSearch&query=<NAME>&maxResults=10&objectType=FUGR%2FFF`, the typed
/// search that resolves the function group of a function module.
fn is_group_search(query: &str) -> bool {
    query
        .strip_prefix("operation=quickSearch&query=")
        .and_then(|rest| rest.strip_suffix(SEARCH_TAIL))
        .is_some_and(|name| is_name(name, false))
}

/// `nodestructure?parent_type=FUGR%2FF&parent_name=<NAME>&withShortDescriptions=false`, the
/// member listing of a function group that a delete plan reads.
fn is_group_members(query: &str) -> bool {
    query
        .strip_prefix("parent_type=FUGR%2FF&parent_name=")
        .and_then(|rest| rest.strip_suffix("&withShortDescriptions=false"))
        .is_some_and(|name| is_name(name, false))
}

fn is_put(path: &str, query: Option<&str>) -> bool {
    let Some(parsed) = parse_path(path) else {
        return false;
    };

    matches!(parsed.part, Part::Source | Part::IncludeSource)
        && query.is_some_and(is_lock_and_request)
}

fn is_post(request: &Request, uri: &str, path: &str, query: Option<&str>) -> bool {
    if uri == CHECKRUNS {
        return has_content_type(request, "application/*") && is_checkrun_body(&request.body);
    }

    if path == DATA_PREVIEW {
        return has_content_type(request, "text/plain")
            && has_header(request, "Accept", "application/*")
            && query.is_some_and(|query| is_data_preview(query, &request.body));
    }

    if path == USAGE_REFERENCES {
        return query.is_some_and(|query| is_where_used_object(query, &request.body));
    }

    if path == NODE_STRUCTURE {
        return request.body.is_empty() && query.is_some_and(is_group_members);
    }

    if path == FUNCTION_VALIDATION || path == OBJECT_VALIDATION {
        return query.is_some_and(|query| is_validation(path, query));
    }

    if path == PROGRAM_VALIDATION {
        return request.body.is_empty() && query.is_some_and(is_program_validation);
    }

    if is_create(path, query) {
        return has_content_type(request, "application/*") && is_create_body(path, &request.body);
    }

    let Some(parsed) = parse_path(path) else {
        return false;
    };

    match parsed.part {
        // The group lock only serves the delete of a function group.
        Part::Object => query.is_some_and(|query| {
            query == LOCK || query.strip_prefix(UNLOCK).is_some_and(is_encoded_handle)
        }),
        // The include POST runs under the class lock.
        Part::Includes => {
            query.is_some_and(is_lock_and_request) && has_content_type(request, "application/*")
        }
        _ => false,
    }
}

/// `DELETE <object>?lockHandle=<handle>[&corrNr=<request>]`: one object of the delete kinds
/// (a program include included), under its lock, never a part, a collection or another endpoint.
fn is_delete(path: &str, query: Option<&str>) -> bool {
    parse_path(path).is_some_and(|parsed| parsed.part == Part::Object)
        && query.is_some_and(is_lock_and_request)
}

fn has_content_type(request: &Request, expected: &str) -> bool {
    has_header(request, "Content-Type", expected)
}

fn has_header(request: &Request, name: &str, expected: &str) -> bool {
    request
        .headers
        .get(name)
        .is_some_and(|value| value == expected)
}

/// `rowNumber=<1..5000>&ddicEntityName=<entity>` with one `SELECT` line that reads that entity.
/// The body is never SQL of its own choosing: a second statement or another table is refused.
fn is_data_preview(query: &str, body: &[u8]) -> bool {
    let Some((rows, entity)) = query.split_once("&ddicEntityName=") else {
        return false;
    };
    let Some(rows) = rows.strip_prefix("rowNumber=") else {
        return false;
    };
    let rows_valid = !rows.starts_with('0')
        && rows.bytes().all(|byte| byte.is_ascii_digit())
        && rows
            .parse::<u32>()
            .is_ok_and(|rows| (1..=MAX_PREVIEW_ROWS).contains(&rows));
    // Only the `/` of a namespace is escaped, as `%2F`; a raw one is refused.
    let decoded = entity.replace("%2F", "/");

    rows_valid && !entity.contains('/') && is_entity(&decoded) && reads_entity(body, &decoded)
}

/// `NAME` or `/NAMESPACE/NAME` of letters, digits and `_`.
fn is_entity(entity: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };

    entity.len() <= MAX_ENTITY
        && match entity.strip_prefix('/') {
            Some(rest) => rest
                .split_once('/')
                .is_some_and(|(space, local)| plain(space) && plain(local)),
            None => plain(entity),
        }
}

/// Only the exact canonical `read-table` rendering is admitted. SAP runs data-preview bodies as
/// is, so this boundary reuses the operation parser rather than trusting a caller's body.
fn reads_entity(body: &[u8], entity: &str) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };

    validate_preview_body(entity, text).is_ok()
}

/// `lockHandle=<handle>[&corrNr=<request>]`, the query of a write under a lock. A lock itself
/// carries no `corrNr`.
fn is_lock_and_request(query: &str) -> bool {
    let Some(rest) = query.strip_prefix("lockHandle=") else {
        return false;
    };

    match rest.split_once("&corrNr=") {
        Some((handle, request)) => is_encoded_handle(handle) && is_request(request),
        None => is_encoded_handle(rest),
    }
}

/// The create `POST` collections; a transportable package adds `?corrNr=<request>`.
fn is_create(path: &str, query: Option<&str>) -> bool {
    let known = match path {
        GROUPS | CLASSES | INTERFACES | PROGRAMS | PROGRAM_INCLUDES => true,
        _ => path
            .strip_prefix(GROUPS)
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rest| rest.strip_suffix("/fmodules"))
            .is_some_and(|group| is_name(group, false)),
    };

    known
        && match query {
            None => true,
            Some(query) => query.strip_prefix("corrNr=").is_some_and(is_request),
        }
}

/// The create validation: fixed key order, and the object types each path knows with the key
/// that names their scope.
fn is_validation(path: &str, query: &str) -> bool {
    let known: &[(&str, &str)] = if path == FUNCTION_VALIDATION {
        &[("FUGR%2FF", "packagename"), ("FUGR%2FFF", "fugrname")]
    } else {
        &[("CLAS%2FOC", "packagename"), ("INTF%2FOI", "packagename")]
    };
    let mut parts = query.split('&');
    let (Some(object_type), Some(name), Some(text), Some(scope), None) = (
        parts.next().and_then(|part| part.strip_prefix("objtype=")),
        parts.next().and_then(|part| part.strip_prefix("objname=")),
        parts
            .next()
            .and_then(|part| part.strip_prefix("description=")),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };

    known.iter().any(|(known_type, scope_key)| {
        *known_type == object_type
            && scope
                .strip_prefix(scope_key)
                .and_then(|rest| rest.strip_prefix('='))
                .is_some_and(|value| !value.is_empty() && is_encoded(value))
    }) && !name.is_empty()
        && is_encoded(name)
        && is_encoded(text)
}

/// `uri=<function module>`, the object a where-used request asks for; or a program or include
/// as `usage_references_uri` emits it, with exactly `USAGE_REQUEST_XML` as the body.
fn is_where_used_object(query: &str, body: &[u8]) -> bool {
    let Some(uri) = query.strip_prefix("uri=") else {
        return false;
    };

    uri.strip_prefix(PROGRAMS)
        .or_else(|| uri.strip_prefix(PROGRAM_INCLUDES))
        .and_then(|rest| rest.strip_prefix('/'))
        .map_or_else(
            || parse_path(uri).is_some_and(|p| p.kind == Kind::Func && p.part == Part::Object),
            // Plain names only: the M1 program rule would admit `$`, `-`, `.` and `%2f`.
            |name| is_name(name, false) && body == USAGE_REQUEST_XML.as_bytes(),
        )
}

/// Splits an ADT path into its object and the part below it. Names are checked as path segments:
/// a namespaced name has no verified path form except for a program.
fn parse_path(path: &str) -> Option<Path> {
    for (prefix, kind) in [
        (PROGRAMS, Kind::Prog),
        (PROGRAM_INCLUDES, Kind::Incl),
        (CLASSES, Kind::Clas),
        (INTERFACES, Kind::Intf),
    ] {
        let Some(rest) = path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) else {
            continue;
        };
        let (name, tail) = split_segment(rest);

        if !is_name(name, kind == Kind::Prog) {
            return None;
        }

        return Some(Path {
            kind,
            part: part_of(kind, tail)?,
        });
    }

    let rest = path.strip_prefix(GROUPS)?.strip_prefix('/')?;
    let (group, tail) = split_segment(rest);

    if !is_name(group, false) {
        return None;
    }

    if tail.is_empty() {
        return Some(Path {
            kind: Kind::Group,
            part: Part::Object,
        });
    }

    let module_and_tail = tail.strip_prefix("/fmodules/")?;
    let (module, tail) = split_segment(module_and_tail);

    if !is_name(module, false) {
        return None;
    }

    Some(Path {
        kind: Kind::Func,
        part: part_of(Kind::Func, tail)?,
    })
}

/// The first path segment and the rest, which is empty or starts with `/`.
fn split_segment(rest: &str) -> (&str, &str) {
    match rest.find('/') {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    }
}

fn part_of(kind: Kind, tail: &str) -> Option<Part> {
    match (kind, tail) {
        (_, "") => Some(Part::Object),
        (Kind::Group, _) => None,
        (_, "/source/main") => Some(Part::Source),
        (Kind::Prog | Kind::Func | Kind::Intf | Kind::Incl, "/source/main/versions") => {
            Some(Part::SourceVersions)
        }
        (Kind::Incl, "/mainprograms") => Some(Part::MainPrograms),
        (Kind::Incl, _) => None,
        (Kind::Clas, "/objectstructure") => Some(Part::Structure),
        (Kind::Clas, "/includes") => Some(Part::Includes),
        (Kind::Clas, tail) => {
            let include = tail.strip_prefix("/includes/")?;

            if INCLUDES.contains(&include) {
                return Some(Part::IncludeSource);
            }

            let feed = include.strip_suffix("/versions")?;

            (feed == "main" || INCLUDES.contains(&feed)).then_some(Part::IncludeVersions)
        }
        _ => None,
    }
}

/// An object name as one path segment. Existing M1 program paths accepted legal SAP segment
/// characters such as `$`, `-` and `.`; preserve those while accepting only the evidenced `%2f`
/// namespace escape. New M3 object paths remain deliberately narrower.
fn is_name(name: &str, namespace_escape: bool) -> bool {
    if namespace_escape {
        return is_m1_program_name(name);
    }

    let mut rest = name;

    if rest.is_empty() {
        return false;
    }

    while let Some(character) = rest.chars().next() {
        if character.is_ascii_alphanumeric() || character == '_' {
            rest = &rest[1..];
        } else {
            return false;
        }
    }

    true
}

fn is_m1_program_name(name: &str) -> bool {
    if name.is_empty() || matches!(name, "." | "..") {
        return false;
    }

    let mut rest = name;
    while let Some(character) = rest.chars().next() {
        if character == '%' {
            if !rest
                .get(..3)
                .is_some_and(|escape| escape.eq_ignore_ascii_case("%2f"))
            {
                return false;
            }
            rest = &rest[3..];
        } else if character == '/'
            || !(character.is_ascii_alphanumeric() || matches!(character, '_' | '$' | '-' | '.'))
        {
            return false;
        } else {
            rest = &rest[character.len_utf8()..];
        }
    }

    true
}

fn is_request(request: &str) -> bool {
    !request.is_empty()
        && request.len() <= MAX_REQUEST
        && request.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// A lock handle is sent fully percent-encoded; anything else could carry a second parameter.
fn is_encoded_handle(handle: &str) -> bool {
    !handle.is_empty() && is_encoded(handle)
}

/// Unreserved characters and `%XX` escapes only.
fn is_encoded(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') => {
                index += 1;
            }
            b'%' => {
                let escape = bytes.get(index + 1..index + 3);

                if !escape.is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit)) {
                    return false;
                }

                index += 3;
            }
            _ => return false,
        }
    }

    true
}

/// Every `objectReference` of an activation body with all its fields, so a resend is compared
/// whole. A body that is not an `objectReferences` document of references with a `uri` has `None`.
fn references(body: &[u8]) -> Option<Vec<ObjectRef>> {
    let text = std::str::from_utf8(body).ok()?;
    let mut reader = Reader::from_str(text);
    let mut found = Vec::new();
    let mut root_seen = false;

    loop {
        match reader.read_event().ok()? {
            Event::Start(element) | Event::Empty(element) => {
                let name = element.local_name();

                if !root_seen {
                    root_seen = true;

                    if name.into_inner() != "objectReferences" {
                        return None;
                    }
                } else if name.into_inner() == "objectReference" {
                    found.push(reference_of(&element)?);
                } else {
                    return None;
                }
            }
            Event::Eof => return root_seen.then_some(found),
            _ => {}
        }
    }
}

/// The reference of an element; an absent `type` or `name` stays empty and an absent `parentUri`
/// stays `None`, so any difference from the first response is unequal.
fn reference_of(element: &BytesStart<'_>) -> Option<ObjectRef> {
    let mut uri = None;
    let mut found = ObjectRef::default();

    for attribute in element.attributes() {
        let attribute = attribute.ok()?;
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .ok()?
            .into_owned();

        match attribute.key.local_name().into_inner() {
            "uri" => uri = Some(value),
            "type" => found.kind = value,
            "name" => found.name = value,
            "parentUri" => found.parent_uri = Some(value),
            _ => {}
        }
    }

    found.uri = uri?;

    Some(found)
}

#[cfg(test)]
mod tests {
    use super::{Kind, Part, is_encoded, is_entity, is_name, parse_path};

    #[test]
    fn names_are_sap_characters_and_only_a_program_takes_a_namespace_escape() {
        assert!(is_name("zcl_a1", false));
        assert!(!is_name("", false));
        assert!(!is_name("..", false));
        assert!(!is_name("%2e%2e", true));
        assert!(!is_name("%2fns%2fcl", false));
        assert!(is_name("%2fns%2fprog", true));
        assert!(!is_name("é", true));
        assert!(!is_name("", true));
        assert!(!is_name(".", true));
        assert!(!is_name("..", true));
        assert!(is_name("z.prog-1$", true));
    }

    #[test]
    fn an_entity_is_a_plain_or_namespaced_name_of_at_most_30_characters() {
        let longest_plain = "A".repeat(30);
        let longest_namespaced = format!("/N/{}", "B".repeat(27));

        for entity in [
            "T001",
            "T_001",
            "a1",
            "/BIC/AFOO",
            &longest_plain,
            &longest_namespaced,
        ] {
            assert!(is_entity(entity), "{entity}");
        }

        let too_long_plain = "A".repeat(31);
        let too_long_namespaced = format!("/N/{}", "B".repeat(28));

        for entity in [
            "",
            "/",
            "//",
            "/NS",
            "/NS/",
            "//X",
            "/NS/A/B",
            "T 001",
            "T-001",
            "T001%20",
            "\u{e9}",
            "/N-S/X",
            "/NS/X-Y",
            &too_long_plain,
            &too_long_namespaced,
        ] {
            assert!(!is_entity(entity), "{entity}");
        }
    }

    #[test]
    fn encoded_values_have_complete_hex_escapes() {
        assert!(is_encoded("a%2Bb%3d"));
        assert!(!is_encoded("a%2"));
        assert!(!is_encoded("a%2g"));
        assert!(!is_encoded("a b"));
    }

    #[test]
    fn a_path_splits_into_its_kind_and_part() {
        let path = parse_path("/sap/bc/adt/functions/groups/zfg/fmodules/zfm/source/main").unwrap();

        assert_eq!((path.kind, path.part), (Kind::Func, Part::Source));
        assert!(parse_path("/sap/bc/adt/oo/classes/zcl/includes/main").is_none());
    }

    #[cfg(feature = "fault-injection")]
    #[test]
    fn a_fault_target_is_a_write_of_the_exact_method_and_shape() {
        use super::{is_create_post, is_put_request};
        use crate::transport::Request;

        let source = "/sap/bc/adt/programs/programs/zprog/source/main?lockHandle=a%2B";
        let create = "/sap/bc/adt/oo/classes?corrNr=DEVK900001";

        assert!(is_put_request(&Request::put(source, Vec::new())));
        assert!(!is_put_request(&Request::post(source, Vec::new())));
        assert!(!is_put_request(&Request::get(source)));
        assert!(!is_put_request(&Request::put(
            "/sap/bc/adt/discovery",
            Vec::new()
        )));
        assert!(!is_put_request(&Request::put(
            format!("{source}#frag"),
            Vec::new()
        )));

        assert!(is_create_post(&Request::post(create, Vec::new())));
        assert!(!is_create_post(&Request::put(create, Vec::new())));
        assert!(!is_create_post(&Request::get(create)));
        assert!(!is_create_post(&Request::post(
            "/sap/bc/adt/discovery",
            Vec::new()
        )));
        assert!(!is_create_post(&Request::post(
            format!("{create}#frag"),
            Vec::new()
        )));
    }

    #[cfg(feature = "fault-injection")]
    #[test]
    fn a_delete_fault_target_is_a_delete_of_an_object_under_its_lock() {
        use super::is_delete_request;
        use crate::transport::Request;

        let object = "/sap/bc/adt/programs/programs/zprog";
        let delete = format!("{object}?lockHandle=a%2Bb&corrNr=DEVK900001");

        assert!(is_delete_request(&Request::delete(delete.clone())));
        assert!(is_delete_request(&Request::delete(format!(
            "{object}?lockHandle=a%2Bb"
        ))));
        // The method, the shape and the lock all count; a fragment never does.
        assert!(!is_delete_request(&Request::get(delete.clone())));
        assert!(!is_delete_request(&Request::put(
            delete.clone(),
            Vec::new()
        )));
        assert!(!is_delete_request(&Request::post(
            delete.clone(),
            Vec::new()
        )));
        assert!(!is_delete_request(&Request::delete(format!(
            "{delete}#frag"
        ))));
        assert!(!is_delete_request(&Request::delete(object)));
        assert!(!is_delete_request(&Request::delete(
            "/sap/bc/adt/discovery"
        )));
        assert!(!is_delete_request(&Request::delete(format!(
            "{object}/source/main?lockHandle=a%2Bb"
        ))));
    }
}
