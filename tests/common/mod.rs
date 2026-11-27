#![allow(dead_code)]

use abapreach::transport::rfc::SystemInfo;
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use serde::Deserialize;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

pub mod runner_fake;

#[derive(Deserialize)]
pub struct Fixture {
    pub system: String,
    pub release: String,
    pub backend: String,
    pub exchanges: Vec<FixtureExchange>,
}

#[derive(Deserialize)]
pub struct FixtureExchange {
    pub request: FixtureRequest,
    pub response: FixtureResponse,
}

#[derive(Deserialize)]
pub struct FixtureRequest {
    pub method: String,
    pub uri: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

#[derive(Deserialize)]
pub struct FixtureResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

pub struct ScriptedTransport {
    pub calls: Vec<Request>,
    pub closes: usize,
    expected: VecDeque<Request>,
    replies: VecDeque<Result<Response, TransportError>>,
    /// The answer of the typed `RFC_SYSTEM_INFO` call, when the script plays an RFC session.
    system_info: Option<Result<SystemInfo, TransportError>>,
}

pub fn fixture_path(name: &str) -> PathBuf {
    fixture_path_in("m1", name)
}

/// A recorded fixture below `tests/fixtures/<directory>/`, such as the M3 read fixtures.
pub fn fixture_path_in(directory: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(directory)
        .join(name)
}

pub fn all(_: &FixtureExchange) -> bool {
    true
}

pub fn from_fixture(
    path: impl AsRef<Path>,
    filter: impl FnMut(&FixtureExchange) -> bool,
) -> ScriptedTransport {
    let text = fs::read_to_string(path).expect("read fixture");
    let fixture: Fixture = serde_json::from_str(&text).expect("parse fixture");
    let exchanges = fixture.exchanges.into_iter().filter(filter);
    let mut expected = VecDeque::new();
    let mut replies = VecDeque::new();

    for exchange in exchanges {
        expected.push_back(Request {
            method: exchange.request.method,
            uri: exchange.request.uri,
            headers: exchange.request.headers,
            body: exchange.request.body.into_bytes(),
        });
        replies.push_back(Ok(Response {
            status: exchange.response.status,
            headers: exchange.response.headers,
            body: exchange.response.body.into_bytes(),
        }));
    }

    ScriptedTransport {
        calls: Vec::new(),
        closes: 0,
        expected,
        replies,
        system_info: None,
    }
}

impl ScriptedTransport {
    pub fn replies(replies: Vec<Result<Response, TransportError>>) -> Self {
        Self {
            calls: Vec::new(),
            closes: 0,
            expected: VecDeque::new(),
            replies: replies.into(),
            system_info: None,
        }
    }

    /// Makes the script an RFC session that answers `RFC_SYSTEM_INFO` with `answer`.
    pub fn with_system_info(mut self, answer: Result<SystemInfo, TransportError>) -> Self {
        self.system_info = Some(answer);

        self
    }

    pub fn assert_finished(&self) {
        assert!(self.expected.is_empty(), "unconsumed expected requests");
        assert!(self.replies.is_empty(), "unconsumed scripted replies");
    }
}

impl SadtTransport for ScriptedTransport {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if let Some(expected) = self.expected.pop_front() {
            assert_eq!(request, expected, "unexpected SADT request");
        }

        self.calls.push(request);
        self.replies
            .pop_front()
            .expect("request without a scripted response")
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closes += 1;

        Ok(())
    }

    fn system_info(&mut self) -> Result<SystemInfo, TransportError> {
        self.system_info.clone().unwrap_or_else(|| {
            Err(TransportError::Protocol(
                "the script has no RFC_SYSTEM_INFO answer".to_owned(),
            ))
        })
    }
}

pub const NS: &str = "http://www.sap.com/adt/dataPreview";

/// A data preview answer: column-major, `None` is the empty value `<data/>`.
pub fn preview(columns: &[(&str, &[Option<&str>])]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><dataPreview:tableData xmlns:dataPreview=\"{NS}\">\
         <dataPreview:name>X</dataPreview:name><dataPreview:isHanaAnalyticalView>false</dataPreview:isHanaAnalyticalView>"
    );

    for (name, values) in columns {
        xml.push_str(&format!(
            "<dataPreview:columns><dataPreview:metadata dataPreview:name=\"{name}\" dataPreview:type=\"C\" \
             dataPreview:description=\"d\" dataPreview:keyAttribute=\"false\" dataPreview:colType=\"\" \
             dataPreview:isKeyFigure=\"false\"/>"
        ));

        if values.is_empty() {
            xml.push_str("<dataPreview:dataSet/>");
        } else {
            xml.push_str("<dataPreview:dataSet>");

            for value in *values {
                match value {
                    Some(text) => xml.push_str(&format!(
                        "<dataPreview:data>{}</dataPreview:data>",
                        text.replace('&', "&amp;").replace('<', "&lt;")
                    )),
                    None => xml.push_str("<dataPreview:data/>"),
                }
            }

            xml.push_str("</dataPreview:dataSet>");
        }

        xml.push_str("</dataPreview:columns>");
    }

    xml.push_str("</dataPreview:tableData>");

    xml
}

