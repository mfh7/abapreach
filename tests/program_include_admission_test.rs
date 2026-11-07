//! M5 Task 4: the RFC-SADT allow-list admits exactly the evidenced program and include shapes, and
//! the ADT request builders emit those same shapes (the HTTP transport sends them unchanged).

use abapreach::cli::Subc;
use abapreach::object::{
    PackageScope, context_uri, include_uri, mainprograms_uri, program_creation_request_uri,
    program_validation_uri,
};
use abapreach::operation::create::program_creation_body;
use abapreach::operation::deploy::{activation_request_xml, checkrun_xml_for, encode_lock_handle};
use abapreach::transport::{
    Request, SadtTransport, TransportError,
    rfc::{FakeRfcApi, RfcTransport},
};

const INCLUDE: &str = "/sap/bc/adt/programs/includes/zincl_a";
const PROGRAM: &str = "/sap/bc/adt/programs/programs/zprog_a";
const CONTEXT: &str = "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_a";
const CHECKRUNS: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const ACTIVATION: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";
const TRANSPORTABLE: &str = "DEVK900001";

fn context_include() -> String {
    format!("{INCLUDE}?context={CONTEXT}")
}

fn with_type(mut request: Request, content_type: &str) -> Request {
    request
        .headers
        .insert("Content-Type".to_owned(), content_type.to_owned());
    request
}

fn any(request: Request) -> Request {
    with_type(request, "application/*")
}

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

fn program_body() -> Vec<u8> {
    program_creation_body(
        Subc::Program,
        "zprog_a",
        "Probe parent",
        "$TMP",
        "DEVUSER",
        "EN",
    )
    .into_bytes()
}

fn include_body() -> Vec<u8> {
    program_creation_body(
        Subc::Include,
        "zincl_a",
        "Probe include",
        "$TMP",
        "DEVUSER",
        "EN",
    )
    .into_bytes()
}

fn checkrun(object: &str, source: &str) -> Request {
    any(Request::post(
        CHECKRUNS,
        checkrun_xml_for(object, source, &["WRITE / 'i3'.".to_owned()]).into_bytes(),
    ))
}

fn activation(uri: &str) -> Request {
    with_type(
        Request::post(
            ACTIVATION,
            activation_request_xml("zincl_a", uri).into_bytes(),
        ),
        "application/xml",
    )
}

fn activation_body(body: &str) -> Request {
    with_type(
        Request::post(ACTIVATION, body.as_bytes().to_vec()),
        "application/xml",
    )
}

fn handle() -> String {
    encode_lock_handle("qdbc+yR6/xoaD=")
}

// ---- Builders: the recorded wire shapes ----------------------------------------------------

#[test]
fn builders_emit_the_recorded_program_and_include_shapes() {
    let transportable = PackageScope::Transportable {
        request: TRANSPORTABLE.to_owned(),
    };

    assert_eq!(include_uri("ZINCL_A"), INCLUDE);
    assert_eq!(
        mainprograms_uri("ZINCL_A"),
        format!("{INCLUDE}/mainprograms")
    );
    assert_eq!(
        context_uri("ZINCL_A", "ZPROG_A"),
        format!("{INCLUDE}?context={CONTEXT}")
    );
    assert_eq!(
        program_creation_request_uri(Subc::Program, &PackageScope::Local),
        "/sap/bc/adt/programs/programs"
    );
    assert_eq!(
        program_creation_request_uri(Subc::Include, &PackageScope::Local),
        "/sap/bc/adt/programs/includes"
    );
    assert_eq!(
        program_creation_request_uri(Subc::Program, &transportable),
        "/sap/bc/adt/programs/programs?corrNr=DEVK900001"
    );
    assert_eq!(
        program_creation_request_uri(Subc::Include, &transportable),
        "/sap/bc/adt/programs/includes?corrNr=DEVK900001"
    );
    assert_eq!(
        program_validation_uri(Subc::Program, "zprog_a", "probe", "$tmp"),
        "/sap/bc/adt/programs/validation?objname=ZPROG_A&packagename=%24TMP&description=probe&objtype=PROG/P"
    );
    assert_eq!(
        program_validation_uri(Subc::Include, "zincl_a", "a b", "$TMP"),
        "/sap/bc/adt/programs/validation?objname=ZINCL_A&packagename=%24TMP&description=a%20b&objtype=PROG/I"
    );
}

