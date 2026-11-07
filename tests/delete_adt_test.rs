//! The ADT side of M4 delete against a scripted ADT: the read-only facts, the one lock-and-DELETE
//! sequence, the fresh-session verification and the classification of every uncertain answer.
//! Every request is matched by method and exact URI; an unscripted request is a failed test.

use abapreach::cli::DeleteKind;
use abapreach::operation::delete::{
    DeleteBackend, DeleteReply, InactiveFact, ObjectFacts, Reconcile, TrLock,
};
use abapreach::operation::delete_adt::AdtDelete;
use abapreach::transport::rfc::{FakeRfcApi, RfcTransport};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

const USER: &str = "TESTUSER";
const HANDLE: &str = "a/b+c=";
const ENCODED: &str = "a%2Fb%2Bc%3D";
const TR: &str = "DEVK900001";
const PROG: &str = "/sap/bc/adt/programs/programs/zprog";
const GROUP: &str = "/sap/bc/adt/functions/groups/zfg";
const FM: &str = "/sap/bc/adt/functions/groups/zfg/fmodules/zfm";
const CLAS: &str = "/sap/bc/adt/oo/classes/zcl_a";
const INTF: &str = "/sap/bc/adt/oo/interfaces/zif_a";
const MEMBERS: &str = "/sap/bc/adt/repository/nodestructure?parent_type=FUGR%2FF&parent_name=ZFG&withShortDescriptions=false";
const LOCK: &str = "?_action=LOCK&accessMode=MODIFY";
const SEARCH: &str = "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch&query=ZFM&maxResults=10&objectType=FUGR%2FFF";

type Reply = Result<Response, TransportError>;
type Calls = Rc<RefCell<Vec<Request>>>;

/// A scripted ADT: a reply queue per method and exact URI.
#[derive(Default)]
struct Fake {
    rules: Vec<(String, String, VecDeque<Reply>)>,
    calls: Calls,
    closes: Rc<RefCell<usize>>,
    stateful: Option<bool>,
    /// Every request after a 2xx DELETE answers 500, as a session that SAP dumped.
    dies_after_delete: bool,
    dead: bool,
    close_fails: bool,
}

impl Fake {
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

    fn delete(self, uri: &str, reply: Reply) -> Self {
        self.on("DELETE", uri, reply)
    }

    fn stateful(mut self, supported: bool) -> Self {
        self.stateful = Some(supported);

        self
    }

    fn dies_after_delete(mut self) -> Self {
        self.dies_after_delete = true;

        self
    }

    fn close_fails(mut self) -> Self {
        self.close_fails = true;

        self
    }

    fn lock(self, uri: &str, local: bool, corrnr: &str) -> Self {
        self.lock_by(uri, local, corrnr, "")
    }

    fn lock_by(self, uri: &str, local: bool, corrnr: &str, corruser: &str) -> Self {
        self.post(
            &format!("{uri}{LOCK}"),
            200,
            &lock_xml(local, corrnr, corruser),
        )
    }
}

impl SadtTransport for Fake {
    fn request(&mut self, request: Request) -> Reply {
        self.calls.borrow_mut().push(request.clone());

        if self.dead {
            return Ok(Response::text(500, "<exc>session dumped</exc>"));
        }

        let reply = self
            .rules
            .iter_mut()
            .find(|(method, uri, _)| *method == request.method && *uri == request.uri)
            .and_then(|(_, _, replies)| replies.pop_front())
            .unwrap_or_else(|| {
                Err(TransportError::RequestNotAllowed(format!(
                    "unscripted {} {}",
                    request.method, request.uri
                )))
            });

        if self.dies_after_delete
            && request.method == "DELETE"
            && matches!(&reply, Ok(response) if (200..300).contains(&response.status))
        {
            self.dead = true;
        }

        reply
    }

    fn close(&mut self) -> Result<(), TransportError> {
        *self.closes.borrow_mut() += 1;

        if self.close_fails {
            return Err(TransportError::PreSendConnectivity(
                "logoff refused".to_owned(),
            ));
        }

        Ok(())
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.stateful
    }
}

fn lock_xml(local: bool, corrnr: &str, corruser: &str) -> String {
    format!(
        "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA>\
         <LOCK_HANDLE>{HANDLE}</LOCK_HANDLE><CORRNR>{corrnr}</CORRNR>\
         <CORRUSER>{corruser}</CORRUSER><IS_LOCAL>{}</IS_LOCAL></DATA></asx:values></asx:abap>",
        if local { "X" } else { "" }
    )
}

fn metadata(package: &str, responsible: &str) -> String {
    format!(
        "<x:object xmlns:x=\"urn:x\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
         adtcore:responsible=\"{responsible}\"><adtcore:packageRef adtcore:name=\"{package}\"/>\
         </x:object>"
    )
}

fn feed(entries: &[(&str, &str)]) -> String {
    let entries: String = entries
        .iter()
        .map(|(id, author)| {
            format!("<entry><id>{id}</id><author><name>{author}</name></author></entry>")
        })
        .collect();

    format!("<feed xmlns=\"http://www.w3.org/2005/Atom\">{entries}</feed>")
}

fn uris(calls: &Calls) -> Vec<String> {
    calls
        .borrow()
        .iter()
        .map(|call| format!("{} {}", call.method, call.uri))
        .collect()
}

fn count(calls: &Calls, method: &str) -> usize {
    calls
        .borrow()
        .iter()
        .filter(|call| call.method == method)
        .count()
}

/// A scripted ADT behind the RFC-SADT request boundary: every request must pass the same
/// admission the RFC transport applies before the script sees it.
struct Gated {
    gate: RfcTransport<FakeRfcApi>,
    inner: Fake,
}

impl SadtTransport for Gated {
    fn request(&mut self, request: Request) -> Reply {
        self.gate.request(request.clone())?;

        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close()
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.gate.stateful_supported()
    }
}

/// Runs `body` with a backend on `main`; `fresh` are the transports a new session opens.
fn with_backend<T>(
    main: Fake,
    fresh: Vec<Fake>,
    body: impl FnOnce(&mut AdtDelete) -> T,
) -> (T, Calls, Vec<Calls>) {
    let calls = main.calls.clone();
    let fresh_calls: Vec<Calls> = fresh.iter().map(|fake| fake.calls.clone()).collect();
    let mut main = main;
    let mut pool: VecDeque<Fake> = fresh.into();
    let mut open = move || -> Result<Box<dyn SadtTransport>, TransportError> {
        match pool.pop_front() {
            Some(fake) => Ok(Box::new(fake)),
            None => Err(TransportError::PreSendConnectivity("no session".to_owned())),
        }
    };
    let result = {
        let mut backend = AdtDelete::new(&mut main, USER, &mut open);

        body(&mut backend)
    };

    (result, calls, fresh_calls)
}

fn gone(uri: &str) -> Fake {
    Fake::default().get(uri, 404, "<exc/>")
}

fn present(uri: &str) -> Fake {
    Fake::default().get(uri, 200, "<x/>")
}

fn facts(result: Result<Option<ObjectFacts>, String>) -> ObjectFacts {
    result.unwrap().expect("object exists")
}

// ---- inspect ---------------------------------------------------------------------------------

fn prog_reads(package: &str) -> Fake {
    prog_reads_without_usage(package).post(
        &abapreach::adt::usage_references_uri(PROG),
        200,
        "<usageReferences:usageReferenceResult xmlns:usageReferences=\"u\"/>",
    )
}

