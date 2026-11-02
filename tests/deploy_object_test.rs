//! The M3 deploy flow for FUNC, CLAS (main and local includes) and INTF, against a scripted
//! ADT. Every request is matched by method and exact URI, so the URIs, the lock parent, the
//! handle encoding and the `corrNr` rules are part of what the tests assert.

use abapreach::adt::{ActivationBody, classify_activation, parse_object_refs};
use abapreach::cli::{IncludeKind, ObjectKind};
use abapreach::config::Connection;
use abapreach::domain::Status;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{
    CancelFlag, DeployInput, DeployProg, DeployResult, DeployScope,
};
use abapreach::policy::{ForeignInactive, Policies};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use std::collections::VecDeque;

const USER: &str = "DEVUSER";
const OTHER: &str = "OTHERUSER";
const HANDLE_QUERY: &str = "lockHandle=h%2B1%2F%3D";
const CHECKRUNS: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const ACTIVATE: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";
const ACTIVATE_AGAIN: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=false";
const INACTIVE_OBJECTS: &str = "/sap/bc/adt/activation/inactiveobjects";

const FM: &str = "YTEST_ABAPREACH_FM";
const FM_URI: &str = "/sap/bc/adt/functions/groups/ytest_abapreach_fg/fmodules/ytest_abapreach_fm";
const GROUP_URI: &str = "/sap/bc/adt/functions/groups/ytest_abapreach_fg";
const CLAS: &str = "YTEST_ABAPREACH_CL";
const CLAS_URI: &str = "/sap/bc/adt/oo/classes/ytest_abapreach_cl";
const INTF: &str = "YTEST_ABAPREACH_IF";
const INTF_URI: &str = "/sap/bc/adt/oo/interfaces/ytest_abapreach_if";
const TR: &str = "DEVK900001";

type Reply = Result<Response, TransportError>;
type Rule = (String, String, VecDeque<Reply>);

/// A scripted ADT: a reply per method and exact URI, taken in order. An unscripted request is a
/// failed test, never a quiet success.
struct Fake {
    rules: Vec<Rule>,
    calls: Vec<Request>,
    closes: usize,
    cancel_on_lock: Option<CancelFlag>,
}

impl Fake {
    fn new() -> Self {
        Self {
            rules: Vec::new(),
            calls: Vec::new(),
            closes: 0,
            cancel_on_lock: None,
        }
    }

