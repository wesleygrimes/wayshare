//! The systemd user unit that `wayshare setup` writes for installs without a package, and the
//! commands that drive it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use demand::Confirm;

use crate::ui;

const TEMPLATE: &str = include_str!("../contrib/wayshare.service.in");
const INSTALL_SCRIPT: &str =
    "https://raw.githubusercontent.com/wesleygrimes/wayshare/main/install.sh";

/// Under mise the binary's path changes with every version, so the unit runs the shim instead.
pub fn exec_path(exe: &Path) -> PathBuf {
    for dir in exe.ancestors().filter(|d| d.ends_with("installs")) {
        let shim = dir.with_file_name("shims").join("wayshare");
        if shim.exists() {
            return shim;
        }
    }
    exe.to_path_buf()
}

pub fn render(exec: &Path) -> String {
    TEMPLATE.replace("@bindir@/wayshare", &exec.display().to_string())
}

pub fn unit_dir() -> Result<PathBuf> {
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join(".local/share"),
    };
    Ok(data.join("systemd/user"))
}

/// Writes the unit and returns its path.
pub fn install() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding the wayshare binary")?;
    let dir = unit_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("wayshare.service");
    std::fs::write(&path, render(&exec_path(&exe)))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Runs `systemctl --user <args>` quietly; its stderr becomes the error message.
pub fn systemctl(args: &[&str]) -> Result<()> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("running systemctl")?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        bail!("systemctl --user {} failed: {}", args.join(" "), why.trim());
    }
    Ok(())
}

