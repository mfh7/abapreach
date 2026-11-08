use abapreach::runner_protocol::{
    Level, MAX_OUTPUT_BYTES, MAX_WARNINGS, Nonce, Parsed, Reason, Report, RunMode, RunStatus, Stop,
    TEXT_LIMIT, parse_output, parse_post_reply, runtime_failure, scrub_text,
};

const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const OTHER: &str = "FEDCBA9876543210FEDCBA9876543210";

fn nonce() -> Nonce {
    Nonce::parse(NONCE).unwrap()
}

fn hex(text: &str) -> String {
    hex_bytes(text.as_bytes())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn bound(nonce: &str, rest: &str) -> String {
    format!("ABAPREACH:{nonce}:{rest}")
}

fn line(rest: &str) -> String {
    bound(NONCE, rest)
}

/// BEGIN, the body lines, STATUS, END with the matching count.
fn run(mode: &str, body: &[String], status: &str) -> Vec<String> {
    let mut lines = vec![line(&format!("BEGIN:{mode}"))];
    lines.extend(body.iter().map(|rest| line(rest)));
    lines.push(line(&format!("STATUS:{status}")));
    let count = lines.len();
    lines.push(line(&format!("END:{count}")));

    lines
}

/// The classrun framing: every `out->write` is the text plus two newlines.
fn framed(lines: &[String]) -> String {
    lines.iter().map(|text| format!("{text}\n\n")).collect()
}

fn parse(mode: RunMode, lines: &[String]) -> Parsed {
    parse_output(&nonce(), mode, &framed(lines))
}

fn complete(parsed: Parsed) -> Report {
    match parsed {
        Parsed::Complete(report) => report,
        other => panic!("expected a complete run, got {other:?}"),
    }
}

fn reason(parsed: Parsed) -> Reason {
    match parsed {
        Parsed::Uncertain { reason, .. } => reason,
        other => panic!("expected an uncertain status, got {other:?}"),
    }
}

#[test]
fn nonce_is_exactly_128_bits_of_upper_case_hex() {
    assert!(Nonce::parse(NONCE).is_ok());
    for bad in [
        "",
        "0123456789ABCDEF0123456789ABCDE",
        "0123456789ABCDEF0123456789ABCDEF0",
        "0123456789abcdef0123456789abcdef",
        "0123456789ABCDEF0123456789ABCDEG",
        "0123456789ABCDEF0123456789ABCDE:",
    ] {
        assert!(Nonce::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn valid_run_for_every_mode() {
    let cases = [
        (RunMode::Read, "READ", "OK", RunStatus::Ok),
        (
            RunMode::Simulate,
            "SIMULATE",
            "SIMULATED_OK",
            RunStatus::SimulatedOk,
        ),
        (
            RunMode::Simulate,
            "SIMULATE",
            "NO_CHANGES",
            RunStatus::NoChanges,
        ),
        (RunMode::Deploy, "DEPLOY", "ACTIVE", RunStatus::Active),
        (RunMode::Deploy, "DEPLOY", "INACTIVE", RunStatus::Inactive),
        (
            RunMode::CreateSim,
            "CREATE_SIM",
            "SIMULATED_OK",
            RunStatus::SimulatedOk,
        ),
        (RunMode::Create, "CREATE", "ACTIVE", RunStatus::Active),
        (RunMode::Create, "CREATE", "INACTIVE", RunStatus::Inactive),
        (RunMode::Create, "CREATE", "PARTIAL", RunStatus::Partial),
        (
            RunMode::DeleteSim,
            "DELETE_SIM",
            "SIMULATED_OK",
            RunStatus::SimulatedOk,
        ),
        (
            RunMode::DeleteSim,
            "DELETE_SIM",
            "NOT_FOUND",
            RunStatus::NotFound,
        ),
        (RunMode::Delete, "DELETE", "DELETED", RunStatus::Deleted),
        (RunMode::Delete, "DELETE", "NOT_FOUND", RunStatus::NotFound),
        (RunMode::Delete, "DELETE", "REJECTED", RunStatus::Rejected),
    ];

    for (mode, token, status_token, status) in cases {
        let report = complete(parse(mode, &run(token, &[], status_token)));
        assert_eq!(
            (report.mode, report.status),
            (mode, status),
            "{token} {status_token}"
        );
        assert!(report.warnings.is_empty());
    }
}

#[test]
fn mode_tokens_round_trip() {
    for (mode, token) in [
        (RunMode::Read, "READ"),
        (RunMode::Simulate, "SIMULATE"),
        (RunMode::Deploy, "DEPLOY"),
        (RunMode::CreateSim, "CREATE_SIM"),
        (RunMode::Create, "CREATE"),
        (RunMode::DeleteSim, "DELETE_SIM"),
        (RunMode::Delete, "DELETE"),
    ] {
        assert_eq!(mode.token(), token);
    }
}

#[test]
fn read_run_carries_messages_facts_and_untrusted_lines() {
    let body = [
        format!("MSG:I:{}", hex("object read")),
        format!("LINE:{}", hex("<abapGit version=\"v1.0.0\">")),
        format!("LINE:{}", hex("  <ddtext>äöü€日</ddtext>")),
        format!("FACT:HASH:{}", hex("ab12")),
    ];
    let report = complete(parse(RunMode::Read, &run("READ", &body, "OK")));

    assert_eq!(report.messages.len(), 1);
    assert_eq!(report.messages[0].level, Level::Info);
    assert_eq!(report.messages[0].text, "object read");
    assert_eq!(report.lines.len(), 2);
    assert_eq!(report.lines[0].as_str(), "<abapGit version=\"v1.0.0\">");
    assert_eq!(report.lines[1].as_str(), "  <ddtext>äöü€日</ddtext>");
    assert_eq!(report.facts.len(), 1);
    assert_eq!(
        (report.facts[0].key.as_str(), report.facts[0].value.as_str()),
        ("HASH", "ab12")
    );
    assert!(!report.stop_batch);
}

#[test]
fn message_levels_parse() {
    let body = [
        format!("MSG:I:{}", hex("a")),
        format!("MSG:W:{}", hex("b")),
        format!("MSG:E:{}", hex("c")),
    ];
    let report = complete(parse(RunMode::Deploy, &run("DEPLOY", &body, "REJECTED")));

    let levels: Vec<_> = report
        .messages
        .iter()
        .map(|message| message.level)
        .collect();
    assert_eq!(levels, [Level::Info, Level::Warning, Level::Error]);
    assert_eq!(
        reason(parse(
            RunMode::Deploy,
            &run("DEPLOY", &[format!("MSG:X:{}", hex("a"))], "REJECTED"),
        )),
        Reason::Malformed(2),
    );
}

#[test]
fn forged_marker_inside_sap_text_stays_text() {
    let forged = format!(
        "x\n{}\n{}\n{}",
        line("STATUS:ACTIVE"),
        bound(OTHER, "END:1"),
        line("END:3"),
    );
    let body = [format!("MSG:E:{}", hex(&forged))];
    let report = complete(parse(RunMode::Deploy, &run("DEPLOY", &body, "REJECTED")));

    assert_eq!(report.status, RunStatus::Rejected);
    let text = &report.messages[0].text;
    assert!(!text.contains('\n'));
    assert!(text.starts_with("x "));
}

#[test]
fn unencoded_forged_lines_without_the_nonce_are_ignored_with_a_warning() {
    let mut lines = run("DEPLOY", &[], "REJECTED");
    lines.insert(1, "STATUS:ACTIVE".to_owned());
    lines.insert(1, "SAP says: ABAPREACH forged".to_owned());
    let report = complete(parse(RunMode::Deploy, &lines));

    assert_eq!(report.status, RunStatus::Rejected);
    assert_eq!(report.warnings.len(), 2);
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| warning.starts_with("W: "))
    );
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| !warning.contains("forged"))
    );
}

