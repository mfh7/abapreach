//! The MCP read tool `abap_ping`: the text of the CLI `ping`, the structured fields, the 404 and
//! 406 readings, the fallback, the refusals and the secret seal, on the fake ADT HTTP transport
//! and the fake RFC API. Every value is synthetic. No SAP.

mod common;

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::mcp::{READ_TOOLS, ToolHandler, ToolResult};
use abapreach::mcp_read::ReadTools;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::rfc::{FakeRfcApi, RfcTransport, SystemInfo};
use abapreach::transport::{Request, Response, SadtTransport, TransportError};
use abapreach::{Factories, RfcLibraryInfo};
use common::{ScriptedTransport, sysinfo_reply};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::rc::Rc;

const PASSWORD: &str = "secret-pw";
const ROUTE: &str = "/H/router.corp.example/S/3299/W/routerpw/H/10.9.8.7/S/3200";
const SECRETS: [&str; 5] = [
    PASSWORD,
    ROUTE,
    "router.corp.example",
    "routerpw",
    "10.9.8.7",
];

type Opened = Result<Box<dyn SadtTransport>, TransportError>;

/// A session that several owners see, so a test reads its calls after the tool dropped it.
struct Shared<T>(Rc<RefCell<T>>);

impl<T: SadtTransport> SadtTransport for Shared<T> {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.0.borrow_mut().request(request)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.0.borrow_mut().close()
    }

    fn system_info(&mut self) -> Result<SystemInfo, TransportError> {
        self.0.borrow_mut().system_info()
    }
}

fn shared<T: SadtTransport + 'static>(transport: T) -> (Rc<RefCell<T>>, Opened) {
    let cell = Rc::new(RefCell::new(transport));

    (cell.clone(), Ok(Box::new(Shared(cell))))
}

#[derive(Default)]
struct Fake {
    adt: VecDeque<Opened>,
    rfc: VecDeque<Opened>,
    adt_opened: usize,
    rfc_opened: usize,
}

impl Factories for Fake {
    fn adt(&mut self, _: &Connection) -> Opened {
        self.adt_opened += 1;
        self.adt.pop_front().expect("unexpected ADT connect")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        self.rfc_opened += 1;
        self.rfc.pop_front().expect("unexpected RFC connect")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        unreachable!("a read tool never probes the library")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        unreachable!("a read tool never pings")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

/// `DEV_100` reaches SAP over ADT HTTP and RFC, `QAS_100` over RFC only, `RTR_100` over RFC with
/// a route.
struct Rig {
    fake: Fake,
    env: BTreeMap<String, String>,
    _directory: tempfile::TempDir,
}

fn rig() -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("connections.json");
    let config = format!(
        concat!(
            r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
            r#""rfc":{{"ashost":"example.com","sysnr":"00"}}}},"#,
            r#"{{"systemId":"QAS","clients":["100"],"rfc":{{"ashost":"example.com","sysnr":"02"}}}},"#,
            r#"{{"systemId":"RTR","clients":["100"],"#,
            r#""rfc":{{"ashost":"example.com","sysnr":"00","saprouter":"{route}"}}}}]}}"#,
        ),
        route = ROUTE
    );

    fs::write(&path, config).unwrap();

    let mut env = BTreeMap::from([("ABAP_CONNECTIONS".to_owned(), path.display().to_string())]);

    for system in ["DEV", "QAS", "RTR"] {
        env.insert(format!("{system}_100_USER"), "TESTUSER".to_owned());
        env.insert(format!("{system}_100_PASS"), PASSWORD.to_owned());
    }

    Rig {
        fake: Fake::default(),
        env,
        _directory: directory,
    }
}

impl Rig {
    fn call(&mut self, arguments: Value) -> ToolResult {
        let config = Config::load(ConfigPaths::from_env(&self.env).unwrap(), &self.env).unwrap();
        let mut tools = ReadTools::new(config, self.env.clone(), &mut self.fake);

        tools.call("abap_ping", arguments.as_object().unwrap())
    }

    fn adt(&mut self, reply: Result<Response, TransportError>) -> Rc<RefCell<ScriptedTransport>> {
        let (cell, opened) = shared(ScriptedTransport::replies(vec![reply]));

        self.fake.adt.push_back(opened);

        cell
    }

