// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Service {
    child: Child,
    root: PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            #[cfg(windows)]
            let _ = self.child.kill();
            #[cfg(unix)]
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGINT)
            };
        }
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn cli(root: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn a_new_cli_process_can_inspect_a_receipt_after_send_closes_its_session() {
    let root = test_temp_dir().join(format!("ch-receipt-{}", uuid::Uuid::new_v4().simple()));
    let child = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&root)
        .args(["serve", "--no-web"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut service = Service { child, root };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&service.root)
        .arg("devices")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        assert!(
            service.child.try_wait().unwrap().is_none(),
            "service exited"
        );
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let report = cli(&service.root, &["doctor", "--devices"]);
    assert_eq!(report["report_version"], "1");
    assert_eq!(report["channels"][0]["verification"], "simulation_only");
    assert_eq!(report["channels"][0]["communication_verified"], false);
    assert!(
        report["channels"][0]["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let occupied = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&service.root)
        .args(["serve", "--no-web"])
        .output()
        .unwrap();
    assert_eq!(occupied.status.code(), Some(5));
    assert!(occupied.stdout.is_empty());
    assert!(String::from_utf8_lossy(&occupied.stderr).contains("already running"));
    let submitted = cli(
        &service.root,
        &[
            "send",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--data",
            "0102",
            "--active",
        ],
    );
    let session = submitted["session_id"].as_str().unwrap();
    let receipt = submitted["receipt"].as_str().unwrap();
    let queried = cli(
        &service.root,
        &["receipt", "--session", session, "--receipt", receipt],
    );
    assert_eq!(submitted, queried);
    assert_eq!(queried["state"], "submitted");
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
