//! The request boundary for the M6 runner: the classrun POST, the runner class create, and the
//! class search. The classrun rules are asserted on both transports through one set of cases:
//! RFC-SADT through its admission, ADT HTTP through the operation-layer gate.

use abapreach::operation::runner::RunnerGated;
use abapreach::transport::{
    Request, Response, RunnerArm, SadtTransport, TransportError,
    http::HttpTransport,
    rfc::{FakeRfcApi, RfcTransport},
};

const RUNNER: &str = "YCL_ABAPREACH_RUN_AB12CD34";
const OTHER: &str = "YCL_ABAPREACH_RUN_ZZ99YY88";
const CLASSRUN: &str = "/sap/bc/adt/oo/classrun";
const CLASSES: &str = "/sap/bc/adt/oo/classes";

fn rfc_admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

fn http_admits(request: &Request) -> bool {
    let mut http = HttpTransport::for_test(vec![Ok(Response::empty(200))]);
    let mut gated = RunnerGated::new(&mut http);

    match gated.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

/// Both transports, each in its own freshly armed state, must agree on `expected`.
#[track_caller]
fn assert_both(name: Option<&str>, request: &Request, expected: bool) {
    for (transport, admits) in [
        ("rfc", rfc_admits as fn(&Request) -> bool),
        ("http", http_admits),
    ] {
        let _arm = name.map(|name| RunnerArm::arm(name).unwrap());

        assert_eq!(
            admits(request),
            expected,
            "{transport} armed with {name:?}: {request:?}"
        );
    }
}

fn classrun(name: &str) -> Request {
    let mut request = Request::post(format!("{CLASSRUN}/{name}"), Vec::new());

    request
        .headers
        .insert("Accept".to_owned(), "text/plain".to_owned());

    request
}

fn class_body(name: &str, package: &str) -> Vec<u8> {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><class:abapClass "#,
            r#"xmlns:class="http://www.sap.com/adt/oo/classes" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core" adtcore:description="runner" "#,
            r#"adtcore:name="{name}" adtcore:type="CLAS/OC" adtcore:language="EN" "#,
            r#"adtcore:masterLanguage="EN"><adtcore:packageRef adtcore:name="{package}"/>"#,
            r#"</class:abapClass>"#,
        ),
        name = name,
        package = package,
    )
    .into_bytes()
}

fn create(uri: &str, body: Vec<u8>) -> Request {
    let mut request = Request::post(uri, body);

    request
        .headers
        .insert("Content-Type".to_owned(), "application/*".to_owned());

    request
}

#[test]
fn the_classrun_post_of_the_armed_name_is_admitted_on_both_transports() {
    assert_both(Some(RUNNER), &classrun(RUNNER), true);
}

#[test]
fn an_unarmed_classrun_post_is_refused() {
    assert_both(None, &classrun(RUNNER), false);
}

#[test]
fn the_classrun_post_of_another_name_is_refused() {
    assert_both(Some(RUNNER), &classrun(OTHER), false);
    // No name pattern: the same prefix and length is still another name.
    assert_both(Some(RUNNER), &classrun("YCL_ABAPREACH_RUN_AB12CD3"), false);
    assert_both(
        Some(RUNNER),
        &classrun("YCL_ABAPREACH_RUN_AB12CD345"),
        false,
    );
    assert_both(Some(RUNNER), &classrun(&RUNNER.to_ascii_lowercase()), false);
}

#[test]
fn a_second_classrun_post_after_the_first_is_refused() {
    for admits in [rfc_admits as fn(&Request) -> bool, http_admits] {
        let _arm = RunnerArm::arm(RUNNER).unwrap();

        assert!(admits(&classrun(RUNNER)));
        assert!(!admits(&classrun(RUNNER)));
    }
}

#[test]
fn a_refused_classrun_post_still_disarms() {
    let _arm = RunnerArm::arm(RUNNER).unwrap();

    assert!(!rfc_admits(&classrun(OTHER)));
    assert!(!rfc_admits(&classrun(RUNNER)));
}

