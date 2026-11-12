// SPDX-License-Identifier: AGPL-3.0-or-later
//! Owned Windows security descriptors and same-user checks. No thread impersonation.
use std::{
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, LocalFree},
    Security::{Authorization::*, *},
    System::Threading::*,
};
pub fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let v: Vec<u16> = path.as_os_str().encode_wide().collect();
    if v.contains(&0) {
        return Err(io::Error::other("NUL in path"));
    }
    Ok(v.into_iter().chain(Some(0)).collect())
}
struct Allocation(*mut core::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}
fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: sid is supplied by a live token or security descriptor.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = Allocation(text.cast());
    let mut n = 0;
    unsafe {
        while *text.add(n) != 0 {
            n += 1;
        }
    }
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(text, n)
    }))
}
fn token_sid(token: HANDLE) -> io::Result<String> {
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut bytes);
    }
    if io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        || bytes == 0
    {
        return Err(io::Error::last_os_error());
    }
    // usize storage preserves TOKEN_USER pointer alignment.
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    sid_string(unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid })
}
fn process_sid(process: HANDLE) -> io::Result<String> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_sid(token.as_raw_handle())
}
pub fn current_sid() -> io::Result<String> {
    process_sid(unsafe { GetCurrentProcess() })
}
pub fn same_user_process(pid: u32) -> io::Result<bool> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let h = unsafe { OwnedHandle::from_raw_handle(h) };
    Ok(process_sid(h.as_raw_handle())? == current_sid()?)
}
pub struct Descriptor {
    allocation: Allocation,
}
impl Descriptor {
    pub fn private() -> io::Result<Self> {
        let s = format!("O:{0}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{0})", current_sid()?);
        Self::from_sddl(&s)
    }
    fn from_sddl(s: &str) -> io::Result<Self> {
        let s: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        let mut p = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                s.as_ptr(),
                1,
                &mut p,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            allocation: Allocation(p),
        })
    }
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.allocation.0,
            bInheritHandle: 0,
        }
    }
}
/// Fail closed on null DACL, foreign owner or any grant to another principal.
pub fn validate_handle(handle: std::os::windows::io::BorrowedHandle<'_>) -> io::Result<()> {
    let (mut owner, mut acl, mut sd) = (ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
    let code = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut sd,
        )
    };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let _allocation = Allocation(sd);
    if owner.is_null() || acl.is_null() || sid_string(owner)? != current_sid()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private object owner/DACL required",
        ));
    }
    let expected = current_sid()?;
    let mut grants_user = false;
    for i in 0..unsafe { (*acl).AceCount } {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(acl, u32::from(i), &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != 0
        /* ACCESS_ALLOWED_ACE_TYPE */
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unexpected private ACL entry",
            ));
        }
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = sid_string((&ace.SidStart as *const u32).cast_mut().cast())?;
        if sid != expected && sid != "S-1-5-18" {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private object grants access to another principal",
            ));
        }
        grants_user |= sid == expected && ace.Mask & 0x001f01ff == 0x001f01ff;
    }
    if !grants_user {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private object lacks user access",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_and_null_dacls_are_rejected() {
        use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
        for acl in ["D:P(A;OICI;FA;;;WD)", "D:NO_ACCESS_CONTROL"] {
            let root = std::env::temp_dir().join(format!(
                "ch-acl-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let descriptor =
                Descriptor::from_sddl(&format!("O:{}{acl}", current_sid().unwrap())).unwrap();
            let sa = descriptor.attributes();
            let path = wide(&root).unwrap();
            assert_ne!(unsafe { CreateDirectoryW(path.as_ptr(), &sa) }, 0);
            assert!(crate::private::check_dir(&root).is_err());
            assert!(crate::private::directory(&root).is_err());
            std::fs::remove_dir(&root).unwrap();
        }
    }
    #[test]
    fn directory_junction_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "ch-junction-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::private::directory(&root).unwrap();
        let target = root.join("target");
        crate::private::directory(&target).unwrap();
        let link = root.join("link");
        let result = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(crate::private::directory(&link).is_err());
        std::fs::remove_dir(&link).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
