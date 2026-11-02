use abapreach::backend::{BackendKind, Failure, OperationKind, Router};
use abapreach::cli::{Cli, Command, CreateKind, IncludeKind, LineRange, ObjectKind};
use abapreach::config::Connection;
use abapreach::hash::{normalize, source_hash};
use abapreach::object::{
    check_create, check_deploy_options, check_deploy_source, check_group_match, check_read,
};
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo, run};
use clap::Parser;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const CLASS: &str = "/sap/bc/adt/oo/classes/zcl_x";

fn stdout() -> &'static Path {
    Path::new("-")
}

// ---- URI forms -------------------------------------------------------------------------

#[test]
fn source_object_and_version_uris_follow_the_evidenced_forms() {
    let intf = ObjectKind::Intf;
    let func = ObjectKind::Func;
    let clas = ObjectKind::Clas;

    assert_eq!(
        intf.source_uri("ZIF_X", None, None).unwrap(),
        "/sap/bc/adt/oo/interfaces/zif_x/source/main"
    );
    assert_eq!(
        intf.versions_uri("ZIF_X", None, None).unwrap(),
        "/sap/bc/adt/oo/interfaces/zif_x/source/main/versions"
    );
    assert_eq!(
        func.object_uri("ZFM", Some("ZFG")).unwrap(),
        "/sap/bc/adt/functions/groups/zfg/fmodules/zfm"
    );
    assert_eq!(
        func.source_uri("ZFM", Some("ZFG"), None).unwrap(),
        "/sap/bc/adt/functions/groups/zfg/fmodules/zfm/source/main"
    );
    assert_eq!(
        func.versions_uri("ZFM", Some("ZFG"), None).unwrap(),
        "/sap/bc/adt/functions/groups/zfg/fmodules/zfm/source/main/versions"
    );
    assert_eq!(
        clas.source_uri("ZCL_X", None, None).unwrap(),
        format!("{CLASS}/source/main")
    );
    assert_eq!(
        ObjectKind::Prog.source_uri("ZP", None, None).unwrap(),
        "/sap/bc/adt/programs/programs/zp/source/main"
    );
}

#[test]
fn a_class_include_has_its_own_source_and_version_uri_but_the_class_is_the_lock_parent() {
    let clas = ObjectKind::Clas;
    let includes = [
        (IncludeKind::Ccimp, "implementations"),
        (IncludeKind::Ccdef, "definitions"),
        (IncludeKind::Ccmac, "macros"),
        (IncludeKind::Ccau, "testclasses"),
    ];

    for (include, segment) in includes {
        assert_eq!(
            clas.source_uri("ZCL_X", None, Some(include)).unwrap(),
            format!("{CLASS}/includes/{segment}")
        );
        assert_eq!(
            clas.versions_uri("ZCL_X", None, Some(include)).unwrap(),
            format!("{CLASS}/includes/{segment}/versions")
        );
        assert_eq!(clas.object_uri("ZCL_X", None).unwrap(), CLASS);
    }

    // The class has no source/main/versions; its main feed is includes/main/versions.
    assert_eq!(
        clas.versions_uri("ZCL_X", None, None).unwrap(),
        format!("{CLASS}/includes/main/versions")
    );
    assert!(
        ObjectKind::Intf
            .source_uri("ZIF_X", None, Some(IncludeKind::Ccau))
            .is_err()
    );
}

#[test]
fn a_function_module_uri_needs_its_group() {
    assert!(ObjectKind::Func.object_uri("ZFM", None).is_err());
    assert!(ObjectKind::Func.source_uri("ZFM", None, None).is_err());
}

#[test]
fn namespaced_source_paths_are_refused_for_the_new_kinds_only() {
    for kind in [ObjectKind::Func, ObjectKind::Clas, ObjectKind::Intf] {
        let error = kind.source_uri("/NS/ZNAME", Some("ZFG"), None).unwrap_err();

        assert!(error.contains("namespaced"), "{error}");
        assert!(kind.versions_uri("/NS/ZNAME", Some("ZFG"), None).is_err());
    }

    assert!(ObjectKind::Func.object_uri("ZFM", Some("/NS/ZFG")).is_err());
    assert_eq!(
        ObjectKind::Prog.source_uri("/NS/ZP", None, None).unwrap(),
        "/sap/bc/adt/programs/programs/%2fns%2fzp/source/main"
    );
}

