use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use num_bigint::BigUint;
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;

pub const N_LEN: usize = 256;
pub const GENERATOR: u8 = 2;
pub const DEFAULT_ITERATIONS: u32 = 20_000;
pub const DK_LEN: usize = 128;
pub const SERVER_OPTIONS: &str = "mda=SHA-512,replay_detection,conf+int=ChaCha20-Poly1305,\
kdf=SALTED-SHA512-PBKDF2,maxbuffersize=65536";

/// RFC 5054 appendix A, 2048-bit group.
const N_HEX: &str = "\
AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC3192943DB56050A37329CBB4A099ED8193E0757767A13D\
D52312AB4B03310DCD7F48A9DA04FD50E8083969EDB767B0CF6095179A163AB3661A05FBD5FAAAE82918A9962F0B93B8\
55F97993EC975EEAA80D740ADBF4FF747359D041D5C33EA71D281E446B14773BCA97B43A23FB801676BD207A436C6481\
F1D2B9078717461A5B9D32E688F87748544523B524B0D57D5EA77A2775D2ECFA032CFBDBF52FB3786160279004E57AE6\
AF874E7303CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8E9DBFBB694B5C803D89F7AE435DE236D525F5475\
9B65E372FCD68EF20FA7111F9E4AFF73";

pub fn modulus() -> BigUint {
    BigUint::parse_bytes(N_HEX.as_bytes(), 16).expect("valid hex")
}

pub(crate) fn pad(n: &BigUint) -> [u8; N_LEN] {
    let bytes = n.to_bytes_be();
    let mut out = [0u8; N_LEN];
    out[N_LEN - bytes.len()..].copy_from_slice(&bytes);
    out
}

fn h(parts: &[&[u8]]) -> [u8; 64] {
    let mut d = Sha512::new();
    for p in parts {
        d.update(p);
    }
    d.finalize().into()
}

pub fn derive_dk(password: &[u8], salt: &[u8], iterations: u32) -> [u8; DK_LEN] {
    let mut dk = [0u8; DK_LEN];
    pbkdf2::pbkdf2_hmac::<Sha512>(password, salt, iterations, &mut dk);
    dk
}

/// v = g^x, x = H(s | H(I ":" P)) with I = "" and P = the PBKDF2 output.
pub fn verifier(salt: &[u8], srp_password: &[u8]) -> BigUint {
    let x = BigUint::from_bytes_be(&h(&[salt, &h(&[b":", srp_password])]));
    BigUint::from(GENERATOR).modpow(&x, &modulus())
}

