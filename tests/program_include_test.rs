//! M5 Task 5: a `PROG` name resolves to a program or an include, an include is read, simulated
//! and deployed under its main program, and every later request uses the URI that answered. The
//! transport is a scripted ADT: a reply per method and exact URI, taken in order, and an
//! unscripted request is a failed test. The fake also models the enqueue table (LOCK adds, UNLOCK
//! removes), so every flow can assert that no lock is left behind.

use abapreach::cli::ObjectKind;
use abapreach::config::Connection;
use abapreach::domain::Status;
use abapreach::hash::source_hash;
use abapreach::operation::deploy::{
    CancelFlag, DeployInput, DeployProg, DeployResult, DeployScope, encode_lock_handle,
};
use abapreach::operation::include::{
    Located, ProgKind, ProgTarget, ResolveError, choose_main, includes_statement, locate,
    parse_main_programs, resolve_main,
};
use abapreach::operation::read::{ReadProg, ReadTarget};
use abapreach::policy::{ForeignInactive, Policies};
use abapreach::transport::rfc::{FakeRfcApi, RfcTransport};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use std::collections::VecDeque;

const USER: &str = "DEVUSER";
const OTHER: &str = "OTHERUSER";
const TR: &str = "DEVK900001";
const PROG: &str = "/sap/bc/adt/programs/programs/zprog_a";
const INCL: &str = "/sap/bc/adt/programs/includes/zincl_a";
/// The include name asked as a program, and the program name asked as an include.
const INCL_AS_PROG: &str = "/sap/bc/adt/programs/programs/zincl_a";
const PROG_AS_INCL: &str = "/sap/bc/adt/programs/includes/zprog_a";
const CONTEXT: &str = "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_a";
const CHECKRUNS: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const ACTIVATE: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";
const HANDLE_QUERY: &str = "lockHandle=h%2B1%2F%3D";

type Reply = Result<Response, TransportError>;
type Rule = (String, String, VecDeque<Reply>);

struct Fake {
    rules: Vec<Rule>,
    calls: Vec<Request>,
    closes: usize,
    /// The lock table: LOCK with a 2xx reply adds one, UNLOCK with a 2xx reply removes one.
    locks: i32,
}

impl Fake {
    fn new() -> Self {
        Self {
            rules: Vec::new(),
            calls: Vec::new(),
            closes: 0,
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

    fn put(self, uri: &str, status: u16, body: &str) -> Self {
        self.on("PUT", uri, Ok(Response::text(status, body)))
    }

    fn count(&self, method: &str, needle: &str) -> usize {
        self.calls
            .iter()
            .filter(|call| call.method == method && call.uri.contains(needle))
            .count()
    }

    fn uris(&self) -> Vec<String> {
        self.calls
            .iter()
            .map(|call| format!("{} {}", call.method, call.uri))
            .collect()
    }

    fn body_of(&self, method: &str, needle: &str) -> String {
        let call = self
            .calls
            .iter()
            .find(|call| call.method == method && call.uri.contains(needle))
            .unwrap_or_else(|| panic!("no {method} {needle} in {:?}", self.uris()));

        String::from_utf8_lossy(&call.body).into_owned()
    }

    fn assert_finished(&self) {
        for (method, uri, replies) in &self.rules {
            assert!(replies.is_empty(), "unused reply for {method} {uri}");
        }
    }

    /// The end state of every flow: nothing is locked, and the session ended once.
    fn assert_clean(&self) {
        assert_eq!(self.locks, 0, "a lock is left: {:?}", self.uris());
        assert_eq!(self.closes, 1, "the session must end once");
        self.assert_finished();
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

fn main_programs(names: &[&str]) -> String {
    let references: String = names
        .iter()
        .map(|name| {
            format!(
                r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/programs/programs/{}" adtcore:type="PROG/P" adtcore:name="{name}"/>"#,
                name.to_ascii_lowercase()
            )
        })
        .collect();

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">{references}</adtcore:objectReferences>"#
    )
}

fn not_found(name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><exc:exception xmlns:exc="http://www.sap.com/abapxml/types/communicationframework"><type id="ExceptionResourceNotFound"/><message lang="EN">Resource PROGRAM {name} does not exist.</message></exc:exception>"#
    )
}

fn report(status: &str, text: &str) -> String {
    format!(
        r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkReport chkrun:reporter="abapCheckRun" chkrun:triggeringUri="{INCL}" chkrun:status="{status}" chkrun:statusText="{text}"/></chkrun:checkRunReports>"#
    )
}

fn processed() -> String {
    report("processed", "Object ZPROG_A has been checked")
}

fn not_processed(text: &str) -> String {
    report("notProcessed", text)
}

const CHECK_ERROR: &str = r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkReport chkrun:status="processed"><chkrun:checkMessageList><chkrun:checkMessage chkrun:uri="/x#start=1,8" chkrun:type="E" chkrun:shortText="Field NO_SUCH_VAR is unknown."/></chkrun:checkMessageList></chkrun:checkReport></chkrun:checkRunReports>"#;
const ACTIVATION_ERROR: &str = r#"<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist"><msg objDescr="Include ZINCL_A" type="E" line="1" href="/x#start=1,8"><shortText><txt>Field "NO_SUCH_VAR" is unknown.</txt></shortText></msg></chkl:messages>"#;
const INVALID_MAIN: &str = r#"<exc:exception xmlns:exc="http://www.sap.com/abapxml/types/communicationframework"><type id="invalidMainProgram"/><message lang="EN">Select a master program for include ZINCL_A in the properties view</message></exc:exception>"#;

fn worklist(uris: &[&str]) -> String {
    let entries: String = uris
        .iter()
        .map(|uri| {
            format!(
                r#"<ioc:entry><ioc:object ioc:user="" ioc:deleted="false"><ioc:ref adtcore:uri="{uri}" adtcore:type="PROG/P" adtcore:name="X" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry>"#
            )
        })
        .collect();

    format!(
        r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects">{entries}</ioc:inactiveObjects>"#
    )
}

fn live_text() -> &'static str {
    "WRITE / 'v1'."
}

fn file_text() -> &'static str {
    "WRITE / 'v2'."
}

fn source(uri: &str) -> String {
    format!("{uri}/source/main")
}

fn active(uri: &str) -> String {
    format!("{}?version=active", source(uri))
}

fn versions(uri: &str) -> String {
    format!("{}/versions", source(uri))
}

fn lock(uri: &str) -> String {
    format!("{uri}?_action=LOCK&accessMode=MODIFY")
}

fn unlock(uri: &str) -> String {
    format!("{uri}?_action=UNLOCK&{HANDLE_QUERY}")
}

