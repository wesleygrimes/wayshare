//! Per-tile change detection on the GPU, optionally fused with RGB -> NV12 conversion into the
//! encoder's input surfaces so each captured pixel is read once and only one GPU -> CPU sync
//! happens per frame.
//!
//! One workgroup per 16x16 block keeps two position-weighted sums of the block's pixels and
//! compares them with the previous frame's. Each pixel's weight in `h1` is odd, so any change
//! to a single pixel always changes `h1`; several changes would have to cancel in both sums at
//! once to be missed.

use std::os::fd::AsRawFd;

use anyhow::{Result, bail};
use glow::HasContext;
use khronos_egl as egl;

use super::DmabufBuffer;
use super::gl::{Gl, PlaneDesc};
use crate::encode::{TILE_COUNT, TileLayout};

const SHADER: &str = r#"
layout(local_size_x = 16, local_size_y = 16) in;
layout(binding = 0) uniform sampler2D cur;
layout(r32ui, binding = 0) uniform uimage2D hashes;
layout(std430, binding = 2) buffer Flags { uint changed[4]; };
layout(location = 0) uniform ivec2 size;
layout(location = 1) uniform int tile_height;
#ifdef CONVERT
layout(r8, binding = 1) writeonly uniform image2D luma[4];
layout(rg8, binding = 5) writeonly uniform image2D chroma[4];
layout(location = 2) uniform vec3 ky;
layout(location = 3) uniform vec3 ku;
layout(location = 4) uniform vec3 kv;
layout(location = 5) uniform float y_off;
shared vec3 px[16][16];
#endif
shared uint h1;
shared uint h2;

void main() {
    uvec2 l = gl_LocalInvocationID.xy;
    uint i = gl_LocalInvocationIndex;
    if (i == 0u) { h1 = 0u; h2 = 0u; }
    barrier();
    ivec2 p = ivec2(gl_GlobalInvocationID.xy);
    // Rows/columns past the frame (strip padding) repeat the last real ones.
    vec3 c = texelFetch(cur, min(p, size - 1), 0).rgb;
    if (p.x < size.x && p.y < size.y) {
        uint u = packUnorm4x8(vec4(c, 0.0));
        atomicAdd(h1, u * ((2u * i + 1u) * 0x9E3779B1u));
        atomicAdd(h2, (u ^ (i * 0x85EBCA77u)) * 0xC2B2AE3Du);
    }
#ifdef CONVERT
    // Workgroups never straddle strips (tile_height is a multiple of 16).
    int t = int(gl_WorkGroupID.y) * 16 / tile_height;
    ivec2 q = ivec2(p.x, p.y - t * tile_height);
    px[l.y][l.x] = c;
    imageStore(luma[t], q, vec4(dot(ky, c) + y_off));
#endif
    barrier();
#ifdef CONVERT
    if ((l.x & 1u) == 0u && (l.y & 1u) == 0u) {
        vec3 a = (px[l.y][l.x] + px[l.y][l.x + 1u] + px[l.y + 1u][l.x] + px[l.y + 1u][l.x + 1u]) * 0.25;
        imageStore(chroma[t], q / 2, vec4(dot(ku, a) + 0.5, dot(kv, a) + 0.5, 0.0, 0.0));
    }
#endif
    if (i == 0u && int(gl_WorkGroupID.y) * 16 < size.y) {
        ivec2 b = ivec2(gl_WorkGroupID.xy);
        ivec2 o = ivec2(2 * b.x, b.y);
        if (imageLoad(hashes, o).r != h1 || imageLoad(hashes, o + ivec2(1, 0)).r != h2) {
            imageStore(hashes, o, uvec4(h1));
            imageStore(hashes, o + ivec2(1, 0), uvec4(h2));
            changed[min(b.y * 16 / tile_height, 3)] = 1u;
        }
    }
}
"#;

const MAX_IMPORTS: usize = 8;

/// One NV12 strip surface the fused pass writes: luma (R8) and interleaved chroma (GR88).
pub struct Nv12Target {
    pub y: PlaneDesc,
    pub uv: PlaneDesc,
}

type Imported = (egl::Image, glow::Texture);

pub struct TileDiffer {
    layout: TileLayout,
    gl: Gl,
    program: glow::Program,
    /// Two hash words per 16x16 block of the last frame.
    hashes: glow::Texture,
    flags: glow::Buffer,
    imports: Vec<(u64, Imported)>,
    /// `[slot][tile]` (luma, chroma) textures when converting.
    targets: Vec<Vec<(Imported, Imported)>>,
    coeffs: [[f32; 3]; 3],
    y_off: f32,
    primed: bool,
}

