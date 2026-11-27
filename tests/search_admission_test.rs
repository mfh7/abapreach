//! The request boundary of the `search` command: exactly
//! `GET search?operation=quickSearch&query=<PATTERN>&maxResults=<1..=1000>[&objectType=<TYPE>]`,
//! in that order, with nothing else under `informationsystem`. The two older search shapes (the
//! function group of a module, the runner class prefix) are covered in `rfc_sadt_test.rs` and
//! `runner_admission_test.rs`.

use abapreach::adt::function_group_search_uri;
use abapreach::operation::doctor_runner::{SEARCH_LIMIT, search_uri};
use abapreach::operation::search::{Query, TYPES};
use abapreach::transport::{
    Request, SadtTransport, TransportError,
    rfc::{FakeRfcApi, RfcTransport},
};

const SEARCH: &str = "/sap/bc/adt/repository/informationsystem/search";

fn admits(request: &Request) -> bool {
    let mut transport = RfcTransport::new(FakeRfcApi::ok());

    match transport.request(request.clone()) {
        Ok(_) => true,
        Err(TransportError::RequestNotAllowed(_)) => false,
        Err(error) => panic!("{request:?}: {error}"),
    }
}

fn search(query: &str) -> Request {
    Request::get(format!("{SEARCH}?{query}"))
}

#[test]
fn the_request_the_command_builds_is_admitted_for_every_type_and_limit() {
    for pattern in ["ZCL_ABAP", "Z*", "*", "*CL*TYPE*", "/NS/ZCL_*", "/NS/*"] {
        for max in [1, 9, 10, 50, 100, 101, 999, 1000] {
            for token in std::iter::once(None).chain(TYPES.iter().map(|(token, _)| Some(*token))) {
                let uri = Query::new(pattern, token, max).unwrap().uri();

                assert!(admits(&Request::get(uri.clone())), "{uri}");
            }
        }
    }
}

/// The two older searches have no shape of their own any more: the exact requests the function
/// group resolver and the `doctor` leftover search send are inside the `search` shape.
#[test]
fn the_function_group_resolver_and_the_doctor_leftover_search_are_admitted() {
    for name in ["zfm", "ZFM_A", "z_fm_1", "/ns/zfm_x"] {
        let uri = function_group_search_uri(name);

        assert!(
            uri.ends_with("&maxResults=10&objectType=FUGR%2FFF"),
            "{uri}"
        );
        assert!(admits(&Request::get(uri.clone())), "{uri}");
    }

    for prefix in ["YCL_ABAPREACH_RUN_", "ZTEST_RUN_", "Y", "Z9"] {
        let uri = search_uri(prefix, SEARCH_LIMIT);

        assert!(
            uri.ends_with("*&maxResults=20&objectType=CLAS%2FOC"),
            "{uri}"
        );
        assert!(admits(&Request::get(uri.clone())), "{uri}");
    }
}

#[test]
fn the_documented_shapes_are_admitted() {
    for query in [
        "operation=quickSearch&query=ZCL_X&maxResults=1",
        "operation=quickSearch&query=ZCL_X&maxResults=1000",
        "operation=quickSearch&query=Z*&maxResults=50&objectType=CLAS%2FOC",
        "operation=quickSearch&query=*&maxResults=1000&objectType=PROG%2FP",
        "operation=quickSearch&query=/NS/ZCL_*&maxResults=5&objectType=FUGR%2FFF",
        "operation=quickSearch&query=Z9_*&maxResults=10&objectType=TABL%2FDT",
    ] {
        assert!(admits(&search(query)), "{query}");
    }
}

#[test]
fn a_broader_or_reordered_query_is_refused() {
    for query in [
        // An extra parameter, before, between and after.
        "operation=quickSearch&query=Z*&maxResults=50&x=1",
        "operation=quickSearch&query=Z*&maxResults=50&objectType=CLAS%2FOC&x=1",
        "operation=quickSearch&x=1&query=Z*&maxResults=50",
        "operation=quickSearch&query=Z*&x=1&maxResults=50",
        "operation=quickSearch&query=Z*&maxResults=50&objectType=CLAS%2FOC&objectType=PROG%2FP",
        "operation=quickSearch&query=Z*&maxResults=50&maxResults=60",
        "operation=quickSearch&query=Z*&maxResults=50&packageName=ZPACK",
        "operation=quickSearch&query=Z*&maxResults=50&withDescriptions=true",
        "operation=quickSearch&query=Z*&maxResults=50&objectType=CLAS%2FOC&",
        "operation=quickSearch&query=Z*&maxResults=50&",
        "query=Z*&maxResults=50",
        "operation=quickSearch&maxResults=50&query=Z*",
        "operation=quickSearch&query=Z*",
        "operation=quickSearch&maxResults=50",
        "operation=quickSearch",
        "",
        // Another order of the two optional keys.
        "operation=quickSearch&query=Z*&objectType=CLAS%2FOC&maxResults=50",
        "operation=quickSearch&maxResults=50&query=Z*&objectType=CLAS%2FOC",
        "maxResults=50&operation=quickSearch&query=Z*",
        // Another operation, or a differently cased key.
        "operation=quickSearch2&query=Z*&maxResults=50",
        "operation=packageSearch&query=Z*&maxResults=50",
        "operation=QuickSearch&query=Z*&maxResults=50",
        "Operation=quickSearch&query=Z*&maxResults=50",
        "operation=quickSearch&Query=Z*&maxResults=50",
        "operation=quickSearch&query=Z*&MaxResults=50",
        "operation=quickSearch&query=Z*&maxResults=50&ObjectType=CLAS%2FOC",
    ] {
        assert!(!admits(&search(query)), "{query}");
    }
}

