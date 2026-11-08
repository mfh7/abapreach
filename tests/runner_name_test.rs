mod common;

use abapreach::config::{Config, ConfigPaths, Connection};
use abapreach::operation::deploy::CancelFlag;
use abapreach::operation::runner::{
    CreateError, DEFAULT_RUNNER_PREFIX, create_with_fresh_name, runner_name, runner_name_with,
    validate_runner_prefix,
};
use abapreach::transport::{Response, TransportError};
use abapreach::{Factories, Opened, RfcLibraryInfo, run};
use common::ScriptedTransport;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

const SUFFIX_ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

fn config_text(prefix_member: &str) -> String {
    format!(
        concat!(
            r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
            r#""adt":{{"insecure":true}}{}}}]}}"#
        ),
        prefix_member
    )
}

fn env(directory: &Path, prefix_member: &str) -> BTreeMap<String, String> {
    fs::write(
        directory.join("connections.json"),
        config_text(prefix_member),
    )
    .unwrap();

    BTreeMap::from([
        (
            "ABAP_CONNECTIONS".to_owned(),
            directory.join("connections.json").display().to_string(),
        ),
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "secret-pw".to_owned()),
    ])
}

fn load(directory: &Path, prefix_member: &str) -> Result<Config, String> {
    let env = env(directory, prefix_member);

    Config::load(
        ConfigPaths {
            connections: directory.join("connections.json"),
            local: directory.join("connections.local.json"),
        },
        &env,
    )
}

fn resolved(directory: &Path, prefix_member: &str) -> Connection {
    let env = env(directory, prefix_member);

    load(directory, prefix_member)
        .unwrap()
        .resolve("dev_100", &env)
        .unwrap()
}

/// Factories that must never be asked for a connection.
struct NoConnection;

