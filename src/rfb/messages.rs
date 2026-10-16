use anyhow::{Result, bail, ensure};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::pixel::PixelFormat;

const MAX_CUT_TEXT: usize = 16 << 20;
const MAX_SKIP: u64 = 100 << 20;

/// Client message 33.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerInfo {
    pub version: u16,
    pub app_id: u32,
    pub app_version: [u32; 3],
    pub os_version: [u32; 3],
    pub features: [u8; 32],
}

impl ViewerInfo {
    pub fn feature_bits(&self) -> Vec<u8> {
        (0..=255u8)
            .filter(|&i| self.features[i as usize / 8] & (0x80 >> (i % 8)) != 0)
            .collect()
    }
}

#[derive(Debug)]
pub enum ClientMessage {
    SetPixelFormat(PixelFormat),
    SetEncodings(Vec<i32>),
    FramebufferUpdateRequest {
        incremental: bool,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
    },
    KeyEvent {
        down: bool,
        keysym: u32,
    },
    PointerEvent {
        buttons: u8,
        x: u16,
        y: u16,
    },
    ClientCutText(Vec<u8>),
    EncryptedEvent {
        flags: u8,
        block: [u8; 16],
    },
    EventMessage2 {
        hdr_flags: u8,
        body: Vec<u8>,
    },
    AutoFramebufferUpdate {
        value: u32,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
    },
    SetEncryption {
        cmd: u16,
        param: u16,
        methods: Vec<u32>,
    },
    MediaStreamConfig(Vec<u8>),
    SetDisplayConfiguration(Vec<u8>),
    /// Msg 26 payload after the u16 size field.
    SetKeyboardInputSource(Vec<u8>),
    ViewerInfo(ViewerInfo),
    ClipboardRequest {
        flags: u8,
        opaque: u32,
    },
    AutoPasteboard {
        cmd: u16,
    },
    ClipboardSend {
        flags: u8,
        session: u32,
        uncompressed_len: u32,
        data: Vec<u8>,
    },
    Ignored {
        kind: u8,
        len: u64,
    },
}