fn deploy_input(text: &str, simulate: bool, tr: Option<&str>) -> DeployInput {
    DeployInput {
        name: "ZINCL_A".to_owned(),
        source: lines(text),
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
        ..Policies::default()
    };

    if let Some(adt) = connection.adt.as_mut() {
        adt.insecure = false;
    }

    connection
}

fn run_with(
    fake: &mut Fake,
    name: &str,
    main: Option<&str>,
    mut input: DeployInput,
    policy: ForeignInactive,
) -> DeployResult {
    input.name = name.to_owned();

    DeployProg::run_scoped(
        fake,
        input,
        DeployScope::new(ObjectKind::Prog).main(main),
        None,
        Some(&connection(policy)),
        &CancelFlag::new(),
    )
}

fn run(fake: &mut Fake, main: Option<&str>, input: DeployInput) -> DeployResult {
    run_with(fake, "ZINCL_A", main, input, ForeignInactive::Reject)
}

fn stderr_lines(result: &DeployResult) -> Vec<&str> {
    result.outcome.stderr.lines().collect()
}

// ---- Scripts: the recorded request order of an include deploy ------------------------------

/// The name is not a program: the program source answers 404, then the properties of the program
/// (404) and of the include (200); the include source is the live source.
fn is_include(live: &str) -> Fake {
    Fake::new()
        .get(&active(INCL_AS_PROG), 404, &not_found("ZINCL_A"))
        .get(INCL_AS_PROG, 404, &not_found("ZINCL_A"))
        .get(INCL, 200, "<include:abapInclude/>")
        .get(&active(INCL), 200, live)
}

/// `GET <include>/mainprograms` and, for a `--main`, the checks of that program.
fn with_main(fake: Fake, listed: &[&str], main: Option<(&str, &str)>) -> Fake {
    let fake = fake.get(&format!("{INCL}/mainprograms"), 200, &main_programs(listed));

    match main {
        Some((name, text)) => {
            let uri = format!(
                "/sap/bc/adt/programs/programs/{}",
                name.to_ascii_lowercase()
            );

            fake.get(&uri, 200, "<program:abapProgram/>")
                .get(&active(&uri), 200, text)
        }
        None => fake,
    }
}

/// The guard before the lock, the lock, the locked re-read and the second guard.
fn through_lock(fake: Fake, corrnr: &str, local: bool, live: &str) -> Fake {
    fake.get(&versions(INCL), 200, &no_inactive())
        .post(&lock(INCL), 200, &lock_body(corrnr, local))
        .get(&active(INCL), 200, live)
        .get(&versions(INCL), 200, &no_inactive())
}

/// A deploy up to and including the accepted PUT and the unlock.
fn through_put(fake: Fake, check: &str) -> Fake {
    through_put_over(fake, check, live_text())
}

/// Like `through_put` for an include whose active source is `live` (empty for a new include).
fn through_put_over(fake: Fake, check: &str, live: &str) -> Fake {
    through_lock(fake, "", true, live)
        .post(CHECKRUNS, 200, check)
        .put(&format!("{}?{HANDLE_QUERY}", source(INCL)), 200, "")
        .post(&unlock(INCL), 200, "")
}

fn activated(fake: Fake, answer: &str) -> Fake {
    fake.post(ACTIVATE, 200, answer)
}

// ---- Resolution ----------------------------------------------------------------------------

#[test]
fn the_properties_of_the_program_are_asked_first_and_the_first_200_wins() {
    let mut fake = Fake::new().get(PROG, 200, "<program:abapProgram/>");

    let Ok(Located::Found(target)) = locate(&mut fake, "ZPROG_A") else {
        panic!("a program must resolve");
    };

    assert_eq!(target, ProgTarget::program("zprog_a"));
    assert_eq!(target.kind, ProgKind::Program);
    assert_eq!(target.uri, PROG);
    // The include URI is never asked once the program answered.
    assert_eq!(fake.uris(), [format!("GET {PROG}")]);
    assert_eq!(
        fake.calls[0].headers.get("Accept").map(String::as_str),
        Some("application/*")
    );
}

#[test]
fn an_include_is_found_after_the_program_answers_404() {
    let mut fake = Fake::new()
        .get(INCL_AS_PROG, 404, &not_found("ZINCL_A"))
        .get(INCL, 200, "<include:abapInclude/>");

    let Ok(Located::Found(target)) = locate(&mut fake, "zincl_a") else {
        panic!("an include must resolve");
    };

    assert_eq!(target.kind, ProgKind::Include);
    assert_eq!(target.uri, INCL);
    assert_eq!(target.source_uri(), source(INCL));
    assert_eq!(target.versions_uri(), versions(INCL));
    assert_eq!(
        fake.uris(),
        [format!("GET {INCL_AS_PROG}"), format!("GET {INCL}")]
    );
}

#[test]
fn a_namespaced_name_resolves_with_the_encoded_slash() {
    let mut fake = Fake::new().get(
        "/sap/bc/adt/programs/programs/%2fns%2fprog",
        200,
        "<program:abapProgram/>",
    );

    let Ok(Located::Found(target)) = locate(&mut fake, "/NS/PROG") else {
        panic!("a namespaced program must resolve");
    };

    assert_eq!(target.uri, "/sap/bc/adt/programs/programs/%2fns%2fprog");
}

#[test]
fn both_uris_answering_404_is_not_found() {
    let mut fake = Fake::new()
        .get(INCL_AS_PROG, 404, &not_found("ZINCL_A"))
        .get(INCL, 404, &not_found("ZINCL_A"));

    assert!(matches!(
        locate(&mut fake, "ZINCL_A"),
        Ok(Located::NotFound)
    ));
}

#[test]
fn a_status_without_a_rule_is_an_error_not_a_guess() {
    for status in [403, 500] {
        let mut fake = Fake::new().get(INCL_AS_PROG, status, "");

        match locate(&mut fake, "ZINCL_A") {
            Err(ResolveError::Status { what, status: got }) => {
                assert_eq!(got, status);
                assert_eq!(what, "cannot resolve PROG ZINCL_A");
            }
            other => panic!("{other:?}"),
        }

        // The include is not asked either: the kind of the object stays unknown.
        assert_eq!(fake.calls.len(), 1);
    }

    let mut fake = Fake::new().get(INCL_AS_PROG, 404, "").get(INCL, 500, "");

    assert!(matches!(
        locate(&mut fake, "ZINCL_A"),
        Err(ResolveError::Status { status: 500, .. })
    ));
}

