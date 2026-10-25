//! Monitor and workspace ownership for opt-in exclusive sharing.

use std::collections::HashSet;

use anyhow::{Context, Result, ensure};

use super::OutputInfo;
use crate::rfb::SessionId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub id: i64,
    pub name: String,
    pub monitor: String,
}

impl WorkspaceInfo {
    pub fn selector(&self) -> String {
        if self.id > 0 {
            self.id.to_string()
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Debug)]
pub struct DesktopSnapshot {
    pub outputs: Vec<OutputInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
}

pub trait DesktopManager {
    fn snapshot(&mut self) -> Result<DesktopSnapshot>;
    fn disable_output(&mut self, output: &str) -> Result<()>;
    /// Restore outputs before moving surviving workspaces and restoring focus.
    fn restore_desktop(&mut self, snapshot: &DesktopSnapshot) -> Result<()>;
}

#[derive(Default)]
pub struct ExclusiveDesktop {
    viewers: HashSet<SessionId>,
    saved: Option<DesktopSnapshot>,
}

impl ExclusiveDesktop {
    pub fn acquire(
        &mut self,
        session: SessionId,
        shared: &str,
        manager: &mut impl DesktopManager,
    ) -> Result<()> {
        if !self.viewers.is_empty() {
            self.viewers.insert(session);
            return Ok(());
        }
        // Never replace a snapshot whose restoration failed on the previous disconnect.
        self.restore(manager)?;
        let mut snapshot = manager.snapshot().context("saving the desktop")?;
        snapshot.outputs.retain(|o| !o.disabled);
        ensure!(
            snapshot
                .outputs
                .iter()
                .any(|o| o.name == shared && o.width > 0 && o.height > 0),
            "shared output {shared:?} is not active"
        );
        let others: Vec<_> = snapshot
            .outputs
            .iter()
            .filter(|o| o.name != shared)
            .map(|o| o.name.clone())
            .collect();
        // Save before the first mutation, including changes that fail after being applied.
        self.saved = Some(snapshot);
        self.viewers.insert(session);
        for output in others {
            if let Err(error) = manager.disable_output(&output) {
                let error = error.context(format!("disabling output {output}"));
                self.release(session);
                return match self.restore(manager) {
                    Ok(()) => Err(error),
                    Err(rollback) => {
                        Err(error.context(format!("desktop rollback also failed: {rollback:#}")))
                    }
                };
            }
        }
        Ok(())
    }

    /// Forget the viewer without compositor access. Returns whether restoration is due.
    pub fn release(&mut self, session: SessionId) -> bool {
        self.viewers.remove(&session);
        self.viewers.is_empty() && self.saved.is_some()
    }

