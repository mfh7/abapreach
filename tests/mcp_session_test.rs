//! The MCP server against a fake client that is a second thread: elicitation (accept, decline,
//! timeout, late and foreign answers, no capability), cancellation, EOF and SIGTERM around a
//! write, the busy rule and reads that run while a confirmation is open. Every wait is bounded:
//! a hang is a failed test, never a stuck one.

mod common;
mod mcprig;
mod replay;

use abapreach::mcp::{Server, serve_with};
use abapreach::mcp_write::Limits;
use abapreach::operation::deploy::{CancelFlag, InterruptAction};
use abapreach::transport::Request;
use mcprig::*;
use replay::{Step, fx, no_inactive, replay};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const FIXTURE: &str = "dev-750-rfc.json";
const WAIT: Duration = Duration::from_secs(10);
const NAME: &str = "YTEST_ABAPREACH01";

// ---- the wire --------------------------------------------------------------------------------

/// The write end of the server's stdin; every holder can close it.
#[derive(Clone)]
struct Wire(Arc<Mutex<Option<Sender<Vec<u8>>>>>);

impl Wire {
    fn send(&self, message: &Value) {
        if let Some(sender) = self.0.lock().unwrap().as_ref() {
            let _ = sender.send(format!("{message}\n").into_bytes());
        }
    }

    fn close(&self) {
        self.0.lock().unwrap().take();
    }
}

struct ChannelRead {
    receiver: Receiver<Vec<u8>>,
    chunk: Vec<u8>,
    position: usize,
}

impl Read for ChannelRead {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.chunk.len() {
            match self.receiver.recv() {
                Ok(chunk) => {
                    self.chunk = chunk;
                    self.position = 0;
                }
                Err(_) => return Ok(0),
            }
        }

        let count = out.len().min(self.chunk.len() - self.position);

        out[..count].copy_from_slice(&self.chunk[self.position..self.position + count]);
        self.position += count;

        Ok(count)
    }
}

/// Server stdout: every line must be one JSON-RPC frame.
struct ChannelWrite {
    frames: Sender<Value>,
    pending: Vec<u8>,
}

impl Write for ChannelWrite {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(bytes);

        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let frame: Value = serde_json::from_slice(&line)
                .unwrap_or_else(|error| panic!("stdout holds a non-frame line: {error}"));

            assert_eq!(frame["jsonrpc"], "2.0");
            let _ = self.frames.send(frame);
        }

        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Client {
    wire: Wire,
    frames: Receiver<Value>,
    waiting: VecDeque<Value>,
}

impl Client {
    fn send(&self, message: Value) {
        self.wire.send(&message);
    }

