//! The DDIC templates of the runner class (`DTEL`, `DOMA`, `TABL`, `TTYP`). The ABAP cannot be
//! compiled or run offline, so these tests pin what the generated text must contain and the order
//! in which a run applies its steps. Compile, activation, and behaviour are proven live (Tasks
//! 12A, 35, 37). The four types share one body: every run knows its type from the payload.

use abapreach::operation::runner_scan::{ALLOWED_FUNCTIONS, scan};
use abapreach::operation::runner_template::{TEMPLATES, TypeTemplate, generate, template_lines};
use abapreach::runner_protocol::RunStatus;

const NAME: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const KINDS: [&str; 4] = ["DTEL", "DOMA", "TABL", "TTYP"];

fn template(name: &str) -> &'static TypeTemplate {
    TEMPLATES
        .iter()
        .copied()
        .find(|template| template.name == name)
        .unwrap_or_else(|| panic!("template {name} is not registered"))
}

fn text(name: &str) -> String {
    template_lines(template(name)).join("\n")
}

/// The lines of one method, from `METHOD x.` to `ENDMETHOD.`.
fn body(text: &str, method: &str) -> String {
    let start = format!("  METHOD {method}.");
    let mut lines = text.lines().skip_while(|line| *line != start);

    assert!(lines.next().is_some(), "method {method} is not implemented");

    lines
        .take_while(|line| line.trim() != "ENDMETHOD.")
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_order(haystack: &str, steps: &[&str]) {
    let mut last = 0;

    for step in steps {
        let found = haystack[last..]
            .find(step)
            .unwrap_or_else(|| panic!("`{step}` is missing or out of order"));

        last += found + step.len();
    }
}

/// Every literal between backticks that follows `prefix`.
fn literals_after(text: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find(prefix) {
        rest = &rest[start + prefix.len()..];

        if let Some(end) = rest.find('`') {
            found.push(rest[..end].to_owned());
        }
    }

    found
}

fn function_modules(text: &str) -> Vec<String> {
    let mut found = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("CALL FUNCTION '")
            && let Some(end) = rest.find('\'')
        {
            found.push(rest[..end].to_owned());
        }
    }

    found.sort();
    found.dedup();

    found
}

fn status_tokens() -> Vec<&'static str> {
    [
        RunStatus::Ok,
        RunStatus::SimulatedOk,
        RunStatus::NoChanges,
        RunStatus::Active,
        RunStatus::Inactive,
        RunStatus::Rejected,
        RunStatus::Partial,
        RunStatus::NotFound,
        RunStatus::Deleted,
    ]
    .map(RunStatus::token)
    .to_vec()
}

// ---- registration and the scanner -----------------------------------------------------------

#[test]
fn the_four_ddic_templates_are_registered_once_each() {
    for kind in KINDS {
        assert_eq!(
            TEMPLATES.iter().filter(|t| t.name == kind).count(),
            1,
            "{kind}"
        );
    }
}

#[test]
fn each_ddic_template_generates_and_passes_the_scanner() {
    for kind in KINDS {
        let template = template(kind);
        let source = generate(template, NAME, &["QUJD".to_owned()]).expect("generated");

        scan(&template_lines(template).join("\n"), None)
            .unwrap_or_else(|f| panic!("{kind}: {f:?}"));
        scan(&source, Some(NAME)).unwrap_or_else(|f| panic!("{kind}: {f:?}"));
    }
}

#[test]
fn each_ddic_template_calls_only_allow_listed_function_modules() {
    let required = [
        "DDIF_DOMA_ACTIVATE",
        "DDIF_DOMA_GET",
        "DDIF_DOMA_PUT",
        "DDIF_DTEL_ACTIVATE",
        "DDIF_DTEL_GET",
        "DDIF_DTEL_PUT",
        "DDIF_TABL_ACTIVATE",
        "DDIF_TABL_GET",
        "DDIF_TABL_PUT",
        "DDIF_TTYP_ACTIVATE",
        "DDIF_TTYP_GET",
        "DDIF_TTYP_PUT",
        "DD_DECIDE",
        "DD_GET_NAMETAB",
        "DD_NTAB_GEN",
        "DD_TABL_EXPAND",
        "RS_ACCESS_PERMISSION",
        "RS_CORR_INSERT",
        "RS_DD_DELETE_OBJ",
        "TR_SYS_PARAMS",
    ];

    for kind in KINDS {
        let calls = function_modules(&text(kind));

        for call in &calls {
            assert!(ALLOWED_FUNCTIONS.contains(&call.as_str()), "{kind}: {call}");
        }

        for call in required {
            assert!(calls.iter().any(|c| c == call), "{kind}: {call} is missing");
        }

        assert!(
            !calls.iter().any(|c| c.starts_with("RPY_")),
            "{kind}: a DDIC template must not call a transaction module"
        );
    }
}

