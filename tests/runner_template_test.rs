//! The generated runner class: what the generator may emit, the pinned template hash, and the
//! safety scanner. The ABAP cannot be compiled offline; compile and activation on 740 and 750 are
//! proven live. These tests pin everything that can be checked without a SAP system.

use abapreach::operation::payload::{Kind, Mode, Request, encode};
use abapreach::operation::runner_scan::{ALLOWED_CLASSES, ALLOWED_FUNCTIONS, scan};
use abapreach::operation::runner_template::{
    CORE_ONLY, NAME_PLACEHOLDER, TEMPLATES, TypeTemplate, generate, template_hash, template_lines,
};
use abapreach::runner_protocol::{RunMode, RunStatus};

const NAME: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const OTHER_NAME: &str = "ZRUN_Z9Y8X7W6";
const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// One pinned hash per shipped template: adding or changing a template line is a deliberate act.
const PINNED: [(&str, &str); 6] = [
    (
        "CORE",
        "5b184ab33877ae31b034c65c07cf2070abb7b897be82dfa57c0cba258fff2fab",
    ),
    (
        "TRAN",
        "8a2cceaac3eee61a8e17011e78d07823cc72ce2516cd0d3d307bf6478e835867",
    ),
    (
        "DTEL",
        "60a90434c26c14489b10f497affa9a86f06d601d00e4482d2d06011a2241c070",
    ),
    (
        "DOMA",
        "60a90434c26c14489b10f497affa9a86f06d601d00e4482d2d06011a2241c070",
    ),
    (
        "TABL",
        "60a90434c26c14489b10f497affa9a86f06d601d00e4482d2d06011a2241c070",
    ),
    (
        "TTYP",
        "60a90434c26c14489b10f497affa9a86f06d601d00e4482d2d06011a2241c070",
    ),
];

