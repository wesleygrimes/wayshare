//! Output sizing so the shared desktop matches the client's window and Retina scale.

pub mod apple;
pub mod hyprland;

use anyhow::Result;

/// Connects to `$WAYLAND_DISPLAY`, or to Hyprland's socket when a service starts without it.
#[cfg(target_os = "linux")]
pub fn connect_wayland() -> Result<wayland_client::Connection> {
    use anyhow::Context;
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return Ok(wayland_client::Connection::connect_to_env()?);
    }
    let path = hyprland::wayland_socket()?;
    let stream = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connect {}", path.display()))?;
    Ok(wayland_client::Connection::from_socket(stream)?)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub scale: f64,
    pub x: i32,
    pub y: i32,
    pub focused: bool,
    pub disabled: bool,
}

impl OutputInfo {
    pub fn mode(&self) -> DisplayMode {
        DisplayMode {
            width: self.width,
            height: self.height,
            refresh_hz: self.refresh_hz,
            scale: self.scale,
        }
    }
}

pub trait DisplayManager: Send {
    fn outputs(&mut self) -> Result<Vec<OutputInfo>>;
    /// Applies `mode` (pixel size) to `output` and returns what the compositor actually set.
    fn set_mode(&mut self, output: &str, mode: &DisplayMode) -> Result<OutputInfo>;
    fn create_headless(&mut self, name: &str) -> Result<()>;
    fn remove_output(&mut self, name: &str) -> Result<()>;
}

/// Rounds a requested pixel size down so `size / scale` is a whole number of logical pixels;
/// compositors reject (Hyprland silently replaces) scales that don't divide the mode.
/// `scale` is snapped to 1/120 steps (wp-fractional-scale).
pub fn fit_mode(width: u32, height: u32, refresh_hz: f64, scale: f64) -> DisplayMode {
    let n = ((scale * 120.0).round() as u32).max(1);
    let step = n / gcd(n, 120);
    let fit = |v: u32| (v / step * step).max(step);
    DisplayMode {
        width: fit(width),
        height: fit(height),
        refresh_hz,
        scale: n as f64 / 120.0,
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_mode_rounds_to_scale() {
        assert_eq!(
            fit_mode(3456, 2160, 60.0, 2.0),
            DisplayMode {
                width: 3456,
                height: 2160,
                refresh_hz: 60.0,
                scale: 2.0
            }
        );
        let m = fit_mode(2001, 1333, 60.0, 2.0);
        assert_eq!((m.width, m.height), (2000, 1332));
        let m = fit_mode(2001, 1333, 60.0, 1.5);
        assert_eq!((m.width, m.height), (2001, 1332));
        let m = fit_mode(1001, 999, 60.0, 1.25);
        assert_eq!((m.width, m.height), (1000, 995));
        let m = fit_mode(1001, 999, 60.0, 1.0);
        assert_eq!((m.width, m.height, m.scale), (1001, 999, 1.0));
        assert_eq!(fit_mode(1000, 1000, 60.0, 1.7).scale, 1.7);
    }
}
