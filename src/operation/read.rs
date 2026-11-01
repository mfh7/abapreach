use crate::adt::prog_uri;
use crate::backend::Failure;
use crate::config::{Connection, tls_warning};
use crate::domain::{Outcome, Status};
use crate::hash::{normalize, source_hash};
use crate::transport::{Request, SadtTransport, TransportError};
use std::fs;
use std::path::Path;

/// A failed read with the routing class that decides whether a fallback is allowed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadError {
    pub failure: Failure,
    pub message: String,
}

impl ReadError {
    fn validation(message: String) -> Self {
        Self {
            failure: Failure::Validation,
            message,
        }
    }
}

pub struct ReadProg;

impl ReadProg {
    pub fn run(
        transport: &mut dyn SadtTransport,
        name: &str,
        output: &Path,
        force: bool,
        connection: Option<&Connection>,
    ) -> Result<Outcome, String> {
        Self::run_classified(transport, name, output, force, connection)
            .map_err(|error| error.message)
    }

    pub fn run_classified(
        transport: &mut dyn SadtTransport,
        name: &str,
        output: &Path,
        force: bool,
        connection: Option<&Connection>,
    ) -> Result<Outcome, ReadError> {
        if output.exists() && !force {
            return Err(ReadError::validation(format!(
                "{} already exists; use --force to overwrite",
                output.display()
            )));
        }

        let mut request = Request::get(format!("{}/source/main?version=active", prog_uri(name)));
        request
            .headers
            .insert("Accept".to_owned(), "text/plain".to_owned());
        let response = transport.request(request).map_err(|error| ReadError {
            failure: Failure::from(&error),
            message: error.to_string(),
        })?;

        // The logon was refused, not the read: report it the way the RFC backend does, never as a
        // stop and never with another attempt.
        if let Some(error) = response.authorization_failure() {
            return Err(ReadError {
                failure: Failure::from(&error),
                message: error.to_string(),
            });
        }

        if response.status != 200 {
            let error = TransportError::Http {
                status: response.status,
                message: String::new(),
            };

            return Err(ReadError {
                failure: Failure::from(&error),
                message: format!("read PROG {name}: HTTP {}", response.status),
            });
        }

        let source = String::from_utf8(response.body).map_err(|error| {
            ReadError::validation(format!("read response is not UTF-8: {error}"))
        })?;
        let lines = normalize(source.split('\n').map(str::to_owned).collect());
        let mut written = lines.join("\n");

        if !written.is_empty() {
            written.push('\n');
        }

        fs::write(output, written).map_err(|error| {
            ReadError::validation(format!("cannot write {}: {error}", output.display()))
        })?;

        let mut stdout = format!(
            "{} lines -> {}\nhash {}",
            lines.len(),
            output.display(),
            source_hash(&lines)
        );

        if let Some(warning) = connection.and_then(tls_warning) {
            stdout = format!("{warning}\n{stdout}");
        }

        Ok(Outcome {
            status: Some(Status::Active),
            exit: 0,
            stdout,
            stderr: String::new(),
        })
    }
}