// ---- modes and statuses ----------------------------------------------------------------------

#[test]
fn the_dispatcher_serves_all_seven_modes_and_refuses_tran() {
    for kind in KINDS {
        let run = body(&text(kind), "run_type");

        for (arm, call) in [
            ("WHEN `READ`.", "do_read( )"),
            ("WHEN `SIMULATE` OR `DEPLOY`.", "do_deploy( )"),
            ("WHEN `CREATE_SIM` OR `CREATE`.", "do_create( )"),
            ("WHEN `DELETE_SIM` OR `DELETE`.", "do_delete( )"),
        ] {
            assert_order(&run, &[arm, call]);
        }

        assert_order(&run, &["mv_type = `TRAN`", "reject("]);
    }
}

#[test]
fn every_mode_handler_ends_only_in_statuses_its_modes_accept() {
    // The sets are the statuses the parser accepts for the modes a handler serves.
    let handlers: [(&str, &[&str]); 6] = [
        ("do_read", &["OK", "NOT_FOUND", "REJECTED"]),
        (
            "do_deploy",
            &[
                "SIMULATED_OK",
                "NO_CHANGES",
                "ACTIVE",
                "INACTIVE",
                "REJECTED",
                "PARTIAL",
            ],
        ),
        (
            "write_definition",
            &["ACTIVE", "INACTIVE", "REJECTED", "PARTIAL"],
        ),
        (
            "do_create",
            &["SIMULATED_OK", "ACTIVE", "INACTIVE", "REJECTED", "PARTIAL"],
        ),
        (
            "do_delete",
            &[
                "SIMULATED_OK",
                "NOT_FOUND",
                "REJECTED",
                "DELETED",
                "PARTIAL",
            ],
        ),
        ("delete_definition", &["DELETED", "PARTIAL", "REJECTED"]),
    ];
    let known = status_tokens();

    for kind in KINDS {
        let text = text(kind);

        for (handler, accepted) in handlers {
            let statuses = literals_after(&body(&text, handler), "finish( `");

            for status in statuses {
                assert!(known.contains(&status.as_str()), "{handler}: {status}");
                assert!(accepted.contains(&status.as_str()), "{handler}: {status}");
            }
        }
    }
}

#[test]
fn the_fact_keys_are_valid_protocol_keys() {
    let all = text("DTEL");
    let keys = literals_after(&all, "iv_key = `");

    for key in &keys {
        let valid = key.len() <= 24
            && key.starts_with(|c: char| c.is_ascii_uppercase())
            && key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');

        assert!(valid, "{key}");
    }

    for key in [
        "HASH",
        "RESULT_HASH",
        "PACKAGE",
        "AUTHOR",
        "REQUEST",
        "STATE",
        "ROWS",
        "GUARD",
    ] {
        assert!(keys.iter().any(|k| k == key), "fact {key} is never emitted");
    }
}

#[test]
fn the_partial_delete_markers_belong_to_the_delete_path_only() {
    // The parser accepts them in a delete run only.
    for kind in KINDS {
        let text = text(kind);
        let delete = body(&text, "delete_definition");

        assert!(delete.contains("partial_delete("));
        assert!(delete.contains("still_exists("));

        for method in [
            "do_read",
            "do_deploy",
            "do_create",
            "write_definition",
            "check_changes",
            "verify_definition",
        ] {
            let other = body(&text, method);

            assert!(!other.contains("partial_delete("), "{method}");
            assert!(!other.contains("still_exists("), "{method}");
        }
    }
}

// ---- the guards the contract names ----------------------------------------------------------