#[test]
fn the_arm_serves_one_post_across_the_transports() {
    let _arm = RunnerArm::arm(RUNNER).unwrap();

    assert!(http_admits(&classrun(RUNNER)));
    assert!(!rfc_admits(&classrun(RUNNER)));
}

#[test]
fn dropping_the_arm_disarms() {
    let arm = RunnerArm::arm(RUNNER).unwrap();

    drop(arm);

    assert_both(None, &classrun(RUNNER), false);
}

#[test]
fn only_one_name_can_be_armed_at_a_time() {
    let arm = RunnerArm::arm(RUNNER).unwrap();

    assert!(RunnerArm::arm(OTHER).is_err());
    assert!(RunnerArm::arm(RUNNER).is_err());
    drop(arm);
    assert!(RunnerArm::arm(OTHER).is_ok());
}

#[test]
fn a_name_that_is_not_a_runner_name_cannot_be_armed() {
    for name in [
        "",
        "ycl_abapreach_run_ab12cd34",
        "CL_ABAPREACH_RUN_AB12CD34",
        "YCL_ABAPREACH_RUN_AB12CD34567890ABCD",
        "YCL_ABAPREACH_RUN_AB12/CD",
        "YCL_ABAPREACH_RUN_AB12CD?",
        "YCL_ABAPREACH_RUN_AB 12",
        "YCL_ABAPREACH_RUN_\u{c9}",
        "YCL_ABAPREACH_RUN_*",
        "../YCL_ABAPREACH_RUN_AB12",
    ] {
        assert!(RunnerArm::arm(name).is_err(), "{name:?}");
    }
}

#[test]
fn classrun_get_put_and_delete_are_refused_even_when_armed() {
    let uri = format!("{CLASSRUN}/{RUNNER}");

    for request in [
        Request::get(uri.clone()),
        Request::put(uri.clone(), Vec::new()),
        Request::delete(uri.clone()),
        Request::get(CLASSRUN),
        Request::get(format!("{CLASSRUN}/")),
    ] {
        assert_both(Some(RUNNER), &request, false);
    }

    // They do not consume the arm.
    let _arm = RunnerArm::arm(RUNNER).unwrap();

    assert!(!rfc_admits(&Request::get(uri.clone())));
    assert!(!http_admits(&Request::put(uri, Vec::new())));
    assert!(rfc_admits(&classrun(RUNNER)));
}

#[test]
fn a_classrun_post_with_a_body_is_refused() {
    let mut request = classrun(RUNNER);

    request.body = b"x".to_vec();
    assert_both(Some(RUNNER), &request, false);

    request.body = b" ".to_vec();
    assert_both(Some(RUNNER), &request, false);
}

#[test]
fn a_classrun_post_with_extra_query_keys_or_a_fragment_is_refused() {
    for suffix in [
        "?",
        "?a=1",
        "?sap-client=100",
        "?corrNr=DEVK900001",
        "#frag",
        "?a=1#f",
    ] {
        let mut request = classrun(RUNNER);

        request.uri.push_str(suffix);
        assert_both(Some(RUNNER), &request, false);
    }
}

#[test]
fn a_classrun_post_with_a_traversal_or_another_path_is_refused() {
    for uri in [
        format!("{CLASSRUN}/../classes/{RUNNER}"),
        format!("{CLASSRUN}/{RUNNER}/.."),
        format!("{CLASSRUN}/{RUNNER}/"),
        format!("{CLASSRUN}/%2e%2e/{RUNNER}"),
        format!("{CLASSRUN}/{RUNNER}%2f..%2f"),
        format!("{CLASSRUN}//{RUNNER}"),
        format!("{CLASSRUN}/{RUNNER}/x"),
        format!("/sap/bc/adt/oo/classes/../classrun/{RUNNER}"),
        format!("/SAP/BC/ADT/OO/CLASSRUN/{RUNNER}"),
        format!("/sap/bc/adt/oo/classrun%2F{RUNNER}"),
        format!("/sap/bc/adt/oo/classrunx/{RUNNER}"),
        format!("/sap/bc/adt/oo/classrun.{RUNNER}"),
        format!("/sap/bc/adt/oo/class%72un/{RUNNER}"),
        format!("/sap/bc/adt/oo/CLASS%52UN/{RUNNER}"),
    ] {
        let mut request = classrun(RUNNER);

        request.uri = uri;
        assert_both(Some(RUNNER), &request, false);
    }
}

