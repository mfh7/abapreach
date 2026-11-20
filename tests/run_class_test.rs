//! `run CLAS` through the whole application: the argument rules, the policy gate, the plan file,
//! the productive-client refusal, the pinned requests of the simulate and of the run, the timeout
//! of the run `POST`, and the outcome mapping. A scripted SAP answers; no network.

mod common;

use abapreach::config::Connection;
use abapreach::hash::{local_source_lines, source_hash};
use abapreach::operation::deploy::{CancelFlag, InterruptAction};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, Opened, RfcLibraryInfo, run};
use common::preview;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

const SOURCE: &str = "CLASS zcl_a DEFINITION PUBLIC.\n  PUBLIC SECTION.\n    INTERFACES if_oo_adt_classrun.\nENDCLASS.\n";
const CLASS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><class:abapClass xmlns:class="http://www.sap.com/adt/oo/classes" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:responsible="DEVUSER" adtcore:version="active" adtcore:name="ZCL_A"><adtcore:packageRef adtcore:name="ZPKG"/></class:abapClass>"#;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Sent {
    request: Request,
    /// The per-request limit; `None` is the limit of the transport.
    limit: Option<Duration>,
}

type Log = Rc<RefCell<Vec<(usize, Sent)>>>;

/// One scripted session: its replies in order, a numbered record of every request, and the
/// capability of a real HTTP session to take a per-request limit.
struct Session {
    number: usize,
    http: bool,
    replies: VecDeque<Result<Response, TransportError>>,
    log: Log,
    closed: Rc<RefCell<Vec<usize>>>,
    /// How many Ctrl-C arrive while the classrun POST is in flight, on the flag of the run.
    interrupts: Option<(CancelFlag, usize, Seen)>,
}

type Seen = Rc<RefCell<Vec<InterruptAction>>>;

impl Session {
    fn reply(
        &mut self,
        request: Request,
        limit: Option<Duration>,
    ) -> Result<Response, TransportError> {
        self.log
            .borrow_mut()
            .push((self.number, Sent { request, limit }));

        if let Some((flag, count, seen)) = &self.interrupts
            && self
                .log
                .borrow()
                .last()
                .is_some_and(|(_, sent)| sent.request.uri.contains("classrun"))
        {
            for _ in 0..*count {
                seen.borrow_mut().push(flag.on_interrupt());
            }
        }

        self.replies
            .pop_front()
            .expect("a request without a scripted reply")
    }
}

impl SadtTransport for Session {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.reply(request, None)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closed.borrow_mut().push(self.number);

        Ok(())
    }

    fn supports_request_timeout(&self) -> bool {
        self.http
    }

    fn request_within(
        &mut self,
        request: Request,
        limit: Duration,
    ) -> Result<Response, TransportError> {
        self.reply(request, Some(limit))
    }
}

/// Hands out the scripted sessions in order, per backend.
struct Sap {
    adt: VecDeque<Result<Vec<Result<Response, TransportError>>, TransportError>>,
    rfc: VecDeque<Vec<Result<Response, TransportError>>>,
    log: Log,
    closed: Rc<RefCell<Vec<usize>>>,
    opened: usize,
    adt_opens: usize,
    rfc_opens: usize,
    cancel: CancelFlag,
    /// How many Ctrl-C the classrun POST sees, and what the handler answered to each.
    interrupts_on_post: usize,
    seen: Seen,
}

impl Sap {
    fn new() -> Self {
        Self {
            adt: VecDeque::new(),
            rfc: VecDeque::new(),
            log: Log::default(),
            closed: Rc::default(),
            opened: 0,
            adt_opens: 0,
            rfc_opens: 0,
            cancel: CancelFlag::new(),
            interrupts_on_post: 0,
            seen: Seen::default(),
        }
    }

    fn adt_session(mut self, replies: Vec<Result<Response, TransportError>>) -> Self {
        self.adt.push_back(Ok(replies));

        self
    }

    fn rfc_session(mut self, replies: Vec<Result<Response, TransportError>>) -> Self {
        self.rfc.push_back(replies);

        self
    }

    fn session(&mut self, http: bool, replies: Vec<Result<Response, TransportError>>) -> Opened {
        self.opened += 1;

        Ok(Box::new(Session {
            number: self.opened,
            http,
            replies: replies.into(),
            log: self.log.clone(),
            closed: self.closed.clone(),
            interrupts: (self.interrupts_on_post > 0).then(|| {
                (
                    self.cancel.clone(),
                    self.interrupts_on_post,
                    self.seen.clone(),
                )
            }),
        }))
    }

