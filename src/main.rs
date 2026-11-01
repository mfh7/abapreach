use abapreach::operation::deploy::{CancelFlag, INTERRUPT_EXIT_CODE, InterruptAction};
use abapreach::{RealFactories, run};
use std::collections::BTreeMap;

fn main() {
    let cancel = CancelFlag::new();
    let on_interrupt = cancel.clone();

    // A held lock must be released before the process ends, so the deploy gets the flag. Without
    // a lock there is nothing to clean up and Ctrl-C keeps its usual meaning.
    let _ = ctrlc::set_handler(move || {
        if on_interrupt.on_interrupt() == InterruptAction::ExitNow {
            std::process::exit(INTERRUPT_EXIT_CODE);
        }
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
