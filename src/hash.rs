use sha2::{Digest, Sha256};

pub const MAX_LINE: usize = 255;
pub const MAX_NAME: usize = 40;

/// Splits a locally read deploy file into lines. Editors on Windows prepend one UTF-8 BOM; it is
/// not source, so it is dropped from the first position only. CR and trailing blanks stay for
/// `normalize`, which the hash and the diff share.
pub fn local_source_lines(text: &str) -> Vec<String> {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// The empty source of a fresh object is an empty body on 7.50 and a single NUL character on
/// 7.40; both normalize to no lines, so read and hash agree on one canonical empty source.
pub fn normalize(lines: Vec<String>) -> Vec<String> {
    let mut normalized = lines
        .into_iter()
        .map(|line| line.trim_end_matches('\r').trim_end_matches(' ').to_owned())
        .collect::<Vec<_>>();

    while normalized.last().is_some_and(String::is_empty) {
        normalized.pop();
    }

    if normalized == ["\0"] {
        normalized.clear();
    }

    normalized
}

pub fn source_hash(lines: &[String]) -> String {
    let source = normalize(lines.to_vec()).join("\n");
    let digest = Sha256::digest(source.as_bytes());

    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The program grammar: `NAME`, optionally prefixed by a namespace `/NAMESPACE/`. Names are
/// compared uppercased; only A-Z, 0-9 and `_` are allowed.
pub fn validate_name(name: &str) -> Result<(), String> {
    validate_name_within(name, MAX_NAME)
}

/// `validate_name` with the length limit of the object type (SAP limit: PROG 40, FUNC/CLAS/INTF
/// 30, FUGR 26).
pub fn validate_name_within(name: &str, max: usize) -> Result<(), String> {
    let normalized = name.to_ascii_uppercase();

    if normalized.is_empty() {
        return Err("object name must not be empty".to_owned());
    }

    if normalized.chars().count() > max {
        return Err(format!("object name > {max} characters: {name}"));
    }

    if !is_name(&normalized) {
        return Err(format!(
            "object name must be {max} characters or less of A-Z 0-9 _ with an optional \
             /NAMESPACE/ prefix: {name}"
        ));
    }

    Ok(())
}

fn is_name(normalized: &str) -> bool {
    match normalized.strip_prefix('/') {
        None => is_name_part(normalized),
        Some(rest) => rest
            .split_once('/')
            .is_some_and(|(namespace, name)| is_name_part(namespace) && is_name_part(name)),
    }
}

fn is_name_part(part: &str) -> bool {
    !part.is_empty()
        && part.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

pub fn validate_prog(name: &str, lines: &[String]) -> Result<(), String> {
    validate_source(name, MAX_NAME, lines)
}

/// A deploy file for an object whose name may be `max` characters long.
pub fn validate_source(name: &str, max: usize, lines: &[String]) -> Result<(), String> {
    validate_name_within(name, max)?;

    let normalized = normalize(lines.to_vec());

    if normalized.is_empty() {
        return Err("source file is empty".to_owned());
    }

    for (index, line) in normalized.iter().enumerate() {
        let length = line.chars().count();

        if length > MAX_LINE {
            return Err(format!(
                "line {} length {length} > {MAX_LINE} characters (no automatic wrap)",
                index + 1
            ));
        }
    }

    Ok(())
}