#[test]
fn wrong_nonce_lines_mixed_with_valid_ones_are_ignored() {
    let mut lines = run("DEPLOY", &[], "REJECTED");
    lines.insert(1, bound(OTHER, "STATUS:ACTIVE"));
    lines.insert(1, bound(OTHER, "BEGIN:DEPLOY"));
    lines.push(bound(OTHER, "END:2"));
    let report = complete(parse(RunMode::Deploy, &lines));

    assert_eq!(report.status, RunStatus::Rejected);
    assert_eq!(report.warnings.len(), 3);
}

#[test]
fn an_output_with_only_wrong_nonce_lines_is_uncertain() {
    let lines: Vec<String> = run("DEPLOY", &[], "ACTIVE")
        .iter()
        .map(|text| text.replace(NONCE, OTHER))
        .collect();
    let parsed = parse(RunMode::Deploy, &lines);

    assert_eq!(reason(parsed), Reason::MissingBegin);
}

#[test]
fn empty_and_html_output_is_uncertain() {
    assert_eq!(
        reason(parse_output(&nonce(), RunMode::Read, "")),
        Reason::MissingBegin
    );
    assert_eq!(
        reason(parse_output(
            &nonce(),
            RunMode::Read,
            "<html>Application Server Error</html>"
        )),
        Reason::MissingBegin,
    );
}

