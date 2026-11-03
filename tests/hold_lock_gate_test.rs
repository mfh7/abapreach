//! The `hold-lock` pause exists only in fault-injection builds: its call site and module are gated.

#[test]
fn the_hold_lock_call_is_compiled_only_with_the_fault_injection_feature() {
    let deploy = include_str!("../src/operation/deploy.rs");
    let call = deploy.find("crate::fault::hold_lock(").expect("call site");
    let before = &deploy[..call];
    assert!(
        before
            .trim_end()
            .ends_with("#[cfg(feature = \"fault-injection\")]")
    );

    let lib = include_str!("../src/lib.rs");
    assert!(lib.contains("#[cfg(feature = \"fault-injection\")]\npub mod fault;"));
}
