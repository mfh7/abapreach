//! `dumps` and `dump`: the tag decoder, the checked inputs, and the reads of SNAP over the data
//! preview and, when SAP refuses it, over `RFC_READ_TABLE` (HD1 750, FED 740, S4D 758 replies from
//! `tests/fixtures/m9`; synthetic rows for `dump`).

mod common;

use abapreach::backend::{BackendKind, Failure};
use abapreach::operation::dictionary::PreviewError;
use abapreach::operation::dumps::{
    DumpId, DumpQuery, ListOptions, ListQuery, Tag, decode_tags, join_stream, run_dump, run_list,
    user_name,
};
use abapreach::policy::Policies;
use abapreach::transport::rfc::{
    Cell, FakeRfcApi, FunctionResult, RfcEndpointResponse, RfcTransport, UcWidth,
};
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, fixture_path_in, preview, read_table_reply};
use std::collections::BTreeMap;
use std::fs;

const TODAY: &str = "20261125";
const REFUSAL: &str = "You cannot display SNAP with the standard tools";

fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_path_in("m9", name)).expect("read fixture")
}

fn tag(id: &str, value: &str) -> Tag {
    Tag {
        id: id.to_owned(),
        value: value.to_owned(),
    }
}

/// The stream of tags: `ID` + three-digit length + value.
fn stream_of(tags: &[Tag]) -> String {
    tags.iter()
        .map(|tag| format!("{}{:03}{}", tag.id, tag.value.chars().count(), tag.value))
        .collect()
}

// ---- the decoder ------------------------------------------------------------------------------

#[test]
fn the_decoder_reads_id_length_and_value_one_after_the_other() {
    let stream = "FC029CREATE_OBJECT_CLASS_NOT_FOUNDAP005ZPROGAL0027 ";

    assert_eq!(
        decode_tags(stream),
        vec![
            tag("FC", "CREATE_OBJECT_CLASS_NOT_FOUND"),
            tag("AP", "ZPROG"),
            tag("AL", "7 "),
        ]
    );
}

#[test]
fn the_decoder_stops_at_padding_a_blank_id_or_a_length_that_is_not_three_digits() {
    for (stream, expected) in [
        ("", vec![]),
        ("FC0", vec![]),
        ("FC002AB     ", vec![tag("FC", "AB")]),
        ("FC002AB  000AP002CD", vec![tag("FC", "AB")]),
        ("FC002ABAP00xCD", vec![tag("FC", "AB")]),
        ("FC002ABAP-02CD", vec![tag("FC", "AB")]),
        ("FC002ABAP 02CD", vec![tag("FC", "AB")]),
        ("FC002ABAP", vec![tag("FC", "AB")]),
    ] {
        assert_eq!(decode_tags(stream), expected, "{stream:?}");
    }
}

#[test]
fn a_value_cut_at_the_end_of_the_stream_is_returned_as_far_as_it_goes() {
    assert_eq!(
        decode_tags("FC002ABAP010ZPR"),
        vec![tag("FC", "AB"), tag("AP", "ZPR")]
    );
    assert_eq!(decode_tags("FC000"), vec![tag("FC", "")]);
}

#[test]
fn the_decoder_reads_characters_not_bytes() {
    assert_eq!(
        decode_tags("TX003äöüAP001X"),
        vec![tag("TX", "äöü"), tag("AP", "X")]
    );
}

#[test]
fn the_tags_of_the_fixture_rows_are_decoded_from_their_flist() {
    let xml = fixture("snap-list-740.xml");
    let table = abapreach::operation::read_table::parse_preview(&xml).unwrap();
    let flist = table.columns.iter().position(|c| c == "FLIST").unwrap();
    let tags = decode_tags(&table.rows[0][flist]);

    assert_eq!(tags[0], tag("FC", "CREATE_OBJECT_CLASS_NOT_FOUND"));
    assert_eq!(tags[1].id, "AP");
    assert_eq!(tags[2].id, "AI");
    assert_eq!(tags[3], tag("AL", "18"));
}

/// A small deterministic generator: the tests need no random crate and every failure repeats.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;

        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

fn random_tags(random: &mut Random) -> Vec<Tag> {
    const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789=_/ ";

    (0..1 + random.below(12))
        .map(|_| {
            let id: String = (0..2)
                .map(|_| char::from(LETTERS[random.below(36)]))
                .collect();
            let size = random.below(330);
            let mut value: String = (0..size)
                .map(|_| char::from(LETTERS[random.below(LETTERS.len())]))
                .collect();

            // A value never ends in a blank here: the join pads, so a trailing blank of the last tag
            // would be indistinguishable from the padding.
            if value.ends_with(' ') {
                value.pop();
                value.push('X');
            }

            Tag { id, value }
        })
        .collect()
}

/// The rows of a dump as SAP stores them: 200-character columns, `per_row` columns per row. The
/// trailing blanks of a column are dropped when `trim`, as the RFC path returns them.
fn split_rows(stream: &str, per_row: usize, trim: bool) -> Vec<Vec<String>> {
    let chars: Vec<char> = stream.chars().collect();
    let mut columns: Vec<String> = chars
        .chunks(200)
        .map(|chunk| {
            let text: String = chunk.iter().collect();

            if trim {
                text.trim_end().to_owned()
            } else {
                text
            }
        })
        .collect();

    while !columns.len().is_multiple_of(per_row) {
        columns.push(String::new());
    }

    columns.chunks(per_row).map(<[String]>::to_vec).collect()
}

#[test]
fn random_tag_streams_split_at_random_column_and_row_boundaries_decode_the_same() {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut crossed_a_column = 0;
    let mut crossed_a_row = 0;

    for case in 0..400 {
        let tags = random_tags(&mut random);
        let stream = stream_of(&tags);
        let per_row = 1 + random.below(8);
        let trim = random.below(2) == 0;
        let rows = split_rows(&stream, per_row, trim);

        crossed_a_column += usize::from(stream.chars().count() > 200);
        crossed_a_row += usize::from(stream.chars().count() > 200 * per_row);

        assert_eq!(
            decode_tags(&join_stream(&rows)),
            tags,
            "case {case}: {per_row} columns per row, trim {trim}"
        );
    }

    assert!(crossed_a_column > 100, "{crossed_a_column}");
    assert!(crossed_a_row > 50, "{crossed_a_row}");
}

#[test]
fn a_tag_header_and_a_value_cut_exactly_at_a_column_boundary_decode() {
    for cut in 195..205 {
        // The first tag fills the stream up to `cut`; the second one starts there.
        let filler = cut - 5;
        let tags = vec![tag("ZZ", &"a".repeat(filler)), tag("AL", "4711")];
        let stream = stream_of(&tags);

        for per_row in [1, 2, 3] {
            let rows = split_rows(&stream, per_row, true);

            assert_eq!(decode_tags(&join_stream(&rows)), tags, "{cut} {per_row}");
        }
    }
}

// ---- the inputs -------------------------------------------------------------------------------

fn policies() -> Policies {
    Policies::default()
}

fn list(options: ListOptions<'_>) -> Result<ListQuery, String> {
    ListQuery::new(&options, "farhan", TODAY, &policies())
}

fn options<'a>() -> ListOptions<'a> {
    ListOptions {
        date: None,
        user: None,
        all: false,
        rows: 20,
    }
}

