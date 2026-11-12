// SPDX-License-Identifier: AGPL-3.0-or-later
//! Service-owned captures with bounded storage and explicit ownership.
use canharbor_capture::{Header, Reason, Report, Writer};
use canharbor_core::runtime::{Notice, Observation, Runtime};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, OnceCell, OwnedSemaphorePermit, Semaphore, watch};
use uuid::Uuid;
const MAX_FILES: usize = 16;
const MAX_ACTIVE: usize = 4;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Starting,
    Recording,
    Completed,
    Incomplete,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Info {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub created_unix_ms: Option<String>,
    pub id: String,
    pub session_id: String,
    pub state: Status,
    pub events: u64,
    pub dropped: u64,
    #[serde(default)]
    pub loss_count_known: bool,
    pub bytes: u64,
    pub reason: Option<Reason>,
    pub expires_unix_ms: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Disk {
    owner: String,
    request_id: String,
    #[serde(default)]
    requested_name: Option<String>,
    info: Info,
}
#[derive(Clone, Copy)]
struct Stop {
    cutoff: u64,
    reason: Option<Reason>,
}
struct Entry {
    disk: Arc<Mutex<Disk>>,
    stop: Option<watch::Sender<Option<Stop>>>,
    observation: Option<Observation>,
    task: Option<tokio::task::JoinHandle<()>>,
    done: watch::Receiver<bool>,
    crashed: Arc<AtomicBool>,
}
struct Completion {
    done: watch::Sender<bool>,
    crashed: Arc<AtomicBool>,
    normal: bool,
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.crashed.store(!self.normal, Ordering::Release);
        self.done.send_replace(true);
    }
}
fn refresh_failure(entry: &Entry, disk: &mut Disk) {
    if *entry.done.borrow() && entry.crashed.load(Ordering::Acquire) {
        disk.info.state = Status::Failed;
        disk.info.reason = Some(Reason::OwnerDropped);
        disk.info.loss_count_known = false;
    }
}
struct Inner {
    runtime: Arc<Runtime>,
    root: PathBuf,
    instance: String,
    entries: Mutex<BTreeMap<String, Entry>>,
    initialized: OnceCell<()>,
    closing: AtomicBool,
    downloads: Arc<Semaphore>,
}
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("capture not found")]
    Missing,
    #[error("capture resource limit")]
    Limit,
    #[error("capture operation invalid in current state")]
    State,
    #[error("capture service is closing")]
    Closing,
    #[error("capture storage is full")]
    DiskFull,
    #[error("capture storage unavailable")]
    Storage,
    #[error("capture worker terminated unexpectedly")]
    WorkerFailed,
    #[error("capture request conflicts with an existing capture")]
    Conflict,
}
pub fn download_filename(info: &Info) -> String {
    fn component(text: &str) -> String {
        let value: String = text
            .chars()
            .take(48)
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if value.is_empty() {
            "recording".into()
        } else {
            value
        }
    }
    format!(
        "{}_{}_{}_{}.ndjson",
        component(&info.name),
        component(info.channel_id.as_deref().unwrap_or("unknown-channel")),
        component(info.created_unix_ms.as_deref().unwrap_or("unknown-time")),
        info.id
    )
}
pub fn download_disposition(info: &Info) -> String {
    fn component(text: &str) -> String {
        let value: String = text
            .chars()
            .take(48)
            .map(|c| {
                if c.is_control()
                    || matches!(c, '/' | '\\' | '"' | ':' | '*' | '?' | '<' | '>' | '|')
                {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let mut end = value.len().min(48);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        if value.is_empty() {
            "recording".into()
        } else {
            value[..end].into()
        }
    }
    let unicode = format!(
        "{}_{}_{}_{}.ndjson",
        component(&info.name),
        component(info.channel_id.as_deref().unwrap_or("unknown-channel")),
        component(info.created_unix_ms.as_deref().unwrap_or("unknown-time")),
        info.id
    );
    let mut encoded = String::new();
    use std::fmt::Write;
    for byte in unicode.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{encoded}",
        download_filename(info)
    )
}
fn io_error(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::StorageFull {
        Error::DiskFull
    } else {
        Error::Storage
    }
}
fn writer_error(error: canharbor_capture::Error) -> Error {
    match error {
        canharbor_capture::Error::Io(error) => io_error(error),
        _ => Error::Storage,
    }
}
fn runtime_error(error: canharbor_core::runtime::RuntimeError) -> Error {
    match error {
        canharbor_core::runtime::RuntimeError::NotFound => Error::Missing,
        canharbor_core::runtime::RuntimeError::Closed => Error::Closing,
        _ => Error::State,
    }
}
fn active(status: Status) -> bool {
    matches!(status, Status::Starting | Status::Recording)
}
fn permitted(stored: &str, owner: &str) -> bool {
    owner == "native" || stored == owner
}
fn id_valid(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
}
fn file(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.ndjson"))
}
fn meta(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}
fn directory(root: &Path) -> Result<(), Error> {
    canharbor_platform::private::directory(root).map_err(|_| Error::Storage)
}
fn open_read(path: &Path) -> Result<File, Error> {
    canharbor_platform::private::open_read(path).map_err(|_| Error::Storage)
}
fn persist(root: &Path, disk: &Disk) -> Result<(), Error> {
    let bytes = serde_json::to_vec(disk).map_err(|_| Error::Storage)?;
    let temporary = root.join(format!("{}.{}.tmp", disk.info.id, Uuid::new_v4().simple()));
    let mut out =
        canharbor_platform::private::create_new(&temporary).map_err(|_| Error::Storage)?;
    let result = out
        .write_all(&bytes)
        .and_then(|_| out.sync_all())
        .and_then(|_| canharbor_platform::private::replace(&temporary, &meta(root, &disk.info.id)));
    if let Err(error) = result {
        let _ = fs::remove_file(temporary);
        return Err(io_error(error));
    }
    Ok(())
}
fn recover(root: &Path) -> Result<Vec<Disk>, Error> {
    directory(root)?;
    let mut entries = Vec::new();
    let paths: Vec<_> = fs::read_dir(root)
        .map_err(|_| Error::Storage)?
        .take(MAX_FILES * 4 + 1)
        .collect();
    if paths.len() > MAX_FILES * 4 {
        return Err(Error::Limit);
    }
    for entry in paths {
        let entry = entry.map_err(|_| Error::Storage)?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !id_valid(id) {
            return Err(Error::Storage);
        }
        let mut bytes = Vec::new();
        open_read(&path)?
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Storage)?;
        if bytes.len() > 8192 {
            return Err(Error::Storage);
        }
        let mut disk: Disk = serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?;
        if disk.info.id != id || disk.owner.len() > 128 {
            return Err(Error::Storage);
        }
        if active(disk.info.state) {
            match open_read(&file(root, id))
                .and_then(|input| canharbor_capture::scan(input).map_err(|_| Error::Storage))
            {
                Ok(scan) => {
                    disk.info.events = scan.events;
                    disk.info.bytes = scan.valid_bytes;
                    disk.info.dropped = scan.dropped;
                    disk.info.loss_count_known = !scan.unknown_loss;
                    disk.info.state = if scan.complete {
                        Status::Completed
                    } else {
                        Status::Incomplete
                    };
                    disk.info.reason = if scan.complete {
                        None
                    } else {
                        scan.reason.or(Some(Reason::OwnerDropped))
                    };
                }
                Err(_) => {
                    disk.info.state = Status::Failed;
                    disk.info.reason = Some(Reason::IoError);
                }
            }
            persist(root, &disk)?;
        }
        entries.push(disk);
        if entries.len() > MAX_FILES {
            return Err(Error::Limit);
        }
    }
    Ok(entries)
}
impl Manager {
    pub fn new(runtime: Arc<Runtime>, root: PathBuf, instance: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                runtime,
                root,
                instance,
                entries: Mutex::new(BTreeMap::new()),
                initialized: OnceCell::new(),
                closing: AtomicBool::new(false),
                downloads: Arc::new(Semaphore::new(2)),
            }),
        }
    }
    async fn initialize(&self) -> Result<(), Error> {
        self.inner
            .initialized
            .get_or_try_init(|| async {
                let root = self.inner.root.clone();
                let recovered = tokio::task::spawn_blocking(move || recover(&root))
                    .await
                    .map_err(|_| Error::Storage)??;
                let mut entries = self.inner.entries.lock().await;
                for disk in recovered {
                    entries.insert(
                        disk.info.id.clone(),
                        Entry {
                            disk: Arc::new(Mutex::new(disk)),
                            stop: None,
                            observation: None,
                            task: None,
                            done: watch::channel(true).1,
                            crashed: Arc::new(AtomicBool::new(false)),
                        },
                    );
                }
                Ok(())
            })
            .await
            .map(|_| ())
    }
    pub async fn list(&self, owner: &str) -> Result<Vec<Info>, Error> {
        self.initialize().await?;
        let entries = self.inner.entries.lock().await;
        let mut out = Vec::new();
        for entry in entries.values() {
            let mut disk = entry.disk.lock().await;
            refresh_failure(entry, &mut disk);
            if permitted(&disk.owner, owner) {
                out.push(disk.info.clone());
            }
        }
        Ok(out)
    }
    pub async fn get(&self, id: &str, owner: &str) -> Result<Info, Error> {
        self.initialize().await?;
        let entries = self.inner.entries.lock().await;
        let entry = entries.get(id).ok_or(Error::Missing)?;
        let mut disk = entry.disk.lock().await;
        refresh_failure(entry, &mut disk);
        if !permitted(&disk.owner, owner) {
            return Err(Error::Missing);
        }
        Ok(disk.info.clone())
    }
    /// Compare-and-set naming; a repeat of the same rename is a no-op.
    pub async fn rename(
        &self,
        id: &str,
        owner: &str,
        expected: &str,
        name: &str,
    ) -> Result<Info, Error> {
        self.initialize().await?;
        if !crate::dto::valid_capture_name(name) || !crate::dto::valid_capture_name(expected) {
            return Err(Error::State);
        }
        let entries = self.inner.entries.lock().await;
        let entry = entries.get(id).ok_or(Error::Missing)?;
        let mut disk = entry.disk.lock().await;
        if !permitted(&disk.owner, owner) {
            return Err(Error::Missing);
        }
        if disk.info.name == name {
            return Ok(disk.info.clone());
        }
        if disk.info.name != expected {
            return Err(Error::Conflict);
        }
        let mut updated = disk.clone();
        updated
            .requested_name
            .get_or_insert_with(|| disk.info.name.clone());
        updated.info.name = name.into();
        persist(&self.inner.root, &updated)?;
        *disk = updated;
        Ok(disk.info.clone())
    }
    pub async fn start(
        &self,
        session: &str,
        owner: &str,
        request_id: &str,
        max_duration_seconds: u32,
    ) -> Result<Info, Error> {
        self.start_named(session, owner, request_id, max_duration_seconds, "")
            .await
    }
    pub async fn start_named(
        &self,
        session: &str,
        owner: &str,
        request_id: &str,
        max_duration_seconds: u32,
        name: &str,
    ) -> Result<Info, Error> {
        self.initialize().await?;
        if !crate::dto::valid_capture_name(name)
            || !crate::dto::valid_client_request_id(request_id)
            || !(1..=86400).contains(&max_duration_seconds)
        {
            return Err(Error::State);
        }
        let mut entries = self.inner.entries.lock().await;
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::Closing);
        }
        let mut active_count = 0;
        for entry in entries.values() {
            let disk = entry.disk.lock().await;
            if disk.owner == owner && disk.request_id == request_id {
                if disk.info.session_id != session
                    || disk.requested_name.as_deref().unwrap_or(&disk.info.name) != name
                {
                    return Err(Error::Conflict);
                }
                return Ok(disk.info.clone());
            }
            if active(disk.info.state) {
                active_count += 1;
            }
        }
        if entries.len() >= MAX_FILES || active_count >= MAX_ACTIVE {
            return Err(Error::Limit);
        }
        let mut input = self
            .inner
            .runtime
            .subscribe_all(session)
            .await
            .map_err(runtime_error)?;
        let observation = self
            .inner
            .runtime
            .observe(session)
            .await
            .map_err(runtime_error)?;
        let channel = self
            .inner
            .runtime
            .get(session)
            .await
            .map_err(runtime_error)?
            .channel;
        let backend=self.inner.runtime.capabilities(&channel).ok().map(|caps|serde_json::json!({"name":caps.backend,"device_id":caps.device_id,"channel_id":channel,"simulated":caps.simulated}));
        let id = Uuid::new_v4().to_string();
        let info = Info {
            name: name.into(),
            channel_id: Some(channel.clone()),
            created_unix_ms: Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128)
                    .to_string(),
            ),
            id: id.clone(),
            session_id: session.into(),
            state: Status::Starting,
            events: 0,
            dropped: 0,
            loss_count_known: true,
            bytes: 0,
            reason: None,
            expires_unix_ms: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                + u128::from(max_duration_seconds) * 1000)
                .min(u64::MAX as u128)
                .to_string(),
        };
        let disk = Arc::new(Mutex::new(Disk {
            owner: owner.into(),
            request_id: request_id.into(),
            requested_name: Some(name.into()),
            info: info.clone(),
        }));
        let (stop, mut stopped) = watch::channel(None);
        let (done_tx, done_rx) = watch::channel(false);
        let crashed = Arc::new(AtomicBool::new(false));
        let worker_crashed = crashed.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let root = self.inner.root.clone();
        let instance = self.inner.instance.clone();
        let observed = observation.clone();
        let progress = disk.clone();
        let capture_id = id.clone();
        let session_id = session.to_owned();
        let task = tokio::spawn(async move {
            let mut completion = Completion {
                done: done_tx,
                crashed: worker_crashed,
                normal: false,
            };
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(u64::from(max_duration_seconds));
            let initial = progress.lock().await.clone();
            let config = observation.config;
            let header = Header {
                capture_id: capture_id.clone(),
                owner: initial.owner.clone(),
                server_instance_id: instance,
                session_epoch: format!("{session_id}@{}", observation.epoch()),
                created_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128)
                    .to_string(),
                config: serde_json::json!({"channel":{"nominal_bitrate":config.nominal_bitrate,"data_bitrate":config.data_bitrate,"listen_only":config.listen_only},"start_sequence":observation.start_sequence.to_string(),"backend":backend}),
            };
            let setup_root = root.clone();
            let setup_id = capture_id.clone();
            let initialized = tokio::task::spawn_blocking(move || {
                persist(&setup_root, &initial)?;
                let sink = canharbor_platform::private::create_new(&file(&setup_root, &setup_id))
                    .map_err(io_error)?;
                Writer::start(sink, header, Default::default()).map_err(writer_error)
            })
            .await;
            let writer = match initialized {
                Ok(Ok(writer)) => writer,
                error => {
                    let full = matches!(error, Ok(Err(Error::DiskFull)));
                    let mut disk = progress.lock().await;
                    disk.info.state = Status::Failed;
                    disk.info.reason = Some(if full {
                        Reason::DiskFull
                    } else {
                        Reason::IoError
                    });
                    let snapshot = disk.clone();
                    let _ = ready_tx.send(Err(if full {
                        Error::DiskFull
                    } else {
                        Error::Storage
                    }));
                    let _ = tokio::task::spawn_blocking(move || persist(&root, &snapshot)).await;
                    completion.normal = true;
                    return;
                }
            };
            {
                progress.lock().await.info.state = Status::Recording;
            }
            let _ = ready_tx.send(Ok(progress.lock().await.info.clone()));
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            let mut ending: Option<Stop> = None;
            loop {
                ending = terminal(ending.or(*stopped.borrow()), observation.finished());
                if let Some(mut stop) = ending {
                    if let Some((cutoff, interrupted)) = observation.finished() {
                        stop.cutoff = stop.cutoff.min(cutoff);
                        if interrupted {
                            stop.reason = stop.reason.or(Some(Reason::SessionStopped));
                        }
                    }
                    for _ in 0..512 {
                        match input.try_recv() {
                            Ok(notice) => {
                                let seq = sequence(&notice);
                                if seq > stop.cutoff {
                                    break;
                                }
                                if seq > observation.start_sequence
                                    && append(&writer, notice).is_err()
                                {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(count)) => {
                                writer.incomplete(Reason::SourceGap, count)
                            }
                            Err(_) => break,
                        }
                    }
                    if let Some(reason) = stop.reason {
                        writer.incomplete(reason, 0);
                    }
                    break;
                }
                tokio::select! {
                    change=stopped.changed()=>{
                        ending=if change.is_ok(){*stopped.borrow()}else{Some(Stop{cutoff:observation.finished().map(|end|end.0).unwrap_or(u64::MAX),reason:Some(Reason::OwnerDropped)})};
                    }
                    _=interval.tick()=>{
                        let report=writer.progress();update(&progress,&report,false).await;
                        if let Some((cutoff,interrupted))=observation.finished(){ending=Some(Stop{cutoff,reason:interrupted.then_some(Reason::SessionStopped)});}
                        if tokio::time::Instant::now()>=deadline{writer.incomplete(Reason::DurationLimit,0);break;}
                        if report.reason.is_some(){break;}
                    }
                    notice=input.recv()=>match notice{
                        Ok(notice)=>{
                            ending=terminal(*stopped.borrow(),observation.finished());
                            if ending.is_some_and(|stop| sequence(&notice)>stop.cutoff) {continue;}
                            if sequence(&notice)>observation.start_sequence&&append(&writer,notice).is_err(){break;}
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count))=>{writer.incomplete(Reason::SourceGap,count);break;}
                        Err(tokio::sync::broadcast::error::RecvError::Closed)=>{ending=terminal(*stopped.borrow(),observation.finished()).or(Some(Stop{cutoff:u64::MAX,reason:Some(Reason::SessionStopped)}));}
                    }
                }
            }
            let report = tokio::task::spawn_blocking(move || writer.finish()).await;
            match report {
                Ok(Ok(report)) => update(&progress, &report, true).await,
                _ => {
                    let mut disk = progress.lock().await;
                    disk.info.state = Status::Failed;
                    disk.info.reason = Some(Reason::IoError);
                }
            }
            // Serialize writer metadata persistence with rename; an older
            // completion snapshot must not overwrite a newly persisted name.
            let mut disk = progress.lock().await;
            let snapshot = disk.clone();
            if !tokio::task::spawn_blocking(move || persist(&root, &snapshot))
                .await
                .is_ok_and(|result| result.is_ok())
            {
                disk.info.state = Status::Failed;
                disk.info.reason = Some(Reason::IoError);
            }
            completion.normal = true;
        });
        entries.insert(
            id,
            Entry {
                disk,
                stop: Some(stop),
                observation: Some(observed),
                task: Some(task),
                done: done_rx,
                crashed,
            },
        );
        drop(entries);
        ready_rx.await.map_err(|_| Error::WorkerFailed)?
    }
    pub async fn stop(&self, id: &str, owner: &str) -> Result<Info, Error> {
        self.initialize().await?;
        let mut entries = self.inner.entries.lock().await;
        let entry = entries.get_mut(id).ok_or(Error::Missing)?;
        let mut disk = entry.disk.lock().await;
        refresh_failure(entry, &mut disk);
        if !permitted(&disk.owner, owner) {
            return Err(Error::Missing);
        }
        let session = disk.info.session_id.clone();
        drop(disk);
        let notify = |cutoff| {
            if let Some(stop) = &entry.stop {
                stop.send_if_modified(|current| {
                    if current.is_none() {
                        *current = Some(Stop {
                            cutoff,
                            reason: None,
                        });
                        true
                    } else {
                        false
                    }
                });
            }
        };
        if self
            .inner
            .runtime
            .at_sequence(&session, notify)
            .await
            .is_err()
        {
            notify(
                entry
                    .observation
                    .as_ref()
                    .and_then(Observation::finished)
                    .map(|end| end.0)
                    .unwrap_or(0),
            );
        }
        let task = entry.task.take();
        let mut done = entry.done.clone();
        let crashed = entry.crashed.clone();
        let disk = entry.disk.clone();
        drop(entries);
        if let Some(task) = task {
            task.await.map_err(|_| Error::WorkerFailed)?;
        }
        while !*done.borrow() {
            done.changed().await.map_err(|_| Error::Storage)?;
        }
        if crashed.load(Ordering::Acquire) {
            return Err(Error::WorkerFailed);
        }
        Ok(disk.lock().await.info.clone())
    }
    pub async fn download(
        &self,
        id: &str,
        owner: &str,
    ) -> Result<(File, u64, OwnedSemaphorePermit), Error> {
        let info = self.get(id, owner).await?;
        if active(info.state) {
            return Err(Error::State);
        }
        let permit = self
            .inner
            .downloads
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Limit)?;
        let path = file(&self.inner.root, id);
        let file = tokio::task::spawn_blocking(move || {
            let file = open_read(&path)?;
            let size = file.metadata().map_err(|_| Error::Storage)?.len();
            if size > canharbor_capture::MAX_FILE {
                return Err(Error::Limit);
            }
            Ok((file, size))
        })
        .await
        .map_err(|_| Error::Storage)??;
        Ok((file.0, file.1, permit))
    }
    pub async fn delete(&self, id: &str, owner: &str) -> Result<(), Error> {
        self.initialize().await?;
        let mut entries = self.inner.entries.lock().await;
        let entry = entries.get(id).ok_or(Error::Missing)?;
        let mut disk = entry.disk.lock().await;
        refresh_failure(entry, &mut disk);
        if !permitted(&disk.owner, owner) {
            return Err(Error::Missing);
        }
        if active(disk.info.state) {
            return Err(Error::State);
        }
        drop(disk);
        let mut done = entry.done.clone();
        while !*done.borrow() {
            done.changed().await.map_err(|_| Error::Storage)?;
        }
        let root = self.inner.root.clone();
        let remove_id = id.to_owned();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            for path in [file(&root, &id), meta(&root, &id)] {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(Error::Storage),
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Storage)??;
        entries.remove(remove_id.as_str());
        Ok(())
    }
    pub async fn shutdown(&self) -> Vec<String> {
        self.inner.closing.store(true, Ordering::Release);
        let mut entries = self.inner.entries.lock().await;
        let mut pending = Vec::new();
        for entry in entries.values_mut() {
            if let Some(stop) = &entry.stop {
                let cutoff = entry
                    .observation
                    .as_ref()
                    .and_then(Observation::finished)
                    .map(|end| end.0)
                    .unwrap_or(u64::MAX);
                let _ = stop.send(Some(Stop {
                    cutoff,
                    reason: Some(Reason::SessionStopped),
                }));
            }
            let task = entry.task.take();
            pending.push((
                task,
                entry.done.clone(),
                entry.disk.clone(),
                entry.stop.is_some(),
                entry.crashed.clone(),
            ));
        }
        drop(entries);
        let mut failures = Vec::new();
        for (task, mut done, disk, owned, crashed) in pending {
            let failed_join = if let Some(task) = task {
                task.await.is_err()
            } else {
                false
            };
            while !*done.borrow() {
                if done.changed().await.is_err() {
                    failures.push("capture completion unavailable".into());
                    break;
                }
            }
            if failed_join || crashed.load(Ordering::Acquire) {
                failures.push("capture worker stopped unexpectedly".into());
            }
            if owned
                && !crashed.load(Ordering::Acquire)
                && disk.lock().await.info.state == Status::Failed
            {
                failures.push("capture storage cleanup failed".into());
            }
        }
        failures
    }
}
fn terminal(request: Option<Stop>, observation: Option<(u64, bool)>) -> Option<Stop> {
    match (request, observation) {
        (Some(mut stop), Some((cutoff, interrupted))) => {
            stop.cutoff = stop.cutoff.min(cutoff);
            if interrupted {
                stop.reason = stop.reason.or(Some(Reason::SessionStopped));
            }
            Some(stop)
        }
        (None, Some((cutoff, interrupted))) => Some(Stop {
            cutoff,
            reason: interrupted.then_some(Reason::SessionStopped),
        }),
        (request, None) => request,
    }
}
fn sequence(notice: &Notice) -> u64 {
    match notice {
        Notice::Frame(event) => event.sequence,
        Notice::Diagnostic { sequence, .. } => *sequence,
    }
}
fn append(writer: &Writer, notice: Notice) -> Result<(), canharbor_capture::Error> {
    let overflow = matches!(&notice,Notice::Diagnostic{diagnostic,..} if diagnostic.overflow);
    writer.event(serde_json::to_value(crate::dto::notice(notice))?)?;
    if overflow {
        writer.incomplete(Reason::SourceGap, 0);
    }
    Ok(())
}
async fn update(progress: &Mutex<Disk>, report: &Report, finished: bool) {
    let mut disk = progress.lock().await;
    disk.info.events = report.events;
    disk.info.bytes = report.bytes;
    disk.info.dropped = report.dropped;
    disk.info.loss_count_known = !report.unknown_loss;
    disk.info.reason = report.reason;
    if finished {
        disk.info.state = if report.complete {
            Status::Completed
        } else if matches!(report.reason, Some(Reason::IoError | Reason::DiskFull)) {
            Status::Failed
        } else {
            Status::Incomplete
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_core::{CanId, Frame, runtime::Mode};
    use canharbor_virtual::VirtualBus;
    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("canharbor-capture-test-{}", Uuid::new_v4())))
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    async fn setup(root: &Root) -> (Arc<Runtime>, Manager, String, u64) {
        let runtime = Runtime::new(Arc::new(VirtualBus::default()));
        let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
        let lease = runtime.acquire(&session.id, "owner").await.unwrap();
        runtime
            .running(&session.id, "owner", lease.generation, true)
            .await
            .unwrap();
        let manager = Manager::new(runtime.clone(), root.0.clone(), "test-instance".into());
        (runtime, manager, session.id, lease.generation)
    }
    async fn send(runtime: &Runtime, session: &str, generation: u64) {
        runtime
            .send(
                session,
                "owner",
                generation,
                Frame::classic(CanId::new(42, false).unwrap(), vec![1, 2]).unwrap(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while runtime.sequence(session).await.unwrap() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn ended(manager: &Manager, id: &str) -> Info {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let info = manager.get(id, "native").await.unwrap();
                if !active(info.state) {
                    return info;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn named_capture_metadata_survives_recovery_and_retries() {
        let root = Root::new();
        let (runtime, manager, session, _) = setup(&root).await;
        assert!(matches!(
            manager
                .start_named(&session, "browser-a", "bad", 60, "bad\nname")
                .await,
            Err(Error::State)
        ));
        let info = manager
            .start_named(&session, "browser-a", "named", 60, "Bench run")
            .await
            .unwrap();
        assert_eq!(info.channel_id.as_deref(), Some("virtual:0"));
        assert!(
            info.created_unix_ms
                .as_ref()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                > 0
        );
        let repeated = manager
            .start_named(&session, "browser-a", "named", 60, "Bench run")
            .await
            .unwrap();
        assert_eq!(info.id, repeated.id);
        assert!(matches!(
            manager
                .start_named(&session, "browser-a", "named", 60, "Other run")
                .await,
            Err(Error::Conflict)
        ));
        manager.stop(&info.id, "browser-a").await.unwrap();
        assert!(matches!(
            manager.get(&info.id, "browser-b").await,
            Err(Error::Missing)
        ));
        let recovered = Manager::new(runtime, root.0.clone(), "next-instance".into());
        let loaded = recovered.get(&info.id, "native").await.unwrap();
        assert_eq!(loaded.name, "Bench run");
        assert_eq!(loaded.channel_id, info.channel_id);
        assert_eq!(loaded.created_unix_ms, info.created_unix_ms);
    }
    #[tokio::test]
    async fn rename_is_owned_persistent_and_preserves_creation_idempotency() {
        let root = Root::new();
        let (runtime, manager, session, _) = setup(&root).await;
        let info = manager
            .start_named(&session, "browser-a", "rename", 60, "Original")
            .await
            .unwrap();
        assert!(matches!(
            manager
                .rename(&info.id, "browser-b", "Original", "Other")
                .await,
            Err(Error::Missing)
        ));
        assert!(matches!(
            manager
                .rename(&info.id, "browser-a", "Original", "bad\nname")
                .await,
            Err(Error::State)
        ));
        manager
            .rename(&info.id, "browser-a", "Original", "Renamed")
            .await
            .unwrap();
        let repeated = manager
            .rename(&info.id, "browser-a", "Original", "Renamed")
            .await
            .unwrap();
        assert_eq!(repeated.name, "Renamed");
        let (left, right) = tokio::join!(
            manager.rename(&info.id, "browser-a", "Renamed", "Concurrent A"),
            manager.rename(&info.id, "browser-a", "Renamed", "Concurrent B")
        );
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        let (winner, loser) = if left.is_ok() {
            (left.unwrap(), right)
        } else {
            (right.unwrap(), left)
        };
        assert!(matches!(loser, Err(Error::Conflict)));
        manager
            .rename(&info.id, "browser-a", &winner.name, "Renamed")
            .await
            .unwrap();
        assert!(matches!(
            manager
                .rename(&info.id, "browser-a", "Original", "Stale edit")
                .await,
            Err(Error::Conflict)
        ));
        assert_eq!(
            manager
                .start_named(&session, "browser-a", "rename", 60, "Original")
                .await
                .unwrap()
                .id,
            info.id
        );
        manager.stop(&info.id, "browser-a").await.unwrap();
        let recovered = Manager::new(runtime, root.0.clone(), "next-instance".into());
        let loaded = recovered.get(&info.id, "native").await.unwrap();
        assert_eq!(loaded.name, "Renamed");
        assert_eq!(
            recovered
                .start_named(&session, "browser-a", "rename", 60, "Original")
                .await
                .unwrap()
                .id,
            info.id
        );
        let filename = download_filename(&loaded);
        assert!(filename.starts_with("Renamed_virtual_0_"));
        assert!(filename.ends_with(&format!("_{}.ndjson", info.id)));
        let mut hostile = loaded;
        hostile.name = "../../\"\r\n".repeat(20);
        hostile.channel_id = Some("/bad\\channel".repeat(20));
        let safe = download_filename(&hostile);
        assert!(safe.len() < 255);
        assert!(!safe.contains(['/', '\\', '\r', '\n', '"']));
        hostile.name = "台架测试".into();
        let disposition = download_disposition(&hostile);
        assert!(disposition.contains("filename*=UTF-8''%E5%8F%B0%E6%9E%B6%E6%B5%8B%E8%AF%95"));
        assert!(!disposition.contains(['\r', '\n']));
    }
    #[test]
    fn legacy_capture_metadata_has_unknown_source_and_creation_time() {
        let value = serde_json::json!({"id":"legacy", "session_id":"session", "state":"completed", "events":0,"dropped":0,"bytes":0,"reason":null,"expires_unix_ms":"1"});
        let info: Info = serde_json::from_value(value).unwrap();
        assert!(info.name.is_empty());
        assert!(info.channel_id.is_none());
        assert!(info.created_unix_ms.is_none());
    }
    #[tokio::test]
    async fn capture_drains_stop_boundary_and_downloads_valid_records() {
        let root = Root::new();
        let (runtime, manager, session, g) = setup(&root).await;
        let info = manager
            .start(&session, "browser-a", "request", 60)
            .await
            .unwrap();
        send(&runtime, &session, g).await;
        let (a, b) = tokio::join!(
            manager.stop(&info.id, "browser-a"),
            manager.stop(&info.id, "browser-a")
        );
        for result in [a, b] {
            let result = result.unwrap();
            assert_eq!(result.state, Status::Completed);
            assert_eq!(result.events, 1);
        }
        let (file, size, _permit) = manager.download(&info.id, "native").await.unwrap();
        let scan = canharbor_capture::scan(file).unwrap();
        assert!(scan.complete);
        assert_eq!(scan.events, 1);
        assert_eq!(scan.header.config["backend"]["name"], "virtual");
        assert_eq!(scan.header.config["backend"]["channel_id"], "virtual:0");
        assert_eq!(scan.valid_bytes, size);
        assert!(matches!(
            manager.get(&info.id, "browser-b").await,
            Err(Error::Missing)
        ));
        assert!(matches!(
            manager.delete(&info.id, "browser-b").await,
            Err(Error::Missing)
        ));
        manager.delete(&info.id, "native").await.unwrap();
        assert!(manager.list("native").await.unwrap().is_empty());
        assert!(manager.shutdown().await.is_empty());
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn stop_restart_under_same_lease_does_not_resume_recording() {
        let root = Root::new();
        let (runtime, manager, session, g) = setup(&root).await;
        let info = manager
            .start(&session, "native", "request", 60)
            .await
            .unwrap();
        let first = runtime.observe(&session).await.unwrap();
        runtime.running(&session, "owner", g, false).await.unwrap();
        runtime.running(&session, "owner", g, true).await.unwrap();
        assert_ne!(
            first.epoch(),
            runtime.observe(&session).await.unwrap().epoch()
        );
        send(&runtime, &session, g).await;
        let info = ended(&manager, &info.id).await;
        assert_eq!(info.state, Status::Completed);
        assert_eq!(info.events, 0);
        manager.shutdown().await;
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn revoked_acquisition_and_service_shutdown_are_incomplete() {
        let root = Root::new();
        let (runtime, manager, session, g) = setup(&root).await;
        let info = manager
            .start(&session, "native", "request", 60)
            .await
            .unwrap();
        send(&runtime, &session, g).await;
        runtime.release(&session, "owner", g).await.unwrap();
        let info = ended(&manager, &info.id).await;
        assert_eq!(info.state, Status::Incomplete);
        assert_eq!(info.reason, Some(Reason::SessionStopped));
        runtime.acquire(&session, "owner").await.unwrap();
        let g = runtime.get(&session).await.unwrap().generation;
        runtime.running(&session, "owner", g, true).await.unwrap();
        let next = manager.start(&session, "native", "next", 60).await.unwrap();
        runtime.shutdown().await;
        assert!(manager.shutdown().await.is_empty());
        assert_eq!(
            manager.get(&next.id, "native").await.unwrap().state,
            Status::Incomplete
        );
        assert!(matches!(
            manager.start(&session, "native", "closed", 60).await,
            Err(Error::Closing)
        ));
    }
    #[tokio::test]
    async fn explicit_stop_preserves_interrupted_epoch_and_restart_boundary() {
        for interrupted in [false, true] {
            let root = Root::new();
            let (runtime, manager, session, g) = setup(&root).await;
            let capture = manager
                .start(&session, "native", "capture", 60)
                .await
                .unwrap();
            if interrupted {
                runtime.release(&session, "owner", g).await.unwrap();
            } else {
                runtime.running(&session, "owner", g, false).await.unwrap();
            }
            let g = if interrupted {
                runtime.acquire(&session, "owner").await.unwrap().generation
            } else {
                g
            };
            runtime.running(&session, "owner", g, true).await.unwrap();
            send(&runtime, &session, g).await;
            let result = manager.stop(&capture.id, "native").await.unwrap();
            assert_eq!(result.events, 0);
            assert_eq!(
                result.state,
                if interrupted {
                    Status::Incomplete
                } else {
                    Status::Completed
                }
            );
            assert_eq!(result.reason, interrupted.then_some(Reason::SessionStopped));
            let (file, _, _permit) = manager.download(&capture.id, "native").await.unwrap();
            let scan = canharbor_capture::scan(file).unwrap();
            assert_eq!(scan.events, 0);
            assert_eq!(scan.complete, !interrupted);
            runtime.shutdown().await;
            manager.shutdown().await;
        }
    }
    #[tokio::test]
    async fn normal_close_is_complete_and_explicit_boundary_excludes_later_notices() {
        for close in [false, true] {
            let root = Root::new();
            let (runtime, manager, session, g) = setup(&root).await;
            let capture = manager
                .start(&session, "native", "capture", 60)
                .await
                .unwrap();
            if close {
                runtime.close(&session, "owner", g).await.unwrap();
            } else {
                let stop = manager
                    .inner
                    .entries
                    .lock()
                    .await
                    .get(&capture.id)
                    .unwrap()
                    .stop
                    .clone()
                    .unwrap();
                runtime
                    .at_sequence(&session, |cutoff| {
                        assert_eq!(cutoff, 0);
                        stop.send_replace(Some(Stop {
                            cutoff,
                            reason: None,
                        }));
                    })
                    .await
                    .unwrap();
                send(&runtime, &session, g).await;
            }
            let result = manager.stop(&capture.id, "native").await.unwrap();
            assert_eq!(result.state, Status::Completed);
            assert_eq!(result.events, 0);
            let (file, _, _permit) = manager.download(&capture.id, "native").await.unwrap();
            assert!(canharbor_capture::scan(file).unwrap().complete);
            runtime.shutdown().await;
            manager.shutdown().await;
        }
    }
    #[tokio::test]
    async fn ownership_idempotency_and_active_limits() {
        let root = Root::new();
        let (runtime, manager, session, _) = setup(&root).await;
        let first = manager.start(&session, "a", "same", 60).await.unwrap();
        assert_eq!(
            first.id,
            manager.start(&session, "a", "same", 60).await.unwrap().id
        );
        assert!(matches!(
            manager.start("missing", "a", "same", 60).await,
            Err(Error::Conflict)
        ));
        for n in 1..MAX_ACTIVE {
            manager
                .start(&session, "a", &n.to_string(), 60)
                .await
                .unwrap();
        }
        assert!(matches!(
            manager.start(&session, "a", "extra", 60).await,
            Err(Error::Limit)
        ));
        assert!(manager.list("b").await.unwrap().is_empty());
        assert!(matches!(
            manager.download(&first.id, "a").await,
            Err(Error::State)
        ));
        runtime.shutdown().await;
        manager.shutdown().await;
    }
    #[tokio::test]
    async fn duration_limit_and_restart_recovery_preserve_loss() {
        let root = Root::new();
        let (runtime, manager, session, _) = setup(&root).await;
        let info = manager
            .start(&session, "native", "duration", 1)
            .await
            .unwrap();
        let info = ended(&manager, &info.id).await;
        assert_eq!(info.reason, Some(Reason::DurationLimit));
        let mut disk: Disk =
            serde_json::from_reader(open_read(&meta(&root.0, &info.id)).unwrap()).unwrap();
        disk.info.state = Status::Recording;
        persist(&root.0, &disk).unwrap();
        let recovered = Manager::new(runtime.clone(), root.0.clone(), "restart".into());
        let recovered_info = recovered.get(&info.id, "native").await.unwrap();
        assert_eq!(recovered_info.state, Status::Incomplete);
        assert_eq!(recovered_info.reason, Some(Reason::DurationLimit));
        manager.shutdown().await;
        recovered.shutdown().await;
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn cancelled_shutdown_waiter_preserves_storage_cleanup_failure() {
        let root = Root::new();
        let (runtime, manager, session, _) = setup(&root).await;
        let capture = manager
            .start(&session, "native", "capture", 60)
            .await
            .unwrap();
        let disk = manager
            .inner
            .entries
            .lock()
            .await
            .get(&capture.id)
            .unwrap()
            .disk
            .clone();
        let held = disk.lock().await;
        fs::remove_dir_all(&root.0).unwrap();
        let copy = manager.clone();
        let first = tokio::spawn(async move { copy.shutdown().await });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if manager
                    .inner
                    .entries
                    .lock()
                    .await
                    .get(&capture.id)
                    .unwrap()
                    .task
                    .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        drop(held);
        let report = manager.shutdown().await;
        assert!(!report.is_empty());
        assert_eq!(
            manager.get(&capture.id, "native").await.unwrap().state,
            Status::Failed
        );
        assert_eq!(manager.shutdown().await, report);
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn cancelled_shutdown_waiter_does_not_hide_an_aborted_capture_worker() {
        let root = Root::new();
        let (runtime, manager, session, _) = setup(&root).await;
        let capture = manager
            .start(&session, "native", "capture", 60)
            .await
            .unwrap();
        let (disk, abort) = {
            let entries = manager.inner.entries.lock().await;
            let entry = entries.get(&capture.id).unwrap();
            (
                entry.disk.clone(),
                entry.task.as_ref().unwrap().abort_handle(),
            )
        };
        let held = disk.lock().await;
        let copy = manager.clone();
        let first = tokio::spawn(async move { copy.shutdown().await });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if manager
                    .inner
                    .entries
                    .lock()
                    .await
                    .get(&capture.id)
                    .unwrap()
                    .task
                    .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        abort.abort();
        drop(held);
        let report = manager.shutdown().await;
        assert!(
            report
                .iter()
                .any(|message| message.contains("worker stopped unexpectedly"))
        );
        assert_eq!(
            manager.get(&capture.id, "native").await.unwrap().state,
            Status::Failed
        );
        assert_eq!(manager.shutdown().await, report);
        runtime.shutdown().await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn storage_rejects_symlinks_and_public_permissions() {
        let root = Root::new();
        fs::create_dir(&root.0).unwrap();
        let runtime = Runtime::new(Arc::new(VirtualBus::default()));
        let manager = Manager::new(runtime.clone(), root.0.clone(), "test".into());
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(manager.list("native").await, Err(Error::Storage)));
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.0.join("link");
        std::os::unix::fs::symlink(&root.0, &link).unwrap();
        assert!(directory(&link).is_err());
        assert!(open_read(&link).is_err());
        assert!(matches!(
            manager.get("../../escape", "native").await,
            Err(Error::Missing)
        ));
        runtime.shutdown().await;
    }
}
