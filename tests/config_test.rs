use abapreach::config::{
    Config, ConfigPaths, Connection, LocalCredentialsPermission, SaprouterSource,
    classify_credentials_mode, local_credentials_permission, scrub, tls_warning,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
fn stops_for_bad_ids_and_missing_credentials() {
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
}

/// The default location stays Python-tooling compatible on every OS: `<home>/.config/abap-rfc`.
#[test]
fn config_paths_default_under_home_when_home_is_set() {
    let paths = ConfigPaths::from_env(&BTreeMap::from([(
        "HOME".to_owned(),
        "/tmp/home".to_owned(),
    )]))
    .unwrap();

    assert_eq!(
        paths.connections,
        PathBuf::from("/tmp/home").join(".config/abap-rfc/connections.json")
    );
    assert_eq!(
        paths.local,
        PathBuf::from("/tmp/home").join(".config/abap-rfc/connections.local.json")
    );
}

/// `USERPROFILE` is the Windows home variable, and an empty `HOME` counts as absent like the other
/// key-map lookups.
#[test]
fn config_paths_use_userprofile_when_home_is_missing() {
    for env in [
        BTreeMap::from([("USERPROFILE".to_owned(), "/tmp/profile".to_owned())]),
        BTreeMap::from([
            ("HOME".to_owned(), String::new()),
            ("USERPROFILE".to_owned(), "/tmp/profile".to_owned()),
        ]),
    ] {
        let paths = ConfigPaths::from_env(&env).unwrap();

        assert_eq!(
            paths.connections,
            PathBuf::from("/tmp/profile").join(".config/abap-rfc/connections.json")
        );
        assert_eq!(
            paths.local,
            PathBuf::from("/tmp/profile").join(".config/abap-rfc/connections.local.json")
        );
    }
}

/// `ABAP_CONNECTIONS` keeps winning over a home-derived default, and `~` expands with the same
/// home rule the default uses.
#[test]
fn config_paths_prefer_abap_connections_over_the_home_default() {
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

    let expanded = ConfigPaths::from_env(&BTreeMap::from([
        ("USERPROFILE".to_owned(), "/tmp/profile".to_owned()),
        ("ABAP_CONNECTIONS".to_owned(), "~/custom.json".to_owned()),
    ]))
    .unwrap();

    assert_eq!(
        expanded.connections,
        PathBuf::from("/tmp/profile/custom.json")
    );

    let absolute = ConfigPaths::from_env(&BTreeMap::from([
        ("HOME".to_owned(), "/tmp/home".to_owned()),
        (
            "ABAP_CONNECTIONS".to_owned(),
            "/elsewhere/connections.json".to_owned(),
        ),
    ]))
    .unwrap();

    assert_eq!(
        absolute.connections,
        PathBuf::from("/elsewhere/connections.json")
    );
    assert_eq!(
        absolute.local,
        PathBuf::from("/elsewhere/connections.local.json")
    );
}

#[test]
fn config_paths_stop_when_no_home_variable_is_set() {
    let missing = ConfigPaths::from_env(&BTreeMap::new()).unwrap_err();

    assert!(missing.contains("HOME"), "{missing}");
    assert!(missing.contains("USERPROFILE"), "{missing}");
}

#[test]
fn an_absent_local_credentials_file_has_no_permission_to_check() {
    let directory = tempfile::tempdir().unwrap();

    assert_eq!(
        local_credentials_permission(&directory.path().join("connections.local.json")),
        LocalCredentialsPermission::NotPresent
    );
}

#[cfg(unix)]
#[test]
fn local_credentials_metadata_errors_are_reported_to_doctor() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("not-a-directory");
    fs::write(&file, "not credentials").unwrap();
    let inaccessible = file.join("connections.local.json");

    let permission = local_credentials_permission(&inaccessible);

    assert!(matches!(
        permission,
        LocalCredentialsPermission::MetadataError { .. }
    ));
    assert!(
        permission
            .label()
            .starts_with("WARNING: local credentials metadata could not be read:"),
        "{}",
        permission.label()
    );
}

#[test]
fn a_platform_without_a_permission_check_reports_it_and_points_at_the_documentation() {
    let label = LocalCredentialsPermission::NotChecked.label();

    assert_eq!(
        label,
        "local credentials: permission not checked; restrict the file to a user-only ACL \
         (see docs/getting-started.md)"
    );
}

