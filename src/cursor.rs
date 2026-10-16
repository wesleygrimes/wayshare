//! Cursor images from the user's XCursor theme, for the viewer to draw locally.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::source::CursorImage;

const IMAGE_CHUNK: u32 = 0xfffd_0002;

/// The theme's default pointer for a display at `scale`.
pub fn default_pointer(scale: f64) -> Result<CursorImage> {
    let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
    let size = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24u32);
    let path = ["default", "left_ptr"]
        .iter()
        .find_map(|name| find(&theme, name, &search_dirs()))
        .with_context(|| format!("no default pointer in cursor theme {theme:?}"))?;
    let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    parse(&data, (size as f64 * scale).round() as u32)
}

fn search_dirs() -> Vec<PathBuf> {
    if let Ok(p) = std::env::var("XCURSOR_PATH") {
        return std::env::split_paths(&p).collect();
    }
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local/share/icons"));
        dirs.push(home.join(".icons"));
    }
    dirs.extend(["/usr/share/icons", "/usr/share/pixmaps"].map(PathBuf::from));
    dirs
}

/// `theme/cursors/name` in the first directory that has it, following `Inherits`.
fn find(theme: &str, name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut queue = vec![theme.to_string()];
    let mut seen = Vec::new();
    while let Some(t) = queue.pop() {
        if seen.contains(&t) || seen.len() > 16 {
            continue;
        }
        for dir in dirs {
            let p = dir.join(&t).join("cursors").join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        let parents: Vec<String> = dirs
            .iter()
            .find_map(|d| inherits(&d.join(&t).join("index.theme")))
            .unwrap_or_default();
        seen.push(t);
        queue.extend(parents.into_iter().rev());
    }
    None
}

fn inherits(index: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(index).ok()?;
    let line = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Inherits="))?;
    Some(
        line.split([',', ';'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// The first image of the nominal size closest to `size`, with straight alpha.
pub fn parse(data: &[u8], size: u32) -> Result<CursorImage> {
    let u32_at = |off: usize| -> Result<u32> {
        let b = data.get(off..off + 4).context("XCursor file truncated")?;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    };
    ensure!(data.starts_with(b"Xcur"), "not an XCursor file");
    let header = u32_at(4)? as usize;
    let ntoc = u32_at(12)? as usize;
    let mut best: Option<(u32, usize)> = None;
    for i in 0..ntoc.min(1024) {
        let entry = header + i * 12;
        if u32_at(entry)? != IMAGE_CHUNK {
            continue;
        }
        let (nominal, pos) = (u32_at(entry + 4)?, u32_at(entry + 8)? as usize);
        if best.is_none_or(|(n, _)| nominal.abs_diff(size) < n.abs_diff(size)) {
            best = Some((nominal, pos));
        }
    }
    let (_, pos) = best.context("XCursor file has no images")?;
    let (w, h) = (u32_at(pos + 16)?, u32_at(pos + 20)?);
    ensure!(
        (1..=256).contains(&w) && (1..=256).contains(&h),
        "cursor image {w}x{h} out of range"
    );
    let (xhot, yhot) = (u32_at(pos + 24)?.min(w - 1), u32_at(pos + 28)?.min(h - 1));
    let pixels = (0..(w * h) as usize)
        .map(|i| u32_at(pos + 36 + 4 * i).map(unpremultiply))
        .collect::<Result<Vec<_>>>()?;
    Ok(CursorImage {
        serial: 1,
        width: w as u16,
        height: h as u16,
        hotspot: (xhot as u16, yhot as u16),
        pixels,
    })
}

fn unpremultiply(p: u32) -> u32 {
    let a = p >> 24;
    if a == 0 || a == 255 {
        return if a == 0 { 0 } else { p };
    }
    let c = |shift: u32| (((p >> shift) & 0xff) * 255 / a).min(255) << shift;
    a << 24 | c(16) | c(8) | c(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xcursor(images: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut v = b"Xcur".to_vec();
        for x in [16u32, 0x10000, images.len() as u32] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        let mut pos = 16 + 12 * images.len() as u32;
        let mut chunks = Vec::new();
        for &(nominal, w, h) in images {
            for x in [IMAGE_CHUNK, nominal, pos] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            for x in [36, IMAGE_CHUNK, nominal, 1, w, h, 1, 2, 0] {
                chunks.extend_from_slice(&u32::to_le_bytes(x));
            }
            for _ in 0..w * h {
                chunks.extend_from_slice(&0x80402010u32.to_le_bytes());
            }
            pos += 36 + 4 * w * h;
        }
        v.extend(chunks);
        v
    }

    #[test]
    fn picks_nearest_size_and_unpremultiplies() {
        let data = xcursor(&[(24, 24, 24), (48, 48, 48), (96, 96, 96)]);
        let img = parse(&data, 50).unwrap();
        assert_eq!((img.width, img.height, img.hotspot), (48, 48, (1, 2)));
        assert_eq!(img.pixels[0], 0x807f3f1f);
        assert_eq!(parse(&data, 20).unwrap().width, 24);
    }

    #[test]
    fn follows_inherits() {
        let root = std::env::temp_dir().join(format!("wayshare-cursor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("base/cursors")).unwrap();
        std::fs::create_dir_all(root.join("default")).unwrap();
        std::fs::write(
            root.join("default/index.theme"),
            "[Icon Theme]\nInherits=missing,base\n",
        )
        .unwrap();
        std::fs::write(root.join("base/cursors/left_ptr"), b"x").unwrap();
        let dirs = [root.clone()];
        assert_eq!(
            find("default", "left_ptr", &dirs),
            Some(root.join("base/cursors/left_ptr"))
        );
        assert_eq!(find("default", "default", &dirs), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