    fn sent(&self) -> Vec<(usize, Sent)> {
        self.log.borrow().clone()
    }

    fn posts(&self) -> Vec<(usize, Sent)> {
        self.sent()
            .into_iter()
            .filter(|(_, sent)| {
                sent.request.method == "POST" && sent.request.uri.contains("classrun")
            })
            .collect()
    }
}

impl Factories for Sap {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opens += 1;

        match self.adt.pop_front() {
            Some(Ok(replies)) => self.session(true, replies),
            Some(Err(error)) => Err(error),
            None => panic!("an ADT session that was not scripted"),
        }
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opens += 1;

        match self.rfc.pop_front() {
            Some(replies) => self.session(false, replies),
            None => panic!("an RFC session that was not scripted"),
        }
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("a run probes no library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("a run pings nothing")
    }

    fn cancel(&self) -> CancelFlag {
        self.cancel.clone()
    }
}

fn ok(body: &str) -> Result<Response, TransportError> {
    Ok(Response::text(200, body))
}

fn t000(category: &str) -> Result<Response, TransportError> {
    ok(&preview(&[("CCCATEGORY", &[Some(category)])]))
}

fn interface_found() -> Result<Response, TransportError> {
    ok(&preview(&[("REFCLSNAME", &[Some("IF_OO_ADT_CLASSRUN")])]))
}

fn none(column: &str) -> Result<Response, TransportError> {
    let nothing: &[Option<&str>] = &[];

    ok(&preview(&[(column, nothing)]))
}

/// The reads of a class that can be run, on one session: client, metadata, active source,
/// interface.
fn reads() -> Vec<Result<Response, TransportError>> {
    vec![t000("C"), ok(CLASS_XML), ok(SOURCE), interface_found()]
}

fn env(directory: &Path, policies: &str, rfc: bool) -> BTreeMap<String, String> {
    let path = directory.join("connections.json");
    let rfc_block = if rfc {
        r#","rfc":{"ashost":"example.com","sysnr":"00"}"#
    } else {
        ""
    };
    let policies = if policies.is_empty() {
        String::new()
    } else {
        format!(r#","policies":{policies}"#)
    };

    fs::write(
        &path,
        format!(
            r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"clients":["100"]{rfc_block}{policies}}}]}}"#
        ),
    )
    .unwrap();

    BTreeMap::from([
        ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn run_with(sap: &mut Sap, directory: &Path, policies: &str, rfc: bool, args: &[&str]) -> Exit {
    let mut full = vec!["abapreach".to_owned(), "run".to_owned()];

    full.extend(args.iter().map(|argument| (*argument).to_owned()));
    full.extend(["--conn".to_owned(), "DEV_100".to_owned()]);

    run(full, env(directory, policies, rfc), sap)
}

fn plan_file(directory: &Path) -> String {
    directory.join("plan.json").display().to_string()
}

/// Simulates `ZCL_A` and returns the plan file path.
fn simulated(directory: &Path) -> String {
    let plan = plan_file(directory);
    let mut sap = Sap::new().adt_session(reads());
    let exit = run_with(
        &mut sap,
        directory,
        "",
        false,
        &["CLAS", "ZCL_A", "--simulate", "--plan", &plan],
    );

    assert_eq!(exit.code, 0, "{exit:?}");

    plan
}

#[test]
fn a_simulate_reads_the_facts_prints_them_and_writes_the_plan() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());
    let mut sap = Sap::new().adt_session(reads());
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "zcl_a", "--simulate", "--plan", &plan],
    );
    let hash = source_hash(&local_source_lines(SOURCE));

    assert_eq!(exit.code, 0, "{exit:?}");
    assert!(
        exit.stdout
            .starts_with("SIMULATED_OK run CLAS ZCL_A package ZPKG author DEVUSER\n"),
        "{}",
        exit.stdout
    );
    assert!(exit.stdout.contains(&format!("HASH {hash}")));
    assert!(exit.stdout.contains("CLIENT 100 (not productive)"));
    assert!(exit.stdout.contains("BACKEND adt"));
    assert!(exit.stdout.contains("W: run is not a dry run"));
    assert!(exit.stdout.ends_with(&format!("plan -> {plan}")));

    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&plan).unwrap()).unwrap();

    assert_eq!(saved["conn"], "DEV_100");
    assert_eq!(saved["client"], "100");
    assert_eq!(saved["backend"], "adt");
    assert_eq!(saved["kind"], "CLAS");
    assert_eq!(saved["name"], "ZCL_A");
    assert_eq!(saved["facts"]["hash"], hash);
    assert_eq!(saved["facts"]["package"], "ZPKG");

    // One session, no POST: the class never ran.
    assert!(sap.posts().is_empty());
    assert_eq!(sap.opened, 1);
    assert_eq!(sap.closed.borrow().as_slice(), [1]);
}

