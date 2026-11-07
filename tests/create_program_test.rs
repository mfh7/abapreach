//! M5 Task 6: `create PROG` for a program (`--subc 1`) and an include (`--subc I`) against a
//! scripted ADT. Every request is matched by method and exact URI, so the resolution, the POST
//! with its `corrNr`, the informational validation, the simulate deploy of a FILE and the
//! read-only reconciliation are part of what the tests assert. The fake also models the enqueue
//! table, so a FILE flow can assert that no lock is left behind.

use abapreach::backend::{BackendKind, Failure, Selection, SelectionReason};
use abapreach::cli::{CreateKind, Subc};
use abapreach::config::Connection;
use abapreach::domain::Status;
use abapreach::operation::create::{CreateInput, CreateProg};
use abapreach::operation::deploy::DeployResult;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use std::collections::VecDeque;

const TR: &str = "DEVK900001";
const NAME: &str = "YTEST_ABAPREACH_NEW";
const PROG: &str = "/sap/bc/adt/programs/programs/ytest_abapreach_new";
const INCL: &str = "/sap/bc/adt/programs/includes/ytest_abapreach_new";
const PROGRAMS: &str = "/sap/bc/adt/programs/programs";
const INCLUDES: &str = "/sap/bc/adt/programs/includes";
const CHECKRUNS: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const HANDLE_QUERY: &str = "lockHandle=h%2B1%2F%3D";
const TEXT: &str = "ABAPReach M5 probe";

type Reply = Result<Response, TransportError>;
type Rule = (String, String, VecDeque<Reply>);

struct Fake {
    rules: Vec<Rule>,
    calls: Vec<Request>,
    closes: usize,
    stateful: Option<bool>,
    /// LOCK with a 2xx reply adds one, UNLOCK with a 2xx reply removes one.
    locks: i32,
}

