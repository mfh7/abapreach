#[test]
fn package_reports_the_milestone_three_version() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.13.0");
}
