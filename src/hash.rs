use sha2::{Digest, Sha256};

pub const MAX_LINE: usize = 255;

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

pub fn validate_prog(name: &str, lines: &[String]) -> Result<(), String> {
    if name.is_empty() {
        return Err("object name must not be empty".to_owned());
    }

    if name.chars().count() > 40 {
        return Err(format!("object name > 40 characters: {name}"));
    }

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
