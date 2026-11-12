// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit serial access with bounded transfers and no implicit write replay.
use std::{io, time::Duration};
pub trait Transport: Send + 'static {
    /// One bounded write. Partial success must be reported, never retried implicitly.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize>;
    /// Return zero on timeout; EOF is an error. At most `bytes.len()` bytes.
    fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

#[cfg(unix)]
#[path = "serial_unix.rs"]
mod native;
#[cfg(windows)]
#[path = "serial_windows.rs"]
mod native;
pub use native::Serial;
