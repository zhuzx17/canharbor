// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "StressTraffic")]
pub enum Traffic {
    #[default]
    Classic,
    ClassicPatternV1 {
        checkpoint_can_id: u32,
        seed: u32,
        stream: u16,
        max_pending: u16,
    },
    FdV1 {
        data_bitrate: u32,
        stream: u16,
        epoch: u32,
        seed: u32,
        variants: Vec<FdVariant>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressFdVariant")]
pub struct FdVariant {
    pub length: u8,
    pub brs: bool,
}
impl Traffic {
    pub fn is_classic(&self) -> bool {
        matches!(self, Self::Classic)
    }
    pub fn is_pattern(&self) -> bool {
        matches!(self, Self::ClassicPatternV1 { .. })
    }
    pub fn checkpoint_id(&self) -> Option<u32> {
        if let Self::ClassicPatternV1 {
            checkpoint_can_id, ..
        } = self
        {
            Some(*checkpoint_can_id)
        } else {
            None
        }
    }
    pub fn data_bitrate(&self) -> Option<u32> {
        match self {
            Self::Classic | Self::ClassicPatternV1 { .. } => None,
            Self::FdV1 { data_bitrate, .. } => Some(*data_bitrate),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Self::ClassicPatternV1 { max_pending, .. } = self
            && (*max_pending == 0 || *max_pending > 4096)
        {
            return Err("Classic pattern window must be 1..4096".into());
        }
        if let Self::FdV1 {
            data_bitrate,
            epoch,
            variants,
            ..
        } = self
            && (*data_bitrate == 0
                || *epoch == 0
                || variants.is_empty()
                || variants.len() > 32
                || variants
                    .iter()
                    .any(|v| !matches!(v.length, 8 | 12 | 16 | 20 | 24 | 32 | 48 | 64)))
        {
            return Err("FD v1 needs data bitrate, nonzero epoch, and 1..32 legal variants of at least 8 bytes".into());
        }
        Ok(())
    }
    pub fn variant(&self, n: u32) -> Option<&FdVariant> {
        match self {
            Self::Classic | Self::ClassicPatternV1 { .. } => None,
            Self::FdV1 { variants, .. } => Some(&variants[n as usize % variants.len()]),
        }
    }
    pub fn integrity(&self) -> &'static str {
        match self {
            Self::Classic => "sequence_only",
            Self::ClassicPatternV1 { .. } => "classic_pattern_v1_checkpoint_required",
            Self::FdV1 { variants, .. } if variants.iter().all(|v| v.length >= 32) => {
                "crc32c_pattern_v1"
            }
            _ => "mixed_crc32c_and_short_sequence_only",
        }
    }
}
/// Castagnoli, reflected polynomial, init/xorout all ones. Application CRC, not authentication.
const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut crc = n as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0x82f63b78u32.wrapping_mul(crc & 1));
            bit += 1;
        }
        table[n] = crc;
        n += 1;
    }
    table
}
const CRC_TABLE: [u32; 256] = crc_table();
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(*byte)) & 255) as usize];
    }
    !crc
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadError {
    Unrelated,
    Envelope,
    Crc,
    Pattern,
}
impl Plan {
    pub fn wire_frame(&self, n: u32, response: bool) -> Frame {
        if let Some(checkpoint) = self.traffic.checkpoint_id() {
            let id = CanId::new(
                if n.is_multiple_of(2) {
                    checkpoint
                } else {
                    self.can_id
                },
                self.extended,
            )
            .expect("validated ID");
            let data = if n.is_multiple_of(2) {
                let mut d = self.run_tag.to_be_bytes().to_vec();
                d.extend((n / 2).to_be_bytes());
                d
            } else {
                self.pattern_bytes(n / 2).to_vec()
            };
            return Frame::classic(id, data).unwrap();
        }
        let id = CanId::new(
            if response {
                self.rtt.as_ref().expect("RTT plan").response_can_id
            } else {
                self.can_id
            },
            self.extended,
        )
        .expect("validated ID");
        let Some(v) = self.traffic.variant(n) else {
            let mut data = self.run_tag.to_be_bytes().to_vec();
            data.extend(n.to_be_bytes());
            return Frame::classic(id, data).unwrap();
        };
        let Traffic::FdV1 {
            stream,
            epoch,
            seed,
            ..
        } = self.traffic
        else {
            unreachable!()
        };
        let mut data = vec![0u8; v.length as usize];
        if v.length < 32 {
            data[..4].copy_from_slice(&self.run_tag.to_be_bytes());
            data[4..8].copy_from_slice(&n.to_be_bytes());
            for (i, b) in data.iter_mut().enumerate().skip(8) {
                *b = pattern(seed, n, i);
            }
        } else {
            data[..4].copy_from_slice(&[0x43, 0x48, 1, u8::from(response)]);
            data[4..8].copy_from_slice(&self.run_tag.to_be_bytes());
            data[8..10].copy_from_slice(&stream.to_be_bytes());
            data[12..16].copy_from_slice(&epoch.to_be_bytes());
            data[16..24].copy_from_slice(&u64::from(n).to_be_bytes());
            data[24..28].copy_from_slice(&seed.to_be_bytes());
            let end = data.len() - 4;
            for (i, b) in data.iter_mut().enumerate().take(end).skip(28) {
                *b = pattern(seed, n, i);
            }
            let checksum = wire_crc(id, v.brs, &data[..end]);
            data[end..].copy_from_slice(&checksum.to_be_bytes());
        }
        Frame::fd(id, data, v.brs).unwrap()
    }
    /// Extract correlation before CRC validation so corrupted tagged frames can be retained as evidence.
    pub fn correlate(&self, frame: &Frame, response: bool) -> Option<u32> {
        if let Some(checkpoint) = self.traffic.checkpoint_id() {
            let d = frame.data();
            if frame.id().value() != checkpoint
                || frame.id().extended() != self.extended
                || !matches!(frame, Frame::ClassicData { .. })
                || d.len() != 8
                || d[..4] != self.run_tag.to_be_bytes()
            {
                return None;
            }
            return u32::from_be_bytes(d[4..8].try_into().ok()?).checked_mul(2);
        }
        let id = if response {
            self.rtt.as_ref()?.response_can_id
        } else {
            self.can_id
        };
        if frame.id().value() != id || frame.id().extended() != self.extended {
            return None;
        }
        let d = frame.data();
        if self.traffic.is_classic() || d.len() < 32 {
            if d.len() < 8 || u32::from_be_bytes(d[..4].try_into().ok()?) != self.run_tag {
                return None;
            }
            Some(u32::from_be_bytes(d[4..8].try_into().ok()?))
        } else {
            if d.len() < 32 || u32::from_be_bytes(d[4..8].try_into().ok()?) != self.run_tag {
                return None;
            }
            u32::try_from(u64::from_be_bytes(d[16..24].try_into().ok()?)).ok()
        }
    }
    pub fn decode(&self, frame: &Frame, response: bool) -> Result<u32, PayloadError> {
        let n = self
            .correlate(frame, response)
            .ok_or(PayloadError::Unrelated)?;
        if !self.traffic.is_classic() && frame.data().len() >= 32 {
            let Frame::FdData { id, data, brs } = frame else {
                return Err(PayloadError::Envelope);
            };
            let end = data.len() - 4;
            if crc32c_wire(*id, *brs, &data[..end])
                != u32::from_be_bytes(data[end..].try_into().unwrap())
            {
                return Err(PayloadError::Crc);
            }
        }
        if *frame != self.wire_frame(n, response) {
            return Err(PayloadError::Pattern);
        }
        Ok(n)
    }
}
fn pattern(seed: u32, n: u32, i: usize) -> u8 {
    let mut x = seed ^ n.wrapping_mul(0x9e3779b9) ^ (i as u32).wrapping_mul(0x85ebca6b);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x as u8
}
fn wire_crc(id: CanId, brs: bool, data: &[u8]) -> u32 {
    crc32c_wire(id, brs, data)
}
fn crc32c_wire(id: CanId, brs: bool, data: &[u8]) -> u32 {
    let mut bytes = b"canharbor-fd-v1".to_vec();
    bytes.extend(id.value().to_be_bytes());
    bytes.extend([u8::from(id.extended()), 1, u8::from(brs)]);
    bytes.push((data.len() + 4) as u8);
    bytes.extend(data);
    crc32c(&bytes)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleWire {
    pub can_id: u32,
    pub extended: bool,
    pub fd: bool,
    pub brs: bool,
}
impl SampleWire {
    pub fn of(f: &Frame) -> Self {
        Self {
            can_id: f.id().value(),
            extended: f.id().extended(),
            fd: matches!(f, Frame::FdData { .. }),
            brs: matches!(f, Frame::FdData { brs: true, .. }),
        }
    }
    pub fn frame(&self, data: Vec<u8>) -> Result<Frame, String> {
        let id = CanId::new(self.can_id, self.extended).map_err(|e| e.to_string())?;
        if self.fd {
            Frame::fd(id, data, self.brs)
        } else if self.brs {
            Err(canharbor_core::Error::InvalidFrame)
        } else {
            Frame::classic(id, data)
        }
        .map_err(|e| e.to_string())
    }
}
