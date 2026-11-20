//! The pure parts of `run PROG` and `run SNIPPET`: the two templates and their scan, the snippet
//! generator, the payload, the protocol lines of an ALV grid, the formatter of the grid, the output
//! budget, and the expiry of a payload. The ABAP cannot be compiled offline; what can be pinned
//! without a SAP system is pinned here, and the live probe settles the rest.

use abapreach::cli::{AlvColumns, AlvMode};
use abapreach::operation::payload::{
    MAX_RUN_OUTPUT, MAX_RUN_PARAMS, RunProg, RunRequest, encode_run,
};
use abapreach::operation::run_alv::format_alv;
use abapreach::operation::runner_scan::{RUN_PROG_CLASSES, RUN_PROG_FUNCTIONS, ScanMode, scan_as};
use abapreach::operation::runner_template::{
    OutType, RUN_PROG_TEMPLATE, SNIPPET_MARKER, SNIPPET_TEMPLATE, TEMPLATES, generate_snippet,
    template_lines,
};
use abapreach::runner_protocol::{
    Alv, AlvColumn, Budget, FRAMING_BYTES, MAX_OUTPUT_BYTES, Nonce, Parsed, RunMode, RunStatus,
    hex_text, line_cost, parse_output,
};

const NAME: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn constant(name: &str) -> String {
    let template = TEMPLATES
        .iter()
        .find(|template| template.name == name)
        .unwrap();

    template_lines(template).join("\n")
}

fn rejected(code: &str, mode: ScanMode) -> Vec<String> {
    scan_as(code, None, mode).expect_err(code)
}

// ---- the RUN_PROG template and its scan -------------------------------------------------------

#[test]
fn the_run_prog_template_passes_with_exactly_the_added_names_and_no_other_mode_does() {
    let code = constant("RUN_PROG");

    scan_as(&code, None, ScanMode::RunProg).unwrap();

    let found = rejected(&code, ScanMode::Strict);
    let mut submits = 0;
    let mut classes: Vec<String> = Vec::new();
    let mut functions: Vec<String> = Vec::new();

    for finding in &found {
        let text = finding.split_once(": ").unwrap().1;

        if text == "SUBMIT is not allowed" {
            submits += 1;
        } else if let Some(name) = text
            .strip_prefix("class ")
            .and_then(|rest| rest.strip_suffix(" is not on the allow-list"))
        {
            classes.push(name.to_ascii_uppercase());
        } else if let Some(name) = text
            .strip_prefix("CALL FUNCTION '")
            .and_then(|rest| rest.strip_suffix("' is not on the allow-list"))
        {
            functions.push(name.to_owned());
        } else {
            panic!("a finding outside the added names: {finding}");
        }
    }

    classes.sort();
    classes.dedup();
    functions.sort();
    functions.dedup();

    // The two generated SUBMIT statements, and every added name is used (no name is allowed that
    // the template does not need).
    assert_eq!(submits, 2);
    assert_eq!(classes, RUN_PROG_CLASSES);
    assert_eq!(functions, RUN_PROG_FUNCTIONS);

    for template in TEMPLATES {
        if template.name != "RUN_PROG" {
            scan_as(&template_lines(template).join("\n"), None, ScanMode::Strict)
                .unwrap_or_else(|found| panic!("{}: {found:?}", template.name));
        }
    }
}

const SUBMIT: &str =
    "SUBMIT (mv_report) WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.";
const SUBMIT_VARIANT: &str = "SUBMIT (mv_report) USING SELECTION-SET mv_variant WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.";

#[test]
fn every_mode_but_run_prog_refuses_submit_even_in_the_generated_shape() {
    for template in TEMPLATES {
        let code = format!("{}\n{SUBMIT}\n{SUBMIT_VARIANT}", constant(template.name));
        let found = rejected(&code, ScanMode::Strict);

        assert_eq!(
            found
                .iter()
                .filter(|finding| finding.ends_with("SUBMIT is not allowed"))
                .count(),
            // The RUN_PROG template holds two of its own.
            if template.name == "RUN_PROG" { 4 } else { 2 },
            "{}: {found:?}",
            template.name
        );
    }
}

