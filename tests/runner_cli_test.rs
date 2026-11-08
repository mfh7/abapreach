//! The command line grammar of the DDIC and `TRAN` types (M6 Task 17): every valid form passes the
//! grammar, every wrong combination stops with its own text before any connection, and the help
//! texts state the temporary-class footprint.

use abapreach::config::Connection;
use abapreach::transport::{SadtTransport, TransportError};
use abapreach::{Exit, Factories, RfcLibraryInfo, run};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

/// A factory set that fails the test on any call: a grammar refusal opens nothing.
struct NoTransport;

impl Factories for NoTransport {
    fn adt(&mut self, _: &Connection) -> Opened {
        panic!("a grammar refusal opens no ADT connection")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        panic!("a grammar refusal opens no RFC connection")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("a grammar refusal probes no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("a grammar refusal pings nothing")
    }
}

fn env_in(directory: &Path) -> BTreeMap<String, String> {
    let path = directory.join("connections.json");

    fs::write(
        &path,
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"]}]}"#,
    )
    .unwrap();

    BTreeMap::from([
        ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn exit_of(arguments: &[&str]) -> Exit {
    let directory = tempfile::tempdir().unwrap();
    let mut args = vec!["abapreach".to_owned()];

    args.extend(arguments.iter().map(|argument| (*argument).to_owned()));
    args.extend(["--conn".to_owned(), "DEV_100".to_owned()]);

    run(args, env_in(directory.path()), &mut NoTransport)
}

/// What a grammar-valid runner command prints until the runner operations are wired (Tasks 18-24).
fn accepted(operation: &str, token: &str) -> String {
    format!("STOP: {operation} {token} is not implemented yet")
}

fn assert_stop(arguments: &[&str], expected: &str) {
    let exit = exit_of(arguments);

    assert_eq!(exit.stderr, format!("STOP: {expected}"), "{arguments:?}");
    assert_eq!(exit.stdout, "", "{arguments:?}");
    assert_eq!(exit.code, 2, "{arguments:?}");
}

const HASH: &str = "63886692bf652fa419147f746d6b7760d6c991606fa9603d8ade3caf08ac7c64";

#[test]
fn every_valid_form_passes_the_grammar_without_a_connection() {
    let name30 = "Z".repeat(30);
    let name20 = "Z".repeat(20);
    let text36 = "t".repeat(36);
    let cases: Vec<(Vec<&str>, &str, &str)> = vec![
        (vec!["read", "DTEL", "ZDTEL", "out.xml"], "read", "DTEL"),
        (
            vec!["read", "DOMA", "ZDOMA", "out.xml", "--force"],
            "read",
            "DOMA",
        ),
        (
            vec!["read", "TABL", "ZTABL", "out.xml", "--backend", "rfc"],
            "read",
            "TABL",
        ),
        (
            vec!["read", "TTYP", "ZTTYP", "out.xml", "--backend", "adt"],
            "read",
            "TTYP",
        ),
        (vec!["read", "TRAN", "ZTRAN", "out.txt"], "read", "TRAN"),
        // A read may name an object of the SAP namespace.
        (
            vec!["read", "TABL", "T000", "out.xml", "--force"],
            "read",
            "TABL",
        ),
        (
            vec!["read", "DTEL", "/ABC/ZDTEL", "out.xml", "--force"],
            "read",
            "DTEL",
        ),
        (
            vec!["read", "DTEL", &name30, "-", "--force"],
            "read",
            "DTEL",
        ),
        (
            vec!["read", "TRAN", &name20, "-", "--force"],
            "read",
            "TRAN",
        ),
        (
            vec!["deploy", "DTEL", "ZDTEL", "file.xml"],
            "deploy",
            "DTEL",
        ),
        (
            vec!["deploy", "--simulate", "DOMA", "ZDOMA", "file.xml"],
            "deploy",
            "DOMA",
        ),
        (
            vec![
                "deploy",
                "--simulate",
                "TABL",
                "ZTABL",
                "file.xml",
                "--base",
                HASH,
            ],
            "deploy",
            "TABL",
        ),
        (
            vec![
                "deploy",
                "TTYP",
                "ZTTYP",
                "file.xml",
                "--base",
                HASH,
                "--tr",
                "DEVK900001",
                "--backend",
                "rfc",
            ],
            "deploy",
            "TTYP",
        ),
        (
            vec!["create", "DTEL", "ZDTEL", "file.xml"],
            "create",
            "DTEL",
        ),
        (
            vec![
                "create",
                "--simulate",
                "DOMA",
                "ZDOMA",
                "file.xml",
                "--package",
                "ZPKG",
                "--tr",
                "DEVK900001",
                "--text",
                "A domain",
                "--backend",
                "rfc",
            ],
            "create",
            "DOMA",
        ),
        (
            vec![
                "create",
                "TABL",
                "ZTABL",
                "file.xml",
                "--package",
                "$TMP",
                "--backend",
                "adt",
            ],
            "create",
            "TABL",
        ),
        (
            vec!["create", "TTYP", "ZTTYP", "file.xml", "--text", "A type"],
            "create",
            "TTYP",
        ),
        (
            vec![
                "create",
                "TRAN",
                "ZTRAN",
                "--prog",
                "ZREPORT",
                "--text",
                "A transaction",
            ],
            "create",
            "TRAN",
        ),
        (
            vec![
                "create",
                "--simulate",
                "TRAN",
                &name20,
                "--prog",
                "ZREPORT",
                "--text",
                &text36,
                "--package",
                "ZPKG",
                "--tr",
                "DEVK900001",
                "--backend",
                "adt",
            ],
            "create",
            "TRAN",
        ),
        (
            vec![
                "create",
                "TRAN",
                "ZTRAN",
                "--prog",
                "ZREPORT",
                "--text",
                "T",
                "--backend",
                "rfc",
            ],
            "create",
            "TRAN",
        ),
        (
            vec!["create", "DTEL", &name30, "file.xml"],
            "create",
            "DTEL",
        ),
        // A lower-case name is the same name.
        (
            vec!["create", "DTEL", "zdtel", "file.xml"],
            "create",
            "DTEL",
        ),
        (
            vec!["deploy", "DTEL", "zdtel", "file.xml"],
            "deploy",
            "DTEL",
        ),
        (
            vec!["delete", "DTEL:ZDTEL", "--plan", "plan.txt"],
            "delete",
            "DTEL",
        ),
        (
            vec![
                "delete",
                "--simulate",
                "TRAN:ZTRAN",
                "TTYP:ZTTYP",
                "TABL:ZTABL",
                "DTEL:ZDTEL",
                "DOMA:ZDOMA",
                "--plan",
                "plan.txt",
                "--tr",
                "DEVK900001",
            ],
            "delete",
            "TRAN",
        ),
        (
            vec!["delete", "PROG:ZPROG", "TRAN:ZTRAN", "--plan", "plan.txt"],
            "delete",
            "TRAN",
        ),
    ];

    for (arguments, operation, token) in cases {
        // `read TRAN` and `create TRAN` run since Task 22; their forms are in
        // tests/runner_tran_ops_test.rs.
        if token == "TRAN" && matches!(operation, "read" | "create") {
            continue;
        }

        // `read` of a DDIC type runs since Task 18; its forms are in
        // tests/runner_read_ddic_test.rs.
        if operation == "read" && matches!(token, "DTEL" | "DOMA" | "TABL" | "TTYP") {
            continue;
        }

        // `delete` of DDIC rows runs since Task 23; its forms are in tests/delete_ddic_test.rs.
        // A batch with a TRAN row still stops here until Task 24.
        if operation == "delete" && token != "TRAN" {
            continue;
        }

        let exit = exit_of(&arguments);

        assert_eq!(exit.stderr, accepted(operation, token), "{arguments:?}");
        assert_eq!(exit.stdout, "", "{arguments:?}");
        assert_eq!(exit.code, 2, "{arguments:?}");
    }
}

#[test]
fn read_refuses_the_options_of_the_source_types_and_the_length_limits() {
    let long30 = "Z".repeat(31);
    let long20 = "Z".repeat(21);

    for (arguments, expected) in [
        (
            vec!["read", "DTEL", "ZD", "out.xml", "--lines", "1-5"],
            "--lines does not apply to DTEL".to_owned(),
        ),
        (
            vec!["read", "TRAN", "ZT", "-", "--lines", "1-5"],
            "--lines does not apply to TRAN".to_owned(),
        ),
        (
            vec!["read", "TABL", "ZT", "out.xml", "--include", "CCIMP"],
            "--include is only valid for CLAS".to_owned(),
        ),
        (
            vec!["read", "DOMA", "ZD", "out.xml", "--fugr", "ZG"],
            "--fugr is only valid for FUNC".to_owned(),
        ),
        (
            vec!["read", "TTYP", "ZT", "out.xml", "--main", "ZM"],
            "--main is only valid for PROG".to_owned(),
        ),
        (
            vec!["read", "DTEL", &long30, "out.xml"],
            format!("object name > 30 characters: {long30}"),
        ),
        (
            vec!["read", "TRAN", &long20, "out.xml"],
            format!("object name > 20 characters: {long20}"),
        ),
        (
            vec!["read", "DTEL", "ZD-1", "out.xml"],
            "object name must be 30 characters or less of A-Z 0-9 _ with an optional /NAMESPACE/ \
             prefix: ZD-1"
                .to_owned(),
        ),
    ] {
        assert_stop(&arguments, &expected);
    }
}

#[test]
fn read_refuses_to_overwrite_a_file_without_force() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.xml");

    fs::write(&out, "x").unwrap();

    let out = out.display().to_string();

    assert_stop(
        &["read", "DTEL", "ZD", &out],
        &format!("{out} already exists; use --force to overwrite"),
    );
    // With --force the command goes on to the connection (tests/runner_read_ddic_test.rs).
}

#[test]
fn deploy_refuses_tran_and_the_options_of_the_source_types() {
    let long30 = "Z".repeat(31);

    for (arguments, expected) in [
        (
            vec!["deploy", "TRAN", "ZT", "file.txt"],
            "TRAN has no deploy; use create or delete".to_owned(),
        ),
        (
            vec!["deploy", "--simulate", "TRAN", "ZT", "file.txt"],
            "TRAN has no deploy; use create or delete".to_owned(),
        ),
        (
            vec!["deploy", "DTEL", "ZD", "f.xml", "--include", "CCIMP"],
            "--include is only valid for CLAS".to_owned(),
        ),
        (
            vec!["deploy", "DOMA", "ZD", "f.xml", "--create-include"],
            "--create-include requires CLAS with --include".to_owned(),
        ),
        (
            vec!["deploy", "TABL", "ZT", "f.xml", "--fugr", "ZG"],
            "--fugr is only valid for FUNC".to_owned(),
        ),
        (
            vec!["deploy", "TTYP", "ZT", "f.xml", "--main", "ZM"],
            "--main is only valid for PROG".to_owned(),
        ),
        (
            vec!["deploy", "DTEL", &long30, "f.xml"],
            format!("object name > 30 characters: {long30}"),
        ),
        (
            vec!["deploy", "DTEL", "ADTEL", "f.xml"],
            "write and delete accept only Z and Y names: ADTEL".to_owned(),
        ),
        (
            vec!["deploy", "DTEL", "/ABC/ZD", "f.xml"],
            "write and delete accept only Z and Y names: /ABC/ZD".to_owned(),
        ),
        (
            vec!["deploy", "DTEL", "ZD", "f.xml", "--tr", "BAD TR"],
            "invalid --tr: BAD TR".to_owned(),
        ),
    ] {
        assert_stop(&arguments, &expected);
    }
}

#[test]
fn create_refuses_every_wrong_combination_with_its_own_text() {
    let long30 = "Z".repeat(31);
    let long20 = "Z".repeat(21);
    let text37 = "t".repeat(37);
    let prog_rule = "create TRAN needs --prog <report>; --prog only applies to TRAN";

    for (arguments, expected) in [
        (
            vec!["create", "DTEL", "ZD"],
            "create DTEL needs FILE (XML)".to_owned(),
        ),
        (
            vec!["create", "--simulate", "TTYP", "ZT", "--text", "t"],
            "create TTYP needs FILE (XML)".to_owned(),
        ),
        (
            vec!["create", "DOMA", "ZD", "f.xml", "--subc", "1"],
            "--subc only applies to PROG".to_owned(),
        ),
        (
            vec!["create", "TABL", "ZT", "f.xml", "--fugr", "ZG"],
            "--fugr is only valid for FUNC".to_owned(),
        ),
        (
            vec!["create", "DTEL", "ZD", "f.xml", "--prog", "ZR"],
            prog_rule.to_owned(),
        ),
        (
            vec!["create", "PROG", "ZP", "--text", "t", "--prog", "ZR"],
            prog_rule.to_owned(),
        ),
        (
            vec!["create", "TRAN", "ZT", "--text", "t"],
            prog_rule.to_owned(),
        ),
        (
            vec![
                "create", "TRAN", "ZT", "f.txt", "--prog", "ZR", "--text", "t",
            ],
            "create TRAN does not accept FILE".to_owned(),
        ),
        (
            vec!["create", "TRAN", "ZT", "--prog", "ZR", "--text", &text37],
            "--text is 37 characters; TRAN allows 36".to_owned(),
        ),
        (
            vec!["create", "TRAN", &long20, "--prog", "ZR", "--text", "t"],
            format!("object name > 20 characters: {long20}"),
        ),
        (
            vec!["create", "DTEL", &long30, "f.xml"],
            format!("object name > 30 characters: {long30}"),
        ),
        (
            vec!["create", "DTEL", "ADTEL", "f.xml"],
            "a created name must start with Z or Y: ADTEL".to_owned(),
        ),
        (
            vec!["create", "DTEL", "/ABC/ZD", "f.xml"],
            "a created name must start with Z or Y: /ABC/ZD".to_owned(),
        ),
        (
            vec!["create", "DOMA", "ZD", "f.xml", "--tr", "DEVK900001"],
            "--tr is not valid for the local package $TMP".to_owned(),
        ),
        (
            vec!["create", "TABL", "ZT", "f.xml", "--package", "ZPKG"],
            "package ZPKG is transportable: --tr is required".to_owned(),
        ),
        (
            vec!["create", "TTYP", "ZT", "f.xml", "--package", "BAD PKG"],
            "invalid --package: BAD PKG".to_owned(),
        ),
        (
            vec![
                "create",
                "TRAN",
                "ZT",
                "--prog",
                "ZR",
                "--text",
                "t",
                "--package",
                "ZPKG",
            ],
            "package ZPKG is transportable: --tr is required".to_owned(),
        ),
    ] {
        assert_stop(&arguments, &expected);
    }
}

#[test]
fn create_rejects_flags_that_no_new_type_takes() {
    // `--remote` belongs to no type of this tool; clap refuses it with its own text.
    let exit = exit_of(&["create", "DTEL", "ZD", "f.xml", "--remote"]);

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr
            .starts_with("STOP: error: unexpected argument '--remote' found"),
        "{}",
        exit.stderr
    );

    // The text of a transaction is mandatory, for DDIC it is optional.
    let exit = exit_of(&["create", "TRAN", "ZT", "--prog", "ZR"]);

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.contains("--text <TEXT>"), "{}", exit.stderr);
    assert!(
        exit.stderr
            .contains("the following required arguments were not provided"),
        "{}",
        exit.stderr
    );

    let exit = exit_of(&["create", "PROG", "ZP"]);

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.contains("--text <TEXT>"), "{}", exit.stderr);
}