#[test]
fn create_bodies_match_the_recorded_requests() {
    assert_eq!(
        String::from_utf8(program_body()).unwrap(),
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<program:abapProgram xmlns:program="http://www.sap.com/adt/programs/programs" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core" adtcore:description="Probe parent" "#,
            r#"adtcore:name="ZPROG_A" adtcore:type="PROG/P" adtcore:language="EN" "#,
            r#"adtcore:masterLanguage="EN" adtcore:responsible="DEVUSER">"#,
            r#"<adtcore:packageRef adtcore:name="$TMP"/></program:abapProgram>"#,
        )
    );
    // The include carries no `containerRef`: the parent is bound by the first deploy.
    assert_eq!(
        String::from_utf8(include_body()).unwrap(),
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<include:abapInclude xmlns:include="http://www.sap.com/adt/programs/includes" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core" adtcore:description="Probe include" "#,
            r#"adtcore:name="ZINCL_A" adtcore:type="PROG/I" adtcore:language="EN" "#,
            r#"adtcore:masterLanguage="EN" adtcore:responsible="DEVUSER">"#,
            r#"<adtcore:packageRef adtcore:name="$TMP"/></include:abapInclude>"#,
        )
    );
}

#[test]
fn the_contextual_checkrun_and_activation_bodies_match_the_recorded_requests() {
    let context = context_uri("zincl_a", "zprog_a");
    let check = checkrun_xml_for(
        &context,
        &format!("{INCLUDE}/source/main"),
        &["WRITE / 'i3'.".to_owned()],
    );

    assert_eq!(
        check,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<chkrun:checkObjectList xmlns:chkrun="http://www.sap.com/adt/checkrun" "#,
            r#"xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<chkrun:checkObject adtcore:uri="/sap/bc/adt/programs/includes/zincl_a"#,
            r#"?context=%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_a" chkrun:version="active">"#,
            r#"<chkrun:artifacts><chkrun:artifact chkrun:contentType="text/plain; charset=utf-8" "#,
            r#"chkrun:uri="/sap/bc/adt/programs/includes/zincl_a/source/main">"#,
            r#"<chkrun:content>V1JJVEUgLyAnaTMnLgo=</chkrun:content></chkrun:artifact>"#,
            r#"</chkrun:artifacts></chkrun:checkObject></chkrun:checkObjectList>"#,
        )
    );
    assert_eq!(
        activation_request_xml("zincl_a", &context),
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
            r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/programs/includes/zincl_a"#,
            r#"?context=%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_a" "#,
            r#"adtcore:name="ZINCL_A"/></adtcore:objectReferences>"#,
        )
    );
}

// ---- Admission: every newly admitted shape -------------------------------------------------

#[test]
fn program_and_include_create_posts_are_admitted_with_and_without_corr_nr() {
    let transportable = PackageScope::Transportable {
        request: TRANSPORTABLE.to_owned(),
    };
    let mut requests = Vec::new();

    for (subc, body) in [
        (Subc::Program, program_body()),
        (Subc::Include, include_body()),
    ] {
        for scope in [&PackageScope::Local, &transportable] {
            requests.push(any(Request::post(
                program_creation_request_uri(subc, scope),
                body.clone(),
            )));
        }
    }

    assert_admitted(&requests);
}

#[test]
fn program_validation_is_admitted_for_both_object_types_with_an_empty_body() {
    assert_admitted(&[
        Request::post(
            program_validation_uri(Subc::Program, "zprog_a", "probe", "$TMP"),
            Vec::new(),
        ),
        Request::post(
            program_validation_uri(Subc::Include, "zincl_a", "probe", "$TMP"),
            Vec::new(),
        ),
        Request::post(
            program_validation_uri(Subc::Include, "zincl_a", "", "ZPKG"),
            Vec::new(),
        ),
    ]);
}

#[test]
fn include_lock_unlock_source_and_versions_are_admitted() {
    let handle = handle();

    assert_admitted(&[
        Request::get(INCLUDE),
        Request::get(include_uri("ZINCL_A")),
        Request::post(
            format!("{INCLUDE}?_action=LOCK&accessMode=MODIFY"),
            Vec::new(),
        ),
        Request::post(
            format!("{INCLUDE}?_action=UNLOCK&lockHandle={handle}"),
            Vec::new(),
        ),
        Request::get(format!("{INCLUDE}/source/main?version=active")),
        Request::get(format!("{INCLUDE}/source/main?version=inactive")),
        Request::get(format!("{INCLUDE}/source/main/versions")),
        with_type(
            Request::put(
                format!("{INCLUDE}/source/main?lockHandle={handle}"),
                b"WRITE 1.".to_vec(),
            ),
            "text/plain; charset=utf-8",
        ),
        with_type(
            Request::put(
                format!("{INCLUDE}/source/main?lockHandle={handle}&corrNr={TRANSPORTABLE}"),
                b"WRITE 1.".to_vec(),
            ),
            "text/plain; charset=utf-8",
        ),
    ]);
}