impl Fake {
    fn new() -> Self {
        Self {
            rules: Vec::new(),
            calls: Vec::new(),
            closes: 0,
            stateful: None,
            locks: 0,
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

    /// The requests that create: every POST except the validation, the lock cycle and the check.
    fn creates(&self) -> Vec<&Request> {
        self.calls
            .iter()
            .filter(|call| {
                call.method == "POST"
                    && (call.uri.starts_with(PROGRAMS) || call.uri.starts_with(INCLUDES))
                    && !call.uri.contains("_action=")
            })
            .collect()
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
    fn request(&mut self, request: Request) -> Reply {
        self.calls.push(request.clone());

        let reply = self
            .rules
            .iter_mut()
            .find(|(method, uri, replies)| {
                *method == request.method && *uri == request.uri && !replies.is_empty()
            })
            .and_then(|(_, _, replies)| replies.pop_front())
            .unwrap_or_else(|| panic!("unscripted request: {} {}", request.method, request.uri));
        let accepted = matches!(&reply, Ok(response) if (200..300).contains(&response.status));

        if accepted && request.uri.contains("_action=LOCK") {
            self.locks += 1;
        }

        if accepted && request.uri.contains("_action=UNLOCK") {
            self.locks -= 1;
        }

        reply
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes += 1;

        Ok(())
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.stateful
    }
}

fn input(subc: Subc, package: Option<&str>, tr: Option<&str>, simulate: bool) -> CreateInput {
    CreateInput {
        kind: CreateKind::Prog,
        name: NAME.to_owned(),
        text: TEXT.to_owned(),
        package: package.map(str::to_owned),
        group: None,
        tr: tr.map(str::to_owned),
        simulate,
        subc,
        source: None,
    }
}

fn with_file(mut input: CreateInput, source: &str) -> CreateInput {
    input.source = Some(source.lines().map(str::to_owned).collect());

    input
}

fn connection() -> Connection {
    let mut connection = Connection::test_insecure();

    connection.user = "TESTUSER".to_owned();

    if let Some(adt) = connection.adt.as_mut() {
        adt.insecure = false;
    }

    connection
}

fn selection() -> Selection {
    Selection::new(BackendKind::AdtHttp, SelectionReason::DefaultAdt)
}

fn run(fake: &mut Fake, input: CreateInput) -> DeployResult {
    CreateProg::run(fake, input, Some(&selection()), Some(&connection()))
}

fn collection(subc: Subc) -> &'static str {
    match subc {
        Subc::Program => PROGRAMS,
        Subc::Include => INCLUDES,
    }
}

fn validation(package: &str, object_type: &str) -> String {
    format!(
        "/sap/bc/adt/programs/validation?objname={NAME}&packagename={}&description=ABAPReach%20M5%20probe&objtype={object_type}",
        package.replace('$', "%24")
    )
}

fn validation_for(subc: Subc, package: &str) -> String {
    validation(
        package,
        match subc {
            Subc::Program => "PROG/P",
            Subc::Include => "PROG/I",
        },
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

/// Neither `programs/programs` nor `programs/includes` knows the name.
fn absent(fake: Fake) -> Fake {
    let missing = exception("ExceptionResourceNotFound", "Resource does not exist.");

    fake.get(PROG, 404, &missing).get(INCL, 404, &missing)
}

/// The resolution, the informational validation and the create POST of a clean create.
fn clean(subc: Subc, package: &str, tr: Option<&str>, post_status: u16) -> Fake {
    let post = match tr {
        Some(tr) => format!("{}?corrNr={tr}", collection(subc)),
        None => collection(subc).to_owned(),
    };

    absent(Fake::new())
        .post(&validation_for(subc, package), 200, CHECK_OK)
        .post(&post, post_status, "")
}

const BOTH: [Subc; 2] = [Subc::Program, Subc::Include];

// ---- success ---------------------------------------------------------------------------

#[test]
fn a_program_and_an_include_are_created_in_tmp_without_a_corrnr_after_both_uris_answer_404() {
    for subc in BOTH {
        let mut fake = clean(subc, "$TMP", None, 200);
        let result = run(&mut fake, input(subc, None, None, false));

        assert_eq!(result.outcome.status, Some(Status::Active), "{subc:?}");
        assert_eq!(result.outcome.exit, 0, "{subc:?}: {:?}", result.outcome);
        assert!(
            result
                .outcome
                .stdout
                .lines()
                .any(|line| line == "CREATE: ACTIVE"),
            "{subc:?}: {}",
            result.outcome.stdout
        );
        // The program URI is asked first, the include second, then the validation and the POST.
        assert_eq!(
            fake.uris(),
            [
                format!("GET {PROG}"),
                format!("GET {INCL}"),
                format!("POST {}", validation_for(subc, "$TMP")),
                format!("POST {}", collection(subc)),
            ],
            "{subc:?}"
        );
        assert_eq!(fake.closes, 1, "{subc:?}");
        fake.assert_finished();
    }
}

#[test]
fn the_posted_bodies_carry_name_text_package_and_no_container_ref() {
    for (subc, element, object_type) in [
        (Subc::Program, "program:abapProgram", "PROG/P"),
        (Subc::Include, "include:abapInclude", "PROG/I"),
    ] {
        let mut fake = clean(subc, "$TMP", None, 200);
        let result = run(&mut fake, input(subc, Some("$tmp"), None, false));

        assert_eq!(result.outcome.exit, 0, "{subc:?}: {:?}", result.outcome);

        let post = fake.creates()[0];
        let body = String::from_utf8(post.body.clone()).unwrap();

        assert_eq!(
            post.headers.get("Content-Type").map(String::as_str),
            Some("application/*"),
            "{subc:?}"
        );
        assert!(body.contains(&format!("<{element} ")), "{subc:?}: {body}");
        assert!(
            body.contains(&format!(r#"adtcore:type="{object_type}""#)),
            "{subc:?}: {body}"
        );
        assert!(
            body.contains(&format!(r#"adtcore:name="{NAME}""#)),
            "{subc:?}: {body}"
        );
        assert!(
            body.contains(r#"adtcore:description="ABAPReach M5 probe""#),
            "{subc:?}: {body}"
        );
        assert!(
            body.contains(r#"adtcore:responsible="TESTUSER""#),
            "{subc:?}: {body}"
        );
        assert!(
            body.contains(r#"<adtcore:packageRef adtcore:name="$TMP"/>"#),
            "{subc:?}: {body}"
        );
        // An include is bound to its main program by the first deploy, never by the create.
        assert!(!body.contains("containerRef"), "{subc:?}: {body}");
    }
}

#[test]
fn a_transportable_create_carries_the_corrnr_on_every_post() {
    for subc in BOTH {
        let mut fake = clean(subc, "ZPKG", Some(TR), 200);
        let result = run(&mut fake, input(subc, Some("ZPKG"), Some(TR), false));

        assert_eq!(result.outcome.status, Some(Status::Active), "{subc:?}");
        assert_eq!(result.outcome.exit, 0, "{subc:?}: {:?}", result.outcome);

        let creates = fake.creates();

        assert_eq!(creates.len(), 1, "{subc:?}");
        assert_eq!(
            creates[0].uri,
            format!("{}?corrNr={TR}", collection(subc)),
            "{subc:?}"
        );
        // No request of a create is a PUT or DELETE, and every create POST names the request.
        assert!(
            fake.calls
                .iter()
                .all(|call| !matches!(call.method.as_str(), "PUT" | "DELETE")),
            "{subc:?}"
        );
        assert!(
            fake.calls
                .iter()
                .filter(|call| call.method == "POST"
                    && !call.uri.contains("/validation")
                    && !call.uri.contains("_action="))
                .all(|call| call.uri.contains(&format!("corrNr={TR}"))),
            "{subc:?}: {:?}",
            fake.uris()
        );
    }
}

// ---- refusals before any request ----------------------------------------------------------

#[test]
fn a_transportable_package_without_tr_and_tr_with_tmp_stop_before_any_request() {
    for subc in BOTH {
        for (package, tr, message) in [
            (
                Some("ZPKG"),
                None,
                "STOP: package ZPKG is transportable: --tr is required",
            ),
            (
                None,
                Some(TR),
                "STOP: --tr is not valid for the local package $TMP",
            ),
        ] {
            for simulate in [false, true] {
                let mut fake = Fake::new();
                let result = run(&mut fake, input(subc, package, tr, simulate));

                assert_eq!(result.outcome.exit, 2, "{subc:?} {package:?} {tr:?}");
                assert_eq!(result.outcome.status, None);
                assert_eq!(result.outcome.stderr, message, "{subc:?} {simulate}");
                assert!(
                    fake.calls.is_empty(),
                    "{subc:?} {package:?} {tr:?} {simulate}: {:?}",
                    fake.uris()
                );
            }
        }
    }
}

#[test]
fn a_session_without_state_stops_a_pinned_create_and_allows_a_fallback_otherwise() {
    let mut fake = absent(Fake::new())
        .post(&validation_for(Subc::Include, "$TMP"), 200, CHECK_OK)
        .stateful(false);
    let result = CreateProg::run(
        &mut fake,
        input(Subc::Include, None, None, false),
        Some(&Selection::new(
            BackendKind::AdtHttp,
            SelectionReason::ExplicitBackend,
        )),
        Some(&connection()),
    );

    assert_eq!(result.outcome.exit, 2);
    assert!(result.outcome.stderr.contains("--backend rfc"));
    assert!(fake.creates().is_empty());

    let mut fake = absent(Fake::new())
        .post(&validation_for(Subc::Include, "$TMP"), 200, CHECK_OK)
        .stateful(false);
    let result = run(&mut fake, input(Subc::Include, None, None, false));

    assert_eq!(result.outcome.exit, 1);
    assert!(result.fallback_allowed);
    assert_eq!(result.failure, Some(Failure::CapabilityUnavailable));
    assert!(fake.creates().is_empty());
}

// ---- existing name ------------------------------------------------------------------------

#[test]
fn an_existing_program_or_include_is_rejected_before_anything_is_posted() {
    for (existing, found_as) in [(PROG, "a program"), (INCL, "an include")] {
        for simulate in [false, true] {
            let mut fake = if existing == PROG {
                Fake::new().get(PROG, 200, "<program:abapProgram/>")
            } else {
                Fake::new()
                    .get(PROG, 404, "")
                    .get(INCL, 200, "<include:abapInclude/>")
            };
            let result = run(&mut fake, input(Subc::Program, None, None, simulate));

            assert_eq!(result.outcome.status, Some(Status::Rejected), "{existing}");
            assert_eq!(result.outcome.exit, 1);
            assert!(
                result.outcome.stdout.ends_with(&format!(
                    "CREATE: REJECTED\nPROG {NAME} already exists as {found_as}"
                )),
                "{existing} {simulate}: {}",
                result.outcome.stdout
            );
            assert!(fake.creates().is_empty());
            assert!(
                !fake.uris().iter().any(|uri| uri.contains("/validation")),
                "{existing}: {:?}",
                fake.uris()
            );
            assert_eq!(fake.closes, 1);
            fake.assert_finished();
        }
    }
}

// ---- simulate -----------------------------------------------------------------------------

#[test]
fn a_simulation_is_the_local_checks_plus_the_resolution_and_posts_nothing() {
    for (subc, word) in [(Subc::Program, ""), (Subc::Include, " (include)")] {
        let mut fake = absent(Fake::new()).post(&validation_for(subc, "ZPKG"), 200, CHECK_OK);
        let result = run(&mut fake, input(subc, Some("ZPKG"), Some(TR), true));

        assert_eq!(result.outcome.status, Some(Status::SimulatedOk), "{subc:?}");
        assert_eq!(result.outcome.exit, 0, "{subc:?}: {:?}", result.outcome);
        assert!(
            result.outcome.stdout.contains(&format!(
                "WILL CREATE PROG{word} {NAME} in package ZPKG (request {TR})"
            )),
            "{subc:?}: {}",
            result.outcome.stdout
        );
        assert!(
            result.outcome.stdout.contains("CREATE_SIM: SIMULATED_OK"),
            "{subc:?}"
        );
        assert!(fake.creates().is_empty(), "{subc:?}");
        assert_eq!(fake.closes, 1);
        fake.assert_finished();
    }
}

#[test]
fn the_validation_is_informational_whatever_it_answers() {
    let answers: Vec<(Reply, &str)> = vec![
        (
            Ok(Response::text(
                400,
                exception("InvalidClifName", "Name refused"),
            )),
            "W: validation: Name refused",
        ),
        (
            Ok(Response::text(200, "<DATA/>")),
            "W: validation: validation did not confirm",
        ),
        (Ok(Response::text(500, "")), "W: validation: HTTP 500"),
        (
            Err(TransportError::PreSendConnectivity("no route".to_owned())),
            "W: validation: ",
        ),
    ];

    for (answer, note) in answers {
        for simulate in [true, false] {
            let post = if simulate { None } else { Some(PROGRAMS) };
            let mut fake = absent(Fake::new()).on(
                "POST",
                &validation_for(Subc::Program, "$TMP"),
                match &answer {
                    Ok(response) => Ok(response.clone()),
                    Err(error) => Err(error.clone()),
                },
            );

            if let Some(post) = post {
                fake = fake.post(post, 200, "");
            }

            let result = run(&mut fake, input(Subc::Program, None, None, simulate));
            let expected = if simulate {
                Status::SimulatedOk
            } else {
                Status::Active
            };

            assert_eq!(result.outcome.status, Some(expected), "{note} {simulate}");
            assert_eq!(result.outcome.exit, 0, "{note}: {:?}", result.outcome);
            assert!(
                result.outcome.stderr.contains(note),
                "{note}: {}",
                result.outcome.stderr
            );
            assert_eq!(fake.creates().len(), usize::from(!simulate));
            fake.assert_finished();
        }
    }
}

// ---- FILE: a simulate deploy, no source write ----------------------------------------------

fn lock_body() -> String {
    concat!(
        r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
        r#"<LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>"#,
        r#"</DATA></asx:values></asx:abap>"#,
    )
    .to_owned()
}

fn no_inactive() -> String {
    r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom"><atom:entry><atom:author><atom:name>TESTUSER</atom:name></atom:author><atom:id>00000</atom:id></atom:entry></atom:feed>"#.to_owned()
}

const PROCESSED: &str = r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkReport chkrun:reporter="abapCheckRun" chkrun:status="processed" chkrun:statusText="checked"/></chkrun:checkRunReports>"#;
const CHECK_ERROR: &str = r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkReport chkrun:status="processed"><chkrun:checkMessageList><chkrun:checkMessage chkrun:uri="/x#start=1,8" chkrun:type="E" chkrun:shortText="Field NO_SUCH_VAR is unknown."/></chkrun:checkMessageList></chkrun:checkReport></chkrun:checkRunReports>"#;

/// The recorded request order of a simulate deploy of a program, after its create.
fn simulate_deploy(fake: Fake, check: &str) -> Fake {
    let source = format!("{PROG}/source/main");

    fake.get(
        &format!("{source}?version=active"),
        200,
        "REPORT ytest_abapreach_new.",
    )
    .get(&format!("{source}/versions"), 200, &no_inactive())
    .post(
        &format!("{PROG}?_action=LOCK&accessMode=MODIFY"),
        200,
        &lock_body(),
    )
    .get(
        &format!("{source}?version=active"),
        200,
        "REPORT ytest_abapreach_new.",
    )
    .get(&format!("{source}/versions"), 200, &no_inactive())
    .post(CHECKRUNS, 200, check)
    .post(&format!("{PROG}?_action=UNLOCK&{HANDLE_QUERY}"), 200, "")
}

const FILE: &str = "REPORT ytest_abapreach_new.\nWRITE / 'v1'.";

#[test]
fn a_file_leads_to_a_simulate_deploy_and_no_source_write() {
    let mut fake = simulate_deploy(clean(Subc::Program, "$TMP", None, 200), PROCESSED);
    let result = run(
        &mut fake,
        with_file(input(Subc::Program, None, None, false), FILE),
    );

    assert_eq!(result.outcome.status, Some(Status::Active));
    assert_eq!(result.outcome.exit, 0, "{:?}", result.outcome);
    assert!(
        result.outcome.stdout.contains("CREATE: ACTIVE"),
        "{}",
        result.outcome.stdout
    );
    assert!(
        result.outcome.stdout.contains("SIMULATED_OK"),
        "{}",
        result.outcome.stdout
    );
    // The source is never written: no PUT, one create POST, and the lock cycle is closed.
    assert!(fake.calls.iter().all(|call| call.method != "PUT"));
    assert_eq!(fake.creates().len(), 1);
    assert_eq!(fake.locks, 0, "{:?}", fake.uris());
    // The deploy ends the session; the create does not end it a second time.
    assert_eq!(fake.closes, 1);
    fake.assert_finished();
}

#[test]
fn a_file_that_fails_its_simulate_deploy_leaves_the_created_object_partial() {
    let mut fake = simulate_deploy(clean(Subc::Program, "$TMP", None, 200), CHECK_ERROR);
    let result = run(
        &mut fake,
        with_file(input(Subc::Program, None, None, false), FILE),
    );

    assert_eq!(result.outcome.status, Some(Status::Partial));
    assert_eq!(result.outcome.exit, 1, "{:?}", result.outcome);
    assert!(
        result.outcome.stdout.contains("CREATE: PARTIAL"),
        "{}",
        result.outcome.stdout
    );
    assert!(
        !result.outcome.stdout.contains("CREATE: ACTIVE"),
        "{}",
        result.outcome.stdout
    );
    assert!(
        result.outcome.stdout.contains("REJECTED"),
        "{}",
        result.outcome.stdout
    );
    assert!(fake.calls.iter().all(|call| call.method != "PUT"));
    assert_eq!(fake.creates().len(), 1);
    assert_eq!(fake.locks, 0);
    assert_eq!(fake.closes, 1);
    fake.assert_finished();
}

#[test]
fn a_file_is_not_deployed_when_the_create_did_not_end_active() {
    let mut fake = absent(Fake::new())
        .post(&validation_for(Subc::Program, "$TMP"), 200, CHECK_OK)
        .post(PROGRAMS, 400, &exception("InvalidName", "Name refused"));
    let result = run(
        &mut fake,
        with_file(input(Subc::Program, None, None, false), FILE),
    );

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert!(!fake.uris().iter().any(|uri| uri.contains("source/main")));
    assert_eq!(fake.closes, 1);
    fake.assert_finished();

    // A simulated create with a FILE never deploys either.
    let mut fake = absent(Fake::new()).post(&validation_for(Subc::Program, "$TMP"), 200, CHECK_OK);
    let result = run(
        &mut fake,
        with_file(input(Subc::Program, None, None, true), FILE),
    );

    assert_eq!(result.outcome.status, Some(Status::SimulatedOk));
    assert!(!fake.uris().iter().any(|uri| uri.contains("source/main")));
}

// ---- uncertain POST: reconciliation, never a retry ----------------------------------------

fn lost() -> Reply {
    Err(TransportError::PostSendUncertain(
        "reply dropped".to_owned(),
    ))
}

/// The create script up to the POST, with `reply` as the answer of the POST.
fn until_post(subc: Subc, reply: Reply) -> Fake {
    absent(Fake::new())
        .post(&validation_for(subc, "$TMP"), 200, CHECK_OK)
        .on("POST", collection(subc), reply)
}

#[test]
fn an_uncertain_post_with_the_object_there_is_partial_and_is_never_repeated() {
    for subc in BOTH {
        for reply in [lost(), Ok(Response::text(500, ""))] {
            // The read-back asks the program URI, then the include URI.
            let mut fake = until_post(subc, reply);

            fake = match subc {
                Subc::Program => fake.get(PROG, 200, "<program:abapProgram/>"),
                Subc::Include => fake
                    .get(PROG, 404, "")
                    .get(INCL, 200, "<include:abapInclude/>"),
            };

            let result = run(&mut fake, input(subc, None, None, false));

            assert_eq!(result.outcome.status, Some(Status::Partial), "{subc:?}");
            assert_eq!(result.outcome.exit, 1);
            assert!(
                result.outcome.stdout.contains("CREATE: PARTIAL"),
                "{subc:?}: {}",
                result.outcome.stdout
            );
            assert!(
                result
                    .outcome
                    .stdout
                    .contains(&format!("PROG {NAME} exists")),
                "{subc:?}: {}",
                result.outcome.stdout
            );
            assert!(!result.fallback_allowed);
            assert_eq!(fake.creates().len(), 1, "{subc:?}: never a second POST");
            assert_eq!(fake.closes, 1);
            fake.assert_finished();
        }
    }
}

#[test]
fn an_uncertain_post_with_the_object_absent_is_rejected() {
    for subc in BOTH {
        let mut fake = until_post(subc, lost())
            .get(PROG, 404, "")
            .get(INCL, 404, "");
        let result = run(&mut fake, input(subc, None, None, false));

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{subc:?}");
        assert_eq!(result.outcome.exit, 1);
        assert!(
            result.outcome.stdout.contains("CREATE: REJECTED"),
            "{subc:?}: {}",
            result.outcome.stdout
        );
        assert!(
            result.outcome.stdout.contains("was not found at read-back"),
            "{subc:?}"
        );
        assert_eq!(fake.creates().len(), 1, "{subc:?}");
        fake.assert_finished();
    }
}

#[test]
fn a_failed_reconciliation_read_is_partial_and_the_create_is_not_repeated() {
    for subc in BOTH {
        let failures: Vec<Fake> = vec![
            until_post(subc, lost()).get(PROG, 500, ""),
            until_post(subc, lost())
                .get(PROG, 404, "")
                .get(INCL, 500, ""),
            until_post(subc, lost()).on(
                "GET",
                PROG,
                Err(TransportError::NoResponse("read failed".to_owned())),
            ),
            until_post(subc, lost()).get(PROG, 401, ""),
        ];

        for mut fake in failures {
            let result = run(&mut fake, input(subc, None, None, false));

            assert_eq!(result.outcome.status, Some(Status::Partial), "{subc:?}");
            assert_eq!(result.outcome.exit, 1);
            assert!(
                result.outcome.stdout.contains("cannot be read back"),
                "{subc:?}: {}",
                result.outcome.stdout
            );
            assert!(!result.fallback_allowed);
            assert_eq!(fake.creates().len(), 1, "{subc:?}: never a second POST");
            fake.assert_finished();
        }
    }
}

#[test]
fn a_client_error_on_the_post_is_a_rejection_without_reconciliation() {
    for subc in BOTH {
        let mut fake = until_post(
            subc,
            Ok(Response::text(
                403,
                exception("ExceptionNoAuthorization", "No create authorization"),
            )),
        );
        let result = run(&mut fake, input(subc, None, None, false));

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{subc:?}");
        assert!(
            result
                .outcome
                .stdout
                .contains("HTTP 403: No create authorization"),
            "{subc:?}: {}",
            result.outcome.stdout
        );
        // Resolution twice (before the create), nothing afterwards.
        assert_eq!(fake.calls.len(), 4, "{subc:?}: {:?}", fake.uris());
        assert_eq!(fake.creates().len(), 1);
    }
}

#[test]
fn a_post_that_was_never_sent_is_an_error_that_still_allows_a_fallback() {
    let mut fake = until_post(
        Subc::Include,
        Err(TransportError::PreSendConnectivity("no route".to_owned())),
    );
    let result = run(&mut fake, input(Subc::Include, None, None, false));

    assert_eq!(result.outcome.status, None);
    assert_eq!(result.outcome.exit, 1);
    assert!(result.fallback_allowed);
    assert_eq!(result.failure, Some(Failure::PreSendConnectivity));
    assert_eq!(fake.creates().len(), 1);
}

#[test]
fn a_resolution_that_cannot_decide_stops_the_create_before_the_post() {
    for status in [403, 500] {
        let mut fake = Fake::new().get(PROG, status, "");
        let result = run(&mut fake, input(Subc::Program, None, None, false));

        assert_eq!(result.outcome.exit, 1, "{status}");
        assert!(
            result
                .outcome
                .stderr
                .contains(&format!("cannot resolve PROG {NAME}: HTTP {status}")),
            "{status}: {}",
            result.outcome.stderr
        );
        assert!(fake.creates().is_empty());
        assert_eq!(fake.calls.len(), 1);
    }
}
