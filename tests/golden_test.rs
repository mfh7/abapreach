//! The public binary without an extension against the output of the old M5 branch (`f7881c8`).
//! `tests/golden/new/` is what it must print; `tests/golden/old/` is what the old branch printed;
//! `tests/golden/README.md` states the rule that relates the two.

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{NoTools, Server, ToolHandler, serve};
use abapreach::mcp_read::ReadTools;
use abapreach::mcp_write::{McpTools, TokenGate};
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::{SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

fn golden(directory: &str, name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(directory)
        .join(name);

    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn binary(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abapreach"))
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    // clap names the program after the executable file: `abapreach.exe` on Windows.
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace("abapreach.exe", "abapreach")
}

/// File name, arguments, and whether the text is on stdout (help, version) or stderr (a STOP).
const COMMANDS: [(&str, &[&str], bool); 14] = [
    ("help-top.txt", &["--help"], true),
    ("help-read.txt", &["read", "--help"], true),
    ("help-deploy.txt", &["deploy", "--help"], true),
    ("help-create.txt", &["create", "--help"], true),
    ("help-read-table.txt", &["read-table", "--help"], true),
    ("help-delete.txt", &["delete", "--help"], true),
    ("help-mcp.txt", &["mcp", "--help"], true),
    ("help-doctor.txt", &["doctor", "--help"], true),
    ("version.txt", &["--version"], true),
    (
        "err-read-unknown-type.txt",
        &["read", "BOGUS", "x", "out", "--conn", "c"],
        false,
    ),
    (
        "err-deploy-unknown-type.txt",
        &["deploy", "BOGUS", "x", "f", "--conn", "c"],
        false,
    ),
    (
        "err-create-unknown-type.txt",
        &["create", "BOGUS", "x", "--text", "t", "--conn", "c"],
        false,
    ),
    (
        "err-delete-unknown-type.txt",
        &["delete", "BOGUS:X", "--plan", "p", "--conn", "c"],
        false,
    ),
    (
        "err-delete-bad-target.txt",
        &["delete", "PROG", "--plan", "p", "--conn", "c"],
        false,
    ),
];

#[test]
fn every_help_version_and_grammar_error_is_the_golden_text() {
    for (file, args, on_stdout) in COMMANDS {
        let output = binary(args);
        let (printed, other) = if on_stdout {
            (text(&output.stdout), text(&output.stderr))
        } else {
            (text(&output.stderr), text(&output.stdout))
        };

        assert_eq!(printed, golden("new", file), "{file}: {args:?}");
        assert_eq!(other, "", "{file}: {args:?}");
        assert_eq!(
            output.status.code(),
            Some(if on_stdout { 0 } else { 2 }),
            "{file}"
        );
    }
}

/// The M6 contract: the DDIC types and `TRAN` are types, `--prog` is an option of `create`, and
/// the footprint of the temporary class is in the help of `read` and of the simulate options.
#[test]
fn the_cli_golden_texts_name_the_new_types_the_prog_option_and_the_footprint() {
    for file in [
        "err-read-unknown-type.txt",
        "err-deploy-unknown-type.txt",
        "err-create-unknown-type.txt",
        "help-read.txt",
        "help-deploy.txt",
        "help-create.txt",
    ] {
        assert!(
            golden("new", file).contains("DTEL, DOMA, TABL, TTYP, TRAN"),
            "{file}"
        );
    }

    for file in ["err-delete-unknown-type.txt", "err-delete-bad-target.txt"] {
        assert!(
            golden("new", file).contains("(PROG|FUGR|FUNC|CLAS|INTF|DTEL|DOMA|TABL|TTYP|TRAN)"),
            "{file}"
        );
    }

    assert!(golden("new", "help-create.txt").contains("--prog <PROG>"));

    for file in ["help-deploy.txt", "help-create.txt", "help-delete.txt"] {
        assert!(
            golden("new", file).contains("a temporary class in $TMP"),
            "{file}"
        );
    }

    assert!(golden("new", "help-read.txt").contains("temporary class"));
    // The top level does not change: the footprint sits in the help of the subcommands.
    assert_eq!(
        minus_tran(&golden("old", "help-top.txt")),
        golden("new", "help-top.txt")
    );
}

// ---- the rule against the old branch (MCP schema) ------------------------------------------------

/// Text: no line that names `--prog`, and `TRAN` gone from the lists that remain.
fn minus_tran(old: &str) -> String {
    let mut text = old
        .lines()
        .filter(|line| !line.contains("--prog"))
        .map(|line| line.replace(", TRAN", "").replace("|TRAN", ""))
        .collect::<Vec<_>>()
        .join("\n");

    text.push('\n');
    text
}

fn remove_tran(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("prog");

            for (key, child) in map.iter_mut() {
                if key == "enum" {
                    if let Value::Array(items) = child {
                        items.retain(|item| item != "TRAN");
                    }
                } else if key == "description"
                    && let Value::String(description) = child
                {
                    *description = description.replace("|TRAN", "");
                } else {
                    remove_tran(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(remove_tran),
        _ => {}
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);

        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tools_list(write_offered: bool) -> Value {
    let mut server = Server::new(NoTools, write_offered);
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    let output = Capture::default();
    let mut log = Vec::new();

    serve(
        &mut server,
        std::io::Cursor::new(format!("{request}\n").into_bytes()),
        output.clone(),
        &mut log,
    )
    .unwrap();

    let printed = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let frame: Value = serde_json::from_str(printed.lines().next().unwrap()).unwrap();

    frame["result"]["tools"].clone()
}

#[test]
fn the_mcp_tool_list_is_the_golden_schema_and_the_old_schema_minus_tran_and_prog() {
    for (file, write_offered) in [
        ("mcp-tools-write.json", true),
        ("mcp-tools-read.json", false),
    ] {
        let tools = tools_list(write_offered);
        let expected: Value = serde_json::from_str(&golden("new", file)).unwrap();
        let mut old: Value = serde_json::from_str(&golden("old", file)).unwrap();

        assert_eq!(tools, expected, "{file}");

        remove_tran(&mut old);

        assert_eq!(old, expected, "{file}");
        assert_eq!(
            format!("{}\n", serde_json::to_string_pretty(&tools).unwrap()),
            golden("new", file),
            "{file}"
        );
    }

    let write = golden("old", "mcp-tools-write.json");

    assert!(
        write.contains("\"prog\"") && write.contains("\"TRAN\""),
        "the old schema had them"
    );
    assert!(!golden("new", "mcp-tools-write.json").contains("TRAN"));
}

// ---- the MCP type errors ------------------------------------------------------------------------

struct NoFactories;

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

impl Factories for NoFactories {
    fn adt(&mut self, _: &Connection) -> Opened {
        panic!("a type error opens no connection")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        panic!("a type error opens no connection")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("a type error probes no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("a type error pings nothing")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

fn mcp_text(tool: &str, arguments: Value) -> (bool, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");

    fs::write(
        &path,
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"adt":{"insecure":true},"policies":{"mcp_write":"token"}}]}"#,
    )
    .unwrap();

    let env = BTreeMap::from([
        ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ]);
    let config = Config::load(ConfigPaths::from_env(&env).unwrap(), &env).unwrap();
    let mut factories = NoFactories;
    let mut tools = McpTools::new(
        ReadTools::new(config, env, &mut factories),
        TokenGate::new(),
    );
    let result = tools.call(tool, arguments.as_object().unwrap());

    (result.is_error, result.text)
}

#[test]
fn tran_as_an_mcp_type_is_the_unknown_type_message_of_the_tool_without_tran() {
    let create = json!({"conn": "DEV_100", "type": "TRAN", "name": "ZT", "text": "t"});
    let read = json!({"conn": "DEV_100", "type": "TRAN", "name": "ZT"});
    let deploy = json!({"conn": "DEV_100", "type": "TRAN", "name": "ZT", "source": "x"});
    let delete = json!({"conn": "DEV_100", "objects": ["TRAN:ZT"]});

    for (tool, arguments, expected) in [
        (
            "abap_create_simulate",
            create.clone(),
            "STOP: type must be one of PROG, FUGR, FUNC, CLAS, INTF",
        ),
        (
            "abap_create",
            json!({"conn": "DEV_100", "type": "TRAN", "name": "ZT", "text": "t", "plan_token": "x"}),
            "STOP: type must be one of PROG, FUGR, FUNC, CLAS, INTF",
        ),
        (
            "abap_read_source",
            read,
            "STOP: type must be one of PROG, FUNC, CLAS, INTF",
        ),
        (
            "abap_simulate",
            deploy,
            "STOP: type must be one of PROG, FUNC, CLAS, INTF",
        ),
        (
            "abap_delete_simulate",
            delete,
            "STOP: error: invalid value 'TRAN:ZT'",
        ),
    ] {
        let (is_error, message) = mcp_text(tool, arguments);

        assert!(is_error, "{tool}");

        if tool == "abap_delete_simulate" {
            assert!(
                message.contains("delete target must be TYPE:NAME (PROG|FUGR|FUNC|CLAS|INTF)")
                    && !message.contains("|TRAN"),
                "{message}"
            );
        } else {
            assert_eq!(message, expected, "{tool}");
        }
    }
}
