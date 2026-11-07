//! The boundary of the public product. The custom function module and the private extension crate
//! are not part of it, so nothing public may name them; the generic function-call layer is, but only
//! the transport, the extension module and the two factory files may name it.
//!
//! The forbidden strings are spelled out of pieces, so this file needs no exception of its own.

use std::fs;
use std::path::{Path, PathBuf};

/// The custom function module, the experimental feature name and the private crate.
const FORBIDDEN: [&str; 3] = [
    concat!("YFM_UTILS_", "SOURCE_DEPLOY"),
    concat!("experimental", "-ext"),
    concat!("abapreach", "-ext"),
];

/// The closed list of files that may hold a forbidden string, each with its reason. A file that no
/// longer holds one is a stale exception and fails the test, so the list cannot grow quiet.
/// `src/operation/delete.rs` is not here: its self-guard holds only the prefix `YFM_UTILS_`.
const EXCEPTIONS: [(&str, &str); 4] = [
    (
        "tests/delete_operation_test.rs",
        "the delete self-guard test names the tool's own function module",
    ),
    (
        "tests/delete_adt_test.rs",
        "the delete self-guard test reads the function group of the tool through ADT",
    ),
    (
        "tests/fixtures/m4/nodestructure-hd1-yfg-utils.xml",
        "the recorded function group of the tool that the self-guard test reads",
    ),
    (
        "tests/rfc_sadt_test.rs",
        "the admission test that rejects a request to a function module REST path",
    ),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn files_below(path: &Path, found: &mut Vec<PathBuf>) {
    if path.is_file() {
        found.push(path.to_path_buf());
    } else if path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();

        entries.sort();
        entries.iter().for_each(|entry| files_below(entry, found));
    }
}

/// The text files below the given paths of the repository, with their repository-relative names.
fn text_files(paths: &[&str]) -> Vec<(String, String)> {
    let mut found = Vec::new();

    for path in paths {
        files_below(&root().join(path), &mut found);
    }

    found
        .into_iter()
        .filter_map(|path| {
            let text = fs::read_to_string(&path).ok()?;
            let name = path
                .strip_prefix(root())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");

            Some((name, text))
        })
        .collect()
}

fn holds_forbidden(text: &str) -> bool {
    FORBIDDEN.iter().any(|forbidden| text.contains(forbidden))
}

fn offenders(paths: &[&str]) -> Vec<String> {
    text_files(paths)
        .into_iter()
        .filter(|(name, text)| {
            holds_forbidden(text) && !EXCEPTIONS.iter().any(|(allowed, _)| allowed == name)
        })
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn the_public_code_names_no_custom_function_module_and_no_private_crate() {
    let found = offenders(&["src", "tests", "Cargo.toml", ".github"]);

    assert!(found.is_empty(), "forbidden strings in: {found:?}");
}

#[test]
fn every_exception_still_holds_a_forbidden_string() {
    let files = text_files(&["tests"]);

    for (name, reason) in EXCEPTIONS {
        let (_, text) = files
            .iter()
            .find(|(file, _)| file == name)
            .unwrap_or_else(|| panic!("{name} ({reason}) is gone: remove the exception"));

        assert!(
            holds_forbidden(text),
            "{name} ({reason}) holds none: remove the exception"
        );
    }
}

/// Green once the public docs are reworded (task S5); until then the old text still names the
/// function module.
#[test]
#[ignore = "green in task S5 (docs, README and CHANGELOG)"]
fn the_public_docs_name_no_custom_function_module_and_no_private_crate() {
    let found = offenders(&["docs", "README.md", "CHANGELOG.md", "CONTRIBUTING.md"]);

    assert!(found.is_empty(), "forbidden strings in: {found:?}");
}

// ---- the extension token and the identifiers ------------------------------------------------------

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `\bEXT\b`: the token `EXT` between non-word characters.
fn has_ext_token(line: &str) -> bool {
    let bytes = line.as_bytes();

    line.match_indices("EXT").any(|(start, token)| {
        let before = start.checked_sub(1).map(|at| bytes[at]);
        let after = bytes.get(start + token.len()).copied();

        !before.is_some_and(is_word_byte) && !after.is_some_and(is_word_byte)
    })
}

/// `Ext[A-Z]`: an identifier that starts a word with `Ext` and a capital.
fn has_ext_identifier(line: &str) -> bool {
    let bytes = line.as_bytes();

    line.match_indices("Ext").any(|(start, token)| {
        bytes
            .get(start + token.len())
            .is_some_and(u8::is_ascii_uppercase)
    })
}

#[test]
fn the_public_source_has_no_ext_token_and_no_ext_identifier() {
    for (name, text) in text_files(&["src"]) {
        for (index, line) in text.lines().enumerate() {
            assert!(!has_ext_token(line), "{name}:{}: {line}", index + 1);
            assert!(!has_ext_identifier(line), "{name}:{}: {line}", index + 1);
        }
    }
}

#[test]
fn the_token_and_identifier_checks_find_what_they_should() {
    assert!(has_ext_token("the EXT layer"));
    assert!(has_ext_token("(EXT)"));
    assert!(has_ext_token("EXT"));
    assert!(!has_ext_token("NEXT SEXT EXTRA EXT_X"));
    assert!(has_ext_identifier("struct ExtGate;"));
    assert!(!has_ext_identifier("fn extend(Extension, Extra)"));
}

// ---- the function-call layer -------------------------------------------------------------------

/// Only the RFC transport, the extension module and the two factory files may name the generic
/// function-call layer; in the factory files it is the factory methods and their forwarders, so
/// they never call it.
#[test]
fn only_the_transport_the_extension_module_and_the_factories_name_the_function_layer() {
    let named = ["FunctionApi", "call_function", "describe_function"];
    let allowed = ["src/transport/rfc.rs", "src/app.rs", "src/mcp_write.rs"];

    for (name, text) in text_files(&["src"]) {
        if !named.iter().any(|word| text.contains(word)) {
            continue;
        }

        assert!(
            allowed.contains(&name.as_str()) || name.starts_with("src/extension/"),
            "{name} names the function-call layer"
        );

        if name == "src/app.rs" || name == "src/mcp_write.rs" {
            for call in ["call_function(", "describe_function("] {
                assert!(
                    !text.contains(call),
                    "{name} calls the function layer: {call}"
                );
            }
        }
    }
}
