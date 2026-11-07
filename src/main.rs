use abapreach::operation::deploy::{CancelFlag, INTERRUPT_EXIT_CODE, InterruptAction};
use abapreach::transport::http::end_open_sessions;
use abapreach::transport::rfc::after_trace_environment;
use abapreach::{RealFactories, run};
use std::collections::BTreeMap;

fn main() {
    let cancel = CancelFlag::new();
    let on_interrupt = cancel.clone();

    // A held lock must be released before the process ends, so the deploy gets the flag. Without
    // a lock there is nothing to clean up and an interrupt keeps its usual meaning: exit 130.
    // `termination` makes SIGTERM and SIGHUP (a dropped ssh session) take the same path on Unix.
    // On Windows the handler also runs for CTRL_BREAK, but the OS ends the process for
    // CTRL_CLOSE, CTRL_LOGOFF and CTRL_SHUTDOWN as soon as the handler returns, so a held lock may
    // stay locked there.
    // The trace environment goes first: `ctrlc` starts a thread, after which `set_var` is unsound.
    after_trace_environment(|| {
        let _ = ctrlc::set_handler(move || {
            if on_interrupt.on_interrupt() == InterruptAction::ExitNow {
                // No lock is held, but an open ADT session still keeps its context on SAP.
                end_open_sessions();
                std::process::exit(INTERRUPT_EXIT_CODE);
            }
        });
    });

    let env = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let exit = run(
        std::env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        env,
        &mut RealFactories::new(cancel),
    );

    if !exit.stdout.is_empty() {
        println!("{}", exit.stdout);
    }

    if !exit.stderr.is_empty() {
        eprintln!("{}", exit.stderr);
    }

    std::process::exit(exit.code);
}
