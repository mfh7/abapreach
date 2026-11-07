mod common;
mod tls_support;

use abapreach::transport::http::{HttpConfig, HttpTransport};
use abapreach::transport::{
    Request, Response, SadtTransport, TransportError, is_authorization_failure,
};
use rustls::crypto::aws_lc_rs::default_provider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tls_support::TlsFiles;

const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
const REQUIRED: &str =
    "HTTP/1.1 403 Forbidden\r\nx-csrf-token: Required\r\nContent-Length: 0\r\n\r\n";
const TOKEN: &str = "HTTP/1.1 200 OK\r\nx-csrf-token: tok\r\nContent-Length: 0\r\n\r\n";
const COOKIES: &str = concat!(
    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n",
    "Set-Cookie: a=1; Path=/\r\nSet-Cookie: b=2; Path=/\r\n\r\n"
);

fn config(base_url: &str, timeout_ms: u64) -> HttpConfig {
    HttpConfig {
        base_url: base_url.to_owned(),
        client: "100".to_owned(),
        username: "user".to_owned(),
        password: "password".to_owned(),
        ca_file: None,
        insecure: false,
        timeout: Duration::from_millis(timeout_ms),
    }
}

fn read_request(stream: impl Read) -> Option<String> {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    let mut length = 0;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
        let end = line == "\r\n";
        head.push_str(&line);
        if end {
            break;
        }
    }

    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    head.push_str(&String::from_utf8(body).unwrap());
    Some(head)
}

fn serve(replies: Vec<&'static str>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut heads = Vec::new();
        let mut replies = replies.into_iter();

        'connections: loop {
            let (mut stream, _) = listener.accept().unwrap();
            while let Some(head) = read_request(&stream) {
                heads.push(head);
                let Some(reply) = replies.next() else {
                    break 'connections;
                };
                stream.write_all(reply.as_bytes()).unwrap();
                if replies.len() == 0 {
                    break 'connections;
                }
            }
        }

        heads
    });

    (base_url, handle)
}

fn serve_silent(hold: Duration) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        read_request(&stream);
        thread::sleep(hold);
    });

    (base_url, handle)
}

/// A reply that never arrived is a connectivity failure, never an uncertain write.
fn assert_no_response(error: &TransportError) {
    assert!(matches!(error, TransportError::NoResponse(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with("no response to a read-only request: "),
        "{error}"
    );
}

#[test]
fn stateful() {
    let mut transport = HttpTransport::for_test(vec![Ok(Response::empty(200)); 4]);

    for uri in ["/lock", "/put", "/unlock"] {
        transport.request(Request::get(uri)).unwrap();
    }

    transport.close().unwrap();
    transport.close().unwrap();

    let recorded = transport.recorded();

    assert_eq!(recorded.len(), 4);
    for request in &recorded[..3] {
        assert_eq!(request.headers["X-sap-adt-sessiontype"], "stateful");
        assert_eq!(request.headers["Cookie"], "sid=test");
    }
    assert_eq!(recorded[3].headers["X-sap-adt-sessiontype"], "stateless");
}

#[test]
fn a_forbidden_unlock_still_ends_an_open_session() {
    let mut transport = HttpTransport::for_test(vec![
        Ok(Response::empty(200)),
        Ok(Response::empty(403)),
        Ok(Response::empty(200)),
    ]);

    transport
        .request(Request::post("/lock", Vec::new()))
        .unwrap();
    assert_eq!(
        transport
            .request(Request::post("/unlock", Vec::new()))
            .unwrap()
            .status,
        403
    );
    transport.close().unwrap();

    assert_eq!(transport.recorded().len(), 3);
    assert_eq!(
        transport.recorded()[2].headers["X-sap-adt-sessiontype"],
        "stateless"
    );
}

const LOGOFF_URI: &str = "/sap/public/bc/icf/logoff";

#[test]
fn close_sends_exactly_one_logoff_for_an_open_session() {
    let mut transport = HttpTransport::for_test(vec![Ok(Response::empty(200)); 3]);

    transport.request(Request::get("/lock")).unwrap();
    transport.close().unwrap();
    transport.close().unwrap();

    let recorded = transport.recorded();

    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1].method, "GET");
    assert_eq!(recorded[1].uri, LOGOFF_URI);
}

