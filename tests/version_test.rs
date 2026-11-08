#[test]
fn package_reports_the_milestone_six_version() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.16.0");
}