#[test]
fn delete_accepts_the_new_types_and_refuses_bad_names_before_a_connection() {
    let long20 = "Z".repeat(21);

    assert_stop(
        &[
            "delete",
            "TRAN:ZT",
            &format!("TRAN:{long20}"),
            "--plan",
            "p",
        ],
        &format!("object name > 20 characters: {long20}"),
    );
    assert_stop(
        &["delete", "DTEL:ZD-1", "--plan", "p"],
        "object name must be 30 characters or less of A-Z 0-9 _ with an optional /NAMESPACE/ \
         prefix: ZD-1",
    );
    assert_stop(
        &["delete", "DTEL:ZD", "--plan", "p", "--tr", "BAD TR"],
        "invalid --tr: BAD TR",
    );
    assert_stop(
        &["delete", "DTEL:ADTEL", "--plan", "p"],
        "write and delete accept only Z and Y names: ADTEL",
    );
    assert_stop(
        &["delete", "DTEL", "--plan", "p"],
        "error: invalid value 'DTEL' for '<TYPE:NAME>...': delete target must be TYPE:NAME \
         (PROG|FUGR|FUNC|CLAS|INTF|DTEL|DOMA|TABL|TTYP|TRAN): \"DTEL\"\n\n\
         For more information, try '--help'.",
    );
    assert_stop(
        &["delete", "VIEW:ZV", "--plan", "p"],
        "error: invalid value 'VIEW:ZV' for '<TYPE:NAME>...': delete target must be TYPE:NAME \
         (PROG|FUGR|FUNC|CLAS|INTF|DTEL|DOMA|TABL|TTYP|TRAN): \"VIEW:ZV\"\n\n\
         For more information, try '--help'.",
    );
}

