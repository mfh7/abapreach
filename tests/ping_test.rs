//! `ping`: the instance, the system facts and the round trip, on the fake ADT HTTP transport and
//! the fake RFC API. Every value is synthetic (see `tests/fixtures/m9/README.md`); the live
//! values of the three test systems are in the live harness, not here.

mod common;

use abapreach::adt::parse_system_information;
use abapreach::config::Connection;
use abapreach::operation::deploy::CancelFlag;
use abapreach::transport::rfc::{FakeRfcApi, RfcTransport, SystemInfo};
use abapreach::transport::{Response, SadtTransport, TransportError};
use abapreach::{Exit, Factories, Opened, RfcLibraryInfo, run};
use common::{ScriptedTransport, sysinfo_reply};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::rc::Rc;

struct Shared<T>(Rc<RefCell<T>>);

impl<T: SadtTransport> SadtTransport for Shared<T> {
    fn request(
        &mut self,
        request: abapreach::transport::Request,
    ) -> Result<Response, TransportError> {
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
        panic!("ping loads no library probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("ping makes no RfcPing")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

fn env(directory: &Path) -> BTreeMap<String, String> {
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}},"#,
        r#"{"systemId":"QAS","clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"02"}},"#,
        r#"{"systemId":"WEB","host":"example.com","port":443,"clients":["100"]}]}"#,
    );

    fs::write(directory.join("connections.json"), config).unwrap();

    let mut env = BTreeMap::from([(
        "ABAP_CONNECTIONS".to_owned(),
        directory.join("connections.json").display().to_string(),
    )]);

    for system in ["DEV", "QAS", "WEB"] {
        env.insert(format!("{system}_100_USER"), "user".to_owned());
        env.insert(format!("{system}_100_PASS"), "secret-pw".to_owned());
    }

    env
}

fn ping(args: &[&str], fake: &mut Fake) -> Exit {
    let directory = tempfile::tempdir().unwrap();
    let mut all = vec!["abapreach", "ping"];

    all.extend(args);

    run(
        all.into_iter().map(str::to_owned).collect(),
        env(directory.path()),
        fake,
    )
}

fn pre_send() -> Result<Response, TransportError> {
    Err(TransportError::PreSendConnectivity("no route".to_owned()))
}

/// The lines of the output with the milliseconds of the round trip replaced, so a test names the
/// whole text.
fn lines(exit: &Exit) -> Vec<String> {
    exit.stdout
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

fn adt_fake(reply: Result<Response, TransportError>) -> (Fake, Rc<RefCell<ScriptedTransport>>) {
    let (cell, opened) = shared(ScriptedTransport::replies(vec![reply]));
    let mut fake = Fake::default();

    fake.adt.push_back(opened);

    (fake, cell)
}

fn rfc_fake(
    answer: Result<SystemInfo, TransportError>,
) -> (Fake, Rc<RefCell<RfcTransport<FakeRfcApi>>>) {
    let (cell, opened) = shared(RfcTransport::new(FakeRfcApi::with_system_info(answer)));
    let mut fake = Fake::default();

    fake.rfc.push_back(opened);

    (fake, cell)
}

#[test]
fn ping_over_adt_prints_the_instance_the_known_fields_and_the_round_trip() {
    let (mut fake, cell) = adt_fake(sysinfo_reply("sysinfo-750.xml"));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(exit.stderr, "");
    assert_eq!(
        lines(&exit),
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
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));

    let transport = cell.borrow();

    assert_eq!(transport.calls.len(), 1);
    assert_eq!(transport.calls[0].method, "GET");
    assert_eq!(transport.calls[0].uri, "/sap/bc/adt/system/information");
    assert_eq!(
        transport.calls[0].headers.get("Accept").map(String::as_str),
        Some("application/atom+xml;type=feed")
    );
    assert!(transport.calls[0].body.is_empty());
    assert_eq!(transport.closes, 1);
}

