use abapreach::domain::Status;

#[test]
fn exits() {
    assert_eq!((Status::Active.exit(), Status::SimulatedOk.exit()), (0, 0));
    assert_eq!(
        (
            Status::Inactive.exit(),
            Status::Rejected.exit(),
            Status::Partial.exit(),
        ),
        (1, 1, 1),
    );
}
