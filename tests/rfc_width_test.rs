//! The RFC marshalling in both character sizes, against a fake RFC library built per test: a
//! UTF-16 one like the NW RFC SDK, and a single-byte one like the SAP GUI 8.00 for Windows library,
//! which reads every string argument as `char*`.

use abapreach::transport::TransportError;
use abapreach::transport::rfc::{
    DynamicRfcApi, Env, RfcApi, RfcEndpointRequest, RfcLibrary, TraceTarget, UcWidth,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Source of the fake. `WIDE` picks the character size; every function reads its string arguments
/// in that size and fails with `RFC_INVALID_PARAMETER` (20), as the real single-byte library does
/// for a string it misreads, when one is not what the SADT call must send.
const FAKE_SOURCE: &str = r#"
use std::os::raw::{c_int, c_uint, c_void};

static BODY: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

unsafe fn read(pointer: *const c_void) -> String {
    if WIDE {
        let units = pointer as *const u16;
        let mut text = Vec::new();
        while *units.add(text.len()) != 0 {
            text.push(*units.add(text.len()));
        }
        String::from_utf16_lossy(&text)
    } else {
        std::ffi::CStr::from_ptr(pointer as *const _).to_string_lossy().into_owned()
    }
}

unsafe fn write(buffer: *mut c_void, text: &str) {
    if WIDE {
        let units = buffer as *mut u16;
        for (index, unit) in text.encode_utf16().chain(Some(0)).enumerate() {
            *units.add(index) = unit;
        }
    } else {
        let bytes = buffer as *mut u8;
        for (index, byte) in text.bytes().chain(Some(0)).enumerate() {
            *bytes.add(index) = byte;
        }
    }
}

unsafe fn fail(info: *mut c_void, code: c_int, message: &str) -> c_int {
    *(info as *mut c_int) = code;
    let offset = if WIDE { 264 } else { 136 };
    write((info as *mut u8).add(offset) as *mut c_void, message);
    code
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetVersion(a: *mut c_uint, b: *mut c_uint, c: *mut c_uint) -> *const c_void {
    *a = 7;
    *b = 53;
    *c = 1524;
    if WIDE {
        static TEXT: [u16; 4] = [55, 53, 51, 0];
        TEXT.as_ptr() as *const c_void
    } else {
        b"753\0".as_ptr() as *const c_void
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcSetTraceDir(directory: *const c_void, info: *mut c_void) -> c_int {
    // A string read in the wrong size is a single character long.
    if read(directory).len() > 1 { 0 } else { fail(info, 20, "bad trace dir") }
}

#[no_mangle]
pub unsafe extern "C" fn RfcOpenConnection(
    parameters: *const [*const c_void; 2],
    count: c_uint,
    info: *mut c_void,
) -> *mut c_void {
    let mut host = None;
    for index in 0..count as usize {
        let pair = &*parameters.add(index);
        if read(pair[0]) == "ASHOST" {
            host = Some(read(pair[1]));
        }
    }
    match host.as_deref() {
        Some("good.example.com") => 1 as *mut c_void,
        Some(_) => { fail(info, 1, "partner not reached"); std::ptr::null_mut() }
        None => { fail(info, 20, "no ASHOST"); std::ptr::null_mut() }
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcPing(_: *mut c_void, _: *mut c_void) -> c_int { 0 }

#[no_mangle]
pub unsafe extern "C" fn RfcGetFunctionDesc(_: *mut c_void, name: *const c_void, info: *mut c_void) -> *mut c_void {
    if read(name) == "SADT_REST_RFC_ENDPOINT" { 2 as *mut c_void } else { fail(info, 20, "bad function"); std::ptr::null_mut() }
}

#[no_mangle]
pub unsafe extern "C" fn RfcCreateFunction(_: *mut c_void, _: *mut c_void) -> *mut c_void { 3 as *mut c_void }

#[no_mangle]
pub unsafe extern "C" fn RfcGetStructure(_: *mut c_void, name: *const c_void, out: *mut *mut c_void, info: *mut c_void) -> c_int {
    match read(name).as_str() {
        "REQUEST" | "REQUEST_LINE" | "RESPONSE" | "STATUS_LINE" => { *out = 4 as *mut c_void; 0 }
        _ => fail(info, 20, "bad structure"),
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetTable(_: *mut c_void, name: *const c_void, out: *mut *mut c_void, info: *mut c_void) -> c_int {
    if read(name) == "HEADER_FIELDS" { *out = 5 as *mut c_void; 0 } else { fail(info, 20, "bad table") }
}

#[no_mangle]
pub unsafe extern "C" fn RfcAppendNewRow(_: *mut c_void, _: *mut c_void) -> *mut c_void { 6 as *mut c_void }

#[no_mangle]
pub unsafe extern "C" fn RfcSetString(_: *mut c_void, name: *const c_void, value: *const c_void, length: c_uint, info: *mut c_void) -> c_int {
    let (name, value) = (read(name), read(value));
    // The length counts characters, not bytes.
    if length as usize != value.encode_utf16().count() { return fail(info, 20, "bad length"); }
    match (name.as_str(), value.as_str()) {
        ("METHOD", "GET") | ("URI", "/sap/bc/adt/discovery") | ("VERSION", "HTTP/1.1") | ("NAME", "Accept") | ("VALUE", "text/plain") => 0,
        _ => fail(info, 20, "bad field"),
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcSetXString(_: *mut c_void, name: *const c_void, value: *const c_void, length: c_uint, info: *mut c_void) -> c_int {
    if read(name) != "MESSAGE_BODY" { return fail(info, 20, "bad xstring"); }
    // The bytes are kept to be handed back: the length counts bytes, whatever the width.
    *BODY.lock().unwrap() = std::slice::from_raw_parts(value as *const u8, length as usize).to_vec();
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcInvoke(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> c_int { 0 }

#[no_mangle]
pub unsafe extern "C" fn RfcGetStringLength(_: *mut c_void, name: *const c_void, length: *mut c_uint, info: *mut c_void) -> c_int {
    match read(name).as_str() {
        "STATUS_CODE" => { *length = 3; 0 }
        "MESSAGE_BODY" => { *length = BODY.lock().unwrap().len().max(2) as c_uint; 0 }
        _ => fail(info, 20, "bad length name"),
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetString(_: *mut c_void, name: *const c_void, buffer: *mut c_void, size: c_uint, written: *mut c_uint, info: *mut c_void) -> c_int {
    // Room for three characters and the terminator.
    if read(name) != "STATUS_CODE" || size < 4 { return fail(info, 20, "bad get string"); }
    write(buffer, "200");
    *written = 3;
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetXString(_: *mut c_void, name: *const c_void, buffer: *mut c_void, size: c_uint, written: *mut c_uint, info: *mut c_void) -> c_int {
    if read(name) != "MESSAGE_BODY" { return fail(info, 20, "bad get xstring"); }
    let kept = BODY.lock().unwrap().clone();
    // A body that was set comes back unchanged; otherwise the answer is "ok".
    let bytes = if kept.is_empty() { b"ok".to_vec() } else { kept };
    if (size as usize) < bytes.len() { return fail(info, 20, "bad get xstring size"); }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
    *written = bytes.len() as c_uint;
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcDestroyFunction(_: *mut c_void, _: *mut c_void) -> c_int { 0 }

#[no_mangle]
pub unsafe extern "C" fn RfcCloseConnection(_: *mut c_void, _: *mut c_void) -> c_int { 0 }
"#;

/// Builds the fake library with the same toolchain that builds the tests.
fn fake_library(directory: &Path, wide: bool) -> PathBuf {
    let source = directory.join("fake.rs");
    let library = directory.join(format!(
        "fake_rfc_{}.{}",
        if wide { "wide" } else { "narrow" },
        std::env::consts::DLL_EXTENSION
    ));

    std::fs::write(
        &source,
        format!("const WIDE: bool = {wide};\n{FAKE_SOURCE}"),
    )
    .unwrap();

    let status = std::process::Command::new(std::env::var_os("RUSTC").unwrap_or("rustc".into()))
        .args(["--crate-type", "cdylib", "--crate-name", "fake_rfc"])
        .arg("-o")
        .arg(&library)
        .arg(&source)
        .status()
        .expect("rustc must be on the PATH to build the fake RFC library");

    assert!(
        status.success(),
        "rustc could not build the fake RFC library"
    );

    library
}

fn parameters(host: &str) -> Vec<(String, String)> {
    [
        ("ASHOST", host),
        ("SYSNR", "00"),
        ("CLIENT", "100"),
        ("USER", "user"),
        ("PASSWD", "secret"),
        ("LANG", "EN"),
    ]
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .to_vec()
}

fn request() -> RfcEndpointRequest {
    RfcEndpointRequest {
        function: "SADT_REST_RFC_ENDPOINT",
        method: "GET".to_owned(),
        uri: "/sap/bc/adt/discovery".to_owned(),
        version: "HTTP/1.1".to_owned(),
        headers: BTreeMap::from([("Accept".to_owned(), "text/plain".to_owned())]),
        body: Vec::new(),
    }
}

fn load(directory: &Path, wide: bool) -> RfcLibrary {
    let library = fake_library(directory, wide);

    RfcLibrary::load(&Env::test_paths(library.to_str().unwrap(), "", ""))
        .unwrap_or_else(|error| panic!("the fake must load: {error}"))
}

#[test]
fn the_width_is_detected_per_library_before_any_string_is_passed() {
    let directory = tempfile::tempdir().unwrap();

    assert_eq!(load(directory.path(), true).width(), UcWidth::Utf16);
    assert_eq!(load(directory.path(), false).width(), UcWidth::Narrow);
}

#[test]
fn the_trace_directory_is_set_in_either_width() {
    let directory = tempfile::tempdir().unwrap();

    for wide in [true, false] {
        // The fake accepts the call only when it reads more than one character.
        assert_eq!(
            load(directory.path(), wide).trace_target(),
            TraceTarget::Symbol,
            "wide: {wide}"
        );
    }
}

#[test]
fn a_connection_opens_and_a_request_round_trips_in_either_width() {
    let directory = tempfile::tempdir().unwrap();

    for wide in [true, false] {
        let library = load(directory.path(), wide);
        let mut api = DynamicRfcApi::load(library, parameters("good.example.com")).unwrap();

        api.ping()
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        let response = api
            .invoke(request())
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        assert_eq!(response.status, 200, "wide: {wide}");
        assert_eq!(response.body, b"ok", "wide: {wide}");
        api.close();
    }
}

#[test]
fn an_error_text_is_read_in_the_width_of_the_library() {
    let directory = tempfile::tempdir().unwrap();

    for wide in [true, false] {
        let library = load(directory.path(), wide);
        let mut api = DynamicRfcApi::load(library, parameters("down.example.com")).unwrap();
        let error = api.ping().unwrap_err();

        assert_eq!(
            error,
            TransportError::PreSendConnectivity(
                "RfcOpenConnection: partner not reached (RFC code 1)".to_owned()
            ),
            "wide: {wide}"
        );
    }
}

#[test]
fn a_single_byte_library_refuses_a_parameter_it_cannot_hold_without_printing_the_value() {
    let directory = tempfile::tempdir().unwrap();
    let mut with_umlaut = parameters("good.example.com");

    with_umlaut[4].1 = "pässword".to_owned();

    let narrow = DynamicRfcApi::load(load(directory.path(), false), with_umlaut.clone());
    let error = narrow
        .err()
        .expect("a single-byte library cannot hold the password");

    assert_eq!(
        error,
        "RFC connection parameter PASSWD: a single-byte RFC library accepts ASCII only; the text \
         contains other characters; use the NW RFC SDK library \
         (ABAPREACH_RFC_LIBRARY=...\\sapnwrfc.dll) instead"
    );
    assert!(!error.contains("pässword"));
    // UTF-16 holds it.
    assert!(DynamicRfcApi::load(load(directory.path(), true), with_umlaut).is_ok());
}

#[test]
fn a_single_byte_library_refuses_a_request_text_it_cannot_hold() {
    let directory = tempfile::tempdir().unwrap();
    let mut api = DynamicRfcApi::load(
        load(directory.path(), false),
        parameters("good.example.com"),
    )
    .unwrap();
    let mut message = request();

    message.uri = "/sap/bc/adt/programs/ZÄ".to_owned();

    let error = api.invoke(message).unwrap_err();

    assert!(
        matches!(&error, TransportError::Protocol(text)
            if text.contains("a single-byte RFC library accepts ASCII only")),
        "{error:?}"
    );
}

#[test]
fn a_non_ascii_body_round_trips_byte_exact_through_the_single_byte_library() {
    // The ABAP source travels as XSTRING bytes, so only the request metadata has to be ASCII.
    let directory = tempfile::tempdir().unwrap();

    for wide in [true, false] {
        let library = load(directory.path(), wide);
        let mut api = DynamicRfcApi::load(library, parameters("good.example.com")).unwrap();
        let mut message = request();

        message.body = "ä".as_bytes().to_vec();

        let response = api
            .invoke(message)
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        assert_eq!(response.body, [0xc3, 0xa4], "wide: {wide}");
        api.close();
    }
}
