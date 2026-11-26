//! `dumps` and `dump`: the ST22 short dumps, read from table `SNAP`.
//!
//! SNAP goes through the same data preview as `read-table`, so the dictionary check and the deny
//! list apply to it. The body is built from validated values only: a date, a user name or the
//! five parts of a dump id, never from free text. Release 7.50 refuses SNAP in the data preview;
//! the RFC backends then read it through the standard function module `RFC_READ_TABLE`, and ADT
//! HTTP stops with the hint to use RFC.
//!
//! A dump is a tag stream: `ID` (2 characters), `LEN` (3 digits) and `LEN` characters of value,
//! cut into `FLIST` to `FLIST08` of 200 characters per row and continued in the next row. The
//! decoder is pure and works across both boundaries.

use crate::backend::{BackendKind, Failure};
use crate::cli::ObjectKind;
use crate::operation::dictionary::PreviewError;
use crate::operation::read::{ReadProg, ReadTarget};
use crate::operation::read_table::{
    CLI_MAX_ROWS, Query, ReadTable, ReadTableRequest, TableData, identifier,
};
use crate::operation::text::is_dropped;
use crate::policy::Policies;
use crate::transport::{SadtTransport, TableCall, TransportError};
use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::time::{SystemTime, UNIX_EPOCH};

/// Dumps `dumps` lists when `--rows` names no number.
pub const DEFAULT_ROWS: u32 = 20;
/// The `--rows` of `dumps`.
pub const ROWS: RangeInclusive<u32> = 1..=1000;
/// How far back `dumps` looks when `--date` names no day.
const DEFAULT_DAYS: i64 = 7;
/// One more day, because the date of this machine (UTC) and the date of the server can differ.
const ZONE_MARGIN_DAYS: i64 = 1;
/// The error key `RFC_READ_TABLE` raises for a column the release does not have.
const FIELD_NOT_VALID: &str = "[FIELD_NOT_VALID]";
/// Rows `dumps` reads before it sorts: the preview has no `ORDER BY`, so all matching rows are
/// read and the newest are kept. A read that fills this is cut and is refused.
const LIST_CAP: u32 = CLI_MAX_ROWS;
/// Rows of one dump `dump` reads: far more than the tags it needs.
const DUMP_CAP: u32 = 1000;
/// Rows of the short text of one error.
const TEXT_CAP: u32 = 100;
/// The width of `FLIST` and of each `FLISTnn`.
const FLIST_WIDTH: usize = 200;
/// Source lines shown on each side of the failing line.
const CONTEXT: usize = 5;
/// The users and hosts of SAP use more than letters and digits, but never a quote.
const USER_EXTRA: &str = "_-.$#@&%!";
const USER_MAX: usize = 12;
const HOST_MAX: usize = 32;
const MODNO_MAX: usize = 3;

const TABLE: &str = "SNAP";
const TEXT_TABLE: &str = "SNAPT";
/// The columns that carry the tag stream, in stream order.
pub const FLIST_FIELDS: [&str; 8] = [
    "FLIST", "FLIST02", "FLIST03", "FLIST04", "FLIST05", "FLIST06", "FLIST07", "FLIST08",
];
const LIST_FIELDS: [&str; 8] = [
    "DATUM", "UZEIT", "AHOST", "UNAME", "MANDT", "MODNO", "SEQNO", "FLIST",
];

// ---- the tag stream ---------------------------------------------------------------------------

/// One tag of a dump: a two-character id and its value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    pub id: String,
    pub value: String,
}

/// The tag stream of one dump: the `FLIST` columns of every row joined, each padded to 200
/// characters so that a value continues in the next column exactly where it was cut. The caller
/// passes the rows in `SEQNO` order and the columns in stream order.
pub fn join_stream(rows: &[Vec<String>]) -> String {
    rows.iter()
        .flatten()
        .map(|part| format!("{part:<FLIST_WIDTH$}"))
        .collect()
}