#[test]
fn an_unknown_type_lists_the_new_types() {
    for (verb, values) in [
        (
            "read",
            "PROG, FUNC, CLAS, INTF, DTEL, DOMA, TABL, TTYP, TRAN",
        ),
        (
            "deploy",
            "PROG, FUNC, CLAS, INTF, DTEL, DOMA, TABL, TTYP, TRAN",
        ),
        (
            "create",
            "PROG, FUGR, FUNC, CLAS, INTF, DTEL, DOMA, TABL, TTYP, TRAN",
        ),
    ] {
        let exit = exit_of(&[verb, "BOGUS", "x", "y"]);

        assert_eq!(exit.code, 2, "{verb}");
        assert!(
            exit.stderr
                .contains(&format!("[possible values: {values}]")),
            "{verb}: {}",
            exit.stderr
        );
    }
}

fn help(arguments: &[&str]) -> String {
    let args = arguments.iter().map(|argument| (*argument).to_owned());
    let exit = run(
        std::iter::once("abapreach".to_owned())
            .chain(args)
            .collect(),
        BTreeMap::new(),
        &mut NoTransport,
    );

    assert_eq!(exit.code, 0, "{arguments:?}");

    exit.stdout
}

#[test]
fn the_help_of_read_states_the_temporary_class() {
    let text = help(&["read", "--help"]);

    assert!(
        text.contains(
            "DTEL, DOMA, TABL, TTYP and TRAN are read through a temporary class that the command \
             creates in $TMP and deletes again"
        ),
        "{text}"
    );
}

#[test]
fn the_help_of_simulate_states_the_temporary_class() {
    for verb in ["deploy", "create", "delete"] {
        let text = help(&[verb, "--help"]);

        assert!(
            text.contains(
                "For DTEL, DOMA, TABL, TTYP and TRAN a simulate still creates and deletes a \
                 temporary class in $TMP"
            ),
            "{verb}: {text}"
        );
    }
}

#[test]
fn the_help_of_create_names_prog_and_the_optional_text() {
    let text = help(&["create", "--help"]);

    assert!(
        text.contains("--prog <PROG>") && text.contains("A `TRAN` create only"),
        "{text}"
    );
    assert!(text.contains("--text <TEXT>"), "{text}");
}
