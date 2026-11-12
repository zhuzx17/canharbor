// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::{
        fs::DirBuilderExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
struct Peer {
    root: PathBuf,
    worker: Option<std::thread::JoinHandle<Vec<String>>>,
}
impl Peer {
    fn spawn(
        count: usize,
        mut response: impl FnMut(&str) -> Option<(u16, Value)> + Send + 'static,
    ) -> Self {
        let root = test_temp_dir().join(format!("ch-exit-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let listener = UnixListener::bind(root.join("service.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let mut paths = vec![];
            for _ in 0..count {
                let until = Instant::now() + Duration::from_secs(12);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < until, "missing CLI request");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let path = read_request(&mut socket);
                paths.push(path.clone());
                if let Some((status, value)) = response(&path) {
                    let body = if status == 204 {
                        String::new()
                    } else {
                        value.to_string()
                    };
                    let header = format!(
                        "HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes());
                    let _ = socket.write_all(body.as_bytes());
                }
            }
            paths
        });
        Self {
            root,
            worker: Some(worker),
        }
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_canharbor"))
            .arg("--runtime-dir")
            .arg(&self.root)
            .args(args)
            .output()
            .unwrap()
    }
    fn finish(&mut self) -> Vec<String> {
        self.worker.take().unwrap().join().unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn read_request(socket: &mut UnixStream) -> String {
    let mut bytes = vec![];
    let mut chunk = [0; 1024];
    loop {
        let n = socket.read(&mut chunk).unwrap();
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
                return headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
            }
        }
    }
}
fn error(code: &str, unknown: bool) -> Value {
    json!({"error":{"code":code,"message":"synthetic failure","retryable":false,"request_id":"fixture","receipt":unknown.then_some("12345678-1234-4234-8234-123456789abc"),"outcome_unknown":unknown}})
}
fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}
const SEND: &[&str] = &[
    "send",
    "--channel",
    "virtual:0",
    "--id",
    "123",
    "--data",
    "0102",
    "--active",
];
fn flow(path: &str) -> Option<(u16, Value)> {
    Some(if path == "/api/v1/sessions" {
        (201, json!({"id":"s_fixture"}))
    } else if path.ends_with("/lease") {
        (200, json!({"generation":"1"}))
    } else if path.ends_with("/start") {
        (200, json!({}))
    } else {
        (204, Value::Null)
    })
}
#[test]
fn input_and_unavailable_service_have_distinct_statuses() {
    let directory = test_temp_dir().join(format!("ch-absent-{}", uuid::Uuid::new_v4().simple()));
    let output = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&directory)
        .args(["devices", "--json"])
        .output()
        .unwrap();
    assert_code(&output, 3);
    let output = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&directory)
        .args([
            "send",
            "--channel",
            "virtual:0",
            "--id",
            "ZZ",
            "--data",
            "00",
            "--active",
        ])
        .output()
        .unwrap();
    assert_code(&output, 2);
    for args in [
        vec!["serve", "--open", "--no-web"],
        vec!["session", "open", "--channel", "virtual:0"],
        vec!["monitor", "--session", "s_fixture", "--count", "0"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_canharbor"))
            .arg("--runtime-dir")
            .arg(&directory)
            .args(args)
            .output()
            .unwrap();
        assert_code(&output, 2);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&directory)
        .args(["monitor", "--session", "s_fixture"])
        .output()
        .unwrap();
    assert_code(&output, 3);
}
#[test]
fn structured_api_categories_preserve_status_and_stderr_only_errors() {
    for (status, code, exit) in [
        (404, "not_found", 4),
        (409, "lease_conflict", 5),
        (503, "bus_off", 6),
        (429, "queue_full", 8),
        (503, "disk_full", 8),
    ] {
        let mut peer = Peer::spawn(1, move |_| Some((status, error(code, false))));
        let output = peer.cli(&["devices", "--json"]);
        assert_code(&output, exit);
        assert_eq!(peer.finish().len(), 1);
    }
}
#[test]
fn lost_or_unknown_submission_is_not_retried_and_reports_status_seven() {
    for lost in [false, true] {
        let mut peer = Peer::spawn(5, move |path| {
            if path.ends_with("/tx") {
                if lost {
                    None
                } else {
                    Some((503, error("transmit_failed", true)))
                }
            } else {
                flow(path)
            }
        });
        let output = peer.cli(SEND);
        assert_code(&output, 7);
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(message.contains("outcome unknown"));
        assert!(message.contains("s_fixture"));
        if !lost {
            assert!(message.contains("12345678-1234-4234-8234-123456789abc"));
        }
        let paths = peer.finish();
        assert_eq!(paths.iter().filter(|p| p.ends_with("/tx")).count(), 1);
        assert_eq!(paths.last().unwrap(), "/api/v1/sessions/s_fixture");
    }
}
#[test]
fn sigint_during_submission_returns_unknown_after_bounded_cleanup() {
    let (entered, ready) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let mut peer = Peer::spawn(4, move |path| {
        if path.ends_with("/tx") {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
            None
        } else {
            flow(path)
        }
    });
    let child = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&peer.root)
        .args(SEND)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    ready.recv_timeout(Duration::from_secs(4)).unwrap();
    let started = Instant::now();
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let output = child.wait_with_output().unwrap();
    let elapsed = started.elapsed();
    release.send(()).unwrap();
    assert_code(&output, 7);
    assert!(elapsed < Duration::from_secs(6));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interrupted while awaiting submission")
    );
    assert_eq!(
        peer.finish().iter().filter(|p| p.ends_with("/tx")).count(),
        1
    );
}

