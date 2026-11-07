use crate::app::{Factories, RfcLibraryInfo, end_session};
use crate::config::{Connection, LocalCredentialsPermission, tls_warning};
use crate::extension::{Registry, doctor_line};
use crate::transport::Request;
use crate::transport::rfc::identity_report;

const DISCOVERY_URI: &str = "/sap/bc/adt/discovery";

/// What stands in for a host the doctor does not print; it is the mask `scrub` uses, so a host
/// that a probe error carries is masked the same way.
const HOST_MASK: &str = "***";

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

fn denied_label(tables: &[String]) -> String {
    if tables.is_empty() {
        "(none)".to_owned()
    } else {
        tables.join(", ")
    }
}

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
        Self::probe_with(connection, local_credentials, factories, &Registry::new())
    }

    /// [`Doctor::probe`] for a process with a registry. An extension adds its line after the
    /// `RFC SADT probe` line; it then gets the logon of the passed ping instead of a second one,
    /// and its line never decides the exit code.
    pub fn probe_with(
        connection: &Connection,
        local_credentials: LocalCredentialsPermission,
        factories: &mut dyn Factories,
        registry: &Registry,
    ) -> DoctorReport {
        let mut lines = vec![
            format!("connection {}: configuration available", connection.id),
            "credential available".to_owned(),
            local_credentials.label(),
            format!(
                "policy foreign inactive: {}",
                connection.policies.foreign_inactive
            ),
            format!(
                "policy read_table_denied: {}",
                denied_label(&connection.policies.read_table_denied)
            ),
            format!("policy mcp_write: {}", connection.policies.mcp_write),
        ];
        let missing = connection.policies.missing_default_denied();

        if !missing.is_empty() {
            lines.push(format!(
                "WARNING: policy read_table_denied lacks default entries: {}",
                missing.join(", ")
            ));
        }

        match &connection.adt {
            Some(adt) => lines.push(format!(
                "ADT HTTP configured: {}://{HOST_MASK}:{}",
                adt.scheme, adt.port
            )),
            None => lines.push("ADT HTTP: not configured".to_owned()),
        }

        // The application server host is as internal as a route host, so it is never printed.
        match &connection.rfc {
            Some(rfc) => lines.push(format!(
                "RFC SADT configured: {HOST_MASK} system {}",
                rfc.sysnr
            )),
            None => lines.push("RFC SADT: not configured".to_owned()),
        }

        // The route may carry a `/W/<password>`, so only its source is ever printed.
        if let Some(source) = connection.saprouter_source {
            lines.push(format!("SAProuter: configured (from {})", source.label()));
        }

        let mut passed = true;
        let mut adt_passed = false;
        let mut rfc_passed = false;
        let mut stateful = None;

        match connection.adt {
            Some(_) => {
                let (probe, close_note) = adt_probe(connection, factories);

                match probe {
                    Ok(probe) => {
                        adt_passed = true;
                        lines.push(format!("ADT HTTP probe: ok (HTTP {})", probe.status));

                        if let Some(supported) = probe.stateful {
                            stateful = Some(supported);
                            lines.push(if supported {
                                "ADT HTTP stateful session: ok".to_owned()
                            } else {
                                "ADT HTTP stateful session: unavailable (no sap-contextid)"
                                    .to_owned()
                            });
                        }
                    }
                    Err(error) => {
                        passed = false;
                        lines.push(format!("ADT HTTP probe: failed: {error}"));
                    }
                }

                lines.extend(close_note);
            }
            None => lines.push("ADT HTTP probe: not run (ADT not configured)".to_owned()),
        }

        // A ping needs the library, so a missing library skips (never fails) the ping probe. An
        // ADT-only connection has no RFC to probe at all. The library error text is kept: an
        // extension line shows it unchanged, instead of loading the library once more.
        let mut library_error = None;
        let mut function = None;
        let library_found = if connection.rfc.is_none() {
            lines.push("RFC library: not run (no RFC)".to_owned());

            false
        } else {
            match factories.rfc_library(connection) {
                Ok(info) => {
                    lines.push(format!("RFC library: {}", library_line(&info)));
                    lines.extend(info.notes);

                    true
                }
                Err(error) => {
                    passed = false;
                    lines.push(format!("RFC library: not found: {error}"));
                    library_error = Some(error);

                    false
                }
            }
        };

        if connection.rfc.is_none() {
            lines.push("RFC SADT probe: not run (no RFC)".to_owned());
        } else if library_found {
            let ping = if registry.extension().is_some() {
                let probe = factories.rfc_probe(connection);

                function = probe.function;

                probe.ping
            } else {
                factories.rfc_ping(connection)
            };

            match ping {
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

        if let Some(line) = doctor_line(
            registry,
            connection,
            library_error.as_deref(),
            rfc_passed,
            function,
        ) {
            lines.push(line);
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

/// The identity fields of the library the probe loaded, in one doctor line.
fn library_line(info: &RfcLibraryInfo) -> String {
    identity_report(
        &info.identity.path,
        &info.identity.version,
        &info.identity.architecture,
        &info.identity.family,
        &info.identity.source,
        &info.identity.chars,
    )
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

/// One discovery request on its own session. A read holds no lock, so a failed close is only the
/// `W:` line returned beside the result.
fn adt_probe(
    connection: &Connection,
    factories: &mut dyn Factories,
) -> (Result<AdtProbe, String>, Option<String>) {
    let mut transport = match factories.adt(connection) {
        Ok(transport) => transport,
        Err(error) => return (Err(error.to_string()), None),
    };
    let response = transport.request(Request::get(DISCOVERY_URI));
    let stateful = transport.stateful_supported();
    let note = end_session(transport.as_mut());
    let probe = response
        .map_err(|error| error.to_string())
        .and_then(|response| {
            if response.status == 200 {
                Ok(AdtProbe {
                    status: response.status,
                    stateful,
                })
            } else {
                Err(format!("HTTP {}", response.status))
            }
        });

    (probe, note)
}