#[test]
fn the_requests_of_a_simulate_are_pinned() {
    let directory = tempfile::tempdir().unwrap();
    let mut sap = Sap::new().adt_session(reads());

    run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &[
            "CLAS",
            "ZCL_A",
            "--simulate",
            "--plan",
            &plan_file(directory.path()),
        ],
    );

    let sent: Vec<(String, String, Option<String>, Option<Duration>)> = sap
        .sent()
        .into_iter()
        .map(|(_, sent)| {
            (
                sent.request.method,
                sent.request.uri,
                sent.request.headers.get("Accept").cloned(),
                sent.limit,
            )
        })
        .collect();

    assert_eq!(sent.len(), 4);
    assert_eq!(sent[0].0, "POST");
    assert!(
        sent[0]
            .1
            .starts_with("/sap/bc/adt/datapreview/ddic?rowNumber=1&ddicEntityName=T000")
    );
    assert_eq!(
        sent[1],
        (
            "GET".to_owned(),
            "/sap/bc/adt/oo/classes/zcl_a".to_owned(),
            Some("application/*".to_owned()),
            None
        )
    );
    assert_eq!(
        sent[2],
        (
            "GET".to_owned(),
            "/sap/bc/adt/oo/classes/zcl_a/source/main?version=active".to_owned(),
            Some("text/plain".to_owned()),
            None
        )
    );
    assert!(sent[3].1.contains("ddicEntityName=SEOMETAREL"));
    assert_eq!(
        String::from_utf8(sap.sent()[0].1.request.body.clone()).unwrap(),
        "SELECT CCCATEGORY FROM T000 WHERE MANDT = '100'"
    );
}

#[test]
fn a_run_with_its_plan_posts_once_from_a_new_session_with_the_default_timeout() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let mut sap = Sap::new()
        .adt_session(reads())
        .adt_session(vec![ok("Hello from ZCL_A\n")]);
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "zcl_a", "--plan", &plan],
    );

    assert_eq!(exit.code, 0, "{exit:?}");
    assert_eq!(exit.stdout, "Hello from ZCL_A\n");
    assert_eq!(exit.stderr, "");

    let posts = sap.posts();

    assert_eq!(posts.len(), 1);

    let (session, sent) = &posts[0];

    assert_eq!(*session, 2, "the POST has a session of its own");
    assert_eq!(sent.request.method, "POST");
    assert_eq!(sent.request.uri, "/sap/bc/adt/oo/classrun/ZCL_A");
    assert!(sent.request.body.is_empty());
    assert_eq!(
        sent.request.headers,
        BTreeMap::from([("Accept".to_owned(), "text/plain".to_owned())])
    );
    assert_eq!(sent.limit, Some(Duration::from_secs(600)));
    // The read session is over before the run starts, and the POST session is ended after it.
    assert_eq!(sap.closed.borrow().first(), Some(&1));
    assert!(sap.closed.borrow().contains(&2));
}

#[test]
fn the_timeout_option_is_the_limit_of_the_post_alone() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let mut sap = Sap::new().adt_session(reads()).adt_session(vec![ok("x")]);
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--plan", &plan, "--timeout", "30"],
    );

    assert_eq!(exit.code, 0, "{exit:?}");

    let limits: Vec<Option<Duration>> = sap.sent().iter().map(|(_, sent)| sent.limit).collect();

    // Only the POST carries a limit; the reads keep the client's own.
    assert_eq!(limits[..4], [None, None, None, None]);
    assert_eq!(limits[4], Some(Duration::from_secs(30)));

    for bad in ["0", "3601", "x"] {
        let mut sap = Sap::new();
        let exit = run_with(
            &mut sap,
            directory.path(),
            "",
            false,
            &["CLAS", "ZCL_A", "--plan", &plan, "--timeout", bad],
        );

        assert_eq!(exit.code, 2, "{bad}");
        assert_eq!(sap.opened, 0);
    }
}

