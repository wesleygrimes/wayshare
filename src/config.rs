//! `$XDG_CONFIG_HOME/wayshare/config.toml`. Every key is optional; the command line overrides it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::input::{KeyboardConfig, ModTarget};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub listen: SocketAddr,
    /// Name shown in Screen Sharing and in Bonjour.
    pub name: Option<String>,
    /// Defaults to `$XDG_CONFIG_HOME/wayshare/srp-$USER`.
    pub srp_file: Option<PathBuf>,
    /// Wayland output to share. Unset: animated test pattern.
    pub output: Option<String>,
    /// Resize the output to the viewer's requested display mode, restoring it afterwards.
    pub resize: bool,
    pub hp: HpSettings,
    pub input: InputSettings,
    pub mdns: MdnsSettings,
    pub dev: DevSettings,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HpSettings {
    pub enabled: bool,
    /// UDP port for video; audio uses `port + 2`. No inbound firewall rule is needed: the
    /// server sends first and the viewer's RTCP returns through connection tracking.
    pub port: u16,
    pub bitrate: Option<u64>,
    pub encoder: Encoder,
    pub chroma: Chroma,
    /// Stream the default sink's monitor to the viewer.
    pub audio: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Encoder {
    Auto,
    Vaapi,
    X265,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum Chroma {
    #[serde(rename = "420")]
    Yuv420,
    #[serde(rename = "444")]
    Yuv444,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct InputSettings {
    pub enabled: bool,
    /// What the Mac's Command key becomes: "ctrl", "super", "alt" or "altgr".
    pub command_key: String,
    pub option_key: String,
    pub layout: String,
    pub variant: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MdnsSettings {
    /// Advertise `_rfb._tcp` so Finder and Screen Sharing list this machine.
    pub enabled: bool,
    /// Mac model for the Finder icon (`_device-info._tcp` TXT `model=`), e.g. "MacPro7,1".
    pub device_model: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevSettings {
    pub stream_encryption: bool,
    pub hp_stub_dir: Option<PathBuf>,
    pub hp_swap_keys: bool,
    pub dh_username: Option<String>,
    pub dh_password_file: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:5900".parse().unwrap(),
            name: None,
            srp_file: None,
            output: None,
            resize: true,
            hp: HpSettings::default(),
            input: InputSettings::default(),
            mdns: MdnsSettings::default(),
            dev: DevSettings::default(),
        }
    }
}

impl Default for HpSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 5960,
            bitrate: None,
            encoder: Encoder::Auto,
            chroma: Chroma::Yuv420,
            audio: true,
        }
    }
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            command_key: "super".into(),
            option_key: "alt".into(),
            layout: "us".into(),
            variant: String::new(),
        }
    }
}

impl Default for MdnsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            device_model: None,
        }
    }
}

pub fn config_dir() -> Result<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir).join("wayshare")),
        _ => Ok(
            PathBuf::from(std::env::var_os("HOME").context("HOME not set")?)
                .join(".config/wayshare"),
        ),
    }
}

pub fn default_srp_path(user: &str) -> Result<PathBuf> {
    Ok(config_dir()?.join(format!("srp-{user}")))
}

impl Settings {
    /// Encoder settings for HP, as the encode module expects them.
    pub fn video_encoder(&self) -> crate::encode::EncoderConfig {
        use crate::encode::{Chroma as C, EncoderBackend as B, EncoderConfig};
        let defaults = EncoderConfig::default();
        EncoderConfig {
            backend: match self.hp.encoder {
                Encoder::Auto => None,
                Encoder::Vaapi => Some(B::Vaapi),
                Encoder::X265 => Some(B::X265),
            },
            chroma: match self.hp.chroma {
                Chroma::Yuv420 => C::Yuv420,
                Chroma::Yuv444 => C::Yuv444,
            },
            bitrate_bps: self.hp.bitrate.or(defaults.bitrate_bps),
            ..defaults
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let s: Self = toml::from_str(text)?;
        s.validate()?;
        Ok(s)
    }

    /// `explicit` must exist; the default path may be missing.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => config_dir()?.join("config.toml"),
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
                Ok(Self::default())
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.hp.chroma == Chroma::Yuv444 && self.hp.encoder != Encoder::X265 {
            bail!(
                "hp.chroma = \"444\" needs hp.encoder = \"x265\" (VAAPI encodes Main 4:2:0 only)"
            );
        }
        self.keyboard()?;
        Ok(())
    }

