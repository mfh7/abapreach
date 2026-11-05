//! The real `abapreach mcp` process over pipes: stdout holds protocol frames and nothing else,
//! stderr is deterministic and holds no input, host or credential. Every wait is bounded.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);
const PASSWORD: &str = "s3cret-pass-word";

/// Kills the child when a failed assertion or a timeout leaves the test early.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Finished {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });

    receiver
}

/// Runs `abapreach mcp`, feeds `input`, closes stdin and waits for the exit.
fn run_mcp(config: &str, input: &[u8]) -> Finished {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");

    std::fs::write(&path, config).unwrap();

    let mut child = Reaper(
        Command::new(env!("CARGO_BIN_EXE_abapreach"))
            .arg("mcp")
            .env("ABAP_CONNECTIONS", &path)
            .env("DEV_100_USER", "user")
            .env("DEV_100_PASS", PASSWORD)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = drain(child.0.stdout.take().unwrap());
    let stderr = drain(child.0.stderr.take().unwrap());
    let mut stdin = child.0.stdin.take().unwrap();

    stdin.write_all(input).unwrap();
    drop(stdin);

    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status.code();
        }

        assert!(start.elapsed() < DEADLINE, "the MCP process did not exit");
        sleep(POLL);
    };

    Finished {
        code,
        stdout: stdout.recv_timeout(DEADLINE).unwrap(),
        stderr: stderr.recv_timeout(DEADLINE).unwrap(),
    }
}

fn config(policies: &str) -> String {
    format!(
        r#"{{"policies":{policies},"servers":[{{"systemId":"DEV","clients":["100"],"rfc":{{"ashost":"secret-host.example.com","sysnr":"00"}}}}]}}"#
    )
}

fn frames(stdout: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(stdout).expect("stdout is UTF-8");

    assert!(text.is_empty() || text.ends_with('\n'), "{text:?}");

    text.lines()
        .map(|line| {
            let frame: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("stdout line is no JSON: {line:?}: {error}"));

            assert_eq!(frame["jsonrpc"], "2.0", "{line}");
            frame
        })
        .collect()
}

const SESSION: &str = concat!(
    r#"{"jsonrpc":"2.0","id":0,"method":"server/discover","params":{"_meta":{"protocolVersion":"2026-07-28"}}}"#,
    "\n",
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"elicitation":{"form":{},"url":{}}},"clientInfo":{"name":"t","version":"0"}}}"#,
    "\n",
    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    "\n",
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    "\n",
    "not json at all\n",
    r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
    "\n",
);

#[test]
fn stdout_holds_only_valid_frames_and_the_process_exits_0_on_eof() {
    let finished = run_mcp(&config("{}"), SESSION.as_bytes());
    let out = frames(&finished.stdout);

    assert_eq!(finished.code, Some(0));
    assert_eq!(
        out.iter()
            .map(|frame| frame["id"].clone())
            .collect::<Vec<_>>(),
        [json!(0), json!(1), json!(2), Value::Null, json!(3)]
    );
    assert_eq!(out[0]["error"]["code"], -32601);
    assert_eq!(out[1]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(out[2]["result"]["tools"].as_array().unwrap().len(), 2);
    assert_eq!(out[3]["error"]["code"], -32700);
    assert_eq!(out[4]["result"], serde_json::json!({}));
}

#[test]
fn stderr_is_deterministic_and_leaks_no_input_host_or_credential() {
    let finished = run_mcp(
        &config("{}"),
        format!(
            "{SESSION}{}\n",
            r#"{"jsonrpc":"2.0","id":9,"method":"leak/secret-host.example.com","params":{"password":"s3cret-pass-word"}}"#
        )
        .as_bytes(),
    );
    let stderr = String::from_utf8(finished.stderr).unwrap();
    let stdout = String::from_utf8(finished.stdout).unwrap();

    assert_eq!(stderr, "abapreach-mcp: start\nabapreach-mcp: stop\n");
    assert!(!stdout.contains(PASSWORD));
    assert!(!stdout.contains("secret-host"), "{stdout}");
}

#[test]
fn an_invalid_policy_stops_with_exit_2_and_an_empty_stdout() {
    let finished = run_mcp(&config(r#"{"mcp_write":"always"}"#), SESSION.as_bytes());

    assert_eq!(finished.code, Some(2));
    assert!(finished.stdout.is_empty());
    assert!(
        String::from_utf8(finished.stderr)
            .unwrap()
            .starts_with("STOP: cannot parse")
    );
}

#[test]
fn write_tools_are_listed_only_when_a_server_allows_mcp_write() {
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let count = |policies: &str| {
        let finished = run_mcp(&config(policies), format!("{list}\n").as_bytes());

        frames(&finished.stdout)[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .len()
    };

    assert_eq!(count("{}"), 2);
    assert_eq!(count(r#"{"mcp_write":"token"}"#), 8);
}