impl TileDiffer {
    /// Change detection only.
    pub fn new(layout: TileLayout) -> Result<Self> {
        Self::build(layout, Vec::new(), true)
    }

    /// Change detection plus BT.709 conversion into `targets[slot][tile]`.
    pub fn with_conversion(
        layout: TileLayout,
        targets: Vec<Vec<Nv12Target>>,
        full_range: bool,
    ) -> Result<Self> {
        Self::build(layout, targets, full_range)
    }

    fn build(layout: TileLayout, targets: Vec<Vec<Nv12Target>>, full_range: bool) -> Result<Self> {
        let gl = Gl::new()?;
        gl.make_current()?;
        let convert = !targets.is_empty();
        let src = format!(
            "#version 430\n{}{SHADER}",
            if convert { "#define CONVERT\n" } else { "" }
        );
        type Parts = (
            glow::Program,
            glow::Texture,
            glow::Buffer,
            Vec<Vec<(Imported, Imported)>>,
        );
        let r = (|| -> Result<Parts> {
            let program = gl.program(&src)?;
            let g = &gl.gl;
            // SAFETY: The GL context was made current on this thread above; the calls only
            // create and size objects owned by that context.
            let (hashes, flags) = unsafe {
                let hashes = g.create_texture().map_err(|e| anyhow::anyhow!(e))?;
                g.bind_texture(glow::TEXTURE_2D, Some(hashes));
                let (bw, bh) = (layout.width.div_ceil(16), layout.height.div_ceil(16));
                g.tex_storage_2d(glow::TEXTURE_2D, 1, glow::R32UI, 2 * bw as i32, bh as i32);
                let flags = g.create_buffer().map_err(|e| anyhow::anyhow!(e))?;
                g.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(flags));
                g.buffer_data_size(
                    glow::SHADER_STORAGE_BUFFER,
                    4 * TILE_COUNT as i32,
                    glow::DYNAMIC_READ,
                );
                (hashes, flags)
            };
            let targets = targets
                .iter()
                .map(|slot| {
                    slot.iter()
                        .map(|t| Ok((gl.import(&t.y)?, gl.import(&t.uv)?)))
                        .collect::<Result<Vec<_>>>()
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((program, hashes, flags, targets))
        })();
        gl.release()?;
        let (program, hashes, flags, targets) = r?;
        // BT.709 (Kr 0.2126, Kb 0.0722), scaled for the output range.
        let (kr, kb) = (0.2126f32, 0.0722f32);
        let kg = 1.0 - kr - kb;
        let (ys, cs) = if full_range {
            (1.0, 1.0)
        } else {
            (219.0 / 255.0, 224.0 / 255.0)
        };
        let coeffs = [
            [kr * ys, kg * ys, kb * ys],
            [
                -kr / (2.0 * (1.0 - kb)) * cs,
                -kg / (2.0 * (1.0 - kb)) * cs,
                0.5 * cs,
            ],
            [
                0.5 * cs,
                -kg / (2.0 * (1.0 - kr)) * cs,
                -kb / (2.0 * (1.0 - kr)) * cs,
            ],
        ];
        Ok(Self {
            layout,
            gl,
            program,
            hashes,
            flags,
            imports: Vec::new(),
            targets,
            coeffs,
            y_off: if full_range { 0.0 } else { 16.0 / 255.0 },
            primed: false,
        })
    }

    pub fn converts(&self) -> bool {
        !self.targets.is_empty()
    }

    /// Makes the next call report every tile as changed.
    pub fn reset(&mut self) {
        self.primed = false;
    }

    /// Tiles of `buf` that differ from the previous call's buffer. Updates the reference.
    pub fn diff(&mut self, buf: &DmabufBuffer) -> Result<[bool; TILE_COUNT]> {
        self.run(buf, None)
    }

    /// `diff`, and converts all strips into slot `slot`'s targets. When this returns, the GPU
    /// has finished writing them.
    pub fn diff_convert(&mut self, buf: &DmabufBuffer, slot: usize) -> Result<[bool; TILE_COUNT]> {
        if slot >= self.targets.len() {
            bail!("no conversion targets for slot {slot}");
        }
        self.run(buf, Some(slot))
    }

    fn run(&mut self, buf: &DmabufBuffer, slot: Option<usize>) -> Result<[bool; TILE_COUNT]> {
        self.gl.make_current()?;
        let r = self.dispatch(buf, slot);
        self.gl.release()?;
        let changed = r?;
        Ok(if std::mem::replace(&mut self.primed, true) {
            changed
        } else {
            [true; TILE_COUNT]
        })
    }

    fn dispatch(&mut self, buf: &DmabufBuffer, slot: Option<usize>) -> Result<[bool; TILE_COUNT]> {
        let tex = self.import(buf)?;
        let l = self.layout;
        let gl = &self.gl.gl;
        // SAFETY: `detect` makes the GL context current before calling `dispatch`, and every
        // texture, buffer and program bound here was created on it and is still alive.
        // `map_buffer_range` is checked for null before the slice is built, covers `TILE_COUNT`
        // u32 flags, and is unmapped before the block ends.
        unsafe {
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(self.flags));
            gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, &[0; 4 * TILE_COUNT]);
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(self.flags));
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.bind_image_texture(
                0,
                Some(self.hashes),
                0,
                false,
                0,
                glow::READ_WRITE,
                glow::R32UI,
            );
            let loc = |n| glow::NativeUniformLocation(n);
            gl.uniform_2_i32(Some(&loc(0)), l.width as i32, l.height as i32);
            gl.uniform_1_i32(Some(&loc(1)), l.tile_height as i32);
            let rows = match slot {
                Some(s) => {
                    for (t, ((_, y), (_, uv))) in self.targets[s].iter().enumerate() {
                        gl.bind_image_texture(
                            1 + t as u32,
                            Some(*y),
                            0,
                            false,
                            0,
                            glow::WRITE_ONLY,
                            glow::R8,
                        );
                        gl.bind_image_texture(
                            5 + t as u32,
                            Some(*uv),
                            0,
                            false,
                            0,
                            glow::WRITE_ONLY,
                            glow::RG8,
                        );
                    }
                    let [ky, ku, kv] = self.coeffs;
                    gl.uniform_3_f32(Some(&loc(2)), ky[0], ky[1], ky[2]);
                    gl.uniform_3_f32(Some(&loc(3)), ku[0], ku[1], ku[2]);
                    gl.uniform_3_f32(Some(&loc(4)), kv[0], kv[1], kv[2]);
                    gl.uniform_1_f32(Some(&loc(5)), self.y_off);
                    TILE_COUNT as u32 * l.tile_height
                }
                None => l.height,
            };
            gl.dispatch_compute(l.width.div_ceil(16), rows.div_ceil(16), 1);
            gl.memory_barrier(glow::ALL_BARRIER_BITS);
            // Mapping waits for the dispatch, image writes included.
            let p = gl.map_buffer_range(
                glow::SHADER_STORAGE_BUFFER,
                0,
                4 * TILE_COUNT as i32,
                glow::MAP_READ_BIT,
            );
            if p.is_null() {
                bail!("map diff flags: GL error {:#x}", gl.get_error());
            }
            let flags = std::slice::from_raw_parts(p as *const u32, TILE_COUNT);
            let changed = std::array::from_fn(|i| flags[i] != 0);
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
            if slot.is_some() {
                // The encoder reads the targets next; make sure every write has landed.
                gl.finish();
            }
            Ok(changed)
        }
    }

    fn import(&mut self, buf: &DmabufBuffer) -> Result<glow::Texture> {
        if let Some((_, (_, t))) = self.imports.iter().find(|(id, _)| *id == buf.id) {
            return Ok(*t);
        }
        if buf.planes.len() != 1 {
            bail!("diff supports single-plane buffers only");
        }
        let p = &buf.planes[0];
        let imported = self.gl.import(&PlaneDesc {
            fd: p.fd.as_raw_fd(),
            fourcc: buf.fourcc,
            width: buf.width,
            height: buf.height,
            offset: p.offset,
            pitch: p.stride,
            modifier: buf.modifier,
        })?;
        if self.imports.len() >= MAX_IMPORTS {
            let (_, old) = self.imports.remove(0);
            self.gl.forget(old);
        }
        let tex = imported.1;
        self.imports.push((buf.id, imported));
        Ok(tex)
    }
}

impl Drop for TileDiffer {
    fn drop(&mut self) {
        if self.gl.make_current().is_ok() {
            for (_, i) in self.imports.drain(..) {
                self.gl.forget(i);
            }
            for slot in self.targets.drain(..) {
                for (y, uv) in slot {
                    self.gl.forget(y);
                    self.gl.forget(uv);
                }
            }
            // SAFETY: `make_current` succeeded above, and these handles were created on this
            // context and are deleted only here.
            unsafe {
                self.gl.gl.delete_texture(self.hashes);
                self.gl.gl.delete_buffer(self.flags);
                self.gl.gl.delete_program(self.program);
            }
            let _ = self.gl.release();
        }
    }
}