#[test]
fn mainprograms_is_admitted_as_a_bare_get() {
    assert_admitted(&[
        Request::get(mainprograms_uri("ZINCL_A")),
        Request::get(format!("{INCLUDE}/mainprograms")),
    ]);
}

#[test]
fn checkrun_and_activation_of_an_include_are_admitted_with_the_context_in_the_body_uri() {
    let context = context_uri("zincl_a", "zprog_a");
    let source = format!("{INCLUDE}/source/main");

    assert_admitted(&[
        checkrun(&context, &source),
        // A checkrun without context only reports `notProcessed`; it is still the M1 shape.
        checkrun(INCLUDE, &source),
        activation(&context),
    ]);
}

#[test]
fn objects_whose_names_contain_context_are_admitted_in_checkrun_and_activation() {
    // 0.14.0 admitted these; the word is only a marker when it is a query, not a name.
    let objects = [
        "/sap/bc/adt/programs/programs/zcontext_demo",
        "/sap/bc/adt/oo/classes/zcl_context_mgr",
        "/sap/bc/adt/oo/interfaces/zif_context",
    ];
    let reference = |uri: &str, parent: &str| {
        activation_body(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="{uri}" adtcore:name="ZX" adtcore:parentUri="{parent}"/></adtcore:objectReferences>"#
        ))
    };

    for object in objects {
        assert_admitted(&[
            checkrun(object, &format!("{object}/source/main")),
            activation(object),
            reference(object, "/sap/bc/adt/packages/zcontext_pkg"),
        ]);
    }

    // The control: a query on the same object is still refused.
    assert_refused(&[
        checkrun(
            &format!("{}?context={CONTEXT}", objects[0]),
            &format!("{}/source/main", objects[0]),
        ),
        checkrun(
            &format!("{}%3Fx", objects[1]),
            &format!("{INCLUDE}/source/main"),
        ),
        checkrun(
            &format!("{}%23x", objects[1]),
            &format!("{INCLUDE}/source/main"),
        ),
        reference(objects[0], &format!("{}?x=1", objects[1])),
    ]);
}

// ---- Admission: negatives ------------------------------------------------------------------

#[test]
fn traversal_wrong_kinds_and_include_shapes_outside_the_evidence_are_refused() {
    let handle = handle();
    let lock = "_action=LOCK&accessMode=MODIFY";

    assert_refused(&[
        Request::get("/sap/bc/adt/programs/includes/../programs/zprog_a"),
        Request::get("/sap/bc/adt/programs/includes/%2e%2e"),
        Request::get("/sap/bc/adt/programs/includes/zincl_a/../x/source/main?version=active"),
        Request::get("/sap/bc/adt/programs/includes/zi/ncl/source/main?version=active"),
        Request::get("/sap/bc/adt/programs/includes/"),
        Request::get("/sap/bc/adt/programs/includes"),
        Request::get("/sap/bc/adt/programs/includes/%2fns%2fincl"),
        Request::get("/sap/bc/adt/programs/includes/z$incl"),
        Request::get(format!("{INCLUDE}/includes/main")),
        Request::get(format!("{INCLUDE}/objectstructure")),
        Request::get(format!("{INCLUDE}/source/main/versions/99999")),
        Request::get(format!("{INCLUDE}/source/main?version=workingArea")),
        Request::get(format!("{INCLUDE}/source/main")),
        Request::get(format!("{INCLUDE}?version=active")),
        Request::get(format!("{INCLUDE}/versions")),
        Request::post(
            format!("{INCLUDE}/source/main?lockHandle={handle}"),
            Vec::new(),
        ),
        Request::post(
            format!("{INCLUDE}?{lock}&corrNr={TRANSPORTABLE}"),
            Vec::new(),
        ),
        Request::post(
            format!("{INCLUDE}?_action=LOCK&accessMode=READ"),
            Vec::new(),
        ),
        Request::post(format!("{INCLUDE}?_action=UNLOCK&lockHandle="), Vec::new()),
        Request::post(format!("{INCLUDE}/mainprograms?{lock}"), Vec::new()),
        // The program endpoints take no include-only parts, and the include has no class parts.
        Request::get(format!("{PROGRAM}/mainprograms")),
        Request::get("/sap/bc/adt/oo/classes/zcl_a/mainprograms"),
        Request::get("/sap/bc/adt/functions/groups/zfg/mainprograms"),
        Request::get("/sap/bc/adt/programs/freestyle/zincl_a/source/main?version=active"),
        Request::get("/sap/bc/adt/datapreview/freestyle"),
    ]);
}

