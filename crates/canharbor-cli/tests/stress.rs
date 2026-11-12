// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::process::Command;

// These cases launch services/CLI processes and fsync evidence against real
// scheduler deadlines. Isolate fixtures, while keeping each case's explicit
// multi-node/multi-flow concurrency, rates and exact loss/count checks.
static TIMED_CASE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

// Socket creation precedes serving requests. Wait for the stress API (including
// evidence recovery) before launching a CLI that submits work only once.
async fn ready_service(
    root: &Path,
    service: &mut tokio::process::Child,
) -> canharbor_client::LocalClient {
    let client = canharbor_client::LocalClient::new(root.into()).unwrap();
    let mut last_error = String::new();
    let ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            assert!(
                service.try_wait().unwrap().is_none(),
                "service exited before becoming ready; see child stderr"
            );
            match client
                .request("GET", "/stress/runs", Value::Null, None)
                .await
            {
                Ok(_) => break,
                Err(error) => last_error = error.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(ready.is_ok(), "service readiness timed out: {last_error}");
    client
}

async fn cli(root: &Path, args: &[&str]) -> std::process::Output {
    tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_canharbor"))
            .arg("--runtime-dir")
            .arg(root)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stress_cli_virtual_service_validation_execution_and_export() {
    let _isolation = TIMED_CASE.acquire().await.unwrap();
    let root = test_temp_dir().join(format!("ch-stress-{}", uuid::Uuid::new_v4()));
    let service = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &root,
        0,
        false,
    )
    .await
    .unwrap();
    let plan = root.join("plan.json");
    let out = root.join("report.json");
    std::fs::write(&plan,json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":42,"criteria":{"max_duplicates":1},"count":5,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":30}).to_string()).unwrap();
    let p = plan.to_str().unwrap();
    assert!(
        cli(&root, &["stress", "validate", "--plan", p])
            .await
            .status
            .success()
    );
    assert!(service.state.runtime.list().await.is_empty());
    assert!(
        !cli(&root, &["stress", "run", "--plan", p])
            .await
            .status
            .success()
    );
    let r = cli(
        &root,
        &[
            "stress",
            "run",
            "--plan",
            p,
            "--active",
            "--request-id",
            "same-run",
        ],
    )
    .await;
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let report: Value = serde_json::from_slice(&r.stdout).unwrap();
    assert_eq!(report["verdict"], "pass");
    assert_eq!(report["submitted"], 5);
    assert_eq!(report["plan"]["criteria"]["max_duplicates"], 1);
    assert_eq!(report["assessment"]["passed"], true);
    assert_eq!(report["simulated"], true);
    let again = cli(
        &root,
        &[
            "stress",
            "run",
            "--plan",
            p,
            "--active",
            "--request-id",
            "same-run",
        ],
    )
    .await;
    let again: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["id"], report["id"]);
    let id = report["id"].as_str().unwrap();
    assert!(
        cli(
            &root,
            &["stress", "report", id, "--output", out.to_str().unwrap()]
        )
        .await
        .status
        .success()
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(saved["id"], report["id"]);
    assert!(
        !cli(
            &root,
            &["stress", "report", id, "--output", out.to_str().unwrap()]
        )
        .await
        .status
        .success()
    );
    assert!(cli(&root, &["stress", "delete", id]).await.status.success());
    let closed = service.shutdown().await;
    assert_eq!(closed.unknown_tasks, 0);
    std::fs::remove_dir_all(root).unwrap();
}