pub async fn read<R: AsyncRead + Unpin>(r: &mut R) -> Result<ClientMessage> {
    let kind = r.read_u8().await?;
    Ok(match kind {
        0 => {
            let mut b = [0u8; 19];
            r.read_exact(&mut b).await?;
            ClientMessage::SetPixelFormat(PixelFormat::from_bytes(b[3..].try_into().unwrap()))
        }
        2 => {
            r.read_u8().await?;
            let n = r.read_u16().await? as usize;
            ensure!(n <= 100, "SetEncodings count {n} > 100");
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.read_i32().await?);
            }
            ClientMessage::SetEncodings(v)
        }
        3 => {
            let incremental = r.read_u8().await? != 0;
            let (x, y, w, h) = (
                r.read_u16().await?,
                r.read_u16().await?,
                r.read_u16().await?,
                r.read_u16().await?,
            );
            ClientMessage::FramebufferUpdateRequest {
                incremental,
                x,
                y,
                w,
                h,
            }
        }
        4 => {
            let down = r.read_u8().await? != 0;
            r.read_u16().await?;
            ClientMessage::KeyEvent {
                down,
                keysym: r.read_u32().await?,
            }
        }
        5 => {
            let buttons = r.read_u8().await?;
            let (x, y) = (r.read_u16().await?, r.read_u16().await?);
            ClientMessage::PointerEvent { buttons, x, y }
        }
        6 => {
            skip(r, 3).await?;
            let len = r.read_u32().await? as usize;
            ensure!(len <= MAX_CUT_TEXT, "ClientCutText length {len} too large");
            let mut text = vec![0u8; len];
            r.read_exact(&mut text).await?;
            ClientMessage::ClientCutText(text)
        }
        9 => {
            skip(r, 3).await?;
            let value = r.read_u32().await?;
            let (x, y, w, h) = (
                r.read_u16().await?,
                r.read_u16().await?,
                r.read_u16().await?,
                r.read_u16().await?,
            );
            ClientMessage::AutoFramebufferUpdate { value, x, y, w, h }
        }
        16 => {
            let flags = r.read_u8().await?;
            let mut block = [0u8; 16];
            r.read_exact(&mut block).await?;
            ClientMessage::EncryptedEvent { flags, block }
        }
        23 => {
            let hdr_flags = r.read_u8().await?;
            let len = r.read_u16().await? as usize;
            ensure!(len <= 50_000, "EventMessage2 body length {len} too large");
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).await?;
            ClientMessage::EventMessage2 { hdr_flags, body }
        }
        11 => {
            let flags = r.read_u8().await?;
            r.read_u16().await?;
            ClientMessage::ClipboardRequest {
                flags,
                opaque: r.read_u32().await?,
            }
        }
        18 => {
            r.read_u8().await?;
            let (cmd, param, count) = (
                r.read_u16().await?,
                r.read_u16().await?,
                r.read_u16().await?,
            );
            ensure!(count <= 100, "SetEncryption count {count} > 100");
            let mut methods = Vec::with_capacity(count as usize);
            for _ in 0..count {
                methods.push(r.read_u32().await?);
            }
            ClientMessage::SetEncryption {
                cmd,
                param,
                methods,
            }
        }
        21 => {
            r.read_u8().await?;
            let cmd = r.read_u16().await?;
            r.read_u32().await?;
            ClientMessage::AutoPasteboard { cmd }
        }
        28 => {
            let pad = r.read_u8().await?;
            let size = r.read_u16().await?;
            let mut msg = vec![28, pad];
            msg.extend_from_slice(&size.to_be_bytes());
            msg.resize(4 + size as usize, 0);
            r.read_exact(&mut msg[4..]).await?;
            ClientMessage::MediaStreamConfig(msg)
        }
        26 => {
            r.read_u8().await?;
            let size = r.read_u16().await? as usize;
            let mut payload = vec![0u8; size];
            r.read_exact(&mut payload).await?;
            ClientMessage::SetKeyboardInputSource(payload)
        }
        29 => {
            let pad = r.read_u8().await?;
            let size = r.read_u16().await?;
            ensure!(size > 0xbf, "SetDisplayConfiguration size {size} too small");
            let mut msg = vec![29, pad];
            msg.extend_from_slice(&size.to_be_bytes());
            msg.resize(4 + size as usize, 0);
            r.read_exact(&mut msg[4..]).await?;
            ClientMessage::SetDisplayConfiguration(msg)
        }
        31 => {
            let mut h = [0u8; 15];
            r.read_exact(&mut h).await?;
            let uncompressed_len = u32::from_be_bytes(h[7..11].try_into().unwrap());
            let len = u32::from_be_bytes(h[11..15].try_into().unwrap()) as usize;
            ensure!(len <= MAX_CUT_TEXT, "ClipboardSend length {len} too large");
            let mut data = vec![0u8; len];
            r.read_exact(&mut data).await?;
            ClientMessage::ClipboardSend {
                flags: h[1],
                session: u32::from_be_bytes(h[3..7].try_into().unwrap()),
                uncompressed_len,
                data,
            }
        }
        33 => {
            r.read_u8().await?;
            let size = r.read_u16().await? as usize;
            ensure!(size + 4 >= 0x42, "ViewerInfo size {size} too small");
            let mut b = vec![0u8; size];
            r.read_exact(&mut b).await?;
            let be32 = |o: usize| u32::from_be_bytes(b[o - 4..o].try_into().unwrap());
            ClientMessage::ViewerInfo(ViewerInfo {
                version: u16::from_be_bytes([b[0], b[1]]),
                app_id: be32(6),
                app_version: std::array::from_fn(|i| be32(0x0a + 4 * i)),
                os_version: std::array::from_fn(|i| be32(0x16 + 4 * i)),
                features: b[0x22 - 4..0x42 - 4].try_into().unwrap(),
            })
        }
        100 => ClientMessage::Ignored { kind, len: 1 },
        _ => {
            let len = ignored_len(kind, r).await?;
            ClientMessage::Ignored { kind, len }
        }
    })
}

/// Consumes the rest of a message we parse only to stay in sync; returns its total length.
async fn ignored_len<R: AsyncRead + Unpin>(kind: u8, r: &mut R) -> Result<u64> {
    let total = match kind {
        7 | 10 => fixed(r, 4).await?,
        8 => fixed(r, 10).await?,
        13 | 14 => fixed(r, 8).await?,
        12 => {
            let mut h = [0u8; 5];
            r.read_exact(&mut h).await?;
            let n = u16::from_be_bytes([h[3], h[4]]) as u64;
            skip(r, n).await?;
            6 + n
        }
        22 | 25 => {
            r.read_u8().await?;
            let size = r.read_u16().await? as u64;
            skip(r, size).await?;
            4 + size
        }
        32 => {
            let mut h = [0u8; 15];
            r.read_exact(&mut h).await?;
            let n = u32::from_be_bytes(h[11..15].try_into().unwrap()) as u64;
            skip(r, n).await?;
            16 + n
        }
        34 => {
            r.read_u8().await?;
            let n = r.read_u32().await? as u64;
            skip(r, n).await?;
            6 + n
        }
        36 => {
            let mut h = [0u8; 10];
            r.read_exact(&mut h).await?;
            let n = u16::from_le_bytes([h[8], h[9]]) as u64;
            skip(r, n).await?;
            11 + n
        }
        37 => {
            let mut h = [0u8; 16];
            r.read_exact(&mut h).await?;
            let n = u16::from_le_bytes([h[14], h[15]]) as u64;
            skip(r, n).await?;
            17 + n
        }
        _ => bail!("unknown client message type {kind} ({kind:#04x})"),
    };
    Ok(total)
}