#[test]
fn client_response_size_limit_is_a_resource_failure() {
    let mut peer = Peer::spawn(1, |_| {
        Some((200, json!({"oversized":"X".repeat(1024*1024)})))
    });
    let output = peer.cli(&["devices", "--json"]);
    assert_code(&output, 8);
    peer.finish();
}

#[test]
fn submission_errors_need_status_or_explicit_metadata_to_establish_a_known_failure() {
    let mut old = error("transmit_failed", false);
    old["error"]
        .as_object_mut()
        .unwrap()
        .remove("outcome_unknown");
    for (status, payload, expected) in [
        (503, json!({}), 7),
        (503, old, 7),
        (503, json!({"error":{"outcome_unknown":false}}), 7),
        (503, error("transmit_failed", false), 6),
        (429, json!({}), 8),
    ] {
        let mut peer = Peer::spawn(5, move |path| {
            if path.ends_with("/tx") {
                Some((status, payload.clone()))
            } else {
                flow(path)
            }
        });
        let output = peer.cli(SEND);
        assert_code(&output, expected);
        let message = String::from_utf8(output.stderr).unwrap();
        if expected == 7 {
            assert!(message.contains("outcome unknown"));
            assert!(message.contains("request "));
            assert!(message.contains("s_fixture"));
            assert!(message.contains("do not resend automatically"));
        }
        assert_eq!(
            peer.finish().iter().filter(|p| p.ends_with("/tx")).count(),
            1
        );
    }
}

#[test]
fn websocket_handshake_preserves_http_failure_categories() {
    for (status, expected) in [(400, 2), (404, 4), (409, 5), (429, 8), (503, 6)] {
        let mut peer = Peer::spawn(1, move |_| Some((status, error("fixture", false))));
        assert_code(&peer.cli(&["monitor", "--session", "s_missing"]), expected);
        assert_eq!(peer.finish().len(), 1);
    }
}

#[test]
fn websocket_transport_loss_is_unavailable_but_invalid_response_is_backend_failure() {
    use tokio_tungstenite::tungstenite::{Message, accept};
    for malformed in [false, true] {
        let peer = Peer::spawn(0, |_| None);
        let listener = UnixListener::bind(peer.root.join("ws.sock")).unwrap();
        // Replace the unused HTTP fixture listener; monitor uses the service socket.
        std::fs::remove_file(peer.root.join("service.sock")).unwrap();
        std::fs::rename(peer.root.join("ws.sock"), peer.root.join("service.sock")).unwrap();
        let worker = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut ws = accept(socket).unwrap();
            let _ = ws.read().unwrap(); // Native subscription.
            if malformed {
                let _ = ws.send(Message::Text("{}".into()));
            }
            // Drop without a close frame, as a service process failure would.
        });
        assert_code(
            &peer.cli(&["monitor", "--session", "s_fixture"]),
            if malformed { 6 } else { 3 },
        );
        worker.join().unwrap();
    }
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