#[test]
fn a_dump_id_has_five_checked_parts() {
    let id = DumpId::parse("20261103/053726/apphost/testuser/21").unwrap();

    assert_eq!(id.user(), "TESTUSER");

    for good in [
        "20261103/053726/apphost/TESTUSER/21",
        "20261103/053726/sap-app_dv.s4d/TESTUSER/2",
        "20280229/235959/h/U/123",
        // What SNAP can hold and a literal takes: no longer limited to letters and digits.
        "20261103/053726/ä-host.1/ÜSER#1/21",
        "20261103/053726/apphost/$SYS&/21",
    ] {
        assert!(DumpId::parse(good).is_ok(), "{good}");
    }

    let long_host = "h".repeat(33);
    let long_user = "U".repeat(13);

    for bad in [
        "",
        "20261103/053726/apphost/TESTUSER",
        "20261103/053726/apphost/TESTUSER/21/9",
        "2026110/053726/apphost/TESTUSER/21",
        "20261301/053726/apphost/TESTUSER/21",
        "20261132/053726/apphost/TESTUSER/21",
        "20270229/053726/apphost/TESTUSER/21",
        "20261103/05372/apphost/TESTUSER/21",
        "20261103/246000/apphost/TESTUSER/21",
        "20261103/056000/apphost/TESTUSER/21",
        "20261103/053760/apphost/TESTUSER/21",
        "20261103/053726//TESTUSER/21",
        "20261103/053726/app host/TESTUSER/21",
        "20261103/053726/app'host/TESTUSER/21",
        "20261103/053726/apphost/TEST'USER/21",
        "20261103/053726/apphost//21",
        "20261103/053726/apphost/TESTUSER/",
        "20261103/053726/apphost/TESTUSER/2 1",
        "20261103/053726/apphost/TESTUSER/2134",
        "20261103/053726/apphost/TESTUSER/21'",
        "20261103/053726/apphost/TESTUSER/21 OR 1=1",
        "20261103/053726/app\u{7}host/TESTUSER/21",
        "20261103/053726/apphost/TEST\tUSER/21",
        "٢٠٢٦١١٠٣/053726/apphost/TESTUSER/21",
        &format!("20261103/053726/{long_host}/TESTUSER/21"),
        &format!("20261103/053726/apphost/{long_user}/21"),
    ] {
        assert!(DumpId::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_dumps_of_the_logon_user_are_the_default_and_all_lists_every_user() {
    assert!(list(options()).is_ok());
    assert!(
        list(ListOptions {
            all: true,
            ..options()
        })
        .is_ok()
    );
    assert!(
        list(ListOptions {
            user: Some("someone"),
            ..options()
        })
        .is_ok()
    );

    let both = list(ListOptions {
        all: true,
        user: Some("someone"),
        ..options()
    })
    .unwrap_err();

    assert!(both.contains("--all") && both.contains("--user"), "{both}");
}

#[test]
fn the_list_options_are_checked_before_any_connection() {
    for (bad, what) in [
        (
            ListOptions {
                rows: 0,
                ..options()
            },
            "rows",
        ),
        (
            ListOptions {
                rows: 1001,
                ..options()
            },
            "rows",
        ),
        (
            ListOptions {
                date: Some("2026-11-25"),
                ..options()
            },
            "--date",
        ),
        (
            ListOptions {
                date: Some("20261301"),
                ..options()
            },
            "--date",
        ),
        (
            ListOptions {
                date: Some("20261125' OR '1'='1"),
                ..options()
            },
            "--date",
        ),
        (
            ListOptions {
                user: Some("U' OR '1'='1"),
                ..options()
            },
            "--user",
        ),
        (
            ListOptions {
                user: Some("A VERY LONG USER NAME"),
                ..options()
            },
            "--user",
        ),
    ] {
        let message = list(bad).unwrap_err();

        assert!(message.contains(what), "{bad:?}: {message}");
    }

    for rows in [1, 20, 1000] {
        assert!(list(ListOptions { rows, ..options() }).is_ok(), "{rows}");
    }

    let odd = ListQuery::new(&options(), "A B", TODAY, &policies()).unwrap_err();

    assert!(odd.contains("logon user") && odd.contains("--all"), "{odd}");
}

#[test]
fn snap_on_the_deny_list_is_refused_before_any_connection() {
    let mut denied = policies();

    denied.read_table_denied.push("SNAP".to_owned());

    let message = ListQuery::new(&options(), "farhan", TODAY, &denied).unwrap_err();

    assert!(message.contains("read_table_denied"), "{message}");
    assert!(
        DumpQuery::new(
            "20261103/053726/apphost/FARHAN/21",
            false,
            "farhan",
            &denied
        )
        .is_err()
    );
}

#[test]
fn the_dump_of_another_user_needs_all() {
    let id = "20261103/053726/apphost/TESTUSER/21";
    let own = DumpQuery::new(
        "20261103/053726/apphost/FARHAN/21",
        false,
        "farhan",
        &policies(),
    );
    let other = DumpQuery::new(id, false, "farhan", &policies()).unwrap_err();

    assert!(own.is_ok());
    assert!(
        other.contains("TESTUSER") && other.contains("--all"),
        "{other}"
    );
    assert!(DumpQuery::new(id, true, "farhan", &policies()).is_ok());
    assert!(DumpQuery::new("bogus", true, "farhan", &policies()).is_err());
}

#[test]
fn user_names_are_upper_cased_and_never_hold_a_quote() {
    assert_eq!(user_name("u", "farhan").as_deref(), Ok("FARHAN"));
    assert_eq!(user_name("u", "A-B_1.$").as_deref(), Ok("A-B_1.$"));

    for bad in ["", "A'B", "A B", "A\"B", "ABCDEFGHIJKLM", "Ä", "A;B", "A/B"] {
        assert!(user_name("u", bad).is_err(), "{bad:?}");
    }
}

// ---- reading SNAP over the data preview ---------------------------------------------------------

fn ok(body: String) -> Result<Response, TransportError> {
    Ok(Response::text(200, body))
}

fn dd02l(name: &str) -> Result<Response, TransportError> {
    ok(preview(&[
        ("TABNAME", &[Some(name)]),
        ("TABCLASS", &[Some("TRANSP")]),
        ("VIEWCLASS", &[None]),
    ]))
}

fn refused() -> Result<Response, TransportError> {
    Ok(Response::text(400, fixture("snap-list-750-400.xml")))
}

fn listing(
    release: &str,
    options: ListOptions<'_>,
) -> (Result<String, PreviewError>, ScriptedTransport) {
    let mut transport = ScriptedTransport::replies(vec![
        dd02l("SNAP"),
        ok(fixture(&format!("snap-list-{release}.xml"))),
    ]);
    let query = ListQuery::new(&options, "TESTUSER", TODAY, &policies()).unwrap();
    let result = run_list(&mut transport, BackendKind::AdtHttp, &query, &policies());

    (result.map(|listing| listing.text), transport)
}

#[test]
fn the_list_of_the_758_release_prints_one_line_per_dump_newest_first() {
    let (result, transport) = listing("758", options());

    assert_eq!(
        result.unwrap(),
        "20261103 053726 TESTUSER 110 error=MESSAGE_TYPE_X_TEXT program=CL_SWF_RUN_WIM_LOCAL==========CP include=CL_SWF_RUN_WIM_LOCAL==========CM0EI line=247 id=20261103/053726/apphost/TESTUSER/21\n\
         20261103 053725 TESTUSER 110 error=MESSAGE_TYPE_X_TEXT program=CL_SWF_RUN_WIM_LOCAL==========CP include=CL_SWF_RUN_WIM_LOCAL==========CM0EI line=247 id=20261103/053725/apphost/TESTUSER/21\n\
         20261103 053724 TESTUSER 110 error=MESSAGE_TYPE_X_TEXT program=CL_SWF_RUN_WIM_LOCAL==========CP include=CL_SWF_RUN_WIM_LOCAL==========CM0EI line=247 id=20261103/053724/apphost/TESTUSER/21"
    );

    let call = &transport.calls[1];

    assert_eq!(
        call.uri,
        "/sap/bc/adt/datapreview/ddic?rowNumber=5000&ddicEntityName=SNAP"
    );
    assert_eq!(
        String::from_utf8(call.body.clone()).unwrap(),
        "SELECT DATUM, UZEIT, AHOST, UNAME, MANDT, MODNO, SEQNO, FLIST FROM SNAP WHERE SEQNO = '000' AND DATUM >= '20261117' AND UNAME = 'TESTUSER'"
    );
}

#[test]
fn the_list_of_the_740_release_prints_the_error_and_the_include_of_each_dump() {
    let (result, _) = listing("740", options());
    let text = result.unwrap();
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].starts_with(
            "20260819 010018 TESTUSER 100 error=CREATE_OBJECT_CLASS_NOT_FOUND program=CL_CI_TESTS"
        ),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains(" line=18 id=20260819/010018/apphost/TESTUSER/11"));
    assert!(lines[1].contains("id=20260818/010017/apphost/TESTUSER/12"));
}

#[test]
fn rows_cuts_the_list_after_sorting_and_all_and_date_change_the_condition() {
    let (result, transport) = listing(
        "758",
        ListOptions {
            rows: 2,
            all: true,
            date: Some("20261101"),
            ..options()
        },
    );

    assert_eq!(result.unwrap().lines().count(), 2);
    assert_eq!(
        String::from_utf8(transport.calls[1].body.clone()).unwrap(),
        "SELECT DATUM, UZEIT, AHOST, UNAME, MANDT, MODNO, SEQNO, FLIST FROM SNAP WHERE SEQNO = '000' AND DATUM >= '20261101'"
    );

    let (result, transport) = listing(
        "758",
        ListOptions {
            rows: 1,
            user: Some("testuser"),
            ..options()
        },
    );

    assert_eq!(result.unwrap().lines().count(), 1);
    assert!(
        String::from_utf8(transport.calls[1].body.clone())
            .unwrap()
            .ends_with("AND UNAME = 'TESTUSER'")
    );
}

#[test]
fn an_empty_list_says_so_and_a_full_read_is_refused_as_cut() {
    let none = preview(&[
        ("DATUM", &[]),
        ("UZEIT", &[]),
        ("AHOST", &[]),
        ("UNAME", &[]),
        ("MANDT", &[]),
        ("MODNO", &[]),
        ("SEQNO", &[]),
        ("FLIST", &[]),
    ]);
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(none)]);
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();

    assert_eq!(
        run_list(&mut transport, BackendKind::AdtHttp, &query, &policies())
            .unwrap()
            .text,
        "(no dumps)"
    );

    let rows: Vec<Option<&str>> = vec![Some("x"); 5000];
    let full = preview(&[
        ("DATUM", &rows),
        ("UZEIT", &rows),
        ("AHOST", &rows),
        ("UNAME", &rows),
        ("MANDT", &rows),
        ("MODNO", &rows),
        ("SEQNO", &rows),
        ("FLIST", &rows),
    ]);
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(full)]);
    let error = run_list(&mut transport, BackendKind::AdtHttp, &query, &policies()).unwrap_err();

    match error {
        PreviewError::Refused(message) => {
            assert!(
                message.contains("--date") && message.contains("--user"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reply_without_the_expected_columns_is_a_failed_read() {
    let wrong = preview(&[("DATUM", &[Some("20261103")])]);
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(wrong)]);
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
    let error = run_list(&mut transport, BackendKind::AdtHttp, &query, &policies()).unwrap_err();

    assert!(
        matches!(&error, PreviewError::Failed { message, .. } if message.contains("no column")),
        "{error:?}"
    );
}

// ---- the refusal of release 7.50 -----------------------------------------------------------------

#[test]
fn adt_http_stops_with_the_hint_when_sap_refuses_snap() {
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), refused()]);
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
    let error = run_list(&mut transport, BackendKind::AdtHttp, &query, &policies()).unwrap_err();

    match error {
        PreviewError::Refused(message) => {
            assert!(message.contains("--backend rfc"), "{message}");
            assert!(message.contains("SNAP"), "{message}");
        }
        other => panic!("{other:?}"),
    }

    assert_eq!(transport.calls.len(), 2, "nothing else was asked");
}

