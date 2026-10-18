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

#[derive(Default)]
struct Report {
    failed: bool,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        println!("✓ {what}: {detail}");
    }

    fn warn(&self, what: &str, detail: &str, fix: &str) {
        println!("! {what}: {detail}\n    {fix}");
    }

    fn fail(&mut self, what: &str, detail: &str, fix: &str) {
        self.failed = true;
        println!("✗ {what}: {detail}\n    {fix}");
    }
}

/// Prints one line per check. Returns false if any check failed.
pub fn run() -> Result<bool> {
    let mut r = Report::default();
    let s = match Settings::load(None) {
        Ok(s) => {
            r.ok("Config", "readable");
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

    match s
        .srp_path()
        .and_then(|p| VerifierRecord::load(&p).map(|rec| (p, rec)))
    {
        Ok((p, rec)) => r.ok(
            "Password",
            &format!("set for {} in {}", rec.username, p.display()),
        ),
        Err(e) => r.fail("Password", &format!("{e:#}"), "run `wayshare set-password`"),
    }

    let enabled = systemctl_query("is-enabled");
    let active = systemctl_query("is-active");
    if enabled.is_empty() || enabled == "not-found" {
        r.fail(
            "Service",
            "not installed",
            "run `wayshare install-service`, then `systemctl --user enable --now wayshare`",
        );
    } else if enabled == "enabled" && active == "active" {
        r.ok("Service", "running, starts with your session");
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
    match setup::firewall(port) {
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

    if !s.hp.enabled {
        r.ok("High Performance", "off in config");
    } else {
        #[cfg(target_os = "linux")]
        match crate::server_linux::hp_encoder(&s.video_encoder()) {
            Ok(backend) => r.ok("High Performance", &format!("{backend:?} encoder")),
            Err(e) => r.warn(
                "High Performance",
                &format!("unavailable: {e:#}"),
                "Standard mode still works; see docs/high-performance.md",
            ),
        }
    }

    Ok(!r.failed)
}

fn systemctl_query(verb: &str) -> String {
    Command::new("systemctl")
        .args(["--user", verb, "wayshare"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}