#[test]
fn the_758_feed_adds_entries_that_are_ignored_and_no_address_or_server_is_printed() {
    let (mut fake, _) = adt_fake(sysinfo_reply("sysinfo-758.xml"));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(
        lines(&exit),
        [
            "backend ADT HTTP",
            "instance apphost_S4D_11",
            "system id S4D",
            "kernel 793",
            "database HDB",
            "os Linux",
            "round trip <n> ms (includes logon)",
        ]
    );

    // The feed holds the address, the database server and the node name; none of them is printed.
    for value in [
        "192.0.2.1",
        "dbhost",
        "SAPHANADB",
        "NotAuthorized",
        "000000000850841174",
    ] {
        assert!(!exit.stdout.contains(value), "{value}: {}", exit.stdout);
    }
}

#[test]
fn a_release_without_the_feed_is_instance_unknown_and_still_exit_zero() {
    let (mut fake, cell) = adt_fake(sysinfo_reply("sysinfo-740-404.xml"));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(exit.stderr, "");
    assert_eq!(
        lines(&exit),
        [
            "backend ADT HTTP",
            "instance unknown (no /system/information on this release)",
            "round trip <n> ms (includes logon)",
        ]
    );
    assert_eq!(cell.borrow().closes, 1);
}

#[test]
fn a_406_is_instance_unknown_and_still_exit_zero() {
    let (mut fake, _) = adt_fake(Ok(Response::empty(406)));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(exit.stderr, "");
    assert_eq!(
        lines(&exit),
        [
            "backend ADT HTTP",
            "instance unknown (HTTP 406)",
            "round trip <n> ms (includes logon)",
        ]
    );
}

#[test]
fn a_feed_without_the_application_server_is_instance_unknown() {
    let (mut fake, _) = adt_fake(Ok(Response::text(
        200,
        r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom"><atom:entry><atom:id>KernelRelease</atom:id><atom:title>753</atom:title></atom:entry></atom:feed>"#,
    )));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(
        lines(&exit),
        [
            "backend ADT HTTP",
            "instance unknown (unreadable reply)",
            "kernel 753",
            "round trip <n> ms (includes logon)",
        ]
    );
}

#[test]
fn an_instance_name_without_the_host_sid_number_shape_has_no_system_id_line() {
    for name in [
        "apphost",
        "apphost_HD1",
        "apphost_hd1_00",
        "apphost_HD1_0",
        "apphost_HD10_00",
    ] {
        let xml = format!(
            r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom"><atom:entry><atom:id>ApplicationServerName</atom:id><atom:title>{name}</atom:title></atom:entry></atom:feed>"#
        );
        let (mut fake, _) = adt_fake(Ok(Response::text(200, xml)));
        let exit = ping(&["--conn", "dev_100"], &mut fake);

        assert_eq!(exit.code, 0, "{name}: {}", exit.stderr);
        assert!(
            !exit.stdout.contains("system id"),
            "{name}: {}",
            exit.stdout
        );
        assert!(exit.stdout.contains(&format!("instance {name}")), "{name}");
    }
}

#[test]
fn a_text_that_is_not_a_feed_is_rejected_with_exit_one() {
    let (mut fake, _) = adt_fake(Ok(Response::text(200, "<atom:feed><atom:entry>")));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    // An unclosed document ends at the end of the text without an XML error; an empty feed is an
    // unknown instance, never a crash.
    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.contains("instance unknown"), "{}", exit.stdout);

    let (mut fake, _) = adt_fake(Ok(Response::text(200, "<a></b>")));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 1);
    assert!(
        exit.stderr
            .starts_with("REJECTED: cannot read the system information reply")
    );
    assert_eq!(exit.stdout, "");
}

#[test]
fn another_status_and_a_refused_logon_are_failures() {
    for (status, text) in [(500, "answered HTTP 500"), (403, "answered HTTP 403")] {
        let (mut fake, _) = adt_fake(Ok(Response::empty(status)));
        let exit = ping(&["--conn", "dev_100", "--backend", "adt"], &mut fake);

        assert_ne!(exit.code, 0, "{status}");
        assert!(exit.stderr.contains(text), "{}", exit.stderr);
        assert_eq!(exit.stdout, "");
    }

    let (mut fake, _) = adt_fake(Ok(Response::empty(401)));
    let exit = ping(&["--conn", "dev_100", "--backend", "adt"], &mut fake);

    assert_ne!(exit.code, 0);
    assert!(exit.stderr.contains("authorization"), "{}", exit.stderr);
}