#[test]
fn the_logoff_carries_no_csrf_token() {
    let mut required = Response::empty(403);
    required
        .headers
        .insert("x-csrf-token".to_owned(), "Required".to_owned());
    let mut token = Response::empty(200);
    token
        .headers
        .insert("x-csrf-token".to_owned(), "secret-token".to_owned());
    let mut transport = HttpTransport::for_test(vec![
        Ok(required),
        Ok(token),
        Ok(Response::empty(200)),
        Ok(Response::empty(200)),
    ]);

    transport.request(Request::post("/x", Vec::new())).unwrap();
    transport.close().unwrap();

    let recorded = transport.recorded();

    assert_eq!(recorded[2].headers["x-csrf-token"], "secret-token");
    assert_eq!(recorded[3].uri, LOGOFF_URI);
    assert!(!recorded[3].headers.contains_key("x-csrf-token"));
}

#[test]
fn a_logoff_the_server_refuses_is_an_error_and_is_not_retried() {
    let mut transport =
        HttpTransport::for_test(vec![Ok(Response::empty(200)), Ok(Response::empty(500))]);

    transport.request(Request::get("/lock")).unwrap();

    assert_eq!(
        transport.close(),
        Err(TransportError::Http {
            status: 500,
            message: "HTTP request failed".to_owned()
        })
    );
    assert_eq!(transport.close(), Ok(()));
    assert_eq!(transport.recorded().len(), 2);
}

#[test]
fn a_connect_failure_opens_no_session_and_closing_sends_nothing() {
    let mut transport = HttpTransport::for_test(vec![Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    ))]);

    assert!(transport.request(Request::get("/lock")).is_err());
    transport.close().unwrap();

    assert_eq!(transport.recorded().len(), 1);
}

#[test]
fn a_first_request_unauthorized_does_not_end_a_session() {
    let mut transport = HttpTransport::for_test(vec![Ok(Response::empty(401))]);

    assert_eq!(
        transport
            .request(Request::get("/discovery"))
            .unwrap()
            .status,
        401
    );
    transport.close().unwrap();

    assert_eq!(transport.recorded().len(), 1);
}

#[test]
fn a_failed_session_end_is_reported_and_not_retried() {
    let failure = TransportError::PostSendUncertain("HTTP timeout after request send".to_owned());
    let mut transport =
        HttpTransport::for_test(vec![Ok(Response::empty(200)), Err(failure.clone())]);

    transport.request(Request::get("/lock")).unwrap();

    assert_eq!(transport.close(), Err(failure));
    assert_eq!(transport.close(), Ok(()));
    assert_eq!(transport.recorded().len(), 2);
}

#[test]
fn unauthorized_doctor_read_and_deploy_requests_do_not_end_a_session() {
    let mut unauthorized = Response::empty(401);
    unauthorized
        .headers
        .insert("Set-Cookie".to_owned(), "sid=test".to_owned());

    for request in [
        Request::get("/sap/bc/adt/discovery"),
        Request::get("/sap/bc/adt/programs/programs/ZTEST/source/main?version=active"),
        Request::put(
            "/sap/bc/adt/programs/programs/ZTEST/source/main",
            Vec::new(),
        ),
    ] {
        let mut transport = HttpTransport::for_test(vec![Ok(unauthorized.clone())]);

        assert_eq!(transport.request(request).unwrap().status, 401);
        assert_eq!(transport.close(), Ok(()));
        assert_eq!(transport.recorded().len(), 1);
    }
}

