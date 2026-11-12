// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unix tty implementation.
use super::Transport;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
    time::{Duration, Instant},
};
pub struct Serial {
    file: File,
    original: libc::termios,
}
impl Serial {
    pub fn open(path: &Path, baud: u32) -> io::Result<Self> {
        let speed = match baud {
            9600 => libc::B9600,
            19200 => libc::B19200,
            38400 => libc::B38400,
            57600 => libc::B57600,
            115200 => libc::B115200,
            230400 => libc::B230400,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported serial baud",
                ));
            }
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)?;
        let fd = file.as_raw_fd();
        // Pair tty exclusivity with an advisory lock for cooperating processes.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is live; termios points to initialized writable storage.
        let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut original) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // Request tty exclusivity; cleanup clears it explicitly.
        if unsafe { libc::ioctl(fd, libc::TIOCEXCL as _) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let serial = Self { file, original };
        let mut settings = original;
        unsafe {
            libc::cfmakeraw(&mut settings);
        }
        settings.c_cflag |= libc::CLOCAL | libc::CREAD;
        settings.c_cflag &= !(libc::CRTSCTS | libc::CSTOPB | libc::PARENB);
        settings.c_cc[libc::VMIN] = 0;
        settings.c_cc[libc::VTIME] = 0;
        if unsafe { libc::cfsetispeed(&mut settings, speed) } < 0
            || unsafe { libc::cfsetospeed(&mut settings, speed) } < 0
            || unsafe { libc::tcsetattr(fd, libc::TCSANOW, &settings) } < 0
        {
            return Err(io::Error::last_os_error());
        }

        if unsafe { libc::tcflush(fd, libc::TCIOFLUSH) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(serial)
    }
}
impl Drop for Serial {
    fn drop(&mut self) {
        // SAFETY: the owned descriptor remains live until after this destructor.
        unsafe {
            libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.original);
            libc::ioctl(self.file.as_raw_fd(), libc::TIOCNXCL as _);
        }
    }
}
impl Transport for Serial {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }
    fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut descriptor = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor refers to one live fd for this bounded call.
            let result = unsafe {
                libc::poll(
                    &mut descriptor,
                    1,
                    remaining.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted && Instant::now() < deadline {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                return Ok(0);
            }
            if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            match self.file.read(bytes) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(0),
                result => return result,
            }
        }
    }
}
