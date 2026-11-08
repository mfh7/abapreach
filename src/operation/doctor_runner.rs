//! The `RUNNER` lines of `doctor`: whether the classrun handler exists, how the POST travels, the
//! runner classes a failed run left behind, and the repository preflight. Everything here is a
//! read: one preview of the interface table, one typed search, one metadata read per listed class,
//! and the two previews of the preflight, all on the session the doctor already opened.

use crate::backend::BackendKind;
use crate::config::Connection;
use crate::operation::runner::{PostTransport, post_transport};
use crate::operation::runner_evidence::{Preflight, classrun_interface, preflight};
use crate::runner_protocol::scrub_text;
use crate::transport::{Request, SadtTransport};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fact U2: the classrun POST passes over SADT on every release tested (750 and 740).
pub const SADT_POST_WORKS: bool = true;
/// The `maxResults` of the leftover search.
pub const SEARCH_LIMIT: u32 = 20;
/// A runner class older than this is marked `stale`.
const STALE_AFTER_SECONDS: u64 = 600;
/// Characters of the random suffix of a runner name.
const SUFFIX_LENGTH: usize = 8;
const NO_ADT_ROUTE: &str = "RUNNER post transport: unavailable (no ADT HTTP route)";

/// One runner class that exists in the system.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Leftover {
    pub name: String,
    /// Seconds since the class was created; `None` when its metadata gave no readable time.
    pub age_seconds: Option<u64>,
}

/// What the doctor read for the runner; each part carries the reason text when it could not be
/// read.
pub struct RunnerFacts {
    classrun: Result<bool, String>,
    /// The listed classes and the number of classes the search returned.
    leftovers: Result<(Vec<Leftover>, usize), String>,
    preflight: Result<Preflight, String>,
}

impl RunnerFacts {
    /// Nothing could be read, for the stated reason.
    pub fn unknown(reason: &str) -> Self {
        Self {
            classrun: Err(reason.to_owned()),
            leftovers: Err(reason.to_owned()),
            preflight: Err(reason.to_owned()),
        }
    }

    /// Reads the facts on one open session. A part that fails does not stop the others.
    pub fn read(transport: &mut dyn SadtTransport, connection: &Connection) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());

        Self {
            classrun: classrun_interface(transport)
                .map_err(|reason| format!("not detectable: {}", scrub_text(&reason))),
            leftovers: read_leftovers(transport, &connection.runner_prefix, now),
            preflight: preflight(transport, &connection.client)
                .map_err(|error| scrub_text(&error.to_string())),
        }
    }

    /// The lines between the `RFC SADT probe` line and the `capability` lines.
    pub fn lines(&self, connection: &Connection) -> Vec<String> {
        let mut lines = vec![
            match &self.classrun {
                Ok(true) => "RUNNER classrun: available".to_owned(),
                Ok(false) => {
                    "RUNNER classrun: unavailable (interface IF_OO_ADT_CLASSRUN not found)"
                        .to_owned()
                }
                Err(reason) => format!("RUNNER classrun: unknown ({reason})"),
            },
            format!("RUNNER prefix: {}", connection.runner_prefix),
            post_transport_line(
                connection.adt.is_some(),
                connection.rfc.is_some(),
                SADT_POST_WORKS,
            ),
            match &self.leftovers {
                Ok((found, returned)) => {
                    leftovers_line(&connection.id, found, *returned, SEARCH_LIMIT)
                }
                Err(reason) => format!("RUNNER leftovers: unknown ({reason})"),
            },
        ];

        match &self.preflight {
            Ok(result) => {
                let facts = result
                    .line
                    .strip_prefix("RUNNER: preflight ")
                    .unwrap_or(&result.line);
                let refusal = result.refusal.as_ref().map_or_else(String::new, |reason| {
                    format!("; repository changes are not allowed ({reason})")
                });

                lines.push(format!("RUNNER preflight: {facts}{refusal}"));
                lines.extend(result.notes.iter().map(|note| scrub_text(note)));
            }
            Err(reason) => lines.push(format!("RUNNER preflight: unknown ({reason})")),
        }

        lines.push("RUNNER authorization: not probed".to_owned());

        lines
    }
}

/// The transport of the classrun POST on the route the operations take first (ADT before RFC).
/// A release where the SADT POST fails posts over HTTP, which an RFC-only connection lacks.
pub fn post_transport_line(has_adt: bool, has_rfc: bool, sadt_post_works: bool) -> String {
    let backend = if has_adt {
        BackendKind::AdtHttp
    } else if has_rfc {
        BackendKind::RfcSadt
    } else {
        return NO_ADT_ROUTE.to_owned();
    };

    match post_transport(backend, sadt_post_works) {
        PostTransport::Http if !has_adt => NO_ADT_ROUTE.to_owned(),
        transport => format!("RUNNER post transport: {}", transport.token()),
    }
}

/// `search?operation=quickSearch&query=<PREFIX>*&maxResults=<limit>&objectType=CLAS%2FOC`, the
/// shape the SADT admission lets through.
pub fn search_uri(prefix: &str, limit: u32) -> String {
    format!(
        "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch\
         &query={prefix}*&maxResults={limit}&objectType=CLAS%2FOC"
    )
}

