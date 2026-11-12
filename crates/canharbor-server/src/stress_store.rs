// SPDX-License-Identifier: AGPL-3.0-or-later
//! Private, quota-bound segmented evidence. No caller-supplied filesystem paths.
use canharbor_stress::{EvidenceEvent, EvidenceMonitor, Report};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Identity {
    pub owner: String,
    pub request: String,
    pub report: Report,
}
pub use canharbor_stress::Artifact;
#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    complete: bool,
    files: Vec<Artifact>,
    software: String,
    measurement: String,
    plan: canharbor_stress::Plan,
    plan_hash: String,
    source_revision: Option<String>,
    driver_binary_sha256: Option<String>,
}
pub(super) fn directory(root: &Path) -> Result<(), String> {
    canharbor_platform::private::directory(root).map_err(|e| e.to_string())
}
pub(super) fn open(root: &Path, name: &str) -> Result<File, String> {
    if !valid_name(name) {
        return Err("invalid evidence filename".into());
    }
    canharbor_platform::private::open_read(&root.join(name)).map_err(|e| e.to_string())
}
pub(super) fn valid_name(name: &str) -> bool {
    matches!(
        name,
        "report.json"
            | "manifest.json"
            | "identity.json"
            | "checkpoint.json"
            | "recovery.json"
            | "recovered.json"
    ) || ((name.starts_with("metrics-")
        || name.starts_with("samples-")
        || name.starts_with("events-")
        || name.starts_with("rtt-")
        || name.starts_with("submissions-"))
        && name.ends_with(".ndjson")
        && name.len() < 40
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'))
}
fn create(root: &Path, name: &str) -> Result<File, String> {
    canharbor_platform::private::create_new(&root.join(name)).map_err(|e| e.to_string())
}
pub(super) fn atomic(root: &Path, name: &str, value: &impl Serialize) -> Result<(), String> {
    let tmp = format!("{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = create(root, &tmp)?;
        serde_json::to_writer(&mut file, value).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        canharbor_platform::private::replace(&root.join(&tmp), &root.join(name))
            .map_err(|e| e.to_string())?;
        canharbor_platform::private::sync_directory(root).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(root.join(tmp));
    }
    result
}
fn digest(root: &Path, name: &str) -> Result<Artifact, String> {
    let mut f = open(root, name)?;
    let bytes = f.metadata().map_err(|e| e.to_string())?.len();
    if bytes > 16 * 1024 * 1024 {
        return Err("artifact exceeds segment bound".into());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut read = 0u64;
    loop {
        let n = f.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > bytes {
            return Err("artifact changed during hashing".into());
        }
        hasher.update(&buffer[..n]);
    }
    if read != bytes {
        return Err("artifact truncated during hashing".into());
    }
    Ok(Artifact {
        name: name.into(),
        bytes,
        sha256: hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    })
}
pub(super) struct RecoveryBatch {
    pub runs: Vec<Identity>,
    pub issues: Vec<canharbor_stress::RecoveryIssue>,
}
#[cfg(test)]
pub(super) fn recover(root: &Path) -> Result<Vec<Identity>, String> {
    Ok(recover_registry(root)?.runs)
}
fn json<T: serde::de::DeserializeOwned>(root: &Path, name: &str) -> Result<T, String> {
    let file = open(root, name)?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 1024 * 1024 {
        return Err("metadata exceeds 1 MiB".into());
    }
    serde_json::from_reader(file).map_err(|e| e.to_string())
}
pub(super) fn recover_registry(root: &Path) -> Result<RecoveryBatch, String> {
    if let Some(parent) = root.parent() {
        directory(parent)?;
    }
    directory(root)?;
    let inventory = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .take(65)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if inventory.len() > 64 {
        return Err("evidence root inventory exceeds bound".into());
    }
    let mut batch = RecoveryBatch {
        runs: vec![],
        issues: vec![],
    };
    for entry in inventory {
        let id = entry.file_name().to_string_lossy().to_string();
        if uuid::Uuid::parse_str(&id).is_err() {
            continue;
        }
        if batch.runs.len() + batch.issues.len() >= 32 {
            return Err("evidence registry exceeds 32 runs".into());
        }
        match recover_one(&entry.path(), &id) {
            Ok(identity) => batch.runs.push(identity),
            Err(error) => batch.issues.push(canharbor_stress::RecoveryIssue {
                id,
                detail: error.chars().take(512).collect(),
            }),
        }
    }
    Ok(batch)
}
fn verify_manifest(path: &Path, r: &Report) -> Result<(), String> {
    let manifest: Manifest = json(path, "manifest.json")?;
    if !manifest.complete
        || manifest.plan_hash != r.plan_hash
        || manifest.plan != r.plan
        || r.plan.fingerprint() != r.plan_hash
        || manifest.files.len() > 1024
        || !manifest.files.iter().any(|f| f.name == "report.json")
    {
        return Err("evidence manifest invalid or incomplete".into());
    }
    if manifest
        .files
        .iter()
        .map(|f| f.bytes)
        .try_fold(0u64, |a, b| a.checked_add(b))
        .is_none_or(|n| n > r.plan.evidence.max_bytes + 4 * 1024 * 1024)
    {
        return Err("manifest exceeds evidence budget".into());
    }
    let mut names = std::collections::HashSet::new();
    let actual = fs::read_dir(path)
        .map_err(|e| e.to_string())?
        .take(1025)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if actual.len() > 1024 {
        return Err("artifact inventory exceeds bound".into());
    }
    let originals = actual
        .iter()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            valid_name(n)
                && !matches!(
                    n.as_str(),
                    "identity.json" | "manifest.json" | "recovery.json" | "recovered.json"
                )
        })
        .collect::<std::collections::HashSet<_>>();
    if originals != manifest.files.iter().map(|f| f.name.clone()).collect() {
        return Err("manifest inventory mismatch".into());
    }
    for expected in manifest.files {
        if !names.insert(expected.name.clone())
            || expected.name == "identity.json"
            || expected.name == "recovered.json"
            || expected.name == "recovery.json"
        {
            return Err("invalid manifest member".into());
        }
        if open(path, &expected.name)?
            .metadata()
            .map_err(|e| e.to_string())?
            .len()
            != expected.bytes
        {
            return Err("artifact size mismatch".into());
        }
        if digest(path, &expected.name)?.sha256 != expected.sha256 {
            return Err("artifact digest mismatch".into());
        }
    }
    Ok(())
}
fn recover_one(path: &Path, id: &str) -> Result<Identity, String> {
    directory(path)?;
    let mut identity: Identity = json(path, "identity.json")?;
    if identity.report.id != id
        || identity.owner.is_empty()
        || identity.owner.len() > 256
        || !crate::dto::valid_client_request_id(&identity.request)
        || identity.report.plan_hash != identity.report.plan.fingerprint()
    {
        return Err("untrusted identity/plan; original files quarantined".into());
    }
    identity.report.plan.compile()?;
    // Rebuild the zero baseline from the admitted plan, never trust initial counters.
    let mut authoritative = Report::initial(
        &identity.report.plan,
        identity.report.simulated,
        id.to_owned(),
    )?;
    authoritative.version = identity.report.version;
    authoritative.provenance = identity.report.provenance.clone();
    let mut problems = vec![];
    let final_report = json::<Report>(path, "report.json");
    let mut finalized = false;
    match final_report {
        Ok(r)
            if r.id == id
                && r.plan == authoritative.plan
                && r.plan_hash == authoritative.plan_hash
                && r.version == authoritative.version
                && r.provenance == authoritative.provenance =>
        {
            identity.report = r;
            finalized = true;
        }
        Ok(_) => problems.push("final report identity mismatch".into()),
        Err(e) => {
            if path.join("report.json").exists() {
                problems.push(format!("final report unreadable: {e}"));
            }
        }
    }
    if !finalized && let Ok(r) = json::<Report>(path, "checkpoint.json") {
        if r.id == id && r.plan == authoritative.plan && r.plan_hash == authoritative.plan_hash {
            identity.report = r;
        } else {
            problems.push("checkpoint identity mismatch".into());
        }
    }
    let (mut scan, replay) = crate::stress_recovery::scan(path, &identity)?;
    let verified = if finalized && identity.report.evidence.complete {
        verify_manifest(path, &identity.report).and_then(|_| {
            if scan.summary.damaged_files > 0 {
                Err("stream has an invalid/torn prefix boundary".into())
            } else {
                replay.verify_report(&identity.report)
            }
        })
    } else {
        Err("service interrupted or evidence was not finalized".into())
    };
    if let Err(error) = verified {
        let original_state = identity.report.state.clone();
        identity.report = authoritative;
        identity.report.state = original_state;
        {
            if let Some(m) = replay.recovered_metric() {
                identity.report.elapsed_seconds = m.elapsed_ms as f64 / 1000.0;
                identity.report.attempted = m.attempted;
                identity.report.submitted = m.submitted;
                identity.report.skipped = m.skipped;
                identity.report.failed = m.failed;
                identity.report.unknown = m.unknown;
                identity.report.unknown_sequence = m.unknown_sequence;
                identity.report.lifecycle = m.lifecycle.clone();
                identity.report.stability = m.stability.clone();
                identity.report.max_dispatch_lateness_ns = m.max_dispatch_lateness_ns;
                if !m.receivers.is_empty() {
                    identity.report.receivers = m.receivers.clone();
                }
            }
            if !finalized {
                identity.report.state = "interrupted".into();
            }
        }
        if !matches!(
            identity.report.state.as_str(),
            "completed" | "failed" | "aborted" | "interrupted"
        ) {
            identity.report.state = "interrupted".into();
        }
        identity.report.lifecycle_history = replay.lifecycle_tail();
        if identity.report.version >= 5
            && identity.report.plan.flows.is_empty()
            && identity.report.plan.adaptive.is_none()
        {
            let q = replay.submission_summary();
            identity.report.attempted = q.attempted();
            identity.report.submitted = q.submitted;
            identity.report.failed = q.failed;
            identity.report.unknown = q.unknown;
            identity.report.unknown_sequence = q.unknown_sequence;
            identity.report.skipped = q.skipped;
            identity.report.submission_evidence = q;
            identity.report.classic_pattern_transmit = replay.pattern_transmit();
            identity.report.stages = replay.recovered_stages();
            identity.report.load = replay.recovered_load();
        }
        if identity.report.plan.adaptive.is_some() {
            identity.report.adaptive = replay.recovered_adaptive(id);
            canharbor_stress::restore_adaptive_totals(&mut identity.report);
        }
        if !identity.report.plan.flows.is_empty() {
            identity.report.group = replay.recovered_group(&identity.report.plan, id);
            if let Some(g) = &mut identity.report.group {
                if let Some(source) = &identity.report.provenance {
                    g.budget.bind_sources(source);
                }
                for f in &mut g.flows {
                    f.report.id = id.into();
                }
                identity.report.attempted = g
                    .flows
                    .iter()
                    .map(|f| {
                        f.report.attempted
                            + if f
                                .report
                                .plan
                                .rtt
                                .as_ref()
                                .is_some_and(|r| r.role == canharbor_stress::RttRole::Loopback)
                            {
                                f.report.rtt_report.as_ref().map_or(0, |r| {
                                    r.responder_submitted + r.responder_failed + r.responder_unknown
                                })
                            } else {
                                0
                            }
                    })
                    .sum();
                identity.report.submitted = g
                    .flows
                    .iter()
                    .map(|f| {
                        f.report.submitted
                            + if f
                                .report
                                .plan
                                .rtt
                                .as_ref()
                                .is_some_and(|r| r.role == canharbor_stress::RttRole::Loopback)
                            {
                                f.report
                                    .rtt_report
                                    .as_ref()
                                    .map_or(0, |r| r.responder_submitted)
                            } else {
                                0
                            }
                    })
                    .sum();
                identity.report.skipped = g
                    .flows
                    .iter()
                    .map(|f| {
                        f.report.skipped
                            * if f
                                .report
                                .plan
                                .rtt
                                .as_ref()
                                .is_some_and(|r| r.role == canharbor_stress::RttRole::Loopback)
                            {
                                2
                            } else {
                                1
                            }
                    })
                    .sum();
                identity.report.failed = g
                    .flows
                    .iter()
                    .map(|f| {
                        f.report.failed
                            + if f
                                .report
                                .plan
                                .rtt
                                .as_ref()
                                .is_some_and(|r| r.role == canharbor_stress::RttRole::Loopback)
                            {
                                f.report
                                    .rtt_report
                                    .as_ref()
                                    .map_or(0, |r| r.responder_failed)
                            } else {
                                0
                            }
                    })
                    .sum();
                identity.report.unknown = g
                    .flows
                    .iter()
                    .map(|f| {
                        f.report.unknown
                            + if f
                                .report
                                .plan
                                .rtt
                                .as_ref()
                                .is_some_and(|r| r.role == canharbor_stress::RttRole::Loopback)
                            {
                                f.report
                                    .rtt_report
                                    .as_ref()
                                    .map_or(0, |r| r.responder_unknown)
                            } else {
                                0
                            }
                    })
                    .sum();
            }
        }
        if identity.report.plan.flows.is_empty() && identity.report.plan.rtt.is_some() {
            identity.report.rtt_report = Some(replay.recovered_rtt());
        }
        identity.report.verdict = if identity.report.known_violation() {
            "fail"
        } else {
            "inconclusive"
        }
        .into();
        identity.report.timing = replay.recovered_timing();
        identity.report.assessment = None;
        identity.report.workload_achieved = false;
        identity.report.measurement_complete = false;
        identity.report.counters_complete = false;
        identity.report.evidence.complete = false;
        identity.report.evidence.error = Some(error.clone());
        problems.push(error);
        scan.summary.status = if scan.summary.damaged_files > 0 {
            "partial_prefix"
        } else if finalized {
            "unverified"
        } else {
            "recovered_prefix"
        }
        .into();
        scan.summary.counters_are_lower_bounds = true;
    } else {
        scan.summary.status = "verified".into();
        scan.summary.counters_are_lower_bounds = false;
    }
    identity.report.evidence.bytes = scan.segments.iter().map(|s| s.bytes).sum();
    for r in &mut scan.summary.sample_receivers {
        r.evidence_complete = scan.summary.status == "verified"
            && identity
                .report
                .receivers
                .iter()
                .any(|s| s.channel == r.channel && s.evidence_complete);
    }
    identity.report.recovery = scan.summary.clone();
    for error in problems {
        if identity.report.errors.len() < 32 && !identity.report.errors.contains(&error) {
            identity.report.errors.push(error);
        }
    }
    // Derived files never overwrite source report/checkpoint/segment bytes.
    atomic(path, "recovery.json", &scan)?;
    if identity.report.recovery.status != "verified" {
        atomic(path, "recovered.json", &identity.report)?;
    }
    Ok(identity)
}
pub(super) fn prepare(root: &Path, identity: &Identity) -> Result<PathBuf, String> {
    directory(root)?;
    if fs2::available_space(root).map_err(|e| e.to_string())?
        < identity.report.plan.evidence.max_bytes + 16 * 1024 * 1024
    {
        return Err("insufficient disk space for evidence reservation".into());
    }
    let path = root.join(&identity.report.id);
    canharbor_platform::private::create_directory(&path).map_err(|e| e.to_string())?;
    if let Err(e) = atomic(&path, "identity.json", identity) {
        let _ = fs::remove_dir_all(&path);
        return Err(e);
    }
    Ok(path)
}
pub(super) fn artifacts(root: &Path) -> Result<Vec<Artifact>, String> {
    directory(root)?;
    let mut files = vec![];
    for e in fs::read_dir(root).map_err(|e| e.to_string())?.take(1025) {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name().to_string_lossy().to_string();
        if name != "identity.json" && valid_name(&name) {
            if files.len() >= 1024 {
                return Err("too many evidence segments".into());
            }
            files.push(digest(root, &name)?);
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}
struct Segment {
    file: File,
    bytes: u64,
}
pub(super) fn write(
    path: PathBuf,
    identity: Identity,
    sink: EvidenceMonitor,
    rx: tokio::sync::mpsc::Receiver<EvidenceEvent>,
) -> Result<Report, String> {
    write_checked(path, identity, sink, rx, |_| Ok(()))
}
// Private per-writer hook: tests inject storage failures without changing the host disk.
fn write_checked(
    path: PathBuf,
    identity: Identity,
    sink: EvidenceMonitor,
    mut rx: tokio::sync::mpsc::Receiver<EvidenceEvent>,
    mut check: impl FnMut(&str) -> Result<(), String>,
) -> Result<Report, String> {
    let mut segment: Option<Segment> = None;
    let mut sample: Option<Segment> = None;
    let mut audit: Option<Segment> = None;
    let mut rtt: Option<Segment> = None;
    let mut submissions: Option<Segment> = None;
    let mut number = 0u32;
    let mut total = 0u64;
    let mut records = 0u64;
    let limit = identity.report.plan.evidence.max_bytes;
    let size = identity.report.plan.evidence.segment_bytes;
    let mut failed = None;
    let mut checkpoint = identity.report.clone();
    while let Some(event) = rx.blocking_recv() {
        if let EvidenceEvent::End(mut report) = event {
            for segment in [
                &mut segment,
                &mut sample,
                &mut audit,
                &mut rtt,
                &mut submissions,
            ]
            .into_iter()
            .flatten()
            {
                if let Err(e) = segment.file.sync_all() {
                    failed.get_or_insert(e.to_string());
                }
            }
            if let Err(e) = check("flush") {
                failed.get_or_insert(e);
            }
            if let Some(reason) = failed.or_else(|| sink.error()) {
                report.evidence.error = Some(reason.clone());
                report.measurement_complete = false;
                report.verdict = if report.known_violation() {
                    "fail"
                } else {
                    "inconclusive"
                }
                .into();
                report.errors.push(reason);
            }
            report.evidence.complete = report.evidence.error.is_none();
            report.evidence.bytes = total;
            report.evidence.records = records;
            report.evidence.queue_high_water = sink.high_water();
            if report.evidence.complete {
                let verification =
                    super::stress_recovery::scan(&path, &identity).and_then(|(scan, replay)| {
                        if scan.summary.damaged_files > 0 {
                            return Err("damaged evidence segment".into());
                        }
                        if report.provenance != identity.report.provenance {
                            return Err("source provenance differs from prepared identity".into());
                        }
                        replay.verify_report(&report)?;
                        report.recovery = scan.summary;
                        for r in &mut report.recovery.sample_receivers {
                            r.evidence_complete = report
                                .receivers
                                .iter()
                                .any(|s| s.channel == r.channel && s.evidence_complete);
                        }
                        report.recovery.status = "verified".into();
                        report.recovery.counters_are_lower_bounds = false;
                        Ok(())
                    });
                if let Err(reason) = verification {
                    report.evidence.error = Some(reason.clone());
                    report.evidence.complete = false;
                    report.measurement_complete = false;
                    report.verdict = if report.known_violation() {
                        "fail"
                    } else {
                        "inconclusive"
                    }
                    .into();
                    report.errors.push(reason);
                }
            }
            check("report")?;
            atomic(&path, "report.json", &*report)?;
            let files = artifacts(&path)?
                .into_iter()
                .filter(|f| {
                    !matches!(
                        f.name.as_str(),
                        "recovered.json" | "recovery.json" | "manifest.json"
                    )
                })
                .collect();
            check("manifest")?;
            atomic(
                &path,
                "manifest.json",
                &Manifest {
                    version: 2,
                    complete: report.evidence.complete,
                    files,
                    software: env!("CARGO_PKG_VERSION").into(),
                    measurement: if report.plan.has_classic_pattern() { "host observations; Classic pattern selected IDs require full raw evidence; not a physical bus capture" } else { "host sequence observation; samples are not a complete capture" }.into(),
                    plan: report.plan.clone(),
                    plan_hash: report.plan_hash.clone(),
                    source_revision: option_env!("CANHARBOR_SOURCE_REVISION").map(str::to_owned),
                    driver_binary_sha256: None,
                },
            )?;
            return Ok(*report);
        }
        if failed.is_some() {
            continue;
        }
        let result = (|| -> Result<(), String> {
            let is_sample = matches!(
                &event,
                EvidenceEvent::Sample { .. } | EvidenceEvent::PatternFrame { .. }
            );
            let is_audit = matches!(&event, EvidenceEvent::Lifecycle { .. });
            let is_rtt = matches!(
                &event,
                EvidenceEvent::Rtt { .. } | EvidenceEvent::Reply { .. }
            );
            let is_submissions = matches!(&event, EvidenceEvent::Submissions { .. });
            let slot = if is_submissions {
                &mut submissions
            } else if is_rtt {
                &mut rtt
            } else if is_sample {
                &mut sample
            } else if is_audit {
                &mut audit
            } else {
                &mut segment
            };
            let mut bytes = serde_json::to_vec(&event).map_err(|e| e.to_string())?;
            bytes.push(b'\n');
            if bytes.len() as u64 > size {
                return Err("evidence record exceeds segment byte limit".into());
            }
            if total + bytes.len() as u64 + 4 * 1024 * 1024 > limit {
                return Err("evidence byte quota exhausted".into());
            }
            if slot
                .as_ref()
                .is_none_or(|s| s.bytes + bytes.len() as u64 > size)
            {
                if let Some(previous) = slot.take() {
                    previous.file.sync_all().map_err(|e| e.to_string())?;
                }
                if number >= 1000 {
                    return Err("evidence segment count exhausted".into());
                }
                let name = format!(
                    "{}-{number:06}.ndjson",
                    if is_submissions {
                        "submissions"
                    } else if is_rtt {
                        "rtt"
                    } else if is_sample {
                        "samples"
                    } else if is_audit {
                        "events"
                    } else {
                        "metrics"
                    }
                );
                number += 1;
                *slot = Some(Segment {
                    file: create(&path, &name)?,
                    bytes: 0,
                });
            }
            let segment = slot.as_mut().unwrap();
            check("write")?;
            segment.file.write_all(&bytes).map_err(|e| e.to_string())?;
            segment.bytes += bytes.len() as u64;
            total += bytes.len() as u64;
            records += 1;
            let checkpoint_event = if let EvidenceEvent::TrackingRecord { event } = &event {
                event.as_ref()
            } else {
                &event
            };
            if let EvidenceEvent::Metric(metric) = checkpoint_event {
                segment.file.sync_data().map_err(|e| e.to_string())?;
                checkpoint.state = metric.state.clone();
                checkpoint.elapsed_seconds = metric.elapsed_ms as f64 / 1000.0;
                checkpoint.attempted = metric.attempted;
                checkpoint.submitted = metric.submitted;
                checkpoint.skipped = metric.skipped;
                checkpoint.failed = metric.failed;
                checkpoint.unknown = metric.unknown;
                checkpoint.stability = metric.stability.clone();
                checkpoint.lifecycle = metric.lifecycle.clone();
                checkpoint.unknown_sequence = metric.unknown_sequence;
                checkpoint.active_stage = metric.active_stage;
                checkpoint.receivers = metric.receivers.clone();
                checkpoint.evidence.bytes = total;
                checkpoint.evidence.records = records;
                checkpoint.counters_complete = false;
                checkpoint.measurement_complete = false;
                checkpoint.evidence.complete = false;
                if checkpoint.history.len() == 120 {
                    checkpoint.history.remove(0);
                }
                checkpoint.history.push(metric.clone());
                check("checkpoint")?;
                atomic(&path, "checkpoint.json", &checkpoint)?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            sink.fail(e.clone());
            failed = Some(e);
        }
    }
    Err("evidence writer ended without final report; files retained".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_stress::{EvidenceEvent, EvidenceSink, Plan};
    fn setup() -> (PathBuf, Identity) {
        let base = std::path::PathBuf::from(if cfg!(windows) {
            std::env::var_os("TEMP").expect("TEMP")
        } else {
            std::ffi::OsString::from("/tmp")
        })
        .join(format!("ch-p2-store-{}", uuid::Uuid::new_v4()));
        directory(&base).unwrap();
        let root = base.join("stress");
        directory(&root).unwrap();
        let plan:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":500,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
        let mut plan = plan;
        plan.evidence.sample_every = 1;
        let mut report = Report::initial(&plan, true, uuid::Uuid::new_v4().to_string()).unwrap();
        report.version = 6;
        (
            root,
            Identity {
                owner: "owner".into(),
                request: "request".into(),
                report,
            },
        )
    }
    #[test]
    fn segments_hashes_checkpoint_and_recovery_do_not_resume() {
        let (root, mut identity) = setup();
        identity.report.plan.evidence.segment_bytes = 4096;
        identity.report.plan_hash = identity.report.plan.fingerprint();
        let path = prepare(&root, &identity).unwrap();
        let (sink, rx) = EvidenceSink::channel(256);
        let observer = sink.monitor();
        let source = identity.clone();
        let writer = std::thread::spawn(move || write(path, source, observer, rx));
        for n in 0..500 {
            sink.blocking_record(EvidenceEvent::Sample {
                channel: "virtual:1".into(),
                sequence: n,
                wire: None,
                data_hex: format!("00000011{n:08x}"),
                elapsed_ns: n as u64,
            })
            .unwrap();
        }
        let mut report = identity.report.clone();
        report.state = "completed".into();
        report.verdict = "pass".into();
        report.measurement_complete = true;
        report.workload_achieved = true;
        sink.blocking_record(EvidenceEvent::Submissions {
            ranges: vec![canharbor_stress::SubmissionRange {
                first: 0,
                count: 500,
                outcome: canharbor_stress::SubmissionOutcome::Submitted,
            }],
        })
        .unwrap();
        report.submission_evidence = canharbor_stress::SubmissionSummary {
            covered: 500,
            submitted: 500,
            ..Default::default()
        };
        report.attempted = 500;
        report.submitted = 500;
        report.stages[0].submitted = 500;
        report.stages[0].received_unique = vec![500];
        report.receivers = vec![canharbor_stress::ReceiverReport {
            channel: "virtual:1".into(),
            ..Default::default()
        }];
        report.receivers[0].unique = 500;
        report.receivers[0].missing_expected = 0;
        report.receivers[0].evidence_complete = true;
        report.assessment = Some(report.assess());
        report.lifecycle.revision = 1;
        sink.blocking_record(EvidenceEvent::Lifecycle {
            elapsed_ns: 1000,
            state: "saving_completed".into(),
            attempted: 500,
            submitted: 500,
            skipped: 0,
            failed: 0,
            unknown: 0,
            unknown_sequence: None,
            lifecycle: report.lifecycle.clone(),
        })
        .unwrap();
        sink.blocking_record(EvidenceEvent::Metric(report.metric()))
            .unwrap();
        sink.blocking_record(EvidenceEvent::End(Box::new(report)))
            .unwrap();
        let result = writer.join().unwrap().unwrap();
        assert!(result.evidence.complete);
        assert!(result.evidence.bytes < identity.report.plan.evidence.max_bytes);
        let files = artifacts(&root.join(&result.id)).unwrap();
        assert!(
            files
                .iter()
                .filter(|f| f.name.starts_with("samples-") || f.name.starts_with("events-"))
                .count()
                > 1
        );
        let recovered = recover(&root).unwrap();
        assert_eq!(recovered[0].report.verdict, "pass");
        let sample = files
            .iter()
            .find(|f| f.name.starts_with("samples-") || f.name.starts_with("events-"))
            .unwrap();
        fs::write(root.join(&result.id).join(&sample.name), "corrupted").unwrap();
        let recovered = recover(&root).unwrap();
        assert_eq!(recovered[0].report.verdict, "inconclusive");
        assert!(!recovered[0].report.evidence.complete);
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
    #[test]
    fn quota_failure_never_finalizes_a_passing_report() {
        let (root, mut identity) = setup();
        identity.report.plan.evidence.max_bytes = 4 * 1024 * 1024 + 4096;
        identity.report.plan_hash = identity.report.plan.fingerprint();
        let path = prepare(&root, &identity).unwrap();
        let (sink, rx) = EvidenceSink::channel(256);
        let observer = sink.monitor();
        let source = identity.clone();
        let writer = std::thread::spawn(move || write(path, source, observer, rx));
        for n in 0..100 {
            sink.blocking_record(EvidenceEvent::Sample {
                channel: "virtual:1".into(),
                sequence: n,
                wire: None,
                data_hex: format!("00000011{n:08x}"),
                elapsed_ns: n as u64,
            })
            .unwrap();
        }
        let mut report = identity.report;
        report.state = "completed".into();
        report.verdict = "pass".into();
        report.measurement_complete = true;
        sink.blocking_record(EvidenceEvent::End(Box::new(report)))
            .unwrap();
        let final_report = writer.join().unwrap().unwrap();
        assert_eq!(final_report.verdict, "inconclusive");
        assert!(!final_report.evidence.complete);
        assert!(final_report.evidence.error.unwrap().contains("quota"));
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
    #[test]
    fn private_paths_and_interruption_are_checked() {
        let (root, identity) = setup();
        let path = prepare(&root, &identity).unwrap();
        assert!(!valid_name("../../secret"));
        assert!(open(&path, "identity.json").is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(path.join("identity.json"), path.join("report.json"))
                .unwrap();
            assert!(open(&path, "report.json").is_err());
            fs::remove_file(path.join("report.json")).unwrap();
        }
        let recovered = recover(&root).unwrap();
        assert_eq!(recovered[0].report.state, "interrupted");
        assert!(!recovered[0].report.counters_complete);
        assert!(!recovered[0].report.measurement_complete);
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
    #[test]
    fn torn_prefix_recovery_is_repeatable_and_preserves_sources() {
        let (root, identity) = setup();
        let path = prepare(&root, &identity).unwrap();
        let mut row = serde_json::to_vec(&EvidenceEvent::Metric(identity.report.metric())).unwrap();
        row.push(b'\n');
        let prefix = row.len();
        row.extend_from_slice(b"{torn");
        use std::io::Write;
        create(&path, "metrics-000000.ndjson")
            .unwrap()
            .write_all(&row)
            .unwrap();
        let source = fs::read(path.join("identity.json")).unwrap();
        for _ in 0..2 {
            let batch = recover_registry(&root).unwrap();
            assert!(batch.issues.is_empty());
            let r = &batch.runs[0].report;
            assert_eq!(r.state, "interrupted");
            assert_eq!(r.verdict, "inconclusive");
            assert_eq!(r.recovery.valid_prefix_bytes, prefix as u64);
            assert_eq!(r.recovery.damaged_files, 1);
            assert!(r.recovery.counters_are_lower_bounds);
            assert_eq!(fs::read(path.join("metrics-000000.ndjson")).unwrap(), row);
            assert_eq!(fs::read(path.join("identity.json")).unwrap(), source);
        }
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
    #[test]
    fn malformed_report_checkpoint_and_quarantined_identity_do_not_hide_healthy_runs() {
        let (root, identity) = setup();
        let path = prepare(&root, &identity).unwrap();
        for name in ["report.json", "checkpoint.json"] {
            create(&path, name).unwrap().write_all(b"broken").unwrap();
        }
        let mut bad = identity.clone();
        bad.report.id = uuid::Uuid::new_v4().to_string();
        bad.request = "bad".into();
        let other = prepare(&root, &bad).unwrap();
        fs::write(other.join("identity.json"), b"broken").unwrap();
        let batch = recover_registry(&root).unwrap();
        assert_eq!(batch.runs.len(), 1);
        assert_eq!(batch.runs[0].report.state, "interrupted");
        assert_eq!(batch.issues.len(), 1);
        assert_eq!(fs::read(path.join("report.json")).unwrap(), b"broken");
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
    #[test]
    fn semantic_regression_stops_stream_at_valid_boundary() {
        let (root, identity) = setup();
        let path = prepare(&root, &identity).unwrap();
        let mut metric = identity.report.metric();
        metric.attempted = 1;
        metric.submitted = 1;
        let first = EvidenceEvent::Metric(metric.clone());
        metric.attempted = 0;
        metric.submitted = 0;
        let mut bytes = serde_json::to_vec(&first).unwrap();
        bytes.push(b'\n');
        let valid = bytes.len();
        bytes.extend(serde_json::to_vec(&EvidenceEvent::Metric(metric)).unwrap());
        bytes.push(b'\n');
        create(&path, "metrics-000000.ndjson")
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let (scan, replay) = crate::stress_recovery::scan(&path, &identity).unwrap();
        assert_eq!(scan.summary.valid_prefix_bytes, valid as u64);
        assert_eq!(scan.summary.damaged_files, 1);
        assert_eq!(replay.last().unwrap().submitted, 1);
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
    #[test]
    fn injected_write_flush_checkpoint_and_commit_failures_cannot_recover_as_pass() {
        for point in ["write", "flush", "checkpoint", "report", "manifest"] {
            let (root, identity) = setup();
            let path = prepare(&root, &identity).unwrap();
            let (sink, rx) = EvidenceSink::channel(8);
            sink.blocking_record(EvidenceEvent::Metric(identity.report.metric()))
                .unwrap();
            sink.blocking_record(EvidenceEvent::End(Box::new(identity.report.clone())))
                .unwrap();
            let result = write_checked(path, identity, sink.monitor(), rx, |at| {
                if at == point {
                    Err(format!("injected {point} failure"))
                } else {
                    Ok(())
                }
            });
            match result {
                Ok(r) => {
                    assert_ne!(r.verdict, "pass");
                    assert!(!r.evidence.complete);
                    assert!(
                        r.evidence
                            .error
                            .unwrap()
                            .contains(&format!("injected {point} failure"))
                    );
                }
                Err(e) => assert!(e.contains(&format!("injected {point} failure"))),
            }
            let batch = recover_registry(&root).unwrap();
            assert!(batch.issues.is_empty());
            assert_ne!(batch.runs[0].report.verdict, "pass");
            assert!(!batch.runs[0].report.evidence.complete);
            fs::remove_dir_all(root.parent().unwrap()).unwrap();
        }
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn fifo_evidence_is_rejected_without_blocking_open() {
        let root = std::path::PathBuf::from(if cfg!(windows) {
            std::env::var_os("TEMP").expect("TEMP")
        } else {
            std::ffi::OsString::from("/tmp")
        })
        .join(format!("ch-audit-fifo-{}", uuid::Uuid::new_v4()));
        directory(&root).unwrap();
        let path = root.join("metrics-000000.ndjson");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: owned test-only path, NUL-terminated, no CAN or device I/O.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(open(&root, "metrics-000000.ndjson").is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_segment_does_not_bridge_the_valid_prefix() {
        use std::io::Write;
        let root = std::path::PathBuf::from(if cfg!(windows) {
            std::env::var_os("TEMP").expect("TEMP")
        } else {
            std::ffi::OsString::from("/tmp")
        })
        .join(format!("ch-audit-gap-{}", uuid::Uuid::new_v4()));
        directory(&root).unwrap();
        let plan:canharbor_stress::Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":4,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
        let identity = Identity {
            owner: "owner".into(),
            request: "gap".into(),
            report: Report::initial(&plan, true, uuid::Uuid::new_v4().to_string()).unwrap(),
        };
        let mut row = serde_json::to_vec(&EvidenceEvent::Metric(identity.report.metric())).unwrap();
        row.push(b'\n');
        create(&root, "metrics-000000.ndjson")
            .unwrap()
            .write_all(&row)
            .unwrap();
        create(&root, "metrics-000002.ndjson")
            .unwrap()
            .write_all(&row)
            .unwrap();
        let (scan, replay) = crate::stress_recovery::scan(&root, &identity).unwrap();
        assert_eq!(scan.summary.valid_prefix_bytes, row.len() as u64);
        assert!(scan.summary.damaged_files >= 1);
        assert_eq!(replay.metric_records, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
