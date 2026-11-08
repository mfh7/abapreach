use abapreach::operation::payload::{Kind, MAX_XML_BYTES, Mode, Request, encode};

const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

const ALL_KINDS: [Kind; 5] = [Kind::Dtel, Kind::Doma, Kind::Tabl, Kind::Ttyp, Kind::Tran];
const DDIC_KINDS: [Kind; 4] = [Kind::Dtel, Kind::Doma, Kind::Tabl, Kind::Ttyp];
const ALL_MODES: [Mode; 7] = [
    Mode::Read,
    Mode::Simulate,
    Mode::Deploy,
    Mode::CreateSim,
    Mode::Create,
    Mode::DeleteSim,
    Mode::Delete,
];

// ---------------------------------------------------------------------------------------------
// Independent oracle: base64 decoder and document parser written for the tests only.

fn safe_alphabet(line: &str) -> bool {
    line.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
}

fn base64_decode(text: &str) -> Vec<u8> {
    let value = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("byte {c} outside the base64 alphabet"),
        }
    };
    let bytes = text.as_bytes();
    assert_eq!(bytes.len() % 4, 0, "padded base64 has a multiple of 4");
    let mut out = Vec::new();

    for quad in bytes.chunks(4) {
        let pad = quad.iter().filter(|&&c| c == b'=').count();
        assert!(quad[..4 - pad].iter().all(|&c| c != b'='));
        let mut acc = 0u32;
        for &c in &quad[..4 - pad] {
            acc = (acc << 6) | value(c);
        }
        acc <<= 6 * pad as u32;
        let triple = [(acc >> 16) as u8, (acc >> 8) as u8, acc as u8];
        out.extend_from_slice(&triple[..3 - pad]);
    }

    out
}