#[test]
fn a_timeout_is_refused_on_rfc_before_a_connection_opens() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let full = |extra: &[&str]| {
        let mut args = vec!["CLAS", "ZCL_A", "--plan", &plan, "--timeout", "30"];

        args.extend(extra.iter().copied());

        args.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };

    // `--backend rfc` on a connection that has both routes.
    let mut sap = Sap::new();
    let args = full(&["--backend", "rfc"]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let exit = run_with(&mut sap, directory.path(), "", true, &args);

    assert_eq!(exit.code, 2, "{exit:?}");
    assert!(
        exit.stderr
            .starts_with("STOP: --timeout is not supported over RFC"),
        "{}",
        exit.stderr
    );
    assert_eq!(sap.opened, 0);

    // A connection without an ADT endpoint is RFC only.
    let path = directory.path().join("connections.json");

    fs::write(
        &path,
        r#"{"servers":[{"systemId":"DEV","clients":["100"],"rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
    )
    .unwrap();

    let mut env = BTreeMap::from([
        ("ABAP_CONNECTIONS".to_owned(), path.display().to_string()),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ]);
    let mut sap = Sap::new();
    let mut argv = vec!["abapreach".to_owned(), "run".to_owned()];

    argv.extend(full(&[]));
    argv.extend(["--conn".to_owned(), "DEV_100".to_owned()]);

    let exit = run(argv, std::mem::take(&mut env), &mut sap);

    assert_eq!(exit.code, 2, "{exit:?}");
    assert!(
        exit.stderr
            .starts_with("STOP: --timeout is not supported over RFC")
    );
    assert_eq!(sap.opened, 0);
}

#[test]
fn a_run_over_rfc_posts_without_a_limit() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());
    let mut sap = Sap::new().rfc_session(reads());
    let simulate = run_with(
        &mut sap,
        directory.path(),
        "",
        true,
        &[
            "CLAS",
            "ZCL_A",
            "--simulate",
            "--plan",
            &plan,
            "--backend",
            "rfc",
        ],
    );

    assert_eq!(simulate.code, 0, "{simulate:?}");
    assert!(simulate.stdout.contains("BACKEND rfc"));

    let mut sap = Sap::new()
        .rfc_session(reads())
        .rfc_session(vec![ok("rfc out")]);
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        true,
        &["CLAS", "ZCL_A", "--plan", &plan],
    );

    assert_eq!(exit.code, 0, "{exit:?}");
    assert_eq!(exit.stdout, "rfc out");
    // The plan pins the backend: no ADT session was tried.
    assert_eq!(sap.adt_opens, 0);

    let posts = sap.posts();

    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].1.limit, None);
    assert_eq!(posts[0].1.request.uri, "/sap/bc/adt/oo/classrun/ZCL_A");
}

#[test]
fn a_plan_that_does_not_match_the_arguments_stops_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());

    for (args, field) in [
        (vec!["CLAS", "ZCL_B", "--plan", &plan], "name"),
        (vec!["CLAS", "YCL_A", "--plan", &plan], "name"),
    ] {
        let mut sap = Sap::new();
        let exit = run_with(&mut sap, directory.path(), "", false, &args);

        assert_eq!(exit.code, 2, "{field}: {exit:?}");
        assert_eq!(
            exit.stderr,
            format!("STOP: plan does not match the arguments ({field}); simulate again")
        );
        assert_eq!(sap.opened, 0);
    }

    // Another connection id and another client are other arguments.
    let mut saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&plan).unwrap()).unwrap();

    for (key, value, field) in [
        ("conn", "DEV_200", "conn"),
        ("client", "200", "client"),
        ("kind", "PROG", "type"),
    ] {
        saved[key] = value.into();

        let other = directory.path().join(format!("other-{key}.json"));

        fs::write(&other, saved.to_string()).unwrap();

        let mut sap = Sap::new();
        let exit = run_with(
            &mut sap,
            directory.path(),
            "",
            false,
            &["CLAS", "ZCL_A", "--plan", &other.display().to_string()],
        );

        assert_eq!(exit.code, 2, "{key}: {exit:?}");
        assert!(
            exit.stderr.contains(&format!("({field})")),
            "{}",
            exit.stderr
        );
        assert_eq!(sap.opened, 0);

        saved = serde_json::from_str(&fs::read_to_string(&plan).unwrap()).unwrap();
    }

    // The plan pins the backend: another `--backend` contradicts it.
    let mut sap = Sap::new();
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        true,
        &["CLAS", "ZCL_A", "--plan", &plan, "--backend", "rfc"],
    );

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.contains("(backend)"), "{}", exit.stderr);
    assert_eq!(sap.opened, 0);
}

