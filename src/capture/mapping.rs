use std::os::fd::{AsRawFd, OwnedFd};

use anyhow::{Context, Result};

const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_END: u64 = 4;

pub(super) struct Mapping {
    ptr: *mut u8,
    len: usize,
}

// Read-only view of shared memory; synchronisation goes through DMA_BUF_IOCTL_SYNC.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    pub(super) fn new(fd: &OwnedFd, len: usize) -> Result<Self> {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error()).context("mmap dmabuf");
        }
        Ok(Self {
            ptr: ptr.cast(),
            len,
        })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

/// CPU access window on a captured buffer; ends the dmabuf CPU access on drop.
pub struct ReadGuard<'a> {
    bytes: &'a [u8],
    fd: &'a OwnedFd,
    pub stride: usize,
}

impl<'a> ReadGuard<'a> {
    pub(super) fn new(
        map: &'a Mapping,
        fd: &'a OwnedFd,
        offset: usize,
        stride: usize,
    ) -> Result<Self> {
        sync(fd, DMA_BUF_SYNC_READ)?;
        let bytes = unsafe { std::slice::from_raw_parts(map.ptr.add(offset), map.len - offset) };
        Ok(Self { bytes, fd, stride })
    }

    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }

    pub fn row(&self, y: usize, width_bytes: usize) -> &[u8] {
        &self.bytes[y * self.stride..y * self.stride + width_bytes]
    }
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        let _ = sync(self.fd, DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END);
    }
}

fn sync(fd: &OwnedFd, flags: u64) -> Result<()> {
    loop {
        let r = unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &flags) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
            return Err(e).context("DMA_BUF_IOCTL_SYNC");
        }
    }
}