#[test]
fn a_missing_status_is_uncertain() {
    let lines = vec![line("BEGIN:DEPLOY"), line("END:1")];

    assert_eq!(
        reason(parse(RunMode::Deploy, &lines)),
        Reason::MissingStatus
    );
}

#[test]
fn a_duplicated_status_is_uncertain() {
    let lines = vec![
        line("BEGIN:DEPLOY"),
        line("STATUS:ACTIVE"),
        line("STATUS:ACTIVE"),
        line("END:3"),
    ];

    assert_eq!(
        reason(parse(RunMode::Deploy, &lines)),
        Reason::DuplicateStatus
    );

    let conflicting = vec![
        line("BEGIN:DEPLOY"),
        line("STATUS:ACTIVE"),
        line("STATUS:REJECTED"),
        line("END:3"),
    ];
    assert_eq!(
        reason(parse(RunMode::Deploy, &conflicting)),
        Reason::DuplicateStatus
    );
}

#[test]
fn a_duplicated_begin_is_uncertain() {
    let lines = vec![
        line("BEGIN:DEPLOY"),
        line("BEGIN:DEPLOY"),
        line("STATUS:ACTIVE"),
        line("END:3"),
    ];

    assert_eq!(
        reason(parse(RunMode::Deploy, &lines)),
        Reason::DuplicateBegin
    );
}

#[test]
fn an_invalid_marker_order_is_uncertain() {
    let body_first = vec![
        line(&format!("MSG:I:{}", hex("a"))),
        line("BEGIN:DEPLOY"),
        line("STATUS:ACTIVE"),
        line("END:3"),
    ];
    let status_first = vec![line("STATUS:ACTIVE"), line("BEGIN:DEPLOY"), line("END:2")];
    let body_after_status = vec![
        line("BEGIN:DEPLOY"),
        line("STATUS:ACTIVE"),
        line(&format!("MSG:I:{}", hex("a"))),
        line("END:3"),
    ];
    let end_first = vec![line("END:0"), line("BEGIN:DEPLOY"), line("STATUS:ACTIVE")];

    for lines in [body_first, status_first, body_after_status, end_first] {
        assert_eq!(
            reason(parse(RunMode::Deploy, &lines)),
            Reason::BadOrder,
            "{lines:?}"
        );
    }
}

#[test]
fn a_bound_line_after_end_is_uncertain() {
    let mut lines = run("DEPLOY", &[], "ACTIVE");
    lines.push(line(&format!("MSG:I:{}", hex("late"))));

    assert_eq!(
        reason(parse(RunMode::Deploy, &lines)),
        Reason::TrailingMarker
    );

    let mut second_end = run("DEPLOY", &[], "ACTIVE");
    second_end.push(line("END:3"));
    assert_eq!(
        reason(parse(RunMode::Deploy, &second_end)),
        Reason::TrailingMarker
    );
}

