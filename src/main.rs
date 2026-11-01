use abapreach::operation::deploy::CancelFlag;
use abapreach::{RealFactories, run};
use std::collections::BTreeMap;

fn main() {
    let cancel = CancelFlag::new();
    let on_interrupt = cancel.clone();

    // The deploy checks the flag while it holds the lock and then unlocks before it ends.
    let _ = ctrlc::set_handler(move || on_interrupt.set());

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
