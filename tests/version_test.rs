#[test]
fn package_reports_the_milestone_four_version() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.14.0");
}