#[test]
fn text_after_end_without_the_nonce_is_only_a_warning() {
    let mut lines = run("DEPLOY", &[], "ACTIVE");
    lines.push("trailing noise".to_owned());
    let report = complete(parse(RunMode::Deploy, &lines));

    assert_eq!(report.status, RunStatus::Active);
    assert_eq!(report.warnings.len(), 1);
}

#[test]
fn a_missing_closing_count_is_uncertain() {
    let mut lines = run("DEPLOY", &[], "ACTIVE");
    lines.pop();

    assert_eq!(reason(parse(RunMode::Deploy, &lines)), Reason::Truncated);
}

#[test]
fn a_wrong_closing_count_is_uncertain() {
    for count in ["0", "1", "3", "4", "03", "+3", "-3", "", "x"] {
        let lines = vec![
            line("BEGIN:DEPLOY"),
            line("STATUS:ACTIVE"),
            line(&format!("END:{count}")),
        ];
        let found = reason(parse(RunMode::Deploy, &lines));

        assert!(
            matches!(found, Reason::CountMismatch { .. } | Reason::Malformed(_)),
            "{count:?}: {found:?}",
        );
    }

    let right = vec![line("BEGIN:DEPLOY"), line("STATUS:ACTIVE"), line("END:2")];
    assert!(matches!(
        parse(RunMode::Deploy, &right),
        Parsed::Complete(_)
    ));
}

#[test]
fn truncation_at_every_line_boundary_is_uncertain() {
    let body = [
        format!("MSG:I:{}", hex("one")),
        format!("LINE:{}", hex("row")),
        format!("FACT:K:{}", hex("v")),
    ];
    let lines = run("READ", &body, "OK");

    for keep in 0..lines.len() {
        let parsed = parse(RunMode::Read, &lines[..keep]);

        assert!(
            matches!(parsed, Parsed::Uncertain { .. }),
            "kept {keep} lines: {parsed:?}"
        );
    }

    assert!(matches!(parse(RunMode::Read, &lines), Parsed::Complete(_)));
}

#[test]
fn truncation_at_every_byte_is_uncertain() {
    let body = [
        format!("MSG:I:{}", hex("one")),
        format!("LINE:{}", hex("row")),
    ];
    let text = run("READ", &body, "OK").join("\n");

    for cut in 0..text.len() {
        let parsed = parse_output(&nonce(), RunMode::Read, &text[..cut]);

        assert!(
            matches!(parsed, Parsed::Uncertain { .. }),
            "cut at {cut}: {parsed:?}"
        );
    }

    assert!(matches!(
        parse_output(&nonce(), RunMode::Read, &text),
        Parsed::Complete(_)
    ));
}

#[test]
fn broken_encoding_is_uncertain() {
    let cases = [
        "abc".to_owned(),               // odd length
        "zz".to_owned(),                // not hex
        hex("hello").to_lowercase(),    // the grammar is upper case only
        hex_bytes(&[0x41, 0xFF, 0x42]), // not UTF-8
        hex_bytes(&[0xC3]),             // cut multibyte sequence
        " 41".to_owned(),
    ];

    for encoded in cases {
        let lines = run("READ", &[format!("LINE:{encoded}")], "OK");

        assert_eq!(
            reason(parse(RunMode::Read, &lines)),
            Reason::BrokenEncoding(2),
            "{encoded:?}",
        );
    }
}

#[test]
fn unknown_markers_and_wrong_field_counts_are_uncertain() {
    let bad_bodies = [
        "WHATEVER:41".to_owned(),
        "LINE".to_owned(),
        format!("LINE:{}:{}", hex("a"), hex("b")),
        "MSG:I".to_owned(),
        format!("FACT:lower:{}", hex("a")),
        format!("FACT::{}", hex("a")),
        format!("FACT:{}:{}", "A".repeat(25), hex("a")),
        "PARTIAL_DELETE".to_owned(),
    ];

    for body in bad_bodies {
        let parsed = parse(
            RunMode::Delete,
            &run("DELETE", std::slice::from_ref(&body), "DELETED"),
        );

        assert!(matches!(reason(parsed), Reason::Malformed(_)), "{body:?}",);
    }
}

