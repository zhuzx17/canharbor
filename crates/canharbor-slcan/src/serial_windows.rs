// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exclusive overlapped Windows COM access. Every issued I/O is reaped before return.
use super::Transport;
use std::{
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
    time::Duration,
};
use windows_sys::Win32::{
    Devices::Communication::*,
    Foundation::*,
    Storage::FileSystem::*,
    System::{IO::*, Threading::*},
};

pub struct Serial {
    handle: OwnedHandle,
    original: DCB,
    timeouts: COMMTIMEOUTS,
}
fn device_path(path: &Path) -> io::Result<Vec<u16>> {
    let text = path.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "COM port name must be ASCII")
    })?;
    let name = text.strip_prefix(r"\\.\").unwrap_or(text);
    let number = name
        .get(3..)
        .filter(|_| name.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("COM")))
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse::<u16>().ok())
        .filter(|n| *n > 0);
    let number = number.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected explicit COM port, for example COM3 or \\\\.\\COM10",
        )
    })?;
    Ok(std::ffi::OsStr::new(&format!(r"\\.\COM{number}"))
        .encode_wide()
        .chain(Some(0))
        .collect())
}
impl Serial {
    pub fn open(path: &Path, baud: u32) -> io::Result<Self> {
        if ![9600, 19200, 38400, 57600, 115200, 230400].contains(&baud) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported serial baud",
            ));
        }
        let name = device_path(path)?;
        // share mode zero requests exclusive access; no arbitrary filesystem paths.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(h) };
        let mut original = DCB {
            DCBlength: std::mem::size_of::<DCB>() as u32,
            ..Default::default()
        };
        let mut timeouts = COMMTIMEOUTS::default();
        if unsafe { GetCommState(h, &mut original) } == 0
            || unsafe { GetCommTimeouts(h, &mut timeouts) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let serial = Self {
            handle,
            original,
            timeouts,
        };
        // Binary, 8N1, DTR/RTS enabled, no software/hardware flow control.
        let settings = DCB {
            BaudRate: baud,
            ByteSize: 8,
            Parity: NOPARITY,
            StopBits: ONESTOPBIT,
            _bitfield: 1 | (1 << 4) | (1 << 12),
            ..original
        };
        // Return when the first byte arrives, then promptly deliver available data.
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: 250,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 250,
        };
        if unsafe { SetCommState(h, &settings) } == 0
            || unsafe { SetCommTimeouts(h, &timeouts) } == 0
            || unsafe { PurgeComm(h, PURGE_RXCLEAR | PURGE_TXCLEAR) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(serial)
    }
}
// The OS may complete normally while cancellation races. Always inspect the final
// completion, never free OVERLAPPED/buffers merely because CancelIoEx returned.
fn complete(
    handle: &OwnedHandle,
    overlap: &mut OVERLAPPED,
    started: windows_sys::core::BOOL,
    timeout: Duration,
) -> io::Result<usize> {
    let h = handle.as_raw_handle();
    if started == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(e);
        }
    }
    let wait = unsafe {
        WaitForSingleObject(
            overlap.hEvent,
            timeout.as_millis().min(u32::MAX as u128 - 1) as u32,
        )
    };
    let timed_out = wait == WAIT_TIMEOUT;
    let wait_error = (wait == WAIT_FAILED).then(io::Error::last_os_error);
    if wait != WAIT_OBJECT_0 {
        unsafe {
            CancelIoEx(h, overlap);
        }
    }
    let mut count = 0;
    let ok = unsafe { GetOverlappedResult(h, overlap, &mut count, 1) };
    if let Some(e) = wait_error {
        return Err(e);
    }
    if ok != 0 {
        return Ok(count as usize);
    }
    let e = io::Error::last_os_error();
    if timed_out && e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "serial I/O cancelled at deadline",
        ));
    }
    Err(e)
}
fn event() -> io::Result<OwnedHandle> {
    let h = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}
fn write_handle(handle: &OwnedHandle, bytes: &[u8]) -> io::Result<usize> {
    let n = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "write too large"))?;
    let event = event()?;
    let mut overlap = OVERLAPPED {
        hEvent: event.as_raw_handle(),
        ..Default::default()
    };
    let started = unsafe {
        WriteFile(
            handle.as_raw_handle(),
            bytes.as_ptr(),
            n,
            ptr::null_mut(),
            &mut overlap,
        )
    };
    complete(handle, &mut overlap, started, Duration::from_millis(250))
}
fn read_handle(handle: &OwnedHandle, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
    let n = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "read too large"))?;
    if n == 0 {
        return Ok(0);
    }
    let event = event()?;
    let mut overlap = OVERLAPPED {
        hEvent: event.as_raw_handle(),
        ..Default::default()
    };
    let started = unsafe {
        ReadFile(
            handle.as_raw_handle(),
            bytes.as_mut_ptr(),
            n,
            ptr::null_mut(),
            &mut overlap,
        )
    };
    match complete(handle, &mut overlap, started, timeout) {
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
        other => other,
    }
}

impl Transport for Serial {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        write_handle(&self.handle, bytes)
    }
    fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
        read_handle(&self.handle, bytes, timeout)
    }
}
impl Drop for Serial {
    fn drop(&mut self) {
        // No operation survives read/write; restoring settings cannot reuse live I/O.
        unsafe {
            SetCommState(self.handle.as_raw_handle(), &self.original);
            SetCommTimeouts(self.handle.as_raw_handle(), &self.timeouts);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_read_is_reaped_and_next_transfer_is_not_replayed() {
        use std::io::{Read, Write};
        use windows_sys::Win32::System::Pipes::*;
        let name = format!(
            r"\\.\pipe\ch-serial-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let h = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                ptr::null(),
            )
        };
        assert_ne!(h, INVALID_HANDLE_VALUE);
        let server = unsafe { OwnedHandle::from_raw_handle(h) };
        let mut client = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        let mut bytes = [0; 4];
        assert_eq!(
            read_handle(&server, &mut bytes, Duration::from_millis(10)).unwrap(),
            0
        );
        client.write_all(b"ping").unwrap();
        assert_eq!(
            read_handle(&server, &mut bytes, Duration::from_secs(1)).unwrap(),
            4
        );
        assert_eq!(&bytes, b"ping");
        assert_eq!(write_handle(&server, b"pong").unwrap(), 4);
        client.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"pong");
        drop(client);
        assert!(read_handle(&server, &mut bytes, Duration::from_secs(1)).is_err());
    }
    #[test]
    fn explicit_com_names_only() {
        assert_eq!(
            device_path(Path::new("com10")).unwrap(),
            device_path(Path::new(r"\\.\COM10")).unwrap()
        );
        for bad in [
            "COM0",
            "COM1/other",
            "COM1x",
            r"\\server\COM3",
            "NUL",
            "C:\\temp\\port",
        ] {
            assert!(device_path(Path::new(bad)).is_err(), "{bad}");
        }
        assert!(Serial::open(Path::new("COM1"), 123).is_err());
    }
}