fn decode(lines: &[String]) -> Vec<(String, String)> {
    let text = String::from_utf8(base64_decode(&lines.concat())).expect("document is UTF-8");
    assert!(text.ends_with('\n'));

    text[..text.len() - 1]
        .split('\n')
        .map(|line| {
            let (key, value) = line.split_once('=').expect("KEY=VALUE");
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

fn values(doc: &[(String, String)], key: &str) -> Vec<String> {
    doc.iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .collect()
}

fn one(doc: &[(String, String)], key: &str) -> String {
    let all = values(doc, key);
    assert_eq!(all.len(), 1, "{key} must occur once in {doc:?}");
    all[0].clone()
}

fn assert_safe(lines: &[String]) {
    assert!(!lines.is_empty());
    for line in lines {
        assert!(
            !line.is_empty() && line.len() <= 200,
            "line width {}",
            line.len()
        );
        assert!(safe_alphabet(line), "unsafe line {line:?}");
    }
    // Only the last line may be shorter than the line width.
    for line in &lines[..lines.len() - 1] {
        assert_eq!(line.len(), 200);
    }
}

// ---------------------------------------------------------------------------------------------
// Fixtures.

fn ddic_xml(kind: Kind, name: &str, text: &str) -> String {
    let (record, name_tag) = match kind {
        Kind::Dtel => ("DD04V", "ROLLNAME"),
        Kind::Doma => ("DD01V", "DOMNAME"),
        Kind::Tabl => ("DD02V", "TABNAME"),
        Kind::Ttyp => ("DD40V", "TYPENAME"),
        Kind::Tran => panic!("TRAN has no XML"),
    };
    let rows = if kind == Kind::Tabl {
        format!(
            "   <DD03P_TABLE>\n    <DD03P>\n     <TABNAME>{name}</TABNAME>\n     \
             <FIELDNAME>MANDT</FIELDNAME>\n    </DD03P>\n    <DD03P>\n     \
             <TABNAME>{name}</TABNAME>\n     <FIELDNAME>KEY1</FIELDNAME>\n     \
             <DDTEXT>field text</DDTEXT>\n    </DD03P>\n   </DD03P_TABLE>\n"
        )
    } else {
        String::new()
    };

    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <abapGit version=\"v1.0.0\" serializer=\"LCL_OBJECT_X\" serializer_version=\"v1.0.0\">\n \
         <asx:abap xmlns:asx=\"http://www.sap.com/abapxml\" version=\"1.0\">\n  <asx:values>\n   \
         <{record}>\n    <{name_tag}>{name}</{name_tag}>\n    <DDLANGUAGE>E</DDLANGUAGE>\n    \
         <DDTEXT>{text}</DDTEXT>\n   </{record}>\n{rows}  </asx:values>\n </asx:abap>\n</abapGit>"
    )
}

fn xml_for(kind: Kind) -> String {
    ddic_xml(kind, "ZOBJ", "Object text")
}

/// A minimal valid request for every mode and kind: the base of the negative tests.
fn valid(mode: Mode, kind: Kind, xml: &'static str) -> Request<'static> {
    let mut request = Request::new(mode, kind, "ZOBJ", NONCE);
    let tran = kind == Kind::Tran;
    let create = matches!(mode, Mode::Create | Mode::CreateSim);

    if matches!(
        mode,
        Mode::Simulate | Mode::Deploy | Mode::CreateSim | Mode::Create
    ) && !tran
    {
        request.xml = Some(xml);
    }
    if tran && create {
        request.report = Some("ZREPORT");
        request.text = Some("Transaction text");
    }
    if mode == Mode::Deploy {
        request.base = Some(HASH_A);
    }
    if mode == Mode::Delete {
        request.plan = Some(HASH_B);
    }
    request
}

type Setter = fn(&mut Request<'_>, &str);

fn xml_mode_of(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Simulate | Mode::Deploy | Mode::CreateSim | Mode::Create
    )
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn xml_static(kind: Kind) -> &'static str {
    leak(xml_for(kind))
}

fn valid_all() -> Vec<(Mode, Kind, Request<'static>)> {
    let mut out = Vec::new();
    for mode in ALL_MODES {
        for kind in ALL_KINDS {
            if kind == Kind::Tran && matches!(mode, Mode::Simulate | Mode::Deploy) {
                continue;
            }
            let xml = if kind == Kind::Tran {
                ""
            } else {
                xml_static(kind)
            };
            out.push((mode, kind, valid(mode, kind, xml)));
        }
    }
    out
}

/// Small deterministic generator for the property tests (no extra dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn up_to(&mut self, alphabet: &[char], max: usize) -> String {
        let len = self.below(max);
        self.string(alphabet, len)
    }

    fn string(&mut self, alphabet: &[char], len: usize) -> String {
        (0..len)
            .map(|_| alphabet[self.below(alphabet.len())])
            .collect()
    }
}

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn err(request: &Request<'_>) -> String {
    encode(request).expect_err("request must be refused")
}

fn encoded(request: &Request<'_>) -> Vec<String> {
    encode(request).unwrap_or_else(|error| panic!("{request:?} refused: {error}"))
}

// ---------------------------------------------------------------------------------------------
// Round trip and alphabet.

#[test]
fn every_valid_mode_and_kind_encodes_into_the_safe_alphabet_and_round_trips() {
    for (mode, kind, request) in valid_all() {
        let lines = encoded(&request);
        assert_safe(&lines);
        let doc = decode(&lines);

        assert_eq!(one(&doc, "FORMAT"), "1");
        assert_eq!(one(&doc, "NONCE"), NONCE);
        let token = match mode {
            Mode::Read => "READ",
            Mode::Simulate => "SIMULATE",
            Mode::Deploy => "DEPLOY",
            Mode::CreateSim => "CREATE_SIM",
            Mode::Create => "CREATE",
            Mode::DeleteSim => "DELETE_SIM",
            Mode::Delete => "DELETE",
        };
        assert_eq!(one(&doc, "MODE"), token);
        let kind_token = match kind {
            Kind::Dtel => "DTEL",
            Kind::Doma => "DOMA",
            Kind::Tabl => "TABL",
            Kind::Ttyp => "TTYP",
            Kind::Tran => "TRAN",
        };
        assert_eq!(one(&doc, "TYPE"), kind_token);
        assert_eq!(one(&doc, "NAME"), "ZOBJ");
    }
}

#[test]
fn full_deploy_document_carries_every_field_and_the_hostile_xml_text_verbatim() {
    // Quote, brace, backtick, backslash, pipe, unicode and an XML escape inside the XML are safe
    // because the document is base64 and decodes back byte for byte.
    let text = "It's {a} `b` \\ | \" * äöü€日 &amp; &lt;x&gt;";
    let xml = ddic_xml(Kind::Dtel, "ZOBJ", text);
    let mut request = Request::new(Mode::Deploy, Kind::Dtel, "zobj", NONCE);
    request.xml = Some(&xml);
    request.package = None;
    request.tr = Some("hd1k917754");
    request.base = Some(HASH_A);

    let lines = encoded(&request);
    assert_safe(&lines);
    let doc = decode(&lines);

    assert_eq!(one(&doc, "NAME"), "ZOBJ");
    assert_eq!(one(&doc, "TR"), "HD1K917754");
    assert_eq!(one(&doc, "BASE"), HASH_A);
    let rows = values(&doc, "XML");
    assert_eq!(one(&doc, "XMLROWS"), rows.len().to_string());
    assert_eq!(rows.join("\n"), xml);
}

#[test]
fn create_document_carries_package_and_a_tran_carries_report_and_text() {
    let mut request = valid(Mode::Create, Kind::Tran, "");
    request.name = "zt_code";
    request.package = Some("$tmp");
    request.report = Some("zreport_x");
    request.text = Some("Tëxt `with` 'quotes' {and braces}");
    let doc = decode(&encoded(&request));

    assert_eq!(one(&doc, "NAME"), "ZT_CODE");
    assert_eq!(one(&doc, "PACKAGE"), "$TMP");
    assert_eq!(one(&doc, "REPORT"), "ZREPORT_X");
    assert_eq!(one(&doc, "TEXT"), "Tëxt `with` 'quotes' {and braces}");
    assert!(values(&doc, "XML").is_empty());
}

#[test]
fn delete_document_carries_plan_hash_and_the_batch_in_order() {
    let batch = [
        ("TRAN", "zt1"),
        ("TTYP", "ztt"),
        ("TABL", "ztab"),
        ("DTEL", "zde"),
        ("PROG", "zp"),
    ];
    let mut request = valid(Mode::Delete, Kind::Dtel, "");
    request.plan = Some(HASH_B);
    request.batch = &batch;
    let doc = decode(&encoded(&request));

    assert_eq!(one(&doc, "PLAN"), HASH_B);
    assert_eq!(
        values(&doc, "BATCH"),
        ["TRAN:ZT1", "TTYP:ZTT", "TABL:ZTAB", "DTEL:ZDE", "PROG:ZP"]
    );
}

#[test]
fn xml_rows_are_normalized_like_the_hash_and_a_bom_is_dropped() {
    let xml = "\u{feff}<?xml version=\"1.0\"?>  \r\n<abapGit><DD04V><ROLLNAME>ZOBJ</ROLLNAME></DD04V></abapGit>\r\n\r\n";
    let mut request = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
    request.xml = Some(xml);
    let doc = decode(&encoded(&request));

    assert_eq!(
        values(&doc, "XML"),
        [
            "<?xml version=\"1.0\"?>",
            "<abapGit><DD04V><ROLLNAME>ZOBJ</ROLLNAME></DD04V></abapGit>"
        ]
    );
}

#[test]
fn property_random_valid_requests_stay_in_the_alphabet_and_round_trip() {
    let upper = chars("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_");
    let first = chars("ZY");
    let hostile_text = chars("aä日'\"{}`\\|*;&<> €");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);

    for _ in 0..400 {
        let kind = ALL_KINDS[rng.below(ALL_KINDS.len())];
        let max = if kind == Kind::Tran { 20 } else { 30 };
        let name = format!("{}{}", rng.string(&first, 1), rng.up_to(&upper, max));
        let text = format!("T{}", rng.up_to(&hostile_text, 30));
        let xml = if kind == Kind::Tran {
            String::new()
        } else {
            ddic_xml(
                kind,
                &name,
                &text.replace('&', "&amp;").replace('<', "&lt;"),
            )
        };
        let mut request = Request::new(
            if kind == Kind::Tran {
                Mode::Create
            } else {
                Mode::Deploy
            },
            kind,
            &name,
            NONCE,
        );
        let package = format!("Z{}", rng.up_to(&chars("ABC0123_$/"), 29));
        let tr = format!("H{}", rng.up_to(&chars("D1K0123456789"), 19));
        request.package = Some(&package);
        request.tr = Some(&tr);
        if kind == Kind::Tran {
            request.report = Some("ZREPORT");
            request.text = Some(&text);
        } else {
            request.xml = Some(&xml);
            request.base = Some(HASH_A);
            request.package = None;
        }

        let lines = encoded(&request);
        assert_safe(&lines);
        let doc = decode(&lines);
        assert_eq!(one(&doc, "NAME"), name);
        if kind != Kind::Tran {
            assert_eq!(values(&doc, "XML").join("\n"), xml);
        } else {
            assert_eq!(one(&doc, "TEXT"), text);
            assert_eq!(one(&doc, "PACKAGE"), package);
        }
    }
}