#[test]
fn a_missing_or_foreign_plan_file_is_a_stop() {
    let directory = tempfile::tempdir().unwrap();
    let mut sap = Sap::new();
    let missing = directory.path().join("nothing.json").display().to_string();
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--plan", &missing],
    );

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr.starts_with("STOP: plan file cannot be read"),
        "{}",
        exit.stderr
    );

    let foreign = directory.path().join("foreign.json");

    fs::write(&foreign, r#"{"conn":"DEV_100","objects":[]}"#).unwrap();

    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--plan", &foreign.display().to_string()],
    );

    assert_eq!(exit.code, 2);
    assert!(
        exit.stderr.starts_with("STOP: plan file cannot be read"),
        "{}",
        exit.stderr
    );
    assert_eq!(sap.opened, 0);
}

#[test]
fn facts_that_moved_on_since_the_simulate_refuse_the_run_and_send_no_post() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let changed_source = format!("{SOURCE}* changed\n");
    let other_package = CLASS_XML.replace("ZPKG", "ZOTHER");

    for (class, source, field) in [
        (CLASS_XML.to_owned(), changed_source, "source hash"),
        (other_package, SOURCE.to_owned(), "package"),
    ] {
        let mut sap =
            Sap::new().adt_session(vec![t000("C"), ok(&class), ok(&source), interface_found()]);
        let exit = run_with(
            &mut sap,
            directory.path(),
            "",
            false,
            &["CLAS", "ZCL_A", "--plan", &plan],
        );

        assert_eq!(exit.code, 1, "{field}: {exit:?}");
        assert_eq!(
            exit.stderr,
            format!("REJECTED: plan changed since simulate ({field}); simulate again")
        );
        assert!(sap.posts().is_empty(), "{field}");
    }
}

#[test]
fn a_productive_client_refuses_every_run_before_the_class_is_read() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());

    for args in [
        vec!["CLAS", "ZCL_A", "--simulate", "--plan", &plan],
        vec!["CLAS", "ZCL_A", "--plan", &plan],
    ] {
        let mut sap = Sap::new().adt_session(vec![t000("P")]);
        let exit = run_with(&mut sap, directory.path(), "", false, &args);

        assert_eq!(exit.code, 1, "{exit:?}");
        assert_eq!(
            exit.stderr,
            "REJECTED: client 100 is productive (T000 CCCATEGORY P); nothing was sent"
        );
        // Only the T000 preview was sent: not the class, not a POST.
        assert_eq!(sap.sent().len(), 1);
        assert!(sap.posts().is_empty());
    }

    // An unreadable or empty T000 is not proof of a safe client.
    let nothing = {
        let empty: &[Option<&str>] = &[];

        ok(&preview(&[("CCCATEGORY", empty)]))
    };

    for reply in [Ok(Response::text(500, "")), nothing] {
        let mut sap = Sap::new().adt_session(vec![reply]);
        let exit = run_with(
            &mut sap,
            directory.path(),
            "",
            false,
            &["CLAS", "ZCL_A", "--plan", &plan],
        );

        assert_eq!(exit.code, 1, "{exit:?}");
        assert!(exit.stderr.starts_with("REJECTED: "), "{}", exit.stderr);
        assert!(sap.posts().is_empty());
        assert_eq!(sap.sent().len(), 1);
    }
}

#[test]
fn the_state_of_the_class_decides_the_simulate() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());
    let args = ["CLAS", "ZCL_A", "--simulate", "--plan", &plan];
    let inactive = CLASS_XML.replace("adtcore:version=\"active\"", "adtcore:version=\"inactive\"");
    let cases: Vec<(Vec<Result<Response, TransportError>>, &str)> = vec![
        (
            vec![t000("C"), Ok(Response::text(404, ""))],
            "class ZCL_A does not exist",
        ),
        (
            vec![t000("C"), ok(&inactive)],
            "class ZCL_A is not active; activate it first",
        ),
        (
            vec![t000("C"), ok(CLASS_XML), Ok(Response::text(404, ""))],
            "class ZCL_A has no active version; activate it first",
        ),
        (
            vec![
                t000("C"),
                ok(CLASS_XML),
                ok(SOURCE),
                none("REFCLSNAME"),
                none("REFCLSNAME"),
            ],
            "class ZCL_A does not implement IF_OO_ADT_CLASSRUN",
        ),
    ];

    for (replies, reason) in cases {
        let mut sap = Sap::new().adt_session(replies);
        let exit = run_with(&mut sap, directory.path(), "", false, &args);

        assert_eq!(exit.code, 1, "{reason}: {exit:?}");
        assert_eq!(exit.stderr, format!("REJECTED: {reason}"));
        assert!(!Path::new(&plan).exists(), "no plan for a refused simulate");
    }
}

