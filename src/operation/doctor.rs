use crate::config::{Connection, tls_warning};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    lines: Vec<String>,
}

impl DoctorReport {
    pub fn render(&self) -> String {
        self.lines.join("\n")
    }
}

pub struct Doctor;

impl Doctor {
    pub fn offline_report(connection: &Connection) -> DoctorReport {
        let mut lines = vec![
            format!("connection {}: configuration available", connection.id),
            "credential available".to_owned(),
            format!(
                "ADT HTTP configured: {}://{}:{}",
                connection.adt.scheme, connection.adt.host, connection.adt.port
            ),
            format!(
                "RFC SADT configured: {} system {}",
                connection.rfc.ashost, connection.rfc.sysnr
            ),
            "ADT HTTP probe: not run".to_owned(),
            "RFC SADT probe: not run".to_owned(),
            "capability read PROG: ADT HTTP, RFC SADT".to_owned(),
            "capability deploy PROG: ADT HTTP, RFC SADT".to_owned(),
            "capability doctor: ADT HTTP, RFC SADT".to_owned(),
        ];

        if let Some(warning) = tls_warning(connection) {
            lines.push(warning);
        }

        DoctorReport { lines }
    }
}
