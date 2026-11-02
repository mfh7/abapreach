//! The real binary, interrupted by a real signal while it waits on a local server that never
//! answers. No lock is held, so every termination signal must end it at once with exit 130.
//! Lock-held interruption is covered in `interrupt_test.rs`; Windows console events are verified
//! on the Windows hosts of the live matrix.
#![cfg(unix)]

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
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
