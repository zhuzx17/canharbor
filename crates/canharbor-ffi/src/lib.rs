// SPDX-License-Identifier: AGPL-3.0-or-later
//! Versioned blocking C ABI for the local UDS API. No native USB library is loaded.
use canharbor_client::{ApiFailure, LocalClient};
mod stream;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
pub use stream::{canharbor_stream_close, canharbor_stream_next, canharbor_stream_open};
const OK: i32 = 0;
const INVALID: i32 = 2;
const UNAVAILABLE: i32 = 3;
const NOT_FOUND: i32 = 4;
const CONFLICT: i32 = 5;
const IO: i32 = 6;
const UNKNOWN: i32 = 7;
const LIMIT: i32 = 8;
const INTERNAL: i32 = 9;
struct Reply {
    status: i32,
    bytes: Vec<u8>,
}
enum Operation {
    Json {
        method: String,
        path: String,
        body: Value,
        generation: Option<String>,
        query: Vec<(String, String)>,
    },
    Download {
        capture: uuid::Uuid,
        output: PathBuf,
    },
}
impl Operation {
    fn is_write(&self) -> bool {
        matches!(self, Self::Json { method, .. } if method != "GET")
    }
    async fn execute(self, client: &LocalClient) -> Result<Value, Box<dyn std::error::Error>> {
        match self {
            Self::Json {
                method,
                path,
                body,
                generation,
                query,
            } => {
                let query: Vec<(&str, &str)> = query
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                client
                    .request_with_query(&method, &path, body, generation.as_deref(), &query)
                    .await
            }
            Self::Download { capture, output } => {
                client.download(capture, output).await?;
                Ok(json!({"downloaded":true}))
            }
        }
    }
}
struct Job {
    operation: Operation,
    until: Instant,
    cancelled: Arc<AtomicBool>,
    reply: mpsc::SyncSender<Reply>,
}
struct Client {
    local: LocalClient,
    sender: mpsc::SyncSender<Job>,
    closing: Arc<AtomicBool>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
    }
}
#[derive(Default)]
struct Registry {
    next: u64,
    clients: BTreeMap<u64, Arc<Client>>,
    responses: BTreeMap<u64, Option<Reply>>,
    streams: BTreeMap<u64, Option<Arc<stream::Handle>>>,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
fn next(registry: &mut Registry) -> Result<u64, i32> {
    registry.next = registry.next.checked_add(1).ok_or(LIMIT)?;
    Ok(registry.next)
}
fn boundary(call: impl FnOnce() -> Result<(), i32>) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(Ok(())) => OK,
        Ok(Err(code)) => code,
        Err(_) => INTERNAL,
    }
}
/// # Safety
/// `pointer` must address `length` readable bytes for the duration of the call.
unsafe fn input(pointer: *const u8, length: usize, max: usize) -> Result<String, i32> {
    if length > max || length > isize::MAX as usize || (length != 0 && pointer.is_null()) {
        return Err(INVALID);
    }
    if length == 0 {
        return Ok(String::new());
    }
    // SAFETY: caller provides readable memory, checked non-NULL and bounded length.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| INVALID)
}
fn failure(status: i32, message: &str) -> Reply {
    Reply{status,bytes:serde_json::to_vec(&json!({"error":{"code":if status==UNKNOWN{"outcome_unknown"}else{"client_error"},"message":message,"retryable":false}})).unwrap()}
}
fn spawn(directory: PathBuf) -> Result<Arc<Client>, i32> {
    let (sender, receiver) = mpsc::sync_channel::<Job>(4);
    let closing = Arc::new(AtomicBool::new(false));
    let stopped = closing.clone();
    let (ready, opened) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("canharbor-c-client".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(_) => {
                    let _ = ready.send(Err(INTERNAL));
                    return;
                }
            };
            let client = match LocalClient::new(directory) {
                Ok(c) => c,
                Err(_) => {
                    let _ = ready.send(Err(UNAVAILABLE));
                    return;
                }
            };
            if ready.send(Ok(client.clone())).is_err() {
                return;
            }
            while let Ok(job) = receiver.recv() {
                if stopped.load(Ordering::Acquire)
                    || job.cancelled.load(Ordering::Acquire)
                    || Instant::now() >= job.until
                {
                    let _ = job.reply.send(failure(
                        CONFLICT,
                        "queued request was cancelled before execution",
                    ));
                    continue;
                }
                let is_write = job.operation.is_write();
                let reply = runtime.block_on(async {
                    match tokio::time::timeout_at(
                        tokio::time::Instant::from_std(job.until),
                        job.operation.execute(&client),
                    )
                    .await
                    {
                        Ok(Ok(value)) => Reply {
                            status: OK,
                            bytes: serde_json::to_vec(&value).unwrap(),
                        },
                        Ok(Err(error)) => {
                            if let Some(api) = error.downcast_ref::<ApiFailure>() {
                                let status = if is_write && api.submission_is_unknown() {
                                    UNKNOWN
                                } else {
                                    match api.status {
                                        404 => NOT_FOUND,
                                        409 | 412 => CONFLICT,
                                        429 | 507 => LIMIT,
                                        400 | 401 | 403 | 405 | 413 | 414 | 415 | 422 | 428
                                        | 431 => INVALID,
                                        _ => IO,
                                    }
                                };
                                Reply {
                                    status,
                                    bytes: serde_json::to_vec(
                                        &json!({"http_status":api.status,"error":api.detail}),
                                    )
                                    .unwrap(),
                                }
                            } else {
                                failure(
                                    if is_write {
                                        UNKNOWN
                                    } else if error.is::<canharbor_client::ResourceLimit>() {
                                        LIMIT
                                    } else if error.is::<canharbor_client::ServiceUnavailable>() {
                                        UNAVAILABLE
                                    } else if let Some(io) = error.downcast_ref::<std::io::Error>()
                                    {
                                        match io.kind() {
                                            std::io::ErrorKind::AlreadyExists => CONFLICT,
                                            std::io::ErrorKind::StorageFull
                                            | std::io::ErrorKind::QuotaExceeded => LIMIT,
                                            _ => IO,
                                        }
                                    } else {
                                        IO
                                    },
                                    &error.to_string(),
                                )
                            }
                        }
                        Err(_) => failure(
                            if !is_write { UNAVAILABLE } else { UNKNOWN },
                            "request deadline exceeded; do not retry a write automatically",
                        ),
                    }
                });
                let _ = job.reply.send(reply);
            }
        })
        .map_err(|_| INTERNAL)?;
    let local = opened
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| UNAVAILABLE)??;
    Ok(Arc::new(Client {
        sender,
        closing,
        local,
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn canharbor_abi_version() -> u32 {
    1
}
/// # Safety
/// Directory bytes must be readable; `output` must be a writable u64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canharbor_client_open(
    directory: *const u8,
    length: usize,
    output: *mut u64,
) -> i32 {
    boundary(|| {
        if output.is_null() {
            return Err(INVALID);
        }
        // SAFETY: required writable output; initialize even on a subsequent error.
        unsafe { output.write(0) };
        // SAFETY: input memory contract is forwarded from caller.
        let directory = unsafe { input(directory, length, 4096) }?;
        if directory.is_empty()
            || directory.contains('\0')
            || !PathBuf::from(&directory).is_absolute()
        {
            return Err(INVALID);
        }
        let mut r = registry().lock().map_err(|_| INTERNAL)?;
        if r.clients.len() >= 32 {
            return Err(LIMIT);
        }
        let client = spawn(directory.into())?;
        let id = next(&mut r)?;
        r.clients.insert(id, client);
        // SAFETY: validated required writable output.
        unsafe { output.write(id) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn canharbor_client_close(client: u64) -> i32 {
    boundary(|| {
        let mut r = registry().lock().map_err(|_| INTERNAL)?;
        let c = r.clients.remove(&client).ok_or(NOT_FOUND)?;
        r.streams.retain(|_, entry| {
            if let Some(s) = entry.as_ref().filter(|s| s.parent == client) {
                s.stop();
                false
            } else {
                true
            }
        });
        c.closing.store(true, Ordering::Release);
        Ok(())
    })
}
/// # Safety
/// Inputs must be readable for their lengths, and `output` writable. All buffers
/// must remain valid throughout the call and not overlap writable output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canharbor_request(
    client: u64,
    method: *const u8,
    method_length: usize,
    path: *const u8,
    path_length: usize,
    body: *const u8,
    body_length: usize,
    generation: *const u8,
    generation_length: usize,
    output: *mut u64,
) -> i32 {
    // SAFETY: forwards the caller's validated pointer contract, with no query.
    unsafe {
        canharbor_request_with_query(
            client,
            method,
            method_length,
            path,
            path_length,
            body,
            body_length,
            generation,
            generation_length,
            std::ptr::null(),
            0,
            output,
        )
    }
}
/// # Safety
/// Inputs must be readable for their lengths, and `output` writable. All buffers
/// must remain valid throughout the call and not overlap writable output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canharbor_request_with_query(
    client: u64,
    method: *const u8,
    method_length: usize,
    path: *const u8,
    path_length: usize,
    body: *const u8,
    body_length: usize,
    generation: *const u8,
    generation_length: usize,
    query: *const u8,
    query_length: usize,
    output: *mut u64,
) -> i32 {
    boundary(|| {
        if output.is_null() {
            return Err(INVALID);
        }
        // SAFETY: required writable output and input pointers as specified above.
        unsafe { output.write(0) };
        let (method, path, body, generation) = unsafe {
            (
                input(method, method_length, 8)?,
                input(path, path_length, 4096)?,
                input(body, body_length, 1024 * 1024)?,
                input(generation, generation_length, 20)?,
            )
        };
        // SAFETY: query has the same input pointer contract as the other fields.
        let raw_query = unsafe { input(query, query_length, 16384)? };
        let query: Vec<(String, String)> = if raw_query.is_empty() {
            vec![]
        } else {
            serde_json::from_str(&raw_query).map_err(|_| INVALID)?
        };
        if query.len() > 16
            || query
                .iter()
                .map(|(k, v)| k.len().saturating_add(v.len()))
                .sum::<usize>()
                > 4096
        {
            return Err(INVALID);
        }
        canharbor_client::validate_api_path(&path).map_err(|_| INVALID)?;
        if !matches!(method.as_str(), "GET" | "POST" | "PATCH" | "DELETE") {
            return Err(INVALID);
        }
        if !generation.is_empty()
            && (!generation.bytes().all(|b| b.is_ascii_digit())
                || generation.parse::<u64>().is_err())
        {
            return Err(INVALID);
        }
        let body = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&body).map_err(|_| INVALID)?
        };
        let id = dispatch(
            client,
            Operation::Json {
                method,
                path,
                body,
                generation: (!generation.is_empty()).then_some(generation),
                query,
            },
        )?;
        // SAFETY: required writable output remains live throughout the call.
        unsafe { output.write(id) };
        Ok(())
    })
}
fn dispatch(client: u64, operation: Operation) -> Result<u64, i32> {
    let is_write = operation.is_write();
    let (c, id) = {
        let mut r = registry().lock().map_err(|_| INTERNAL)?;
        let c = r.clients.get(&client).cloned().ok_or(NOT_FOUND)?;
        if r.responses.len() >= 64 {
            return Err(LIMIT);
        }
        let id = next(&mut r)?;
        r.responses.insert(id, None);
        (c, id)
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let (send, receive) = mpsc::sync_channel(1);
    let job = Job {
        operation,
        until: Instant::now() + Duration::from_secs(10),
        cancelled: cancelled.clone(),
        reply: send,
    };
    if c.closing.load(Ordering::Acquire) || c.sender.try_send(job).is_err() {
        registry()
            .lock()
            .map_err(|_| INTERNAL)?
            .responses
            .remove(&id);
        return Err(LIMIT);
    }
    let reply = match receive.recv_timeout(Duration::from_secs(11)) {
        Ok(reply) => reply,
        Err(_) => {
            cancelled.store(true, Ordering::Release);
            failure(
                if is_write { UNKNOWN } else { UNAVAILABLE },
                "response unavailable; do not retry uncertain writes",
            )
        }
    };
    registry()
        .lock()
        .map_err(|_| INTERNAL)?
        .responses
        .insert(id, Some(reply));
    Ok(id)
}
/// # Safety
/// Inputs must be readable for their lengths; response must be writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canharbor_capture_download(
    client: u64,
    capture: *const u8,
    capture_length: usize,
    destination: *const u8,
    destination_length: usize,
    response: *mut u64,
) -> i32 {
    boundary(|| {
        if response.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller supplies writable response storage and readable inputs.
        unsafe { response.write(0) };
        let capture = unsafe { input(capture, capture_length, 36)? }
            .parse::<uuid::Uuid>()
            .map_err(|_| INVALID)?;
        let output = PathBuf::from(unsafe { input(destination, destination_length, 4096)? });
        if !output.is_absolute()
            || output.file_name().is_none()
            || output.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(INVALID);
        }
        let id = dispatch(client, Operation::Download { capture, output })?;
        // SAFETY: required output remains valid for the duration of the call.
        unsafe { response.write(id) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn canharbor_response_status(response: u64) -> i32 {
    match registry().lock() {
        Ok(r) => r
            .responses
            .get(&response)
            .and_then(Option::as_ref)
            .map(|v| v.status)
            .unwrap_or(NOT_FOUND),
        Err(_) => INTERNAL,
    }
}
/// # Safety
/// `needed` must be writable. A nonzero capacity requires writable, disjoint
/// `buffer` memory of that capacity. No Rust-owned pointer is returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canharbor_response_copy(
    response: u64,
    buffer: *mut u8,
    capacity: usize,
    needed: *mut usize,
) -> i32 {
    boundary(|| {
        if needed.is_null() || capacity > isize::MAX as usize || (capacity > 0 && buffer.is_null())
        {
            return Err(INVALID);
        }
        // SAFETY: required writable length output.
        unsafe { needed.write(0) };
        let r = registry().lock().map_err(|_| INTERNAL)?;
        let reply = r
            .responses
            .get(&response)
            .and_then(Option::as_ref)
            .ok_or(NOT_FOUND)?;
        // SAFETY: caller guarantees valid writable output length pointer.
        unsafe { needed.write(reply.bytes.len()) };
        if capacity < reply.bytes.len() {
            return Err(LIMIT);
        }
        if !reply.bytes.is_empty() {
            // SAFETY: checked capacity, caller guarantees writable disjoint buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(reply.bytes.as_ptr(), buffer, reply.bytes.len())
            };
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn canharbor_response_close(response: u64) -> i32 {
    boundary(|| {
        let mut r = registry().lock().map_err(|_| INTERNAL)?;
        if !r.responses.get(&response).is_some_and(Option::is_some) {
            return Err(NOT_FOUND);
        }
        r.responses.remove(&response);
        Ok(())
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::{Read, Write};
    #[cfg(unix)]
    #[test]
    fn close_cancels_queued_calls_without_repeating_inflight_io() {
        let directory = std::env::temp_dir().join(format!("ch-c-close-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(directory.join("service.sock")).unwrap();
        let (accepted, ready) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            let mut request = vec![];
            while !request.ends_with(b"{}") {
                let length = socket.read(&mut buf).unwrap();
                assert!(length > 0);
                request.extend_from_slice(&buf[..length]);
                assert!(request.len() < 8192);
            }
            accepted.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
            listener.set_nonblocking(true).unwrap();
            listener
        });
        let path = directory.to_str().unwrap();
        let mut client = 0;
        assert_eq!(
            unsafe { canharbor_client_open(path.as_ptr(), path.len(), &mut client) },
            OK
        );
        let invoke = move || {
            let mut response = 0;
            let code = unsafe {
                canharbor_request(
                    client,
                    b"POST".as_ptr(),
                    4,
                    b"/version".as_ptr(),
                    8,
                    b"{}".as_ptr(),
                    2,
                    std::ptr::null(),
                    0,
                    &mut response,
                )
            };
            (code, response)
        };
        let first = std::thread::spawn(invoke);
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let pending = *registry().lock().unwrap().responses.keys().next().unwrap();
        assert_eq!(canharbor_response_close(pending), NOT_FOUND);

        let selected = registry().lock().unwrap().clients[&client].clone();
        let mut replies = vec![];
        for _ in 0..4 {
            let (reply, receive) = mpsc::sync_channel(1);
            selected
                .sender
                .try_send(Job {
                    operation: Operation::Json {
                        query: vec![],
                        method: "POST".into(),
                        path: "/version".into(),
                        body: json!({}),
                        generation: None,
                    },
                    until: Instant::now() + Duration::from_secs(10),
                    cancelled: Arc::new(AtomicBool::new(false)),
                    reply,
                })
                .unwrap();
            replies.push(receive);
        }
        let (code, response) = invoke();
        assert_eq!(code, LIMIT);
        assert_eq!(response, 0);
        assert_eq!(canharbor_client_close(client), OK);
        release.send(()).unwrap();
        let (code, response) = first.join().unwrap();
        assert_eq!(code, OK);
        assert_eq!(canharbor_response_status(response), OK);
        canharbor_response_close(response);
        for reply in replies {
            assert_eq!(
                reply.recv_timeout(Duration::from_secs(2)).unwrap().status,
                CONFLICT
            );
        }
        drop(selected);
        let listener = peer.join().unwrap();
        assert!(
            matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock)
        );
        drop(listener);
        assert!(registry().lock().unwrap().responses.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
