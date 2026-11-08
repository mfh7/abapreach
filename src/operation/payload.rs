//! Rust-side validation and the encoded payload document of the DDIC and `TRAN` runner.
//!
//! The classrun endpoint has no input, so every value of an operation travels inside the
//! generated class source. The generator emits only constant template lines plus the lines made
//! here, and these lines hold nothing but base64 characters: no quote, backslash, brace,
//! backtick, or newline can reach the code. `encode` validates every value first (it runs before
//! any connection); the class re-validates after decoding. The decoded document is UTF-8 text of
//! `KEY=VALUE` lines.

use crate::adt::reference_text;
use crate::hash::{MAX_LINE, local_source_lines, normalize, validate_name_within};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

/// Final file size limit (M6 Task 6): proven over ADT HTTP up to a 1 MiB payload.
pub const MAX_XML_BYTES: usize = 256 * 1024;

/// Width of one literal line of the encoded document.
const LINE_WIDTH: usize = 200;
const MAX_PACKAGE: usize = 30;
const MAX_TR: usize = 20;
const MAX_TRAN_TEXT: usize = 36;
const MAX_REPORT: usize = 40;
const BATCH_TYPES: [&str; 10] = [
    "PROG", "FUNC", "CLAS", "INTF", "FUGR", "TRAN", "TTYP", "TABL", "DTEL", "DOMA",
];
const DTD_MESSAGE: &str = "XML file must not declare a DTD or entity (<!DOCTYPE, <!ENTITY)";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Read,
    Simulate,
    Deploy,
    CreateSim,
    Create,
    DeleteSim,
    Delete,
}

impl Mode {
    fn token(self) -> &'static str {
        match self {
            Self::Read => "READ",
            Self::Simulate => "SIMULATE",
            Self::Deploy => "DEPLOY",
            Self::CreateSim => "CREATE_SIM",
            Self::Create => "CREATE",
            Self::DeleteSim => "DELETE_SIM",
            Self::Delete => "DELETE",
        }
    }

    fn takes_xml(self) -> bool {
        matches!(
            self,
            Self::Simulate | Self::Deploy | Self::CreateSim | Self::Create
        )
    }

    fn creates(self) -> bool {
        matches!(self, Self::CreateSim | Self::Create)
    }

    fn deletes(self) -> bool {
        matches!(self, Self::DeleteSim | Self::Delete)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dtel,
    Doma,
    Tabl,
    Ttyp,
    Tran,
}

impl Kind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Dtel => "DTEL",
            Self::Doma => "DOMA",
            Self::Tabl => "TABL",
            Self::Ttyp => "TTYP",
            Self::Tran => "TRAN",
        }
    }

    pub fn max_name(self) -> usize {
        if self == Self::Tran { 20 } else { 30 }
    }

    /// The record element of the abapGit-like XML and its element that names the object.
    fn xml_names(self) -> (&'static str, &'static str) {
        match self {
            Self::Dtel => ("DD04V", "ROLLNAME"),
            Self::Doma => ("DD01V", "DOMNAME"),
            Self::Tabl => ("DD02V", "TABNAME"),
            Self::Ttyp => ("DD40V", "TYPENAME"),
            Self::Tran => ("", ""),
        }
    }
}

/// Everything one runner operation carries. `nonce` is 32 upper-case hex digits (128 bits).
#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub mode: Mode,
    pub kind: Kind,
    pub name: &'a str,
    pub nonce: &'a str,
    pub package: Option<&'a str>,
    pub tr: Option<&'a str>,
    pub base: Option<&'a str>,
    pub plan: Option<&'a str>,
    /// `TRAN` only: the report the transaction starts.
    pub report: Option<&'a str>,
    /// `TRAN`: the transaction text. DDIC: optional, must equal the description in the XML.
    pub text: Option<&'a str>,
    /// Delete only: every `(type, name)` of the batch, for the `Batch order` check.
    pub batch: &'a [(&'a str, &'a str)],
    /// The text of the XML file of a DDIC object.
    pub xml: Option<&'a str>,
}

impl<'a> Request<'a> {
    pub fn new(mode: Mode, kind: Kind, name: &'a str, nonce: &'a str) -> Self {
        Self {
            mode,
            kind,
            name,
            nonce,
            package: None,
            tr: None,
            base: None,
            plan: None,
            report: None,
            text: None,
            batch: &[],
            xml: None,
        }
    }
}

