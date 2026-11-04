//! The interface of a function module, read from its ADT source: the statement from `FUNCTION`
//! to the first `.`. Only the interface is compared, so a change of the body is never reported.

use similar::TextDiff;
use std::fmt;

const SECTIONS: [&str; 6] = [
    "IMPORTING",
    "EXPORTING",
    "CHANGING",
    "TABLES",
    "EXCEPTIONS",
    "RAISING",
];

/// The unified diff (`live` against `file`) of the two signatures, or `None` when the signatures
/// are equal after normalizing whitespace, line layout, comments and keyword case, or when
/// either source has no readable signature (SAP always renders one, so a file without one was
/// already refused before this point).
///
/// shortcut: parameters are compared as tokens and shown per line, not parsed into typed
/// parameters; upgrade when a reader needs "parameter X changed" instead of a line diff.
pub fn interface_diff(
    live: &[String],
    file: &[String],
) -> Result<Option<Vec<String>>, SignatureError> {
    let live = signature(live)?;
    let file = signature(file)?;

    if tokens(&live) == tokens(&file) {
        return Ok(None);
    }

    let diff = TextDiff::from_lines(render(&live), render(&file));
    let text = diff.unified_diff().header("live", "file").to_string();

    Ok(Some(text.lines().map(str::to_owned).collect()))
}

/// Rejecting a malformed local FUNC source before checkrun prevents it from being treated as a
/// body-only edit. A malformed live source is equally not comparable.
pub fn validate_signature(source: &[String]) -> Result<(), SignatureError> {
    signature(source).map(|_| ())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureError {
    Unreadable,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable => {
                formatter.write_str("FUNCTION statement is missing or unterminated")
            }
        }
    }
}

/// The signature as display lines: one section keyword or one declaration per line.
fn signature(source: &[String]) -> Result<Vec<Vec<String>>, SignatureError> {
    let mut statement: Vec<Vec<String>> = Vec::new();
    let mut ended = false;

    for line in source {
        if line.starts_with('*') {
            continue;
        }

        let (found, terminated) = scan(line);

        if !found.is_empty() {
            statement.push(found);
        }

        if terminated {
            ended = true;
            break;
        }
    }

    let Some(first) = statement.first().and_then(|line| line.first()) else {
        return Err(SignatureError::Unreadable);
    };

    if !ended || first != "FUNCTION" {
        return Err(SignatureError::Unreadable);
    }

    let mut lines = Vec::new();

    for line in statement {
        let mut current = Vec::new();

        for token in line {
            if SECTIONS.contains(&token.as_str()) {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }

                lines.push(vec![token]);
            } else {
                current.push(token);
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }
    }

    Ok(lines)
}

fn flush(word: &mut String, tokens: &mut Vec<String>) {
    if !word.is_empty() {
        tokens.push(word.to_uppercase());
        word.clear();
    }
}

/// The tokens of one source line and whether the statement-ending `.` was found. Keywords and
/// names are upper-cased like the server renders them; string literals keep their case.
fn scan(line: &str) -> (Vec<String>, bool) {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => flush(&mut word, &mut tokens),
            '"' => break,
            '\'' | '`' => {
                flush(&mut word, &mut tokens);

                let mut literal = String::from(c);

                while let Some(next) = chars.next() {
                    literal.push(next);

                    if next == c {
                        // A doubled quote is one quote inside the literal.
                        if chars.peek() == Some(&c) {
                            literal.extend(chars.next());
                        } else {
                            break;
                        }
                    }
                }

                tokens.push(literal);
            }
            '(' | ')' => {
                flush(&mut word, &mut tokens);
                tokens.push(c.to_string());
            }
            '.' if chars.peek().is_none_or(|next| next.is_whitespace()) => {
                flush(&mut word, &mut tokens);

                return (tokens, true);
            }
            c => word.push(c),
        }
    }

    flush(&mut word, &mut tokens);

    (tokens, false)
}

fn tokens(lines: &[Vec<String>]) -> Vec<&String> {
    lines.iter().flatten().collect()
}

fn render(lines: &[Vec<String>]) -> String {
    lines
        .iter()
        .map(|line| {
            let mut text = String::new();

            for token in line {
                if !text.is_empty() && token != ")" && token != "(" && !text.ends_with('(') {
                    text.push(' ');
                }

                text.push_str(token);
            }

            text + "\n"
        })
        .collect()
}
