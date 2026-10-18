//! SRTP/SRTCP with AES_256_CM_HMAC_SHA1_80 (RFC 3711, RFC 6188), kdr 0, no MKI.
//!
//! One context per master key, i.e. per sending direction: the server protects with the
//! server→viewer key and unprotects with the viewer→server key.

use std::collections::HashMap;

use aes::Aes256;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use anyhow::{Result, bail, ensure};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;

pub const MASTER_LEN: usize = 46;
pub const TAG_LEN: usize = 10;
const KEY_LEN: usize = 32;
const SALT_LEN: usize = 14;
const SRTCP_E: u32 = 0x8000_0000;

struct SessionKeys {
    cipher: Aes256,
    auth: Hmac<Sha1>,
    salt: [u8; SALT_LEN],
}

fn keystream_xor(cipher: &Aes256, iv: [u8; 16], data: &mut [u8]) {
    let mut ctr = u128::from_be_bytes(iv);
    // Independent counter blocks let the AES backend process several rounds in parallel.
    let (batches, tail) = data.as_chunks_mut::<128>();
    for chunk in batches {
        let mut blocks: [aes::Block; 8] = std::array::from_fn(|_| {
            let block = ctr.to_be_bytes().into();
            ctr = ctr.wrapping_add(1);
            block
        });
        cipher.encrypt_blocks(&mut blocks);
        for (dst, block) in chunk.as_chunks_mut::<16>().0.iter_mut().zip(blocks) {
            for (d, k) in dst.iter_mut().zip(block) {
                *d ^= k;
            }
        }
    }
    for chunk in tail.chunks_mut(16) {
        let mut block = ctr.to_be_bytes().into();
        cipher.encrypt_block(&mut block);
        chunk
            .iter_mut()
            .zip(block.iter())
            .for_each(|(d, k)| *d ^= k);
        ctr = ctr.wrapping_add(1);
    }
}

impl SessionKeys {
    fn derive(master: &[u8; MASTER_LEN], first_label: u8) -> Self {
        let master_cipher = Aes256::new(master[..KEY_LEN].try_into().unwrap());
        let prf = |label: u8, out: &mut [u8]| {
            let mut iv = [0u8; 16];
            iv[..SALT_LEN].copy_from_slice(&master[KEY_LEN..]);
            iv[7] ^= label;
            keystream_xor(&master_cipher, iv, out);
        };
        let (mut key, mut auth, mut salt) = ([0u8; KEY_LEN], [0u8; 20], [0u8; SALT_LEN]);
        prf(first_label, &mut key);
        prf(first_label + 1, &mut auth);
        prf(first_label + 2, &mut salt);
        SessionKeys {
            cipher: Aes256::new(&key.into()),
            auth: Hmac::new_from_slice(&auth).unwrap(),
            salt,
        }
    }

    fn iv(&self, ssrc: u32, index: u64) -> [u8; 16] {
        let mut iv = [0u8; 16];
        iv[..SALT_LEN].copy_from_slice(&self.salt);
        iv[4..8]
            .iter_mut()
            .zip(ssrc.to_be_bytes())
            .for_each(|(a, b)| *a ^= b);
        iv[8..14]
            .iter_mut()
            .zip(&index.to_be_bytes()[2..])
            .for_each(|(a, b)| *a ^= b);
        iv
    }

    fn tag(&self, parts: &[&[u8]]) -> [u8; TAG_LEN] {
        let mut mac = self.auth.clone();
        parts.iter().for_each(|p| mac.update(p));
        mac.finalize().into_bytes()[..TAG_LEN].try_into().unwrap()
    }
}

#[derive(Clone, Copy)]
struct RocState {
    roc: u32,
    last_seq: u16,
}

pub struct SrtpContext {
    rtp: SessionKeys,
    rtcp: SessionKeys,
    rtp_state: HashMap<u32, RocState>,
    srtcp_index: HashMap<u32, u32>,
}

/// True if a packet on the shared RTP/RTCP socket is RTCP (PT 192..=223, incl. legacy FIR 192).
pub fn is_rtcp(pkt: &[u8]) -> bool {
    pkt.len() >= 8 && (192..=223).contains(&pkt[1])
}

/// Length of the fixed RTP header plus CSRCs and header extension.
pub fn rtp_header_len(pkt: &[u8]) -> Result<usize> {
    ensure!(pkt.len() >= 12 && pkt[0] >> 6 == 2, "not an RTP packet");
    let mut n = 12 + 4 * usize::from(pkt[0] & 0x0f);
    if pkt[0] & 0x10 != 0 {
        ensure!(pkt.len() >= n + 4, "truncated RTP extension");
        n += 4 + 4 * usize::from(u16::from_be_bytes([pkt[n + 2], pkt[n + 3]]));
    }
    ensure!(pkt.len() >= n, "truncated RTP header");
    Ok(n)
}

impl SrtpContext {
    pub fn new(master: &[u8; MASTER_LEN]) -> Self {
        SrtpContext {
            rtp: SessionKeys::derive(master, 0),
            rtcp: SessionKeys::derive(master, 3),
            rtp_state: HashMap::new(),
            srtcp_index: HashMap::new(),
        }
    }