#[test]
fn the_generated_shapes_pass_and_every_other_submit_is_refused_in_run_prog_mode() {
    scan_as(SUBMIT, None, ScanMode::RunProg).unwrap();
    scan_as(SUBMIT_VARIANT, None, ScanMode::RunProg).unwrap();
    scan_as(
        &format!("IF a = 1.\n  {SUBMIT}\nENDIF."),
        None,
        ScanMode::RunProg,
    )
    .unwrap();

    for code in [
        // a literal report, another variable, a missing or extra addition, a second statement
        "SUBMIT zreport WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.",
        "SUBMIT ('ZREPORT') WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.",
        "SUBMIT (mv_name) WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.",
        "SUBMIT (mv_report) WITH SELECTION-TABLE lt_other EXPORTING LIST TO MEMORY AND RETURN.",
        "SUBMIT (mv_report) WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY.",
        "SUBMIT (mv_report) WITH SELECTION-TABLE mt_sel AND RETURN.",
        "SUBMIT (mv_report) EXPORTING LIST TO MEMORY AND RETURN.",
        "SUBMIT (mv_report) WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN VIA JOB lv_job NUMBER lv_n.",
        "SUBMIT (mv_report) USING SELECTION-SET `V1` WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.",
        "SUBMIT (mv_report) WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN",
        "SUBMIT (mv_report) WITH p_a = 1 EXPORTING LIST TO MEMORY AND RETURN.",
        "submit (mv_report) with selection-table mt_sel exporting list to memory and return via selection-screen.",
    ] {
        let found = rejected(code, ScanMode::RunProg);

        assert!(
            found
                .iter()
                .any(|finding| finding.ends_with("SUBMIT is not allowed")),
            "{code}: {found:?}"
        );
    }

    // A comment or a literal that holds the word is no statement.
    scan_as(
        "\" SUBMIT is done here\nx = `SUBMIT`.",
        None,
        ScanMode::RunProg,
    )
    .unwrap();
}

#[test]
fn the_added_names_exist_in_run_prog_mode_only() {
    for code in [
        "CALL FUNCTION 'LIST_FROM_MEMORY'.",
        "CALL FUNCTION 'RS_REPORTSELECTIONS_INFO'.",
        "cl_salv_bs_runtime_info=>clear_all( ).",
        "CATCH cx_salv_bs_sc_runtime_info.",
    ] {
        scan_as(code, None, ScanMode::RunProg).unwrap();
        assert!(!rejected(code, ScanMode::Strict).is_empty(), "{code}");
    }

    // The mode does not widen anything else.
    for code in [
        "CALL FUNCTION 'RFC_ABAP_INSTALL_AND_RUN'.",
        "x = cl_gui_alv_grid=>y( ).",
        "GENERATE SUBROUTINE POOL lt_src NAME lv_pool.",
        "UPDATE t000 SET cccategory = 'C'.",
    ] {
        assert!(!rejected(code, ScanMode::RunProg).is_empty(), "{code}");
    }
}

#[test]
fn the_added_names_are_standard_names_only() {
    for name in RUN_PROG_CLASSES.iter().chain(RUN_PROG_FUNCTIONS) {
        assert!(!name.starts_with(['Y', 'Z']), "{name}");
    }
}

// ---- the checks the template carries ----------------------------------------------------------

#[test]
fn the_core_refuses_a_payload_that_expired_or_belongs_to_another_user() {
    let core = constant("CORE");

    for line in [
        "METHOD check_run_values.",
        "GET TIME STAMP FIELD lv_now.",
        "lv_limit = CONV timestamp( mv_expires ).",
        "IF mv_user <> to_upper( CONV string( sy-uname ) ) OR lv_now > lv_limit.",
        "reject( `run payload expired or foreign user` ).",
    ] {
        assert!(
            core.lines().any(|candidate| candidate.trim() == line),
            "{line}"
        );
    }

    // The run mode is decided after the guards of the frame, before the report is read.
    let at = |text: &str| core.find(text).unwrap_or_else(|| panic!("{text}"));

    assert!(
        at("rv_ok = check_run_values( ).") < at("METHOD guard_system."),
        "the values are checked in check_values"
    );
    assert!(core.contains("OR `RUN_PROG` OR `RUN_SNIPPET`."));
}

