//! The `TRAN` template of the runner class. The ABAP cannot be compiled or run offline; compile and
//! activation are proven live. These tests pin what can be checked without a SAP system: the
//! registration, the pinned hash, the scanner, and the branches that must exist as template lines.

use abapreach::operation::payload::{Kind, Mode, Request, encode};
use abapreach::operation::runner_scan::{ALLOWED_FUNCTIONS, scan};
use abapreach::operation::runner_template::{
    TEMPLATES, TRAN_TEMPLATE, generate, template_hash, template_lines,
};

const NAME: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The pinned hash of the `TRAN` template: a template line that changes is a deliberate act.
const PINNED: &str = "8a2cceaac3eee61a8e17011e78d07823cc72ce2516cd0d3d307bf6478e835867";

/// The text of one method of the `TRAN` part, from `METHOD` to `ENDMETHOD.`.
fn body(method: &str) -> String {
    let start = format!("  METHOD {method}.");
    let lines: Vec<&str> = TRAN_TEMPLATE
        .methods
        .lines()
        .skip_while(|line| *line != start)
        .take_while(|line| *line != "  ENDMETHOD.")
        .collect();

    assert!(!lines.is_empty(), "method {method} is not implemented");

    lines.join("\n")
}

/// The positions of the needles in the text, in the order given; a needle that is missing or out of
/// order fails the test.
fn in_order(text: &str, needles: &[&str]) {
    let mut from = 0;

    for needle in needles {
        match text[from..].find(needle) {
            Some(at) => from += at + needle.len(),
            None => panic!("{needle:?} is missing or out of order in:\n{text}"),
        }
    }
}

fn has(text: &str, needles: &[&str]) {
    for needle in needles {
        assert!(text.contains(needle), "{needle:?} is missing in:\n{text}");
    }
}

fn function_calls(text: &str) -> Vec<String> {
    let mut found: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("CALL FUNCTION '"))
        .filter_map(|rest| rest.split('\'').next())
        .map(str::to_owned)
        .collect();

    found.sort_unstable();
    found.dedup();

    found
}

// ---- the registration ------------------------------------------------------------------------------

#[test]
fn the_tran_template_is_registered_once() {
    assert_eq!(TRAN_TEMPLATE.name, "TRAN");
    assert_eq!(
        TEMPLATES
            .iter()
            .filter(|template| template.name == "TRAN")
            .count(),
        1
    );
}

#[test]
fn the_tran_template_hash_is_pinned() {
    assert_eq!(
        template_hash(&TRAN_TEMPLATE),
        PINNED,
        "the TRAN template changed"
    );
}

#[test]
fn the_tran_template_passes_the_scanner_alone_and_as_a_class() {
    scan(&template_lines(&TRAN_TEMPLATE).join("\n"), None).expect("constant part");

    let lines = encode(&Request::new(Mode::Read, Kind::Tran, "YTRAN", NONCE)).unwrap();
    let source = generate(&TRAN_TEMPLATE, NAME, &lines).unwrap();

    scan(&source, Some(NAME)).expect("whole class");
}

#[test]
fn the_tran_part_calls_only_the_function_modules_the_task_names() {
    let calls = function_calls(&format!(
        "{}\n{}",
        TRAN_TEMPLATE.declarations, TRAN_TEMPLATE.methods
    ));

    assert_eq!(
        calls,
        [
            "RPY_TRANSACTION_DELETE",
            "RPY_TRANSACTION_INSERT",
            "RS_CORR_INSERT",
            "TR_TADIR_INTERFACE"
        ]
    );

    for call in &calls {
        assert!(ALLOWED_FUNCTIONS.contains(&call.as_str()), "{call}");
    }

    // The transaction is read from its tables, like the Python tool; no read module is on the
    // allow-list.
    assert!(!TRAN_TEMPLATE.methods.contains("RPY_TRANSACTION_READ"));
}