#[test]
fn create_uris_and_validation_queries_follow_the_evidenced_forms() {
    assert_eq!(
        CreateKind::Fugr.creation_uri(None).unwrap(),
        "/sap/bc/adt/functions/groups"
    );
    assert_eq!(
        CreateKind::Func.creation_uri(Some("ZFG")).unwrap(),
        "/sap/bc/adt/functions/groups/zfg/fmodules"
    );
    assert!(CreateKind::Func.creation_uri(None).is_err());
    assert_eq!(
        CreateKind::Clas.creation_uri(None).unwrap(),
        "/sap/bc/adt/oo/classes"
    );
    assert_eq!(
        CreateKind::Intf.creation_uri(None).unwrap(),
        "/sap/bc/adt/oo/interfaces"
    );
    assert_eq!(
        CreateKind::Fugr.validation_uri("zfg", "My group", "$TMP"),
        "/sap/bc/adt/functions/validation?objtype=FUGR%2FF&objname=ZFG&description=My%20group&packagename=%24TMP"
    );
    assert_eq!(
        CreateKind::Func.validation_uri("ZFM", "Text", "ZFG"),
        "/sap/bc/adt/functions/validation?objtype=FUGR%2FFF&objname=ZFM&description=Text&fugrname=ZFG"
    );
    assert_eq!(
        CreateKind::Clas.validation_uri("ZCL_X", "Text", "ZPKG"),
        "/sap/bc/adt/oo/validation/objectname?objtype=CLAS%2FOC&objname=ZCL_X&description=Text&packagename=ZPKG"
    );
    assert_eq!(
        CreateKind::Intf.validation_uri("ZIF_X", "Text", "$TMP"),
        "/sap/bc/adt/oo/validation/objectname?objtype=INTF%2FOI&objname=ZIF_X&description=Text&packagename=%24TMP"
    );
}

#[test]
fn a_namespaced_name_is_percent_encoded_in_the_validation_query() {
    assert_eq!(
        CreateKind::Func.validation_uri("/NS/ZFM", "T", "/NS/ZFG"),
        "/sap/bc/adt/functions/validation?objtype=FUGR%2FFF&objname=%2FNS%2FZFM&description=T&fugrname=%2FNS%2FZFG"
    );
}

// ---- names, texts and limits -----------------------------------------------------------

#[test]
fn name_and_text_limits_per_type() {
    let long = |count: usize| "Z".repeat(count);

    for (kind, max) in [
        (ObjectKind::Prog, 40),
        (ObjectKind::Func, 30),
        (ObjectKind::Clas, 30),
        (ObjectKind::Intf, 30),
    ] {
        assert_eq!(kind.max_name(), max);
        assert!(
            check_deploy_options(kind, &long(max), None, false, None).is_ok(),
            "{kind:?}"
        );
        assert!(
            check_deploy_options(kind, &long(max + 1), None, false, None).is_err(),
            "{kind:?}"
        );
        assert!(check_read(kind, &long(max + 1), stdout(), None, None, None).is_err());
    }

    for (kind, name_max, text_max) in [
        (CreateKind::Fugr, 26, 40),
        (CreateKind::Func, 30, 74),
        (CreateKind::Clas, 30, 60),
        (CreateKind::Intf, 30, 60),
    ] {
        let fugr = (kind == CreateKind::Func).then_some("ZFG");
        let ok = |name: &str, text: &str| check_create(kind, name, text, None, fugr, None);

        assert!(
            ok(&long(name_max), &"t".repeat(text_max)).is_ok(),
            "{kind:?}"
        );
        assert!(ok(&long(name_max + 1), "t").is_err(), "{kind:?}");
        assert!(ok("ZNAME", &"t".repeat(text_max + 1)).is_err(), "{kind:?}");
    }
}

#[test]
fn created_names_are_z_or_y_only() {
    for name in ["ZNAME", "YNAME", "zname", "yname"] {
        assert!(
            check_create(CreateKind::Intf, name, "t", None, None, None).is_ok(),
            "{name}"
        );
    }

    for name in ["ANAME", "SAPNAME", "/NS/ZNAME", "Z NAME", ""] {
        assert!(
            check_create(CreateKind::Intf, name, "t", None, None, None).is_err(),
            "{name}"
        );
    }
}

