// SPDX-License-Identifier: AGPL-3.0-or-later
//! Serialized, bounded command engine. Blocking calls belong on an owned worker.
use crate::{Decoder, Profile, Response, encode_with_profile, serial::Transport};
use canharbor_core::{Config, Frame};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
const WAIT: Duration = Duration::from_millis(250);
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Protocol(#[from] crate::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Control(#[from] canharbor_core::Error),
    #[error("adapter reply timed out")]
    Timeout,
    #[error("adapter stream or queue overflow")]
    Overflow,
    #[error("adapter state requires an explicit reopen")]
    Poisoned,
    #[error("unexpected adapter reply")]
    Unexpected,
}
#[derive(Debug)]
pub struct Received {
    pub frame: Frame,
    pub timestamp_ms: Option<u16>,
    pub received_at: tokio::time::Instant,
}
#[derive(Debug, PartialEq, Eq)]
pub enum FirmwareVersion {
    Lawicel { hardware: u8, software: u8 },
    Canable(String),
}
pub struct Engine<T> {
    profile: Profile,
    transport: T,
    decoder: Decoder,
    replies: VecDeque<Result<Response, crate::Error>>,
    frames: VecDeque<Received>,
    statuses: VecDeque<u8>,
    poisoned: bool,
    initialized: bool,
    started: Option<Config>,
    unknown: u64,
}
impl<T: Transport> Engine<T> {
    pub fn new(transport: T) -> Self {
        Self::with_profile(transport, Profile::Lawicel)
    }
    pub fn with_profile(transport: T, profile: Profile) -> Self {
        Self {
            profile,
            transport,
            decoder: Decoder::with_profile(profile),
            replies: VecDeque::new(),
            frames: VecDeque::new(),
            statuses: VecDeque::new(),
            poisoned: false,
            initialized: false,
            started: None,
            unknown: 0,
        }
    }
    pub fn unknown_transmissions(&self) -> u64 {
        self.unknown
    }
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }
    fn ready(&mut self) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if !self.replies.is_empty() {
            self.poisoned = true;
            return Err(Error::Unexpected);
        }
        Ok(())
    }
    fn write(
        &mut self,
        command: &[u8],
        guard: &impl Fn() -> Result<(), canharbor_core::Error>,
    ) -> Result<(), Error> {
        self.ready()?;
        guard()?;
        match self.transport.write(command) {
            Ok(n) if n == command.len() => Ok(()),
            Ok(_) => {
                self.poisoned = true;
                Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into())
            }
            Err(error) => {
                self.poisoned = true;
                Err(error.into())
            }
        }
    }
    fn receive(&mut self, timeout: Duration) -> Result<(), Error> {
        let mut bytes = [0u8; 256];
        let count = match self.transport.read(&mut bytes, timeout) {
            Ok(n) if n <= bytes.len() => n,
            Ok(_) => {
                self.poisoned = true;
                return Err(Error::Overflow);
            }
            Err(error) => {
                self.poisoned = true;
                return Err(error.into());
            }
        };
        for &byte in &bytes[..count] {
            if let Some(reply) = self.decoder.push(byte) {
                match reply {
                    Ok(Response::Frame {
                        frame,
                        timestamp_ms,
                    }) => {
                        if self.frames.len() == 256 {
                            self.poisoned = true;
                            return Err(Error::Overflow);
                        }
                        self.frames.push_back(Received {
                            frame,
                            timestamp_ms,
                            received_at: tokio::time::Instant::now(),
                        });
                    }
                    reply => {
                        if self.replies.len() == 64 {
                            self.poisoned = true;
                            return Err(Error::Overflow);
                        }
                        self.replies.push_back(reply);
                    }
                }
            }
        }
        Ok(())
    }
    fn reply(&mut self, deadline: Instant) -> Result<Response, Error> {
        loop {
            if let Some(reply) = self.replies.pop_front() {
                return match reply {
                    Ok(reply) => Ok(reply),
                    Err(error) => {
                        if error != crate::Error::Rejected {
                            self.poisoned = true;
                        }
                        Err(error.into())
                    }
                };
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.poisoned = true;
                return Err(Error::Timeout);
            }
            self.receive(remaining.min(Duration::from_millis(20)))?;
        }
    }
    fn ack(
        &mut self,
        command: &[u8],
        guard: &impl Fn() -> Result<(), canharbor_core::Error>,
    ) -> Result<(), Error> {
        self.write(command, guard)?;
        // CANable commands have no ACK/NACK. A complete host write is the only
        // available confirmation, including configuration and close requests.
        if self.profile == Profile::CanableFd {
            return Ok(());
        }
        match self.reply(Instant::now() + WAIT)? {
            Response::Ack => Ok(()),
            _ => {
                self.poisoned = true;
                Err(Error::Unexpected)
            }
        }
    }
    /// Version handshake sends no leading CR: terminating an unknown partial TX
    /// record could itself cause transmission. No EEPROM-changing commands are used.
    pub fn initialize(&mut self) -> Result<FirmwareVersion, Error> {
        self.decoder.expect_version();
        self.write(b"V\r", &|| Ok(()))?;
        let deadline = Instant::now() + WAIT;
        for _ in 0..64 {
            match self.reply(deadline)? {
                Response::Version { hardware, software } => {
                    self.initialized = true;
                    return Ok(match self.profile {
                        Profile::Lawicel => FirmwareVersion::Lawicel { hardware, software },
                        Profile::CanableFd => {
                            FirmwareVersion::Canable(format!("V{hardware:02X}{software:02X}"))
                        }
                    });
                }
                Response::FirmwareVersion(version) if self.profile == Profile::CanableFd => {
                    self.initialized = true;
                    return Ok(FirmwareVersion::Canable(version));
                }
                Response::Ack => continue,
                _ => {
                    self.poisoned = true;
                    return Err(Error::Unexpected);
                }
            }
        }
        self.poisoned = true;
        Err(Error::Unexpected)
    }
    pub fn start(
        &mut self,
        config: Config,
        guard: impl Fn() -> Result<(), canharbor_core::Error>,
    ) -> Result<(), Error> {
        if !self.initialized {
            return Err(canharbor_core::Error::InvalidState.into());
        }
        self.profile.validate_config(config)?;
        let rate = self.profile.bitrate(config.nominal_bitrate)?;
        // A closed CAN232 may reject C. The subsequent S command must positively
        // confirm configuration while closed before O or L can be sent.
        match self.ack(b"C\r", &guard) {
            Ok(()) | Err(Error::Protocol(crate::Error::Rejected)) => {}
            Err(error) => return Err(error),
        }
        self.started = None;
        self.ack(&rate, &guard)?;
        if self.profile == Profile::CanableFd {
            if let Some(rate) = config.data_bitrate {
                let command = match rate {
                    2_000_000 => b"Y2\r",
                    5_000_000 => b"Y5\r",
                    _ => return Err(crate::Error::Unsupported.into()),
                };
                self.ack(command, &guard)?;
            }
            self.ack(if config.listen_only { b"M1\r" } else { b"M0\r" }, &guard)?;
        }
        self.frames.clear();
        self.ack(
            if config.listen_only && self.profile == Profile::Lawicel {
                b"L\r"
            } else {
                b"O\r"
            },
            &guard,
        )?;
        self.started = Some(config);
        Ok(())
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.started.is_some() {
            self.ack(b"C\r", &|| Ok(()))?;
            self.started = None;
        }
        Ok(())
    }
    pub fn transmit(
        &mut self,
        frame: &Frame,
        guard: impl Fn() -> Result<(), canharbor_core::Error>,
    ) -> Result<(), Error> {
        if self.started.is_none_or(|config| config.listen_only) {
            return Err(canharbor_core::Error::InvalidState.into());
        }
        if matches!(frame, Frame::FdData { .. }) && self.started.unwrap().data_bitrate.is_none() {
            return Err(crate::Error::Unsupported.into());
        }
        let command = encode_with_profile(frame, self.profile)?;
        self.ready()?;
        guard()?;
        // Once an I/O attempt begins, retain uncertainty until firmware replies.
        self.unknown = self.unknown.saturating_add(1);
        if let Err(error) = self.write(&command, &guard) {
            if matches!(error, Error::Control(_) | Error::Poisoned) {
                self.unknown -= 1;
            }
            return Err(error);
        }
        if self.profile == Profile::CanableFd {
            // No firmware acceptance or echo exists. Keep this count even when
            // a remote node later replies: it cannot confirm individual writes.
            return Ok(());
        }
        match self.reply(Instant::now() + WAIT) {
            Ok(Response::Ack) => {
                self.unknown -= 1;
                Ok(())
            }
            Ok(Response::TxAccepted { extended }) if extended == frame.id().extended() => {
                self.unknown -= 1;
                Ok(())
            }
            Err(Error::Protocol(crate::Error::Rejected)) => {
                self.unknown -= 1;
                Err(crate::Error::Rejected.into())
            }
            Err(error) => Err(error),
            _ => {
                self.poisoned = true;
                Err(Error::Unexpected)
            }
        }
    }
    /// Streaming mode assumes firmware already streams frames. Polling mode uses
    /// P without changing persistent firmware settings. Callers select the profile.
    pub fn poll(
        &mut self,
        polling: bool,
        guard: impl Fn() -> Result<(), canharbor_core::Error>,
    ) -> Result<(), Error> {
        if polling && self.profile == Profile::CanableFd {
            return Err(crate::Error::Unsupported.into());
        }
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.started.is_none() {
            return Err(canharbor_core::Error::InvalidState.into());
        }
        guard()?;
        if polling {
            self.write(b"P\r", &guard)?;
            // A polled frame is itself the reply; receive() routes it to frames.
            let before = self.frames.len();
            let deadline = Instant::now() + WAIT;
            loop {
                if self.frames.len() > before {
                    break;
                }
                if let Some(reply) = self.replies.pop_front() {
                    match reply? {
                        Response::Ack => break,
                        _ => {
                            self.poisoned = true;
                            return Err(Error::Unexpected);
                        }
                    }
                }
                if Instant::now() >= deadline {
                    self.poisoned = true;
                    return Err(Error::Timeout);
                }
                self.receive(
                    Duration::from_millis(20)
                        .min(deadline.saturating_duration_since(Instant::now())),
                )?;
            }
        } else {
            self.receive(Duration::from_millis(20))?;
        }
        // Unsolicited command replies cannot be attributed to a future command.
        if !self.replies.is_empty() {
            self.poisoned = true;
            return Err(Error::Unexpected);
        }
        Ok(())
    }
    pub fn status(
        &mut self,
        guard: impl Fn() -> Result<(), canharbor_core::Error>,
    ) -> Result<(), Error> {
        if self.profile == Profile::CanableFd {
            return Err(crate::Error::Unsupported.into());
        }
        self.write(b"F\r", &guard)?;
        match self.reply(Instant::now() + WAIT)? {
            Response::Status(flags) => {
                if self.statuses.len() == 64 {
                    self.poisoned = true;
                    return Err(Error::Overflow);
                }
                self.statuses.push_back(flags);
                Ok(())
            }
            _ => {
                self.poisoned = true;
                Err(Error::Unexpected)
            }
        }
    }
    pub fn frames(&mut self) -> impl Iterator<Item = Received> + '_ {
        self.frames.drain(..)
    }
    pub fn statuses(&mut self) -> impl Iterator<Item = u8> + '_ {
        self.statuses.drain(..)
    }
}