/// Session key for the RFB cryptors: SHA-256 of the SRP K, truncated.
pub fn rfb_session_key(k: &[u8]) -> [u8; 16] {
    Sha256::digest(k)[..16].try_into().unwrap()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierRecord {
    pub username: String,
    pub salt: Vec<u8>,
    pub iterations: u32,
    pub verifier: Vec<u8>,
}

impl VerifierRecord {
    pub fn generate(username: &str, password: &[u8], iterations: u32) -> Self {
        let mut salt = vec![0u8; 16];
        rand::fill(&mut salt[..]);
        let dk = derive_dk(password, &salt, iterations);
        let verifier = pad(&verifier(&salt, &dk)).to_vec();
        Self {
            username: username.to_owned(),
            salt,
            iterations,
            verifier,
        }
    }

    pub fn to_text(&self) -> String {
        format!(
            "user={}\nsalt={}\niterations={}\nverifier={}\n",
            self.username,
            hex(&self.salt),
            self.iterations,
            hex(&self.verifier)
        )
    }

    pub fn parse(text: &str) -> Result<Self> {
        let (mut user, mut salt, mut iterations, mut verifier) = (None, None, None, None);
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (k, v) = line
                .split_once('=')
                .with_context(|| format!("bad line {line:?}"))?;
            match k {
                "user" => user = Some(v.to_owned()),
                "salt" => salt = Some(unhex(v)?),
                "iterations" => iterations = Some(v.parse()?),
                "verifier" => verifier = Some(unhex(v)?),
                _ => bail!("unknown key {k:?}"),
            }
        }
        let rec = Self {
            username: user.context("missing user")?,
            salt: salt.context("missing salt")?,
            iterations: iterations.context("missing iterations")?,
            verifier: verifier.context("missing verifier")?,
        };
        ensure!(
            !rec.salt.is_empty() && rec.salt.len() <= 255,
            "bad salt length"
        );
        ensure!(
            rec.verifier.len() == N_LEN,
            "verifier must be {N_LEN} bytes"
        );
        Ok(rec)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading SRP verifier file {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts
            .open(path)
            .with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(self.to_text().as_bytes())?;
        Ok(())
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    ensure!(s.len().is_multiple_of(2), "odd-length hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}

pub struct Server {
    n: BigUint,
    v: BigUint,
    b: BigUint,
    public: BigUint,
    pub salt: Vec<u8>,
    pub iterations: u32,
}

pub struct Verified {
    pub m2: [u8; 64],
    pub key: [u8; 64],
}

impl Server {
    pub fn new(record: &VerifierRecord) -> Self {
        let mut b = [0u8; 64];
        rand::fill(&mut b[..]);
        Self::with_private(record, &b)
    }

    pub fn with_private(record: &VerifierRecord, b: &[u8]) -> Self {
        let n = modulus();
        let g = BigUint::from(GENERATOR);
        let v = BigUint::from_bytes_be(&record.verifier);
        let b = BigUint::from_bytes_be(b);
        let k = BigUint::from_bytes_be(&h(&[&n.to_bytes_be(), &pad(&g)]));
        let public = (k * &v + g.modpow(&b, &n)) % &n;
        Self {
            n,
            v,
            b,
            public,
            salt: record.salt.clone(),
            iterations: record.iterations,
        }
    }

    pub fn public(&self) -> &BigUint {
        &self.public
    }

    pub fn verify(&self, a_bytes: &[u8], m1: &[u8]) -> Result<Verified> {
        let a = BigUint::from_bytes_be(a_bytes);
        ensure!(
            &a % &self.n != BigUint::ZERO,
            "client SRP public key A is 0 mod N"
        );
        let (pa, pb) = (pad(&a), pad(&self.public));
        let u = BigUint::from_bytes_be(&h(&[&pa, &pb]));
        ensure!(u != BigUint::ZERO, "SRP u is 0");
        let s = (a * self.v.modpow(&u, &self.n)).modpow(&self.b, &self.n);
        let key = h(&[&pad(&s)]);

        let hn = h(&[&self.n.to_bytes_be()]);
        let hg = h(&[&pad(&BigUint::from(GENERATOR))]);
        let mut hng = [0u8; 64];
        for (o, (x, y)) in hng.iter_mut().zip(hn.iter().zip(hg.iter())) {
            *o = x ^ y;
        }
        let expected = h(&[&hng, &h(&[b""]), &self.salt, &pa, &pb, &key]);
        ensure!(
            bool::from(expected.ct_eq(m1)),
            "SRP M1 mismatch (wrong password)"
        );
        Ok(Verified {
            m2: h(&[&pa, m1, &key]),
            key,
        })
    }
}

/// SASL-SRP style buffer: BE32 length of the rest, then fields.
#[derive(Default)]
pub struct BufWriter(Vec<u8>);

impl BufWriter {
    pub fn c(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    pub fn m(self, n: &BigUint) -> Self {
        self.m_bytes(&n.to_bytes_be())
    }
    pub fn m_bytes(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(&(b.len() as u16).to_be_bytes());
        self.0.extend_from_slice(b);
        self
    }
    pub fn o(mut self, b: &[u8]) -> Self {
        assert!(b.len() <= 255);
        self.0.push(b.len() as u8);
        self.0.extend_from_slice(b);
        self
    }
    pub fn s(mut self, s: &str) -> Self {
        self.0.extend_from_slice(&(s.len() as u16).to_be_bytes());
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    pub fn u(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn q(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn finish(self) -> Vec<u8> {
        let mut out = (self.0.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&self.0);
        out
    }
}

pub struct BufReader<'a>(&'a [u8]);

impl<'a> BufReader<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        ensure!(buf.len() >= 4, "SRP buffer too short");
        let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
        ensure!(
            len == buf.len() - 4,
            "SRP buffer length {len} != {}",
            buf.len() - 4
        );
        Ok(Self(&buf[4..]))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.0.len() >= n, "SRP buffer truncated");
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    pub fn c(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn len16(&mut self) -> Result<usize> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize)
    }
    pub fn m(&mut self) -> Result<&'a [u8]> {
        let n = self.len16()?;
        self.take(n)
    }
    pub fn o(&mut self) -> Result<&'a [u8]> {
        let n = self.c()? as usize;
        self.take(n)
    }
    pub fn s(&mut self) -> Result<String> {
        let n = self.len16()?;
        Ok(String::from_utf8(self.take(n)?.to_vec())?)
    }
    pub fn u(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn q(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn finish(self) -> Result<()> {
        ensure!(
            self.0.is_empty(),
            "{} trailing bytes in SRP buffer",
            self.0.len()
        );
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Client side, mirroring corecrypto's ccsrp client as used by ScreenSharing.framework.
    pub struct Client {
        a: BigUint,
        pub public: BigUint,
    }

    impl Client {
        pub fn new() -> Self {
            let mut a = [0u8; 64];
            rand::fill(&mut a[..]);
            let a = BigUint::from_bytes_be(&a);
            let public = BigUint::from(GENERATOR).modpow(&a, &modulus());
            Self { a, public }
        }

        /// Returns (M1, K).
        pub fn respond(
            &self,
            salt: &[u8],
            srp_password: &[u8],
            b_pub: &BigUint,
        ) -> ([u8; 64], [u8; 64]) {
            let n = modulus();
            let g = BigUint::from(GENERATOR);
            let k = BigUint::from_bytes_be(&h(&[&n.to_bytes_be(), &pad(&g)]));
            let x = BigUint::from_bytes_be(&h(&[salt, &h(&[b":", srp_password])]));
            let u = BigUint::from_bytes_be(&h(&[&pad(&self.public), &pad(b_pub)]));
            let gx = (k * g.modpow(&x, &n)) % &n;
            let base = (b_pub + &n - gx) % &n;
            let s = base.modpow(&(&self.a + u * x), &n);
            let key = h(&[&pad(&s)]);
            let hn = h(&[&n.to_bytes_be()]);
            let hg = h(&[&pad(&g)]);
            let hng: Vec<u8> = hn.iter().zip(hg.iter()).map(|(x, y)| x ^ y).collect();
            let m1 = h(&[
                &hng,
                &h(&[b""]),
                salt,
                &pad(&self.public),
                &pad(b_pub),
                &key,
            ]);
            (m1, key)
        }
    }

    fn record(salt: &[u8], srp_password: &[u8]) -> VerifierRecord {
        VerifierRecord {
            username: "u".into(),
            salt: salt.to_vec(),
            iterations: 1,
            verifier: pad(&verifier(salt, srp_password)).to_vec(),
        }
    }

    #[test]
    fn round_trip() {
        let rec = VerifierRecord::generate("wes", b"hunter2", 1000);
        let server = Server::new(&rec);
        let client = Client::new();
        let dk = derive_dk(b"hunter2", &rec.salt, 1000);
        let (m1, key) = client.respond(&rec.salt, &dk, server.public());
        let ok = server.verify(&client.public.to_bytes_be(), &m1).unwrap();
        assert_eq!(ok.key, key);

        let bad = derive_dk(b"hunter3", &rec.salt, 1000);
        let (m1, _) = client.respond(&rec.salt, &bad, server.public());
        assert!(server.verify(&client.public.to_bytes_be(), &m1).is_err());
    }

    #[test]
    fn rejects_zero_a() {
        let server = Server::new(&record(b"salt", b"pw"));
        assert!(server.verify(&modulus().to_bytes_be(), &[0; 64]).is_err());
        assert!(server.verify(&[0], &[0; 64]).is_err());
    }

    #[test]
    fn record_text_round_trip() {
        let rec = VerifierRecord::generate("wes", b"pw", 10);
        assert_eq!(VerifierRecord::parse(&rec.to_text()).unwrap(), rec);
        assert!(!rec.to_text().contains("pw\n"));
    }

    #[test]
    fn sasl_buffers() {
        let buf = BufWriter::default()
            .c(0)
            .m_bytes(&[1, 2])
            .o(&[9])
            .s("ab")
            .q(7)
            .u(3)
            .finish();
        assert_eq!(
            buf,
            [
                0, 0, 0, 23, 0, 0, 2, 1, 2, 1, 9, 0, 2, b'a', b'b', 0, 0, 0, 0, 0, 0, 0, 7, 0, 0,
                0, 3
            ]
        );
        let mut r = BufReader::new(&buf).unwrap();
        assert_eq!(r.c().unwrap(), 0);
        assert_eq!(r.m().unwrap(), [1, 2]);
        assert_eq!(r.o().unwrap(), [9]);
        assert_eq!(r.s().unwrap(), "ab");
        assert_eq!(r.q().unwrap(), 7);
        assert_eq!(r.u().unwrap(), 3);
        r.finish().unwrap();
        assert!(BufReader::new(&[0, 0, 0, 5, 1]).is_err());
    }

    // Vectors from corecrypto's ccsrp client (wayshare/tools/ccsrp-xcheck.c): salt 0102030405060708,
    // SRP password "pass", server b = 0xab54a98ceb1f0ad2. One case has a 255-byte A, the other a
    // 255-byte premaster secret S, which pins down the PAD() rules.
    const XCHECK_B: [u8; 8] = [0xab, 0x54, 0xa9, 0x8c, 0xeb, 0x1f, 0x0a, 0xd2];
    include!("srp_vectors.rs");

    #[test]
    fn matches_corecrypto() {
        let rec = record(&[1, 2, 3, 4, 5, 6, 7, 8], b"pass");
        let server = Server::with_private(&rec, &XCHECK_B);
        for (a, m1, k, m2) in XCHECK_VECTORS {
            let a = unhex(a).unwrap();
            let ok = server.verify(&a, &unhex(m1).unwrap()).unwrap();
            assert_eq!(hex(&ok.key), *k);
            assert_eq!(hex(&ok.m2), *m2);
        }
    }
}