impl Factories for NoConnection {
    fn adt(&mut self, _: &Connection) -> Opened {
        panic!("no connection may be opened")
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        panic!("no connection may be opened")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("no connection may be opened")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("no connection may be opened")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

/// Doctor factories for an ADT-only server: one discovery answer.
struct AdtOnly;

impl Factories for AdtOnly {
    fn adt(&mut self, _: &Connection) -> Opened {
        Ok(Box::new(ScriptedTransport::replies(vec![Ok(
            Response::text(200, "<app:service/>"),
        )])))
    }

    fn rfc(&mut self, _: &Connection) -> Opened {
        panic!("doctor must not open an SADT RFC session")
    }

    fn rfc_library(&mut self, _: &Connection) -> Result<RfcLibraryInfo, String> {
        panic!("ADT-only server has no RFC library probe")
    }

    fn rfc_ping(&mut self, _: &Connection) -> Result<(), TransportError> {
        panic!("ADT-only server has no RFC ping")
    }

    fn cancel(&self) -> CancelFlag {
        CancelFlag::new()
    }
}

fn conflict() -> TransportError {
    TransportError::Http {
        status: 409,
        message: "class already exists".to_owned(),
    }
}

#[test]
fn the_default_prefix_is_the_fixed_product_prefix() {
    assert_eq!(DEFAULT_RUNNER_PREFIX, "YCL_ABAPREACH_RUN_");
    assert_eq!(validate_runner_prefix(DEFAULT_RUNNER_PREFIX), Ok(()));
}

#[test]
fn a_generated_name_is_prefix_plus_eight_alphabet_characters_within_thirty() {
    let name = runner_name(DEFAULT_RUNNER_PREFIX);
    let suffix = name.strip_prefix(DEFAULT_RUNNER_PREFIX).unwrap();

    assert_eq!(suffix.len(), 8);
    assert!(suffix.chars().all(|c| SUFFIX_ALPHABET.contains(c)));
    assert_eq!(name.len(), 26);
    assert!(name.len() <= 30);
}

#[test]
fn a_maximal_prefix_gives_exactly_thirty_characters() {
    let prefix = format!("Y{}", "A".repeat(21));

    assert_eq!(prefix.len(), 22);
    assert_eq!(validate_runner_prefix(&prefix), Ok(()));
    assert_eq!(runner_name(&prefix).len(), 30);
}

#[test]
fn many_draws_are_unique_and_use_the_whole_alphabet() {
    let names: HashSet<String> = (0..5000)
        .map(|_| runner_name(DEFAULT_RUNNER_PREFIX))
        .collect();
    let used: HashSet<char> = names
        .iter()
        .flat_map(|name| name[DEFAULT_RUNNER_PREFIX.len()..].chars())
        .collect();

    assert_eq!(names.len(), 5000);
    assert_eq!(used.len(), 36);
}

#[test]
fn the_suffix_comes_from_the_byte_source_without_modulo_bias() {
    // 252 = 7 * 36 is the first byte value that is rejected; 0..=251 map by `% 36`.
    let mut bytes = vec![252u8, 255, 0, 1, 35, 36, 251, 71].into_iter();
    let mut calls = 0;
    let name = runner_name_with("YA_", &mut |buffer: &mut [u8]| {
        calls += 1;
        for slot in buffer.iter_mut() {
            *slot = bytes.next().unwrap_or(2);
        }
    });

    // 252 and 255 are discarded; 0, 1, 35, 36, 251, 71 give 0, 1, Z, 0, Z, Z; the redraw adds 2, 2.
    assert_eq!(name, "YA_01Z0ZZ22");
    assert_eq!(calls, 2);
}

#[test]
fn a_custom_prefix_in_range_is_used_verbatim() {
    let name = runner_name("ZMY_RUN_1_");

    assert!(name.starts_with("ZMY_RUN_1_"));
    assert_eq!(name.len(), 18);
    assert_eq!(validate_runner_prefix("ZMY_RUN_1_"), Ok(()));
    assert_eq!(validate_runner_prefix("Y"), Ok(()));
}

#[test]
fn every_invalid_prefix_class_is_rejected_by_the_validator() {
    let too_long = format!("Y{}", "A".repeat(22));

    for (label, prefix) in [
        ("empty", ""),
        ("lower case", "Ycl_run_"),
        ("wrong first letter", "ACL_RUN_"),
        ("digit first", "1CL_RUN_"),
        ("underscore first", "_YCL_RUN_"),
        ("too long", too_long.as_str()),
        ("hyphen", "Y-RUN_"),
        ("space", "Y RUN"),
        ("quote", "Y'RUN"),
        ("slash", "Y/RUN/"),
        ("dollar", "Y$RUN"),
        ("newline", "YRUN\n"),
        ("non ASCII", "YRÜN_"),
    ] {
        assert!(
            validate_runner_prefix(prefix).is_err(),
            "{label} must be rejected"
        );
    }
}

#[test]
fn without_the_key_the_effective_prefix_is_the_default_and_never_a_test_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let connection = resolved(directory.path(), "");

    assert_eq!(connection.runner_prefix, DEFAULT_RUNNER_PREFIX);
    assert!(!connection.runner_prefix.starts_with("YTEST_ABAPREACH"));
    assert!(
        (0..200)
            .map(|_| runner_name(&connection.runner_prefix))
            .all(|name| !name.starts_with("YTEST_ABAPREACH"))
    );
}

#[test]
fn the_runner_prefix_key_sets_the_effective_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let connection = resolved(
        directory.path(),
        r#","runner_prefix":"YTEST_ABAPREACH_RUN_""#,
    );

    assert_eq!(connection.runner_prefix, "YTEST_ABAPREACH_RUN_");
}