    pub fn restore(&mut self, manager: &mut impl DesktopManager) -> Result<()> {
        if let Some(snapshot) = &self.saved {
            manager
                .restore_desktop(snapshot)
                .context("restoring the desktop")?;
            self.saved = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    struct FakeDesktop {
        state: DesktopSnapshot,
        log: Vec<String>,
        fail_disable: Option<String>,
        fail_restore: bool,
    }

    fn output(name: &str, x: i32) -> OutputInfo {
        OutputInfo {
            name: name.into(),
            width: 3840,
            height: 2160,
            refresh_hz: 60.0,
            scale: 1.6,
            x,
            y: 0,
            focused: x == 0,
            disabled: false,
            transform: 0,
            active_workspace: Some(if x == 0 { 1 } else { 2 }),
            available_modes: Vec::new(),
        }
    }

    fn desktop() -> FakeDesktop {
        let mut disabled = output("DP-3", 4800);
        disabled.disabled = true;
        FakeDesktop {
            state: DesktopSnapshot {
                outputs: vec![output("DP-1", 0), output("DP-2", 2400), disabled],
                workspaces: vec![WorkspaceInfo {
                    id: 1,
                    name: "1".into(),
                    monitor: "DP-1".into(),
                }],
            },
            log: Vec::new(),
            fail_disable: None,
            fail_restore: false,
        }
    }

    impl DesktopManager for FakeDesktop {
        fn snapshot(&mut self) -> Result<DesktopSnapshot> {
            self.log.push("snapshot".into());
            Ok(self.state.clone())
        }

        fn disable_output(&mut self, name: &str) -> Result<()> {
            self.log.push(format!("disable {name}"));
            self.state
                .outputs
                .iter_mut()
                .find(|o| o.name == name)
                .unwrap()
                .disabled = true;
            for workspace in &mut self.state.workspaces {
                if workspace.monitor == name {
                    workspace.monitor = "DP-2".into();
                }
            }
            if self.fail_disable.as_deref() == Some(name) {
                bail!("disable failed after applying");
            }
            Ok(())
        }

        fn restore_desktop(&mut self, snapshot: &DesktopSnapshot) -> Result<()> {
            self.log.push("restore".into());
            if self.fail_restore {
                bail!("restore failed");
            }
            for saved in &snapshot.outputs {
                *self
                    .state
                    .outputs
                    .iter_mut()
                    .find(|o| o.name == saved.name)
                    .unwrap() = saved.clone();
            }
            self.state.workspaces = snapshot.workspaces.clone();
            Ok(())
        }
    }

    fn disconnect(
        exclusive: &mut ExclusiveDesktop,
        session: SessionId,
        manager: &mut impl DesktopManager,
    ) -> Result<()> {
        if exclusive.release(session) {
            exclusive.restore(manager)?;
        }
        Ok(())
    }

    #[test]
    fn restores_monitors_and_workspaces_only_after_last_viewer() {
        let mut manager = desktop();
        let original = manager.state.clone();
        let mut exclusive = ExclusiveDesktop::default();
        exclusive.acquire(1, "DP-2", &mut manager).unwrap();
        assert!(manager.state.outputs[0].disabled);
        assert!(!manager.state.outputs[1].disabled);
        assert_eq!(manager.state.workspaces[0].monitor, "DP-2");
        exclusive.acquire(2, "DP-2", &mut manager).unwrap();
        exclusive.acquire(2, "DP-2", &mut manager).unwrap();
        disconnect(&mut exclusive, 99, &mut manager).unwrap();
        disconnect(&mut exclusive, 1, &mut manager).unwrap();
        assert_eq!(manager.log, ["snapshot", "disable DP-1"]);
        disconnect(&mut exclusive, 2, &mut manager).unwrap();
        assert_eq!(manager.state.outputs, original.outputs);
        assert_eq!(manager.state.workspaces, original.workspaces);
        disconnect(&mut exclusive, 2, &mut manager).unwrap();
        assert_eq!(manager.log, ["snapshot", "disable DP-1", "restore"]);
    }

    #[test]
    fn missing_or_disabled_shared_output_never_disables_anything() {
        for name in ["missing", "DP-3"] {
            let mut manager = desktop();
            let mut exclusive = ExclusiveDesktop::default();
            assert!(exclusive.acquire(1, name, &mut manager).is_err());
            disconnect(&mut exclusive, 1, &mut manager).unwrap();
            assert_eq!(manager.log, ["snapshot"]);
        }
    }

    #[test]
    fn partial_failure_rolls_back_and_preserves_originally_disabled_outputs() {
        let mut manager = desktop();
        let original = manager.state.clone();
        manager.fail_disable = Some("DP-1".into());
        let mut exclusive = ExclusiveDesktop::default();
        assert!(exclusive.acquire(1, "DP-2", &mut manager).is_err());
        assert_eq!(manager.state.outputs, original.outputs);
        assert_eq!(manager.state.workspaces, original.workspaces);
        assert_eq!(manager.log, ["snapshot", "disable DP-1", "restore"]);
        manager.fail_disable = None;
        exclusive.acquire(2, "DP-2", &mut manager).unwrap();
        disconnect(&mut exclusive, 2, &mut manager).unwrap();
    }

    #[test]
    fn failed_rollback_is_retried_by_session_teardown() {
        let mut manager = desktop();
        manager.fail_disable = Some("DP-1".into());
        manager.fail_restore = true;
        let mut exclusive = ExclusiveDesktop::default();
        let error = exclusive.acquire(1, "DP-2", &mut manager).unwrap_err();
        assert!(format!("{error:#}").contains("rollback also failed"));
        manager.fail_restore = false;
        disconnect(&mut exclusive, 1, &mut manager).unwrap();
        assert!(!manager.state.outputs[0].disabled);
        assert!(manager.state.outputs[2].disabled);
    }

    #[test]
    fn failed_restore_is_not_overwritten_by_a_new_snapshot() {
        let mut manager = desktop();
        let original = manager.state.outputs.clone();
        let mut exclusive = ExclusiveDesktop::default();
        exclusive.acquire(1, "DP-2", &mut manager).unwrap();
        manager.fail_restore = true;
        assert!(disconnect(&mut exclusive, 1, &mut manager).is_err());
        assert!(exclusive.acquire(2, "DP-2", &mut manager).is_err());
        assert_eq!(manager.log.iter().filter(|s| *s == "snapshot").count(), 1);
        manager.fail_restore = false;
        exclusive.acquire(3, "DP-2", &mut manager).unwrap();
        disconnect(&mut exclusive, 3, &mut manager).unwrap();
        assert_eq!(manager.state.outputs, original);
    }

    #[test]
    fn workspace_selectors_include_named_special_workspaces() {
        let mut workspace = WorkspaceInfo {
            id: 3,
            name: "custom".into(),
            monitor: "DP-1".into(),
        };
        assert_eq!(workspace.selector(), "3");
        workspace.id = -99;
        workspace.name = "special:scratch".into();
        assert_eq!(workspace.selector(), "special:scratch");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "temporarily disables real monitors; set WAYSHARE_TEST_OUTPUT explicitly"]
    fn live_exclusive_round_trip() {
        use crate::display::hyprland::Hyprland;

        struct LiveDesktop {
            exclusive: ExclusiveDesktop,
            manager: Hyprland,
        }
        impl Drop for LiveDesktop {
            fn drop(&mut self) {
                if let Err(e) = disconnect(&mut self.exclusive, 1, &mut self.manager) {
                    eprintln!("live test cleanup failed: {e:#}; recover with hyprctl reload");
                }
            }
        }

        let shared = std::env::var("WAYSHARE_TEST_OUTPUT")
            .expect("set WAYSHARE_TEST_OUTPUT to the monitor that must stay enabled");
        let mut live = LiveDesktop {
            exclusive: ExclusiveDesktop::default(),
            manager: Hyprland::from_env().unwrap(),
        };
        let before = live.manager.snapshot().unwrap();
        live.exclusive
            .acquire(1, &shared, &mut live.manager)
            .unwrap();
        let during = live.manager.snapshot().unwrap();
        let enabled: Vec<_> = during.outputs.iter().filter(|o| !o.disabled).collect();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, shared);
        assert!(during.workspaces.iter().all(|w| w.monitor == shared));
        disconnect(&mut live.exclusive, 1, &mut live.manager).unwrap();
        let after = live.manager.snapshot().unwrap();
        for original in before.outputs.iter().filter(|o| !o.disabled) {
            let restored = after
                .outputs
                .iter()
                .find(|o| o.name == original.name)
                .unwrap();
            assert!(!restored.disabled, "{} stayed disabled", original.name);
            assert_eq!(
                (restored.width, restored.height, restored.x, restored.y),
                (original.width, original.height, original.x, original.y)
            );
            assert!((restored.scale - original.scale).abs() < 1e-3);
            assert_eq!(restored.transform, original.transform);
            assert_eq!(restored.focused, original.focused);
        }
        for original in &before.workspaces {
            if let Some(restored) = after.workspaces.iter().find(|w| w.id == original.id) {
                assert_eq!(restored.monitor, original.monitor);
            }
        }
    }
}
