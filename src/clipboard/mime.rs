//! Pasteboard UTIs <-> Wayland MIME types.

use super::pack::{Flavor, Item};

pub const TEXT: &str = "text/plain;charset=utf-8";
pub const HTML: &str = "text/html";
pub const RTF: &str = "text/rtf";
pub const PNG: &str = "image/png";
pub const JPEG: &str = "image/jpeg";
pub const TIFF: &str = "image/tiff";

const UTI_MIME_TAG: &str = "public.mime-type";

/// (UTI, canonical MIME, extra MIME names Linux clients ask for). Order = Mac flavor order.
const TABLE: &[(&str, &str, &[&str])] = &[
    ("public.rtf", RTF, &["application/rtf"]),
    ("public.html", HTML, &[]),
    ("public.png", PNG, &[]),
    ("public.jpeg", JPEG, &[]),
    ("public.tiff", TIFF, &[]),
    (
        "public.utf8-plain-text",
        TEXT,
        &["UTF8_STRING", "text/plain"],
    ),
];

/// Clipboard content as canonical MIME type -> bytes, in preference order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipContent(pub Vec<(String, Vec<u8>)>);

impl ClipContent {
    pub fn text(text: &str) -> Self {
        Self(vec![(TEXT.into(), text.as_bytes().to_vec())])
    }

    pub fn get(&self, mime: &str) -> Option<&[u8]> {
        self.0
            .iter()
            .find(|(m, _)| m == mime)
            .map(|(_, d)| d.as_slice())
    }

    pub fn as_text(&self) -> Option<String> {
        self.get(TEXT)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn total_len(&self) -> usize {
        self.0.iter().map(|(_, d)| d.len()).sum()
    }

    /// Sizes per MIME type; safe to log (no content).
    pub fn summary(&self) -> Vec<(String, usize)> {
        self.0.iter().map(|(m, d)| (m.clone(), d.len())).collect()
    }

    /// First pasteboard item, mapped to the MIME types we bridge.
    pub fn from_items(items: &[Item]) -> Self {
        let Some(item) = items.first() else {
            return Self::default();
        };
        let mut out = Vec::new();
        for &(uti, mime, _) in TABLE {
            if let Some(f) = item
                .iter()
                .find(|f| f.uti == uti || (f.uti.is_empty() && f.tag(UTI_MIME_TAG) == Some(mime)))
            {
                out.push((mime.to_string(), f.data.clone()));
            }
        }
        if !out.iter().any(|(m, _)| m == TEXT)
            && let Some(f) = item.iter().find(|f| f.uti == "public.utf16-plain-text")
        {
            out.push((TEXT.into(), utf16le_to_utf8(&f.data).into_bytes()));
        }
        Self(out)
    }

    pub fn to_items(&self) -> Vec<Item> {
        let item: Item = TABLE
            .iter()
            .filter_map(|&(uti, mime, _)| {
                self.get(mime).map(|d| Flavor {
                    uti: uti.into(),
                    flags: 0,
                    tags: vec![(UTI_MIME_TAG.into(), mime.into())],
                    data: d.to_vec(),
                })
            })
            .collect();
        if item.is_empty() { vec![] } else { vec![item] }
    }
}

/// Canonical MIME for a UTI, if bridged.
pub fn mime_for_uti(uti: &str) -> Option<&'static str> {
    TABLE.iter().find(|(u, _, _)| *u == uti).map(|(_, m, _)| *m)
}

/// Every MIME name to offer on Wayland for a canonical type.
pub fn offered_names(mime: &str) -> Vec<&'static str> {
    TABLE
        .iter()
        .filter(|(_, m, _)| *m == mime)
        .flat_map(|(_, m, extra)| std::iter::once(*m).chain(extra.iter().copied()))
        .collect()
}

/// Canonical MIME for a name a Wayland client offered or asked for.
pub fn canonical(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    TABLE
        .iter()
        .find(|(_, m, extra)| lower == *m || extra.iter().any(|e| e.eq_ignore_ascii_case(name)))
        .map(|(_, m, _)| *m)
        .or_else(|| (lower == "text/plain;charset=utf8").then_some(TEXT))
}

fn utf16le_to_utf8(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let s = String::from_utf16_lossy(&units);
    s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flavor(uti: &str, data: &[u8]) -> Flavor {
        Flavor {
            uti: uti.into(),
            data: data.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn mac_item_to_mime_and_back() {
        let item = vec![
            flavor("public.utf8-plain-text", b"hello"),
            flavor("public.html", b"<b>hello</b>"),
            flavor("com.apple.webarchive", b"x"),
            flavor("public.rtf", b"{\\rtf1 hello}"),
        ];
        let c = ClipContent::from_items(&[item]);
        assert_eq!(
            c.summary(),
            [
                (RTF.to_string(), 13),
                (HTML.to_string(), 12),
                (TEXT.to_string(), 5)
            ]
        );
        let back = c.to_items();
        let utis: Vec<_> = back[0].iter().map(|f| f.uti.as_str()).collect();
        assert_eq!(
            utis,
            ["public.rtf", "public.html", "public.utf8-plain-text"]
        );
        assert_eq!(back[0][2].tag("public.mime-type"), Some(TEXT));
    }

    #[test]
    fn utf16_fallback_and_dyn_by_tag() {
        let utf16: Vec<u8> = "héllo".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let c = ClipContent::from_items(&[vec![flavor("public.utf16-plain-text", &utf16)]]);
        assert_eq!(c.as_text().as_deref(), Some("héllo"));
        let mut png = flavor("", b"\x89PNG");
        png.tags.push(("public.mime-type".into(), PNG.into()));
        assert_eq!(
            ClipContent::from_items(&[vec![png]]).get(PNG),
            Some(&b"\x89PNG"[..])
        );
    }

    #[test]
    fn wayland_names() {
        assert_eq!(canonical("UTF8_STRING"), Some(TEXT));
        assert_eq!(canonical("text/plain;charset=UTF-8"), Some(TEXT));
        assert_eq!(canonical("application/rtf"), Some(RTF));
        assert_eq!(canonical("image/png"), Some(PNG));
        assert_eq!(canonical("text/uri-list"), None);
        assert_eq!(offered_names(TEXT), [TEXT, "UTF8_STRING", "text/plain"]);
        assert_eq!(mime_for_uti("public.html"), Some(HTML));
        assert!(ClipContent::default().to_items().is_empty());
    }
}