#[test]
fn every_message_of_the_contract_is_a_template_line() {
    let all = text("TABL");

    for line in [
        "TABCLASS { iv_class } not supported",
        "TABCLASS { mv_name } from/to TRANSP not supported (SE11/SE14)",
        "Transparent table name { mv_name } > 16 characters",
        "Table { mv_name } ({ lv_rows } rows) needs SE14 conversion; change rejected",
        "CONVERSION { mv_name }: action={ lv_action } modeflag={ lv_modeflag } daction={ lv_daction } allowed={ lv_allowed } rows={ lv_rows }",
        "CONVERSION { iv_tabname }: action={ lv_action } modeflag={ lv_modeflag } daction={ lv_daction } allowed={ lv_allowed } rows={ lv_rows }",
        "Batch order: delete users{ lv_later } first before { mv_type } { mv_name }",
        "Plan changed since simulate; simulate again",
        "Table { mv_name } contains { lv_rows } rows; delete rejected",
        "still in use:{ lv_users }; delete rejected",
        "Base changed since the diff was shown; run deploy again",
        "WHERE-USED { ls_user-object } { ls_user-name }",
        "(transparent)",
        "active version does not match the file after activation",
        "still awaiting DB adjustment (TBATG)",
        "The row count of { mv_name } is not available",
        "XML file is for { lv_file_name }, not { mv_type } { mv_name }",
        "--text differs from the description in the XML file",
        "XML row longer than 255 characters",
        "has no TADIR R3TR",
        "already exists",
        "XML file without <asx:abap>",
    ] {
        assert!(all.contains(line), "`{line}` is not a template line");
    }

    assert!(!all.contains("transparan"), "the token is English");
}

#[test]
fn the_conversion_verdict_follows_the_contract_path() {
    let all = text("TABL");
    let core = body(&all, "decide_core");

    // Expand mode 1, the active definition, the buffered key guard, then the nametabs, then
    // DD_DECIDE in mode K without a program and without execution.
    assert_order(
        &core,
        &[
            "expanded(",
            "'DDIF_TABL_GET'",
            "changed on a buffered table",
            "'DD_NTAB_GEN'",
            "'DD_GET_NAMETAB'",
            "'DD_DECIDE'",
            "ev_daction = ls_info-daction",
            "ev_action = 'MDF' AND ls_info-daction = 'ALT'",
            "ev_allowed = xsdbool( ev_modeflag = 'A'",
        ],
    );

    for line in [
        "modus       = 'K'",
        "no_prog     = 'X'",
        "no_exec     = 'X'",
        "incl_align     = 'X'",
        "status    = 'A'",
        "get_all   = 'X'",
        "OR ( ev_modeflag = 'F' AND action_allowed( ev_action ) = abap_true )",
    ] {
        assert!(core.contains(line), "decide_core: {line}");
    }

    assert!(body(&all, "expanded").contains("mode              = 1"));

    // The allowed action comes from TADBM for the system name CLN on a customer system.
    let allowed = body(&all, "action_allowed");

    assert_order(
        &allowed,
        &["'TR_SYS_PARAMS'", "'CLN'", "FROM tadbm", "uname = '*'"],
    );

    // The row count spans all clients and is minus one, never a silent zero, when it fails.
    let rows = body(&all, "count_rows");

    assert!(rows.contains("CLIENT SPECIFIED"));
    assert!(rows.contains("rv_rows = -1"));

    for caller in ["decide_table", "decide_dependent", "check_delete"] {
        assert!(
            body(&all, caller).contains("< 0"),
            "{caller} must refuse an unavailable row count"
        );
    }
}

#[test]
fn the_transitive_check_reaches_tables_through_data_elements_domains_and_structures() {
    let all = text("DTEL");
    let dependents = body(&all, "check_dependents");

    assert_order(
        &dependents,
        &["users( )", "is_transp = abap_true", "decide_dependent("],
    );

    for case in ["WHEN `DTEL`", "WHEN `DOMA`", "WHEN `TABL`"] {
        assert!(dependents.contains(case), "{case}");
    }

    assert!(dependents.contains("field change rejected"));

    // A table type has no dependent transparent table to assess.
    assert_order(&dependents, &["mv_type = `TTYP`", "RETURN"]);

    let check = body(&all, "check_changes");

    assert_order(
        &check,
        &[
            "tabclass_changed( )",
            "decide_table(",
            "check_dependents( )",
            "ROLLBACK WORK",
        ],
    );
}