#[test]
fn read_and_deploy_keep_the_program_name_grammar() {
    for kind in [ObjectKind::Func, ObjectKind::Clas, ObjectKind::Intf] {
        for name in ["", "Z?X", "Z X", "/NS/", "ZX/"] {
            assert!(
                check_read(kind, name, stdout(), None, None, None).is_err(),
                "{kind:?} {name:?}"
            );
        }

        // Not Z/Y-only: read and deploy accept any valid existing name.
        assert!(check_read(kind, "SAPLNAME", stdout(), None, None, None).is_ok());
    }
}

// ---- option combinations ---------------------------------------------------------------

#[test]
fn include_is_valid_only_for_clas() {
    let include = Some(IncludeKind::Ccau);

    for kind in [ObjectKind::Prog, ObjectKind::Func, ObjectKind::Intf] {
        let out = Path::new("out.abap");

        assert!(check_read(kind, "ZX", out, include, None, None).is_err());
        assert!(check_deploy_options(kind, "ZX", include, false, None).is_err());
    }

    assert!(check_read(ObjectKind::Clas, "ZX", Path::new("o"), include, None, None).is_ok());
    assert!(check_deploy_options(ObjectKind::Clas, "ZX", include, false, None).is_ok());
    assert!(check_deploy_options(ObjectKind::Clas, "ZX", None, false, None).is_ok());
}

#[test]
fn fugr_is_valid_only_for_func() {
    for kind in [ObjectKind::Prog, ObjectKind::Clas, ObjectKind::Intf] {
        assert!(check_read(kind, "ZX", Path::new("o"), None, Some("ZFG"), None).is_err());
        assert!(check_deploy_options(kind, "ZX", None, false, Some("ZFG")).is_err());
    }

    let func = ObjectKind::Func;

    assert!(check_read(func, "ZX", Path::new("o"), None, Some("ZFG"), None).is_ok());
    assert!(check_deploy_options(func, "ZX", None, false, Some("ZFG")).is_ok());
    assert!(check_deploy_options(func, "ZX", None, false, None).is_ok());
    // The group name follows the group limit and has no verified namespaced path.
    assert!(check_deploy_options(func, "ZX", None, false, Some(&"G".repeat(27))).is_err());
    assert!(check_deploy_options(func, "ZX", None, false, Some("/NS/ZFG")).is_err());
}

#[test]
fn a_supplied_group_must_equal_the_server_resolution() {
    assert!(check_group_match(None, "ZFG").is_ok());
    assert!(check_group_match(Some("ZFG"), "ZFG").is_ok());
    assert!(check_group_match(Some("zfg"), "ZFG").is_ok());

    let error = check_group_match(Some("ZOTHER"), "ZFG").unwrap_err();

    assert!(error.contains("ZOTHER") && error.contains("ZFG"), "{error}");
}

#[test]
fn create_include_needs_clas_with_include() {
    let create = |kind, include| check_deploy_options(kind, "ZX", include, true, None);

    assert!(create(ObjectKind::Clas, Some(IncludeKind::Ccau)).is_ok());
    assert!(create(ObjectKind::Clas, None).is_err());
    assert!(create(ObjectKind::Prog, None).is_err());
    assert!(create(ObjectKind::Func, None).is_err());
    assert!(create(ObjectKind::Intf, None).is_err());
    // --include on a non-CLAS stops first, whatever the flag says.
    assert!(create(ObjectKind::Intf, Some(IncludeKind::Ccau)).is_err());
}

#[test]
fn namespaced_read_and_deploy_stop_for_the_new_kinds_but_not_for_prog() {
    for kind in [ObjectKind::Func, ObjectKind::Clas, ObjectKind::Intf] {
        assert!(check_read(kind, "/NS/ZX", stdout(), None, None, None).is_err());
        assert!(check_deploy_options(kind, "/NS/ZX", None, false, None).is_err());
    }

    assert!(check_read(ObjectKind::Prog, "/NS/ZX", Path::new("o"), None, None, None).is_ok());
}

