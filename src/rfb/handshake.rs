use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, info, warn};

use super::dh::{self, DhServer, KEY_LEN};
use super::pixel::PixelFormat;
use super::srp::{self, BufReader, BufWriter, VerifierRecord};
use crate::auth::Authenticator;

pub const SERVER_VERSION: &[u8; 12] = b"RFB 003.889\n";
pub const SECURITY_DH: u8 = 30;
pub const SECURITY_SRP: u8 = 36;

pub const CLIENT_FLAG_SHARED: u8 = 0x01;
pub const CLIENT_FLAG_APPLE: u8 = 0x80;

/// Client message types always advertised in the extended ServerInit bitmap.
pub const BASE_CLIENT_MESSAGES: &[u8] = &[0, 2, 3, 4, 5, 6, 9, 33];
pub const MSG_SET_ENCRYPTION: u8 = 18;
pub const MSG_MEDIA_STREAM_CONFIG: u8 = 28;
pub const MSG_SET_DISPLAY_CONFIG: u8 = 29;
pub const MAX_DISPLAYS: u32 = 1;

const EXT_HEADER_LEN: usize = 22;
const MAX_NAME_FIELD: usize = 1024;
const MAX_SRP_MESSAGE: usize = 0x8000;

#[derive(Clone, Default)]
pub struct AuthConfig {
    pub srp: Option<VerifierRecord>,
    pub dh: Option<Arc<dyn Authenticator>>,
}

impl AuthConfig {
    pub fn security_types(&self) -> Vec<u8> {
        let mut types = Vec::new();
        if self.srp.is_some() {
            types.push(SECURITY_SRP);
        }
        if self.dh.is_some() {
            types.push(SECURITY_DH);
        }
        types
    }
}

pub struct Session {
    pub username: String,
    pub session_key: [u8; 16],
    pub client_flags: u8,
}

pub async fn run<S>(
    stream: &mut S,
    auth: &AuthConfig,
    width: u16,
    height: u16,
    pixel_format: &PixelFormat,
    name: &str,
    server_cmds: &[u8],
) -> Result<Option<Session>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(SERVER_VERSION).await?;
    let mut version = [0u8; 12];
    stream
        .read_exact(&mut version)
        .await
        .context("reading client version")?;
    debug!(client_version = ?String::from_utf8_lossy(&version), "ProtocolVersion");
    let (major, minor) = parse_version(&version)?;

    if !(major == 3 && (minor == 888 || minor == 889)) {
        warn!(major, minor, "non-Apple client, rejecting");
        let reason = b"this server requires the macOS Screen Sharing client";
        if minor >= 7 {
            stream.write_u8(0).await?;
        } else {
            stream.write_u32(0).await?;
        }
        stream.write_u32(reason.len() as u32).await?;
        stream.write_all(reason).await?;
        stream.flush().await?;
        return Ok(None);
    }

    let types = auth.security_types();
    ensure!(!types.is_empty(), "no authentication method configured");
    let mut offer = vec![types.len() as u8];
    offer.extend_from_slice(&types);
    stream.write_all(&offer).await?;
    debug!(?types, "offered security types");

    let selected = stream
        .read_u8()
        .await
        .context("reading selected security type")?;
    debug!(selected, "client selected security type");
    let result = match (selected, &auth.srp, &auth.dh) {
        (SECURITY_SRP, Some(rec), _) => srp_auth(stream, rec).await,
        (30..=32, _, Some(dh)) => dh_auth(stream, dh.as_ref()).await,
        _ => {
            fail(stream, "unsupported security type").await?;
            bail!("client selected unsupported security type {selected}");
        }
    };
    let (username, session_key) = result?;
    stream.write_u32(0).await?;
    info!(user = %username, method = selected, "authenticated");

    let client_flags = stream.read_u8().await.context("reading ClientInit")?;
    debug!(
        client_flags = format_args!("{client_flags:#04x}"),
        "ClientInit"
    );
    let apple = client_flags & CLIENT_FLAG_APPLE != 0;
    stream
        .write_all(&server_init(
            width,
            height,
            pixel_format,
            name,
            apple,
            server_cmds,
        ))
        .await?;
    stream.flush().await?;
    debug!(width, height, apple, "sent ServerInit");

    Ok(Some(Session {
        username,
        session_key,
        client_flags,
    }))
}

