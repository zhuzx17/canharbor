// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(unix)]
use canharbor_slcan::serial::{Serial, Transport};
use std::{
    ffi::CStr,
    fs::File,
    io::{Read, Write},
    os::fd::FromRawFd,
    path::PathBuf,
    time::{Duration, Instant},
};
fn pty() -> (File, PathBuf) {
    let (mut master, mut slave) = (-1, -1);
    let mut name = [0 as libc::c_char; 256];
    // SAFETY: output pointers and name buffer are live; default tty settings are requested.
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
    // SAFETY: openpty returned initialized descriptors and a NUL-terminated name.
    let master = unsafe { File::from_raw_fd(master) };
    drop(unsafe { File::from_raw_fd(slave) });
    let path = PathBuf::from(unsafe { CStr::from_ptr(name.as_ptr()) }.to_str().unwrap());
    (master, path)
}
#[test]
fn tty_is_exclusive_raw_and_bounded_without_hardware() {
    let (mut master, path) = pty();
    let mut serial = Serial::open(&path, 115200).unwrap();
    assert!(Serial::open(&path, 115200).is_err());
    master.write_all(b"t5511BC\r").unwrap();
    let mut bytes = [0; 64];
    let n = serial.read(&mut bytes, Duration::from_secs(1)).unwrap();
    assert_eq!(&bytes[..n], b"t5511BC\r");
    assert_eq!(serial.write(b"V\r").unwrap(), 2);
    master.read_exact(&mut bytes[..2]).unwrap();
    assert_eq!(&bytes[..2], b"V\r");
    let before = Instant::now();
    assert_eq!(
        serial.read(&mut bytes, Duration::from_millis(15)).unwrap(),
        0
    );
    assert!(before.elapsed() < Duration::from_secs(1));
    drop(serial);
    let mut serial = Serial::open(&path, 115200).unwrap();
    drop(master);
    assert!(serial.read(&mut bytes, Duration::from_millis(15)).is_err());
}
#[test]
fn refuses_non_tty_unsupported_baud_and_symlinks() {
    assert!(Serial::open(std::path::Path::new("/dev/null"), 115200).is_err());
    assert!(Serial::open(std::path::Path::new("/dev/null"), 12345).is_err());
    let (_master, path) = pty();
    let link = std::env::temp_dir().join(format!("canharbor-serial-link-{}", std::process::id()));
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let result = Serial::open(&link, 115200);
    std::fs::remove_file(link).unwrap();
    assert!(result.is_err());
}
