use abapreach::transport::rfc::{
    Env, RFC_TRACE_DIR_ENV, RfcLibrary, TraceTarget, apply_trace_directory, from_sap_uc, sap_uc,
    trace_directory, trace_environment,
};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

/// The trace directory the fake `RfcSetTraceDir` was called with.
static TRACE_CALLS: Mutex<Option<String>> = Mutex::new(None);

unsafe extern "C" fn record_trace_dir(directory: *const u16, _: *mut c_void) -> i32 {
    let mut units = Vec::new();

    // The pointer is NUL-terminated UTF-16 that stays alive for the duration of the call.
    unsafe {
        while *directory.add(units.len()) != 0 {
            units.push(*directory.add(units.len()));
        }
    }

    *TRACE_CALLS.lock().unwrap() = Some(from_sap_uc(&units).unwrap());

    0
}

#[test]
fn candidate_paths_preserve_override_gui_sdk_order() {
    let env = Env::test_paths("/override", "/gui", "/sdk");

    assert_eq!(
        RfcLibrary::candidate_paths(&env),
        vec![
            PathBuf::from("/override"),
            PathBuf::from("/gui"),
            PathBuf::from("/sdk"),
        ]
    );
}

#[test]
fn sap_uc_is_nul_terminated_utf16_and_rejects_invalid_units() {
    let units = sap_uc("Aé");

    assert_eq!(units, vec![65, 233, 0]);
    assert_eq!(from_sap_uc(&units).unwrap(), "Aé");
    assert!(from_sap_uc(&[0xd800, 0]).is_err());
}

#[test]
fn the_trace_goes_to_the_temp_dir_by_symbol_or_by_environment() {
    let temp = std::env::temp_dir();

    assert_eq!(RFC_TRACE_DIR_ENV, "RFC_TRACE_DIR");
    assert_eq!(trace_directory(), temp);
    assert_eq!(
        trace_environment(),
        (RFC_TRACE_DIR_ENV, temp.display().to_string())
    );

    assert_eq!(apply_trace_directory(None), TraceTarget::Environment);
    assert_eq!(
        apply_trace_directory(Some(record_trace_dir)),
        TraceTarget::Symbol
    );
    assert_eq!(
        TRACE_CALLS.lock().unwrap().clone(),
        Some(temp.display().to_string())
    );
}
