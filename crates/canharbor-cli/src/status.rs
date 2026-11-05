// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{error::Error, fmt};
#[derive(Debug)]
pub enum Failure {
    Input(String),
    Unknown(String),
    Reported { code: i32, message: String },
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(message) | Self::Unknown(message) | Self::Reported { message, .. } => {
                f.write_str(message)
            }
        }
    }
}
impl Error for Failure {}
pub fn input(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(Failure::Input(message.into()))
}
pub fn unknown(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(Failure::Unknown(message.into()))
}
pub fn submission_error(
    error: Box<dyn Error>,
    session: &str,
    request: &str,
    generation: &str,
) -> Box<dyn Error> {
    if let Some(api) = error.downcast_ref::<canharbor_client::ApiFailure>()
        && !api.submission_is_unknown()
    {
        return Box::new(Failure::Reported {
            code: exit_code(error.as_ref()),
            message: format!("{error}; session {session}"),
        });
    }
    let description = error
        .downcast_ref::<canharbor_client::ApiFailure>()
        .and_then(|api| api.detail.as_ref())
        .map(|detail| {
            format!(
                "{}{}",
                detail.message,
                detail
                    .receipt
                    .as_ref()
                    .map(|id| format!("; receipt {id}"))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_else(|| error.to_string());
    unknown(format!(
        "transmission outcome unknown; session {session}, request {request}: {description}; inspect with canharbor receipt --session {session} --request {request} --generation {generation}; do not resend automatically"
    ))
}
pub fn exit_code(error: &(dyn Error + 'static)) -> i32 {
    if let Some(error) = error.downcast_ref::<Failure>() {
        return match error {
            Failure::Input(_) => 2,
            Failure::Unknown(_) => 7,
            Failure::Reported { code, .. } => *code,
        };
    }
    if error.is::<canharbor_client::TaskStartUnknown>() {
        return 7;
    }
    if error.is::<canharbor_client::AmbiguousTask>() {
        return 5;
    }
    if error.is::<canharbor_client::ServiceUnavailable>()
        || error.is::<canharbor_client::StreamUnavailable>()
    {
        return 3;
    }
    if error.is::<canharbor_client::ResourceLimit>() {
        return 8;
    }
    if error.is::<canharbor_client::InvalidPath>()
        || error.is::<canharbor_client::InvalidArgument>()
    {
        return 2;
    }
    if let Some(error) = error.downcast_ref::<canharbor_client::ApiFailure>() {
        if error.outcome_unknown == Some(true)
            || error
                .detail
                .as_ref()
                .is_some_and(|detail| detail.outcome_unknown)
        {
            return 7;
        }
        if error.detail.as_ref().is_some_and(|detail| {
            matches!(
                detail.code.as_str(),
                "disk_full" | "resource_limit" | "queue_full" | "tx_rate_limited"
            )
        }) {
            return 8;
        }
        if error.detail.as_ref().is_some_and(|detail| {
            detail.code == "device_disconnected" || detail.code == "channel_not_found"
        }) {
            return 4;
        }
        return http_status(error.status);
    }
    if let Some(error) = error.downcast_ref::<tokio_tungstenite::tungstenite::Error>() {
        use tokio_tungstenite::tungstenite::{Error as WsError, error::ProtocolError};
        return match error {
            WsError::Http(response) => http_status(response.status().as_u16()),
            WsError::Io(_)
            | WsError::ConnectionClosed
            | WsError::AlreadyClosed
            | WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake) => 3,
            _ => 6,
        };
    }
    if let Some(error) = error.downcast_ref::<canharbor_core::Error>() {
        return match error {
            canharbor_core::Error::UnknownChannel => 4,
            canharbor_core::Error::InvalidState | canharbor_core::Error::ControlExpired => 5,
            canharbor_core::Error::QueueFull => 8,
            canharbor_core::Error::Unsupported | canharbor_core::Error::InvalidFrame => 2,
            canharbor_core::Error::Fault(canharbor_core::BackendFault::Disconnected) => 4,
            _ => 6,
        };
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return match error.kind() {
            std::io::ErrorKind::AddrInUse => 5,
            std::io::ErrorKind::StorageFull
            | std::io::ErrorKind::QuotaExceeded
            | std::io::ErrorKind::OutOfMemory
            | std::io::ErrorKind::AlreadyExists => 8,
            _ => 6,
        };
    }
    6
}

fn http_status(status: u16) -> i32 {
    match status {
        400 | 401 | 403 | 422 | 428 => 2,
        404 => 4,
        409 | 412 => 5,
        429 | 507 => 8,
        _ => 6,
    }
}
