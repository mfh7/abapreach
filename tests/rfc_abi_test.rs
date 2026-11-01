use abapreach::backend::Failure;
use abapreach::config::Connection;
use abapreach::transport::TransportError;
use abapreach::transport::rfc::{
    DynamicRfcApi, Env, RfcLibrary, RfcPhase, RfcSymbols, classify_rfc_error,
    connection_parameters, init_trace_environment,
};

// RFC_RC values from sapnwrfc.h, kept as literals so a wrong constant in the product is caught.
const COMMUNICATION_FAILURE: i32 = 1;
const LOGON_FAILURE: i32 = 2;
const ABAP_RUNTIME_FAILURE: i32 = 3;
const ABAP_EXCEPTION: i32 = 5;
const CLOSED: i32 = 6;
const TIMEOUT: i32 = 8;
const CONVERSION_FAILURE: i32 = 22;
const ABAP_CLASS_EXCEPTION: i32 = 27;
const AUTHORIZATION_FAILURE: i32 = 29;

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
fn authorization_failures_keep_their_class_and_mark_invoke_as_sent() {
    for phase in [RfcPhase::Open, RfcPhase::Prepare, RfcPhase::Invoke] {
        for code in [LOGON_FAILURE, AUTHORIZATION_FAILURE] {
            let error = classify_rfc_error(phase, code, "denied");

            assert_eq!(Failure::from(&error), Failure::Authorization);
            assert_eq!(error.was_sent(), phase == RfcPhase::Invoke);
        }
    }
}

#[test]
fn sdk_authorization_and_conversion_codes_are_never_swapped() {
    let denied = classify_rfc_error(RfcPhase::Open, AUTHORIZATION_FAILURE, "denied");

    assert_eq!(Failure::from(&denied), Failure::Authorization);
    assert!(!denied.was_sent(), "{denied:?}");

    let class_exception = classify_rfc_error(RfcPhase::Invoke, ABAP_CLASS_EXCEPTION, "ZCX_FAILURE");
    let conversion = classify_rfc_error(RfcPhase::Invoke, CONVERSION_FAILURE, "bad parameter");

    assert_eq!(Failure::from(&class_exception), Failure::Validation);
    assert!(class_exception.was_sent(), "{class_exception:?}");
    assert!(conversion.was_sent(), "{conversion:?}");
    assert!(
        !matches!(conversion, TransportError::PreSendConnectivity(_)),
        "{conversion:?}"
    );
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
fn invoke_abap_failures_are_sent_and_keep_their_failure_class() {
    let exception = classify_rfc_error(RfcPhase::Invoke, ABAP_EXCEPTION, "NO_SUCH_ENTRY");
    let runtime = classify_rfc_error(RfcPhase::Invoke, ABAP_RUNTIME_FAILURE, "SYSTEM_FAILURE");
    let denied = classify_rfc_error(
        RfcPhase::Invoke,
        ABAP_RUNTIME_FAILURE,
        "No authorization for S_RFC",
    );

    assert!(exception.was_sent());
    assert!(runtime.was_sent());
    assert!(denied.was_sent());
    assert_eq!(Failure::from(&exception), Failure::Validation);
    assert_eq!(Failure::from(&runtime), Failure::Validation);
    assert_eq!(Failure::from(&denied), Failure::Authorization);
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
    // `load` no longer sets the trace environment; `main` does, before any thread exists.
    init_trace_environment();

    let Ok(library) = RfcLibrary::load(&Env::current()) else {
        eprintln!("skipped: no loadable RFC library on this machine");
        return;
    };
    let api = DynamicRfcApi::load(library, Vec::new()).unwrap();
    let (major, _minor, _patch) = api.version();

    assert!(major >= 7, "unexpected RFC library major version {major}");
}