    pub fn protect_rtp(&mut self, pkt: &mut Vec<u8>) -> Result<()> {
        let hdr = rtp_header_len(pkt)?;
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        let ssrc = u32::from_be_bytes(pkt[8..12].try_into()?);
        let st = self.rtp_state.entry(ssrc).or_insert(RocState {
            roc: 0,
            last_seq: seq,
        });
        let roc = if seq.wrapping_sub(st.last_seq) < 0x8000 {
            if seq < st.last_seq {
                st.roc = st.roc.wrapping_add(1);
            }
            st.last_seq = seq;
            st.roc
        } else if seq > st.last_seq {
            st.roc.wrapping_sub(1)
        } else {
            st.roc
        };
        let index = u64::from(roc) << 16 | u64::from(seq);
        keystream_xor(&self.rtp.cipher, self.rtp.iv(ssrc, index), &mut pkt[hdr..]);
        let tag = self.rtp.tag(&[pkt, &roc.to_be_bytes()]);
        pkt.extend_from_slice(&tag);
        Ok(())
    }

    pub fn unprotect_rtp(&mut self, pkt: &mut Vec<u8>) -> Result<()> {
        ensure!(pkt.len() >= 12 + TAG_LEN, "SRTP packet too short");
        let body_end = pkt.len() - TAG_LEN;
        let hdr = rtp_header_len(&pkt[..body_end])?;
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        let ssrc = u32::from_be_bytes(pkt[8..12].try_into()?);
        let st = self.rtp_state.get(&ssrc).copied();
        let roc = match st {
            None => 0,
            Some(RocState { roc, last_seq }) if last_seq < 0x8000 => {
                if seq > last_seq && seq - last_seq > 0x8000 {
                    roc.wrapping_sub(1)
                } else {
                    roc
                }
            }
            Some(RocState { roc, last_seq }) => {
                if last_seq - 0x8000 > seq {
                    roc.wrapping_add(1)
                } else {
                    roc
                }
            }
        };
        let tag = self.rtp.tag(&[&pkt[..body_end], &roc.to_be_bytes()]);
        if !bool::from(tag.ct_eq(&pkt[body_end..])) {
            bail!("SRTP auth failed");
        }
        pkt.truncate(body_end);
        let index = u64::from(roc) << 16 | u64::from(seq);
        keystream_xor(&self.rtp.cipher, self.rtp.iv(ssrc, index), &mut pkt[hdr..]);
        let newer = match st {
            None => true,
            Some(s) => {
                (u64::from(roc) << 16 | u64::from(seq))
                    > (u64::from(s.roc) << 16 | u64::from(s.last_seq))
            }
        };
        if newer {
            self.rtp_state.insert(ssrc, RocState { roc, last_seq: seq });
        }
        Ok(())
    }

    pub fn protect_rtcp(&mut self, pkt: &mut Vec<u8>) -> Result<()> {
        ensure!(pkt.len() >= 8, "RTCP packet too short");
        let ssrc = u32::from_be_bytes(pkt[4..8].try_into()?);
        let idx = self.srtcp_index.entry(ssrc).or_insert(0);
        *idx = (*idx + 1) & 0x7fff_ffff;
        let index = *idx;
        keystream_xor(
            &self.rtcp.cipher,
            self.rtcp.iv(ssrc, index.into()),
            &mut pkt[8..],
        );
        pkt.extend_from_slice(&(SRTCP_E | index).to_be_bytes());
        let tag = self.rtcp.tag(&[pkt]);
        pkt.extend_from_slice(&tag);
        Ok(())
    }