#[test]
fn a_class_that_inherits_the_interface_is_accepted() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());
    let mut sap = Sap::new().adt_session(vec![
        t000("C"),
        ok(CLASS_XML),
        ok(SOURCE),
        none("REFCLSNAME"),
        ok(&preview(&[("REFCLSNAME", &[Some("zcl_base")])])),
        interface_found(),
    ]);
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--simulate", "--plan", &plan],
    );

    assert_eq!(exit.code, 0, "{exit:?}");

    let bodies: Vec<String> = sap
        .sent()
        .iter()
        .map(|(_, sent)| String::from_utf8_lossy(&sent.request.body).into_owned())
        .collect();

    assert!(
        bodies[5].contains("CLSNAME IN ( 'ZCL_BASE' ) AND REFCLSNAME IN"),
        "{}",
        bodies[5]
    );
}

#[test]
fn the_name_rule_admits_z_and_y_and_a_standard_name_only_on_request() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());
    let mut sap = Sap::new();
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "cl_demo_output", "--simulate", "--plan", &plan],
    );

    assert_eq!(exit.code, 2);
    assert_eq!(
        exit.stderr,
        "STOP: class CL_DEMO_OUTPUT is outside the Z/Y scope; --allow-standard allows a standard class"
    );
    assert_eq!(sap.opened, 0);

    let mut sap = Sap::new().adt_session(vec![
        t000("C"),
        ok(&CLASS_XML.replace("ZCL_A", "CL_DEMO_OUTPUT")),
        ok(SOURCE),
        interface_found(),
    ]);
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &[
            "CLAS",
            "cl_demo_output",
            "--simulate",
            "--plan",
            &plan,
            "--allow-standard",
        ],
    );

    assert_eq!(exit.code, 0, "{exit:?}");
}

#[test]
fn the_run_policy_off_plan_and_direct() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());

    // off: refused, simulate included.
    for args in [
        vec!["CLAS", "ZCL_A", "--plan", &plan],
        vec!["CLAS", "ZCL_A", "--simulate", "--plan", &plan],
    ] {
        let mut sap = Sap::new();
        let exit = run_with(&mut sap, directory.path(), r#"{"run":"off"}"#, false, &args);

        assert_eq!(exit.code, 2);
        assert_eq!(exit.stderr, "STOP: run is off for DEV_100 (policy run)");
        assert_eq!(sap.opened, 0);
    }

    // plan (the default): a run needs its file, and a simulate needs somewhere to write it.
    for (args, text) in [
        (vec!["CLAS", "ZCL_A"], "run needs --plan <FILE>"),
        (
            vec!["CLAS", "ZCL_A", "--simulate"],
            "--simulate needs --plan <FILE>",
        ),
    ] {
        let mut sap = Sap::new();
        let exit = run_with(&mut sap, directory.path(), "", false, &args);

        assert_eq!(exit.code, 2);
        assert!(exit.stderr.contains(text), "{}", exit.stderr);
        assert_eq!(sap.opened, 0);
    }

    // direct: the CLI runs without a plan, and a simulate only prints.
    let mut sap = Sap::new()
        .adt_session(reads())
        .adt_session(vec![ok("direct")]);
    let exit = run_with(
        &mut sap,
        directory.path(),
        r#"{"run":"direct"}"#,
        false,
        &["CLAS", "ZCL_A"],
    );

    assert_eq!(exit.code, 0, "{exit:?}");
    assert_eq!(exit.stdout, "direct");

    let mut sap = Sap::new().adt_session(reads());
    let exit = run_with(
        &mut sap,
        directory.path(),
        r#"{"run":"direct"}"#,
        false,
        &["CLAS", "ZCL_A", "--simulate"],
    );

    assert_eq!(exit.code, 0, "{exit:?}");
    assert!(!exit.stdout.contains("plan ->"));
}