#[test]
fn the_context_cookie_decides_whether_the_stateful_session_is_supported() {
    let mut kept = Response::empty(200);
    kept.headers.insert(
        "Set-Cookie".to_owned(),
        "SAP_SESSIONID_DEV_100=s1; path=/; sap-contextid=ctx".to_owned(),
    );
    let mut transport = HttpTransport::for_test(vec![Ok(kept)]);

    assert_eq!(transport.stateful_supported(), None);
    transport
        .request(Request::get("/sap/bc/adt/discovery"))
        .unwrap();
    assert_eq!(transport.stateful_supported(), Some(true));

    for reply in [Response::empty(200), {
        let mut other = Response::empty(200);
        other
            .headers
            .insert("Set-Cookie".to_owned(), "SAP_SESSIONID=1".to_owned());
        other
    }] {
        let mut transport = HttpTransport::for_test(vec![Ok(reply)]);

        transport
            .request(Request::get("/sap/bc/adt/discovery"))
            .unwrap();
        assert_eq!(transport.stateful_supported(), Some(false));
    }
}

#[test]
fn a_context_cookie_seen_later_keeps_the_stateful_session_supported() {
    let mut with_cookie = Response::empty(200);
    with_cookie.headers.insert(
        "Set-Cookie".to_owned(),
        "SAP_SESSIONID_DEV_100=s1; path=/; sap-contextid=ctx".to_owned(),
    );
    let mut transport = HttpTransport::for_test(vec![
        Ok(Response::empty(200)),
        Ok(with_cookie),
        Ok(Response::empty(200)),
    ]);

    transport
        .request(Request::get(
            "/sap/bc/adt/programs/programs/ZTEST/source/main?version=active",
        ))
        .unwrap();
    assert_eq!(transport.stateful_supported(), Some(false));

    transport
        .request(Request::get("/sap/bc/adt/checkruns"))
        .unwrap();
    assert_eq!(transport.stateful_supported(), Some(true));

    transport
        .request(Request::get("/sap/bc/adt/activation"))
        .unwrap();
    assert_eq!(
        transport.stateful_supported(),
        Some(true),
        "a cookie seen once is never taken back"
    );
}

#[test]
fn a_context_cookie_on_a_non_2xx_reply_also_counts() {
    let mut not_found = Response::empty(404);
    not_found.headers.insert(
        "Set-Cookie".to_owned(),
        "SAP_SESSIONID_DEV_100=s1; path=/; sap-contextid=ctx".to_owned(),
    );
    let mut transport = HttpTransport::for_test(vec![Ok(not_found)]);

    assert_eq!(transport.stateful_supported(), None);
    transport
        .request(Request::get("/sap/bc/adt/discovery"))
        .unwrap();
    assert_eq!(transport.stateful_supported(), Some(true));
}

#[test]
fn a_stateless_request_says_nothing_about_the_stateful_session() {
    let mut transport = HttpTransport::for_test(vec![Ok(Response::empty(200))]);

    transport.close().unwrap();

    assert_eq!(transport.stateful_supported(), None);
}

#[test]
fn csrf_timeout() {
    assert!(HttpTransport::classify(403, Some("Required")).is_retryable_csrf());
    assert!(!HttpTransport::classify(401, None).is_retryable_csrf());
    assert!(HttpTransport::timeout_after_send().is_post_send_uncertain());
}

/// A 401 refuses the logon. A 403 keeps its operation-specific behavior, while the CSRF marker
/// remains the token flow that retries once with a fresh token.
#[test]
fn only_http_401_refuses_the_logon() {
    assert!(is_authorization_failure(401));
    assert!(!is_authorization_failure(403));
    assert!(!is_authorization_failure(200));
    assert!(!is_authorization_failure(409));

    let mut marker = Response::empty(403);
    marker
        .headers
        .insert("x-csrf-token".to_owned(), "Required".to_owned());
    assert_eq!(marker.authorization_failure(), None);

    let mut denied = Response::empty(401);
    denied
        .headers
        .insert("x-csrf-token".to_owned(), "Required".to_owned());
    assert_eq!(
        denied.authorization_failure(),
        Some(TransportError::Authorization("HTTP 401".to_owned()))
    );

    assert_eq!(Response::text(403, "denied").authorization_failure(), None);
    assert_eq!(Response::empty(500).authorization_failure(), None);
}