fn endpoint(response: Result<Response, TransportError>) -> RfcEndpointResponse {
    let response = response.unwrap();

    RfcEndpointResponse {
        status: response.status,
        headers: BTreeMap::new(),
        body: response.body,
    }
}

const LIST_WIDTHS: [(&str, usize); 8] = [
    ("DATUM", 8),
    ("UZEIT", 6),
    ("AHOST", 32),
    ("UNAME", 12),
    ("MANDT", 3),
    ("MODNO", 3),
    ("SEQNO", 3),
    ("FLIST", 200),
];

#[test]
fn the_rfc_backend_reads_snap_through_rfc_read_table_when_sap_refuses_it() {
    let first = "FC019MESSAGE_TYPE_X_TEXTAP005ZPROGAI006ZINCL AL0023 ";
    let second = "FC006OTHERXAP005ZPROGAI005ZINCLAL00212";
    let rows = vec![
        vec![
            "20261103", "053724", "apphost", "TESTUSER", "110", "21", "000", first,
        ],
        vec![
            "20261104", "101500", "apphost", "TESTUSER", "110", "2", "000", second,
        ],
    ];
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_table_replies(vec![Ok(read_table_reply(&LIST_WIDTHS, &rows))]),
    );
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
    let text = run_list(&mut transport, BackendKind::RfcSadt, &query, &policies())
        .unwrap()
        .text;

    assert_eq!(
        text,
        "20261104 101500 TESTUSER 110 error=OTHERX program=ZPROG include=ZINCL line=12 id=20261104/101500/apphost/TESTUSER/2\n\
         20261103 053724 TESTUSER 110 error=MESSAGE_TYPE_X_TEXT program=ZPROG include=ZINCL line=3 id=20261103/053724/apphost/TESTUSER/21"
    );

    let calls = transport.api().table_calls();

    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function, "RFC_READ_TABLE");
    assert_eq!(
        calls[0].imports,
        [
            ("QUERY_TABLE".to_owned(), "SNAP".to_owned()),
            ("ROWCOUNT".to_owned(), "5000".to_owned()),
        ]
    );

    let fields: Vec<&str> = calls[0].tables[0]
        .rows
        .iter()
        .map(|row| row[0].1.as_str())
        .collect();

    assert_eq!(
        fields,
        [
            "DATUM", "UZEIT", "AHOST", "UNAME", "MANDT", "MODNO", "SEQNO", "FLIST"
        ]
    );

    let options: Vec<&str> = calls[0].tables[1]
        .rows
        .iter()
        .map(|row| row[0].1.as_str())
        .collect();

    assert!(options.iter().all(|line| line.len() <= 72), "{options:?}");
    assert_eq!(
        options.join(" "),
        "SEQNO = '000' AND DATUM >= '20261117' AND UNAME = 'TESTUSER'"
    );
    assert_eq!(
        transport.api().counts().2,
        2,
        "the dictionary check and the refusal"
    );
}

