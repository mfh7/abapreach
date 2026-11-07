//! The M3 create flow for FUGR, FUNC, CLAS and INTF against a scripted ADT. Every request is
//! matched by method and exact URI, so the validation URIs, the POST URI with its `corrNr`, the
//! single POST and the read-only reconciliation are part of what the tests assert.

use abapreach::backend::{BackendKind, Failure, Selection, SelectionReason};
use abapreach::cli::{CreateKind, Subc};
use abapreach::config::Connection;
use abapreach::domain::Status;
use abapreach::object::PackageScope;
use abapreach::operation::create::{CreateInput, CreateProg};
use abapreach::operation::deploy::{
    CancelFlag, DeployResult, INTERRUPT_EXIT_CODE, InterruptAction,
};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use std::collections::VecDeque;

const TR: &str = "DEVK900001";
const GROUP: &str = "YTEST_ABAPREACH_FG";
const GROUP_URI: &str = "/sap/bc/adt/functions/groups/ytest_abapreach_fg";
const FM_URI: &str = "/sap/bc/adt/functions/groups/ytest_abapreach_fg/fmodules/ytest_abapreach_fm";
const CLAS_URI: &str = "/sap/bc/adt/oo/classes/ytest_abapreach_cl";
const INTF_URI: &str = "/sap/bc/adt/oo/interfaces/ytest_abapreach_if";
const CLAS_STRUCTURE: &str = "/sap/bc/adt/oo/classes/ytest_abapreach_cl/objectstructure?version=active&withShortDescriptions=true";
const TEXT: &str = "ABAPReach M3 probe";

type Reply = Result<Response, TransportError>;
type Rule = (String, String, VecDeque<Reply>);

/// A scripted ADT: a reply per method and exact URI, taken in order. An unscripted request is a
/// failed test, never a quiet success.
struct Fake {
    rules: Vec<Rule>,
    calls: Vec<Request>,
    closes: usize,
    stateful: Option<bool>,
    interrupt_on_create: Option<CancelFlag>,
}

impl Fake {
    fn new() -> Self {
        Self {
            rules: Vec::new(),
            calls: Vec::new(),
            closes: 0,
            stateful: None,
            interrupt_on_create: None,
        }
    }

    fn on(mut self, method: &str, uri: &str, reply: Reply) -> Self {
        match self
            .rules
            .iter_mut()
            .find(|(rule_method, rule_uri, _)| rule_method == method && rule_uri == uri)
        {
            Some((_, _, replies)) => replies.push_back(reply),
            None => self
                .rules
                .push((method.to_owned(), uri.to_owned(), VecDeque::from([reply]))),
        }

        self
    }

    fn get(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("GET", uri, Ok(Response::text(status, body)))
    }

    fn post(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("POST", uri, Ok(Response::text(status, body)))
    }

    fn stateful(mut self, supported: bool) -> Self {
        self.stateful = Some(supported);

        self
    }

    fn interrupt_on_create(mut self, cancel: CancelFlag) -> Self {
        self.interrupt_on_create = Some(cancel);

        self
    }

    fn posts(&self) -> Vec<&Request> {
        self.calls
            .iter()
            .filter(|call| call.method == "POST" && !call.uri.contains("/validation"))
            .collect()
    }

    fn writes(&self) -> usize {
        self.calls
            .iter()
            .filter(|call| {
                !matches!(call.method.as_str(), "GET") && !call.uri.contains("/validation")
            })
            .count()
    }

    fn uris(&self) -> Vec<String> {
        self.calls
            .iter()
            .map(|call| format!("{} {}", call.method, call.uri))
            .collect()
    }

    fn assert_finished(&self) {
        for (method, uri, replies) in &self.rules {
            assert!(replies.is_empty(), "unused reply for {method} {uri}");
        }
    }
}

impl SadtTransport for Fake {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.calls.push(request.clone());

        if request.method == "POST"
            && !request.uri.contains("/validation")
            && let Some(cancel) = &self.interrupt_on_create
        {
            assert_eq!(cancel.on_interrupt(), InterruptAction::Deferred);
        }