#[test]
fn a_lost_adt_connection_falls_back_to_rfc_and_names_the_backend_that_answered() {
    let (mut fake, _) = adt_fake(pre_send());
    let (rfc_cell, rfc) = shared(RfcTransport::new(FakeRfcApi::with_system_info(Ok(
        common::system_info(),
    ))));

    fake.rfc.push_back(rfc);

    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(lines(&exit)[0], "backend RFC SADT");
    assert!(exit.stdout.contains("instance apphost_HD1_00"));
    assert!(
        exit.stderr
            .contains("NOTE: ADT HTTP failed before anything was written"),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 1));
    assert_eq!(rfc_cell.borrow().api().system_info_calls(), 1);
}

#[test]
fn backend_adt_pins_the_path_and_never_falls_back() {
    let (mut fake, _) = adt_fake(pre_send());
    let exit = ping(&["--conn", "dev_100", "--backend", "adt"], &mut fake);

    assert_ne!(exit.code, 0);
    assert_eq!((fake.adt_opened, fake.rfc_opened), (1, 0));
    assert!(exit.stderr.contains("pins the backend"), "{}", exit.stderr);
}

#[test]
fn ping_over_rfc_calls_the_typed_function_once_and_prints_the_same_fields() {
    let (mut fake, cell) = rfc_fake(Ok(common::system_info()));
    let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(exit.stderr, "");
    assert_eq!(
        lines(&exit),
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
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1));
    // The API timed the invoke alone, so the line is exactly that time and names no logon.
    assert!(exit.stdout.ends_with("round trip 7 ms"), "{}", exit.stdout);

    let transport = cell.borrow();

    // One typed call on one session, no SADT request, and the session is ended.
    assert_eq!(transport.api().system_info_calls(), 1);
    assert_eq!(transport.api().counts(), (1, 1, 0));
}

#[test]
fn an_rfc_api_that_cannot_time_the_invoke_says_the_time_includes_the_logon() {
    let info = SystemInfo {
        invoke_time: None,
        ..common::system_info()
    };
    let (mut fake, _) = rfc_fake(Ok(info));
    let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(
        lines(&exit).last().map(String::as_str),
        Some("round trip <n> ms (includes logon)")
    );
}

#[test]
fn an_rfc_only_connection_pings_over_rfc_by_default() {
    let (mut fake, _) = rfc_fake(Ok(common::system_info()));
    let exit = ping(&["--conn", "qas_100"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(lines(&exit)[0], "backend RFC SADT");
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 1));
}

#[test]
fn a_backend_the_connection_lacks_is_a_stop() {
    let mut fake = Fake::default();
    let exit = ping(&["--conn", "web_100", "--backend", "rfc"], &mut fake);

    assert_eq!(exit.code, 2, "{}", exit.stderr);
    assert!(
        exit.stderr.contains("RFC SADT is not available for ping"),
        "{}",
        exit.stderr
    );
    assert_eq!((fake.adt_opened, fake.rfc_opened), (0, 0));
}

#[test]
fn an_empty_rfc_dest_is_instance_unknown() {
    let info = SystemInfo {
        instance: "   ".to_owned(),
        ..common::system_info()
    };
    let (mut fake, _) = rfc_fake(Ok(info));
    let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(lines(&exit)[1], "instance unknown (unreadable reply)");
}

#[test]
fn a_failed_rfc_call_is_the_usual_failure() {
    for (error, text) in [
        (
            TransportError::PreSendConnectivity("no route".to_owned()),
            "no route",
        ),
        (
            TransportError::Authorization("logon failed".to_owned()),
            "logon failed",
        ),
        (
            TransportError::Protocol("this RFC API has no RFC_SYSTEM_INFO call".to_owned()),
            "RFC_SYSTEM_INFO",
        ),
    ] {
        let (mut fake, _) = rfc_fake(Err(error));
        let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);

        assert_ne!(exit.code, 0, "{text}");
        assert!(exit.stderr.contains(text), "{}", exit.stderr);
        assert_eq!(exit.stdout, "");
    }
}