#[test]
fn the_rfc_where_lines_are_cut_at_72_characters_for_a_long_condition() {
    // The dump id condition is the longest one.
    let id = "20261103/053726/sap-application-server-host-01/TESTUSER/21";
    let query = DumpQuery::new(id, true, "farhan", &policies()).unwrap();
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_table_replies(vec![Err(TransportError::Protocol("stop here".to_owned()))]),
    );
    let error = run_dump(
        &mut transport,
        BackendKind::RfcSadt,
        &query,
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert!(matches!(error, PreviewError::Failed { .. }), "{error:?}");

    let call = &transport.api().table_calls()[0];
    let lines: Vec<&str> = call.tables[1]
        .rows
        .iter()
        .map(|row| row[0].1.as_str())
        .collect();

    assert!(lines.len() > 1, "{lines:?}");
    assert!(lines.iter().all(|line| line.len() <= 72), "{lines:?}");
    assert_eq!(
        lines.join(" "),
        "DATUM = '20261103' AND UZEIT = '053726' AND AHOST = 'sap-application-server-host-01' AND UNAME = 'TESTUSER' AND MODNO = '21'"
    );
}

#[test]
fn a_failed_rfc_read_keeps_its_routing_class() {
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_table_replies(vec![Err(TransportError::PreSendConnectivity(
                "no route".to_owned(),
            ))]),
    );
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
    let error = run_list(&mut transport, BackendKind::RfcSadt, &query, &policies()).unwrap_err();

    assert!(
        matches!(
            &error,
            PreviewError::Failed {
                failure: Failure::PreSendConnectivity,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn another_refusal_or_error_of_the_preview_is_never_answered_with_rfc() {
    let other = Response::text(
        400,
        "<exc:exception><localizedMessage>Unknown column name \"NOPE\"</localizedMessage></exc:exception>",
    );
    let mut transport = RfcTransport::new(FakeRfcApi::with_responses(vec![
        endpoint(dd02l("SNAP")),
        endpoint(Ok(other)),
    ]));
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
    let error = run_list(&mut transport, BackendKind::RfcSadt, &query, &policies()).unwrap_err();

    assert!(matches!(error, PreviewError::Failed { .. }), "{error:?}");
    assert!(transport.api().table_calls().is_empty());
}

#[test]
fn the_transports_without_the_function_module_answer_with_an_error() {
    // ADT HTTP has no function module: the default of the trait answers an error.
    let mut http_like = ScriptedTransport::replies(vec![dd02l("SNAP"), refused()]);
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
    let error = run_list(&mut http_like, BackendKind::RfcSadt, &query, &policies()).unwrap_err();

    assert!(
        matches!(&error, PreviewError::Failed { message, .. } if message.contains("needs the RFC backend")),
        "{error:?}"
    );

    // An RFC API that has no reply scripted.
    let mut rfc = RfcTransport::new(FakeRfcApi::with_responses(vec![
        endpoint(dd02l("SNAP")),
        endpoint(refused()),
    ]));
    let error = run_list(&mut rfc, BackendKind::RfcSadt, &query, &policies()).unwrap_err();

    assert!(
        matches!(&error, PreviewError::Failed { message, .. } if message.contains("no reply")),
        "{error:?}"
    );
}

// ---- dump ---------------------------------------------------------------------------------------

const DUMP_ID: &str = "20261103/053726/apphost/TESTUSER/21";

/// The tags of a synthetic dump: the four that are shown, one that must never be shown, and a long
/// one that moves the later tags across the column boundaries.
fn dump_tags() -> Vec<Tag> {
    vec![
        tag("FC", "CREATE_OBJECT_CLASS_NOT_FOUND"),
        tag("TD", &"d".repeat(150)),
        tag("AP", "ZPROGRAM"),
        tag("ZZ", "RAWMARKER"),
        tag("AI", "ZINCLUDE"),
        tag("TD", &"e".repeat(260)),
        tag("AL", "7"),
    ]
}

fn source_lines() -> String {
    (1..=20)
        .map(|number| format!("line {number}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn flist_name(index: usize) -> String {
    if index == 0 {
        "FLIST".to_owned()
    } else {
        format!("FLIST{:02}", index + 1)
    }
}

/// The preview of the rows of the synthetic dump, three columns per row.
fn dump_rows(stream: &str) -> String {
    dump_rows_with(stream, 3, false, "110")
}

/// The preview of the rows of a dump, `per_row` `FLIST` columns per row, the rows in the reverse
/// order of `SEQNO` when `reversed`.
fn dump_rows_with(stream: &str, per_row: usize, reversed: bool, client: &str) -> String {
    let mut rows: Vec<(String, Vec<String>)> = split_rows(stream, per_row, false)
        .into_iter()
        .enumerate()
        .map(|(n, row)| (format!("{n:03}"), row))
        .collect();

    if reversed {
        rows.reverse();
    }

    let seqno: Vec<Option<&str>> = rows.iter().map(|(n, _)| Some(n.as_str())).collect();
    let mut columns: Vec<(String, Vec<Option<&str>>)> = vec![
        ("SEQNO".to_owned(), seqno),
        ("MANDT".to_owned(), vec![Some(client); rows.len()]),
    ];

    for index in 0..per_row {
        columns.push((
            flist_name(index),
            rows.iter()
                .map(|(_, row)| Some(row[index].as_str()))
                .collect(),
        ));
    }

    let borrowed: Vec<(&str, &[Option<&str>])> = columns
        .iter()
        .map(|(name, values)| (name.as_str(), values.as_slice()))
        .collect();

    preview(&borrowed)
}

fn snapt(lines: &[&str]) -> String {
    let seqno: Vec<String> = (0..lines.len()).map(|n| format!("{n:03}")).collect();
    let seqno: Vec<Option<&str>> = seqno.iter().map(|s| Some(s.as_str())).collect();
    let text: Vec<Option<&str>> = lines.iter().map(|line| Some(*line)).collect();

    preview(&[("SEQNO", &seqno), ("TLINE", &text)])
}

fn dump_query(all: bool) -> DumpQuery {
    DumpQuery::new(DUMP_ID, all, "TESTUSER", &policies()).unwrap()
}

#[test]
fn a_dump_prints_the_decoded_fields_the_short_text_and_the_source_around_the_line() {
    let stream = stream_of(&dump_tags());

    assert!(stream.len() > 400, "the tags must cross the first row");

    let mut transport = ScriptedTransport::replies(vec![
        dd02l("SNAP"),
        ok(dump_rows(&stream)),
        dd02l("SNAPT"),
        ok(snapt(&["A class was not found.", "Second line."])),
        ok(source_lines()),
    ]);
    let text = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert_eq!(
        text,
        "error: CREATE_OBJECT_CLASS_NOT_FOUND\n\
         program: ZPROGRAM\n\
         include: ZINCLUDE\n\
         line: 7\n\
         user: TESTUSER\n\
         client: 110\n\
         short text:\n\
         A class was not found.\n\
         Second line.\n\
         source ZINCLUDE:\n\
         \u{20}     2 line 2\n\
         \u{20}     3 line 3\n\
         \u{20}     4 line 4\n\
         \u{20}     5 line 5\n\
         \u{20}     6 line 6\n\
         >     7 line 7\n\
         \u{20}     8 line 8\n\
         \u{20}     9 line 9\n\
         \u{20}    10 line 10\n\
         \u{20}    11 line 11\n\
         \u{20}    12 line 12"
    );
    assert!(!text.contains("RAWMARKER") && !text.contains("dddd"));

    let calls = &transport.calls;

    assert_eq!(
        calls[1].uri,
        "/sap/bc/adt/datapreview/ddic?rowNumber=1000&ddicEntityName=SNAP"
    );
    assert_eq!(
        String::from_utf8(calls[1].body.clone()).unwrap(),
        "SELECT * FROM SNAP WHERE DATUM = '20261103' AND UZEIT = '053726' AND AHOST = 'apphost' AND UNAME = 'TESTUSER' AND MODNO = '21'"
    );
    assert_eq!(
        String::from_utf8(calls[3].body.clone()).unwrap(),
        "SELECT SEQNO, TLINE FROM SNAPT WHERE LANGU = 'E' AND ERRID = 'CREATE_OBJECT_CLASS_NOT_FOUND' AND TTYPE = 'K'"
    );
    assert_eq!(
        calls[4].uri,
        "/sap/bc/adt/programs/programs/zinclude/source/main?version=active"
    );
}

#[test]
fn the_source_window_starts_at_line_one_near_the_top_and_stops_at_the_end() {
    for (line, shown) in [("2", (1, 7)), ("19", (14, 20)), ("99", (0, 0))] {
        let tags = vec![tag("FC", "X_ERROR"), tag("AI", "ZINCLUDE"), tag("AL", line)];
        let mut transport = ScriptedTransport::replies(vec![
            dd02l("SNAP"),
            ok(dump_rows(&stream_of(&tags))),
            dd02l("SNAPT"),
            ok(snapt(&[""])),
            // No text in English: the logon language is English too, so any language is tried.
            dd02l("SNAPT"),
            ok(snapt(&[""])),
            ok(source_lines()),
        ]);
        let text = run_dump(
            &mut transport,
            BackendKind::AdtHttp,
            &dump_query(false),
            &policies(),
            "EN",
        )
        .unwrap();
        let numbers: Vec<usize> = text
            .lines()
            .skip_while(|l| !l.starts_with("source"))
            .skip(1)
            .map(|l| l[2..7].trim().parse().unwrap())
            .collect();

        assert!(
            !text.contains("short text"),
            "an empty text is not shown: {text}"
        );

        if shown == (0, 0) {
            assert!(numbers.is_empty(), "{numbers:?}");
        } else {
            assert_eq!((numbers[0], *numbers.last().unwrap()), shown, "{line}");
        }
    }
}

#[test]
fn a_dump_without_the_optional_parts_still_prints_its_fields() {
    let tags = vec![tag("FC", "X_ERROR"), tag("AP", "ZPROGRAM")];
    let mut transport = ScriptedTransport::replies(vec![
        dd02l("SNAP"),
        ok(dump_rows(&stream_of(&tags))),
        dd02l("SNAPT"),
        Ok(Response::text(
            400,
            "<exc:exception><localizedMessage>no</localizedMessage></exc:exception>",
        )),
    ]);
    let text = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert_eq!(
        text,
        "error: X_ERROR\nprogram: ZPROGRAM\ninclude: -\nline: -\nuser: TESTUSER\nclient: 110"
    );
}

#[test]
fn an_unreadable_source_is_a_note_and_the_dump_still_succeeds() {
    let tags = vec![tag("FC", "X_ERROR"), tag("AI", "ZNOWHERE"), tag("AL", "7")];
    let mut transport = ScriptedTransport::replies(vec![
        dd02l("SNAP"),
        ok(dump_rows(&stream_of(&tags))),
        dd02l("SNAPT"),
        ok(snapt(&["Short."])),
        // The source, then the program and the include of that name.
        Ok(Response::empty(404)),
        Ok(Response::empty(404)),
        Ok(Response::empty(404)),
    ]);
    let text = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert!(
        text.contains("(source ZNOWHERE unreadable: ") && text.contains("NOT_FOUND"),
        "{text}"
    );
    assert!(text.contains("short text:\nShort."), "{text}");
}

/// The dump of one include and line, with the text of the error found and `extra` replies after.
fn dump_with_include(
    include: &str,
    line: &str,
    extra: Vec<Result<Response, TransportError>>,
) -> (String, ScriptedTransport) {
    let tags = vec![tag("FC", "X_ERROR"), tag("AI", include), tag("AL", line)];
    let mut replies = vec![
        dd02l("SNAP"),
        ok(dump_rows(&stream_of(&tags))),
        dd02l("SNAPT"),
        ok(snapt(&["Short."])),
    ];

    replies.extend(extra);

    let mut transport = ScriptedTransport::replies(replies);
    let text = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    (text, transport)
}

#[test]
fn an_include_name_of_the_dump_is_checked_before_it_reaches_a_uri() {
    for hostile in [
        "ZCL_X====CM001",
        "../../ETC",
        "ZINC?x=1",
        "ZINC#frag",
        "ZINC%2F..%2F",
        "ZINC/..",
        "Z INC",
        "ZINC\u{1b}[0m",
        "//host/x",
    ] {
        let (text, transport) = dump_with_include(hostile, "7", Vec::new());

        assert_eq!(transport.calls.len(), 4, "{hostile:?}: nothing was read");
        assert!(
            text.contains("not read: not a program or include name)"),
            "{hostile:?}: {text}"
        );
        assert!(!text.contains('\u{1b}'), "{hostile:?}");
    }
}

#[test]
fn a_line_number_of_any_size_never_overflows_the_window() {
    for line in [
        "18446744073709551615",
        "18446744073709551610",
        "99999999999999999999",
        "99999999999999999999999",
        "21",
        "0",
        "-3",
    ] {
        let (text, _) = dump_with_include("ZINCLUDE", line, vec![ok(source_lines())]);

        assert!(text.starts_with("error: X_ERROR\n"), "{line}: {text}");
        assert!(!text.contains("source ZINCLUDE:"), "{line}: {text}");
    }

    // The last line of the source still has its window.
    let (text, _) = dump_with_include("ZINCLUDE", "20", vec![ok(source_lines())]);

    assert!(text.contains(">    20 line 20"), "{text}");
}

#[test]
fn the_short_text_falls_back_to_the_logon_language_and_then_to_the_first_language() {
    let tags = vec![tag("FC", "X_ERROR")];
    let sequence = |language: &str, replies: Vec<Result<Response, TransportError>>| {
        let mut all = vec![dd02l("SNAP"), ok(dump_rows(&stream_of(&tags)))];

        all.extend(replies);

        let mut transport = ScriptedTransport::replies(all);
        let text = run_dump(
            &mut transport,
            BackendKind::AdtHttp,
            &dump_query(false),
            &policies(),
            language,
        )
        .unwrap();

        (text, transport)
    };
    let (text, transport) = sequence(
        "DE",
        vec![
            dd02l("SNAPT"),
            ok(snapt(&[])),
            dd02l("SNAPT"),
            ok(snapt(&["Deutscher Text."])),
        ],
    );
    let body = |index: usize| String::from_utf8(transport.calls[index].body.clone()).unwrap();

    assert!(text.ends_with("short text:\nDeutscher Text."), "{text}");
    assert!(
        body(3).contains("LANGU = 'E' AND ERRID = 'X_ERROR'"),
        "{}",
        body(3)
    );
    assert!(
        body(5).contains("LANGU = 'D' AND ERRID = 'X_ERROR'"),
        "{}",
        body(5)
    );

    // An unknown language skips the second step; the last one keeps the rows of one language.
    let any = ok(preview(&[
        (
            "SEQNO",
            &[Some("000"), Some("001"), Some("000"), Some("001")],
        ),
        (
            "TLINE",
            &[Some("Texte"), Some("deux"), Some("Text"), Some("zwei")],
        ),
        ("LANGU", &[Some("F"), Some("F"), Some("D"), Some("D")]),
    ]));
    let (text, transport) = sequence(
        "XX",
        vec![dd02l("SNAPT"), ok(snapt(&[])), dd02l("SNAPT"), any],
    );

    assert!(text.ends_with("short text:\nText\nzwei"), "{text}");
    assert_eq!(transport.calls.len(), 6);
    assert_eq!(
        String::from_utf8(transport.calls[5].body.clone()).unwrap(),
        "SELECT SEQNO, TLINE, LANGU FROM SNAPT WHERE ERRID = 'X_ERROR' AND TTYPE = 'K'"
    );
}

#[test]
fn a_dump_that_fills_the_row_cap_is_refused_as_cut() {
    let rows: Vec<String> = (0..1000).map(|n| format!("{n:03}")).collect();
    let seqno: Vec<Option<&str>> = rows.iter().map(|r| Some(r.as_str())).collect();
    let flist: Vec<Option<&str>> = vec![Some("FC007X_ERROR"); 1000];
    let full = preview(&[("SEQNO", &seqno), ("MANDT", &flist), ("FLIST", &flist)]);
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(full)]);
    let error = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert!(
        matches!(&error, PreviewError::Refused(m) if m.contains("1000 or more rows")),
        "{error:?}"
    );
}

#[test]
fn a_listed_row_whose_id_dump_would_refuse_says_so_and_its_values_are_cleaned() {
    let rows = |host: &'static str, user: &'static str| {
        preview(&[
            ("DATUM", &[Some("20261103")]),
            ("UZEIT", &[Some("053726")]),
            ("AHOST", &[Some(host)]),
            ("UNAME", &[Some(user)]),
            ("MANDT", &[Some("110")]),
            ("MODNO", &[Some("21 ")]),
            ("SEQNO", &[Some("000")]),
            ("FLIST", &[Some("FC007X_ERROR")]),
        ])
    };
    let line = |host: &'static str, user: &'static str| {
        let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(rows(host, user))]);
        let all = ListOptions {
            all: true,
            ..options()
        };
        let query = ListQuery::new(&all, "TESTUSER", TODAY, &policies()).unwrap();

        run_list(&mut transport, BackendKind::AdtHttp, &query, &policies())
            .unwrap()
            .text
    };

    assert!(line("app'host", "TESTUSER").ends_with(" id=unusable"));
    assert!(line("apphost", "TEST\u{1b}USER").ends_with(" id=unusable"));
    assert!(!line("apphost", "TEST\u{1b}USER").contains('\u{1b}'));
    assert!(line("a/b", "TESTUSER").ends_with(" id=unusable"));

    let usable = line("ä-host.1", "ÜSER#1");

    assert!(usable.contains(" ÜSER#1 110 "), "{usable}");
    assert!(
        usable.ends_with(" id=20261103/053726/ä-host.1/ÜSER#1/21"),
        "{usable}"
    );
    assert!(DumpId::parse("20261103/053726/ä-host.1/ÜSER#1/21").is_ok());
}

#[test]
fn control_characters_of_a_dump_never_reach_the_output() {
    let tags = vec![
        tag("FC", "X_ERROR\nSTOP: forged"),
        tag("AP", "ZPROG\u{1b}[31m"),
    ];
    let mut transport = ScriptedTransport::replies(vec![
        dd02l("SNAP"),
        ok(dump_rows(&stream_of(&tags))),
        dd02l("SNAPT"),
        ok(snapt(&[])),
    ]);
    let text = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert!(!text.contains('\u{1b}'));
    assert_eq!(text.lines().next(), Some("error: X_ERROR STOP: forged"));
}

#[test]
fn a_dump_that_does_not_exist_is_a_rejection() {
    let empty = preview(&[("SEQNO", &[]), ("MANDT", &[]), ("FLIST", &[])]);
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(empty)]);
    let error = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert_eq!(
        error,
        PreviewError::Rejected(format!("dump not found: {DUMP_ID}"))
    );
}