#[test]
fn mode_and_status_must_agree_with_the_request() {
    let wrong_mode = run("DEPLOY", &[], "ACTIVE");
    assert_eq!(
        reason(parse(RunMode::Read, &wrong_mode)),
        Reason::ModeMismatch
    );

    let unknown_mode = run("FROB", &[], "ACTIVE");
    assert_eq!(
        reason(parse(RunMode::Deploy, &unknown_mode)),
        Reason::Malformed(1)
    );

    for (mode, token, status) in [
        (RunMode::Read, "READ", "ACTIVE"),
        (RunMode::Read, "READ", "SIMULATED_OK"),
        (RunMode::Simulate, "SIMULATE", "ACTIVE"),
        (RunMode::Deploy, "DEPLOY", "DELETED"),
        (RunMode::Deploy, "DEPLOY", "OK"),
        (RunMode::Delete, "DELETE", "ACTIVE"),
        (RunMode::DeleteSim, "DELETE_SIM", "DELETED"),
        (RunMode::Create, "CREATE", "SIMULATED_OK"),
        (RunMode::CreateSim, "CREATE_SIM", "NO_CHANGES"),
    ] {
        assert_eq!(
            reason(parse(mode, &run(token, &[], status))),
            Reason::StatusNotForMode,
            "{token} {status}",
        );
    }

    for status in ["", "active", "PARTIAL_DELETE", "NOTHING", "ACTIVE:X"] {
        let found = reason(parse(RunMode::Deploy, &run("DEPLOY", &[], status)));

        assert!(
            matches!(found, Reason::Malformed(_) | Reason::StatusNotForMode),
            "{status:?}: {found:?}",
        );
    }
}

#[test]
fn partial_markers_map_to_partial_and_stop_the_batch() {
    for (marker, expected) in [
        (
            "PARTIAL_DELETE",
            Stop::PartialDelete("DTEL YTEST_X".to_owned()),
        ),
        (
            "OBJECT_STILL_EXISTS",
            Stop::ObjectStillExists("DTEL YTEST_X".to_owned()),
        ),
    ] {
        for status in ["DELETED", "NOT_FOUND", "REJECTED"] {
            let body = [format!("{marker}:{}", hex("DTEL YTEST_X"))];
            let report = complete(parse(RunMode::Delete, &run("DELETE", &body, status)));

            assert_eq!(report.status, RunStatus::Partial, "{marker} {status}");
            assert!(report.stop_batch);
            assert_eq!(report.stops, std::slice::from_ref(&expected));
        }
    }
}

#[test]
fn partial_markers_carry_scrubbed_text() {
    let body = [format!(
        "PARTIAL_DELETE:{}",
        hex("a\nb\u{1b}[31mc\u{202e}d")
    )];
    let report = complete(parse(RunMode::Delete, &run("DELETE", &body, "DELETED")));

    assert_eq!(report.stops, [Stop::PartialDelete("a b[31mcd".to_owned())]);
}

#[test]
fn partial_markers_are_not_valid_outside_delete() {
    for (mode, token) in [
        (RunMode::Read, "READ"),
        (RunMode::Deploy, "DEPLOY"),
        (RunMode::Create, "CREATE"),
        (RunMode::DeleteSim, "DELETE_SIM"),
    ] {
        for marker in ["PARTIAL_DELETE", "OBJECT_STILL_EXISTS"] {
            let body = [format!("{marker}:{}", hex("x"))];
            let status = if token == "READ" { "OK" } else { "REJECTED" };

            assert_eq!(
                reason(parse(mode, &run(token, &body, status))),
                Reason::MarkerNotForMode,
                "{token} {marker}",
            );
        }
    }
}

#[test]
fn a_partial_status_stops_the_batch_and_other_statuses_do_not() {
    let partial = complete(parse(RunMode::Deploy, &run("DEPLOY", &[], "PARTIAL")));
    assert_eq!(partial.status, RunStatus::Partial);
    assert!(partial.stop_batch);
    assert!(partial.stops.is_empty());

    for status in ["ACTIVE", "INACTIVE", "REJECTED"] {
        assert!(!complete(parse(RunMode::Deploy, &run("DEPLOY", &[], status))).stop_batch);
    }
}