#[test]
fn cookie_store_keeps_every_set_cookie_and_close_ends_the_session() {
    let (base_url, server) = serve(vec![COOKIES, OK, OK]);
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    transport
        .request(Request::get("/sap/bc/adt/discovery"))
        .unwrap();
    transport.request(Request::get("/lock")).unwrap();
    transport.close().unwrap();
    transport.close().unwrap();

    let heads = server
        .join()
        .unwrap()
        .iter()
        .map(|head| head.to_ascii_lowercase())
        .collect::<Vec<_>>();

    assert_eq!(heads.len(), 3);
    assert!(heads[0].starts_with("get /sap/bc/adt/discovery?sap-client=100 http/1.1"));
    assert!(!heads[0].contains("cookie:"));
    for head in &heads[1..] {
        let cookie = head
            .lines()
            .find(|line| line.starts_with("cookie:"))
            .unwrap();
        assert!(cookie.contains("a=1") && cookie.contains("b=2"));
    }
    assert!(heads[1].contains("x-sap-adt-sessiontype: stateful"));
    assert!(heads[2].starts_with("get /sap/public/bc/icf/logoff?sap-client=100 http/1.1"));
    assert!(heads[2].contains("x-sap-adt-sessiontype: stateless"));
}

#[test]
fn a_session_that_is_only_dropped_is_still_logged_off() {
    let (base_url, server) = serve(vec![COOKIES, OK]);
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    transport
        .request(Request::get("/sap/bc/adt/discovery"))
        .unwrap();
    drop(transport);

    let heads = server.join().unwrap();

    assert_eq!(heads.len(), 2);
    assert!(heads[1].starts_with("GET /sap/public/bc/icf/logoff?sap-client=100 HTTP/1.1"));
    assert!(
        heads[1].to_ascii_lowercase().contains("cookie:") && heads[1].contains("a=1"),
        "the logoff carries the session cookies: {}",
        heads[1]
    );
}

#[test]
fn a_redirect_is_returned_and_never_followed() {
    // Following it would re-send the request (and its lock handle) to another location.
    let found = "HTTP/1.1 302 Found\r\nLocation: /followed\r\nContent-Length: 0\r\n\r\n";
    let (base_url, server) = serve(vec![found]);
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    let response = transport
        .request(Request::get("/sap/bc/adt/discovery"))
        .unwrap();
    let heads = server.join().unwrap();

    assert_eq!(response.status, 302);
    assert_eq!(heads.len(), 1);
    assert!(heads[0].starts_with("GET /sap/bc/adt/discovery?sap-client=100 HTTP/1.1"));
}

#[test]
fn request_is_sent_verbatim_with_basic_auth() {
    let (base_url, server) = serve(vec![OK]);
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();
    let uri = "/p/source/main?lockHandle=a%2Bb%2Fc%3D";
    let mut request = Request::put(uri, "é".as_bytes().to_vec());
    request.headers.insert(
        "Content-Type".to_owned(),
        "text/plain; charset=utf-8".to_owned(),
    );

    assert_eq!(transport.request(request).unwrap().status, 200);

    let head = server.join().unwrap().remove(0);

    assert!(head.starts_with("PUT /p/source/main?lockHandle=a%2Bb%2Fc%3D&sap-client=100 HTTP/1.1"));
    assert!(head.to_ascii_lowercase().contains("authorization: basic "));
    assert!(head.contains("dXNlcjpwYXNzd29yZA=="));
    assert!(head.ends_with("\r\n\r\né"));
}

#[test]
fn csrf_is_refreshed_once_and_retried_with_the_token() {
    let (base_url, server) = serve(vec![REQUIRED, TOKEN, OK]);
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    let response = transport.request(Request::put("/x", Vec::new())).unwrap();
    let heads = server.join().unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(heads.len(), 3);
    assert!(heads[1].starts_with("GET /sap/bc/adt/discovery"));
    assert!(
        heads[1]
            .to_ascii_lowercase()
            .contains("x-csrf-token: fetch")
    );
    assert!(heads[2].starts_with("PUT /x?sap-client=100"));
    assert!(heads[2].to_ascii_lowercase().contains("x-csrf-token: tok"));
}