#[test]
fn adt_http_stops_with_the_hint_when_sap_refuses_the_dump_rows() {
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), refused()]);
    let error = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert!(
        matches!(&error, PreviewError::Refused(message) if message.contains("--backend rfc")),
        "{error:?}"
    );
}

#[test]
fn the_rfc_backend_reads_a_dump_field_by_field_and_skips_columns_the_release_lacks() {
    let stream = stream_of(&dump_tags());
    let columns = split_rows(&stream, 3, true);
    let column = |at: usize| -> Vec<&str> { columns.iter().map(|row| row[at].as_str()).collect() };
    let seqno: Vec<String> = (0..columns.len()).map(|n| format!("{n:03}")).collect();
    let numbers: Vec<&str> = seqno.iter().map(String::as_str).collect();
    let with_client: Vec<Vec<&str>> = (0..columns.len())
        .map(|row| vec![numbers[row], "110", column(0)[row]])
        .collect();
    let single = |at: usize| -> Vec<Vec<&str>> {
        (0..columns.len())
            .map(|row| vec![numbers[row], column(at)[row]])
            .collect()
    };
    let missing = || {
        Err(TransportError::PostSendUncertain(
            "RfcInvoke [FIELD_NOT_VALID]: field FLIST04 is not valid".to_owned(),
        ))
    };
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![
            endpoint(dd02l("SNAP")),
            endpoint(refused()),
            endpoint(dd02l("SNAPT")),
            endpoint(refused_for("SNAPT")),
            endpoint(ok(source_lines())),
        ])
        .with_table_replies(vec![
            Ok(read_table_reply(
                &[("SEQNO", 3), ("MANDT", 3), ("FLIST", 200)],
                &with_client,
            )),
            Ok(read_table_reply(
                &[("SEQNO", 3), ("FLIST02", 200)],
                &single(1),
            )),
            Ok(read_table_reply(
                &[("SEQNO", 3), ("FLIST03", 200)],
                &single(2),
            )),
            missing(),
            missing(),
            missing(),
            missing(),
            missing(),
            Ok(read_table_reply(
                &[("SEQNO", 3), ("TLINE", 255)],
                &[vec!["000", "Read through RFC."]],
            )),
        ]),
    );
    let text = run_dump(
        &mut transport,
        BackendKind::RfcSadt,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert!(text.starts_with("error: CREATE_OBJECT_CLASS_NOT_FOUND\nprogram: ZPROGRAM\ninclude: ZINCLUDE\nline: 7\nuser: TESTUSER\nclient: 110\nshort text:\nRead through RFC.\nsource ZINCLUDE:\n"), "{text}");
    assert!(!text.contains("RAWMARKER"));

    let calls = transport.api().table_calls();
    let fields = |call: &abapreach::transport::rfc::FunctionCall| -> Vec<String> {
        call.tables[0].rows.iter().map(|r| r[0].1.clone()).collect()
    };

    assert_eq!(calls.len(), 9);
    assert_eq!(fields(&calls[0]), ["SEQNO", "MANDT", "FLIST"]);
    assert_eq!(fields(&calls[1]), ["SEQNO", "FLIST02"]);
    assert_eq!(fields(&calls[7]), ["SEQNO", "FLIST08"]);
    assert_eq!(calls[8].imports[0].1, "SNAPT");
    // The query of the dump caps the rows of every call.
    assert!(
        calls[..8]
            .iter()
            .all(|call| call.imports[1] == ("ROWCOUNT".to_owned(), "1000".to_owned()))
    );
}