#[test]
fn a_number_of_results_outside_one_to_a_thousand_or_not_plain_is_refused() {
    for limit in [
        "0",
        "1001",
        "1010",
        "10000",
        "00",
        "01",
        "050",
        "0050",
        "-1",
        "+5",
        " 5",
        "5 ",
        "5.0",
        "1e2",
        "0x10",
        "٥",
        "5%30",
        "%35",
        "",
        "4294967296",
        "99999999999999999999",
        "5;",
        "5,0",
    ] {
        let query = format!("operation=quickSearch&query=Z*&maxResults={limit}");

        assert!(!admits(&search(&query)), "{query}");
        assert!(
            !admits(&search(&format!("{query}&objectType=CLAS%2FOC"))),
            "{query} typed"
        );
    }
}

#[test]
fn an_object_type_outside_the_closed_list_or_not_encoded_the_same_way_is_refused() {
    for object_type in [
        "",
        "CLAS",
        "CLAS/OC",
        "clas%2Foc",
        "CLAS%2foc",
        "CLAS%2Foc",
        "CLAS%2FOC%20",
        "CLAS%252FOC",
        "CLAS%2F",
        "%2FOC",
        "CLAS%2FOO",
        "CLAS%2FOCX",
        "PROG%2FI",
        "TABL%2FDS",
        "SUSH",
        "DEVC%2FK",
        "FUGR%2FFF%2CCLAS%2FOC",
        "FUGR%2FFF,CLAS%2FOC",
        "FUGR%2FFF&objectType=CLAS%2FOC",
        "*",
        "CLAS%2F*",
    ] {
        let query =
            format!("operation=quickSearch&query=Z*&maxResults=50&objectType={object_type}");

        assert!(!admits(&search(&query)), "{query}");
    }
}

#[test]
fn an_encoded_or_invalid_pattern_character_is_refused() {
    for pattern in [
        "",
        "ycl_*",
        "Zcl",
        "Z%2A",
        "Z%2a",
        "%2A",
        "Z%41",
        "Z%20",
        "Z+X",
        "Z X",
        "Z\tX",
        "Z?",
        "Z-X",
        "Z.X",
        "Z&X",
        "Z=X",
        "Z;X",
        "Z#X",
        "Z~X",
        "Z'X",
        "Z\"X",
        "Z<X",
        "Z\\X",
        "Z%00",
        "Z%2F",
        "%2FNS%2FZ",
        "$TMP",
        "ZÄ",
        // A slash outside the namespace form.
        "/",
        "/NS",
        "/NS/",
        "NS/ZCL",
        "ZCL/",
        "/NS/ZCL/X",
        "/N*S/ZCL",
        "//NS/ZCL",
        "./ZCL",
        "../ZCL",
        "/NS/../ZCL",
        &"Z".repeat(41),
    ] {
        let query = format!("operation=quickSearch&query={pattern}&maxResults=50");

        assert!(!admits(&search(&query)), "{query}");
    }
}

#[test]
fn another_path_method_or_body_is_refused() {
    let query = "operation=quickSearch&query=Z*&maxResults=50";

    assert!(admits(&search(query)));

    for path in [
        "/sap/bc/adt/repository/informationsystem/search/",
        "/sap/bc/adt/repository/informationsystem/search/x",
        "/sap/bc/adt/repository/informationsystem/Search",
        "/sap/bc/adt/repository/informationsystem/searchx",
        "/sap/bc/adt/repository/informationsystem",
        "/sap/bc/adt/repository/informationsystem/objecttypes",
        "/sap/bc/adt/repository/informationsystem/usageReferences",
        "/sap/bc/adt/repository/informationsystem/../informationsystem/search",
        "//sap/bc/adt/repository/informationsystem/search",
        "/sap/bc/adt/repository/nodestructure",
        "/sap/bc/adt/repository/informationsystem/search;x=1",
        "/sap/bc/adt/repository/informationsystem/%73earch",
        "/sap/bc/adt/search",
    ] {
        assert!(!admits(&Request::get(format!("{path}?{query}"))), "{path}");
    }

    for method in ["POST", "PUT", "DELETE"] {
        let mut request = search(query);

        request.method = method.to_owned();

        assert!(!admits(&request), "{method}");
    }

    // A fragment never leaves the process.
    assert!(!admits(&search(&format!("{query}#f"))));
    assert!(!admits(&search(&format!("{query}&objectType=CLAS%2FOC#f"))));
}
