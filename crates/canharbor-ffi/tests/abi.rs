// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_ffi::*;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn open(path: &str) -> u64 {
    let mut handle = 0;
    assert_eq!(
        unsafe { canharbor_client_open(path.as_ptr(), path.len(), &mut handle) },
        0
    );
    handle
}
fn request(client: u64, method: &str, path: &str, body: Value, generation: &str) -> u64 {
    let body = body.to_string();
    let mut response = 0;
    assert_eq!(
        unsafe {
            canharbor_request(
                client,
                method.as_ptr(),
                method.len(),
                path.as_ptr(),
                path.len(),
                body.as_ptr(),
                body.len(),
                generation.as_ptr(),
                generation.len(),
                &mut response,
            )
        },
        0
    );
    response
}
fn read(response: u64) -> Value {
    let mut size = 0;
    assert_eq!(
        unsafe { canharbor_response_copy(response, std::ptr::null_mut(), 0, &mut size) },
        8
    );
    let mut bytes = vec![0; size];
    assert_eq!(
        unsafe { canharbor_response_copy(response, bytes.as_mut_ptr(), bytes.len(), &mut size) },
        0
    );
    serde_json::from_slice(&bytes).unwrap()
}
fn call(client: u64, method: &str, path: &str, body: Value, generation: &str) -> Value {
    let response = request(client, method, path, body, generation);
    assert_eq!(canharbor_response_status(response), 0);
    let value = read(response);
    assert_eq!(canharbor_response_close(response), 0);
    assert_eq!(canharbor_response_close(response), 4);
    value
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_handles_support_control_errors_and_bounded_responses() {
    let _serial = SERIAL.lock().await;
    let root = test_temp_dir().join(format!("ch-ffi-{}", uuid::Uuid::new_v4().simple()));
    let bus = Arc::new(VirtualBus::default());
    let server = RunningServer::start(Runtime::new(bus.clone()), &root, 0, false)
        .await
        .unwrap();
    let path = root.to_str().unwrap().to_owned();
    tokio::task::spawn_blocking(move||{
        assert_eq!(canharbor_abi_version(),1);let client=open(&path);
        let created=call(client,"POST","/sessions",json!({"channel_id":"virtual:0","config":{"mode":"active","nominal_bitrate":500000}}),"");let id=created["id"].as_str().unwrap();
        let acquired=call(client,"POST",&format!("/sessions/{id}/lease"),json!({}),"");let g=acquired["generation"].as_str().unwrap();
        call(client,"POST",&format!("/sessions/{id}/start"),json!({}),g);
        let tx=json!({"client_request_id":"ffi-once","frame":{"kind":"classic_data","id":291,"data_hex":"0102"}});
        let first=call(client,"POST",&format!("/sessions/{id}/tx"),tx.clone(),g);let second=call(client,"POST",&format!("/sessions/{id}/tx"),tx,g);assert_eq!(first["receipt"],second["receipt"]);
        let error=request(client,"POST",&format!("/sessions/{id}/tx"),json!({}),g);assert_eq!(canharbor_response_status(error),2);assert!(read(error)["error"].is_object());canharbor_response_close(error);
        call(client,"POST",&format!("/sessions/{id}/stop"),json!({}),g);
        call(client,"POST",&format!("/sessions/{id}/start"),json!({}),g);
        bus.inject(canharbor_virtual::Fault::FailNextTransmit);
        let failed=request(client,"POST",&format!("/sessions/{id}/tx"),json!({"client_request_id":"known-failure","frame":{"kind":"classic_data","id":1,"data_hex":"00"}}),g);
        assert_eq!(canharbor_response_status(failed),6);assert_eq!(read(failed)["error"]["outcome_unknown"],false);canharbor_response_close(failed);

        let mut responses=vec![];for _ in 0..64{responses.push(request(client,"GET","/version",Value::Null,""));}
        let mut handle=9;assert_eq!(unsafe{canharbor_request(client,b"GET".as_ptr(),3,b"/version".as_ptr(),8,std::ptr::null(),0,std::ptr::null(),0,&mut handle)},8);assert_eq!(handle,0);
        assert_eq!(canharbor_client_close(client),0);assert_eq!(canharbor_client_close(client),4);
        // Response ownership is independent of the closed client.
        assert_eq!(read(responses[0])["api_version"],"1.0");for response in responses{assert_eq!(canharbor_response_close(response),0);}
        let other=open(&path);assert!(other>client);canharbor_client_close(other);
        let mut out=7;assert_eq!(unsafe{canharbor_client_open(std::ptr::null(),1,&mut out)},2);assert_eq!(out,0);assert_eq!(unsafe{canharbor_client_open([255u8].as_ptr(),1,&mut out)},2);
    }).await.unwrap();
    assert!(server.shutdown().await.failures.is_empty());
    let _ = std::fs::remove_dir_all(root);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_header_and_example_link_and_run_against_the_shared_library() {
    let _serial = SERIAL.lock().await;
    let root = test_temp_dir().join(format!("ch-c-{}", uuid::Uuid::new_v4().simple()));
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &root,
        0,
        false,
    )
    .await
    .unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let binary = root.join(if cfg!(windows) {
        "example.exe"
    } else {
        "example"
    });
    let mut command = std::process::Command::new(if cfg!(windows) { "clang" } else { "cc" });
    command
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("-I")
        .arg(manifest.join("../../include"))
        .arg(manifest.join("examples/local-client.c"));
    if cfg!(windows) {
        command
            .arg("--target=x86_64-pc-windows-msvc")
            .arg(deps.join("canharbor_ffi.dll.lib"));
    } else {
        command
            .arg("-L")
            .arg(&deps)
            .arg("-lcanharbor_ffi")
            .arg(format!("-Wl,-rpath,{}", deps.display()));
    }
    let output = command.arg("-o").arg(&binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut paths = vec![deps];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = std::process::Command::new(binary)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let records = stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["api_version"], "1.0");
    assert_eq!(
        records[1]["devices"][0]["channels"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(server.shutdown().await.failures.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn locally_invalid_paths_are_not_reported_as_unavailable_or_unknown() {
    let _serial = SERIAL.blocking_lock();
    let absent = test_temp_dir().join("canharbor-invalid-path-no-service");
    let client = open(absent.to_str().unwrap());
    for method in ["GET", "POST"] {
        for path in [
            "sessions",
            "/sessions?limit=1",
            "/sessions#fragment",
            "/../sessions",
        ] {
            let mut response = 77;
            assert_eq!(
                unsafe {
                    canharbor_request(
                        client,
                        method.as_ptr(),
                        method.len(),
                        path.as_ptr(),
                        path.len(),
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                        &mut response,
                    )
                },
                2
            );
            assert_eq!(response, 0);
        }
    }
    assert_eq!(canharbor_client_close(client), 0);
}

#[test]
fn query_extension_rejects_invalid_or_oversized_parameters_before_io() {
    let _serial = SERIAL.blocking_lock();
    let root = test_temp_dir().join(format!("ch-query-{}", uuid::Uuid::new_v4().simple()));
    let client = open(root.to_str().unwrap());
    for query in [
        json!({"not":"pairs"}),
        json!([["key", 1]]),
        json!((0..17).map(|i| (i.to_string(), "")).collect::<Vec<_>>()),
        json!([["key", "X".repeat(4096)]]),
    ] {
        let query = query.to_string();
        let mut response = 99;
        assert_eq!(
            unsafe {
                canharbor_request_with_query(
                    client,
                    b"GET".as_ptr(),
                    3,
                    b"/version".as_ptr(),
                    8,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    0,
                    query.as_ptr(),
                    query.len(),
                    &mut response,
                )
            },
            2
        );
        assert_eq!(response, 0);
    }
    assert_eq!(canharbor_client_close(client), 0);
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
