// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use std::{path::PathBuf, sync::Arc};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_wrapper_runs_control_against_the_compiled_c_abi() {
    assert_eq!(canharbor_ffi::canharbor_abi_version(), 1);
    let root = test_temp_dir().join(format!("ch-py-{}", uuid::Uuid::new_v4().simple()));
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &root,
        0,
        false,
    )
    .await
    .unwrap();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let library = deps.join(if cfg!(target_os = "macos") {
        "libcanharbor_ffi.dylib"
    } else if cfg!(windows) {
        "canharbor_ffi.dll"
    } else {
        "libcanharbor_ffi.so"
    });
    let python = std::env::var_os("CANHARBOR_TEST_PYTHON").unwrap_or_else(|| "python3".into());
    for script in ["tests/integration.py", "examples/discover.py"] {
        let result = std::process::Command::new(&python)
            .arg(repository.join("python").join(script))
            .arg(&root)
            .arg(&library)
            .env("PYTHONPATH", repository.join("python/src"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!result.stdout.is_empty());
    }
    for operation in ["monitor", "send", "periodic"] {
        let mut command = std::process::Command::new(&python);
        command
            .arg(repository.join("python/examples/workbench.py"))
            .arg(operation)
            .arg("--runtime-dir")
            .arg(&root)
            .arg("--library")
            .arg(&library)
            .args([
                "--channel",
                "virtual:1",
                "--seconds",
                "1",
                "--count",
                "3",
                "--interval-ms",
                "100",
            ])
            .env("PYTHONPATH", repository.join("python/src"));
        if operation != "monitor" {
            command.arg("--active");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{operation}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert!(server.shutdown().await.failures.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