#[test]
fn the_runner_counts_bytes_the_way_the_reference_budget_does() {
    let core = constant("CORE");

    assert_eq!(FRAMING_BYTES, 4);
    assert!(
        core.contains("mv_bytes = mv_bytes + strlen( |ABAPREACH:{ mv_nonce }:{ iv_body }| ) + 4.")
    );
    assert!(core.contains(
        "IF mv_cut = abap_true OR mv_bytes + strlen( |ABAPREACH:{ mv_nonce }:{ iv_body }| ) + 4 > mv_budget."
    ));
    assert!(core.contains("emit_budgeted( |LINE:{ hex_of( iv_text ) }| )."));
    // The cut line comes before the status, and `RAN` neither commits nor rolls back.
    assert!(
        core.contains("msg( iv_level = `W` iv_text = |output cut after { mv_lines } lines| ).")
    );

    let run = constant("RUN_PROG");

    assert!(
        run.contains(
            "emit_budgeted( |ALVROW:{ concat_lines_of( table = lt_cells sep = `,` ) }| )."
        )
    );
    assert!(run.contains("cut_note( ).\n    finish( `RAN` )."));

    let tail = core.split("WHEN `RAN`.").nth(1).unwrap();

    assert!(
        tail.lines()
            .take(4)
            .all(|line| !line.contains("COMMIT") && !line.contains("ROLLBACK")),
        "RAN is the dialog step's own commit"
    );
}

#[test]
fn run_prog_checks_the_report_in_the_order_of_sa38_and_submits_nothing_before_the_checks() {
    let run = constant("RUN_PROG");
    let at = |text: &str| run.find(text).unwrap_or_else(|| panic!("{text}"));
    let steps = [
        "SELECT SINGLE subc, secu FROM trdir",
        "READ REPORT lv_name INTO lt_source STATE 'A'.",
        "IF ls_dir-subc <> '1'.",
        "AUTHORITY-CHECK OBJECT 'S_PROGRAM'",
        "ID 'P_ACTION' FIELD 'SUBMIT'",
        "ID 'P_GROUP' FIELD ls_dir-secu.",
        "reject( `report changed since the simulate` ).",
        "SELECT SINGLE variant FROM varid",
    ];
    let mut last = 0;

    for step in steps {
        assert!(at(step) >= last, "{step} is out of order");
        last = at(step);
    }

    assert!(at("IF check_report( ) = abap_false.") < at("SUBMIT (mv_report) WITH"));
    assert!(at("IF build_selection( ) = abap_false.") < at("SUBMIT (mv_report) WITH"));
    // The ALV capture is set before the report runs and cleared after it, also after an exception.
    assert!(
        at(
            "cl_salv_bs_runtime_info=>set( display = abap_false metadata = abap_true data = abap_true )."
        ) < at("SUBMIT (mv_report) WITH")
    );
    assert!(run.contains("CLEANUP.\n            cl_salv_bs_runtime_info=>clear_all( )."));
    assert!(
        at("ENDTRY.\n        cl_salv_bs_runtime_info=>clear_all( ).")
            > at("SUBMIT (mv_report) USING")
    );
    // An amount and a quantity are written with their own reference, as SA38 does.
    assert!(
        run.contains("WRITE <lv_cell> TO lv_out CURRENCY lv_waers NO-GROUPING LEFT-JUSTIFIED.")
    );
    assert!(run.contains("WRITE <lv_cell> TO lv_out UNIT lv_unit NO-GROUPING LEFT-JUSTIFIED."));
    // The list is freed in every case.
    assert!(run.contains("CALL FUNCTION 'LIST_FREE_MEMORY'"));
    // The runtime exception of the frame is the only catch, and it rolls back.
    assert!(run.contains("CATCH cx_root INTO DATA(lx_error).\n        run_failed( lx_error )."));
}