#[test]
fn a_refused_logon_and_a_transport_failure_are_transport_errors() {
    let mut fake = Fake::new().get(INCL_AS_PROG, 401, "");

    assert!(matches!(
        locate(&mut fake, "ZINCL_A"),
        Err(ResolveError::Transport(TransportError::Authorization(_)))
    ));

    let mut fake = Fake::new().on(
        "GET",
        INCL_AS_PROG,
        Err(TransportError::PreSendConnectivity("no route".to_owned())),
    );

    assert!(matches!(
        locate(&mut fake, "ZINCL_A"),
        Err(ResolveError::Transport(
            TransportError::PreSendConnectivity(_)
        ))
    ));
}

// ---- Main program choice -------------------------------------------------------------------

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn exactly_one_listed_main_program_is_used() {
    let main = choose_main("zincl_a", &names(&["ZPROG_A"]), None).unwrap();

    assert_eq!(
        (main.name.as_str(), main.first_activation),
        ("ZPROG_A", false)
    );
}

#[test]
fn several_listed_main_programs_stop_and_ask_for_main() {
    let error = choose_main("zincl_a", &names(&["ZPROG_A", "ZPROG_B"]), None).unwrap_err();

    assert_eq!(
        error,
        "include ZINCL_A has several main programs (ZPROG_A, ZPROG_B); pass --main <PROG>"
    );
}

#[test]
fn no_listed_main_program_stops_with_the_contract_text() {
    let error = choose_main("zincl_a", &[], None).unwrap_err();

    assert_eq!(
        error,
        "include ZINCL_A has no main program; pass --main <PROG>"
    );
}

#[test]
fn a_given_main_must_be_listed_when_anything_is_listed() {
    let listed = names(&["ZPROG_A", "ZPROG_B"]);
    let error = choose_main("ZINCL_A", &listed, Some("zprog_c")).unwrap_err();

    assert_eq!(
        error,
        "main program ZPROG_C is not a main program of include ZINCL_A (listed: ZPROG_A, ZPROG_B)"
    );

    let main = choose_main("ZINCL_A", &listed, Some("zprog_b")).unwrap();

    assert_eq!(
        (main.name.as_str(), main.first_activation),
        ("ZPROG_B", false)
    );
}

#[test]
fn a_given_main_with_nothing_listed_is_the_first_activation() {
    let main = choose_main("ZINCL_A", &[], Some("zprog_a")).unwrap();

    assert_eq!(
        (main.name.as_str(), main.first_activation),
        ("ZPROG_A", true)
    );
}

#[test]
fn the_main_program_answer_lists_names_once_in_upper_case() {
    assert_eq!(
        parse_main_programs(&main_programs(&[])).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        parse_main_programs(&main_programs(&["ZPROG_A"])).unwrap(),
        ["ZPROG_A"]
    );
    assert_eq!(
        parse_main_programs(&main_programs(&["ZPROG_A", "ZPROG_B", "ZPROG_A"])).unwrap(),
        ["ZPROG_A", "ZPROG_B"]
    );
}

#[test]
fn a_main_program_reference_that_is_not_a_program_is_refused() {
    let function_group = r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="/sap/bc/adt/functions/groups/yfg_utils" adtcore:type="FUGR/F" adtcore:name="YFG_UTILS"/></adtcore:objectReferences>"#;

    assert!(parse_main_programs(function_group).is_err());
    assert!(parse_main_programs("<broken").is_err());
}

// ---- The INCLUDE statement -----------------------------------------------------------------

fn holds(source: &[&str], include: &str) -> bool {
    includes_statement(&names(source), include)
}

#[test]
fn the_include_statement_is_found_case_insensitively() {
    assert!(holds(&["REPORT zprog_a.", "INCLUDE zincl_a."], "ZINCL_A"));
    assert!(holds(&["include ZINCL_A."], "zincl_a"));
    assert!(holds(&["  INCLUDE   zincl_a   ."], "ZINCL_A"));
    assert!(holds(&["INCLUDE zincl_a", "  ."], "ZINCL_A"));
    assert!(holds(&["WRITE 1. INCLUDE zincl_a. WRITE 2."], "ZINCL_A"));
    assert!(holds(&["INCLUDE zincl_a. \" the include"], "ZINCL_A"));
    assert!(holds(&["INCLUDE /ns/zincl_a."], "/NS/ZINCL_A"));
}

#[test]
fn a_commented_out_include_statement_does_not_count() {
    assert!(!holds(&["* INCLUDE zincl_a."], "ZINCL_A"));
    assert!(!holds(&["*INCLUDE zincl_a."], "ZINCL_A"));
    assert!(!holds(&["\" INCLUDE zincl_a."], "ZINCL_A"));
    assert!(!holds(&["WRITE 1. \" INCLUDE zincl_a."], "ZINCL_A"));
    // Only column 1 makes a line comment.
    assert!(holds(&["* a comment", " INCLUDE zincl_a."], "ZINCL_A"));
}

#[test]
fn text_that_only_mentions_the_include_does_not_count() {
    assert!(!holds(&["WRITE 'INCLUDE zincl_a.'."], "ZINCL_A"));
    assert!(!holds(&["WRITE `x. INCLUDE zincl_a.`."], "ZINCL_A"));
    assert!(!holds(&["WRITE |x. INCLUDE zincl_a.|."], "ZINCL_A"));
    assert!(!holds(&["INCLUDE zincl_ab."], "ZINCL_A"));
    assert!(!holds(&["INCLUDE zincl_a"], "ZINCL_A"));
    assert!(!holds(&["INCLUDE TYPE zincl_a."], "ZINCL_A"));
    assert!(!holds(&["DATA zincl_a."], "ZINCL_A"));
    assert!(!holds(&[], "ZINCL_A"));
}

#[test]
fn a_closed_literal_ends_where_its_delimiter_says_and_only_a_template_escapes() {
    // A closed literal or template is over: the statement after it still counts.
    assert!(holds(&["WRITE |a|. INCLUDE zincl_a."], "ZINCL_A"));
    assert!(holds(&["WRITE 'a'. INCLUDE zincl_a."], "ZINCL_A"));
    assert!(holds(&["WRITE `a`. INCLUDE zincl_a."], "ZINCL_A"));
    // In a template `\|` is an escaped bar: the text goes on, so the INCLUDE inside it is text.
    assert!(!holds(&["WRITE |a\\| x. INCLUDE zincl_a.|."], "ZINCL_A"));
    // A backslash escapes nothing in a plain or backquote literal.
    assert!(holds(&["WRITE 'a\\'. INCLUDE zincl_a."], "ZINCL_A"));
    assert!(holds(&["WRITE `a\\`. INCLUDE zincl_a."], "ZINCL_A"));
}

// ---- Main program resolution against the server --------------------------------------------

