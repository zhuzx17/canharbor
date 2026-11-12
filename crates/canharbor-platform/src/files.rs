// SPDX-License-Identifier: AGPL-3.0-or-later
//! Handle-based observations of regular files. This does not confer trust in file contents.
use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
    time::SystemTime,
};
#[derive(Debug, PartialEq, Eq)]
pub struct Stamp {
    pub bytes: u64,
    modified: SystemTime,
    identity: (u64, u64),
    change: (i64, i64),
}
pub fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    let file = options.open(path)?;
    let m = file.metadata()?;
    if !m.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(io::Error::other("reparse point rejected"));
        }
    }
    Ok(file)
}
pub fn stamp(file: &File) -> io::Result<Stamp> {
    let m = file.metadata()?;
    #[cfg(unix)]
    let (identity, change) = {
        use std::os::unix::fs::MetadataExt;
        ((m.dev(), m.ino()), (m.ctime(), m.ctime_nsec()))
    };
    #[cfg(windows)]
    let (identity, change) = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::*;
        let mut id: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: live owned file handle and correctly sized writable outputs.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut id) } == 0
            || unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileBasicInfo,
                    (&mut basic as *mut FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        (
            (
                u64::from(id.dwVolumeSerialNumber),
                (u64::from(id.nFileIndexHigh) << 32) | u64::from(id.nFileIndexLow),
            ),
            (basic.ChangeTime, 0),
        )
    };
    Ok(Stamp {
        bytes: m.len(),
        modified: m.modified()?,
        identity,
        change,
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn directories_are_not_file_sources() {
        assert!(super::open_regular(&std::env::temp_dir()).is_err());
    }
    #[test]
    fn source_stamp_detects_content_change() {
        use std::{
            io::Write,
            time::{SystemTime, UNIX_EPOCH},
        };
        let p = std::env::temp_dir().join(format!(
            "ch-stamp-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&p)
            .unwrap();
        f.write_all(b"a").unwrap();
        let a = super::stamp(&f).unwrap();
        f.write_all(b"bc").unwrap();
        assert_ne!(a, super::stamp(&f).unwrap());
        drop(f);
        std::fs::remove_file(p).unwrap();
    }
}
