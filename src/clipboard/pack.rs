//! Apple "packed scrap" pasteboard serialization and its zlib framing.

use anyhow::{Context, Result, bail, ensure};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

const MAX_FLAVORS: u32 = 1024;
const MAX_TAGS: u32 = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flavor {
    /// UTI, e.g. `public.utf8-plain-text`. Empty for dynamic (`dyn.`) types, which are
    /// identified by `tags` instead.
    pub uti: String,
    pub flags: u32,
    /// UTType tag class -> tag, e.g. `public.mime-type` -> `text/plain`.
    pub tags: Vec<(String, String)>,
    pub data: Vec<u8>,
}

impl Flavor {
    pub fn tag(&self, class: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == class)
            .map(|(_, v)| v.as_str())
    }
}

/// One pasteboard item; a pasteboard holds one or more.
pub type Item = Vec<Flavor>;

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

pub fn pack(items: &[Item]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        out.extend_from_slice(&(item.len() as u32).to_be_bytes());
        for f in item {
            put_bytes(
                &mut out,
                if f.uti.starts_with("dyn.") {
                    b""
                } else {
                    f.uti.as_bytes()
                },
            );
            out.extend_from_slice(&f.flags.to_be_bytes());
            out.extend_from_slice(&(f.tags.len() as u32).to_be_bytes());
            for (k, v) in &f.tags {
                put_bytes(&mut out, k.as_bytes());
                put_bytes(&mut out, v.as_bytes());
            }
            put_bytes(&mut out, &f.data);
        }
    }
    out
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Result<u32> {
        ensure!(self.0.len() >= 4, "packed scrap truncated");
        let (h, t) = self.0.split_at(4);
        self.0 = t;
        Ok(u32::from_be_bytes(h.try_into().unwrap()))
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        ensure!(
            self.0.len() >= n,
            "packed scrap field of {n} bytes overruns buffer"
        );
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Ok(h)
    }

    fn string(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
}

/// Items are concatenated without an item count; the reader walks until the buffer ends.
pub fn unpack(buf: &[u8]) -> Result<Vec<Item>> {
    let mut r = Reader(buf);
    let mut items = Vec::new();
    while !r.0.is_empty() {
        let n = r.u32()?;
        ensure!(n <= MAX_FLAVORS, "packed scrap: {n} flavors");
        let mut item = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let uti = r.string()?;
            let flags = r.u32()?;
            let tag_count = r.u32()?;
            ensure!(tag_count <= MAX_TAGS, "packed scrap: {tag_count} tags");
            let tags = (0..tag_count)
                .map(|_| Ok((r.string()?, r.string()?)))
                .collect::<Result<_>>()?;
            let data = r.bytes()?.to_vec();
            item.push(Flavor {
                uti,
                flags,
                tags,
                data,
            });
        }
        items.push(item);
    }
    Ok(items)
}

/// zlib, level 9, ended with a sync flush and no stream end, as Apple's `CreateCompressedData`
/// does. Its `inflate(Z_SYNC_FLUSH)` treats any return other than `Z_OK` as an error, so a
/// finished stream (`Z_STREAM_END`) must not be sent.
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut c = Compress::new(Compression::new(9), true);
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    loop {
        if out.capacity() - out.len() < 64 {
            out.reserve(out.capacity().max(1024));
        }
        let consumed = c.total_in() as usize;
        c.compress_vec(&data[consumed..], &mut out, FlushCompress::Sync)
            .expect("deflate");
        if c.total_in() as usize == data.len() && out.len() < out.capacity() {
            return out;
        }
    }
}