#[test]
fn a_classrun_post_without_the_text_accept_header_is_refused() {
    for accept in [
        None,
        Some("application/xml"),
        Some("*/*"),
        Some("text/plain, */*"),
    ] {
        let mut request = classrun(RUNNER);

        request.headers.clear();

        if let Some(accept) = accept {
            request
                .headers
                .insert("Accept".to_owned(), accept.to_owned());
        }

        assert_both(Some(RUNNER), &request, false);
    }
}

#[test]
fn the_runner_create_in_tmp_without_a_request_is_admitted() {
    let request = create(CLASSES, class_body(RUNNER, "$TMP"));

    assert_both(Some(RUNNER), &request, true);
}

#[test]
fn a_runner_create_with_a_request_is_refused() {
    for uri in [
        format!("{CLASSES}?corrNr=DEVK900001"),
        format!("{CLASSES}?corrNr="),
        format!("{CLASSES}?a=1"),
    ] {
        assert_both(
            Some(RUNNER),
            &create(&uri, class_body(RUNNER, "$TMP")),
            false,
        );
    }
}

#[test]
fn a_runner_create_outside_tmp_is_refused() {
    for package in ["ZABAPTEST", "$tmp", "$TMP2", " $TMP", "", "$TMP/x"] {
        let request = create(CLASSES, class_body(RUNNER, package));

        assert_both(Some(RUNNER), &request, false);
    }

    // A body without any package.
    let body = class_body(RUNNER, "$TMP");
    let text = String::from_utf8(body)
        .unwrap()
        .replace(r#"<adtcore:packageRef adtcore:name="$TMP"/>"#, "");

    assert_both(Some(RUNNER), &create(CLASSES, text.into_bytes()), false);
}

#[test]
fn a_body_that_names_the_runner_on_another_endpoint_or_unparsed_is_refused() {
    let body = class_body(RUNNER, "$TMP");

    assert_both(
        Some(RUNNER),
        &create("/sap/bc/adt/oo/interfaces", body.clone()),
        false,
    );
    assert_both(
        Some(RUNNER),
        &create("/sap/bc/adt/programs/programs", body),
        false,
    );
    assert_both(
        Some(RUNNER),
        &create(CLASSES, format!("<broken {RUNNER}").into_bytes()),
        false,
    );
}

#[test]
fn a_create_of_another_class_is_unchanged_by_the_arm() {
    for package in ["$TMP", "ZABAPTEST"] {
        let request = create(CLASSES, class_body("ZCL_OTHER", package));

        assert!(rfc_admits(&request), "{package}");
        assert!(http_admits(&request), "{package}");

        let _arm = RunnerArm::arm(RUNNER).unwrap();

        assert!(rfc_admits(&request), "armed {package}");
        assert!(http_admits(&request), "armed {package}");
    }

    let transportable = create(
        &format!("{CLASSES}?corrNr=DEVK900001"),
        class_body("ZCL_OTHER", "ZABAPTEST"),
    );

    assert!(rfc_admits(&transportable));
}

fn search(query: &str) -> Request {
    Request::get(format!(
        "/sap/bc/adt/repository/informationsystem/search?{query}"
    ))
}

#[test]
fn the_class_search_by_prefix_is_admitted() {
    for query in [
        "operation=quickSearch&query=YCL_ABAPREACH_RUN_*&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YTEST_ABAPREACH_RUN_*&maxResults=1&objectType=CLAS%2FOC",
        "operation=quickSearch&query=Z*&maxResults=100&objectType=CLAS%2FOC",
    ] {
        assert!(rfc_admits(&search(query)), "{query}");
    }
}

#[test]
fn other_searches_stay_refused_and_the_group_search_is_unchanged() {
    assert!(rfc_admits(&search(
        "operation=quickSearch&query=ZFM_A&maxResults=10&objectType=FUGR%2FFF"
    )));

    for query in [
        // Not a prefix search, a wildcard elsewhere, or a name that is not a runner prefix.
        "operation=quickSearch&query=YCL_ABAPREACH_RUN_&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=*&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=*YCL&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=Y*CL*&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YCL**&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=ycl_*&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=CL_*&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YCL%2A&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YCL_ABAPREACH_RUN_AB12CD34AB*&maxResults=50&objectType=CLAS%2FOC",
        // maxResults out of range or not a plain number.
        "operation=quickSearch&query=YCL_*&maxResults=0&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YCL_*&maxResults=101&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YCL_*&maxResults=050&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YCL_*&maxResults=&objectType=CLAS%2FOC",
        "operation=quickSearch&query=YCL_*&maxResults=5%30&objectType=CLAS%2FOC",
        // Other object types, extra keys, another order.
        "operation=quickSearch&query=YCL_*&maxResults=50&objectType=PROG%2FP",
        "operation=quickSearch&query=YCL_*&maxResults=50&objectType=CLAS%2FOC&x=1",
        "operation=quickSearch&query=YCL_*&maxResults=50",
        "operation=quickSearch&query=YCL_*&objectType=CLAS%2FOC&maxResults=50",
        "operation=quickSearch&query=YCL_*&maxResults=50&objectType=CLAS/OC",
        "operation=quickSearch&query=YCL_*&maxResults=50&objectType=INTF%2FOI",
        "operation=quickSearch&query=YCL_*&maxResults=50&objectType=FUGR%2FFF",
        "operation=quickSearch&query=ZFM_A*&maxResults=10&objectType=FUGR%2FFF",
        "operation=quickSearch&query=ZFM_A&maxResults=11&objectType=FUGR%2FFF",
        "operation=quickSearch&query=YCL_*&maxResults=50&objectType=CLAS%2FOC#f",
    ] {
        assert!(!rfc_admits(&search(query)), "{query}");
    }
}

#[test]
fn a_freestyle_data_preview_stays_refused() {
    for uri in [
        "/sap/bc/adt/datapreview/freestyle",
        "/sap/bc/adt/datapreview/freestyle?rowNumber=1",
    ] {
        let mut request = Request::post(uri, b"SELECT * FROM T001".to_vec());

        request
            .headers
            .insert("Content-Type".to_owned(), "text/plain".to_owned());
        request
            .headers
            .insert("Accept".to_owned(), "application/*".to_owned());

        let _arm = RunnerArm::arm(RUNNER).unwrap();

        assert!(!rfc_admits(&request), "{uri}");
    }
}

#[test]
fn the_other_writes_of_the_runner_class_are_unchanged_by_the_arm() {
    let _arm = RunnerArm::arm(RUNNER).unwrap();
    let class = format!("{CLASSES}/{}", RUNNER.to_ascii_lowercase());
    let activation = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="{class}" adtcore:type="CLAS/OC" adtcore:name="{RUNNER}"/></adtcore:objectReferences>"#
    );
    let mut activate = Request::post(
        "/sap/bc/adt/activation?method=activate&preauditRequested=true",
        activation.into_bytes(),
    );
    let mut put = Request::put(
        format!("{class}/source/main?lockHandle=a%2Bb"),
        RUNNER.as_bytes().to_vec(),
    );
    let lock = Request::post(
        format!("{class}?_action=LOCK&accessMode=MODIFY"),
        Vec::new(),
    );

    activate
        .headers
        .insert("Content-Type".to_owned(), "application/xml".to_owned());
    put.headers.insert(
        "Content-Type".to_owned(),
        "text/plain; charset=utf-8".to_owned(),
    );

    for request in [
        &activate,
        &put,
        &lock,
        &Request::delete(format!("{class}?lockHandle=a%2Bb")),
    ] {
        assert!(rfc_admits(request), "{request:?}");
        assert!(http_admits(request), "{request:?}");
    }
}