#[test]
fn very_large_output_parses() {
    let row = format!("LINE:{}", hex(&"x".repeat(200)));
    let body: Vec<String> = (0..10_000).map(|_| row.clone()).collect();
    let text = framed(&run("READ", &body, "OK"));
    assert!(text.len() > 4_000_000 && text.len() < MAX_OUTPUT_BYTES);

    let report = complete(parse_output(&nonce(), RunMode::Read, &text));
    assert_eq!(report.lines.len(), 10_000);
    assert_eq!(report.lines[9_999].as_str().len(), 200);
}

#[test]
fn output_above_the_cap_is_uncertain_without_parsing() {
    let text = "x".repeat(MAX_OUTPUT_BYTES + 1);

    assert_eq!(
        reason(parse_output(&nonce(), RunMode::Read, &text)),
        Reason::TooLarge
    );

    let valid = framed(&run("READ", &[], "OK"));
    let padded = format!("{valid}{}", "y".repeat(MAX_OUTPUT_BYTES));
    assert_eq!(
        reason(parse_output(&nonce(), RunMode::Read, &padded)),
        Reason::TooLarge
    );
}

#[test]
fn a_very_long_text_is_cut_by_the_scrub() {
    let body = [format!("MSG:I:{}", hex(&"m".repeat(70_000)))];
    let report = complete(parse(RunMode::Read, &run("READ", &body, "OK")));

    assert!(report.messages[0].text.chars().count() <= TEXT_LIMIT);
}

#[test]
fn warnings_are_capped() {
    let mut lines = run("READ", &[], "OK");
    for index in 0..1_000 {
        lines.insert(1, format!("noise {index}"));
    }
    let report = complete(parse(RunMode::Read, &lines));

    assert_eq!(report.warnings.len(), MAX_WARNINGS + 1);
    assert!(report.warnings.last().unwrap().contains("980"));
}

#[test]
fn carriage_returns_at_line_ends_are_tolerated() {
    let text: String = run("READ", &[], "OK")
        .iter()
        .map(|text| format!("{text}\r\n"))
        .collect();

    assert!(matches!(
        parse_output(&nonce(), RunMode::Read, &text),
        Parsed::Complete(_)
    ));
}

#[test]
fn a_5xx_post_reply_is_uncertain_even_with_a_complete_body() {
    let good = framed(&run("DEPLOY", &[], "ACTIVE"));

    for status in [500, 502, 503, 504, 599] {
        let html = parse_post_reply(&nonce(), RunMode::Deploy, status, "<html>Error</html>");
        assert_eq!(reason(html), Reason::ServerError(status));

        let with_run = parse_post_reply(&nonce(), RunMode::Deploy, status, &good);
        assert_eq!(reason(with_run), Reason::ServerError(status));
    }
}

#[test]
fn a_2xx_post_reply_is_parsed() {
    let good = framed(&run("DEPLOY", &[], "ACTIVE"));

    for status in [200, 201, 204] {
        let parsed = parse_post_reply(&nonce(), RunMode::Deploy, status, &good);
        assert!(matches!(parsed, Parsed::Complete(_)), "{status}");
    }

    assert_eq!(
        reason(parse_post_reply(
            &nonce(),
            RunMode::Deploy,
            200,
            "<html>x</html>"
        )),
        Reason::MissingBegin,
    );
}

#[test]
fn a_4xx_post_reply_means_the_class_did_not_run() {
    let good = framed(&run("DEPLOY", &[], "ACTIVE"));

    for status in [400, 403, 404, 406, 409] {
        assert!(
            matches!(
                parse_post_reply(&nonce(), RunMode::Deploy, status, &good),
                Parsed::NotRun(code) if code == status,
            ),
            "{status}",
        );
    }
}

