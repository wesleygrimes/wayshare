//! `wayshare setup`, and the helpers it shares with `wayshare doctor` and the server.

use std::io::{BufRead, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::{Settings, config_dir};
use crate::display::hyprland::Hyprland;
use crate::display::{DisplayManager, OutputInfo};
use crate::rfb::srp::{self, VerifierRecord};

pub fn run() -> Result<()> {
    let mut hypr =
        Hyprland::from_env().context("run `wayshare setup` inside your Hyprland session")?;
    hypr.check_version()?;
    let config_path = config_dir()?.join("config.toml");
    let settings = Settings::load(None)?;

    let user = current_user()?;
    let srp_path = settings.srp_path()?;
    if settings.srp_file.is_some() {
        println!("Using the password in {}", srp_path.display());
    } else if !srp_path.exists() || ask_yes_no(&format!("Change the password for {user}?"))? {
        println!("Pick a password. Screen Sharing asks for it with the user name {user}.");
        let password = read_password(None)?;
        VerifierRecord::generate(&user, &password, srp::DEFAULT_ITERATIONS).save(&srp_path)?;
    }

    let output = choose_output(&hypr.outputs()?)?;
    let addrs = interface_addrs();
    let listen = choose_listen(settings.listen, &addrs)?;
    let text = match std::fs::read_to_string(&config_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", config_path.display())),
    };
    let mut text = set_top_level_key(&text, "output", &output);
    if listen != settings.listen {
        text = set_top_level_key(&text, "listen", &listen.to_string());
    }
    let settings =
        Settings::parse(&text).with_context(|| format!("in {}", config_path.display()))?;
    std::fs::create_dir_all(config_path.parent().unwrap())?;
    std::fs::write(&config_path, text)
        .with_context(|| format!("writing {}", config_path.display()))?;
    println!("Sharing {output}; saved to {}", config_path.display());

    // A package's unit in /usr/lib would be shadowed by one written to ~/.local.
    if !unit_loaded() {
        crate::service::install()?;
        systemctl(&["daemon-reload"])?;
    }
    systemctl(&["enable", "wayshare"])?;
    systemctl(&["restart", "wayshare"])?;
    println!("wayshare is running and starts with your session.");

    let bound = addrs.iter().find(|a| IpAddr::V4(a.ip) == listen.ip());
    let ip = bound.map(|a| a.ip).or_else(lan_ipv4);
    match firewall(listen.port()) {
        Firewall::Off | Firewall::Allows => {}
        Firewall::Blocks | Firewall::Unknown => {
            println!("\nufw is on. If you haven't already, let the Mac in:");
            println!("  {}", ufw_hint(listen.port(), bound, ip, &addrs));
        }
    }

    let name = settings.name.clone().unwrap_or_else(hostname);
    println!("\nOn your Mac, open Screen Sharing, choose Connection > New, and enter:");
    match ip {
        Some(ip) => println!("  {ip}"),
        None => println!("  this machine's IP address"),
    }
    if settings.mdns.enabled && bound.is_none() {
        println!("Or click {name} under Network in Finder's sidebar.");
    }
    println!("Sign in as {user}. Something wrong? Run `wayshare doctor`.");
    Ok(())
}

pub fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "wayshare".into())
}

pub fn current_user() -> Result<String> {
    std::env::var("USER").context("USER not set; pass --user")
}

pub fn read_password(path: Option<&Path>) -> Result<Vec<u8>> {
    let pw = match path {
        Some(p) => {
            let raw = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            let end = raw
                .iter()
                .rposition(|&b| b != b'\n' && b != b'\r')
                .map_or(0, |i| i + 1);
            raw[..end].to_vec()
        }
        None => {
            let a = rpassword::prompt_password("Password: ")?;
            let b = rpassword::prompt_password("Again: ")?;
            if a != b {
                bail!("passwords do not match");
            }
            a.into_bytes()
        }
    };
    if pw.is_empty() {
        bail!("empty password");
    }
    Ok(pw)
}