#[test]
fn ping_prints_the_instance_name_as_sap_reports_it_and_still_scrubs_the_password() {
    // The name holds the configured host on purpose: `ping` does not mask a host, `doctor` does.
    let info = SystemInfo {
        instance: "example.com_HD1_00 secret-pw".to_owned(),
        ..common::system_info()
    };
    let (mut fake, _) = rfc_fake(Ok(info));
    let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(
        exit.stdout.contains("instance example.com_HD1_00 ***"),
        "{}",
        exit.stdout
    );
    assert!(!exit.stdout.contains("secret-pw"), "{}", exit.stdout);
    assert!(!exit.stderr.contains("secret-pw"), "{}", exit.stderr);
}

#[test]
fn ping_prints_the_full_name_over_both_backends() {
    let (mut fake, _) = adt_fake(sysinfo_reply("sysinfo-750.xml"));
    let exit = ping(&["--conn", "dev_100"], &mut fake);

    assert!(
        exit.stdout.contains("instance apphost_HD1_00\n"),
        "{}",
        exit.stdout
    );

    let (mut fake, _) = rfc_fake(Ok(common::system_info()));
    let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);

    assert!(
        exit.stdout.contains("instance apphost_HD1_00\n"),
        "{}",
        exit.stdout
    );
}

#[test]
fn a_value_is_printed_as_one_short_clean_line() {
    let info = SystemInfo {
        instance: format!("a\u{202e}b\nc\td{}", "x".repeat(200)),
        release: "7\u{0}50".to_owned(),
        ..common::system_info()
    };
    let (mut fake, _) = rfc_fake(Ok(info));
    let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);
    let printed = lines(&exit);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    // A control or format character is dropped, a run of whitespace is one space, and a value
    // stops at 64 characters.
    assert!(printed[1].starts_with("instance ab c d"), "{}", printed[1]);
    assert_eq!(printed[1].chars().count(), "instance ".len() + 64);
    assert!(printed.contains(&"release 750".to_owned()), "{printed:?}");
    assert_eq!(printed.len(), 7);
}

#[test]
fn the_feed_parser_keeps_four_ids_and_ignores_every_other_entry() {
    let info = parse_system_information(
        &fs::read_to_string(common::fixture_path_in("m9", "sysinfo-758.xml")).unwrap(),
    )
    .unwrap();

    assert_eq!(info.application_server.as_deref(), Some("apphost_S4D_11"));
    assert_eq!(info.kernel_release.as_deref(), Some("793"));
    assert_eq!(info.database_system.as_deref(), Some("HDB"));
    assert_eq!(info.os_name.as_deref(), Some("Linux"));

    // A title with an entity, a CDATA section and surrounding blanks; the first entry of an id wins.
    let info = parse_system_information(
        r#"<f xmlns:atom="http://www.w3.org/2005/Atom">
            <atom:entry><atom:id> OSName </atom:id><atom:title> a&amp;b <![CDATA[c]]> </atom:title></atom:entry>
            <atom:entry><atom:id>OSName</atom:id><atom:title>second</atom:title></atom:entry>
            <atom:title>outside</atom:title>
        </f>"#,
    )
    .unwrap();

    assert_eq!(info.os_name.as_deref(), Some("a&b c"));
    assert_eq!(info.application_server, None);
}

#[test]
fn the_rfc_fields_are_the_six_named_ones_with_trailing_blanks_dropped() {
    let mut asked = Vec::new();
    let info = SystemInfo::from_fields(|name| {
        asked.push(name.to_owned());

        Ok(format!("<{name}>   "))
    })
    .unwrap();

    assert_eq!(
        asked,
        [
            "RFCDEST",
            "RFCHOST",
            "RFCSYSID",
            "RFCSAPRL",
            "RFCKERNRL",
            "RFCDBSYS"
        ]
    );
    assert_eq!(info.instance, "<RFCDEST>");
    assert_eq!(info.host, "<RFCHOST>");
    assert_eq!(info.system_id, "<RFCSYSID>");
    assert_eq!(info.release, "<RFCSAPRL>");
    assert_eq!(info.kernel, "<RFCKERNRL>");
    assert_eq!(info.database, "<RFCDBSYS>");

    // `RFCHOST` is optional: a failed read leaves only that field empty.
    let info = SystemInfo::from_fields(|name| {
        if name == "RFCHOST" {
            Err(TransportError::Protocol("no host".to_owned()))
        } else {
            Ok(format!("<{name}>"))
        }
    })
    .unwrap();

    assert_eq!(info.host, "");
    assert_eq!(info.instance, "<RFCDEST>");
    assert_eq!(info.invoke_time, None);

    let error = SystemInfo::from_fields(|name| {
        if name == "RFCSAPRL" {
            Err(TransportError::Protocol("read failed".to_owned()))
        } else {
            Ok(String::new())
        }
    })
    .unwrap_err();

    assert_eq!(error, TransportError::Protocol("read failed".to_owned()));
}

