use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use gbm::{BufferObjectFlags, Device, Format, Modifier};

use super::Plane;

const DEFAULT_NODE: &str = "/dev/dri/renderD128";
pub(super) const MOD_LINEAR: u64 = 0;
pub(super) const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

fn explicit_modifiers(modifiers: &[u64]) -> Option<&[u64]> {
    if modifiers.is_empty() || modifiers.contains(&MOD_INVALID) {
        None
    } else {
        Some(modifiers)
    }
}

pub(super) fn supports_linear(modifiers: &[u64]) -> bool {
    explicit_modifiers(modifiers).is_none() || modifiers.contains(&MOD_LINEAR)
}

pub(super) fn import_modifier(modifier: u64, offered: &[u64]) -> u64 {
    // Keep the import implicit when the compositor did not offer the actual layout.
    if offered.contains(&MOD_INVALID) && !offered.contains(&modifier) {
        MOD_INVALID
    } else {
        modifier
    }
}

/// Where capture buffers live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Memory {
    /// GBM allocation (VRAM on this APU). CPU reads are uncached and slow.
    #[default]
    Gpu,
    /// Cached system RAM via udmabuf, LINEAR. Fast CPU reads; the compositor and VAAPI still
    /// import it as a dmabuf. Requires LINEAR or implicit modifier support.
    System,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Allocation {
    pub id: u64,
    pub modifier: u64,
    pub planes: Vec<Plane>,
}

pub struct Allocator {
    node: PathBuf,
    gbm: Device<File>,
    pub memory: Memory,
    udmabuf: Option<File>,
}

// SAFETY: The GBM device and the udmabuf file are owned solely by the `Allocator` and only used
// through it, and the capturer that owns it uses it from one thread at a time.
unsafe impl Send for Allocator {}

impl Allocator {
    pub fn new(node: Option<&Path>) -> Result<Self> {
        let node = node.unwrap_or(Path::new(DEFAULT_NODE)).to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .with_context(|| format!("open {}", node.display()))?;
        Ok(Self {
            gbm: Device::new(file)?,
            node,
            memory: Memory::Gpu,
            udmabuf: None,
        })
    }

    pub fn render_node(&self) -> &PathBuf {
        &self.node
    }

    /// Switches to the render node of `dev` (a dev_t naming either the primary or render node).
    pub fn use_device(&mut self, dev: u64) -> Result<()> {
        let node = render_node_for(dev)?;
        if node != self.node {
            let memory = self.memory;
            *self = Self::new(Some(&node))?;
            self.memory = memory;
        }
        Ok(())
    }

