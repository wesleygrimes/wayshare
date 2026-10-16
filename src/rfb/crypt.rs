use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use anyhow::{Result, ensure};
use sha1::{Digest, Sha1};

use super::dh::ecb_encrypt;

pub const ENC_ENCRYPTION: i32 = 1103;
pub const MAX_CHUNK: usize = 0x8000;
const MAC_LEN: usize = 20;

pub fn record_len(data_len: usize) -> usize {
    (data_len + 2 + MAC_LEN + 15) & !15
}

fn mac(seq: u32, plain: &[u8]) -> [u8; MAC_LEN] {
    let mut h = Sha1::new();
    h.update(seq.to_be_bytes());
    h.update(plain);
    h.finalize().into()
}

/// Outbound AES-128-CBC record stream. CBC state carries across records.
pub struct RecordWriter {
    cipher: Aes128,
    chain: [u8; 16],
    seq: u32,
}

impl RecordWriter {
    pub fn new(key: &[u8; 16], iv: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(key.into()),
            chain: *iv,
            seq: 0,
        }
    }

    pub fn seal(&mut self, data: &[u8], out: &mut Vec<u8>) {
        for chunk in data.chunks(MAX_CHUNK) {
            let t = record_len(chunk.len());
            let mut plain = vec![0u8; t];
            plain[..2].copy_from_slice(&(chunk.len() as u16).to_be_bytes());
            plain[2..2 + chunk.len()].copy_from_slice(chunk);
            let m = mac(self.seq, &plain[..t - MAC_LEN]);
            plain[t - MAC_LEN..].copy_from_slice(&m);
            self.seq = self.seq.wrapping_add(1);

            for block in plain.as_chunks_mut::<16>().0 {
                for (b, c) in block.iter_mut().zip(self.chain) {
                    *b ^= c;
                }
                self.cipher.encrypt_block(block.into());
                self.chain.copy_from_slice(block);
            }
            out.extend_from_slice(&(t as u16).to_be_bytes());
            out.extend_from_slice(&plain);
        }
    }
}

/// Inbound counterpart: feed it the T ciphertext bytes that follow each BE16 T.
pub struct RecordReader {
    cipher: Aes128,
    chain: [u8; 16],
    seq: u32,
}

impl RecordReader {
    pub fn new(key: &[u8; 16], iv: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(key.into()),
            chain: *iv,
            seq: 0,
        }
    }

    pub fn open(&mut self, record: &[u8]) -> Result<Vec<u8>> {
        let t = record.len();
        ensure!(
            t >= 32 && t.is_multiple_of(16),
            "bad encrypted record length {t}"
        );
        let mut plain = record.to_vec();
        for block in plain.as_chunks_mut::<16>().0 {
            let ct: [u8; 16] = *block;
            self.cipher.decrypt_block(block.into());
            for (b, c) in block.iter_mut().zip(self.chain) {
                *b ^= c;
            }
            self.chain = ct;
        }
        let expected = mac(self.seq, &plain[..t - MAC_LEN]);
        ensure!(
            plain[t - MAC_LEN..] == expected,
            "record MAC mismatch (seq {})",
            self.seq
        );
        self.seq = self.seq.wrapping_add(1);
        let len = u16::from_be_bytes([plain[0], plain[1]]) as usize;
        ensure!(
            len + 2 + MAC_LEN <= t,
            "record length {len} exceeds record size {t}"
        );
        Ok(plain[2..2 + len].to_vec())
    }
}

/// FramebufferUpdate carrying encoding 1103: u32 version 1, then the stream key and IV, each
/// AES-128-ECB encrypted with the auth session key.
pub fn encryption_update(auth_key: &[u8; 16], stream_key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let mut wrapped = [0u8; 32];
    wrapped[..16].copy_from_slice(stream_key);
    wrapped[16..].copy_from_slice(iv);
    ecb_encrypt(auth_key, &mut wrapped);
    let mut out = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&ENC_ENCRYPTION.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&wrapped);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rfb::dh::ecb_decrypt;

    #[test]
    fn record_lengths() {
        assert_eq!(record_len(0), 32);
        assert_eq!(record_len(10), 32);
        assert_eq!(record_len(11), 48);
        assert_eq!(record_len(MAX_CHUNK), 0x8020);
    }

    #[test]
    fn round_trip_with_chaining_and_seq() {
        let (key, iv) = ([7u8; 16], [9u8; 16]);
        let mut w = RecordWriter::new(&key, &iv);
        let mut r = RecordReader::new(&key, &iv);
        let big: Vec<u8> = (0..MAX_CHUNK + 100).map(|i| i as u8).collect();
        let mut wire = Vec::new();
        for msg in [&b"hello"[..], &[], &big] {
            w.seal(msg, &mut wire);
        }
        let mut got = Vec::new();
        let mut rest = &wire[..];
        while !rest.is_empty() {
            let t = u16::from_be_bytes([rest[0], rest[1]]) as usize;
            got.extend(r.open(&rest[2..2 + t]).unwrap());
            rest = &rest[2 + t..];
        }
        let mut expect = b"hello".to_vec();
        expect.extend(&big);
        assert_eq!(got, expect);
        assert_eq!(r.seq, 3);
    }

    #[test]
    fn record_layout_and_mac() {
        let (key, iv) = ([1u8; 16], [2u8; 16]);
        let mut wire = Vec::new();
        RecordWriter::new(&key, &iv).seal(b"abc", &mut wire);
        assert_eq!(&wire[..2], &[0, 32]);
        let mut plain = wire[2..].to_vec();
        let cipher = Aes128::new((&key).into());
        let mut chain = iv;
        for block in plain.as_chunks_mut::<16>().0 {
            let ct: [u8; 16] = *block;
            cipher.decrypt_block(block.into());
            block.iter_mut().zip(chain).for_each(|(b, c)| *b ^= c);
            chain = ct;
        }
        assert_eq!(&plain[..5], &[0, 3, b'a', b'b', b'c']);
        assert_eq!(plain[12..], mac(0, &plain[..12]));
    }

    #[test]
    fn tampering_and_replay_are_rejected() {
        let (key, iv) = ([3u8; 16], [4u8; 16]);
        let mut wire = Vec::new();
        RecordWriter::new(&key, &iv).seal(b"x", &mut wire);
        let mut bad = wire[2..].to_vec();
        bad[5] ^= 1;
        assert!(RecordReader::new(&key, &iv).open(&bad).is_err());
        let mut r = RecordReader::new(&key, &iv);
        r.seq = 1;
        assert!(r.open(&wire[2..]).is_err());
    }

    #[test]
    fn encryption_update_layout() {
        let auth = [5u8; 16];
        let msg = encryption_update(&auth, &[6; 16], &[7; 16]);
        assert_eq!(&msg[..4], &[0, 0, 0, 1]);
        assert_eq!(i32::from_be_bytes(msg[12..16].try_into().unwrap()), 1103);
        assert_eq!(&msg[16..20], &[0, 0, 0, 1]);
        let mut blocks = msg[20..52].to_vec();
        ecb_decrypt(&auth, &mut blocks);
        assert_eq!(&blocks[..16], &[6; 16]);
        assert_eq!(&blocks[16..], &[7; 16]);
        assert_eq!(msg.len(), 52);
    }
}
