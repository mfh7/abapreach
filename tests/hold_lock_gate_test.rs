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
    let start = runner.find("fn post(").expect("fn post");
    let end = runner[start..]
        .find("\n    fn ")
        .map_or(runner.len(), |offset| start + offset);
    let post = &runner[start..end];
    let call = post
        .find("crate::fault::hold_post(self.cancel);")
        .expect("call site");
    let mark = post
        .find("crate::fault::mark_post();")
        .expect("marker call site");

    assert!(
        post[..call]
            .trim_end()
            .ends_with("#[cfg(feature = \"fault-injection\")]")
    );
    assert!(
        post[..mark]
            .trim_end()
            .ends_with("#[cfg(feature = \"fault-injection\")]")
    );
    // The pause follows the classrun POST and precedes the mapping of its outcome; the marker
    // follows the run guard and precedes the POST request.
    let running = post.find("hold_run_leaving").expect("run guard");
    let request = post.find("session.request(request)").expect("POST request");
    let outcome = call + post[call..].find("match result {").expect("outcome match");

    assert!(running < mark, "the marker comes after the run guard");
    assert!(mark < request, "the marker comes before the POST");
    assert!(request < call, "the pause comes after the POST");
    assert!(call < outcome, "the pause comes before the outcome match");
}