/// Validates the request and returns the lines of the encoded payload document: base64 only, at
/// most 200 characters each. An `Err` is a STOP with exit 2 and means no connection was made.
pub fn encode(request: &Request<'_>) -> Result<Vec<String>, String> {
    let (mode, kind) = (request.mode, request.kind);
    let tran = kind == Kind::Tran;

    if tran && matches!(mode, Mode::Simulate | Mode::Deploy) {
        return Err("TRAN has no deploy; use create or delete".to_owned());
    }

    let name = object_name(request.name, kind.max_name(), mode != Mode::Read)?;
    let nonce = nonce(request.nonce)?;
    let package = applies("--package", request.package, mode.creates(), false)?
        .map(|value| {
            token("--package", value, MAX_PACKAGE, "A-Z 0-9 _ $ /", |c| {
                c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | '$' | '/')
            })
        })
        .transpose()?;
    let tr = applies("--tr", request.tr, mode != Mode::Read, false)?
        .map(|value| {
            token("--tr", value, MAX_TR, "A-Z 0-9", |c| {
                c.is_ascii_uppercase() || c.is_ascii_digit()
            })
        })
        .transpose()?;
    let base = applies(
        "--base",
        request.base,
        matches!(mode, Mode::Simulate | Mode::Deploy),
        mode == Mode::Deploy,
    )?
    .map(|value| hash("--base", value))
    .transpose()?;
    let plan = applies("plan hash", request.plan, mode == Mode::Delete, true)?
        .map(|value| hash("plan hash", value))
        .transpose()?;
    let report = applies("--prog", request.report, tran && mode.creates(), true)?
        .map(|value| object_name(value, MAX_REPORT, true))
        .transpose()?;
    // A TRAN text is the transaction text; a DDIC text only has to agree with the XML.
    let text = applies(
        "--text",
        request.text,
        if tran {
            mode.creates()
        } else {
            mode.takes_xml()
        },
        tran,
    )?;
    let batch = batch(request.batch, mode.deletes())?;
    let rows = applies("XML file", request.xml, mode.takes_xml() && !tran, !tran)?
        .map(|xml| xml_rows(xml, kind, &name, text.filter(|_| !tran)))
        .transpose()?;
    let text = text.filter(|_| tran).map(tran_text).transpose()?;

    let mut lines = vec![
        "FORMAT=1".to_owned(),
        format!("NONCE={nonce}"),
        format!("MODE={}", mode.token()),
        format!("TYPE={}", kind.token()),
        format!("NAME={name}"),
    ];
    let optional = [
        ("PACKAGE", package),
        ("TR", tr),
        ("BASE", base),
        ("PLAN", plan),
        ("REPORT", report),
        ("TEXT", text),
    ];

    lines.extend(
        optional
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| format!("{key}={value}"))),
    );
    lines.extend(batch.into_iter().map(|entry| format!("BATCH={entry}")));

    if let Some(rows) = rows {
        lines.push(format!("XMLROWS={}", rows.len()));
        lines.extend(rows.into_iter().map(|row| format!("XML={row}")));
    }

    let mut document = lines.join("\n");
    document.push('\n');

    Ok(wrap(&base64(document.as_bytes())))
}

/// A field must be present exactly where the operation takes it.
fn applies<'a>(
    label: &str,
    value: Option<&'a str>,
    allowed: bool,
    required: bool,
) -> Result<Option<&'a str>, String> {
    match (value, allowed) {
        (Some(_), false) => Err(format!("{label} does not apply to this operation")),
        (None, true) if required => Err(format!("{label} is required for this operation")),
        _ => Ok(value),
    }
}

/// The object grammar of the tool with the length of the type. Everything except `read` stays
/// inside the Z/Y customer scope, which also rules out the `/NAMESPACE/` forms.
fn object_name(name: &str, max: usize, customer_only: bool) -> Result<String, String> {
    validate_name_within(name, max)?;

    let name = name.to_ascii_uppercase();

    if customer_only && !name.starts_with(['Z', 'Y']) {
        return Err(format!(
            "write and delete accept only Z and Y names: {name}"
        ));
    }

    Ok(name)
}

fn token(
    label: &str,
    value: &str,
    max: usize,
    charset: &str,
    allowed: fn(char) -> bool,
) -> Result<String, String> {
    let value = value.to_ascii_uppercase();

    if value.is_empty() || value.chars().count() > max || !value.chars().all(allowed) {
        return Err(format!(
            "{label} must be 1 to {max} characters of {charset}: {value:?}"
        ));
    }

    Ok(value)
}

fn hash(label: &str, value: &str) -> Result<String, String> {
    let value = value.to_ascii_lowercase();

    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(value)
    } else {
        Err(format!("{label} must be 64 hex digits: {value:?}"))
    }
}

fn nonce(value: &str) -> Result<&str, String> {
    if value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    {
        Ok(value)
    } else {
        Err("nonce must be 32 upper-case hex digits".to_owned())
    }
}

fn tran_text(text: &str) -> Result<String, String> {
    if text.is_empty() || text.chars().count() > MAX_TRAN_TEXT || text.chars().any(char::is_control)
    {
        return Err(format!(
            "--text must be 1 to {MAX_TRAN_TEXT} printable characters: {text:?}"
        ));
    }

    Ok(text.to_owned())
}