#[test]
fn options_of_other_run_types_are_refused_on_a_class() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());

    for option in [
        vec!["--param", "A=1"],
        vec!["--variant", "V"],
        vec!["--alv", "off"],
        vec!["--alv-columns", "all"],
        vec!["--max-output", "10"],
    ] {
        let mut sap = Sap::new();
        let mut args = vec!["CLAS", "ZCL_A", "--simulate", "--plan", &plan];

        args.extend(option.iter().copied());

        let exit = run_with(&mut sap, directory.path(), "", false, &args);

        assert_eq!(exit.code, 2, "{option:?}: {exit:?}");
        assert_eq!(sap.opened, 0);
    }
}

#[test]
fn a_report_and_a_snippet_stop_as_not_implemented_after_their_gates() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());
    let snippet = directory.path().join("s.abap");

    fs::write(&snippet, "WRITE 1.").unwrap();

    let snippet = snippet.display().to_string();
    let mut sap = Sap::new();
    let report = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &[
            "PROG",
            "zreport",
            "--param",
            "P_A=1",
            "--simulate",
            "--plan",
            &plan,
        ],
    );
    let file = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["SNIPPET", &snippet, "--simulate", "--plan", &plan],
    );

    for exit in [report, file] {
        assert_eq!(exit.code, 2);
        assert!(
            exit.stderr.ends_with("is not implemented yet"),
            "{}",
            exit.stderr
        );
    }

    // The rules still come first: a standard report, a bad parameter, an off policy.
    for (args, policies, text) in [
        (
            vec!["PROG", "rsparam", "--simulate", "--plan", &plan],
            "",
            "outside the Z/Y scope",
        ),
        (
            vec![
                "PROG",
                "zreport",
                "--param",
                "X",
                "--simulate",
                "--plan",
                &plan,
            ],
            "",
            "NAME=VALUE",
        ),
        (
            vec!["PROG", "zreport", "--simulate", "--plan", &plan],
            r#"{"run":"off"}"#,
            "policy run",
        ),
    ] {
        let exit = run_with(&mut sap, directory.path(), policies, false, &args);

        assert_eq!(exit.code, 2);
        assert!(exit.stderr.contains(text), "{}", exit.stderr);
    }

    assert_eq!(sap.opened, 0);
}

#[test]
fn the_outcome_of_the_post_maps_to_the_documented_tokens_and_exit_codes() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let cases: Vec<(Result<Response, TransportError>, i32, &str, &str)> = vec![
        (ok("done"), 0, "done", ""),
        (
            Ok(Response::text(404, "")),
            2,
            "",
            "STOP: run CLAS needs ADT classrun; not available on DEV",
        ),
        (
            Ok(Response::text(403, "")),
            1,
            "",
            "REJECTED: the classrun POST of ZCL_A was refused: HTTP 403; the class did not run",
        ),
        (
            Ok(Response::text(
                500,
                "<html>ABAP runtime error ... ST22</html>",
            )),
            1,
            "",
            "RUN_FAILED: runtime error; see ST22 (dumps)",
        ),
        (
            Ok(Response::text(504, "gateway timeout")),
            1,
            "",
            "RUN_UNKNOWN: the reply was lost; the code may have run\nW: the classrun reply was HTTP 504",
        ),
        (
            Err(TransportError::PostSendUncertain(
                "HTTP timeout after request send".to_owned(),
            )),
            1,
            "",
            "RUN_UNKNOWN: the reply was lost; the code may have run\nW: post-send outcome uncertain: HTTP timeout after request send",
        ),
    ];

    for (reply, code, stdout, stderr) in cases {
        let mut sap = Sap::new().adt_session(reads()).adt_session(vec![reply]);
        let exit = run_with(
            &mut sap,
            directory.path(),
            "",
            false,
            &["CLAS", "ZCL_A", "--plan", &plan],
        );

        assert_eq!(exit.code, code, "{stderr}: {exit:?}");
        assert_eq!(exit.stdout, stdout, "{stderr}");
        assert_eq!(exit.stderr, stderr);
        // Never a second POST, whatever happened to the first.
        assert_eq!(sap.posts().len(), 1, "{stderr}");
    }
}

#[test]
fn an_unreachable_adt_before_anything_is_sent_falls_back_to_rfc_and_the_plan_names_it() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());
    let mut sap = Sap::new();

    sap.adt.push_back(Err(TransportError::PreSendConnectivity(
        "unreachable".to_owned(),
    )));

    let mut sap = sap.rfc_session(reads());
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        true,
        &["CLAS", "ZCL_A", "--simulate", "--plan", &plan],
    );

    assert_eq!(exit.code, 0, "{exit:?}");
    assert!(exit.stdout.contains("BACKEND rfc"), "{}", exit.stdout);
    assert!(
        exit.stderr.contains("falling back to RFC SADT"),
        "{}",
        exit.stderr
    );

    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&plan).unwrap()).unwrap();

    assert_eq!(saved["backend"], "rfc");
}

