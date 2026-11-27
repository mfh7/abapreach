//! Deploy by edits: the arguments of an `edits` call and the rules that turn the live source plus
//! those edits into the full source a deploy writes. The checks, the diff and the write run on the
//! result exactly as they do on a full source.

use crate::hash::cut_line;
use crate::operation::delete::sha256_hex;
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;

/// One replacement: `old` must match exactly once in the live text. The field order is the order
/// of the canonical JSON the ticket hashes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Edit {
    pub old: String,
    pub new: String,
}

/// The edits of a call with the live hash the client based them on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditPlan {
    pub base: String,
    pub edits: Vec<Edit>,
}

/// The `edits` argument: a non-empty array of objects with the string keys `old` and `new` and
/// no other key.
pub fn parse_edits(value: &Value) -> Result<Vec<Edit>, String> {
    let items = value
        .as_array()
        .filter(|items| !items.is_empty())
        .ok_or("edits must be a non-empty array of {old, new} objects")?;

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let number = index + 1;
            let object = item
                .as_object()
                .ok_or_else(|| format!("edit {number}: must be an object with old and new"))?;

            if let Some(key) = object
                .keys()
                .find(|key| !matches!(key.as_str(), "old" | "new"))
            {
                return Err(format!("edit {number}: unknown key {key}"));
            }

            let text = |key: &str| {
                object
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("edit {number}: {key} must be a string"))
            };

            Ok(Edit {
                old: text("old")?,
                new: text("new")?,
            })
        })
        .collect()
}

/// The hash of the canonical JSON of `edits`: an array of `{"old":…,"new":…}`, compact, UTF-8, in
/// the order the client sent them and with the text as sent.
pub fn edits_sha256(edits: &[Edit]) -> String {
    sha256_hex(&serde_json::to_string(edits).expect("Vec<Edit> of Strings always serializes"))
}

/// `text` as the match sees it: CR and trailing blanks are cut on every segment that ends with a
/// line feed. The final segment, after the last line feed or the whole text when there is none, is
/// used as sent, so an `old` that ends in the middle of a line keeps its last characters.
fn normalize_segments(text: &str) -> Cow<'_, str> {
    let Some((head, last)) = text.rsplit_once('\n') else {
        return Cow::Borrowed(text);
    };

    if head.split('\n').all(|segment| cut_line(segment) == segment) {
        return Cow::Borrowed(text);
    }

    let mut cut = head
        .split('\n')
        .map(cut_line)
        .collect::<Vec<_>>()
        .join("\n");

    cut.push('\n');
    cut.push_str(last);

    Cow::Owned(cut)
}

/// How often `needle` occurs in `text`, overlapping occurrences included, and where the first
/// one starts. `needle` is not empty.
fn occurrences(text: &str, needle: &str) -> (usize, Option<usize>) {
    let mut count = 0;
    let mut first = None;
    let mut from = 0;

    while let Some(found) = text[from..].find(needle) {
        let at = from + found;

        first.get_or_insert(at);
        count += 1;
        from = at + text[at..].chars().next().map_or(1, char::len_utf8);
    }

    (count, first)
}

/// Applies `edits` in order to the live lines (the lines `abap_read_source` returns, so joined
/// with line feeds) and returns the lines of the result. Each edit sees the result of the earlier
/// ones. Edits are numbered from 1 in the messages.
pub fn apply_edits(live: &[String], edits: &[Edit]) -> Result<Vec<String>, String> {
    let mut text = live.join("\n");

    for (index, edit) in edits.iter().enumerate() {
        let number = index + 1;
        let old = normalize_segments(&edit.old);

        if old.is_empty() {
            return Err(format!("edit {number}: old is empty"));
        }

        match occurrences(&text, &old) {
            (_, None) => return Err(format!("edit {number}: old not found")),
            (1, Some(at)) => {
                let new = normalize_segments(&edit.new);

                text.replace_range(at..at + old.len(), &new);
            }
            (count, Some(_)) => {
                return Err(format!(
                    "edit {number}: old matches {count} times; add context"
                ));
            }
        }
    }

    Ok(text.split('\n').map(str::to_owned).collect())
}
