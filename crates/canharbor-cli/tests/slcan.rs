// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(unix, feature = "slcan"))]
use std::{
    ffi::CStr,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::DirBuilderExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
struct Service {
    child: Child,
    root: PathBuf,
    quit: Arc<AtomicBool>,
    responder: Option<thread::JoinHandle<()>>,
}
impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGINT);
        }
        let _ = self.child.wait();
        self.quit.store(true, Ordering::Release);
        if let Some(thread) = self.responder.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn cli(root: &std::path::Path, args: &[&str]) -> String {
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
fn explicit_serial_service_supports_cli_discovery_monitor_and_send_without_hardware() {
    serial_service(false);
}
#[test]
fn canable_fd_service_supports_explicit_profile_and_no_ack_firmware() {
    serial_service(true);
}
fn serial_service(canable: bool) {
    let (mut master, mut slave) = (-1, -1);
    let mut name = [0 as libc::c_char; libc::PATH_MAX as usize];
    // SAFETY: live output pointers and a PATH_MAX buffer are supplied to openpty.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let path = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned();
    let fd = master.as_raw_fd();
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let commands = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let log = commands.clone();
    let quit = Arc::new(AtomicBool::new(false));
    let stopped = quit.clone();
    let responder = thread::spawn(move || {
        let mut line = Vec::new();
        let mut bytes = [0; 256];
        while !stopped.load(Ordering::Acquire) {
            match master.read(&mut bytes) {
                Ok(n) => {
                    for &byte in &bytes[..n] {
                        if byte != b'\r' {
                            if line.len() < 139 {
                                line.push(byte);
                            }
                            continue;
                        }
                        log.lock().unwrap().push(line.clone());
                        let response = match line.as_slice() {
                            b"V" if canable => b"custom-build\r".as_slice(),
                            b"O" if canable => b"d5521AC\r",
                            _ if canable => b"",
                            b"V" => b"V0234\r".as_slice(),
                            b"L" => b"\rt5521AC\r",
                            b"F" => b"F00\r",
                            b"t5531BD" => b"z\r",
                            _ => b"\r",
                        };
                        if master.write_all(response).is_err() {
                            return;
                        }
                        line.clear();
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(_) => thread::sleep(Duration::from_millis(2)),
            }
        }
    });
    let root = test_temp_dir().join(format!("ch-sc-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_canharbor"));
    command
        .arg("--runtime-dir")
        .arg(&root)
        .args([
            "serve",
            "--no-web",
            "--slcan",
            &path,
            "--serial-baud",
            "115200",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if canable {
        command.args(["--slcan-profile", "canable-fd"]);
    }
    let child = command.spawn().unwrap();
    let mut service = Service {
        child,
        root,
        quit,
        responder: Some(responder),
    };
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
        assert!(Instant::now() < deadline, "service startup timed out");
        thread::sleep(Duration::from_millis(10));
    }
    let devices: serde_json::Value =
        serde_json::from_str(&cli(&service.root, &["devices", "--json"])).unwrap();
    let device = devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["backend"] == "slcan")
        .unwrap();
    assert_eq!(device["verification"], "hardware_unverified");
    assert_eq!(device["channels"][0]["fd"], canable);
    let channel = device["channels"][0]["id"].as_str().unwrap();
    let mut monitor = vec![
        "monitor",
        "--channel",
        channel,
        "--listen-only",
        "--count",
        "1",
    ];
    if canable {
        monitor.extend(["--fd", "--data-bitrate", "5000000"]);
    }
    let received = cli(&service.root, &monitor);
    let received: serde_json::Value = serde_json::from_str(&received).unwrap();
    assert_eq!(received["frame"]["id"], 0x552);
    assert_eq!(received["direction"], "rx");
    let mut send = vec![
        "send",
        "--channel",
        channel,
        "--id",
        "553",
        "--data",
        "BD",
        "--active",
    ];
    if canable {
        send.extend(["--fd", "--data-bitrate", "5000000"]);
    }
    let receipt: serde_json::Value = serde_json::from_str(&cli(&service.root, &send)).unwrap();
    assert_eq!(receipt["confirmation"], "backend_submitted");
    assert_eq!(
        commands
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with(if canable { b"d553" } else { b"t553" }))
            .count(),
        1
    );
    if canable {
        assert!(commands.lock().unwrap().iter().any(|line| line == b"M1"));
        assert!(commands.lock().unwrap().iter().any(|line| line == b"Y5"));
        assert!(
            !commands
                .lock()
                .unwrap()
                .iter()
                .any(|line| line == b"F" || line == b"P" || line == b"L")
        );
        unsafe {
            libc::kill(service.child.id() as libc::pid_t, libc::SIGINT);
        }
        assert_eq!(
            service.child.wait().unwrap().code(),
            Some(7),
            "unconfirmed TX must remain visible at shutdown"
        );
    }
    drop(slave);
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
