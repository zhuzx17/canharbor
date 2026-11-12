// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::mpsc;
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressEvidenceStatus")]
pub struct EvidenceStatus {
    pub complete: bool,
    pub bytes: u64,
    pub records: u64,
    pub queue_high_water: u32,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressMetric")]
pub struct Metric {
    #[serde(default)]
    pub max_dispatch_lateness_ns: u64,
    pub elapsed_ms: u64,
    pub state: String,
    pub attempted: u64,
    pub submitted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub unknown: u64,
    pub active_stage: u32,
    pub target_fps: u32,
    pub receivers: Vec<ReceiverReport>,
    pub evidence_queue_high_water: u32,
    pub process_cpu_seconds: Option<f64>,
    pub process_peak_rss_bytes: Option<u64>,
    #[serde(default)]
    pub process_current_rss_bytes: Option<u64>,
    #[serde(default)]
    pub stability: StabilityReport,
    #[serde(default)]
    pub lifecycle: Lifecycle,
    #[serde(default)]
    pub unknown_sequence: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvidenceEvent {
    TrackingRecord {
        event: Box<EvidenceEvent>,
    },
    TrackingAdvance {
        channel: String,
        next_sequence: u32,
    },
    QueueSample {
        channel: String,
        ordinal: u64,
        received_ns: u64,
        processed_ns: u64,
        depth: u32,
    },
    LoadWindow(LoadObservation),
    AdaptiveDecision(AdaptiveDecision),
    PatternFrame {
        channel: String,
        data_hex: String,
        wire: SampleWire,
        elapsed_ns: u64,
    },
    Diagnostic {
        channel: String,
        elapsed_ns: u64,
        classes: u32,
        data: [u8; 8],
        overflow: bool,
    },
    SchedulerSkip {
        first: u32,
        count: u32,
        elapsed_ns: u64,
        lateness_ns: u64,
    },
    GroupRelease {
        start_ns: u64,
        ready_flows: u32,
    },
    FlowRecord {
        name: String,
        event: Box<EvidenceEvent>,
    },
    FlowEnd {
        name: String,
        report: Box<Report>,
    },
    FlowStop {
        name: String,
        requested_ns: u64,
    },
    Timing(TimingEvent),
    Provenance(Provenance),
    Metric(Metric),
    Submissions {
        ranges: Vec<SubmissionRange>,
    },
    Lifecycle {
        elapsed_ns: u64,
        state: String,
        attempted: u64,
        submitted: u64,
        skipped: u64,
        failed: u64,
        unknown: u64,
        unknown_sequence: Option<u32>,
        lifecycle: Lifecycle,
    },
    Sample {
        channel: String,
        sequence: u32,
        data_hex: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wire: Option<SampleWire>,
        elapsed_ns: u64,
    },
    Reply {
        sequence: u32,
        outcome: String,
    },
    Rtt {
        sequence: u32,
        sent_ns: u64,
        received_ns: u64,
    },
    End(Box<Report>),
}
struct Health {
    failed: AtomicBool,
    error: Mutex<Option<String>>,
    high: AtomicUsize,
}
#[derive(Clone)]
pub struct EvidenceSink {
    flow: Option<String>,
    ordered: bool,
    tx: mpsc::Sender<EvidenceEvent>,
    health: Arc<Health>,
}
impl EvidenceSink {
    pub(crate) fn for_flow(&self, name: &str) -> Self {
        Self {
            flow: Some(name.into()),
            ordered: self.ordered,
            tx: self.tx.clone(),
            health: self.health.clone(),
        }
    }
    pub(crate) fn ordered(mut self, ordered: bool) -> Self {
        self.ordered = ordered;
        self
    }
    fn wrap(&self, event: EvidenceEvent) -> EvidenceEvent {
        let event = if self.ordered {
            EvidenceEvent::TrackingRecord {
                event: Box::new(event),
            }
        } else {
            event
        };
        if let Some(name) = &self.flow {
            EvidenceEvent::FlowRecord {
                name: name.clone(),
                event: Box::new(event),
            }
        } else {
            event
        }
    }
    pub fn blocking_record(&self, event: EvidenceEvent) -> Result<(), String> {
        self.tx
            .blocking_send(self.wrap(event))
            .map_err(|_| "evidence writer closed".into())
    }

    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<EvidenceEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                flow: None,
                ordered: false,
                tx,
                health: Arc::new(Health {
                    failed: AtomicBool::new(false),
                    error: Mutex::new(None),
                    high: AtomicUsize::new(0),
                }),
            },
            rx,
        )
    }
    pub fn fail(&self, error: impl Into<String>) {
        let mut reason = self.health.error.lock().unwrap();
        if reason.is_none() {
            *reason = Some(error.into());
        }
        self.health.failed.store(true, Ordering::Release);
    }
    pub fn failed(&self) -> bool {
        self.health.failed.load(Ordering::Acquire)
    }
    pub fn error(&self) -> Option<String> {
        self.health.error.lock().unwrap().clone()
    }
    pub fn high_water(&self) -> u32 {
        self.health.high.load(Ordering::Relaxed) as u32
    }
    pub fn emit(&self, event: EvidenceEvent) {
        if self.failed() {
            return;
        }
        match self.tx.try_send(self.wrap(event)) {
            Ok(()) => {
                let used = self.tx.max_capacity() - self.tx.capacity();
                self.health.high.fetch_max(used, Ordering::Relaxed);
            }
            Err(_) => self.fail("evidence queue full or writer closed"),
        }
    }
    pub async fn finish(&self, report: Report) -> Result<(), String> {
        self.tx
            .send(EvidenceEvent::End(Box::new(report)))
            .await
            .map_err(|_| "evidence writer closed".into())
    }
}
impl Report {
    pub fn metric(&self) -> Metric {
        Metric {
            max_dispatch_lateness_ns: self.max_dispatch_lateness_ns,
            elapsed_ms: (self.elapsed_seconds * 1000.0) as u64,
            state: self.state.clone(),
            attempted: self.attempted,
            submitted: self.submitted,
            skipped: self.skipped,
            failed: self.failed,
            unknown: self.unknown,
            active_stage: self.active_stage,
            target_fps: self
                .stages
                .get(self.active_stage as usize)
                .map_or(0, |s| s.fps),
            receivers: self.receivers.clone(),
            evidence_queue_high_water: self.evidence.queue_high_water,
            process_cpu_seconds: process_usage().map(|v| v.0),
            process_peak_rss_bytes: process_usage().map(|v| v.1),
            process_current_rss_bytes: current_rss(),
            stability: self.stability.clone(),
            lifecycle: self.lifecycle.clone(),
            unknown_sequence: self.unknown_sequence,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressArtifact")]
pub struct Artifact {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressArtifacts")]
pub struct Artifacts {
    pub files: Vec<Artifact>,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressRunSummary")]
pub struct Summary {
    pub id: String,
    pub state: String,
    pub verdict: String,
    pub submitted: u64,
    pub planned: u32,
    pub simulated: bool,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressRuns")]
pub struct Runs {
    pub runs: Vec<Summary>,
    #[serde(default)]
    pub recovery_issues: Vec<RecoveryIssue>,
}
#[derive(Clone)]
pub struct EvidenceMonitor {
    health: Arc<Health>,
}
impl EvidenceSink {
    pub fn monitor(&self) -> EvidenceMonitor {
        EvidenceMonitor {
            health: self.health.clone(),
        }
    }
}
impl EvidenceMonitor {
    pub fn fail(&self, error: String) {
        let mut e = self.health.error.lock().unwrap();
        if e.is_none() {
            *e = Some(error);
        }
        self.health.failed.store(true, Ordering::Release);
    }
    pub fn error(&self) -> Option<String> {
        self.health.error.lock().unwrap().clone()
    }
    pub fn high_water(&self) -> u32 {
        self.health.high.load(Ordering::Relaxed) as u32
    }
}

use canharbor_platform::process::process_usage;
