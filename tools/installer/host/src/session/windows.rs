//! Windows state storage requires persistent ACLs and handle-pinned directories.
use anyhow::{ensure, Context, Result};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, HANDLE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        GetAce, GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE, ACE_HEADER,
        DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSID,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateDirectoryW, GetVolumeInformationW, GetVolumePathNameW, MoveFileExW,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE, MOVEFILE_WRITE_THROUGH, READ_CONTROL,
    },
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, FILE_PERSISTENT_ACLS},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

fn wide(value: &OsStr) -> Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    ensure!(!value.contains(&0), "path contains NUL");
    value.push(0);
    Ok(value)
}
struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
unsafe fn sid_string(sid: PSID) -> Result<String> {
    ensure!(!sid.is_null(), "missing Windows owner SID");
    let mut text = std::ptr::null_mut();
    ensure!(
        ConvertSidToStringSidW(sid, &mut text) != 0,
        "invalid Windows SID"
    );
    let _allocation = Allocation(text.cast());
    let mut length = 0;
    while length < 256 && *text.add(length) != 0 {
        length += 1;
    }
    ensure!(length < 256, "Windows SID exceeds bound");
    Ok(String::from_utf16(std::slice::from_raw_parts(
        text, length,
    ))?)
}
fn current_sid() -> Result<String> {
    unsafe {
        let mut handle = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let token = Token(handle);
        let mut bytes = 0;
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut bytes);
        ensure!(
            bytes > 0 && bytes <= 64 * 1024,
            "invalid Windows token size"
        );
        // usize storage provides alignment for TOKEN_USER and its SID pointer.
        let mut storage = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(
            token.0,
            TokenUser,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        sid_string((*(storage.as_ptr().cast::<TOKEN_USER>())).User.Sid)
    }
}
fn trusted(sid: &str, current: &str) -> bool {
    sid == current || sid == "S-1-5-18" || sid == "S-1-5-32-544"
}
fn require_acl_volume(path: &Path) -> Result<()> {
    let path = wide(path.as_os_str())?;
    let mut root = vec![0u16; 32768];
    let mut flags = 0;
    unsafe {
        if GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) == 0
            || GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut flags,
                std::ptr::null_mut(),
                0,
            ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    ensure!(
        flags & FILE_PERSISTENT_ACLS != 0,
        "private sessions require a filesystem with persistent Windows ACLs"
    );
    Ok(())
}
fn validate_handle(file: &File) -> Result<()> {
    let attributes = file.metadata()?.file_attributes();
    ensure!(
        attributes & FILE_ATTRIBUTE_DIRECTORY != 0
            && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "session directories must be real directories, not reparse points"
    );
    validate_acl(file)
}
fn validate_acl(file: &File) -> Result<()> {
    let current = current_sid()?;
    unsafe {
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let error = GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32).into());
        }
        let _allocation = Allocation(descriptor);
        ensure!(
            trusted(&sid_string(owner)?, &current),
            "session directory has an untrusted Windows owner"
        );
        ensure!(
            !dacl.is_null(),
            "session directory has an unrestricted Windows DACL"
        );
        for index in 0..(*dacl).AceCount {
            let mut entry = std::ptr::null_mut();
            ensure!(
                GetAce(dacl, index as u32, &mut entry) != 0,
                "invalid session directory ACL"
            );
            let header = &*entry.cast::<ACE_HEADER>();
            // Inherit-only entries cannot authorize access to this parent. New
            // session directories have a protected DACL and do not inherit them.
            if header.AceFlags as u32 & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            match header.AceType as u32 {
                ACCESS_DENIED_ACE_TYPE => {}
                ACCESS_ALLOWED_ACE_TYPE => {
                    ensure!(
                        header.AceSize as usize >= std::mem::size_of::<ACCESS_ALLOWED_ACE>(),
                        "invalid access ACE"
                    );
                    let allowed = &*entry.cast::<ACCESS_ALLOWED_ACE>();
                    if allowed.Mask != 0 {
                        let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
                        ensure!(
                            trusted(&sid_string(sid)?, &current),
                            "session parent grants access to another Windows identity"
                        );
                    }
                }
                _ => anyhow::bail!("unsupported Windows session ACL entry"),
            }
        }
    }
    Ok(())
}
/// Keep both the parent and new session directory leased until the guard drops.
/// Omitting FILE_SHARE_DELETE blocks rename/replacement while their paths are used.
pub(super) struct DirectoryLease {
    _file: File,
}
pub(super) fn lease_directory(path: &Path) -> Result<DirectoryLease> {
    require_acl_volume(path)?;
    let file = OpenOptions::new()
        // A metadata-only handle did not block rename on the current Windows
        // runner. Request directory read access so delete-sharing is enforced.
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .context("cannot pin private session directory")?;
    validate_handle(&file)?;
    Ok(DirectoryLease { _file: file })
}
pub(super) fn validate_parent(path: &Path) -> Result<()> {
    let _lease = lease_directory(path)?;
    Ok(())
}
fn create_with_acl(path: &Path, sddl: &str) -> Result<()> {
    let sddl = wide(OsStr::new(sddl))?;
    let name = wide(path.as_os_str())?;
    unsafe {
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let _allocation = Allocation(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        if CreateDirectoryW(name.as_ptr(), &attributes) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
pub(super) fn private_dir(path: &Path) -> Result<()> {
    require_acl_volume(path.parent().context("directory needs parent")?)?;
    // Use the actual token user, not OWNER RIGHTS: an elevated process can
    // default its object's owner to Administrators. The user still needs access.
    let sid = current_sid()?;
    create_with_acl(path, &format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"))?;
    let _lease = lease_directory(path)?;
    Ok(())
}
pub(super) fn sync_directory(_: &Path) -> Result<()> {
    // FlushFileBuffers does not provide the Unix directory-fsync contract.
    // Files are flushed before publication and MoveFileEx uses WRITE_THROUGH;
    // this is not a claim of tested power-loss durability on every filesystem.
    Ok(())
}
pub(super) fn publish(file: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    let source = file.into_temp_path();
    let from = wide(source.as_os_str())?;
    let to = wide(path.as_os_str())?;
    // Same-directory publication; no COPY_ALLOWED or REPLACE_EXISTING.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_user_acl_is_private_and_directory_lease_blocks_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private");
        private_dir(&path).unwrap();
        validate_parent(&path).unwrap();
        let event = tempfile::NamedTempFile::new_in(&path).unwrap();
        validate_acl(event.as_file()).unwrap();
        drop(event);
        let lease = lease_directory(&path).unwrap();
        assert!(std::fs::rename(&path, root.path().join("moved")).is_err());
        drop(lease);
        std::fs::rename(path, root.path().join("moved")).unwrap();
    }
    #[test]
    fn permissive_parent_acl_is_rejected_without_changing_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("public");
        let sid = current_sid().unwrap();
        create_with_acl(&path, &format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;WD)")).unwrap();
        assert!(validate_parent(&path).is_err());
        assert!(path.is_dir());
    }
}