        self.rules
            .iter_mut()
            .find(|(method, uri, replies)| {
                *method == request.method && *uri == request.uri && !replies.is_empty()
            })
            .and_then(|(_, _, replies)| replies.pop_front())
            .unwrap_or_else(|| panic!("unscripted request: {} {}", request.method, request.uri))
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes += 1;

        Ok(())
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.stateful
    }
}

fn name_of(kind: CreateKind) -> &'static str {
    match kind {
        CreateKind::Prog => "YTEST_ABAPREACH_NEW",
        CreateKind::Fugr => "YTEST_ABAPREACH_FG",
        CreateKind::Func => "YTEST_ABAPREACH_FM",
        CreateKind::Clas => "YTEST_ABAPREACH_CL",
        CreateKind::Intf => "YTEST_ABAPREACH_IF",
    }
}

fn object_uri(kind: CreateKind) -> &'static str {
    match kind {
        CreateKind::Prog => "",
        CreateKind::Fugr => GROUP_URI,
        CreateKind::Func => FM_URI,
        CreateKind::Clas => CLAS_URI,
        CreateKind::Intf => INTF_URI,
    }
}

fn creation_uri(kind: CreateKind) -> &'static str {
    match kind {
        CreateKind::Prog => "",
        CreateKind::Fugr => "/sap/bc/adt/functions/groups",
        CreateKind::Func => "/sap/bc/adt/functions/groups/ytest_abapreach_fg/fmodules",
        CreateKind::Clas => "/sap/bc/adt/oo/classes",
        CreateKind::Intf => "/sap/bc/adt/oo/interfaces",
    }
}

fn input(kind: CreateKind, package: Option<&str>, tr: Option<&str>, simulate: bool) -> CreateInput {
    CreateInput {
        kind,
        name: name_of(kind).to_owned(),
        text: TEXT.to_owned(),
        package: package.map(str::to_owned),
        group: (kind == CreateKind::Func).then(|| GROUP.to_owned()),
        tr: tr.map(str::to_owned),
        simulate,
        subc: Subc::Program,
        source: None,
    }
}

/// The validation request exactly as the object contract builds it for the input.
fn validation_uri(kind: CreateKind, package: &str) -> String {
    let scope = if kind == CreateKind::Func {
        GROUP
    } else {
        package
    };

    kind.validation_uri(name_of(kind), TEXT, scope).unwrap()
}

fn status_message(severity: &str, text: &str) -> String {
    format!(
        concat!(
            r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
            r#"<SEVERITY>{}</SEVERITY><SHORT_TEXT>{}</SHORT_TEXT><LONG_TEXT/>"#,
            r#"</DATA></asx:values></asx:abap>"#,
        ),
        severity, text
    )
}

const CHECK_OK: &str = concat!(
    r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
    r#"<CHECK_RESULT>X</CHECK_RESULT></DATA></asx:values></asx:abap>"#,
);

fn exception(kind: &str, message: &str) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<exc:exception xmlns:exc="http://www.sap.com/abapxml/types/communicationframework">"#,
            r#"<namespace id="com.sap.adt"/><type id="{}"/>"#,
            r#"<message lang="EN">{}</message><localizedMessage lang="EN">{}</localizedMessage>"#,
            r#"<properties/></exc:exception>"#,
        ),
        kind, message, message
    )
}

fn group_metadata(package: &str) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<group:abapFunctionGroup xmlns:group="http://www.sap.com/adt/functions/groups" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="YTEST_ABAPREACH_FG" "#,
            r#"adtcore:type="FUGR/F"><adtcore:packageRef "#,
            r#"adtcore:uri="/sap/bc/adt/vit/wb/object_type/devck/object_name/x" "#,
            r#"adtcore:type="DEVC/K" adtcore:name="{}"/></group:abapFunctionGroup>"#,
        ),
        package
    )
}

fn connection() -> Connection {
    let mut connection = Connection::test_insecure();

    connection.user = "TESTUSER".to_owned();

    connection
}

fn selection() -> Selection {
    Selection::new(BackendKind::AdtHttp, SelectionReason::DefaultAdt)
}

fn run(fake: &mut Fake, input: CreateInput) -> DeployResult {
    CreateProg::run(fake, input, Some(&selection()), Some(&connection()))
}

fn run_with_cancel(fake: &mut Fake, input: CreateInput, cancel: &CancelFlag) -> DeployResult {
    CreateProg::run_with_cancel(fake, input, Some(&selection()), Some(&connection()), cancel)
}

