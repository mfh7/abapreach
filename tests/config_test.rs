use abapreach::config::{Config, ConfigPaths, Connection, scrub, tls_warning};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[test]
fn resolves_config_env_credentials_and_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let connections = directory.path().join("connections.json");
    let local = directory.path().join("connections.local.json");
    let config = concat!(
        r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"clients":["100"],"#,
        r#""client":"ignored","rfc":{"ashost":"example.com","sysnr":"00"}},{"systemId":"DV2","#,
        r#""host":"example.com","port":80,"clients":["100"],"rfc":{"ashost":"example.com","#,
        r#""sysnr":"01"},"adt":{"scheme":"http","ca_file":"ca.pem","insecure":true}}]}"#,
    );
    fs::write(&connections, config).unwrap();
    fs::write(
        &local,
        r#"{"DEV_100_USER":"local-user","DEV_100_PASS":"local-password"}"#,
    )
    .unwrap();
    let env = BTreeMap::from([
        ("DEV_100_USER".to_owned(), "environment-user".to_owned()),
        ("DEV_100_PASS".to_owned(), "environment-password".to_owned()),
        ("DV2_100_USER".to_owned(), "dv2-user".to_owned()),
        ("DV2_100_PASS".to_owned(), "dv2-password".to_owned()),
    ]);
    let connection = Config::load(ConfigPaths { connections, local }, &env)
        .unwrap()
        .resolve("dev_100", &env)
        .unwrap();

    assert_eq!(connection.id, "DEV_100");
    assert_eq!(connection.lang, "EN");
    assert_eq!(connection.user, "environment-user");
    assert_eq!(connection.password, "environment-password");
    assert_eq!(connection.adt.host, "example.com");
    assert_eq!(connection.adt.port, 443);
    assert_eq!(connection.adt.scheme, "https");
    assert_eq!(connection.adt.ca_file, None);
    assert!(!connection.adt.insecure);
    assert_eq!(connection.rfc.ashost, "example.com");
    assert_eq!(connection.rfc.sysnr, "00");
    assert_eq!(connection.rfc.saprouter, None);
    let http_connection = Config::load(
        ConfigPaths {
            connections: directory.path().join("connections.json"),
            local: directory.path().join("connections.local.json"),
        },
        &env,
    )
    .unwrap()
    .resolve("dv2_100", &env)
    .unwrap();

    assert_eq!(http_connection.adt.scheme, "http");
    assert_eq!(http_connection.adt.ca_file, Some(PathBuf::from("ca.pem")));
    assert!(http_connection.adt.insecure);
}

#[test]
fn warning_and_scrub_cover_insecure_and_http_connections() {
    let mut connection = Connection::test_insecure();
    assert_eq!(
        tls_warning(&connection).as_deref(),
        Some("WARNING: connection DEV_100 uses insecure TLS or HTTP.")
    );
    connection.adt.insecure = false;
    connection.adt.scheme = "http".to_owned();
    assert_eq!(
        tls_warning(&connection).as_deref(),
        Some("WARNING: connection DEV_100 uses insecure TLS or HTTP.")
    );
    connection.adt.scheme = "https".to_owned();
    assert_eq!(tls_warning(&connection), None);
    assert_eq!(
        scrub("password=secret", &["secret".to_owned(), "".to_owned()]),
        "password=***"
    );
}

#[test]
fn local_file_is_optional_but_an_unreadable_one_stops() {
    let directory = tempfile::tempdir().unwrap();
    let connections = directory.path().join("connections.json");
    let config = r#"{"servers":[{"systemId":"DEV","host":"example.com","port":443,"#;
    let config =
        format!(r#"{config}"clients":["100"],"rfc":{{"ashost":"example.com","sysnr":"00"}}}}]}}"#);
    fs::write(&connections, config).unwrap();
    let env = BTreeMap::from([
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "password".to_owned()),
    ]);
    let missing = ConfigPaths {
        connections: connections.clone(),
        local: directory.path().join("absent.json"),
    };
    let connection = Config::load(missing, &env)
        .unwrap()
        .resolve("DEV_100", &env)
        .unwrap();

    assert_eq!(connection.user, "user");

    let unreadable = ConfigPaths {
        connections,
        local: directory.path().to_owned(),
    };
    assert!(
        Config::load(unreadable, &env)
            .unwrap_err()
            .starts_with("cannot read")
    );
}

#[test]
fn stops_for_bad_ids_missing_credentials_and_bad_default_paths() {
    assert_eq!(
        Config::parse_id("dev_land_100").unwrap(),
        ("DEV_LAND".to_owned(), "100".to_owned())
    );
    assert!(Config::parse_id("dev").is_err());
    assert!(Config::parse_id("dev_").is_err());
    let missing = BTreeMap::from([("DEV_100_USER".to_owned(), "user".to_owned())]);
    assert_eq!(
        Config::credentials("DEV", "100", &missing, &BTreeMap::new()).unwrap_err(),
        "missing DEV_100_PASS"
    );
    let local = BTreeMap::from([("DEV_100_PASS".to_owned(), "local-password".to_owned())]);
    assert_eq!(
        Config::credentials("DEV", "100", &missing, &local).unwrap(),
        ("user".to_owned(), "local-password".to_owned())
    );
    let defaults = ConfigPaths::from_env(&BTreeMap::from([(
        "HOME".to_owned(),
        "/tmp/home".to_owned(),
    )]))
    .unwrap();
    assert_eq!(
        defaults.connections,
        PathBuf::from("/tmp/home").join(".config/abap-rfc/connections.json")
    );
    assert_eq!(
        defaults.local,
        PathBuf::from("/tmp/home").join(".config/abap-rfc/connections.local.json")
    );
    let overridden = ConfigPaths::from_env(&BTreeMap::from([
        ("HOME".to_owned(), "/tmp/home".to_owned()),
        ("ABAP_CONNECTIONS".to_owned(), "~/custom.json".to_owned()),
    ]))
    .unwrap();
    assert_eq!(
        overridden.connections,
        PathBuf::from("/tmp/home/custom.json")
    );
    assert_eq!(
        overridden.local,
        PathBuf::from("/tmp/home/connections.local.json")
    );
}
