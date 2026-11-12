// SPDX-License-Identifier: AGPL-3.0-or-later
//! Independently implemented LAWICEL and opt-in CANable FD ASCII framing.
//! Protocol facts: https://www.canusb.com/files/can232_v3.pdf (3.0B).
use canharbor_core::{CanId, Frame};
use std::fmt::Write;
pub const MAX_LINE: usize = 32;
/// Extended ID + DLC + 64 payload bytes, excluding CR.
pub const MAX_CANABLE_LINE: usize = 138;
pub const BITRATES: [u32; 9] = [
    10_000, 20_000, 50_000, 100_000, 125_000, 250_000, 500_000, 800_000, 1_000_000,
];
pub const CANABLE_BITRATES: [u32; 10] = [
    10_000, 20_000, 50_000, 100_000, 125_000, 250_000, 500_000, 750_000, 1_000_000, 83_300,
];
pub const CANABLE_DATA_BITRATES: [u32; 2] = [2_000_000, 5_000_000];
/// Explicit firmware dialect; version text is not used to infer capabilities.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    #[default]
    Lawicel,
    CanableFd,
}
impl Profile {
    pub fn nominal_bitrates(self) -> &'static [u32] {
        match self {
            Self::Lawicel => &BITRATES,
            Self::CanableFd => &CANABLE_BITRATES,
        }
    }
    pub fn data_bitrates(self) -> &'static [u32] {
        match self {
            Self::Lawicel => &[],
            Self::CanableFd => &CANABLE_DATA_BITRATES,
        }
    }
    pub fn validate_config(self, config: canharbor_core::Config) -> Result<(), Error> {
        if !self.nominal_bitrates().contains(&config.nominal_bitrate)
            || config
                .data_bitrate
                .is_some_and(|rate| !self.data_bitrates().contains(&rate))
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
    pub fn bitrate(self, rate: u32) -> Result<[u8; 3], Error> {
        let index = self
            .nominal_bitrates()
            .iter()
            .position(|&v| v == rate)
            .ok_or(Error::Unsupported)?;
        Ok([b'S', b'0' + index as u8, b'\r'])
    }
    fn max_line(self) -> usize {
        match self {
            Self::Lawicel => MAX_LINE,
            Self::CanableFd => MAX_CANABLE_LINE,
        }
    }
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("malformed SLCAN record")]
    Malformed,
    #[error("SLCAN line exceeds its bound")]
    TooLong,
    #[error("adapter rejected command")]
    Rejected,
    #[error("unsupported SLCAN operation")]
    Unsupported,
    #[error(transparent)]
    Frame(#[from] canharbor_core::Error),
}
#[derive(Debug, PartialEq, Eq)]
pub enum Response {
    Ack,
    /// Firmware accepted a command; this is not a CAN bus acknowledgement.
    TxAccepted {
        extended: bool,
    },
    Frame {
        frame: Frame,
        timestamp_ms: Option<u16>,
    },
    Status(u8),
    Version {
        hardware: u8,
        software: u8,
    },
    Serial([u8; 4]),
    /// Only decoded during an explicitly requested CANable version handshake.
    FirmwareVersion(String),
}
fn hex(bytes: &[u8]) -> Result<u32, Error> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(Error::Malformed);
    }
    bytes.iter().try_fold(0u32, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => byte - b'A' + 10,
        };
        value
            .checked_mul(16)
            .and_then(|value| value.checked_add(u32::from(digit)))
            .ok_or(Error::Malformed)
    })
}
/// Parse one complete line without its CR terminator. No LF, whitespace, padding,
/// arbitrary-length identifiers, or CAN FD extension is inferred.
pub fn parse(bytes: &[u8]) -> Result<Response, Error> {
    if bytes.len() > MAX_LINE {
        return Err(Error::TooLong);
    }
    let Some(&kind) = bytes.first() else {
        return Ok(Response::Ack);
    };
    match kind {
        b'z' | b'Z' if bytes.len() == 1 => Ok(Response::TxAccepted {
            extended: kind == b'Z',
        }),
        b'F' if bytes.len() == 3 => Ok(Response::Status(hex(&bytes[1..])? as u8)),
        b'V' if bytes.len() == 5 => Ok(Response::Version {
            hardware: hex(&bytes[1..3])? as u8,
            software: hex(&bytes[3..])? as u8,
        }),
        b'N' if bytes.len() == 5 && bytes[1..].iter().all(u8::is_ascii_alphanumeric) => {
            Ok(Response::Serial(bytes[1..].try_into().unwrap()))
        }
        b't' | b'T' | b'r' | b'R' => {
            let extended = matches!(kind, b'T' | b'R');
            let remote = matches!(kind, b'r' | b'R');
            let width = if extended { 8 } else { 3 };
            if bytes.len() < width + 2 {
                return Err(Error::Malformed);
            }
            let id = CanId::new(hex(&bytes[1..=width])?, extended)?;
            let dlc = hex(&bytes[width + 1..width + 2])? as u8;
            if dlc > 8 {
                return Err(Error::Malformed);
            }
            let payload = if remote { 0 } else { usize::from(dlc) * 2 };
            let end = width + 2 + payload;
            let timestamp_ms = match bytes.len().checked_sub(end) {
                Some(0) => None,
                Some(4) => {
                    let stamp = hex(&bytes[end..])? as u16;
                    if stamp >= 60_000 {
                        return Err(Error::Malformed);
                    }
                    Some(stamp)
                }
                _ => return Err(Error::Malformed),
            };
            let frame = if remote {
                Frame::remote(id, dlc)?
            } else {
                let data = bytes[width + 2..end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| hex(pair).map(|v| v as u8))
                    .collect::<Result<Vec<_>, _>>()?;
                Frame::classic(id, data)?
            };
            Ok(Response::Frame {
                frame,
                timestamp_ms,
            })
        }
        _ => Err(Error::Malformed),
    }
}
/// Encode a validated Classic data or RTR frame with a single CR terminator.
pub fn encode(frame: &Frame) -> Result<Vec<u8>, Error> {
    frame.validate()?;
    if matches!(frame, Frame::FdData { .. }) {
        return Err(Error::Unsupported);
    }
    let extended = frame.id().extended();
    let remote = matches!(frame, Frame::ClassicRemote { .. });
    let kind = match (extended, remote) {
        (false, false) => 't',
        (true, false) => 'T',
        (false, true) => 'r',
        (true, true) => 'R',
    };
    let width = if extended { 8 } else { 3 };
    let mut output = String::with_capacity(MAX_LINE + 1);
    write!(
        output,
        "{kind}{:0width$X}{:X}",
        frame.id().value(),
        frame.dlc()
    )
    .unwrap();
    for byte in frame.data() {
        write!(output, "{byte:02X}").unwrap();
    }
    output.push('\r');
    Ok(output.into_bytes())
}
/// CANable FD extension: d/D without BRS, b/B with BRS; DLC is hexadecimal.
/// Classic framing remains available in either profile.
pub fn encode_with_profile(frame: &Frame, profile: Profile) -> Result<Vec<u8>, Error> {
    if profile == Profile::Lawicel || !matches!(frame, Frame::FdData { .. }) {
        return encode(frame);
    }
    frame.validate()?;
    let Frame::FdData { id, brs, .. } = frame else {
        unreachable!()
    };
    let kind = match (id.extended(), brs) {
        (false, false) => 'd',
        (true, false) => 'D',
        (false, true) => 'b',
        (true, true) => 'B',
    };
    let width = if id.extended() { 8 } else { 3 };
    let mut output = String::with_capacity(MAX_CANABLE_LINE + 1);
    write!(output, "{kind}{:0width$X}{:X}", id.value(), frame.dlc()).unwrap();
    for byte in frame.data() {
        write!(output, "{byte:02X}").unwrap();
    }
    output.push('\r');
    Ok(output.into_bytes())
}
pub fn parse_with_profile(bytes: &[u8], profile: Profile) -> Result<Response, Error> {
    if profile == Profile::Lawicel || !matches!(bytes.first(), Some(b'd' | b'D' | b'b' | b'B')) {
        return parse(bytes);
    }
    if bytes.len() > MAX_CANABLE_LINE {
        return Err(Error::TooLong);
    }
    let extended = matches!(bytes[0], b'D' | b'B');
    let brs = matches!(bytes[0], b'b' | b'B');
    let width = if extended { 8 } else { 3 };
    if bytes.len() < width + 2 {
        return Err(Error::Malformed);
    }
    let id = CanId::new(hex(&bytes[1..=width])?, extended)?;
    let dlc = hex(&bytes[width + 1..width + 2])? as usize;
    let length = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64][dlc];
    if bytes.len() != width + 2 + length * 2 {
        return Err(Error::Malformed);
    }
    let data = bytes[width + 2..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| hex(pair).map(|v| v as u8))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Response::Frame {
        frame: Frame::fd(id, data, brs)?,
        timestamp_ms: None,
    })
}
pub fn bitrate(rate: u32) -> Result<[u8; 3], Error> {
    Profile::Lawicel.bitrate(rate)
}
/// A bounded incremental parser. Oversized records are discarded through CR;
/// BEL terminates a rejected command even when no CR follows it.
#[derive(Default)]
pub struct Decoder {
    profile: Profile,
    version_reply: bool,
    line: Vec<u8>,
    discarding: bool,
}
impl Decoder {
    pub fn with_profile(profile: Profile) -> Self {
        Self {
            profile,
            ..Self::default()
        }
    }
    pub(crate) fn expect_version(&mut self) {
        self.version_reply = self.profile == Profile::CanableFd;
    }
    pub fn push(&mut self, byte: u8) -> Option<Result<Response, Error>> {
        if byte == 7 {
            self.line.clear();
            self.discarding = false;
            return Some(Err(Error::Rejected));
        }
        if byte == b'\r' {
            let result = if self.discarding {
                Err(Error::TooLong)
            } else {
                let parsed = parse_with_profile(&self.line, self.profile);
                if self.version_reply
                    && matches!(parsed, Err(Error::Malformed | Error::TooLong))
                    && !self.line.is_empty()
                    && self.line.iter().all(|b| b.is_ascii_graphic() || *b == b' ')
                {
                    Ok(Response::FirmwareVersion(
                        String::from_utf8(self.line.clone()).unwrap(),
                    ))
                } else {
                    parsed
                }
            };
            if matches!(
                result,
                Ok(Response::FirmwareVersion(_) | Response::Version { .. })
            ) {
                self.version_reply = false;
            }
            self.line.clear();
            self.discarding = false;
            return Some(result);
        }
        if !self.discarding {
            if self.line.len() == self.profile.max_line() {
                self.line.clear();
                self.discarding = true;
            } else {
                self.line.push(byte);
            }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classic_and_remote_roundtrip_for_all_widths_and_lengths() {
        for extended in [false, true] {
            for dlc in 0..=8 {
                for remote in [false, true] {
                    let id =
                        CanId::new(if extended { 0x123abcd } else { 0x542 }, extended).unwrap();
                    let frame = if remote {
                        Frame::remote(id, dlc).unwrap()
                    } else {
                        Frame::classic(id, (0..dlc).map(|v| v * 17).collect()).unwrap()
                    };
                    let encoded = encode(&frame).unwrap();
                    assert_eq!(encoded.last(), Some(&b'\r'));
                    assert_eq!(
                        parse(&encoded[..encoded.len() - 1]).unwrap(),
                        Response::Frame {
                            frame,
                            timestamp_ms: None
                        }
                    );
                }
            }
        }
    }
    #[test]
    fn timestamps_are_raw_modulo_60000_and_never_guessed() {
        assert!(matches!(
            parse(b"t5421ABEA5F"),
            Ok(Response::Frame {
                timestamp_ms: Some(59999),
                ..
            })
        ));
        assert!(parse(b"t5421ABEA60").is_err());
        assert!(parse(b"r54220000").is_ok());
        assert!(parse(b"t5421AB000").is_err());
    }
    #[test]
    fn rejects_invalid_ids_lengths_hex_flags_and_fd() {
        for line in [
            b"t8000".as_slice(),
            b"T200000000",
            b"t5429",
            b"t5421AG",
            b"r5421AA",
            b"t5421ABextra",
            b"t5420\n",
            b"d5420",
            b"zX",
            b"F0G",
            b"V123",
        ] {
            assert!(parse(line).is_err(), "{line:?}");
        }
        assert!(encode(&Frame::fd(CanId::new(1, false).unwrap(), vec![], false).unwrap()).is_err());
        assert!(
            encode(&Frame::ClassicData {
                id: CanId::new(1, false).unwrap(),
                data: vec![0; 9]
            })
            .is_err()
        );
        for (index, &rate) in BITRATES.iter().enumerate() {
            assert_eq!(bitrate(rate).unwrap(), [b'S', b'0' + index as u8, b'\r']);
        }
        assert!(bitrate(333_333).is_err());
    }
    #[test]
    fn incremental_parser_bounds_and_recovers_after_long_records_and_bell() {
        let mut parser = Decoder::default();
        for _ in 0..100_000 {
            assert!(parser.push(b'X').is_none());
            assert!(parser.line.len() <= MAX_LINE);
        }
        assert_eq!(parser.push(b'\r'), Some(Err(Error::TooLong)));
        for &byte in b"invalid" {
            assert!(parser.push(byte).is_none());
        }
        assert_eq!(parser.push(7), Some(Err(Error::Rejected)));
        for &byte in b"t5420" {
            assert!(parser.push(byte).is_none());
        }
        assert!(matches!(
            parser.push(b'\r'),
            Some(Ok(Response::Frame { .. }))
        ));
        assert_eq!(parser.push(b'\r'), Some(Ok(Response::Ack)));
        assert_eq!(
            parse(b"z").unwrap(),
            Response::TxAccepted { extended: false }
        );
        assert_eq!(
            parse(b"Z").unwrap(),
            Response::TxAccepted { extended: true }
        );
        assert_eq!(
            parse(b"V0132").unwrap(),
            Response::Version {
                hardware: 1,
                software: 0x32
            }
        );
        assert_eq!(parse(b"N12AB").unwrap(), Response::Serial(*b"12AB"));
        assert_eq!(parse(b"Nq7Z9").unwrap(), Response::Serial(*b"q7Z9"));
        assert!(parse(b"N12-4").is_err());
        assert_eq!(parse(b"F09").unwrap(), Response::Status(9));
    }
}

pub mod backend;
pub mod engine;
pub mod serial;
