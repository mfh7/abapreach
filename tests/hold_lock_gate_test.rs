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

#[test]
fn the_skip_activate_stop_is_compiled_only_with_the_fault_injection_feature() {
    let deploy = include_str!("../src/operation/deploy.rs").replace("\r\n", "\n");
    let call = deploy
        .find("if crate::fault::skip_activate() {")
        .expect("call site");

    assert!(
        deploy[..call]
            .trim_end()
            .ends_with("#[cfg(feature = \"fault-injection\")]")
    );
    // The stop comes after the UNLOCK and before the activation.
    assert!(deploy[..call].rfind("self.unlock(&lock)").is_some());
    assert!(deploy[call..].find(".activate(&input.name").is_some());
}

#[test]
fn the_hold_post_pause_is_compiled_only_with_the_fault_injection_feature() {
    let runner = include_str!("../src/operation/runner.rs").replace("\r\n", "\n");
    let post = &runner[runner.find("fn post(").expect("fn post")..];
    let call = post
        .find("crate::fault::hold_post(self.cancel);")
        .expect("call site");

    assert!(
        post[..call]
            .trim_end()
            .ends_with("#[cfg(feature = \"fault-injection\")]")
    );
    // The pause follows the classrun POST and precedes the mapping of its outcome.
    let request = post.find("session.request(request)").expect("POST request");
    let outcome = call + post[call..].find("match result {").expect("outcome match");

    assert!(request < call, "the pause comes after the POST");
    assert!(call < outcome, "the pause comes before the outcome match");
}
