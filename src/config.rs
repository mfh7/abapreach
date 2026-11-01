use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigPaths {
    pub connections: PathBuf,
    pub local: PathBuf,
}

impl ConfigPaths {
    pub fn from_env(env: &BTreeMap<String, String>) -> Result<Self, String> {
        let configured = env
            .get("ABAP_CONNECTIONS")
            .filter(|value| !value.is_empty())
            .map(String::as_str)
            .unwrap_or("~/.config/abap-rfc/connections.json");
        let connections = expand_home(configured, env)?;
        let parent = connections.parent().ok_or_else(|| {
            format!(
                "configuration path has no parent directory: {}",
                connections.display()
            )
        })?;

        Ok(Self {
            local: parent.join("connections.local.json"),
            connections,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub id: String,
    pub system_id: String,
    pub client: String,
    pub user: String,
    pub password: String,
    pub lang: String,
    /// `None` for an RFC-only server: the config has no `host`/`port` ADT endpoint.
    pub adt: Option<AdtConfig>,
    pub rfc: RfcConfig,
}

impl Connection {
    #[cfg(feature = "test-support")]
    pub fn test_insecure() -> Self {
        Self {
            id: "DEV_100".to_owned(),
            system_id: "DEV".to_owned(),
            client: "100".to_owned(),
            user: "user".to_owned(),
            password: "password".to_owned(),
            lang: "EN".to_owned(),
            adt: Some(AdtConfig {
                host: "example.com".to_owned(),
                port: 443,
                scheme: "https".to_owned(),
                ca_file: None,
                insecure: true,
            }),
            rfc: RfcConfig {
                ashost: "example.com".to_owned(),
                sysnr: "00".to_owned(),
                saprouter: None,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdtConfig {
    pub host: String,
    pub port: u16,
    pub scheme: String,
    pub ca_file: Option<PathBuf>,
    pub insecure: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RfcConfig {
    pub ashost: String,
    pub sysnr: String,
    #[serde(default)]
    pub saprouter: Option<String>,
}

#[derive(Debug)]
pub struct Config {
    servers: Vec<Server>,
    local: BTreeMap<String, String>,
}

impl Config {
    pub fn load(paths: ConfigPaths, _env: &BTreeMap<String, String>) -> Result<Self, String> {
        let connections = read_json::<ConfigFile>(&paths.connections)?;
        for (index, server) in connections.servers.iter().enumerate() {
            if server.system_id.is_empty() {
                return Err(format!(
                    "{}: servers[{index}].systemId",
                    paths.connections.display()
                ));
            }
        }
        let local = match fs::read_to_string(&paths.local) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| format!("cannot parse {}: {error}", paths.local.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                return Err(format!("cannot read {}: {error}", paths.local.display()));
            }
        };

        Ok(Self {
            servers: connections.servers,
            local,
        })
    }

    pub fn resolve(
        &self,
        connection_id: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<Connection, String> {
        let (system_id, client) = Self::parse_id(connection_id)?;
        let server = self
            .servers
            .iter()
            .find(|server| server.system_id == system_id)
            .ok_or_else(|| format!("unknown system {system_id}"))?;
        let rfc = server
            .rfc
            .clone()
            .ok_or_else(|| format!("{system_id} has no rfc/clients block in config"))?;
        if !server
            .clients
            .iter()
            .any(|configured| configured == &client)
        {
            return Err(format!("client {client} not configured for {system_id}"));
        }
        let (user, password) = Self::credentials(&system_id, &client, env, &self.local)?;
        let adt = Self::adt_endpoint(&system_id, server)?;

        Ok(Connection {
            id: format!("{system_id}_{client}"),
            system_id,
            client,
            user,
            password,
            lang: server.lang.clone().unwrap_or_else(|| "EN".to_owned()),
            adt,
            rfc,
        })
    }

    /// `host` and `port` form the ADT endpoint together; a server with neither is RFC-only,
    /// and a server with only one of them is a configuration error.
    fn adt_endpoint(system_id: &str, server: &Server) -> Result<Option<AdtConfig>, String> {
        let (host, port) = match (&server.host, server.port) {
            (None, None) => return Ok(None),
            (Some(host), Some(port)) => (host, port),
            (Some(_), None) => {
                return Err(format!("{system_id} has host but no port in config"));
            }
            (None, Some(_)) => {
                return Err(format!("{system_id} has port but no host in config"));
            }
        };
        let scheme = server.adt.scheme.to_ascii_lowercase();

        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(format!("unsupported ADT scheme: {scheme}"));
        }

        Ok(Some(AdtConfig {
            host: host.clone(),
            port,
            scheme,
            ca_file: server.adt.ca_file.clone(),
            insecure: server.adt.insecure,
        }))
    }

    pub fn parse_id(connection_id: &str) -> Result<(String, String), String> {
        let (system_id, client) = connection_id
            .rsplit_once('_')
            .ok_or_else(|| format!("connection id must be <sid>_<client>: {connection_id}"))?;
        if system_id.is_empty() || client.is_empty() {
            return Err(format!(
                "connection id must be <sid>_<client>: {connection_id}"
            ));
        }

        Ok((system_id.to_ascii_uppercase(), client.to_owned()))
    }

    pub fn credentials(
        system_id: &str,
        client: &str,
        env: &BTreeMap<String, String>,
        local: &BTreeMap<String, String>,
    ) -> Result<(String, String), String> {
        let system_id = system_id.to_ascii_uppercase();
        let user = credential(&system_id, client, "USER", env, local)?;
        let password = credential(&system_id, client, "PASS", env, local)?;

        Ok((user, password))
    }
}

pub fn scrub(text: impl Into<String>, secrets: &[String]) -> String {
    let mut text = text.into();
    for secret in secrets {
        if !secret.is_empty() {
            text = text.replace(secret, "***");
        }
    }

    text
}

pub fn tls_warning(connection: &Connection) -> Option<String> {
    let adt = connection.adt.as_ref()?;

    if adt.insecure || adt.scheme == "http" {
        return Some(format!(
            "WARNING: connection {} uses insecure TLS or HTTP.",
            connection.id
        ));
    }

    None
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    servers: Vec<Server>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Server {
    system_id: String,
    /// Optional: without it (and `port`) the server is reachable through RFC only.
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    clients: Vec<String>,
    #[serde(default)]
    rfc: Option<RfcConfig>,
    #[serde(default)]
    adt: AdtSettings,
    #[serde(default)]
    lang: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdtSettings {
    #[serde(default = "default_scheme")]
    scheme: String,
    #[serde(default)]
    ca_file: Option<PathBuf>,
    #[serde(default)]
    insecure: bool,
}

impl Default for AdtSettings {
    fn default() -> Self {
        Self {
            scheme: default_scheme(),
            ca_file: None,
            insecure: false,
        }
    }
}

fn credential(
    system_id: &str,
    client: &str,
    kind: &str,
    env: &BTreeMap<String, String>,
    local: &BTreeMap<String, String>,
) -> Result<String, String> {
    let key = format!("{system_id}_{client}_{kind}");
    env.get(&key)
        .filter(|value| !value.is_empty())
        .or_else(|| local.get(&key).filter(|value| !value.is_empty()))
        .cloned()
        .ok_or_else(|| format!("missing {key}"))
}

fn default_scheme() -> String {
    "https".to_owned()
}

fn expand_home(path: &str, env: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    if path == "~" {
        return home(env);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(home(env)?.join(rest));
    }

    Ok(PathBuf::from(path))
}

fn home(env: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    env.get("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required to resolve configuration path".to_owned())
}

fn read_json<T>(path: &Path) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

    serde_json::from_str(&text).map_err(|error| format!("cannot parse {}: {error}", path.display()))
}