const SINGLE_BYTE_STOP: &str = "this RFC library uses single-byte characters and cannot read the dump text of SNAP on this release; use the SAP NW RFC SDK (ABAPREACH_RFC_LIBRARY) to read it";

#[test]
fn a_single_byte_rfc_library_refuses_dump_before_any_read_table_call() {
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_width(UcWidth::Narrow),
    );
    let error = run_dump(
        &mut transport,
        BackendKind::RfcSadt,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert!(
        matches!(&error, PreviewError::Refused(message) if message == SINGLE_BYTE_STOP),
        "{error:?}"
    );
    assert!(transport.api().table_calls().is_empty());
}

#[test]
fn a_wide_rfc_library_still_reads_a_dump_through_rfc_read_table() {
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_width(UcWidth::Utf16)
            .with_table_replies(vec![Err(TransportError::PostSendUncertain(
                "RfcInvoke [FIELD_NOT_VALID]: field FLIST is not valid".to_owned(),
            ))]),
    );
    let error = run_dump(
        &mut transport,
        BackendKind::RfcSadt,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert!(matches!(error, PreviewError::Failed { .. }), "{error:?}");
    assert_eq!(transport.api().table_calls().len(), 1);
}

#[test]
fn dumps_still_lists_on_a_single_byte_rfc_library() {
    let rows = vec![vec![
        "20261103",
        "053724",
        "apphost",
        "TESTUSER",
        "110",
        "21",
        "000",
        "FC006OTHERXAP005ZPROGAI005ZINCLAL00212",
    ]];
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_width(UcWidth::Narrow)
            .with_table_replies(vec![Ok(read_table_reply(&LIST_WIDTHS, &rows))]),
    );
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
    let text = run_list(&mut transport, BackendKind::RfcSadt, &query, &policies())
        .unwrap()
        .text;

    assert!(text.contains("error=OTHERX"), "{text}");
    assert_eq!(transport.api().table_calls().len(), 1);
}

