//! Optional `StatusNotifierItem` (`ksni`) system tray integration and live recording top-bar controls.

use std::process::Command;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
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
                label: "Record Screen".to_string(),
                activate: Box::new(|_| spawn_self(&["--mode", "record"])),
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

/// Actions triggered from the desktop top-bar (`StatusNotifierItem`) during screen recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayRecordingCmd {
    /// Pause an active recording or resume a paused recording.
    TogglePause,
    /// Stop and finalize the recording.
    Stop,
    /// Show or hide the optional floating recording bar.
    ToggleFloatingBar,
}

/// Top-bar `StatusNotifierItem` indicator and menu for controlling an active screen recording.
pub struct RecordingTray {
    /// Elapsed active recording duration in seconds.
    pub elapsed_secs: u64,
    /// Whether the recording is currently paused.
    pub paused: bool,
    /// Whether the fallback/optional floating bar window is currently shown.
    pub floating_bar_visible: bool,
    tx: UnboundedSender<TrayRecordingCmd>,
}

impl RecordingTray {
    fn format_elapsed(&self) -> String {
        let mins = self.elapsed_secs / 60;
        let secs = self.elapsed_secs % 60;
        format!("{mins:02}:{secs:02}")
    }
}

impl ksni::Tray for RecordingTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "io.github.lx_snippingtool.Recorder".to_string()
    }

    fn title(&self) -> String {
        let ts = self.format_elapsed();
        if self.paused {
            format!("Pausado {ts}")
        } else {
            format!("Gravando {ts}")
        }
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![render_tray_icon(self.paused)]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.title(),
            description: "Clique para Pausar ou Parar a gravação".to_string(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let ts = self.format_elapsed();
        let status_label = if self.paused {
            format!("⏸ Pausado — {ts}")
        } else {
            format!("● Gravando — {ts}")
        };
        let pause_label = if self.paused {
            "Retomar gravação"
        } else {
            "Pausar gravação"
        };
        let bar_label = if self.floating_bar_visible {
            "Ocultar barra flutuante"
        } else {
            "Mostrar barra flutuante"
        };

        vec![
            StandardItem {
                label: status_label,
                enabled: false,
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: pause_label.to_string(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayRecordingCmd::TogglePause);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Parar gravação".to_string(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayRecordingCmd::Stop);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: bar_label.to_string(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayRecordingCmd::ToggleFloatingBar);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Spawns the `RecordingTray` indicator in the desktop top bar / system tray.
pub async fn spawn_recording_tray() -> Result<
    (
        ksni::Handle<RecordingTray>,
        UnboundedReceiver<TrayRecordingCmd>,
    ),
    ksni::Error,
> {
    use ksni::TrayMethods;
    let (tx, rx) = mpsc::unbounded_channel();
    let tray = RecordingTray {
        elapsed_secs: 0,
        paused: false,
        floating_bar_visible: false,
        tx,
    };
    let handle = tray.spawn().await?;
    Ok((handle, rx))
}

/// Renders a 24x24 ARGB32 icon (red dot when recording, amber dot with pause bars when paused).
pub fn render_tray_icon(paused: bool) -> ksni::Icon {
    const SIZE: usize = 24;
    let mut data = vec![0u8; SIZE * SIZE * 4];
    let center = (SIZE as f32 - 1.0) * 0.5;
    let outer_r = 9.8f32;
    let inner_r = 7.5f32;

    let (fill_r, fill_g, fill_b) = if paused {
        (245u8, 158u8, 11u8)
    } else {
        (239u8, 68u8, 68u8)
    };

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let offset = (y * SIZE + x) * 4;

            if dist <= inner_r {
                let is_pause_bar = paused
                    && (7..=16).contains(&y)
                    && ((8..=10).contains(&x) || (13..=15).contains(&x));
                if is_pause_bar {
                    data[offset] = 255;
                    data[offset + 1] = 255;
                    data[offset + 2] = 255;
                    data[offset + 3] = 255;
                } else {
                    data[offset] = 255;
                    data[offset + 1] = fill_r;
                    data[offset + 2] = fill_g;
                    data[offset + 3] = fill_b;
                }
            } else if dist <= outer_r {
                let alpha = ((outer_r - dist).clamp(0.0, 1.0) * 255.0).round() as u8;
                data[offset] = alpha;
                data[offset + 1] = 255;
                data[offset + 2] = 255;
                data[offset + 3] = 255;
            }
        }
    }

    ksni::Icon {
        width: SIZE as i32,
        height: SIZE as i32,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_pixmap_has_expected_argb32_size() {
        let rec = render_tray_icon(false);
        assert_eq!(rec.width, 24);
        assert_eq!(rec.height, 24);
        assert_eq!(rec.data.len(), 24 * 24 * 4);

        let paused = render_tray_icon(true);
        assert_eq!(paused.width, 24);
        assert_eq!(paused.height, 24);
        assert_eq!(paused.data.len(), 24 * 24 * 4);
        assert_ne!(rec.data, paused.data);
    }
}
