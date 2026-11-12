// SPDX-License-Identifier: AGPL-3.0-or-later
//! Classic pattern-v1: even physical slots are checkpoints, odd slots are data.
//! Correlation uses received checkpoints only, never an inferred ordinal.
use super::*;
use std::collections::{BTreeMap, VecDeque};
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressClassicPatternReceiver")]
pub struct ClassicPatternReceiver {
    pub checkpoints_unique: u64,
    pub data_verified_unique: u64,
    pub missing_checkpoints: u64,
    pub unverified_data: u64,
    pub ambiguous_data: u64,
    pub invalid_envelopes: u64,
    pub checkpoint_evictions: u64,
    pub pending_data: u32,
    pub window_high_water: u32,
    pub finalized: bool,
}
impl ClassicPatternReceiver {
    pub fn passed(&self) -> bool {
        self.finalized
            && self.missing_checkpoints == 0
            && self.unverified_data == 0
            && self.ambiguous_data == 0
            && self.invalid_envelopes == 0
            && self.pending_data == 0
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressClassicPatternTransmit")]
pub struct ClassicPatternTransmit {
    pub checkpoint_submitted: u64,
    pub data_submitted: u64,
    pub checkpoint_skipped: u64,
    pub data_skipped: u64,
    pub checkpoint_failed: u64,
    pub data_failed: u64,
    pub checkpoint_unknown: u64,
    pub data_unknown: u64,
}
impl ClassicPatternTransmit {
    pub(crate) fn add(&mut self, first: u32, count: u32, outcome: SubmissionOutcome) {
        let checkpoints =
            u64::from(count / 2 + u32::from(count % 2 == 1 && first.is_multiple_of(2)));
        let data = u64::from(count) - checkpoints;
        match outcome {
            SubmissionOutcome::Submitted => {
                self.checkpoint_submitted += checkpoints;
                self.data_submitted += data;
            }
            SubmissionOutcome::Skipped => {
                self.checkpoint_skipped += checkpoints;
                self.data_skipped += data;
            }
            SubmissionOutcome::Failed => {
                self.checkpoint_failed += checkpoints;
                self.data_failed += data;
            }
            SubmissionOutcome::Unknown => {
                self.checkpoint_unknown += checkpoints;
                self.data_unknown += data;
            }
        }
    }
}
impl Plan {
    pub fn has_classic_pattern(&self) -> bool {
        self.traffic.is_pattern() || self.flows.iter().any(|f| f.traffic.is_pattern())
    }
    pub fn pattern_id_matches(&self, frame: &Frame) -> bool {
        self.traffic.checkpoint_id().is_some_and(|id| {
            frame.id().extended() == self.extended
                && (frame.id().value() == self.can_id || frame.id().value() == id)
        }) && matches!(frame, Frame::ClassicData { .. } | Frame::FdData { .. })
    }
    pub fn pattern_bytes(&self, ordinal: u32) -> [u8; 8] {
        let Traffic::ClassicPatternV1 { seed, stream, .. } = self.traffic else {
            unreachable!()
        };
        // A bijection of u64 makes payload collisions impossible within this u32 stream.
        // SplitMix64 finalizer, with a frozen stream/run salt; no authentication claim.
        let mut x = u64::from(ordinal)
            ^ (u64::from(seed) << 32 | u64::from(self.run_tag))
            ^ (u64::from(stream) << 48);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        (x ^ (x >> 31)).to_be_bytes()
    }
}
#[derive(Clone)]
pub(crate) struct PatternTracker {
    checkpoints: BTreeMap<u32, [u8; 8]>,
    order: VecDeque<u32>,
    pending: VecDeque<(Vec<u8>, u64)>,
    limit: usize,
}
impl PatternTracker {
    pub fn new(limit: usize) -> Self {
        Self {
            checkpoints: BTreeMap::new(),
            order: VecDeque::new(),
            pending: VecDeque::new(),
            limit,
        }
    }
    fn retire(&self, r: &mut ClassicPatternReceiver) {
        r.unverified_data += 1;
        if self.checkpoints.len() > 1 {
            r.ambiguous_data += 1;
        }
    }
    pub fn event(
        &mut self,
        p: &Plan,
        frame: &Frame,
        at: u64,
        repeated_checkpoint: bool,
        r: &mut ClassicPatternReceiver,
    ) -> Vec<(u32, u64)> {
        let mut accepted = vec![];
        if !matches!(frame, Frame::ClassicData { .. }) || frame.data().len() != 8 {
            r.invalid_envelopes += 1;
            return accepted;
        }
        if frame.id().value() == p.traffic.checkpoint_id().unwrap() {
            let Some(slot) = p.correlate(frame, false) else {
                return accepted;
            };
            if slot >= p.count {
                return vec![(slot, at)];
            }
            accepted.push((slot, at));
            if !repeated_checkpoint {
                if self.order.len() == self.limit {
                    self.checkpoints.remove(&self.order.pop_front().unwrap());
                    r.checkpoint_evictions += 1;
                }
                let ordinal = slot / 2;
                self.order.push_back(ordinal);
                self.checkpoints.insert(ordinal, p.pattern_bytes(ordinal));
                let mut keep = VecDeque::new();
                while let Some((data, at)) = self.pending.pop_front() {
                    if data.as_slice() == p.pattern_bytes(ordinal) {
                        accepted.push((slot + 1, at));
                    } else {
                        keep.push_back((data, at));
                    }
                }
                self.pending = keep;
            }
        } else {
            let matches = self
                .checkpoints
                .iter()
                .filter(|(_, data)| data.as_slice() == frame.data())
                .map(|(n, _)| *n)
                .collect::<Vec<_>>();
            if matches.len() == 1 {
                accepted.push((matches[0] * 2 + 1, at));
            } else if matches.len() > 1 {
                r.unverified_data += 1;
                r.ambiguous_data += 1;
            } else {
                if self.pending.len() == self.limit {
                    self.pending.pop_front();
                    self.retire(r);
                }
                self.pending.push_back((frame.data().to_vec(), at));
            }
        }
        r.pending_data = self.pending.len() as u32;
        r.window_high_water = r
            .window_high_water
            .max(self.pending.len().max(self.order.len()) as u32);
        accepted
    }
    pub fn finish(&mut self, count: u32, r: &mut ClassicPatternReceiver) {
        while self.pending.pop_front().is_some() {
            self.retire(r);
        }
        r.pending_data = 0;
        r.missing_checkpoints = u64::from(count / 2).saturating_sub(r.checkpoints_unique);
        r.finalized = true;
    }
}
impl Report {
    pub fn classic_pattern_consistent(&self) -> bool {
        if !self.plan.flows.is_empty()
            || self.plan.adaptive.is_some()
            || !self.plan.traffic.is_pattern()
        {
            return self.classic_pattern_transmit.is_none();
        }
        let Some(t) = &self.classic_pattern_transmit else {
            return false;
        };
        t.checkpoint_submitted.checked_add(t.data_submitted) == Some(self.submitted)
            && t.checkpoint_failed.checked_add(t.data_failed) == Some(self.failed)
            && t.checkpoint_unknown.checked_add(t.data_unknown) == Some(self.unknown)
            && t.checkpoint_skipped.checked_add(t.data_skipped) == Some(self.skipped)
            && [
                [
                    t.checkpoint_submitted,
                    t.checkpoint_failed,
                    t.checkpoint_unknown,
                    t.checkpoint_skipped,
                ],
                [
                    t.data_submitted,
                    t.data_failed,
                    t.data_unknown,
                    t.data_skipped,
                ],
            ]
            .into_iter()
            .all(|counts| {
                counts.into_iter().map(u128::from).sum::<u128>() <= u128::from(self.planned / 2)
            })
            && self.receivers.iter().all(|r| {
                r.classic_pattern.as_ref().is_some_and(|v| {
                    v.checkpoints_unique <= u64::from(self.planned / 2)
                        && v.data_verified_unique <= u64::from(self.planned / 2)
                        && v.checkpoints_unique + v.data_verified_unique == r.unique
                        && (!v.finalized
                            || v.missing_checkpoints
                                == u64::from(self.planned / 2) - v.checkpoints_unique)
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan(window: u16) -> Plan {
        serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":12,"fps":20,"bitrate":500000,"receivers_active":true,"drain_ms":20,"evidence":{"sample_every":1,"max_bytes":268435456,"segment_bytes":4194304},"traffic":{"format":"classic_pattern_v1","checkpoint_can_id":292,"seed":42,"stream":7,"max_pending":window}})).unwrap()
    }
    fn receive(v: &mut Verifier, p: &Plan, frame: Frame) {
        v.event(
            p,
            Notice::Frame(canharbor_core::runtime::Event {
                sequence: 0,
                elapsed_ns: 10,
                frame,
                direction: "rx",
                metadata: Default::default(),
            }),
        );
    }
    #[test]
    fn golden_pattern_and_checkpoint_keep_sequence_v1_wire_and_fingerprint_compatible() {
        let p = plan(4);
        assert_eq!(p.frame(0).data(), &[0, 0, 0, 17, 0, 0, 0, 0]);
        assert_eq!(p.frame(0).id().value(), 292);
        assert_eq!(
            p.frame(1).data(),
            &[0xc3, 0xe5, 0xdc, 0x09, 0x49, 0xaa, 0xdf, 0x8a]
        );
        assert_eq!(p.frame(1).id().value(), 291);
        let mut legacy = p;
        legacy.traffic = Traffic::Classic;
        let mut value = serde_json::to_value(&legacy).unwrap();
        assert!(value.get("traffic").is_none());
        let hash = legacy.fingerprint();
        value["traffic"] = serde_json::json!({"format":"classic"});
        let explicit: Plan = serde_json::from_value(value).unwrap();
        assert_eq!(hash, explicit.fingerprint());
        assert_eq!(explicit.frame(1).data(), &[0, 0, 0, 17, 0, 0, 0, 1]);
    }
    #[test]
    fn data_before_checkpoint_correlates_within_window_and_duplicates_stay_separate() {
        let p = plan(2);
        let mut v = Verifier::new("virtual:1".into(), p.count);
        receive(&mut v, &p, p.frame(1));
        assert_eq!(v.report.unique, 0);
        assert_eq!(v.report.classic_pattern.as_ref().unwrap().pending_data, 1);
        receive(&mut v, &p, p.frame(0));
        assert_eq!(v.report.unique, 2);
        receive(&mut v, &p, p.frame(1));
        receive(&mut v, &p, p.frame(0));
        assert_eq!(v.report.duplicates, 2);
        assert_eq!(v.report.unique, 2);
        v.finish_pattern(&p);
        let r = v.report.classic_pattern.unwrap();
        assert_eq!(r.data_verified_unique, 1);
        assert_eq!(r.unverified_data, 0);
        assert_eq!(r.missing_checkpoints, 5);
    }
    #[test]
    fn lost_checkpoint_eviction_and_late_control_never_rewrite_retired_data() {
        let p = plan(1);
        let mut v = Verifier::new("virtual:1".into(), p.count);
        receive(&mut v, &p, p.frame(1));
        receive(&mut v, &p, p.frame(3)); // first orphan is retired
        receive(&mut v, &p, p.frame(0)); // cannot resurrect retired data
        assert_eq!(v.report.unique, 1);
        receive(&mut v, &p, p.frame(2));
        assert_eq!(v.report.unique, 3);
        receive(&mut v, &p, p.frame(0));
        receive(&mut v, &p, p.frame(1)); // old duplicate checkpoint cannot reopen its window
        v.finish_pattern(&p);
        let r = v.report.classic_pattern.unwrap();
        assert_eq!(r.data_verified_unique, 1);
        assert_eq!(r.unverified_data, 2);
        assert_eq!(r.checkpoint_evictions, 1);
        assert!(r.finalized);
        assert!(!r.passed());
    }
    #[test]
    fn corrupted_data_with_multiple_candidates_is_ambiguous_not_a_guessed_sequence() {
        let p = plan(4);
        let mut v = Verifier::new("virtual:1".into(), p.count);
        receive(&mut v, &p, p.frame(0));
        receive(&mut v, &p, p.frame(2));
        let mut d = p.frame(1).data().to_vec();
        d[7] ^= 1;
        receive(
            &mut v,
            &p,
            Frame::classic(CanId::new(291, false).unwrap(), d).unwrap(),
        );
        v.finish_pattern(&p);
        assert_eq!(v.report.unique, 2);
        let r = v.report.classic_pattern.unwrap();
        assert_eq!(r.data_verified_unique, 0);
        assert_eq!(r.ambiguous_data, 1);
        assert_eq!(r.unverified_data, 1);
        assert!(!r.passed());
    }
    #[test]
    fn malformed_envelope_and_out_of_range_control_cannot_verify_data() {
        let p = plan(4);
        let mut v = Verifier::new("virtual:1".into(), p.count);
        receive(
            &mut v,
            &p,
            Frame::fd(
                CanId::new(291, false).unwrap(),
                p.frame(1).data().to_vec(),
                false,
            )
            .unwrap(),
        );
        let mut d = p.run_tag.to_be_bytes().to_vec();
        d.extend(u32::MAX.to_be_bytes());
        receive(
            &mut v,
            &p,
            Frame::classic(CanId::new(292, false).unwrap(), d).unwrap(),
        );
        assert_eq!(v.report.out_of_range, 1);
        assert_eq!(v.report.classic_pattern.unwrap().invalid_envelopes, 1);
        assert_eq!(v.report.unique, 0);
    }
    #[test]
    fn every_payload_bit_is_checked_and_orphan_storage_stays_bounded() {
        let p = plan(4);
        for byte in 0..8 {
            for bit in 0..8 {
                let mut v = Verifier::new("virtual:1".into(), p.count);
                receive(&mut v, &p, p.frame(0));
                let mut data = p.frame(1).data().to_vec();
                data[byte] ^= 1 << bit;
                receive(
                    &mut v,
                    &p,
                    Frame::classic(CanId::new(291, false).unwrap(), data).unwrap(),
                );
                v.finish_pattern(&p);
                let r = v.report.classic_pattern.unwrap();
                assert_eq!(r.data_verified_unique, 0);
                assert_eq!(r.unverified_data, 1);
            }
        }
        let mut v = Verifier::new("virtual:1".into(), p.count);
        for _ in 0..10_000 {
            receive(&mut v, &p, p.frame(1));
        }
        assert_eq!(v.report.classic_pattern.as_ref().unwrap().pending_data, 4);
        assert_eq!(v.pattern.as_ref().unwrap().pending.len(), 4);
        v.finish_pattern(&p);
        let r = v.report.classic_pattern.unwrap();
        assert_eq!(r.unverified_data, 10_000);
        assert_eq!(r.window_high_water, 4);
    }
    #[test]
    fn physical_skip_partition_does_not_round_odd_ranges_or_overflow_on_untrusted_totals() {
        let mut t = ClassicPatternTransmit::default();
        t.add(1, 3, SubmissionOutcome::Skipped);
        assert_eq!(t.checkpoint_skipped, 1);
        assert_eq!(t.data_skipped, 2);
        let mut r = Report::initial(&plan(4), true, "test".into()).unwrap();
        r.classic_pattern_transmit = Some(ClassicPatternTransmit {
            checkpoint_submitted: u64::MAX,
            checkpoint_failed: u64::MAX,
            ..Default::default()
        });
        r.submitted = u64::MAX;
        r.failed = u64::MAX;
        assert!(!r.classic_pattern_consistent());
    }
}
