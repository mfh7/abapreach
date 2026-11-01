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

    fn push(&mut self, field: Field, text: &str) {
        let target = match field {
            Field::Id => &mut self.id,
            Field::Author => &mut self.author,
        };

        target.push_str(text);
    }
}

/// The `atom:entry` elements of a version list by local name: `atom:id` and
/// `atom:author/atom:name`. A body that is not XML is an error, never an empty list.
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
            Event::GeneralRef(reference) => {
                if let (Some(entry), Some(field)) = (open.as_mut(), field) {
                    entry.push(field, &reference_text(&reference)?);
                }
            }
            Event::End(element) => match element.local_name().into_inner() {
                "id" | "name" => field = None,
                "author" => in_author = false,
                "entry" => entries.extend(open.take()),
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
