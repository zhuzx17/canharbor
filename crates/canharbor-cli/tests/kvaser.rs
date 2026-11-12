// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(unix, feature = "kvaser"))]
use std::{
    io::Read,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Service {
    child: Child,
    root: PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGINT);
        }
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn cli(root: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap()
}
#[test]
fn explicit_kvaser_library_runs_through_service_cli_and_native_abi_without_hardware() {
    let root = test_temp_dir().join(format!("ch-kv-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let library = root.join(if cfg!(target_os = "macos") {
        "libmock.dylib"
    } else {
        "libmock.so"
    });
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let compile = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .args(["-fPIC", "-std=c11", "-Werror"])
        .arg("-I")
        .arg(directory.join("../../third-party/macOS-CAN-Drivers/KvaserUSB/include"))
        .arg(directory.join("../canharbor-kvaser/tests/mock.c"))
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let path = library.to_str().unwrap();
    let discovered: serde_json::Value =
        serde_json::from_str(&cli(&root, &["kvaser-list", "--library", path])).unwrap();
    assert_eq!(discovered["channels"][0]["handle"], 0x10200);
    assert_eq!(discovered["channels"][1]["fd"], true);
    let child = Command::new(env!("CARGO_BIN_EXE_canharbor"))
        .arg("--runtime-dir")
        .arg(&root)
        .args([
            "serve",
            "--no-web",
            "--kvaser-library",
            path,
            "--kvaser-channel",
            "0x10200",
            "--kvaser-channel",
            "0x10201",
        ])
        .env("CANHARBOR_KVASER_TEST_RX", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut service = Service { child, root };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !canharbor_server::local::socket_path(&service.root).exists() {
        if let Some(status) = service.child.try_wait().unwrap() {
            let mut error = String::new();
            service
                .child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            panic!("serve exited {status}: {error}");
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let devices: serde_json::Value =
        serde_json::from_str(&cli(&service.root, &["devices", "--json"])).unwrap();
    let devices = devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|device| device["backend"] == "kvaser")
        .collect::<Vec<_>>();
    assert_eq!(devices.len(), 2);
    for device in &devices {
        assert_eq!(device["verification"], "hardware_unverified");
    }
    let classic = devices
        .iter()
        .find(|device| device["channels"][0]["fd"] == false)
        .unwrap()["channels"][0]["id"]
        .as_str()
        .unwrap();
    let fd = devices
        .iter()
        .find(|device| device["channels"][0]["fd"] == true)
        .unwrap()["channels"][0]["id"]
        .as_str()
        .unwrap();
    let frame: serde_json::Value = serde_json::from_str(&cli(
        &service.root,
        &[
            "monitor",
            "--channel",
            classic,
            "--listen-only",
            "--count",
            "1",
        ],
    ))
    .unwrap();
    assert_eq!(frame["frame"]["id"], 0x654);
    assert_eq!(frame["direction"], "rx");
    let receipt: serde_json::Value = serde_json::from_str(&cli(
        &service.root,
        &[
            "send",
            "--channel",
            fd,
            "--id",
            "654",
            "--data",
            "00112233445566778899AABB",
            "--active",
            "--fd",
            "--data-bitrate",
            "2000000",
            "--brs",
        ],
    ))
    .unwrap();
    assert_eq!(receipt["confirmation"], "backend_submitted");
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