/// The rule is portable, so every OS exercises it even where the mode bits cannot be read.
#[test]
fn the_permission_rule_flags_any_group_or_other_bit() {
    assert_eq!(
        classify_credentials_mode(None),
        LocalCredentialsPermission::NotChecked
    );

    for mode in [0o600, 0o400, 0o700] {
        assert_eq!(
            classify_credentials_mode(Some(mode)),
            LocalCredentialsPermission::UserOnly,
            "{mode:04o}"
        );
    }

    for mode in [0o640, 0o604, 0o644, 0o660] {
        assert_eq!(
            classify_credentials_mode(Some(mode)),
            LocalCredentialsPermission::GroupOrOtherReadable { mode },
            "{mode:04o}"
        );
    }
}

/// Group and other must not be able to read a file that holds credentials.
#[cfg(unix)]
#[test]
fn local_credentials_permission_rejects_group_or_other_access() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let local = directory.path().join("connections.local.json");

    fs::write(&local, r#"{"DEV_100_PASS":"secret-pw"}"#).unwrap();

    for mode in [0o640, 0o604, 0o644, 0o660] {
        fs::set_permissions(&local, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            local_credentials_permission(&local),
            LocalCredentialsPermission::GroupOrOtherReadable { mode },
            "{mode:04o}"
        );
    }
}

#[cfg(unix)]
#[test]
fn local_credentials_with_mode_0600_are_user_only() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let local = directory.path().join("connections.local.json");

    fs::write(&local, r#"{"DEV_100_PASS":"secret-pw"}"#).unwrap();

    for mode in [0o600, 0o400] {
        fs::set_permissions(&local, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            local_credentials_permission(&local),
            LocalCredentialsPermission::UserOnly,
            "{mode:04o}"
        );
    }
}

/// The warning names the modes but never a byte of the file content.
#[test]
fn the_group_or_other_warning_names_the_mode_without_the_secret() {
    let label = LocalCredentialsPermission::GroupOrOtherReadable { mode: 0o644 }.label();

    assert!(label.starts_with("WARNING: "), "{label}");
    assert!(label.contains("0644"), "{label}");
    assert!(label.contains("docs/getting-started.md"), "{label}");
    assert!(!label.contains("secret"), "{label}");
    assert_eq!(
        LocalCredentialsPermission::UserOnly.label(),
        "local credentials: user-only permission"
    );
    assert_eq!(
        LocalCredentialsPermission::NotPresent.label(),
        "local credentials: not present"
    );
}

/// A config of two servers (`DEV_100`, `QAS_100`) with an optional `policies` object at the top
/// level and one per server; the resolve of `dev_100` then shows the effective policy.
fn policies_of(top: &str, server: &str) -> Result<String, String> {
    let directory = tempfile::tempdir().unwrap();
    let connections = directory.path().join("connections.json");

    fs::write(
        &connections,
        format!(
            concat!(
                r#"{{"servers":[{{"systemId":"DEV","host":"example.com","port":443,"#,
                r#""clients":["100"],"rfc":{{"ashost":"example.com","sysnr":"00"}}{server}}}]{top}}}"#,
            ),
            server = server,
            top = top
        ),
    )
    .unwrap();
    let env = BTreeMap::from([
        ("DEV_100_USER".to_owned(), "user".to_owned()),
        ("DEV_100_PASS".to_owned(), "password".to_owned()),
    ]);
    let connection = Config::load(
        ConfigPaths {
            connections,
            local: directory.path().join("connections.local.json"),
        },
        &env,
    )?
    .resolve("dev_100", &env)?;

    Ok(connection.policies.foreign_inactive.token().to_owned())
}

