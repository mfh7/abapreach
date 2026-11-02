use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// The `atom:id` of the inactive version in the version list of an object.
pub const INACTIVE_VERSION_ID: &str = "99999";

/// The ADT URI of a program: the name is lowercased and every namespace `/` becomes `%2f`.
pub fn prog_uri(name: &str) -> String {
    format!(
        "/sap/bc/adt/programs/programs/{}",
        name.to_ascii_lowercase().replace('/', "%2f")
    )
}

/// The ADT URI of the source version list of a program: `GET <source>/versions`. The version
/// list of the program resource itself does not exist on 7.40 and 7.50.
pub fn versions_uri(program_uri: &str) -> String {
    format!("{program_uri}/source/main/versions")
}

/// The quickSearch that resolves the function group of a function module. The untyped search
/// returns an empty list while the module has only an inactive version, the typed one does not.
pub fn function_group_search_uri(name: &str) -> String {
    let query = format!(
        "operation=quickSearch&query={}&maxResults=10",
        name.to_ascii_uppercase()
    );

    format!("/sap/bc/adt/repository/informationsystem/search?{query}&objectType=FUGR%2FFF")
}

/// The function group of a function module from a quickSearch result. The `adtcore:uri` of the
/// matching `FUGR/FF` reference is `/sap/bc/adt/functions/groups/<fg>/fmodules/<fm>`; a result
/// without such a reference has `None`.
pub fn function_group_of(xml: &str, module: &str) -> Result<Option<String>, String> {
    let mut reader = Reader::from_str(xml);
    let mut group = None;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) | Event::Empty(element)
                if local_name(&element) == "objectReference" =>
            {
                if let Some(found) = group_of_reference(&element, module)? {
                    group = Some(found);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(group)
}

fn group_of_reference(element: &BytesStart<'_>, module: &str) -> Result<Option<String>, String> {
    let mut name = String::new();
    let mut uri = String::new();

    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| error.to_string())?
            .into_owned();

        match attribute.key.local_name().into_inner() {
            "name" => name = value,
            "uri" => uri = value,
            _ => {}
        }
    }

    if !name.eq_ignore_ascii_case(module) {
        return Ok(None);
    }

    Ok(group_of_uri(&uri, module))
}

fn group_of_uri(uri: &str, module: &str) -> Option<String> {
    let rest = uri.strip_prefix("/sap/bc/adt/functions/groups/")?;
    let (group, tail) = rest.split_once("/fmodules/")?;

    if group.is_empty() || tail.is_empty() || tail.contains('/') {
        return None;
    }

    if !tail.eq_ignore_ascii_case(module) {
        return None;
    }

    Some(group.to_ascii_uppercase())
}

/// One entry of `GET <source>/versions`: the version number and the user who wrote it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VersionEntry {
    pub id: String,
    pub author: String,
}

impl VersionEntry {
    pub fn is_inactive(&self) -> bool {
        self.id == INACTIVE_VERSION_ID
    }

    fn trimmed(mut self) -> Self {
        self.id = self.id.trim().to_owned();
        self.author = self.author.trim().to_owned();

        self
    }

    fn push(&mut self, field: Field, text: &str) {
        let target = match field {
            Field::Id => &mut self.id,
            Field::Author => &mut self.author,
        };

        target.push_str(text);
    }
}

