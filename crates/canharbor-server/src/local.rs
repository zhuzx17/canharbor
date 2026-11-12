// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{AppState, router};
use canharbor_core::runtime::Runtime;
use fs2::FileExt;
#[cfg(unix)]
use std::fs;
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};
#[cfg(unix)]
pub fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}
pub fn default_runtime_dir() -> PathBuf {
    #[cfg(unix)]
    {
        std::env::temp_dir().join(format!("canharbor-{}", current_uid()))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("CANHarbor")
            .join("runtime")
    }
}
pub fn socket_path(dir: &Path) -> PathBuf {
    canharbor_ipc::endpoint(dir).unwrap_or_else(|_| dir.join("service.sock"))
}
struct Files {
    _dir: PathBuf,
    _lock: File,
}
impl Files {
    fn acquire(dir: &Path) -> io::Result<Self> {
        canharbor_platform::private::directory(dir)?;
        let lock = canharbor_platform::private::open_lock(&dir.join("service.lock"))?;
        lock.try_lock_exclusive().map_err(|_| {
            io::Error::new(io::ErrorKind::AddrInUse, "CANHarbor is already running")
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            let socket = socket_path(dir);
            match fs::symlink_metadata(&socket) {
                Ok(m) => {
                    if !m.file_type().is_socket() || m.uid() != current_uid() {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "refusing to remove an unknown socket path",
                        ));
                    }
                    fs::remove_file(socket)?
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Self {
            _dir: dir.into(),
            _lock: lock,
        })
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::remove_file(socket_path(&self._dir));
    }
}
struct SameUser(canharbor_ipc::Listener);
impl axum::serve::Listener for SameUser {
    type Io = canharbor_ipc::StreamServer;
    type Addr = ();
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok(s) => return (s, ()),
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}
#[derive(Debug, Clone, Default)]
pub struct ServiceCloseSummary {
    pub unknown_transmissions: u64,
    /// Tasks with an uncertain terminal result during this service lifetime,
    /// plus tasks still pending after the shutdown accounting deadline.
    /// Counted separately from backend transmission records.
    pub unknown_tasks: u64,
    pub failures: Vec<String>,
}
pub struct RunningServer {
    pub state: Arc<AppState>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    _files: Files,
}
impl RunningServer {
    pub async fn start(
        runtime: Arc<Runtime>,
        dir: &Path,
        port: u16,
        web: bool,
    ) -> io::Result<Self> {
        if web && !cfg!(feature = "web-ui") {
            return Err(io::Error::other(
                "build with --features web-ui after building web/, or use serve --no-web",
            ));
        }
        let files = Files::acquire(dir)?;
        let uds = canharbor_ipc::Listener::bind(dir)?;
        let tcp = if web {
            Some(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?)
        } else {
            None
        };
        let origin = match &tcp {
            Some(listener) => format!("http://{}", listener.local_addr()?),
            None => "http://127.0.0.1:0".into(),
        };
        let state = AppState::with_preferences_file(
            runtime,
            origin,
            web,
            Some(dir.join("captures")),
            crate::preferences::default_path(),
        );
        let (stop, rx) = watch::channel(false);
        let mut tasks = Vec::new();
        let mut uds_stop = rx.clone();
        let app = router(state.clone(), true);
        tasks.push(tokio::spawn(async move {
            if let Err(e) = axum::serve(SameUser(uds), app)
                .with_graceful_shutdown(async move {
                    let _ = uds_stop.changed().await;
                })
                .await
            {
                eprintln!("UDS server stopped: {e}")
            }
        }));
        if let Some(tcp) = tcp {
            let app = router(state.clone(), false);
            let mut tcp_stop = rx.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = axum::serve(tcp, app)
                    .with_graceful_shutdown(async move {
                        let _ = tcp_stop.changed().await;
                    })
                    .await
                {
                    eprintln!("HTTP server stopped: {e}")
                }
            }));
        }
        let r = state.runtime.clone();
        let mut maintenance_stop = rx;
        tasks.push(tokio::spawn(async move{let mut interval=tokio::time::interval(Duration::from_millis(100));loop{tokio::select!{_=interval.tick()=>r.maintain().await,_=maintenance_stop.changed()=>break}}}));
        Ok(Self {
            state,
            stop,
            tasks,
            _files: files,
        })
    }
    pub async fn shutdown(mut self) -> ServiceCloseSummary {
        self.state.stress.stop_all();
        self.state.periodic.begin_shutdown();
        self.state.runtime.fence_shutdown();
        let _ = self.stop.send(true);
        // Start owned backend cleanup before the first await. Dropping this
        // service shutdown waiter must not strand runtime teardown behind task
        // accounting, which can legitimately wait for an in-flight submission.
        let runtime = self.state.runtime.clone();
        let backend = tokio::spawn(async move { runtime.shutdown().await });
        let unknown_tasks =
            self.state.periodic.shutdown().await + self.state.stress.shutdown().await;
        let backend = backend
            .await
            .unwrap_or_else(|error| canharbor_core::CloseSummary {
                unknown_transmissions: 0,
                failures: vec![format!("runtime shutdown accounting failed: {error}")],
            });
        let mut summary = ServiceCloseSummary {
            unknown_tasks,
            unknown_transmissions: backend.unknown_transmissions,
            failures: backend.failures,
        };
        summary
            .failures
            .extend(self.state.captures.shutdown().await);
        for mut task in self.tasks.drain(..) {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        summary
    }
}
impl Drop for RunningServer {
    fn drop(&mut self) {
        self.state.stress.stop_all();
        self.state.periodic.begin_shutdown();
        self.state.runtime.fence_shutdown();
        let _ = self.stop.send(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn rejects_symlink_and_foreign_socket_paths() {
        let base =
            std::env::temp_dir().join(format!("ch-local-test-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let real = base.join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&real, base.join("link")).unwrap();
        assert!(Files::acquire(&base.join("link")).is_err());
        fs::write(socket_path(&real), b"do not remove").unwrap();
        assert!(Files::acquire(&real).is_err());
        assert_eq!(fs::read(socket_path(&real)).unwrap(), b"do not remove");
        fs::remove_dir_all(&base).unwrap();
    }
}
