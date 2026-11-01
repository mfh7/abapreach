use crate::transport::TransportError;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    AdtHttp,
    RfcSadt,
}

impl fmt::Display for BackendKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdtHttp => write!(formatter, "ADT HTTP"),
            Self::RfcSadt => write!(formatter, "RFC SADT"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationKind {
    ReadProg,
    DeployProg,
    Doctor,
}

impl fmt::Display for OperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadProg => write!(formatter, "read PROG"),
            Self::DeployProg => write!(formatter, "deploy PROG"),
            Self::Doctor => write!(formatter, "doctor"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Initial,
    PreSendConnectivity,
    CapabilityUnavailable,
    Authorization,
    Conflict,
    Validation,
    PostSendUncertain,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initial => write!(formatter, "initial selection"),
            Self::PreSendConnectivity => write!(formatter, "pre-send connectivity"),
            Self::CapabilityUnavailable => write!(formatter, "capability unavailable"),
            Self::Authorization => write!(formatter, "authorization"),
            Self::Conflict => write!(formatter, "conflict"),
            Self::Validation => write!(formatter, "validation"),
            Self::PostSendUncertain => write!(formatter, "post-send uncertainty"),
        }
    }
}

impl From<&TransportError> for Failure {
    fn from(error: &TransportError) -> Self {
        match error {
            TransportError::PreSendConnectivity(_) => Self::PreSendConnectivity,
            TransportError::PostSendUncertain(_) => Self::PostSendUncertain,
            TransportError::Authorization(_) | TransportError::CsrfRequired => Self::Authorization,
            TransportError::Http { status, .. } => match status {
                401 | 403 => Self::Authorization,
                409 | 423 => Self::Conflict,
                _ => Self::Validation,
            },
            TransportError::Protocol(_) | TransportError::RequestNotAllowed(_) => Self::Validation,
        }
    }
}

/// Chooses a backend. ADT is always tried first, so a fallback means RFC; the caller never
/// asks again after the RFC backend itself failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Router {
    adt_http: bool,
    rfc_sadt: bool,
}

impl Router {
    pub fn new(adt_http: bool, rfc_sadt: bool) -> Self {
        Self { adt_http, rfc_sadt }
    }

    pub fn both() -> Self {
        Self::new(true, true)
    }

    /// `base_given` is true for a deploy with `--base`; like `--backend`, it pins the backend.
    pub fn select(
        &self,
        operation: OperationKind,
        explicit: Option<BackendKind>,
        base_given: bool,
        failure: Failure,
    ) -> Result<BackendKind, String> {
        if failure == Failure::Initial {
            return match explicit {
                Some(backend) => self.require(operation, backend),
                None if self.adt_http => Ok(BackendKind::AdtHttp),
                None => self.require(operation, BackendKind::RfcSadt),
            };
        }

        if explicit.is_some() || base_given {
            let pin = if explicit.is_some() {
                "--backend"
            } else {
                "--base"
            };

            return Err(format!(
                "{operation} cannot fall back after {failure}: {pin} pins the backend"
            ));
        }

        match failure {
            Failure::PreSendConnectivity | Failure::CapabilityUnavailable => {
                self.require(operation, BackendKind::RfcSadt)
            }
            _ => Err(format!("{operation} cannot fall back after {failure}")),
        }
    }

    fn require(
        &self,
        operation: OperationKind,
        backend: BackendKind,
    ) -> Result<BackendKind, String> {
        let available = match backend {
            BackendKind::AdtHttp => self.adt_http,
            BackendKind::RfcSadt => self.rfc_sadt,
        };

        if available {
            Ok(backend)
        } else {
            Err(format!("{backend} is not available for {operation}"))
        }
    }
}