#[test]
fn mainprograms_refuses_a_query_and_every_other_method() {
    let uri = format!("{INCLUDE}/mainprograms");

    assert_refused(&[
        Request::get(format!("{uri}?context={CONTEXT}")),
        Request::get(format!("{uri}?x=1")),
        Request::get(format!("{uri}?")),
        Request::get(format!("{uri}/")),
        Request::get(format!("{uri}/x")),
        Request::post(uri.clone(), Vec::new()),
        any(Request::post(uri.clone(), b"<x/>".to_vec())),
        Request::put(uri.clone(), Vec::new()),
        with_type(
            Request::put(uri.clone(), Vec::new()),
            "text/plain; charset=utf-8",
        ),
        Request::delete(uri.clone()),
        Request::new("PATCH", uri.clone(), Vec::new()),
        Request::new("HEAD", uri, Vec::new()),
    ]);
}

#[test]
fn context_is_refused_as_a_request_query_on_every_request() {
    let handle = handle();
    let context = format!("context={CONTEXT}");

    assert_refused(&[
        Request::get(format!("{INCLUDE}?{context}")),
        Request::get(format!("{INCLUDE}/source/main?version=active&{context}")),
        Request::get(format!("{INCLUDE}/source/main?{context}")),
        Request::get(format!("{INCLUDE}/source/main/versions?{context}")),
        Request::get(format!("{PROGRAM}?{context}")),
        Request::post(
            format!("{INCLUDE}?_action=LOCK&accessMode=MODIFY&{context}"),
            Vec::new(),
        ),
        Request::post(
            format!("{INCLUDE}?_action=UNLOCK&lockHandle={handle}&{context}"),
            Vec::new(),
        ),
        with_type(
            Request::put(
                format!("{INCLUDE}/source/main?lockHandle={handle}&{context}"),
                Vec::new(),
            ),
            "text/plain; charset=utf-8",
        ),
        any(Request::post(format!("{CHECKRUNS}&{context}"), Vec::new())),
        with_type(
            Request::post(format!("{ACTIVATION}&{context}"), Vec::new()),
            "application/xml",
        ),
        any(Request::post(
            format!("/sap/bc/adt/programs/includes?{context}"),
            include_body(),
        )),
        any(Request::post(
            format!("/sap/bc/adt/programs/programs?{context}"),
            program_body(),
        )),
    ]);
}

#[test]
fn a_context_that_is_not_a_program_uri_is_refused_in_a_body_uri() {
    let source = format!("{INCLUDE}/source/main");
    let bad = [
        // Not a `programs/programs/<p>` URI.
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fincludes%2Fzincl_b",
        "%2Fsap%2Fbc%2Fadt%2Foo%2Fclasses%2Fzcl_a",
        "%2Fsap%2Fbc%2Fadt%2Ffunctions%2Fgroups%2Fzfg",
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms",
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2F",
        // Deeper than the program, traversal, escaping tricks and extra keys.
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_a%2Fsource%2Fmain",
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2F..",
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2F%2e%2e",
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_a%26x%3D1",
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2Fzprog_a&x=1",
        "%252Fsap%252Fbc%252Fadt%252Fprograms%252Fprograms%252Fzprog_a",
        "%2fsap%2fbc%2fadt%2fprograms%2fprograms%2fzprog_a",
        "%2Fsap%2Fbc%2Fadt%2Fprograms%2Fprograms%2F%252fns%252fprog",
        "/sap/bc/adt/programs/programs/zprog_a",
        "zprog_a",
        "",
    ];

    for context in bad {
        let object = format!("{INCLUDE}?context={context}");

        assert_refused(&[checkrun(&object, &source), activation(&object)]);
    }
}

