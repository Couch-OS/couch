//! Read-only Linux helper. No path resolver, writable opener or cache fallback.
use crate::{require, Hash, Result, ALIGNMENT, CHUNK};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;

struct Aligned(*mut libc::c_void);
impl Drop for Aligned {
    fn drop(&mut self) {
        unsafe { libc::free(self.0) }
    }
}

pub fn hash(file: &File, size: u64) -> Result<Hash> {
    hash_with_progress(file, size, &mut |_, _| Ok(()))
}

pub fn hash_with_progress(
    file: &File,
    size: u64,
    progress: &mut dyn FnMut(u64, u64) -> Result<()>,
) -> Result<Hash> {
    let metadata = file.metadata()?;
    require(
        metadata.is_file() || metadata.file_type().is_block_device(),
        "unsupported readback descriptor type",
    )?;
    if metadata.is_file() {
        require(
            metadata.len() == size,
            "readback must cover the whole regular fixture",
        )?;
    }
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    require(
        flags >= 0 && flags & libc::O_ACCMODE == libc::O_RDONLY && flags & libc::O_DIRECT != 0,
        "independent readback requires O_RDONLY|O_DIRECT; no fallback",
    )?;
    require(
        size > 0 && size % ALIGNMENT == 0 && size <= i64::MAX as u64,
        "unaligned direct readback size",
    )?;
    let mut pointer = std::ptr::null_mut();
    let code = unsafe { libc::posix_memalign(&mut pointer, ALIGNMENT as usize, CHUNK) };
    if code != 0 {
        return Err(std::io::Error::from_raw_os_error(code));
    }
    let memory = Aligned(pointer);
    let mut digest = Sha256::new();
    let mut offset = 0;
    progress(0, size)?;
    while offset < size {
        let count = (size - offset).min(CHUNK as u64) as usize;
        let read = unsafe { libc::pread(fd, memory.0, count, offset as libc::off_t) };
        if read < 0 {
            return Err(std::io::Error::last_os_error());
        }
        require(read as usize == count, "short direct readback")?;
        let bytes = unsafe { std::slice::from_raw_parts(memory.0.cast::<u8>(), count) };
        digest.update(bytes);
        offset += count as u64;
        progress(offset, size)?;
    }
    Ok(digest.finalize().into())
}
