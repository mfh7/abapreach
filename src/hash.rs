use sha2::{Digest, Sha256};

pub const MAX_LINE: usize = 255;
pub const MAX_NAME: usize = 40;

pub fn normalize(lines: Vec<String>) -> Vec<String> {
    let mut normalized = lines
        .into_iter()
        .map(|line| line.trim_end_matches('\r').trim_end_matches(' ').to_owned())
        .collect::<Vec<_>>();

    while normalized.last().is_some_and(String::is_empty) {
        normalized.pop();
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
    let normalized = name.to_ascii_uppercase();

    if normalized.is_empty() {
        return Err("object name must not be empty".to_owned());
    }

    if normalized.chars().count() > MAX_NAME {
        return Err(format!("object name > {MAX_NAME} characters: {name}"));
    }

    if !is_name(&normalized) {
        return Err(format!(
            "object name must be {MAX_NAME} characters or less of A-Z 0-9 _ with an optional \
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
    validate_name(name)?;

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
