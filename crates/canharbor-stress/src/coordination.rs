// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
/// Transport is authenticated out-of-band (same-UID socket or SSH). No clock offset is assumed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressCoordination")]
pub struct Coordination {
    pub group_id: String,
    pub participant: String,
    pub lease_ms: u32,
    pub start_delay_ms: u32,
    pub clock_quality: String,
}
impl Coordination {
    pub fn validate(&self) -> Result<(), String> {
        if uuid::Uuid::parse_str(&self.group_id).is_err()
            || self.participant.is_empty()
            || self.participant.len() > 64
            || !self
                .participant
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !(3000..=15000).contains(&self.lease_ms)
            || self.start_delay_ms < 500
            || self.start_delay_ms > self.lease_ms - 1000
            || self.clock_quality != "unsynchronized"
        {
            return Err("coordination needs UUID, bounded participant, 3..15s lease, guarded start and explicit unsynchronized clocks".into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressCoordRequest")]
pub struct CoordRequest {
    pub operation: String,
    pub group_id: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub request: Option<Request>,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressCoordStatus")]
pub struct CoordStatus {
    pub group_id: String,
    pub state: String,
    pub token: Option<String>,
    pub expires_in_ms: u64,
    pub run_id: Option<String>,
    pub report: Option<Report>,
    pub server_instance_id: String,
    pub clock_quality: String,
}
impl Run {
    pub(crate) fn coordination_deadline(&self) -> Option<Instant> {
        if let Some(parent) = self.parent.as_ref().and_then(std::sync::Weak::upgrade) {
            return parent.coordination_deadline();
        }
        *self.coord_deadline.lock().unwrap()
    }
    pub(crate) fn coordination_expired(&self) -> bool {
        self.coordination_deadline()
            .is_some_and(|t| Instant::now() >= t)
    }
    pub fn execution_closed(&self) -> bool {
        self.gate.is_revoked()
    }
    pub fn renew_coordination(&self) -> Result<(), String> {
        if self.gate.is_revoked() {
            return Err("execution gate is closed; cannot revive".into());
        }
        let lease = self
            .report
            .lock()
            .unwrap()
            .plan
            .coordination
            .as_ref()
            .map(|c| c.lease_ms)
            .ok_or("run is not coordinated")?;
        let mut d = self.coord_deadline.lock().unwrap();
        if d.is_none_or(|t| Instant::now() >= t) {
            drop(d);
            self.stop();
            return Err("coordination lease expired; cannot revive".into());
        }
        *d = Some(Instant::now() + Duration::from_millis(u64::from(lease)));
        Ok(())
    }
}