#[test]
fn a_401_while_refetching_csrf_is_an_authorization_failure_without_a_retry() {
    let mut required = Response::empty(403);
    required
        .headers
        .insert("x-csrf-token".to_owned(), "Required".to_owned());
    let mut transport = HttpTransport::for_test(vec![Ok(required), Ok(Response::empty(401))]);

    let error = transport
        .request(Request::put("/x", Vec::new()))
        .unwrap_err();

    assert_eq!(error, TransportError::Authorization("HTTP 401".to_owned()));
    let recorded = transport.recorded();
    assert_eq!(recorded.len(), 2, "the original PUT is not retried");
    assert_eq!(recorded[0].method, "PUT");
    assert_eq!(recorded[1].method, "GET");
    assert_eq!(recorded[1].uri, "/sap/bc/adt/discovery");
}

#[test]
fn second_csrf_rejection_is_an_error_and_other_statuses_are_responses() {
    let (base_url, server) = serve(vec![REQUIRED, TOKEN, REQUIRED]);
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    let error = transport
        .request(Request::put("/x", Vec::new()))
        .unwrap_err();

    assert_eq!(error, TransportError::CsrfRequired);
    assert_eq!(server.join().unwrap().len(), 3);

    let (base_url, server) = serve(vec![
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n",
    ]);
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    assert_eq!(transport.request(Request::get("/x")).unwrap().status, 401);
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn connect_error_is_pre_send() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/", listener.local_addr().unwrap());
    drop(listener);
    let mut transport = HttpTransport::connect(config(&base_url, 2000)).unwrap();

    let error = transport.request(Request::get("/x")).unwrap_err();

    assert!(
        matches!(error, TransportError::PreSendConnectivity(_)),
        "{error:?}"
    );
}

/// A connect that times out (no SYN-ACK, as on a closed Windows loopback port or a firewall that
/// drops) sent nothing, so it is pre-send for a write as well as a read. 192.0.2.1 is TEST-NET-1:
/// it never connects, so both a timeout and an immediate "unreachable" prove the same thing.
#[test]
fn connect_phase_failure_is_pre_send_for_reads_and_writes() {
    for request in [Request::get("/x"), Request::put("/x", b"body".to_vec())] {
        let mut transport = HttpTransport::connect(config("http://192.0.2.1:9/", 400)).unwrap();

        let error = transport.request(request).unwrap_err();

        assert!(
            matches!(error, TransportError::PreSendConnectivity(_)),
            "{error:?}"
        );
        assert!(!error.is_post_send_uncertain(), "{error:?}");
    }
}

#[test]
fn builder_error_is_pre_send() {
    let mut transport = HttpTransport::connect(config("http://127.0.0.1:9/", 2000)).unwrap();
    let mut request = Request::get("/x");
    request
        .headers
        .insert("bad header name".to_owned(), "v".to_owned());

    let error = transport.request(request).unwrap_err();

    assert!(
        matches!(error, TransportError::PreSendConnectivity(_)),
        "{error:?}"
    );
}

#[test]
fn a_get_dropped_before_its_reply_is_a_connectivity_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        read_request(&stream);
    });
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    let error = transport.request(Request::get("/x")).unwrap_err();

    server.join().unwrap();
    assert!(!error.is_post_send_uncertain(), "{error:?}");
    assert_no_response(&error);
    assert!(
        error
            .to_string()
            .contains("connection closed before message completed"),
        "the message names the cause: {error}"
    );
}

#[test]
fn a_get_timeout_is_a_connectivity_failure() {
    let (base_url, server) = serve_silent(Duration::from_millis(1500));
    let mut transport = HttpTransport::connect(config(&base_url, 300)).unwrap();

    let error = transport.request(Request::get("/x")).unwrap_err();

    server.join().unwrap();
    assert!(!error.is_post_send_uncertain(), "{error:?}");
    assert_no_response(&error);
}

