// SPDX-License-Identifier: AGPL-3.0-or-later
//! Transport-independent gs_usb protocol and device engine.
//! USB access requires explicit selection. No production runtime registration.
pub mod actor;
pub mod backend;
pub mod control;
pub mod device;
pub mod timing;
pub mod usb;
pub mod wire;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid gs_usb packet: {0}")]
    Protocol(&'static str),
    #[error("unsupported gs_usb capability: {0}")]
    Unsupported(&'static str),
    #[error("device actor is closed")]
    Closed,
    #[error("invalid device state")]
    State,
    #[error("transmit slots exhausted")]
    QueueFull,
    #[error("firmware echo deadline expired; submitted transmission outcome is unknown")]
    EchoTimeout,
    #[error("transport failed; submitted transmission outcome may be unknown")]
    Transport,
    #[error("USB {stage} failed: {detail}")]
    UsbAccess { stage: &'static str, detail: String },
    #[error(transparent)]
    Frame(#[from] canharbor_core::Error),
}
