use abapreach::config::Connection;
use abapreach::transport::TransportError;
use abapreach::transport::rfc::{
    DynamicRfcApi, Env, RfcLibrary, RfcPhase, RfcSymbols, classify_rfc_error, connection_parameters,
};

// RFC_RC values from sapnwrfc.h, kept as literals so a wrong constant in the product is caught.
const COMMUNICATION_FAILURE: i32 = 1;
const LOGON_FAILURE: i32 = 2;
const ABAP_RUNTIME_FAILURE: i32 = 3;
const ABAP_EXCEPTION: i32 = 5;
const CLOSED: i32 = 6;
const TIMEOUT: i32 = 8;
const AUTHORIZATION_FAILURE: i32 = 24;

#[test]
fn symbols_are_the_complete_dynamic_abi_surface() {
    assert_eq!(
        RfcSymbols::required_names(),
        vec![
            "RfcOpenConnection",
            "RfcPing",
            "RfcGetVersion",
            "RfcGetFunctionDesc",
            "RfcCreateFunction",
            "RfcGetStructure",
            "RfcGetTable",
            "RfcAppendNewRow",
            "RfcSetString",
            "RfcSetXString",
            "RfcGetStringLength",
            "RfcGetString",
            "RfcGetXString",
            "RfcInvoke",
            "RfcDestroyFunction",
            "RfcCloseConnection",
        ]
    );
}

#[test]
fn ffi_layout_matches_the_spike_definitions() {
    assert!(DynamicRfcApi::abi_layout_checked());
}

#[test]
fn open_connect_failure_is_pre_send() {
    let error = classify_rfc_error(RfcPhase::Open, COMMUNICATION_FAILURE, "no route");

    assert!(
        matches!(error, TransportError::PreSendConnectivity(_)),
        "{error:?}"
    );
}

#[test]
fn logon_and_authorization_failures_never_allow_a_fallback() {
    for phase in [RfcPhase::Open, RfcPhase::Prepare, RfcPhase::Invoke] {
        for code in [LOGON_FAILURE, AUTHORIZATION_FAILURE] {
            let error = classify_rfc_error(phase, code, "denied");

            assert!(
                matches!(error, TransportError::Authorization(_)),
                "{phase:?} {error:?}"
            );
        }
    }
}

#[test]
fn prepare_communication_failure_is_pre_send_and_other_prepare_errors_stop() {
    let lost = classify_rfc_error(RfcPhase::Prepare, COMMUNICATION_FAILURE, "lost");
    let other = classify_rfc_error(RfcPhase::Prepare, 15, "invalid parameter");

    assert!(
        matches!(lost, TransportError::PreSendConnectivity(_)),
        "{lost:?}"
    );
    assert!(matches!(other, TransportError::Protocol(_)), "{other:?}");
}

#[test]
fn invoke_communication_failure_is_post_send_uncertain() {
    for code in [COMMUNICATION_FAILURE, CLOSED, TIMEOUT] {
        let error = classify_rfc_error(RfcPhase::Invoke, code, "connection lost");

        assert!(error.is_post_send_uncertain(), "{code}: {error:?}");
    }
}

#[test]
fn invoke_abap_failures_are_authorization_or_protocol_never_uncertain() {
    let exception = classify_rfc_error(RfcPhase::Invoke, ABAP_EXCEPTION, "NO_SUCH_ENTRY");
    let runtime = classify_rfc_error(RfcPhase::Invoke, ABAP_RUNTIME_FAILURE, "SYSTEM_FAILURE");
    let denied = classify_rfc_error(
        RfcPhase::Invoke,
        ABAP_RUNTIME_FAILURE,
        "No authorization for S_RFC",
    );

    assert!(
        matches!(exception, TransportError::Protocol(_)),
        "{exception:?}"
    );
    assert!(
        matches!(runtime, TransportError::Protocol(_)),
        "{runtime:?}"
    );
    assert!(
        matches!(denied, TransportError::Authorization(_)),
        "{denied:?}"
    );
}

fn names(parameters: &[(String, String)]) -> Vec<&str> {
    parameters.iter().map(|(name, _)| name.as_str()).collect()
}

#[test]
fn connection_parameters_use_sdk_names_and_optional_saprouter() {
    let mut connection = Connection::test_insecure();
    let parameters = connection_parameters(&connection);

    assert_eq!(
        names(&parameters),
        vec!["ASHOST", "SYSNR", "CLIENT", "USER", "PASSWD", "LANG"]
    );
    assert_eq!(parameters[0].1, "example.com");
    assert_eq!(parameters[2].1, "100");

    connection.rfc.saprouter = Some("/H/router.example.com/S/3299/H/".to_owned());
    let parameters = connection_parameters(&connection);

    assert_eq!(parameters.last().unwrap().0, "SAPROUTER");
}

#[test]
fn installed_library_resolves_every_symbol_and_reports_a_version() {
    let Ok(library) = RfcLibrary::load(&Env::current()) else {
        eprintln!("skipped: no loadable RFC library on this machine");
        return;
    };
    let api = DynamicRfcApi::load(library, Vec::new()).unwrap();
    let (major, _minor, _patch) = api.version();

    assert!(major >= 7, "unexpected RFC library major version {major}");
}