#[test]
fn create_option_rules() {
    let create = |kind: CreateKind, package: Option<&str>, fugr: Option<&str>, tr: Option<&str>| {
        check_create(kind, "ZX", "t", package, fugr, tr)
    };

    // --fugr: required for FUNC, invalid elsewhere.
    assert!(create(CreateKind::Func, None, Some("ZFG"), None).is_ok());
    assert!(create(CreateKind::Func, None, None, None).is_err());
    assert!(create(CreateKind::Func, None, Some("/NS/ZFG"), None).is_err());
    assert!(create(CreateKind::Func, None, Some(&"G".repeat(27)), None).is_err());

    for kind in [CreateKind::Fugr, CreateKind::Clas, CreateKind::Intf] {
        assert!(create(kind, None, Some("ZFG"), None).is_err(), "{kind:?}");
    }

    // --package: not for FUNC (its group decides).
    assert!(create(CreateKind::Func, Some("ZPKG"), Some("ZFG"), None).is_err());
    assert!(create(CreateKind::Func, None, Some("ZFG"), Some("DEVK000001")).is_ok());

    for kind in [CreateKind::Fugr, CreateKind::Clas, CreateKind::Intf] {
        // $TMP (the default) rejects --tr; a transportable package requires it.
        assert!(create(kind, None, None, None).is_ok(), "{kind:?}");
        assert!(create(kind, Some("$TMP"), None, None).is_ok(), "{kind:?}");
        assert!(create(kind, Some("$TMP"), None, Some("DEVK000001")).is_err());
        assert!(create(kind, None, None, Some("DEVK000001")).is_err());
        assert!(create(kind, Some("ZPKG"), None, None).is_err(), "{kind:?}");
        assert!(create(kind, Some("ZPKG"), None, Some("DEVK000001")).is_ok());
        assert!(create(kind, Some("Z PKG"), None, Some("DEVK000001")).is_err());
        assert!(create(kind, Some(&"P".repeat(31)), None, Some("DEVK000001")).is_err());
    }
}

// ---- deploy file -----------------------------------------------------------------------

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

#[test]
fn a_python_include_format_func_file_is_refused_with_an_adt_format_explanation() {
    let python =
        lines("FUNCTION zfm.\n*\"----------\n*\"*\"Local Interface:\n  WRITE 'x'.\nENDFUNCTION.");
    let error = check_deploy_source(ObjectKind::Func, "ZFM", &python).unwrap_err();

    assert!(error.contains("ADT format"), "{error}");
    assert!(error.contains("line 2"), "{error}");

    let adt = lines("FUNCTION zfm\n  IMPORTING VALUE(iv_x) TYPE i.\n  WRITE 'x'.\nENDFUNCTION.");

    assert!(check_deploy_source(ObjectKind::Func, "ZFM", &adt).is_ok());
    // The header rule is FUNC only; a class or program may carry such a comment line.
    assert!(check_deploy_source(ObjectKind::Clas, "ZCL_X", &python).is_ok());
}

#[test]
fn deploy_source_keeps_the_empty_and_line_length_rules_for_every_kind() {
    for kind in [
        ObjectKind::Prog,
        ObjectKind::Func,
        ObjectKind::Clas,
        ObjectKind::Intf,
    ] {
        assert!(check_deploy_source(kind, "ZX", &[]).is_err(), "{kind:?}");
        assert!(
            check_deploy_source(kind, "ZX", &lines("x")).is_ok(),
            "{kind:?}"
        );
        assert!(check_deploy_source(kind, "ZX", &["x".repeat(256)]).is_err());
    }
}

#[test]
fn an_empty_body_and_a_lone_nul_are_one_canonical_empty_source() {
    let empty = source_hash(&[]);

    assert_eq!(source_hash(&[String::new()]), empty);
    assert_eq!(source_hash(&["\0".to_owned()]), empty);
    assert_eq!(source_hash(&["\0\r".to_owned(), String::new()]), empty);
    assert!(normalize(vec!["\0".to_owned()]).is_empty());
    // Only the lone NUL is empty; NUL inside real source stays.
    assert_ne!(source_hash(&["\0".to_owned(), "x".to_owned()]), empty);
    assert_ne!(source_hash(&["a\0".to_owned()]), empty);
    // SAP still rejects an empty FUNC PUT, and so does the deploy file check.
    assert!(check_deploy_source(ObjectKind::Func, "ZFM", &["\0".to_owned()]).is_err());
}

