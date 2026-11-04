use abapreach::signature::{SignatureError, interface_diff, validate_signature};

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

const LIVE: &str = "FUNCTION YTEST_FM\n  IMPORTING\n    VALUE(IV_IN) TYPE I\n  EXPORTING\n    VALUE(EV_OUT) TYPE I\n  EXCEPTIONS\n    NOT_FOUND.\n\n\n\n  ev_out = iv_in.\nENDFUNCTION.";

fn diff(file: &str) -> Option<Vec<String>> {
    interface_diff(&lines(LIVE), &lines(file)).expect("readable test signatures")
}

#[test]
fn equivalent_signatures_are_not_a_change() {
    // Keyword case, spacing, line layout, comments and the body do not matter.
    let file = "function ytest_fm importing value( iv_in ) type i \" the input\n  exporting\n*  a comment line\n    value(ev_out)   type i\n  exceptions not_found.\n  ev_out = iv_in + 1.\nENDFUNCTION.";

    assert_eq!(diff(file), None);
    assert_eq!(diff(LIVE), None);
}

#[test]
fn a_body_only_change_is_not_a_change() {
    let file = LIVE.replace("ev_out = iv_in.", "ev_out = iv_in * 2.");

    assert_eq!(diff(&file), None);
}

#[test]
fn a_changed_parameter_type_is_shown_as_a_unified_diff() {
    let file = LIVE.replace("VALUE(IV_IN) TYPE I", "VALUE(IV_IN) TYPE STRING");
    let diff = diff(&file).expect("changed");

    assert_eq!(&diff[..2], ["--- live", "+++ file"]);
    assert!(
        diff.contains(&"-VALUE(IV_IN) TYPE I".to_owned()),
        "{diff:?}"
    );
    assert!(
        diff.contains(&"+VALUE(IV_IN) TYPE STRING".to_owned()),
        "{diff:?}"
    );
}

#[test]
fn a_changed_direction_name_or_exception_is_a_change() {
    for file in [
        LIVE.replace("EXPORTING", "CHANGING"),
        LIVE.replace("IV_IN", "IV_INPUT"),
        LIVE.replace("    NOT_FOUND.", "    NOT_FOUND\n    OTHER."),
        LIVE.replace("  EXCEPTIONS\n    NOT_FOUND.", "."),
        LIVE.replace("VALUE(IV_IN) TYPE I", "VALUE(IV_IN) TYPE I OPTIONAL"),
        LIVE.replace("VALUE(IV_IN)", "IV_IN"),
    ] {
        assert!(diff(&file).is_some(), "{file}");
    }
}

#[test]
fn a_removed_parameter_shows_only_the_removed_line() {
    let file = LIVE.replace("  EXPORTING\n    VALUE(EV_OUT) TYPE I\n", "");
    let diff = diff(&file).expect("changed");

    assert!(
        diff.contains(&"-VALUE(EV_OUT) TYPE I".to_owned()),
        "{diff:?}"
    );
    assert!(diff.iter().all(|line| !line.starts_with("+V")), "{diff:?}");
}

#[test]
fn string_literals_keep_their_case_and_their_dots() {
    let live = "FUNCTION YTEST_FM\n  IMPORTING\n    VALUE(IV_IN) TYPE C DEFAULT 'a. b'.\n  WRITE 'x'.\nENDFUNCTION.";

    assert_eq!(
        interface_diff(&lines(live), &lines(&live.replace("DEFAULT", "default"))).unwrap(),
        None
    );
    assert!(
        interface_diff(&lines(live), &lines(&live.replace("'a. b'", "'A. b'")))
            .unwrap()
            .is_some()
    );
    // The first dot inside the literal does not end the signature.
    assert!(
        interface_diff(&lines(live), &lines(&live.replace("'a. b'", "'a. c'")))
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_source_without_a_readable_signature_is_explicitly_rejected() {
    let unreadable = [
        "",
        "WRITE 'x'.",
        "FUNCTION YTEST_FM",
        "FUNCTION YTEST_FM\n  IMPORTING",
    ];

    for text in unreadable {
        assert!(
            interface_diff(&lines(LIVE), &lines(text)).is_err(),
            "{text:?}"
        );
        assert!(
            interface_diff(&lines(text), &lines(LIVE)).is_err(),
            "{text:?}"
        );
    }
}

#[test]
fn the_server_rendering_with_crlf_and_upper_case_equals_an_adt_file() {
    let rendered = "FUNCTION YTEST_FM\r\n  IMPORTING\r\n    VALUE(IV_IN) TYPE I\r\n  EXPORTING\r\n    VALUE(EV_OUT) TYPE I\r\n  EXCEPTIONS\r\n    NOT_FOUND.\r\n\r\n\r\n\r\n  ev_out = iv_in.\r\nENDFUNCTION.";
    let file = "FUNCTION ytest_fm\n  IMPORTING\n    VALUE(iv_in) TYPE i\n  EXPORTING\n    VALUE(ev_out) TYPE i\n  EXCEPTIONS\n    not_found.\n\n  ev_out = iv_in.\nENDFUNCTION.";
    let rendered: Vec<String> = rendered.split('\n').map(str::to_owned).collect();

    assert_eq!(interface_diff(&rendered, &lines(file)).unwrap(), None);
}

#[test]
fn only_a_readable_signature_validates_and_the_error_names_the_problem() {
    assert_eq!(validate_signature(&lines(LIVE)), Ok(()));

    for text in ["", "WRITE 'x'.", "FUNCTION YTEST_FM\n  IMPORTING"] {
        assert_eq!(
            validate_signature(&lines(text)),
            Err(SignatureError::Unreadable),
            "{text:?}"
        );
    }

    assert_eq!(
        SignatureError::Unreadable.to_string(),
        "FUNCTION statement is missing or unterminated"
    );
}

#[test]
fn a_dot_inside_a_word_does_not_end_the_signature() {
    let live = "FUNCTION YTEST_FM\n  IMPORTING\n    VALUE(IV_IN) TYPE P DEFAULT 1.5.\n  WRITE 'x'.\nENDFUNCTION.";

    assert!(
        interface_diff(&lines(live), &lines(&live.replace("1.5", "1.6")))
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_literal_may_be_followed_by_the_end_of_the_statement() {
    let live = "FUNCTION YTEST_FM\n  IMPORTING\n    VALUE(IV_IN) TYPE C DEFAULT 'a'.\n  WRITE 'x'.\nENDFUNCTION.";

    // The dot after the closing quote ends the statement, so the body is not compared.
    assert_eq!(
        interface_diff(
            &lines(live),
            &lines(&live.replace("WRITE 'x'", "WRITE 'y'"))
        )
        .unwrap(),
        None
    );

    // A doubled quote stays inside the literal.
    let doubled = live.replace("'a'", "'it''s'");

    assert_eq!(
        interface_diff(&lines(&doubled), &lines(&doubled)).unwrap(),
        None
    );
    assert!(
        interface_diff(&lines(&doubled), &lines(&doubled.replace("it''s", "it''t")))
            .unwrap()
            .is_some()
    );
}
