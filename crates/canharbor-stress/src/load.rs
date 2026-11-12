// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "StressLoadShape")]
pub enum LoadShape {
    #[default]
    Uniform,
    Burst {
        frames: u16,
    },
    SeededIntervals {
        seed: u32,
        jitter_ppm: u32,
    },
}
impl LoadShape {
    pub fn is_uniform(&self) -> bool {
        matches!(self, Self::Uniform)
    }
    pub fn peak(&self, fps: u32) -> Result<u32, String> {
        if fps == 0 || fps > MAX_FPS {
            return Err("shape rate out of bounds".into());
        }
        let peak = match self {
            Self::Uniform => fps,
            Self::Burst { frames }
                if *frames > 0
                    && u32::from(*frames) <= MAX_BATCH
                    && fps <= u32::from(*frames) * 100 =>
            {
                u32::from(*frames) * 100
            }
            Self::SeededIntervals { jitter_ppm, .. } if (1..=500_000).contains(jitter_ppm) => {
                (fps.div_ceil(100) + 1) * 100
            }
            _ => return Err("invalid burst size or seeded interval jitter (1..500000 ppm)".into()),
        };
        if peak > MAX_FPS {
            return Err("load shape exceeds physical peak/quantum budget".into());
        }
        Ok(peak)
    }
    pub fn offset(&self, n: u32, fps: u32) -> u64 {
        match self {
            Self::Uniform => u64::from(n) * 1_000_000_000 / u64::from(fps),
            Self::Burst { frames } => {
                u64::from(n / u32::from(*frames) * u32::from(*frames)) * 1_000_000_000
                    / u64::from(fps)
            }
            Self::SeededIntervals { seed, jitter_ppm } => {
                let mut x = seed ^ n.wrapping_mul(0x9e3779b9);
                x ^= x >> 16;
                x = x.wrapping_mul(0x7feb352d);
                x ^= x >> 15;
                x = x.wrapping_mul(0x846ca68b);
                x ^= x >> 16;
                let bound = 1_000_000_000 / u64::from(fps) * u64::from(*jitter_ppm) / 1_000_000;
                u64::from(n) * 1_000_000_000 / u64::from(fps) + u64::from(x) % (bound + 1)
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressLoadBudget")]
pub struct LoadBudget {
    pub reserved_planned: u32,
    pub reserved_peak_fps: u32,
    pub reserved_duration_ns: u64,
    pub estimated_evidence_bytes: u64,
    pub maximum_quantum_frames: u32,
}
impl Plan {
    pub fn load_budget(&self) -> Result<Option<LoadBudget>, String> {
        if !self.requires_load_v10() {
            return Ok(None);
        }
        let c = self.compile()?;
        Ok(Some(LoadBudget {
            reserved_planned: c.count,
            reserved_peak_fps: c.max_fps,
            reserved_duration_ns: c.duration_ns,
            estimated_evidence_bytes: c.evidence_bytes,
            maximum_quantum_frames: if self.flows.is_empty() {
                c.max_fps.div_ceil(100)
            } else {
                self.producers().iter().map(|p| p.3.div_ceil(100)).sum()
            },
        }))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressLoadObservation")]
pub struct LoadObservation {
    pub start_ns: u64,
    pub end_ns: u64,
    pub scheduled_duration_ns: u64,
    pub completed: bool,
    pub submitted: u64,
    pub skipped: u64,
    pub actual_fps_milli: u64,
}
impl LoadObservation {
    pub(crate) fn new(
        start_ns: u64,
        end_ns: u64,
        duration: u64,
        completed: bool,
        submitted: u64,
        skipped: u64,
    ) -> Self {
        let actual_fps_milli = if end_ns > start_ns {
            (u128::from(submitted) * 1_000_000_000_000 / u128::from(end_ns - start_ns))
                .min(u128::from(u64::MAX)) as u64
        } else {
            0
        };
        Self {
            start_ns,
            end_ns,
            scheduled_duration_ns: duration,
            completed,
            submitted,
            skipped,
            actual_fps_milli,
        }
    }
    pub fn valid(&self, r: &Report) -> bool {
        let Ok(c) = r.plan.compile() else {
            return false;
        };
        self.start_ns <= 73 * 3600 * 1_000_000_000
            && self.end_ns <= 73 * 3600 * 1_000_000_000
            && (self.submitted == 0
                || r.lifecycle
                    .last_submitted_ns
                    .is_some_and(|n| n >= self.start_ns))
            && r.lifecycle.last_attempt_ns.is_none_or(|n| n <= self.end_ns)
            && r.lifecycle
                .last_submitted_ns
                .is_none_or(|n| n <= self.end_ns)
            && (self.end_ns >= self.start_ns || !self.completed && self.submitted == 0)
            && self.scheduled_duration_ns == c.duration_ns
            && self.submitted == r.submitted
            && self.skipped == r.skipped
            && *self
                == Self::new(
                    self.start_ns,
                    self.end_ns,
                    c.duration_ns,
                    self.completed,
                    r.submitted,
                    r.skipped,
                )
            && (!self.completed || self.end_ns - self.start_ns >= c.duration_ns)
            && r.lifecycle
                .execution_end_ns
                .is_some_and(|n| self.end_ns <= n)
    }
}
impl Plan {
    pub fn requires_load_v10(&self) -> bool {
        self.adaptive.is_some()
            || !self.shape.is_uniform()
            || self.flows.iter().any(|f| !f.shape.is_uniform())
    }
}