/// The tags of a stream. It stops at padding (a blank id) or at a length that is not three
/// digits; a value cut at the end of the stream is returned as far as it goes.
pub fn decode_tags(stream: &str) -> Vec<Tag> {
    let chars: Vec<char> = stream.chars().collect();
    let mut tags = Vec::new();
    let mut at = 0;

    while at + 5 <= chars.len() {
        let id: String = chars[at..at + 2].iter().collect();
        let length: String = chars[at + 2..at + 5].iter().collect();

        if id.trim().is_empty() || length.len() != 3 || !length.bytes().all(|b| b.is_ascii_digit())
        {
            break;
        }

        let size = length.parse::<usize>().unwrap_or(0);
        let end = (at + 5 + size).min(chars.len());

        tags.push(Tag {
            id,
            value: chars[at + 5..end].iter().collect(),
        });
        at += 5 + size;
    }

    tags
}

/// The first value of a tag, without trailing blanks, and with control characters replaced: a
/// dump is data of the program that failed, never text to print as it is.
fn first(tags: &[Tag], id: &str) -> String {
    tags.iter()
        .find(|tag| tag.id == id)
        .map(|tag| clean(&tag.value))
        .unwrap_or_default()
}

/// A value of a dump as it is printed and returned: a control character is a space, and every
/// other character no printed field keeps (a Unicode format character such as a bidi override, see
/// `text::is_dropped`) is removed, so that a dump value cannot reorder or hide the text around it.
fn clean(value: &str) -> String {
    value
        .chars()
        .filter_map(|c| {
            if c.is_control() {
                Some(' ')
            } else if is_dropped(c) {
                None
            } else {
                Some(c)
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// A value the dump holds: `None` for an empty one.
fn some(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

// ---- dates ------------------------------------------------------------------------------------

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400;

    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The days since the epoch of a real calendar day written `YYYYMMDD`.
fn parse_date(text: &str) -> Option<i64> {
    if text.len() != 8 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let (year, month, day) = (
        text[..4].parse::<i64>().ok()?,
        text[4..6].parse::<i64>().ok()?,
        text[6..].parse::<i64>().ok()?,
    );
    let days = days_from_civil(year, month, day);

    (civil_from_days(days) == (year, month, day) && year >= 1900).then_some(days)
}

fn format_date(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);

    format!("{year:04}{month:02}{day:02}")
}

/// Today as `YYYYMMDD` (UTC).
pub fn today() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());

    format_date(i64::try_from(seconds / 86_400).unwrap_or(0))
}

// ---- inputs -----------------------------------------------------------------------------------

/// A SAP user name in upper case: 1 to 12 letters, digits or one of `_-.$#@&%!`.
pub fn user_name(what: &str, text: &str) -> Result<String, String> {
    let name = text.to_ascii_uppercase();
    let valid = !name.is_empty()
        && name.len() <= USER_MAX
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || USER_EXTRA.contains(c));

    if valid {
        Ok(name)
    } else {
        Err(format!(
            "{what} {text:?} is not a SAP user name (at most {USER_MAX} letters, digits or one of {USER_EXTRA})"
        ))
    }
}

/// What `dumps` was asked for, before any check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListOptions<'a> {
    /// `--date`: the first day, `YYYYMMDD`.
    pub date: Option<&'a str>,
    /// `--user`.
    pub user: Option<&'a str>,
    /// `--all`.
    pub all: bool,
    /// `--rows`.
    pub rows: u32,
}

/// The listing of one run: the validated read of SNAP and the number of dumps to show.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListQuery {
    read: Query,
    rows: u32,
    /// The user the reply must hold: checked again on every row, because the filter of the
    /// query is only a request.
    user: Option<String>,
}