#[test]
fn property_document_sizes_cover_every_padding_case() {
    // The XML length decides the padding of the base64 text: all three residues must round trip.
    for extra in 0..12 {
        let text = "x".repeat(extra);
        let xml = ddic_xml(Kind::Dtel, "ZOBJ", &text);
        let mut request = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
        request.xml = Some(&xml);
        let lines = encoded(&request);
        assert_safe(&lines);
        assert_eq!(values(&decode(&lines), "XML").join("\n"), xml);
    }
}

// ---------------------------------------------------------------------------------------------
// Hostile values are refused before any connection (the encoder is pure: refusal is the Err).

const HOSTILE: [char; 26] = [
    '\'', '"', '\n', '\r', '|', '{', '}', '`', '*', 'ü', '日', '\0', '\t', '\\', ' ', '%', ';',
    '-', '.', ',', '<', '>', '&', '\u{202e}', '\u{ff21}', '\u{7f}',
];

/// Every way to put a hostile character into `valid` (front, middle, end, replacing one).
fn mutate(valid: &str, hostile: char) -> Vec<String> {
    let middle = valid.len() / 2;
    vec![
        format!("{hostile}{valid}"),
        format!("{}{hostile}{}", &valid[..middle], &valid[middle..]),
        format!("{valid}{hostile}"),
        format!("{hostile}{}", &valid[1..]),
        hostile.to_string(),
    ]
}

#[test]
fn fuzz_object_names_with_hostile_characters_are_refused_for_every_mode_and_kind() {
    for (mode, kind, request) in valid_all() {
        for hostile in HOSTILE {
            for bad in mutate("ZOBJ_NAME", hostile) {
                let mut candidate = request.clone();
                candidate.name = &bad;
                assert!(
                    encode(&candidate).is_err(),
                    "{mode:?} {kind:?} accepted name {bad:?}"
                );
            }
        }
    }
}

#[test]
fn fuzz_package_tr_report_hashes_batch_and_nonce_refuse_hostile_characters() {
    let batch_ok = [("DTEL", "ZDE")];

    for hostile in HOSTILE {
        for bad in mutate("ZPKG", hostile) {
            let mut r = valid(Mode::Create, Kind::Tran, "");
            r.package = Some(&bad);
            assert!(encode(&r).is_err(), "package {bad:?}");

            let mut r = valid(Mode::Create, Kind::Tran, "");
            r.report = Some(&bad);
            assert!(encode(&r).is_err(), "report {bad:?}");
        }
        for bad in mutate("HD1K917754", hostile) {
            let mut r = valid(Mode::Create, Kind::Tran, "");
            r.tr = Some(&bad);
            assert!(encode(&r).is_err(), "tr {bad:?}");
        }
        for bad in mutate(HASH_A, hostile) {
            let mut r = valid(Mode::Deploy, Kind::Dtel, xml_static(Kind::Dtel));
            r.base = Some(&bad);
            assert!(encode(&r).is_err(), "base {bad:?}");

            let mut r = valid(Mode::Delete, Kind::Dtel, "");
            r.plan = Some(&bad);
            assert!(encode(&r).is_err(), "plan {bad:?}");
        }
        for bad in mutate(NONCE, hostile) {
            let mut r = valid(Mode::Read, Kind::Dtel, "");
            r.nonce = &bad;
            assert!(encode(&r).is_err(), "nonce {bad:?}");
        }
        for bad in mutate("ZDE", hostile) {
            let batch = [("DTEL", bad.as_str())];
            let mut r = valid(Mode::Delete, Kind::Dtel, "");
            r.batch = &batch;
            assert!(encode(&r).is_err(), "batch name {bad:?}");
        }
        for bad in mutate("DTEL", hostile) {
            let batch = [(bad.as_str(), "ZDE")];
            let mut r = valid(Mode::Delete, Kind::Dtel, "");
            r.batch = &batch;
            assert!(encode(&r).is_err(), "batch type {bad:?}");
        }
        let mut r = valid(Mode::Delete, Kind::Dtel, "");
        r.batch = &batch_ok;
        encoded(&r);
    }
}