    fn request(&self, id: u64, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    fn call(&self, id: u64, name: &str, arguments: Value) {
        self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
    }

    fn initialize(&mut self, version: &str, elicitation: bool) {
        let capabilities = if elicitation {
            json!({"elicitation": {"form": {}, "url": {}}})
        } else {
            json!({"roots": {}})
        };

        self.request(
            1,
            "initialize",
            json!({"protocolVersion": version, "capabilities": capabilities,
                   "clientInfo": {"name": "t", "version": "0"}}),
        );
        self.take(|frame| frame["id"] == 1);
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    /// The first frame (queued or arriving) that `matches`, within the bound.
    fn take(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        if let Some(position) = self.waiting.iter().position(&matches) {
            return self.waiting.remove(position).unwrap();
        }

        let end = Instant::now() + WAIT;

        loop {
            let left = end
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| {
                    panic!("no matching frame in {WAIT:?}; waiting: {:?}", self.waiting)
                });
            let frame = self.frames.recv_timeout(left).unwrap_or_else(|_| {
                panic!("no matching frame in {WAIT:?}; waiting: {:?}", self.waiting)
            });

            if matches(&frame) {
                return frame;
            }

            self.waiting.push_back(frame);
        }
    }

    fn reply_to(&mut self, id: u64) -> Value {
        self.take(|frame| frame["id"] == id && frame.get("method").is_none())
    }

    fn elicitation(&mut self) -> Value {
        self.take(|frame| frame["method"] == "elicitation/create")
    }

    fn answer(&self, ask: &Value, action: &str, typed: Option<&str>) {
        let mut result = json!({"action": action});

        if let Some(typed) = typed {
            result["content"] = json!({"confirm": typed});
        }

        self.send(json!({"jsonrpc": "2.0", "id": ask["id"], "result": result}));
    }

    fn ping(&mut self, id: u64) -> Value {
        self.request(id, "ping", json!({}));
        self.reply_to(id)
    }
}

struct Run {
    log: String,
    /// The watchdog had to close the input: the server did not end on its own.
    watchdog_fired: bool,
}

fn wire() -> (Wire, ChannelRead) {
    let (sender, receiver) = mpsc::channel();

    (
        Wire(Arc::new(Mutex::new(Some(sender)))),
        ChannelRead {
            receiver,
            chunk: Vec::new(),
            position: 0,
        },
    )
}

/// Runs the server on this thread against `script`, which plays the client on another one. A
/// watchdog closes the input after the bound, so a server that fails to end cannot hang a test.
fn serve(
    rig: &mut Rig,
    cancel: &CancelFlag,
    (wire, input): (Wire, ChannelRead),
    script: impl FnOnce(&mut Client) + Send,
) -> Run {
    let writable = rig.config().mcp_writable();
    let (frames_sender, frames) = mpsc::channel();
    let mut server = Server::new(rig.tools(), false).with_writable(writable);
    let mut log = Vec::new();
    let finished = Arc::new(AtomicBool::new(false));
    let fired = Arc::new(AtomicBool::new(false));

    thread::scope(|scope| {
        let client_wire = wire.clone();

        scope.spawn(move || {
            script(&mut Client {
                wire: client_wire,
                frames,
                waiting: VecDeque::new(),
            });
        });

        let (watched, done, flag) = (wire.clone(), finished.clone(), fired.clone());

        scope.spawn(move || {
            let end = Instant::now() + Duration::from_secs(30);

            while Instant::now() < end && !done.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(20));
            }

            if !done.load(Ordering::SeqCst) {
                flag.store(true, Ordering::SeqCst);
                watched.close();
            }
        });

        serve_with(
            &mut server,
            BufReader::new(input),
            ChannelWrite {
                frames: frames_sender,
                pending: Vec::new(),
            },
            &mut log,
            cancel.clone(),
        )
        .unwrap();

        finished.store(true, Ordering::SeqCst);
    });
    drop(server);

    Run {
        log: String::from_utf8(log).unwrap(),
        watchdog_fired: fired.load(Ordering::SeqCst),
    }
}

/// Blocks until `condition` holds, within the bound.
fn wait_until(condition: impl Fn() -> bool) {
    let end = Instant::now() + WAIT;

    while !condition() {
        assert!(Instant::now() < end, "the condition never held");
        thread::sleep(Duration::from_millis(5));
    }
}

fn simulate_steps() -> Vec<Step> {
    vec![
        fx(6),
        no_inactive(),
        fx(7),
        fx(6),
        no_inactive(),
        fx(8),
        fx(9),
    ]
}

fn execute_steps(tail: &[usize]) -> Vec<Step> {
    let mut steps = vec![
        fx(6),
        no_inactive(),
        fx(7),
        fx(6),
        no_inactive(),
        fx(8),
        fx(17),
    ];

    steps.extend(tail.iter().map(|index| fx(*index)));
    steps
}

fn text_of(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Simulates through the session and returns the token.
fn simulate(client: &mut Client, id: u64) -> String {
    client.call(id, "abap_simulate", deploy_arguments());

    let reply = client.reply_to(id);

    reply["result"]["structuredContent"]["plan_token"]
        .as_str()
        .unwrap_or_else(|| panic!("no token: {reply}"))
        .to_owned()
}

fn deploy(client: &Client, id: u64, token: &str) {
    client.call(id, "abap_deploy", with_token(deploy_arguments(), token));
}

const CONFIRM: &str = r#"{"mcp_write":"confirm"}"#;

fn confirm_rig(requests: &Log, second: Vec<Step>) -> Rig {
    let mut rig = rig(CONFIRM);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), requests));
    rig.adt(recording(replay(FIXTURE, second), requests));
    rig
}

// ---- capability ------------------------------------------------------------------------------

#[test]
fn a_client_without_elicitation_cannot_confirm_and_nothing_is_written() {
    let requests = log();
    let mut rig = confirm_rig(&requests, Vec::new());
    let run = serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", false);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let reply = client.reply_to(4);
        let text = text_of(&reply);

        assert_eq!(reply["result"]["isError"], true);
        assert!(text.contains("elicitation"), "{text}");
        assert!(text.contains("mcp_write"), "{text}");
        client.wire.close();
    });

    assert!(!run.watchdog_fired);
    assert!(!any(&requests, "PUT "));
}

