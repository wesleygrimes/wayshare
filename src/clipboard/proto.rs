//! Apple clipboard RFB messages: client 11/21/31, server 31 and MiscStateChange 2/3.

use anyhow::{Result, ensure};

use super::mime::{ClipContent, mime_for_uti};
use super::pack::{compress, decompress, pack, unpack};

/// Apple's client limit for both directions (`HandleServerClipboardData`).
pub const MAX_UNCOMPRESSED: usize = 0x63f_ffff;
/// The client refuses to send more than this (compressed) from its side.
pub const CLIENT_SEND_LIMIT: usize = 7 << 20;

pub const STATE_PASTEBOARD_UPDATED: u16 = 2;
pub const STATE_PASTEBOARD_NEEDS_DATA: u16 = 3;

pub const AUTO_PASTEBOARD_ON: u16 = 1;
pub const AUTO_PASTEBOARD_OFF: u16 = 2;

/// Client message types the server must advertise in the ServerInit cmds bitmap.
pub const CLIENT_MSGS: [u8; 4] = [6, 11, 21, 31];

#[derive(Debug, PartialEq, Eq)]
pub enum ViewerPasteboard {
    /// Full data, already mapped to MIME types.
    Data(ClipContent),
    /// Promises: the types the viewer can supply. Ask for the data with MiscStateChange 3.
    Promises(Vec<&'static str>),
}

/// Decodes client msg 31 (as parsed by `rfb::messages`: flags byte is msg[2]).
pub fn decode_viewer_send(
    flags: u8,
    uncompressed_len: u32,
    data: &[u8],
) -> Result<ViewerPasteboard> {
    let len = uncompressed_len as usize;
    ensure!(
        len <= MAX_UNCOMPRESSED,
        "pasteboard of {len} bytes too large"
    );
    let items = unpack(&decompress(data, len)?)?;
    Ok(if flags & 1 != 0 {
        let mut types: Vec<_> = items
            .iter()
            .flatten()
            .filter_map(|f| mime_for_uti(&f.uti))
            .collect();
        types.dedup();
        ViewerPasteboard::Promises(types)
    } else {
        ViewerPasteboard::Data(ClipContent::from_items(&items))
    })
}

/// Server msg 31 answering client msg 11 (`session` echoed from the request; 0 is only accepted
/// by the client for auto transfers after it sent AutoPasteboard cmd 1). Always full data.
pub fn server_clipboard_data(session: u32, content: &ClipContent) -> Vec<u8> {
    let packed = pack(&content.to_items());
    let z = compress(&packed);
    let mut out = Vec::with_capacity(16 + z.len());
    out.extend_from_slice(&[31, 0, 0, 0]);
    out.extend_from_slice(&session.to_be_bytes());
    out.extend_from_slice(&(packed.len() as u32).to_be_bytes());
    out.extend_from_slice(&(z.len() as u32).to_be_bytes());
    out.extend_from_slice(&z);
    out
}

/// Server msg 20 MiscStateChange.
pub fn misc_state_change(state: u16) -> Vec<u8> {
    let mut out = vec![20, 0, 0, 4, 0, 1];
    out.extend_from_slice(&state.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::super::mime::{HTML, TEXT};
    use super::super::pack::Flavor;
    use super::*;

    #[test]
    fn server_message_layout() {
        let m = server_clipboard_data(0x01020304, &ClipContent::text("hi"));
        assert_eq!(&m[..8], &[31, 0, 0, 0, 1, 2, 3, 4]);
        let ulen = u32::from_be_bytes(m[8..12].try_into().unwrap()) as usize;
        let zlen = u32::from_be_bytes(m[12..16].try_into().unwrap()) as usize;
        assert_eq!(m.len(), 16 + zlen);
        let back = decode_viewer_send(0, ulen as u32, &m[16..]).unwrap();
        assert_eq!(back, ViewerPasteboard::Data(ClipContent::text("hi")));
        assert_eq!(
            misc_state_change(STATE_PASTEBOARD_UPDATED),
            [20, 0, 0, 4, 0, 1, 0, 2]
        );
    }

    /// Cross-check with Apple's code: run `tools-harness/clipcheck pack /tmp/wayshare-clip` first,
    /// then this, then `clipcheck unpack /tmp/wayshare-clip/server31.bin`.
    #[test]
    #[ignore]
    fn apple_vectors() {
        let dir = std::path::Path::new("/tmp/wayshare-clip");
        let raw = std::fs::read(dir.join("apple-packed.bin")).unwrap();
        let items = unpack(&raw).unwrap();
        assert_eq!(pack(&items), raw);
        let z = std::fs::read(dir.join("apple-compressed.bin")).unwrap();
        let ulen: u32 = std::fs::read_to_string(dir.join("apple-compressed.len"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let ViewerPasteboard::Data(c) = decode_viewer_send(0, ulen, &z).unwrap() else {
            panic!()
        };
        assert_eq!(c.as_text().as_deref(), Some("hello wayshare"));
        assert_eq!(c.get(HTML), Some(&b"<b>hello</b>"[..]));
        let content = ClipContent(vec![
            (
                super::super::mime::RTF.into(),
                b"{\\rtf1 rtf wayshare}".to_vec(),
            ),
            (HTML.into(), b"<i>html wayshare</i>".to_vec()),
            (TEXT.into(), "text wayshare \u{2603}".as_bytes().to_vec()),
        ]);
        std::fs::write(dir.join("server31.bin"), server_clipboard_data(9, &content)).unwrap();
    }

    #[test]
    fn viewer_promises() {
        let item = vec![
            Flavor {
                uti: "public.html".into(),
                ..Default::default()
            },
            Flavor {
                uti: "public.utf8-plain-text".into(),
                ..Default::default()
            },
            Flavor {
                uti: "com.apple.flat-rtfd".into(),
                ..Default::default()
            },
        ];
        let packed = pack(&[item]);
        let z = compress(&packed);
        assert_eq!(
            decode_viewer_send(1, packed.len() as u32, &z).unwrap(),
            ViewerPasteboard::Promises(vec![HTML, TEXT])
        );
        assert!(decode_viewer_send(0, (MAX_UNCOMPRESSED + 1) as u32, &z).is_err());
    }

    #[test]
    fn viewer_send_rejects_bad_payloads() {
        let packed = pack(&[vec![Flavor::default()]]);
        let z = compress(&packed);
        let len = packed.len() as u32;
        assert!(decode_viewer_send(0, len, &z).is_ok());
        assert!(decode_viewer_send(0, len + 1, &z).is_err());
        assert!(decode_viewer_send(0, len - 1, &z).is_err());
        assert!(decode_viewer_send(0, u32::MAX, &z).is_err());
        assert!(decode_viewer_send(0, len, b"not zlib at all").is_err());
        assert!(decode_viewer_send(0, len, &[]).is_err());
        assert_eq!(
            decode_viewer_send(1, 0, &[]).unwrap(),
            ViewerPasteboard::Promises(vec![])
        );
    }
}