fn ask(prompt: &str) -> Result<String> {
    print!("{prompt} ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        bail!("no answer on stdin");
    }
    Ok(line.trim().to_string())
}

fn ask_yes_no(prompt: &str) -> Result<bool> {
    let a = ask(&format!("{prompt} [y/N]"))?;
    Ok(matches!(a.to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn choose_output(outputs: &[OutputInfo]) -> Result<String> {
    let enabled: Vec<_> = outputs.iter().filter(|o| !o.disabled).collect();
    match enabled.as_slice() {
        [] => bail!("Hyprland reports no enabled monitors"),
        [only] => return Ok(only.name.clone()),
        _ => {}
    }
    let default = enabled.iter().position(|o| o.focused).unwrap_or(0);
    println!("Which monitor should the Mac see?");
    for (i, o) in enabled.iter().enumerate() {
        println!("  {}) {} ({}x{})", i + 1, o.name, o.width, o.height);
    }
    loop {
        let a = ask(&format!("Monitor [{}]:", default + 1))?;
        if a.is_empty() {
            return Ok(enabled[default].name.clone());
        }
        match a.parse::<usize>() {
            Ok(n) if (1..=enabled.len()).contains(&n) => return Ok(enabled[n - 1].name.clone()),
            _ => println!("Enter a number from 1 to {}.", enabled.len()),
        }
    }
}

fn choose_listen(current: SocketAddr, addrs: &[InterfaceAddr]) -> Result<SocketAddr> {
    let port = current.port();
    let mut choices = vec![(
        SocketAddr::from(([0, 0, 0, 0], port)),
        "all interfaces".to_string(),
    )];
    for a in addrs.iter().filter(|a| !a.ip.is_loopback()) {
        choices.push((
            SocketAddr::from((a.ip, port)),
            format!("{} {}", a.name, a.ip),
        ));
    }
    let default = choices
        .iter()
        .position(|(addr, _)| *addr == current)
        .unwrap_or(0);
    println!("Which network should wayshare listen on?");
    for (i, (_, label)) in choices.iter().enumerate() {
        println!("  {}) {label}", i + 1);
    }
    loop {
        let a = ask(&format!("Network [{}]:", default + 1))?;
        if a.is_empty() {
            return Ok(choices[default].0);
        }
        match a.parse::<usize>() {
            Ok(n) if (1..=choices.len()).contains(&n) => return Ok(choices[n - 1].0),
            _ => println!("Enter a number from 1 to {}.", choices.len()),
        }
    }
}

/// The output to share when the config doesn't name one: the only enabled monitor.
pub fn default_output(outputs: &[OutputInfo]) -> Result<String> {
    let enabled: Vec<_> = outputs.iter().filter(|o| !o.disabled).collect();
    match enabled.as_slice() {
        [only] => Ok(only.name.clone()),
        [] => bail!("Hyprland reports no enabled monitors"),
        many => {
            let names: Vec<_> = many.iter().map(|o| o.name.as_str()).collect();
            bail!(
                "no output set and there are {} monitors ({}); run `wayshare setup` or set \
                 output in config.toml",
                many.len(),
                names.join(", ")
            )
        }
    }
}

/// Sets a top-level `key = "value"` line, keeping the rest of the file as written.
pub fn set_top_level_key(text: &str, key: &str, value: &str) -> String {
    let line = format!("{key} = {}", toml::Value::String(value.into()));
    let mut out = Vec::new();
    let mut done = false;
    let mut in_table = false;
    for l in text.lines() {
        let t = l.trim_start();
        if t.starts_with('[') {
            if !done {
                out.push(line.clone());
                done = true;
            }
            in_table = true;
        }
        let is_key = t
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if !in_table && is_key && !done {
            out.push(line.clone());
            done = true;
            continue;
        }
        out.push(l.to_string());
    }
    if !done {
        out.push(line);
    }
    out.join("\n") + "\n"
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

fn unit_loaded() -> bool {
    Command::new("systemctl")
        .args(["--user", "show", "-p", "LoadState", "--value", "wayshare"])
        .output()
        .is_ok_and(|o| o.stdout.trim_ascii() == b"loaded")
}

/// The address other machines on the LAN most likely use for this one.
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    // connect() on UDP only picks a route and source address; nothing is sent.
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceAddr {
    pub name: String,
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

pub fn interface_addrs() -> Vec<InterfaceAddr> {
    Command::new("ip")
        .args(["-o", "-4", "addr", "show"])
        .output()
        .map(|o| parse_addrs(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Parses `ip -o -4 addr show`, e.g. `2: enp1s0    inet 192.168.1.118/24 brd ...`.
pub fn parse_addrs(text: &str) -> Vec<InterfaceAddr> {
    text.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace().skip(1);
            let name = w.next()?.to_string();
            if w.next()? != "inet" {
                return None;
            }
            let (ip, prefix) = w.next()?.split_once('/')?;
            Some(InterfaceAddr {
                name,
                ip: ip.parse().ok()?,
                prefix: prefix.parse().ok().filter(|&p| p <= 32)?,
            })
        })
        .collect()
}

pub fn subnet(ip: Ipv4Addr, prefix: u8) -> String {
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    format!("{}/{prefix}", Ipv4Addr::from(u32::from(ip) & mask))
}

/// The ufw rule for `port`: by interface when wayshare listens on one, else by LAN range.
pub fn ufw_hint(
    port: u16,
    bound: Option<&InterfaceAddr>,
    lan_ip: Option<Ipv4Addr>,
    addrs: &[InterfaceAddr],
) -> String {
    if let Some(a) = bound {
        return format!(
            "sudo ufw allow in on {} to any port {port} proto tcp",
            a.name
        );
    }
    let range = lan_ip
        .and_then(|ip| addrs.iter().find(|a| a.ip == ip))
        .map_or_else(|| "192.168.1.0/24".into(), |a| subnet(a.ip, a.prefix));
    format!("sudo ufw allow from {range} to any port {port} proto tcp")
}

#[derive(Debug, PartialEq, Eq)]
pub enum Firewall {
    Off,
    Allows,
    Blocks,
    /// ufw is on, but its rules are only readable by root.
    Unknown,
}

pub fn firewall(port: u16) -> Firewall {
    let Ok(conf) = std::fs::read_to_string("/etc/ufw/ufw.conf") else {
        return Firewall::Off;
    };
    if !ufw_enabled(&conf) {
        return Firewall::Off;
    }
    match std::fs::read_to_string("/etc/ufw/user.rules") {
        Ok(rules) if ufw_allows(&rules, port) => Firewall::Allows,
        Ok(_) => Firewall::Blocks,
        Err(_) => Firewall::Unknown,
    }
}

fn ufw_enabled(conf: &str) -> bool {
    conf.lines()
        .any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes"))
}

/// Looks for an allow rule for `port` in ufw's `user.rules` tuple comments, e.g.
/// `### tuple ### allow tcp 5900 0.0.0.0/0 any 192.168.1.0/24 in`.
fn ufw_allows(rules: &str, port: u16) -> bool {
    let port = port.to_string();
    rules.lines().any(|l| {
        let mut w = l.split_whitespace();
        if w.next() != Some("###") || w.next() != Some("tuple") || w.next() != Some("###") {
            return false;
        }
        let (action, proto, dport) = (w.next(), w.next(), w.next());
        action == Some("allow") && matches!(proto, Some("tcp" | "any")) && dport == Some(&port)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, focused: bool, disabled: bool) -> OutputInfo {
        OutputInfo {
            name: name.into(),
            width: 1920,
            height: 1080,
            refresh_hz: 60.0,
            scale: 1.0,
            x: 0,
            y: 0,
            focused,
            disabled,
        }
    }

    #[test]
    fn defaults_to_the_only_enabled_monitor() {
        let outs = [out("DP-1", false, false), out("HDMI-A-1", true, true)];
        assert_eq!(default_output(&outs).unwrap(), "DP-1");
        let err = default_output(&[out("DP-1", false, false), out("DP-2", true, false)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("DP-1, DP-2"), "{err}");
        assert!(default_output(&[]).is_err());
    }

    #[test]
    fn sets_output_in_an_empty_file() {
        assert_eq!(
            set_top_level_key("", "output", "DP-1"),
            "output = \"DP-1\"\n"
        );
    }

    #[test]
    fn replaces_output_and_keeps_the_rest() {
        let text = "# mine\noutput = \"DP-1\"\nname = \"studio\"\n\n[input]\noutput = 1\n";
        assert_eq!(
            set_top_level_key(text, "output", "DP-2"),
            "# mine\noutput = \"DP-2\"\nname = \"studio\"\n\n[input]\noutput = 1\n"
        );
    }

    #[test]
    fn adds_output_before_the_first_table() {
        let text = "name = \"studio\"\n# outputs = 2\n[hp]\nenabled = false\n";
        let new = set_top_level_key(text, "output", "DP-1");
        assert_eq!(
            new,
            "name = \"studio\"\n# outputs = 2\noutput = \"DP-1\"\n[hp]\nenabled = false\n"
        );
        let s = Settings::parse(&new).unwrap();
        assert_eq!(s.output.as_deref(), Some("DP-1"));
        assert!(!s.hp.enabled);
    }

    const IP_ADDR: &str = "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever\n\
        2: enp1s0    inet 192.168.1.118/24 brd 192.168.1.255 scope global enp1s0\n\
        3: tailscale0    inet 100.101.2.3/32 scope global tailscale0\n";

    #[test]
    fn parses_interface_addresses() {
        let addrs = parse_addrs(IP_ADDR);
        let names: Vec<_> = addrs.iter().map(|a| (a.name.as_str(), a.prefix)).collect();
        assert_eq!(names, [("lo", 8), ("enp1s0", 24), ("tailscale0", 32)]);
        assert_eq!(addrs[1].ip, Ipv4Addr::new(192, 168, 1, 118));
        assert!(parse_addrs("garbage\n").is_empty());
    }

    #[test]
    fn hints_a_ufw_rule() {
        let addrs = parse_addrs(IP_ADDR);
        assert_eq!(
            ufw_hint(5900, None, Some(addrs[1].ip), &addrs),
            "sudo ufw allow from 192.168.1.0/24 to any port 5900 proto tcp"
        );
        assert_eq!(
            ufw_hint(5900, Some(&addrs[2]), Some(addrs[2].ip), &addrs),
            "sudo ufw allow in on tailscale0 to any port 5900 proto tcp"
        );
    }

    #[test]
    fn finds_the_subnet() {
        let ip = Ipv4Addr::new(192, 168, 1, 118);
        assert_eq!(subnet(ip, 24), "192.168.1.0/24");
        assert_eq!(subnet("10.1.2.3".parse().unwrap(), 12), "10.0.0.0/12");
        assert_eq!(subnet(ip, 0), "0.0.0.0/0");
    }

    #[test]
    fn reads_ufw_state() {
        assert!(ufw_enabled("# comment\nENABLED=yes\nLOGLEVEL=low\n"));
        assert!(!ufw_enabled("ENABLED=no\n"));
        let rules = "### tuple ### allow tcp 22 0.0.0.0/0 any 0.0.0.0/0 in\n\
                     -A ufw-user-input -p tcp --dport 22 -j ACCEPT\n\
                     ### tuple ### allow tcp 5900 0.0.0.0/0 any 192.168.1.0/24 in\n";
        assert!(ufw_allows(rules, 5900));
        assert!(!ufw_allows(rules, 5901));
        assert!(!ufw_allows(
            "### tuple ### deny tcp 5900 0.0.0.0/0 any 0.0.0.0/0 in\n",
            5900
        ));
        assert!(ufw_allows(
            "### tuple ### allow any 5900 0.0.0.0/0 any 0.0.0.0/0 in\n",
            5900
        ));
    }
}