#[test]
fn a_protocol_older_than_elicitation_cannot_confirm_even_with_the_capability() {
    let requests = log();
    let mut rig = confirm_rig(&requests, Vec::new());

    serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-03-26", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let text = text_of(&client.reply_to(4));

        assert!(text.contains("2025-06-18"), "{text}");
        client.wire.close();
    });

    assert!(!any(&requests, "PUT "));
}

// ---- the human answers -----------------------------------------------------------------------

#[test]
fn the_server_asks_with_a_form_of_one_required_string_field_and_writes_on_accept() {
    let requests = log();
    let mut rig = confirm_rig(&requests, execute_steps(&[18, 19, 20]));
    let run = serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let ask = client.elicitation();
        let schema = &ask["params"]["requestedSchema"];

        assert_eq!(ask["params"]["mode"], "form");
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["confirm"]));
        assert_eq!(schema["properties"].as_object().unwrap().len(), 1);
        assert_eq!(schema["properties"]["confirm"]["type"], "string");
        assert!(
            schema["properties"]["confirm"].get("default").is_none(),
            "never prefilled"
        );
        assert!(ask["params"]["message"].as_str().unwrap().contains(NAME));

        client.answer(&ask, "accept", Some(NAME));

        let reply = client.reply_to(4);

        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(reply["result"]["structuredContent"]["status"], "ACTIVE");
        client.wire.close();
    });

    assert!(!run.watchdog_fired);
    assert!(any(&requests, "PUT "));
}

#[test]
fn a_decline_writes_nothing_and_the_server_keeps_serving() {
    let requests = log();
    let mut rig = confirm_rig(&requests, Vec::new());

    serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let ask = client.elicitation();

        client.answer(&ask, "decline", None);

        let text = text_of(&client.reply_to(4));

        assert!(text.contains("not confirmed"), "{text}");
        assert_eq!(client.ping(5)["result"], json!({}));
        client.wire.close();
    });

    assert!(!any(&requests, "PUT "));
}

#[test]
fn an_answer_to_another_request_id_is_ignored() {
    let requests = log();
    let mut rig = confirm_rig(&requests, Vec::new());

    serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let ask = client.elicitation();

        // An accept for an id the server never used must not confirm anything.
        client.send(json!({"jsonrpc": "2.0", "id": "someone-else",
                           "result": {"action": "accept", "content": {"confirm": NAME}}}));
        client.answer(&ask, "decline", None);

        let text = text_of(&client.reply_to(4));

        assert!(text.contains("not confirmed"), "{text}");
        client.wire.close();
    });

    assert!(!any(&requests, "PUT "));
}

#[test]
fn a_timeout_declines_and_a_late_accept_writes_nothing() {
    let requests = log();
    let mut rig = confirm_rig(&requests, Vec::new());

    rig.limits = Limits {
        elicit: Duration::from_millis(200),
        ..Limits::default()
    };

    serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let ask = client.elicitation();
        let text = text_of(&client.reply_to(4));

        assert!(text.contains("not confirmed"), "{text}");
        assert!(text.contains("timed out"), "{text}");

        // The human answers after the server gave up: the form was still on the screen.
        client.answer(&ask, "accept", Some(NAME));
        assert_eq!(client.ping(5)["result"], json!({}));
        client.wire.close();
    });

    assert!(!any(&requests, "PUT "));
}

#[test]
fn the_confirmation_wait_is_outside_the_tool_deadline() {
    let requests = log();
    let mut rig = confirm_rig(&requests, execute_steps(&[18, 19, 20]));

    // The deadline would have passed during the wait; the clock starts after the answer.
    rig.limits = Limits {
        call: Duration::from_millis(400),
        ..Limits::default()
    };

    serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let ask = client.elicitation();

        thread::sleep(Duration::from_millis(600));
        client.answer(&ask, "accept", Some(NAME));

        let reply = client.reply_to(4);

        assert_eq!(reply["result"]["isError"], false, "{reply}");
        client.wire.close();
    });
}

// ---- cancellation ----------------------------------------------------------------------------

