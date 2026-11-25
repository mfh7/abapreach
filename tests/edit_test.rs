//! The edit rules on their own: matching, order, normalization and the canonical hash. No SAP and
//! no server; the tool wiring is in `mcp_edits_test.rs`.

use abapreach::edit::{Edit, apply_edits, edits_sha256, parse_edits};
use serde_json::json;

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

fn edit(old: &str, new: &str) -> Edit {
    Edit {
        old: old.to_owned(),
        new: new.to_owned(),
    }
}

fn apply(live: &str, edits: &[Edit]) -> Result<String, String> {
    apply_edits(&lines(live), edits).map(|result| result.join("\n"))
}

#[test]
fn an_edit_replaces_its_one_match() {
    assert_eq!(
        apply("a\nWRITE 1.\nb", &[edit("WRITE 1.", "WRITE 2.")]).unwrap(),
        "a\nWRITE 2.\nb"
    );
}

#[test]
fn an_old_that_is_not_in_the_live_source_is_refused_with_its_number() {
    let edits = [edit("a", "A"), edit("zzz", "y")];

    assert_eq!(
        apply("a\nb", &edits),
        Err("edit 2: old not found".to_owned())
    );
}

#[test]
fn an_old_that_matches_more_than_once_is_refused_with_the_count() {
    assert_eq!(
        apply("x\nx\nx", &[edit("x", "y")]),
        Err("edit 1: old matches 3 times; add context".to_owned())
    );
}

#[test]
fn overlapping_occurrences_count_as_separate_matches() {
    // "aa" starts at offset 0 and at offset 1 of "aaa".
    assert_eq!(
        apply("aaa", &[edit("aa", "b")]),
        Err("edit 1: old matches 2 times; add context".to_owned())
    );
}

#[test]
fn overlap_stepping_keeps_multibyte_characters_whole() {
    assert_eq!(
        apply("ééé", &[edit("éé", "b")]),
        Err("edit 1: old matches 2 times; add context".to_owned())
    );
    assert_eq!(apply("xéy", &[edit("é", "e")]).unwrap(), "xey");
}

#[test]
fn an_empty_old_is_refused() {
    assert_eq!(
        apply("a", &[edit("", "b")]),
        Err("edit 1: old is empty".to_owned())
    );
}

#[test]
fn an_empty_new_deletes_the_match() {
    assert_eq!(
        apply("a\nDELETE ME\nb", &[edit("DELETE ME\n", "")]).unwrap(),
        "a\nb"
    );
}

#[test]
fn edits_apply_in_order_and_each_sees_the_result_of_the_one_before() {
    let forward = [edit("one", "two"), edit("two", "three")];

    // The second edit finds the text the first one wrote.
    assert_eq!(apply("one", &forward).unwrap(), "three");

    // The other way round, "two" is not there yet.
    let reversed = [edit("two", "three"), edit("one", "two")];

    assert_eq!(
        apply("one", &reversed),
        Err("edit 1: old not found".to_owned())
    );
}

#[test]
fn an_edit_can_make_a_later_old_ambiguous() {
    let edits = [edit("a", "b"), edit("b", "c")];

    assert_eq!(
        apply("a\nb", &edits),
        Err("edit 2: old matches 2 times; add context".to_owned())
    );
}

#[test]
fn cr_and_trailing_blanks_are_cut_on_segments_that_end_with_a_line_feed() {
    // The live text is normalized; `old` and `new` are cut the same way before matching.
    assert_eq!(
        apply(
            "WRITE 1.\nWRITE 2.\nend.",
            &[edit("WRITE 1.  \r\nWRITE 2.\r\n", "WRITE 9.  \r\n")]
        )
        .unwrap(),
        "WRITE 9.\nend."
    );
}

#[test]
fn the_final_segment_of_old_and_new_is_used_as_sent() {
    // `old` ends mid-line with a blank: it matches exactly that blank, not a trimmed text.
    assert_eq!(apply("a b", &[edit("a ", "x")]).unwrap(), "xb");
    assert_eq!(
        apply("a b", &[edit("a", "x ")]).unwrap(),
        "x  b",
        "the blank of `new` is kept"
    );

    // A final segment with a trailing CR is not cut either.
    assert_eq!(
        apply("a\nb", &[edit("a\r", "x")]),
        Err("edit 1: old not found".to_owned())
    );
}

#[test]
fn a_result_is_returned_as_lines() {
    assert_eq!(
        apply_edits(&lines("a\nb"), &[edit("a", "a1\na2")]).unwrap(),
        vec!["a1", "a2", "b"]
    );
}

#[test]
fn the_edits_hash_is_the_sha256_of_the_compact_canonical_json() {
    let edits = [edit("a\"b", "c\n")];
    let canonical = r#"[{"old":"a\"b","new":"c\n"}]"#;

    assert_eq!(edits_sha256(&edits), sha256(canonical));
}

fn sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};

    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn the_edits_hash_binds_text_and_order() {
    let base = edits_sha256(&[edit("a", "b"), edit("c", "d")]);

    assert_ne!(base, edits_sha256(&[edit("c", "d"), edit("a", "b")]));
    assert_ne!(base, edits_sha256(&[edit("a", "b"), edit("c", "e")]));
    assert_ne!(base, edits_sha256(&[edit("a", "b")]));
    // Text as sent: a blank that normalization would cut still changes the hash.
    assert_ne!(
        edits_sha256(&[edit("a", "b")]),
        edits_sha256(&[edit("a", "b ")])
    );
}

#[test]
fn edits_are_parsed_from_an_array_of_old_new_objects() {
    let parsed = parse_edits(&json!([{"old": "a", "new": "b"}, {"new": "d", "old": "c"}])).unwrap();

    assert_eq!(parsed, vec![edit("a", "b"), edit("c", "d")]);
}

#[test]
fn a_malformed_edits_argument_is_refused() {
    for (value, message) in [
        (json!("x"), "edits must be a non-empty array"),
        (json!([]), "edits must be a non-empty array"),
        (json!(["x"]), "edit 1: must be an object"),
        (json!([{"old": "a"}]), "edit 1: new must be a string"),
        (json!([{"new": "a"}]), "edit 1: old must be a string"),
        (
            json!([{"old": "a", "new": 1}]),
            "edit 1: new must be a string",
        ),
        (
            json!([{"old": "a", "new": "b"}, {"old": "a", "new": "b", "flags": "g"}]),
            "edit 2: unknown key flags",
        ),
    ] {
        let error = parse_edits(&value).unwrap_err();

        assert!(error.contains(message), "{value}: {error}");
    }
}
