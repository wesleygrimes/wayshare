//! `wayshare doctor`: checks each piece a working setup needs and says how to fix what's missing.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;

use crate::config::Settings;
use crate::display::DisplayManager;
use crate::display::hyprland::Hyprland;
use crate::rfb::srp::VerifierRecord;
use crate::setup::{self, Firewall};
use crate::ui;

#[derive(Default)]
struct Report {
    failed: usize,
    warned: usize,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        ui::check(Some(true), what, detail);
    }

    fn warn(&mut self, what: &str, detail: &str, fix: &str) {
        self.warned += 1;
        ui::check(None, what, detail);
        ui::check_hint(fix);
    }

    fn fail(&mut self, what: &str, detail: &str, fix: &str) {
        self.failed += 1;
        ui::check(Some(false), what, detail);
        ui::check_hint(fix);
    }

    fn summary(&self) {
        match (self.failed, self.warned) {
            (0, 0) => ui::heading("Everything looks good."),
            (0, w) => ui::heading(&format!("No problems; {w} warning{}.", plural(w))),
            (f, _) => ui::alert(&format!("{f} problem{} found.", plural(f))),
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Prints one line per check. Returns false if any check failed.
pub fn run() -> Result<bool> {
    ui::title("wayshare doctor");
    let mut r = Report::default();
    let s = match Settings::load(None) {
        Ok(s) => {
            let path = crate::config::config_dir()?.join("config.toml");
            let detail = if path.exists() {
                ui::path(&path)
            } else {
                "defaults (no config.toml)".into()
            };
            r.ok("Config", &detail);
            s
        }
        Err(e) => {
            r.fail(
                "Config",
                &format!("{e:#}"),
                "fix config.toml, then run this again",
            );
            return Ok(false);
        }
    };

    let mut hypr = Hyprland::from_env().and_then(|h| h.check_version().map(|()| h));
    match &hypr {
        Ok(_) => r.ok("Hyprland", "0.56 or newer"),
        Err(e) => r.fail(
            "Hyprland",
            &format!("{e:#}"),
            "run this inside a Hyprland 0.56+ session",
        ),
    }

    if let Ok(hypr) = &mut hypr {
        match hypr.outputs() {
            Ok(outs) => match &s.output {
                Some(name) if outs.iter().any(|o| &o.name == name && !o.disabled) => {
                    r.ok("Monitor", name)
                }
                Some(name) => r.fail(
                    "Monitor",
                    &format!("no enabled monitor named {name}"),
                    "run `wayshare setup` to pick one",
                ),
                None => match setup::default_output(&outs) {
                    Ok(name) => r.ok("Monitor", &format!("{name} (the only one)")),
                    Err(e) => r.fail("Monitor", &format!("{e:#}"), "run `wayshare setup`"),
                },
            },
            Err(e) => r.fail(
                "Monitor",
                &format!("{e:#}"),
                "check that Hyprland is running",
            ),
        }
    }

    let set_password = |p: &std::path::Path| match &s.srp_file {
        Some(_) => format!(
            "run `wayshare set-password --user NAME --srp-file {}`",
            p.display()
        ),
        None => "run `wayshare set-password`".into(),
    };
    match s.srp_path() {
        Ok(p) if !p.exists() => r.fail("Password", "not set", &set_password(&p)),
        Ok(p) => match VerifierRecord::load(&p) {
            Ok(rec) => r.ok(
                "Password",
                &format!("set for {} in {}", rec.username, ui::path(&p)),
            ),
            Err(e) => r.fail("Password", &format!("{e:#}"), &set_password(&p)),
        },
        Err(e) => r.fail("Password", &format!("{e:#}"), "set srp_file in config.toml"),
    }

    let enabled = systemctl_query("is-enabled");
    let active = systemctl_query("is-active");
    if enabled.is_empty() || enabled == "not-found" {
        r.fail("Service", "not installed", "run `wayshare setup`");
    } else if enabled == "enabled" && active == "active" {
        r.ok("Service", "running, starts when you log in");
    } else {
        r.fail(
            "Service",
            &format!("{active}, {enabled}"),
            "run `systemctl --user enable --now wayshare`; logs: `journalctl --user -u wayshare`",
        );
    }

    let probe = match s.listen.ip() {
        ip if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), s.listen.port())
        }
        _ => s.listen,
    };
    match TcpStream::connect_timeout(&probe, Duration::from_secs(1)) {
        Ok(_) => r.ok("Listening", &s.listen.to_string()),
        Err(e) => r.fail(
            "Listening",
            &format!("nothing answers on {probe}: {e}"),
            "check `journalctl --user -u wayshare`",
        ),
    }

    let port = s.listen.port();
    let addrs = setup::interface_addrs();
    let bound = addrs.iter().find(|a| IpAddr::V4(a.ip) == s.listen.ip());
    let lan_ip = bound.map(|a| a.ip).or_else(setup::lan_ipv4);
    let hint = || setup::ufw_hint(port, bound, lan_ip, &addrs);
    match setup::firewall(s.listen) {
        Firewall::Off => r.ok("Firewall", "ufw is off"),
        Firewall::Allows => r.ok("Firewall", &format!("ufw allows TCP {port}")),
        Firewall::Blocks => r.fail(
            "Firewall",
            &format!("ufw has no rule for TCP {port}"),
            &hint(),
        ),
        Firewall::Unknown => r.warn(
            "Firewall",
            &format!("ufw is on; can't read its rules without root to check TCP {port}"),
            &format!("if the Mac can't connect: {}", hint()),
        ),
    }

    if let Some(rdp_port) = rdp_firewall_port(&s) {
        let rdp_bound = addrs.iter().find(|a| IpAddr::V4(a.ip) == s.rdp.listen.ip());
        let rdp_ip = rdp_bound.map(|a| a.ip).or_else(setup::lan_ipv4);
        let rdp_hint = || setup::ufw_hint(rdp_port, rdp_bound, rdp_ip, &addrs);
        match setup::firewall(s.rdp.listen) {
            Firewall::Off => r.ok("RDP firewall", "ufw is off"),
            Firewall::Allows => r.ok("RDP firewall", &format!("ufw allows TCP {rdp_port}")),
            Firewall::Blocks => r.warn(
                "RDP firewall",
                &format!("ufw has no rule for TCP {rdp_port}"),
                &format!("for RDP clients on other machines: {}", rdp_hint()),
            ),
            Firewall::Unknown => r.warn(
                "RDP firewall",
                &format!("ufw is on; can't read its rules without root to check TCP {rdp_port}"),
                &format!("if RDP clients can't connect: {}", rdp_hint()),
            ),
        }
    }

    if !s.hp.enabled {
        r.ok("High Performance", "off in config");
    } else {
        #[cfg(target_os = "linux")]
        match crate::server_linux::hp_encoder(&s.video_encoder()) {
            Ok(backend) => {
                let name = match backend {
                    crate::encode::EncoderBackend::Vaapi => "VA-API (hardware)",
                    crate::encode::EncoderBackend::Nvenc => "NVENC (hardware)",
                    crate::encode::EncoderBackend::X265 => "x265 (software)",
                };
                r.ok("High Performance", name)
            }
            Err(e) => r.warn(
                "High Performance",
                &format!("unavailable: {e:#}"),
                "Standard mode still works; see docs/high-performance.md",
            ),
        }
    }

    r.summary();
    Ok(r.failed == 0)
}