    fn rfc(
        &mut self,
        answer: Result<SystemInfo, TransportError>,
    ) -> Rc<RefCell<RfcTransport<FakeRfcApi>>> {
        let (cell, opened) = shared(RfcTransport::new(FakeRfcApi::with_system_info(answer)));

        self.fake.rfc.push_back(opened);

        cell
    }
}

fn assert_no_secret(result: &ToolResult, context: &str) {
    let output = format!("{}\n{}", result.text, result.structured);

    for secret in SECRETS {
        assert!(
            !output.contains(secret),
            "{context} leaks {secret}: {output}"
        );
    }
}

/// The text of the result with the milliseconds of the round trip replaced.
fn lines(result: &ToolResult) -> Vec<String> {
    result
        .text
        .lines()
        .map(|line| match line.strip_prefix("round trip ") {
            Some(rest) => {
                let (milliseconds, tail) = rest.split_once(" ms").expect("round trip has ms");

                assert!(milliseconds.parse::<u64>().is_ok(), "{line}");

                format!("round trip <n> ms{tail}")
            }
            None => line.to_owned(),
        })
        .collect()
}

#[test]
fn abap_ping_is_a_read_tool() {
    assert!(READ_TOOLS.contains(&"abap_ping"));
}

#[test]
fn over_adt_the_text_is_the_ping_text_and_the_structure_carries_its_fields() {
    let mut rig = rig();
    let cell = rig.adt(sysinfo_reply("sysinfo-750.xml"));
    let result = rig.call(json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        lines(&result),
        [
            "backend ADT HTTP",
            "instance apphost_HD1_00",
            "system id HD1",
            "kernel 753",
            "database DB6",
            "os Linux",
            "round trip <n> ms (includes logon)",
        ]
    );

    let structured = &result.structured;

    assert_eq!(structured["backend"], "ADT HTTP");
    assert_eq!(structured["instance"], "apphost_HD1_00");
    assert_eq!(structured["instance_unknown"], Value::Null);
    assert_eq!(structured["system_id"], "HD1");
    assert_eq!(structured["release"], Value::Null);
    assert_eq!(structured["kernel"], "753");
    assert_eq!(structured["database"], "DB6");
    assert_eq!(structured["os"], "Linux");
    assert!(structured["round_trip_ms"].is_u64(), "{structured}");
    assert_eq!(structured["includes_logon"], true);
    assert_eq!((rig.fake.adt_opened, rig.fake.rfc_opened), (1, 0));

    let transport = cell.borrow();

    assert_eq!(transport.calls.len(), 1);
    assert_eq!(transport.calls[0].method, "GET");
    assert_eq!(transport.calls[0].uri, "/sap/bc/adt/system/information");
    assert_eq!(transport.closes, 1);
}

#[test]
fn over_rfc_the_structure_carries_the_six_fields_and_the_timed_invoke() {
    let mut rig = rig();
    let cell = rig.rfc(Ok(common::system_info()));
    let result = rig.call(json!({"conn": "QAS_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        lines(&result),
        [
            "backend RFC SADT",
            "instance apphost_HD1_00",
            "system id HD1",
            "release 750",
            "kernel 753",
            "database DB6",
            "round trip <n> ms",
        ]
    );

    let structured = &result.structured;

    assert_eq!(structured["backend"], "RFC SADT");
    assert_eq!(structured["instance"], "apphost_HD1_00");
    assert_eq!(structured["system_id"], "HD1");
    assert_eq!(structured["release"], "750");
    assert_eq!(structured["kernel"], "753");
    assert_eq!(structured["database"], "DB6");
    assert_eq!(structured["os"], Value::Null);
    assert_eq!(structured["round_trip_ms"], 7);
    assert_eq!(structured["includes_logon"], false);
    assert_eq!((rig.fake.adt_opened, rig.fake.rfc_opened), (0, 1));
    assert_eq!(cell.borrow().api().system_info_calls(), 1);
}

#[test]
fn a_404_is_an_unknown_instance_and_not_an_error() {
    let mut rig = rig();

    rig.adt(sysinfo_reply("sysinfo-740-404.xml"));

    let result = rig.call(json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        lines(&result),
        [
            "backend ADT HTTP",
            "instance unknown (no /system/information on this release)",
            "round trip <n> ms (includes logon)",
        ]
    );
    assert_eq!(result.structured["instance"], Value::Null);
    assert_eq!(
        result.structured["instance_unknown"],
        "no /system/information on this release"
    );
    assert_eq!(result.structured["system_id"], Value::Null);
}

#[test]
fn a_406_is_an_unknown_instance_and_not_an_error() {
    let mut rig = rig();

    rig.adt(Ok(Response::empty(406)));

    let result = rig.call(json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(lines(&result)[1], "instance unknown (HTTP 406)");
    assert_eq!(result.structured["instance"], Value::Null);
    assert_eq!(result.structured["instance_unknown"], "HTTP 406");
}

#[test]
fn a_lost_adt_connection_falls_back_to_rfc_and_names_the_backend_that_answered() {
    let mut rig = rig();

    rig.adt(Err(TransportError::PreSendConnectivity(
        "no route".to_owned(),
    )));
    rig.rfc(Ok(common::system_info()));

    let result = rig.call(json!({"conn": "DEV_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(result.structured["backend"], "RFC SADT");
    assert!(
        result.structured["notes"]
            .as_str()
            .unwrap()
            .contains("NOTE: ADT HTTP failed before anything was written"),
        "{}",
        result.structured
    );
    assert_eq!((rig.fake.adt_opened, rig.fake.rfc_opened), (1, 1));
}

#[test]
fn a_failed_reading_is_an_error_without_data() {
    for (reply, needle) in [
        (Ok(Response::text(200, "<a></b>")), "REJECTED"),
        (Ok(Response::empty(500)), "HTTP 500"),
        (
            Err(TransportError::Authorization("logon failed".to_owned())),
            "logon failed",
        ),
    ] {
        let mut rig = rig();

        rig.adt(reply);

        let result = rig.call(json!({"conn": "DEV_100"}));

        assert!(result.is_error, "{needle}: {}", result.text);
        assert!(result.text.contains(needle), "{}", result.text);
        assert!(result.structured.get("instance").is_none(), "{needle}");
    }
}

#[test]
fn a_bad_or_unknown_connection_is_refused_before_any_logon() {
    let mut rig = rig();
    let missing = rig.call(json!({}));
    let unknown = rig.call(json!({"conn": PASSWORD}));
    let wrong_type = rig.call(json!({"conn": 7}));

    assert!(missing.is_error);
    assert_eq!(missing.text, "STOP: conn is required");
    assert!(unknown.is_error);
    assert_eq!(
        unknown.text,
        "STOP: unknown connection id; use <SID>_<client> of a configured system and client"
    );
    assert!(!unknown.text.contains(PASSWORD));
    assert!(wrong_type.is_error);
    assert_eq!((rig.fake.adt_opened, rig.fake.rfc_opened), (0, 0));
}

#[test]
fn the_instance_is_printed_in_clear_with_its_host() {
    let mut rig = rig();

    rig.rfc(Ok(SystemInfo {
        instance: "example.com_HD1_00".to_owned(),
        ..common::system_info()
    }));

    let result = rig.call(json!({"conn": "QAS_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(lines(&result)[1], "instance example.com_HD1_00");
    assert_eq!(result.structured["instance"], "example.com_HD1_00");
}

#[test]
fn the_password_is_scrubbed_from_every_field_before_the_value_is_cut() {
    let mut rig = rig();

    rig.rfc(Ok(SystemInfo {
        instance: format!("{}secret-pw", "x".repeat(60)),
        release: "se\u{200b}cret-pw".to_owned(),
        kernel: "a secret-pw b".to_owned(),
        ..common::system_info()
    }));

    let result = rig.call(json!({"conn": "QAS_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_eq!(
        result.structured["instance"],
        format!("{}***", "x".repeat(60))
    );
    assert_eq!(result.structured["release"], "***");
    assert_eq!(result.structured["kernel"], "a *** b");

    for fragment in ["secret", "ecret-pw", "-pw", "cret"] {
        let output = format!("{}\n{}", result.text, result.structured);

        assert!(!output.contains(fragment), "{fragment}: {output}");
    }
}

#[test]
fn a_route_and_its_fragments_never_reach_the_text_or_the_structure() {
    let mut rig = rig();

    rig.rfc(Ok(SystemInfo {
        instance: "sec\u{200b}ret-pw_HD1_00".to_owned(),
        release: "r\u{202e}outerpw".to_owned(),
        kernel: "10.9\u{200b}.8.7".to_owned(),
        database: "router.corp.\u{2060}example".to_owned(),
        ..common::system_info()
    }));

    let result = rig.call(json!({"conn": "RTR_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert_no_secret(&result, "the reading");
    assert_eq!(result.structured["instance"], "***_HD1_00");
}

#[test]
fn an_error_or_a_close_note_with_a_secret_or_a_route_never_reaches_the_result() {
    let leaking = format!(
        "logon secret-pw failed via {ROUTE} host router.corp.example pw routerpw gw 10.9.8.7"
    );

    for error in [
        TransportError::Authorization(leaking.clone()),
        TransportError::Protocol(leaking.clone()),
        TransportError::PreSendConnectivity(leaking.clone()),
        TransportError::NoResponse(leaking.clone()),
        TransportError::Http {
            status: 502,
            message: leaking.clone(),
        },
    ] {
        let mut rig = rig();
        let (_, opened) =
            shared(ScriptedTransport::replies(Vec::new()).with_system_info(Err(error.clone())));

        rig.fake.rfc.push_back(opened);

        let result = rig.call(json!({"conn": "RTR_100"}));

        assert!(result.is_error, "{error:?}");
        assert_no_secret(&result, &format!("{error:?}"));
    }

    let mut rig = rig();
    let (_, opened) = shared(common::CloseFails {
        inner: ScriptedTransport::replies(Vec::new()).with_system_info(Ok(common::system_info())),
        error: TransportError::Protocol(leaking),
    });

    rig.fake.rfc.push_back(opened);

    let result = rig.call(json!({"conn": "RTR_100"}));

    assert!(!result.is_error, "{}", result.text);
    assert!(
        result.text.contains("session close failed"),
        "{}",
        result.text
    );
    assert_no_secret(&result, "the close note");
}