#[test]
fn connection_dropped_after_send_is_uncertain() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        read_request(&stream);
    });
    let mut transport = HttpTransport::connect(config(&base_url, 5000)).unwrap();

    let error = transport
        .request(Request::put("/x", b"body".to_vec()))
        .unwrap_err();

    server.join().unwrap();
    assert!(error.is_post_send_uncertain(), "{error:?}");
}

#[test]
fn timeout_after_send_is_uncertain() {
    let (base_url, server) = serve_silent(Duration::from_millis(1500));
    let mut transport = HttpTransport::connect(config(&base_url, 300)).unwrap();

    let error = transport
        .request(Request::put("/x", b"body".to_vec()))
        .unwrap_err();

    server.join().unwrap();
    assert!(error.is_post_send_uncertain(), "{error:?}");
}

#[test]
fn fixture_preserves_v1_source_and_hash() {
    let source_uri = "/sap/bc/adt/programs/programs/ytest_abapreach01/source/main?version=active";
    let v1 = "REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v1'.";
    let mut transport =
        common::from_fixture(common::fixture_path("dev-750-http.json"), |exchange| {
            exchange.request.method == "GET"
                && exchange.request.uri.ends_with("source/main?version=active")
                && exchange.response.body == "REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v1'."
        });
    let request = Request {
        method: "GET".to_owned(),
        uri: source_uri.to_owned(),
        headers: BTreeMap::from([("Accept".to_owned(), "text/plain".to_owned())]),
        body: Vec::new(),
    };
    let mut source = String::new();

    for _ in 0..3 {
        let response = transport.request(request.clone()).unwrap();
        assert_eq!(response.status, 200);
        source = String::from_utf8(response.body).unwrap();
        assert_eq!(source, v1);
    }

    assert_eq!(
        source.split("\r\n").collect::<Vec<_>>(),
        vec!["REPORT ytest_abapreach01.", "", "WRITE / 'v1'."]
    );
    let actual_hash = Sha256::digest(source.replace('\r', ""))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        actual_hash,
        "63886692bf652fa419147f746d6b7760d6c991606fa9603d8ade3caf08ac7c64"
    );
    transport.assert_finished();
}

// TLS. `tls_support` generates a CA, a server certificate for 127.0.0.1 and an unrelated CA
// into a temporary directory per test. None of them is in an OS trust store, so a client trusts
// the server only through `adt.ca_file`.

const TLS_DEADLINE: Duration = Duration::from_secs(20);
const TLS_TIMEOUT_MS: u64 = 5_000;
const CHILD_ENV: &str = "ABAPREACH_TLS_CHILD";

/// Serves one TLS connection and answers its first request with `200 OK`. The accept loop and
/// the stream have deadlines, so a client that never connects cannot hang the test.
fn serve_tls(files: &TlsFiles) -> (String, JoinHandle<()>) {
    let chain = CertificateDer::pem_file_iter(files.server_cert())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file(files.server_key()).unwrap();
    let tls_config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("https://{}/", listener.local_addr().unwrap());

    listener.set_nonblocking(true).unwrap();

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + TLS_DEADLINE;
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Err(_) => return,
            }
        };

        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let connection = ServerConnection::new(Arc::new(tls_config)).unwrap();
        let mut tls = StreamOwned::new(connection, stream);

        // A failed handshake (the client refused the certificate) ends the read with an error.
        if read_request(&mut tls).is_some() {
            let _ = tls.write_all(OK.as_bytes());
            let _ = tls.flush();
        }
    });

    (base_url, handle)
}

fn tls_config(base_url: &str, ca_file: Option<PathBuf>, insecure: bool) -> HttpConfig {
    HttpConfig {
        ca_file,
        insecure,
        ..config(base_url, TLS_TIMEOUT_MS)
    }
}

fn tls_get(config: HttpConfig) -> Result<Response, TransportError> {
    HttpTransport::connect(config)
        .unwrap()
        .request(Request::get("/sap/bc/adt/discovery"))
}

fn assert_certificate_refused(result: Result<Response, TransportError>) {
    // The handshake fails before a byte of the request leaves: connectivity, never uncertain.
    match result {
        Err(TransportError::PreSendConnectivity(message)) => {
            assert!(
                message.to_ascii_lowercase().contains("certificate"),
                "{message}"
            );
        }
        other => panic!("expected a refused certificate, got {other:?}"),
    }
}