async fn dh_auth<S>(stream: &mut S, auth: &dyn Authenticator) -> Result<(String, [u8; 16])>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let dh = DhServer::new();
    let mut msg = Vec::with_capacity(4 + 2 * KEY_LEN);
    msg.extend_from_slice(&dh::GENERATOR.to_be_bytes());
    msg.extend_from_slice(&(KEY_LEN as u16).to_be_bytes());
    msg.extend_from_slice(&dh.prime_bytes());
    msg.extend_from_slice(&dh.public);
    stream.write_all(&msg).await?;
    debug!(g = dh::GENERATOR, key_len = KEY_LEN, "sent DH parameters");

    let mut reply = [0u8; 128 + KEY_LEN];
    stream
        .read_exact(&mut reply)
        .await
        .context("reading DH response")?;
    debug!("received DH response");

    let key = match dh.session_key(&reply[128..]) {
        Ok(k) => k,
        Err(e) => {
            fail(stream, "authentication failed").await?;
            return Err(e);
        }
    };
    let creds = match dh::decrypt_credentials(&key, reply[..128].try_into().unwrap()) {
        Ok(c) => c,
        Err(e) => {
            fail(stream, "authentication failed").await?;
            return Err(e.context("decrypting credentials"));
        }
    };
    if !auth.verify(&creds.username, &creds.password) {
        fail(stream, "authentication failed").await?;
        bail!("authentication failed for user {:?}", creds.username);
    }
    Ok((creds.username, key))
}

async fn read_srp_message<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>> {
    let len = stream.read_u32().await? as usize;
    ensure!(len < MAX_SRP_MESSAGE, "SRP message length {len} too large");
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_srp_message<S: AsyncWrite + Unpin>(stream: &mut S, buf: &[u8]) -> Result<()> {
    stream.write_u32(buf.len() as u32).await?;
    stream.write_all(buf).await?;
    stream.flush().await?;
    Ok(())
}

async fn srp_auth<S>(stream: &mut S, rec: &VerifierRecord) -> Result<(String, [u8; 16])>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let msg = read_srp_message(stream)
        .await
        .context("reading SRP step 1")?;
    let mut r = BufReader::new(&msg)?;
    let (authid, userid, sid, cn) = (r.s()?, r.s()?, r.s()?, r.o()?.to_vec());
    r.finish()?;
    debug!(?authid, ?userid, ?sid, cn_len = cn.len(), "SRP step 1");
    let username = if userid.is_empty() { authid } else { userid };

    // An unknown user gets a throwaway verifier so the failure looks like a bad password.
    let known = username == rec.username;
    let decoy;
    let rec = if known {
        rec
    } else {
        decoy = VerifierRecord::generate(&username, b"", rec.iterations);
        &decoy
    };
    let server = srp::Server::new(rec);
    let challenge = BufWriter::default()
        .c(0)
        .m(&srp::modulus())
        .m_bytes(&[srp::GENERATOR])
        .o(&server.salt)
        .m(server.public())
        .q(server.iterations as u64)
        .s(srp::SERVER_OPTIONS)
        .finish();
    write_srp_message(stream, &challenge).await?;
    debug!(iterations = server.iterations, "sent SRP challenge");

    let msg = read_srp_message(stream)
        .await
        .context("reading SRP step 3")?;
    let mut r = BufReader::new(&msg)?;
    let (a, m1, options, civ) = (r.m()?.to_vec(), r.o()?.to_vec(), r.s()?, r.o()?.to_vec());
    r.finish()?;
    debug!(a_len = a.len(), m1_len = m1.len(), %options, civ_len = civ.len(), "SRP step 3");

    let result = server.verify(&a, &m1).and_then(|v| {
        ensure!(known, "unknown user");
        Ok(v)
    });
    let verified = match result {
        Ok(v) => v,
        Err(e) => {
            stream.write_all(&[0, 0, 0, 1, 0]).await?;
            stream.flush().await?;
            bail!("SRP authentication failed for user {username:?}: {e}");
        }
    };
    let mut siv = [0u8; 16];
    rand::fill(&mut siv[..]);
    let reply = BufWriter::default()
        .o(&verified.m2)
        .o(&siv)
        .s("")
        .u(0)
        .finish();
    write_srp_message(stream, &reply).await?;
    debug!("sent SRP M2");
    Ok((username, srp::rfb_session_key(&verified.key)))
}