fn payload() -> Vec<String> {
    vec![
        "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=".to_owned(),
        "ZZZZ+/09".to_owned(),
    ]
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn template(declarations: &str, methods: &str) -> TypeTemplate {
    TypeTemplate {
        name: "TEST",
        declarations: leak(declarations.to_owned()),
        methods: leak(methods.to_owned()),
    }
}

fn payload_line_shape(line: &str) -> Option<&str> {
    line.strip_prefix("    lv_b64 = lv_b64 && '")?
        .strip_suffix("'.")
}

fn source(template: &TypeTemplate) -> String {
    generate(template, NAME, &payload()).expect("generated")
}

// ---- the generator ------------------------------------------------------------------------------

#[test]
fn the_generator_emits_only_template_lines_and_the_payload_lines() {
    for template in TEMPLATES {
        let output = source(template);
        let constant = template_lines(template);
        let mut payload_seen = Vec::new();
        let mut rest = Vec::new();

        for line in output.lines() {
            match payload_line_shape(line) {
                Some(literal) => payload_seen.push(literal.to_owned()),
                None => rest.push(line.replace(NAME, NAME_PLACEHOLDER)),
            }
        }

        assert_eq!(payload_seen, payload(), "{}", template.name);
        assert_eq!(
            rest, constant,
            "{}: only template lines remain",
            template.name
        );
    }
}

#[test]
fn the_same_inputs_give_identical_output() {
    for template in TEMPLATES {
        assert_eq!(source(template), source(template));
    }
}

#[test]
fn only_the_two_name_lines_depend_on_the_name() {
    for template in TEMPLATES {
        let first = generate(template, NAME, &payload()).unwrap();
        let second = generate(template, OTHER_NAME, &payload()).unwrap();
        let (first, second): (Vec<_>, Vec<_>) = (first.lines().collect(), second.lines().collect());

        assert_eq!(first.len(), second.len());

        let changed: Vec<usize> = (0..first.len())
            .filter(|&i| first[i] != second[i])
            .collect();

        assert_eq!(changed.len(), 2, "{}: {changed:?}", template.name);
        assert_eq!(first[changed[0]], format!("CLASS {NAME} DEFINITION"));
        assert_eq!(second[changed[0]], format!("CLASS {OTHER_NAME} DEFINITION"));
        assert_eq!(first[changed[1]], format!("CLASS {NAME} IMPLEMENTATION."));
        assert_eq!(
            second[changed[1]],
            format!("CLASS {OTHER_NAME} IMPLEMENTATION.")
        );
        assert!(changed[0] < changed[1]);
    }
}

#[test]
fn a_name_that_could_carry_code_is_refused() {
    let long = format!("Y{}", "A".repeat(30));
    let bad = [
        "",
        "yclrun",
        "ACL_RUN",
        "YCL RUN",
        "YCL_RUN\n",
        "YCL_RUN.",
        "YCL'RUN",
        "YCL|RUN",
        "YCL{RUN}",
        "YCL`RUN",
        "YCL\"RUN",
        "YCL*RUN",
        "YCL_RÜN",
        "YCL/RUN/X",
        long.as_str(),
    ];

    for name in bad {
        assert!(
            generate(&CORE_ONLY, name, &payload()).is_err(),
            "{name:?} was accepted"
        );
    }

    assert!(generate(&CORE_ONLY, &format!("Y{}", "A".repeat(29)), &payload()).is_ok());
    assert!(generate(&CORE_ONLY, "Z", &payload()).is_ok());
}

#[test]
fn a_payload_line_outside_the_safe_alphabet_is_refused() {
    let wide = "A".repeat(201);
    let bad = [
        "",
        "AB CD",
        "AB'CD",
        "AB\"CD",
        "AB\\CD",
        "AB{CD",
        "AB}CD",
        "AB`CD",
        "AB|CD",
        "AB\nCD",
        "AB\rCD",
        "AB.CD",
        "AB-CD",
        "AB_CD",
        "ABÄD",
        wide.as_str(),
    ];

    for line in bad {
        assert!(
            generate(&CORE_ONLY, NAME, &[line.to_owned()]).is_err(),
            "{line:?} was accepted"
        );
        assert!(
            generate(&CORE_ONLY, NAME, &["QUJD".to_owned(), line.to_owned()]).is_err(),
            "{line:?} was accepted after a good line"
        );
    }

    assert!(generate(&CORE_ONLY, NAME, &[]).is_err(), "no payload");
    assert!(generate(&CORE_ONLY, NAME, &["A".repeat(200)]).is_ok());
    assert!(generate(&CORE_ONLY, NAME, &["a1+/=".to_owned()]).is_ok());
}

#[test]
fn a_payload_line_holds_only_what_the_encoder_may_produce() {
    for mode in [Mode::Read, Mode::CreateSim, Mode::Delete] {
        let mut request = Request::new(mode, Kind::Tran, "YTRAN", NONCE);

        match mode {
            Mode::CreateSim => {
                request.report = Some("ZREPORT");
                request.text = Some("A transaction");
            }
            Mode::Delete => request.plan = Some(HASH),
            _ => {}
        }

        let lines = encode(&request).expect("encoded");
        let output = generate(&CORE_ONLY, NAME, &lines).expect("generated");
        let found: Vec<&str> = output.lines().filter_map(payload_line_shape).collect();

        assert_eq!(found, lines.iter().map(String::as_str).collect::<Vec<_>>());
    }
}

// ---- the pinned hash ---------------------------------------------------------------------------

#[test]
fn every_template_hash_is_pinned() {
    assert_eq!(PINNED.len(), TEMPLATES.len(), "pin each template");

    for (template, (name, hash)) in TEMPLATES.iter().zip(PINNED) {
        assert_eq!(template.name, name);
        assert_eq!(template_hash(template), hash, "{name}: a template changed");
    }
}

#[test]
fn the_hash_fails_when_a_template_line_changes() {
    let base = template("", "");
    let same = template("", "");
    let added_declaration = template("    DATA mv_x TYPE i.", "");
    let added_method = template("", "  METHOD run_type.\n  ENDMETHOD.");
    let edited = template("    DATA mv_x TYPE i.", "  METHOD run_type.\n  ENDMETHOD.");
    let edited_again = template(
        "    DATA mv_x TYPE i.",
        "  METHOD run_type.\n    CLEAR mv_x.\n  ENDMETHOD.",
    );
    let hashes = [
        template_hash(&base),
        template_hash(&added_declaration),
        template_hash(&added_method),
        template_hash(&edited),
        template_hash(&edited_again),
    ];

    assert_eq!(template_hash(&base), template_hash(&same));

    for (index, hash) in hashes.iter().enumerate() {
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert!(!hashes[..index].contains(hash), "template {index} collides");
    }
}

#[test]
fn the_hash_ignores_the_name_and_the_payload() {
    // The hash is computed from the constant lines, so neither the name nor the payload can reach it.
    for template in TEMPLATES {
        let lines = template_lines(template);

        assert!(
            lines
                .iter()
                .any(|l| l == &format!("CLASS {NAME_PLACEHOLDER} DEFINITION"))
        );
        assert!(lines.iter().all(|l| payload_line_shape(l).is_none()));
        assert!(lines.iter().all(|l| !l.contains(NAME)));
    }
}

// ---- the shape of the ABAP ------------------------------------------------------------------------

fn words(line: &str) -> Vec<&str> {
    line.trim()
        .trim_end_matches('.')
        .split_whitespace()
        .collect()
}

#[test]
fn declared_methods_are_implemented_and_the_class_is_well_formed() {
    for template in TEMPLATES {
        let lines = template_lines(template);
        let declared: Vec<&str> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with("METHODS "))
            .map(|l| words(l)[1])
            .collect();
        let implemented: Vec<&str> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with("METHOD "))
            .map(|l| words(l)[1])
            .filter(|name| *name != "if_oo_adt_classrun~main")
            .collect();
        let mut sorted_declared = declared.clone();
        let mut sorted_implemented = implemented.clone();

        sorted_declared.sort_unstable();
        sorted_implemented.sort_unstable();

        assert!(declared.contains(&"run_type"), "{}", template.name);
        assert_eq!(sorted_declared, sorted_implemented, "{}", template.name);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.trim_start().starts_with("METHOD "))
                .count(),
            lines.iter().filter(|l| l.trim() == "ENDMETHOD.").count()
        );
        assert_eq!(
            lines.iter().filter(|l| l.ends_with(" DEFINITION")).count(),
            1
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.ends_with(" IMPLEMENTATION."))
                .count(),
            1
        );
        assert_eq!(lines.iter().filter(|l| l.trim() == "ENDCLASS.").count(), 2);
        assert!(
            lines
                .iter()
                .all(|l| l.chars().count() <= 255 && !l.contains('\t'))
        );
        assert!(lines.iter().all(|l| l.is_ascii()), "ASCII only");
    }
}