    fn on(mut self, method: &str, uri: &str, reply: Result<Response, TransportError>) -> Self {
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

    fn put(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("PUT", uri, Ok(Response::text(status, body)))
    }

    fn cancelling(mut self, cancel: &CancelFlag) -> Self {
        self.cancel_on_lock = Some(cancel.clone());

        self
    }

    fn count(&self, method: &str, needle: &str) -> usize {
        self.calls
            .iter()
            .filter(|call| call.method == method && call.uri.contains(needle))
            .count()
    }

    /// The position of the first call, so the order of the writes can be asserted.
    fn position(&self, method: &str, needle: &str) -> usize {
        self.calls
            .iter()
            .position(|call| call.method == method && call.uri.contains(needle))
            .unwrap_or_else(|| panic!("no {method} {needle} in {:?}", self.uris()))
    }

    fn uris(&self) -> Vec<String> {
        self.calls
            .iter()
            .map(|call| format!("{} {}", call.method, call.uri))
            .collect()
    }

    fn unlocks(&self) -> usize {
        self.count("POST", "_action=UNLOCK")
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

        let reply = self
            .rules
            .iter_mut()
            .find(|(method, uri, replies)| {
                *method == request.method && *uri == request.uri && !replies.is_empty()
            })
            .and_then(|(_, _, replies)| replies.pop_front())
            .unwrap_or_else(|| panic!("unscripted request: {} {}", request.method, request.uri));

        if request.uri.contains("_action=LOCK")
            && let Some(cancel) = &self.cancel_on_lock
        {
            cancel.set();
        }

        reply
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes += 1;

        Ok(())
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn lock_body(corrnr: &str, local: bool) -> String {
    format!(
        concat!(
            r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
            r#"<LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR>{}</CORRNR><IS_LOCAL>{}</IS_LOCAL>"#,
            r#"</DATA></asx:values></asx:abap>"#,
        ),
        corrnr,
        if local { "X" } else { "" }
    )
}

fn feed(entries: &[(&str, &str)]) -> String {
    let entries: String = entries
        .iter()
        .map(|(author, id)| {
            format!(
                "<atom:entry><atom:author><atom:name>{author}</atom:name></atom:author>\
                 <atom:id>{id}</atom:id></atom:entry>"
            )
        })
        .collect();

    format!(r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom">{entries}</atom:feed>"#)
}

fn no_inactive() -> String {
    feed(&[(USER, "00000")])
}

fn inactive_of(author: &str) -> String {
    feed(&[(author, "99999"), (USER, "00000")])
}

const CLEAN: &str = r#"<chkrun:checkReport xmlns:chkrun="http://www.sap.com/adt/checkrun" chkrun:status="processed"/>"#;
const CHECK_ERROR: &str = r#"<chkrun:checkReport xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkMessageList><chkrun:checkMessage chkrun:type="E" chkrun:uri="/x#start=2,0" chkrun:shortText="bad"/></chkrun:checkMessageList></chkrun:checkReport>"#;
const MESSAGES_E: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="E" line="1"><shortText><txt>broken</txt></shortText></msg></chkl:messages>"#;
const MESSAGES_W: &str = r#"<chkl:messages xmlns:chkl="x"><msg type="W" line="1"><shortText><txt>unused include</txt></shortText></msg></chkl:messages>"#;

fn input(source: &[String], simulate: bool, tr: Option<&str>) -> DeployInput {
    DeployInput {
        name: String::new(),
        source: source.to_vec(),
        base: None,
        simulate,
        tr: tr.map(str::to_owned),
    }
}

fn connection(policy: ForeignInactive) -> Connection {
    let mut connection = Connection::test_insecure();

    connection.user = USER.to_owned();
    connection.policies = Policies {
        foreign_inactive: policy,
    };

    if let Some(adt) = connection.adt.as_mut() {
        adt.insecure = false;
    }

    connection
}

fn run(
    fake: &mut Fake,
    name: &str,
    scope: DeployScope,
    mut deploy: DeployInput,
    policy: ForeignInactive,
) -> DeployResult {
    deploy.name = name.to_owned();

    DeployProg::run_scoped(
        fake,
        deploy,
        scope,
        None,
        Some(&connection(policy)),
        &CancelFlag::new(),
    )
}

fn stdout_of(result: &DeployResult) -> Vec<&str> {
    result.outcome.stdout.lines().collect()
}

// ---------------------------------------------------------------- FUNC

fn func_source(version: &str) -> Vec<String> {
    lines(&format!(
        "FUNCTION ytest_abapreach_fm\n  IMPORTING\n    VALUE(iv_in) TYPE i.\n\n  WRITE / '{version}'.\nENDFUNCTION."
    ))
}

/// How SAP renders the module: upper-case signature, three blank lines, CRLF.
fn func_rendered(version: &str) -> String {
    format!(
        "FUNCTION YTEST_ABAPREACH_FM\r\n  IMPORTING\r\n    VALUE(IV_IN) TYPE I.\r\n\r\n\r\n\r\n  WRITE / '{version}'.\r\nENDFUNCTION."
    )
}

fn func_scope() -> DeployScope {
    DeployScope::new(ObjectKind::Func)
}

fn search_reply() -> String {
    format!(
        concat!(
            r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/YTEST_ABAPREACH_FG/fmodules/{}" "#,
            r#"adtcore:type="FUGR/FF" adtcore:name="{}"/></adtcore:objectReferences>"#,
        ),
        FM, FM
    )
}

fn func_search() -> String {
    abapreach::adt::function_group_search_uri(FM)
}

fn empty_worklist() -> String {
    r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#.to_owned()
}

/// The reads that every FUNC deploy makes before the lock.
fn func_before_lock(live: &str, inactive_objects: &str) -> Fake {
    Fake::new()
        .get(&func_search(), 200, &search_reply())
        .get(&format!("{FM_URI}/source/main?version=active"), 200, live)
        .get(
            &format!("{FM_URI}/source/main/versions"),
            200,
            &no_inactive(),
        )
        .get(INACTIVE_OBJECTS, 200, inactive_objects)
}

fn func_locked_phase(fake: Fake, live: &str) -> Fake {
    fake.post(
        &format!("{FM_URI}?_action=LOCK&accessMode=MODIFY"),
        200,
        &lock_body("", true),
    )
    .get(&format!("{FM_URI}/source/main?version=active"), 200, live)
    .get(
        &format!("{FM_URI}/source/main/versions"),
        200,
        &no_inactive(),
    )
    .post(CHECKRUNS, 200, CLEAN)
}

#[test]
fn a_func_deploy_compares_the_server_rendered_source_and_locks_the_module() {
    let live = func_rendered("v1");
    let rendered = func_rendered("v2");
    let mut fake = func_locked_phase(func_before_lock(&live, &empty_worklist()), &live)
        .put(
            &format!("{FM_URI}/source/main?{HANDLE_QUERY}"),
            200,
            &rendered,
        )
        .post(&format!("{FM_URI}?_action=UNLOCK&{HANDLE_QUERY}"), 200, "")
        .post(ACTIVATE, 200, "")
        .get(
            &format!("{FM_URI}/source/main?version=active"),
            200,
            &rendered,
        );
    let result = run(
        &mut fake,
        FM,
        func_scope(),
        input(&func_source("v2"), false, None),
        ForeignInactive::Reject,
    );
    let hash = source_hash(&lines(&rendered));

    assert_eq!(result.outcome.status, Some(Status::Active));
    assert_eq!(
        (result.outcome.exit, result.outcome.stderr.as_str()),
        (0, "")
    );
    assert!(
        result
            .outcome
            .stdout
            .ends_with(&format!("ACTIVE\nhash {hash}")),
        "{}",
        result.outcome.stdout
    );
    assert_ne!(
        hash,
        source_hash(&func_source("v2")),
        "the file is not what is active"
    );
    // The module is locked and activated as itself, not as its function group.
    assert!(
        fake.calls[fake.position("POST", "_action=LOCK")]
            .uri
            .starts_with(FM_URI)
    );
    let activation = &fake.calls[fake.position("POST", ACTIVATE)];
    let body = String::from_utf8_lossy(&activation.body);
    assert!(
        body.contains(&format!(r#"adtcore:uri="{FM_URI}""#)),
        "{body}"
    );
    let check =
        String::from_utf8_lossy(&fake.calls[fake.position("POST", CHECKRUNS)].body).into_owned();
    assert!(
        check.contains(&format!(r#"chkrun:uri="{FM_URI}/source/main""#)),
        "{check}"
    );
    assert_eq!(fake.unlocks(), 1);
    assert_eq!(fake.closes, 1);
    fake.assert_finished();
}

#[test]
fn a_func_file_equal_to_the_rendered_source_is_a_fixpoint_without_changes() {
    let live = func_rendered("v1");
    let mut fake = Fake::new().get(&func_search(), 200, &search_reply()).get(
        &format!("{FM_URI}/source/main?version=active"),
        200,
        &live,
    );
    let result = run(
        &mut fake,
        FM,
        func_scope(),
        input(&lines(&live), false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.stdout, "no changes");
    assert_eq!(result.outcome.exit, 0);
    assert_eq!(fake.calls.len(), 2, "{:?}", fake.uris());
    fake.assert_finished();
}

#[test]
fn a_func_put_reply_without_a_body_accepts_only_a_changed_source_with_the_same_lines() {
    let live = func_rendered("v1");
    let active_after = func_rendered("v2");

    for (after, expected) in [
        (active_after.as_str(), Status::Active),
        (live.as_str(), Status::Inactive),
    ] {
        let mut fake = func_locked_phase(func_before_lock(&live, &empty_worklist()), &live)
            .put(&format!("{FM_URI}/source/main?{HANDLE_QUERY}"), 200, "")
            .post(&format!("{FM_URI}?_action=UNLOCK&{HANDLE_QUERY}"), 200, "")
            .post(ACTIVATE, 200, "")
            .get(&format!("{FM_URI}/source/main?version=active"), 200, after);
        let result = run(
            &mut fake,
            FM,
            func_scope(),
            input(&func_source("v2"), false, None),
            ForeignInactive::Reject,
        );

        assert_eq!(
            result.outcome.status,
            Some(expected),
            "{}",
            result.outcome.stdout
        );
    }
}

#[test]
fn a_func_with_its_own_inactive_version_is_rejected_even_under_allow() {
    for owner in [USER, OTHER] {
        let live = func_rendered("v1");
        let mut fake = Fake::new()
            .get(&func_search(), 200, &search_reply())
            .get(&format!("{FM_URI}/source/main?version=active"), 200, &live)
            .get(
                &format!("{FM_URI}/source/main/versions"),
                200,
                &inactive_of(owner),
            );
        let result = run(
            &mut fake,
            FM,
            func_scope(),
            input(&func_source("v2"), false, None),
            ForeignInactive::Allow,
        );

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{owner}");
        assert_eq!(result.outcome.exit, 1);
        assert!(
            result.outcome.stdout.ends_with(
                "REJECTED: Function module YTEST_ABAPREACH_FM has an inactive version; \
                 activate or discard it first in SE37"
            ),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.count("POST", "_action=LOCK"), 0);
        assert_eq!(fake.count("PUT", ""), 0);
    }
}

#[test]
fn inactive_siblings_in_the_function_group_warn_without_rejecting() {
    let live = func_rendered("v1");
    let worklist = format!(
        concat!(
            r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects">"#,
            r#"<ioc:entry><ioc:object><ioc:ref adtcore:uri="{group}" adtcore:type="FUGR/F" "#,
            r#"adtcore:name="YTEST_ABAPREACH_FG" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object></ioc:entry>"#,
            r#"<ioc:entry><ioc:object><ioc:ref adtcore:uri="{group}/fmodules/ytest_abapreach_fm2" "#,
            r#"adtcore:type="FUGR/FF" adtcore:name="YTEST_ABAPREACH_FM2" adtcore:parentUri="{group}" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object></ioc:entry>"#,
            r#"<ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zother" "#,
            r#"adtcore:type="CLAS/OC" adtcore:name="ZOTHER" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object></ioc:entry>"#,
            r#"</ioc:inactiveObjects>"#,
        ),
        group = GROUP_URI
    );
    let mut fake = func_locked_phase(func_before_lock(&live, &worklist), &live).post(
        &format!("{FM_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
        200,
        "",
    );
    let result = run(
        &mut fake,
        FM,
        func_scope(),
        input(&func_source("v2"), true, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.status, Some(Status::SimulatedOk));
    assert!(
        result.outcome.stderr.contains(
            "W: inactive objects in function group YTEST_ABAPREACH_FG: YTEST_ABAPREACH_FM2\n"
        ),
        "{}",
        result.outcome.stderr
    );
    assert!(
        result
            .outcome
            .stderr
            .contains("W: the syntax check of YTEST_ABAPREACH_FM can fail because of them"),
        "{}",
        result.outcome.stderr
    );
    assert!(!result.outcome.stderr.contains("ZOTHER"));
}

#[test]
fn a_supplied_group_that_differs_from_the_resolved_one_stops_after_the_resolution() {
    let mut fake = Fake::new().get(&func_search(), 200, &search_reply());
    let result = run(
        &mut fake,
        FM,
        func_scope().group(Some("OTHER_FG")),
        input(&func_source("v2"), false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.exit, 2);
    assert_eq!(
        result.outcome.stderr,
        "STOP: --fugr OTHER_FG does not match function group YTEST_ABAPREACH_FG of the function module"
    );
    assert_eq!(fake.calls.len(), 1);
}

#[test]
fn a_func_group_that_cannot_be_resolved_before_any_write_may_fall_back() {
    let mut fake = Fake::new().on(
        "GET",
        &func_search(),
        Err(TransportError::PreSendConnectivity("no route".to_owned())),
    );
    let result = run(
        &mut fake,
        FM,
        func_scope(),
        input(&func_source("v2"), false, None),
        ForeignInactive::Reject,
    );

    assert!(result.fallback_allowed);
    assert_eq!(result.outcome.exit, 1);
}

// ---------------------------------------------------------------- CLAS

fn main_source(version: &str) -> Vec<String> {
    lines(&format!(
        "CLASS ytest_abapreach_cl DEFINITION PUBLIC.\nENDCLASS.\nCLASS ytest_abapreach_cl IMPLEMENTATION.\n\" {version}\nENDCLASS."
    ))
}

fn include_source(version: &str) -> Vec<String> {
    lines(&format!(
        "CLASS ltc_a DEFINITION FOR TESTING.\nENDCLASS.\n\" {version}"
    ))
}

fn text_of(source: &[String]) -> String {
    source.join("\r\n")
}

fn include_uri(include: IncludeKind) -> String {
    format!("{CLAS_URI}/includes/{}", include.segment())
}

fn clas_scope(include: Option<IncludeKind>) -> DeployScope {
    DeployScope::new(ObjectKind::Clas).include(include)
}

/// The class deploy up to the PUT, for the source URI `source` (main or one include).
fn clas_through_check(source: &str, live: &str, corrnr: &str, local: bool, feeds: &[&str]) -> Fake {
    let mut fake = Fake::new().get(&format!("{source}?version=active"), 200, live);

    // The feeds are read before the lock and again right after it.
    for _ in 0..2 {
        for feed in feeds {
            fake = fake.get(feed, 200, &no_inactive());
        }
    }

    fake.post(
        &format!("{CLAS_URI}?_action=LOCK&accessMode=MODIFY"),
        200,
        &lock_body(corrnr, local),
    )
    .get(&format!("{source}?version=active"), 200, live)
    .post(CHECKRUNS, 200, CLEAN)
}

fn main_feed() -> String {
    format!("{CLAS_URI}/includes/main/versions")
}

fn clas_activation_tail(fake: Fake, source: &str, file: &str) -> Fake {
    fake.post(
        &format!("{CLAS_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
        200,
        "",
    )
    .post(ACTIVATE, 200, "")
    .get(&format!("{source}?version=active"), 200, file)
    .get(&format!("{source}?version=inactive"), 200, file)
}

#[test]
fn a_class_main_deploy_locks_and_activates_the_class_and_reads_it_back_inactive_too() {
    let source = format!("{CLAS_URI}/source/main");
    let live = text_of(&main_source("v1"));
    let file = main_source("v2");
    let fake = clas_through_check(&source, &live, "", true, &[&main_feed()]).put(
        &format!("{source}?{HANDLE_QUERY}"),
        200,
        "",
    );
    let mut fake = clas_activation_tail(fake, &source, &text_of(&file));
    let result = run(
        &mut fake,
        CLAS,
        clas_scope(None),
        input(&file, false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::Active),
        "{:?}",
        result.outcome
    );
    assert!(
        result
            .outcome
            .stdout
            .ends_with(&format!("ACTIVE\nhash {}", source_hash(&file))),
        "{}",
        result.outcome.stdout
    );
    assert_eq!(fake.count("GET", &main_feed()), 2);
    assert_eq!(fake.unlocks(), 1);
    fake.assert_finished();
}

#[test]
fn a_class_include_keeps_its_own_source_and_hash_and_locks_and_activates_the_class() {
    let include = IncludeKind::Ccimp;
    let source = include_uri(include);
    let own_feed = format!("{source}/versions");
    let live = text_of(&include_source("v1"));
    let file = include_source("v2");
    let fake = clas_through_check(&source, &live, "", true, &[&own_feed, &main_feed()]).put(
        &format!("{source}?{HANDLE_QUERY}"),
        200,
        "",
    );
    let mut fake = clas_activation_tail(fake, &source, &text_of(&file));
    let mut deploy = input(&file, false, None);

    deploy.base = Some(source_hash(&lines(&live)));

    let result = run(
        &mut fake,
        CLAS,
        clas_scope(Some(include)),
        deploy,
        ForeignInactive::Reject,
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::Active),
        "{:?}",
        result.outcome
    );
    assert!(
        result
            .outcome
            .stdout
            .ends_with(&format!("ACTIVE\nhash {}", source_hash(&file)))
    );
    let lock = &fake.calls[fake.position("POST", "_action=LOCK")];
    let check =
        String::from_utf8_lossy(&fake.calls[fake.position("POST", CHECKRUNS)].body).into_owned();
    let activation =
        String::from_utf8_lossy(&fake.calls[fake.position("POST", ACTIVATE)].body).into_owned();

    assert!(lock.uri.starts_with(CLAS_URI) && !lock.uri.contains("/includes/"));
    assert!(
        check.contains(&format!(r#"adtcore:uri="{CLAS_URI}""#)),
        "{check}"
    );
    assert!(
        check.contains(&format!(r#"chkrun:uri="{source}""#)),
        "{check}"
    );
    assert!(
        activation.contains(&format!(r#"adtcore:uri="{CLAS_URI}""#)),
        "{activation}"
    );
    // The guard reads the include feed and, because the class activates as a whole, the main feed.
    assert_eq!(fake.count("GET", &own_feed), 2);
    assert_eq!(fake.count("GET", &main_feed()), 2);
    assert_eq!(fake.unlocks(), 1);
    fake.assert_finished();
}

#[test]
fn a_base_of_another_source_stops_the_include_deploy_before_the_lock() {
    let include = IncludeKind::Ccdef;
    let source = include_uri(include);
    let mut fake = Fake::new().get(&format!("{source}?version=active"), 200, "live");
    let mut deploy = input(&include_source("v2"), false, None);

    deploy.base = Some(source_hash(&main_source("v1")));

    let result = run(
        &mut fake,
        CLAS,
        clas_scope(Some(include)),
        deploy,
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.exit, 2);
    assert!(
        result
            .outcome
            .stderr
            .starts_with("STOP: live changed: live hash ")
    );
    assert_eq!(fake.calls.len(), 1);
}

#[test]
fn a_transportable_class_sends_the_corrnr_on_the_put_and_never_on_lock_or_activation() {
    let source = format!("{CLAS_URI}/source/main");
    let live = text_of(&main_source("v1"));
    let file = main_source("v2");
    let fake = clas_through_check(&source, &live, TR, false, &[&main_feed()]).put(
        &format!("{source}?{HANDLE_QUERY}&corrNr={TR}"),
        200,
        "",
    );
    let mut fake = clas_activation_tail(fake, &source, &text_of(&file));
    let result = run(
        &mut fake,
        CLAS,
        clas_scope(None),
        input(&file, false, Some(TR)),
        ForeignInactive::Reject,
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::Active),
        "{:?}",
        result.outcome
    );
    assert!(
        result
            .outcome
            .stdout
            .contains(&format!("transport request {TR}"))
    );

    for call in &fake.calls {
        let writes = call.method == "PUT";

        assert_eq!(
            call.uri.contains("corrNr="),
            writes,
            "{} {}",
            call.method,
            call.uri
        );
    }
}

#[test]
fn a_transport_request_other_than_the_lock_owner_stops_and_unlocks_without_a_write() {
    let source = format!("{CLAS_URI}/source/main");
    let live = text_of(&main_source("v1"));
    let fake = clas_through_check(&source, &live, TR, false, &[&main_feed()]);
    let mut fake = fake.post(
        &format!("{CLAS_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
        200,
        "",
    );
    let result = run(
        &mut fake,
        CLAS,
        clas_scope(None),
        input(&main_source("v2"), false, Some("DEVK900002")),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.exit, 2);
    assert!(
        result
            .outcome
            .stderr
            .contains("does not match the transport request")
    );
    assert_eq!(fake.count("PUT", ""), 0);
    assert_eq!(fake.unlocks(), 1);
}

fn clas_entry(uri: &str, kind: &str, name: &str, parent: Option<&str>) -> String {
    let parent = parent.map_or(String::new(), |parent| {
        format!(r#" adtcore:parentUri="{parent}""#)
    });

    format!(
        concat!(
            r#"<ioc:entry><ioc:object ioc:user="" ioc:deleted="false"><ioc:ref adtcore:uri="{}" "#,
            r#"adtcore:type="{}" adtcore:name="{}"{} xmlns:adtcore="http://www.sap.com/adt/core"/>"#,
            r#"</ioc:object><ioc:transport/></ioc:entry>"#,
        ),
        uri, kind, name, parent
    )
}

fn worklist(extra: &[String]) -> String {
    let transport = r#"<ioc:entry><ioc:object/><ioc:transport ioc:user="DEVUSER" ioc:linked="false"><ioc:ref adtcore:uri="/sap/bc/adt/vit/wb/object_type/%20%20%20%20rq/object_name/DEVK900001" adtcore:type="/RQ" adtcore:name="DEVK900001" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:transport></ioc:entry>"#;
    let mut entries = vec![
        transport.to_owned(),
        clas_entry(CLAS_URI, "CLAS/OC", CLAS, None),
        clas_entry(
            &format!("{CLAS_URI}/includes/definitions"),
            "CLAS/OCN/definitions",
            CLAS,
            Some(CLAS_URI),
        ),
        clas_entry(
            &format!("{CLAS_URI}/source/main#type=CLAS%2FOM;name=PROBE"),
            "CLAS/OM/public",
            "YTEST_ABAPREACH_CL           PROBE",
            Some(CLAS_URI),
        ),
    ];

    entries.extend_from_slice(extra);

    format!(
        r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects">{}</ioc:inactiveObjects>"#,
        entries.concat()
    )
}

fn transportable_class_put() -> (String, Fake) {
    let source = format!("{CLAS_URI}/source/main");
    let live = text_of(&main_source("v1"));
    let fake = clas_through_check(&source, &live, TR, false, &[&main_feed()])
        .put(&format!("{source}?{HANDLE_QUERY}&corrNr={TR}"), 200, "")
        .post(
            &format!("{CLAS_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
            200,
            "",
        );

    (source, fake)
}

fn deploy_transportable_class(fake: &mut Fake, file: &[String]) -> DeployResult {
    run(
        fake,
        CLAS,
        clas_scope(None),
        input(file, false, Some(TR)),
        ForeignInactive::Reject,
    )
}

#[test]
fn a_worklist_answer_is_resent_with_every_object_as_listed_and_without_the_preaudit() {
    let file = main_source("v2");
    let (source, fake) = transportable_class_put();
    let mut fake = fake
        .post(ACTIVATE, 200, &worklist(&[]))
        .post(ACTIVATE_AGAIN, 200, "")
        .get(&format!("{source}?version=active"), 200, &text_of(&file))
        .get(&format!("{source}?version=inactive"), 200, &text_of(&file));
    let result = deploy_transportable_class(&mut fake, &file);

    assert_eq!(
        result.outcome.status,
        Some(Status::Active),
        "{:?}",
        result.outcome
    );
    assert_eq!(fake.count("POST", "method=activate"), 2);
    assert_eq!(fake.count("POST", ACTIVATE), 1);

    let resend = String::from_utf8_lossy(&fake.calls[fake.position("POST", ACTIVATE_AGAIN)].body)
        .into_owned();

    for expected in [
        format!(r#"adtcore:uri="{CLAS_URI}" adtcore:type="CLAS/OC" adtcore:name="{CLAS}"/>"#),
        format!(
            r#"adtcore:uri="{CLAS_URI}/includes/definitions" adtcore:type="CLAS/OCN/definitions" adtcore:name="{CLAS}" adtcore:parentUri="{CLAS_URI}"/>"#
        ),
        format!(
            r#"adtcore:uri="{CLAS_URI}/source/main#type=CLAS%2FOM;name=PROBE" adtcore:type="CLAS/OM/public" adtcore:name="YTEST_ABAPREACH_CL           PROBE" adtcore:parentUri="{CLAS_URI}"/>"#
        ),
    ] {
        assert!(resend.contains(&expected), "{expected}\nin {resend}");
    }

    // The request entry carries no object and is not an object reference.
    assert!(!resend.contains("DEVK900001"), "{resend}");
    assert_eq!(resend.matches("<adtcore:objectReference ").count(), 3);
}

#[test]
fn a_worklist_entry_outside_the_class_is_inactive_and_nothing_is_resent() {
    for outside in [
        "/sap/bc/adt/oo/classes/ytest_abapreach_other",
        // Shares the prefix of the class URI but is another class.
        "/sap/bc/adt/oo/classes/ytest_abapreach_cl2",
    ] {
        let file = main_source("v2");
        let (_, fake) = transportable_class_put();
        let mut fake = fake.post(
            ACTIVATE,
            200,
            &worklist(&[clas_entry(outside, "CLAS/OC", "OTHER", None)]),
        );
        let result = deploy_transportable_class(&mut fake, &file);

        assert_eq!(result.outcome.status, Some(Status::Inactive), "{outside}");
        assert_eq!(result.outcome.exit, 1);
        assert!(
            result.outcome.stdout.contains("outside the class"),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.count("POST", "method=activate"), 1);
        assert_eq!(fake.unlocks(), 1);
    }
}

#[test]
fn a_worklist_that_comes_back_after_the_resend_is_inactive() {
    let file = main_source("v2");
    let (_, fake) = transportable_class_put();
    let mut fake =
        fake.post(ACTIVATE, 200, &worklist(&[]))
            .post(ACTIVATE_AGAIN, 200, &worklist(&[]));
    let result = deploy_transportable_class(&mut fake, &file);

    assert_eq!(result.outcome.status, Some(Status::Inactive));
    assert_eq!(fake.count("POST", "method=activate"), 2);
}

#[test]
fn class_activation_messages_with_an_error_are_inactive_and_warnings_need_a_reread() {
    let file = main_source("v2");
    let old = main_source("v1");

    // An error message is a failure whatever the reread would say.
    let (_, fake) = transportable_class_put();
    let mut fake = fake.post(ACTIVATE, 200, MESSAGES_E);
    let result = deploy_transportable_class(&mut fake, &file);

    assert_eq!(result.outcome.status, Some(Status::Inactive));
    assert!(result.outcome.stdout.contains("E row 1: broken"));
    assert_eq!(
        fake.count("GET", "version=active"),
        2,
        "no reread after an error"
    );

    // A warning-only answer proves nothing: the active source decides.
    for (active, expected) in [(&file, Status::Active), (&old, Status::Inactive)] {
        let (source, fake) = transportable_class_put();
        let mut fake = fake
            .post(ACTIVATE, 200, MESSAGES_W)
            .get(&format!("{source}?version=active"), 200, &text_of(active))
            .get(&format!("{source}?version=inactive"), 200, &text_of(active));
        let result = deploy_transportable_class(&mut fake, &file);

        assert_eq!(
            result.outcome.status,
            Some(expected),
            "{:?}",
            result.outcome
        );
        assert!(result.outcome.stdout.contains("W row 1: unused include"));
    }
}

#[test]
fn a_class_whose_inactive_source_still_differs_after_activation_is_inactive() {
    let file = main_source("v2");
    let (source, fake) = transportable_class_put();
    let mut fake = fake
        .post(ACTIVATE, 200, "")
        .get(&format!("{source}?version=active"), 200, &text_of(&file))
        .get(
            &format!("{source}?version=inactive"),
            200,
            &text_of(&main_source("v3")),
        );
    let result = deploy_transportable_class(&mut fake, &file);

    assert_eq!(result.outcome.status, Some(Status::Inactive));
    assert_eq!(result.outcome.exit, 1);
}

#[test]
fn the_guard_reads_every_class_feed_and_fails_closed() {
    let include = IncludeKind::Ccimp;
    let source = include_uri(include);
    let own_feed = format!("{source}/versions");
    let cases: [(Response, Response, &str); 3] = [
        (
            Response::text(200, inactive_of(OTHER)),
            Response::text(200, no_inactive()),
            "inactive version of another user exists",
        ),
        (
            Response::text(200, no_inactive()),
            Response::text(200, inactive_of(OTHER)),
            "inactive version of another user exists",
        ),
        (
            Response::text(200, no_inactive()),
            Response::empty(500),
            "version list unavailable: HTTP 500",
        ),
    ];

    for (own, main, reason) in cases {
        let mut fake = Fake::new()
            .get(&format!("{source}?version=active"), 200, "live")
            .on("GET", &own_feed, Ok(own))
            .on("GET", &main_feed(), Ok(main));
        let result = run(
            &mut fake,
            CLAS,
            clas_scope(Some(include)),
            input(&include_source("v2"), false, None),
            ForeignInactive::Reject,
        );

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{reason}");
        assert!(
            result.outcome.stdout.contains(reason),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.count("POST", "_action=LOCK"), 0);
    }
}

#[test]
fn a_failing_checkrun_rejects_unlocks_and_writes_nothing() {
    let source = format!("{CLAS_URI}/source/main");
    let live = text_of(&main_source("v1"));
    let mut fake = Fake::new()
        .get(&format!("{source}?version=active"), 200, &live)
        .get(&main_feed(), 200, &no_inactive())
        .get(&main_feed(), 200, &no_inactive())
        .post(
            &format!("{CLAS_URI}?_action=LOCK&accessMode=MODIFY"),
            200,
            &lock_body("", true),
        )
        .get(&format!("{source}?version=active"), 200, &live)
        .post(CHECKRUNS, 200, CHECK_ERROR)
        .post(
            &format!("{CLAS_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
            200,
            "",
        );
    let result = run(
        &mut fake,
        CLAS,
        clas_scope(None),
        input(&main_source("v2"), false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(fake.count("PUT", ""), 0);
    assert_eq!(fake.unlocks(), 1);
    fake.assert_finished();
}

#[test]
fn ctrl_c_while_the_class_lock_is_held_unlocks_and_exits_130() {
    let include = IncludeKind::Ccau;
    let source = include_uri(include);
    let own_feed = format!("{source}/versions");
    let live = text_of(&include_source("v1"));
    let cancel = CancelFlag::new();
    let mut fake = Fake::new()
        .get(&format!("{source}?version=active"), 200, &live)
        .get(&own_feed, 200, &no_inactive())
        .get(&main_feed(), 200, &no_inactive())
        .post(
            &format!("{CLAS_URI}?_action=LOCK&accessMode=MODIFY"),
            200,
            &lock_body("", true),
        )
        .post(
            &format!("{CLAS_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
            200,
            "",
        )
        .cancelling(&cancel);
    let mut deploy = input(&include_source("v2"), false, None);

    deploy.name = CLAS.to_owned();

    let result = DeployProg::run_scoped(
        &mut fake,
        deploy,
        clas_scope(Some(include)),
        None,
        Some(&connection(ForeignInactive::Reject)),
        &cancel,
    );

    assert_eq!(result.outcome.exit, 130);
    assert_eq!(fake.unlocks(), 1);
    assert_eq!(fake.count("PUT", ""), 0);
    assert_eq!(fake.closes, 1);
}

// ---------------------------------------------------------------- missing include

const STRUCTURE: &str = "/sap/bc/adt/oo/classes/ytest_abapreach_cl/objectstructure?version=active&withShortDescriptions=true";

fn ccau() -> (String, DeployScope) {
    (
        include_uri(IncludeKind::Ccau),
        clas_scope(Some(IncludeKind::Ccau)),
    )
}

fn missing_include_before_lock(source: &str) -> Fake {
    Fake::new()
        .get(&format!("{source}?version=active"), 404, "")
        .get(STRUCTURE, 200, "<structure/>")
}

fn missing_include_locked(fake: Fake, source: &str) -> Fake {
    fake.get(&main_feed(), 200, &no_inactive())
        .get(&main_feed(), 200, &no_inactive())
        .post(
            &format!("{CLAS_URI}?_action=LOCK&accessMode=MODIFY"),
            200,
            &lock_body("", true),
        )
        .get(&format!("{source}?version=active"), 404, "")
        .post(CHECKRUNS, 200, CLEAN)
}

#[test]
fn a_missing_include_without_the_flag_stops_before_the_lock_and_writes_nothing() {
    let (source, scope) = ccau();
    let mut fake = missing_include_before_lock(&source);
    let result = run(
        &mut fake,
        CLAS,
        scope,
        input(&include_source("v2"), false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.status, None);
    assert_eq!(result.outcome.exit, 1);
    assert_eq!(
        result.outcome.stdout,
        "INCLUDE_MISSING: CCAU (re-run with --create-include)"
    );
    assert_eq!(fake.calls.len(), 2, "{:?}", fake.uris());
    fake.assert_finished();
}

#[test]
fn an_include_of_a_class_that_does_not_exist_is_the_plain_404() {
    let (source, scope) = ccau();
    let mut fake = Fake::new()
        .get(&format!("{source}?version=active"), 404, "")
        .get(STRUCTURE, 404, "");
    let result = run(
        &mut fake,
        CLAS,
        scope.create_include(true),
        input(&include_source("v2"), false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.exit, 2);
    assert_eq!(
        result.outcome.stderr,
        "STOP: cannot read live source: HTTP 404"
    );
}

#[test]
fn simulate_names_the_include_it_would_create_and_writes_nothing() {
    let (source, scope) = ccau();
    let fake = missing_include_locked(missing_include_before_lock(&source), &source).post(
        &format!("{CLAS_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
        200,
        "",
    );
    let mut fake = fake;
    let result = run(
        &mut fake,
        CLAS,
        scope.create_include(true),
        input(&include_source("v2"), true, None),
        ForeignInactive::Reject,
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::SimulatedOk),
        "{:?}",
        result.outcome
    );
    assert!(stdout_of(&result).contains(&"WILL CREATE INCLUDE CCAU"));
    assert_eq!(fake.count("POST", "/includes?"), 0);
    assert_eq!(fake.count("PUT", ""), 0);
    assert_eq!(fake.unlocks(), 1);
    fake.assert_finished();
}

#[test]
fn the_create_include_deploy_posts_once_under_the_lock_then_puts_and_activates_the_class_once() {
    let (source, scope) = ccau();
    let file = include_source("v2");
    let tr_free = missing_include_locked(missing_include_before_lock(&source), &source)
        .post(&format!("{CLAS_URI}/includes?{HANDLE_QUERY}"), 201, "")
        .put(&format!("{source}?{HANDLE_QUERY}"), 200, "");
    let mut fake = clas_activation_tail(tr_free, &source, &text_of(&file));
    let result = run(
        &mut fake,
        CLAS,
        scope.create_include(true),
        input(&file, false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::Active),
        "{:?}",
        result.outcome
    );
    assert_eq!(fake.count("POST", "/includes?"), 1);
    assert_eq!(fake.count("POST", "method=activate"), 1);
    assert_eq!(fake.unlocks(), 1);

    let order = [
        fake.position("POST", "_action=LOCK"),
        fake.position("POST", "/includes?"),
        fake.position("PUT", ""),
        fake.position("POST", "_action=UNLOCK"),
        fake.position("POST", "method=activate"),
    ];

    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");

    let post = &fake.calls[fake.position("POST", "/includes?")];
    let body = String::from_utf8_lossy(&post.body);

    assert!(
        body.contains(r#"class:includeType="testclasses""#),
        "{body}"
    );
    assert_eq!(post.headers["Content-Type"], "application/*");
    fake.assert_finished();
}

#[test]
fn the_include_post_of_a_transportable_class_carries_the_corrnr() {
    let (source, scope) = ccau();
    let file = include_source("v2");
    let mut fake = Fake::new()
        .get(&format!("{source}?version=active"), 404, "")
        .get(STRUCTURE, 200, "<structure/>")
        .get(&main_feed(), 200, &no_inactive())
        .get(&main_feed(), 200, &no_inactive())
        .post(
            &format!("{CLAS_URI}?_action=LOCK&accessMode=MODIFY"),
            200,
            &lock_body(TR, false),
        )
        .get(&format!("{source}?version=active"), 404, "")
        .post(CHECKRUNS, 200, CLEAN)
        .post(
            &format!("{CLAS_URI}/includes?{HANDLE_QUERY}&corrNr={TR}"),
            201,
            "",
        )
        .put(&format!("{source}?{HANDLE_QUERY}&corrNr={TR}"), 200, "");
    fake = clas_activation_tail(fake, &source, &text_of(&file));
    let result = run(
        &mut fake,
        CLAS,
        scope.create_include(true),
        input(&file, false, Some(TR)),
        ForeignInactive::Reject,
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::Active),
        "{:?}",
        result.outcome
    );
    fake.assert_finished();
}

#[test]
fn an_unconfirmed_include_post_is_never_repeated_and_is_reconciled_by_the_inactive_source() {
    let file = include_source("v2");
    let other = include_source("other");
    let cases: [(Reply, &str, Option<Status>, bool); 4] = [
        (
            Ok(Response::empty(404)),
            "REJECTED: include CCAU does not exist",
            Some(Status::Rejected),
            false,
        ),
        (
            Ok(Response::text(200, text_of(&file))),
            "PARTIAL: include CCAU exists with the file",
            Some(Status::Partial),
            false,
        ),
        (
            Ok(Response::text(200, text_of(&other))),
            "PARTIAL: include CCAU exists but",
            Some(Status::Partial),
            true,
        ),
        (
            Ok(Response::empty(500)),
            "PARTIAL: the include CCAU create is unconfirmed",
            Some(Status::Partial),
            false,
        ),
    ];

    for post in [
        Err(TransportError::PostSendUncertain("reset".to_owned())),
        Ok(Response::empty(500)),
    ] {
        for (inactive, line, status, differs) in &cases {
            let (source, scope) = ccau();
            let mut fake = missing_include_locked(missing_include_before_lock(&source), &source)
                .on(
                    "POST",
                    &format!("{CLAS_URI}/includes?{HANDLE_QUERY}"),
                    post.clone(),
                )
                .post(
                    &format!("{CLAS_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
                    200,
                    "",
                )
                .on(
                    "GET",
                    &format!("{source}?version=inactive"),
                    inactive.clone(),
                );
            let result = run(
                &mut fake,
                CLAS,
                scope.create_include(true),
                input(&file, false, None),
                ForeignInactive::Reject,
            );

            assert_eq!(result.outcome.status, *status, "{line}");
            assert_eq!(result.outcome.exit, 1);
            assert!(
                result.outcome.stdout.contains(line),
                "{}",
                result.outcome.stdout
            );
            assert_eq!(
                stdout_of(&result).contains(&"INCLUDE_SOURCE_DIFFERS"),
                *differs,
                "{}",
                result.outcome.stdout
            );
            assert_eq!(fake.count("POST", "/includes?"), 1, "never a second POST");
            assert_eq!(fake.count("PUT", ""), 0);
            assert_eq!(fake.unlocks(), 1);
            assert!(
                fake.position("POST", "_action=UNLOCK") < fake.position("GET", "version=inactive"),
                "the lock is released before the reconciliation read"
            );
            fake.assert_finished();
        }
    }
}

// ---------------------------------------------------------------- INTF

#[test]
fn an_interface_deploy_uses_its_source_feed_and_reports_activation_errors() {
    let source = format!("{INTF_URI}/source/main");
    let feed_uri = format!("{source}/versions");
    let live = "INTERFACE ytest_abapreach_if PUBLIC.\nENDINTERFACE.";
    let file = lines("INTERFACE ytest_abapreach_if PUBLIC.\n  METHODS run.\nENDINTERFACE.");

    for (activation, expected) in [("", Status::Active), (MESSAGES_E, Status::Inactive)] {
        let mut fake = Fake::new()
            .get(&format!("{source}?version=active"), 200, live)
            .get(&feed_uri, 200, &no_inactive())
            .get(&feed_uri, 200, &no_inactive())
            .post(
                &format!("{INTF_URI}?_action=LOCK&accessMode=MODIFY"),
                200,
                &lock_body("", true),
            )
            .get(&format!("{source}?version=active"), 200, live)
            .post(CHECKRUNS, 200, CLEAN)
            .put(&format!("{source}?{HANDLE_QUERY}"), 200, "")
            .post(
                &format!("{INTF_URI}?_action=UNLOCK&{HANDLE_QUERY}"),
                200,
                "",
            )
            .post(ACTIVATE, 200, activation);

        if expected == Status::Active {
            fake = fake.get(&format!("{source}?version=active"), 200, &text_of(&file));
        }

        let result = run(
            &mut fake,
            INTF,
            DeployScope::new(ObjectKind::Intf),
            input(&file, false, None),
            ForeignInactive::Reject,
        );

        assert_eq!(
            result.outcome.status,
            Some(expected),
            "{:?}",
            result.outcome
        );
        assert_eq!(fake.unlocks(), 1);
        // An interface has no inactive reread: only a class activates its includes.
        assert_eq!(fake.count("GET", "version=inactive"), 0);
        fake.assert_finished();
    }
}

// ---------------------------------------------------------------- activation parsing

#[test]
fn the_activation_body_is_classified_by_its_root() {
    assert_eq!(classify_activation(""), ActivationBody::Empty);
    assert_eq!(classify_activation(" \n"), ActivationBody::Empty);
    assert_eq!(
        classify_activation(&worklist(&[])),
        ActivationBody::Worklist
    );
    assert_eq!(classify_activation(MESSAGES_W), ActivationBody::Messages);
    assert_eq!(classify_activation("<x/>"), ActivationBody::Other);
    assert_eq!(classify_activation("not xml"), ActivationBody::Other);
}

#[test]
fn only_the_object_references_of_a_worklist_are_listed() {
    let refs = parse_object_refs(&worklist(&[])).unwrap();
    let uris: Vec<&str> = refs.iter().map(|object| object.uri.as_str()).collect();

    assert_eq!(
        uris,
        [
            CLAS_URI,
            "/sap/bc/adt/oo/classes/ytest_abapreach_cl/includes/definitions",
            "/sap/bc/adt/oo/classes/ytest_abapreach_cl/source/main#type=CLAS%2FOM;name=PROBE",
        ]
    );
    assert_eq!(refs[0].parent_uri, None);
    assert_eq!(refs[1].parent_uri.as_deref(), Some(CLAS_URI));
    assert_eq!(refs[2].name, "YTEST_ABAPREACH_CL           PROBE");
}