// ---- the snippet template ----------------------------------------------------------------------

fn payload() -> Vec<String> {
    vec!["QUJD".to_owned()]
}

#[test]
fn a_snippet_stands_in_its_own_method_at_the_marker() {
    let text = "DATA(lv_x) = 1.\nprint( |x = { lv_x }| ).\n";
    let made = generate_snippet(NAME, &payload(), text, OutType::Internal).unwrap();
    let lines: Vec<&str> = made.source.lines().collect();

    assert_eq!(made.line_count, 2);
    assert_eq!(lines[made.first_line - 1], "DATA(lv_x) = 1.");
    assert_eq!(lines[made.first_line], "print( |x = { lv_x }| ).");
    assert_eq!(
        lines[made.first_line - 2],
        "    \" The statements of the snippet start on the next line."
    );
    assert_eq!(lines[made.first_line + 1].trim(), "ENDMETHOD.");
    assert!(!made.source.contains(SNIPPET_MARKER));
    // The method that holds it is the one the template declares, called from the frame.
    assert!(made.source.contains("  METHOD run_snippet.\n"));
    assert!(made.source.contains("run_snippet( )."));

    // The position does not depend on the name, the payload, or the type of OUT.
    let other = generate_snippet(
        "ZRUN_9",
        &["QUJDREVG".to_owned(), "QUJD".to_owned()],
        text,
        OutType::Out,
    )
    .unwrap();

    assert_eq!(other.first_line, made.first_line);
    assert_eq!(other.line_count, made.line_count);
}

#[test]
fn the_frame_of_a_snippet_class_keeps_its_scan_and_the_snippet_is_not_scanned() {
    let made = generate_snippet(
        NAME,
        &payload(),
        "CALL FUNCTION 'RFC_ABAP_INSTALL_AND_RUN'.\nx = zcl_user_class=>run( ).",
        OutType::Internal,
    )
    .unwrap();
    let mut lines: Vec<String> = made.source.lines().map(str::to_owned).collect();

    // The frame is the class with the marker where the snippet stands.
    lines.splice(
        made.first_line - 1..made.first_line - 1 + made.line_count,
        [SNIPPET_MARKER.to_owned()],
    );

    scan_as(&lines.join("\n"), Some(NAME), ScanMode::Strict).unwrap();
    assert_eq!(
        constant("SNIPPET")
            .lines()
            .filter(|line| *line == SNIPPET_MARKER)
            .count(),
        1
    );
    // The snippet is the user's own code: the 0.16 rules do not apply to it, so the whole source
    // does not pass the scan.
    assert!(scan_as(&made.source, Some(NAME), ScanMode::Strict).is_err());
}

#[test]
fn a_snippet_that_could_leave_its_method_is_refused_before_a_class_exists() {
    for text in [
        "ENDMETHOD.",
        "x = 1. endmethod. y = 2.",
        "INCLUDE zinc.",
        "INCLUDE: zinc1, zinc2.",
        "CLASS lcl DEFINITION.",
        "FORM f.",
        "DEFINE m.",
        "METHODS: a, b.",
        "x = 1",
        "",
        "   \n\n",
        "a = 1.\tb = 2.",
        "a = 'open.",
    ] {
        assert!(
            generate_snippet(NAME, &payload(), text, OutType::Internal).is_err(),
            "{text:?}"
        );
    }

    // The name and the payload are still checked.
    assert!(generate_snippet("ABC", &payload(), "x = 1.", OutType::Internal).is_err());
    assert!(generate_snippet(NAME, &[], "x = 1.", OutType::Internal).is_err());
    assert!(generate_snippet(NAME, &["A B".to_owned()], "x = 1.", OutType::Internal).is_err());
    assert!(SNIPPET_TEMPLATE.declarations.contains("METHODS print"));
    assert!(RUN_PROG_TEMPLATE.declarations.contains("METHODS run_prog."));
}

