// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
fn command(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_canharbor"));
    c.arg("--runtime-dir")
        .arg(root)
        .args(args)
        .kill_on_drop(true);
    c
}
async fn output(root: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(Duration::from_secs(25), command(root, args).output())
        .await
        .unwrap()
        .unwrap()
}
async fn setup() -> (PathBuf, RunningServer) {
    let root = test_temp_dir().join(format!("ch-task-cli-{}", uuid::Uuid::new_v4().simple()));
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &root,
        0,
        false,
    )
    .await
    .unwrap();
    (root, server)
}
fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {} / {}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_query_and_recover_keep_results_without_restarting_transmission() {
    let (root, server) = setup().await;
    let out = output(
        &root,
        &[
            "task",
            "create",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--data",
            "0102",
            "--count",
            "1",
            "--interval-ms",
            "1000",
            "--active",
            "--request-id",
            "cli-once",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let task = json(&out);
    assert_eq!(task["submitted"], "1");
    let shown = output(&root, &["task", "show", task["id"].as_str().unwrap()]).await;
    assert_eq!(json(&shown)["id"], task["id"]);
    let found = output(
        &root,
        &[
            "task",
            "recover",
            "--session",
            task["session_id"].as_str().unwrap(),
            "--request",
            "cli-once",
            "--generation",
            task["generation"].as_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(json(&found)["id"], task["id"]);
    assert!(server.state.runtime.session_snapshot().await.is_empty());
    assert!(output(&root, &["task", "list"]).await.status.success());
    assert!(server.shutdown().await.failures.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_cli_can_cancel_and_sigint_reports_the_final_task_state() {
    for signal in [false, cfg!(unix)] {
        let (root, server) = setup().await;
        let mut child = command(
            &root,
            &[
                "task",
                "create",
                "--channel",
                "virtual:0",
                "--id",
                "123",
                "--count",
                "2",
                "--interval-ms",
                "60000",
                "--active",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let ready = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let id = ready
            .split_whitespace()
            .nth(1)
            .unwrap()
            .trim_end_matches(':')
            .to_owned();
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "{ready}");
        child.stderr = Some(lines.into_inner().into_inner());
        if signal {
            #[cfg(unix)]
            assert_eq!(
                unsafe { libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGINT) },
                0
            );
        } else {
            assert!(
                output(&root, &["task", "cancel", &id])
                    .await
                    .status
                    .success()
            );
        }
        // Cancellation observation and session cleanup are separate bounded
        // requests (up to 3s + the client's 10s request deadline). Match the
        // complete-command budget instead of racing either phase at 10s.
        let out = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
            .await
            .unwrap_or_else(|error| {
                panic!("task {id} did not exit after cancellation (signal={signal}): {error}")
            })
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(6),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(json(&out)["state"], "cancelled");
        assert!(server.state.runtime.session_snapshot().await.is_empty());
        server.shutdown().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_task_renews_existing_control_across_lease_expiry() {
    let (root, server) = setup().await;
    let out = output(
        &root,
        &[
            "task",
            "create",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--count",
            "2",
            "--interval-ms",
            "16000",
            "--active",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json(&out)["submitted"], "2");
    assert!(server.state.runtime.session_snapshot().await.is_empty());
    server.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_plans_and_missing_active_opt_in_do_not_open_devices() {
    let (root, server) = setup().await;
    for args in [
        vec!["task", "create", "--channel", "virtual:0", "--id", "123"],
        vec![
            "task",
            "create",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--active",
            "--count",
            "6000",
            "--interval-ms",
            "1000",
        ],
    ] {
        assert_eq!(output(&root, &args).await.status.code(), Some(2));
        assert!(server.state.runtime.session_snapshot().await.is_empty());
    }
    server.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_setup_and_admission_responses_do_not_delay_original_lease_renewals() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (root, server) = setup().await;
    let proxy_root =
        test_temp_dir().join(format!("ch-task-slow-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&proxy_root).unwrap();
    let listener = tokio::net::UnixListener::bind(proxy_root.join("service.sock")).unwrap();
    let held = Arc::new(AtomicBool::new(false));
    let renewed_during_setup = Arc::new(AtomicBool::new(false));
    let admissions = Arc::new(AtomicUsize::new(0));
    let held_proxy = held.clone();
    let renewed = renewed_during_setup.clone();
    let admitted = admissions.clone();
    let socket = root.join("service.sock");
    let proxy = tokio::spawn(async move {
        let mut jobs = tokio::task::JoinSet::new();
        loop {
            let (mut downstream, _) = listener.accept().await.unwrap();
            let socket = socket.clone();
            let held = held_proxy.clone();
            let renewed = renewed.clone();
            let admitted = admitted.clone();
            jobs.spawn(async move {
                let mut bytes = vec![];
                let mut part = [0; 4096];
                let end = loop {
                    let n = downstream.read(&mut part).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&part[..n]);
                    assert!(bytes.len() < 16384);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&bytes[..end]).unwrap();
                        let length = header
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break end;
                        }
                    }
                };
                let header = std::str::from_utf8(&bytes[..end]).unwrap();
                let line = header.lines().next().unwrap();
                let start = line.starts_with("POST ") && line.contains("/start ");
                let task = line.starts_with("POST ") && line.contains("/tasks ");
                if start {
                    held.store(true, Ordering::Release);
                }
                if task {
                    admitted.fetch_add(1, Ordering::AcqRel);
                }
                if line.contains("/lease/renew ") && held.load(Ordering::Acquire) {
                    renewed.store(true, Ordering::Release);
                }
                let mut forward = header
                    .lines()
                    .filter(|l| !l.to_ascii_lowercase().starts_with("connection:"))
                    .collect::<Vec<_>>()
                    .join("\r\n")
                    .into_bytes();
                forward.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
                forward.extend_from_slice(&bytes[end + 4..]);
                let mut upstream = tokio::net::UnixStream::connect(socket).await.unwrap();
                upstream.write_all(&forward).await.unwrap();
                let mut response = vec![];
                upstream
                    .take(1024 * 1024)
                    .read_to_end(&mut response)
                    .await
                    .unwrap();
                if start {
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    held.store(false, Ordering::Release);
                }
                if task {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                let _ = downstream.write_all(&response).await;
            });
            while let Some(result) = jobs.try_join_next() {
                result.unwrap();
            }
        }
    });
    let out = output(
        &proxy_root,
        &[
            "task",
            "create",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--count",
            "2",
            "--interval-ms",
            "10000",
            "--active",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json(&out)["submitted"], "2");
    assert!(
        renewed_during_setup.load(Ordering::Acquire),
        "no renewal occurred while startup response was held"
    );
    assert_eq!(admissions.load(Ordering::Acquire), 1);
    proxy.abort();
    let _ = proxy.await;
    assert!(server.shutdown().await.failures.is_empty());
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(proxy_root).unwrap();
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