#[test]
fn the_table_class_rules_are_applied_to_the_file_and_to_the_live_definition() {
    let all = text("TABL");
    let check = body(&all, "check_tabclass");

    assert!(check.contains("iv_class <> 'TRANSP' AND iv_class <> 'INTTAB'"));
    assert!(body(&all, "parse_file").contains("check_tabclass( ms_file-dd02v-tabclass )"));
    assert!(body(&all, "do_deploy").contains("check_tabclass( live_class( ) )"));
    assert!(body(&all, "do_delete").contains("check_tabclass( live_class( ) )"));
    assert!(!body(&all, "do_read").contains("check_tabclass"));

    // A transparent name over 16 characters is refused in the create run only.
    let create = body(&all, "do_create");

    assert_order(&create, &["'TRANSP' AND strlen( mv_name ) > 16", "reject("]);
    assert!(!body(&all, "do_deploy").contains("> 16"));
}

#[test]
fn the_data_element_texts_are_checked_against_their_lengths_before_any_put() {
    let all = text("DTEL");
    let texts = body(&all, "check_dtel_texts");

    for (text_field, length_field) in [
        ("scrtext_s", "scrlen1"),
        ("scrtext_m", "scrlen2"),
        ("scrtext_l", "scrlen3"),
        ("reptext", "headlen"),
    ] {
        assert!(
            texts.contains(&format!(
                "strlen( ms_file-dd04v-{text_field} ) > ms_file-dd04v-{length_field}"
            )),
            "{text_field}"
        );
    }

    assert!(body(&all, "parse_file").contains("check_dtel_texts( )"));

    // Parse (with the check) comes before the write in both writing handlers, and the PUT has
    // one caller.
    for handler in ["do_deploy", "do_create"] {
        assert_order(
            &body(&all, handler),
            &["parse_file( )", "write_definition("],
        );
    }

    assert_eq!(all.matches("put_definition( )").count(), 1);
    assert_order(&body(&all, "write_definition"), &["put_definition( )"]);
}

// ---- the order of a run -------------------------------------------------------------------------

#[test]
fn deploy_takes_the_lock_before_the_base_check_and_reads_the_base_again_before_the_write() {
    let all = text("DOMA");
    let deploy = body(&all, "do_deploy");

    assert_order(
        &deploy,
        &[
            "read_live( )",
            "object_package( )",
            "check_inactive( )",
            "mv_mode = `DEPLOY` AND lock_object( )",
            "check_base( lv_live_hash )",
            "parse_file( )",
            "rows_hash( lt_file ) = lv_live_hash",
            "check_transport(",
            "check_changes( )",
            "mv_mode = `SIMULATE`",
            "finish( `SIMULATED_OK` )",
            "read_live( )",
            "mv_live_hash <> lv_live_hash",
            "write_definition( abap_false )",
        ],
    );

    // Simulate prints the live rows; deploy does not.
    assert!(deploy.contains("mv_mode = `SIMULATE` AND emit_rows( )"));
    assert!(deploy.contains("finish( `NO_CHANGES` )"));
}

#[test]
fn the_write_records_puts_activates_and_proves_in_that_order() {
    let all = text("TTYP");
    let write = body(&all, "write_definition");

    assert_order(
        &write,
        &[
            "record_object(",
            "put_definition( )",
            "activate_definition( )",
            "verify_definition( )",
            "iv_key = `RESULT_HASH`",
            "finish( `ACTIVE` )",
        ],
    );

    // An existing local object is rewritten without a request entry; a create always records.
    assert!(write.contains("mv_local = abap_false OR iv_create = abap_true"));
    // A failed activation or proof leaves the inactive version and says so.
    assert!(write.matches("finish( `INACTIVE` )").count() >= 3);
    assert_order(
        &write,
        &[
            "iv_create = abap_true AND exists_object( abap_false )",
            "finish( `INACTIVE` )",
            "finish( `REJECTED` )",
        ],
    );
}