#[test]
fn the_template_speaks_the_grammar_the_parser_reads() {
    for template in TEMPLATES {
        let text = template_lines(template).join("\n");

        for marker in [
            "ABAPREACH:{ mv_nonce }:",
            "BEGIN:{ mv_mode }",
            "MSG:{ iv_level }:",
            "LINE:{ hex_of",
            "FACT:{ iv_key }:",
            "PARTIAL_DELETE:{ hex_of",
            "OBJECT_STILL_EXISTS:{ hex_of",
            "STATUS:{ iv_status }",
            "END:{ mv_count }",
        ] {
            assert!(text.contains(marker), "{}: {marker}", template.name);
        }

        for mode in [
            RunMode::Read,
            RunMode::Simulate,
            RunMode::Deploy,
            RunMode::CreateSim,
            RunMode::Create,
            RunMode::DeleteSim,
            RunMode::Delete,
        ] {
            assert!(
                text.contains(&format!("`{}`", mode.token())),
                "{}",
                mode.token()
            );
        }

        for status in [RunStatus::Rejected, RunStatus::Active, RunStatus::Inactive] {
            assert!(
                text.contains(&format!("`{}`", status.token())),
                "{}",
                status.token()
            );
        }
    }
}

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

#[test]
fn every_payload_key_the_encoder_writes_is_known_to_the_template() {
    let xml = "<abapGit>\n <DD04V>\n  <ROLLNAME>YDTEL</ROLLNAME>\n  <DDTEXT>text</DDTEXT>\n </DD04V>\n</abapGit>\n";
    let batch = [("DTEL", "YDTEL"), ("TRAN", "YTRAN")];
    let mut keys = Vec::new();

    for mode in [
        Mode::Read,
        Mode::Simulate,
        Mode::Deploy,
        Mode::CreateSim,
        Mode::Create,
        Mode::DeleteSim,
        Mode::Delete,
    ] {
        let mut request = Request::new(mode, Kind::Dtel, "YDTEL", NONCE);

        request.package = Some("ZPACKAGE");
        request.tr = Some("DEVK900001");
        request.text = Some("text");
        request.base = Some(HASH);
        request.plan = Some(HASH);
        request.xml = Some(xml);
        request.batch = &batch;

        let applies = [
            ("package", matches!(mode, Mode::CreateSim | Mode::Create)),
            ("tr", mode != Mode::Read),
            ("base", matches!(mode, Mode::Simulate | Mode::Deploy)),
            ("plan", mode == Mode::Delete),
            (
                "xml",
                matches!(
                    mode,
                    Mode::Simulate | Mode::Deploy | Mode::CreateSim | Mode::Create
                ),
            ),
            ("batch", matches!(mode, Mode::DeleteSim | Mode::Delete)),
        ];

        for (field, applies) in applies {
            if !applies {
                match field {
                    "package" => request.package = None,
                    "tr" => request.tr = None,
                    "base" => request.base = None,
                    "plan" => request.plan = None,
                    "xml" => request.xml = None,
                    _ => request.batch = &[],
                }
            }
        }

        if !matches!(
            mode,
            Mode::Simulate | Mode::Deploy | Mode::CreateSim | Mode::Create
        ) {
            request.text = None;
        }

        let lines = encode(&request).unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        let document = String::from_utf8(base64_decode(&lines.concat())).unwrap();

        keys.extend(
            document
                .lines()
                .map(|l| l.split_once('=').unwrap().0.to_owned()),
        );
    }

    let mut request = Request::new(Mode::Create, Kind::Tran, "YTRAN", NONCE);

    request.report = Some("ZREPORT");
    request.text = Some("A transaction");
    keys.extend(
        String::from_utf8(base64_decode(&encode(&request).unwrap().concat()))
            .unwrap()
            .lines()
            .map(|l| l.split_once('=').unwrap().0.to_owned()),
    );
    keys.sort_unstable();
    keys.dedup();

    assert!(keys.len() >= 12, "{keys:?}");

    for template in TEMPLATES {
        let text = template_lines(template).join("\n");

        for key in &keys {
            assert!(
                text.contains(&format!("`{key}`")),
                "{}: key {key}",
                template.name
            );
        }
    }
}

