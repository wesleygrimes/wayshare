//! Hyprland IPC (`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;
use tracing::{debug, warn};

use super::exclusive::{DesktopManager, DesktopSnapshot, WorkspaceInfo};
use super::{DisplayManager, DisplayMode, OutputInfo};

const MIN_VERSION: (u64, u64) = (0, 56);

pub struct Hyprland {
    socket: PathBuf,
}

impl Hyprland {
    pub fn from_env() -> Result<Self> {
        Ok(Self::with_socket(instance_dir()?.join(".socket.sock")))
    }

    pub fn with_socket(socket: PathBuf) -> Self {
        Self { socket }
    }

    pub fn request(&self, cmd: &str) -> Result<String> {
        let mut s = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect {}", self.socket.display()))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.write_all(cmd.as_bytes())?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        debug!(cmd, reply = out.trim(), "hyprctl");
        Ok(out)
    }

    pub fn check_version(&self) -> Result<()> {
        let version = parse_version(&self.request("j/version")?)?;
        ensure!(
            supported(&version),
            "wayshare needs Hyprland {}.{} or newer, found {version}",
            MIN_VERSION.0,
            MIN_VERSION.1
        );
        Ok(())
    }

    fn ok(&self, cmd: &str) -> Result<()> {
        let r = self.request(cmd)?;
        ensure!(r.trim() == "ok", "hyprland `{cmd}`: {}", r.trim());
        Ok(())
    }

    fn apply_rule(&self, output: &str, mode: &DisplayMode, position: &str) -> Result<()> {
        let m = format!(
            "{}x{}@{}",
            mode.width,
            mode.height,
            fmt_num(mode.refresh_hz)
        );
        self.ok(&lua_monitor_rule(
            output,
            &m,
            position,
            &fmt_num(mode.scale),
        ))
    }

    fn workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        parse_workspaces(&self.request("j/workspaces")?)
    }

    fn wait_outputs(&mut self, expected: &[OutputInfo]) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let current = self.outputs()?;
            if expected.iter().all(|saved| {
                current
                    .iter()
                    .find(|o| o.name == saved.name)
                    .is_none_or(|now| {
                        !now.disabled
                            && now.width == saved.width
                            && now.height == saved.height
                            && now.x == saved.x
                            && now.y == saved.y
                            && (now.scale - saved.scale).abs() < 1e-3
                            && now.transform == saved.transform
                    })
            }) {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "monitor restoration timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn runtime_dir() -> Result<PathBuf> {
    Ok(std::env::var_os("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR not set")?
        .into())
}

fn instance_dir() -> Result<PathBuf> {
    let hypr = runtime_dir()?.join("hypr");
    match std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        Ok(sig) if !sig.is_empty() => Ok(hypr.join(sig)),
        _ => newest_live_instance(&hypr),
    }
}

/// The Wayland socket of the Hyprland instance, for when `$WAYLAND_DISPLAY` is unset.
pub fn wayland_socket() -> Result<PathBuf> {
    let lock = instance_dir()?.join("hyprland.lock");
    let text =
        std::fs::read_to_string(&lock).with_context(|| format!("read {}", lock.display()))?;
    let name = lock_wayland_display(&text)
        .ok_or_else(|| anyhow!("no Wayland socket in {}", lock.display()))?;
    Ok(runtime_dir()?.join(name))
}

/// `hyprland.lock` holds the compositor's pid, then its Wayland socket name.
fn lock_wayland_display(lock: &str) -> Option<&str> {
    lock.lines().nth(1).map(str::trim).filter(|s| !s.is_empty())
}

fn newest_live_instance(hypr: &Path) -> Result<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(hypr).with_context(|| format!("read {}", hypr.display()))? {
        let dir = e?.path();
        let sock = dir.join(".socket.sock");
        let Ok(meta) = std::fs::metadata(&sock) else {
            continue;
        };
        if UnixStream::connect(&sock).is_err() {
            continue;
        }
        let t = meta.modified()?;
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, dir));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| anyhow!("no running Hyprland instance under {}", hypr.display()))
}

