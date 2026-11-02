#[test]
fn package_reports_the_milestone_two_version() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.12.0");
}