// ---- --lines and CLI parsing -----------------------------------------------------------

#[test]
fn lines_grammar_is_one_based_inclusive_with_a_not_after_b() {
    assert_eq!("1-1".parse(), Ok(LineRange { start: 1, end: 1 }));
    assert_eq!("3-12".parse(), Ok(LineRange { start: 3, end: 12 }));

    for bad in [
        "",
        "1",
        "-",
        "1-",
        "-2",
        "0-3",
        "5-3",
        "a-b",
        "1-2-3",
        " 1-2",
        "1-2 ",
        "+1-2",
        "1--2",
        "1:2",
        "1-99999999999999999999999",
    ] {
        assert!(bad.parse::<LineRange>().is_err(), "{bad:?}");
    }
}

#[test]
fn lines_require_stdout() {
    let range = Some(LineRange { start: 2, end: 4 });

    assert!(check_read(ObjectKind::Prog, "ZX", stdout(), None, None, range).is_ok());
    assert!(
        check_read(
            ObjectKind::Prog,
            "ZX",
            Path::new("out.abap"),
            None,
            None,
            range
        )
        .is_err()
    );
    assert!(
        check_read(
            ObjectKind::Prog,
            "ZX",
            Path::new("out.abap"),
            None,
            None,
            None
        )
        .is_ok()
    );
}

fn parse(args: &[&str]) -> Result<Command, String> {
    Cli::try_parse_from(args)
        .map(|cli| cli.command)
        .map_err(|error| error.to_string())
}