// ---- the scanner ---------------------------------------------------------------------------------

fn rejected(code: &str, runner: Option<&str>) -> Vec<String> {
    scan(code, runner).expect_err(code)
}

#[test]
fn the_shipped_templates_pass_the_scanner() {
    for template in TEMPLATES {
        scan(&template_lines(template).join("\n"), None)
            .unwrap_or_else(|found| panic!("{}: {found:?}", template.name));
        scan(&source(template), Some(NAME))
            .unwrap_or_else(|found| panic!("{}: {found:?}", template.name));
    }
}

#[test]
fn the_scanner_accepts_every_allow_listed_function_and_class() {
    assert!(ALLOWED_FUNCTIONS.contains(&"RS_CORR_INSERT"));
    assert!(ALLOWED_FUNCTIONS.contains(&"DD_DECIDE"));
    assert!(ALLOWED_CLASSES.contains(&"CL_HTTP_UTILITY"));

    for function in ALLOWED_FUNCTIONS {
        scan(
            &format!("    CALL FUNCTION '{function}'\n      EXPORTING a = b."),
            None,
        )
        .unwrap();
        scan(
            &format!("call function '{}'", function.to_lowercase()),
            None,
        )
        .unwrap();
    }

    for class in ALLOWED_CLASSES {
        scan(&format!("x = {class}=>method( )."), None).unwrap();
        scan(&format!("DATA lo_x TYPE REF TO {class}."), None).unwrap();
        scan(&format!("CATCH {class} INTO DATA(lx)."), None).unwrap();
        scan(&format!("INTERFACES {class}."), None).unwrap();
        scan(&format!("x = NEW {class}( )."), None).unwrap();
    }
}