// ---- the payload of a run -----------------------------------------------------------------------

fn base64_decode(text: &str) -> Vec<u8> {
    let value = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("byte {c} outside the base64 alphabet"),
        }
    };
    let mut out = Vec::new();

    for quad in text.as_bytes().chunks(4) {
        let pad = quad.iter().filter(|&&c| c == b'=').count();
        let mut acc = 0u32;

        for &c in &quad[..4 - pad] {
            acc = (acc << 6) | value(c);
        }

        acc <<= 6 * pad as u32;
        out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8][..3 - pad]);
    }

    out
}

fn document(lines: &[String]) -> Vec<String> {
    String::from_utf8(base64_decode(&lines.concat()))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn request<'a>(prog: Option<RunProg<'a>>) -> RunRequest<'a> {
    RunRequest {
        nonce: NONCE,
        user: "DEVUSER",
        expires: "20261120103045",
        max_bytes: 1 << 20,
        prog,
    }
}

#[test]
fn a_report_payload_carries_the_report_the_bound_hash_and_every_option() {
    let params = vec![
        ("P_A".to_owned(), "1".to_owned()),
        ("P_B".to_owned(), "x=y z".to_owned()),
    ];
    let lines = encode_run(&request(Some(RunProg {
        report: "zreport",
        base: HASH,
        params: &params,
        variant: Some("v_1"),
        alv: AlvMode::On,
        alv_columns: AlvColumns::All,
    })))
    .unwrap();

    assert_eq!(
        document(&lines),
        [
            "FORMAT=1",
            &format!("NONCE={NONCE}"),
            "MODE=RUN_PROG",
            "USER=DEVUSER",
            "EXPIRES=20261120103045",
            "MAXBYTES=1048576",
            "REPORT=ZREPORT",
            &format!("BASE={HASH}"),
            "PARAM=P_A=1",
            "PARAM=P_B=x=y z",
            "VARIANT=V_1",
            "ALV=on",
            "ALVCOLS=all",
        ]
    );
    assert!(lines.iter().all(|line| line.len() <= 200));
}

#[test]
fn a_snippet_payload_has_no_object_and_no_code() {
    let lines = encode_run(&request(None)).unwrap();

    assert_eq!(
        document(&lines),
        [
            "FORMAT=1",
            &format!("NONCE={NONCE}"),
            "MODE=RUN_SNIPPET",
            "USER=DEVUSER",
            "EXPIRES=20261120103045",
            "MAXBYTES=1048576",
        ]
    );
}

#[test]
fn a_payload_value_that_breaks_a_rule_is_refused_before_a_connection() {
    let none: Vec<(String, String)> = Vec::new();
    let prog = |params: &'static [(String, String)],
                variant: Option<&'static str>,
                base: &'static str| RunProg {
        report: "ZREPORT",
        base,
        params,
        variant,
        alv: AlvMode::On,
        alv_columns: AlvColumns::Visible,
    };
    let long = vec![("P_A".to_owned(), "x".repeat(46))];
    let control = vec![("P_A".to_owned(), "a\nb".to_owned())];
    let name = vec![("P-A".to_owned(), "1".to_owned())];
    let many: Vec<(String, String)> = (0..=MAX_RUN_PARAMS)
        .map(|index| (format!("P{index}"), "1".to_owned()))
        .collect();
    let leaked: &'static [(String, String)] = Box::leak(long.into_boxed_slice());
    let control: &'static [(String, String)] = Box::leak(control.into_boxed_slice());
    let name: &'static [(String, String)] = Box::leak(name.into_boxed_slice());
    let many: &'static [(String, String)] = Box::leak(many.into_boxed_slice());
    let none: &'static [(String, String)] = Box::leak(none.into_boxed_slice());

    assert!(
        encode_run(&request(Some(prog(leaked, None, HASH)))).is_err(),
        "46 characters"
    );
    assert!(encode_run(&request(Some(prog(control, None, HASH)))).is_err());
    assert!(encode_run(&request(Some(prog(name, None, HASH)))).is_err());
    assert!(encode_run(&request(Some(prog(many, None, HASH)))).is_err());
    assert!(encode_run(&request(Some(prog(none, Some("a b"), HASH)))).is_err());
    assert!(encode_run(&request(Some(prog(none, None, "ABC")))).is_err());
    assert!(encode_run(&request(Some(prog(none, None, HASH)))).is_ok());

    // 45 characters are the limit of RSPARAMS-LOW.
    let edge = vec![("P_A".to_owned(), "x".repeat(45))];
    let edge: &'static [(String, String)] = Box::leak(edge.into_boxed_slice());

    assert!(encode_run(&request(Some(prog(edge, None, HASH)))).is_ok());

    for (user, expires, max) in [
        ("", "20261120103045", 1u64),
        ("DEV USER", "20261120103045", 1),
        ("DEVUSER", "2026112010304", 1),
        ("DEVUSER", "2026112010304x", 1),
        ("DEVUSER", "20261120103045", 0),
        ("DEVUSER", "20261120103045", MAX_RUN_OUTPUT + 1),
    ] {
        let mut broken = request(None);

        broken.user = user;
        broken.expires = expires;
        broken.max_bytes = max;

        assert!(encode_run(&broken).is_err(), "{user:?} {expires:?} {max}");
    }

    let mut edge = request(None);

    edge.max_bytes = MAX_RUN_OUTPUT;

    assert!(encode_run(&edge).is_ok());
}