/// One dump of the list. A field the dump does not hold is `None` (printed as `-`); a value may
/// hold a space or `=`, so a program reads these fields and not the printed line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DumpRow {
    pub date: Option<String>,
    pub time: Option<String>,
    pub user: Option<String>,
    pub client: Option<String>,
    pub error: Option<String>,
    pub program: Option<String>,
    pub include: Option<String>,
    pub line: Option<String>,
    /// The id `dump` takes; `None` for a row it would refuse (printed as `unusable`).
    pub id: Option<String>,
}

impl DumpRow {
    /// `DATUM UZEIT UNAME MANDT error= program= include= line= id=`.
    pub fn line(&self) -> String {
        let shown = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_owned());

        format!(
            "{} {} {} {} error={} program={} include={} line={} id={}",
            shown(&self.date),
            shown(&self.time),
            shown(&self.user),
            shown(&self.client),
            shown(&self.error),
            shown(&self.program),
            shown(&self.include),
            shown(&self.line),
            self.id.as_deref().unwrap_or("unusable"),
        )
    }
}

/// What `dumps` found: the dumps, the lines printed from them, and a warning for the stderr when
/// rows had to be dropped. `text` is made from `rows`, so the two cannot disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Listing {
    pub rows: Vec<DumpRow>,
    pub text: String,
    pub warning: Option<String>,
}

/// One dump as `dump` shows it. The short text and the source lines are printed as they are; a
/// line of either can look like anything, so they stay separate from the fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dump {
    pub error: Option<String>,
    pub program: Option<String>,
    pub include: Option<String>,
    pub line: Option<String>,
    pub user: String,
    pub client: Option<String>,
    /// `None` where the release has no text for the error.
    pub short_text: Option<Vec<String>>,
    /// The lines printed after the short text: the header and the lines of the source around the
    /// failing line, or the note why it was not read; empty when nothing is shown.
    pub source: Vec<String>,
}

impl Dump {
    /// The text `dump` prints.
    pub fn text(&self) -> String {
        let shown = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_owned());
        let mut out = vec![
            format!("error: {}", shown(&self.error)),
            format!("program: {}", shown(&self.program)),
            format!("include: {}", shown(&self.include)),
            format!("line: {}", shown(&self.line)),
            format!("user: {}", self.user),
            format!("client: {}", shown(&self.client)),
        ];

        if let Some(text) = &self.short_text {
            out.push("short text:".to_owned());
            out.extend(text.iter().cloned());
        }

        out.extend(self.source.iter().cloned());

        out.join("\n")
    }
}

impl ListQuery {
    /// `logon_user` is the default scope. Any `Err` is a `STOP` and no connection is opened.
    /// `today` is `YYYYMMDD`.
    pub fn new(
        options: &ListOptions<'_>,
        logon_user: &str,
        today: &str,
        policies: &Policies,
    ) -> Result<Self, String> {
        if !ROWS.contains(&options.rows) {
            return Err(format!(
                "rows must be between {} and {}",
                ROWS.start(),
                ROWS.end()
            ));
        }

        if options.all && options.user.is_some() {
            return Err("--all and --user cannot be used together".to_owned());
        }

        let since = match options.date {
            Some(date) => parse_date(date)
                .map(format_date)
                .ok_or_else(|| format!("--date {date:?} is not a day written YYYYMMDD"))?,
            None => {
                let today = parse_date(today)
                    .ok_or_else(|| format!("today {today:?} is not a day written YYYYMMDD"))?;

                format_date(today - DEFAULT_DAYS - ZONE_MARGIN_DAYS)
            }
        };
        let mut condition = format!("SEQNO = '000' AND DATUM >= '{since}'");
        let mut filter = None;

        if !options.all {
            let user = match options.user {
                Some(user) => user_name("--user", user)?,
                None => user_name("the logon user", logon_user)
                    .map_err(|message| format!("{message}; use --user or --all"))?,
            };

            condition.push_str(&format!(" AND UNAME = '{user}'"));
            filter = Some(user);
        }

        let read = Query::new(
            &ReadTableRequest {
                table: TABLE.to_owned(),
                fields: LIST_FIELDS.map(str::to_owned).to_vec(),
                where_clause: Some(condition),
                rows: LIST_CAP,
            },
            LIST_CAP,
            policies,
        )?;

        Ok(Self {
            read,
            rows: options.rows,
            user: filter,
        })
    }
}

