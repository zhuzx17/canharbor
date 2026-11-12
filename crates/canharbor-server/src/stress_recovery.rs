// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded NDJSON valid-prefix scans; never writes or truncates source segments.
use super::stress_store::{self, Identity};
use canharbor_stress::{EvidenceEvent, Recovery, Replay, SegmentRecovery};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader},
    path::Path,
};
#[derive(Serialize, Deserialize)]
pub(super) struct Scan {
    pub summary: Recovery,
    pub segments: Vec<SegmentRecovery>,
}
fn segment(name: &str) -> Option<u32> {
    let tail = name
        .strip_prefix("metrics-")
        .or_else(|| name.strip_prefix("samples-"))
        .or_else(|| name.strip_prefix("events-"))
        .or_else(|| name.strip_prefix("rtt-"))
        .or_else(|| name.strip_prefix("submissions-"))?
        .strip_suffix(".ndjson")?;
    if tail.len() != 6 || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    tail.parse().ok()
}
pub(super) fn scan(path: &Path, identity: &Identity) -> Result<(Scan, Replay), String> {
    let mut paths = vec![];
    let entries = std::fs::read_dir(path)
        .map_err(|e| e.to_string())?
        .take(1025)
        .collect::<Vec<_>>();
    if entries.len() > 1024 {
        return Err("evidence directory exceeds inventory bound".into());
    }
    let mut broken_streams = std::collections::HashSet::new();
    let mut metadata_bytes = 0u64;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(index) = segment(&name) {
            paths.push((index, name));
        } else {
            let orphan = name
                .strip_suffix(".tmp")
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok());
            if !orphan
                && !matches!(
                    name.as_str(),
                    "identity.json"
                        | "checkpoint.json"
                        | "report.json"
                        | "manifest.json"
                        | "recovery.json"
                        | "recovered.json"
                )
            {
                return Err("unrecognized evidence artifact; registry quarantined".into());
            }
            let f =
                canharbor_platform::private::open_read(&entry.path()).map_err(|e| e.to_string())?;
            let m = f.metadata().map_err(|e| e.to_string())?;
            if m.len() > 1024 * 1024 {
                return Err("invalid private evidence metadata".into());
            }
            metadata_bytes += m.len();
            if metadata_bytes > 4 * 1024 * 1024 {
                return Err("metadata reservation exceeded".into());
            }
        }
    }
    if paths.len() > 1000 {
        return Err("evidence scan exceeds segment bound".into());
    }
    paths.sort();
    let mut replay = Replay::new(&identity.report.plan)?;
    let mut segments = vec![];
    let mut total = 0u64;
    let mut records = 0u64;
    let mut valid = 0u64;
    let mut seen_indices = std::collections::HashSet::new();
    let mut expected = 0;
    let mut gap = false;
    for (index, name) in paths {
        if index != expected && !gap {
            gap = true;
            segments.push(SegmentRecovery {
                name: format!("missing-{expected:06}"),
                error: Some(
                    "global segment ordinal missing; later streams are not a valid prefix".into(),
                ),
                ..Default::default()
            });
        }
        expected = index + 1;
        if !seen_indices.insert(index) {
            return Err("duplicate global segment ordinal".into());
        }
        let file = stress_store::open(path, &name)?;
        let bytes = file.metadata().map_err(|e| e.to_string())?.len();
        total = total
            .checked_add(bytes)
            .ok_or("evidence byte sum overflow")?;
        if bytes > 16 * 1024 * 1024 || total > identity.report.plan.evidence.max_bytes {
            return Err("evidence scan exceeds byte budget".into());
        }
        let mut reader = BufReader::new(file);
        let mut row = Vec::with_capacity(8192);
        let mut item = SegmentRecovery {
            name: name.clone(),
            bytes,
            ..Default::default()
        };
        let kind = name.split_once('-').unwrap().0.to_owned();
        if gap || broken_streams.contains(&kind) {
            item.error = Some("earlier segment in this stream is damaged".into());
        }
        loop {
            if item.error.is_some() {
                break;
            }
            row.clear();
            loop {
                let available = reader.fill_buf().map_err(|e| e.to_string())?;
                if available.is_empty() {
                    break;
                }
                let end = available
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(available.len(), |n| n + 1);
                if row.len() + end > 65536 {
                    item.error = Some("record exceeds 64 KiB bound".into());
                    break;
                }
                row.extend_from_slice(&available[..end]);
                reader.consume(end);
                if row.last() == Some(&b'\n') {
                    break;
                }
            }
            if item.error.is_some() || row.is_empty() {
                break;
            }
            if item.valid_prefix_bytes + row.len() as u64 > bytes {
                item.error = Some("segment grew beyond observed scan boundary".into());
                break;
            }
            if row.last() != Some(&b'\n') {
                item.error = Some("torn final record; original bytes retained".into());
                break;
            }
            let parsed = serde_json::from_slice::<EvidenceEvent>(&row)
                .map_err(|e| e.to_string())
                .and_then(|event| {
                    let compatible = match &event {
                        EvidenceEvent::Submissions { .. } => name.starts_with("submissions-"),
                        EvidenceEvent::TrackingRecord { .. }
                        | EvidenceEvent::TrackingAdvance { .. }
                        | EvidenceEvent::QueueSample { .. }
                        | EvidenceEvent::LoadWindow(_)
                        | EvidenceEvent::AdaptiveDecision(_)
                        | EvidenceEvent::GroupRelease { .. }
                        | EvidenceEvent::FlowRecord { .. }
                        | EvidenceEvent::FlowEnd { .. }
                        | EvidenceEvent::FlowStop { .. }
                        | EvidenceEvent::Metric(_)
                        | EvidenceEvent::Timing(_)
                        | EvidenceEvent::Provenance(_)
                        | EvidenceEvent::Diagnostic { .. }
                        | EvidenceEvent::SchedulerSkip { .. } => name.starts_with("metrics-"),
                        EvidenceEvent::Sample { .. } | EvidenceEvent::PatternFrame { .. } => {
                            name.starts_with("samples-")
                        }
                        EvidenceEvent::Lifecycle { .. } => name.starts_with("events-"),
                        EvidenceEvent::Rtt { .. } | EvidenceEvent::Reply { .. } => {
                            name.starts_with("rtt-")
                        }
                        EvidenceEvent::End(_) => false,
                    };
                    if !compatible {
                        return Err("record kind does not match segment".into());
                    }
                    replay.event(&event)
                });
            if let Err(error) = parsed {
                item.error = Some(error.chars().take(256).collect());
                break;
            }
            item.records += 1;
            item.valid_prefix_bytes += row.len() as u64;
        }
        if item.error.is_none() && item.valid_prefix_bytes != bytes {
            item.error = Some("segment truncated during scan".into());
        }
        if item.error.is_some() {
            broken_streams.insert(kind);
        }
        records += item.records;
        valid += item.valid_prefix_bytes;
        segments.push(item);
    }
    let damaged = segments.iter().filter(|s| s.error.is_some()).count() as u32;
    let summary = Recovery {
        status: if damaged > 0 {
            "partial_prefix"
        } else {
            "scanned"
        }
        .into(),
        records,
        valid_prefix_bytes: valid,
        files: segments.len() as u32,
        damaged_files: damaged,
        segments: segments.iter().take(64).cloned().collect(),
        segments_omitted: segments.len().saturating_sub(64) as u32,
        counters_are_lower_bounds: true,
        sample_receivers: replay.sample_receivers(),
        sample_coverage: if identity.report.plan.evidence.sample_every == 1 {
            "all_matching_rx_rows"
        } else if identity.report.plan.evidence.sample_every == 0 {
            "disabled"
        } else {
            "sampled_rows_only"
        }
        .into(),
        audit_records: replay.audit_records,
        submission_coverage: if identity.report.version >= 5 {
            "range_v1"
        } else {
            "legacy_aggregate_only"
        }
        .into(),
    };
    Ok((Scan { summary, segments }, replay))
}
