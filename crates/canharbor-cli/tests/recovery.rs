// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use serde_json::Value;
use std::{
    os::unix::fs::DirBuilderExt, path::PathBuf, process::Command, sync::Arc, time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
async fn request_bytes(socket: &mut tokio::net::UnixStream) -> Vec<u8> {
    let mut bytes = vec![];
    let mut chunk = [0; 1024];
    loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 8192);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return bytes;
            }
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_tx_response_is_recovered_by_request_id_without_another_send() {
    lost_response(0).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequence_stops_after_lost_response_and_preserves_request_lookup() {
    lost_response(1).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_task_start_response_is_recovered_without_another_admission() {
    lost_response(2).await;
}
async fn lost_response(mode: u8) {
    let root = PathBuf::from("/tmp").join(format!("ch-recover-{}", uuid::Uuid::new_v4().simple()));
    let proxy_root =
        PathBuf::from("/tmp").join(format!("ch-proxy-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&proxy_root)
        .unwrap();
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &root,
        0,
        false,
    )
    .await
    .unwrap();
    let listener = tokio::net::UnixListener::bind(proxy_root.join("service.sock")).unwrap();
    let upstream_path = canharbor_server::local::socket_path(&root);
    let (saved, recorded) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let mut saved = Some(saved);
        let mut posts = 0;
        for _ in 0..if mode == 1 { 7 } else { 6 } {
            let (mut client, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let bytes = tokio::time::timeout(Duration::from_secs(5), request_bytes(&mut client))
                .await
                .unwrap();
            let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let first = header.lines().next().unwrap();
            let mut words = first.split_whitespace();
            let method = words.next().unwrap();
            let path = words.next().unwrap();
            let mut forward = header
                .lines()
                .filter(|line| !line.to_ascii_lowercase().starts_with("connection:"))
                .collect::<Vec<_>>()
                .join("\r\n")
                .into_bytes();
            forward.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
            forward.extend_from_slice(&bytes[end + 4..]);
            let mut upstream = tokio::net::UnixStream::connect(&upstream_path)
                .await
                .unwrap();
            upstream.write_all(&forward).await.unwrap();
            let mut response = vec![];
            tokio::time::timeout(
                Duration::from_secs(5),
                upstream.take(1024 * 1024).read_to_end(&mut response),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(response.len() < 1024 * 1024);
            if method == "POST" && (path.ends_with("/tx") || path.ends_with("/tasks")) {
                posts += 1;
                assert_eq!(posts, 1, "a lost response caused another TX request");
                let request: Value = serde_json::from_slice(&bytes[end + 4..]).unwrap();
                let body_start = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let receipt: Value = serde_json::from_slice(&response[body_start..]).unwrap();
                assert_eq!(
                    receipt["state"],
                    if mode == 2 { "starting" } else { "submitted" }
                );
                let generation = header
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("x-canharbor-lease-generation")
                            .then(|| v.trim().to_owned())
                    })
                    .unwrap();
                saved
                    .take()
                    .unwrap()
                    .send((
                        receipt,
                        request["client_request_id"].as_str().unwrap().to_owned(),
                        generation,
                    ))
                    .unwrap();
                // The real service has committed the result; discard only its
                // response, preserving the sender's original native identity.
            } else {
                client.write_all(&response).await.unwrap();
            }
        }
        posts
    });
    let directory = proxy_root.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_canharbor"))
            .arg("--runtime-dir")
            .arg(directory)
            .args(if mode == 2 {
                vec![
                    "task",
                    "create",
                    "--channel",
                    "virtual:0",
                    "--id",
                    "123",
                    "--count",
                    "4",
                    "--request-id",
                    "lost-task",
                    "--active",
                ]
            } else if mode == 1 {
                vec![
                    "sequence",
                    "send",
                    "--channel",
                    "virtual:0",
                    "--id",
                    "123",
                    "--run-id",
                    "1",
                    "--count",
                    "4",
                    "--active",
                ]
            } else {
                vec![
                    "send",
                    "--channel",
                    "virtual:0",
                    "--id",
                    "123",
                    "--data",
                    "0102",
                    "--active",
                ]
            })
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if mode == 1 {
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["attempted"], 1);
        assert_eq!(report["unknown"], 1);
        assert_eq!(report["submitted"], 0);
        assert_eq!(report["state"], "outcome_unknown");
        assert_eq!(report["passed"], false);
    } else {
        assert!(output.stdout.is_empty());
    }
    let (receipt, request, generation) = recorded.await.unwrap();
    let session = receipt["session_id"].as_str().unwrap().to_owned();
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains(&request));
    if mode != 2 {
        assert!(message.contains("--request"));
    }
    let directory = proxy_root.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_canharbor"))
            .arg("--runtime-dir")
            .arg(directory)
            .args(if mode == 2 {
                vec![
                    "task",
                    "recover",
                    "--session",
                    &session,
                    "--request",
                    &request,
                    "--generation",
                    &generation,
                ]
            } else {
                vec![
                    "receipt",
                    "--session",
                    &session,
                    "--request",
                    &request,
                    "--generation",
                    &generation,
                ]
            })
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recovered: Value = serde_json::from_slice(&output.stdout).unwrap();
    if mode == 2 {
        assert_eq!(recovered["id"], receipt["id"]);
    } else {
        assert_eq!(recovered, receipt);
    }
    assert_eq!(proxy.await.unwrap(), 1);
    assert!(server.shutdown().await.failures.is_empty());
    let _ = std::fs::remove_dir_all(root);
    std::fs::remove_dir_all(proxy_root).unwrap();
}
