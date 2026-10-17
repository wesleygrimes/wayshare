use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use gbm::{BufferObjectFlags, Device, Format, Modifier};

use super::Plane;

const DEFAULT_NODE: &str = "/dev/dri/renderD128";
pub(super) const MOD_LINEAR: u64 = 0;
pub(super) const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

fn explicit_modifiers(modifiers: &[u64]) -> Vec<u64> {
    modifiers
        .iter()
        .copied()
        .filter(|&m| m != MOD_INVALID)
        .collect()
}

pub(super) fn supports_linear(modifiers: &[u64]) -> bool {
    modifiers.iter().all(|&m| m == MOD_INVALID) || modifiers.contains(&MOD_LINEAR)
}

pub(super) fn import_modifier(modifier: u64, offered: &[u64]) -> Result<u64> {
    if offered.contains(&modifier) {
        Ok(modifier)
    } else if offered.is_empty() || offered.contains(&MOD_INVALID) {
        // A format-only advertisement permits implicit import, not an arbitrary modifier.
        Ok(MOD_INVALID)
    } else {
        bail!("allocated modifier {modifier:#018x} was not advertised")
    }
}

/// Where capture buffers live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Memory {
    /// GBM allocation. Nonlinear buffers use GBM for CPU reads.
    #[default]
    Gpu,
    /// Prefer cached system RAM via udmabuf, LINEAR. Use GBM when only tiled layouts are offered.
    System,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Allocation {
    pub id: u64,
    pub modifier: u64,
    pub planes: Vec<Plane>,
    pub gbm: Option<GbmBuffer>,
}

pub(super) struct GbmBuffer {
    bo: Mutex<gbm::BufferObject<()>>,
    // Keep the device fd open until the buffer is destroyed, even after a node switch.
    _device: Device<Arc<File>>,
}

impl GbmBuffer {
    pub fn with_read<T>(
        &self,
        width: u32,
        height: u32,
        read: impl FnOnce(&super::ReadGuard<'_>) -> T,
    ) -> Result<T> {
        let bo = self.bo.lock().unwrap();
        // The callback consumes GBM's detiled view before map returns and unmaps it.
        bo.map(0, 0, width, height, |map| {
            read(&super::ReadGuard::borrowed(
                map.buffer(),
                map.stride() as usize,
            ))
        })
        .context("gbm_bo_map capture buffer")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum AllocationPath {
    Udmabuf,
    GbmImplicit,
    GbmExplicit(Vec<u64>),
}

pub(super) fn allocation_path(memory: Memory, modifiers: &[u64]) -> AllocationPath {
    if memory == Memory::System && supports_linear(modifiers) {
        AllocationPath::Udmabuf
    } else {
        let explicit = explicit_modifiers(modifiers);
        if explicit.is_empty() {
            AllocationPath::GbmImplicit
        } else {
            AllocationPath::GbmExplicit(explicit)
        }
    }
}

pub struct Allocator {
    node: PathBuf,
    gbm: Device<Arc<File>>,
    pub memory: Memory,
    udmabuf: Option<File>,
}

impl Allocator {
    pub fn new(node: Option<&Path>) -> Result<Self> {
        let node = node.unwrap_or(Path::new(DEFAULT_NODE)).to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .with_context(|| format!("open {}", node.display()))?;
        Ok(Self {
            gbm: Device::new(Arc::new(file))?,
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

    /// Use explicit modifiers when offered; an empty list or INVALID alone is implicit.
    pub fn allocate(
        &mut self,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
    ) -> Result<Allocation> {
        if self.memory == Memory::System {
            super::validate_cpu_format(fourcc)?;
        }
        let path = allocation_path(self.memory, modifiers);
        if path == AllocationPath::Udmabuf {
            return self.allocate_udmabuf(width, height);
        }
        let format = Format::try_from(fourcc).context("unknown fourcc")?;
        let flags = BufferObjectFlags::RENDERING;
        let bo = match path {
            AllocationPath::GbmExplicit(modifiers) => {
                self.gbm.create_buffer_object_with_modifiers2::<()>(
                    width,
                    height,
                    format,
                    modifiers.into_iter().map(Modifier::from),
                    flags,
                )?
            }
            AllocationPath::GbmImplicit => self
                .gbm
                .create_buffer_object::<()>(width, height, format, flags)?,
            AllocationPath::Udmabuf => unreachable!(),
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
            gbm: Some(GbmBuffer {
                bo: Mutex::new(bo),
                _device: self.gbm.clone(),
            }),
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
                gbm: None,
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
        for modifiers in [&[][..], &[MOD_INVALID]] {
            assert!(explicit_modifiers(modifiers).is_empty());
            assert!(supports_linear(modifiers));
        }
    }

    #[test]
    fn explicit_modifiers_keep_the_offered_layouts() {
        for modifiers in [&[MOD_LINEAR][..], &[TILED], &[TILED, MOD_LINEAR]] {
            assert_eq!(explicit_modifiers(modifiers), modifiers);
            assert_eq!(supports_linear(modifiers), modifiers.contains(&MOD_LINEAR));
        }
    }

    #[test]
    fn mixed_offers_keep_explicit_modifiers() {
        let offered = [TILED, MOD_INVALID, MOD_LINEAR];
        assert_eq!(explicit_modifiers(&offered), vec![TILED, MOD_LINEAR]);
        assert_eq!(
            allocation_path(Memory::Gpu, &offered),
            AllocationPath::GbmExplicit(vec![TILED, MOD_LINEAR])
        );
        assert_eq!(
            allocation_path(Memory::System, &[TILED, MOD_INVALID]),
            AllocationPath::GbmExplicit(vec![TILED])
        );
        assert_eq!(
            import_modifier(TILED, &[MOD_INVALID, MOD_LINEAR]).unwrap(),
            MOD_INVALID
        );
    }

    #[test]
    fn implicit_import_does_not_require_an_unadvertised_layout() {
        assert_eq!(
            import_modifier(MOD_LINEAR, &[MOD_INVALID]).unwrap(),
            MOD_INVALID
        );
        assert_eq!(import_modifier(TILED, &[MOD_INVALID]).unwrap(), MOD_INVALID);
        assert_eq!(
            import_modifier(MOD_LINEAR, &[MOD_INVALID, MOD_LINEAR]).unwrap(),
            MOD_LINEAR
        );
        assert_eq!(import_modifier(TILED, &[TILED]).unwrap(), TILED);
        assert_eq!(import_modifier(MOD_LINEAR, &[]).unwrap(), MOD_INVALID);
        assert_eq!(import_modifier(TILED, &[]).unwrap(), MOD_INVALID);
        assert!(import_modifier(TILED, &[MOD_LINEAR]).is_err());
        assert!(import_modifier(MOD_LINEAR, &[TILED]).is_err());
    }
}
