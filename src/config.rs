use crate::policy::{McpWrite, Policies, PolicyDocument};
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

/// The read-only permission assessment of `connections.local.json`, which holds credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalCredentialsPermission {
    /// No local credentials file, so there is nothing to protect.
    NotPresent,
    /// Only the owning user holds a permission bit on the file.
    UserOnly,
    /// A group or other user holds a permission bit on a file that stores credentials.
    GroupOrOtherReadable { mode: u32 },
    /// The file exists but its metadata could not be inspected.
    MetadataError { error: String },
    /// The platform reports no portable permission bits (Windows ACLs), so nothing was checked.
    NotChecked,
}

impl LocalCredentialsPermission {
    /// The doctor line for this assessment; it names the file mode, never the file content.
    pub fn label(&self) -> String {
        match self {
            Self::NotPresent => "local credentials: not present".to_owned(),
            Self::UserOnly => "local credentials: user-only permission".to_owned(),
            Self::GroupOrOtherReadable { mode } => format!(
                "WARNING: local credentials are readable by group or other (mode {mode:04o}); \
                 restrict the file to the owner (see docs/getting-started.md)."
            ),
            Self::MetadataError { error } => {
                format!("WARNING: local credentials metadata could not be read: {error}")
            }
            Self::NotChecked => concat!(
                "local credentials: permission not checked; restrict the file to a user-only ",
                "ACL (see docs/getting-started.md)"
            )
            .to_owned(),
        }
    }
}

/// Assesses the local credentials file without opening it: only its metadata is read, so the
/// assessment never touches the secret content. A missing file stays optional.
pub fn local_credentials_permission(path: &Path) -> LocalCredentialsPermission {
    match fs::metadata(path) {
        Ok(metadata) => classify_credentials_mode(platform_mode(&metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => LocalCredentialsPermission::NotPresent,
        Err(error) => LocalCredentialsPermission::MetadataError {
            error: error.to_string(),
        },
    }
}

/// The portable rule: `None` means the platform exposes no mode bits, and any group or other bit
/// counts as exposure, because a secret file has no legitimate group or other access at all.
pub fn classify_credentials_mode(mode: Option<u32>) -> LocalCredentialsPermission {
    match mode {
        None => LocalCredentialsPermission::NotChecked,
        Some(mode) if mode & 0o077 == 0 => LocalCredentialsPermission::UserOnly,
        Some(mode) => LocalCredentialsPermission::GroupOrOtherReadable { mode },
    }
}

#[cfg(unix)]
fn platform_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    Some(metadata.permissions().mode() & 0o777)
}

/// Windows ACLs carry no mode bits, so the assessment reports that it did not check.
#[cfg(not(unix))]
fn platform_mode(_: &fs::Metadata) -> Option<u32> {
    None
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
    /// `None` for an ADT-only server: the config has no `rfc` block.
    pub rfc: Option<RfcConfig>,
    /// Where the route in `rfc.saprouter` came from; `None` when no route is configured.
    pub saprouter_source: Option<SaprouterSource>,
    /// The effective write-guard policies of this connection.
    pub policies: Policies,
}

impl Connection {
    /// Values that must never be printed: the password and the route, including its sensitive
    /// fragments when an RFC library reports only part of a route.
    pub fn secrets(&self) -> Vec<String> {
        let mut secrets = vec![self.password.clone()];

        if let Some(rfc) = &self.rfc
            && let Some(route) = &rfc.saprouter
        {
            secrets.push(route.clone());
            for pair in route.split('/').collect::<Vec<_>>().windows(2) {
                if matches!(pair[0], "H" | "W") && pair[1].len() >= 3 {
                    secrets.push(pair[1].to_owned());
                }
            }
        }

        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets
    }

    /// The server hosts. Only the doctor hides them: its probe errors may quote them, while a
    /// read or deploy prints source text that has no business being rewritten.
    pub fn hosts(&self) -> Vec<String> {
        let mut hosts = self
            .rfc
            .iter()
            .map(|rfc| rfc.ashost.clone())
            .collect::<Vec<_>>();

        hosts.extend(self.adt.iter().map(|adt| adt.host.clone()));
        hosts
    }

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
            rfc: Some(RfcConfig {
                ashost: "example.com".to_owned(),
                sysnr: "00".to_owned(),
                saprouter: None,
            }),
            saprouter_source: None,
            policies: Policies::default(),
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

/// Where the effective `SAPROUTER` value of a connection came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaprouterSource {
    /// `<SID>_SAPROUTER` in the process environment.
    Environment,
    /// `<SID>_SAPROUTER` in `connections.local.json`.
    LocalFile,
    /// `servers[].rfc.saprouter` in `connections.json`.
    ConfigFile,
}

impl SaprouterSource {
    /// How reports name the source; the route itself is never printed.
    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::LocalFile => "connections.local.json",
            Self::ConfigFile => "connections.json",
        }
    }
}

/// The per-system policies the MCP server loaded at start; nothing re-reads the file afterwards.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpPolicies {
    by_system: BTreeMap<String, Policies>,
}