#[test]
fn context_outside_the_object_uri_of_the_body_is_refused() {
    let source = format!("{INCLUDE}/source/main");
    let contextual_source = format!("{source}?context={CONTEXT}");
    let good = context_include();
    let reference = |uri: &str, extra: &str| {
        format!(r#"<adtcore:objectReference adtcore:uri="{uri}" adtcore:name="ZINCL_A"{extra}/>"#)
    };
    let references = |inner: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">{inner}</adtcore:objectReferences>"#
        )
    };

    assert_refused(&[
        // On the artifact URI, alone or next to the object URI.
        checkrun(INCLUDE, &contextual_source),
        checkrun(&good, &contextual_source),
        // On a program, a class or a fragment of another URI.
        checkrun(&format!("{PROGRAM}?context={CONTEXT}"), &source),
        checkrun(
            &format!("/sap/bc/adt/oo/classes/zcl_a?context={CONTEXT}"),
            &source,
        ),
        checkrun(&format!("{good}#x"), &source),
        checkrun(&format!("{good}&x=1"), &source),
        checkrun(&format!("{INCLUDE}?x=1"), &source),
        checkrun(&format!("{INCLUDE}?CONTEXT={CONTEXT}"), &source),
        checkrun(&format!("{INCLUDE}%3Fcontext={CONTEXT}"), &source),
        // Activation: a program or a class with a context, a parent URI, a second reference.
        activation(&format!("{PROGRAM}?context={CONTEXT}")),
        activation(&format!("/sap/bc/adt/oo/classes/zcl_a?context={CONTEXT}")),
        activation_body(&references(&reference(
            &good,
            &format!(r#" adtcore:parentUri="{PROGRAM}""#),
        ))),
        activation_body(&references(&reference(
            INCLUDE,
            &format!(r#" adtcore:parentUri="{PROGRAM}?context={CONTEXT}""#),
        ))),
        activation_body(&references(&reference(
            PROGRAM,
            &format!(r#" adtcore:parentUri="{INCLUDE}?context={CONTEXT}""#),
        ))),
        activation_body(&references(&format!(
            "{}{}",
            reference(PROGRAM, ""),
            reference(&good, "")
        ))),
        activation_body(&references(&format!(
            "{}{}",
            reference(&good, ""),
            reference(&good, "")
        ))),
        // A body that mentions a context but cannot be read.
        checkrun_raw(b"<chkrun:checkObjectList context=".to_vec()),
        checkrun_raw(vec![0xff, b'c', b'o', b'n', b't', b'e', b'x', b't']),
    ]);
}

fn checkrun_raw(body: Vec<u8>) -> Request {
    any(Request::post(CHECKRUNS, body))
}

#[test]
fn a_checkrun_body_that_cannot_be_read_is_judged_as_before_by_the_word_context_alone() {
    // Neither malformed nor non-UTF-8, and no `context` in it: the M1 judgement stays admitted.
    assert_admitted(&[
        checkrun_raw(b"<chkrun:checkObjectList".to_vec()),
        checkrun_raw(vec![0xff, 0xfe]),
    ]);
    assert_refused(&[
        checkrun_raw(b"<chkrun:checkObjectList CONTEXT=".to_vec()),
        checkrun_raw(vec![0xff, b'C', b'o', b'n', b't', b'e', b'x', b't']),
    ]);
}

#[test]
fn an_include_is_never_activated_without_its_context() {
    assert_refused(&[
        activation(INCLUDE),
        activation(&format!("{INCLUDE}?")),
        activation(&format!("{INCLUDE}?context=")),
        activation_body(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:objectReference adtcore:uri="{INCLUDE}" adtcore:name="ZINCL_A" adtcore:parentUri="{PROGRAM}"/></adtcore:objectReferences>"#
        )),
    ]);
    // The control: the same include with its context passes, so the refusals above are the rule.
    assert_admitted(&[activation(&context_include())]);
}

#[test]
fn create_refuses_other_shapes_methods_content_types_and_bodies() {
    let program = "/sap/bc/adt/programs/programs";
    let includes = "/sap/bc/adt/programs/includes";
    let with_container = String::from_utf8(include_body()).unwrap().replace(
        "</include:abapInclude>",
        &format!(
            r#"<include:containerRef adtcore:name="ZPROG_A" adtcore:type="PROG/P" adtcore:uri="{PROGRAM}"/></include:abapInclude>"#
        ),
    );
    let swapped_type = String::from_utf8(include_body())
        .unwrap()
        .replace("PROG/I", "PROG/P");
    let class_body = String::from_utf8(program_body())
        .unwrap()
        .replace("program:abapProgram", "class:abapClass")
        .replace("xmlns:program", "xmlns:class");

    assert_refused(&[
        // Unlisted or malformed queries.
        any(Request::post(format!("{program}?x=1"), program_body())),
        any(Request::post(
            format!("{includes}?corrNr={TRANSPORTABLE}&x=1"),
            include_body(),
        )),
        any(Request::post(format!("{includes}?corrNr="), include_body())),
        any(Request::post(
            format!("{includes}?corrNr=A%20B"),
            include_body(),
        )),
        any(Request::post(
            format!("{program}?lockHandle=a"),
            program_body(),
        )),
        // Not a collection POST: members, siblings, trailing slash, traversal.
        any(Request::post(format!("{program}/"), program_body())),
        any(Request::post(format!("{program}/zprog_a"), program_body())),
        any(Request::post(format!("{includes}/"), include_body())),
        any(Request::post(
            "/sap/bc/adt/programs/includes/../programs",
            include_body(),
        )),
        any(Request::post("/sap/bc/adt/programs", program_body())),
        any(Request::post(
            "/sap/bc/adt/programs/freestyle",
            program_body(),
        )),
        // Content type.
        Request::post(program, program_body()),
        with_type(Request::post(includes, include_body()), "text/plain"),
        with_type(Request::post(includes, include_body()), "application/xml"),
        with_type(
            Request::post(program, program_body()),
            "application/*; charset=utf-8",
        ),
        // Methods.
        any(Request::put(program, program_body())),
        any(Request::new("PATCH", includes, include_body())),
        Request::get(program),
        Request::get(includes),
        Request::delete(program),
        Request::delete(includes),
        // Bodies outside the evidenced shape.
        any(Request::post(includes, with_container.into_bytes())),
        any(Request::post(includes, swapped_type.into_bytes())),
        any(Request::post(includes, program_body())),
        any(Request::post(program, include_body())),
        any(Request::post(program, class_body.into_bytes())),
        any(Request::post(program, Vec::new())),
        any(Request::post(includes, b"not xml".to_vec())),
        any(Request::post(includes, vec![0xff, 0xfe])),
    ]);
}

#[test]
fn validation_refuses_other_shapes_keys_and_methods() {
    let base = "/sap/bc/adt/programs/validation";
    let query = "objname=ZPROG_A&packagename=%24TMP&description=probe&objtype=PROG/P";

    // The control: the recorded form passes, so each variation below is the rule.
    assert_admitted(&[Request::post(format!("{base}?{query}"), Vec::new())]);
    assert_refused(&[
        // The M3 order and encoding are not the recorded form for programs.
        Request::post(
            format!("{base}?objtype=PROG%2FP&objname=ZPROG_A&description=probe&packagename=%24TMP"),
            Vec::new(),
        ),
        Request::post(
            format!("{base}?objname=ZPROG_A&packagename=%24TMP&description=probe&objtype=PROG%2FP"),
            Vec::new(),
        ),
        // Unknown object types, extra or missing or reordered keys, unencoded values.
        Request::post(
            format!("{base}?{}", query.replace("PROG/P", "PROG/X")),
            Vec::new(),
        ),
        Request::post(
            format!("{base}?{}", query.replace("PROG/P", "CLAS/OC")),
            Vec::new(),
        ),
        Request::post(format!("{base}?{query}&x=1"), Vec::new()),
        Request::post(format!("{base}?x=1&{query}"), Vec::new()),
        Request::post(
            format!("{base}?packagename=%24TMP&objname=ZPROG_A&description=probe&objtype=PROG/P"),
            Vec::new(),
        ),
        Request::post(
            format!("{base}?objname=&packagename=%24TMP&description=probe&objtype=PROG/P"),
            Vec::new(),
        ),
        Request::post(
            format!("{base}?objname=ZPROG_A&packagename=&description=probe&objtype=PROG/P"),
            Vec::new(),
        ),
        Request::post(
            format!("{base}?objname=ZPROG_A&packagename=$TMP&description=probe&objtype=PROG/P"),
            Vec::new(),
        ),
        Request::post(
            format!("{base}?objname=ZPROG_A&packagename=%24TMP&description=a b&objtype=PROG/P"),
            Vec::new(),
        ),
        Request::post(
            format!("{base}?objname=ZPROG_A&packagename=%24TMP&description=a&b&objtype=PROG/P"),
            Vec::new(),
        ),
        Request::post(base, Vec::new()),
        Request::post(format!("{base}?"), Vec::new()),
        Request::post(format!("{base}/?{query}"), Vec::new()),
        Request::post(format!("{base}2?{query}"), Vec::new()),
        // A body, or another method.
        Request::post(format!("{base}?{query}"), b"x".to_vec()),
        Request::get(format!("{base}?{query}")),
        Request::put(format!("{base}?{query}"), Vec::new()),
        Request::delete(format!("{base}?{query}")),
        // No other validation endpoint opens.
        Request::post(
            format!("/sap/bc/adt/programs/validation2?{query}"),
            Vec::new(),
        ),
        Request::post(format!("/sap/bc/adt/ddic/validation?{query}"), Vec::new()),
        Request::post(
            format!("/sap/bc/adt/packages/validation?{query}"),
            Vec::new(),
        ),
        Request::post(format!("/sap/bc/adt/validation?{query}"), Vec::new()),
    ]);
}

#[test]
fn other_methods_and_content_types_on_the_include_shapes_are_refused() {
    let handle = handle();
    let put = format!("{INCLUDE}/source/main?lockHandle={handle}");
    let context = context_include();
    let source = format!("{INCLUDE}/source/main");

    assert_refused(&[
        // A source PUT needs its lock handle and the text content type.
        with_type(
            Request::put(format!("{INCLUDE}/source/main"), Vec::new()),
            "text/plain; charset=utf-8",
        ),
        Request::put(put.clone(), Vec::new()),
        with_type(Request::put(put.clone(), Vec::new()), "text/plain"),
        with_type(Request::put(put.clone(), Vec::new()), "application/xml"),
        with_type(
            Request::put(format!("{put}&corrNr="), Vec::new()),
            "text/plain; charset=utf-8",
        ),
        with_type(
            Request::put(format!("{INCLUDE}?lockHandle={handle}"), Vec::new()),
            "text/plain; charset=utf-8",
        ),
        with_type(
            Request::put(format!("{INCLUDE}/source/main/versions"), Vec::new()),
            "text/plain; charset=utf-8",
        ),
        // The wrong content type of a checkrun and an activation.
        with_type(
            Request::post(
                CHECKRUNS,
                checkrun_xml_for(&context, &source, &[]).into_bytes(),
            ),
            "application/xml",
        ),
        with_type(
            Request::post(
                ACTIVATION,
                activation_request_xml("zincl_a", &context).into_bytes(),
            ),
            "text/plain",
        ),
        Request::post(
            ACTIVATION,
            activation_request_xml("zincl_a", &context).into_bytes(),
        ),
        // Other methods on the include.
        Request::new(
            "PATCH",
            format!("{INCLUDE}/source/main?version=active"),
            Vec::new(),
        ),
        Request::new("HEAD", INCLUDE, Vec::new()),
        Request::put(INCLUDE, Vec::new()),
        Request::post(INCLUDE, Vec::new()),
        Request::post(format!("{INCLUDE}/source/main/versions"), Vec::new()),
    ]);
}

#[test]
fn an_include_delete_is_admitted_in_the_program_delete_shape_only() {
    let handle = handle();

    assert_admitted(&[
        Request::delete(format!("{INCLUDE}?lockHandle={handle}")),
        Request::delete(format!(
            "{INCLUDE}?lockHandle={handle}&corrNr={TRANSPORTABLE}"
        )),
        Request::delete(format!("{PROGRAM}?lockHandle={handle}")),
        Request::delete(format!(
            "{PROGRAM}?lockHandle={handle}&corrNr={TRANSPORTABLE}"
        )),
    ]);

    let mut with_body = Request::delete(format!("{INCLUDE}?lockHandle={handle}"));
    with_body.body = b"x".to_vec();

    assert_refused(&[
        with_body,
        Request::delete(format!("{INCLUDE}/source/main?lockHandle={handle}")),
        Request::delete(format!("{INCLUDE}/mainprograms?lockHandle={handle}")),
        Request::delete(INCLUDE),
        Request::delete(format!("{INCLUDE}?context=x")),
        Request::delete(format!("{INCLUDE}?context={CONTEXT}")),
        Request::delete(format!("{INCLUDE}?lockHandle={handle}&context=x")),
        Request::delete(format!("{INCLUDE}?lockHandle={handle}&x=1")),
        Request::delete(format!("{INCLUDE}?lockHandle={handle}&corrNr=")),
        Request::delete(format!("{INCLUDE}?corrNr={TRANSPORTABLE}")),
        Request::delete(format!("{INCLUDE}?lockHandle={handle}#frag")),
        Request::delete(format!("/sap/bc/adt/programs/includes?lockHandle={handle}")),
        Request::delete(format!(
            "/sap/bc/adt/programs/includes/zincl%2fa?lockHandle={handle}"
        )),
        Request::delete(format!(
            "/sap/bc/adt/programs/includes/zincl-a?lockHandle={handle}"
        )),
        Request::delete(format!(
            "/sap/bc/adt/programs/includes/..?lockHandle={handle}"
        )),
    ]);
}

fn usage(uri: &str) -> String {
    abapreach::adt::usage_references_uri(uri)
}

fn usage_post(uri: &str) -> Request {
    Request::post(
        usage(uri),
        abapreach::adt::USAGE_REQUEST_XML.as_bytes().to_vec(),
    )
}

#[test]
fn a_where_used_request_on_a_program_or_include_is_admitted_in_the_emitted_shape_only() {
    assert_admitted(&[usage_post(PROGRAM), usage_post(INCLUDE)]);
    assert_eq!(
        usage(INCLUDE),
        format!("/sap/bc/adt/repository/informationsystem/usageReferences?uri={INCLUDE}")
    );

    let base = "/sap/bc/adt/repository/informationsystem/usageReferences?uri=";
    let other_body = |uri: String, body: &[u8]| Request::post(uri, body.to_vec());
    let mut wrong_xml = abapreach::adt::USAGE_REQUEST_XML.to_owned();
    wrong_xml.push(' ');

    assert_refused(&[
        // Body: empty, other, or the right one with a trailing byte.
        other_body(usage(PROGRAM), b""),
        other_body(usage(INCLUDE), b"<x/>"),
        other_body(usage(INCLUDE), wrong_xml.as_bytes()),
        // Encoded slashes, either case, and encoded characters of the name.
        usage_post("/sap/bc/adt/programs/includes/zincl%2fa"),
        Request::post(
            format!("{base}%2Fsap%2Fbc%2Fadt%2Fprograms%2Fincludes%2Fzincl_a"),
            abapreach::adt::USAGE_REQUEST_XML.as_bytes().to_vec(),
        ),
        Request::post(
            format!("{base}%2fsap%2fbc%2fadt%2fprograms%2fprograms%2fzprog_a"),
            abapreach::adt::USAGE_REQUEST_XML.as_bytes().to_vec(),
        ),
        Request::post(
            format!("{base}/sap/bc/adt/programs/programs/zprog%2fa"),
            abapreach::adt::USAGE_REQUEST_XML.as_bytes().to_vec(),
        ),
        // Names the M1 program rule allows but the plain rule does not.
        usage_post("/sap/bc/adt/programs/programs/zprog$a"),
        usage_post("/sap/bc/adt/programs/programs/zprog-a"),
        usage_post("/sap/bc/adt/programs/programs/zprog.a"),
        usage_post("/sap/bc/adt/programs/programs/."),
        usage_post("/sap/bc/adt/programs/programs/.."),
        usage_post("/sap/bc/adt/programs/includes/zincl$a"),
        usage_post("/sap/bc/adt/programs/includes/zincl-a"),
        usage_post("/sap/bc/adt/programs/includes/zincl.a"),
        usage_post("/sap/bc/adt/programs/includes/."),
        usage_post("/sap/bc/adt/programs/includes/.."),
        usage_post("/sap/bc/adt/programs/includes/"),
        usage_post("/sap/bc/adt/programs/programs/"),
        // A URI part, a context, another collection or kind.
        usage_post(&format!("{INCLUDE}/source/main")),
        usage_post(&format!("{PROGRAM}/source/main")),
        usage_post(&format!("{INCLUDE}/mainprograms")),
        usage_post(&context_include()),
        usage_post("/sap/bc/adt/programs/includes"),
        usage_post("/sap/bc/adt/oo/classes/zcl_a"),
        usage_post("/sap/bc/adt/oo/interfaces/zif_a"),
        usage_post("/sap/bc/adt/functions/groups/zfg"),
        // Extra or misplaced query.
        Request::post(
            format!("{}&x=1", usage(INCLUDE)),
            abapreach::adt::USAGE_REQUEST_XML.as_bytes().to_vec(),
        ),
        Request::post(
            format!("{}?context={CONTEXT}", usage(INCLUDE)),
            abapreach::adt::USAGE_REQUEST_XML.as_bytes().to_vec(),
        ),
        Request::post(
            format!("{}#frag", usage(INCLUDE)),
            abapreach::adt::USAGE_REQUEST_XML.as_bytes().to_vec(),
        ),
        Request::get(usage(INCLUDE)),
    ]);
}