    pub fn keyboard(&self) -> Result<KeyboardConfig> {
        Ok(KeyboardConfig {
            command: mod_target(&self.input.command_key)?,
            option: mod_target(&self.input.option_key)?,
            layout: self.input.layout.clone(),
            variant: self.input.variant.clone(),
        })
    }

    pub fn srp_path(&self) -> Result<PathBuf> {
        match &self.srp_file {
            Some(p) => Ok(p.clone()),
            None => {
                let user = std::env::var("USER").context("USER not set; set srp_file")?;
                default_srp_path(&user)
            }
        }
    }
}

fn mod_target(s: &str) -> Result<ModTarget> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => ModTarget::Ctrl,
        "super" | "meta" | "logo" => ModTarget::Super,
        "alt" => ModTarget::Alt,
        "altgr" => ModTarget::AltGr,
        other => bail!("unknown modifier {other:?} (use ctrl, super, alt or altgr)"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_suit_the_real_setup() {
        let s = Settings::parse("").unwrap();
        assert_eq!(s, Settings::default());
        assert!(s.hp.enabled && s.input.enabled && s.resize && s.mdns.enabled);
        assert_eq!(s.listen.port(), 5900);
        assert_eq!(s.keyboard().unwrap().command, ModTarget::Super);
    }

    #[test]
    fn full_file() {
        let s = Settings::parse(
            r#"
            listen = "0.0.0.0:5901"
            name = "omarchy"
            srp_file = "/tmp/srp"
            output = "HEADLESS-1"
            resize = false
            [hp]
            enabled = false
            port = 6000
            bitrate = 30000000
            encoder = "vaapi"
            chroma = "420"
            [input]
            command_key = "super"
            option_key = "altgr"
            layout = "de"
            [mdns]
            device_model = "MacPro7,1"
            [dev]
            hp_swap_keys = true
            "#,
        )
        .unwrap();
        assert_eq!(s.output.as_deref(), Some("HEADLESS-1"));
        assert!(!s.resize && !s.hp.enabled && s.dev.hp_swap_keys);
        assert_eq!((s.hp.port, s.hp.bitrate), (6000, Some(30_000_000)));
        let k = s.keyboard().unwrap();
        assert_eq!(
            (k.command, k.option, k.layout.as_str()),
            (ModTarget::Super, ModTarget::AltGr, "de")
        );
        assert_eq!(s.mdns.device_model.as_deref(), Some("MacPro7,1"));
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Settings::parse("bogus = 1").is_err());
        assert!(Settings::parse("[hp]\nchroma = \"444\"").is_err());
        assert!(Settings::parse("[hp]\nchroma = \"444\"\nencoder = \"x265\"").is_ok());
        assert!(Settings::parse("[input]\ncommand_key = \"hyper\"").is_err());
        assert!(Settings::parse("[hp]\nencoder = \"nvenc\"").is_err());
    }

    #[test]
    fn example_config_parses() {
        let s = Settings::parse(include_str!("../contrib/config.example.toml")).unwrap();
        assert_eq!(s.output.as_deref(), Some("HEADLESS-1"));
        assert!(s.hp.enabled && s.input.enabled && s.resize && s.mdns.enabled);
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        assert!(Settings::load(Some(Path::new("/nonexistent/wayshare.toml"))).is_err());
    }
}
