//! The request boundary of the RFC/SADT tunnel: the one function that decides whether a request
//! may leave the process. It admits exactly the request shapes the commands send (M1 programs and
//! the M3 function module, class and interface operations) and nothing broader; every shape is
//! parsed rather than prefix-matched, so a traversal, a wrong object-kind path or an extra query
//! parameter is refused. Pure logic with no platform dependency.

use super::Request;
use crate::adt::{ActivationBody, ObjectRef, classify_activation, parse_object_refs};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

const DISCOVERY: &str = "/sap/bc/adt/discovery";
const INACTIVE_OBJECTS: &str = "/sap/bc/adt/activation/inactiveobjects";
const CHECKRUNS: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const ACTIVATION: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=";
const SEARCH: &str = "/sap/bc/adt/repository/informationsystem/search";
const USAGE_REFERENCES: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";
const FUNCTION_VALIDATION: &str = "/sap/bc/adt/functions/validation";
const OBJECT_VALIDATION: &str = "/sap/bc/adt/oo/validation/objectname";
const GROUPS: &str = "/sap/bc/adt/functions/groups";
const CLASSES: &str = "/sap/bc/adt/oo/classes";
const INTERFACES: &str = "/sap/bc/adt/oo/interfaces";
const PROGRAMS: &str = "/sap/bc/adt/programs/programs";
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
    /// A function group: readable as metadata, never locked or written.
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
        let Some(parsed) = parse_path(&reference.uri) else {
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
        (Part::SourceVersions | Part::IncludeVersions, None) => true,
        (Part::Structure, Some(OBJECT_STRUCTURE)) => true,
        // The metadata the create reconciliation and the group package lookup read.
        (Part::Object, None) => matches!(parsed.kind, Kind::Group | Kind::Func | Kind::Intf),
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

fn is_put(path: &str, query: Option<&str>) -> bool {
    let Some(parsed) = parse_path(path) else {
        return false;
    };

    matches!(parsed.part, Part::Source | Part::IncludeSource)
        && query.is_some_and(is_lock_and_request)
}

fn is_post(request: &Request, uri: &str, path: &str, query: Option<&str>) -> bool {
    if uri == CHECKRUNS {
        return has_content_type(request, "application/*");
    }

    if path == USAGE_REFERENCES {
        return query.is_some_and(is_where_used_object);
    }

    if path == FUNCTION_VALIDATION || path == OBJECT_VALIDATION {
        return query.is_some_and(|query| is_validation(path, query));
    }

    if is_create(path, query) {
        return has_content_type(request, "application/*");
    }

    let Some(parsed) = parse_path(path) else {
        return false;
    };

    match parsed.part {
        Part::Object if parsed.kind != Kind::Group => query.is_some_and(|query| {
            query == LOCK || query.strip_prefix(UNLOCK).is_some_and(is_encoded_handle)
        }),
        // The include POST runs under the class lock.
        Part::Includes => {
            query.is_some_and(is_lock_and_request) && has_content_type(request, "application/*")
        }
        _ => false,
    }
}

fn has_content_type(request: &Request, expected: &str) -> bool {
    request
        .headers
        .get("Content-Type")
        .is_some_and(|value| value == expected)
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
        GROUPS | CLASSES | INTERFACES => true,
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

/// `uri=<function module>`, the object a where-used request asks for.
fn is_where_used_object(query: &str) -> bool {
    query
        .strip_prefix("uri=")
        .and_then(parse_path)
        .is_some_and(|parsed| parsed.kind == Kind::Func && parsed.part == Part::Object)
}

/// Splits an ADT path into its object and the part below it. Names are checked as path segments:
/// a namespaced name has no verified path form except for a program.
fn parse_path(path: &str) -> Option<Path> {
    for (prefix, kind) in [
        (PROGRAMS, Kind::Prog),
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
        (Kind::Prog | Kind::Func | Kind::Intf, "/source/main/versions") => {
            Some(Part::SourceVersions)
        }
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
    use super::{Kind, Part, is_encoded, is_name, parse_path};

    #[test]
    fn names_are_sap_characters_and_only_a_program_takes_a_namespace_escape() {
        assert!(is_name("zcl_a1", false));
        assert!(!is_name("", false));
        assert!(!is_name("..", false));
        assert!(!is_name("%2e%2e", true));
        assert!(!is_name("%2fns%2fcl", false));
        assert!(is_name("%2fns%2fprog", true));
        assert!(!is_name("é", true));
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
}