#[test]
fn a_given_main_is_resolved_as_a_program_and_its_source_is_checked() {
    let mut fake = Fake::new()
        .get(&format!("{INCL}/mainprograms"), 200, &main_programs(&[]))
        .get(PROG, 200, "<program:abapProgram/>")
        .get(&active(PROG), 200, "REPORT zprog_a.\nINCLUDE zincl_a.\n");

    let main = resolve_main(&mut fake, "ZINCL_A", Some("zprog_a")).unwrap();

    assert_eq!(
        (main.name.as_str(), main.first_activation),
        ("ZPROG_A", true)
    );
    // Resolved through programs/programs: the include URI of the main is never asked.
    assert_eq!(
        fake.uris(),
        [
            format!("GET {INCL}/mainprograms"),
            format!("GET {PROG}"),
            format!("GET {}", active(PROG)),
        ]
    );
    fake.assert_finished();
}

#[test]
fn a_given_main_that_is_not_a_program_or_lacks_the_statement_stops() {
    let mut fake = Fake::new()
        .get(&format!("{INCL}/mainprograms"), 200, &main_programs(&[]))
        .get(PROG, 404, &not_found("ZPROG_A"));

    match resolve_main(&mut fake, "ZINCL_A", Some("zprog_a")) {
        Err(ResolveError::Stop(message)) => {
            assert_eq!(message, "main program ZPROG_A does not exist as a program");
        }
        other => panic!("{other:?}"),
    }

    let mut fake = Fake::new()
        .get(&format!("{INCL}/mainprograms"), 200, &main_programs(&[]))
        .get(PROG, 200, "<program:abapProgram/>")
        .get(&active(PROG), 200, "REPORT zprog_a.\n* INCLUDE zincl_a.\n");

    match resolve_main(&mut fake, "ZINCL_A", Some("zprog_a")) {
        Err(ResolveError::Stop(message)) => assert_eq!(
            message,
            "main program ZPROG_A does not contain the statement INCLUDE ZINCL_A."
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_listed_main_needs_no_extra_request() {
    let mut fake = Fake::new().get(
        &format!("{INCL}/mainprograms"),
        200,
        &main_programs(&["ZPROG_A"]),
    );
    let main = resolve_main(&mut fake, "ZINCL_A", None).unwrap();

    assert_eq!(main.name, "ZPROG_A");
    assert_eq!(fake.calls.len(), 1);
    assert_eq!(
        fake.calls[0].headers.get("Accept").map(String::as_str),
        Some("application/*")
    );
}

// ---- Read ----------------------------------------------------------------------------------

fn read(fake: &mut Fake, name: &str, main: Option<&str>) -> Result<String, String> {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out.abap");
    let target = ReadTarget::new(ObjectKind::Prog, name).main(main);

    ReadProg::run_target_classified(fake, target, &out, false, None)
        .map(|outcome| outcome.stdout)
        .map_err(|error| {
            format!(
                "{}{}",
                if error.stop { "STOP: " } else { "ERROR: " },
                error.message
            )
        })
}

#[test]
fn reading_a_program_keeps_its_single_request() {
    let mut fake = Fake::new().get(&active(PROG), 200, live_text());
    let stdout = read(&mut fake, "ZPROG_A", None).unwrap();

    assert!(stdout.contains("1 lines"), "{stdout}");
    assert_eq!(fake.uris(), [format!("GET {}", active(PROG))]);
}

#[test]
fn reading_an_include_uses_its_own_uri_and_needs_no_main() {
    let mut fake = is_include(live_text());
    let stdout = read(&mut fake, "ZINCL_A", None).unwrap();

    assert!(
        stdout.contains(&source_hash(&lines(live_text()))),
        "{stdout}"
    );
    assert_eq!(
        fake.uris(),
        [
            format!("GET {}", active(INCL_AS_PROG)),
            format!("GET {INCL_AS_PROG}"),
            format!("GET {INCL}"),
            format!("GET {}", active(INCL)),
        ]
    );
    fake.assert_finished();
}

#[test]
fn reading_an_include_with_main_checks_the_main() {
    let fake = with_main(
        is_include(live_text()),
        &["ZPROG_A"],
        Some(("ZPROG_A", "INCLUDE zincl_a.")),
    );
    let mut fake = fake;

    assert!(read(&mut fake, "ZINCL_A", Some("ZPROG_A")).is_ok());
    fake.assert_finished();

    let mut fake = with_main(is_include(live_text()), &["ZPROG_B"], None);
    let error = read(&mut fake, "ZINCL_A", Some("ZPROG_A")).unwrap_err();

    assert!(error.starts_with("STOP: main program ZPROG_A is not a main program"));
}

#[test]
fn reading_a_program_with_main_is_a_stop() {
    let mut fake = Fake::new().get(&active(PROG), 200, live_text());
    let error = read(&mut fake, "ZPROG_A", Some("ZOTHER")).unwrap_err();

    assert_eq!(
        error,
        "STOP: --main is only valid for an include; PROG ZPROG_A is a program"
    );
}

#[test]
fn reading_a_name_neither_uri_knows_is_not_found() {
    let mut fake = Fake::new()
        .get(&active(PROG), 404, "")
        .get(PROG, 404, "")
        .get(PROG_AS_INCL, 404, "");
    let error = read(&mut fake, "ZPROG_A", None).unwrap_err();

    assert_eq!(
        error,
        "ERROR: read PROG ZPROG_A: NOT_FOUND (no program or include of that name)"
    );
}

// ---- Deploy: the include flow --------------------------------------------------------------

#[test]
fn a_deploy_of_an_include_runs_every_request_on_its_own_uri_under_the_context() {
    let fake = with_main(is_include(live_text()), &["ZPROG_A"], None);
    let mut fake =
        activated(through_put(fake, &processed()), "").get(&active(INCL), 200, file_text());
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));
    let hash = source_hash(&lines(file_text()));

    assert_eq!(result.outcome.status, Some(Status::Active), "{result:?}");
    assert_eq!(result.outcome.exit, 0);
    assert!(
        result
            .outcome
            .stdout
            .ends_with(&format!("ACTIVE\nhash {hash}")),
        "{}",
        result.outcome.stdout
    );
    // The main program is printed, once.
    assert_eq!(
        result
            .outcome
            .stdout
            .lines()
            .filter(|line| *line == "main ZPROG_A")
            .count(),
        1
    );
    assert_eq!(result.outcome.stderr, "");
    assert_eq!(
        fake.uris(),
        [
            format!("GET {}", active(INCL_AS_PROG)),
            format!("GET {INCL_AS_PROG}"),
            format!("GET {INCL}"),
            format!("GET {}", active(INCL)),
            format!("GET {INCL}/mainprograms"),
            format!("GET {}", versions(INCL)),
            format!("POST {}", lock(INCL)),
            format!("GET {}", active(INCL)),
            format!("GET {}", versions(INCL)),
            format!("POST {CHECKRUNS}"),
            format!("PUT {}?{HANDLE_QUERY}", source(INCL)),
            format!("POST {}", unlock(INCL)),
            format!("POST {ACTIVATE}"),
            format!("GET {}", active(INCL)),
        ]
    );
    // The check and the activation name the include under the context, and nothing else does.
    let context = format!("{INCL}?context={CONTEXT}");
    let check = fake.body_of("POST", "checkruns");
    let activation = fake.body_of("POST", "activation");

    assert!(
        check.contains(&format!(r#"adtcore:uri="{context}""#)),
        "{check}"
    );
    assert!(
        check.contains(&format!(r#"chkrun:uri="{}""#, source(INCL))),
        "{check}"
    );
    assert!(
        activation.contains(&format!(r#"adtcore:uri="{context}""#)),
        "{activation}"
    );
    assert_eq!(
        fake.calls
            .iter()
            .filter(|call| call.uri.contains("context="))
            .count(),
        0,
        "the context travels only in a body"
    );
    fake.assert_clean();
}

#[test]
fn every_request_of_the_include_deploy_is_admitted_by_the_sadt_boundary() {
    let fake = with_main(is_include(live_text()), &["ZPROG_A"], None);
    let mut fake =
        activated(through_put(fake, &processed()), "").get(&active(INCL), 200, file_text());

    run(&mut fake, None, deploy_input(file_text(), false, None));

    for request in &fake.calls {
        let mut transport = RfcTransport::new(FakeRfcApi::ok());

        if let Err(TransportError::RequestNotAllowed(reason)) = transport.request(request.clone()) {
            panic!("not admitted: {request:?}: {reason}");
        }
    }
}

#[test]
fn a_deploy_with_a_listed_main_given_checks_the_main_source_before_the_lock() {
    let fake = with_main(
        is_include(live_text()),
        &["ZPROG_A"],
        Some(("ZPROG_A", "REPORT zprog_a.\nINCLUDE zincl_a.\n")),
    );
    let mut fake =
        activated(through_put(fake, &processed()), "").get(&active(INCL), 200, file_text());
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), false, None),
    );
    let uris = fake.uris();
    let lock_at = uris
        .iter()
        .position(|uri| uri.contains("_action=LOCK"))
        .unwrap();
    let main_at = uris
        .iter()
        .position(|uri| *uri == format!("GET {}", active(PROG)))
        .unwrap();

    assert_eq!(result.outcome.status, Some(Status::Active), "{result:?}");
    assert!(main_at < lock_at, "{uris:?}");
    fake.assert_clean();
}

fn assert_stop_before_the_lock(fake: &Fake, result: &DeployResult, expected: &str) {
    assert_eq!(result.outcome.exit, 2, "{result:?}");
    assert_eq!(result.outcome.status, None);
    assert_eq!(result.outcome.stderr, format!("STOP: {expected}"));
    assert_eq!(fake.count("POST", "_action=LOCK"), 0, "{:?}", fake.uris());
    assert_eq!(fake.count("PUT", ""), 0);
    assert_eq!(fake.count("POST", "activation"), 0);
    assert!(!result.fallback_allowed);
    assert_eq!(fake.locks, 0);
    assert_eq!(fake.closes, 1);
    fake.assert_finished();
}

#[test]
fn several_main_programs_stop_before_the_lock_and_list_the_candidates() {
    let mut fake = with_main(is_include(live_text()), &["ZPROG_A", "ZPROG_B"], None);
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_stop_before_the_lock(
        &fake,
        &result,
        "include ZINCL_A has several main programs (ZPROG_A, ZPROG_B); pass --main <PROG>",
    );
}

#[test]
fn no_main_program_stops_before_the_lock() {
    let mut fake = with_main(is_include(live_text()), &[], None);
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_stop_before_the_lock(
        &fake,
        &result,
        "include ZINCL_A has no main program; pass --main <PROG>",
    );
}

#[test]
fn a_main_that_is_not_listed_stops_before_the_lock() {
    let mut fake = with_main(is_include(live_text()), &["ZPROG_B"], None);
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), false, None),
    );

    assert_stop_before_the_lock(
        &fake,
        &result,
        "main program ZPROG_A is not a main program of include ZINCL_A (listed: ZPROG_B)",
    );
}

#[test]
fn a_main_whose_source_lacks_the_include_statement_stops_before_the_lock() {
    for text in [
        "REPORT zprog_a.\nWRITE 1.\n",
        "REPORT zprog_a.\n* INCLUDE zincl_a.\n",
        "REPORT zprog_a.\n\" INCLUDE zincl_a.\n",
    ] {
        let mut fake = with_main(is_include(live_text()), &[], Some(("ZPROG_A", text)));
        let result = run(
            &mut fake,
            Some("zprog_a"),
            deploy_input(file_text(), false, None),
        );

        assert_stop_before_the_lock(
            &fake,
            &result,
            "main program ZPROG_A does not contain the statement INCLUDE ZINCL_A.",
        );
    }
}

#[test]
fn a_main_that_does_not_exist_stops_before_the_lock() {
    let mut fake = is_include(live_text())
        .get(&format!("{INCL}/mainprograms"), 200, &main_programs(&[]))
        .get(PROG, 404, &not_found("ZPROG_A"));
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), false, None),
    );

    assert_stop_before_the_lock(
        &fake,
        &result,
        "main program ZPROG_A does not exist as a program",
    );
}

