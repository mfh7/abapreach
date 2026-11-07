#[test]
fn package_reports_the_milestone_five_version() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.15.0");
}