/// The `atom:entry` elements of a version list by local name: `atom:id` and
/// `atom:author/atom:name`. A body that is not XML is an error, never an empty list. The values
/// are trimmed, because a pretty-printed feed puts the indentation of the element in the text.
pub fn parse_versions(xml: &str) -> Result<Vec<VersionEntry>, String> {
    let mut reader = Reader::from_str(xml);
    let mut entries = Vec::new();
    let mut open: Option<VersionEntry> = None;
    let mut field: Option<Field> = None;
    let mut in_author = false;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) => match local_name(&element) {
                "entry" => {
                    open = Some(VersionEntry::default());
                    field = None;
                    in_author = false;
                }
                "id" if open.is_some() => field = Some(Field::Id),
                "author" => in_author = open.is_some(),
                "name" if in_author => field = Some(Field::Author),
                _ => {}
            },
            Event::Text(text) => {
                if let (Some(entry), Some(field)) = (open.as_mut(), field) {
                    entry.push(field, &text.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::CData(text) => {
                if let (Some(entry), Some(field)) = (open.as_mut(), field) {
                    entry.push(field, &text.into_inner());
                }
            }
            Event::GeneralRef(reference) => {
                if let (Some(entry), Some(field)) = (open.as_mut(), field) {
                    entry.push(field, &reference_text(&reference)?);
                }
            }
            Event::End(element) => match element.local_name().into_inner() {
                "id" | "name" => field = None,
                "author" => in_author = false,
                "entry" => entries.extend(open.take().map(VersionEntry::trimmed)),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(entries)
}

#[derive(Clone, Copy)]
enum Field {
    Id,
    Author,
}

/// What the body of an activation answer is. The status is 200 in every case, so the root of the
/// body decides: nothing, a worklist that activated nothing, or messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationBody {
    Empty,
    /// `ioc:inactiveObjects`: the objects SAP still wants activated; nothing was activated.
    Worklist,
    /// `chkl:messages`.
    Messages,
    Other,
}

pub fn classify_activation(body: &str) -> ActivationBody {
    if body.trim().is_empty() {
        return ActivationBody::Empty;
    }

    let mut reader = Reader::from_str(body);

    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                return match local_name(&element) {
                    "inactiveObjects" => ActivationBody::Worklist,
                    "messages" => ActivationBody::Messages,
                    _ => ActivationBody::Other,
                };
            }
            Ok(Event::Eof) | Err(_) => return ActivationBody::Other,
            Ok(_) => {}
        }
    }
}

/// An object reference of a worklist (`ioc:object/ioc:ref`), kept as listed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObjectRef {
    pub uri: String,
    pub kind: String,
    pub name: String,
    pub parent_uri: Option<String>,
}

/// The object references of an `ioc:inactiveObjects` list. An entry without `ioc:object` carries
/// only its transport request, and a reference inside `ioc:transport` is the request, so neither
/// is an object.
pub fn parse_object_refs(xml: &str) -> Result<Vec<ObjectRef>, String> {
    let mut reader = Reader::from_str(xml);
    let mut refs = Vec::new();
    let mut in_object = false;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) => match local_name(&element) {
                "object" => in_object = true,
                "ref" if in_object => refs.push(object_ref(&element)?),
                _ => {}
            },
            Event::Empty(element) if in_object && local_name(&element) == "ref" => {
                refs.push(object_ref(&element)?);
            }
            Event::End(element) if element.local_name().into_inner() == "object" => {
                in_object = false;
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(refs)
}

fn object_ref(element: &BytesStart<'_>) -> Result<ObjectRef, String> {
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

    Ok(found)
}

/// The body of the where-used request: `usageReferences` takes the object from the query.
pub const USAGE_REQUEST_XML: &str = r#"<?xml version="1.0" encoding="ASCII"?><usagereferences:usageReferenceRequest xmlns:usagereferences="http://www.sap.com/adt/ris/usageReferences"><usagereferences:affectedObjects/></usagereferences:usageReferenceRequest>"#;

/// The where-used request of an object: the `uri` query keeps the slashes of the object URI
/// literal, every other character outside the URI-safe set is percent-encoded.
pub fn usage_references_uri(object_uri: &str) -> String {
    let mut query = String::new();

    for byte in object_uri.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                query.push(char::from(byte));
            }
            other => query.push_str(&format!("%{other:02X}")),
        }
    }

    format!("/sap/bc/adt/repository/informationsystem/usageReferences?uri={query}")
}

/// One calling object of a where-used result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Caller {
    pub kind: String,
    pub name: String,
}