#[test]
fn main_on_a_program_stops_before_the_lock() {
    let mut fake = Fake::new().get(&active(PROG), 200, live_text());
    let result = run_with(
        &mut fake,
        "ZPROG_A",
        Some("ZOTHER"),
        deploy_input(file_text(), false, None),
        ForeignInactive::Reject,
    );

    assert_stop_before_the_lock(
        &fake,
        &result,
        "--main is only valid for an include; PROG ZPROG_A is a program",
    );
}

#[test]
fn a_name_neither_uri_knows_is_not_found_and_nothing_is_locked() {
    let mut fake = Fake::new()
        .get(&active(INCL_AS_PROG), 404, "")
        .get(INCL_AS_PROG, 404, "")
        .get(INCL, 404, "");
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.exit, 1);
    assert_eq!(result.outcome.status, None);
    assert_eq!(
        result.outcome.stdout,
        "NOT_FOUND: PROG ZINCL_A (no program or include of that name)"
    );
    assert!(!result.fallback_allowed);
    fake.assert_clean();
}

#[test]
fn a_program_without_source_keeps_the_plain_404_stop() {
    let mut fake = Fake::new()
        .get(&active(PROG), 404, "")
        .get(PROG, 200, "<program:abapProgram/>");
    let result = run_with(
        &mut fake,
        "ZPROG_A",
        None,
        deploy_input(file_text(), false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.exit, 2);
    assert_eq!(
        result.outcome.stderr,
        "STOP: cannot read live source: HTTP 404"
    );
    fake.assert_clean();
}

#[test]
fn an_unknown_status_while_resolving_is_a_stop_with_the_status() {
    let mut fake = Fake::new()
        .get(&active(INCL_AS_PROG), 404, "")
        .get(INCL_AS_PROG, 500, "");
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.exit, 2);
    assert_eq!(
        result.outcome.stderr,
        "STOP: cannot resolve PROG ZINCL_A: HTTP 500"
    );
    fake.assert_clean();
}