/// The five parts of a dump id, as `dumps` prints them: `DATUM/UZEIT/AHOST/UNAME/MODNO`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DumpId {
    date: String,
    time: String,
    host: String,
    user: String,
    modno: String,
}

/// A host or a user of a dump id: what SNAP holds there and a data preview literal takes, so no
/// blank, no control character, no quote and no `/`, which separates the parts of the id.
fn id_part(part: &str, max: usize) -> bool {
    !part.is_empty()
        && part.chars().count() <= max
        && part
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && c != '/' && c != '\'')
}

impl DumpId {
    pub fn parse(text: &str) -> Result<Self, String> {
        let wrong = || format!("dump id {text:?} must be DATUM/UZEIT/AHOST/UNAME/MODNO");
        let parts: Vec<&str> = text.split('/').collect();
        let [date, time, host, user, modno] = parts[..] else {
            return Err(wrong());
        };
        let digits = |part: &str, width: usize| {
            part.len() == width && part.bytes().all(|byte| byte.is_ascii_digit())
        };
        let clock = digits(time, 6)
            && time[..2].parse::<u32>().is_ok_and(|hour| hour < 24)
            && time[2..4].parse::<u32>().is_ok_and(|minute| minute < 60)
            && time[4..].parse::<u32>().is_ok_and(|second| second < 60);
        let host_ok = id_part(host, HOST_MAX);
        let modno_ok = !modno.is_empty()
            && modno.len() <= MODNO_MAX
            && modno.bytes().all(|byte| byte.is_ascii_alphanumeric());

        if parse_date(date).is_none() || !clock || !host_ok || !modno_ok || !id_part(user, USER_MAX)
        {
            return Err(wrong());
        }

        Ok(Self {
            date: date.to_owned(),
            time: time.to_owned(),
            host: host.to_owned(),
            user: user.to_ascii_uppercase(),
            modno: modno.to_owned(),
        })
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    fn render(&self) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            self.date, self.time, self.host, self.user, self.modno
        )
    }

    fn condition(&self) -> String {
        format!(
            "DATUM = '{}' AND UZEIT = '{}' AND AHOST = '{}' AND UNAME = '{}' AND MODNO = '{}'",
            self.date, self.time, self.host, self.user, self.modno
        )
    }
}

/// The dump of one run: a checked id and the validated read of its rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DumpQuery {
    id: DumpId,
    read: Query,
}

impl DumpQuery {
    /// The dump of another user needs `all`. Any `Err` is a `STOP` and no connection is opened.
    pub fn new(
        text: &str,
        all: bool,
        logon_user: &str,
        policies: &Policies,
    ) -> Result<Self, String> {
        let id = DumpId::parse(text)?;

        if !all && !id.user().eq_ignore_ascii_case(logon_user.trim()) {
            return Err(format!(
                "the dump belongs to user {}, not to the logon user; a dump of another user needs --all",
                id.user()
            ));
        }

        let read = Query::new(
            &ReadTableRequest {
                table: TABLE.to_owned(),
                fields: Vec::new(),
                where_clause: Some(id.condition()),
                rows: DUMP_CAP,
            },
            DUMP_CAP,
            policies,
        )?;

        Ok(Self { id, read })
    }
}

// ---- reading SNAP -----------------------------------------------------------------------------

/// The STOP of `dump` on a release that refuses `SNAP` in the data preview, read through a
/// single-byte RFC library.
const SINGLE_BYTE_REFUSAL: &str = "this RFC library uses single-byte characters and cannot read the dump text of SNAP on this release; use the SAP NW RFC SDK (ABAPREACH_RFC_LIBRARY) to read it";