fn refused_for(table: &str) -> Result<Response, TransportError> {
    Ok(Response::text(
        400,
        fixture("snap-list-750-400.xml").replace(
            REFUSAL,
            &format!("You cannot display {table} with the standard tools"),
        ),
    ))
}

#[test]
fn a_missing_first_column_group_is_an_error_not_a_skip() {
    let mut transport = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_table_replies(vec![Err(TransportError::PostSendUncertain(
                "RfcInvoke [FIELD_NOT_VALID]: field FLIST is not valid".to_owned(),
            ))]),
    );
    let error = run_dump(
        &mut transport,
        BackendKind::RfcSadt,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert!(matches!(error, PreviewError::Failed { .. }), "{error:?}");
    assert_eq!(transport.api().table_calls().len(), 1);
}

#[test]
fn a_malformed_function_reply_is_an_error() {
    for bad in [
        FunctionResult::default(),
        FunctionResult {
            exports: Vec::new(),
            tables: vec![
                (
                    "FIELDS".to_owned(),
                    vec![vec![
                        Cell::Text("A".to_owned()),
                        Cell::Text("x".to_owned()),
                        Cell::Text("3".to_owned()),
                    ]],
                ),
                ("DATA".to_owned(), vec![]),
            ],
        },
        FunctionResult {
            exports: Vec::new(),
            tables: vec![
                (
                    "FIELDS".to_owned(),
                    vec![vec![
                        Cell::Text("A".to_owned()),
                        Cell::Text("500".to_owned()),
                        Cell::Text("30".to_owned()),
                    ]],
                ),
                ("DATA".to_owned(), vec![]),
            ],
        },
    ] {
        let mut transport = RfcTransport::new(
            FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
                .with_table_replies(vec![Ok(bad)]),
        );
        let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();
        let error =
            run_list(&mut transport, BackendKind::RfcSadt, &query, &policies()).unwrap_err();

        assert!(
            matches!(&error, PreviewError::Failed { message, .. } if message.contains("unreadable reply")),
            "{error:?}"
        );
    }
}

#[test]
fn the_client_and_every_printed_field_of_a_dump_are_cleaned() {
    let tags = vec![tag("FC", "X_ERROR")];
    let mut transport = ScriptedTransport::replies(vec![
        dd02l("SNAP"),
        ok(dump_rows_with(&stream_of(&tags), 3, false, "\u{1b}[31m110")),
        dd02l("SNAPT"),
        ok(snapt(&["Text\u{1b}[0m"])),
    ]);
    let text = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.contains("client:  [31m110\n"), "{text:?}");
}

#[test]
fn the_rows_of_a_dump_are_put_together_in_seqno_order_whatever_the_reply_order() {
    // Eight columns per row, filled: the stream crosses every FLIST column and a second row.
    let tags = vec![
        tag("FC", "X_ERROR"),
        tag("AP", "ZPROGRAM"),
        tag("TD", &"d".repeat(900)),
        tag("TD", &"e".repeat(800)),
        tag("AI", "ZINCLUDE"),
        tag("AL", "7"),
    ];
    let stream = stream_of(&tags);

    assert!(stream.chars().count() > 1600 && stream.chars().count() < 3200);

    for (per_row, reversed) in [(8, false), (8, true), (3, true), (1, true)] {
        let mut transport = ScriptedTransport::replies(vec![
            dd02l("SNAP"),
            ok(dump_rows_with(&stream, per_row, reversed, "110")),
            dd02l("SNAPT"),
            ok(snapt(&["Text."])),
            ok(source_lines()),
        ]);
        let text = run_dump(
            &mut transport,
            BackendKind::AdtHttp,
            &dump_query(false),
            &policies(),
            "EN",
        )
        .unwrap();

        assert!(
            text.starts_with(
                "error: X_ERROR\nprogram: ZPROGRAM\ninclude: ZINCLUDE\nline: 7\nuser: TESTUSER\nclient: 110\n"
            ),
            "{per_row} {reversed}: {text}"
        );
        assert!(text.contains(">     7 line 7"), "{per_row} {reversed}");
    }
}

