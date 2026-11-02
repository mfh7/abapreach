//! Windows only: the trace variable must reach a C library that reads the environment through the
//! shared UCRT (`ucrtbase.dll`), as both SAP DLLs do. The reader is a separate DLL built here with
//! a dynamic CRT, so the check never runs through the CRT of this test executable: a dynamic-CRT
//! test binary shares `ucrtbase.dll` with it, a static-CRT one (the release build) does not, and
//! the variable must arrive in both. One test in its own binary because it changes the process
//! environment.
#![cfg(windows)]

use abapreach::transport::rfc::{RFC_TRACE_DIR_ENV, init_trace_environment, trace_directory};
use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;

/// What a SAP library does on its first call: `getenv`, answered from its UCRT's own copy.
const READER_SOURCE: &str = r#"
use std::os::raw::c_char;

extern "C" {
    fn getenv(name: *const c_char) -> *const c_char;
}

#[no_mangle]
pub unsafe extern "C" fn read_env(name: *const c_char) -> *const c_char {
    getenv(name)
}
"#;

fn reader_library(directory: &std::path::Path) -> PathBuf {
    let source = directory.join("reader.rs");
    let library = directory.join("env_reader.dll");

    std::fs::write(&source, READER_SOURCE).unwrap();

    let status = std::process::Command::new(std::env::var_os("RUSTC").unwrap_or("rustc".into()))
        .args(["--crate-type", "cdylib", "--crate-name", "env_reader"])
        .args(["-C", "target-feature=-crt-static"])
        .arg("-o")
        .arg(&library)
        .arg(&source)
        .status()
        .expect("rustc must be on the PATH to build the environment reader");

    assert!(status.success(), "rustc could not build the reader");

    library
}

#[test]
fn the_trace_variable_is_visible_to_a_library_that_reads_the_ucrt_environment() {
    let directory = tempfile::tempdir().unwrap();
    let library = reader_library(directory.path());

    // As in `main`: the variable is set first, the library is mapped afterwards.
    init_trace_environment();

    let library = unsafe { libloading::Library::new(&library) }.expect("the reader must load");
    let read_env = unsafe {
        library.get::<unsafe extern "C" fn(*const c_char) -> *const c_char>(b"read_env\0")
    }
    .expect("the reader exports read_env");
    let name = CString::new(RFC_TRACE_DIR_ENV).unwrap();
    let value = unsafe { read_env(name.as_ptr()) };

    assert!(
        !value.is_null(),
        "a UCRT library does not see {RFC_TRACE_DIR_ENV}"
    );

    let seen = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();

    assert_eq!(seen, trace_directory().display().to_string());
}