/// The start of the STOP that ADT HTTP gives when SAP refuses `SNAP` in the data preview.
const PREVIEW_REFUSAL_LEAD: &str = "SAP does not show";

/// Whether `error` is the refusal of `SNAP` over ADT HTTP. A run that pinned no backend and has an
/// RFC backend configured does not stop there: the router moves on to RFC.
pub(crate) fn is_preview_refusal(error: &PreviewError) -> bool {
    matches!(error, PreviewError::Refused(message) if message.starts_with(PREVIEW_REFUSAL_LEAD))
}

fn failed(error: &TransportError) -> PreviewError {
    PreviewError::Failed {
        failure: Failure::from(error),
        message: error.to_string(),
    }
}

fn unreadable(message: &str) -> PreviewError {
    PreviewError::Failed {
        failure: Failure::Validation,
        message: format!("cannot read the {TABLE} reply: {message}"),
    }
}

/// Reads the table of `query` through the data preview. When SAP refuses it ("You cannot display
/// SNAP with the standard tools"), the RFC backends read each group of `groups` through
/// `RFC_READ_TABLE` (a row of that function module is 512 characters wide, so a wide read is
/// split by field); ADT HTTP stops with the hint to use RFC. A group after the first that names a
/// column the release does not have is skipped. The refusal is matched on its English text: its
/// reply has no key of its own, so a connection that logs on in another language gets the preview
/// error as it is, and no fallback. With `needs_wide_text` the groups hold the wide text columns of
/// `SNAP`, which a single-byte RFC library cannot send (the server fails with
/// `RFC_CONVERSION_SEND_TABLE` and writes a short dump for the logon user), so the RFC read is
/// refused before any call.
fn read_snap(
    transport: &mut dyn SadtTransport,
    backend: BackendKind,
    query: &Query,
    groups: &[Vec<&str>],
    needs_wide_text: bool,
    policies: &Policies,
) -> Result<Vec<TableData>, PreviewError> {
    let table = query.table();

    match ReadTable::run(transport, query, policies) {
        Ok(data) => return Ok(vec![data]),
        Err(PreviewError::Failed { message, .. })
            if message.contains(&format!(
                "You cannot display {table} with the standard tools"
            )) => {}
        Err(error) => return Err(error),
    }

    if backend != BackendKind::RfcSadt {
        return Err(PreviewError::Refused(format!(
            "{PREVIEW_REFUSAL_LEAD} {table} in the data preview of this release; use --backend rfc to read it through RFC_READ_TABLE"
        )));
    }

    if needs_wide_text && transport.uses_single_byte_characters() {
        return Err(PreviewError::Refused(SINGLE_BYTE_REFUSAL.to_owned()));
    }

    let mut tables = Vec::new();

    for (index, fields) in groups.iter().enumerate() {
        let call = TableCall::from_query(query, fields, policies).map_err(|e| failed(&e))?;

        match transport.read_table_function(&call) {
            Ok(rows) if rows.rows.len() > query.rows() as usize => {
                // ROWCOUNT only asks: a reply past the cap of the query breaks it.
                return Err(unreadable(&format!(
                    "RFC_READ_TABLE returned {} rows for a cap of {}",
                    rows.rows.len(),
                    query.rows()
                )));
            }
            Ok(rows) => tables.push(TableData {
                columns: rows.columns,
                rows: rows.rows,
            }),
            Err(error) if index > 0 && error.to_string().contains(FIELD_NOT_VALID) => {}
            Err(error) => return Err(failed(&error)),
        }
    }

    Ok(tables)
}

fn column(data: &TableData, name: &str) -> Result<usize, PreviewError> {
    data.column(name)
        .ok_or_else(|| unreadable(&format!("no column {name}")))
}

