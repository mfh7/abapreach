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
    let adt = connection.adt.clone().unwrap();

    assert_eq!(adt.host, "example.com");
    assert_eq!(adt.port, 443);
    assert_eq!(adt.scheme, "https");
    assert_eq!(adt.ca_file, None);
    assert!(!adt.insecure);
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

    let adt = http_connection.adt.unwrap();

    assert_eq!(adt.scheme, "http");
    assert_eq!(adt.ca_file, Some(PathBuf::from("ca.pem")));
    assert!(adt.insecure);
}

#[test]
fn a_server_without_an_adt_endpoint_resolves_as_rfc_only() {
    let directory = tempfile::tempdir().unwrap();
    let connections = directory.path().join("connections.json");
    let config = concat!(
        r#"{"servers":[{"systemId":"QAS","clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"02"}},{"systemId":"DEV","#,
        r#""host":"example.com","port":443,"clients":["100"],"#,
        r#""rfc":{"ashost":"example.com","sysnr":"00"}}]}"#,
    );
    fs::write(&connections, config).unwrap();
    let env = BTreeMap::from([
        ("QAS_100_USER".to_owned(), "qas-user".to_owned()),
        ("QAS_100_PASS".to_owned(), "qas-password".to_owned()),
        ("DEV_100_USER".to_owned(), "dev-user".to_owned()),
        ("DEV_100_PASS".to_owned(), "dev-password".to_owned()),
    ]);
    let config = Config::load(
        ConfigPaths {
            connections,
            local: directory.path().join("connections.local.json"),
        },
        &env,
    )
    .unwrap();
    let rfc_only = config.resolve("qas_100", &env).unwrap();

    assert_eq!(rfc_only.id, "QAS_100");
    assert_eq!(rfc_only.adt, None);
    assert_eq!(rfc_only.rfc.ashost, "example.com");
    assert_eq!(rfc_only.rfc.sysnr, "02");
    assert_eq!(tls_warning(&rfc_only), None);
    assert_eq!(
        config.resolve("dev_100", &env).unwrap().adt.unwrap().host,
        "example.com"
    );
}

#[test]
fn a_half_configured_adt_endpoint_names_the_missing_key() {
    let directory = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([
        ("QAS_100_USER".to_owned(), "user".to_owned()),
        ("QAS_100_PASS".to_owned(), "password".to_owned()),
    ]);
    let resolve = |server: &str| {
        let connections = directory.path().join("connections.json");
        fs::write(
            &connections,
            format!(
                concat!(
                    r#"{{"servers":[{{"systemId":"QAS",{server},"clients":["100"],"#,
                    r#""rfc":{{"ashost":"example.com","sysnr":"00"}}}}]}}"#,
                ),
                server = server
            ),
        )
        .unwrap();
        Config::load(
            ConfigPaths {
                connections,
                local: directory.path().join("connections.local.json"),
            },
            &env,
        )
        .unwrap()
        .resolve("qas_100", &env)
        .unwrap_err()
    };

    assert_eq!(
        resolve(r#""host":"example.com""#),
        "QAS has host but no port in config"
    );
    assert_eq!(
        resolve(r#""port":443"#),
        "QAS has port but no host in config"
    );
}

#[test]
fn warning_and_scrub_cover_insecure_and_http_connections() {
    let mut connection = Connection::test_insecure();
    assert_eq!(
        tls_warning(&connection).as_deref(),
        Some("WARNING: connection DEV_100 uses insecure TLS or HTTP.")
    );
    connection.adt.as_mut().unwrap().insecure = false;
    connection.adt.as_mut().unwrap().scheme = "http".to_owned();
    assert_eq!(
        tls_warning(&connection).as_deref(),
        Some("WARNING: connection DEV_100 uses insecure TLS or HTTP.")
    );
    connection.adt.as_mut().unwrap().scheme = "https".to_owned();
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
