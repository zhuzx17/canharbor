// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded SSH/stdio controller. No remote shell interpolation of manifest values.
use canharbor_stress::{CoordStatus, Coordination, Plan, RttRole, Validation};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde_json::{Value, json};
use std::io::Read;
use std::{io::Write, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
type Failure = Box<dyn std::error::Error>;
#[derive(Clone, Debug)]
struct Host {
    name: String,
    bus: String,
    ssh: Option<String>,
    runtime: String,
    executable: String,
    plan: Value,
    token: Option<String>,
    instance: Option<String>,
    run_id: Option<String>,
    finished: Option<Value>,
}
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
async fn rpc(host: &Host, body: Value) -> Result<Value, String> {
    let mut command = if let Some(ssh) = &host.ssh {
        let mut c = Command::new("ssh");
        c.args(["-T", "-oBatchMode=yes", "-oConnectTimeout=5", "--", ssh]);
        c.arg(format!(
            "exec {} --runtime-dir {} stress agent",
            quoted(&host.executable),
            quoted(&host.runtime)
        ));
        c
    } else {
        let mut c = Command::new(&host.executable);
        c.arg("--runtime-dir")
            .arg(&host.runtime)
            .args(["stress", "agent"]);
        c
    };
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(&serde_json::to_vec(&body).unwrap())
            .await
            .map_err(|e| e.to_string())?;
        drop(stdin);
        let mut stdout = child.stdout.take().unwrap().take(1024 * 1024 + 1);
        let mut stderr = child.stderr.take().unwrap().take(65537);
        let mut out = vec![];
        let mut err = vec![];
        let (a, b, status) = tokio::join!(
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait()
        );
        a.map_err(|e| e.to_string())?;
        b.map_err(|e| e.to_string())?;
        if out.len() > 1024 * 1024 || err.len() > 65536 {
            return Err("agent output exceeds bound".into());
        }
        if !status.map_err(|e| e.to_string())?.success() {
            return Err(String::from_utf8_lossy(&err).chars().take(512).collect());
        }
        serde_json::from_slice(&out).map_err(|e| e.to_string())
    })
    .await;
    match result {
        Ok(r) => r,
        Err(_) => {
            let _ = child.kill().await;
            Err("agent transport deadline; mutation outcome unknown; no retry".into())
        }
    }
}
fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "aborted" | "interrupted")
}
fn status(host: &Host, group: &str, operation: &str, value: &Value) -> Result<CoordStatus, String> {
    let result: CoordStatus =
        serde_json::from_value(value.clone()).map_err(|e| format!("invalid agent status: {e}"))?;
    if result.group_id != group
        || uuid::Uuid::parse_str(&result.server_instance_id).is_err()
        || host
            .instance
            .as_ref()
            .is_some_and(|id| *id != result.server_instance_id)
    {
        return Err("coordinator group or service instance changed".into());
    }
    if result.clock_quality != "unsynchronized; no cross-host one-way latency" {
        return Err("unsupported clock quality".into());
    }
    if operation == "prepare" {
        if result.state != "prepared"
            || result.run_id.is_some()
            || result.report.is_some()
            || result.expires_in_ms == 0
            || result
                .token
                .as_ref()
                .is_none_or(|t| uuid::Uuid::parse_str(t).is_err())
        {
            return Err("agent is not prepared with a live token".into());
        }
    } else {
        let report = result.report.as_ref().ok_or("agent omitted run evidence")?;
        let plan: Plan =
            serde_json::from_value(host.plan["plan"].clone()).map_err(|e| e.to_string())?;
        if result.run_id.as_deref() != Some(&report.id)
            || uuid::Uuid::parse_str(&report.id).is_err()
            || report.plan != plan
            || report.plan_hash != plan.fingerprint()
            || report.state != result.state
            || report.version
                < if plan.requires_resource_v11() {
                    11
                } else if plan.requires_load_v10() {
                    10
                } else if plan.has_classic_pattern() {
                    9
                } else if !plan.flows.is_empty() {
                    8
                } else if plan.timing.is_some() || plan.topology.is_some() {
                    7
                } else if plan.criteria.is_default() {
                    5
                } else {
                    6
                }
            || host.run_id.as_ref().is_some_and(|id| *id != report.id)
        {
            return Err("agent run identity, state, or immutable plan mismatch".into());
        }
        let planned = plan.compile().map_err(|e| e.to_string())?.count;
        if if plan.adaptive.is_some() {
            report
                .adaptive
                .as_ref()
                .is_none_or(|a| a.reserved_planned != planned)
                || report.planned > planned
        } else {
            report.planned != planned
        } {
            return Err("participant planned count changed".into());
        }
        if terminal(&result.state)
            && (result.state != "completed"
                || report.verdict != "pass"
                || !report.evidence.complete
                || !report.passing_invariants()
                || report.lifecycle.execution_end_ns.is_none()
                || report.recovery.status != "verified"
                || (plan.flows.is_empty()
                    && plan.adaptive.is_none()
                    && (report.submission_evidence.submitted != report.submitted
                        || report.submission_evidence.covered != planned
                        || report.submission_evidence.failed != 0
                        || report.submission_evidence.unknown != 0
                        || report.submission_evidence.skipped != report.skipped
                        || report.submission_evidence.unknown_sequence.is_some()
                        || plan.rtt.is_some() != report.rtt_report.is_some()))
                || (plan.stability.max_rss_growth_bytes.is_some()
                    && (report.stability.status != "monitoring"
                        || report.stability.limit_exceeded)))
        {
            return Err(format!(
                "participant ended without passing: {} / {}",
                result.state, report.verdict
            ));
        }
        if !terminal(&result.state) && result.expires_in_ms == 0 {
            return Err("participant lease already expired".into());
        }
        if !terminal(&result.state)
            && !matches!(
                result.state.as_str(),
                "arming"
                    | "reserved"
                    | "warming"
                    | "running"
                    | "draining"
                    | "finalizing"
                    | "saving_completed"
            )
        {
            return Err(format!("participant cannot continue: {}", result.state));
        }
    }
    Ok(result)
}
async fn phase(
    hosts: &[Host],
    group: &str,
    operation: &str,
) -> Vec<(usize, Result<Value, String>, u64)> {
    let mut tasks = FuturesUnordered::new();
    for (index, host) in hosts.iter().enumerate() {
        let body = if operation == "prepare" {
            json!({"operation":operation,"group_id":group,"request":{"active":true,"client_request_id":format!("{group}:{}",host.name),"plan_hash":host.plan["plan_hash"],"plan":host.plan["plan"]}})
        } else {
            json!({"operation":operation,"group_id":group,"token":host.token})
        };
        tasks.push(async move {
            let start = std::time::Instant::now();
            let result = if operation == "heartbeat" && host.finished.is_some() {
                Ok(host.finished.clone().unwrap())
            } else {
                rpc(host, body).await
            };
            let result = result.and_then(|v| {
                if operation == "abort" {
                    Ok(v)
                } else {
                    status(host, group, operation, &v).map(|_| v)
                }
            });
            (index, result, start.elapsed().as_millis() as u64)
        });
    }
    let mut results = vec![];
    while let Some(r) = tasks.next().await {
        let failed = r.1.is_err();
        results.push(r);
        if failed && operation != "abort" {
            break;
        }
    }
    results.sort_by_key(|r| r.0);
    results
}
fn record(file: &mut std::fs::File, value: &Value) -> Result<(), Failure> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > 12 * 1024 * 1024
        || file.metadata()?.len() + bytes.len() as u64 + 1 > 32 * 1024 * 1024
    {
        return Err("coordination evidence exceeds bounded reservation".into());
    }
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn parse(m: Value) -> Result<(String, u64, Vec<Host>), Failure> {
    if m.as_object().is_none_or(|o| {
        o.keys()
            .any(|k| !matches!(k.as_str(), "group_id" | "hosts" | "timeout_seconds"))
    }) {
        return Err("unknown coordination manifest field".into());
    }
    let group = m["group_id"]
        .as_str()
        .ok_or("group_id UUID required")?
        .to_owned();
    uuid::Uuid::parse_str(&group)?;
    let duration = m["timeout_seconds"]
        .as_u64()
        .filter(|s| (1..=259230).contains(s))
        .ok_or("timeout_seconds must be 1..259230")?;
    let nodes = m["hosts"]
        .as_array()
        .filter(|n| (2..=8).contains(&n.len()))
        .ok_or("need 2..8 hosts")?;
    let mut hosts = vec![];
    let mut names = std::collections::HashSet::new();
    let mut producers = std::collections::HashSet::new();
    let mut bus_settings = std::collections::HashMap::new();
    let mut tags = std::collections::HashMap::<u32, (String, Plan, bool)>::new();
    for node in nodes {
        if node.as_object().is_none_or(|o| {
            o.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "name" | "ssh" | "runtime_dir" | "executable" | "plan" | "bus"
                )
            })
        }) || (!node["ssh"].is_null() && !node["ssh"].is_string())
            || (!node["executable"].is_null() && !node["executable"].is_string())
        {
            return Err("unknown/invalid host field".into());
        }
        let name = node["name"].as_str().ok_or("host name required")?;
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !names.insert(name.to_owned())
        {
            return Err("invalid/duplicate host name".into());
        }
        let bus = node
            .get("bus")
            .map_or(Ok("shared"), |v| v.as_str().ok_or("bus must be a string"))?;
        if bus.is_empty()
            || bus.len() > 64
            || !bus
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid bus identity".into());
        }
        let ssh = node["ssh"].as_str().map(str::to_owned);
        if ssh.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 256
                || s.starts_with('-')
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"@._-".contains(&b))
        }) {
            return Err("SSH target must be an explicit host/config alias".into());
        }
        let runtime = node["runtime_dir"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.contains('\0'))
            .ok_or("bounded runtime_dir required")?
            .to_owned();
        let executable = node["executable"]
            .as_str()
            .unwrap_or(if ssh.is_some() { "canharbor" } else { "" })
            .to_owned();
        if executable.is_empty() || executable.len() > 1024 || executable.contains('\0') {
            return Err("explicit local executable required".into());
        }
        let mut plan: Plan = serde_json::from_value(node["plan"].clone())?;
        if plan.coordination.is_some() {
            return Err("controller must bind coordination identity".into());
        }
        plan.compile()?;
        let settings = (
            plan.bitrate,
            if plan.flows.is_empty() {
                plan.traffic.data_bitrate()
            } else {
                plan.group_budget()?.data_bitrate
            },
        );
        if bus_settings
            .insert(bus.to_owned(), settings)
            .is_some_and(|old| old != settings)
        {
            return Err("same-bus nominal/FD data rates or frame modes differ".into());
        }
        for (_, id, extended, _) in plan.producers() {
            if !producers.insert((bus.to_owned(), extended, id)) {
                return Err("same-bus CAN ID has multiple data producers".into());
            }
        }
        for flow in plan.identity_plans() {
            let role = flow.rtt.as_ref().map(|r| r.role);
            if let Some((prior_bus, prior, paired)) = tags.get_mut(&flow.run_tag) {
                if *paired
                    || prior_bus != bus
                    || !matches!(
                        (prior.rtt.as_ref().map(|r| r.role), role),
                        (Some(RttRole::Probe), Some(RttRole::Responder))
                            | (Some(RttRole::Responder), Some(RttRole::Probe))
                    )
                    || prior.traffic != flow.traffic
                    || prior.can_id != flow.can_id
                    || prior.extended != flow.extended
                    || prior.count != flow.count
                    || prior.fps != flow.fps
                    || prior.stages != flow.stages
                    || prior.bitrate != flow.bitrate
                    || prior.rtt.as_ref().unwrap().response_can_id
                        != flow.rtt.as_ref().unwrap().response_can_id
                {
                    return Err(
                        "reused run tag needs one matching same-bus probe/responder pair".into(),
                    );
                }
                *paired = true;
            } else {
                tags.insert(flow.run_tag, (bus.to_owned(), flow.clone(), false));
            }
        }
        plan.coordination = Some(Coordination {
            group_id: group.clone(),
            participant: name.into(),
            lease_ms: 15000,
            start_delay_ms: 10000,
            clock_quality: "unsynchronized".into(),
        });
        hosts.push(Host {
            name: name.into(),
            bus: bus.into(),
            ssh,
            runtime,
            executable,
            plan: serde_json::to_value(plan)?,
            token: None,
            instance: None,
            run_id: None,
            finished: None,
        });
    }
    if tags.values().any(|(_, p, paired)| {
        p.rtt
            .as_ref()
            .is_some_and(|r| matches!(r.role, RttRole::Probe | RttRole::Responder))
            && !*paired
    }) {
        return Err("coordinated probe/responder requires both managed participants".into());
    }
    Ok((group, duration, hosts))
}
async fn execute_group(
    hosts: &mut [Host],
    group: &str,
    file: &mut std::fs::File,
) -> Result<Vec<Value>, Failure> {
    let mut validation = FuturesUnordered::new();
    for (i, h) in hosts.iter().enumerate() {
        validation.push(async move {
            (
                i,
                rpc(h, json!({"operation":"validate","plan":h.plan})).await,
            )
        });
    }
    let mut validated = vec![];
    while let Some((i, r)) = validation.next().await {
        let v: Validation = serde_json::from_value(r?)?;
        let sent: Plan = serde_json::from_value(hosts[i].plan.clone())?;
        if !v.valid
            || v.version
                < if sent.requires_resource_v11() {
                    11
                } else if sent.requires_load_v10() {
                    10
                } else if sent.has_classic_pattern() {
                    9
                } else if !sent.flows.is_empty() {
                    8
                } else if sent.timing.is_some() || sent.topology.is_some() {
                    7
                } else if sent.criteria.is_default() {
                    5
                } else {
                    6
                }
            || v.plan != sent
            || v.plan_hash != sent.fingerprint()
        {
            return Err("agent validation differs from immutable controller plan".into());
        }
        validated.push((i, v));
    }
    drop(validation);
    for (i, v) in validated {
        hosts[i].plan = json!({"plan_hash":v.plan_hash,"plan":v.plan});
    }
    for operation in ["prepare", "commit"] {
        let results = phase(hosts, group, operation).await;
        let mut failure = None;
        for (i, r, ms) in results {
            match r {
                Ok(mut v) => {
                    if operation == "prepare" {
                        hosts[i].token = v["token"].as_str().map(str::to_owned);
                        hosts[i].instance = v["server_instance_id"].as_str().map(str::to_owned);
                    } else {
                        hosts[i].run_id = v["run_id"].as_str().map(str::to_owned);
                        if terminal(v["state"].as_str().unwrap_or("")) {
                            hosts[i].finished = Some(v.clone());
                        }
                    }
                    v["token"] = Value::Null;
                    record(
                        file,
                        &json!({"phase":operation,"host":hosts[i].name,"round_trip_ms":ms,"status":v}),
                    )?;
                }
                Err(e) => {
                    record(
                        file,
                        &json!({"phase":operation,"host":hosts[i].name,"outcome_unknown":true,"error":e}),
                    )?;
                    failure = Some(e);
                }
            }
        }
        if let Some(e) = failure {
            return Err(e.into());
        }
    }
    loop {
        let cached = hosts
            .iter()
            .map(|h| h.finished.is_some())
            .collect::<Vec<_>>();
        let results = phase(hosts, group, "heartbeat").await;
        let mut reports = vec![];
        for (i, r, ms) in results {
            let mut v = r.map_err(|e| format!("{}: {e}", hosts[i].name))?;
            if terminal(v["state"].as_str().unwrap_or("")) {
                hosts[i].finished = Some(v.clone());
            }
            v["token"] = Value::Null;
            reports.push(json!({"host":hosts[i].name,"round_trip_ms":if cached[i]{None}else{Some(ms)},"cached_terminal":cached[i],"status":v}));
        }
        if hosts.iter().all(|h| h.finished.is_some()) {
            return Ok(reports);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
pub async fn run(manifest: PathBuf, active: bool, output: PathBuf) -> Result<(), Failure> {
    if !active {
        return Err(crate::status::input("coordinate requires --active"));
    }
    let mut bytes = vec![];
    std::fs::File::open(manifest)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("manifest exceeds 1 MiB".into());
    }
    let (group, duration, mut hosts) = parse(serde_json::from_slice(&bytes)?)?;
    let mut file = canharbor_platform::private::create_new(&output)?;
    record(
        &mut file,
        &json!({"version":2,"group_id":group,"phase":"created","clock_quality":"unsynchronized","one_way_latency":null,"hosts":hosts.iter().map(|h|json!({"name":h.name,"bus":h.bus,"plan_hash":serde_json::from_value::<Plan>(h.plan.clone()).unwrap().fingerprint()})).collect::<Vec<_>>()}),
    )?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(duration);
    // One finally-style abort path also covers local evidence I/O errors and cancellation in any phase.
    let result = tokio::select! {r=tokio::time::timeout_at(deadline,execute_group(&mut hosts,&group,&mut file))=>match r{Ok(r)=>r,Err(_)=>Err("controller deadline".into())},_=tokio::signal::ctrl_c()=>Err("operator stop".into())};
    let (mut reports, failure) = match result {
        Ok(r) => (r, None),
        Err(e) => (vec![], Some(e.to_string())),
    };
    if failure.is_some() {
        for (i, result, ms) in phase(&hosts, &group, "abort").await {
            let mut row = json!({"phase":"abort","host":hosts[i].name,"round_trip_ms":ms,"acknowledged":result.is_ok()});
            match result {
                Ok(mut status) => {
                    status["token"] = Value::Null;
                    row["status"] = status;
                }
                Err(e) => {
                    row["error"] = e.into();
                    row["outcome_unknown"] = true.into();
                }
            }
            reports.push(row.clone());
            let _ = record(&mut file, &row);
        }
    }
    let mut final_report = json!({"version":2,"group_id":group,"phase":"final","passed":failure.is_none(),"error":failure,"clock_quality":"unsynchronized","one_way_latency":null,"hosts":reports});
    if let Err(e) = record(&mut file, &final_report) {
        final_report["passed"] = false.into();
        final_report["evidence_error"] = e.to_string().into();
    }
    let passed = final_report["passed"] == true;
    crate::print(final_report);
    if !passed {
        return Err(Box::new(crate::status::Failure::Reported {
            code: 6,
            message:
                "coordinated run did not pass; no retry; inspect evidence and final node states"
                    .into(),
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan(id: u32, tag: u32) -> Value {
        json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":id,"extended":false,"run_tag":tag,"count":4,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})
    }
    fn manifest() -> Value {
        json!({"group_id":uuid::Uuid::new_v4().to_string(),"timeout_seconds":10,"hosts":[{"name":"a","runtime_dir":"/tmp/a","executable":"unused","plan":plan(291,1)},{"name":"b","runtime_dir":"/tmp/b","executable":"unused","plan":plan(293,2)}]})
    }
    #[test]
    fn same_bus_id_and_bitrate_conflicts_are_rejected_without_starting_agents() {
        let mut m = manifest();
        m["hosts"][1]["plan"]["can_id"] = 291.into();
        assert!(
            parse(m)
                .unwrap_err()
                .to_string()
                .contains("multiple data producers")
        );
        let mut m = manifest();
        m["hosts"][1]["plan"]["bitrate"] = 250000.into();
        assert!(parse(m).unwrap_err().to_string().contains("rates"));
        let mut m = manifest();
        m["hosts"][1]["bus"] = "separate".into();
        m["hosts"][1]["plan"]["can_id"] = 291.into();
        assert!(parse(m).is_ok());
    }
    #[test]
    fn additional_flow_producers_and_tags_are_checked_across_hosts() {
        let mut m = manifest();
        let mut extra = plan(293, 3);
        for key in ["bitrate", "receivers_active", "drain_ms"] {
            extra.as_object_mut().unwrap().remove(key);
        }
        extra["name"] = "reverse".into();
        extra["sender"] = "virtual:1".into();
        extra["receivers"] = json!(["virtual:0"]);
        m["hosts"][0]["plan"]["flows"] = json!([extra]);
        assert!(
            parse(m.clone())
                .unwrap_err()
                .to_string()
                .contains("multiple data producers")
        );
        m["hosts"][0]["plan"]["flows"][0]["can_id"] = 295.into();
        assert!(parse(m.clone()).is_ok());
        m["hosts"][0]["plan"]["flows"][0]["run_tag"] = 2.into();
        assert!(parse(m).is_err());
    }
    #[test]
    fn classic_pattern_control_producer_conflicts_are_rejected_before_prepare() {
        let mut m = manifest();
        m["hosts"][0]["plan"]["traffic"] = json!({"format":"classic_pattern_v1","checkpoint_can_id":293,"seed":42,"stream":1,"max_pending":4});
        m["hosts"][0]["plan"]["evidence"] =
            json!({"sample_every":1,"max_bytes":268435456,"segment_bytes":4194304});
        assert!(
            parse(m.clone())
                .unwrap_err()
                .to_string()
                .contains("multiple data producers")
        );
        m["hosts"][0]["plan"]["traffic"]["checkpoint_can_id"] = 295.into();
        assert!(parse(m).is_ok());
    }
    #[test]
    fn adaptive_reserves_all_derived_step_tags_before_any_prepare() {
        let mut m = manifest();
        m["hosts"][0]["plan"]["count"] = 0.into();
        m["hosts"][0]["plan"]["fps"] = 0.into();
        m["hosts"][0]["plan"]["adaptive"] = json!({"rates":[2,4],"exploration_seconds":1,"measurement_seconds":1,"min_observed_ppm":800000});
        assert!(parse(m.clone()).is_err());
        m["hosts"][1]["plan"]["run_tag"] = 99.into();
        assert!(parse(m).is_ok());
    }
    #[test]
    fn status_rejects_instance_changes_wrong_plans_and_unproven_pass() {
        let (group, _, mut hosts) = parse(manifest()).unwrap();
        let p: Plan = serde_json::from_value(hosts[0].plan.clone()).unwrap();
        hosts[0].plan = json!({"plan_hash":p.fingerprint(),"plan":p});
        let server = uuid::Uuid::new_v4().to_string();
        hosts[0].instance = Some(server.clone());
        let report =
            canharbor_stress::Report::initial(&p, true, uuid::Uuid::new_v4().to_string()).unwrap();
        let mut v = json!({"group_id":group,"state":"arming","token":uuid::Uuid::new_v4().to_string(),"expires_in_ms":10000,"run_id":report.id,"report":report,"server_instance_id":server,"clock_quality":"unsynchronized; no cross-host one-way latency"});
        assert!(status(&hosts[0], &group, "commit", &v).is_ok());
        v["server_instance_id"] = uuid::Uuid::new_v4().to_string().into();
        assert!(status(&hosts[0], &group, "heartbeat", &v).is_err());
        v["server_instance_id"] = server.into();
        v["state"] = "completed".into();
        v["report"]["state"] = "completed".into();
        v["report"]["verdict"] = "pass".into();
        assert!(status(&hosts[0], &group, "heartbeat", &v).is_err());
    }
    #[test]
    fn coordinated_terminal_accepts_frozen_nonzero_quality_budget_but_not_forged_coverage() {
        let (group, _, mut hosts) = parse(manifest()).unwrap();
        let mut p: Plan = serde_json::from_value(hosts[0].plan.clone()).unwrap();
        p.criteria.max_duplicates = 1;
        hosts[0].plan = json!({"plan_hash":p.fingerprint(),"plan":p});
        let mut r =
            canharbor_stress::Report::initial(&p, true, uuid::Uuid::new_v4().to_string()).unwrap();
        r.capture_sources(&canharbor_core::runtime::Runtime::new(std::sync::Arc::new(
            canharbor_virtual::VirtualBus::default(),
        )));
        r.state = "completed".into();
        r.verdict = "pass".into();
        r.attempted = u64::from(r.planned);
        r.submitted = r.attempted;
        r.workload_achieved = true;
        r.measurement_complete = true;
        r.evidence.complete = true;
        r.recovery.status = "verified".into();
        r.lifecycle.execution_end_ns = Some(1);
        r.submission_evidence.covered = r.planned;
        r.submission_evidence.submitted = r.submitted;
        r.receivers = p
            .receivers
            .iter()
            .map(|c| canharbor_stress::ReceiverReport {
                channel: c.clone(),
                unique: u64::from(r.planned),
                duplicates: 1,
                evidence_complete: true,
                ..Default::default()
            })
            .collect();
        for stage in &mut r.stages {
            stage.submitted = u64::from(stage.count);
            stage.received_unique = vec![u64::from(stage.count); p.receivers.len()];
        }
        r.assessment = Some(r.assess());
        let mut v = json!({"group_id":group,"state":"completed","token":null,"expires_in_ms":0,"run_id":r.id,"report":r,"server_instance_id":uuid::Uuid::new_v4().to_string(),"clock_quality":"unsynchronized; no cross-host one-way latency"});
        assert!(status(&hosts[0], &group, "heartbeat", &v).is_ok());
        v["report"]["version"] = 5.into();
        assert!(status(&hosts[0], &group, "heartbeat", &v).is_err());
        v["report"]["version"] = 7.into();
        v["report"]["receivers"][0]["unique"] = 0.into();
        assert!(status(&hosts[0], &group, "heartbeat", &v).is_err());
    }
}