// ---- cargo-mutants triage (M6 Task 33): create-body rules that no test pinned -------------------

/// A runner create body in which each rule of the admission can be broken alone: the root element,
/// the `adtcore:name`, and the `adtcore:type`. The description always names the armed runner, so
/// the body is judged as a runner create whatever else it says.
fn class_body_of(root: &str, name: &str, kind: &str) -> Vec<u8> {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><class:{root} "#,
            r#"xmlns:class="http://www.sap.com/adt/oo/classes" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core" adtcore:description="{runner}" "#,
            r#"adtcore:name="{name}" adtcore:type="{kind}" adtcore:language="EN" "#,
            r#"adtcore:masterLanguage="EN"><adtcore:packageRef adtcore:name="$TMP"/>"#,
            r#"</class:{root}>"#,
        ),
        root = root,
        runner = RUNNER,
        name = name,
        kind = kind,
    )
    .into_bytes()
}

#[test]
fn a_create_that_names_the_runner_is_admitted_only_when_root_name_and_type_all_match() {
    assert_both(
        Some(RUNNER),
        &create(CLASSES, class_body_of("abapClass", RUNNER, "CLAS/OC")),
        true,
    );

    // Each rule broken alone.
    for body in [
        class_body_of("abapInterface", RUNNER, "CLAS/OC"),
        class_body_of("abapClass", OTHER, "CLAS/OC"),
        class_body_of("abapClass", RUNNER, "PROG/P"),
    ] {
        assert_both(Some(RUNNER), &create(CLASSES, body), false);
    }
}

