// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressTermination")]
pub struct Termination {
    pub code: String,
    pub detail: String,
    pub channel: Option<String>,
    pub sequence: Option<u32>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressLifecycle")]
pub struct Lifecycle {
    pub revision: u64,
    pub stop_requested_ns: Option<u64>,
    pub last_attempt_ns: Option<u64>,
    pub last_attempt_sequence: Option<u32>,
    pub last_submitted_ns: Option<u64>,
    pub inflight_sequence: Option<u32>,
    pub execution_end_ns: Option<u64>,
    pub termination: Option<Termination>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressSegmentRecovery")]
pub struct SegmentRecovery {
    pub name: String,
    pub bytes: u64,
    pub valid_prefix_bytes: u64,
    pub records: u64,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressRecovery")]
pub struct Recovery {
    pub status: String,
    pub records: u64,
    pub valid_prefix_bytes: u64,
    pub files: u32,
    pub damaged_files: u32,
    pub segments: Vec<SegmentRecovery>,
    pub segments_omitted: u32,
    pub counters_are_lower_bounds: bool,
    pub sample_receivers: Vec<ReceiverReport>,
    pub sample_coverage: String,
    pub audit_records: u64,
    #[serde(default)]
    pub submission_coverage: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressRecoveryIssue")]
pub struct RecoveryIssue {
    pub id: String,
    pub detail: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressLifecycleRecord")]
pub struct LifecycleRecord {
    pub revision: u64,
    pub elapsed_ns: u64,
    pub state: String,
    pub attempted: u64,
    pub submitted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub unknown: u64,
    pub lifecycle: Lifecycle,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressEvents")]
pub struct Events {
    pub run_id: String,
    pub latest_revision: u64,
    pub gap: bool,
    pub events: Vec<LifecycleRecord>,
}
impl Run {
    pub fn elapsed_ns(&self) -> u64 {
        self.started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
    }
    pub(crate) fn journal(r: &mut Report, sink: &Option<EvidenceSink>, elapsed_ns: u64) {
        r.lifecycle.revision += 1;
        if r.lifecycle_history.len() == 64 {
            r.lifecycle_history.remove(0);
        }
        r.lifecycle_history.push(LifecycleRecord {
            revision: r.lifecycle.revision,
            elapsed_ns,
            state: r.state.clone(),
            attempted: r.attempted,
            submitted: r.submitted,
            skipped: r.skipped,
            failed: r.failed,
            unknown: r.unknown,
            lifecycle: r.lifecycle.clone(),
        });
        if let Some(sink) = sink {
            sink.emit(EvidenceEvent::Lifecycle {
                elapsed_ns,
                state: r.state.clone(),
                attempted: r.attempted,
                submitted: r.submitted,
                skipped: r.skipped,
                failed: r.failed,
                unknown: r.unknown,
                unknown_sequence: r.unknown_sequence,
                lifecycle: r.lifecycle.clone(),
            });
        }
    }
    pub fn transition(&self, state: &str) {
        let mut r = self.report.lock().unwrap();
        if matches!(
            r.state.as_str(),
            "completed" | "failed" | "aborted" | "interrupted"
        ) || r.state.starts_with("saving_")
        {
            return;
        }
        if r.lifecycle.stop_requested_ns.is_some()
            && matches!(
                state,
                "arming" | "reserved" | "warming" | "running" | "draining"
            )
        {
            return;
        }
        let state = if r.lifecycle.stop_requested_ns.is_some()
            && matches!(state, "completed" | "saving_completed")
        {
            r.measurement_complete = false;
            if r.verdict == "pass" {
                r.verdict = "inconclusive".into();
            }
            if state.starts_with("saving_") {
                "saving_aborted"
            } else {
                "aborted"
            }
        } else {
            state
        };
        if r.state == state && r.lifecycle.revision > 0 {
            return;
        }
        if matches!(
            state,
            "aborted" | "failed" | "interrupted" | "saving_aborted" | "saving_failed"
        ) && r.verdict == "pass"
        {
            r.verdict = "inconclusive".into();
        }
        r.state = state.into();
        Self::journal(&mut r, &self.sink, self.elapsed_ns());
    }
    pub fn terminate_if_unset(
        &self,
        code: &str,
        detail: &str,
        channel: Option<String>,
        sequence: Option<u32>,
    ) {
        let mut r = self.report.lock().unwrap();
        if r.lifecycle.termination.is_none() {
            r.lifecycle.termination = Some(Termination {
                code: code.into(),
                detail: detail.into(),
                channel,
                sequence,
            });
        }
    }
    pub fn terminate(
        &self,
        code: &str,
        detail: impl Into<String>,
        channel: Option<String>,
        sequence: Option<u32>,
    ) {
        let mut detail = detail.into();
        if detail.len() > 512 {
            let mut end = 512;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
        }
        self.report.lock().unwrap().lifecycle.termination = Some(Termination {
            code: code.into(),
            detail,
            channel,
            sequence,
        });
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressCleanupObservation")]
pub struct CleanupObservation {
    pub channel: String,
    pub backend_unconfirmed: Option<u64>,
    pub failures: Vec<String>,
    pub scope: String,
}