async fn fail<S: AsyncWrite + Unpin>(stream: &mut S, reason: &str) -> Result<()> {
    stream.write_u32(1).await?;
    stream.write_u32(reason.len() as u32).await?;
    stream.write_all(reason.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn parse_version(v: &[u8; 12]) -> Result<(u32, u32)> {
    let s = std::str::from_utf8(v).ok();
    let parsed = s.and_then(|s| {
        let rest = s.strip_prefix("RFB ")?.strip_suffix('\n')?;
        let (a, b) = rest.split_once('.')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    });
    parsed.with_context(|| format!("malformed client version {v:02x?}"))
}

pub fn server_cmds_bitmap(types: &[u8]) -> [u8; 16] {
    let mut bits = [0u8; 16];
    for &t in types {
        bits[t as usize / 8] |= 0x80 >> (t % 8);
    }
    bits
}

pub fn server_init(
    width: u16,
    height: u16,
    pf: &PixelFormat,
    name: &str,
    apple: bool,
    server_cmds: &[u8],
) -> Vec<u8> {
    let mut name_field = Vec::new();
    let max_name = if apple {
        MAX_NAME_FIELD - EXT_HEADER_LEN
    } else {
        MAX_NAME_FIELD
    };
    let mut end = name.len().min(max_name);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    if apple {
        name_field.extend_from_slice(&0u16.to_be_bytes());
        name_field.extend_from_slice(&(MAX_DISPLAYS << 5).to_be_bytes());
        name_field.extend_from_slice(&server_cmds_bitmap(server_cmds));
    }
    name_field.extend_from_slice(&name.as_bytes()[..end]);

    let mut out = Vec::with_capacity(24 + name_field.len());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&pf.to_bytes());
    out.extend_from_slice(&(name_field.len() as u32).to_be_bytes());
    out.extend_from_slice(&name_field);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::auth::StaticPassword;
    use crate::rfb::dh::tests::client_respond;
    use crate::rfb::srp::tests::Client as SrpClient;
    use num_bigint::BigUint;
    use sha2::Digest;

    fn dh_config() -> AuthConfig {
        AuthConfig {
            dh: Some(Arc::new(StaticPassword::new("wes", "pw"))),
            srp: None,
        }
    }

    fn both_config() -> AuthConfig {
        AuthConfig {
            srp: Some(VerifierRecord::generate("wes", b"pw", 1000)),
            ..dh_config()
        }
    }

    #[test]
    fn bitmap_matches_legacy_preset() {
        let bits = server_cmds_bitmap(&[0, 2, 3, 4, 5, 6]);
        assert_eq!(bits[0], 0xbe);
        assert!(bits[1..].iter().all(|&b| b == 0));
        let base = server_cmds_bitmap(BASE_CLIENT_MESSAGES);
        assert_eq!(&base[..5], &[0xbe, 0x40, 0, 0, 0x40]);
        let hp = server_cmds_bitmap(&[18, 28]);
        assert_eq!(hp[2], 0x20);
        assert_eq!(hp[3], 0x08);
    }

    #[test]
    fn extended_server_init_layout() {
        let si = server_init(
            1920,
            1080,
            &PixelFormat::NATIVE,
            "wayshare",
            true,
            BASE_CLIENT_MESSAGES,
        );
        assert_eq!(&si[0..4], &[0x07, 0x80, 0x04, 0x38]);
        assert_eq!(&si[4..20], &PixelFormat::NATIVE.to_bytes());
        let name_len = u32::from_be_bytes(si[20..24].try_into().unwrap()) as usize;
        assert_eq!(name_len, 22 + 8);
        assert_eq!(si.len(), 24 + name_len);
        let field = &si[24..];
        assert_eq!(&field[0..2], &[0, 0]);
        let flags = u32::from_be_bytes(field[2..6].try_into().unwrap());
        assert_eq!(flags, 0x20);
        assert_eq!(flags & 0x08, 0);
        assert_eq!(&field[6..22], &server_cmds_bitmap(BASE_CLIENT_MESSAGES));
        assert_eq!(&field[22..], b"wayshare");
    }

    #[test]
    fn extended_server_init_name_bounds() {
        let si = server_init(800, 600, &PixelFormat::NATIVE, "", true, &[]);
        assert_eq!(u32::from_be_bytes(si[20..24].try_into().unwrap()), 22);
        let long = "x".repeat(5000);
        let si = server_init(800, 600, &PixelFormat::NATIVE, &long, true, &[]);
        assert!(u32::from_be_bytes(si[20..24].try_into().unwrap()) < 1025);
    }

    pub(crate) async fn drive_client<S: AsyncRead + AsyncWrite + Unpin>(
        client: &mut S,
        user: &str,
        pass: &str,
    ) -> Result<u32> {
        let mut v = [0u8; 12];
        client.read_exact(&mut v).await?;
        assert_eq!(&v, SERVER_VERSION);
        client.write_all(b"RFB 003.889\n").await?;
        let mut types = [0u8; 2];
        client.read_exact(&mut types).await?;
        assert_eq!(types, [1, 30]);
        client.write_u8(30).await?;
        let g = client.read_u16().await?;
        let key_len = client.read_u16().await? as usize;
        let mut p = vec![0u8; key_len];
        let mut y = vec![0u8; key_len];
        client.read_exact(&mut p).await?;
        client.read_exact(&mut y).await?;
        let (msg, _) = client_respond(g, &p, &y, user, pass);
        client.write_all(&msg).await?;
        Ok(client.read_u32().await?)
    }

    #[tokio::test]
    async fn full_handshake_success() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let auth = dh_config();
        let srv = tokio::spawn(async move {
            run(
                &mut server,
                &auth,
                640,
                480,
                &PixelFormat::NATIVE,
                "test",
                BASE_CLIENT_MESSAGES,
            )
            .await
        });
        assert_eq!(drive_client(&mut client, "wes", "pw").await.unwrap(), 0);
        client
            .write_u8(CLIENT_FLAG_APPLE | CLIENT_FLAG_SHARED)
            .await
            .unwrap();
        let mut init = [0u8; 24 + 22 + 4];
        client.read_exact(&mut init).await.unwrap();
        assert_eq!(&init[46..], b"test");
        let session = srv.await.unwrap().unwrap().unwrap();
        assert_eq!(session.username, "wes");
    }

    #[tokio::test]
    async fn full_handshake_bad_password() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let auth = dh_config();
        let srv = tokio::spawn(async move {
            run(
                &mut server,
                &auth,
                640,
                480,
                &PixelFormat::NATIVE,
                "test",
                BASE_CLIENT_MESSAGES,
            )
            .await
        });
        assert_eq!(drive_client(&mut client, "wes", "nope").await.unwrap(), 1);
        let len = client.read_u32().await.unwrap() as usize;
        let mut reason = vec![0u8; len];
        client.read_exact(&mut reason).await.unwrap();
        assert_eq!(reason, b"authentication failed");
        assert!(srv.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn legacy_client_rejected() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let auth = dh_config();
        let srv = tokio::spawn(async move {
            run(
                &mut server,
                &auth,
                640,
                480,
                &PixelFormat::NATIVE,
                "test",
                BASE_CLIENT_MESSAGES,
            )
            .await
        });
        let mut v = [0u8; 12];
        client.read_exact(&mut v).await.unwrap();
        client.write_all(b"RFB 003.003\n").await.unwrap();
        assert_eq!(client.read_u32().await.unwrap(), 0);
        assert!(srv.await.unwrap().unwrap().is_none());
    }

    async fn srp_client(
        client: &mut tokio::io::DuplexStream,
        user: &str,
        pass: &str,
    ) -> Result<u32> {
        let mut v = [0u8; 12];
        client.read_exact(&mut v).await?;
        client.write_all(b"RFB 003.889\n").await?;
        let mut types = [0u8; 3];
        client.read_exact(&mut types).await?;
        assert_eq!(types, [2, 36, 30]);

        let step1 = BufWriter::default().s("").s(user).s("").o(&[]).finish();
        client.write_u8(SECURITY_SRP).await?;
        client.write_u32(step1.len() as u32).await?;
        client.write_all(&step1).await?;

        let len = client.read_u32().await? as usize;
        let mut buf = vec![0u8; len];
        client.read_exact(&mut buf).await?;
        let mut r = BufReader::new(&buf)?;
        assert_eq!(r.c()?, 0);
        assert_eq!(r.m()?, srp::modulus().to_bytes_be());
        assert_eq!(r.m()?, [2]);
        let salt = r.o()?.to_vec();
        let b_pub = BigUint::from_bytes_be(r.m()?);
        let iterations = r.q()? as u32;
        assert_eq!(r.s()?, srp::SERVER_OPTIONS);
        r.finish()?;

        let c = SrpClient::new();
        let dk = srp::derive_dk(pass.as_bytes(), &salt, iterations);
        let (m1, key) = c.respond(&salt, &dk, &b_pub);
        let step3 = BufWriter::default()
            .m(&c.public)
            .o(&m1)
            .s("mda=SHA-512")
            .o(&[7; 16])
            .finish();
        client.write_u32(step3.len() as u32).await?;
        client.write_all(&step3).await?;

        let len = client.read_u32().await? as usize;
        if len == 1 {
            return Ok(1);
        }
        let mut buf = vec![0u8; len];
        client.read_exact(&mut buf).await?;
        let mut r = BufReader::new(&buf)?;
        let m2 = r.o()?.to_vec();
        assert_eq!(r.o()?.len(), 16);
        r.s()?;
        r.u()?;
        r.finish()?;
        let mut pa = vec![0u8; srp::N_LEN];
        let a = c.public.to_bytes_be();
        pa[srp::N_LEN - a.len()..].copy_from_slice(&a);
        let mut h = sha2::Sha512::new();
        sha2::Digest::update(&mut h, &pa);
        sha2::Digest::update(&mut h, m1);
        sha2::Digest::update(&mut h, key);
        assert_eq!(m2, h.finalize().to_vec());
        Ok(client.read_u32().await?)
    }

    #[tokio::test]
    async fn srp_handshake_success() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let auth = both_config();
        let srv = tokio::spawn(async move {
            run(
                &mut server,
                &auth,
                640,
                480,
                &PixelFormat::NATIVE,
                "test",
                BASE_CLIENT_MESSAGES,
            )
            .await
        });
        assert_eq!(srp_client(&mut client, "wes", "pw").await.unwrap(), 0);
        client.write_u8(CLIENT_FLAG_APPLE).await.unwrap();
        let mut init = [0u8; 24 + 22 + 4];
        client.read_exact(&mut init).await.unwrap();
        let session = srv.await.unwrap().unwrap().unwrap();
        assert_eq!(session.username, "wes");
    }

    #[tokio::test]
    async fn srp_handshake_failures() {
        for (user, pass) in [("wes", "nope"), ("bob", "pw"), ("bob", "")] {
            let (mut client, mut server) = tokio::io::duplex(4096);
            let auth = both_config();
            let srv = tokio::spawn(async move {
                run(
                    &mut server,
                    &auth,
                    640,
                    480,
                    &PixelFormat::NATIVE,
                    "test",
                    BASE_CLIENT_MESSAGES,
                )
                .await
            });
            assert_eq!(srp_client(&mut client, user, pass).await.unwrap(), 1);
            assert!(srv.await.unwrap().is_err());
        }
    }

    #[tokio::test]
    async fn srp_rejects_public_value_congruent_to_zero() {
        let n = srp::modulus();
        let forged = [
            vec![],
            vec![0],
            n.to_bytes_be(),
            (&n * 2u32).to_bytes_be(),
            (&n * 3u32).to_bytes_be(),
        ];
        for a in forged {
            let (mut client, mut server) = tokio::io::duplex(4096);
            let auth = both_config();
            let srv = tokio::spawn(async move {
                run(
                    &mut server,
                    &auth,
                    640,
                    480,
                    &PixelFormat::NATIVE,
                    "test",
                    BASE_CLIENT_MESSAGES,
                )
                .await
            });
            client.read_exact(&mut [0u8; 12]).await.unwrap();
            client.write_all(b"RFB 003.889\n").await.unwrap();
            client.read_exact(&mut [0u8; 3]).await.unwrap();
            let step1 = BufWriter::default().s("").s("wes").s("").o(&[]).finish();
            client.write_u8(SECURITY_SRP).await.unwrap();
            client.write_u32(step1.len() as u32).await.unwrap();
            client.write_all(&step1).await.unwrap();
            let len = client.read_u32().await.unwrap() as usize;
            client.read_exact(&mut vec![0u8; len]).await.unwrap();
            let step3 = BufWriter::default()
                .m_bytes(&a)
                .o(&[0; 64])
                .s("mda=SHA-512")
                .o(&[7; 16])
                .finish();
            client.write_u32(step3.len() as u32).await.unwrap();
            client.write_all(&step3).await.unwrap();
            assert_eq!(client.read_u32().await.unwrap(), 1);
            let Err(err) = srv.await.unwrap() else {
                panic!("A = {a:02x?} accepted")
            };
            assert!(format!("{err:#}").contains("is 0 mod N"), "{err:#}");
        }
    }
}