fn prog_reads_without_usage(package: &str) -> Fake {
    Fake::default()
        .get(PROG, 200, &metadata(package, "OWNER"))
        .get(
            &format!("{PROG}/source/main?version=active"),
            200,
            "REPORT zprog.\nWRITE 1.",
        )
        .get(
            &format!("{PROG}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
}

#[test]
fn a_local_program_reads_package_author_source_and_no_inactive_version() {
    let (result, calls, _) = with_backend(prog_reads("$TMP"), vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });
    let facts = facts(result);

    assert_eq!(facts.package, "$TMP");
    assert_eq!(facts.author, "OWNER");
    assert_eq!(facts.group, None);
    assert_eq!(facts.tr, TrLock::Local);
    assert_eq!(facts.inactive, InactiveFact::Absent);
    assert_eq!(facts.sources.len(), 1);
    assert_eq!(facts.sources[0].label, "MAIN");
    assert_eq!(
        facts.sources[0].active.as_deref(),
        Some(&["REPORT zprog.".to_owned(), "WRITE 1.".to_owned()][..])
    );
    assert!(facts.contents.is_empty());
    assert!(facts.where_used.is_empty());
    assert_eq!(
        uris(&calls),
        [
            format!("GET {PROG}"),
            format!("GET {PROG}/source/main?version=active"),
            format!("POST {}", abapreach::adt::usage_references_uri(PROG)),
            format!("GET {PROG}/source/main/versions"),
        ]
    );
    // The usage search is the only POST; it changes nothing on the server.
    assert!(
        calls
            .borrow()
            .iter()
            .all(|call| call.method == "GET" || call.uri.contains("/usageReferences?"))
    );
}

#[test]
fn a_transportable_package_is_not_locked_until_the_delete_asks_for_the_lock() {
    let (result, calls, _) = with_backend(prog_reads("ZABAPTEST"), vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });

    assert_eq!(facts(result).tr, TrLock::Unlocked);
    assert!(
        calls
            .borrow()
            .iter()
            .all(|call| !call.uri.contains("_action=LOCK")),
        "inspect never locks"
    );
}

#[test]
fn an_absent_object_is_none_and_an_unreadable_one_is_an_error() {
    let neither = gone(PROG).get("/sap/bc/adt/programs/includes/zprog", 404, "<exc/>");
    let (result, _, _) = with_backend(neither, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });

    assert_eq!(result, Ok(None));

    let (result, _, _) = with_backend(
        Fake::default().get(PROG, 500, "<exc/>"),
        vec![],
        |backend| backend.inspect(DeleteKind::Prog, "ZPROG"),
    );

    assert_eq!(result.unwrap_err(), "HTTP 500");

    let (result, _, _) = with_backend(Fake::default().get(PROG, 200, "<x/>"), vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });

    assert!(result.unwrap_err().contains("package"));
}

#[test]
fn an_object_without_an_active_source_is_no_active() {
    let main = Fake::default()
        .get(PROG, 200, &metadata("$TMP", "OWNER"))
        .get(&format!("{PROG}/source/main?version=active"), 404, "<exc/>")
        .get(
            &format!("{PROG}/source/main/versions"),
            200,
            &feed(&[("99999", "OWNER")]),
        );
    let (result, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });
    let facts = facts(result);

    assert_eq!(facts.sources[0].active, None);
    assert_eq!(facts.inactive, InactiveFact::Owned("OWNER".to_owned()));
}

#[test]
fn another_users_inactive_version_wins_and_an_unreadable_feed_is_unknown() {
    let main = Fake::default()
        .get(PROG, 200, &metadata("$TMP", "OWNER"))
        .get(&format!("{PROG}/source/main?version=active"), 200, "x")
        .get(
            &format!("{PROG}/source/main/versions"),
            200,
            &feed(&[("99999", "testuser"), ("00000", "OWNER")]),
        );
    let (own, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });

    assert_eq!(
        facts(own).inactive,
        InactiveFact::Owned("testuser".to_owned())
    );

    let main = Fake::default()
        .get(PROG, 200, &metadata("$TMP", "OWNER"))
        .get(&format!("{PROG}/source/main?version=active"), 200, "x")
        .get(&format!("{PROG}/source/main/versions"), 500, "boom");
    let (unknown, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });

    assert!(matches!(
        facts(unknown).inactive,
        InactiveFact::Unknown(reason) if reason.contains("500")
    ));
}

#[test]
fn a_class_lists_its_main_source_and_every_include_that_exists() {
    let main = Fake::default()
        .get(CLAS, 200, &metadata("$TMP", "OWNER"))
        .get(
            &format!("{CLAS}/source/main?version=active"),
            200,
            "CLASS zcl_a DEFINITION.",
        )
        .get(
            &format!("{CLAS}/includes/definitions?version=active"),
            404,
            "",
        )
        .get(
            &format!("{CLAS}/includes/implementations?version=active"),
            404,
            "",
        )
        .get(&format!("{CLAS}/includes/macros?version=active"), 404, "")
        .get(
            &format!("{CLAS}/includes/testclasses?version=active"),
            200,
            "t",
        )
        .get(
            &format!("{CLAS}/includes/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
        .get(
            &format!("{CLAS}/includes/testclasses/versions"),
            200,
            &feed(&[("99999", "OTHER")]),
        );
    let (result, calls, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Clas, "ZCL_A")
    });
    let facts = facts(result);
    let labels: Vec<&str> = facts.sources.iter().map(|s| s.label.as_str()).collect();

    assert_eq!(labels, ["MAIN", "CCAU"]);
    assert_eq!(
        facts.contents,
        ["INCLUDE ZCL_A=========================CCAU"]
    );
    assert_eq!(facts.inactive, InactiveFact::Owned("OTHER".to_owned()));
    assert!(calls.borrow().iter().all(|call| call.method == "GET"));
}

#[test]
fn an_interface_reads_like_a_program() {
    let main = Fake::default()
        .get(INTF, 200, &metadata("$TMP", "OWNER"))
        .get(
            &format!("{INTF}/source/main?version=active"),
            200,
            "INTERFACE zif_a PUBLIC.",
        )
        .get(
            &format!("{INTF}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        );
    let (result, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Intf, "ZIF_A")
    });

    assert_eq!(facts(result).package, "$TMP");
}

fn search_hit() -> String {
    format!(
        "<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\">\
         <adtcore:objectReference adtcore:uri=\"{FM}\" adtcore:type=\"FUGR/FF\" \
         adtcore:name=\"ZFM\"/></adtcore:objectReferences>"
    )
}

#[test]
fn a_function_module_resolves_its_group_and_reports_where_used() {
    let callers = "<usageReferences:usageReferenceResult xmlns:usageReferences=\"u\">\
        <usageReferences:referencedObjects><usageReferences:referencedObject>\
        <usageReferences:adtObject adtcore:name=\"ZCALLER\" adtcore:type=\"PROG/P\" \
        xmlns:adtcore=\"http://www.sap.com/adt/core\"/></usageReferences:referencedObject>\
        </usageReferences:referencedObjects></usageReferences:usageReferenceResult>";
    let main = Fake::default()
        .get(SEARCH, 200, &search_hit())
        .get(FM, 200, &metadata("", "OWNER"))
        .get(GROUP, 200, &metadata("ZABAPTEST", "OWNER"))
        .get(
            &format!("{FM}/source/main?version=active"),
            200,
            "FUNCTION zfm.",
        )
        .get(
            &format!("{FM}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
        .post(&abapreach::adt::usage_references_uri(FM), 200, callers);
    let (result, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Func, "ZFM")
    });
    let facts = facts(result);

    assert_eq!(facts.group.as_deref(), Some("ZFG"));
    assert_eq!(facts.package, "ZABAPTEST");
    assert_eq!(facts.tr, TrLock::Unlocked);
    assert_eq!(facts.where_used, ["PROG/P ZCALLER"]);
}

fn group_metadata() -> String {
    "<group:abapFunctionGroup xmlns:group=\"g\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
     adtcore:responsible=\"OWNER\"><adtcore:packageRef adtcore:name=\"$TMP\"/>\
     </group:abapFunctionGroup>"
        .to_owned()
}

fn node(object_type: &str, name: &str) -> String {
    format!(
        "<SEU_ADT_REPOSITORY_OBJ_NODE><OBJECT_TYPE>{object_type}</OBJECT_TYPE>\
         <OBJECT_NAME>{name}</OBJECT_NAME></SEU_ADT_REPOSITORY_OBJ_NODE>"
    )
}

fn listing(nodes: &str) -> String {
    format!(
        "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA>\
         <TREE_CONTENT>{nodes}</TREE_CONTENT></DATA></asx:values></asx:abap>"
    )
}

fn inspect_group(listing: Fake) -> (Result<Option<ObjectFacts>, String>, Calls) {
    let (result, calls, _) = with_backend(listing, vec![], |backend| {
        backend.inspect(DeleteKind::Fugr, "ZFG")
    });

    (result, calls)
}

