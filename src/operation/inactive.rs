//! `inactive`: the inactive objects of the logon user, from `GET /sap/bc/adt/activation/inactiveobjects`.
//!
//! The reply is read tolerantly: any element that is not listed here is ignored. Two shapes are
//! known: the `ioc:inactiveObjects` document (an `ioc:object` carries the user, its `ioc:ref` the
//! object) and the flat 7.40 feed of `adtcore:objectReference` elements, which names no user.

use crate::adt::{ObjectRef, parse_versions};
use crate::domain::{Outcome, Status};
use crate::operation::deploy::INACTIVE_OBJECTS_ACCEPT;
use crate::operation::quality::QualityError;
use crate::transport::{Request, SadtTransport};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

pub const INACTIVE_URI: &str = "/sap/bc/adt/activation/inactiveobjects";
/// What `--all` sends as the user filter: every user's entries. Assumed, not yet probed live.
pub const ALL_USERS: &str = "*";
const PROGRAMS_PREFIX: &str = "/sap/bc/adt/programs/programs/";

/// One object of the inactive list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InactiveEntry {
    /// The ADT type, such as `PROG/P`, `CLAS/OC` or `FUGR/FF`.
    pub kind: String,
    pub name: String,
    pub uri: String,
    pub parent_uri: Option<String>,
    /// The user the entry belongs to; `None` when the reply names none.
    pub user: Option<String>,
}

impl InactiveEntry {
    /// The program an include belongs to, from its parent URI.
    pub fn main_program(&self) -> Option<String> {
        let parent = self.parent_uri.as_deref()?;
        let path = parent.split(['?', '#']).next().unwrap_or_default();
        let name = path.strip_prefix(PROGRAMS_PREFIX)?;

        (!name.is_empty() && !name.contains('/')).then(|| {
            name.replace("%2f", "/")
                .replace("%2F", "/")
                .to_ascii_uppercase()
        })
    }

    /// `<ADT type> <name> <user>`; an include also names its main program.
    pub fn line(&self) -> String {
        let user = self.user.as_deref().unwrap_or("?");
        let mut line = format!("{} {} {}", self.kind, self.name, user);

        if self.kind == "PROG/I"
            && let Some(main) = self.main_program()
        {
            line.push_str(&format!(" main {main}"));
        }

        line
    }
}

/// The request URI: the plain list is the logon user's own.
pub fn inactive_uri(all: bool) -> String {
    if all {
        format!("{INACTIVE_URI}?USERNAME={ALL_USERS}")
    } else {
        INACTIVE_URI.to_owned()
    }
}

/// The inactive-objects request.
pub fn inactive_request(all: bool) -> Request {
    let mut request = Request::get(inactive_uri(all));

    request
        .headers
        .insert("Accept".to_owned(), INACTIVE_OBJECTS_ACCEPT.to_owned());

    request
}

