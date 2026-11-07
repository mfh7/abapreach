//! The real binary, interrupted by a real signal while it waits on a local server that never
//! answers. No lock is held, so every termination signal must end it at once with exit 130.
//! Lock-held interruption is covered in `interrupt_test.rs`; Windows console events are verified
//! on the Windows hosts of the live matrix.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);

/// Kills the child when a failed assertion or a timeout leaves the test early.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();

    loop {
        if let Some(value) = check() {
            return value;
        }

        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        sleep(POLL);
    }
}

fn interrupted_with(signal: &str) -> Option<i32> {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    listener.set_nonblocking(true).unwrap();
    std::fs::write(
        directory.path().join("connections.json"),
        format!(
            concat!(
                r#"{{"servers":[{{"systemId":"DEV","host":"127.0.0.1","port":{},"clients":["100"],"#,
                r#""rfc":{{"ashost":"example.com","sysnr":"00"}},"adt":{{"scheme":"http"}}}}]}}"#
            ),
            port
        ),
    )
    .unwrap();

    let mut child = Reaper(
        Command::new(env!("CARGO_BIN_EXE_abapreach"))
            .args(["read", "PROG", "YTEST_ABAPREACH01"])
            .arg(directory.path().join("out.abap"))
            .args(["--conn", "DEV_100"])
            .env(
                "ABAP_CONNECTIONS",
                directory.path().join("connections.json"),
            )
            .env("DEV_100_USER", "user")
            .env("DEV_100_PASS", "password")
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // The connection arrives only after the handler is installed and the request is in flight.
    let _held = wait_until("the binary to connect", || {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("the binary ended before it connected: {status}");
        }

        listener.accept().ok()
    });

    let sent = Command::new("kill")
        .args([format!("-{signal}"), child.0.id().to_string()])
        .status()
        .unwrap();

    assert!(sent.success());

    wait_until("the binary to exit", || child.0.try_wait().unwrap()).code()
}

#[test]
fn sigint_without_a_lock_exits_130() {
    assert_eq!(interrupted_with("INT"), Some(130));
}

#[test]
fn sigterm_without_a_lock_exits_130() {
    assert_eq!(interrupted_with("TERM"), Some(130));
}

#[test]
fn sighup_without_a_lock_exits_130() {
    assert_eq!(interrupted_with("HUP"), Some(130));
}

/// A server whose first reply opens a stateful session (context cookie) and whose later replies
/// never come, except the answer to a logoff. It lists the request lines it received.
fn session_server(listener: TcpListener, seen: Arc<Mutex<Vec<String>>>) {
    let answered = Arc::new(Mutex::new(false));

    for stream in listener.incoming().flatten() {
        let (seen, answered) = (seen.clone(), answered.clone());

        // Detached: a connection held open for the rest of the test ends with the process.
        thread::spawn(move || serve_connection(stream, &seen, &answered));
    }
}

fn serve_connection(mut stream: TcpStream, seen: &Mutex<Vec<String>>, answered: &Mutex<bool>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    loop {
        let mut line = String::new();
        let mut request_line = None;

        // One request head: the request line, the headers, an empty line.
        loop {
            line.clear();

            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }

            if request_line.is_none() {
                request_line = Some(line.trim_end().to_owned());
            }

            if line == "\r\n" {
                break;
            }
        }

        let request_line = request_line.unwrap();

        seen.lock().unwrap().push(request_line.clone());

        let first = !std::mem::replace(&mut *answered.lock().unwrap(), true);

        if request_line.contains("/sap/public/bc/icf/logoff") {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        } else if first {
            let _ = stream.write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\nSet-Cookie: sap-contextid=ctx; Path=/\r\n",
                    "Content-Length: 9\r\n\r\nREPORT x."
                )
                .as_bytes(),
            );
        }
    }
}

/// The binary holds an open ADT session (first reply seen) and waits for the second reply when
/// `signal` arrives. Returns its exit code and the request lines the server saw.
fn interrupted_session_with(signal: &str) -> (Option<i32>, Vec<String>) {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));

    std::fs::write(
        directory.path().join("connections.json"),
        format!(
            concat!(
                r#"{{"servers":[{{"systemId":"DEV","host":"127.0.0.1","port":{},"clients":["100"],"#,
                r#""rfc":{{"ashost":"example.com","sysnr":"00"}},"adt":{{"scheme":"http"}}}}]}}"#
            ),
            port
        ),
    )
    .unwrap();
    std::fs::write(directory.path().join("src.abap"), "REPORT x.\nWRITE 'y'.\n").unwrap();

    let server_seen = seen.clone();

    thread::spawn(move || session_server(listener, server_seen));

    let mut child = Reaper(
        Command::new(env!("CARGO_BIN_EXE_abapreach"))
            .args(["deploy", "PROG", "YTEST_ABAPREACH01"])
            .arg(directory.path().join("src.abap"))
            .args(["--simulate", "--conn", "DEV_100"])
            .env(
                "ABAP_CONNECTIONS",
                directory.path().join("connections.json"),
            )
            .env("DEV_100_USER", "user")
            .env("DEV_100_PASS", "password")
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Two requests: the first opened the session, the second is unanswered.
    wait_until("the second request", || {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("the binary ended before the second request: {status}");
        }

        (seen.lock().unwrap().len() >= 2).then_some(())
    });

    let sent = Command::new("kill")
        .args([format!("-{signal}"), child.0.id().to_string()])
        .status()
        .unwrap();

    assert!(sent.success());

    let code = wait_until("the binary to exit", || child.0.try_wait().unwrap()).code();
    let lines = seen.lock().unwrap().clone();

    (code, lines)
}

#[test]
fn every_termination_signal_logs_an_open_session_off_before_exit_130() {
    for signal in ["INT", "TERM", "HUP"] {
        let (code, lines) = interrupted_session_with(signal);
        let logoffs = lines
            .iter()
            .filter(|line| line.starts_with("GET /sap/public/bc/icf/logoff"))
            .count();

        assert_eq!(code, Some(130), "{signal}: {lines:?}");
        assert_eq!(logoffs, 1, "{signal}: {lines:?}");
    }
}