#[test]
fn a_function_group_reports_its_members_and_has_no_source() {
    let nodes = [
        node("FUGR/FF", "ZFM_B"),
        node("FUGR/I", "LZFGTOP"),
        node("FUGR/FF", "zfm_a"),
        node("FUGR/FF", "ZFM_A"),
    ]
    .concat();
    let main =
        Fake::default()
            .get(GROUP, 200, &group_metadata())
            .post(MEMBERS, 200, &listing(&nodes));
    let (result, calls) = inspect_group(main);
    let facts = facts(result);

    assert_eq!(facts.contents, ["FUNC ZFM_A", "FUNC ZFM_B"]);
    assert!(facts.sources.is_empty());
    assert_eq!(facts.inactive, InactiveFact::Absent);
    assert_eq!(
        uris(&calls),
        [format!("GET {GROUP}"), format!("POST {MEMBERS}")]
    );

    let post = &calls.borrow()[1];

    assert!(post.body.is_empty());
    assert_eq!(
        post.headers.get("Accept").map(String::as_str),
        Some("application/*")
    );
}

#[test]
fn a_function_group_without_function_modules_has_no_members() {
    let main = Fake::default().get(GROUP, 200, &group_metadata()).post(
        MEMBERS,
        200,
        &listing(&node("FUGR/I", "LZFGTOP")),
    );

    assert!(facts(inspect_group(main).0).contents.is_empty());
}

#[test]
fn the_recorded_member_listing_yields_its_function_module_and_ignores_its_includes() {
    let recorded = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/m4/nodestructure-hd1-yfg-utils.xml"
    ))
    .unwrap();
    let uri = "/sap/bc/adt/repository/nodestructure?parent_type=FUGR%2FF&parent_name=YFG_UTILS&withShortDescriptions=false";
    let main = Fake::default()
        .get(
            "/sap/bc/adt/functions/groups/yfg_utils",
            200,
            &group_metadata(),
        )
        .post(uri, 200, &recorded);
    let (result, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Fugr, "YFG_UTILS")
    });

    assert_eq!(facts(result).contents, ["FUNC YFM_UTILS_SOURCE_DEPLOY"]);
}

#[test]
fn the_recorded_listing_of_a_group_without_function_modules_is_an_empty_plan() {
    let recorded = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/m4/nodestructure-hd1-empty-group.xml"
    ))
    .unwrap();
    let uri = "/sap/bc/adt/repository/nodestructure?parent_type=FUGR%2FF&parent_name=YTEST_ABAPREACH_PMEG1&withShortDescriptions=false";
    let main = Fake::default()
        .get(
            "/sap/bc/adt/functions/groups/ytest_abapreach_pmeg1",
            200,
            &group_metadata(),
        )
        .post(uri, 200, &recorded);
    let (result, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Fugr, "YTEST_ABAPREACH_PMEG1")
    });

    assert!(facts(result).contents.is_empty());
}

#[test]
fn a_function_module_that_has_only_an_inactive_version_is_a_member() {
    let recorded = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/m4/nodestructure-hd1-inactive-fm.xml"
    ))
    .unwrap();
    let uri = "/sap/bc/adt/repository/nodestructure?parent_type=FUGR%2FF&parent_name=YTEST_ABAPREACH_PMEG3&withShortDescriptions=false";
    let main = Fake::default()
        .get(
            "/sap/bc/adt/functions/groups/ytest_abapreach_pmeg3",
            200,
            &group_metadata(),
        )
        .post(uri, 200, &recorded);
    let (result, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Fugr, "YTEST_ABAPREACH_PMEG3")
    });

    assert_eq!(facts(result).contents, ["FUNC YTEST_ABAPREACH_PMEF3"]);
}

#[test]
fn an_unusable_member_listing_fails_the_plan_instead_of_emptying_it() {
    let answers = [
        (500, listing("")),
        (404, listing("")),
        (200, String::new()),
        (200, "not xml <".to_owned()),
        (200, "<asx:abap xmlns:asx=\"urn:x\"/>".to_owned()),
        // Cut after the first node: no closing TREE_CONTENT.
        (
            200,
            listing(&node("FUGR/FF", "ZFM_A"))
                .split("</TREE_CONTENT>")
                .next()
                .unwrap()
                .to_owned(),
        ),
        // A function module without a usable name, in each spelling.
        (200, listing(&node("FUGR/FF", ""))),
        (200, listing(&node("FUGR/FF", "ZFM A"))),
        (200, listing(&node("FUGR/FF", "ZFM<"))),
        (200, listing(&node("FUGR/FF", &"Z".repeat(31)))),
        (
            200,
            listing(
                "<SEU_ADT_REPOSITORY_OBJ_NODE><OBJECT_TYPE>FUGR/FF</OBJECT_TYPE>\
                 <OBJECT_NAME><![CDATA[]]></OBJECT_NAME></SEU_ADT_REPOSITORY_OBJ_NODE>",
            ),
        ),
        // The summary names function modules, the tree lists none.
        (
            200,
            listing(&node("FUGR/I", "LZFGTOP")).replace(
                "</DATA>",
                "<OBJECT_TYPES><SEU_ADT_OBJECT_TYPE_INFO><OBJECT_TYPE>FUGR/FF</OBJECT_TYPE>\
                 </SEU_ADT_OBJECT_TYPE_INFO></OBJECT_TYPES></DATA>",
            ),
        ),
    ];

    for (status, body) in answers {
        let main = Fake::default()
            .get(GROUP, 200, &group_metadata())
            .post(MEMBERS, status, &body);
        let (result, _) = inspect_group(main);

        assert!(
            result.is_err(),
            "{status} {body:?} must not give a plan: {result:?}"
        );
    }

    let unscripted = Fake::default().get(GROUP, 200, &group_metadata());

    assert!(inspect_group(unscripted).0.is_err());
}

// ---- delete ----------------------------------------------------------------------------------

