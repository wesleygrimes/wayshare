//! The systemd user unit written by `wayshare install-service`, for installs without a package,
//! and the commands that drive it.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

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

/// Runs `systemctl --user <args>`.
pub fn systemctl(args: &[&str]) -> Result<()> {
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
    match install_kind(&exe) {
        InstallKind::Mise => println!(
            "Installed with mise. Run `mise up github:wesleygrimes/wayshare`, then `wayshare restart`."
        ),
        InstallKind::Cargo => println!(
            "Installed with cargo. Run `cargo install --locked --path .` in your checkout, then \
             `wayshare restart`."
        ),
        InstallKind::System => println!(
            "Installed by your package manager. Upgrade it there, then run `wayshare restart`."
        ),
        InstallKind::Script => {
            let dir = exe.parent().context("binary has no parent directory")?;
            let status = Command::new("sh")
                .arg("-c")
                .arg(format!("curl -fsSL --proto '=https' {INSTALL_SCRIPT} | sh"))
                .env("WAYSHARE_INSTALL_DIR", dir)
                .status()
                .context("running the installer")?;
            if !status.success() {
                bail!("the installer failed");
            }
            systemctl(&["try-restart", "wayshare"])?;
        }
    }
    Ok(())
}

/// Stops the service and removes the unit and, for install.sh installs, the binary. Keeps the
/// config directory.
pub fn uninstall() -> Result<()> {
    if !crate::setup::ask_yes_no("Remove wayshare?")? {
        return Ok(());
    }
    let exe = std::env::current_exe().context("finding the wayshare binary")?;
    let _ = systemctl(&["disable", "--now", "wayshare"]);
    let unit = unit_dir()?.join("wayshare.service");
    if unit.exists() {
        std::fs::remove_file(&unit).with_context(|| format!("removing {}", unit.display()))?;
        println!("Removed {}", unit.display());
        let _ = systemctl(&["daemon-reload"]);
    }
    match install_kind(&exe) {
        InstallKind::Script => {
            std::fs::remove_file(&exe).with_context(|| format!("removing {}", exe.display()))?;
            println!("Removed {}", exe.display());
        }
        InstallKind::Mise => {
            println!("Run `mise uninstall github:wesleygrimes/wayshare` to finish.")
        }
        InstallKind::Cargo => println!("Run `cargo uninstall wayshare` to finish."),
        InstallKind::System => println!("Remove the wayshare package to finish."),
    }
    println!(
        "Your password and settings are still in {}; delete it to remove them.",
        crate::config::config_dir()?.display()
    );
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