    /// Empty `modifiers` or INVALID lets the driver choose an implicit modifier.
    pub fn allocate(
        &mut self,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
    ) -> Result<Allocation> {
        if self.memory == Memory::System {
            super::validate_cpu_format(fourcc)?;
            if !supports_linear(modifiers) {
                tracing::error!(?modifiers, "CPU capture requires LINEAR buffers");
                bail!("CPU capture requires LINEAR buffers; offered modifiers: {modifiers:#x?}");
            }
            return self.allocate_udmabuf(width, height);
        }
        let format = Format::try_from(fourcc).context("unknown fourcc")?;
        let flags = BufferObjectFlags::RENDERING;
        let bo = if let Some(modifiers) = explicit_modifiers(modifiers) {
            self.gbm.create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                format,
                modifiers.iter().map(|&m| Modifier::from(m)),
                flags,
            )?
        } else {
            self.gbm
                .create_buffer_object::<()>(width, height, format, flags)?
        };
        let planes = (0..bo.plane_count() as i32)
            .map(|i| {
                Ok(Plane {
                    fd: bo.fd_for_plane(i)?,
                    offset: bo.offset(i),
                    stride: bo.stride_for_plane(i),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Allocation {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            modifier: bo.modifier().into(),
            planes,
        })
    }
}

const UDMABUF_CREATE: libc::c_ulong = 0x4018_7542;
const UDMABUF_FLAGS_CLOEXEC: u32 = 1;

#[repr(C)]
struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

impl Allocator {
    /// Single-plane 32 bpp LINEAR buffer backed by a sealed memfd.
    fn allocate_udmabuf(&mut self, width: u32, height: u32) -> Result<Allocation> {
        if self.udmabuf.is_none() {
            self.udmabuf = Some(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/udmabuf")
                    .context("open /dev/udmabuf")?,
            );
        }
        let stride = (width * 4).next_multiple_of(256);
        let page = 4096u64;
        let size = (stride as u64 * height as u64).next_multiple_of(page);
        // SAFETY: `memfd` is checked for failure before `OwnedFd` takes sole ownership.
        // `UdmabufCreate` is `repr(C)` with the layout of the kernel's struct udmabuf_create
        // and outlives the ioctl. The returned fd is checked for failure before
        // `OwnedFd::from_raw_fd` takes ownership.
        unsafe {
            let memfd = libc::memfd_create(
                c"wayshare-capture".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            );
            if memfd < 0 {
                return Err(std::io::Error::last_os_error()).context("memfd_create");
            }
            let memfd = OwnedFd::from_raw_fd(memfd);
            if libc::ftruncate(memfd.as_raw_fd(), size as libc::off_t) < 0
                || libc::fcntl(memfd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) < 0
            {
                return Err(std::io::Error::last_os_error()).context("size memfd");
            }
            let create = UdmabufCreate {
                memfd: memfd.as_raw_fd() as u32,
                flags: UDMABUF_FLAGS_CLOEXEC,
                offset: 0,
                size,
            };
            let fd = libc::ioctl(
                self.udmabuf.as_ref().unwrap().as_raw_fd(),
                UDMABUF_CREATE,
                &create,
            );
            if fd < 0 {
                return Err(std::io::Error::last_os_error()).context("UDMABUF_CREATE");
            }
            Ok(Allocation {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                modifier: MOD_LINEAR,
                planes: vec![Plane {
                    fd: OwnedFd::from_raw_fd(fd),
                    offset: 0,
                    stride,
                }],
            })
        }
    }
}

fn render_node_for(dev: u64) -> Result<PathBuf> {
    for entry in std::fs::read_dir("/dev/dri")? {
        let path = entry?.path();
        if std::fs::metadata(&path)?.rdev() != dev {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("renderD"))
        {
            return Ok(path);
        }
        let (major, minor) = (libc::major(dev), libc::minor(dev));
        let sys = format!("/sys/dev/char/{major}:{minor}/device/drm");
        for e in std::fs::read_dir(&sys)? {
            let name = e?.file_name();
            if name.to_string_lossy().starts_with("renderD") {
                return Ok(Path::new("/dev/dri").join(name));
            }
        }
    }
    bail!("no render node for dev {dev:#x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TILED: u64 = 0x0100_0000_0000_0001;

    #[test]
    fn invalid_and_empty_modifiers_use_implicit_allocation() {
        for modifiers in [&[][..], &[MOD_INVALID], &[TILED, MOD_INVALID, MOD_LINEAR]] {
            assert_eq!(explicit_modifiers(modifiers), None);
            assert!(supports_linear(modifiers));
        }
    }

    #[test]
    fn explicit_modifiers_keep_the_offered_layouts() {
        for modifiers in [&[MOD_LINEAR][..], &[TILED], &[TILED, MOD_LINEAR]] {
            assert_eq!(explicit_modifiers(modifiers), Some(modifiers));
            assert_eq!(supports_linear(modifiers), modifiers.contains(&MOD_LINEAR));
        }
    }

    #[test]
    fn implicit_import_does_not_require_an_unadvertised_layout() {
        assert_eq!(import_modifier(MOD_LINEAR, &[MOD_INVALID]), MOD_INVALID);
        assert_eq!(import_modifier(TILED, &[MOD_INVALID]), MOD_INVALID);
        assert_eq!(
            import_modifier(MOD_LINEAR, &[MOD_INVALID, MOD_LINEAR]),
            MOD_LINEAR
        );
        assert_eq!(import_modifier(TILED, &[TILED]), TILED);
        assert_eq!(import_modifier(MOD_LINEAR, &[]), MOD_LINEAR);
    }
}