#[test]
fn package_accepts_dollar_and_slash_but_nothing_else() {
    for good in ["$TMP", "ZPKG", "/NS/PKG", "Z_1"] {
        let mut r = valid(Mode::Create, Kind::Tran, "");
        r.package = Some(good);
        encoded(&r);
    }
    for bad in ["", "ZP KG", "ZP-KG", "ZPKGü", "ZP.KG"] {
        let mut r = valid(Mode::Create, Kind::Tran, "");
        r.package = Some(bad);
        assert!(encode(&r).is_err(), "package {bad:?}");
    }
}

#[test]
fn tran_text_refuses_control_characters_and_newlines() {
    for bad in ["a\nb", "a\rb", "a\0b", "a\u{7f}b", "a\tb", "", "\n"] {
        let mut r = valid(Mode::Create, Kind::Tran, "");
        r.text = Some(bad);
        assert!(encode(&r).is_err(), "text {bad:?}");
    }
}

#[test]
fn random_garbage_never_yields_an_unsafe_line() {
    let alphabet = chars("ZY_/AB19 '\"\n|{}`*ü日\0$-.");
    let mut rng = Rng(42);

    for _ in 0..3000 {
        let name = rng.up_to(&alphabet, 40);
        let package = rng.up_to(&alphabet, 40);
        let tr = rng.up_to(&alphabet, 30);
        let text = rng.up_to(&alphabet, 50);
        let kind = ALL_KINDS[rng.below(5)];
        let mode = ALL_MODES[rng.below(7)];
        let mut request = Request::new(mode, kind, &name, NONCE);
        request.package = Some(&package);
        request.tr = Some(&tr);
        request.text = Some(&text);
        request.report = Some(&name);

        if let Ok(lines) = encode(&request) {
            assert_safe(&lines);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Lengths.

fn name_of(len: usize) -> String {
    format!("Z{}", "A".repeat(len - 1))
}

#[test]
fn object_names_stop_at_the_length_of_the_type() {
    for (kind, max) in [
        (Kind::Dtel, 30),
        (Kind::Doma, 30),
        (Kind::Tabl, 30),
        (Kind::Ttyp, 30),
        (Kind::Tran, 20),
    ] {
        let ok = name_of(max);
        let bad = name_of(max + 1);
        let xml_ok = if kind == Kind::Tran {
            String::new()
        } else {
            ddic_xml(kind, &ok, "t")
        };
        let xml_bad = if kind == Kind::Tran {
            String::new()
        } else {
            ddic_xml(kind, &bad, "t")
        };

        for (name, xml, accepted) in [(&ok, &xml_ok, true), (&bad, &xml_bad, false)] {
            let mode = if kind == Kind::Tran {
                Mode::Read
            } else {
                Mode::Simulate
            };
            let mut r = Request::new(mode, kind, name, NONCE);
            if kind != Kind::Tran {
                r.xml = Some(xml);
            }
            assert_eq!(
                encode(&r).is_ok(),
                accepted,
                "{kind:?} length {}",
                name.len()
            );
        }
    }
}

#[test]
fn namespaced_names_are_allowed_for_read_only() {
    let mut r = Request::new(Mode::Read, Kind::Tran, "/IWFND/MAINT_SERVICE", NONCE);
    encoded(&r);
    r.mode = Mode::Delete;
    r.plan = Some(HASH_B);
    assert!(encode(&r).is_err());
}

#[test]
fn package_tr_text_and_report_stop_at_their_limits() {
    let long = |n: usize, ch: char| ch.to_string().repeat(n);
    let cases: [(&str, Setter, usize); 4] = [
        ("package", |r, v| r.package = Some(leak(v.to_owned())), 30),
        ("tr", |r, v| r.tr = Some(leak(v.to_owned())), 20),
        ("text", |r, v| r.text = Some(leak(v.to_owned())), 36),
        ("report", |r, v| r.report = Some(leak(v.to_owned())), 40),
    ];

    for (label, set, max) in cases {
        for (len, accepted) in [(max, true), (max + 1, false)] {
            let value = if label == "report" {
                name_of(len)
            } else {
                long(len, 'Z')
            };
            let mut r = valid(Mode::Create, Kind::Tran, "");
            set(&mut r, &value);
            assert_eq!(encode(&r).is_ok(), accepted, "{label} length {len}");
        }
    }
}

#[test]
fn tran_text_counts_characters_not_bytes() {
    let mut r = valid(Mode::Create, Kind::Tran, "");
    let ok = "ä".repeat(36);
    r.text = Some(&ok);
    encoded(&r);
    let bad = "ä".repeat(37);
    r.text = Some(&bad);
    assert!(encode(&r).is_err());
}

#[test]
fn xml_rows_stop_at_255_characters() {
    for (width, accepted) in [(255, true), (256, false)] {
        for fill in ["x", "ä"] {
            let row = format!("<!--{}-->", fill.repeat(width - 7));
            let xml = xml_for(Kind::Dtel).replace("</abapGit>", &format!("{row}\n</abapGit>"));
            let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
            r.xml = Some(&xml);
            let result = encode(&r);

            assert_eq!(result.is_ok(), accepted, "row width {width} fill {fill}");
            if !accepted {
                assert!(
                    result
                        .unwrap_err()
                        .starts_with("XML row longer than 255 characters")
                );
            }
        }
    }
}

/// A valid DTEL document of exactly `size` bytes whose rows are all short.
fn padded(size: usize) -> String {
    let head = xml_for(Kind::Dtel).replace("</abapGit>", "");
    let tail = "</abapGit>";
    let mut xml = head;
    let budget = size - tail.len();

    while xml.len() + 200 + 8 < budget {
        xml.push_str(&format!("<!--{}-->\n", "x".repeat(190)));
    }
    let rest = budget - xml.len();
    assert!(rest >= 8, "rest {rest}");
    xml.push_str(&format!("<!--{}-->\n", "x".repeat(rest - 8)));
    xml.push_str(tail);
    assert_eq!(xml.len(), size);
    xml
}

#[test]
fn file_size_limit_is_256_kib_inclusive() {
    assert_eq!(MAX_XML_BYTES, 256 * 1024);

    let at_limit = padded(MAX_XML_BYTES);
    let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
    r.xml = Some(&at_limit);
    assert_safe(&encoded(&r));

    let over = padded(MAX_XML_BYTES + 1);
    r.xml = Some(&over);
    let message = err(&r);
    assert!(message.contains("larger than 262144 bytes"), "{message}");
}

#[test]
fn file_size_limit_counts_bytes_not_characters() {
    // MAX + 2 bytes made of exactly MAX characters must still be refused.
    let mut xml = padded(MAX_XML_BYTES - 10);
    xml = xml.replace(
        "</abapGit>",
        &format!("<!--{}-->\n</abapGit>", "ä".repeat(2)),
    );
    let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
    r.xml = Some(&xml);

    assert!(xml.len() > MAX_XML_BYTES);
    assert_eq!(xml.chars().count(), MAX_XML_BYTES);
    assert!(err(&r).contains("larger than"));
}

// ---------------------------------------------------------------------------------------------
// XML content: DTD, entities, control characters, object identity, --text.

#[test]
fn dtd_and_entity_declarations_are_refused_in_any_case() {
    for marker in [
        "<!DOCTYPE abapGit [<!ENTITY x \"y\">]>",
        "<!DOCTYPE x>",
        "<!ENTITY x \"y\">",
        "<!doctype x>",
        "<!Entity x \"y\">",
    ] {
        for place in ["top", "inside"] {
            let base = xml_for(Kind::Dtel);
            let xml = if place == "top" {
                format!("{marker}\n{base}")
            } else {
                base.replace("</abapGit>", &format!("{marker}\n</abapGit>"))
            };
            let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
            r.xml = Some(&xml);
            let message = err(&r);

            assert!(
                message.contains("DOCTYPE") && message.contains("ENTITY"),
                "{marker} {place}: {message}"
            );
        }
    }
}

#[test]
fn control_characters_in_xml_rows_are_refused() {
    for bad in ["a\0b", "a\u{1}b", "a\rb", "a\u{7f}b", "a\u{1b}b"] {
        let xml = xml_for(Kind::Dtel).replace("</abapGit>", &format!("<!--{bad}-->\n</abapGit>"));
        let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
        r.xml = Some(&xml);

        assert!(encode(&r).is_err(), "{bad:?}");
    }
    // A tab is legal XML whitespace.
    let xml = xml_for(Kind::Dtel).replace("<DDLANGUAGE>", "\t<DDLANGUAGE>");
    let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
    r.xml = Some(&xml);
    encoded(&r);
}

#[test]
fn empty_and_malformed_xml_are_refused() {
    for bad in [
        "",
        "\n\n",
        "\0",
        "<abapGit><DD04V>",
        "not xml at all <",
        "<a></b>",
    ] {
        let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
        r.xml = Some(bad);

        assert!(encode(&r).is_err(), "{bad:?}");
    }
}

#[test]
fn xml_for_another_object_is_refused_with_the_contract_message() {
    for kind in DDIC_KINDS {
        let xml = ddic_xml(kind, "ZOTHER", "t");
        let kind_token = format!("{kind:?}").to_ascii_uppercase();
        let mut r = Request::new(Mode::Deploy, kind, "ZOBJ", NONCE);
        r.xml = Some(&xml);
        r.base = Some(HASH_A);

        assert_eq!(
            err(&r),
            format!("XML file is for ZOTHER, not {kind_token} ZOBJ"),
            "{kind:?}"
        );
    }
}

#[test]
fn xml_of_another_type_is_refused() {
    // A data element file offered as a domain carries no DOMNAME.
    let xml = ddic_xml(Kind::Dtel, "ZOBJ", "t");
    let mut r = Request::new(Mode::Simulate, Kind::Doma, "ZOBJ", NONCE);
    r.xml = Some(&xml);

    assert_eq!(err(&r), "XML file is for , not DOMA ZOBJ");
}

#[test]
fn xml_name_match_ignores_surrounding_whitespace_and_the_case_of_the_request() {
    let xml = ddic_xml(Kind::Ttyp, "ZOBJ", "t").replace("<TYPENAME>ZOBJ", "<TYPENAME>  ZOBJ ");
    let mut r = Request::new(Mode::Simulate, Kind::Ttyp, "zobj", NONCE);
    r.xml = Some(&xml);
    encoded(&r);
}

#[test]
fn table_field_rows_must_all_carry_the_requested_tabname() {
    let good = ddic_xml(Kind::Tabl, "ZOBJ", "t");
    let mut r = Request::new(Mode::Simulate, Kind::Tabl, "ZOBJ", NONCE);
    r.xml = Some(&good);
    encoded(&r);

    // The second row points to another table.
    let other = good.replacen(
        "<TABNAME>ZOBJ</TABNAME>\n     <FIELDNAME>KEY1",
        "<TABNAME>ZOTHER</TABNAME>\n     <FIELDNAME>KEY1",
        1,
    );
    assert_ne!(other, good);
    r.xml = Some(&other);
    assert_eq!(err(&r), "XML file is for ZOTHER, not TABL ZOBJ");

    // A row without a TABNAME is refused too.
    let missing = good.replacen(
        "<TABNAME>ZOBJ</TABNAME>\n     <FIELDNAME>KEY1",
        "<FIELDNAME>KEY1",
        1,
    );
    assert_ne!(missing, good);
    r.xml = Some(&missing);
    assert_eq!(err(&r), "XML file is for , not TABL ZOBJ");

    // A TABNAME outside a DD03P row does not count for the rows.
    let outside = good.replace("</DD03P_TABLE>", "</DD03P_TABLE><TABNAME>ZOTHER</TABNAME>");
    r.xml = Some(&outside);
    encoded(&r);
}

#[test]
fn sub_table_rows_must_all_name_the_requested_object() {
    // (kind, row element, name tag): every name-keyed sub-table the payload accepts.
    let rows = [
        (Kind::Doma, "DD07V_TAB", "DD07V", "DOMNAME"),
        (Kind::Tabl, "", "DD09L", "TABNAME"),
        (Kind::Tabl, "DD05M_TABLE", "DD05M", "TABNAME"),
        (Kind::Tabl, "DD08V_TABLE", "DD08V", "TABNAME"),
        (Kind::Tabl, "DD35V_TABLE", "DD35V", "TABNAME"),
        (Kind::Tabl, "DD36M_TABLE", "DD36M", "TABNAME"),
        (Kind::Ttyp, "DD42V_TABLE", "DD42V", "TYPENAME"),
        (Kind::Ttyp, "DD43V_TABLE", "DD43V", "TYPENAME"),
    ];

    for (kind, table, row, tag) in rows {
        let with = |object: &str| {
            let element = format!("<{row}><{tag}>{object}</{tag}></{row}>");
            let wrapped = if table.is_empty() {
                element
            } else {
                format!("<{table}>{element}</{table}>")
            };

            ddic_xml(kind, "ZOBJ", "t").replace("</asx:values>", &format!("{wrapped}</asx:values>"))
        };
        let kind_token = format!("{kind:?}").to_ascii_uppercase();
        let mut r = Request::new(Mode::Simulate, kind, "ZOBJ", NONCE);

        let good = with("ZOBJ");
        r.xml = Some(&good);
        encoded(&r);

        let foreign = with("MATNR");
        r.xml = Some(&foreign);
        assert_eq!(
            err(&r),
            format!("XML file is for MATNR, not {kind_token} ZOBJ"),
            "{row}"
        );
    }
}

#[test]
fn sub_table_rows_without_a_name_are_not_judged() {
    // Empty tables and rows that carry no name field stay accepted (only a foreign name is refused).
    let xml = ddic_xml(Kind::Ttyp, "ZOBJ", "t").replace(
        "</asx:values>",
        "<DD42V/><DD43V/><DD07V_TAB><DD07V><VALPOS>0001</VALPOS></DD07V></DD07V_TAB></asx:values>",
    );
    let mut r = Request::new(Mode::Simulate, Kind::Ttyp, "ZOBJ", NONCE);
    r.xml = Some(&xml);
    encoded(&r);
}

#[test]
fn only_the_record_element_names_the_object() {
    // A matching name in the wrong place does not satisfy the check.
    let xml = "<abapGit><DD04V><DDTEXT>ZOBJ</DDTEXT></DD04V><OTHER><ROLLNAME>ZOBJ</ROLLNAME></OTHER></abapGit>";
    let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
    r.xml = Some(xml);

    assert_eq!(err(&r), "XML file is for , not DTEL ZOBJ");
}

#[test]
fn text_argument_must_equal_the_description_in_the_xml() {
    for kind in DDIC_KINDS {
        let xml = ddic_xml(kind, "ZOBJ", "Same text");
        let mut r = Request::new(Mode::Create, kind, "ZOBJ", NONCE);
        r.xml = Some(&xml);

        r.text = None;
        encoded(&r);
        r.text = Some("Same text");
        encoded(&r);
        // Not part of the document for DDIC: the XML is the source of the description.
        assert!(values(&decode(&encoded(&r)), "TEXT").is_empty());

        r.text = Some("Different text");
        let message = err(&r);
        assert!(
            message.contains("--text") && message.contains("XML"),
            "{kind:?}: {message}"
        );
    }
}

#[test]
fn text_argument_is_compared_to_the_unescaped_description_and_a_missing_one_differs() {
    let xml = ddic_xml(Kind::Dtel, "ZOBJ", "A &amp; B &#x41; &lt;");
    let mut r = Request::new(Mode::CreateSim, Kind::Dtel, "ZOBJ", NONCE);
    r.xml = Some(&xml);
    r.text = Some("A & B A <");
    encoded(&r);

    let no_description = xml_for(Kind::Dtel).replace("<DDTEXT>Object text</DDTEXT>", "");
    r.xml = Some(&no_description);
    r.text = Some("anything");
    assert!(err(&r).contains("--text"));
    r.text = None;
    encoded(&r);

    // The DDTEXT of a field row does not stand in for the description of the table.
    let table = ddic_xml(Kind::Tabl, "ZOBJ", "Table text");
    r.kind = Kind::Tabl;
    r.xml = Some(&table);
    r.text = Some("field text");
    assert!(err(&r).contains("--text"));
}

// ---------------------------------------------------------------------------------------------
// Z/Y scope, mode and field applicability.

#[test]
fn write_modes_accept_only_z_and_y_names_and_read_accepts_sap_names() {
    for (mode, kind, request) in valid_all() {
        let mut sap = request.clone();
        let sap_xml = if kind == Kind::Tran {
            String::new()
        } else {
            ddic_xml(kind, "SFLIGHT", "t")
        };
        sap.name = "SFLIGHT";
        if kind != Kind::Tran && request.xml.is_some() {
            sap.xml = Some(&sap_xml);
        }
        let result = encode(&sap);

        assert_eq!(
            result.is_ok(),
            mode == Mode::Read,
            "{mode:?} {kind:?} SAP name"
        );

        let mut yes = request.clone();
        let y_xml = if kind == Kind::Tran {
            String::new()
        } else {
            ddic_xml(kind, "YOBJ", "t")
        };
        yes.name = "yobj";
        if kind != Kind::Tran && request.xml.is_some() {
            yes.xml = Some(&y_xml);
        }
        encoded(&yes);

        let mut namespaced = request.clone();
        namespaced.name = "/NS/ZOBJ";
        assert_eq!(
            encode(&namespaced).is_ok(),
            mode == Mode::Read,
            "{mode:?} {kind:?} namespace"
        );
    }
}

#[test]
fn batch_and_report_names_are_z_y_only() {
    let batch = [("DTEL", "SFLIGHT")];
    let mut r = valid(Mode::Delete, Kind::Dtel, "");
    r.batch = &batch;
    assert!(encode(&r).is_err());

    let mut r = valid(Mode::Create, Kind::Tran, "");
    r.report = Some("SAPMSYST");
    assert!(encode(&r).is_err());
}

#[test]
fn batch_types_are_limited_to_the_object_types_of_the_tool() {
    for good in [
        "PROG", "FUNC", "CLAS", "INTF", "FUGR", "TRAN", "TTYP", "TABL", "DTEL", "DOMA",
    ] {
        let batch = [(good, "ZOBJ")];
        let mut r = valid(Mode::DeleteSim, Kind::Dtel, "");
        r.batch = &batch;
        encoded(&r);
    }
    for bad in ["", "MSAG", "dtel1", "TEXT", "DDLS", "DTE"] {
        let batch = [(bad, "ZOBJ")];
        let mut r = valid(Mode::DeleteSim, Kind::Dtel, "");
        r.batch = &batch;
        assert!(encode(&r).is_err(), "{bad:?}");
    }
}

#[test]
fn tran_has_no_deploy() {
    for mode in [Mode::Simulate, Mode::Deploy] {
        let mut r = Request::new(mode, Kind::Tran, "ZT", NONCE);
        r.base = Some(HASH_A);
        assert_eq!(err(&r), "TRAN has no deploy; use create or delete");
    }
}

#[test]
fn tran_create_needs_report_and_text_and_no_xml() {
    let r = valid(Mode::Create, Kind::Tran, "");
    encoded(&r);

    let mut no_report = r.clone();
    no_report.report = None;
    assert!(encode(&no_report).is_err());

    let mut no_text = r.clone();
    no_text.text = None;
    assert!(encode(&no_text).is_err());

    let mut with_xml = r.clone();
    with_xml.xml = Some("<a/>");
    assert!(encode(&with_xml).is_err());
}

#[test]
fn fields_that_do_not_apply_to_the_mode_or_kind_are_refused() {
    for (mode, kind, request) in valid_all() {
        let create = matches!(mode, Mode::Create | Mode::CreateSim);
        let tran = kind == Kind::Tran;

        let mut r = request.clone();
        r.package = Some("ZPKG");
        assert_eq!(encode(&r).is_ok(), create, "package {mode:?} {kind:?}");

        let mut r = request.clone();
        r.tr = Some("HD1K1");
        assert_eq!(
            encode(&r).is_ok(),
            mode != Mode::Read,
            "tr {mode:?} {kind:?}"
        );

        let mut r = request.clone();
        r.base = Some(HASH_A);
        assert_eq!(
            encode(&r).is_ok(),
            matches!(mode, Mode::Simulate | Mode::Deploy),
            "base {mode:?} {kind:?}"
        );

        let mut r = request.clone();
        r.plan = Some(HASH_A);
        assert_eq!(
            encode(&r).is_ok(),
            mode == Mode::Delete,
            "plan {mode:?} {kind:?}"
        );

        let mut r = request.clone();
        r.report = Some("ZREPORT");
        assert_eq!(
            encode(&r).is_ok(),
            tran && create,
            "report {mode:?} {kind:?}"
        );

        let mut r = request.clone();
        let batch = [("DTEL", "ZDE")];
        r.batch = &batch;
        assert_eq!(
            encode(&r).is_ok(),
            matches!(mode, Mode::DeleteSim | Mode::Delete),
            "batch {mode:?} {kind:?}"
        );

        let mut r = request.clone();
        r.text = Some("Object text");
        assert_eq!(
            encode(&r).is_ok(),
            (tran && create) || (!tran && xml_mode_of(mode)),
            "text {mode:?} {kind:?}"
        );

        let mut r = request.clone();
        r.xml = Some(xml_static(if tran { Kind::Dtel } else { kind }));
        assert_eq!(
            encode(&r).is_ok(),
            xml_mode_of(mode) && !tran,
            "xml {mode:?} {kind:?}"
        );
    }
}

#[test]
fn required_fields_are_enforced() {
    let mut r = valid(Mode::Deploy, Kind::Dtel, xml_static(Kind::Dtel));
    r.base = None;
    assert!(encode(&r).is_err(), "deploy without base");

    let mut r = valid(Mode::Delete, Kind::Dtel, "");
    r.plan = None;
    assert!(encode(&r).is_err(), "delete without plan");

    for mode in [Mode::Simulate, Mode::Deploy, Mode::CreateSim, Mode::Create] {
        let mut r = valid(mode, Kind::Dtel, xml_static(Kind::Dtel));
        r.xml = None;
        assert!(encode(&r).is_err(), "{mode:?} without xml");
    }
}

#[test]
fn hashes_and_nonce_have_a_fixed_shape() {
    let mut r = valid(Mode::Deploy, Kind::Dtel, xml_static(Kind::Dtel));
    // An upper-case hash is lowered like the hash of the tool.
    let upper = HASH_B.to_ascii_uppercase();
    r.base = Some(&upper);
    assert_eq!(one(&decode(&encoded(&r)), "BASE"), HASH_B);

    let too_long = format!("{HASH_A}a");
    let not_hex = "g".repeat(64);
    for bad in [&HASH_A[..63], too_long.as_str(), "", not_hex.as_str()] {
        r.base = Some(bad);
        assert!(encode(&r).is_err(), "base {bad:?}");
    }

    let mut r = valid(Mode::Read, Kind::Dtel, "");
    let too_long = format!("{NONCE}0");
    let lower = NONCE.to_ascii_lowercase();
    let not_hex = NONCE.replace('0', "G");
    for bad in [
        &NONCE[..31],
        too_long.as_str(),
        "",
        lower.as_str(),
        not_hex.as_str(),
    ] {
        r.nonce = bad;
        assert!(encode(&r).is_err(), "nonce {bad:?}");
    }
    let other = NONCE.replace('0', "F");
    r.nonce = &other;
    encoded(&r);
}

#[test]
fn empty_optional_values_are_refused_not_dropped() {
    for field in ["package", "tr"] {
        let mut r = valid(Mode::Create, Kind::Tran, "");
        match field {
            "package" => r.package = Some(""),
            _ => r.tr = Some(""),
        }
        assert!(encode(&r).is_err(), "{field}");
    }
}

#[test]
fn an_empty_name_is_refused() {
    let mut r = valid(Mode::Read, Kind::Dtel, "");
    r.name = "";
    assert!(encode(&r).is_err());
}

#[test]
fn empty_elements_and_cdata_are_read_like_text() {
    let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);

    let empty_name = "<abapGit><DD04V><ROLLNAME/></DD04V></abapGit>";
    r.xml = Some(empty_name);
    assert_eq!(err(&r), "XML file is for , not DTEL ZOBJ");

    let cdata = "<abapGit><DD04V><ROLLNAME><![CDATA[ZOBJ]]></ROLLNAME><DDTEXT><![CDATA[A & B]]></DDTEXT></DD04V></abapGit>";
    r.xml = Some(cdata);
    r.mode = Mode::CreateSim;
    r.text = Some("A & B");
    encoded(&r);

    let mut t = Request::new(Mode::Simulate, Kind::Tabl, "ZOBJ", NONCE);
    let empty_row = "<abapGit><DD02V><TABNAME>ZOBJ</TABNAME></DD02V><DD03P_TABLE><DD03P><TABNAME/></DD03P></DD03P_TABLE></abapGit>";
    t.xml = Some(empty_row);
    assert_eq!(err(&t), "XML file is for , not TABL ZOBJ");

    let bare_row = "<abapGit><DD02V><TABNAME>ZOBJ</TABNAME></DD02V><DD03P_TABLE><DD03P/></DD03P_TABLE></abapGit>";
    t.xml = Some(bare_row);
    assert_eq!(err(&t), "XML file is for , not TABL ZOBJ");
}

#[test]
fn object_names_in_errors_do_not_carry_control_characters() {
    let xml = ddic_xml(Kind::Dtel, "ZOBJ", "t").replace("<ROLLNAME>ZOBJ", "<ROLLNAME>Z&#27;[2JX");
    let mut r = Request::new(Mode::Simulate, Kind::Dtel, "ZOBJ", NONCE);
    r.xml = Some(&xml);

    assert_eq!(err(&r), "XML file is for Z?[2JX, not DTEL ZOBJ");
}
