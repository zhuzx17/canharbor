// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validated CAN values and the backend boundary. No HTTP or USB dependencies.
pub mod measurement;
mod suspend;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("CAN identifier exceeds the selected identifier width")]
    InvalidId,
    #[error("invalid payload length or flags for this frame type")]
    InvalidFrame,
    #[error("control permit expired or revoked")]
    ControlExpired,
    #[error("backend command or transmit queue full")]
    QueueFull,
    #[error("backend operation invalid in current state")]
    InvalidState,
    #[error("backend operation unsupported")]
    Unsupported,
    #[error("unknown channel")]
    UnknownChannel,
    #[error("backend fault: {0:?}")]
    Fault(BackendFault),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanId {
    value: u32,
    extended: bool,
}
impl CanId {
    pub fn new(value: u32, extended: bool) -> Result<Self, Error> {
        if value > if extended { 0x1fff_ffff } else { 0x7ff } {
            return Err(Error::InvalidId);
        }
        Ok(Self { value, extended })
    }
    pub fn value(self) -> u32 {
        self.value
    }
    pub fn extended(self) -> bool {
        self.extended
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    ClassicData { id: CanId, data: Vec<u8> },
    ClassicRemote { id: CanId, dlc: u8 },
    FdData { id: CanId, data: Vec<u8>, brs: bool },
}
impl Frame {
    pub fn classic(id: CanId, data: Vec<u8>) -> Result<Self, Error> {
        if data.len() > 8 {
            return Err(Error::InvalidFrame);
        }
        Ok(Self::ClassicData { id, data })
    }
    pub fn remote(id: CanId, dlc: u8) -> Result<Self, Error> {
        if dlc > 8 {
            return Err(Error::InvalidFrame);
        }
        Ok(Self::ClassicRemote { id, dlc })
    }
    pub fn fd(id: CanId, data: Vec<u8>, brs: bool) -> Result<Self, Error> {
        if !matches!(data.len(), 0..=8 | 12 | 16 | 20 | 24 | 32 | 48 | 64) {
            return Err(Error::InvalidFrame);
        }
        Ok(Self::FdData { id, data, brs })
    }
    /// Revalidate values constructed through public enum variants at the backend boundary.
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::ClassicData { id, data } => Self::classic(*id, data.clone()),
            Self::ClassicRemote { id, dlc } => Self::remote(*id, *dlc),
            Self::FdData { id, data, brs } => Self::fd(*id, data.clone(), *brs),
        }
        .map(|_| ())
    }
    pub fn id(&self) -> CanId {
        match self {
            Self::ClassicData { id, .. }
            | Self::ClassicRemote { id, .. }
            | Self::FdData { id, .. } => *id,
        }
    }
    pub fn data(&self) -> &[u8] {
        match self {
            Self::ClassicData { data, .. } | Self::FdData { data, .. } => data,
            Self::ClassicRemote { .. } => &[],
        }
    }
    pub fn dlc(&self) -> u8 {
        match self {
            Self::ClassicRemote { dlc, .. } => *dlc,
            _ => match self.data().len() {
                n @ 0..=8 => n as u8,
                12 => 9,
                16 => 10,
                20 => 11,
                24 => 12,
                32 => 13,
                48 => 14,
                64 => 15,
                _ => unreachable!("validated frame"),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: String,
    pub label: String,
}
#[derive(Debug, Clone, Default)]
pub struct FrameMetadata {
    pub stamp: timestamp::Stamp,
    pub esi: bool,
    pub raw_dlc: Option<u8>,
}
#[derive(Debug, Clone)]
pub struct BusFrame {
    /// Monotonic host receipt time, captured before publishing to subscribers.
    pub received_at: tokio::time::Instant,
    pub frame: Frame,
    pub source_channel: String,
    pub generation: u64,
    pub metadata: FrameMetadata,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendFault {
    Disconnected,
    BusOff,
    ReceiveOverflow,
    TransmitFailed,
    StopFailed,
}
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Frame(BusFrame),
    Diagnostic(Diagnostic),
    Fault {
        fault: BackendFault,
        generation: u64,
    },
}
pub mod control;
pub type BackendFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, Error>> + Send + 'a>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub nominal_bitrate: u32,
    pub data_bitrate: Option<u32>,
    pub listen_only: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            nominal_bitrate: 500_000,
            data_bitrate: Some(2_000_000),
            listen_only: true,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub received_at: tokio::time::Instant,
    pub generation: u64,
    pub classes: u32,
    pub data: [u8; 8],
    pub overflow: bool,
    pub stamp: timestamp::Stamp,
}
/// Backends publish through bounded receivers; slow subscribers never block senders.
#[derive(Debug, Clone)]
pub struct TimingLimits {
    pub clock_hz: u32,
    pub tseg1_min: u32,
    pub tseg1_max: u32,
    pub tseg2_min: u32,
    pub tseg2_max: u32,
    pub sjw_max: u32,
    pub brp_min: u32,
    pub brp_max: u32,
    pub brp_inc: u32,
}
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub device_id: String,
    pub backend: String,
    pub simulated: bool,
    pub fd: bool,
    pub listen_only: bool,
    pub timestamps: bool,
    pub nominal_timing: Option<TimingLimits>,
    pub data_timing: Option<TimingLimits>,
    pub fixed_nominal_bitrates: Vec<u32>,
    pub fixed_data_bitrates: Vec<u32>,
}
#[derive(Debug, Clone, Default)]
pub struct CloseSummary {
    pub unknown_transmissions: u64,
    pub failures: Vec<String>,
}
pub trait Backend: Send + Sync {
    fn source_evidence(&self, _channel: &str) -> Vec<measurement::SourceEvidence> {
        vec![]
    }
    fn submit_observed<'a>(
        &'a self,
        channel: &'a str,
        frame: Frame,
        permit: control::Permit,
        trace: measurement::SubmissionTrace,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            trace.call_started("backend_boundary; worker_queue_unknown");
            let result = self.submit(channel, frame, permit).await;
            trace.call_completed();
            result
        })
    }

    /// Read-only cumulative cleanup accounting; None means unsupported, never zero.
    fn cleanup_observation(&self, _channel: &str) -> Option<CloseSummary> {
        None
    }
    fn shutdown_report(&self) -> BackendFuture<'_, CloseSummary> {
        Box::pin(async {
            self.shutdown().await?;
            Ok(CloseSummary::default())
        })
    }
    fn capabilities(&self, _channel: &str) -> Result<Capabilities, Error> {
        Err(Error::Unsupported)
    }
    fn shutdown(&self) -> BackendFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn channels(&self) -> Vec<Channel>;
    fn subscribe(&self, channel: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error>;
    /// Return the current backend generation when healthy. Fault notifications
    /// and frames from earlier generations cannot invalidate an explicit restart.
    fn check_channel(&self, channel: &str) -> Result<u64, Error>;
    /// Low-level injection boundary, used by virtual test harnesses. Services
    /// must use submit, which carries execution-time authorization.
    fn transmit(&self, channel: &str, frame: Frame) -> Result<(), Error>;
    fn configure<'a>(&'a self, channel: &'a str, config: Config) -> BackendFuture<'a, ()>;
    fn start<'a>(&'a self, channel: &'a str, permit: control::Permit) -> BackendFuture<'a, ()>;
    fn stop<'a>(&'a self, channel: &'a str) -> BackendFuture<'a, ()>;
    fn close<'a>(&'a self, channel: &'a str) -> BackendFuture<'a, ()>;
    fn submit<'a>(
        &'a self,
        channel: &'a str,
        frame: Frame,
        permit: control::Permit,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            permit.check()?;
            self.transmit(channel, frame)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_and_wire_lengths_are_checked() {
        assert!(CanId::new(0x800, false).is_err());
        assert!(CanId::new(0x2000_0000, true).is_err());
        let id = CanId::new(0x123, false).unwrap();
        for n in 0..=65 {
            assert_eq!(
                Frame::fd(id, vec![0; n], true).is_ok(),
                matches!(n, 0..=8 | 12 | 16 | 20 | 24 | 32 | 48 | 64)
            );
        }
        assert!(Frame::classic(id, vec![0; 9]).is_err());
        assert!(Frame::remote(id, 9).is_err());
        assert_eq!(Frame::remote(id, 8).unwrap().data(), &[]);
    }
}

pub mod runtime;

pub mod registry;

pub mod timestamp;