/// The replies of the `doctor` runner reads on one session after the discovery: the interface
/// preview (found), an empty leftover search, the two preflight previews, the type of the classrun
/// `OUT` parameter, the two orphan previews (no rows), and the system information of the
/// instance line.
pub fn doctor_runner_replies() -> Vec<Result<Response, TransportError>> {
    let found = preview(&[("CLSNAME", &[Some("IF_OO_ADT_CLASSRUN")])]);
    let t000 = preview(&[
        ("CCCATEGORY", &[Some("C")]),
        ("CCCORACTIV", &[Some("1")]),
        ("CCNOCLIIND", &[None]),
    ]);
    let space = preview(&[("EDITFLAG", &[Some("X")])]);
    let out = preview(&[
        ("CLSNAME", &[Some("IF_OO_ADT_CLASSRUN")]),
        ("CMPNAME", &[Some("MAIN")]),
        ("SCONAME", &[Some("OUT")]),
        ("TYPE", &[Some("IF_OO_ADT_INTRNL_CLASSRUN")]),
    ]);
    let none: &[Option<&str>] = &[];
    let directory = preview(&[("PGMID", none), ("OBJECT", none), ("OBJ_NAME", none)]);
    let classes = preview(&[("CLSNAME", none)]);
    let search = concat!(
        r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">"#,
        r#"</adtcore:objectReferences>"#,
    );

    [
        found.as_str(),
        search,
        t000.as_str(),
        space.as_str(),
        out.as_str(),
        directory.as_str(),
        classes.as_str(),
    ]
    .into_iter()
    .map(|body| Ok(Response::text(200, body)))
    .chain([sysinfo_reply("sysinfo-750.xml")])
    .collect()
}

/// What `RFC_READ_TABLE` returns: `FIELDS` with the offsets and `DATA` as fixed-width rows whose
/// trailing blanks the library drops.
pub fn read_table_reply(
    fields: &[(&str, usize)],
    rows: &[Vec<&str>],
) -> abapreach::transport::rfc::FunctionResult {
    let mut offset = 0;
    let mut spans = Vec::new();

    for (name, length) in fields {
        spans.push((*name, offset, *length));
        offset += length;
    }

    let text = |value: &str| abapreach::transport::rfc::Cell::Text(value.to_owned());

    abapreach::transport::rfc::FunctionResult {
        exports: Vec::new(),
        tables: vec![
            (
                "FIELDS".to_owned(),
                spans
                    .iter()
                    .map(|(name, offset, length)| {
                        vec![
                            text(name),
                            text(&format!("{offset:06}")),
                            text(&format!("{length:06}")),
                        ]
                    })
                    .collect(),
            ),
            (
                "DATA".to_owned(),
                rows.iter()
                    .map(|row| {
                        let wa: String = row
                            .iter()
                            .zip(fields)
                            .map(|(value, (_, length))| format!("{value:<width$}", width = *length))
                            .collect();

                        vec![text(wa.trim_end())]
                    })
                    .collect(),
            ),
        ],
    }
}

/// A synthetic `RFC_SYSTEM_INFO` answer; no live value.
pub fn system_info() -> SystemInfo {
    SystemInfo {
        instance: "apphost_HD1_00".to_owned(),
        host: "apphost".to_owned(),
        system_id: "HD1".to_owned(),
        release: "750".to_owned(),
        kernel: "753".to_owned(),
        database: "DB6".to_owned(),
        invoke_time: Some(std::time::Duration::from_millis(7)),
    }
}

/// The reply of `/system/information` of a fixture in `tests/fixtures/m9`.
pub fn sysinfo_reply(name: &str) -> Result<Response, TransportError> {
    let status = if name.ends_with("-404.xml") { 404 } else { 200 };
    let body = fs::read_to_string(fixture_path_in("m9", name)).expect("read fixture");

    Ok(Response::text(status, body))
}

/// A session whose close fails with `error`; every other call goes to `inner`.
pub struct CloseFails<T> {
    pub inner: T,
    pub error: TransportError,
}

impl<T: SadtTransport> SadtTransport for CloseFails<T> {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.inner.request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.inner.close();

        Err(self.error.clone())
    }

    fn system_info(&mut self) -> Result<SystemInfo, TransportError> {
        self.inner.system_info()
    }
}
