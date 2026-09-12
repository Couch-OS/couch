//! Darwin extended ACLs can grant access even when POSIX mode is 0700.
use anyhow::{ensure, Result};
use std::ffi::c_void;
use std::{
    fs::OpenOptions,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
};
// Darwin sys/acl.h: ACL_TYPE_EXTENDED=0x100, ACL_FIRST_ENTRY=0.
// libc does not currently expose these macOS ACL calls. Returned ACL storage
// belongs to libSystem and is released with acl_free, never Rust's allocator.
unsafe extern "C" {
    fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut c_void;
    fn acl_get_entry(
        acl: *mut c_void,
        entry_id: libc::c_int,
        entry: *mut *mut c_void,
    ) -> libc::c_int;
    fn acl_free(value: *mut c_void) -> libc::c_int;
}
struct Acl(*mut c_void);
impl Drop for Acl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                acl_free(self.0);
            }
        }
    }
}
pub(super) fn validate_parent(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    unsafe {
        let acl = Acl(acl_get_fd_np(file.as_raw_fd(), 0x100));
        if acl.0.is_null() {
            let error = std::io::Error::last_os_error();
            // The descriptor is already open: ENOENT here means no extended
            // ACL attribute, not a missing path. Other retrieval failures reject.
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(());
            }
            return Err(error.into());
        }
        let mut entry = std::ptr::null_mut();
        let result = acl_get_entry(acl.0, 0, &mut entry);
        ensure!(
            result != 0,
            "session parent must not have an extended macOS ACL"
        );
        // Darwin returns -1/EINVAL for an empty ACL, unlike Linux's 0 result.
        // The ACL above was obtained directly from the kernel for this fd.
        let error = std::io::Error::last_os_error();
        if result != -1 || error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error.into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mode_0700_does_not_hide_inherited_extended_acl_grants() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        validate_parent(root.path()).unwrap();
        let status = std::process::Command::new("/bin/chmod").args(["+a", "everyone allow list,search,readattr,readextattr,readsecurity,file_inherit,directory_inherit"]).arg(root.path()).status().unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(validate_parent(root.path()).is_err());
        let child = root.path().join("inherited");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&child)
            .unwrap();
        assert_eq!(
            std::fs::metadata(&child).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(validate_parent(&child).is_err());
    }
}