#[test]
fn a_pre_send_failure_while_resolving_may_fall_back() {
    let mut fake = Fake::new().get(&active(INCL_AS_PROG), 404, "").on(
        "GET",
        INCL_AS_PROG,
        Err(TransportError::PreSendConnectivity("no route".to_owned())),
    );
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.exit, 1);
    assert!(result.fallback_allowed);
    fake.assert_clean();
}

// ---- Deploy: no changes, base, guards ------------------------------------------------------

#[test]
fn no_changes_locks_nothing() {
    let mut fake = with_main(is_include(live_text()), &["ZPROG_A"], None);
    let result = run(&mut fake, None, deploy_input(live_text(), false, None));

    assert_eq!(result.outcome.exit, 0);
    assert!(result.outcome.stdout.ends_with("no changes"));
    assert_eq!(fake.count("POST", "_action=LOCK"), 0);
    fake.assert_clean();
}

#[test]
fn a_base_that_does_not_match_the_include_stops_before_the_lock() {
    let mut fake = with_main(is_include(live_text()), &["ZPROG_A"], None);
    let mut input = deploy_input(file_text(), false, None);

    input.base = Some("0".repeat(64));

    let result = run(&mut fake, None, input);
    let live = source_hash(&lines(live_text()));

    assert_eq!(result.outcome.exit, 2);
    assert!(
        result
            .outcome
            .stderr
            .starts_with(&format!("STOP: live changed: live hash {live} != --base")),
        "{}",
        result.outcome.stderr
    );
    assert_eq!(fake.count("POST", "_action=LOCK"), 0);
    // The base the run read is the include's, for the caller that wants to repeat it.
    assert_eq!(result.base.as_deref(), Some(live.as_str()));
    fake.assert_clean();
}

#[test]
fn the_foreign_inactive_guard_reads_the_include_version_feed() {
    let foreign = feed(&[(OTHER, "99999"), (USER, "00000")]);
    let mut fake =
        with_main(is_include(live_text()), &["ZPROG_A"], None).get(&versions(INCL), 200, &foreign);
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.status, Some(Status::Rejected), "{result:?}");
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .contains("REJECTED: inactive version of another user exists: OTHERUSER"),
        "{}",
        result.outcome.stdout
    );
    assert_eq!(fake.count("POST", "_action=LOCK"), 0);
    fake.assert_clean();
}

#[test]
fn the_allow_policy_overwrites_a_foreign_inactive_include_with_a_warning() {
    let foreign = feed(&[(OTHER, "99999"), (USER, "00000")]);
    let fake = with_main(is_include(live_text()), &["ZPROG_A"], None)
        .get(&versions(INCL), 200, &foreign)
        .post(&lock(INCL), 200, &lock_body("", true))
        .get(&active(INCL), 200, live_text())
        .get(&versions(INCL), 200, &foreign)
        .post(CHECKRUNS, 200, &processed())
        .put(&format!("{}?{HANDLE_QUERY}", source(INCL)), 200, "")
        .post(&unlock(INCL), 200, "");
    let mut fake = activated(fake, "").get(&active(INCL), 200, file_text());
    let result = run_with(
        &mut fake,
        "ZINCL_A",
        None,
        deploy_input(file_text(), false, None),
        ForeignInactive::Allow,
    );

    assert_eq!(result.outcome.status, Some(Status::Active), "{result:?}");
    assert!(
        stderr_lines(&result)
            .iter()
            .any(|line| line.starts_with("W: overwriting the inactive version of OTHERUSER")),
        "{}",
        result.outcome.stderr
    );
    fake.assert_clean();
}

// ---- Simulate and the check ----------------------------------------------------------------

#[test]
fn a_simulation_checks_under_the_context_and_leaves_no_residue() {
    let fake = through_lock(
        with_main(is_include(live_text()), &["ZPROG_A"], None),
        "",
        true,
        live_text(),
    )
    .post(CHECKRUNS, 200, &processed())
    .post(&unlock(INCL), 200, "");
    let mut fake = fake;
    let result = run(&mut fake, None, deploy_input(file_text(), true, None));

    assert_eq!(
        result.outcome.status,
        Some(Status::SimulatedOk),
        "{result:?}"
    );
    assert_eq!(result.outcome.exit, 0);
    assert!(result.outcome.stdout.ends_with("SIMULATED_OK"));
    assert_eq!(result.outcome.stderr, "");
    assert_eq!(fake.count("PUT", ""), 0);
    assert_eq!(fake.count("POST", "activation"), 0);
    assert!(
        fake.body_of("POST", "checkruns")
            .contains(&format!("{INCL}?context={CONTEXT}")),
    );
    fake.assert_clean();
}

#[test]
fn a_check_that_found_errors_is_rejected_and_unlocks() {
    let fake = through_lock(
        with_main(is_include(live_text()), &["ZPROG_A"], None),
        "",
        true,
        live_text(),
    )
    .post(CHECKRUNS, 200, CHECK_ERROR)
    .post(&unlock(INCL), 200, "");
    let mut fake = fake;
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .contains("REJECTED: the check found errors"),
        "{}",
        result.outcome.stdout
    );
    assert_eq!(fake.count("PUT", ""), 0);
    fake.assert_clean();
}

#[test]
fn a_check_that_was_not_processed_is_rejected_with_the_status_text() {
    for (listed, main) in [
        (vec!["ZPROG_A"], None),
        (vec!["ZPROG_A"], Some(("ZPROG_A", "INCLUDE zincl_a."))),
    ] {
        let given = main.map(|(name, _)| name);
        let fake = through_lock(
            with_main(is_include(live_text()), &listed, main),
            "",
            true,
            live_text(),
        )
        .post(
            CHECKRUNS,
            200,
            &not_processed("Master program ZPROG_A is invalid for include ZINCL_A"),
        )
        .post(&unlock(INCL), 200, "");
        let mut fake = fake;
        let result = run(&mut fake, given, deploy_input(file_text(), true, None));

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{result:?}");
        assert_eq!(result.outcome.exit, 1);
        assert!(
            result.outcome.stdout.contains(
                "REJECTED: the check was not processed: Master program ZPROG_A is invalid for include ZINCL_A"
            ),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.count("PUT", ""), 0);
        fake.assert_clean();
    }
}

