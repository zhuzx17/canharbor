// SPDX-License-Identifier: AGPL-3.0-or-later
//! Private storage shared by IPC discovery, preferences, captures and evidence.
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::{
    fs::{MetadataExt, OpenOptionsExt},
    io::{AsHandle, FromRawHandle},
};
use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};
pub fn validate(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        let m = file.metadata()?;
        if m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private, owned file required",
            ));
        }
    }
    #[cfg(windows)]
    {
        crate::windows::validate_handle(file.as_handle())?;
        if file.metadata()?.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "reparse point rejected",
            ));
        }
    }
    Ok(())
}
pub fn check_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let m = fs::symlink_metadata(path)?;
        if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private, owned directory required",
            ));
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::*;
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        if !f.metadata()?.is_dir() {
            return Err(io::Error::other("expected directory"));
        }
        validate(&f)?;
    }
    Ok(())
}
pub fn create_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let made = fs::DirBuilder::new().mode(0o700).create(path);
    #[cfg(windows)]
    let made = {
        use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
        let sd = crate::windows::Descriptor::private()?;
        let sa = sd.attributes();
        let p = crate::windows::wide(path)?;
        if unsafe { CreateDirectoryW(p.as_ptr(), &sa) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    made?;
    check_dir(path)
}
pub fn directory(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !parent.exists()
    {
        directory(parent)?;
    }
    match create_directory(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => check_dir(path),
        Err(e) => Err(e),
    }
}
pub fn open_read(path: &Path) -> io::Result<File> {
    let f = crate::files::open_regular(path)?;
    validate(&f)?;
    Ok(f)
}
fn writable(path: &Path, new: bool) -> io::Result<File> {
    #[cfg(unix)]
    let f = {
        let mut o = OpenOptions::new();
        o.read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        if new {
            o.create_new(true);
        } else {
            o.create(true).truncate(false);
        }
        o.open(path)?
    };
    #[cfg(windows)]
    let f = {
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
            Storage::FileSystem::*,
        };
        let sd = crate::windows::Descriptor::private()?;
        let sa = sd.attributes();
        let p = crate::windows::wide(path)?;
        let h = unsafe {
            CreateFileW(
                p.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &sa,
                if new { CREATE_NEW } else { OPEN_ALWAYS },
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        unsafe { File::from_raw_handle(h) }
    };
    if !f.metadata()?.is_file() {
        return Err(io::Error::other("expected regular file"));
    }
    validate(&f)?;
    Ok(f)
}
pub fn create_new(path: &Path) -> io::Result<File> {
    writable(path, true)
}
pub fn open_lock(path: &Path) -> io::Result<File> {
    writable(path, false)
}
pub fn replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::rename(from, to)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::*;
        let a = crate::windows::wide(from)?;
        let b = crate::windows::wide(to)?;
        if unsafe {
            MoveFileExW(
                a.as_ptr(),
                b.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
pub fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    // Windows metadata commits use MOVEFILE_WRITE_THROUGH after file.sync_all.
    // This is not a claim of hardware power-loss durability on every filesystem.
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ch-private-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    #[test]
    fn private_directory_files_and_replacement() {
        use std::io::{Read, Write};
        let r = root();
        super::directory(&r).unwrap();
        let a = r.join("a");
        let b = r.join("b");
        super::create_new(&a).unwrap().write_all(b"one").unwrap();
        assert!(super::create_new(&a).is_err());
        super::create_new(&b).unwrap().write_all(b"two").unwrap();
        super::replace(&b, &a).unwrap();
        let mut s = String::new();
        super::open_read(&a)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "two");
        std::fs::remove_dir_all(r).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn rejects_public_directory_and_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let r = root();
        std::fs::create_dir(&r).unwrap();
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(super::directory(&r).is_err());
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = r.join("target");
        super::create_new(&target).unwrap();
        let link = r.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(super::open_read(&link).is_err());
        std::fs::remove_dir_all(r).unwrap();
    }
}