/// The entries of an inactive-objects reply. Sub-objects of a class (a `#` fragment in the URI)
/// are parts of their class, not objects of their own, and are left out; so is a repeated entry.
pub fn parse_inactive_objects(xml: &str) -> Result<Vec<InactiveEntry>, String> {
    let mut reader = Reader::from_str(xml);
    let mut entries: Vec<InactiveEntry> = Vec::new();
    let mut object_user: Option<String> = None;
    let mut in_object = false;
    let mut in_transport = false;

    loop {
        let event = reader.read_event().map_err(|error| error.to_string())?;
        let (element, empty) = match &event {
            Event::Start(element) => (Some(element), false),
            Event::Empty(element) => (Some(element), true),
            _ => (None, false),
        };

        if let Some(element) = element {
            match element.local_name().into_inner() {
                "object" => {
                    in_object = !empty;
                    object_user = attribute(element, "user")?.filter(|user| !user.is_empty());
                }
                "transport" => in_transport = !empty,
                "ref" if in_object && !in_transport => {
                    push(&mut entries, entry_of(element, object_user.clone())?);
                }
                "objectReference" if !in_object && !in_transport => {
                    let user = attribute(element, "responsible")?.filter(|user| !user.is_empty());

                    push(&mut entries, entry_of(element, user)?);
                }
                _ => {}
            }
        }

        match event {
            Event::End(element) => match element.local_name().into_inner() {
                "object" => {
                    in_object = false;
                    object_user = None;
                }
                "transport" => in_transport = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(entries)
}

fn push(entries: &mut Vec<InactiveEntry>, entry: InactiveEntry) {
    let member = entry.uri.contains('#');
    let known = entries
        .iter()
        .any(|known| known.kind == entry.kind && known.name.eq_ignore_ascii_case(&entry.name));

    if !member && !known && !entry.kind.is_empty() && !entry.name.is_empty() {
        entries.push(entry);
    }
}

fn entry_of(element: &BytesStart<'_>, user: Option<String>) -> Result<InactiveEntry, String> {
    let mut found = ObjectRef::default();

    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| error.to_string())?
            .into_owned();

        match attribute.key.local_name().into_inner() {
            "uri" => found.uri = value,
            "type" => found.kind = value,
            "name" => found.name = value,
            "parentUri" => found.parent_uri = Some(value),
            _ => {}
        }
    }

    Ok(InactiveEntry {
        kind: found.kind.trim().to_owned(),
        name: found.name.trim().to_owned(),
        uri: found.uri,
        parent_uri: found.parent_uri,
        user: user.map(|user| user.trim().to_owned()),
    })
}

fn attribute(element: &BytesStart<'_>, local: &str) -> Result<Option<String>, String> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;

        if attribute.key.local_name().into_inner() == local {
            return attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .map(|value| Some(value.into_owned()))
                .map_err(|error| error.to_string());
        }
    }

    Ok(None)
}

/// Reads the inactive list. `all` asks for every user's entries.
pub fn fetch_inactive(
    transport: &mut dyn SadtTransport,
    all: bool,
) -> Result<Vec<InactiveEntry>, QualityError> {
    let response = transport
        .request(inactive_request(all))
        .map_err(|error| QualityError::from_transport(&error))?;

    if let Some(error) = response.authorization_failure() {
        return Err(QualityError::from_transport(&error));
    }

    if response.status != 200 {
        return Err(QualityError::rejected(format!(
            "the inactive objects list answered HTTP {}{}",
            response.status,
            if all {
                " (this release may not offer --all)"
            } else {
                ""
            }
        )));
    }

    let text = String::from_utf8_lossy(&response.body);

    parse_inactive_objects(&text).map_err(|message| {
        QualityError::rejected(format!("cannot read the inactive objects list: {message}"))
    })
}

/// The user who wrote the inactive version, from a version feed; `None` when the feed holds none.
pub fn inactive_author(feed: &str) -> Result<Option<String>, String> {
    Ok(parse_versions(feed)?
        .into_iter()
        .find(|entry| entry.is_inactive())
        .map(|entry| entry.author)
        .filter(|author| !author.is_empty()))
}

/// The entries to show: without `all` only the logon user's own stay; an entry that names no
/// user is kept, because the plain list is the logon user's own on the releases that name none.
pub fn list(
    transport: &mut dyn SadtTransport,
    all: bool,
    user: &str,
) -> Result<Vec<InactiveEntry>, QualityError> {
    let mut entries = fetch_inactive(transport, all)?;

    if !all {
        entries.retain(|entry| {
            entry
                .user
                .as_deref()
                .is_none_or(|owner| owner.eq_ignore_ascii_case(user))
        });
    }

    Ok(entries)
}

/// The `inactive` command: one line per entry.
pub fn run(
    transport: &mut dyn SadtTransport,
    all: bool,
    user: &str,
) -> Result<Outcome, QualityError> {
    let entries = list(transport, all, user)?;
    let mut lines: Vec<String> = entries.iter().map(InactiveEntry::line).collect();

    lines.push(format!("SUMMARY: {} inactive objects", entries.len()));

    Ok(Outcome {
        status: Some(Status::Active),
        exit: 0,
        stdout: lines.join("\n"),
        stderr: String::new(),
    })
}
