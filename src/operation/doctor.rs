use crate::app::Factories;
use crate::config::{Connection, LocalCredentialsPermission, tls_warning};
use crate::transport::Request;

const DISCOVERY_URI: &str = "/sap/bc/adt/discovery";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    lines: Vec<String>,
    /// False as soon as one probe failed; only a report with every probe passing exits 0.
    passed: bool,
}

impl DoctorReport {
    pub fn render(&self) -> String {
        self.lines.join("\n")
    }

    pub fn exit(&self) -> i32 {
        if self.passed { 0 } else { 1 }
    }
}

pub struct Doctor;

/// One ADT discovery probe: the HTTP status and, when the transport knows it, whether the
/// server keeps the stateful session.
struct AdtProbe {
    status: u16,
    stateful: Option<bool>,
}

impl Doctor {
    /// Read-only probes on one connection: every probe runs even when an earlier one failed, and
    /// each backend gets at most one logon (ADT discovery GET, RFC ping). `local_credentials` is
    /// the permission assessment of the file the credentials came from; it is only reported.
    pub fn probe(
        connection: &Connection,
        local_credentials: LocalCredentialsPermission,
        factories: &mut dyn Factories,
    ) -> DoctorReport {
        let mut lines = vec![
            format!("connection {}: configuration available", connection.id),
            "credential available".to_owned(),
            local_credentials.label(),
            format!(
                "policy foreign inactive: {}",
                connection.policies.foreign_inactive
            ),
        ];

        match &connection.adt {
            Some(adt) => lines.push(format!(
                "ADT HTTP configured: {}://{}:{}",
                adt.scheme, adt.host, adt.port
            )),
            None => lines.push("ADT HTTP: not configured".to_owned()),
        }

        lines.push(format!(
            "RFC SADT configured: {} system {}",
            connection.rfc.ashost, connection.rfc.sysnr
        ));

        // The route may carry a `/W/<password>`, so only its source is ever printed.
        if let Some(source) = connection.saprouter_source {
            lines.push(format!("SAProuter: configured (from {})", source.label()));
        }

        let mut passed = true;
        let mut adt_passed = false;
        let mut rfc_passed = false;
        let mut stateful = None;

        match connection.adt {
            Some(_) => match adt_probe(connection, factories) {
                Ok(probe) => {
                    adt_passed = true;
                    lines.push(format!("ADT HTTP probe: ok (HTTP {})", probe.status));

                    if let Some(supported) = probe.stateful {
                        stateful = Some(supported);
                        lines.push(if supported {
                            "ADT HTTP stateful session: ok".to_owned()
                        } else {
                            "ADT HTTP stateful session: unavailable (no sap-contextid)".to_owned()
                        });
                    }
                }
                Err(error) => {
                    passed = false;
                    lines.push(format!("ADT HTTP probe: failed: {error}"));
                }
            },
            None => lines.push("ADT HTTP probe: not run (ADT not configured)".to_owned()),
        }

        // A ping needs the library, so a missing library skips (never fails) the ping probe.
        let library_found = match factories.rfc_library(connection) {
            Ok(info) => {
                lines.push(format!(
                    "RFC library: {} version {}",
                    info.path, info.version
                ));
                lines.extend(info.notes);

                true
            }
            Err(error) => {
                passed = false;
                lines.push(format!("RFC library: not found: {error}"));

                false
            }
        };

        if library_found {
            match factories.rfc_ping(connection) {
                Ok(()) => {
                    rfc_passed = true;
                    lines.push("RFC SADT probe: ok".to_owned());
                }
                Err(error) => {
                    passed = false;
                    lines.push(format!("RFC SADT probe: failed: {error}"));
                }
            }
        } else {
            lines.push("RFC SADT probe: not run (RFC library not found)".to_owned());
        }

        // A capability line names only the backends whose probe passed; nothing passed reads `none`.
        let adt = adt_passed.then_some("ADT HTTP");
        let rfc = rfc_passed.then_some("RFC SADT");
        let capabilities = capability(&[adt, rfc]);
        // Without a stateful session a LOCK cannot survive to the PUT, so only RFC can deploy.
        let deployed = capability(&[adt.filter(|_| stateful != Some(false)), rfc]);
        lines.extend([
            format!("capability read PROG: {capabilities}"),
            format!("capability deploy PROG: {deployed}"),
            format!("capability doctor: {capabilities}"),
        ]);

        if let Some(warning) = tls_warning(connection) {
            lines.push(warning);
        }

        DoctorReport { lines, passed }
    }
}

/// Renders the backends a capability line may list, in probe order.
fn capability(backends: &[Option<&str>]) -> String {
    let listed: Vec<&str> = backends.iter().flatten().copied().collect();

    if listed.is_empty() {
        "none".to_owned()
    } else {
        listed.join(", ")
    }
}

/// One discovery request on its own session; a read holds no lock, so a failed close changes
/// nothing here.
fn adt_probe(connection: &Connection, factories: &mut dyn Factories) -> Result<AdtProbe, String> {
    let mut transport = factories
        .adt(connection)
        .map_err(|error| error.to_string())?;
    let response = transport.request(Request::get(DISCOVERY_URI));
    let stateful = transport.stateful_supported();
    let _ = transport.close();
    let response = response.map_err(|error| error.to_string())?;

    if response.status != 200 {
        return Err(format!("HTTP {}", response.status));
    }

    Ok(AdtProbe {
        status: response.status,
        stateful,
    })
}