#[test]
fn a_cancel_notification_while_the_form_is_open_stops_the_write_and_never_the_process() {
    let requests = log();
    let mut rig = confirm_rig(&requests, Vec::new());
    let run = serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);
        client.elicitation();
        client.send(
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                           "params": {"requestId": 4, "reason": "user"}}),
        );

        let text = text_of(&client.reply_to(4));

        assert!(text.contains("not confirmed"), "{text}");
        assert_eq!(client.ping(5)["result"], json!({}));
        client.wire.close();
    });

    assert!(!run.watchdog_fired);
    assert!(!any(&requests, "PUT "));
}

#[test]
fn a_cancel_after_the_lock_unlocks_and_sends_no_write() {
    let requests = log();
    let cancel = CancelFlag::new();
    let (wire, input) = wire();
    let (hook_wire, hook_cancel) = (wire.clone(), cancel.clone());
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));
    rig.adt(recording(
        Hooked {
            inner: replay(FIXTURE, vec![fx(6), no_inactive(), fx(7), fx(9)]),
            hook: Box::new(move |request: &Request| {
                if request.uri.contains("_action=LOCK") {
                    hook_wire.send(
                        &json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                                           "params": {"requestId": 4}}),
                    );
                    wait_until(|| hook_cancel.is_set());
                }
            }),
        },
        &requests,
    ));

    let run = serve(&mut rig, &cancel, (wire, input), |client| {
        client.initialize("2025-11-25", false);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let reply = client.reply_to(4);

        assert_eq!(reply["result"]["isError"], true);
        assert!(text_of(&reply).contains("CANCELED"), "{}", text_of(&reply));
        assert_eq!(client.ping(5)["result"], json!({}));
        client.wire.close();
    });

    assert!(!run.watchdog_fired);
    assert!(any(&requests, "_action=UNLOCK"));
    assert!(!any(&requests, "PUT "));
}

#[test]
fn eof_after_the_put_unlocks_activates_nothing_and_logs_partial() {
    let requests = log();
    let cancel = CancelFlag::new();
    let (wire, input) = wire();
    let (hook_wire, hook_cancel) = (wire.clone(), cancel.clone());
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));
    rig.adt(recording(
        Hooked {
            inner: replay(FIXTURE, execute_steps(&[18])),
            hook: Box::new(move |request: &Request| {
                if request.method == "PUT" {
                    hook_wire.close();
                    wait_until(|| hook_cancel.is_set());
                }
            }),
        },
        &requests,
    ));

    let run = serve(&mut rig, &cancel, (wire, input), |client| {
        client.initialize("2025-11-25", false);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let reply = client.reply_to(4);

        assert!(text_of(&reply).contains("PARTIAL"), "{}", text_of(&reply));
    });

    assert!(
        !run.watchdog_fired,
        "EOF ends the server once the write settled"
    );
    assert!(any(&requests, "_action=UNLOCK"));
    assert!(!any(&requests, "activation"));
    assert!(
        run.log
            .contains("abapreach-mcp: abap_deploy exit 130 status PARTIAL"),
        "{}",
        run.log
    );
}

#[test]
fn sigterm_in_the_write_window_defers_unlocks_activates_nothing_and_ends_the_server() {
    let requests = log();
    let cancel = CancelFlag::new();
    let (wire, input) = wire();
    let hook_cancel = cancel.clone();
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));
    rig.adt(recording(
        Hooked {
            inner: replay(FIXTURE, execute_steps(&[18])),
            hook: Box::new(move |request: &Request| {
                if request.method == "PUT" {
                    // What the signal handler of `main` does.
                    assert_eq!(hook_cancel.on_interrupt(), InterruptAction::Deferred);
                }
            }),
        },
        &requests,
    ));

    // The client never closes its input: only the shutdown can end the loop.
    let run = serve(&mut rig, &cancel, (wire, input), |client| {
        client.initialize("2025-11-25", false);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        assert!(text_of(&client.reply_to(4)).contains("PARTIAL"));
    });

    assert!(
        !run.watchdog_fired,
        "the shutdown ends the loop without an EOF"
    );
    assert!(any(&requests, "_action=UNLOCK"));
    assert!(!any(&requests, "activation"));
}

#[test]
fn eof_while_the_form_is_open_writes_nothing_and_ends_the_server() {
    let requests = log();
    let mut rig = confirm_rig(&requests, Vec::new());
    let run = serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);
        client.elicitation();
        client.wire.close();
    });

    assert!(!run.watchdog_fired);
    assert!(!any(&requests, "PUT "));
}