async fn fixed<R: AsyncRead + Unpin>(r: &mut R, total: u64) -> Result<u64> {
    skip(r, total - 1).await?;
    Ok(total)
}

async fn skip<R: AsyncRead + Unpin>(r: &mut R, n: u64) -> Result<()> {
    ensure!(n <= MAX_SKIP, "refusing to skip {n} bytes");
    let copied = tokio::io::copy(&mut r.take(n), &mut tokio::io::sink()).await?;
    ensure!(copied == n, "connection closed mid-message");
    Ok(())
}

/// Decoded plaintext of the 16-byte encrypted key/mouse block (client msgs 16 and 23/12).
#[derive(Debug)]
pub struct EventBlock {
    pub key: Option<(bool, u32)>,
    pub mouse: Option<(u8, i16, i16)>,
}

impl EventBlock {
    pub fn parse(b: &[u8; 16]) -> Result<Self> {
        ensure!(
            matches!(b[0], 0 | 0xff) && matches!(b[10], 0 | 0xff),
            "decryption error: {b:02x?}"
        );
        let key =
            (b[0] == 0xff).then(|| (b[1] != 0, u32::from_be_bytes(b[2..6].try_into().unwrap())));
        let mouse = (b[10] == 0xff).then(|| {
            (
                b[11],
                i16::from_be_bytes([b[12], b[13]]),
                i16::from_be_bytes([b[14], b[15]]),
            )
        });
        Ok(Self { key, mouse })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn parse(bytes: &[u8]) -> (ClientMessage, usize) {
        let mut r = bytes;
        let m = read(&mut r).await.unwrap();
        (m, bytes.len() - r.len())
    }

    #[tokio::test]
    async fn standard_messages() {
        let (m, n) = parse(&[2, 0, 0, 2, 0, 0, 0, 6, 0xff, 0xff, 0xff, 0x21, 9]).await;
        assert!(matches!(m, ClientMessage::SetEncodings(ref v) if v == &[6, -223]));
        assert_eq!(n, 12);
        let (m, n) = parse(&[5, 1, 0, 10, 0, 20]).await;
        assert!(matches!(
            m,
            ClientMessage::PointerEvent {
                buttons: 1,
                x: 10,
                y: 20
            }
        ));
        assert_eq!(n, 6);
        let (m, _) = parse(&[4, 1, 0, 0, 0, 0, 0xff, 0x0d]).await;
        assert!(matches!(
            m,
            ClientMessage::KeyEvent {
                down: true,
                keysym: 0xff0d
            }
        ));
    }

    #[tokio::test]
    async fn ignored_messages_consume_exact_length() {
        let (m, n) = parse(&[25, 0, 0, 3, 1, 2, 3, 0xaa]).await;
        assert!(matches!(m, ClientMessage::Ignored { kind: 25, len: 7 }));
        assert_eq!(n, 7);
        let (_, n) = parse(&[13, 0, 0, 0, 0, 0, 0, 1, 0xaa]).await;
        assert_eq!(n, 8);
    }

    #[tokio::test]
    async fn set_encryption() {
        let (m, n) = parse(&[18, 0, 0, 2, 0, 1, 0, 0, 0xaa]).await;
        assert!(
            matches!(m, ClientMessage::SetEncryption { cmd: 2, param: 1, ref methods } if methods.is_empty())
        );
        assert_eq!(n, 8);
        let (m, n) = parse(&[18, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1]).await;
        assert!(
            matches!(m, ClientMessage::SetEncryption { cmd: 1, ref methods, .. } if methods == &[1])
        );
        assert_eq!(n, 12);
    }

    #[tokio::test]
    async fn viewer_info() {
        let mut msg = vec![33, 0, 0, 0x3e, 0, 1, 0, 0, 0, 7];
        for w in 1..=6u32 {
            msg.extend_from_slice(&w.to_be_bytes());
        }
        let mut features = [0u8; 32];
        features[0] = 0x80;
        features[31] = 0x01;
        msg.extend_from_slice(&features);
        assert_eq!(msg.len(), 0x42);
        msg.push(0xaa);
        let (m, n) = parse(&msg).await;
        assert_eq!(n, 0x42);
        let ClientMessage::ViewerInfo(v) = m else {
            panic!()
        };
        assert_eq!((v.version, v.app_id), (1, 7));
        assert_eq!((v.app_version, v.os_version), ([1, 2, 3], [4, 5, 6]));
        assert_eq!(v.feature_bits(), [0, 255]);
    }

    #[tokio::test]
    async fn clipboard_messages() {
        let (m, n) = parse(&[11, 1, 0, 0, 0, 0, 0, 9]).await;
        assert!(matches!(
            m,
            ClientMessage::ClipboardRequest {
                flags: 1,
                opaque: 9
            }
        ));
        assert_eq!(n, 8);
        let (m, _) = parse(&[21, 0, 0, 2, 0, 0, 0, 0]).await;
        assert!(matches!(m, ClientMessage::AutoPasteboard { cmd: 2 }));
        let (m, n) = parse(&[31, 0, 1, 0, 0, 0, 0, 5, 0, 0, 0, 3, 0, 0, 0, 2, 7, 8, 0xaa]).await;
        assert!(
            matches!(m, ClientMessage::ClipboardSend { flags: 1, session: 5, uncompressed_len: 3, ref data } if data == &[7, 8])
        );
        assert_eq!(n, 18);
    }

    #[tokio::test]
    async fn keyboard_input_source() {
        let (m, n) = parse(&[26, 0, 0, 6, 0, 1, 0, 2, b'u', b's', 0xaa]).await;
        let ClientMessage::SetKeyboardInputSource(p) = m else {
            panic!()
        };
        assert_eq!(n, 10);
        assert_eq!(crate::input::parse_input_source(&p).as_deref(), Some("us"));
    }

    #[tokio::test]
    async fn set_display_configuration() {
        let mut msg = vec![29, 0, 0, 0xc0];
        msg.extend((0..0xc0u32).map(|i| i as u8));
        msg.push(0xaa);
        let (m, n) = parse(&msg).await;
        assert!(matches!(m, ClientMessage::SetDisplayConfiguration(ref b) if b[..] == msg[..0xc4]));
        assert_eq!(n, 0xc4);
        let mut r: &[u8] = &[29, 0, 0, 0xbf];
        assert!(read(&mut r).await.is_err());
    }

    #[tokio::test]
    async fn media_stream_config_is_read_whole() {
        let (m, n) = parse(&[28, 0, 0, 2, 0, 3, 0xaa]).await;
        assert!(matches!(m, ClientMessage::MediaStreamConfig(ref b) if b == &[28, 0, 0, 2, 0, 3]));
        assert_eq!(n, 6);
    }

    async fn read_err(bytes: &[u8]) -> String {
        let mut r = bytes;
        read(&mut r).await.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn oversized_lengths_are_rejected() {
        let cut_text = [6, 0, 0, 0, 0x01, 0x00, 0x00, 0x01];
        assert!(read_err(&cut_text).await.contains("too large"));
        let event2 = [23, 0, 0xc3, 0x51];
        assert!(read_err(&event2).await.contains("too large"));
        let mut clip = vec![31];
        clip.extend_from_slice(&[0; 11]);
        clip.extend_from_slice(&[0x01, 0x00, 0x00, 0x01]);
        assert!(read_err(&clip).await.contains("too large"));
        let skip = [34, 0, 0x06, 0x40, 0x00, 0x01];
        assert!(read_err(&skip).await.contains("refusing to skip"));
    }

    #[tokio::test]
    async fn truncated_variable_messages_error() {
        let mut viewer_info = vec![33, 0, 0, 0x3e];
        viewer_info.extend_from_slice(&[0; 0x3e]);
        let mut display = vec![29, 0, 0, 0xc0];
        display.extend_from_slice(&[0; 0xc0]);
        let mut clip_send = vec![31, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 3, 0, 0, 0, 2];
        clip_send.extend_from_slice(&[7, 8]);
        let messages: [&[u8]; 11] = [
            &[2, 0, 0, 2, 0, 0, 0, 6, 0xff, 0xff, 0xff, 0x21],
            &[6, 0, 0, 0, 0, 0, 0, 2, b'h', b'i'],
            &[18, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1],
            &[23, 0, 0, 2, 1, 2],
            &[26, 0, 0, 2, 1, 2],
            &[28, 0, 0, 2, 0, 3],
            &[25, 0, 0, 3, 1, 2, 3],
            &[12, 0, 0, 0, 0, 2, 1, 2],
            &viewer_info,
            &display,
            &clip_send,
        ];
        for msg in messages {
            for cut in 1..msg.len() {
                let mut r = &msg[..cut];
                let res = tokio::time::timeout(Duration::from_secs(5), read(&mut r)).await;
                assert!(
                    res.expect("read hung").is_err(),
                    "kind {} cut at {cut} did not error",
                    msg[0]
                );
            }
        }
    }

    #[tokio::test]
    async fn unknown_type_errors() {
        let mut r: &[u8] = &[1, 0, 0];
        assert!(read(&mut r).await.is_err());
    }
}