fn check_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "invalid output name {name:?}"
    );
    Ok(())
}

fn fmt_num(v: f64) -> String {
    let s = format!("{v:.5}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn lua_monitor_rule(output: &str, mode: &str, position: &str, scale: &str) -> String {
    format!(
        "eval hl.monitor({{ output = \"{output}\", mode = \"{mode}\", position = \"{position}\", scale = {scale} }})"
    )
}

// JSON string escapes are not all valid Lua; quote workspace names as Lua literals.
fn lua_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let mut bytes = [0; 4];
                for byte in c.encode_utf8(&mut bytes).bytes() {
                    out.push_str(&format!("\\{byte:03}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn restore_output_command(output: &OutputInfo) -> Result<String> {
    check_name(&output.name)?;
    Ok(format!(
        "eval hl.monitor({{ output = {}, disabled = false, mode = \"{}x{}@{}\", position = \"{}x{}\", scale = {}, transform = {} }})",
        lua_string(&output.name),
        output.width,
        output.height,
        fmt_num(output.refresh_hz),
        output.x,
        output.y,
        fmt_num(output.scale),
        output.transform,
    ))
}

fn parse_workspaces(json: &str) -> Result<Vec<WorkspaceInfo>> {
    let value: Value = serde_json::from_str(json).context("workspaces JSON")?;
    value
        .as_array()
        .context("workspaces: not an array")?
        .iter()
        .map(|w| {
            Ok(WorkspaceInfo {
                id: w["id"].as_i64().context("workspace: missing id")?,
                name: w["name"]
                    .as_str()
                    .context("workspace: missing name")?
                    .into(),
                monitor: w["monitor"]
                    .as_str()
                    .context("workspace: missing monitor")?
                    .into(),
            })
        })
        .collect()
}

fn parse_version(json: &str) -> Result<String> {
    let v: Value = serde_json::from_str(json).context("version JSON")?;
    Ok(v["version"]
        .as_str()
        .ok_or_else(|| anyhow!("version: missing version"))?
        .to_string())
}

fn supported(version: &str) -> bool {
    let mut parts = version.split('.').map(str::parse::<u64>);
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= MIN_VERSION,
        _ => false,
    }
}

pub fn parse_monitors(json: &str) -> Result<Vec<OutputInfo>> {
    let v: Value = serde_json::from_str(json).context("monitors JSON")?;
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("monitors: not an array"))?;
    arr.iter()
        .map(|m| {
            let u = |k: &str| {
                m[k].as_u64()
                    .ok_or_else(|| anyhow!("monitors: missing {k}"))
            };
            let i = |k: &str| {
                m[k].as_i64()
                    .ok_or_else(|| anyhow!("monitors: missing {k}"))
            };
            let f = |k: &str| {
                m[k].as_f64()
                    .ok_or_else(|| anyhow!("monitors: missing {k}"))
            };
            Ok(OutputInfo {
                name: m["name"]
                    .as_str()
                    .ok_or_else(|| anyhow!("monitors: missing name"))?
                    .to_string(),
                width: u("width")? as u32,
                height: u("height")? as u32,
                refresh_hz: f("refreshRate")?,
                scale: f("scale")?,
                x: i("x")? as i32,
                y: i("y")? as i32,
                focused: m["focused"].as_bool().unwrap_or(false),
                disabled: m["disabled"].as_bool().unwrap_or(false),
                transform: m["transform"].as_u64().unwrap_or(0) as u32,
                active_workspace: m["activeWorkspace"]["id"].as_i64(),
            })
        })
        .collect()
}

impl DisplayManager for Hyprland {
    fn outputs(&mut self) -> Result<Vec<OutputInfo>> {
        parse_monitors(&self.request("j/monitors all")?)
    }

    fn set_mode(&mut self, output: &str, mode: &DisplayMode) -> Result<OutputInfo> {
        check_name(output)?;
        let cur = self
            .outputs()?
            .into_iter()
            .find(|o| o.name == output)
            .ok_or_else(|| anyhow!("no output {output}"))?;
        self.apply_rule(output, mode, &format!("{}x{}", cur.x, cur.y))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let now = self.outputs()?.into_iter().find(|o| o.name == output);
            if let Some(o) = &now
                && o.width == mode.width
                && o.height == mode.height
            {
                if (o.scale - mode.scale).abs() > 1e-3 {
                    warn!(
                        output,
                        requested = mode.scale,
                        actual = o.scale,
                        "compositor changed scale"
                    );
                }
                return Ok(o.clone());
            }
            if Instant::now() > deadline {
                bail!(
                    "{output} did not switch to {}x{} (now {:?})",
                    mode.width,
                    mode.height,
                    now.map(|o| o.mode())
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn create_headless(&mut self, name: &str) -> Result<()> {
        check_name(name)?;
        self.ok(&format!("output create headless {name}"))
    }

    fn remove_output(&mut self, name: &str) -> Result<()> {
        check_name(name)?;
        self.ok(&format!("output remove {name}"))
    }
}

impl DesktopManager for Hyprland {
    fn snapshot(&mut self) -> Result<DesktopSnapshot> {
        Ok(DesktopSnapshot {
            outputs: self.outputs()?,
            workspaces: self.workspaces()?,
        })
    }

    fn disable_output(&mut self, output: &str) -> Result<()> {
        check_name(output)?;
        self.ok(&format!(
            "eval hl.monitor({{ output = {}, disabled = true }})",
            lua_string(output)
        ))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if self
                .outputs()?
                .iter()
                .find(|o| o.name == output)
                .is_none_or(|o| o.disabled)
            {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "disabling {output} timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn restore_desktop(&mut self, snapshot: &DesktopSnapshot) -> Result<()> {
        let current = self.outputs()?;
        let mut errors = Vec::new();
        let mut expected = Vec::new();
        for output in snapshot.outputs.iter().filter(|o| !o.disabled) {
            // Rules survive unplugging. Clear disabled=true even for absent outputs,
            // without waiting for them or creating an output, so replugging works.
            match restore_output_command(output).and_then(|cmd| self.ok(&cmd)) {
                Ok(()) => {
                    if current.iter().any(|o| o.name == output.name) {
                        expected.push(output.clone());
                    }
                }
                Err(e) => errors.push(format!("restoring {}: {e:#}", output.name)),
            }
        }
        if let Err(e) = self.wait_outputs(&expected) {
            errors.push(format!("{e:#}"));
        }
        let active = self.outputs()?;
        let workspaces = self.workspaces()?;
        let available = |name: &str| active.iter().any(|o| o.name == name && !o.disabled);
        let exists = |id| workspaces.iter().any(|w| w.id == id);
        // Re-enabling monitors alone leaves their workspaces on the shared output.
        for workspace in &snapshot.workspaces {
            if !available(&workspace.monitor) || !exists(workspace.id) {
                continue;
            }
            let command = format!(
                "eval hl.dispatch(hl.dsp.workspace.move({{ workspace = {}, monitor = {} }}))",
                lua_string(&workspace.selector()),
                lua_string(&workspace.monitor),
            );
            if let Err(e) = self.ok(&command) {
                errors.push(format!("restoring workspace {}: {e:#}", workspace.id));
            }
        }
        for output in snapshot
            .outputs
            .iter()
            .filter(|o| !o.disabled && available(&o.name))
        {
            let Some(workspace) = output
                .active_workspace
                .filter(|&id| exists(id))
                .and_then(|id| snapshot.workspaces.iter().find(|w| w.id == id))
            else {
                continue;
            };
            let command = format!(
                "eval hl.dispatch(hl.dsp.focus({{ workspace = {} }}))",
                lua_string(&workspace.selector())
            );
            if let Err(e) = self.ok(&command) {
                errors.push(format!("restoring active workspace: {e:#}"));
            }
        }
        if let Some(output) = snapshot
            .outputs
            .iter()
            .find(|o| o.focused && available(&o.name))
        {
            let command = format!(
                "eval hl.dispatch(hl.dsp.focus({{ monitor = {} }}))",
                lua_string(&output.name)
            );
            if let Err(e) = self.ok(&command) {
                errors.push(format!("restoring focus: {e:#}"));
            }
        }
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = r#"[{
        "id": 0, "name": "HEADLESS-1", "description": "", "width": 3456, "height": 2160,
        "refreshRate": 60.00000, "x": 0, "y": 0, "scale": 2, "focused": true, "disabled": false,
        "availableModes": ["1920x1080@60.00Hz"]
    },{
        "id": 1, "name": "ARDTEST-1", "width": 2001, "height": 1333, "refreshRate": 30.0,
        "x": 1728, "y": 0, "scale": 1, "focused": false, "disabled": false
    }]"#;

    #[test]
    fn parses_monitors() {
        let m = parse_monitors(MONITORS).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[0].mode(),
            DisplayMode {
                width: 3456,
                height: 2160,
                refresh_hz: 60.0,
                scale: 2.0
            }
        );
        assert!(m[0].focused);
        assert_eq!((m[1].name.as_str(), m[1].x), ("ARDTEST-1", 1728));
        assert!(parse_monitors("{}").is_err());
    }

    #[test]
    fn commands() {
        assert_eq!(fmt_num(60.0), "60");
        assert_eq!(fmt_num(59.94), "59.94");
        assert_eq!(fmt_num(1.25), "1.25");
        assert_eq!(
            lua_monitor_rule("ARDTEST-1", "2940x1912@60", "0x0", "2"),
            r#"eval hl.monitor({ output = "ARDTEST-1", mode = "2940x1912@60", position = "0x0", scale = 2 })"#
        );
        assert!(check_name("HEADLESS-1").is_ok());
        assert!(check_name("x\" }) os.execute(\"").is_err());
        assert_eq!(lua_string("a\"\\\n\u{1}9"), "\"a\\\"\\\\\\010\\0019\"");
        assert_eq!(lua_string("日本語"), "\"日本語\"");
    }

    fn monitors_json(outputs: &[OutputInfo]) -> String {
        serde_json::to_string(
            &outputs
                .iter()
                .map(|o| {
                    serde_json::json!({
                        "name": o.name, "width": o.width, "height": o.height,
                        "refreshRate": o.refresh_hz, "scale": o.scale, "x": o.x, "y": o.y,
                        "focused": o.focused, "disabled": o.disabled, "transform": o.transform,
                        "activeWorkspace": { "id": o.active_workspace },
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    fn fake_ipc(script: Vec<(String, String)>) -> (Hyprland, std::thread::JoinHandle<()>) {
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "wayshare-desktop-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let thread = std::thread::spawn(move || {
            for (command, reply) in script {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut client = loop {
                    match listener.accept() {
                        Ok((client, _)) => break client,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing IPC command: {command}");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(e) => panic!("accept: {e}"),
                    }
                };
                // macOS inherits the listener's nonblocking mode on accepted sockets.
                client.set_nonblocking(false).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = [0u8; 4096];
                let n = client.read(&mut buf).unwrap();
                assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), command);
                client.write_all(reply.as_bytes()).unwrap();
            }
            std::fs::remove_dir_all(dir).unwrap();
        });
        (Hyprland::with_socket(path), thread)
    }

    #[test]
    fn fake_ipc_waits_for_request_bytes() {
        let (hypr, thread) = fake_ipc(vec![("j/version".into(), "ok".into())]);
        let mut client = UnixStream::connect(&hypr.socket).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let err = client.read(&mut [0]).unwrap_err();
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(b"j/version").unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "ok");
        thread.join().unwrap();
    }

    fn saved_desktop() -> DesktopSnapshot {
        let mut outputs = parse_monitors(MONITORS).unwrap();
        outputs[0].active_workspace = Some(1);
        outputs[0].transform = 1;
        outputs[1].active_workspace = Some(2);
        DesktopSnapshot {
            workspaces: outputs
                .iter()
                .enumerate()
                .map(|(i, o)| WorkspaceInfo {
                    id: i as i64 + 1,
                    name: (i + 1).to_string(),
                    monitor: o.name.clone(),
                })
                .collect(),
            outputs,
        }
    }

    #[test]
    fn snapshots_monitor_metadata_and_workspaces() {
        let saved = saved_desktop();
        let (mut hypr, thread) = fake_ipc(vec![
            ("j/monitors all".into(), monitors_json(&saved.outputs)),
            (
                "j/workspaces".into(),
                r#"[{"id":1,"name":"dev","monitor":"HEADLESS-1"}]"#.into(),
            ),
        ]);
        let snapshot = hypr.snapshot().unwrap();
        assert_eq!(snapshot.outputs, saved.outputs);
        assert_eq!(snapshot.workspaces[0].selector(), "1");
        assert_eq!(snapshot.workspaces[0].monitor, "HEADLESS-1");
        thread.join().unwrap();
    }

    #[test]
    fn restore_does_not_convert_active_vrr_into_a_configured_policy() {
        let mut json: Value = serde_json::from_str(MONITORS).unwrap();
        let original = parse_monitors(MONITORS).unwrap();
        for active in [false, true] {
            json[0]["vrr"] = Value::Bool(active);
            let outputs = parse_monitors(&json.to_string()).unwrap();
            assert_eq!(outputs, original);
            let command = restore_output_command(&outputs[0]).unwrap();
            assert_eq!(
                command,
                r#"eval hl.monitor({ output = "HEADLESS-1", disabled = false, mode = "3456x2160@60", position = "0x0", scale = 2, transform = 0 })"#
            );
            assert!(!command.contains("vrr"));
        }
    }

    #[test]
    fn unplugged_outputs_get_enabled_rules_without_waiting_for_replug() {
        let saved = saved_desktop();
        let (mut hypr, thread) = fake_ipc(vec![
            ("j/monitors all".into(), "[]".into()),
            (
                restore_output_command(&saved.outputs[0]).unwrap(),
                "ok".into(),
            ),
            (
                restore_output_command(&saved.outputs[1]).unwrap(),
                "ok".into(),
            ),
            ("j/monitors all".into(), "[]".into()),
            ("j/monitors all".into(), "[]".into()),
            ("j/workspaces".into(), "[]".into()),
        ]);
        hypr.restore_desktop(&saved).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn disable_waits_for_the_output_to_be_disabled() {
        let mut saved = saved_desktop();
        saved.outputs[0].disabled = true;
        let (mut hypr, thread) = fake_ipc(vec![
            (
                r#"eval hl.monitor({ output = "HEADLESS-1", disabled = true })"#.into(),
                "ok".into(),
            ),
            ("j/monitors all".into(), monitors_json(&saved.outputs)),
        ]);
        hypr.disable_output("HEADLESS-1").unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn restore_outputs_before_workspaces_and_focus() {
        let mut saved = saved_desktop();
        let ready = monitors_json(&saved.outputs);
        let mut disabled = saved.outputs.clone();
        disabled[0].disabled = true;
        let mut unplugged = saved.outputs[0].clone();
        unplugged.name = "DP-gone".into();
        saved.outputs.push(unplugged);
        saved.workspaces.push(WorkspaceInfo {
            id: 3,
            name: "3".into(),
            monitor: "DP-gone".into(),
        });
        saved.workspaces.push(WorkspaceInfo {
            id: 4,
            name: "4".into(),
            monitor: "HEADLESS-1".into(),
        });
        let (mut hypr, thread) = fake_ipc(vec![
            ("j/monitors all".into(), monitors_json(&disabled)),
            (restore_output_command(&saved.outputs[0]).unwrap(), "ok".into()),
            (restore_output_command(&saved.outputs[1]).unwrap(), "ok".into()),
            (restore_output_command(&saved.outputs[2]).unwrap(), "ok".into()),
            ("j/monitors all".into(), ready.clone()),
            ("j/monitors all".into(), ready),
            ("j/workspaces".into(), r#"[{"id":1,"name":"1","monitor":"ARDTEST-1"},{"id":2,"name":"2","monitor":"ARDTEST-1"},{"id":3,"name":"3","monitor":"ARDTEST-1"},{"id":5,"name":"new","monitor":"ARDTEST-1"}]"#.into()),
            (r#"eval hl.dispatch(hl.dsp.workspace.move({ workspace = "1", monitor = "HEADLESS-1" }))"#.into(), "ok".into()),
            (r#"eval hl.dispatch(hl.dsp.workspace.move({ workspace = "2", monitor = "ARDTEST-1" }))"#.into(), "ok".into()),
            (r#"eval hl.dispatch(hl.dsp.focus({ workspace = "1" }))"#.into(), "ok".into()),
            (r#"eval hl.dispatch(hl.dsp.focus({ workspace = "2" }))"#.into(), "ok".into()),
            (r#"eval hl.dispatch(hl.dsp.focus({ monitor = "HEADLESS-1" }))"#.into(), "ok".into()),
        ]);
        hypr.restore_desktop(&saved).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn restore_continues_after_an_output_error() {
        let saved = saved_desktop();
        let mut disabled = saved.outputs.clone();
        disabled[0].disabled = true;
        let ready = monitors_json(&disabled);
        let (mut hypr, thread) = fake_ipc(vec![
            ("j/monitors all".into(), ready.clone()),
            (restore_output_command(&saved.outputs[0]).unwrap(), "error".into()),
            (restore_output_command(&saved.outputs[1]).unwrap(), "ok".into()),
            ("j/monitors all".into(), ready.clone()),
            ("j/monitors all".into(), ready),
            ("j/workspaces".into(), r#"[{"id":1,"name":"1","monitor":"ARDTEST-1"},{"id":2,"name":"2","monitor":"ARDTEST-1"}]"#.into()),
            (r#"eval hl.dispatch(hl.dsp.workspace.move({ workspace = "2", monitor = "ARDTEST-1" }))"#.into(), "ok".into()),
            (r#"eval hl.dispatch(hl.dsp.focus({ workspace = "2" }))"#.into(), "ok".into()),
        ]);
        assert!(hypr.restore_desktop(&saved).is_err());
        thread.join().unwrap();
    }

    #[test]
    fn version_check() {
        let json = r#"{"branch": "v0.56.2", "version": "0.56.2", "tag": "v0.56.2"}"#;
        assert_eq!(parse_version(json).unwrap(), "0.56.2");
        assert!(parse_version(r#"{"tag": "v0.56.2"}"#).is_err());
        assert!(parse_version("not json").is_err());
        assert!(supported("0.56.2"));
        assert!(supported("0.56"));
        assert!(supported("1.0.0"));
        assert!(!supported("0.55.9"));
        assert!(!supported("0.9.0"));
        assert!(!supported("garbage"));
    }

    #[test]
    fn lock_file() {
        assert_eq!(lock_wayland_display("974\nwayland-1\n"), Some("wayland-1"));
        assert_eq!(lock_wayland_display("974\n"), None);
        assert_eq!(lock_wayland_display(""), None);
    }

    #[test]
    fn talks_to_a_fake_socket() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("wayshare-hypr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".socket.sock");
        let _ = std::fs::remove_file(&path);
        let l = UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = c.read(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"j/monitors all");
            c.write_all(MONITORS.as_bytes()).unwrap();
        });
        let mut h = Hyprland::with_socket(path.clone());
        assert_eq!(h.outputs().unwrap().len(), 2);
        t.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
