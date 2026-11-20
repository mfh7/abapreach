//! The generated runner class: what the generator may emit, the pinned template hash, and the
//! safety scanner. The ABAP cannot be compiled offline; compile and activation on 740 and 750 are
//! proven live. These tests pin everything that can be checked without a SAP system.

use abapreach::operation::payload::{Kind, Mode, Request, encode};
use abapreach::operation::runner_scan::{
    ALLOWED_CLASSES, ALLOWED_FUNCTIONS, ScanMode, scan, scan_as,
};
use abapreach::operation::runner_template::{
    CORE_ONLY, NAME_PLACEHOLDER, OutType, TEMPLATES, TypeTemplate, generate, generate_for,
    template_hash, template_lines,
};
use abapreach::runner_protocol::{RunMode, RunStatus};

const NAME: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const OTHER_NAME: &str = "ZRUN_Z9Y8X7W6";
const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// One pinned hash per shipped template: adding or changing a template line is a deliberate act.
const PINNED: [(&str, &str); 8] = [
    (
        "CORE",
        "c46de5889e8dad7a4270b711f64ef2329e32dee85f8ee2e352ffa9e8c076cd2b",
    ),
    (
        "TRAN",
        "c1bac263713b53f50ace5e500d4b0b4ecac0fc1c7e5f255bbf021d91888379f9",
    ),
    (
        "DTEL",
        "3c7f649a2a679b37678f51005e117130da31617268f53072f184bbdaa55c2ae9",
    ),
    (
        "DOMA",
        "3c7f649a2a679b37678f51005e117130da31617268f53072f184bbdaa55c2ae9",
    ),
    (
        "TABL",
        "3c7f649a2a679b37678f51005e117130da31617268f53072f184bbdaa55c2ae9",
    ),
    (
        "TTYP",
        "3c7f649a2a679b37678f51005e117130da31617268f53072f184bbdaa55c2ae9",
    ),
    (
        "RUN_PROG",
        "8f7d6b09910d8208d283a9e23510c609639b45f4a96247ba55035d26163e457f",
    ),
    (
        "SNIPPET",
        "fe6c7da81eaf2435d16d8a701fd7bfb79b8a3cc86a522cd3b237aa03fe4c6da9",
    ),
];

