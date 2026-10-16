use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use anyhow::{Result, ensure};
use md5::{Digest, Md5};
use num_bigint::BigUint;

pub const GENERATOR: u16 = 2;
pub const KEY_LEN: usize = 256;

/// RFC 3526 group 14 (2048-bit MODP).
const MODP_2048_HEX: &str = "\
FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD\
EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F\
83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B\
E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA0510\
15728E5A8AACAA68FFFFFFFFFFFFFFFF";

pub fn prime() -> BigUint {
    BigUint::parse_bytes(MODP_2048_HEX.as_bytes(), 16).expect("valid hex")
}

pub fn to_fixed_be(n: &BigUint) -> [u8; KEY_LEN] {
    let bytes = n.to_bytes_be();
    let mut out = [0u8; KEY_LEN];
    out[KEY_LEN - bytes.len()..].copy_from_slice(&bytes);
    out
}

pub fn session_key(shared: &BigUint) -> [u8; 16] {
    Md5::digest(to_fixed_be(shared)).into()
}

pub struct DhServer {
    p: BigUint,
    private: BigUint,
    pub public: [u8; KEY_LEN],
}

impl DhServer {
    pub fn new() -> Self {
        let mut x = [0u8; KEY_LEN];
        rand::fill(&mut x[..]);
        Self::with_private(&x)
    }

    pub fn with_private(x: &[u8]) -> Self {
        let p = prime();
        let private = BigUint::from_bytes_be(x);
        let public = to_fixed_be(&BigUint::from(GENERATOR).modpow(&private, &p));
        Self { p, private, public }
    }

    pub fn prime_bytes(&self) -> [u8; KEY_LEN] {
        to_fixed_be(&self.p)
    }

    pub fn session_key(&self, client_public: &[u8]) -> Result<[u8; 16]> {
        let a = BigUint::from_bytes_be(client_public);
        let one = BigUint::from(1u8);
        ensure!(
            a > one && a < &self.p - &one,
            "client DH public key out of range"
        );
        Ok(session_key(&a.modpow(&self.private, &self.p)))
    }
}

impl Default for DhServer {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Credentials {
    pub username: String,
    pub password: Vec<u8>,
}

pub fn ecb_decrypt(key: &[u8; 16], data: &mut [u8]) {
    let cipher = Aes128::new(key.into());
    for block in data.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(block.into());
    }
}

pub fn ecb_encrypt(key: &[u8; 16], data: &mut [u8]) {
    let cipher = Aes128::new(key.into());
    for block in data.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(block.into());
    }
}

pub fn decrypt_credentials(key: &[u8; 16], encrypted: &[u8; 128]) -> Result<Credentials> {
    let mut plain = *encrypted;
    ecb_decrypt(key, &mut plain);
    let field = |b: &[u8]| -> Result<Vec<u8>> {
        let end = b.iter().position(|&c| c == 0);
        ensure!(end.is_some(), "credential field not NUL-terminated");
        Ok(b[..end.unwrap()].to_vec())
    };
    let username = String::from_utf8(field(&plain[..64])?)?;
    let password = field(&plain[64..])?;
    Ok(Credentials { username, password })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Client side of the DH exchange, as `_AuthenticateDHNamePassword` does it.
    pub fn client_respond(
        g: u16,
        p: &[u8],
        server_public: &[u8],
        user: &str,
        pass: &str,
    ) -> (Vec<u8>, [u8; 16]) {
        let p = BigUint::from_bytes_be(p);
        let mut x = [0u8; KEY_LEN];
        rand::fill(&mut x[..]);
        let x = BigUint::from_bytes_be(&x);
        let public = to_fixed_be(&BigUint::from(g).modpow(&x, &p));
        let key = session_key(&BigUint::from_bytes_be(server_public).modpow(&x, &p));

        let mut creds = [0u8; 128];
        rand::fill(&mut creds[..]);
        creds[..user.len()].copy_from_slice(user.as_bytes());
        creds[user.len()] = 0;
        creds[64..64 + pass.len()].copy_from_slice(pass.as_bytes());
        creds[64 + pass.len()] = 0;
        ecb_encrypt(&key, &mut creds);

        let mut msg = creds.to_vec();
        msg.extend_from_slice(&public);
        (msg, key)
    }

    #[test]
    fn prime_is_2048_bit_and_passes_fermat() {
        let p = prime();
        assert_eq!(p.bits(), 2048);
        let one = BigUint::from(1u8);
        assert_eq!(BigUint::from(3u8).modpow(&(&p - &one), &p), one);
    }

    #[test]
    fn round_trip() {
        let server = DhServer::new();
        let (msg, client_key) = client_respond(
            GENERATOR,
            &server.prime_bytes(),
            &server.public,
            "wes",
            "s3cret!",
        );
        let key = server.session_key(&msg[128..]).unwrap();
        assert_eq!(key, client_key);
        let creds = decrypt_credentials(&key, msg[..128].try_into().unwrap()).unwrap();
        assert_eq!(creds.username, "wes");
        assert_eq!(creds.password, b"s3cret!");
    }

    #[test]
    fn rejects_degenerate_public_keys() {
        let server = DhServer::new();
        assert!(server.session_key(&[0u8; KEY_LEN]).is_err());
        assert!(server.session_key(&[1u8]).is_err());
        assert!(server.session_key(&server.prime_bytes()).is_err());
    }
}