const NEW_INCLUDE_WARNING: &str =
    "W: no syntax check before the first activation of a new include (main ZPROG_A)";

fn new_include(live: &str) -> Fake {
    with_main(
        is_include(live),
        &[],
        Some(("ZPROG_A", "REPORT zprog_a.\nINCLUDE zincl_a.\n")),
    )
}

#[test]
fn a_first_simulation_of_a_new_include_is_simulated_ok_with_the_warning() {
    let fake = through_lock(new_include(""), "", true, "")
        .post(
            CHECKRUNS,
            200,
            &not_processed("Select a master program for include ZINCL_A in the properties view"),
        )
        .post(&unlock(INCL), 200, "");
    let mut fake = fake;
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), true, None),
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::SimulatedOk),
        "{result:?}"
    );
    assert_eq!(result.outcome.exit, 0);
    assert!(result.outcome.stdout.ends_with("SIMULATED_OK"));
    assert_eq!(stderr_lines(&result), [NEW_INCLUDE_WARNING]);
    assert_eq!(fake.count("PUT", ""), 0);
    assert_eq!(fake.count("POST", "activation"), 0);
    fake.assert_clean();
}

#[test]
fn the_new_include_exception_is_decided_by_the_index_not_by_the_status_text() {
    // The same status text as the recorded one, but the include is indexed: REJECTED.
    let fake = through_lock(
        with_main(
            is_include(live_text()),
            &["ZPROG_A"],
            Some(("ZPROG_A", "INCLUDE zincl_a.")),
        ),
        "",
        true,
        live_text(),
    )
    .post(
        CHECKRUNS,
        200,
        &not_processed("Select a master program for include ZINCL_A in the properties view"),
    )
    .post(&unlock(INCL), 200, "");
    let mut fake = fake;
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), true, None),
    );

    assert_eq!(result.outcome.status, Some(Status::Rejected), "{result:?}");
    assert!(!result.outcome.stderr.contains("W: no syntax check"));
    fake.assert_clean();

    // And an empty index with an unrecorded status text is still the exception.
    let fake = through_lock(new_include(""), "", true, "")
        .post(CHECKRUNS, 200, &not_processed("some other text"))
        .post(&unlock(INCL), 200, "");
    let mut fake = fake;
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), true, None),
    );

    assert_eq!(
        result.outcome.status,
        Some(Status::SimulatedOk),
        "{result:?}"
    );
    assert_eq!(stderr_lines(&result), [NEW_INCLUDE_WARNING]);
    fake.assert_clean();
}

#[test]
fn the_first_deploy_of_a_new_include_activates_under_the_context_as_the_check() {
    let fake = through_put_over(
        new_include(""),
        &not_processed("Select a master program for include ZINCL_A in the properties view"),
        "",
    );
    // The PUT of the new include is the first write; the locked re-read is empty as well.
    let mut fake = activated(fake, "").get(&active(INCL), 200, file_text());
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), false, None),
    );

    assert_eq!(result.outcome.status, Some(Status::Active), "{result:?}");
    assert_eq!(result.outcome.exit, 0);
    assert_eq!(stderr_lines(&result), [NEW_INCLUDE_WARNING]);
    assert_eq!(fake.count("PUT", ""), 1);
    assert!(
        fake.body_of("POST", "activation")
            .contains(&format!("{INCL}?context={CONTEXT}"))
    );
    fake.assert_clean();
}

#[test]
fn a_syntax_error_in_a_new_include_leaves_it_inactive() {
    let fake = through_put_over(
        new_include(""),
        &not_processed("Select a master program for include ZINCL_A in the properties view"),
        "",
    );
    let mut fake = activated(fake, ACTIVATION_ERROR);
    let result = run(
        &mut fake,
        Some("zprog_a"),
        deploy_input(file_text(), false, None),
    );

    assert_eq!(result.outcome.status, Some(Status::Inactive), "{result:?}");
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .contains("INACTIVE: activation reported errors"),
        "{}",
        result.outcome.stdout
    );
    assert!(
        result.outcome.stdout.contains("E row 1: "),
        "{}",
        result.outcome.stdout
    );
    // The active source is not read back as success, and the activation is sent once.
    assert_eq!(fake.count("POST", "activation"), 1);
    assert_eq!(fake.count("GET", "version=active"), 4);
    fake.assert_clean();
}

// ---- Activation ----------------------------------------------------------------------------

#[test]
fn an_invalid_main_program_answer_leaves_the_include_inactive_without_a_resend() {
    let fake = through_put(
        with_main(is_include(live_text()), &["ZPROG_A"], None),
        &processed(),
    );
    let mut fake = fake
        .on("POST", ACTIVATE, Ok(Response::text(500, INVALID_MAIN)))
        .get(&active(INCL), 200, live_text());
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.status, Some(Status::Inactive), "{result:?}");
    assert_eq!(result.outcome.exit, 1);
    assert!(
        result
            .outcome
            .stdout
            .contains("INACTIVE: activation failed with HTTP 500 and the active source differs"),
        "{}",
        result.outcome.stdout
    );
    assert_eq!(fake.count("POST", "activation"), 1);
    assert!(
        fake.body_of("POST", "activation").contains("?context="),
        "an activation never goes without the context"
    );
    fake.assert_clean();
}

#[test]
fn a_worklist_with_an_entry_outside_the_include_is_inactive_and_never_resent() {
    for outside in [
        PROG,
        "/sap/bc/adt/programs/includes/zother",
        // The include's own path with a prefix of its name is another object.
        "/sap/bc/adt/programs/includes/zincl_ab",
    ] {
        let fake = through_put(
            with_main(is_include(live_text()), &["ZPROG_A"], None),
            &processed(),
        );
        let mut fake = activated(fake, &worklist(&[INCL, outside]));
        let result = run(&mut fake, None, deploy_input(file_text(), false, None));

        assert_eq!(result.outcome.status, Some(Status::Inactive), "{result:?}");
        assert_eq!(result.outcome.exit, 1);
        assert!(
            result.outcome.stdout.contains(&format!(
                "INACTIVE: the activation worklist lists an object outside the include, nothing was resent: {outside}"
            )),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.count("POST", "activation"), 1, "no resend");
        fake.assert_clean();
    }
}

