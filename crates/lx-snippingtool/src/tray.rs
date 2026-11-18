//! Optional `StatusNotifierItem` (`ksni`) system tray integration.

use std::process::Command;

use tracing::{debug, warn};

/// System tray item for `lx-snippingtool`.
#[derive(Debug, Default)]
pub struct SnippingTray;

impl ksni::Tray for SnippingTray {
    fn id(&self) -> String {
        "io.github.lx_snippingtool.SnippingTool".to_string()
    }

    fn title(&self) -> String {
        "LX Snipping Tool".to_string()
    }

    fn icon_name(&self) -> String {
        "applets-screenshooter".to_string()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        spawn_self(&["--mode", "rect"]);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "New Snip (Rect)".to_string(),
                activate: Box::new(|_| spawn_self(&["--mode", "rect"])),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Snip History".to_string(),
                activate: Box::new(|_| spawn_self(&["--history"])),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Registers the `ksni` tray service when `--tray` is requested, logging at `debug`
/// and continuing as a no-op if no `StatusNotifierWatcher` host is available.
pub fn spawn_tray_if_available() {
    use ksni::blocking::TrayMethods;
    match SnippingTray.spawn() {
        Ok(_handle) => {
            debug!("ksni StatusNotifierItem tray service spawned");
        }
        Err(err) => {
            debug!(error = %err, "ksni StatusNotifierItem tray unavailable; continuing without tray");
        }
    }
}

fn spawn_self(args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else {
        warn!("failed to resolve current_exe for tray action");
        return;
    };
    if let Err(err) = Command::new(exe).args(args).spawn() {
        warn!(error = %err, ?args, "failed to spawn lx-snippingtool from tray");
    }
}