// ---- one write, many reads -------------------------------------------------------------------

#[test]
fn a_second_write_is_busy_and_a_read_runs_over_its_own_connection_while_the_form_is_open() {
    let requests = log();
    let mut rig = rig(CONFIRM);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));
    rig.adt(Script::new(&requests).get(
        "/sap/bc/adt/programs/programs/ytest/source/main?version=active",
        200,
        "REPORT ytest.",
    ));

    serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", true);

        let token = simulate(client, 3);

        deploy(client, 4, &token);

        let ask = client.elicitation();

        // Another write while one waits for its human: refused at once.
        client.call(5, "abap_simulate", deploy_arguments());

        let busy = client.reply_to(5);

        assert_eq!(busy["result"]["isError"], true);
        assert!(text_of(&busy).contains("busy"), "{}", text_of(&busy));

        // A read is served while the form is open, on a connection of its own.
        client.call(
            6,
            "abap_read_source",
            json!({"conn": "DEV_100", "type": "PROG", "name": "YTEST"}),
        );

        let read = client.reply_to(6);

        assert_eq!(read["result"]["isError"], false, "{read}");
        assert_eq!(
            read["result"]["structuredContent"]["source"],
            "REPORT ytest."
        );

        client.answer(&ask, "decline", None);
        client.reply_to(4);
        client.wire.close();
    });

    // The simulation and the read each opened a connection; the declined write opened none.
    assert_eq!(rig.opened(), (2, 0));
}

#[test]
fn a_write_after_the_first_ended_is_not_busy() {
    let requests = log();
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));
    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));

    serve(&mut rig, &CancelFlag::new(), wire(), |client| {
        client.initialize("2025-11-25", false);
        simulate(client, 3);
        simulate(client, 4);
        client.wire.close();
    });
}

#[test]
fn the_next_tool_result_carries_a_w_line_about_a_cancelled_write() {
    let requests = log();
    let cancel = CancelFlag::new();
    let (wire, input) = wire();
    let (hook_wire, hook_cancel) = (wire.clone(), cancel.clone());
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));
    rig.adt(recording(
        Hooked {
            inner: replay(FIXTURE, execute_steps(&[18])),
            hook: Box::new(move |request: &Request| {
                if request.method == "PUT" {
                    hook_wire.send(
                        &json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                                           "params": {"requestId": 4}}),
                    );
                    wait_until(|| hook_cancel.is_set());
                }
            }),
        },
        &requests,
    ));

    serve(&mut rig, &cancel, (wire, input), |client| {
        client.initialize("2025-11-25", false);

        let token = simulate(client, 3);

        deploy(client, 4, &token);
        client.reply_to(4);

        // Any later tool result names the outcome of the write the client gave up on.
        client.call(5, "abap_simulate", json!({"conn": "DEV_100"}));

        let next = client.reply_to(5);
        let text = text_of(&next);

        assert!(
            text.lines().any(|line| line.starts_with("W: abap_deploy")),
            "{text}"
        );
        assert!(next["result"]["structuredContent"]["warnings"].is_array());
        client.wire.close();
    });
}

#[test]
fn eof_before_the_put_unlocks_and_sends_no_write() {
    let requests = log();
    let cancel = CancelFlag::new();
    let (wire, input) = wire();
    let (hook_wire, hook_cancel) = (wire.clone(), cancel.clone());
    let mut rig = rig(r#"{"mcp_write":"token"}"#);

    rig.adt(recording(replay(FIXTURE, simulate_steps()), &requests));
    rig.adt(recording(
        Hooked {
            inner: replay(FIXTURE, vec![fx(6), no_inactive(), fx(7), fx(9)]),
            hook: Box::new(move |request: &Request| {
                if request.uri.contains("_action=LOCK") {
                    hook_wire.close();
                    wait_until(|| hook_cancel.is_set());
                }
            }),
        },
        &requests,
    ));

    let run = serve(&mut rig, &cancel, (wire, input), |client| {
        client.initialize("2025-11-25", false);

        let token = simulate(client, 3);

        deploy(client, 4, &token);
        client.reply_to(4);
    });

    assert!(!run.watchdog_fired);
    assert!(any(&requests, "_action=UNLOCK"));
    assert!(!any(&requests, "PUT "));
    assert!(
        run.log.contains("abapreach-mcp: abap_deploy exit 130"),
        "{}",
        run.log
    );
}
