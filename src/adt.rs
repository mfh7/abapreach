use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

pub fn prog_uri(name: &str) -> String {
    format!(
        "/sap/bc/adt/programs/programs/{}",
        name.to_ascii_lowercase()
    )
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
                let resolved = match reference.resolve_char_ref().map_err(|e| e.to_string())? {
                    Some(character) => character.to_string(),
                    None => resolve_predefined_entity(&reference)
                        .ok_or_else(|| format!("unknown entity &{};", &*reference))?
                        .to_owned(),
                };

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