#[test]
fn the_proof_compares_every_part_of_the_definition_like_the_python_subset() {
    let all = text("TABL");
    let verify = body(&all, "verify_definition");

    assert_order(
        &verify,
        &[
            "canonicalize( CHANGING cs_def = ms_file )",
            "read_live( )",
            "subset_table(",
            "subset_struct(",
            "FROM tbatg",
            "count_inactive(",
        ],
    );

    for label in [
        "DD04V", "DD01V", "DD07V", "DD02V", "DD09L", "DD03P", "DD05M", "DD08V", "DD35V", "DD36M",
        "DD40V", "DD42V", "DD43V",
    ] {
        assert!(verify.contains(&format!("`{label}`")), "{label}");
    }

    // Only the attributes SAP derives itself may be filled when the file left them empty.
    let subset = body(&all, "subset_struct");

    for field in [
        "ROWORCOLST",
        "INTTYPE",
        "INTLEN",
        "OUTPUTLEN",
        "TYPELEN",
        "DOMNAME3L",
        "AS4LOCAL",
        "AS4VERS",
        "DDLANGUAGE",
    ] {
        assert!(subset.contains(&format!("`{field}`")), "{field}");
    }

    assert!(body(&all, "subset_table").contains("lines( it_file ) <> lines( it_active )"));
}

#[test]
fn a_master_language_left_empty_in_the_file_is_not_a_difference() {
    // SAP fills the original language into DD04V-DTELMASTER, DD01V-DOMMASTER, and
    // DD02V-MASTERLANG; a file written by hand never carries it. A value in the file must match.
    let subset = body(&text("DTEL"), "subset_struct");

    for field in ["DTELMASTER", "DOMMASTER", "MASTERLANG"] {
        assert!(subset.contains(&format!("`{field}`")), "{field}");
    }

    // Equal components pass first; an empty file component passes only when it is on the derived
    // list; anything else is the first difference.
    assert_order(
        &subset,
        &[
            "IF <lv_file> = <lv_active>.",
            "IF <lv_file> IS INITIAL AND line_exists( lt_derived[ table_line = ls_comp-name ] ).",
            "ev_diff =",
            "RETURN.",
        ],
    );
}

#[test]
fn a_failed_proof_names_the_first_differing_component_with_both_values() {
    let all = text("DTEL");
    let subset = body(&all, "subset_struct");

    assert!(subset.contains("lo_struct->components"));
    assert!(subset.contains("|{ ls_comp-name } file='{ <lv_file> }' active='{ <lv_active> }'|"));
    assert!(body(&all, "subset_table").contains("|row { lv_index }: { lv_diff }|"));

    let verify = body(&all, "verify_definition");

    assert!(verify.contains("ev_diff = lv_diff"));
    assert!(verify.contains("after activation ({ lt_labels[ lv_index ] }): { lv_diff }|"));
}

#[test]
fn create_checks_the_name_the_package_and_the_request_before_it_writes() {
    let all = text("DOMA");
    let create = body(&all, "do_create");

    assert_order(
        &create,
        &[
            "create_package( )",
            "exists_object( abap_true )",
            "check_transport(",
            "parse_file( )",
            "mode = `CREATE_SIM`",
            "finish( `SIMULATED_OK` )",
            "write_definition( abap_true )",
        ],
    );

    let package = body(&all, "create_package");

    assert_order(&package, &["`$TMP`", "FROM tdevc", "is not $, Z, or Y"]);
    assert!(body(&all, "write_definition").contains("`I`"));
}

#[test]
fn delete_checks_rows_use_and_order_then_the_plan_hash_then_deletes() {
    let all = text("TABL");
    let delete = body(&all, "do_delete");

    assert_order(
        &delete,
        &[
            "exists_object( abap_false )",
            "FROM tadir",
            "check_tabclass(",
            "check_delete( )",
            "delete_request( )",
            "read_live( )",
            "mv_mode = `DELETE_SIM`",
            "finish( `SIMULATED_OK` )",
            "mv_plan <> lv_hash",
            "Plan changed since simulate; simulate again",
            "delete_definition( )",
        ],
    );

    // A definition without an active version hashes the single row NO_ACTIVE.
    assert!(delete.contains("`NO_ACTIVE`"));

    let check = body(&all, "check_delete");

    assert_order(
        &check,
        &[
            "contains { lv_rows } rows; delete rejected",
            "users( )",
            "Batch order: delete users",
            "still in use",
        ],
    );
    assert!(check.contains("mt_batch[ table_line = lv_key ]"));

    let remove = body(&all, "delete_definition");

    assert_order(
        &remove,
        &[
            "'RS_DD_DELETE_OBJ'",
            "no_ask               = 'X'",
            "COMMIT WORK AND WAIT",
            "exists_object( abap_false )",
            "finish( `DELETED` )",
        ],
    );
    assert!(remove.contains("WHEN `TTYP` THEN 'A'"));
    assert!(remove.contains("WHEN `DOMA` THEN 'D'"));
    assert!(remove.contains("WHEN `DTEL` THEN 'E'"));
    assert!(remove.contains("WHEN `TABL` THEN 'T'"));
}

