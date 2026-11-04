use abapreach::{
    cli::{Backend, Cli, Command, ObjectKind},
    hash::{local_source_lines, normalize, source_hash, validate_prog},
};
use clap::Parser;
use std::path::PathBuf;

const V1_RESPONSE_BODY: &str = "REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v1'.";
const V1_HASH: &str = "63886692bf652fa419147f746d6b7760d6c991606fa9603d8ade3caf08ac7c64";

#[test]
fn fixture_v1_hash() {
    let source = V1_RESPONSE_BODY
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();

    assert_eq!(
        source,
        vec![
            "REPORT ytest_abapreach01.\r".to_owned(),
            "\r".to_owned(),
            "WRITE / 'v1'.".to_owned(),
        ]
    );
    assert_eq!(
        normalize(source.clone()),
        vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            "WRITE / 'v1'.".to_owned(),
        ]
    );
    assert_eq!(source_hash(&source), V1_HASH);
}

#[test]
fn normalization_and_validation() {
    let lines = vec!["a  \r".to_owned(), "b\t".to_owned(), String::new()];

    assert_eq!(
        normalize(lines.clone()),
        vec!["a".to_owned(), "b\t".to_owned()]
    );
    assert_eq!(
        source_hash(&lines),
        "446e2f450392011e73fb48bd72b2afe4ba2dc19e1ec0a9f2e4ca9242c14af857"
    );
    assert!(validate_prog(&"X".repeat(40), &["x".to_owned()]).is_ok());
    assert!(validate_prog("Z", &["x".repeat(255)]).is_ok());
    assert!(validate_prog(&"X".repeat(41), &["x".to_owned()]).is_err());
    assert!(validate_prog("Z", &["x".repeat(256)]).is_err());
    assert!(validate_prog("Z", &[]).is_err());
}

#[test]
fn deploy_strips_only_leading_utf8_bom_and_normalizes_crlf() {
    let lines = local_source_lines("\u{feff}REPORT z.  \r\n\r\nWRITE / 'v1'.\r\n");

    assert_eq!(
        lines,
        vec![
            "REPORT z.  \r".to_owned(),
            "\r".to_owned(),
            "WRITE / 'v1'.\r".to_owned(),
            String::new(),
        ]
    );
    assert_eq!(
        normalize(lines),
        vec![
            "REPORT z.".to_owned(),
            String::new(),
            "WRITE / 'v1'.".to_owned(),
        ]
    );
}

#[test]
fn local_source_without_a_bom_is_split_unchanged() {
    assert_eq!(
        local_source_lines("a\r\nb"),
        vec!["a\r".to_owned(), "b".to_owned()]
    );
    assert_eq!(local_source_lines(""), vec![String::new()]);
}

#[test]
fn a_bom_outside_the_first_position_is_kept() {
    assert_eq!(
        local_source_lines("a\n\u{feff}b"),
        vec!["a".to_owned(), "\u{feff}b".to_owned()]
    );
    // Only one BOM is a byte-order mark; a second one at the start is content.
    assert_eq!(
        local_source_lines("\u{feff}\u{feff}a"),
        vec!["\u{feff}a".to_owned()]
    );
}

#[test]
fn bom_and_crlf_source_hashes_like_the_plain_lf_source() {
    let windows = local_source_lines("\u{feff}REPORT ytest_abapreach01.\r\n\r\nWRITE / 'v1'.\r\n");
    let unix = local_source_lines("REPORT ytest_abapreach01.\n\nWRITE / 'v1'.\n");

    assert_eq!(source_hash(&windows), V1_HASH);
    assert_eq!(source_hash(&windows), source_hash(&unix));
    assert!(validate_prog("YTEST_ABAPREACH01", &windows).is_ok());
}

#[test]
fn cli_only_accepts_m1_commands_with_a_connection() {
    let read_args = ["abapreach", "read", "PROG", "Z", "o", "--conn", "dev_100"];
    let read = Cli::try_parse_from(read_args).unwrap();

    match read.command {
        Command::Read {
            kind,
            name,
            out,
            force,
            conn,
            ..
        } => {
            assert_eq!(conn, "dev_100");
            assert_eq!(kind, ObjectKind::Prog);
            assert_eq!(name, "Z");
            assert_eq!(out, PathBuf::from("o"));
            assert!(!force);
        }
        _ => panic!("expected read command"),
    }

    let deploy = Cli::try_parse_from([
        "abapreach",
        "deploy",
        "PROG",
        "Z",
        "source.abap",
        "--simulate",
        "--base",
        V1_HASH,
        "--tr",
        "DEVK000001",
        "--backend",
        "rfc",
        "--conn",
        "dev_100",
    ])
    .unwrap();

    match deploy.command {
        Command::Deploy {
            kind,
            name,
            file,
            simulate,
            base,
            tr,
            backend,
            conn,
            ..
        } => {
            assert_eq!(conn, "dev_100");
            assert_eq!(kind, ObjectKind::Prog);
            assert_eq!(name, "Z");
            assert_eq!(file, PathBuf::from("source.abap"));
            assert!(simulate);
            assert_eq!(base.as_deref(), Some(V1_HASH));
            assert_eq!(tr.as_deref(), Some("DEVK000001"));
            assert_eq!(backend, Some(Backend::Rfc));
        }
        _ => panic!("expected deploy command"),
    }

    let doctor = Cli::try_parse_from(["abapreach", "doctor", "--conn", "dev_100"]).unwrap();

    assert!(matches!(doctor.command, Command::Doctor { conn } if conn == "dev_100"));
    assert!(Cli::try_parse_from(["abapreach", "doctor"]).is_err());
    assert!(Cli::try_parse_from(["abapreach", "deploy", "PROG", "Z", "file"]).is_err());
    assert!(Cli::try_parse_from(["abapreach", "read", "PROG", "Z", "o"]).is_err());
}