#[test]
fn a_worklist_inside_the_include_is_not_resent_either() {
    let fake = through_put(
        with_main(is_include(live_text()), &["ZPROG_A"], None),
        &processed(),
    );
    let mut fake = activated(
        fake,
        &worklist(&[INCL, &format!("{INCL}?context={CONTEXT}")]),
    );
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.status, Some(Status::Inactive), "{result:?}");
    assert!(
        result
            .outcome
            .stdout
            .contains("INACTIVE: the activation answered a worklist and activated nothing"),
        "{}",
        result.outcome.stdout
    );
    assert_eq!(fake.count("POST", "activation"), 1);
    fake.assert_clean();
}

#[test]
fn an_activation_that_reports_no_error_but_leaves_the_source_old_is_inactive() {
    let fake = through_put(
        with_main(is_include(live_text()), &["ZPROG_A"], None),
        &processed(),
    );
    let mut fake = activated(fake, "").get(&active(INCL), 200, live_text());
    let result = run(&mut fake, None, deploy_input(file_text(), false, None));

    assert_eq!(result.outcome.status, Some(Status::Inactive), "{result:?}");
    assert_eq!(result.outcome.exit, 1);
    fake.assert_clean();
}

// ---- Transportable include -----------------------------------------------------------------

#[test]
fn a_transportable_include_sends_corr_nr_on_the_put_and_prints_the_request() {
    let fake = with_main(is_include(live_text()), &["ZPROG_A"], None);
    let fake = through_lock(fake, TR, false, live_text())
        .post(CHECKRUNS, 200, &processed())
        .put(
            &format!("{}?{HANDLE_QUERY}&corrNr={TR}", source(INCL)),
            200,
            "",
        )
        .post(&unlock(INCL), 200, "");
    let mut fake = activated(fake, "").get(&active(INCL), 200, file_text());
    let result = run(&mut fake, None, deploy_input(file_text(), false, Some(TR)));

    assert_eq!(result.outcome.status, Some(Status::Active), "{result:?}");
    assert!(
        result
            .outcome
            .stdout
            .contains(&format!("transport request {TR}")),
        "{}",
        result.outcome.stdout
    );
    // Every write of the include flow carries the request: the PUT does; the lock, unlock and
    // activation are not object writes with a corrNr parameter.
    let put = fake.calls.iter().find(|call| call.method == "PUT").unwrap();

    assert!(put.uri.ends_with(&format!("&corrNr={TR}")), "{}", put.uri);
    assert!(
        put.uri
            .contains(&format!("lockHandle={}", encode_lock_handle("h+1/="))),
        "{}",
        put.uri
    );
    fake.assert_clean();
}

#[test]
fn a_transportable_include_without_a_matching_tr_stops_after_the_lock_and_unlocks() {
    let fake = with_main(is_include(live_text()), &["ZPROG_A"], None);
    let fake = fake
        .get(&versions(INCL), 200, &no_inactive())
        .post(&lock(INCL), 200, &lock_body(TR, false))
        .post(&unlock(INCL), 200, "");
    let mut fake = fake;
    let result = run(
        &mut fake,
        None,
        deploy_input(file_text(), false, Some("DEVK900002")),
    );

    assert_eq!(result.outcome.exit, 2);
    assert!(
        result
            .outcome
            .stderr
            .contains("--tr DEVK900002 does not match the transport request DEVK900001"),
        "{}",
        result.outcome.stderr
    );
    assert_eq!(fake.count("PUT", ""), 0);
    fake.assert_clean();
}

// ---- Programs keep the M1 flow -------------------------------------------------------------

#[test]
fn a_program_deploy_resolves_nothing_and_activates_without_a_context() {
    let program_source = source(PROG);
    let mut fake = Fake::new()
        .get(&active(PROG), 200, live_text())
        .get(&versions(PROG), 200, &no_inactive())
        .post(&lock(PROG), 200, &lock_body("", true))
        .get(&active(PROG), 200, live_text())
        .get(&versions(PROG), 200, &no_inactive())
        .post(CHECKRUNS, 200, &processed())
        .put(&format!("{program_source}?{HANDLE_QUERY}"), 200, "")
        .post(&unlock(PROG), 200, "")
        .post(ACTIVATE, 200, "")
        .get(&active(PROG), 200, file_text());
    let result = run_with(
        &mut fake,
        "ZPROG_A",
        None,
        deploy_input(file_text(), false, None),
        ForeignInactive::Reject,
    );

    assert_eq!(result.outcome.status, Some(Status::Active), "{result:?}");
    assert!(!result.outcome.stdout.contains("main "));
    assert_eq!(fake.count("GET", "/mainprograms"), 0);
    assert!(!fake.body_of("POST", "activation").contains("context"));
    assert!(!fake.body_of("POST", "checkruns").contains("context"));
    fake.assert_clean();
}

#[test]
fn every_recorded_not_processed_text_is_rejected_for_an_indexed_include() {
    // 750 and 740 word the same condition differently; none of them is a pass.
    for text in [
        "No main program for include ZINCL_A found",
        "Master program ZPROG_A is invalid for include ZINCL_A",
        "Select a master program for include ZINCL_A in the properties view",
    ] {
        let fake = through_lock(
            with_main(is_include(live_text()), &["ZPROG_A"], None),
            "",
            true,
            live_text(),
        )
        .post(CHECKRUNS, 200, &not_processed(text))
        .post(&unlock(INCL), 200, "");
        let mut fake = fake;
        let result = run(&mut fake, None, deploy_input(file_text(), false, None));

        assert_eq!(result.outcome.status, Some(Status::Rejected), "{text}");
        assert!(
            result.outcome.stdout.contains(text),
            "{}",
            result.outcome.stdout
        );
        assert_eq!(fake.count("PUT", ""), 0);
        fake.assert_clean();
    }
}

#[test]
fn a_given_main_picks_one_of_several_listed_and_names_the_context() {
    let other = "/sap/bc/adt/programs/programs/zprog_b";
    let fake = with_main(
        is_include(live_text()),
        &["ZPROG_A", "ZPROG_B"],
        Some(("ZPROG_B", "INCLUDE zincl_a.")),
    );
    let mut fake =
        activated(through_put(fake, &processed()), "").get(&active(INCL), 200, file_text());
    let result = run(
        &mut fake,
        Some("zprog_b"),
        deploy_input(file_text(), false, None),
    );

    assert_eq!(result.outcome.status, Some(Status::Active), "{result:?}");
    assert!(result.outcome.stdout.contains("main ZPROG_B"));
    assert!(
        fake.body_of("POST", "checkruns").contains(&format!(
            "{INCL}?context=%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_b"
        )),
        "the context names {other}"
    );
    fake.assert_clean();
}
