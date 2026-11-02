use abapreach::transport::rfc::{RFC_TRACE_DIR_ENV, after_trace_environment, trace_directory};

/// One test in its own binary: it mutates the process environment, which nothing else may race.
#[test]
fn the_trace_environment_is_set_before_the_signal_handler_installer_runs() {
    let seen = after_trace_environment(|| std::env::var_os(RFC_TRACE_DIR_ENV));

    assert_eq!(seen, Some(trace_directory().into_os_string()));
}
