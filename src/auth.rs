use std::path::Path;

use anyhow::{Context, Result};
use subtle::ConstantTimeEq;

pub trait Authenticator: Send + Sync {
    fn verify(&self, username: &str, password: &[u8]) -> bool;
}

pub struct StaticPassword {
    username: String,
    password: Vec<u8>,
}

impl StaticPassword {
    pub fn new(username: impl Into<String>, password: impl Into<Vec<u8>>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }

    pub fn from_file(username: impl Into<String>, path: &Path) -> Result<Self> {
        let raw = std::fs::read(path)
            .with_context(|| format!("reading password file {}", path.display()))?;
        let end = raw
            .iter()
            .rposition(|&b| b != b'\n' && b != b'\r')
            .map_or(0, |i| i + 1);
        anyhow::ensure!(end > 0, "password file {} is empty", path.display());
        Ok(Self::new(username, &raw[..end]))
    }
}

impl Authenticator for StaticPassword {
    fn verify(&self, username: &str, password: &[u8]) -> bool {
        let user_ok = username.as_bytes().ct_eq(self.username.as_bytes());
        let pass_ok = password.ct_eq(&self.password);
        (user_ok & pass_ok).into()
    }
}

// RDP receives a password inside TLS rather than an SRP proof. Recompute the same
// verifier using the Apple record's salt, iterations and username, without storing it.
#[cfg(feature = "rdp")]
impl Authenticator for crate::rfb::AuthConfig {
    fn verify(&self, username: &str, password: &[u8]) -> bool {
        let srp_ok = self.srp.as_ref().is_some_and(|rec| {
            let dk = zeroize::Zeroizing::new(crate::rfb::srp::derive_dk(
                password,
                &rec.salt,
                rec.iterations,
            ));
            let candidate =
                crate::rfb::srp::pad(&crate::rfb::srp::verifier(&rec.salt, dk.as_ref()));
            let user_ok = username.as_bytes().ct_eq(rec.username.as_bytes());
            let pass_ok = candidate.as_slice().ct_eq(&rec.verifier);
            bool::from(user_ok & pass_ok)
        });
        let dh_ok = self
            .dh
            .as_ref()
            .is_some_and(|auth| auth.verify(username, password));
        srp_ok | dh_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "rdp")]
    #[test]
    fn apple_credentials_verify_the_same_srp_record_and_ard_backend() {
        use crate::rfb::{AuthConfig, srp::VerifierRecord};
        use std::sync::Arc;
        let generated = VerifierRecord::generate("Viewer@local", b"test credential", 4);
        let record = VerifierRecord::parse(&generated.to_text()).unwrap();
        let auth = AuthConfig {
            srp: Some(record),
            dh: None,
        };
        assert!(auth.verify("Viewer@local", b"test credential"));
        assert!(!auth.verify("viewer@local", b"test credential"));
        assert!(!auth.verify("Viewer@local", b"wrong"));
        assert!(!auth.verify("Viewer@local", b""));
        let auth = AuthConfig {
            dh: Some(Arc::new(StaticPassword::new("ard", "ard credential"))),
            ..auth
        };
        assert!(auth.verify("ard", b"ard credential"));
        assert!(auth.verify("Viewer@local", b"test credential"));
        assert!(!AuthConfig::default().verify("ard", b"ard credential"));
    }

    #[test]
    fn static_password() {
        let a = StaticPassword::new("wes", "hunter2");
        assert!(a.verify("wes", b"hunter2"));
        assert!(!a.verify("wes", b"hunter3"));
        assert!(!a.verify("bob", b"hunter2"));
        assert!(!a.verify("wes", b""));
    }
}