#[test]
fn every_declared_method_of_the_tran_part_is_implemented() {
    let declared: Vec<&str> = TRAN_TEMPLATE
        .declarations
        .lines()
        .filter_map(|line| line.trim().strip_prefix("METHODS "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|name| name.trim_end_matches('.'))
        .collect();

    assert!(declared.contains(&"tran_live"), "{declared:?}");

    for name in declared {
        let _ = body(name);
    }

    let _ = body("run_type");
}

// ---- the modes ---------------------------------------------------------------------------------------

#[test]
fn run_type_handles_the_five_modes_and_refuses_the_rest() {
    let text = body("run_type");

    has(
        &text,
        &[
            "WHEN `READ`.",
            "WHEN `CREATE_SIM` OR `CREATE`.",
            "WHEN `DELETE_SIM` OR `DELETE`.",
            "WHEN OTHERS.",
            "tran_read( )",
            "tran_create( )",
            "tran_delete( )",
        ],
    );
    assert!(
        text.contains("mv_type <> `TRAN`"),
        "other types are refused"
    );
    assert!(!text.contains("`SIMULATE`") && !text.contains("`DEPLOY`"));
    assert!(!TRAN_TEMPLATE.methods.contains("PARITY"), "no PARITY line");
}

// ---- read --------------------------------------------------------------------------------------------

#[test]
fn read_returns_the_six_lines_of_the_python_tool_and_does_not_check_the_type() {
    in_order(
        &body("tran_live"),
        &[
            "FROM tstc",
            "FROM tstct",
            "FROM tadir",
            "|TRAN { mv_tcode }|",
            "|program={ ms_tran-program }|",
            "|dynpro={ ms_tran-dynpro }|",
            "|cinfo={ ms_tran-cinfo }|",
            "|text={ ms_tran-text }|",
            "|package={ ms_tran-devclass }|",
        ],
    );

    let read = body("tran_read");

    in_order(
        &read,
        &[
            "tran_live( ) = abap_false",
            "`NOT_FOUND`",
            "data_line(",
            "finish( `OK` )",
        ],
    );
    assert!(!read.contains("tstc-cinfo"), "the type is not checked");
    assert!(!read.contains("RPY_"), "read calls no module");
    assert!(!read.contains("`Y`"), "read takes SAP names");
}

// ---- create --------------------------------------------------------------------------------------------

#[test]
fn create_needs_one_executable_customer_report_and_stops_before_the_write_in_simulate() {
    let text = body("tran_create");

    in_order(
        &text,
        &[
            "Only customer names can be created",
            "FROM tstc",
            "already exists",
            "FROM tdevc",
            "FROM trdir",
            "<> '1'",
            "exactly one existing executable report",
            "is not a customer report",
            "check_transport( mv_package )",
            "mv_mode = `CREATE_SIM`",
            "`SIMULATED_OK`",
            "tran_insert( )",
        ],
    );
    has(&text, &["mv_package = `$TMP`", "mv_report", "SUBC 1"]);
}

#[test]
fn insert_registers_the_object_then_inserts_a_report_transaction_on_screen_1000() {
    let text = body("tran_insert");

    in_order(
        &text,
        &[
            "FROM tadir",
            "record_object( iv_devclass = mv_package iv_mode = `I` )",
            "CALL FUNCTION 'RPY_TRANSACTION_INSERT'",
        ],
    );
    has(
        &text,
        &[
            "transaction_type     = 'R'",
            "dynpro               = '1000'",
            "suppress_corr_insert = 'X'",
            "program              = lv_report",
            "shorttext            = CONV tstct-ttext( mv_text )",
        ],
    );
}

#[test]
fn a_failed_insert_removes_the_tadir_row_and_a_failed_cleanup_is_partial() {
    let insert = body("tran_insert");

    in_order(
        &insert,
        &[
            "CALL FUNCTION 'RPY_TRANSACTION_INSERT'",
            "RPY_TRANSACTION_INSERT failed",
            "tran_undo(",
            "RETURN.",
        ],
    );

    let undo = body("tran_undo");

    in_order(
        &undo,
        &[
            "IF iv_had_row = abap_true",
            "iv_delflag",
            "ELSE.",
            "wi_delete_tadir_entry",
            "COMMIT WORK AND WAIT",
            "delflag = @space",
            "TADIR R3TR TRAN { mv_name } left behind without TSTC",
            "finish( `PARTIAL` )",
        ],
    );
}

#[test]
fn a_tstc_tadir_mismatch_after_the_insert_is_incomplete() {
    let text = body("tran_insert");

    in_order(
        &text,
        &[
            "COMMIT WORK AND WAIT",
            "FROM tstc",
            "FROM tadir",
            "incomplete",
            "finish( `INACTIVE` )",
            "finish( `ACTIVE` )",
        ],
    );
}

// ---- delete ----------------------------------------------------------------------------------------------

#[test]
fn delete_is_customer_only_and_prints_the_plan_facts_before_any_write() {
    let text = body("tran_delete");

    in_order(
        &text,
        &[
            "<> `Z` AND lv_first <> `Y`",
            "tran_live( ) = abap_false",
            "`NOT_FOUND`",
            "tran_request( ) = abap_false",
            "tran_hash( )",
            "mv_mode = `DELETE_SIM`",
            "`PACKAGE`",
            "`AUTHOR`",
            "`CINFO`",
            "`REQUEST`",
            "`HASH`",
            "finish( `SIMULATED_OK` )",
            "mv_plan <> lv_hash",
            "Plan changed since simulate; simulate again",
            "tran_remove( )",
        ],
    );
}

#[test]
fn the_delete_run_records_the_delete_removes_the_transaction_and_checks_the_result() {
    let text = body("tran_remove");

    in_order(
        &text,
        &[
            "CALL FUNCTION 'RS_CORR_INSERT'",
            "mode                = 'D'",
            "CALL FUNCTION 'RPY_TRANSACTION_DELETE'",
            "suppress_corr_insert = 'X'",
            "ROLLBACK WORK",
            "partial_delete(",
            "finish( `PARTIAL` )",
            "wi_delete_tadir_entry",
            "iv_delflag",
            "COMMIT WORK AND WAIT",
            "FROM tstc",
            "finish( `DELETED` )",
            "still_exists(",
            "finish( `PARTIAL` )",
        ],
    );
}

#[test]
fn the_request_rules_follow_a_lock_or_demand_a_request() {
    let text = body("tran_request");

    in_order(
        &text,
        &[
            "`$`",
            "FROM e071",
            "INNER JOIN e070",
            "e070~trstatus = 'D'",
            "WHEN 0.",
            "check_transport(",
            "WHEN 1.",
            "as4user <> sy-uname",
            "is locked in request",
            "mv_tr = lv_request",
            "WHEN OTHERS.",
            "more than one request",
        ],
    );
}

#[test]
fn the_plan_hash_is_a_lower_case_sha256_of_the_normalized_lines() {
    in_order(
        &body("tran_hash"),
        &[
            "shift_right(",
            "concat_lines_of(",
            "|\\n|",
            "'UTF-8'",
            "'SHA256'",
            "to_lower(",
        ],
    );
}

// ---- the payload --------------------------------------------------------------------------------------------

#[test]
fn every_tran_request_the_encoder_builds_gives_a_scanned_class() {
    for mode in [
        Mode::Read,
        Mode::CreateSim,
        Mode::Create,
        Mode::DeleteSim,
        Mode::Delete,
    ] {
        let mut request = Request::new(mode, Kind::Tran, "YTRAN", NONCE);

        match mode {
            Mode::CreateSim | Mode::Create => {
                request.report = Some("ZREPORT");
                request.text = Some("A transaction");
                request.package = Some("ZPACKAGE");
                request.tr = Some("DEVK900001");
            }
            Mode::Delete => request.plan = Some(HASH),
            _ => {}
        }

        let lines = encode(&request).unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        let source = generate(&TRAN_TEMPLATE, NAME, &lines).unwrap();

        scan(&source, Some(NAME)).unwrap_or_else(|found| panic!("{mode:?}: {found:?}"));
    }
}
