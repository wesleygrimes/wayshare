//! The systemd user unit written by `wayshare install-service`, for installs without a package.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const TEMPLATE: &str = include_str!("../contrib/wayshare.service.in");

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
    fn renders_exec_start() {
        let unit = render(Path::new("/home/me/.local/bin/wayshare"));
        assert!(unit.contains("\nExecStart=/home/me/.local/bin/wayshare\n"));
        assert!(!unit.contains('@'));
    }
}
