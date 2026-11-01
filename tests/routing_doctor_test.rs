mod common;

use abapreach::backend::{BackendKind, Failure, OperationKind, Router};
use abapreach::transport::TransportError;
use common::Fixture;
use std::fs;

const READ: OperationKind = OperationKind::ReadProg;
const DEPLOY: OperationKind = OperationKind::DeployProg;

#[test]
fn initial_selection_prefers_adt_then_rfc() {
    let fixture: Fixture = serde_json::from_str(
        &fs::read_to_string(common::fixture_path("dev-750-rfc.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(fixture.system, "DEV");
    assert_eq!(fixture.release, "750");
    assert_eq!(fixture.backend, "rfc");
    assert_eq!(
        Router::both()
            .select(READ, None, false, Failure::Initial)
            .unwrap(),
        BackendKind::AdtHttp
    );
    assert_eq!(
        Router::new(false, true)
            .select(READ, None, false, Failure::Initial)
            .unwrap(),
        BackendKind::RfcSadt
    );
    assert!(
        Router::new(false, false)
            .select(READ, None, false, Failure::Initial)
            .is_err()
    );
    assert_eq!(
        Router::both()
            .select(DEPLOY, Some(BackendKind::RfcSadt), false, Failure::Initial)
            .unwrap(),
        BackendKind::RfcSadt
    );
    assert!(
        Router::new(true, false)
            .select(DEPLOY, Some(BackendKind::RfcSadt), false, Failure::Initial)
            .is_err()
    );
}

#[test]
fn falls_back_only_before_send_or_for_a_missing_capability() {
    let router = Router::both();

    for failure in [Failure::PreSendConnectivity, Failure::CapabilityUnavailable] {
        assert_eq!(
            router.select(READ, None, false, failure).unwrap(),
            BackendKind::RfcSadt
        );
        assert_eq!(
            router.select(DEPLOY, None, false, failure).unwrap(),
            BackendKind::RfcSadt
        );
    }
    assert!(
        Router::new(true, false)
            .select(READ, None, false, Failure::PreSendConnectivity)
            .is_err()
    );

    for failure in [
        Failure::Authorization,
        Failure::Conflict,
        Failure::Validation,
        Failure::PostSendUncertain,
    ] {
        assert!(
            router.select(READ, None, false, failure).is_err(),
            "{failure}"
        );
        assert!(
            router.select(DEPLOY, None, false, failure).is_err(),
            "{failure}"
        );
    }
}

#[test]
fn explicit_backend_or_base_never_switches() {
    let router = Router::both();

    for failure in [Failure::PreSendConnectivity, Failure::CapabilityUnavailable] {
        let explicit = router.select(READ, Some(BackendKind::AdtHttp), false, failure);
        let base = router.select(DEPLOY, None, true, failure);

        assert!(explicit.unwrap_err().contains("--backend"), "{failure}");
        assert!(base.unwrap_err().contains("--base"), "{failure}");
    }

    assert_eq!(
        router.select(DEPLOY, None, true, Failure::Initial).unwrap(),
        BackendKind::AdtHttp
    );
}

#[test]
fn refusals_name_the_operation_backend_and_failure() {
    let router = Router::both();
    let refusal = |failure| router.select(DEPLOY, None, false, failure).unwrap_err();

    assert_eq!(
        Router::new(true, false)
            .select(DEPLOY, Some(BackendKind::RfcSadt), false, Failure::Initial)
            .unwrap_err(),
        "RFC SADT is not available for deploy PROG"
    );
    assert_eq!(
        Router::new(false, true)
            .select(READ, Some(BackendKind::AdtHttp), false, Failure::Initial)
            .unwrap_err(),
        "ADT HTTP is not available for read PROG"
    );
    assert_eq!(
        Router::new(false, false)
            .select(OperationKind::Doctor, None, false, Failure::Initial)
            .unwrap_err(),
        "RFC SADT is not available for doctor"
    );
    assert_eq!(
        router
            .select(READ, Some(BackendKind::AdtHttp), false, Failure::Conflict)
            .unwrap_err(),
        "read PROG cannot fall back after conflict: --backend pins the backend"
    );
    assert_eq!(
        refusal(Failure::Authorization),
        "deploy PROG cannot fall back after authorization"
    );
    assert_eq!(
        refusal(Failure::Conflict),
        "deploy PROG cannot fall back after conflict"
    );
    assert_eq!(
        refusal(Failure::Validation),
        "deploy PROG cannot fall back after validation"
    );
    assert_eq!(
        refusal(Failure::PostSendUncertain),
        "deploy PROG cannot fall back after post-send uncertainty"
    );
    assert_eq!(
        Router::new(true, false)
            .select(DEPLOY, None, false, Failure::PreSendConnectivity)
            .unwrap_err(),
        "RFC SADT is not available for deploy PROG"
    );
    assert_eq!(
        Failure::PreSendConnectivity.to_string(),
        "pre-send connectivity"
    );
    assert_eq!(
        Failure::CapabilityUnavailable.to_string(),
        "capability unavailable"
    );
    assert_eq!(Failure::Initial.to_string(), "initial selection");
}

#[test]
fn transport_errors_map_to_fallback_classes() {
    let http = |status| TransportError::Http {
        status,
        message: String::new(),
    };
    let cases = [
        (
            TransportError::PreSendConnectivity(String::new()),
            Failure::PreSendConnectivity,
        ),
        (
            TransportError::PostSendUncertain(String::new()),
            Failure::PostSendUncertain,
        ),
        (
            TransportError::NoResponse(String::new()),
            Failure::PreSendConnectivity,
        ),
        (
            TransportError::Authorization(String::new()),
            Failure::Authorization,
        ),
        (TransportError::Protocol(String::new()), Failure::Validation),
        (TransportError::CsrfRequired, Failure::Authorization),
        (
            TransportError::RequestNotAllowed(String::new()),
            Failure::Validation,
        ),
        (http(401), Failure::Authorization),
        (http(403), Failure::Validation),
        (http(409), Failure::Conflict),
        (http(423), Failure::Conflict),
        (http(500), Failure::Validation),
    ];

    for (error, expected) in cases {
        assert_eq!(Failure::from(&error), expected, "{error:?}");
    }
}