#[test]
fn the_sysinfo_fixtures_carry_no_live_value() {
    for name in ["sysinfo-750.xml", "sysinfo-758.xml", "sysinfo-740-404.xml"] {
        let text = fs::read_to_string(common::fixture_path_in("m9", name)).unwrap();

        assert!(!text.contains("saphd1db"), "{name}");
        assert!(!text.contains("sap-app-dv"), "{name}");
    }
}

#[test]
fn a_reply_that_cannot_be_read_never_quotes_its_content() {
    for body in [
        r#"<atom:feed xmlns:atom="http://www.w3.org/2005/Atom"><atom:entry><atom:id>ApplicationServerName</atom:id><atom:title>&apphost.corp.net;</atom:title></atom:entry></atom:feed>"#,
        "<a></apphost.corp.net>",
    ] {
        let (mut fake, _) = adt_fake(Ok(Response::text(200, body)));
        let exit = ping(&["--conn", "dev_100", "--backend", "adt"], &mut fake);

        assert_eq!(exit.code, 1, "{body}");
        assert_eq!(
            exit.stderr,
            "REJECTED: cannot read the system information reply"
        );
        assert_eq!(exit.stdout, "");
    }
}

#[test]
fn a_secret_is_scrubbed_before_the_value_is_cleaned_and_cut() {
    // The password starts at character 60 of the value: a cut before the scrub would leave its
    // first four characters.
    let info = SystemInfo {
        instance: format!("{}secret-pw", "x".repeat(60)),
        release: "se\u{200b}cret-pw".to_owned(),
        kernel: "a secret-pw b".to_owned(),
        ..common::system_info()
    };
    let (mut fake, _) = rfc_fake(Ok(info));
    let exit = ping(&["--conn", "dev_100", "--backend", "rfc"], &mut fake);
    let printed = lines(&exit);

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(printed[1], format!("instance {}***", "x".repeat(60)));
    assert!(printed.contains(&"release ***".to_owned()), "{printed:?}");
    assert!(
        printed.contains(&"kernel a *** b".to_owned()),
        "{printed:?}"
    );

    for fragment in ["secret", "ecret-pw", "-pw", "cret"] {
        assert!(
            !exit.stdout.contains(fragment),
            "{fragment}: {}",
            exit.stdout
        );
    }
}

#[test]
fn a_router_and_its_fragments_are_scrubbed_from_every_value() {
    let directory = tempfile::tempdir().unwrap();
    let config = concat!(
        r#"{"servers":[{"systemId":"RTR","clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00","saprouter":"/H/router.example.net/S/3299/H/gw.example.net"}}]}"#,
    );

    fs::write(directory.path().join("connections.json"), config).unwrap();

    let env = BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory
                .path()
                .join("connections.json")
                .display()
                .to_string(),
        ),
        ("RTR_100_USER".to_owned(), "user".to_owned()),
        ("RTR_100_PASS".to_owned(), "secret-pw".to_owned()),
    ]);
    let info = SystemInfo {
        instance: "x router.example.net y".to_owned(),
        database: "gw.example.net\u{200b}".to_owned(),
        release: "gw.exam\u{200b}ple.net".to_owned(),
        ..common::system_info()
    };
    let (mut fake, _) = rfc_fake(Ok(info));
    let exit = run(
        ["abapreach", "ping", "--conn", "rtr_100"]
            .map(str::to_owned)
            .to_vec(),
        env,
        &mut fake,
    );

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert!(exit.stdout.contains("instance x *** y"), "{}", exit.stdout);
    assert!(exit.stdout.contains("database ***"), "{}", exit.stdout);
    assert!(exit.stdout.contains("release ***"), "{}", exit.stdout);

    for fragment in ["router.example", "gw.example", "example.net"] {
        assert!(
            !exit.stdout.contains(fragment),
            "{fragment}: {}",
            exit.stdout
        );
    }
}