// A real process boundary, with only the built-in virtual backend registered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_service_recovers_prefix_without_restarting_transmission() {
    let _isolation = TIMED_CASE.acquire().await.unwrap();
    for adaptive in [false, true] {
        let root = test_temp_dir().join(format!("ch-p3-{}", uuid::Uuid::new_v4().simple()));
        let spawn = || {
            Command::new(env!("CARGO_BIN_EXE_canharbor"))
                .arg("--runtime-dir")
                .arg(&root)
                .args(["serve", "--no-web"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .unwrap()
        };
        let mut service = spawn();
        let client = ready_service(&root, &mut service).await;
        let plan = root.join("plan.json");
        let mut definition = json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":99,"count":10000,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":30});
        if adaptive {
            definition["count"] = 0.into();
            definition["fps"] = 0.into();
            definition["adaptive"] = json!({"rates":[10,20],"exploration_seconds":30,"measurement_seconds":30,"min_observed_ppm":800000});
            definition["evidence"] =
                json!({"sample_every":1,"max_bytes":268435456,"segment_bytes":4194304});
        }
        std::fs::write(&plan, definition.to_string()).unwrap();
        let mut runner = Command::new(env!("CARGO_BIN_EXE_canharbor"))
            .arg("--runtime-dir")
            .arg(&root)
            .args([
                "stress",
                "run",
                "--plan",
                plan.to_str().unwrap(),
                "--active",
                "--request-id",
                "interruption",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut last_list = Value::Null;
        let started = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                assert!(
                    service.try_wait().unwrap().is_none(),
                    "service exited before SIGKILL; see child stderr"
                );
                assert!(
                    runner.try_wait().unwrap().is_none(),
                    "stress CLI exited before SIGKILL (adaptive={adaptive}); see child stderr"
                );
                last_list = client
                    .request("GET", "/stress/runs", Value::Null, None)
                    .await
                    .unwrap();
                if let Some(run) = last_list["runs"].as_array().and_then(|a| a.first()) {
                    assert!(
                        !matches!(
                            run["state"].as_str(),
                            Some("completed" | "failed" | "aborted" | "interrupted")
                        ),
                        "run ended before SIGKILL: {run}"
                    );
                    if run["submitted"].as_u64().unwrap_or(0) >= 10 {
                        break run["id"].as_str().unwrap().to_owned();
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let id = started.unwrap_or_else(|_| {
            panic!("run did not submit 10 frames before SIGKILL (adaptive={adaptive}): {last_list}")
        });
        service.kill().await.unwrap(); // SIGKILL, no application finalization.
        let _ = runner.kill().await;
        let evidence = root.join("captures").join("stress").join(&id);
        let sources = std::fs::read_dir(&evidence)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                let b = std::fs::read(&p).unwrap();
                (p, b)
            })
            .collect::<Vec<_>>();
        assert!(
            sources
                .iter()
                .any(|(p, _)| p.file_name().unwrap() == "checkpoint.json")
        );
        let mut restarted = spawn();
        ready_service(&root, &mut restarted).await;
        let result = cli(&root, &["stress", "status", &id]).await;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["state"], "interrupted");
        if adaptive {
            assert_eq!(report["adaptive"]["phase"], "interrupted");
            assert!(
                report["adaptive"]["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|s| s["name"] != "measurement")
            );
        }
        // A fixed-rate run with persisted skips has already violated its
        // frozen zero-skip criterion; interruption must preserve that failure.
        // Adaptive exploration skips alone do not fail an unfinished measurement.
        let frozen: canharbor_stress::Plan =
            serde_json::from_value(report["plan"].clone()).unwrap();
        assert_eq!(frozen.criteria.max_scheduler_skipped, 0);
        let skipped = report["skipped"].as_u64().unwrap();
        assert_eq!(
            report["verdict"],
            if !adaptive && skipped > 0 {
                "fail"
            } else {
                "inconclusive"
            },
            "{report}"
        );
        assert_eq!(report["measurement_complete"], false);
        assert_eq!(report["counters_complete"], false);
        assert_eq!(report["evidence"]["complete"], false);
        assert_eq!(report["workload_achieved"], false);
        assert_eq!(report["recovery"]["counters_are_lower_bounds"], true);
        let again = cli(
            &root,
            &[
                "stress",
                "run",
                "--plan",
                plan.to_str().unwrap(),
                "--active",
                "--request-id",
                "interruption",
            ],
        )
        .await;
        let again: Value = serde_json::from_slice(&again.stdout).unwrap();
        assert_eq!(again["id"], id);
        assert_eq!(again["state"], "interrupted");
        assert_eq!(again["verdict"], report["verdict"]);
        assert_eq!(again["submitted"], report["submitted"]);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let after = cli(&root, &["stress", "status", &id]).await;
        let after: Value = serde_json::from_slice(&after.stdout).unwrap();
        assert_eq!(after["submitted"], report["submitted"]);
        for (path, bytes) in sources {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
        restarted.kill().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_agent_coordinate_commits_once_and_aborts_on_lost_commit_response() {
    let _isolation = TIMED_CASE.acquire().await.unwrap();
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    for failure_mode in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
        let lose_response =
            (1..=4).contains(&failure_mode) || failure_mode == 7 || failure_mode == 9;
        let root = test_temp_dir().join(format!("ch-audit-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let a = root.join("a");
        let b = root.join("b");
        let bus = Arc::new(VirtualBus::with_channel_count(8192, 4));
        let one = RunningServer::start(Runtime::new(bus.clone()), &a, 0, false)
            .await
            .unwrap();
        let two = RunningServer::start(Runtime::new(bus), &b, 0, false)
            .await
            .unwrap();
        let executable = env!("CARGO_BIN_EXE_canharbor");
        let proxy = root.join("agent.py");
        let calls = root.join("commits");
        let evidence = root.join("coordination.ndjson");
        if failure_mode != 0 {
            let code = format!(
                "#!/usr/bin/python3\nimport json,sys,subprocess,os,uuid\nb=sys.stdin.buffer.read();op=json.loads(b).get('operation')\nr=subprocess.run([{executable:?}]+sys.argv[1:],input=b,stdout=subprocess.PIPE,stderr=subprocess.PIPE)\nout=r.stdout\nif op=='commit':\n with open({calls:?},'a') as f:f.write('commit\\n')\n if {mode} in (1,7,9):sys.exit(1)\n if {mode}==2:\n  v=json.loads(out);v['server_instance_id']=str(uuid.uuid4());out=json.dumps(v).encode()\n if {mode}==3:\n  v=json.loads(out);v['state']='failed';v['report']['state']='failed';v['report']['verdict']='fail';out=json.dumps(v).encode()\n if {mode}==4:\n  with open({evidence:?},'r+b') as f:f.truncate(32*1024*1024)\nif op=='heartbeat' and {mode}==5 and json.loads(out).get('state')=='completed':\n if os.path.exists({marker:?}):sys.exit(1)\n with open({marker:?},'w') as f:f.write('completed')\nsys.stdout.buffer.write(out);sys.stderr.buffer.write(r.stderr);sys.exit(r.returncode)\n",
                calls = calls.to_str().unwrap(),
                mode = failure_mode,
                evidence = evidence.to_str().unwrap(),
                marker = root.join("finished").to_str().unwrap()
            );
            std::fs::write(&proxy, code).unwrap();
            std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let p = |tag: u32| json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":tag,"count":5,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":30,"traffic":{"format":"fd_v1","data_bitrate":2000000,"stream":1,"epoch":1,"seed":42,"variants":[{"length":32,"brs":true},{"length":64,"brs":false}]}});
        let mut probe = p(101);
        let mut responder = p(102);
        responder["can_id"] = 293.into();
        if failure_mode == 0 {
            probe["timing"] = json!({"sample_every":1,"max_pending":4});
            probe["rtt"] =
                json!({"role":"probe","response_can_id":292,"timeout_ms":500,"max_inflight":32});
            responder = probe.clone();
            responder["rtt"]["role"] = "responder".into();
            responder["sender"] = "virtual:2".into();
            responder["receivers"] = json!(["virtual:3"]);
        }
        if failure_mode == 5 {
            probe["count"] = 1.into();
            // This case checks terminal caching, not host throughput; keep a longer, low-rate peer.
            responder["count"] = 5.into();
            responder["fps"] = 2.into();
        }
        if (6..=7).contains(&failure_mode) {
            for (host, base_id, tag) in [(&mut probe, 300, 201), (&mut responder, 310, 202)] {
                let mut extra = p(tag);
                for key in ["bitrate", "receivers_active", "drain_ms"] {
                    extra.as_object_mut().unwrap().remove(key);
                }
                extra["name"] = "reverse".into();
                extra["sender"] = "virtual:1".into();
                extra["receivers"] = json!(["virtual:0"]);
                extra["can_id"] = base_id.into();
                extra["count"] = 6.into();
                extra["traffic"] = json!({"format":"classic_pattern_v1","checkpoint_can_id":base_id+1,"seed":42,"stream":7,"max_pending":4});
                host["evidence"] =
                    json!({"sample_every":1,"max_bytes":268435456,"segment_bytes":4194304});
                host["flows"] = json!([extra]);
            }
        }
        if failure_mode >= 8 {
            responder["sender"] = "virtual:2".into();
            responder["receivers"] = json!(["virtual:3"]);
            responder["run_tag"] = 201.into();
            for host in [&mut probe, &mut responder] {
                host["count"] = 0.into();
                host["fps"] = 0.into();
                host["adaptive"] = json!({"rates":[2,4],"exploration_seconds":1,"measurement_seconds":1,"min_observed_ppm":800000});
                host["evidence"] =
                    json!({"sample_every":1,"max_bytes":268435456,"segment_bytes":4194304});
            }
        }
        let manifest = root.join("group.json");
        std::fs::write(&manifest,json!({"group_id":uuid::Uuid::new_v4().to_string(),"timeout_seconds":20,"hosts":[{"name":"alpha","runtime_dir":a,"executable":if failure_mode==5{proxy.to_str().unwrap()}else{executable},"plan":probe},{"name":"beta","runtime_dir":b,"executable":if lose_response{proxy.to_str().unwrap()}else{executable},"plan":responder}]}).to_string()).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(40),
            Command::new(executable)
                .arg("--runtime-dir")
                .arg(&a)
                .args([
                    "stress",
                    "coordinate",
                    "--manifest",
                    manifest.to_str().unwrap(),
                    "--active",
                    "--output",
                    evidence.to_str().unwrap(),
                ])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        let final_report: Value = serde_json::from_slice(&result.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&result.stderr)));
        assert_eq!(
            result.status.success(),
            !lose_response,
            "mode {failure_mode}: {}; final node states: {final_report}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(final_report["passed"], !lose_response);
        assert_eq!(final_report["one_way_latency"], Value::Null);
        if lose_response {
            assert_eq!(std::fs::read_to_string(&calls).unwrap(), "commit\n");
            for dir in [&a, &b] {
                let list = cli(dir, &["stress", "list"]).await;
                let list: Value = serde_json::from_slice(&list.stdout).unwrap();
                assert_eq!(list["runs"][0]["submitted"], 0);
            }
        } else if failure_mode == 0 {
            assert_eq!(final_report["hosts"].as_array().unwrap().len(), 2);
            assert_eq!(
                final_report["hosts"][0]["status"]["report"]["rtt_report"]["latency"]["samples"],
                5
            );
            assert_eq!(
                final_report["hosts"][1]["status"]["report"]["rtt_report"]["responder_submitted"],
                5
            );
            for host in final_report["hosts"].as_array().unwrap() {
                assert_eq!(host["status"]["report"]["recovery"]["status"], "verified");
            }
        }
        if failure_mode == 6 {
            for host in final_report["hosts"].as_array().unwrap() {
                let report = &host["status"]["report"];
                assert_eq!(report["recovery"]["status"], "verified");
                assert_eq!(report["group"]["flows"].as_array().unwrap().len(), 2);
                assert_eq!(report["submitted"], 11);
            }
        }
        if failure_mode == 8 {
            for host in final_report["hosts"].as_array().unwrap() {
                let r = &host["status"]["report"];
                assert_eq!(r["adaptive"]["selected_fps"], 4);
                assert_eq!(r["adaptive"]["steps"].as_array().unwrap().len(), 3);
                assert_eq!(r["submitted"], 10);
                assert_eq!(r["recovery"]["status"], "verified");
            }
        }
        if failure_mode == 5 {
            assert!(root.join("finished").exists());
        }
        if failure_mode == 4 {
            assert!(final_report["evidence_error"].is_string());
        }
        for line in if failure_mode == 4 {
            String::new()
        } else {
            std::fs::read_to_string(evidence).unwrap()
        }
        .lines()
        {
            let row: Value = serde_json::from_str(line).unwrap();
            assert!(row.to_string().find("\"token\":\"").is_none());
        }
        one.shutdown().await;
        two.shutdown().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_cli_shares_budget_and_stops_one_flow_without_stopping_its_peer() {
    let _isolation = TIMED_CASE.acquire().await.unwrap();
    let root = test_temp_dir().join(format!("ch-b3-cli-{}", uuid::Uuid::new_v4().simple()));
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::new(8192))),
        &root,
        0,
        false,
    )
    .await
    .unwrap();
    let plan = root.join("group.json");
    std::fs::write(&plan,json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":12,"fps":4,"bitrate":500000,"receivers_active":true,"drain_ms":20,"flows":[{"name":"reverse","sender":"virtual:1","receivers":["virtual:0"],"can_id":128,"extended":false,"run_tag":18,"count":12,"fps":4}]}).to_string()).unwrap();
    let validated = cli(
        &root,
        &["stress", "validate", "--plan", plan.to_str().unwrap()],
    )
    .await;
    assert!(
        validated.status.success(),
        "{}",
        String::from_utf8_lossy(&validated.stderr)
    );
    let v: Value = serde_json::from_slice(&validated.stdout).unwrap();
    assert_eq!(v["group_budget"]["peak_transmit_fps"], 8);
    let child_root = root.clone();
    let task = tokio::spawn(async move {
        cli(
            &child_root,
            &[
                "stress",
                "run",
                "--plan",
                plan.to_str().unwrap(),
                "--active",
                "--request-id",
                "b3-group",
            ],
        )
        .await
    });
    let id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let list = cli(&root, &["stress", "list"]).await;
            let value: Value = serde_json::from_slice(&list.stdout).unwrap();
            if let Some(id) = value["runs"][0]["id"].as_str() {
                let status = cli(&root, &["stress", "status", id]).await;
                let r: Value = serde_json::from_slice(&status.stdout).unwrap();
                if r["group"]["flows"].as_array().is_some_and(|flows| {
                    flows.len() == 2
                        && flows
                            .iter()
                            .all(|f| f["report"]["submitted"].as_u64().unwrap_or(0) >= 2)
                }) {
                    break id.to_owned();
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        cli(&root, &["stress", "stop-flow", &id, "reverse"])
            .await
            .status
            .success()
    );
    assert!(
        cli(&root, &["stress", "stop-flow", &id, "reverse"])
            .await
            .status
            .success()
    );
    let out = task.await.unwrap();
    assert!(!out.status.success());
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    let flows = r["group"]["flows"].as_array().unwrap();
    assert_eq!(
        flows.iter().find(|f| f["name"] == "primary").unwrap()["report"]["submitted"],
        12
    );
    assert!(
        flows.iter().find(|f| f["name"] == "reverse").unwrap()["report"]["submitted"]
            .as_u64()
            .unwrap()
            < 12
    );
    assert_ne!(r["verdict"], "pass");
    assert_eq!(r["recovery"]["status"], "verified", "{r}");
    assert!(
        cli(&root, &["stress", "delete", &id])
            .await
            .status
            .success()
    );
    server.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
