//! The request boundary for M4 delete: exactly the evidenced DELETE shape, the function group
//! lock, and the bare object read that verifies a delete. Everything else stays refused.

use abapreach::transport::{
    Request, SadtTransport, TransportError,
    rfc::{FakeRfcApi, RfcTransport},
};

const HANDLE: &str = "qdbcyR%2BxAqwY%3D";
const KINDS: [&str; 5] = [
    "/sap/bc/adt/programs/programs/zprog",
    "/sap/bc/adt/functions/groups/zfg",
    "/sap/bc/adt/functions/groups/zfg/fmodules/zfm",
    "/sap/bc/adt/oo/classes/zcl_a",
    "/sap/bc/adt/oo/interfaces/zif_a",
];

fn admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

#[track_caller]
fn assert_admitted(requests: &[Request]) {
    for request in requests {
        assert!(admits(request), "must be admitted: {request:?}");
    }
}

#[track_caller]
fn assert_refused(requests: &[Request]) {
    for request in requests {
        assert!(!admits(request), "must be refused: {request:?}");
    }
}

#[test]
fn a_delete_of_each_kind_is_admitted_with_and_without_a_request() {
    for uri in KINDS {
        assert_admitted(&[
            Request::delete(format!("{uri}?lockHandle={HANDLE}")),
            Request::delete(format!("{uri}?lockHandle={HANDLE}&corrNr=DEVK900001")),
        ]);
    }
}

#[test]
fn the_function_group_lock_and_unlock_are_admitted() {
    assert_admitted(&[
        Request::post(
            "/sap/bc/adt/functions/groups/zfg?_action=LOCK&accessMode=MODIFY",
            Vec::new(),
        ),
        Request::post(
            format!("/sap/bc/adt/functions/groups/zfg?_action=UNLOCK&lockHandle={HANDLE}"),
            Vec::new(),
        ),
    ]);
}

#[test]
fn the_verification_read_of_a_program_and_a_class_is_admitted() {
    assert_admitted(&[
        Request::get("/sap/bc/adt/programs/programs/zprog"),
        Request::get("/sap/bc/adt/oo/classes/zcl_a"),
    ]);
}

#[test]
fn a_delete_outside_the_evidenced_shape_is_refused() {
    let program = KINDS[0];
    let class = KINDS[3];
    let mut body = Request::delete(format!("{program}?lockHandle={HANDLE}"));
    body.body = b"x".to_vec();

    assert_refused(&[
        // No lock handle, an empty or unencoded one, a bad or reordered request.
        Request::delete(program),
        Request::delete(format!("{program}?")),
        Request::delete(format!("{program}?lockHandle=")),
        Request::delete(format!("{program}?lockHandle=a+b")),
        Request::delete(format!("{program}?lockHandle=a%2")),
        Request::delete(format!("{program}?corrNr=DEVK900001")),
        Request::delete(format!("{program}?corrNr=DEVK900001&lockHandle={HANDLE}")),
        Request::delete(format!("{program}?lockHandle={HANDLE}&corrNr=")),
        Request::delete(format!("{program}?lockHandle={HANDLE}&corrNr=DEV K")),
        Request::delete(format!(
            "{program}?lockHandle={HANDLE}&corrNr=DEVK9000010000000000X"
        )),
        Request::delete(format!("{program}?lockHandle={HANDLE}&x=1")),
        Request::delete(format!(
            "{program}?lockHandle={HANDLE}&corrNr=DEVK900001&x=1"
        )),
        Request::delete(format!("{program}?lockHandle={HANDLE}#frag")),
        body,
        // Not an object: collections, parts below an object, other endpoints.
        Request::delete(format!("/sap/bc/adt/oo/classes?lockHandle={HANDLE}")),
        Request::delete(format!("/sap/bc/adt/functions/groups?lockHandle={HANDLE}")),
        Request::delete(format!("{class}/source/main?lockHandle={HANDLE}")),
        Request::delete(format!("{class}/includes/testclasses?lockHandle={HANDLE}")),
        Request::delete(format!("{class}/objectstructure?lockHandle={HANDLE}")),
        Request::delete(format!("/sap/bc/adt/discovery?lockHandle={HANDLE}")),
        Request::delete(format!(
            "/sap/bc/adt/datapreview/freestyle?lockHandle={HANDLE}"
        )),
        Request::delete("/sap/bc/adt/datapreview/ddic?rowNumber=1&ddicEntityName=T000"),
        Request::delete(format!("/sap/bc/adt/activation?lockHandle={HANDLE}")),
        // Traversal, wrong kind path, namespaced class names.
        Request::delete(format!(
            "/sap/bc/adt/programs/programs/../classes/zcl?lockHandle={HANDLE}"
        )),
        Request::delete(format!(
            "/sap/bc/adt/oo/classes/%2e%2e/zcl?lockHandle={HANDLE}"
        )),
        Request::delete(format!(
            "/sap/bc/adt/oo/classes/zcl_a/fmodules/zfm?lockHandle={HANDLE}"
        )),
        Request::delete(format!(
            "/sap/bc/adt/functions/groups/zfg/fmodules/../zfm?lockHandle={HANDLE}"
        )),
        Request::delete(format!(
            "/sap/bc/adt/oo/classes/%2fns%2fcl?lockHandle={HANDLE}"
        )),
        Request::delete(format!("/sap/bc/adt/oo/classes//zcl?lockHandle={HANDLE}")),
        // Method spelling is not normalized.
        Request::new(
            "delete",
            format!("{program}?lockHandle={HANDLE}"),
            Vec::new(),
        ),
    ]);
}

#[test]
fn nothing_else_of_the_function_group_opens() {
    let group = "/sap/bc/adt/functions/groups/zfg";

    assert_refused(&[
        // A group lock carries no request and is never written under.
        Request::post(
            format!("{group}?_action=LOCK&accessMode=MODIFY&corrNr=DEVK900001"),
            Vec::new(),
        ),
        Request::post(format!("{group}?_action=LOCK"), Vec::new()),
        Request::post(
            "/sap/bc/adt/functions/groups?_action=LOCK&accessMode=MODIFY",
            Vec::new(),
        ),
        Request::post(format!("{group}?_action=UNLOCK&lockHandle="), Vec::new()),
        Request::post(format!("{group}?_action=UNLOCK&lockHandle=a+b"), Vec::new()),
        Request::put(format!("{group}?lockHandle={HANDLE}"), b"x".to_vec()),
        Request::get(format!("{group}/source/main?version=active")),
        Request::get(format!("{group}?version=active")),
    ]);
}

#[test]
fn bare_reads_stay_limited_to_the_object_itself() {
    assert_refused(&[
        Request::get("/sap/bc/adt/programs/programs/zprog?version=active"),
        Request::get("/sap/bc/adt/oo/classes/zcl_a?x=1"),
        Request::get("/sap/bc/adt/programs/programs"),
        Request::get("/sap/bc/adt/oo/classes"),
        Request::get("/sap/bc/adt/programs/programs/../x"),
        Request::get("/sap/bc/adt/oo/classes/%2fns%2fcl"),
    ]);
}