const ROUTE: &str = "/H/router.corp.example/S/3299/W/routerpw/H/10.9.8.7/S/3200";
/// Every piece of the connection that must never reach an output.
const SECRETS: [&str; 5] = [
    "secret-pw",
    ROUTE,
    "router.corp.example",
    "routerpw",
    "10.9.8.7",
];

/// `ping --backend rfc` on `dev_100` with the route set, over `transport`.
fn ping_with_route(transport: impl SadtTransport + 'static) -> Exit {
    let directory = tempfile::tempdir().unwrap();
    let mut env = env(directory.path());
    let mut fake = Fake::default();

    env.insert("DEV_SAPROUTER".to_owned(), ROUTE.to_owned());
    fake.rfc.push_back(shared(transport).1);

    run(
        ["abapreach", "ping", "--conn", "dev_100", "--backend", "rfc"]
            .map(str::to_owned)
            .to_vec(),
        env,
        &mut fake,
    )
}

fn assert_no_secret(exit: &Exit, context: &str) {
    let output = format!("{}\n{}", exit.stdout, exit.stderr);

    for secret in SECRETS {
        assert!(
            !output.contains(secret),
            "{context} leaks {secret}: {output}"
        );
    }
}

#[test]
fn an_error_text_with_a_secret_or_a_route_never_reaches_the_ping_output() {
    let leaking = format!(
        "logon secret-pw failed via {ROUTE} host router.corp.example pw routerpw gw 10.9.8.7"
    );

    for error in [
        TransportError::Authorization(leaking.clone()),
        TransportError::Protocol(leaking.clone()),
        TransportError::PreSendConnectivity(leaking.clone()),
        TransportError::NoResponse(leaking.clone()),
        TransportError::PostSendUncertain(leaking.clone()),
        TransportError::Http {
            status: 502,
            message: leaking.clone(),
        },
        TransportError::Protocol(leaking.clone()).after_send(),
    ] {
        let exit = ping_with_route(
            common::ScriptedTransport::replies(Vec::new()).with_system_info(Err(error.clone())),
        );

        assert_ne!(exit.code, 0, "{error:?}");
        assert_eq!(exit.stdout, "", "{error:?}");
        assert_no_secret(&exit, &format!("{error:?}"));
    }
}

#[test]
fn a_close_note_with_a_secret_or_a_route_never_reaches_the_ping_output() {
    let leaking = format!("close secret-pw via {ROUTE} router.corp.example routerpw 10.9.8.7");
    let exit = ping_with_route(common::CloseFails {
        inner: common::ScriptedTransport::replies(Vec::new())
            .with_system_info(Ok(common::system_info())),
        error: TransportError::Protocol(leaking),
    });

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_eq!(lines(&exit)[1], "instance apphost_HD1_00");
    // The note is there, so the masking is tested on a line that exists.
    assert!(
        exit.stderr.contains("session close failed"),
        "{}",
        exit.stderr
    );
    assert_no_secret(&exit, "the close note");
}

#[test]
fn a_secret_split_by_a_format_character_in_a_reading_never_reaches_the_ping_output() {
    let info = SystemInfo {
        instance: "sec\u{200b}ret-pw_HD1_00".to_owned(),
        release: "r\u{202e}outerpw".to_owned(),
        kernel: "10.9\u{200b}.8.7".to_owned(),
        database: "router.corp.\u{2060}example".to_owned(),
        ..common::system_info()
    };
    let exit =
        ping_with_route(common::ScriptedTransport::replies(Vec::new()).with_system_info(Ok(info)));

    assert_eq!(exit.code, 0, "{}", exit.stderr);
    assert_no_secret(&exit, "the reading");
    assert!(
        exit.stdout.contains("instance ***_HD1_00"),
        "{}",
        exit.stdout
    );
}
