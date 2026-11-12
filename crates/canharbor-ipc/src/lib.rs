// SPDX-License-Identifier: AGPL-3.0-or-later
//! Same-user byte streams. Native authority never travels over TCP.
use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
#[cfg(unix)]
mod native {
    use super::*;
    pub type Client = tokio::net::UnixStream;
    pub type Server = tokio::net::UnixStream;
    pub struct Listener(tokio::net::UnixListener);
    pub fn endpoint(dir: &Path) -> io::Result<PathBuf> {
        Ok(dir.join("service.sock"))
    }
    pub fn user_key() -> io::Result<String> {
        Ok(unsafe { libc::geteuid() }.to_string())
    }
    pub async fn connect(dir: &Path) -> io::Result<Client> {
        let s = Client::connect(endpoint(dir)?).await?;
        if s.peer_cred()?.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "foreign local server",
            ));
        }
        Ok(s)
    }
    impl Listener {
        pub fn bind(dir: &Path) -> io::Result<Self> {
            use std::os::unix::fs::PermissionsExt;
            let p = endpoint(dir)?;
            let l = tokio::net::UnixListener::bind(&p)?;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self(l))
        }
        pub async fn accept(&mut self) -> io::Result<Server> {
            let (s, _) = self.0.accept().await?;
            if s.peer_cred()?.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "foreign local client",
                ));
            }
            Ok(s)
        }
    }
}
#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
    };
    use windows_sys::Win32::{
        Foundation::ERROR_PIPE_BUSY,
        System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
    };
    pub type Client = NamedPipeClient;
    pub type Server = NamedPipeServer;
    pub struct Listener {
        pending: NamedPipeServer,
        path: PathBuf,
    }
    pub fn user_key() -> io::Result<String> {
        canharbor_platform::windows::current_sid()
    }
    pub fn endpoint(dir: &Path) -> io::Result<PathBuf> {
        use sha2::{Digest, Sha256};
        use std::os::windows::ffi::OsStrExt;
        let dir = dir.canonicalize()?;
        let mut h = Sha256::new();
        h.update(user_key()?.as_bytes());
        for c in dir.as_os_str().encode_wide() {
            h.update(c.to_le_bytes());
        }
        let hash = h.finalize();
        let key = hash.iter().map(|b| format!("{b:02x}")).collect::<String>();
        Ok(PathBuf::from(format!(r"\\.\pipe\CANHarbor-v1-{key}")))
    }
    fn create(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
        let sd = canharbor_platform::windows::Descriptor::private()?;
        let sa = sd.attributes();
        // SECURITY_ATTRIBUTES is copied by CreateNamedPipe; its descriptor lives
        // until this synchronous call returns. Handles are not inheritable.
        unsafe {
            ServerOptions::new()
                .pipe_mode(PipeMode::Byte)
                .reject_remote_clients(true)
                .first_pipe_instance(first)
                .max_instances(64)
                .create_with_security_attributes_raw(
                    path,
                    std::ptr::from_ref(&sa).cast_mut().cast(),
                )
        }
    }
    fn check_peer(handle: *mut core::ffi::c_void, server: bool) -> io::Result<()> {
        let mut pid = 0;
        let ok = unsafe {
            if server {
                GetNamedPipeServerProcessId(handle, &mut pid)
            } else {
                GetNamedPipeClientProcessId(handle, &mut pid)
            }
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if !canharbor_platform::windows::same_user_process(pid)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "foreign local peer",
            ));
        }
        Ok(())
    }
    pub async fn connect(dir: &Path) -> io::Result<Client> {
        canharbor_platform::private::check_dir(dir)?;
        let path = endpoint(dir)?;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match ClientOptions::new().open(&path) {
                    Ok(p) => {
                        check_peer(p.as_raw_handle(), true)?;
                        return Ok(p);
                    }
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await
                    }
                    Err(e) => return Err(e),
                }
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "local pipe connection timeout"))?
    }
    impl Listener {
        pub fn bind(dir: &Path) -> io::Result<Self> {
            let path = endpoint(dir)?;
            Ok(Self {
                pending: create(&path, true)?,
                path,
            })
        }
        pub async fn accept(&mut self) -> io::Result<Server> {
            self.pending.connect().await?;
            let next = create(&self.path, false)?;
            let connected = std::mem::replace(&mut self.pending, next);
            check_peer(connected.as_raw_handle(), false)?;
            Ok(connected)
        }
    }
}
pub use native::{endpoint, user_key};
pub struct StreamClient(native::Client);
pub struct StreamServer(native::Server);
pub struct Listener(native::Listener);
pub async fn connect(dir: &Path) -> io::Result<StreamClient> {
    Ok(StreamClient(native::connect(dir).await?))
}
impl Listener {
    pub fn bind(dir: &Path) -> io::Result<Self> {
        Ok(Self(native::Listener::bind(dir)?))
    }
    pub async fn accept(&mut self) -> io::Result<StreamServer> {
        Ok(StreamServer(self.0.accept().await?))
    }
}
macro_rules! io_stream {
    ($t:ty) => {
        impl AsyncRead for $t {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_read(cx, buf)
            }
        }
        impl AsyncWrite for $t {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                b: &[u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.0).poll_write(cx, b)
            }
            fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_flush(cx)
            }
            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_shutdown(cx)
            }
        }
    };
}
io_stream!(StreamClient);
io_stream!(StreamServer);
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn same_user_roundtrip_and_single_listener() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = std::env::temp_dir().join(format!(
            "ch-ipc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        canharbor_platform::private::directory(&dir).unwrap();
        let mut l = super::Listener::bind(&dir).unwrap();
        assert!(super::Listener::bind(&dir).is_err());
        let client = tokio::spawn({
            let dir = dir.clone();
            async move {
                let mut c = super::connect(&dir).await.unwrap();
                c.write_all(b"ping").await.unwrap();
                let mut b = [0; 4];
                c.read_exact(&mut b).await.unwrap();
                assert_eq!(&b, b"pong");
            }
        });
        let mut s = l.accept().await.unwrap();
        let mut b = [0; 4];
        s.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"ping");
        s.write_all(b"pong").await.unwrap();
        client.await.unwrap();
        drop(s);
        drop(l);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