#[test]
fn cli_parses_the_m3_commands() {
    match parse(&[
        "abapreach",
        "read",
        "CLAS",
        "ZCL_X",
        "-",
        "--include",
        "CCAU",
        "--lines",
        "2-9",
        "--conn",
        "c",
    ])
    .unwrap()
    {
        Command::Read {
            kind,
            out,
            include,
            fugr,
            lines,
            ..
        } => {
            assert_eq!(kind, ObjectKind::Clas);
            assert_eq!(out, PathBuf::from("-"));
            assert_eq!(include, Some(IncludeKind::Ccau));
            assert_eq!(fugr, None);
            assert_eq!(lines, Some(LineRange { start: 2, end: 9 }));
        }
        other => panic!("{other:?}"),
    }

    match parse(&[
        "abapreach",
        "deploy",
        "FUNC",
        "ZFM",
        "f.abap",
        "--fugr",
        "ZFG",
        "--conn",
        "c",
    ])
    .unwrap()
    {
        Command::Deploy {
            kind,
            fugr,
            include,
            create_include,
            ..
        } => {
            assert_eq!(kind, ObjectKind::Func);
            assert_eq!(fugr.as_deref(), Some("ZFG"));
            assert_eq!(include, None);
            assert!(!create_include);
        }
        other => panic!("{other:?}"),
    }

    match parse(&[
        "abapreach",
        "deploy",
        "CLAS",
        "ZCL_X",
        "f.abap",
        "--include",
        "CCIMP",
        "--create-include",
        "--conn",
        "c",
    ])
    .unwrap()
    {
        Command::Deploy {
            include,
            create_include,
            ..
        } => {
            assert_eq!(include, Some(IncludeKind::Ccimp));
            assert!(create_include);
        }
        other => panic!("{other:?}"),
    }

    match parse(&[
        "abapreach",
        "create",
        "FUNC",
        "ZFM",
        "--text",
        "A text",
        "--fugr",
        "ZFG",
        "--tr",
        "DEVK000001",
        "--simulate",
        "--backend",
        "adt",
        "--conn",
        "c",
    ])
    .unwrap()
    {
        Command::Create {
            kind,
            name,
            text,
            package,
            fugr,
            tr,
            simulate,
            ..
        } => {
            assert_eq!(kind, CreateKind::Func);
            assert_eq!(name, "ZFM");
            assert_eq!(text, "A text");
            assert_eq!(package, None);
            assert_eq!(fugr.as_deref(), Some("ZFG"));
            assert_eq!(tr.as_deref(), Some("DEVK000001"));
            assert!(simulate);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn cli_rejects_unknown_kinds_includes_and_missing_arguments() {
    let read = |kind: &'static str| ["abapreach", "read", kind, "ZX", "o", "--conn", "c"];

    for kind in ["FUGR", "TRAN", "prog", ""] {
        assert!(Cli::try_parse_from(read(kind)).is_err(), "{kind:?}");
    }

    for kind in ["PROG", "FUNC", "CLAS", "INTF"] {
        assert!(Cli::try_parse_from(read(kind)).is_ok(), "{kind:?}");
    }

    assert!(
        parse(&[
            "abapreach",
            "read",
            "CLAS",
            "ZX",
            "o",
            "--include",
            "CCXX",
            "--conn",
            "c"
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "abapreach",
            "read",
            "PROG",
            "ZX",
            "-",
            "--lines",
            "5-3",
            "--conn",
            "c"
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "abapreach",
            "create",
            "PROG",
            "ZX",
            "--text",
            "t",
            "--conn",
            "c"
        ])
        .is_err()
    );
    assert!(parse(&["abapreach", "create", "INTF", "ZX", "--conn", "c"]).is_err());
    assert!(parse(&["abapreach", "create", "INTF", "ZX", "--text", "t"]).is_err());
}

#[test]
fn prog_syntax_is_unchanged() {
    assert!(
        parse(&[
            "abapreach",
            "read",
            "PROG",
            "Z",
            "o",
            "--force",
            "--conn",
            "c"
        ])
        .is_ok()
    );
    assert!(
        parse(&[
            "abapreach",
            "deploy",
            "PROG",
            "Z",
            "f",
            "--simulate",
            "--base",
            "h",
            "--tr",
            "T",
            "--backend",
            "rfc",
            "--conn",
            "c",
        ])
        .is_ok()
    );
}

// ---- routing ---------------------------------------------------------------------------

#[test]
fn fallback_policy_is_the_same_for_every_object_operation() {
    let router = Router::both();
    let operations = [
        OperationKind::Read(ObjectKind::Func),
        OperationKind::Deploy(ObjectKind::Clas),
        OperationKind::Deploy(ObjectKind::Intf),
        OperationKind::Create(CreateKind::Fugr),
    ];

    for operation in operations {
        assert_eq!(
            router
                .select(operation, None, false, Failure::Initial)
                .unwrap(),
            BackendKind::AdtHttp
        );
        assert_eq!(
            router
                .select(operation, None, false, Failure::PreSendConnectivity)
                .unwrap(),
            BackendKind::RfcSadt
        );

        for failure in [
            Failure::Authorization,
            Failure::Conflict,
            Failure::Validation,
            Failure::PostSendUncertain,
        ] {
            let error = router.select(operation, None, false, failure).unwrap_err();

            assert!(error.contains(&operation.to_string()), "{error}");
        }

        assert!(
            router
                .select(
                    operation,
                    Some(BackendKind::AdtHttp),
                    false,
                    Failure::PreSendConnectivity
                )
                .unwrap_err()
                .contains("--backend")
        );
    }

    assert_eq!(
        OperationKind::Deploy(ObjectKind::Func).to_string(),
        "deploy FUNC"
    );
    assert_eq!(
        OperationKind::Create(CreateKind::Clas).to_string(),
        "create CLAS"
    );
}

// ---- nothing connects for an invalid command -------------------------------------------

struct NoConnect;

impl Factories for NoConnect {
    fn adt(&mut self, _: &Connection) -> Result<Box<dyn SadtTransport>, TransportError> {
        panic!("ADT transport opened for a command that must stop first");
    }

    fn rfc(&mut self, _: &Connection) -> Result<Box<dyn SadtTransport>, TransportError> {
        panic!("RFC transport opened for a command that must stop first");
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("RFC library probed");
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("RFC ping");
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

fn env(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn stops(directory: &Path, args: &[&str]) -> String {
    let mut full = vec!["abapreach".to_owned()];

    full.extend(args.iter().map(|arg| (*arg).to_owned()));
    full.extend(["--conn".to_owned(), "dev_100".to_owned()]);

    let exit = run(full, env(directory), &mut NoConnect);

    assert_eq!(exit.code, 2, "{args:?}: {}", exit.stderr);
    assert!(exit.stdout.is_empty(), "{args:?}: {}", exit.stdout);
    assert!(
        exit.stderr.starts_with("STOP: "),
        "{args:?}: {}",
        exit.stderr
    );

    exit.stderr
}

#[test]
fn invalid_combinations_stop_with_exit_2_before_any_transport_is_opened() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let file = dir.join("source.abap");

    fs::write(&file, "FUNCTION zfm.\n*\"----\nENDFUNCTION.\n").unwrap();

    let file = file.to_str().unwrap();
    let toolong = "Z".repeat(31);
    let long_text = "t".repeat(75);
    let cases: Vec<Vec<&str>> = vec![
        vec!["read", "FUNC", "ZFM", "o", "--include", "CCAU"],
        vec!["read", "PROG", "ZP", "o", "--include", "CCAU"],
        vec!["read", "CLAS", "ZCL_X", "o", "--fugr", "ZFG"],
        vec!["read", "INTF", "ZIF_X", "o", "--lines", "1-2"],
        vec!["read", "CLAS", &toolong, "o"],
        vec!["read", "CLAS", "/NS/ZCL_X", "o"],
        vec!["read", "PROG", "ZP", "o", "--lines", "5-3"],
        vec!["deploy", "INTF", "ZIF_X", file, "--include", "CCDEF"],
        vec!["deploy", "CLAS", "ZCL_X", file, "--fugr", "ZFG"],
        vec!["deploy", "CLAS", "ZCL_X", file, "--create-include"],
        vec!["deploy", "PROG", "ZP", file, "--create-include"],
        vec!["deploy", "FUNC", "ZFM", file, "--fugr", "/NS/ZFG"],
        vec!["deploy", "FUNC", "ZFM", file],
        vec!["create", "INTF", "ANAME", "--text", "t"],
        vec!["create", "CLAS", &toolong, "--text", "t"],
        vec![
            "create", "FUNC", "ZFM", "--text", &long_text, "--fugr", "ZFG",
        ],
        vec!["create", "FUNC", "ZFM", "--text", "t"],
        vec![
            "create",
            "FUNC",
            "ZFM",
            "--text",
            "t",
            "--fugr",
            "ZFG",
            "--package",
            "ZP",
        ],
        vec!["create", "CLAS", "ZCL_X", "--text", "t", "--fugr", "ZFG"],
        vec![
            "create",
            "CLAS",
            "ZCL_X",
            "--text",
            "t",
            "--package",
            "ZPKG",
        ],
        vec![
            "create",
            "CLAS",
            "ZCL_X",
            "--text",
            "t",
            "--tr",
            "DEVK000001",
        ],
        vec!["create", "PROG", "ZP", "--text", "t"],
    ];

    for args in cases {
        stops(dir, &args);
    }

    let message = stops(dir, &["deploy", "FUNC", "ZFM", file]);

    assert!(message.contains("ADT format"), "{message}");

    let message = stops(dir, &["read", "PROG", "ZP", "o.abap", "--lines", "1-2"]);

    assert!(message.contains("stdout"), "{message}");
}

#[test]
fn valid_new_kind_commands_that_are_not_implemented_yet_stop_before_any_transport() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let file = dir.join("source.abap");

    fs::write(
        &file,
        "FUNCTION zfm\n  IMPORTING VALUE(iv_x) TYPE i.\nENDFUNCTION.\n",
    )
    .unwrap();

    let file = file.to_str().unwrap();

    for args in [
        vec!["deploy", "FUNC", "ZFM", file],
        vec![
            "deploy",
            "CLAS",
            "ZCL_X",
            file,
            "--include",
            "CCAU",
            "--create-include",
        ],
        vec!["create", "FUGR", "ZFG", "--text", "t"],
        vec!["create", "FUNC", "ZFM", "--text", "t", "--fugr", "ZFG"],
    ] {
        let message = stops(dir, &args);

        assert!(message.contains("not available yet"), "{message}");
    }
}