/// The RDP port whose ufw rule doctor checks, or `None` when RDP is off (including builds
/// without the `rdp` feature, where it can't be enabled) or only listens on loopback.
/// Matches the ports `wayshare setup` offers rules for.
fn rdp_firewall_port(s: &Settings) -> Option<u16> {
    (s.rdp.enabled && !s.rdp.listen.ip().is_loopback()).then(|| s.rdp.listen.port())
}

fn systemctl_query(verb: &str) -> String {
    Command::new("systemctl")
        .args(["--user", verb, "wayshare"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdp_firewall_port_follows_rdp_settings() {
        let mut s = Settings::default();
        s.rdp.enabled = true;
        assert_eq!(rdp_firewall_port(&s), Some(3389));
        s.rdp.listen = "192.168.1.50:3390".parse().unwrap();
        assert_eq!(rdp_firewall_port(&s), Some(3390));
        s.rdp.listen = "127.0.0.1:3389".parse().unwrap();
        assert_eq!(rdp_firewall_port(&s), None);
        s.rdp.listen = "0.0.0.0:3389".parse().unwrap();
        s.rdp.enabled = false;
        assert_eq!(rdp_firewall_port(&s), None);
    }

    #[test]
    fn rdp_firewall_port_is_off_by_default_without_the_feature() {
        let expected = cfg!(feature = "rdp").then_some(3389);
        assert_eq!(rdp_firewall_port(&Settings::default()), expected);
    }
}
