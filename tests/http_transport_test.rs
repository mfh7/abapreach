mod common;

use abapreach::transport::http::{HttpConfig, HttpTransport};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

fn read_request(stream: &TcpStream) -> Option<String> {
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
    assert!(heads[2].contains("x-sap-adt-sessiontype: stateless"));
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