impl McpPolicies {
    /// Whether any server allows MCP writes; only then does `tools/list` offer the write tools.
    pub fn write_offered(&self) -> bool {
        self.by_system
            .values()
            .any(|policies| policies.mcp_write != McpWrite::Off)
    }

    /// The policies of `system_id` (case-insensitive), `None` for an unknown system.
    pub fn get(&self, system_id: &str) -> Option<&Policies> {
        self.by_system.get(&system_id.to_ascii_uppercase())
    }
}

#[derive(Debug)]
pub struct Config {
    servers: Vec<Server>,
    local: BTreeMap<String, String>,
    /// The top-level `policies`; a `servers[].policies` block overrides single keys of it.
    policies: Policies,
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
            policies: policies_of(&connections.policies),
            servers: connections.servers,
            local,
        })
    }

    /// The policies of every server as the MCP server starts with them: one immutable snapshot,
    /// so a later edit of `connections.json` changes nothing until the next start.
    pub fn mcp_policies(&self) -> McpPolicies {
        McpPolicies {
            by_system: self
                .servers
                .iter()
                .map(|server| {
                    (
                        server.system_id.to_ascii_uppercase(),
                        self.effective_policies(server).for_mcp(),
                    )
                })
                .collect(),
        }
    }

    /// The ids (`SID_client`) of the connections whose server allows MCP writes, in
    /// configuration order; the write tool descriptions name them.
    pub fn mcp_writable(&self) -> Vec<String> {
        self.servers
            .iter()
            .filter(|server| self.effective_policies(server).mcp_write != McpWrite::Off)
            .flat_map(|server| {
                server
                    .clients
                    .iter()
                    .map(|client| format!("{}_{client}", server.system_id))
            })
            .collect()
    }

    fn effective_policies(&self, server: &Server) -> Policies {
        server.policies.as_ref().map_or_else(
            || self.policies.clone(),
            |document| document.apply(&self.policies),
        )
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
        let rfc = server.rfc.clone();
        if !server
            .clients
            .iter()
            .any(|configured| configured == &client)
        {
            return Err(format!("client {client} not configured for {system_id}"));
        }
        let (user, password) = Self::credentials(&system_id, &client, env, &self.local)?;
        let adt = Self::adt_endpoint(&system_id, server)?;
        let (rfc, saprouter_source) = match rfc {
            Some(rfc) => {
                let (rfc, source) = Self::saprouter(&system_id, rfc, env, &self.local);

                (Some(rfc), source)
            }
            None => (None, None),
        };
        if adt.is_none() && rfc.is_none() {
            return Err(format!(
                "{system_id} has neither an ADT endpoint (host/port) nor an rfc block in config"
            ));
        }
        let policies = self.effective_policies(server);

        Ok(Connection {
            id: format!("{system_id}_{client}"),
            system_id,
            client,
            user,
            password,
            lang: server.lang.clone().unwrap_or_else(|| "EN".to_owned()),
            adt,
            rfc,
            saprouter_source,
            policies,
        })
    }

    /// `servers[].rfc.saprouter` is per system, so `<SID>_SAPROUTER` replaces it for every client
    /// of that system: environment first, then `connections.local.json`, then the config file.
    /// The key exists because a route string may carry a `/W/<password>`.
    fn saprouter(
        system_id: &str,
        mut rfc: RfcConfig,
        env: &BTreeMap<String, String>,
        local: &BTreeMap<String, String>,
    ) -> (RfcConfig, Option<SaprouterSource>) {
        let key = format!("{system_id}_SAPROUTER");

        if let Some(route) = non_empty(env, &key) {
            rfc.saprouter = Some(route);

            return (rfc, Some(SaprouterSource::Environment));
        }
        if let Some(route) = non_empty(local, &key) {
            rfc.saprouter = Some(route);

            return (rfc, Some(SaprouterSource::LocalFile));
        }
        rfc.saprouter = rfc.saprouter.clone().filter(|route| !route.is_empty());
        let source = rfc.saprouter.as_ref().map(|_| SaprouterSource::ConfigFile);

        (rfc, source)
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
            .ok_or_else(|| format!("connection id must be <SID>_<client>: {connection_id}"))?;
        if system_id.is_empty() || client.is_empty() {
            return Err(format!(
                "connection id must be <SID>_<client>: {connection_id}"
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
    #[serde(default)]
    policies: Option<PolicyDocument>,
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
    #[serde(default)]
    policies: Option<PolicyDocument>,
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

/// A key-map value; an empty value counts as absent, like the credential keys.
fn non_empty(map: &BTreeMap<String, String>, key: &str) -> Option<String> {
    map.get(key).filter(|value| !value.is_empty()).cloned()
}

fn policies_of(document: &Option<PolicyDocument>) -> Policies {
    document
        .as_ref()
        .map_or_else(Policies::default, |document| {
            document.apply(&Policies::default())
        })
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

/// `HOME` on Unix and macOS, then Windows `USERPROFILE`; an empty value counts as absent.
fn home(env: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|key| non_empty(env, key).map(PathBuf::from))
        .ok_or_else(|| "HOME or USERPROFILE is required to resolve configuration path".to_owned())
}

fn read_json<T>(path: &Path) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

    serde_json::from_str(&text).map_err(|error| format!("cannot parse {}: {error}", path.display()))
}