#[test]
fn a_delete_follows_the_request_that_holds_the_lock_and_refuses_another_users() {
    let request = body(&text("DTEL"), "delete_request");

    assert_order(
        &request,
        &[
            "mv_local = abap_true",
            "FROM e071",
            "lockflag = 'X'",
            "WHEN 0.",
            "check_transport(",
            "WHEN 1.",
            "is locked in request",
            "mv_tr = lv_request",
            "WHEN OTHERS.",
            "more than one request",
        ],
    );
}

// ---- the file format ---------------------------------------------------------------------------

#[test]
fn the_xml_is_the_python_envelope_with_the_python_element_names() {
    let all = text("TABL");
    let serialize = body(&all, "serialize");

    for line in [
        "`<?xml version=\"1.0\" encoding=\"utf-8\"?>`",
        "serializer=\"LCL_OBJECT_{ mv_type }\" serializer_version=\"v1.0.0\"",
        "initial_components = 'suppress'",
        "SOURCE dd04v = is_def-dd04v",
        "dd07v_tab = is_def-dd07v",
        "dd02v = is_def-dd02v dd09l = is_def-dd09l dd03p_table = is_def-dd03p",
        "dd05m_table = is_def-dd05m dd08v_table = is_def-dd08v",
        "dd35v_tale = is_def-dd35v dd36m = is_def-dd36m",
        "SOURCE dd40v = is_def-dd40v dd42v = is_def-dd42v dd43v = is_def-dd43v",
        "`</abapGit>`",
    ] {
        assert!(serialize.contains(line), "serialize: {line}");
    }

    assert!(
        !serialize.contains("dd35v_table"),
        "the Python spelling stays"
    );

    let parse = body(&all, "parse_file");

    for line in [
        "dd35v_tale = ms_file-dd35v dd36m = ms_file-dd36m",
        "SOURCE XML lv_xml",
        "`<asx:abap`",
        "`</asx:abap>`",
    ] {
        assert!(parse.contains(line), "parse_file: {line}");
    }
}

#[test]
fn the_hash_is_the_sha256_the_client_computes_over_the_normalized_rows() {
    let all = text("DTEL");
    let hash = body(&all, "rows_hash");

    assert_order(
        &hash,
        &[
            "sep = lv_lf",
            "encoding = 'UTF-8'",
            "'SHA256'",
            "rv_hash = to_lower( lv_hash )",
        ],
    );
    assert!(hash.contains("lv_lf = |\\n|"));

    let normalize = body(&all, "normalize_rows");

    assert!(normalize.contains("regex = ` +$`"));
    assert!(normalize.contains("DELETE ct_rows INDEX"));

    // The rows go out one per line and are never wider than the client accepts.
    let rows = body(&all, "emit_rows");

    assert_order(&rows, &["strlen( lv_row ) > 255", "data_line( lv_row )"]);
}

#[test]
fn a_read_names_no_guard_a_write_would() {
    // Read accepts a SAP name and any table class: it has no TADIR, no request, and no class check.
    let read = body(&text("DOMA"), "do_read");

    for guard in [
        "check_tabclass",
        "check_transport",
        "lock_object",
        "object_package",
        "record_object",
    ] {
        assert!(!read.contains(guard), "{guard}");
    }
}

#[test]
fn the_pretty_printer_reads_the_next_tag_only_after_it_knows_there_is_one() {
    // Live defect on release 753: a table expression after AND ended the method in an internal
    // error on the last tag. The next tag is read inside an IF that has already proven it exists.
    let pretty = body(&text("DTEL"), "pretty");

    assert!(
        !pretty.contains("AND substring("),
        "no function call after AND"
    );
    assert!(!pretty.contains("lt_tags[ lv_index + 1 ]-offset len = 2 ) = `</`."));
    assert_order(
        &pretty,
        &[
            "IF lv_index < lv_count.",
            "lt_tags[ lv_index + 1 ]-offset",
            "IF lv_next = `</`.",
        ],
    );
}