#[test]
fn a_ctrl_c_during_the_post_waits_for_the_reply_and_ends_with_exit_130() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let mut sap = Sap::new()
        .adt_session(reads())
        .adt_session(vec![ok("finished\n")]);

    sap.interrupts_on_post = 1;

    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--plan", &plan],
    );

    // The first Ctrl-C only asks to wait: the reply is printed, the exit says it was interrupted.
    assert_eq!(
        sap.seen.borrow().as_slice(),
        [InterruptAction::Waiting(
            "I: run in progress on the server; waiting for the reply (Ctrl-C again to leave)"
        )]
    );
    assert_eq!(exit.stdout, "finished\n");
    assert_eq!(exit.code, 130);
    assert_eq!(sap.posts().len(), 1);
}

#[test]
fn a_second_ctrl_c_during_the_post_leaves_the_process() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let mut sap = Sap::new().adt_session(reads()).adt_session(vec![ok("x")]);

    sap.interrupts_on_post = 2;

    run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--plan", &plan],
    );

    assert_eq!(sap.seen.borrow().last(), Some(&InterruptAction::ExitNow));
}

#[test]
fn an_interrupt_before_the_post_sends_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let mut sap = Sap::new().adt_session(Vec::new());

    sap.cancel.set();

    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--plan", &plan],
    );

    assert_eq!(exit.code, 130);
    assert!(sap.sent().is_empty());
}

/// The reply of the classrun POST is dropped after SAP ran the class: the outcome is unknown and
/// the request is never sent again (built only with the fault-injection feature).
#[cfg(feature = "fault-injection")]
#[test]
fn a_dropped_reply_of_the_run_is_unknown_and_never_resent() {
    use abapreach::fault::{self, Fault};

    let directory = tempfile::tempdir().unwrap();
    let plan = simulated(directory.path());
    let mut sap = Sap::new()
        .adt_session(reads())
        .adt_session(vec![ok("ran, reply lost")]);
    let _scope = fault::scoped(Fault::from_lookup(Some("drop-reply:CLASSRUN")).unwrap());
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--plan", &plan],
    );

    assert_eq!(exit.code, 1, "{exit:?}");
    assert_eq!(exit.stdout, "");
    assert!(
        exit.stderr
            .starts_with("RUN_UNKNOWN: the reply was lost; the code may have run"),
        "{}",
        exit.stderr
    );
    assert_eq!(sap.posts().len(), 1);
}

#[test]
fn a_temporary_runner_class_is_never_run_and_nothing_is_opened() {
    let directory = tempfile::tempdir().unwrap();
    let plan = plan_file(directory.path());

    for name in ["YCL_ABAPREACH_RUN_AB12CD34", "ycl_abapreach_run_ab12cd34"] {
        for args in [
            vec!["CLAS", name, "--simulate", "--plan", &plan],
            vec!["CLAS", name, "--plan", &plan],
        ] {
            // No session is scripted: the refusal comes before any connection.
            let mut sap = Sap::new();
            let exit = run_with(&mut sap, directory.path(), "", false, &args);

            assert_eq!(exit.code, 2, "{exit:?}");
            assert!(
                exit.stderr.starts_with("STOP: ")
                    && exit
                        .stderr
                        .contains("a temporary runner class is not run; delete it (doctor)"),
                "{}",
                exit.stderr
            );
            assert_eq!(sap.opened, 0);
            assert!(sap.posts().is_empty());
        }
    }
}

#[test]
fn a_failed_plan_save_reports_no_passed_simulate() {
    let directory = tempfile::tempdir().unwrap();
    let plan = directory
        .path()
        .join("missing")
        .join("plan.json")
        .display()
        .to_string();
    let mut sap = Sap::new().adt_session(reads());
    let exit = run_with(
        &mut sap,
        directory.path(),
        "",
        false,
        &["CLAS", "ZCL_A", "--simulate", "--plan", &plan],
    );

    assert_eq!(exit.code, 1, "{exit:?}");
    assert!(exit.stderr.starts_with("ERROR: "), "{}", exit.stderr);
    assert!(!exit.stdout.contains("SIMULATED_OK"), "{}", exit.stdout);
    assert!(exit.stdout.is_empty(), "{}", exit.stdout);
    assert!(sap.posts().is_empty());
}