#[test]
fn the_scanner_rejects_a_function_module_that_is_not_allow_listed() {
    for code in [
        "CALL FUNCTION 'RFC_ABAP_INSTALL_AND_RUN'",
        "CALL FUNCTION 'RPY_PROGRAM_INSERT'\n  EXPORTING a = b.",
        "call function 'ddif_dtel_putx'",
        "CALL   FUNCTION\n   'EDITOR_SYNTAX_CHECK'",
    ] {
        let found = rejected(code, None);

        assert!(
            found.iter().any(|f| f.contains("CALL FUNCTION")),
            "{code}: {found:?}"
        );
    }

    // A call that is not a literal cannot be checked, and a destination makes it a remote call.
    for code in [
        "CALL FUNCTION lv_name",
        "CALL FUNCTION lv_name\n  EXPORTING a = b.",
        "CALL FUNCTION 'DD_DECIDE' DESTINATION 'NONE'",
        "CALL FUNCTION 'DD_DECIDE'\n  DESTINATION lv_dest",
    ] {
        assert!(!rejected(code, None).is_empty(), "{code}");
    }
}

#[test]
fn the_scanner_rejects_a_class_that_is_not_allow_listed() {
    for code in [
        "x = cl_abap_char_utilities=>newline.",
        "DATA lo_x TYPE REF TO cl_gui_frontend_services.",
        "lo_x = NEW cl_abap_matcher( ).",
        "TRY. x = 1. CATCH cx_sy_zerodivide INTO DATA(lx).",
        "TRY. x = 1. CATCH cx_root cx_sy_zerodivide INTO DATA(lx).",
        "INTERFACES if_serializable_object.",
        "CREATE OBJECT lo_x TYPE cl_gui_alv_grid.",
        "lo_x = CAST cl_abap_datadescr( lo_y ).",
        "RAISE EXCEPTION TYPE cx_sy_itab_error.",
        "CLASS lcl_x DEFINITION INHERITING FROM cl_abap_datadescr.",
        "x = cl_abap_char_utilities=>newline&&cl_http_utility=>decode_x_base64( y ).",
    ] {
        let found = rejected(code, None);

        assert!(
            found.iter().any(|f| f.contains("class")),
            "{code}: {found:?}"
        );
    }
}

#[test]
fn the_scanner_rejects_a_y_or_z_name_except_the_runner_class() {
    for code in [
        "x = zcl_helper=>run( ).",
        "CALL FUNCTION 'Z_MY_FM'",
        "CALL FUNCTION 'YFM_TOOL'",
        "SELECT * FROM ztable INTO TABLE @DATA(lt_x).",
        "DATA lv_x TYPE zdomain.",
        "SELECT * FROM /abc/ztable INTO TABLE @DATA(lt_x).",
        "lv_x = 'ZFOO'.",
        "lv_x = `yfoo`.",
        "\" a comment that names zthing",
        "* zthing",
        "lv_x = |text { zvar }|.",
        "yvar = 1.",
    ] {
        let found = rejected(code, None);

        assert!(found.iter().any(|f| f.contains("Y/Z")), "{code}: {found:?}");
    }

    // One letter is the range test of the name guard, not a name.
    scan(
        "IF lv_first <> `Z` AND lv_first <> `Y`.\n  x = 'Z'.\n  y = 'Y'.",
        None,
    )
    .unwrap();
}

#[test]
fn the_runner_class_name_is_allowed_only_as_itself_and_only_when_given() {
    let class = format!("CLASS {NAME} DEFINITION\n  PUBLIC.\nCLASS {NAME} IMPLEMENTATION.");

    scan(&class, Some(NAME)).unwrap();
    scan(&class.to_lowercase(), Some(NAME)).unwrap();

    assert!(!rejected(&class, None).is_empty(), "no runner name given");
    assert!(
        !rejected(&class, Some(OTHER_NAME)).is_empty(),
        "another runner name"
    );
    assert!(!rejected(&format!("{class}\nx = {NAME}_X."), Some(NAME)).is_empty());
    assert!(!rejected(&format!("{class}\nx = Y{NAME}."), Some(NAME)).is_empty());
    assert!(!rejected(&format!("{class}\nx = ZOTHER."), Some(NAME)).is_empty());
}

#[test]
fn the_scanner_skips_a_payload_line_and_only_that() {
    // Base64 holds words such as ZXN0; a line of exactly the payload shape is data, not code.
    scan("    lv_b64 = lv_b64 && 'ZXN0YWJj+/=='.", None).unwrap();

    for code in [
        "    lv_b64 = lv_b64 && 'ZXN0 YWJj'.",
        "    lv_b64 = lv_b64 && 'ZXN0YWJj'. zfoo = 1.",
        "  lv_b64 = lv_b64 && 'ZXN0YWJj'.",
        "    lv_b64 = zthing && 'ZXN0YWJj'.",
        "    lv_b64 = lv_b64 && 'ZXN0YWJj'",
    ] {
        assert!(!rejected(code, None).is_empty(), "{code}");
    }
}