/// The callers of a `usageReferenceResult`. The result also lists the package of each caller
/// (`DEVC/K`), which is not a caller, so every package entry is dropped; a repeated object
/// counts once.
pub fn parse_callers(xml: &str) -> Result<Vec<Caller>, String> {
    let mut reader = Reader::from_str(xml);
    let mut callers: Vec<Caller> = Vec::new();

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) | Event::Empty(element)
                if local_name(&element) == "adtObject" =>
            {
                let mut caller = Caller::default();

                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    let value = attribute
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|error| error.to_string())?
                        .into_owned();

                    match attribute.key.local_name().into_inner() {
                        "name" => caller.name = value,
                        "type" => caller.kind = value,
                        _ => {}
                    }
                }

                if !caller.name.is_empty()
                    && !caller.kind.starts_with("DEVC")
                    && !callers.contains(&caller)
                {
                    callers.push(caller);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(callers)
}

/// One checkrun or activation message, printed as `TYPE row N: MSG`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdtMessage {
    pub kind: String,
    pub row: u32,
    pub text: String,
}

impl AdtMessage {
    pub fn is_error(&self) -> bool {
        self.kind == "E"
    }

    pub fn render(&self) -> String {
        format!("{} row {}: {}", self.kind, self.row, self.text)
    }
}

/// Reads `checkMessage` (checkrun) and `msg` (activation `chkl:messages`) elements by local name.
pub fn parse_messages(xml: &str) -> Result<Vec<AdtMessage>, String> {
    let mut reader = Reader::from_str(xml);
    let mut messages = Vec::new();
    let mut open: Option<AdtMessage> = None;
    let mut in_text = false;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) => match local_name(&element) {
                "checkMessage" => messages.push(message_from(&element, "shortText", "uri")?),
                "msg" => open = Some(message_from(&element, "", "href")?),
                "txt" => in_text = open.is_some(),
                _ => {}
            },
            Event::Empty(element) => match local_name(&element) {
                "checkMessage" => messages.push(message_from(&element, "shortText", "uri")?),
                "msg" => messages.push(message_from(&element, "", "href")?),
                _ => {}
            },
            Event::Text(text) if in_text => {
                if let Some(message) = open.as_mut() {
                    message
                        .text
                        .push_str(&text.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::GeneralRef(reference) if in_text => {
                let resolved = reference_text(&reference)?;

                if let Some(message) = open.as_mut() {
                    message.text.push_str(&resolved);
                }
            }
            Event::End(element) => match element.local_name().into_inner() {
                "txt" => in_text = false,
                "msg" => messages.extend(open.take()),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(messages)
}

/// Messages as `TYPE row N: MSG` lines; an unparsable body yields no lines (use
/// `parse_messages` to detect that case).
pub fn format_messages(xml: &str) -> Vec<String> {
    parse_messages(xml)
        .unwrap_or_default()
        .iter()
        .map(AdtMessage::render)
        .collect()
}

fn local_name<'a>(element: &'a BytesStart<'_>) -> &'a str {
    element.local_name().into_inner()
}

/// Text of a `&name;` or `&#nnn;` reference inside element text.
fn reference_text(reference: &BytesRef<'_>) -> Result<String, String> {
    let resolved = match reference
        .resolve_char_ref()
        .map_err(|error| error.to_string())?
    {
        Some(character) => character.to_string(),
        None => resolve_predefined_entity(reference)
            .ok_or_else(|| format!("unknown entity &{};", &**reference))?
            .to_owned(),
    };

    Ok(resolved)
}

fn message_from(
    element: &BytesStart<'_>,
    text_attribute: &str,
    location_attribute: &str,
) -> Result<AdtMessage, String> {
    let (mut kind, mut text, mut location, mut line) =
        (String::new(), String::new(), String::new(), String::new());

    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| error.to_string())?
            .into_owned();

        match attribute.key.local_name().into_inner() {
            "type" => kind = value,
            "line" => line = value,
            name if name == text_attribute => text = value,
            name if name == location_attribute => location = value,
            _ => {}
        }
    }

    let row = row_of(&location).or_else(|| line.parse().ok()).unwrap_or(0);

    Ok(AdtMessage { kind, row, text })
}

fn row_of(location: &str) -> Option<u32> {
    let (_, fragment) = location.split_once('#')?;
    let start = fragment
        .split('&')
        .find_map(|part| part.strip_prefix("start="))?;

    start.split(',').next()?.parse().ok()
}