/// A session that counts its closes and answers a scripted stateful verdict.
struct Spy {
    closes: usize,
    stateful: Option<bool>,
}

impl SadtTransport for Spy {
    fn request(&mut self, _request: Request) -> Result<Response, TransportError> {
        Ok(Response::empty(200))
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes += 1;

        Err(TransportError::Protocol("close refused".to_owned()))
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.stateful
    }
}

#[test]
fn the_http_gate_leaves_close_and_the_stateful_verdict_to_the_session_it_wraps() {
    for stateful in [None, Some(true), Some(false)] {
        let mut spy = Spy {
            closes: 0,
            stateful,
        };
        let mut gated = RunnerGated::new(&mut spy);

        assert_eq!(gated.stateful_supported(), stateful);
        assert_eq!(
            gated.close(),
            Err(TransportError::Protocol("close refused".to_owned()))
        );
        assert_eq!(spy.closes, 1);
    }
}

// ---- Task 32A: the word `classrun` inside an object name is not the classrun resource ----------

/// The reads and writes of a class named like `ZCL_MY_CLASSRUN`, as the M1-M5 operations send them.
fn class_requests(class: &str) -> Vec<Request> {
    let base = format!("{CLASSES}/{class}");
    let mut put = Request::put(
        format!("{base}/source/main?lockHandle=a%2Bb"),
        b"CLASS x DEFINITION.".to_vec(),
    );

    put.headers.insert(
        "Content-Type".to_owned(),
        "text/plain; charset=utf-8".to_owned(),
    );

    vec![
        Request::get(format!("{base}/source/main?version=active")),
        Request::get(base.clone()),
        Request::post(format!("{base}?_action=LOCK&accessMode=MODIFY"), Vec::new()),
        put,
        Request::delete(format!("{base}?lockHandle=a%2Bb")),
    ]
}