/// A table expression that fails on a missing row raises inside a condition instead of ending
/// it: the kernel of release 753 does not short-circuit AND and OR around it (found live in the
/// DDIC pretty printer). Only line_exists and line_index take such an expression without raising.
const OLD_PRETTY_CONDITION: &str = "        IF lv_index < lv_count\n           AND substring( val = iv_xml off = lt_tags[ lv_index + 1 ]-offset len = 2 ) = `</`.\n        ENDIF.";

#[test]
fn the_scanner_rejects_a_table_expression_in_a_condition_with_and_or_or() {
    for code in [
        OLD_PRETTY_CONDITION,
        "IF lv_a = 1 AND lt_x[ 1 ]-f = 2.",
        "IF lt_x[ lv_i ]-f = 2 OR lv_a = 1.",
        "IF lv_a = 1 and lt_x[ f = lv_b ]-g IS NOT INITIAL.",
        "lv_ok = xsdbool( lv_a = 1 OR lt_x[ 1 ] IS INITIAL ).",
        "IF line_exists( lt_x[ f = lt_y[ 1 ]-g ] ) AND lv_a = 1.",
        "WHILE lv_i < lv_n AND lt_x[ lv_i ] IS NOT INITIAL.",
    ] {
        let found = rejected(code, None);

        assert!(
            found.iter().any(|f| f.contains("table expression")),
            "{code}: {found:?}"
        );
    }
}

#[test]
fn the_scanner_accepts_a_table_expression_that_cannot_raise_or_has_no_logic_around_it() {
    for code in [
        "IF line_exists( lt_x[ f = 1 ] ) AND lv_a = 1.",
        "IF NOT line_exists( lt_x[ f = 1 ] ) OR lv_a = 1.",
        "IF lv_a = line_index( lt_x[ table_line = lv_k ] ) AND lv_b = 2.",
        "IF lt_x[ 1 ]-f = 2.",
        "DATA(lv_r) = lt_x[ 1 ]-f.",
        "lv_a = lt_x[ lv_i ]-f + 1.",
        "IF lv_i < lv_n.\n  IF lt_x[ lv_i ]-f = 2.\n  ENDIF.\nENDIF.",
        "IF lv_a = 1 AND lv_b = 2.\n  lv_c = lt_x[ 1 ]-f.\nENDIF.",
        "lv_x = `a AND b[ 1 ]`.",
    ] {
        scan(code, None).unwrap_or_else(|found| panic!("{code}: {found:?}"));
    }
}

#[test]
fn the_scanner_reports_every_finding_with_its_line() {
    let found = rejected(
        "x = 1.\nCALL FUNCTION 'BAD_ONE'\nx = cl_bad=>y.\nzfoo = 2.",
        None,
    );

    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found[0].contains("line 2"), "{found:?}");
    assert!(found[1].contains("line 3"), "{found:?}");
    assert!(found[2].contains("line 4"), "{found:?}");
}

#[test]
fn the_pinned_templates_stay_inside_the_allow_list() {
    // The allow-list itself holds only standard SAP names.
    for name in ALLOWED_FUNCTIONS.iter().chain(ALLOWED_CLASSES) {
        assert!(
            !name.starts_with(['Y', 'Z'])
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
            "{name}"
        );
    }
}

// SAP's own rule (TR_SYS_GET_EU_LOCK_STATUS): the system is locked only when EDTFLAG is 'N'.
#[test]
fn the_system_change_option_guard_rejects_only_n() {
    let lines = template_lines(&CORE_ONLY);
    let guard = lines
        .iter()
        .position(|l| l.contains("lv_sysedit") && l.trim_start().starts_with("IF "))
        .expect("the EDTFLAG guard");

    assert_eq!(lines[guard].trim(), "IF lv_sysedit = 'N'.");
    assert!(lines[guard + 1].contains("The system change option is not modifiable (EDTFLAG="));
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("lv_sysedit IS NOT INITIAL")),
        "a value other than N must not be rejected"
    );
}