// ---- dumps ------------------------------------------------------------------------------------

/// One line per dump, newest first, or `(no dumps)`.
pub fn run_list(
    transport: &mut dyn SadtTransport,
    backend: BackendKind,
    query: &ListQuery,
    policies: &Policies,
) -> Result<Listing, PreviewError> {
    let tables = read_snap(
        transport,
        backend,
        &query.read,
        &[LIST_FIELDS.to_vec()],
        false,
        policies,
    )?;
    let data = tables
        .first()
        .ok_or_else(|| unreadable("the read returned nothing"))?;

    if data.rows.len() >= LIST_CAP as usize {
        return Err(PreviewError::Refused(format!(
            "{LIST_CAP} or more dumps match; narrow the list with --date or --user"
        )));
    }

    let at = |name: &str| column(data, name);
    let (date, time, host, user, client, modno, flist) = (
        at("DATUM")?,
        at("UZEIT")?,
        at("AHOST")?,
        at("UNAME")?,
        at("MANDT")?,
        at("MODNO")?,
        at("FLIST")?,
    );
    let mut rows: Vec<&Vec<String>> = data.rows.iter().collect();
    let mut warning = None;

    if let Some(wanted) = &query.user {
        let before = rows.len();

        rows.retain(|row| row[user].trim().eq_ignore_ascii_case(wanted));

        if rows.len() < before {
            warning = Some(format!(
                "W: {} rows of other users than {wanted} were dropped: the reply did not keep to the user filter",
                before - rows.len()
            ));
        }
    }

    // The newest first; the host and the number only order dumps of the same second.
    let key = [date, time, host, modno];

    rows.sort_by(|a, b| {
        key.iter()
            .map(|&index| b[index].as_str())
            .cmp(key.iter().map(|&index| a[index].as_str()))
    });

    let rows: Vec<DumpRow> = rows
        .iter()
        .take(query.rows as usize)
        .map(|row| {
            let raw = |index: usize| row[index].trim();
            let shown = |index: usize| some(clean(raw(index)));
            let tags = decode_tags(&format!("{:<FLIST_WIDTH$}", row[flist]));
            // `dump` takes only an id it can parse, so a row it would refuse says so.
            let id = DumpId::parse(&format!(
                "{}/{}/{}/{}/{}",
                raw(date),
                raw(time),
                raw(host),
                raw(user),
                raw(modno)
            ))
            .ok()
            .map(|id| id.render());

            DumpRow {
                date: shown(date),
                time: shown(time),
                user: shown(user),
                client: shown(client),
                error: some(first(&tags, "FC")),
                program: some(first(&tags, "AP")),
                include: some(first(&tags, "AI")),
                line: some(first(&tags, "AL")),
                id,
            }
        })
        .collect();
    let text = if rows.is_empty() {
        "(no dumps)".to_owned()
    } else {
        rows.iter()
            .map(DumpRow::line)
            .collect::<Vec<_>>()
            .join("\n")
    };

    Ok(Listing {
        rows,
        text,
        warning,
    })
}

// ---- dump -------------------------------------------------------------------------------------

/// The rows of one dump by `SEQNO`: the client and the columns of the tag stream.
#[derive(Default)]
struct Rows {
    by_seqno: BTreeMap<String, BTreeMap<&'static str, String>>,
    client: String,
    present: Vec<&'static str>,
}