#[test]
fn a_listing_drops_the_rows_of_other_users_and_says_so() {
    let rows = preview(&[
        (
            "DATUM",
            &[Some("20261103"), Some("20261104"), Some("20261105")],
        ),
        ("UZEIT", &[Some("053726"), Some("053727"), Some("053728")]),
        (
            "AHOST",
            &[Some("apphost"), Some("apphost"), Some("apphost")],
        ),
        (
            "UNAME",
            &[Some("TESTUSER"), Some("OTHER"), Some("testuser")],
        ),
        ("MANDT", &[Some("110"), Some("110"), Some("110")]),
        ("MODNO", &[Some("21"), Some("21"), Some("22")]),
        ("SEQNO", &[Some("000"), Some("000"), Some("000")]),
        (
            "FLIST",
            &[
                Some("FC007X_ERROR"),
                Some("FC007X_ERROR"),
                Some("FC007X_ERROR"),
            ],
        ),
    ]);
    let list = |options: ListOptions<'_>| {
        let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(rows.clone())]);
        let query = ListQuery::new(&options, "TESTUSER", TODAY, &policies()).unwrap();

        run_list(&mut transport, BackendKind::AdtHttp, &query, &policies()).unwrap()
    };

    // The logon user, then a named user: the same check.
    for named in [None, Some("TestUser")] {
        let listing = list(ListOptions {
            user: named,
            ..options()
        });

        assert_eq!(listing.text.lines().count(), 2, "{}", listing.text);
        assert!(!listing.text.contains("OTHER"));
        assert_eq!(
            listing.warning.as_deref(),
            Some(
                "W: 1 rows of other users than TESTUSER were dropped: the reply did not keep to the user filter"
            )
        );
    }

    let other = list(ListOptions {
        user: Some("other"),
        ..options()
    });

    assert_eq!(other.text.lines().count(), 1);
    assert!(other.text.contains(" OTHER "));
    assert!(other.warning.unwrap().starts_with("W: 2 rows"));

    // --all keeps every row and has nothing to drop.
    let all = list(ListOptions {
        all: true,
        ..options()
    });

    assert_eq!(all.text.lines().count(), 3);
    assert_eq!(all.warning, None);

    let honest = {
        let mut transport =
            ScriptedTransport::replies(vec![dd02l("SNAP"), ok(fixture("snap-list-758.xml"))]);
        let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();

        run_list(&mut transport, BackendKind::AdtHttp, &query, &policies()).unwrap()
    };

    assert_eq!(honest.warning, None);
}

fn list_row_reply(rows: usize) -> FunctionResult {
    let row = [
        "20261103",
        "053726",
        "apphost",
        "TESTUSER",
        "110",
        "21",
        "000",
        "FC007X_ERROR",
    ];

    read_table_reply(&LIST_WIDTHS, &vec![row.to_vec(); rows])
}

#[test]
fn a_reply_past_the_cap_of_the_query_is_refused_on_both_paths() {
    let query = ListQuery::new(&options(), "TESTUSER", TODAY, &policies()).unwrap();

    // RFC: ROWCOUNT only asks, so a longer reply breaks the cap of the query.
    let mut rfc = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_table_replies(vec![Ok(list_row_reply(5001))]),
    );
    let error = run_list(&mut rfc, BackendKind::RfcSadt, &query, &policies()).unwrap_err();

    assert!(
        matches!(&error, PreviewError::Failed { message, .. } if message.contains("returned 5001 rows for a cap of 5000")),
        "{error:?}"
    );

    // RFC at exactly the cap is a cut list.
    let mut rfc = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_table_replies(vec![Ok(list_row_reply(5000))]),
    );
    let error = run_list(&mut rfc, BackendKind::RfcSadt, &query, &policies()).unwrap_err();

    assert!(matches!(error, PreviewError::Refused(_)), "{error:?}");

    // Preview: the 750 release answers one row more than asked, and more is cut at the cap.
    let many: Vec<Option<&str>> = vec![Some("x"); 5002];
    let full = preview(&[
        ("DATUM", &many),
        ("UZEIT", &many),
        ("AHOST", &many),
        ("UNAME", &many),
        ("MANDT", &many),
        ("MODNO", &many),
        ("SEQNO", &many),
        ("FLIST", &many),
    ]);
    let mut transport = ScriptedTransport::replies(vec![dd02l("SNAP"), ok(full)]);
    let error = run_list(&mut transport, BackendKind::AdtHttp, &query, &policies()).unwrap_err();

    assert!(matches!(error, PreviewError::Refused(_)), "{error:?}");
}

#[test]
fn a_dump_reply_past_its_cap_is_refused_on_the_rfc_path_too() {
    let rows: Vec<Vec<&str>> = (0..1001)
        .map(|_| vec!["000", "110", "FC007X_ERROR"])
        .collect();
    let mut rfc = RfcTransport::new(
        FakeRfcApi::with_responses(vec![endpoint(dd02l("SNAP")), endpoint(refused())])
            .with_table_replies(vec![Ok(read_table_reply(
                &[("SEQNO", 3), ("MANDT", 3), ("FLIST", 200)],
                &rows,
            ))]),
    );
    let error = run_dump(
        &mut rfc,
        BackendKind::RfcSadt,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap_err();

    assert!(
        matches!(&error, PreviewError::Failed { message, .. } if message.contains("returned 1001 rows for a cap of 1000")),
        "{error:?}"
    );
}

#[test]
fn the_short_text_is_cut_at_its_cap_on_the_preview_and_dropped_on_the_rfc_path() {
    let tags = vec![tag("FC", "X_ERROR")];
    let lines = vec!["line"; 105];
    let mut transport = ScriptedTransport::replies(vec![
        dd02l("SNAP"),
        ok(dump_rows(&stream_of(&tags))),
        dd02l("SNAPT"),
        ok(snapt(&lines)),
    ]);
    let text = run_dump(
        &mut transport,
        BackendKind::AdtHttp,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert_eq!(text.lines().filter(|l| *l == "line").count(), 100, "{text}");

    // RFC: a reply of 101 rows for a cap of 100 is no text at all.
    let stream = stream_of(&tags);
    let columns = split_rows(&stream, 3, true);
    let mut replies: Vec<Result<FunctionResult, TransportError>> = vec![Ok(read_table_reply(
        &[("SEQNO", 3), ("MANDT", 3), ("FLIST", 200)],
        &[vec!["000", "110", columns[0][0].as_str()]],
    ))];

    replies.extend((0..7).map(|_| {
        Err(TransportError::PostSendUncertain(
            "RfcInvoke [FIELD_NOT_VALID]".to_owned(),
        ))
    }));

    let rows: Vec<Vec<&str>> = (0..101).map(|_| vec!["000", "line"]).collect();

    replies.push(Ok(read_table_reply(&[("SEQNO", 3), ("TLINE", 255)], &rows)));

    let mut rfc = RfcTransport::new(
        FakeRfcApi::with_responses(vec![
            endpoint(dd02l("SNAP")),
            endpoint(refused()),
            endpoint(dd02l("SNAPT")),
            endpoint(refused_for("SNAPT")),
        ])
        .with_table_replies(replies),
    );
    let text = run_dump(
        &mut rfc,
        BackendKind::RfcSadt,
        &dump_query(false),
        &policies(),
        "EN",
    )
    .unwrap();

    assert!(!text.contains("short text"), "{text}");
    assert!(text.starts_with("error: X_ERROR\n"), "{text}");
}