pub fn decompress(data: &[u8], uncompressed_len: usize) -> Result<Vec<u8>> {
    let mut d = Decompress::new(true);
    let mut out = Vec::with_capacity(uncompressed_len + 1);
    loop {
        let (consumed, produced) = (d.total_in(), d.total_out());
        let status = d
            .decompress_vec(&data[consumed as usize..], &mut out, FlushDecompress::Sync)
            .context("inflate pasteboard")?;
        if status == Status::StreamEnd || d.total_in() as usize == data.len() {
            break;
        }
        if out.len() > uncompressed_len {
            bail!("pasteboard larger than announced {uncompressed_len}");
        }
        ensure!(
            d.total_in() != consumed || d.total_out() != produced,
            "inflate made no progress"
        );
    }
    ensure!(
        out.len() == uncompressed_len,
        "pasteboard inflated to {} bytes, header says {uncompressed_len}",
        out.len()
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_item() -> Item {
        vec![Flavor {
            uti: "public.utf8-plain-text".into(),
            flags: 0,
            tags: vec![("public.mime-type".into(), "text/plain".into())],
            data: b"hi".to_vec(),
        }]
    }

    #[test]
    fn pack_layout() {
        let p = pack(&[text_item()]);
        let mut want = vec![0, 0, 0, 1, 0, 0, 0, 22];
        want.extend_from_slice(b"public.utf8-plain-text");
        want.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 16]);
        want.extend_from_slice(b"public.mime-type");
        want.extend_from_slice(&[0, 0, 0, 10]);
        want.extend_from_slice(b"text/plain");
        want.extend_from_slice(&[0, 0, 0, 2, b'h', b'i']);
        assert_eq!(p, want);
    }

    #[test]
    fn round_trip_multiple_items_and_dyn() {
        let dyn_flavor = Flavor {
            uti: "dyn.ah62d4rv4gu8y".into(),
            flags: 0,
            tags: vec![("public.mime-type".into(), "application/x-foo".into())],
            data: vec![1, 2, 3],
        };
        let items = vec![text_item(), vec![dyn_flavor]];
        let back = unpack(&pack(&items)).unwrap();
        assert_eq!(back[0], items[0]);
        assert_eq!(back[1][0].uti, "");
        assert_eq!(
            back[1][0].tag("public.mime-type"),
            Some("application/x-foo")
        );
    }

    #[test]
    fn unpack_rejects_truncation() {
        let p = pack(&[text_item()]);
        assert!(unpack(&p[..p.len() - 1]).is_err());
        assert!(unpack(&[0, 0, 0x10, 0]).is_err());
    }

    #[test]
    fn unpack_enforces_count_caps() {
        let mut flavors = 1025u32.to_be_bytes().to_vec();
        let err = unpack(&flavors).unwrap_err().to_string();
        assert!(err.contains("1025 flavors"), "{err}");
        flavors.clear();
        flavors.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut tags = flavors.clone();
        tags.extend_from_slice(&65u32.to_be_bytes());
        let err = unpack(&tags).unwrap_err().to_string();
        assert!(err.contains("65 tags"), "{err}");
    }

    #[test]
    fn unpack_rejects_huge_field_length() {
        let mut p = vec![0, 0, 0, 1];
        p.extend_from_slice(&u32::MAX.to_be_bytes());
        p.extend_from_slice(&[1, 2, 3]);
        let err = unpack(&p).unwrap_err().to_string();
        assert!(err.contains("overruns buffer"), "{err}");
    }

    #[test]
    fn zlib_sync_flush_round_trip() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let z = compress(&data);
        assert_eq!(&z[..2], &[0x78, 0xda]);
        // Sync flush marker, no adler32 trailer.
        assert_eq!(&z[z.len() - 4..], &[0, 0, 0xff, 0xff]);
        assert_eq!(decompress(&z, data.len()).unwrap(), data);
        assert!(decompress(&z, data.len() - 1).is_err());
        assert_eq!(decompress(&compress(b""), 0).unwrap(), b"");
    }

    #[test]
    fn decompress_accepts_finished_streams() {
        use std::io::Write;
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(b"abc").unwrap();
        assert_eq!(decompress(&e.finish().unwrap(), 3).unwrap(), b"abc");
    }
}