// ---- the protocol lines of a grid -----------------------------------------------------------------

fn nonce() -> Nonce {
    Nonce::parse(NONCE).unwrap()
}

fn line(rest: &str) -> String {
    format!("ABAPREACH:{NONCE}:{rest}")
}

fn output(mode: &str, body: &[String], status: &str) -> String {
    let mut lines = vec![line(&format!("BEGIN:{mode}"))];

    lines.extend(body.iter().map(|rest| line(rest)));
    lines.push(line(&format!("STATUS:{status}")));

    let count = lines.len();

    lines.push(line(&format!("END:{count}")));

    lines.iter().map(|text| format!("{text}\n\n")).collect()
}

fn col(visible: bool, name: &str, text: &str) -> String {
    format!(
        "ALVCOL:{}:{}:{}",
        u8::from(visible),
        hex_text(name),
        hex_text(text)
    )
}

fn row(cells: &[&str]) -> String {
    format!(
        "ALVROW:{}",
        cells
            .iter()
            .map(|cell| hex_text(cell))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn complete(parsed: Parsed) -> abapreach::runner_protocol::Report {
    match parsed {
        Parsed::Complete(report) => report,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_grid_arrives_as_columns_then_rows_and_a_run_is_ran_or_failed() {
    let body = vec![
        col(true, "CARRID", "Airline"),
        col(false, "MANDT", "Client"),
        col(true, "PRICE", "Price"),
        row(&["LH", "100", "12.50"]),
        row(&["AA", "100", ""]),
        format!("LINE:{}", hex_text("list line")),
        format!("MSG:W:{}", hex_text("output cut after 3 lines")),
    ];
    let report = complete(parse_output(
        &nonce(),
        RunMode::RunProg,
        &output("RUN_PROG", &body, "RAN"),
    ));
    let alv = report.alv.unwrap();

    assert_eq!(report.status, RunStatus::Ran);
    assert_eq!(alv.columns.len(), 3);
    assert_eq!(
        alv.columns[1],
        AlvColumn {
            visible: false,
            name: "MANDT".into(),
            text: "Client".into()
        }
    );
    assert_eq!(alv.rows, [["LH", "100", "12.50"], ["AA", "100", ""]]);
    assert_eq!(report.lines.len(), 1);

    // A snippet that raised a catchable exception, and a refusal, are the other two statuses.
    for status in ["RUN_FAILED", "REJECTED"] {
        let report = complete(parse_output(
            &nonce(),
            RunMode::RunSnippet,
            &output(
                "RUN_SNIPPET",
                &[format!("MSG:E:{}", hex_text("exception CX_X: boom"))],
                status,
            ),
        ));

        assert_eq!(report.status.token(), status);
        assert!(report.alv.is_none());
    }

    // No grid: a normal run with the list only.
    let plain = complete(parse_output(
        &nonce(),
        RunMode::RunProg,
        &output("RUN_PROG", &[], "RAN"),
    ));

    assert!(plain.alv.is_none());
}

#[test]
fn a_grid_that_breaks_the_grammar_makes_the_run_uncertain() {
    let cases = [
        // a row before any column, a column after a row, a row of the wrong width
        vec![row(&["a"])],
        vec![col(true, "A", "A"), row(&["a"]), col(true, "B", "B")],
        vec![col(true, "A", "A"), row(&["a", "b"])],
        vec![col(true, "A", "A"), "ALVROW:ZZ".to_owned()],
        vec!["ALVCOL:2:41:41".to_owned()],
        vec!["ALVCOL:1:41".to_owned()],
    ];

    for body in cases {
        assert!(
            matches!(
                parse_output(
                    &nonce(),
                    RunMode::RunProg,
                    &output("RUN_PROG", &body, "RAN")
                ),
                Parsed::Uncertain { .. }
            ),
            "{body:?}"
        );
    }

    // A grid belongs to a report run only, and RAN to a run mode only.
    let grid = vec![col(true, "A", "A")];

    assert!(matches!(
        parse_output(
            &nonce(),
            RunMode::RunSnippet,
            &output("RUN_SNIPPET", &grid, "RAN")
        ),
        Parsed::Uncertain { .. }
    ));
    assert!(matches!(
        parse_output(&nonce(), RunMode::Read, &output("READ", &[], "RAN")),
        Parsed::Uncertain { .. }
    ));
    assert!(matches!(
        parse_output(&nonce(), RunMode::RunProg, &output("RUN_PROG", &[], "OK")),
        Parsed::Uncertain { .. }
    ));
}

// ---- the formatter of the grid ----------------------------------------------------------------------

fn grid() -> Alv {
    let column = |visible, name: &str, text: &str| AlvColumn {
        visible,
        name: name.to_owned(),
        text: text.to_owned(),
    };
    let cells = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();

    Alv {
        columns: vec![
            column(true, "CARRID", "Airline"),
            column(false, "MANDT", "Client"),
            column(true, "PRICE", "Price"),
            column(true, "WAERS", ""),
        ],
        rows: vec![
            cells(&["LH", "100", "12.50", "EUR"]),
            // JPY has no decimals, a tab and a line break in a value become blanks
            cells(&["JA\tL", "100", "1250", "JPY\nx"]),
        ],
    }
}

#[test]
fn the_visible_columns_print_in_catalog_order_with_their_texts() {
    assert_eq!(
        format_alv(&grid(), AlvColumns::Visible),
        [
            "Airline\tPrice\tWAERS",
            "LH\t12.50\tEUR",
            "JA L\t1250\tJPY x"
        ]
    );
}

#[test]
fn all_columns_print_every_column_of_the_table() {
    assert_eq!(
        format_alv(&grid(), AlvColumns::All),
        [
            "Airline\tClient\tPrice\tWAERS",
            "LH\t100\t12.50\tEUR",
            "JA L\t100\t1250\tJPY x"
        ]
    );
}

#[test]
fn a_grid_without_a_visible_column_or_without_rows_prints_what_it_has() {
    let mut hidden = grid();

    for column in &mut hidden.columns {
        column.visible = false;
    }

    assert!(format_alv(&hidden, AlvColumns::Visible).is_empty());
    assert_eq!(format_alv(&hidden, AlvColumns::All).len(), 3);

    let mut empty = grid();

    empty.rows.clear();

    assert_eq!(
        format_alv(&empty, AlvColumns::Visible),
        ["Airline\tPrice\tWAERS"]
    );
    assert!(format_alv(&Alv::default(), AlvColumns::All).is_empty());
}

// ---- the output budget ---------------------------------------------------------------------------------

fn run_with_budget(limit: u64, texts: impl Iterator<Item = String>) -> (String, Budget) {
    let nonce = nonce();
    let mut budget = Budget::new(usize::try_from(limit).unwrap());
    let mut lines = Vec::new();
    let mut body = |budget: &mut Budget, rest: String, data: bool| -> bool {
        let kept = if data {
            budget.offer(&nonce, &rest)
        } else {
            budget.spend(&nonce, &rest);

            true
        };

        if kept {
            lines.push(rest);
        }

        kept
    };

    body(&mut budget, "BEGIN:RUN_SNIPPET".to_owned(), false);

    for text in texts {
        if !body(&mut budget, format!("LINE:{}", hex_text(&text)), true) {
            break;
        }
    }

    if budget.is_cut() {
        let note = format!(
            "MSG:W:{}",
            hex_text(&format!("output cut after {} lines", budget.lines()))
        );

        body(&mut budget, note, false);
    }

    body(&mut budget, "STATUS:RAN".to_owned(), false);

    let count = lines.len();

    lines.push(format!("END:{count}"));

    (
        lines
            .iter()
            .map(|rest| format!("{}\n\n", line(rest)))
            .collect(),
        budget,
    )
}

#[test]
fn one_character_lines_stop_at_the_limit_and_the_reply_stays_under_the_parse_cap() {
    let limit = MAX_RUN_OUTPUT;
    // Far more lines than fit: the cheapest line the grammar allows is an empty one.
    let (reply, budget) = run_with_budget(limit, std::iter::repeat_n("x".to_owned(), 400_000));

    assert!(budget.is_cut());
    assert!(reply.len() < MAX_OUTPUT_BYTES, "{} bytes", reply.len());
    assert!(
        reply.len() as u64 <= limit + 2_000,
        "the closing lines fit in the headroom"
    );

    let report = complete(parse_output(&nonce(), RunMode::RunSnippet, &reply));

    assert_eq!(report.status, RunStatus::Ran);
    assert_eq!(report.lines.len(), budget.lines());
    assert!(
        report
            .messages
            .iter()
            .any(|message| message.text == format!("output cut after {} lines", budget.lines()))
    );
    assert!(report.lines.iter().all(|text| text.as_str() == "x"));
}

#[test]
fn non_ascii_text_costs_its_hex_and_stays_under_the_cap_too() {
    // Three bytes a character, six hex digits each: a short line is dear.
    let text = "日本語".repeat(40);
    let (reply, budget) =
        run_with_budget(MAX_RUN_OUTPUT, std::iter::repeat_n(text.clone(), 50_000));

    assert!(budget.is_cut());
    assert!(reply.len() < MAX_OUTPUT_BYTES, "{} bytes", reply.len());

    let report = complete(parse_output(&nonce(), RunMode::RunSnippet, &reply));

    assert_eq!(report.status, RunStatus::Ran);
    assert!(report.lines.iter().all(|line| line.as_str() == text));
}

#[test]
fn a_limit_that_the_output_never_reaches_cuts_nothing_and_a_cut_is_final() {
    let (reply, budget) =
        run_with_budget(1 << 20, ["one".to_owned(), "two".to_owned()].into_iter());

    assert!(!budget.is_cut());
    assert!(!reply.contains(&hex_text("output cut")));

    let report = complete(parse_output(&nonce(), RunMode::RunSnippet, &reply));

    assert_eq!(report.lines.len(), 2);
    assert!(report.messages.is_empty());

    // After one line did not fit, a shorter one does not either: the cut is final.
    let mut budget = Budget::new(300);
    let nonce = nonce();

    assert!(budget.offer(&nonce, "LINE:41"));
    assert!(!budget.offer(&nonce, &format!("LINE:{}", "41".repeat(200))));
    assert!(!budget.offer(&nonce, "LINE:41"));
    assert!(budget.is_cut());
    assert_eq!(budget.lines(), 1);
    assert_eq!(
        line_cost(&nonce, "LINE:41"),
        10 + 32 + 1 + 7 + FRAMING_BYTES
    );
}