#[test]
fn a_sadt_runtime_failure_is_uncertain_with_a_scrubbed_key() {
    assert_eq!(
        reason(runtime_failure("COMPUTE_INT_ZERODIVIDE")),
        Reason::RuntimeFailure("COMPUTE_INT_ZERODIVIDE".to_owned()),
    );
    assert_eq!(
        reason(runtime_failure("KEY\n\u{1b}[2J\u{202e}")),
        Reason::RuntimeFailure("KEY [2J".to_owned()),
    );
}

#[test]
fn reasons_have_plain_english_text() {
    for found in [
        Reason::TooLarge,
        Reason::MissingBegin,
        Reason::Truncated,
        Reason::MissingStatus,
        Reason::DuplicateStatus,
        Reason::DuplicateBegin,
        Reason::BadOrder,
        Reason::TrailingMarker,
        Reason::ModeMismatch,
        Reason::StatusNotForMode,
        Reason::MarkerNotForMode,
        Reason::Malformed(3),
        Reason::BrokenEncoding(4),
        Reason::CountMismatch {
            expected: 5,
            found: 6,
        },
        Reason::ServerError(500),
        Reason::RuntimeFailure("X".to_owned()),
    ] {
        let text = found.to_string();

        assert!(
            text.is_ascii() && !text.is_empty() && !text.contains('\n'),
            "{text}"
        );
    }
}

#[test]
fn scrub_removes_control_and_bidi_characters_and_newlines() {
    let cases = [
        ("plain text", "plain text"),
        ("äöü€日 stays", "äöü€日 stays"),
        ("a\nb", "a b"),
        ("a\r\nb", "a b"),
        ("a\tb", "a b"),
        ("a\u{0}b", "ab"),
        ("a\u{7}b\u{7f}c", "abc"),
        ("a\u{1b}[31mred", "a[31mred"),
        ("a\u{85}b", "ab"),
        ("a\u{2028}b\u{2029}c", "a b c"),
        ("a\u{202e}b", "ab"),
        ("\u{202a}\u{202b}\u{202c}\u{202d}x", "x"),
        ("\u{2066}\u{2067}\u{2068}\u{2069}x", "x"),
        ("a\u{200e}b\u{200f}c\u{61c}d", "abcd"),
        ("  padded  ", "padded"),
        ("", ""),
    ];

    for (input, expected) in cases {
        assert_eq!(scrub_text(input), expected, "{input:?}");
    }
}

#[test]
fn scrub_cuts_at_the_limit() {
    let long = "a".repeat(TEXT_LIMIT + 50);
    let cut = scrub_text(&long);

    assert_eq!(cut.chars().count(), TEXT_LIMIT);
    assert!(cut.ends_with("..."));
    assert_eq!(scrub_text(&"a".repeat(TEXT_LIMIT)), "a".repeat(TEXT_LIMIT));

    let wide = "日".repeat(TEXT_LIMIT + 1);
    assert_eq!(scrub_text(&wide).chars().count(), TEXT_LIMIT);
}

#[test]
fn scrubbed_hostile_text_has_no_control_or_bidi_characters() {
    let alphabet = [
        'a', '\n', '\r', '\t', '\u{0}', '\u{1b}', '\u{7f}', '\u{85}', '\u{202e}', '\u{2066}',
        '\u{200f}', '\u{2028}', ' ', '日',
    ];
    let mut seed = 7_u32;
    for _ in 0..200 {
        let mut text = String::new();
        for _ in 0..40 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            text.push(alphabet[(seed >> 16) as usize % alphabet.len()]);
        }

        let body = [format!("MSG:E:{}", hex(&text))];
        let report = complete(parse(RunMode::Deploy, &run("DEPLOY", &body, "REJECTED")));
        let shown = &report.messages[0].text;

        assert!(
            shown.chars().all(|c| !c.is_control()
                && !matches!(
                    c,
                    '\u{200e}' | '\u{200f}' | '\u{61c}' | '\u{2028}' | '\u{2029}'
                )
                && !('\u{202a}'..='\u{202e}').contains(&c)
                && !('\u{2066}'..='\u{2069}').contains(&c)),
            "{shown:?}",
        );
    }
}