fn generated_tls() -> (tempfile::TempDir, TlsFiles) {
    let directory = tempfile::tempdir().unwrap();
    let files = TlsFiles::generate(directory.path());

    (directory, files)
}

#[test]
fn an_explicit_ca_file_trusts_a_private_ca_server() {
    let (_directory, files) = generated_tls();
    let (base_url, server) = serve_tls(&files);
    let response = tls_get(tls_config(&base_url, Some(files.ca()), false)).unwrap();

    assert_eq!(response.status, 200);
    server.join().unwrap();
}

#[test]
fn a_certificate_no_trust_source_knows_is_refused_by_default() {
    let (_directory, files) = generated_tls();
    let (base_url, server) = serve_tls(&files);

    assert_certificate_refused(tls_get(tls_config(&base_url, None, false)));
    server.join().unwrap();
}

#[test]
fn a_ca_file_of_another_ca_does_not_trust_the_server() {
    let (_directory, files) = generated_tls();
    let (base_url, server) = serve_tls(&files);

    assert_certificate_refused(tls_get(tls_config(
        &base_url,
        Some(files.other_ca()),
        false,
    )));
    server.join().unwrap();
}

#[test]
fn only_the_insecure_option_accepts_an_untrusted_certificate() {
    let (_directory, files) = generated_tls();
    let (base_url, server) = serve_tls(&files);
    let response = tls_get(tls_config(&base_url, None, true)).unwrap();

    assert_eq!(response.status, 200);
    server.join().unwrap();
}

#[test]
fn an_unreadable_or_invalid_ca_file_fails_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let invalid = directory.path().join("invalid.pem");

    std::fs::write(&invalid, "not a certificate").unwrap();

    for ca_file in [directory.path().join("missing.pem"), invalid] {
        let result =
            HttpTransport::connect(tls_config("https://example.com/", Some(ca_file), false));

        assert!(
            matches!(result, Err(TransportError::PreSendConnectivity(_))),
            "{:?}",
            result.err()
        );
    }
}

/// The child half of the OS trust store test: it runs only when the parent sets `CHILD_ENV`,
/// because the platform verifier reads its roots once per process.
#[test]
fn os_trust_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }

    let files = TlsFiles::open(Path::new(&std::env::var_os(CHILD_ENV).unwrap()));
    let (base_url, server) = serve_tls(&files);

    assert_eq!(
        tls_get(tls_config(&base_url, None, false)).unwrap().status,
        200
    );
    server.join().unwrap();
}

/// Without `adt.ca_file` the roots come from the OS. Where the platform reads `SSL_CERT_FILE`
/// (Linux and other Unix except macOS) that file stands in for the OS store; macOS and Windows
/// have no such override, so the same child must still refuse the certificate there.
#[test]
fn the_os_trust_store_is_the_source_of_default_roots() {
    let (_directory, files) = generated_tls();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "os_trust_child", "--test-threads=1"])
        .env(CHILD_ENV, files.dir())
        .env("SSL_CERT_FILE", files.ca())
        .env_remove("SSL_CERT_DIR")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the trust store child did not finish within 60 s");
        }

        thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();

    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();

    assert!(stdout.contains("running 1 test"), "{stdout}");
    assert_eq!(status.success(), cfg!(target_os = "linux"), "{stdout}");
}

/// Roots must stay with the OS verifier: no bundled root list and no native-tls.
#[test]
fn the_build_keeps_the_platform_verifier_and_bundles_no_roots() {
    let manifest = include_str!("../Cargo.toml");
    let lock = include_str!("../Cargo.lock");

    assert!(manifest.contains("\"rustls\""));
    assert!(lock.contains("name = \"rustls-platform-verifier\""));

    for bundled in ["webpki-roots", "native-tls", "openssl-sys"] {
        assert!(
            !lock.contains(&format!("name = \"{bundled}\"")),
            "{bundled} is in the dependency tree"
        );
    }
}
