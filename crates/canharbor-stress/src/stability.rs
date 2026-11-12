// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressStabilityLimits")]
pub struct StabilityLimits {
    pub max_rss_growth_bytes: Option<u64>,
    pub warmup_seconds: u32,
}
impl Default for StabilityLimits {
    fn default() -> Self {
        Self {
            max_rss_growth_bytes: None,
            warmup_seconds: 60,
        }
    }
}
impl StabilityLimits {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressStabilityReport")]
pub struct StabilityReport {
    pub baseline_rss_bytes: Option<u64>,
    pub latest_rss_bytes: Option<u64>,
    pub max_growth_bytes: u64,
    pub samples: u64,
    pub limit_exceeded: bool,
    pub status: String,
}
impl StabilityReport {
    pub fn observe(&mut self, limits: &StabilityLimits, elapsed: Duration, rss: Option<u64>) {
        self.samples += 1;
        self.latest_rss_bytes = rss;
        let Some(limit) = limits.max_rss_growth_bytes else {
            self.status = "disabled".into();
            return;
        };
        if elapsed < Duration::from_secs(u64::from(limits.warmup_seconds)) {
            self.status = "warming".into();
            return;
        }
        let Some(rss) = rss else {
            self.status = "unsupported".into();
            return;
        };
        let baseline = *self.baseline_rss_bytes.get_or_insert(rss);
        self.max_growth_bytes = self.max_growth_bytes.max(rss.saturating_sub(baseline));
        self.limit_exceeded = self.max_growth_bytes > limit;
        self.status = if self.limit_exceeded {
            "exceeded"
        } else {
            "monitoring"
        }
        .into();
    }
}
pub use canharbor_platform::process::current_rss;