#[test]
fn the_foreign_inactive_policy_defaults_to_reject_without_a_policies_block() {
    assert_eq!(policies_of("", "").unwrap(), "reject");
    assert_eq!(policies_of(r#","policies":{}"#, "").unwrap(), "reject");
}

#[test]
fn the_top_level_policy_applies_and_a_server_policy_overrides_it() {
    assert_eq!(
        policies_of(r#","policies":{"foreign_inactive":"allow"}"#, "").unwrap(),
        "allow"
    );
    assert_eq!(
        policies_of(
            r#","policies":{"foreign_inactive":"allow"}"#,
            r#","policies":{"foreign_inactive":"ask"}"#
        )
        .unwrap(),
        "ask"
    );
    assert_eq!(
        policies_of(
            r#","policies":{"foreign_inactive":"ask"}"#,
            r#","policies":{}"#
        )
        .unwrap(),
        "ask"
    );
}

#[test]
fn an_unknown_policy_name_or_value_stops_at_load() {
    let unknown_value = policies_of(r#","policies":{"foreign_inactive":"maybe"}"#, "").unwrap_err();
    let unknown_name = policies_of(r#","policies":{"other_guard":"reject"}"#, "").unwrap_err();
    let unknown_server_name =
        policies_of("", r#","policies":{"other_guard":"reject"}"#).unwrap_err();

    for message in [unknown_value, unknown_name, unknown_server_name] {
        assert!(message.starts_with("cannot parse"), "{message}");
    }

    assert!(
        policies_of(r#","policies":{"foreign_inactive":"maybe"}"#, "")
            .unwrap_err()
            .contains("unknown variant `maybe`")
    );
}

/// One config of two systems: `DEV` has a plain route in `rfc.saprouter` and two clients, `QAS`
/// has no route at all. Both systems have a credentials entry for client `100`.
fn saprouter_config(directory: &Path) -> (ConfigPaths, BTreeMap<String, String>) {
    let connections = directory.join("connections.json");
    let local = directory.join("connections.local.json");
    let config = r#"{"servers":[
        {"systemId":"DEV","host":"example.com","port":443,"clients":["100","200"],
         "rfc":{"ashost":"example.com","sysnr":"00","saprouter":"/H/config.example.com/S/3299"}},
        {"systemId":"QAS","clients":["100"],"rfc":{"ashost":"example.com","sysnr":"02"}}]}"#;
    fs::write(&connections, config).unwrap();
    let env = BTreeMap::from([
        ("DEV_100_USER".to_owned(), "dev-user".to_owned()),
        ("DEV_100_PASS".to_owned(), "dev-password".to_owned()),
        ("DEV_200_USER".to_owned(), "dev-user".to_owned()),
        ("DEV_200_PASS".to_owned(), "dev-password".to_owned()),
        ("QAS_100_USER".to_owned(), "qas-user".to_owned()),
        ("QAS_100_PASS".to_owned(), "qas-password".to_owned()),
    ]);

    (ConfigPaths { connections, local }, env)
}

fn resolve_route(
    paths: ConfigPaths,
    env: &BTreeMap<String, String>,
    connection_id: &str,
) -> (Option<String>, Option<SaprouterSource>) {
    let connection = Config::load(paths, env)
        .unwrap()
        .resolve(connection_id, env)
        .unwrap();

    (connection.rfc.saprouter, connection.saprouter_source)
}

#[test]
fn the_system_route_comes_from_env_then_local_then_config() {
    let directory = tempfile::tempdir().unwrap();
    let (paths, mut env) = saprouter_config(directory.path());

    fs::write(
        &paths.local,
        concat!(
            r#"{"DEV_SAPROUTER":"/H/local.example.com/S/3299/W/local-secret","#,
            r#""QAS_SAPROUTER":"/H/qas-local.example.com/S/3299"}"#,
        ),
    )
    .unwrap();
    env.insert(
        "DEV_SAPROUTER".to_owned(),
        "/H/env.example.com/S/3299/W/env-secret".to_owned(),
    );

    for client in ["dev_100", "dev_200"] {
        assert_eq!(
            resolve_route(paths.clone(), &env, client),
            (
                Some("/H/env.example.com/S/3299/W/env-secret".to_owned()),
                Some(SaprouterSource::Environment)
            ),
            "{client}"
        );
    }

    env.remove("DEV_SAPROUTER");
    assert_eq!(
        resolve_route(paths.clone(), &env, "dev_200"),
        (
            Some("/H/local.example.com/S/3299/W/local-secret".to_owned()),
            Some(SaprouterSource::LocalFile)
        )
    );
    assert_eq!(
        resolve_route(paths.clone(), &env, "qas_100"),
        (
            Some("/H/qas-local.example.com/S/3299".to_owned()),
            Some(SaprouterSource::LocalFile)
        )
    );

    fs::write(&paths.local, "{}").unwrap();
    assert_eq!(
        resolve_route(paths.clone(), &env, "dev_100"),
        (
            Some("/H/config.example.com/S/3299".to_owned()),
            Some(SaprouterSource::ConfigFile)
        )
    );
    assert_eq!(resolve_route(paths, &env, "qas_100"), (None, None));
}

#[test]
fn secrets_hide_the_password_the_route_and_only_its_host_and_password_fragments() {
    let mut connection = Connection::test_insecure();

    connection.password = "password-x".to_owned();

    assert_eq!(connection.secrets(), ["password-x"]);

    let route = "/H/router.example.com/S/3299/H/ab/W/secret";

    connection.rfc.saprouter = Some(route.to_owned());

    // Only the value after an `H` or `W` marker counts, and only from three characters on: the
    // port after `S` and the short value after `H` stay readable. Longest first.
    assert_eq!(
        connection.secrets(),
        [route, "router.example.com", "password-x", "secret"]
    );
}
