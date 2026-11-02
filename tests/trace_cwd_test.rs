use abapreach::transport::rfc::{
    Env, RfcLibrary, TraceTarget, UcWidth, apply_trace_directory, init_trace_environment,
};
use std::ffi::c_void;

unsafe extern "C" fn trace_ok(_: *const c_void, _: *mut c_void) -> i32 {
    0
}

unsafe extern "C" fn trace_error(_: *const c_void, _: *mut c_void) -> i32 {
    20
}

fn dev_rfc_files() -> Vec<String> {
    std::fs::read_dir(".")
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("dev_rfc"))
        .collect()
}

/// One test in its own binary: it changes the working directory and the environment of the whole
/// process. It proves the three `RfcSetTraceDir` cases leave the cwd clean, and, where this
/// machine has an RFC library, that loading the real one does too.
#[test]
fn no_dev_rfc_file_appears_in_the_working_directory() {
    let directory = tempfile::tempdir().unwrap();

    std::env::set_current_dir(directory.path()).unwrap();
    init_trace_environment();

    assert_eq!(
        apply_trace_directory(Some(trace_ok), UcWidth::Utf16),
        TraceTarget::Symbol
    );
    assert_eq!(
        apply_trace_directory(None, UcWidth::Utf16),
        TraceTarget::Environment
    );
    assert_eq!(
        apply_trace_directory(Some(trace_error), UcWidth::Narrow),
        TraceTarget::Rejected(20)
    );
    assert_eq!(dev_rfc_files(), Vec::<String>::new());

    match RfcLibrary::load(&Env::current()) {
        Ok(library) => {
            let target = library.trace_target();

            eprintln!("RfcSetTraceDir on {}: {target:?}", library.path().display());
            library.identity().unwrap();

            assert!(
                matches!(target, TraceTarget::Symbol | TraceTarget::Environment),
                "RfcSetTraceDir returned an error: {target:?}"
            );
            assert_eq!(dev_rfc_files(), Vec::<String>::new());
        }
        Err(_) => eprintln!("skipped: no loadable RFC library on this machine"),
    }
}
