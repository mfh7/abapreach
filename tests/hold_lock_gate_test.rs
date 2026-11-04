//! The `hold-lock` pause exists only in fault-injection builds: its call site and module are gated.

#[test]
fn the_hold_lock_call_is_compiled_only_with_the_fault_injection_feature() {
    // Windows checkouts use CRLF, so line endings are normalized before matching.
    let deploy = include_str!("../src/operation/deploy.rs").replace("\r\n", "\n");
    let call = deploy.find("crate::fault::hold_lock(").expect("call site");
    let before = &deploy[..call];
    assert!(
        before
            .trim_end()
            .ends_with("#[cfg(feature = \"fault-injection\")]")
    );

    let lib = include_str!("../src/lib.rs").replace("\r\n", "\n");
    assert!(lib.contains("#[cfg(feature = \"fault-injection\")]\npub mod fault;"));
}