fn read_leftovers(
    transport: &mut dyn SadtTransport,
    prefix: &str,
    now: u64,
) -> Result<(Vec<Leftover>, usize), String> {
    let failed = |reason: String| format!("search failed: {}", scrub_text(&reason));
    let response = transport
        .request(Request::get(search_uri(prefix, SEARCH_LIMIT)))
        .map_err(|error| failed(error.to_string()))?;

    if let Some(error) = response.authorization_failure() {
        return Err(failed(error.to_string()));
    }

    if response.status != 200 {
        return Err(failed(format!("HTTP {}", response.status)));
    }

    let (returned, names) = parse_runner_names(&String::from_utf8_lossy(&response.body), prefix)
        .map_err(|reason| failed(format!("unreadable reply: {reason}")))?;
    let found = names
        .into_iter()
        .map(|name| {
            let age_seconds = created_at(transport, &name)
                .and_then(|stamp| epoch_seconds(&stamp))
                .map(|created| now.saturating_sub(created));

            Leftover { name, age_seconds }
        })
        .collect();

    Ok((found, returned))
}

/// The creation time of a runner class from its metadata, or its change time when the system
/// reports no creation time.
fn created_at(transport: &mut dyn SadtTransport, name: &str) -> Option<String> {
    let uri = format!("/sap/bc/adt/oo/classes/{}", name.to_ascii_lowercase());
    let response = transport.request(Request::get(uri)).ok()?;

    if response.status != 200 {
        return None;
    }

    let text = String::from_utf8_lossy(&response.body);
    let mut reader = Reader::from_str(&text);
    let mut created = None;
    let mut changed = None;

    loop {
        match reader.read_event().ok()? {
            Event::Start(element) | Event::Empty(element) => {
                for attribute in element.attributes().flatten() {
                    let value = attribute
                        .normalized_value(XmlVersion::Implicit1_0)
                        .ok()?
                        .into_owned();

                    match attribute.key.local_name().into_inner() {
                        "createdAt" => created = Some(value),
                        "changedAt" => changed = Some(value),
                        _ => {}
                    }
                }

                break;
            }
            Event::Eof => break,
            _ => {}
        }
    }

    created.or(changed)
}

/// The class references of a search reply: how many classes it returned and which of them are
/// runner names (the prefix plus eight characters of `[0-9A-Z]`). A search for a short prefix can
/// return other classes, which are never reported.
pub fn parse_runner_names(xml: &str, prefix: &str) -> Result<(usize, Vec<String>), String> {
    let mut reader = Reader::from_str(xml);
    let mut returned = 0;
    let mut names = Vec::new();

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().into_inner() == "objectReference" =>
            {
                let (name, kind) = name_and_type(&element)?;

                if kind == "CLAS/OC" {
                    returned += 1;

                    if is_runner_name(&name, prefix) {
                        names.push(name);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok((returned, names))
}

fn name_and_type(element: &BytesStart<'_>) -> Result<(String, String), String> {
    let mut name = String::new();
    let mut kind = String::new();

    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| error.to_string())?
            .into_owned();

        match attribute.key.local_name().into_inner() {
            "name" => name = value,
            "type" => kind = value,
            _ => {}
        }
    }

    Ok((name, kind))
}

fn is_runner_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == SUFFIX_LENGTH
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

/// `YYYY-MM-DDTHH:MM:SS` with an optional fraction and a trailing `Z`, read as UTC, in seconds
/// since 1970. Any other form, including a numeric offset, has no epoch.
pub fn epoch_seconds(text: &str) -> Option<u64> {
    let text = text.strip_suffix('Z').unwrap_or(text);
    let (date, time) = text.split_once('T')?;
    let time = time.split_once('.').map_or(time, |(whole, _)| whole);
    let number = |part: &str, digits: usize| {
        (part.len() == digits && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<i64>().ok())
            .flatten()
    };
    let mut date = date.split('-');
    let mut time = time.split(':');
    let (year, month, day) = (
        number(date.next()?, 4)?,
        number(date.next()?, 2)?,
        number(date.next()?, 2)?,
    );
    let (hour, minute, second) = (
        number(time.next()?, 2)?,
        number(time.next()?, 2)?,
        number(time.next()?, 2)?,
    );

    if date.next().is_some()
        || time.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    // Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

fn days_in_month(year: i64, month: i64) -> i64 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);

    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// An age in the largest whole unit: minutes below an hour, hours below two days, then days.
pub fn age_label(seconds: u64) -> String {
    match seconds {
        0..3_600 => format!("{}m", seconds / 60),
        3_600..172_800 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// The leftover line. `returned` is the number of classes the search returned: reaching `limit`
/// means the list may be cut, so the count prints with a `+`.
pub fn leftovers_line(conn: &str, found: &[Leftover], returned: usize, limit: u32) -> String {
    let cut = returned >= limit as usize;

    if found.is_empty() && !cut {
        return "RUNNER leftovers: none".to_owned();
    }

    let count = format!("{}{}", found.len(), if cut { "+" } else { "" });

    if found.is_empty() {
        return format!("RUNNER leftovers: {count}");
    }

    let names = found
        .iter()
        .map(|leftover| match leftover.age_seconds {
            None => format!("{} age unknown", leftover.name),
            Some(age) if age > STALE_AFTER_SECONDS => {
                format!("{} {} stale", leftover.name, age_label(age))
            }
            Some(age) => format!("{} {}", leftover.name, age_label(age)),
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "RUNNER leftovers: {count} ({names}; remove with abapreach delete --simulate \
         CLAS:<name> --plan <FILE> --conn {conn}, then without --simulate)"
    )
}