#[test]
fn an_invalid_prefix_stops_the_config_load_naming_the_key() {
    let too_long = format!("Y{}", "A".repeat(22));

    for value in [
        "",
        "ycl_run_",
        "ACL_RUN_",
        too_long.as_str(),
        "Y-RUN",
        "Y RUN",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let error =
            load(directory.path(), &format!(r#","runner_prefix":"{value}""#)).expect_err(value);

        assert!(error.contains("servers[0].runner_prefix"), "{error}");
    }
}

#[test]
fn a_non_string_prefix_stops_the_config_load() {
    let directory = tempfile::tempdir().unwrap();

    assert!(load(directory.path(), r#","runner_prefix":7"#).is_err());
}

#[test]
fn an_invalid_prefix_is_stop_exit_2_before_any_connection() {
    let directory = tempfile::tempdir().unwrap();
    let env = env(directory.path(), r#","runner_prefix":"lower_""#);
    let exit = run(
        ["abapreach", "doctor", "--conn", "dev_100"]
            .map(str::to_owned)
            .to_vec(),
        env,
        &mut NoConnection,
    );

    assert_eq!(exit.code, 2);
    assert!(exit.stderr.starts_with("STOP: "), "{}", exit.stderr);
    assert!(exit.stderr.contains("runner_prefix"), "{}", exit.stderr);
    assert_eq!(exit.stdout, "");
}

#[test]
fn doctor_prints_the_effective_prefix() {
    for (member, expected) in [
        ("", "RUNNER prefix: YCL_ABAPREACH_RUN_"),
        (
            r#","runner_prefix":"YTEST_ABAPREACH_RUN_""#,
            "RUNNER prefix: YTEST_ABAPREACH_RUN_",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let exit = run(
            ["abapreach", "doctor", "--conn", "dev_100"]
                .map(str::to_owned)
                .to_vec(),
            env(directory.path(), member),
            &mut AdtOnly,
        );

        assert_eq!(exit.code, 0, "{}", exit.stderr);
        assert!(
            exit.stdout.lines().any(|line| line == expected),
            "{}",
            exit.stdout
        );
    }
}

#[test]
fn a_free_name_is_created_once() {
    let mut seen = Vec::new();
    let mut names = ["YCL_X_AAAAAAAA", "YCL_X_BBBBBBBB"].into_iter();
    let name = create_with_fresh_name(
        || names.next().unwrap().to_owned(),
        |name| {
            seen.push(name.to_owned());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(name, "YCL_X_AAAAAAAA");
    assert_eq!(seen, ["YCL_X_AAAAAAAA"]);
}

#[test]
fn one_collision_retries_once_with_a_new_name() {
    let mut seen = Vec::new();
    let mut names = ["YCL_X_AAAAAAAA", "YCL_X_BBBBBBBB", "YCL_X_CCCCCCCC"].into_iter();
    let name = create_with_fresh_name(
        || names.next().unwrap().to_owned(),
        |name| {
            seen.push(name.to_owned());
            if name == "YCL_X_AAAAAAAA" {
                Err(conflict())
            } else {
                Ok(())
            }
        },
    )
    .unwrap();

    assert_eq!(name, "YCL_X_BBBBBBBB");
    assert_eq!(seen, ["YCL_X_AAAAAAAA", "YCL_X_BBBBBBBB"]);
}

#[test]
fn two_collisions_stop_without_a_third_attempt() {
    let mut seen = 0;
    let mut names = ["YCL_X_AAAAAAAA", "YCL_X_BBBBBBBB", "YCL_X_CCCCCCCC"].into_iter();
    let error = create_with_fresh_name(
        || names.next().unwrap().to_owned(),
        |_| {
            seen += 1;
            Err(conflict())
        },
    )
    .unwrap_err();

    assert!(matches!(error, CreateError::TwoCollisions));
    assert_eq!(seen, 2);
    assert!(error.to_string().contains("twice"), "{error}");
}

#[test]
fn another_failure_is_not_retried() {
    let mut seen = 0;
    let error = create_with_fresh_name(
        || "YCL_X_AAAAAAAA".to_owned(),
        |_| {
            seen += 1;
            Err(TransportError::Http {
                status: 403,
                message: "forbidden".to_owned(),
            })
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CreateError::Failed(TransportError::Http { status: 403, .. })
    ));
    assert_eq!(seen, 1);
}

#[test]
fn a_collision_then_another_failure_is_that_failure() {
    let mut seen = 0;
    let mut names = ["YCL_X_AAAAAAAA", "YCL_X_BBBBBBBB"].into_iter();
    let error = create_with_fresh_name(
        || names.next().unwrap().to_owned(),
        |_| {
            seen += 1;
            if seen == 1 {
                Err(conflict())
            } else {
                Err(TransportError::PreSendConnectivity("down".to_owned()))
            }
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CreateError::Failed(TransportError::PreSendConnectivity(_))
    ));
    assert_eq!(seen, 2);
}
