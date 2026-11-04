//! The ADT side of M4 delete against a scripted ADT: the read-only facts, the one lock-and-DELETE
//! sequence, the fresh-session verification and the classification of every uncertain answer.
//! Every request is matched by method and exact URI; an unscripted request is a failed test.

use abapreach::cli::DeleteKind;
use abapreach::operation::delete::{
    DeleteBackend, DeleteReply, InactiveFact, ObjectFacts, Reconcile, TrLock,
};
use abapreach::operation::delete_adt::AdtDelete;
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

    fn lock(self, uri: &str, local: bool, corrnr: &str) -> Self {
        self.post(&format!("{uri}{LOCK}"), 200, &lock_xml(local, corrnr))
    }
}

impl SadtTransport for Fake {
    fn request(&mut self, request: Request) -> Reply {
        self.calls.borrow_mut().push(request.clone());

        self.rules
            .iter_mut()
            .find(|(method, uri, _)| *method == request.method && *uri == request.uri)
            .and_then(|(_, _, replies)| replies.pop_front())
            .unwrap_or_else(|| {
                Err(TransportError::RequestNotAllowed(format!(
                    "unscripted {} {}",
                    request.method, request.uri
                )))
            })
    }

    fn close(&mut self) -> Result<(), TransportError> {
        *self.closes.borrow_mut() += 1;

        Ok(())
    }

    fn stateful_supported(&self) -> Option<bool> {
        self.stateful
    }
}

fn lock_xml(local: bool, corrnr: &str) -> String {
    format!(
        "<asx:abap xmlns:asx=\"http://www.sap.com/abapxml\"><asx:values><DATA>\
         <LOCK_HANDLE>{HANDLE}</LOCK_HANDLE><CORRNR>{corrnr}</CORRNR>\
         <IS_LOCAL>{}</IS_LOCAL></DATA></asx:values></asx:abap>",
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
            format!("GET {PROG}/source/main/versions"),
        ]
    );
    assert!(calls.borrow().iter().all(|call| call.method == "GET"));
}

#[test]
fn a_transportable_package_is_not_locked_until_the_delete_asks_for_the_lock() {
    let (result, calls, _) = with_backend(prog_reads("ZABAPTEST"), vec![], |backend| {
        backend.inspect(DeleteKind::Prog, "ZPROG")
    });

    assert_eq!(facts(result).tr, TrLock::Unlocked);
    assert_eq!(count(&calls, "POST"), 0);
}

#[test]
fn an_absent_object_is_none_and_an_unreadable_one_is_an_error() {
    let (result, _, _) = with_backend(gone(PROG), vec![], |backend| {
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

#[test]
fn a_function_group_reports_its_members_and_has_no_source() {
    let group = format!(
        "<group:abapFunctionGroup xmlns:group=\"g\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
         xmlns:atom=\"http://www.w3.org/2005/Atom\" adtcore:responsible=\"OWNER\">\
         <adtcore:packageRef adtcore:name=\"$TMP\"/>\
         <atom:link href=\"{GROUP}/fmodules/zfm_b\" rel=\"x\"/>\
         <atom:link href=\"{GROUP}/fmodules/zfm_a\" rel=\"x\"/>\
         <atom:link href=\"{GROUP}/fmodules/zfm_a\" rel=\"y\"/></group:abapFunctionGroup>"
    );
    let (result, calls, _) =
        with_backend(Fake::default().get(GROUP, 200, &group), vec![], |backend| {
            backend.inspect(DeleteKind::Fugr, "ZFG")
        });
    let facts = facts(result);

    assert_eq!(facts.contents, ["FUNC ZFM_A", "FUNC ZFM_B"]);
    assert!(facts.sources.is_empty());
    assert_eq!(facts.inactive, InactiveFact::Absent);
    assert_eq!(uris(&calls), [format!("GET {GROUP}")]);
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
