// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
pub const MAX_FPS: u32 = 20_000;
pub const QUANTUM_NS: u64 = 10_000_000;
pub const MAX_BATCH: u32 = 256;
pub const MEMORY_LIMIT: u64 = 256 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressStage")]
pub struct Stage {
    pub name: String,
    pub fps: u32,
    pub duration_seconds: u32,
    #[serde(default)]
    pub warmup_seconds: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressEvidenceOptions")]
pub struct EvidenceOptions {
    pub max_bytes: u64,
    pub segment_bytes: u64,
    /// Zero disables frame samples; online verification remains complete.
    pub sample_every: u32,
}
impl Default for EvidenceOptions {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024 * 1024,
            segment_bytes: 4 * 1024 * 1024,
            sample_every: 0,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressStageReport")]
pub struct StageReport {
    pub name: String,
    pub fps: u32,
    pub warmup: bool,
    pub first: u32,
    pub count: u32,
    pub start_ns: u64,
    pub submitted: u64,
    pub skipped: u64,
    pub received_unique: Vec<u64>,
}
#[derive(Debug)]
pub struct Compiled {
    pub shape: LoadShape,
    pub evidence_bytes: u64,
    pub count: u32,
    pub duration_ns: u64,
    pub tracking_bytes: u64,
    pub stages: Vec<StageReport>,
    pub max_fps: u32,
}
impl Plan {
    pub fn compile(&self) -> Result<Compiled, String> {
        if self.adaptive.is_some() {
            return self.compile_adaptive();
        }
        if !self.flows.is_empty() {
            return self.compile_group();
        }
        if let Some(w) = self.tracking.capacity() {
            if !(64..=1_048_576).contains(&w) || !w.is_power_of_two() {
                return Err("sliding capacity must be a power of two in 64..1048576".into());
            }
            if self
                .rtt
                .as_ref()
                .is_some_and(|r| r.role == RttRole::Responder)
            {
                return Err("sliding tracking requires a local transmit sequence watermark".into());
            }
        }
        if self.queues.as_ref().is_some_and(|q| q.sample_every == 0) {
            return Err("queue sample_every must be positive".into());
        }
        self.criteria.validate()?;
        if let Some(t) = &self.topology {
            t.validate(self)?;
        }
        if let Some(t) = &self.timing
            && (t.sample_every == 0 || t.max_pending == 0 || t.max_pending > 4096)
        {
            return Err("timing needs sample_every>0 and max_pending 1..4096".into());
        }
        CanId::new(self.can_id, self.extended).map_err(|e| e.to_string())?;
        if self.bitrate == 0
            || !(10..=10000).contains(&self.drain_ms)
            || self.receivers.is_empty()
            || self.receivers.len() > 7
        {
            return Err("invalid static role/bitrate/drain bounds".into());
        }
        let mut roles = HashSet::new();
        for role in std::iter::once(&self.sender).chain(self.receivers.iter()) {
            if role.is_empty() || role.len() > 256 || !roles.insert(role) {
                return Err("invalid or duplicate channel role".into());
            }
        }

        self.traffic.validate()?;
        if let Some(c) = &self.coordination {
            c.validate()?;
        }
        self.validate_rtt()?;
        let mut stages = vec![];
        let mut count = 0u32;
        let mut ns = 0u64;
        let mut max_fps = 0;
        let mut add = |name: String, fps: u32, seconds: u32, warmup: bool| -> Result<(), String> {
            if fps == 0 || fps > MAX_FPS || seconds == 0 {
                return Err("stage rate/duration out of bounds".into());
            }
            let n = fps
                .checked_mul(seconds)
                .ok_or("stage sequence range overflow")?;
            let first = count;
            count = count
                .checked_add(n)
                .ok_or("u32 sequence range exhausted; reduce duration or rate")?;
            stages.push(StageReport {
                name,
                fps,
                warmup,
                first,
                count: n,
                start_ns: ns,
                submitted: 0,
                skipped: 0,
                received_unique: vec![],
            });
            ns = ns
                .checked_add(u64::from(seconds) * 1_000_000_000)
                .ok_or("duration overflow")?;
            max_fps = max_fps.max(fps);
            Ok(())
        };
        if self.stages.is_empty() {
            if self.count == 0 || self.fps == 0 || self.fps > MAX_FPS {
                return Err("count/rate out of bounds".into());
            }
            count = self.count;
            ns = if self.shape.is_uniform() {
                u64::from(count).div_ceil(u64::from(self.fps)) * 1_000_000_000
            } else {
                (u64::from(count) * 1_000_000_000).div_ceil(u64::from(self.fps))
            };
            max_fps = self.fps;
            stages.push(StageReport {
                name: "fixed".into(),
                fps: self.fps,
                warmup: false,
                first: 0,
                count,
                start_ns: 0,
                submitted: 0,
                skipped: 0,
                received_unique: vec![],
            });
        } else {
            if self.count != 0 || self.fps != 0 || self.stages.len() > 32 {
                return Err("staged plans require count=0, fps=0 and at most 32 stages".into());
            }
            for stage in &self.stages {
                if stage.name.is_empty() || stage.name.len() > 64 {
                    return Err("stage name must have 1..64 bytes".into());
                }
                if stage.warmup_seconds > 0 {
                    add(stage.name.clone(), stage.fps, stage.warmup_seconds, true)?;
                }
                add(stage.name.clone(), stage.fps, stage.duration_seconds, false)?;
            }
        }
        if self
            .stability
            .max_rss_growth_bytes
            .is_some_and(|limit| limit == 0)
            || (self.stability.max_rss_growth_bytes.is_some()
                && u64::from(self.stability.warmup_seconds) * 1_000_000_000 >= ns)
        {
            return Err(
                "RSS stability criterion needs a nonzero limit and a warmup shorter than the plan"
                    .into(),
            );
        }
        if ns > 72 * 3600 * 1_000_000_000 {
            return Err("plan exceeds 72 hours".into());
        }
        if !self.shape.is_uniform() && self.rtt.is_some() {
            return Err(
                "shaped schedules do not include asynchronous RTT replies; use uniform for RTT"
                    .into(),
            );
        }
        max_fps = self.shape.peak(max_fps)?;
        if let Traffic::ClassicPatternV1 {
            checkpoint_can_id,
            max_pending,
            ..
        } = self.traffic
        {
            CanId::new(checkpoint_can_id, self.extended).map_err(|e| e.to_string())?;
            if checkpoint_can_id == self.can_id
                || self.rtt.is_some()
                || self.evidence.sample_every != 1
                || stages.iter().any(|s| s.count % 2 != 0 || s.fps % 2 != 0)
            {
                return Err("Classic pattern needs distinct control ID, even physical frame count/rate, sample_every=1, and no RTT".into());
            }
            let _ = max_pending;
        }
        let mut tracking_bytes =
            u64::from(count).div_ceil(64) * 8 * (1 + 2 * u64::from(self.rtt.is_some()))
                + u64::from(self.tracking.capacity().unwrap_or(count)).div_ceil(64)
                    * 8
                    * self.receivers.len() as u64;
        if self.requires_resource_v11() {
            tracking_bytes += self.receivers.len() as u64 * (stages.len() as u64 * 8 + 4096);
        }
        let records = self.resource_records(max_fps)
            + self.timing.as_ref().map_or(0, |t| {
                max_fps.div_ceil(t.sample_every)
                    * (if self.rtt.is_some() { 4 } else { 2 } + self.receivers.len() as u32)
            })
            + if self.evidence.sample_every == 0 {
                0
            } else {
                max_fps.div_ceil(self.evidence.sample_every) * self.receivers.len() as u32
            };
        if records > 2000 {
            return Err(
                "combined frame/timing/queue/window evidence exceeds 2000 records/s".into(),
            );
        }
        if let Traffic::ClassicPatternV1 { max_pending, .. } = self.traffic {
            tracking_bytes += u64::from(max_pending) * 256 * self.receivers.len() as u64;
        }
        if let Some(t) = &self.timing {
            let lanes = if self.rtt.is_some() { 2 } else { 1 };
            if u64::from(max_fps).div_ceil(u64::from(t.sample_every))
                * (2 * lanes + self.receivers.len() as u64)
                + if self.evidence.sample_every > 0 {
                    u64::from(max_fps).div_ceil(u64::from(self.evidence.sample_every))
                        * self.receivers.len() as u64
                } else {
                    0
                }
                > 2000
            {
                return Err("timing evidence exceeds 2000 records/s; increase sample_every".into());
            }
            tracking_bytes += u64::from(count).div_ceil(u64::from(t.sample_every))
                * (4 + self.receivers.len() as u64)
                + u64::from(t.max_pending) * 4096;
        }
        if tracking_bytes + 16 * 1024 * 1024 > MEMORY_LIMIT {
            return Err("sequence tracking plus queue reserve exceeds 256 MiB".into());
        }
        if self.evidence.max_bytes < 8 * 1024 * 1024
            || self.evidence.max_bytes > 2 * 1024 * 1024 * 1024
            || self.evidence.segment_bytes < 4096
            || self.evidence.segment_bytes > 16 * 1024 * 1024
            || self.evidence.segment_bytes > self.evidence.max_bytes
        {
            return Err("evidence disk/segment budget out of bounds".into());
        }
        if self.evidence.sample_every > 0
            && u64::from(max_fps).div_ceil(u64::from(self.evidence.sample_every))
                * self.receivers.len() as u64
                > 2000
        {
            return Err("frame sampling exceeds 2000 records/s; increase sample_every".into());
        }
        if self.rtt.is_some() && max_fps > 1000 {
            return Err("RTT is limited to 1000 requests/s plus at most 1000 replies/s".into());
        }
        // Compact metrics are at most 4 KiB; reject a guaranteed undersized reservation.
        let outcome_estimate =
            u64::from(count).min(ns.div_ceil(1_000_000_000) * u64::from(max_fps.min(100)) * 2) * 96;
        let metric_estimate = ns.div_ceil(1_000_000_000)
            * if self.requires_resource_v11() {
                16 * 1024
            } else {
                4096
            }
            + outcome_estimate
            + 4 * 1024 * 1024
            + if self.rtt.is_some() {
                u64::from(count) * 256
            } else {
                0
            };
        let timing_estimate = self.timing.as_ref().map_or(0, |t| {
            u64::from(count).div_ceil(u64::from(t.sample_every))
                * (2048 * (if self.rtt.is_some() { 2 } else { 1 })
                    + self.receivers.len() as u64 * 256)
                + 4096
        });
        let resource_estimate = self.queues.as_ref().map_or(0, |q| {
            u64::from(count).div_ceil(u64::from(q.sample_every)) * self.receivers.len() as u64 * 320
        }) + self.tracking.capacity().map_or(0, |w| {
            u64::from(count).div_ceil(u64::from(w / 2)) * (self.receivers.len() as u64 + 1) * 512
        });
        let sample_estimate = resource_estimate
            + timing_estimate
            + if self.evidence.sample_every == 0 {
                0
            } else {
                u64::from(count).div_ceil(u64::from(self.evidence.sample_every))
                    * self.receivers.len() as u64
                    * 512
            };
        if (metric_estimate - 4 * 1024 * 1024 + sample_estimate)
            .div_ceil(self.evidence.segment_bytes)
            + 5
            > 1000
        {
            return Err("requested evidence segmentation exceeds 1000 files".into());
        }
        if metric_estimate.saturating_add(sample_estimate) > self.evidence.max_bytes {
            return Err(
                "evidence budget cannot hold metrics and requested frame samples for this duration"
                    .into(),
            );
        }
        Ok(Compiled {
            shape: self.shape.clone(),
            evidence_bytes: metric_estimate + sample_estimate,
            count,
            duration_ns: ns,
            tracking_bytes,
            stages,
            max_fps,
        })
    }
}
impl Compiled {
    pub fn stage(&self, n: u32) -> usize {
        self.stages
            .partition_point(|s| s.first <= n)
            .saturating_sub(1)
    }
    pub fn deadline_ns(&self, n: u32) -> u64 {
        let s = &self.stages[self.stage(n)];
        s.start_ns + self.shape.offset(n - s.first, s.fps)
    }
    pub fn target_ns(&self, n: u32) -> u64 {
        self.deadline_ns(n) / QUANTUM_NS * QUANTUM_NS
    }
    pub fn next_after(&self, now_ns: u64, from: u32) -> u32 {
        let mut low = from;
        let mut high = self.count;
        while low < high {
            let mid = low + (high - low) / 2;
            if self.target_ns(mid) <= now_ns {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }
}