impl Rows {
    fn merge(&mut self, data: &TableData) -> Result<(), PreviewError> {
        let seqno = column(data, "SEQNO")?;
        let client = data.column("MANDT");
        let parts: Vec<(usize, &'static str)> = FLIST_FIELDS
            .iter()
            .filter_map(|name| data.column(name).map(|index| (index, *name)))
            .collect();

        for row in &data.rows {
            let slot = self
                .by_seqno
                .entry(row[seqno].trim().to_owned())
                .or_default();

            for (index, name) in &parts {
                slot.insert(name, row[*index].clone());
            }

            if let Some(index) = client
                && self.client.is_empty()
            {
                self.client = row[index].trim().to_owned();
            }
        }

        for (_, name) in parts {
            if !self.present.contains(&name) {
                self.present.push(name);
            }
        }

        Ok(())
    }

    /// The stream: every `SEQNO` in order, each column of the dump in stream order.
    fn stream(&self) -> String {
        let mut present = self.present.clone();

        present.sort_by_key(|name| FLIST_FIELDS.iter().position(|known| known == name));

        let rows: Vec<Vec<String>> = self
            .by_seqno
            .values()
            .map(|slot| {
                present
                    .iter()
                    .map(|name| slot.get(name).cloned().unwrap_or_default())
                    .collect()
            })
            .collect();

        join_stream(&rows)
    }
}

/// The error, program, include, line, user and client of a dump, its short text and the source
/// around the failing line. Nothing else of the stream is printed.
pub fn run_dump(
    transport: &mut dyn SadtTransport,
    backend: BackendKind,
    query: &DumpQuery,
    policies: &Policies,
    language: &str,
) -> Result<String, PreviewError> {
    read_dump(transport, backend, query, policies, language).map(|dump| dump.text())
}

/// The dump of the query: its fields, the short text and the source around the failing line.
pub fn read_dump(
    transport: &mut dyn SadtTransport,
    backend: BackendKind,
    query: &DumpQuery,
    policies: &Policies,
    language: &str,
) -> Result<Dump, PreviewError> {
    let mut groups: Vec<Vec<&str>> = vec![vec!["SEQNO", "MANDT", "FLIST"]];

    groups.extend(FLIST_FIELDS[1..].iter().map(|field| vec!["SEQNO", *field]));

    let mut rows = Rows::default();

    for data in read_snap(transport, backend, &query.read, &groups, true, policies)? {
        // A read that fills its cap was cut: what is shown would be a prefix, so it is refused.
        if data.rows.len() >= DUMP_CAP as usize {
            return Err(PreviewError::Refused(format!(
                "the dump has {DUMP_CAP} or more rows: too large to show"
            )));
        }

        rows.merge(&data)?;
    }

    if rows.by_seqno.is_empty() {
        return Err(PreviewError::Rejected(format!(
            "dump not found: {}",
            query.id.render()
        )));
    }

    let tags = decode_tags(&rows.stream());
    let error = first(&tags, "FC");
    let include = first(&tags, "AI");
    let line = first(&tags, "AL");
    let short_text = short_text(transport, backend, policies, &error, language);
    let source = if !include.is_empty()
        && let Ok(at) = line.parse::<usize>()
        && at > 0
    {
        source_excerpt(transport, &include, at)
    } else {
        Vec::new()
    };

    Ok(Dump {
        error: some(error),
        program: some(first(&tags, "AP")),
        include: some(include),
        line: some(line),
        user: query.id.user().to_owned(),
        client: some(clean(&rows.client)),
        short_text,
        source,
    })
}

/// The SAP language key of an ISO language code: the one-character key `SNAPT` holds.
fn sap_language(iso: &str) -> Option<char> {
    const KEYS: [(&str, char); 22] = [
        ("EN", 'E'),
        ("DE", 'D'),
        ("FR", 'F'),
        ("ES", 'S'),
        ("IT", 'I'),
        ("PT", 'P'),
        ("NL", 'N'),
        ("JA", 'J'),
        ("RU", 'R'),
        ("ZH", '1'),
        ("PL", 'L'),
        ("DA", 'K'),
        ("FI", 'U'),
        ("SV", 'V'),
        ("NO", 'O'),
        ("TR", 'T'),
        ("KO", '3'),
        ("CS", 'C'),
        ("HU", 'H'),
        ("EL", 'G'),
        ("HE", 'B'),
        ("AR", 'A'),
    ];

    KEYS.iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(iso.trim()))
        .map(|(_, key)| *key)
}