/// A property of the wayshare unit from `systemctl --user show`, or "" if systemctl fails.
pub fn unit_property(name: &str) -> String {
    Command::new("systemctl")
        .args(["--user", "show", "-p", name, "--value", "wayshare"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `wayshare start`, `stop` and `restart`.
pub fn control(verb: &str) -> Result<()> {
    let (doing, done) = match verb {
        "start" => ("Starting", "started"),
        "stop" => ("Stopping", "stopped"),
        _ => ("Restarting", "restarted"),
    };
    ui::spin(&format!("{doing} wayshare…"), || {
        systemctl(&[verb, "wayshare"])
    })?;
    ui::done("wayshare", done);
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum InstallKind {
    Script,
    Mise,
    Cargo,
    System,
}

pub fn install_kind(exe: &Path) -> InstallKind {
    if exec_path(exe) != exe {
        InstallKind::Mise
    } else if exe.starts_with("/usr") || exe.starts_with("/opt") {
        InstallKind::System
    } else if exe.parent().is_some_and(|d| d.ends_with(".cargo/bin")) {
        InstallKind::Cargo
    } else {
        InstallKind::Script
    }
}

/// Re-runs install.sh into the current binary's directory, or says how to upgrade otherwise.
pub fn upgrade() -> Result<()> {
    let exe = std::env::current_exe().context("finding the wayshare binary")?;
    let how = match install_kind(&exe) {
        InstallKind::Mise => "mise up github:wesleygrimes/wayshare",
        InstallKind::Cargo => "cargo install --locked --path .",
        InstallKind::System => "your package manager",
        InstallKind::Script => {
            let dir = exe.parent().context("binary has no parent directory")?;
            let old = env!("CARGO_PKG_VERSION");
            ui::spin("Downloading the latest release…", || -> Result<()> {
                let script = Command::new("curl")
                    .args(["-fsSL", "--proto", "=https", INSTALL_SCRIPT])
                    .output()
                    .context("running curl")?;
                if !script.status.success() {
                    let why = String::from_utf8_lossy(&script.stderr);
                    bail!("downloading the installer failed: {}", why.trim());
                }
                let mut sh = Command::new("sh")
                    .env("WAYSHARE_INSTALL_DIR", dir)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("running the installer")?;
                sh.stdin.take().unwrap().write_all(&script.stdout)?;
                let out = sh.wait_with_output()?;
                if !out.status.success() {
                    let why = String::from_utf8_lossy(&out.stderr);
                    bail!("the installer failed: {}", why.trim());
                }
                systemctl(&["try-restart", "wayshare"])
            })?;
            let new = Command::new(&exe)
                .arg("--version")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|v| v.split_whitespace().nth(1).map(str::to_string))
                .unwrap_or_default();
            if new == old {
                ui::done("wayshare", &format!("{old} is the latest release"));
            } else {
                ui::done("wayshare", &format!("upgraded {old} → {new} and restarted"));
            }
            return Ok(());
        }
    };
    ui::warn("wayshare", "isn't managed by install.sh");
    ui::hint(&format!("upgrade with {how}, then run `wayshare restart`"));
    Ok(())
}

/// Stops the service and removes the unit and, for install.sh installs, the binary. Keeps the
/// config directory.
pub fn uninstall() -> Result<()> {
    let config = crate::config::config_dir()?;
    let remove = Confirm::new("Remove wayshare?")
        .description(&format!(
            "Stops the service and removes wayshare. Your settings stay in {}.",
            ui::path(&config)
        ))
        .affirmative("Remove")
        .negative("Cancel")
        .selected(false)
        .theme(crate::ui::theme())
        .run()?;
    if !remove {
        return Ok(());
    }
    let exe = std::env::current_exe().context("finding the wayshare binary")?;
    if let Err(e) = systemctl(&["disable", "--now", "wayshare"])
        && unit_property("LoadState") == "loaded"
    {
        return Err(e.context("couldn't stop the service, so nothing was removed"));
    }
    ui::done("Service", "stopped and disabled");
    let unit = unit_dir()?.join("wayshare.service");
    if unit.exists() {
        std::fs::remove_file(&unit).with_context(|| format!("removing {}", unit.display()))?;
        let _ = systemctl(&["daemon-reload"]);
        ui::done("Unit", &format!("removed {}", ui::path(&unit)));
    }
    let finish = match install_kind(&exe) {
        InstallKind::Script => {
            std::fs::remove_file(&exe).with_context(|| format!("removing {}", exe.display()))?;
            ui::done("Binary", &format!("removed {}", ui::path(&exe)));
            None
        }
        InstallKind::Mise => Some("mise uninstall github:wesleygrimes/wayshare"),
        InstallKind::Cargo => Some("cargo uninstall wayshare"),
        InstallKind::System => Some("remove the wayshare package"),
    };
    if let Some(how) = finish {
        ui::warn("Binary", "still installed");
        ui::hint(&format!("to finish: {how}"));
    }
    ui::line(&ui::dim(&format!(
        "Your password and settings are still in {}.",
        ui::path(&config)
    )));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_the_mise_shim_when_there_is_one() {
        let root = std::env::temp_dir().join(format!("wayshare-service-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let exe = root.join("installs/github-wesleygrimes-wayshare/0.1.0/wayshare");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(exec_path(&exe), exe);
        std::fs::create_dir_all(root.join("shims")).unwrap();
        std::fs::write(root.join("shims/wayshare"), b"").unwrap();
        assert_eq!(exec_path(&exe), root.join("shims/wayshare"));
        assert_eq!(
            exec_path(Path::new("/usr/bin/wayshare")),
            Path::new("/usr/bin/wayshare")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tells_install_kinds_apart() {
        let kind = |p: &str| install_kind(Path::new(p));
        assert_eq!(kind("/home/me/.local/bin/wayshare"), InstallKind::Script);
        assert_eq!(kind("/home/me/bin/wayshare"), InstallKind::Script);
        assert_eq!(kind("/home/me/.cargo/bin/wayshare"), InstallKind::Cargo);
        assert_eq!(kind("/usr/bin/wayshare"), InstallKind::System);
    }

    #[test]
    fn renders_exec_start() {
        let unit = render(Path::new("/home/me/.local/bin/wayshare"));
        assert!(unit.contains("\nExecStart=/home/me/.local/bin/wayshare\n"));
        assert!(!unit.contains('@'));
    }
}