#[test]
fn an_object_whose_name_contains_classrun_is_admitted_on_both_transports() {
    for class in [
        "zcl_my_classrun",
        "ZCL_MY_CLASSRUN",
        "zclassrun",
        "zcl_classrun_demo",
    ] {
        for request in class_requests(class) {
            assert_both(None, &request, true);
            assert_both(Some(RUNNER), &request, true);
        }
    }

    // Other object kinds: the word is a name, not the resource.
    for uri in [
        "/sap/bc/adt/programs/programs/zclassrun/source/main?version=active",
        "/sap/bc/adt/oo/interfaces/zif_classrun/source/main?version=active",
    ] {
        assert_both(Some(RUNNER), &Request::get(uri), true);
    }
}

#[test]
fn a_runner_cycle_with_classrun_in_its_prefix_works_on_both_transports() {
    let name = "YCL_CLASSRUN_AB12CD34";
    let class = name.to_ascii_lowercase();
    let created = create(CLASSES, class_body(name, "$TMP"));

    assert_both(Some(name), &created, true);
    assert_both(Some(name), &classrun(name), true);

    for request in class_requests(&class) {
        assert_both(Some(name), &request, true);
    }

    // The same name is still only one POST at the real resource.
    assert_both(Some(name), &classrun(RUNNER), false);
}

#[test]
fn every_spelling_that_reaches_the_classrun_resource_stays_refused() {
    for uri in [
        CLASSRUN.to_owned(),
        format!("{CLASSRUN}/{RUNNER}"),
        format!("{CLASSRUN}?x=1"),
        format!("{CLASSRUN}%3Fx=1"),
        format!("/sap/bc/adt/oo/classes/../classrun/{RUNNER}"),
        format!("/sap/bc/adt/oo/./classrun/{RUNNER}"),
        format!("/sap/bc/adt/oo//classrun/{RUNNER}"),
        format!("/sap//bc/adt/oo/classrun/{RUNNER}"),
        format!("/sap/bc/adt/oo/classes/%2e%2e/classrun/{RUNNER}"),
        format!("/sap/bc/adt/oo/classes/%252e%252e/classrun/{RUNNER}"),
        format!("/sap/bc/adt/oo/classes/..%2fclassrun/{RUNNER}"),
        format!("/sap/bc/adt/oo/classes/..%252fclassrun/{RUNNER}"),
        format!("/sap/bc/adt/oo/%252563lassrun/{RUNNER}"),
        format!("/sap/bc/adt/oo/CLASSRUN/{RUNNER}"),
        format!("/sap/bc/adt/oo/classrun;a=b/{RUNNER}"),
        format!("/sap/bc/adt/oo/classrun/{RUNNER}#f"),
        format!("/sap/bc/adt/oo/classrunx/{RUNNER}"),
        format!("/sap/bc/adt/oo/classes/zcl_x/../../classrun/{RUNNER}"),
        format!("/sap/bc/adt/oo\\classrun/{RUNNER}"),
        format!("https://host.example.com/sap/bc/adt/oo/classrun/{RUNNER}"),
        format!("http://host.example.com:8000/sap/bc/adt/oo/classrun/{RUNNER}"),
        format!("HTTPS://HOST.EXAMPLE.COM/SAP/BC/ADT/OO/CLASSRUN/{RUNNER}"),
        format!("//host.example.com/sap/bc/adt/oo/classrun/{RUNNER}"),
        format!("https://host.example.com/sap/bc/adt/oo/classes/../classrun/{RUNNER}"),
        format!("https://host.example.com/sap/bc/adt/oo/%63lassrun/{RUNNER}"),
        format!("/prefix/sap/bc/adt/oo/classrun/{RUNNER}"),
        "https://host.example.com/sap/bc/adt/oo/classrun".to_owned(),
    ] {
        for request in [
            Request::get(uri.clone()),
            Request::delete(uri.clone()),
            Request::put(uri.clone(), Vec::new()),
        ] {
            assert_both(Some(RUNNER), &request, false);
        }

        let mut post = classrun(RUNNER);

        post.uri = uri.clone();

        // An armed POST is judged on the exact uri; none of these spellings is it.
        if uri != format!("{CLASSRUN}/{RUNNER}") {
            assert_both(Some(RUNNER), &post, false);
        }
    }
}