/// The short text of the error from `SNAPT`: in English, else in the logon language, else in the
/// first language the table has. `None` when the release has none or it cannot be read, because
/// the dump is useful without it.
fn short_text(
    transport: &mut dyn SadtTransport,
    backend: BackendKind,
    policies: &Policies,
    error: &str,
    language: &str,
) -> Option<Vec<String>> {
    let error = identifier("error", error).ok()?;
    let mut languages = vec![Some('E')];

    if let Some(key) = sap_language(language).filter(|key| *key != 'E') {
        languages.push(Some(key));
    }

    languages.push(None);

    for language in languages {
        let (condition, fields) = match language {
            Some(key) => (
                format!("LANGU = '{key}' AND ERRID = '{error}' AND TTYPE = 'K'"),
                vec!["SEQNO", "TLINE"],
            ),
            None => (
                format!("ERRID = '{error}' AND TTYPE = 'K'"),
                vec!["SEQNO", "TLINE", "LANGU"],
            ),
        };
        let query = Query::new(
            &ReadTableRequest {
                table: TEXT_TABLE.to_owned(),
                fields: fields.iter().map(|field| (*field).to_owned()).collect(),
                where_clause: Some(condition),
                rows: TEXT_CAP,
            },
            TEXT_CAP,
            policies,
        )
        .ok()?;
        let tables = read_snap(transport, backend, &query, &[fields], false, policies).ok()?;

        if let Some(lines) = text_lines(tables.first()?, language.is_none()) {
            return Some(lines);
        }
    }

    None
}

/// The lines of one text, in `SEQNO` order; with `any_language` only the rows of the first
/// language of the reply, so two languages never mix. `None` when every line is empty.
fn text_lines(data: &TableData, any_language: bool) -> Option<Vec<String>> {
    let (seqno, text) = (data.column("SEQNO")?, data.column("TLINE")?);
    let mut rows: Vec<&Vec<String>> = data.rows.iter().collect();

    if any_language {
        let language = data.column("LANGU")?;
        let first = rows
            .iter()
            .map(|row| row[language].trim())
            .min()?
            .to_owned();

        rows.retain(|row| row[language].trim() == first);
    }

    rows.sort_by(|a, b| a[seqno].cmp(&b[seqno]));

    let lines: Vec<String> = rows.iter().map(|row| clean(&row[text])).collect();

    lines.iter().any(|line| !line.is_empty()).then_some(lines)
}

/// The lines `at - 5` to `at + 5` of the include, with `>` on `at`; a note when it cannot be
/// read. The include name comes from the dump, so it must be a DDIC-style name before it reaches
/// a URI, and a line past the end of the source shows nothing.
fn source_excerpt(transport: &mut dyn SadtTransport, include: &str, at: usize) -> Vec<String> {
    let Ok(include) = identifier("include", include) else {
        return vec![format!(
            "(source {} not read: not a program or include name)",
            clean(include)
        )];
    };
    let target = ReadTarget::new(ObjectKind::Prog, &include);

    match ReadProg::fetch_lines(transport, target) {
        Ok(lines) if at <= lines.len() => {
            let start = at.saturating_sub(CONTEXT).max(1);
            let end = at.saturating_add(CONTEXT).min(lines.len());
            let mut out = vec![format!("source {include}:")];

            out.extend(
                lines
                    .iter()
                    .enumerate()
                    .take(end)
                    .skip(start - 1)
                    .map(|(index, line)| {
                        let number = index + 1;
                        let mark = if number == at { '>' } else { ' ' };

                        format!("{mark} {number:5} {}", clean(line))
                    }),
            );

            out
        }
        Ok(_) => Vec::new(),
        Err(error) => vec![format!(
            "(source {include} unreadable: {})",
            clean(&error.message)
        )],
    }
}