/// The generated source of every template for `NAME` and `payload()`. The 0.16.0 values moved with
/// 0.17.0, which added the run-mode lines to the shared core.
const GENERATED: [(&str, &str); 8] = [
    (
        "CORE",
        "ef83dd89a1a1d9bdb231f285ac537bd5e3081900ac29fb576ab59df665432dd5",
    ),
    (
        "TRAN",
        "81e6fb90b96fdff13282b894fd95624712a08939102e6f74113fefb801797995",
    ),
    (
        "DTEL",
        "691514f0848d7334a08d42f406f2a6570aabc4e6a0920b1a9be85a2be8c4fe93",
    ),
    (
        "DOMA",
        "691514f0848d7334a08d42f406f2a6570aabc4e6a0920b1a9be85a2be8c4fe93",
    ),
    (
        "TABL",
        "691514f0848d7334a08d42f406f2a6570aabc4e6a0920b1a9be85a2be8c4fe93",
    ),
    (
        "TTYP",
        "691514f0848d7334a08d42f406f2a6570aabc4e6a0920b1a9be85a2be8c4fe93",
    ),
    (
        "RUN_PROG",
        "bf9414aa0f9730cbf908af096fd9d4e307c7a534be5200fe6e2965d10d05736a",
    ),
    (
        "SNIPPET",
        "e17b8c2eb438cffa84bafef955a31eb6744c35ff3778bfa2bc91f46b3e80d1cd",
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

/// The scan mode of a template: only `RUN_PROG` has the generated `SUBMIT` and its names.
fn mode_of(template: &TypeTemplate) -> ScanMode {
    if template.name == "RUN_PROG" {
        ScanMode::RunProg
    } else {
        ScanMode::Strict
    }
}

fn rejected(code: &str, runner: Option<&str>) -> Vec<String> {
    scan(code, runner).expect_err(code)
}

#[test]
fn the_shipped_templates_pass_the_scanner() {
    for template in TEMPLATES {
        let mode = mode_of(template);

        scan_as(&template_lines(template).join("\n"), None, mode)
            .unwrap_or_else(|found| panic!("{}: {found:?}", template.name));
        scan_as(&source(template), Some(NAME), mode)
            .unwrap_or_else(|found| panic!("{}: {found:?}", template.name));
    }
}

#[test]
fn the_run_prog_template_reads_the_selection_screen_through_field_info() {
    let template = TEMPLATES
        .iter()
        .find(|template| template.name == "RUN_PROG")
        .unwrap();
    let text = template_lines(template).join("\n").to_lowercase();

    assert!(!text.contains("fupararef"));
    assert!(text.contains("call function 'rs_reportselections_info'"));
    assert!(text.contains("field_info          = lt_info"));
    assert!(text.contains("no_selections       = 1"));
    assert!(text.contains("type standard table of rsel_info"));
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
fn the_scanner_rejects_statements_that_run_other_code_or_change_the_database() {
    for code in [
        "SUBMIT lv_rep AND RETURN.",
        "submit (lv_name).",
        "EXEC SQL.\n  DELETE FROM t000\nENDEXEC.",
        "CALL 'C_SAPGPARAM' ID 'NAME' FIELD lv_n.",
        "GENERATE SUBROUTINE POOL lt_src NAME lv_pool.",
        "INSERT REPORT lv_name FROM lt_src.",
        "CALL TRANSACTION 'SE38'.",
        "CALL METHOD (lv_class)=>(lv_method).",
        "CALL METHOD (lv_method).",
        "lo_x->(lv_method)( ).",
        "lo_x->(lv_method).",
        "cl_abap_typedescr=>(lv_method)( ).",
        "INSERT t000 FROM ls_row.",
        "INSERT t000 FROM TABLE lt_rows.",
        "INSERT INTO t000 VALUES ls_row.",
        "INSERT (lv_table) FROM ls_row.",
        "UPDATE t000 SET cccategory = 'C'.",
        "x = 1.\nUPDATE t000 FROM ls_row.",
        "MODIFY t000 FROM ls_row.",
        "MODIFY t000 FROM TABLE lt_rows.",
        "DELETE FROM t000 WHERE mandt = '001'.",
        "DELETE t000 FROM ls_row.",
        "DELETE t000 FROM TABLE lt_rows.",
        "DELETE (lv_table) FROM ls_row.",
        "\" a comment\nUPDATE t000 SET x = 1.",
    ] {
        let found = rejected(code, None);

        assert!(
            found
                .iter()
                .all(|f| f.contains("is not allowed") && !f.contains("Y/Z")),
            "{code}: {found:?}"
        );
    }
}

#[test]
fn the_scanner_accepts_internal_table_statements_and_words_in_comments_and_literals() {
    for code in [
        "INSERT ls_row INTO TABLE lt_rows.",
        "INSERT VALUE #( a = 1 ) INTO TABLE lt_rows.",
        "INSERT ls_row INTO lt_rows INDEX 1.",
        "INSERT LINES OF lt_a FROM 2 TO 3 INTO TABLE lt_b.",
        "DELETE lt_rows INDEX lv_i.",
        "DELETE lt_rows WHERE a = 1.",
        "DELETE cs_def-dd03p WHERE adminfield <> '0'.",
        "DELETE TABLE lt_rows FROM ls_row.",
        "DELETE ADJACENT DUPLICATES FROM lt_rows.",
        "x = lt_rows[ 1 ].\n\" Update the row, then modify it and submit.",
        "* Delete from the database is never done here.",
        "lv_mode = 'MODIFY'. lv_text = `UPDATE t000 SET x = 1`.",
        "lo_x->method( ).",
        "CALL FUNCTION 'DD_DECIDE'.",
        "mv_transaction = 'SE11'. lv_report = 1.",
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

// ---- cargo-mutants triage (M6 Task 33): scanner behaviour that no test pinned -------------------

#[test]
fn a_call_off_the_allow_list_names_the_function_and_a_computed_call_says_why() {
    assert_eq!(
        rejected("CALL FUNCTION 'RFC_ABAP_INSTALL_AND_RUN'", None),
        ["line 1: CALL FUNCTION 'RFC_ABAP_INSTALL_AND_RUN' is not on the allow-list"]
    );
    assert_eq!(
        rejected("CALL FUNCTION lv_name", None),
        ["line 1: CALL FUNCTION needs a literal function name"]
    );
}

#[test]
fn a_keyword_pattern_names_a_class_only_with_its_second_keyword() {
    for code in [
        // REF, INHERITING, and CREATE name a class only after TO, FROM, and OBJECT.
        "lr = REF lt_x lt_y.",
        "x = INHERITING foo bar.",
        "CREATE DATA lr TYPE string.",
        // A TYPE after the end of the statement belongs to another statement.
        "CREATE OBJECT lo_x.\nDATA lv_x TYPE string.",
        // BEFORE UNWIND are keywords of CATCH, not classes.
        "TRY. x = 1. CATCH BEFORE UNWIND cx_root INTO DATA(lx). ENDTRY.",
        // The class is the word after TYPE, and an allow-listed one passes.
        "CREATE OBJECT lo_x TYPE cx_root.",
    ] {
        scan(code, None).unwrap_or_else(|found| panic!("{code}: {found:?}"));
    }
}

#[test]
fn a_class_after_type_is_found_wherever_the_statement_stands() {
    let found = rejected("lv_a = 1. CREATE OBJECT lo_x TYPE cl_gui_alv_grid.", None);

    assert_eq!(
        found,
        ["line 1: class cl_gui_alv_grid is not on the allow-list"]
    );
}

#[test]
fn a_name_right_after_an_arrow_is_scanned() {
    let found = rejected("CL_HTTP_UTILITY=>zfoo( ).", None);

    assert_eq!(found, ["line 1: Y/Z name zfoo is not allowed"]);
}

#[test]
fn a_statement_may_start_with_a_bracket_inside_logic() {
    scan("[ lv_a AND lv_b ].", None).unwrap();
}

// ---- the type of the classrun OUT parameter (issue #12) -------------------------------------------

#[test]
fn only_the_two_known_out_types_are_accepted_and_each_names_its_interface() {
    assert_eq!(
        OutType::from_found("IF_OO_ADT_INTRNL_CLASSRUN"),
        Ok(OutType::Internal)
    );
    assert_eq!(
        OutType::from_found("IF_OO_ADT_CLASSRUN_OUT"),
        Ok(OutType::Out)
    );
    assert_eq!(OutType::Internal.interface(), "IF_OO_ADT_INTRNL_CLASSRUN");
    assert_eq!(OutType::Out.interface(), "IF_OO_ADT_CLASSRUN_OUT");

    for other in [
        "",
        " ",
        "IF_OO_ADT_CLASSRUN",
        "if_oo_adt_classrun_out",
        "ZIF_X",
    ] {
        let error = OutType::from_found(other).unwrap_err();

        assert!(error.contains(&format!("{other:?}")), "{error}");
    }
}

#[test]
fn the_default_generation_is_the_internal_interface_and_stays_byte_identical() {
    for template in TEMPLATES {
        assert_eq!(
            generate_for(template, NAME, &payload(), OutType::Internal).unwrap(),
            source(template),
            "{}",
            template.name
        );
        assert!(
            source(template).contains("mo_out     TYPE REF TO if_oo_adt_intrnl_classrun,"),
            "{}",
            template.name
        );
    }
}

/// The SHA-256 of the generated source of every template for `NAME` and `payload()`, as 0.16.0
/// generated it: a change of the default path, not only of a template line, changes one of them.
#[test]
fn the_generated_source_of_the_default_path_is_pinned() {
    use sha2::{Digest, Sha256};

    for (name, hash) in GENERATED {
        let template = TEMPLATES
            .iter()
            .find(|template| template.name == name)
            .unwrap();
        let digest = Sha256::digest(source(template).as_bytes());
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();

        assert_eq!(hex, hash, "{name}");
    }
}

#[test]
fn the_out_type_changes_exactly_the_one_declaration_line_and_still_scans() {
    for template in TEMPLATES {
        let internal = source(template);
        let out = generate_for(template, NAME, &payload(), OutType::Out).unwrap();
        let (internal, out): (Vec<_>, Vec<_>) = (internal.lines().collect(), out.lines().collect());

        assert_eq!(internal.len(), out.len());

        let changed: Vec<usize> = (0..internal.len())
            .filter(|&i| internal[i] != out[i])
            .collect();

        assert_eq!(changed.len(), 1, "{}: {changed:?}", template.name);
        assert_eq!(
            out[changed[0]],
            "      mo_out     TYPE REF TO if_oo_adt_classrun_out,"
        );
        scan_as(&out.join("\n"), Some(NAME), mode_of(template))
            .unwrap_or_else(|found| panic!("{}: {found:?}", template.name));
    }
}

#[test]
fn the_scanner_allow_lists_both_out_interfaces() {
    assert!(ALLOWED_CLASSES.contains(&"IF_OO_ADT_INTRNL_CLASSRUN"));
    assert!(ALLOWED_CLASSES.contains(&"IF_OO_ADT_CLASSRUN_OUT"));
}
