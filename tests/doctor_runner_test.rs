use abapreach::operation::doctor_runner::{
    Leftover, SEARCH_LIMIT, age_label, epoch_seconds, leftovers_line, parse_runner_names,
    post_transport_line, search_uri,
};

const PREFIX: &str = "YCL_ABAPREACH_RUN_";

fn references(names: &[&str]) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
    );

    for name in names {
        xml.push_str(&format!(
            r#"<adtcore:objectReference adtcore:uri="/sap/bc/adt/oo/classes/{}" adtcore:type="CLAS/OC" adtcore:name="{name}" adtcore:packageName="$TMP"/>"#,
            name.to_ascii_lowercase()
        ));
    }

    xml.push_str("</adtcore:objectReferences>");

    xml
}

#[test]
fn the_search_uri_has_the_admitted_shape() {
    assert_eq!(
        search_uri(PREFIX, 20),
        "/sap/bc/adt/repository/informationsystem/search?operation=quickSearch\
         &query=YCL_ABAPREACH_RUN_*&maxResults=20&objectType=CLAS%2FOC"
    );
}

#[test]
fn only_runner_shaped_names_of_the_prefix_are_leftovers() {
    let xml = references(&[
        "YCL_ABAPREACH_RUN_AB12CD34",
        "YCL_ABAPREACH_RUN_SHORT",
        "YCL_ABAPREACH_RUN_lower123",
        "YCL_ABAPREACH_RUN_AB12CD34X",
        "YCL_OTHER_AB12CD34",
        "YCL_ABAPREACH_RUN_ZZ000000",
    ]);
    let (raw, names) = parse_runner_names(&xml, PREFIX).unwrap();

    assert_eq!(raw, 6);
    assert_eq!(
        names,
        ["YCL_ABAPREACH_RUN_AB12CD34", "YCL_ABAPREACH_RUN_ZZ000000"]
    );
}

#[test]
fn a_reference_of_another_type_is_not_a_leftover() {
    let xml = references(&["YCL_ABAPREACH_RUN_AB12CD34"])
        .replace(r#"adtcore:type="CLAS/OC""#, r#"adtcore:type="PROG/P""#);

    assert_eq!(parse_runner_names(&xml, PREFIX).unwrap(), (0, Vec::new()));
}

#[test]
fn malformed_search_xml_is_an_error() {
    assert!(parse_runner_names("<a><b></a>", PREFIX).is_err());
}

#[test]
fn timestamps_parse_to_epoch_seconds() {
    assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(epoch_seconds("2000-03-01T00:00:00Z"), Some(951_868_800));
    assert_eq!(epoch_seconds("2026-11-08T07:57:03Z"), Some(1_794_124_623));
    assert_eq!(epoch_seconds("2026-11-08T07:57:03"), Some(1_794_124_623));
    assert_eq!(
        epoch_seconds("2026-11-08T07:57:03.123Z"),
        Some(1_794_124_623)
    );
}

#[test]
fn unreadable_timestamps_have_no_epoch() {
    for text in [
        "",
        "yesterday",
        "2026-13-08T07:57:03Z",
        "2026-11-32T07:57:03Z",
        "2026-11-08T24:00:00Z",
        "2026-11-08T07:57:03+01:00",
        "1969-12-31T23:59:59Z",
    ] {
        assert_eq!(epoch_seconds(text), None, "{text}");
    }
}

#[test]
fn ages_are_rounded_down_to_the_largest_unit() {
    for (seconds, label) in [
        (0, "0m"),
        (59, "0m"),
        (60, "1m"),
        (3_599, "59m"),
        (3_600, "1h"),
        (172_799, "47h"),
        (172_800, "2d"),
    ] {
        assert_eq!(age_label(seconds), label, "{seconds}");
    }
}

fn entry(name: &str, age: Option<u64>) -> Leftover {
    Leftover {
        name: name.to_owned(),
        age_seconds: age,
    }
}

#[test]
fn no_leftover_prints_none() {
    assert_eq!(
        leftovers_line("DEV_100", &[], 0, SEARCH_LIMIT),
        "RUNNER leftovers: none"
    );
}

#[test]
fn leftovers_list_names_ages_and_the_stale_mark() {
    let line = leftovers_line(
        "DEV_100",
        &[
            entry("YCL_ABAPREACH_RUN_AB12CD34", Some(120)),
            entry("YCL_ABAPREACH_RUN_ZZ000000", Some(601)),
            entry("YCL_ABAPREACH_RUN_QQ111111", Some(600)),
            entry("YCL_ABAPREACH_RUN_NO000000", None),
        ],
        4,
        SEARCH_LIMIT,
    );

    assert_eq!(
        line,
        "RUNNER leftovers: 4 (YCL_ABAPREACH_RUN_AB12CD34 2m, \
         YCL_ABAPREACH_RUN_ZZ000000 10m stale, YCL_ABAPREACH_RUN_QQ111111 10m, \
         YCL_ABAPREACH_RUN_NO000000 age unknown; remove with abapreach delete --simulate \
         CLAS:<name> --plan <FILE> --conn DEV_100, then without --simulate)"
    );
}

#[test]
fn a_search_that_reached_its_limit_prints_a_plus() {
    let line = leftovers_line(
        "DEV_100",
        &[entry("YCL_ABAPREACH_RUN_AB12CD34", Some(5))],
        SEARCH_LIMIT as usize,
        SEARCH_LIMIT,
    );

    assert!(line.starts_with("RUNNER leftovers: 1+ ("), "{line}");
}

#[test]
fn post_transport_follows_the_route_and_fact_u2() {
    // (adt route, rfc route, sadt post works) -> line
    for (adt, rfc, works, expected) in [
        (true, false, true, "RUNNER post transport: http"),
        (true, true, true, "RUNNER post transport: http"),
        (false, true, true, "RUNNER post transport: sadt"),
        (true, false, false, "RUNNER post transport: http"),
        (true, true, false, "RUNNER post transport: http"),
        (
            false,
            true,
            false,
            "RUNNER post transport: unavailable (no ADT HTTP route)",
        ),
        (
            false,
            false,
            true,
            "RUNNER post transport: unavailable (no ADT HTTP route)",
        ),
    ] {
        assert_eq!(
            post_transport_line(adt, rfc, works),
            expected,
            "{adt} {rfc} {works}"
        );
    }
}
