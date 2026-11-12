// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded, lossless outcome ranges, separate from sampled RX evidence.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionOutcome {
    Submitted,
    Failed,
    Unknown,
    Skipped,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionRange {
    pub first: u32,
    pub count: u32,
    pub outcome: SubmissionOutcome,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressSubmissionEvidence")]
pub struct SubmissionSummary {
    pub covered: u32,
    pub submitted: u64,
    pub failed: u64,
    pub unknown: u64,
    pub skipped: u64,
    pub unknown_sequence: Option<u32>,
}
impl SubmissionSummary {
    pub(crate) fn add(&mut self, first: u32, count: u32, outcome: SubmissionOutcome) {
        self.covered += count;
        match outcome {
            SubmissionOutcome::Submitted => self.submitted += u64::from(count),
            SubmissionOutcome::Failed => self.failed += u64::from(count),
            SubmissionOutcome::Unknown => {
                self.unknown += u64::from(count);
                self.unknown_sequence = Some(first);
            }
            SubmissionOutcome::Skipped => self.skipped += u64::from(count),
        }
    }
    pub fn attempted(&self) -> u64 {
        self.submitted + self.failed + self.unknown
    }
}
pub(crate) struct OutcomeJournal {
    pub pattern: ClassicPatternTransmit,
    pending: Vec<SubmissionRange>,
    sink: Option<EvidenceSink>,
    pub summary: SubmissionSummary,
}
impl OutcomeJournal {
    pub fn new(sink: Option<EvidenceSink>) -> Self {
        Self {
            pattern: Default::default(),
            pending: Vec::with_capacity(32),
            sink,
            summary: Default::default(),
        }
    }
    pub fn record(&mut self, first: u32, count: u32, outcome: SubmissionOutcome) {
        self.summary.add(first, count, outcome);
        self.pattern.add(first, count, outcome);
        if let Some(last) = self.pending.last_mut()
            && last.outcome == outcome
            && u64::from(last.first) + u64::from(last.count) == u64::from(first)
        {
            last.count += count;
            return;
        }
        if self.pending.len() == 32 {
            self.flush();
        }
        self.pending.push(SubmissionRange {
            first,
            count,
            outcome,
        });
    }
    pub fn flush(&mut self) {
        if !self.pending.is_empty() {
            let ranges = std::mem::replace(&mut self.pending, Vec::with_capacity(32));
            if let Some(sink) = &self.sink {
                sink.emit(EvidenceEvent::Submissions { ranges });
            }
        }
    }
}