/// A fake that accepts a clean create of the kind in a local or transportable package: the group
/// read of a FUNC, the validation and the POST.
fn clean(kind: CreateKind, package: &str, tr: Option<&str>, post_status: u16) -> Fake {
    let mut fake = Fake::new();

    if kind == CreateKind::Func {
        fake = fake.get(GROUP_URI, 200, &group_metadata(package));
    }

    let ok = match kind {
        CreateKind::Fugr | CreateKind::Func => status_message("OK", ""),
        _ => CHECK_OK.to_owned(),
    };
    let post_uri = match tr {
        Some(tr) => format!("{}?corrNr={tr}", creation_uri(kind)),
        None => creation_uri(kind).to_owned(),
    };

    fake.post(&validation_uri(kind, package), 200, &ok)
        .post(&post_uri, post_status, "")
}

const KINDS: [CreateKind; 4] = [
    CreateKind::Fugr,
    CreateKind::Func,
    CreateKind::Clas,
    CreateKind::Intf,
];

// ---- success ---------------------------------------------------------------------------

#[test]
fn every_kind_is_created_in_a_local_package_without_a_corrnr() {
    for kind in KINDS {
        let status = if kind == CreateKind::Func { 201 } else { 200 };
        let package = (kind != CreateKind::Func).then_some("$TMP");
        let mut fake = clean(kind, "$TMP", None, status);
        let result = run(&mut fake, input(kind, package, None, false));

        assert_eq!(result.outcome.status, Some(Status::Active), "{kind:?}");
        assert_eq!(result.outcome.exit, 0, "{kind:?}: {:?}", result.outcome);
        assert!(
            result
                .outcome
                .stdout
                .lines()
                .any(|line| line == "CREATE: ACTIVE"),
            "{kind:?}: {}",
            result.outcome.stdout
        );
        assert_eq!(fake.posts().len(), 1, "{kind:?}");
        assert!(!fake.posts()[0].uri.contains("corrNr"), "{kind:?}");
        assert_eq!(fake.closes, 1, "{kind:?}");
        fake.assert_finished();
    }
}