fn batch(entries: &[(&str, &str)], allowed: bool) -> Result<Vec<String>, String> {
    if !allowed && !entries.is_empty() {
        return Err("batch does not apply to this operation".to_owned());
    }

    entries
        .iter()
        .map(|(kind, name)| {
            if !BATCH_TYPES.contains(kind) {
                return Err(format!("unknown object type in the batch: {kind:?}"));
            }

            Ok(format!("{kind}:{}", object_name(name, 40, true)?))
        })
        .collect()
}

/// The checked rows of an XML file: size, DTD and entity declarations, row width, control
/// characters, then the object the file is for and the `--text` rule.
fn xml_rows(xml: &str, kind: Kind, name: &str, text: Option<&str>) -> Result<Vec<String>, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err(format!("XML file larger than {MAX_XML_BYTES} bytes"));
    }

    // The transformation runs on the server, so no declaration may reach it, in any case.
    let lower = xml.to_ascii_lowercase();

    if lower.contains("<!doctype") || lower.contains("<!entity") {
        return Err(DTD_MESSAGE.to_owned());
    }

    let rows = normalize(local_source_lines(xml));

    if rows.is_empty() {
        return Err("XML file is empty".to_owned());
    }

    for (index, row) in rows.iter().enumerate() {
        if row.chars().count() > MAX_LINE {
            return Err(format!(
                "XML row longer than {MAX_LINE} characters (row {})",
                index + 1
            ));
        }

        if row.chars().any(|c| c.is_control() && c != '\t') {
            return Err(format!("XML row {} has a control character", index + 1));
        }
    }

    let found = scan(&rows.join("\n"), kind)?;
    let file_name = found.name.as_deref().unwrap_or_default();

    if file_name != name {
        return Err(wrong_object(file_name, kind, name));
    }

    if kind == Kind::Tabl
        && let Some(row) = found
            .rows
            .iter()
            .map(|row| row.as_deref().unwrap_or_default())
            .find(|row| *row != name)
    {
        return Err(wrong_object(row, kind, name));
    }

    if let Some(text) = text
        && found.description.as_deref() != Some(text)
    {
        return Err("--text differs from the description in the XML file".to_owned());
    }

    Ok(rows)
}

fn wrong_object(file_name: &str, kind: Kind, name: &str) -> String {
    let shown: String = file_name
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();

    format!("XML file is for {shown}, not {} {name}", kind.token())
}

#[derive(Default)]
struct Found {
    name: Option<String>,
    description: Option<String>,
    /// The `TABNAME` of every `DD03P` row; `None` when the row has none.
    rows: Vec<Option<String>>,
}

fn scan(xml: &str, kind: Kind) -> Result<Found, String> {
    let (record, name_tag) = kind.xml_names();
    let mut reader = Reader::from_str(xml);
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut found = Found::default();

    loop {
        let event = reader
            .read_event()
            .map_err(|error| format!("XML file cannot be read: {error}"))?;

        match event {
            Event::Start(element) => {
                path.push(element.local_name().into_inner().to_ascii_uppercase());
                text.clear();
                found.open(&path);
            }
            Event::Empty(element) => {
                path.push(element.local_name().into_inner().to_ascii_uppercase());
                found.open(&path);
                found.close(&path, "", record, name_tag);
                path.pop();
            }
            Event::Text(content) => text.push_str(&content.xml_content(XmlVersion::Implicit1_0)),
            Event::CData(content) => text.push_str(&content.into_inner()),
            Event::GeneralRef(reference) => text.push_str(&reference_text(&reference)?),
            Event::End(_) => {
                found.close(&path, &text, record, name_tag);
                path.pop();
                text.clear();
            }
            Event::Eof => return Ok(found),
            _ => {}
        }
    }
}

impl Found {
    fn open(&mut self, path: &[String]) {
        if path.last().is_some_and(|last| last == "DD03P") {
            self.rows.push(None);
        }
    }

    fn close(&mut self, path: &[String], text: &str, record: &str, name_tag: &str) {
        let [.., parent, last] = path else {
            return;
        };

        if parent == record && last == name_tag {
            self.name.get_or_insert_with(|| text.trim().to_owned());
        } else if parent == record && last == "DDTEXT" {
            self.description.get_or_insert_with(|| text.to_owned());
        } else if parent == "DD03P"
            && last == "TABNAME"
            && let Some(row) = self.rows.last_mut()
        {
            row.get_or_insert_with(|| text.trim().to_owned());
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |acc, (index, &byte)| {
            acc | (u32::from(byte) << (16 - 8 * index))
        });

        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(BASE64[(bits >> (18 - 6 * index)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }

    out
}

fn wrap(encoded: &str) -> Vec<String> {
    encoded
        .as_bytes()
        .chunks(LINE_WIDTH)
        .map(|chunk| chunk.iter().map(|&byte| char::from(byte)).collect())
        .collect()
}