    pub fn unprotect_rtcp(&mut self, pkt: &mut Vec<u8>) -> Result<()> {
        ensure!(pkt.len() >= 8 + 4 + TAG_LEN, "SRTCP packet too short");
        let body_end = pkt.len() - TAG_LEN;
        let tag = self.rtcp.tag(&[&pkt[..body_end]]);
        if !bool::from(tag.ct_eq(&pkt[body_end..])) {
            bail!("SRTCP auth failed");
        }
        let e_index = u32::from_be_bytes(pkt[body_end - 4..body_end].try_into()?);
        pkt.truncate(body_end - 4);
        if e_index & SRTCP_E != 0 {
            let ssrc = u32::from_be_bytes(pkt[4..8].try_into()?);
            let iv = self.rtcp.iv(ssrc, (e_index & !SRTCP_E).into());
            keystream_xor(&self.rtcp.cipher, iv, &mut pkt[8..]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn master() -> [u8; MASTER_LEN] {
        std::array::from_fn(|i| i as u8)
    }

    fn rtp(seq: u16) -> Vec<u8> {
        let mut p = vec![
            0x90,
            100,
            (seq >> 8) as u8,
            seq as u8,
            0,
            0,
            1,
            0,
            0xde,
            0xad,
            0xbe,
            0xef,
        ];
        p.extend_from_slice(&[0x93, 0x11, 0, 1, 0, 3, 0x20, 0]);
        p.extend((0..50u8).map(|i| i.wrapping_mul(7)));
        p
    }

    #[test]
    fn batched_keystream_matches_scalar_at_boundaries() {
        let cipher = Aes256::new(&[0x47; 32].into());
        for counter in [0, 0xffff, u128::MAX - 3] {
            for len in [0, 1, 15, 16, 17, 127, 128, 129, 255, 256, 257, 1300, 4096] {
                let mut expected: Vec<u8> = (0..len).map(|i| i as u8).collect();
                let mut actual = expected.clone();
                let mut ctr = counter;
                for chunk in expected.chunks_mut(16) {
                    let mut block = ctr.to_be_bytes().into();
                    cipher.encrypt_block(&mut block);
                    for (d, k) in chunk.iter_mut().zip(block) {
                        *d ^= k;
                    }
                    ctr = ctr.wrapping_add(1);
                }
                keystream_xor(&cipher, counter.to_be_bytes(), &mut actual);
                assert_eq!(actual, expected, "counter {counter}, length {len}");
            }
        }
    }

    #[test]
    fn rfc6188_keystream() {
        let key = hex("57f82fe3613fd170a85ec93c40b1f0922ec4cb0dc025b58272147cc438944a98");
        let cipher = Aes256::new(key.as_slice().try_into().unwrap());
        let mut ks = [0u8; 32];
        keystream_xor(
            &cipher,
            hex("f0f1f2f3f4f5f6f7f8f9fafbfcfd0000").try_into().unwrap(),
            &mut ks,
        );
        assert_eq!(
            ks.to_vec(),
            hex("92bdd28a93c3f52511c677d08b5515a49da71b2378a854f67050756ded165bac")
        );
    }

    #[test]
    fn rtp_roundtrip_and_tamper() {
        let (mut tx, mut rx) = (SrtpContext::new(&master()), SrtpContext::new(&master()));
        for seq in [65530u16, 65535, 0, 1, 2] {
            let plain = rtp(seq);
            let mut p = plain.clone();
            tx.protect_rtp(&mut p).unwrap();
            assert_eq!(p.len(), plain.len() + TAG_LEN);
            assert_eq!(p[..20], plain[..20], "header and extension stay clear");
            assert_ne!(p[20..plain.len()], plain[20..]);
            rx.unprotect_rtp(&mut p).unwrap();
            assert_eq!(p, plain);
        }
        assert_eq!(tx.rtp_state[&0xdeadbeef].roc, 1);
        let mut p = rtp(5);
        tx.protect_rtp(&mut p).unwrap();
        p[30] ^= 1;
        assert!(rx.unprotect_rtp(&mut p).is_err());
    }

    /// Generated with libsrtp 2.8.1, `srtp_crypto_policy_set_aes_cm_256_hmac_sha1_80`, same master key.
    #[test]
    fn matches_libsrtp() {
        let mut tx = SrtpContext::new(&master());
        let expect = [
            (
                65530,
                "9064fffa00000100deadbeef9311000100032000cf8a2d5454bab17b9aec6f05d7cc5de53f095d46301b1e4414878ab1f5e2313a2252785629a2bd19deb697cef5de64146b45944943377ebc585e8b78",
            ),
            (
                65535,
                "9064ffff00000100deadbeef93110001000320009f5f6ced9f022eecbd8149c1945ddddad21fb34a628eaf66ebfd6861ad8434ed59d84b1947efc51d01a495ca1a5d3fb923f3af5c50fecad6f031f023",
            ),
            (
                0,
                "9064000000000100deadbeef931100010003200091588910dbb5667d38580414039e583941d6fb203e7503e48d4e57c26135cc11ee8b20647cf3155785b9b9fb29432cc0bbfb377a9b8490cdb3fd871b",
            ),
            (
                1,
                "9064000100000100deadbeef931100010003200065cb5340b694ae7439d5dbb0ae1cfbc766ff74173fdaba327c71f0b1547dc0ce44a23b073583a3c3c7564fd2c46bbebf7406917051766cccdf12b907",
            ),
        ];
        for (seq, want) in expect {
            let mut p = rtp(seq);
            tx.protect_rtp(&mut p).unwrap();
            assert_eq!(p, hex(want), "seq {seq}");
        }
        for want in [
            "80c90001deadbeefcffdfa8fb459dd048000000118bc6a424110a7b4629f",
            "80c90001deadbeef201c0654264b003d80000002fab937dd2da2f9f78408",
        ] {
            let mut p = hex("80c90001deadbeef81ca0001deadbeef");
            tx.protect_rtcp(&mut p).unwrap();
            assert_eq!(p, hex(want));
        }
    }

    #[test]
    fn rtcp_roundtrip() {
        let (mut tx, mut rx) = (SrtpContext::new(&master()), SrtpContext::new(&master()));
        let plain = hex("80c90001deadbeef81ca0001deadbeef");
        for i in 0..3u32 {
            let mut p = plain.clone();
            tx.protect_rtcp(&mut p).unwrap();
            assert_eq!(
                p[plain.len()..plain.len() + 4],
                (SRTCP_E | (i + 1)).to_be_bytes()
            );
            rx.unprotect_rtcp(&mut p).unwrap();
            assert_eq!(p, plain);
        }
    }
}