#[test]
fn the_default_package_is_tmp_and_the_posted_bodies_carry_name_text_and_package() {
    let mut fake = clean(CreateKind::Clas, "$TMP", None, 200);
    let result = run(&mut fake, input(CreateKind::Clas, None, None, false));

    assert_eq!(result.outcome.exit, 0, "{:?}", result.outcome);

    let post = fake.posts()[0];
    let body = String::from_utf8(post.body.clone()).unwrap();

    assert_eq!(
        post.headers.get("Content-Type").map(String::as_str),
        Some("application/*")
    );
    assert!(body.contains("<class:abapClass "), "{body}");
    assert!(
        body.contains(r#"adtcore:name="YTEST_ABAPREACH_CL""#),
        "{body}"
    );
    assert!(body.contains(r#"adtcore:type="CLAS/OC""#), "{body}");
    assert!(
        body.contains(r#"adtcore:description="ABAPReach M3 probe""#),
        "{body}"
    );
    assert!(body.contains(r#"adtcore:responsible="TESTUSER""#), "{body}");
    assert!(
        body.contains(r#"<adtcore:packageRef adtcore:name="$TMP"/>"#),
        "{body}"
    );

    let mut fake = clean(CreateKind::Func, "$TMP", None, 201);
    run(&mut fake, input(CreateKind::Func, None, None, false));
    let body = String::from_utf8(fake.posts()[0].body.clone()).unwrap();

    assert!(body.contains(r#"adtcore:type="FUGR/FF""#), "{body}");
    assert!(
        body.contains(
            r#"<adtcore:containerRef adtcore:name="YTEST_ABAPREACH_FG" adtcore:type="FUGR/F" adtcore:uri="/sap/bc/adt/functions/groups/ytest_abapreach_fg"/>"#
        ),
        "{body}"
    );
    assert!(!body.contains("packageRef"), "{body}");
}

#[test]
fn the_description_is_escaped_in_the_body() {
    let mut text_input = input(CreateKind::Intf, None, None, false);
    text_input.text = r#"A & B <"x">"#.to_owned();
    let uri = CreateKind::Intf
        .validation_uri(name_of(CreateKind::Intf), &text_input.text, "$TMP")
        .unwrap();
    let mut fake =
        Fake::new()
            .post(&uri, 200, CHECK_OK)
            .post(creation_uri(CreateKind::Intf), 200, "");
    let result = run(&mut fake, text_input);

    assert_eq!(result.outcome.exit, 0, "{:?}", result.outcome);

    let body = String::from_utf8(fake.posts()[0].body.clone()).unwrap();

    assert!(
        body.contains(r#"adtcore:description="A &amp; B &lt;&quot;x&quot;&gt;""#),
        "{body}"
    );
}

#[test]
fn every_transportable_post_carries_the_corrnr() {
    for kind in KINDS {
        let status = if kind == CreateKind::Func { 201 } else { 200 };
        let package = (kind != CreateKind::Func).then_some("ZPKG_TEST");
        let mut fake = clean(kind, "ZPKG_TEST", Some(TR), status);
        let result = run(&mut fake, input(kind, package, Some(TR), false));

        assert_eq!(result.outcome.exit, 0, "{kind:?}: {:?}", result.outcome);
        assert_eq!(
            fake.posts()[0].uri,
            format!("{}?corrNr={TR}", creation_uri(kind)),
            "{kind:?}"
        );
        fake.assert_finished();
    }
}

#[test]
fn a_transportable_package_without_a_request_cannot_become_a_scope() {
    assert_eq!(PackageScope::of("$TMP", None), Ok(PackageScope::Local));
    assert_eq!(
        PackageScope::of("ZPKG_TEST", Some(TR)),
        Ok(PackageScope::Transportable {
            request: TR.to_owned()
        })
    );
    assert!(PackageScope::of("ZPKG_TEST", None).is_err());
    assert!(PackageScope::of("$TMP", Some(TR)).is_err());
    assert!(PackageScope::of("ZPKG_TEST", Some("")).is_err());
    assert!(PackageScope::of("ZPKG_TEST", Some("DEVK9&corrNr=X")).is_err());
    assert!(PackageScope::of("ZPKG_TEST", Some(&"D".repeat(21))).is_err());
}

#[test]
fn the_creation_uri_appends_the_corrnr_only_for_a_transportable_scope() {
    let local = PackageScope::Local;
    let transportable = PackageScope::of("ZPKG_TEST", Some(TR)).unwrap();

    for kind in KINDS {
        let base = kind.creation_uri(Some("YTEST_ABAPREACH_FG")).unwrap();

        assert_eq!(
            kind.creation_request_uri(Some("YTEST_ABAPREACH_FG"), &local)
                .unwrap(),
            base
        );
        assert_eq!(
            kind.creation_request_uri(Some("YTEST_ABAPREACH_FG"), &transportable)
                .unwrap(),
            format!("{base}?corrNr={TR}")
        );
    }
}

// ---- simulate --------------------------------------------------------------------------

#[test]
fn simulate_validates_and_writes_nothing() {
    for kind in KINDS {
        let ok = match kind {
            CreateKind::Fugr | CreateKind::Func => status_message("OK", ""),
            _ => CHECK_OK.to_owned(),
        };
        let mut fake = Fake::new();

        if kind == CreateKind::Func {
            fake = fake.get(GROUP_URI, 200, &group_metadata("ZPKG_TEST"));
        }

        let mut fake = fake.post(&validation_uri(kind, "ZPKG_TEST"), 200, &ok);
        let package = (kind != CreateKind::Func).then_some("ZPKG_TEST");
        let result = run(&mut fake, input(kind, package, Some(TR), true));

        assert_eq!(result.outcome.status, Some(Status::SimulatedOk), "{kind:?}");
        assert_eq!(result.outcome.exit, 0, "{kind:?}: {:?}", result.outcome);
        assert!(
            result
                .outcome
                .stdout
                .lines()
                .any(|line| line == "CREATE_SIM: SIMULATED_OK"),
            "{kind:?}: {}",
            result.outcome.stdout
        );
        assert_eq!(fake.writes(), 0, "{kind:?}: {:?}", fake.uris());
        fake.assert_finished();
    }
}

// ---- rejection before any write --------------------------------------------------------

#[test]
fn an_existing_function_group_or_module_name_is_rejected_by_the_validation() {
    for kind in [CreateKind::Fugr, CreateKind::Func] {
        let mut fake = Fake::new();

        if kind == CreateKind::Func {
            fake = fake.get(GROUP_URI, 200, &group_metadata("$TMP"));
        }

        let mut fake = fake.post(
            &validation_uri(kind, "$TMP"),
            200,
            &status_message("ERROR", "Function group YTEST already exists"),
        );
        let result = run(&mut fake, input(kind, None, None, false));

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{kind:?}");
        assert_eq!(result.outcome.exit, 1, "{kind:?}");
        assert_eq!(
            result
                .outcome
                .stdout
                .lines()
                .find(|line| line.starts_with("CREATE:")),
            Some("CREATE: REJECTED"),
            "{kind:?}: {}",
            result.outcome.stdout
        );
        assert!(
            result.outcome.stdout.contains("already exists"),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.writes(), 0, "{kind:?}");
    }
}

#[test]
fn an_existing_class_or_interface_name_including_the_shared_namespace_is_rejected() {
    for (kind, message) in [
        (CreateKind::Clas, "Class YTEST_ABAPREACH_CL already exists"),
        (
            CreateKind::Clas,
            "Interface YTEST_ABAPREACH_CL already exists",
        ),
        (CreateKind::Intf, "Class YTEST_ABAPREACH_IF already exists"),
        (
            CreateKind::Intf,
            "Interface YTEST_ABAPREACH_IF already exists",
        ),
    ] {
        let mut fake = Fake::new().post(
            &validation_uri(kind, "$TMP"),
            400,
            &exception("InvalidClifName", message),
        );
        let result = run(&mut fake, input(kind, None, None, false));

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{message}");
        assert_eq!(result.outcome.exit, 1, "{message}");
        assert!(
            result.outcome.stdout.contains(message),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.writes(), 0, "{message}");
    }
}

#[test]
fn a_validation_that_does_not_confirm_the_name_is_a_rejection() {
    let mut fake = Fake::new().post(
        &validation_uri(CreateKind::Clas, "$TMP"),
        200,
        "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA/></asx:values></asx:abap>",
    );
    let result = run(&mut fake, input(CreateKind::Clas, None, None, false));

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(fake.writes(), 0);

    let mut fake = Fake::new().post(
        &validation_uri(CreateKind::Fugr, "$TMP"),
        200,
        &status_message("FATAL", "odd"),
    );
    let result = run(&mut fake, input(CreateKind::Fugr, None, None, false));

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(fake.writes(), 0);
}

#[test]
fn an_info_validation_is_not_a_rejection() {
    let mut fake = Fake::new()
        .get(GROUP_URI, 200, &group_metadata("$TMP"))
        .post(
            &validation_uri(CreateKind::Func, "$TMP"),
            200,
            &status_message("INFO", "Function module name is reserved for SAP"),
        )
        .post(creation_uri(CreateKind::Func), 201, "");
    let result = run(&mut fake, input(CreateKind::Func, None, None, false));

    assert_eq!(result.outcome.exit, 0, "{:?}", result.outcome);
    assert!(
        result.outcome.stderr.contains("reserved for SAP"),
        "{}",
        result.outcome.stderr
    );
}

#[test]
fn a_function_module_without_its_group_is_rejected_before_validation() {
    let mut fake = Fake::new().get(
        GROUP_URI,
        404,
        &exception("ExceptionResourceNotFound", "gone"),
    );
    let result = run(&mut fake, input(CreateKind::Func, None, None, false));

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result.outcome.stdout.contains("CREATE: REJECTED"),
        "{}",
        result.outcome.stdout
    );
    assert!(
        result.outcome.stdout.contains("function group"),
        "{}",
        result.outcome.stdout
    );
    assert_eq!(fake.calls.len(), 1, "{:?}", fake.uris());
}

#[test]
fn a_function_module_follows_the_transport_rules_of_its_group_package() {
    // A transportable group needs --tr; a local group rejects it. Both stop with exit 2.
    let mut fake = Fake::new().get(GROUP_URI, 200, &group_metadata("ZPKG_TEST"));
    let result = run(&mut fake, input(CreateKind::Func, None, None, false));

    assert_eq!(result.outcome.exit, 2);
    assert!(
        result.outcome.stderr.starts_with("STOP: "),
        "{}",
        result.outcome.stderr
    );
    assert!(
        result.outcome.stderr.contains("--tr is required"),
        "{}",
        result.outcome.stderr
    );
    assert_eq!(fake.writes(), 0);

    let mut fake = Fake::new().get(GROUP_URI, 200, &group_metadata("$TMP"));
    let result = run(&mut fake, input(CreateKind::Func, None, Some(TR), false));

    assert_eq!(result.outcome.exit, 2);
    assert!(
        result.outcome.stderr.contains("--tr is not valid"),
        "{}",
        result.outcome.stderr
    );
    assert_eq!(fake.writes(), 0);
}

#[test]
fn a_group_reply_without_a_package_fails_closed() {
    let mut fake = Fake::new().get(
        GROUP_URI,
        200,
        "<group:abapFunctionGroup xmlns:group=\"urn:x\"/>",
    );
    let result = run(&mut fake, input(CreateKind::Func, None, None, false));

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert_eq!(fake.writes(), 0);
}

// ---- the POST is definitive or reconciled, never repeated ------------------------------

#[test]
fn a_client_error_on_the_post_is_a_rejection_without_reconciliation() {
    let mut fake = clean(CreateKind::Fugr, "$TMP", None, 403);
    let result = run(&mut fake, input(CreateKind::Fugr, None, None, false));

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result.outcome.stdout.contains("HTTP 403"),
        "{}",
        result.outcome.stdout
    );
    assert_eq!(fake.posts().len(), 1);
    assert_eq!(fake.calls.last().unwrap().method, "POST");
}

#[test]
fn a_lost_post_reply_with_the_object_present_is_partial_and_never_repeated() {
    for kind in KINDS {
        let package = (kind != CreateKind::Func).then_some("$TMP");
        let mut fake = clean(kind, "$TMP", None, 200);
        // Replace the POST reply by a lost one.
        let lost = TransportError::PostSendUncertain("reply dropped".to_owned());
        let post = creation_uri(kind);

        fake.rules
            .iter_mut()
            .find(|(method, uri, _)| method == "POST" && uri == post)
            .unwrap()
            .2 = VecDeque::from([Err(lost)]);

        let read = if kind == CreateKind::Clas {
            CLAS_STRUCTURE
        } else {
            object_uri(kind)
        };
        let mut fake = fake.get(read, 200, "<x/>");
        let result = run(&mut fake, input(kind, package, None, false));

        assert_eq!(result.outcome.status, Some(Status::Partial), "{kind:?}");
        assert_eq!(result.outcome.exit, 1, "{kind:?}");
        assert!(
            result
                .outcome
                .stdout
                .lines()
                .any(|line| line == "CREATE: PARTIAL"),
            "{kind:?}: {}",
            result.outcome.stdout
        );
        assert_eq!(fake.posts().len(), 1, "{kind:?}: {:?}", fake.uris());
        assert!(!result.fallback_allowed, "{kind:?}");
        fake.assert_finished();
    }
}

fn uncertain(kind: CreateKind, reconcile: Reply) -> (Fake, DeployResult) {
    let package = (kind != CreateKind::Func).then_some("$TMP");
    let mut fake = clean(kind, "$TMP", None, 200);
    let post = creation_uri(kind);

    fake.rules
        .iter_mut()
        .find(|(method, uri, _)| method == "POST" && uri == post)
        .unwrap()
        .2 = VecDeque::from([Err(TransportError::PostSendUncertain("dropped".to_owned()))]);

    let read = if kind == CreateKind::Clas {
        CLAS_STRUCTURE
    } else {
        object_uri(kind)
    };
    let mut fake = fake.on("GET", read, reconcile);
    let result = run(&mut fake, input(kind, package, None, false));

    (fake, result)
}

#[test]
fn a_lost_post_reply_with_the_object_absent_is_rejected_and_never_repeated() {
    for kind in KINDS {
        let (fake, result) = uncertain(kind, Ok(Response::text(404, "")));

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{kind:?}");
        assert_eq!(result.outcome.exit, 1, "{kind:?}");
        assert!(
            result
                .outcome
                .stdout
                .lines()
                .any(|line| line == "CREATE: REJECTED"),
            "{kind:?}: {}",
            result.outcome.stdout
        );
        assert_eq!(fake.posts().len(), 1, "{kind:?}");
        assert!(
            result
                .outcome
                .stdout
                .contains("not found at read-back; verify before repeating"),
            "{kind:?}: {}",
            result.outcome.stdout
        );
    }
}

#[test]
fn an_interrupt_during_create_post_defers_through_reconciliation_then_exits_130() {
    let cancel = CancelFlag::new();
    let mut fake = clean(CreateKind::Fugr, "$TMP", None, 500)
        .get(GROUP_URI, 200, "<group/>")
        .interrupt_on_create(cancel.clone());

    let result = run_with_cancel(
        &mut fake,
        input(CreateKind::Fugr, None, None, false),
        &cancel,
    );

    assert_eq!(result.outcome.status, Some(Status::Partial));
    assert_eq!(result.outcome.exit, INTERRUPT_EXIT_CODE);
    assert!(result.outcome.stdout.contains("CREATE: PARTIAL"));
    assert_eq!(fake.posts().len(), 1);
    assert!(fake.calls.iter().any(|call| call.method == "GET"));
    fake.assert_finished();
}

#[test]
fn an_unreadable_reconciliation_stays_partial_without_a_retry() {
    for reconcile in [
        Ok(Response::text(500, "")),
        Ok(Response::text(403, "")),
        Err(TransportError::NoResponse("closed".to_owned())),
    ] {
        let (fake, result) = uncertain(CreateKind::Intf, reconcile);

        assert_eq!(result.outcome.status, Some(Status::Partial));
        assert_eq!(result.outcome.exit, 1);
        assert_eq!(fake.posts().len(), 1);
        assert_eq!(
            fake.calls
                .iter()
                .filter(|call| call.method == "GET")
                .count(),
            1,
            "{:?}",
            fake.uris()
        );
    }
}

#[test]
fn a_fresh_class_is_reconciled_through_objectstructure_never_its_source() {
    let (fake, result) = uncertain(CreateKind::Clas, Ok(Response::text(200, "<x/>")));

    assert_eq!(result.outcome.status, Some(Status::Partial));
    assert!(
        fake.calls.iter().any(|call| call.uri == CLAS_STRUCTURE),
        "{:?}",
        fake.uris()
    );
    assert!(
        !fake.calls.iter().any(|call| call.uri.contains("/source/")),
        "{:?}",
        fake.uris()
    );
}

#[test]
fn a_server_error_on_the_post_is_reconciled_like_a_lost_reply() {
    // The class create without a corrNr answered 500 and still created the object.
    let mut fake = clean(CreateKind::Clas, "$TMP", None, 500).get(CLAS_STRUCTURE, 200, "<x/>");
    let result = run(&mut fake, input(CreateKind::Clas, None, None, false));

    assert_eq!(result.outcome.status, Some(Status::Partial));
    assert_eq!(result.outcome.exit, 1);
    assert_eq!(fake.posts().len(), 1);

    let mut fake = clean(CreateKind::Clas, "$TMP", None, 500).get(CLAS_STRUCTURE, 404, "");
    let result = run(&mut fake, input(CreateKind::Clas, None, None, false));

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert_eq!(fake.posts().len(), 1);
}

#[test]
fn a_post_that_was_never_sent_is_an_error_that_still_allows_a_fallback() {
    let mut fake = clean(CreateKind::Fugr, "$TMP", None, 200);

    fake.rules
        .iter_mut()
        .find(|(method, uri, _)| method == "POST" && uri == "/sap/bc/adt/functions/groups")
        .unwrap()
        .2 = VecDeque::from([Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    ))]);

    let result = run(&mut fake, input(CreateKind::Fugr, None, None, false));

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.outcome.status, None);
    assert_eq!(result.failure, Some(Failure::PreSendConnectivity));
    assert!(result.fallback_allowed);
    assert_eq!(
        result.outcome.stderr,
        "ERROR: pre-send connectivity failure: no route"
    );
    assert_eq!(
        result.detail.as_deref(),
        Some("pre-send connectivity failure: no route")
    );
    assert_eq!(fake.posts().len(), 1);
}

#[test]
fn a_refused_logon_on_the_post_is_not_reconciled_or_retried() {
    let mut fake = clean(CreateKind::Fugr, "$TMP", None, 401);
    let result = run(&mut fake, input(CreateKind::Fugr, None, None, false));

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.failure, Some(Failure::Authorization));
    assert!(!result.fallback_allowed);
    assert_eq!(
        result.outcome.stderr,
        "ERROR: authorization failed: HTTP 401"
    );
    assert_eq!(
        result.detail.as_deref(),
        Some("authorization failed: HTTP 401")
    );
    assert_eq!(fake.posts().len(), 1);
    assert_eq!(fake.calls.last().unwrap().method, "POST");
}

#[test]
fn a_status_the_create_does_not_define_is_an_error_without_fallback() {
    // The group read of a FUNC answers something other than 200 or 404.
    let mut fake = Fake::new().get(GROUP_URI, 500, "");
    let result = run(&mut fake, input(CreateKind::Func, None, None, false));

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.outcome.status, None);
    assert_eq!(result.failure, Some(Failure::Validation));
    assert!(!result.fallback_allowed);
    assert_eq!(
        result.outcome.stderr,
        "ERROR: read function group: HTTP 500"
    );
    assert_eq!(
        result.detail.as_deref(),
        Some("read function group: HTTP 500")
    );
    assert_eq!(fake.writes(), 0);

    // The validation answers a status it does not define.
    let mut fake = Fake::new().post(&validation_uri(CreateKind::Fugr, "$TMP"), 503, "");
    let result = run(&mut fake, input(CreateKind::Fugr, None, None, false));

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.failure, Some(Failure::Validation));
    assert!(!result.fallback_allowed);
    assert_eq!(
        result.outcome.stderr,
        "ERROR: create validation failed: HTTP 503"
    );
    assert_eq!(
        result.detail.as_deref(),
        Some("create validation failed: HTTP 503")
    );
    assert_eq!(fake.writes(), 0);
}

// ---- backend capability ----------------------------------------------------------------

#[test]
fn a_backend_without_a_stateful_session_creates_nothing() {
    let mut fake = Fake::new()
        .post(
            &validation_uri(CreateKind::Fugr, "$TMP"),
            200,
            &status_message("OK", ""),
        )
        .stateful(false);
    let result = run(&mut fake, input(CreateKind::Fugr, None, None, false));

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.failure, Some(Failure::CapabilityUnavailable));
    assert!(result.fallback_allowed);
    assert_eq!(
        result.outcome.stderr,
        "ERROR: ADT HTTP stateful session unavailable (no sap-contextid)"
    );
    assert_eq!(
        result.detail.as_deref(),
        Some("ADT HTTP stateful session unavailable (no sap-contextid)")
    );
    assert_eq!(fake.writes(), 0);

    // Pinned by --backend: no fallback exists, so the run stops.
    let mut fake = Fake::new()
        .post(
            &validation_uri(CreateKind::Fugr, "$TMP"),
            200,
            &status_message("OK", ""),
        )
        .stateful(false);
    let pinned = Selection::new(BackendKind::AdtHttp, SelectionReason::ExplicitBackend);
    let result = CreateProg::run(
        &mut fake,
        input(CreateKind::Fugr, None, None, false),
        Some(&pinned),
        Some(&connection()),
    );

    assert_eq!(result.outcome.exit, 2);
    assert!(
        result.outcome.stderr.starts_with("STOP: "),
        "{}",
        result.outcome.stderr
    );
    assert!(!result.fallback_allowed);
    assert_eq!(fake.writes(), 0);
}

#[test]
fn the_report_names_the_backend_and_the_plan() {
    let mut fake = clean(CreateKind::Intf, "ZPKG_TEST", Some(TR), 200);
    let result = run(
        &mut fake,
        input(CreateKind::Intf, Some("ZPKG_TEST"), Some(TR), false),
    );
    let lines: Vec<&str> = result.outcome.stdout.lines().collect();

    assert_eq!(lines[0], "backend: adt");
    assert_eq!(lines[1], "reason: default ADT");
    assert!(
        lines.contains(&"CREATE: ACTIVE"),
        "{}",
        result.outcome.stdout
    );
}