#[test]
fn a_local_delete_locks_once_deletes_once_and_proves_it_from_a_fresh_session() {
    let main = Fake::default().lock(PROG, true, "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    );
    let (reply, calls, fresh) = with_backend(main, vec![gone(PROG)], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);
    assert_eq!(
        uris(&calls),
        [
            format!("POST {PROG}{LOCK}"),
            format!("DELETE {PROG}?lockHandle={ENCODED}"),
        ]
    );
    assert_eq!(uris(&fresh[0]), [format!("GET {PROG}")]);
}

#[test]
fn a_transportable_delete_carries_the_request_of_the_lock() {
    let main = Fake::default().lock(CLAS, false, TR).delete(
        &format!("{CLAS}?lockHandle={ENCODED}&corrNr={TR}"),
        Ok(Response::empty(200)),
    );
    let (reply, calls, _) = with_backend(main, vec![gone(CLAS)], |backend| {
        backend.delete(DeleteKind::Clas, "ZCL_A", Some(TR))
    });

    assert_eq!(reply, DeleteReply::Deleted);
    assert_eq!(count(&calls, "DELETE"), 1);
}

#[test]
fn a_transportable_delete_without_an_explicit_tr_uses_the_request_of_the_lock() {
    let main = Fake::default().lock(CLAS, false, TR).delete(
        &format!("{CLAS}?lockHandle={ENCODED}&corrNr={TR}"),
        Ok(Response::empty(200)),
    );
    let (reply, calls, _) = with_backend(main, vec![gone(CLAS)], |backend| {
        backend.delete(DeleteKind::Clas, "ZCL_A", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);
    assert!(uris(&calls).contains(&format!("DELETE {CLAS}?lockHandle={ENCODED}&corrNr={TR}")));
}

#[test]
fn a_different_explicit_tr_unlocks_and_never_deletes() {
    let main = Fake::default().lock(PROG, false, TR).post(
        &format!("{PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
        200,
        "",
    );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", Some("DEVK900002"))
    });

    assert!(
        matches!(reply, DeleteReply::Rejected(ref reason) if reason.contains("does not match"))
    );
    assert_eq!(count(&calls, "DELETE"), 0);
    assert_eq!(count(&calls, "POST"), 2);
}

#[test]
fn an_own_request_of_the_lock_is_followed() {
    let main = Fake::default().lock_by(CLAS, false, TR, USER).delete(
        &format!("{CLAS}?lockHandle={ENCODED}&corrNr={TR}"),
        Ok(Response::empty(200)),
    );
    let (reply, calls, _) = with_backend(main, vec![gone(CLAS)], |backend| {
        backend.delete(DeleteKind::Clas, "ZCL_A", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);
    assert!(uris(&calls).contains(&format!("DELETE {CLAS}?lockHandle={ENCODED}&corrNr={TR}")));
}

#[test]
fn a_request_of_another_user_unlocks_and_never_deletes() {
    let main = Fake::default().lock_by(PROG, false, TR, "OTHERUSR").post(
        &format!("{PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
        200,
        "",
    );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Rejected(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(
        reason.contains(TR) && reason.contains("another user"),
        "{reason}"
    );
    assert!(!reason.contains("OTHERUSR"), "{reason}");
    assert_eq!(count(&calls, "DELETE"), 0);
    assert_eq!(
        uris(&calls),
        [
            format!("POST {PROG}{LOCK}"),
            format!("POST {PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
        ]
    );
}

#[test]
fn an_empty_owner_on_a_local_object_changes_nothing() {
    let main = Fake::default().lock_by(PROG, true, "", "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    );
    let (reply, _, _) = with_backend(main, vec![gone(PROG)], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);
}

#[test]
fn a_transportable_lock_without_a_request_unlocks_and_never_deletes() {
    let main = Fake::default().lock(PROG, false, "").post(
        &format!("{PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
        200,
        "",
    );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    assert!(
        matches!(reply, DeleteReply::Rejected(ref reason) if reason.contains("no transport request"))
    );
    assert_eq!(count(&calls, "DELETE"), 0);
}

#[test]
fn the_request_of_the_lock_is_used_when_the_plan_names_none_and_checked_when_it_does() {
    let main = Fake::default().lock(INTF, false, TR).delete(
        &format!("{INTF}?lockHandle={ENCODED}&corrNr={TR}"),
        Ok(Response::empty(200)),
    );
    let (reply, _, _) = with_backend(main, vec![gone(INTF)], |backend| {
        backend.delete(DeleteKind::Intf, "ZIF_A", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);

    let cases: [(bool, &str, Option<&str>, &str); 3] = [
        (false, "DEVK900002", Some(TR), "does not match"),
        (true, "", Some(TR), "local"),
        (false, "", None, "no transport request"),
    ];

    for (local, corrnr, tr, expected) in cases {
        let main = Fake::default().lock(PROG, local, corrnr).post(
            &format!("{PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
            200,
            "",
        );
        let (reply, calls, _) = with_backend(main, vec![], |backend| {
            backend.delete(DeleteKind::Prog, "ZPROG", tr)
        });

        let DeleteReply::Rejected(reason) = reply else {
            panic!("{local} {corrnr}: {reply:?}");
        };

        assert!(reason.contains(expected), "{reason}");
        assert_eq!(count(&calls, "DELETE"), 0, "{reason}");
        assert_eq!(count(&calls, "POST"), 2, "lock and unlock: {reason}");
    }
}

#[test]
fn a_function_group_is_locked_and_deleted_as_a_group() {
    let main = Fake::default().lock(GROUP, true, "").delete(
        &format!("{GROUP}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    );
    let (reply, calls, _) = with_backend(main, vec![gone(GROUP)], |backend| {
        backend.delete(DeleteKind::Fugr, "ZFG", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);
    assert_eq!(
        uris(&calls),
        [
            format!("POST {GROUP}{LOCK}"),
            format!("DELETE {GROUP}?lockHandle={ENCODED}"),
        ]
    );
}

#[test]
fn a_function_module_takes_its_own_lock_not_the_group_lock() {
    let main = Fake::default()
        .get(SEARCH, 200, &search_hit())
        .lock(FM, true, "")
        .delete(
            &format!("{FM}?lockHandle={ENCODED}"),
            Ok(Response::empty(200)),
        );
    let (reply, calls, fresh) = with_backend(main, vec![gone(FM)], |backend| {
        backend.delete(DeleteKind::Func, "ZFM", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);
    assert!(
        uris(&calls).contains(&format!("DELETE {FM}?lockHandle={ENCODED}")),
        "{:?}",
        uris(&calls)
    );
    assert!(
        uris(&calls)
            .iter()
            .all(|call| !call.contains(GROUP) || call.contains("/fmodules/"))
    );
    assert_eq!(uris(&fresh[0]), [format!("GET {FM}")]);
}

#[test]
fn a_refused_lock_sends_no_delete_and_leaves_nothing_to_unlock() {
    let main = Fake::default().post(
        &format!("{PROG}{LOCK}"),
        403,
        "<exc:exception xmlns:exc=\"e\"><localizedMessage>User X is currently editing ZPROG\
         </localizedMessage></exc:exception>",
    );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Rejected(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(reason.contains("lock failed: HTTP 403"), "{reason}");
    assert!(reason.contains("User X is currently editing"), "{reason}");
    assert_eq!(uris(&calls), [format!("POST {PROG}{LOCK}")]);
}

#[test]
fn a_lock_whose_reply_is_lost_names_the_lock_that_may_remain() {
    let main = Fake::default().on(
        "POST",
        &format!("{PROG}{LOCK}"),
        Err(TransportError::PostSendUncertain("reply lost".to_owned())),
    );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Rejected(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(reason.contains("a lock may remain"), "{reason}");
    assert_eq!(count(&calls, "DELETE"), 0);
}

#[test]
fn an_unreadable_lock_reply_sends_no_delete() {
    let main = Fake::default().post(&format!("{PROG}{LOCK}"), 200, "<x/>");
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Rejected(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(reason.contains("unreadable lock response"), "{reason}");
    assert!(reason.contains("a lock may remain"), "{reason}");
    assert_eq!(count(&calls, "DELETE"), 0);
}

#[test]
fn a_session_without_state_is_refused_before_any_request() {
    let (reply, calls, _) = with_backend(Fake::default().stateful(false), vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Rejected(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(reason.contains("stateful session unavailable"), "{reason}");
    assert!(calls.borrow().is_empty());
}

#[test]
fn a_refused_delete_unlocks_and_never_prints_the_lock_handle() {
    let echo = format!(
        "<exc:exception xmlns:exc=\"e\"><localizedMessage>Resource MAIN ZPROG is not locked \
         (invalid lock handle: {HANDLE} / {ENCODED})</localizedMessage></exc:exception>"
    );
    let main = Fake::default()
        .lock(PROG, true, "")
        .delete(
            &format!("{PROG}?lockHandle={ENCODED}"),
            Ok(Response::text(423, echo)),
        )
        .post(
            &format!("{PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
            200,
            "",
        );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Rejected(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(reason.contains("DELETE failed: HTTP 423"), "{reason}");
    assert!(!reason.contains(HANDLE), "{reason}");
    assert!(!reason.contains(ENCODED), "{reason}");
    assert!(!reason.contains("a lock may remain"), "{reason}");
    assert_eq!(count(&calls, "DELETE"), 1);
    assert_eq!(count(&calls, "POST"), 2);
}

#[test]
fn a_refused_delete_whose_unlock_fails_says_a_lock_may_remain() {
    let main = Fake::default()
        .lock(PROG, true, "")
        .delete(
            &format!("{PROG}?lockHandle={ENCODED}"),
            Ok(Response::empty(400)),
        )
        .post(
            &format!("{PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
            500,
            "",
        );
    let (reply, _, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Rejected(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(reason.contains("a lock may remain"), "{reason}");
}

#[test]
fn a_transport_error_text_is_scrubbed_of_the_lock_handle() {
    let main = Fake::default().lock(PROG, true, "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Err(TransportError::Sent(Box::new(TransportError::Http {
            status: 423,
            message: format!("invalid lock handle: {HANDLE} {ENCODED}"),
        }))),
    );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    let DeleteReply::Uncertain(reason) = reply else {
        panic!("{reply:?}");
    };

    assert!(!reason.contains(HANDLE), "{reason}");
    assert!(!reason.contains(ENCODED), "{reason}");
    assert_eq!(count(&calls, "DELETE"), 1);
    assert_eq!(
        count(&calls, "POST"),
        1,
        "no unlock after an unknown outcome"
    );
}

#[test]
fn a_lost_delete_reply_is_uncertain_and_is_never_repeated() {
    let main = Fake::default().lock(PROG, true, "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Err(TransportError::PostSendUncertain("reply lost".to_owned())),
    );
    let (reply, calls, fresh) = with_backend(main, vec![gone(PROG)], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    assert!(matches!(reply, DeleteReply::Uncertain(_)));
    assert_eq!(count(&calls, "DELETE"), 1);
    assert_eq!(count(&calls, "POST"), 1);
    assert!(
        fresh[0].borrow().is_empty(),
        "reconciling is the caller's step"
    );
}

#[test]
fn a_server_error_on_the_delete_is_uncertain_not_rejected() {
    let main = Fake::default().lock(PROG, true, "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Ok(Response::empty(500)),
    );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    assert!(matches!(reply, DeleteReply::Uncertain(reason) if reason.contains("HTTP 500")));
    assert_eq!(count(&calls, "POST"), 1);
}

#[test]
fn a_delete_that_answers_404_is_not_found() {
    let main = Fake::default().lock(PROG, true, "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Ok(Response::empty(404)),
    );
    let (reply, _, _) = with_backend(main, vec![], |backend| {
        backend.delete(DeleteKind::Prog, "ZPROG", None)
    });

    assert_eq!(reply, DeleteReply::NotFound);
}

#[test]
fn a_200_that_leaves_the_object_in_a_fresh_session_is_partial() {
    let main = Fake::default().lock(GROUP, true, "").delete(
        &format!("{GROUP}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    );
    let (reply, _, _) = with_backend(main, vec![present(GROUP)], |backend| {
        backend.delete(DeleteKind::Fugr, "ZFG", None)
    });

    assert!(matches!(reply, DeleteReply::Partial(reason) if reason.contains("still exists")));
}

#[test]
fn a_200_stays_deleted_when_the_fresh_session_cannot_be_read() {
    for fresh in [vec![], vec![Fake::default().get(PROG, 500, "")]] {
        let main = Fake::default().lock(PROG, true, "").delete(
            &format!("{PROG}?lockHandle={ENCODED}"),
            Ok(Response::empty(200)),
        );
        let (reply, _, _) = with_backend(main, fresh, |backend| {
            backend.delete(DeleteKind::Prog, "ZPROG", None)
        });

        assert_eq!(reply, DeleteReply::Deleted);
    }
}

// ---- reconcile -------------------------------------------------------------------------------

#[test]
fn reconciling_reads_the_object_once_from_a_new_session_and_closes_it() {
    let fresh = gone(CLAS);
    let closes = fresh.closes.clone();
    let (outcome, calls, fresh_calls) = with_backend(Fake::default(), vec![fresh], |backend| {
        backend.reconcile(DeleteKind::Clas, "ZCL_A")
    });

    assert_eq!(outcome, Reconcile::Gone);
    assert!(calls.borrow().is_empty(), "the old session is not reused");
    assert_eq!(uris(&fresh_calls[0]), [format!("GET {CLAS}")]);
    assert_eq!(*closes.borrow(), 1);
}

#[test]
fn reconciling_classifies_every_answer() {
    let cases: [(Fake, Reconcile); 3] = [
        (present(PROG), Reconcile::StillExists),
        (
            Fake::default().get(PROG, 500, ""),
            Reconcile::Failed("HTTP 500".to_owned()),
        ),
        (
            Fake::default().on(
                "GET",
                PROG,
                Err(TransportError::NoResponse("closed".to_owned())),
            ),
            Reconcile::Failed("no response to a read-only request: closed".to_owned()),
        ),
    ];

    for (fresh, expected) in cases {
        let (outcome, _, _) = with_backend(Fake::default(), vec![fresh], |backend| {
            backend.reconcile(DeleteKind::Prog, "ZPROG")
        });

        assert_eq!(outcome, expected);
    }

    let (outcome, _, _) = with_backend(Fake::default(), vec![], |backend| {
        backend.reconcile(DeleteKind::Prog, "ZPROG")
    });

    assert!(matches!(outcome, Reconcile::Failed(reason) if reason.contains("no session")));
}

#[test]
fn a_function_module_is_reconciled_by_its_own_uri() {
    let main = Fake::default().get(SEARCH, 200, &search_hit());
    let (outcome, _, fresh) = with_backend(main, vec![gone(FM)], |backend| {
        backend.reconcile(DeleteKind::Func, "ZFM")
    });

    assert_eq!(outcome, Reconcile::Gone);
    assert_eq!(uris(&fresh[0]), [format!("GET {FM}")]);
}

// ---- program includes ------------------------------------------------------------------------

const INCL: &str = "/sap/bc/adt/programs/includes/zinc";
/// The program URI of the include's name: the first place resolution asks.
const INCL_AS_PROG: &str = "/sap/bc/adt/programs/programs/zinc";
const USAGE_TRAN: &str = include_str!("fixtures/m5/usage-references-tran-hd1.xml");
const INCL_LOCK: &str = include_str!("fixtures/m5/include-lock-hd1-transportable.xml");

fn usage_with(callers: &[(&str, &str)]) -> String {
    let objects: String = callers
        .iter()
        .map(|(kind, name)| {
            format!(
                "<usageReferences:referencedObject><usageReferences:adtObject \
                 adtcore:name=\"{name}\" adtcore:type=\"{kind}\" \
                 xmlns:adtcore=\"http://www.sap.com/adt/core\"/>\
                 </usageReferences:referencedObject>"
            )
        })
        .collect();

    format!(
        "<usageReferences:usageReferenceResult xmlns:usageReferences=\"u\">\
         <usageReferences:referencedObjects>{objects}</usageReferences:referencedObjects>\
         </usageReferences:usageReferenceResult>"
    )
}

fn usage_of(uri: &str) -> String {
    abapreach::adt::usage_references_uri(uri)
}

/// The reads of an include `ZINC`: resolution, properties, active source, versions, usage.
fn incl_reads(package: &str, usage: &str) -> Fake {
    Fake::default()
        .get(INCL_AS_PROG, 404, "<exc/>")
        .get(INCL, 200, &metadata(package, "OWNER"))
        .get(
            &format!("{INCL}/source/main?version=active"),
            200,
            "WRITE 1.",
        )
        .get(
            &format!("{INCL}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
        .post(&usage_of(INCL), 200, usage)
}

#[test]
fn an_include_resolves_to_its_own_uri_and_reads_source_versions_and_callers() {
    let usage = usage_with(&[("PROG/P", "ZPROG"), ("DEVC/K", "$TMP")]);
    let (result, calls, _) = with_backend(incl_reads("$TMP", &usage), vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZINC")
    });
    let facts = facts(result);

    assert_eq!(facts.package, "$TMP");
    assert_eq!(facts.tr, TrLock::Local);
    assert_eq!(facts.sources.len(), 1);
    assert_eq!(facts.sources[0].label, "MAIN");
    assert_eq!(
        facts.sources[0].active.as_deref(),
        Some(&["WRITE 1.".to_owned()][..])
    );
    assert_eq!(facts.where_used, ["PROG/P ZPROG"]);
    assert_eq!(
        uris(&calls),
        [
            format!("GET {INCL_AS_PROG}"),
            format!("GET {INCL}"),
            format!("GET {INCL}/source/main?version=active"),
            format!("POST {}", usage_of(INCL)),
            format!("GET {INCL}/source/main/versions"),
        ]
    );
}

#[test]
fn an_include_that_neither_uri_knows_is_none_and_a_failed_resolution_is_an_error() {
    let both = Fake::default()
        .get(INCL_AS_PROG, 404, "<exc/>")
        .get(INCL, 404, "<exc/>");
    let (result, _, _) = with_backend(both, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZINC")
    });

    assert_eq!(result, Ok(None));

    let broken = Fake::default()
        .get(INCL_AS_PROG, 404, "<exc/>")
        .get(INCL, 500, "<exc/>");
    let (result, _, _) = with_backend(broken, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZINC")
    });

    assert_eq!(result.unwrap_err(), "HTTP 500");
}

#[test]
fn an_include_without_an_active_source_is_no_active_here_too() {
    let main = Fake::default()
        .get(INCL_AS_PROG, 404, "<exc/>")
        .get(INCL, 200, &metadata("$TMP", "OWNER"))
        .get(&format!("{INCL}/source/main?version=active"), 404, "<exc/>")
        .get(
            &format!("{INCL}/source/main/versions"),
            200,
            &feed(&[("99999", "OWNER")]),
        )
        .post(&usage_of(INCL), 200, &usage_with(&[]));
    let (result, _, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZINC")
    });
    let facts = facts(result);

    assert_eq!(facts.sources[0].active, None);
    assert_eq!(facts.inactive, InactiveFact::Owned("OWNER".to_owned()));
}

fn prog_with_usage(usage: Fake) -> Fake {
    let mut main = prog_reads_without_usage("$TMP");

    main.rules.extend(usage.rules);

    main
}

fn prog_where_used(answer: Fake) -> Result<Option<ObjectFacts>, String> {
    with_backend(prog_with_usage(answer), vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    })
    .0
}

#[test]
fn a_program_reports_the_transaction_of_the_recorded_usage_answer() {
    let answer = Fake::default().post(&usage_of(PROG), 200, USAGE_TRAN);

    assert_eq!(facts(prog_where_used(answer)).where_used, ["TRAN ZTRAN"]);
}

#[test]
fn programs_come_before_transactions_and_packages_are_dropped() {
    let usage = usage_with(&[
        ("TRAN/T", "ZTRAN"),
        ("PROG/P", "ZCALLER"),
        ("DEVC/K", "$TMP"),
        ("TRAN/T", "ZTRAN2"),
    ]);
    let answer = Fake::default().post(&usage_of(PROG), 200, &usage);

    assert_eq!(
        facts(prog_where_used(answer)).where_used,
        ["PROG/P ZCALLER", "TRAN ZTRAN", "TRAN ZTRAN2"]
    );
}

#[test]
fn a_program_without_users_reports_nothing() {
    let answer = Fake::default().post(&usage_of(PROG), 200, &usage_with(&[("DEVC/K", "$TMP")]));

    assert!(facts(prog_where_used(answer)).where_used.is_empty());
}

#[test]
fn a_failed_usage_lookup_is_the_m3_warning_and_does_not_fail_the_plan() {
    let refused = Fake::default().post(&usage_of(PROG), 500, "<exc/>");
    let lost = Fake::default().on(
        "POST",
        &usage_of(PROG),
        Err(TransportError::NoResponse("closed".to_owned())),
    );
    let unscripted = Fake::default();

    for (answer, reason) in [
        (refused, "HTTP 500"),
        (lost, "no response to a read-only request: closed"),
        (unscripted, "SADT request is not allowed: unscripted POST"),
    ] {
        let facts = facts(prog_where_used(answer));

        assert_eq!(facts.where_used.len(), 1);
        assert!(
            facts.where_used[0].starts_with(&format!("W: where-used unavailable: {reason}")),
            "{:?}",
            facts.where_used
        );
        assert_eq!(facts.sources.len(), 1, "the plan is still complete");
    }
}

#[test]
fn only_a_resolved_program_include_is_asked_once() {
    let main = Fake::default()
        .get(INCL_AS_PROG, 404, "<exc/>")
        .get(INCL, 200, "<x/>")
        .get(PROG, 200, "<x/>");
    let (answers, calls, _) = with_backend(main, vec![], |backend| {
        (
            backend.is_include(DeleteKind::Prog, "ZINC"),
            backend.is_include(DeleteKind::Prog, "zinc"),
            backend.is_include(DeleteKind::Prog, "ZPROG"),
            backend.is_include(DeleteKind::Clas, "ZINC"),
        )
    });

    assert_eq!(answers, (true, true, false, false));
    assert_eq!(
        uris(&calls),
        [
            format!("GET {INCL_AS_PROG}"),
            format!("GET {INCL}"),
            format!("GET {PROG}"),
        ]
    );

    let failing = Fake::default().get(INCL_AS_PROG, 500, "<exc/>");
    let (answer, _, _) = with_backend(failing, vec![], |backend| {
        backend.is_include(DeleteKind::Prog, "ZINC")
    });

    assert!(!answer, "a failed read surfaces at inspect, not here");
}

#[test]
fn an_include_is_locked_deleted_and_proved_gone_by_its_own_uri() {
    let main = incl_reads("$TMP", &usage_with(&[]))
        .lock(INCL, true, "")
        .delete(
            &format!("{INCL}?lockHandle={ENCODED}"),
            Ok(Response::empty(200)),
        );
    let (reply, calls, fresh) = with_backend(main, vec![gone(INCL)], |backend| {
        backend.inspect(DeleteKind::Prog, "ZINC").unwrap();

        backend.delete(DeleteKind::Prog, "ZINC", None)
    });

    assert_eq!(reply, DeleteReply::Deleted);
    assert_eq!(
        &uris(&calls)[5..],
        [
            format!("POST {INCL}{LOCK}"),
            format!("DELETE {INCL}?lockHandle={ENCODED}"),
        ]
    );
    assert_eq!(uris(&fresh[0]), [format!("GET {INCL}")]);
}

#[test]
fn a_transportable_include_delete_follows_the_recorded_lock_and_sends_its_request() {
    let lock = INCL_LOCK
        .replace("a/b+c=", HANDLE)
        .replace("DEVK900001", TR);
    let main = incl_reads("ZABAPTEST", &usage_with(&[]))
        .post(&format!("{INCL}{LOCK}"), 200, &lock)
        .delete(
            &format!("{INCL}?lockHandle={ENCODED}&corrNr={TR}"),
            Ok(Response::empty(200)),
        );
    let (outcome, calls, _) = with_backend(main, vec![gone(INCL)], |backend| {
        let inspected = facts(backend.inspect(DeleteKind::Prog, "ZINC"));

        (inspected.tr, backend.delete(DeleteKind::Prog, "ZINC", None))
    });

    assert_eq!(outcome, (TrLock::Unlocked, DeleteReply::Deleted));
    assert_eq!(count(&calls, "DELETE"), 1);
    assert!(uris(&calls).contains(&format!("DELETE {INCL}?lockHandle={ENCODED}&corrNr={TR}")));
}

#[test]
fn an_include_that_a_program_still_references_is_rejected_and_unlocked() {
    let refused = "<exc:exception xmlns:exc=\"e\"><localizedMessage>Program ZINC is referenced in \
                   other programs</localizedMessage></exc:exception>";
    let main = incl_reads("$TMP", &usage_with(&[("PROG/P", "ZPROG")]))
        .lock(INCL, true, "")
        .delete(
            &format!("{INCL}?lockHandle={ENCODED}"),
            Ok(Response::text(403, refused)),
        )
        .post(
            &format!("{INCL}?_action=UNLOCK&lockHandle={ENCODED}"),
            200,
            "",
        );
    let (reply, calls, _) = with_backend(main, vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZINC").unwrap();

        backend.delete(DeleteKind::Prog, "ZINC", None)
    });

    assert_eq!(
        reply,
        DeleteReply::Rejected(
            "DELETE failed: HTTP 403: Program ZINC is referenced in other programs".to_owned()
        )
    );
    assert_eq!(count(&calls, "DELETE"), 1);
    assert!(uris(&calls).contains(&format!("POST {INCL}?_action=UNLOCK&lockHandle={ENCODED}")));
}

#[test]
fn an_include_is_reconciled_by_its_own_uri() {
    let (outcome, _, fresh) = with_backend(
        incl_reads("$TMP", &usage_with(&[])),
        vec![gone(INCL)],
        |backend| {
            backend.inspect(DeleteKind::Prog, "ZINC").unwrap();

            backend.reconcile(DeleteKind::Prog, "ZINC")
        },
    );

    assert_eq!(outcome, Reconcile::Gone);
    assert_eq!(uris(&fresh[0]), [format!("GET {INCL}")]);
}

struct Never;

impl abapreach::policy::Confirmer for Never {
    fn interactive(&self) -> bool {
        false
    }

    fn confirm(&self, _prompt: &str) -> bool {
        false
    }
}

/// The inspect reads of the program `ZPROG` and the include `ZINC`, once, in `ZPROG`, `ZINC` order
/// after the resolution answers `resolution` gave.
fn mixed_reads(main: Fake) -> Fake {
    let mut main = main
        .get(PROG, 200, &metadata("$TMP", "OWNER"))
        .get(
            &format!("{PROG}/source/main?version=active"),
            200,
            "REPORT zprog.\nINCLUDE zinc.",
        )
        .get(
            &format!("{PROG}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
        .post(&usage_of(PROG), 200, &usage_with(&[]))
        .get(INCL, 200, &metadata("$TMP", "OWNER"))
        .get(
            &format!("{INCL}/source/main?version=active"),
            200,
            "WRITE 1.",
        )
        .get(
            &format!("{INCL}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
        .post(&usage_of(INCL), 200, &usage_with(&[("PROG/P", "ZPROG")]));

    main.stateful = Some(true);

    main
}

#[test]
fn a_mixed_batch_deletes_the_program_before_its_include_with_the_same_plan() {
    use abapreach::cli::DeleteTarget;
    use abapreach::operation::delete::{Context, execute, simulate};
    use abapreach::policy::Policies;

    let targets: Vec<DeleteTarget> = ["PROG:ZINC", "PROG:ZPROG"]
        .iter()
        .map(|text| text.parse().expect("target"))
        .collect();
    let policies = Policies::default();
    let context = Context {
        conn: "DEV_100",
        user: USER,
        policies: &policies,
        confirmer: &Never,
    };
    // Ordering asks the include, then the program; each inspect reads its object again.
    let resolution = || {
        Fake::default()
            .get(INCL_AS_PROG, 404, "<exc/>")
            .get(INCL, 200, "<x/>")
            .get(PROG, 200, "<x/>")
    };
    let simulation = mixed_reads(resolution());
    let (report, sim_calls, _) = with_backend(simulation, vec![], |backend| {
        simulate(backend, &context, &targets, None).unwrap()
    });
    let plan = report.plan.clone().expect("plan");
    let out = &report.outcome.stdout;

    assert_eq!(report.outcome.exit, 0, "{out}");
    assert_eq!(
        plan.objects
            .iter()
            .map(|(_, name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["ZPROG", "ZINC"]
    );
    assert!(out.contains("WHERE-USED: PROG/P ZPROG"), "{out}");
    assert_eq!(count(&sim_calls, "DELETE"), 0);

    let execution = mixed_reads(resolution()).lock(PROG, true, "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    );
    // The delete of the program ends its session: the include runs in a new one, and each
    // delete is verified from a short-lived session of its own.
    let include_session = Fake::default()
        .stateful(true)
        .get(INCL, 200, &metadata("$TMP", "OWNER"))
        .get(
            &format!("{INCL}/source/main?version=active"),
            200,
            "WRITE 1.",
        )
        .get(
            &format!("{INCL}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
        .post(&usage_of(INCL), 200, &usage_with(&[("PROG/P", "ZPROG")]))
        .lock(INCL, true, "")
        .delete(
            &format!("{INCL}?lockHandle={ENCODED}"),
            Ok(Response::empty(200)),
        );
    let include_calls = include_session.calls.clone();
    let (report, calls, _) = with_backend(
        execution,
        vec![gone(PROG), include_session, gone(INCL)],
        |backend| execute(backend, &context, &targets, None, &plan).unwrap(),
    );
    let deletes: Vec<String> = uris(&calls)
        .into_iter()
        .chain(uris(&include_calls))
        .filter(|call| call.starts_with("DELETE"))
        .collect();

    assert_eq!(report.outcome.exit, 0, "{}", report.outcome.stdout);
    assert_eq!(
        deletes,
        [
            format!("DELETE {PROG}?lockHandle={ENCODED}"),
            format!("DELETE {INCL}?lockHandle={ENCODED}"),
        ]
    );
}

#[test]
fn a_mixed_batch_runs_through_the_rfc_sadt_boundary_and_prints_the_program_where_used() {
    use abapreach::cli::DeleteTarget;
    use abapreach::operation::delete::{Context, execute, simulate};
    use abapreach::policy::Policies;

    let targets: Vec<DeleteTarget> = ["PROG:ZINC", "PROG:ZPROG"]
        .iter()
        .map(|text| text.parse().expect("target"))
        .collect();
    let policies = Policies::default();
    let context = Context {
        conn: "DEV_100",
        user: USER,
        policies: &policies,
        confirmer: &Never,
    };
    let resolution = || {
        Fake::default()
            .get(INCL_AS_PROG, 404, "<exc/>")
            .get(INCL, 200, "<x/>")
            .get(PROG, 200, "<x/>")
    };
    let gated = |inner: Fake| Gated {
        gate: RfcTransport::new(FakeRfcApi::ok()),
        inner,
    };
    let simulation = mixed_reads(resolution());
    let sim_calls = simulation.calls.clone();
    let mut main = gated(simulation);
    let mut open = || -> Result<Box<dyn SadtTransport>, TransportError> {
        Err(TransportError::PreSendConnectivity("no session".to_owned()))
    };
    let report = {
        let mut backend = AdtDelete::new(&mut main, USER, &mut open);

        simulate(&mut backend, &context, &targets, None).unwrap()
    };
    let plan = report.plan.clone().expect("plan");
    let out = &report.outcome.stdout;

    assert_eq!(report.outcome.exit, 0, "{out}");
    assert!(out.contains("WHERE-USED: PROG/P ZPROG"), "{out}");
    assert_eq!(count(&sim_calls, "DELETE"), 0);

    let execution = mixed_reads(resolution())
        .lock(PROG, true, "")
        .delete(
            &format!("{PROG}?lockHandle={ENCODED}"),
            Ok(Response::empty(200)),
        )
        .lock(INCL, true, "")
        .delete(
            &format!("{INCL}?lockHandle={ENCODED}"),
            Ok(Response::empty(200)),
        );
    let exec_calls = execution.calls.clone();
    let mut main = gated(execution);
    let mut fresh = VecDeque::from([gated(gone(PROG)), gated(gone(INCL))]);
    let mut open = move || -> Result<Box<dyn SadtTransport>, TransportError> {
        match fresh.pop_front() {
            Some(session) => Ok(Box::new(session)),
            None => Err(TransportError::PreSendConnectivity("no session".to_owned())),
        }
    };
    let report = {
        let mut backend = AdtDelete::new(&mut main, USER, &mut open);

        execute(&mut backend, &context, &targets, None, &plan).unwrap()
    };

    assert_eq!(report.outcome.exit, 0, "{}", report.outcome.stdout);
    assert!(
        uris(&exec_calls).contains(&format!("DELETE {INCL}?lockHandle={ENCODED}")),
        "{:?}",
        uris(&exec_calls)
    );
}

// ---- one stateful session per deleted row ------------------------------------------------------

struct Session {
    calls: Calls,
    closes: Rc<RefCell<usize>>,
}

impl Session {
    fn of(fake: &Fake) -> Self {
        Self {
            calls: fake.calls.clone(),
            closes: fake.closes.clone(),
        }
    }

    fn closes(&self) -> usize {
        *self.closes.borrow()
    }
}

fn batch_context<'a>(
    policies: &'a abapreach::policy::Policies,
) -> abapreach::operation::delete::Context<'a> {
    abapreach::operation::delete::Context {
        conn: "DEV_100",
        user: USER,
        policies,
        confirmer: &Never,
    }
}

fn targets(texts: &[&str]) -> Vec<abapreach::cli::DeleteTarget> {
    texts
        .iter()
        .map(|text| text.parse().expect("target"))
        .collect()
}

fn include_reads(session: Fake) -> Fake {
    session
        .get(INCL, 200, &metadata("$TMP", "OWNER"))
        .get(
            &format!("{INCL}/source/main?version=active"),
            200,
            "WRITE 1.",
        )
        .get(
            &format!("{INCL}/source/main/versions"),
            200,
            &feed(&[("00000", "OWNER")]),
        )
        .post(&usage_of(INCL), 200, &usage_with(&[("PROG/P", "ZPROG")]))
}

fn include_delete(session: Fake) -> Fake {
    session.lock(INCL, true, "").delete(
        &format!("{INCL}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    )
}

fn parent_delete(session: Fake) -> Fake {
    session.lock(PROG, true, "").delete(
        &format!("{PROG}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    )
}

/// The simulate of `PROG:ZINC PROG:ZPROG` and its plan; the execute needs the same plan.
fn include_and_parent_plan() -> abapreach::operation::delete::Plan {
    use abapreach::operation::delete::simulate;

    let policies = abapreach::policy::Policies::default();
    let context = batch_context(&policies);
    let simulation = mixed_reads(
        Fake::default()
            .get(INCL_AS_PROG, 404, "<exc/>")
            .get(INCL, 200, "<x/>")
            .get(PROG, 200, "<x/>"),
    );
    let (report, _, _) = with_backend(simulation, vec![], |backend| {
        simulate(
            backend,
            &context,
            &targets(&["PROG:ZINC", "PROG:ZPROG"]),
            None,
        )
        .unwrap()
    });

    report.plan.expect("plan")
}

fn resolution() -> Fake {
    Fake::default()
        .get(INCL_AS_PROG, 404, "<exc/>")
        .get(INCL, 200, "<x/>")
        .get(PROG, 200, "<x/>")
}

/// The main session of the batch: ordering, then everything the parent program needs.
fn main_session() -> Fake {
    let mut main = parent_delete(mixed_reads(resolution())).dies_after_delete();

    main.stateful = Some(true);

    main
}

#[test]
fn a_session_that_dies_after_the_parent_delete_does_not_fail_the_include() {
    use abapreach::operation::delete::execute;

    let plan = include_and_parent_plan();
    let policies = abapreach::policy::Policies::default();
    let context = batch_context(&policies);
    let main = main_session();
    let main_log = Session::of(&main);
    let parent_check = gone(PROG);
    let parent_check_log = Session::of(&parent_check);
    let include_session = include_delete(include_reads(Fake::default().stateful(true)));
    let include_log = Session::of(&include_session);
    let include_check = gone(INCL);
    let include_check_log = Session::of(&include_check);
    let (report, _, _) = with_backend(
        main,
        vec![parent_check, include_session, include_check],
        |backend| {
            let report = execute(
                backend,
                &context,
                &targets(&["PROG:ZINC", "PROG:ZPROG"]),
                None,
                &plan,
            )
            .unwrap();

            assert_eq!(backend.close_note(), None);

            report
        },
    );
    let out = &report.outcome.stdout;

    assert_eq!(report.outcome.exit, 0, "{out}");
    assert!(out.contains("DELETE: DELETED"), "{out}");
    assert_eq!(out.matches("DELETE: DELETED").count(), 2, "{out}");
    // Nothing reaches the dead session after the parent DELETE.
    assert_eq!(
        uris(&main_log.calls).last().map(String::as_str),
        Some(format!("DELETE {PROG}?lockHandle={ENCODED}").as_str())
    );
    // Logoffs equal stateful sessions: the main session and the include session once each, and
    // no logoff for the sessions that only verify.
    assert_eq!(main_log.closes(), 1);
    assert_eq!(include_log.closes(), 1);
    assert_eq!(parent_check_log.closes(), 1);
    assert_eq!(include_check_log.closes(), 1);
    assert!(
        uris(&include_log.calls)
            .iter()
            .any(|call| call == &format!("DELETE {INCL}?lockHandle={ENCODED}")),
        "{:?}",
        uris(&include_log.calls)
    );
}

#[test]
fn a_batch_without_a_successful_delete_keeps_one_session() {
    use abapreach::operation::delete::{execute, simulate};

    let policies = abapreach::policy::Policies::default();
    let context = batch_context(&policies);
    let rejecting = || {
        let mut main = prog_reads("$TMP")
            .lock(PROG, true, "")
            .on(
                "DELETE",
                &format!("{PROG}?lockHandle={ENCODED}"),
                Ok(Response::text(403, "<exc/>")),
            )
            .post(
                &format!("{PROG}?_action=UNLOCK&lockHandle={ENCODED}"),
                200,
                "",
            );

        main.stateful = Some(true);

        main
    };
    let (sim, _, _) = with_backend(prog_reads("$TMP"), vec![], |backend| {
        simulate(backend, &context, &targets(&["PROG:ZPROG"]), None).unwrap()
    });
    let plan = sim.plan.expect("plan");
    let main = rejecting();
    let main_log = Session::of(&main);
    let (report, _, _) = with_backend(main, vec![], |backend| {
        let report = execute(backend, &context, &targets(&["PROG:ZPROG"]), None, &plan).unwrap();

        assert_eq!(backend.close_note(), None);

        report
    });

    assert_eq!(report.outcome.exit, 1, "{}", report.outcome.stdout);
    assert!(report.outcome.stdout.contains("REJECTED"));
    // The backend never ends the caller's session itself when nothing was deleted; the caller does.
    assert_eq!(main_log.closes(), 0);
}

#[test]
fn a_logoff_failure_between_rows_prints_the_note_and_the_batch_continues() {
    use abapreach::operation::delete::execute;

    let plan = include_and_parent_plan();
    let policies = abapreach::policy::Policies::default();
    let context = batch_context(&policies);
    let main = main_session().close_fails();
    let include_session = include_delete(include_reads(Fake::default().stateful(true)));
    let (report, _, _) = with_backend(
        main,
        vec![gone(PROG), include_session, gone(INCL)],
        |backend| {
            let report = execute(
                backend,
                &context,
                &targets(&["PROG:ZINC", "PROG:ZPROG"]),
                None,
                &plan,
            )
            .unwrap();
            let note = backend.close_note().expect("note");

            assert_eq!(
                note,
                "W: session close failed: pre-send connectivity failure: logoff refused"
            );

            report
        },
    );

    assert_eq!(report.outcome.exit, 0, "{}", report.outcome.stdout);
    assert_eq!(
        report.outcome.stdout.matches("DELETE: DELETED").count(),
        2,
        "{}",
        report.outcome.stdout
    );
}

#[test]
fn a_new_session_that_cannot_be_opened_stops_the_batch_after_the_row() {
    use abapreach::operation::delete::{execute, simulate};

    let policies = abapreach::policy::Policies::default();
    let context = batch_context(&policies);
    let rows = ["INTF:ZIF_A", "PROG:ZINC", "PROG:ZPROG"];
    let reads = || {
        let mut main = mixed_reads(resolution())
            .get(INTF, 200, &metadata("$TMP", "OWNER"))
            .get(
                &format!("{INTF}/source/main?version=active"),
                200,
                "INTERFACE zif_a PUBLIC.",
            )
            .get(
                &format!("{INTF}/source/main/versions"),
                200,
                &feed(&[("00000", "OWNER")]),
            );

        main.stateful = Some(true);

        main
    };
    let (sim, _, _) = with_backend(reads(), vec![], |backend| {
        simulate(backend, &context, &targets(&rows), None).unwrap()
    });
    let plan = sim.plan.expect("plan");
    let main = reads().lock(INTF, true, "").delete(
        &format!("{INTF}?lockHandle={ENCODED}"),
        Ok(Response::empty(200)),
    );
    let main_log = Session::of(&main);
    // Only the verification session is available: the one for the next row cannot be opened.
    let (report, _, _) = with_backend(main, vec![gone(INTF)], |backend| {
        execute(backend, &context, &targets(&rows), None, &plan).unwrap()
    });
    let out = &report.outcome.stdout;

    assert_eq!(report.outcome.exit, 1, "{out}");
    assert!(out.contains("cannot open a new session"), "{out}");
    assert!(out.contains("SUMMARY: 1 OK, 1 FAILED, 1 SKIPPED"), "{out}");
    assert_eq!(main_log.closes(), 1);
    assert_eq!(count(&main_log.calls, "DELETE"), 1);
}
