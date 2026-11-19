//! `lx-snippingtool` binary entry point: CLI, single-instance lock, runner dispatch
//! (`iced::daemon` vs `iced_layershell::build_pattern::daemon`), and post-copy clipboard holder.

mod capture_wayland;
mod capture_x11;
mod clipboard;
mod editor;
mod history_view;
mod icons;
mod notification;
mod overlay;
mod recorder;
mod tray;

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use iced::keyboard;
use iced::widget::{button, container, mouse_area, row, text, Column};
use iced::{Alignment, Element, Length, Size, Subscription, Task};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::LayerShellSettings;
use iced_layershell::{to_layer_message, WindowInfoMarker};
use lx_snip_core::config::{load_or_default, AppConfig, CaptureMode};
use lx_snip_core::export::{encode_rgba, ExportFormat};
use lx_snip_core::platform::{CaptureBackend, CapturedImage, ClipboardBackend, OutputGeometry};
use lx_snip_core::storage::SnipStore;
use lx_snip_core::telemetry::init_telemetry;
use tracing::{error, info, warn};

use crate::capture_wayland::PortalCaptureBackend;
use crate::capture_x11::{list_x11_windows, X11CaptureBackend, X11WindowBounds};
use crate::clipboard::AutoClipboardBackend;
use crate::editor::{save_as_via_portal, EditorAction, EditorMessage, EditorState};
use crate::history_view::{HistoryAction, HistoryMessage, HistoryState};
use crate::overlay::{OverlayMessage, OverlayOutcome, OverlayState};
use crate::recorder::ActiveRecording;
use crate::tray::{RecordingTray, TrayRecordingCmd};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliMode {
    Rect,
    Freeform,
    Window,
    Full,
    Record,
}

impl From<CliMode> for CaptureMode {
    fn from(value: CliMode) -> Self {
        match value {
            CliMode::Rect => CaptureMode::Rect,
            CliMode::Freeform => CaptureMode::Freeform,
            CliMode::Window => CaptureMode::Window,
            CliMode::Full => CaptureMode::Fullscreen,
            CliMode::Record => CaptureMode::Record,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "lx-snippingtool",
    version,
    about = "Windows 11 Snipping Tool clone for Linux desktops"
)]
struct Cli {
    /// Capture mode: rect, freeform, window, or full.
    #[arg(long, value_enum)]
    mode: Option<CliMode>,

    /// Pre-capture delay in seconds (e.g. 3, 5, 10).
    #[arg(long)]
    delay: Option<u8>,

    /// Open an existing PNG file directly in the annotation editor.
    #[arg(long, value_name = "PATH")]
    editor: Option<PathBuf>,

    /// Open the local Snip History browser.
    #[arg(long)]
    history: bool,

    /// Register an optional StatusNotifierItem (`ksni`) system tray icon.
    #[arg(long)]
    tray: bool,

    /// Register Ctrl+Shift+S global shortcut in GNOME Settings (gsettings) and exit.
    #[arg(long)]
    install_shortcut: bool,
}

#[derive(Debug, Clone)]
struct ClipboardJob {
    png_bytes: Vec<u8>,
    snip_path: PathBuf,
}

#[derive(Clone)]
struct SharedRecording(Arc<tokio::sync::Mutex<Option<ActiveRecording>>>);

impl std::fmt::Debug for SharedRecording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedRecording").finish()
    }
}

#[derive(Clone)]
struct RecordingSessionBundle {
    recording: SharedRecording,
    tray: Option<ksni::Handle<RecordingTray>>,
    tray_rx:
        Option<Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<TrayRecordingCmd>>>>,
}

impl std::fmt::Debug for RecordingSessionBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingSessionBundle")
            .field("has_tray", &self.tray.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, WindowInfoMarker)]
enum WindowInfo {
    DelayPill,
    Overlay(Option<OutputGeometryKey>),
    Editor,
    History,
    RecordingBar,
    RecordingDone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputGeometryKey {
    name: String,
}

#[derive(Debug, Clone)]
enum AppPhase {
    DelayPill {
        remaining_secs: u8,
    },
    Capturing,
    Overlay(OverlayState),
    StartingRecording,
    Recording {
        elapsed_secs: u64,
        paused: bool,
        busy: bool,
        floating_bar_visible: bool,
    },
    RecordingDone {
        path: PathBuf,
        duration_secs: u64,
        status: Option<String>,
    },
    Editor(EditorState),
    History(HistoryState),
    Error(String),
}

#[derive(Debug, Clone)]
enum AppMessage {
    DelayTick,
    Cancel,
    CaptureReady(Result<(CapturedImage, bool, Vec<X11WindowBounds>), String>),
    Overlay(OverlayMessage),
    Editor(EditorMessage),
    History(HistoryMessage),
    RecordingStarted(Result<RecordingSessionBundle, String>),
    RecordingTick,
    TogglePauseRecording,
    PauseToggled(Result<bool, String>),
    TrayRecordingEvent(Option<TrayRecordingCmd>),
    DragRecordingBar,
    StopRecording,
    RecordingStopped(Result<(PathBuf, u64), String>),
    SaveRecordingAs,
    SaveRecordingDone(Result<Option<PathBuf>, String>),
    OpenRecordingFolder,
    StartNewFromRecording,
    WindowClosed(iced::window::Id),
}

struct AppCore {
    phase: AppPhase,
    mode: CaptureMode,
    delay_secs: u8,
    config: AppConfig,
    store: SnipStore,
    windows: HashMap<iced::window::Id, WindowInfo>,
    outputs: Vec<OutputGeometry>,
    pending_clipboard: Arc<Mutex<Option<ClipboardJob>>>,
    is_wayland: bool,
    capture_lock: Option<File>,
    active_recording: Option<SharedRecording>,
    recording_tray: Option<ksni::Handle<RecordingTray>>,
    recording_tray_rx:
        Option<Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<TrayRecordingCmd>>>>,
}

impl AppCore {
    #[allow(clippy::too_many_arguments)]
    fn new(
        initial_phase: AppPhase,
        mode: CaptureMode,
        delay_secs: u8,
        config: AppConfig,
        store: SnipStore,
        pending_clipboard: Arc<Mutex<Option<ClipboardJob>>>,
        is_wayland: bool,
        capture_lock: Option<File>,
    ) -> Self {
        Self {
            phase: initial_phase,
            mode,
            delay_secs,
            config,
            store,
            windows: HashMap::new(),
            outputs: Vec::new(),
            pending_clipboard,
            is_wayland,
            capture_lock,
            active_recording: None,
            recording_tray: None,
            recording_tray_rx: None,
        }
    }

    fn initial_iced_task(&mut self) -> Task<AppMessage> {
        match &self.phase {
            AppPhase::DelayPill { .. } => {
                let (id, open_task) = iced::window::open(iced::window::Settings {
                    size: Size::new(260.0, 72.0),
                    decorations: false,
                    resizable: false,
                    level: iced::window::Level::AlwaysOnTop,
                    ..Default::default()
                });
                self.windows.insert(id, WindowInfo::DelayPill);
                Task::batch([open_task.discard(), schedule_delay_tick()])
            }
            AppPhase::Capturing => start_capture_task(self.mode, self.is_wayland),
            AppPhase::Editor(_) => {
                let (id, open_task) = iced::window::open(iced::window::Settings {
                    size: Size::new(980.0, 680.0),
                    decorations: false,
                    ..Default::default()
                });
                self.windows.insert(id, WindowInfo::Editor);
                open_task.discard()
            }
            AppPhase::History(_) => {
                let (id, open_task) = iced::window::open(iced::window::Settings {
                    size: Size::new(720.0, 560.0),
                    ..Default::default()
                });
                self.windows.insert(id, WindowInfo::History);
                open_task.discard()
            }
            AppPhase::Overlay(_)
            | AppPhase::StartingRecording
            | AppPhase::Recording { .. }
            | AppPhase::RecordingDone { .. }
            | AppPhase::Error(_) => Task::none(),
        }
    }

    fn update_core(&mut self, msg: AppMessage, use_layershell: bool) -> Task<AppMessage> {
        match msg {
            AppMessage::DelayTick => {
                if let AppPhase::DelayPill { remaining_secs } = &mut self.phase {
                    if *remaining_secs > 1 {
                        *remaining_secs -= 1;
                        return schedule_delay_tick();
                    }
                    self.phase = AppPhase::Capturing;
                    let close_all = self.close_all_windows_task();
                    let cap = start_capture_task(self.mode, self.is_wayland);
                    return Task::batch([close_all, cap]);
                }
                Task::none()
            }
            AppMessage::Cancel => {
                if matches!(self.phase, AppPhase::Recording { .. }) {
                    // Do not abort active recordings when user hits Escape in another app.
                    Task::none()
                } else {
                    iced::exit()
                }
            }
            AppMessage::WindowClosed(id) => {
                self.windows.remove(&id);
                if matches!(self.phase, AppPhase::Recording { .. }) {
                    let has_bar = self
                        .windows
                        .values()
                        .any(|w| matches!(w, WindowInfo::RecordingBar));
                    if !has_bar {
                        if self.recording_tray.is_some() {
                            if let AppPhase::Recording {
                                floating_bar_visible,
                                ..
                            } = &mut self.phase
                            {
                                *floating_bar_visible = false;
                            }
                            return self.update_tray_task();
                        } else if self.windows.is_empty() {
                            return self.stop_recording_task();
                        }
                    }
                    return Task::none();
                }
                if self.windows.is_empty()
                    && !matches!(
                        self.phase,
                        AppPhase::Capturing | AppPhase::StartingRecording
                    )
                {
                    return iced::exit();
                }
                Task::none()
            }
            AppMessage::CaptureReady(res) => match res {
                Ok((captured, supports_window, x11_windows)) => {
                    if (self.mode == CaptureMode::Window && !supports_window)
                        || self.mode == CaptureMode::Fullscreen
                    {
                        return self.handle_captured_rgba(
                            captured.rgba,
                            captured.width,
                            captured.height,
                        );
                    }

                    self.outputs = captured.outputs.clone();
                    let overlay = OverlayState::new(
                        captured,
                        self.mode,
                        self.delay_secs,
                        supports_window,
                        x11_windows,
                    );
                    self.phase = AppPhase::Overlay(overlay);

                    if use_layershell {
                        // Layer-shell overlay windows are opened via LayerMsg::NewLayerShell in LayerShellApp.
                        Task::none()
                    } else {
                        self.open_iced_overlay_windows()
                    }
                }
                Err(err) => {
                    warn!(error = %err, "capture failed or cancelled");
                    self.capture_lock = None;
                    if err.to_ascii_lowercase().contains("cancelled") {
                        iced::exit()
                    } else {
                        self.phase = AppPhase::Error(err);
                        let (id, open_task) = iced::window::open(iced::window::Settings {
                            size: Size::new(560.0, 180.0),
                            ..Default::default()
                        });
                        self.windows.insert(id, WindowInfo::Editor);
                        open_task.discard()
                    }
                }
            },
            AppMessage::Overlay(ov_msg) => {
                let AppPhase::Overlay(overlay) = &mut self.phase else {
                    return Task::none();
                };
                match overlay.update(ov_msg) {
                    Some(OverlayOutcome::Captured {
                        rgba,
                        width,
                        height,
                    }) => self.handle_captured_rgba(rgba, width, height),
                    Some(OverlayOutcome::CaptureWindowPortal) => {
                        let close_old = self.close_all_windows_task();
                        self.mode = CaptureMode::Window;
                        self.phase = AppPhase::Capturing;
                        Task::batch([close_old, start_capture_task(self.mode, self.is_wayland)])
                    }
                    Some(OverlayOutcome::StartRecording {
                        target,
                        with_system_audio,
                        with_mic,
                    }) => {
                        self.capture_lock = None;
                        let close_old = self.close_all_windows_task();
                        self.phase = AppPhase::StartingRecording;
                        let is_wayland = self.is_wayland;
                        let start_task = Task::perform(
                            async move {
                                let rec = recorder::start_recording(
                                    is_wayland,
                                    target,
                                    with_system_audio,
                                    with_mic,
                                )
                                .await?;
                                let (tray, tray_rx) = match tray::spawn_recording_tray().await {
                                    Ok((handle, rx)) => {
                                        (Some(handle), Some(Arc::new(tokio::sync::Mutex::new(rx))))
                                    }
                                    Err(err) => {
                                        warn!(error = %err, "recording top-bar tray unavailable; falling back to floating bar");
                                        (None, None)
                                    }
                                };
                                Ok(RecordingSessionBundle {
                                    recording: SharedRecording(Arc::new(tokio::sync::Mutex::new(
                                        Some(rec),
                                    ))),
                                    tray,
                                    tray_rx,
                                })
                            },
                            AppMessage::RecordingStarted,
                        );
                        Task::batch([close_old, start_task])
                    }
                    Some(OverlayOutcome::Cancelled) => iced::exit(),
                    None => Task::none(),
                }
            }
            AppMessage::Editor(ed_msg) => {
                let AppPhase::Editor(editor) = &mut self.phase else {
                    return Task::none();
                };
                match editor.update(ed_msg) {
                    Some(EditorAction::CopyAndExit(png_bytes)) => {
                        let snip_path = match self.store.save_png(&png_bytes) {
                            Ok(path) => path,
                            Err(err) => {
                                warn!(error = %err, "failed to save snip to local history; copying anyway");
                                PathBuf::new()
                            }
                        };
                        if let Ok(mut guard) = self.pending_clipboard.lock() {
                            *guard = Some(ClipboardJob {
                                png_bytes: png_bytes.clone(),
                                snip_path,
                            });
                        }
                        tokio::spawn(async move {
                            if let Err(err) = AutoClipboardBackend::new()
                                .copy_png_and_hold(png_bytes)
                                .await
                            {
                                warn!(error = %err, "background clipboard copy failed");
                            }
                        });
                        editor.status_text = Some("Copied to clipboard".to_string());
                        Task::none()
                    }
                    Some(EditorAction::NewCapture) => {
                        let close_old = self.close_all_windows_task();
                        self.phase = AppPhase::Capturing;
                        Task::batch([close_old, start_capture_task(self.mode, self.is_wayland)])
                    }
                    Some(EditorAction::OpenHistory) => {
                        let close_old = self.close_all_windows_task();
                        self.phase = AppPhase::History(HistoryState::new(self.store.clone()));
                        let (id, open_task) = iced::window::open(iced::window::Settings {
                            size: Size::new(720.0, 560.0),
                            ..Default::default()
                        });
                        self.windows.insert(id, WindowInfo::History);
                        Task::batch([close_old, open_task.discard()])
                    }
                    Some(EditorAction::RunSaveAs {
                        base_rgba,
                        width,
                        height,
                        scene,
                    }) => {
                        Task::perform(save_as_via_portal(base_rgba, width, height, scene), |res| {
                            AppMessage::Editor(EditorMessage::SaveCompleted(res))
                        })
                    }
                    Some(EditorAction::Minimize) => {
                        if let Some((&id, _)) = self
                            .windows
                            .iter()
                            .find(|(_, info)| matches!(info, WindowInfo::Editor))
                        {
                            iced::window::minimize(id, true)
                        } else {
                            Task::none()
                        }
                    }
                    Some(EditorAction::ToggleMaximize) => {
                        if let Some((&id, _)) = self
                            .windows
                            .iter()
                            .find(|(_, info)| matches!(info, WindowInfo::Editor))
                        {
                            iced::window::toggle_maximize(id)
                        } else {
                            Task::none()
                        }
                    }
                    Some(EditorAction::DragWindow) => {
                        if let Some((&id, _)) = self
                            .windows
                            .iter()
                            .find(|(_, info)| matches!(info, WindowInfo::Editor))
                        {
                            iced::window::drag(id)
                        } else {
                            Task::none()
                        }
                    }
                    Some(EditorAction::Close) => iced::exit(),
                    None => Task::none(),
                }
            }
            AppMessage::History(hist_msg) => {
                let AppPhase::History(history) = &mut self.phase else {
                    return Task::none();
                };
                match history.update(hist_msg) {
                    Some(HistoryAction::CopyPng { path, png_bytes }) => {
                        if let Ok(mut guard) = self.pending_clipboard.lock() {
                            *guard = Some(ClipboardJob {
                                png_bytes,
                                snip_path: path,
                            });
                        }
                        iced::exit()
                    }
                    Some(HistoryAction::OpenEditor(path)) => {
                        match EditorState::from_png_file(&path) {
                            Ok(editor) => {
                                self.phase = AppPhase::Editor(editor);
                                let close_old = self.close_all_windows_task();
                                let (id, open_task) = iced::window::open(iced::window::Settings {
                                    size: Size::new(980.0, 680.0),
                                    decorations: false,
                                    ..Default::default()
                                });
                                self.windows.insert(id, WindowInfo::Editor);
                                Task::batch([close_old, open_task.discard()])
                            }
                            Err(err) => {
                                self.phase = AppPhase::Error(err);
                                Task::none()
                            }
                        }
                    }
                    None => Task::none(),
                }
            }
            AppMessage::RecordingStarted(res) => match res {
                Ok(bundle) => {
                    let has_tray = bundle.tray.is_some();
                    self.active_recording = Some(bundle.recording);
                    self.recording_tray = bundle.tray;
                    self.recording_tray_rx = bundle.tray_rx.clone();
                    let floating_bar_visible = !has_tray;
                    self.phase = AppPhase::Recording {
                        elapsed_secs: 0,
                        paused: false,
                        busy: false,
                        floating_bar_visible,
                    };

                    let mut tasks = vec![schedule_recording_tick()];
                    if let Some(rx) = bundle.tray_rx {
                        tasks.push(listen_tray_cmd(rx));
                    }
                    if floating_bar_visible {
                        tasks.push(self.open_recording_bar_window());
                    }
                    Task::batch(tasks)
                }
                Err(err) => {
                    warn!(error = %err, "recording failed to start");
                    if err.to_ascii_lowercase().contains("cancelled") {
                        iced::exit()
                    } else {
                        self.phase = AppPhase::Error(err);
                        let (id, open_task) = iced::window::open(iced::window::Settings {
                            size: Size::new(560.0, 180.0),
                            ..Default::default()
                        });
                        self.windows.insert(id, WindowInfo::Editor);
                        open_task.discard()
                    }
                }
            },
            AppMessage::RecordingTick => {
                if let AppPhase::Recording {
                    elapsed_secs,
                    paused,
                    busy,
                    ..
                } = &mut self.phase
                {
                    if !*paused && !*busy {
                        *elapsed_secs += 1;
                    }
                    return Task::batch([schedule_recording_tick(), self.update_tray_task()]);
                }
                Task::none()
            }
            AppMessage::TogglePauseRecording => self.toggle_pause_task(),
            AppMessage::PauseToggled(res) => match res {
                Ok(now_paused) => {
                    if let AppPhase::Recording { paused, busy, .. } = &mut self.phase {
                        *paused = now_paused;
                        *busy = false;
                    }
                    self.update_tray_task()
                }
                Err(err) => {
                    warn!(error = %err, "failed to toggle recording pause");
                    if let AppPhase::Recording { busy, .. } = &mut self.phase {
                        *busy = false;
                    }
                    Task::none()
                }
            },
            AppMessage::TrayRecordingEvent(Some(cmd)) => {
                if !matches!(self.phase, AppPhase::Recording { .. }) {
                    return Task::none();
                }
                match cmd {
                    TrayRecordingCmd::TogglePause => {
                        let pause_task = self.toggle_pause_task();
                        if let Some(rx) = self.recording_tray_rx.clone() {
                            Task::batch([pause_task, listen_tray_cmd(rx)])
                        } else {
                            pause_task
                        }
                    }
                    TrayRecordingCmd::Stop => self.stop_recording_task(),
                    TrayRecordingCmd::ToggleFloatingBar => {
                        let mut tasks = Vec::new();
                        let show = match &mut self.phase {
                            AppPhase::Recording {
                                floating_bar_visible,
                                ..
                            } => {
                                *floating_bar_visible = !*floating_bar_visible;
                                *floating_bar_visible
                            }
                            _ => false,
                        };
                        if show {
                            tasks.push(self.open_recording_bar_window());
                        } else {
                            tasks.push(self.close_recording_bar_windows());
                        }
                        tasks.push(self.update_tray_task());
                        if let Some(rx) = self.recording_tray_rx.clone() {
                            tasks.push(listen_tray_cmd(rx));
                        }
                        Task::batch(tasks)
                    }
                }
            }
            AppMessage::TrayRecordingEvent(None) => Task::none(),
            AppMessage::DragRecordingBar => {
                if let Some((&id, _)) = self
                    .windows
                    .iter()
                    .find(|(_, info)| matches!(info, WindowInfo::RecordingBar))
                {
                    iced::window::drag(id)
                } else {
                    Task::none()
                }
            }
            AppMessage::StopRecording => self.stop_recording_task(),
            AppMessage::RecordingStopped(res) => {
                let close_old = self.close_all_windows_task();
                match res {
                    Ok((path, duration_secs)) => {
                        self.phase = AppPhase::RecordingDone {
                            path,
                            duration_secs,
                            status: None,
                        };
                        let (id, open_task) = iced::window::open(iced::window::Settings {
                            size: Size::new(520.0, 260.0),
                            ..Default::default()
                        });
                        self.windows.insert(id, WindowInfo::RecordingDone);
                        Task::batch([close_old, open_task.discard()])
                    }
                    Err(err) => {
                        self.phase = AppPhase::Error(err);
                        let (id, open_task) = iced::window::open(iced::window::Settings {
                            size: Size::new(560.0, 180.0),
                            ..Default::default()
                        });
                        self.windows.insert(id, WindowInfo::Editor);
                        Task::batch([close_old, open_task.discard()])
                    }
                }
            }
            AppMessage::SaveRecordingAs => {
                let AppPhase::RecordingDone { path, .. } = &self.phase else {
                    return Task::none();
                };
                let src = path.clone();
                Task::perform(
                    save_recording_via_portal(src),
                    AppMessage::SaveRecordingDone,
                )
            }
            AppMessage::SaveRecordingDone(res) => {
                if let AppPhase::RecordingDone { status, .. } = &mut self.phase {
                    *status = Some(match res {
                        Ok(Some(dest)) => format!("Saved to {}", dest.display()),
                        Ok(None) => "Save cancelled".to_string(),
                        Err(err) => format!("Save failed: {err}"),
                    });
                }
                Task::none()
            }
            AppMessage::OpenRecordingFolder => {
                if let AppPhase::RecordingDone { path, status, .. } = &mut self.phase {
                    if let Some(parent) = path.parent() {
                        if let Err(err) = std::process::Command::new("xdg-open").arg(parent).spawn()
                        {
                            *status = Some(format!("Failed to open folder: {err}"));
                        }
                    }
                }
                Task::none()
            }
            AppMessage::StartNewFromRecording => {
                let close_old = self.close_all_windows_task();
                self.phase = AppPhase::Capturing;
                Task::batch([close_old, start_capture_task(self.mode, self.is_wayland)])
            }
        }
    }

    fn open_recording_bar_window(&mut self) -> Task<AppMessage> {
        if self
            .windows
            .values()
            .any(|info| matches!(info, WindowInfo::RecordingBar))
        {
            return Task::none();
        }
        let (id, open_task) = iced::window::open(iced::window::Settings {
            size: Size::new(310.0, 54.0),
            decorations: false,
            resizable: false,
            level: iced::window::Level::AlwaysOnTop,
            ..Default::default()
        });
        self.windows.insert(id, WindowInfo::RecordingBar);
        open_task.discard()
    }

    fn close_recording_bar_windows(&mut self) -> Task<AppMessage> {
        let bar_ids: Vec<_> = self
            .windows
            .iter()
            .filter(|(_, info)| matches!(info, WindowInfo::RecordingBar))
            .map(|(&id, _)| id)
            .collect();
        for id in &bar_ids {
            self.windows.remove(id);
        }
        Task::batch(bar_ids.into_iter().map(iced::window::close))
    }

    fn update_tray_task(&self) -> Task<AppMessage> {
        let Some(tray) = self.recording_tray.clone() else {
            return Task::none();
        };
        let AppPhase::Recording {
            elapsed_secs,
            paused,
            floating_bar_visible,
            ..
        } = self.phase
        else {
            return Task::none();
        };
        Task::future(async move {
            tray.update(|t| {
                t.elapsed_secs = elapsed_secs;
                t.paused = paused;
                t.floating_bar_visible = floating_bar_visible;
            })
            .await;
        })
        .discard()
    }

    fn toggle_pause_task(&mut self) -> Task<AppMessage> {
        let Some(shared) = self.active_recording.clone() else {
            return Task::none();
        };
        let target_paused = match &mut self.phase {
            AppPhase::Recording { paused, busy, .. } => {
                if *busy {
                    return Task::none();
                }
                *busy = true;
                !*paused
            }
            _ => return Task::none(),
        };
        Task::perform(
            async move {
                let mut guard = shared.0.lock().await;
                let Some(rec) = guard.as_mut() else {
                    return Err("Recording is no longer active".to_string());
                };
                if target_paused {
                    rec.pause().await?;
                } else {
                    rec.resume().await?;
                }
                Ok(target_paused)
            },
            AppMessage::PauseToggled,
        )
    }

    fn stop_recording_task(&mut self) -> Task<AppMessage> {
        if let AppPhase::Recording { busy, .. } = &mut self.phase {
            *busy = true;
        }
        let tray = self.recording_tray.take();
        self.recording_tray_rx = None;
        let close_bars = self.close_recording_bar_windows();
        let Some(shared) = self.active_recording.take() else {
            if let Some(handle) = tray {
                return Task::batch([
                    close_bars,
                    Task::future(async move {
                        handle.shutdown().await;
                    })
                    .discard(),
                    iced::exit(),
                ]);
            }
            return iced::exit();
        };
        let stop_task = Task::perform(
            async move {
                if let Some(handle) = tray {
                    handle.shutdown().await;
                }
                let mut guard = shared.0.lock().await;
                let Some(rec) = guard.take() else {
                    return Err("Recording already stopped".to_string());
                };
                drop(guard);
                rec.stop().await
            },
            AppMessage::RecordingStopped,
        );
        Task::batch([close_bars, stop_task])
    }

    fn handle_captured_rgba(&mut self, rgba: Vec<u8>, width: u32, height: u32) -> Task<AppMessage> {
        // Capture overlay is finished; release single-instance capture lock so new captures can start.
        self.capture_lock = None;
        if self.config.auto_copy {
            match encode_rgba(&rgba, width, height, ExportFormat::Png) {
                Ok(png_bytes) => {
                    let snip_path = match self.store.save_png(&png_bytes) {
                        Ok(path) => path,
                        Err(err) => {
                            warn!(error = %err, "failed to save snip to local history; copying anyway");
                            PathBuf::new()
                        }
                    };
                    if let Ok(mut guard) = self.pending_clipboard.lock() {
                        *guard = Some(ClipboardJob {
                            png_bytes,
                            snip_path,
                        });
                    }
                }
                Err(err) => {
                    error!(error = %err, "failed to encode captured PNG");
                }
            }
        }
        let close_old = self.close_all_windows_task();
        self.phase = AppPhase::Editor(EditorState::new(rgba, width, height));
        let (id, open_task) = iced::window::open(iced::window::Settings {
            size: Size::new(980.0, 680.0),
            decorations: false,
            ..Default::default()
        });
        self.windows.insert(id, WindowInfo::Editor);
        Task::batch([close_old, open_task.discard()])
    }

    fn open_iced_overlay_windows(&mut self) -> Task<AppMessage> {
        let mut tasks = Vec::new();
        if self.outputs.is_empty() {
            let (id, open_task) = iced::window::open(iced::window::Settings {
                decorations: false,
                level: iced::window::Level::AlwaysOnTop,
                ..Default::default()
            });
            self.windows.insert(id, WindowInfo::Overlay(None));
            tasks.push(open_task.discard());
            tasks.push(iced::window::change_mode(
                id,
                iced::window::Mode::Fullscreen,
            ));
        } else {
            for out in &self.outputs {
                let (id, open_task) = iced::window::open(iced::window::Settings {
                    position: iced::window::Position::Specific(iced::Point::new(
                        out.x as f32,
                        out.y as f32,
                    )),
                    size: Size::new(out.width as f32, out.height as f32),
                    decorations: false,
                    resizable: false,
                    level: iced::window::Level::AlwaysOnTop,
                    ..Default::default()
                });
                self.windows.insert(
                    id,
                    WindowInfo::Overlay(Some(OutputGeometryKey {
                        name: out.name.clone(),
                    })),
                );
                tasks.push(open_task.discard());
                tasks.push(iced::window::change_mode(
                    id,
                    iced::window::Mode::Fullscreen,
                ));
            }
        }
        Task::batch(tasks)
    }

    fn close_all_windows_task(&mut self) -> Task<AppMessage> {
        let ids: Vec<_> = self.windows.keys().copied().collect();
        self.windows.clear();
        Task::batch(ids.into_iter().map(iced::window::close))
    }

    fn view_core(&self, id: iced::window::Id) -> Element<'_, AppMessage> {
        match &self.phase {
            AppPhase::DelayPill { remaining_secs } => {
                let pill = row![
                    text(format!("Snipping in {remaining_secs}s...")).size(15),
                    button(text("Cancel").size(13))
                        .padding([4, 10])
                        .on_press(AppMessage::Cancel),
                ]
                .spacing(12)
                .align_y(Alignment::Center);
                container(pill)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into()
            }
            AppPhase::Capturing => container(text("Capturing screen...").size(14))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into(),
            AppPhase::Overlay(overlay) => {
                let out = match self.windows.get(&id) {
                    Some(WindowInfo::Overlay(Some(key))) => {
                        self.outputs.iter().find(|o| o.name == key.name)
                    }
                    _ => None,
                };
                overlay.view(out).map(AppMessage::Overlay)
            }
            AppPhase::StartingRecording => container(text("Starting recording...").size(14))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into(),
            AppPhase::Recording {
                elapsed_secs,
                paused,
                busy,
                ..
            } => {
                let mins = elapsed_secs / 60;
                let secs = elapsed_secs % 60;
                let dot_color = if *paused {
                    iced::Color::from_rgb8(245, 158, 11)
                } else {
                    iced::Color::from_rgb8(220, 38, 38)
                };
                let dot = container(text(" ").size(8))
                    .width(Length::Fixed(10.0))
                    .height(Length::Fixed(10.0))
                    .style(move |_theme| container::Style {
                        background: Some(iced::Background::Color(dot_color)),
                        border: iced::Border {
                            radius: 5.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    });

                let timer_drag = mouse_area(
                    row![dot, text(format!("{mins:02}:{secs:02}")).size(15)]
                        .spacing(8)
                        .align_y(Alignment::Center),
                )
                .on_press(AppMessage::DragRecordingBar);

                let (pause_icon, pause_label) = if *paused {
                    (icons::ICON_PLAY, "Retomar")
                } else {
                    (icons::ICON_PAUSE, "Pausar")
                };
                let mut pause_btn = button(
                    row![
                        icons::icon(pause_icon, 14.0, iced::Color::WHITE),
                        text(pause_label).size(13),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .padding([6, 12])
                .style(|_theme, status| {
                    let bg = match status {
                        button::Status::Hovered => iced::Color::from_rgb8(72, 72, 82),
                        button::Status::Pressed => iced::Color::from_rgb8(86, 86, 98),
                        _ => iced::Color::from_rgb8(54, 54, 62),
                    };
                    button::Style {
                        background: Some(iced::Background::Color(bg)),
                        text_color: iced::Color::WHITE,
                        border: iced::Border {
                            radius: 6.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                });
                if !*busy {
                    pause_btn = pause_btn.on_press(AppMessage::TogglePauseRecording);
                }

                let mut stop_btn = button(
                    row![
                        icons::icon(icons::ICON_STOP, 14.0, iced::Color::WHITE),
                        text("Parar").size(13),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .padding([6, 12])
                .style(|_theme, status| {
                    let bg = match status {
                        button::Status::Hovered => iced::Color::from_rgb8(185, 28, 28),
                        button::Status::Pressed => iced::Color::from_rgb8(153, 27, 27),
                        _ => iced::Color::from_rgb8(220, 38, 38),
                    };
                    button::Style {
                        background: Some(iced::Background::Color(bg)),
                        text_color: iced::Color::WHITE,
                        border: iced::Border {
                            radius: 6.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                });
                if !*busy {
                    stop_btn = stop_btn.on_press(AppMessage::StopRecording);
                }

                let bar = row![timer_drag, pause_btn, stop_btn]
                    .spacing(10)
                    .align_y(Alignment::Center);

                container(bar)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .style(|_theme| container::Style {
                        background: Some(iced::Background::Color(iced::Color::from_rgb8(
                            32, 32, 36,
                        ))),
                        text_color: Some(iced::Color::WHITE),
                        border: iced::Border {
                            color: iced::Color::from_rgb8(64, 64, 72),
                            width: 1.0,
                            radius: 8.0.into(),
                        },
                        ..Default::default()
                    })
                    .into()
            }
            AppPhase::RecordingDone {
                path,
                duration_secs,
                status,
            } => {
                let mins = duration_secs / 60;
                let secs = duration_secs % 60;
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("recording");
                let mut col = Column::new()
                    .push(text("Screen Recording Saved").size(18))
                    .push(text(format!("File: {file_name} ({mins:02}:{secs:02})")).size(13))
                    .push(text(format!("Location: {}", path.display())).size(12))
                    .spacing(10);
                if let Some(st) = status {
                    col = col.push(text(st).size(12));
                }
                let actions = row![
                    button(text("Save As...").size(13))
                        .padding([6, 12])
                        .on_press(AppMessage::SaveRecordingAs),
                    button(text("Open Folder").size(13))
                        .padding([6, 12])
                        .on_press(AppMessage::OpenRecordingFolder),
                    button(text("+ New").size(13))
                        .padding([6, 12])
                        .on_press(AppMessage::StartNewFromRecording),
                    button(text("Close").size(13))
                        .padding([6, 12])
                        .on_press(AppMessage::Cancel),
                ]
                .spacing(8);
                col = col.push(actions);
                container(col)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(24)
                    .into()
            }
            AppPhase::Editor(editor) => editor.view().map(AppMessage::Editor),
            AppPhase::History(history) => history.view().map(AppMessage::History),
            AppPhase::Error(err) => container(
                Column::new()
                    .push(text(format!("Error: {err}")).size(14))
                    .push(button(text("Close")).on_press(AppMessage::Cancel))
                    .spacing(12),
            )
            .padding(24)
            .into(),
        }
    }
}

fn schedule_delay_tick() -> Task<AppMessage> {
    Task::perform(
        async {
            tokio::time::sleep(Duration::from_secs(1)).await;
        },
        |()| AppMessage::DelayTick,
    )
}

fn schedule_recording_tick() -> Task<AppMessage> {
    Task::perform(
        async {
            tokio::time::sleep(Duration::from_secs(1)).await;
        },
        |()| AppMessage::RecordingTick,
    )
}

fn listen_tray_cmd(
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<TrayRecordingCmd>>>,
) -> Task<AppMessage> {
    Task::perform(
        async move {
            let mut guard = rx.lock().await;
            guard.recv().await
        },
        AppMessage::TrayRecordingEvent,
    )
}

async fn save_recording_via_portal(src_path: PathBuf) -> Result<Option<PathBuf>, String> {
    let default_name = src_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("recording.mp4")
        .to_string();
    let req = ashpd::desktop::file_chooser::SaveFileRequest::default()
        .title("Save Screen Recording")
        .current_name(default_name.as_str());
    let resp = req
        .send()
        .await
        .map_err(|e| format!("file chooser failed: {e}"))?
        .response()
        .map_err(|e| format!("file chooser response failed: {e}"))?;
    let Some(uri) = resp.uris().first() else {
        return Ok(None);
    };
    let dest = uri
        .to_file_path()
        .map_err(|()| format!("non-file URI returned: {uri}"))?;
    std::fs::copy(&src_path, &dest).map_err(|e| format!("failed to copy recording: {e}"))?;
    Ok(Some(dest))
}

fn start_capture_task(mode: CaptureMode, is_wayland: bool) -> Task<AppMessage> {
    Task::perform(
        async move {
            if is_wayland {
                let backend = PortalCaptureBackend::new();
                let res = if mode == CaptureMode::Window {
                    backend.capture_window().await
                } else {
                    backend.capture_desktop().await
                };
                res.map(|img| (img, backend.supports_window_mode(), Vec::new()))
                    .map_err(|e| e.to_string())
            } else {
                let backend = X11CaptureBackend::new();
                let img = backend.capture_desktop().await.map_err(|e| e.to_string())?;
                let windows = list_x11_windows().unwrap_or_default();
                Ok((img, backend.supports_window_mode(), windows))
            }
        },
        AppMessage::CaptureReady,
    )
}

fn common_subscription() -> Subscription<AppMessage> {
    let keys = keyboard::on_key_press(|key, _mods| match key {
        keyboard::Key::Named(keyboard::key::Named::Escape) => Some(AppMessage::Cancel),
        _ => None,
    });
    let closed = iced::window::close_events().map(AppMessage::WindowClosed);
    Subscription::batch([keys, closed])
}

// --- Runner A: iced::daemon (GNOME Wayland + X11) ---

struct IcedApp {
    core: AppCore,
}

impl IcedApp {
    fn title(&self, _id: iced::window::Id) -> String {
        "LX Snipping Tool".to_string()
    }

    fn update(&mut self, msg: AppMessage) -> Task<AppMessage> {
        self.core.update_core(msg, false)
    }

    fn view(&self, id: iced::window::Id) -> Element<'_, AppMessage> {
        self.core.view_core(id)
    }

    fn subscription(&self) -> Subscription<AppMessage> {
        common_subscription()
    }
}

// --- Runner B: iced_layershell::build_pattern::daemon (KDE/wlroots Wayland) ---

#[to_layer_message(multi, info_name = "WindowInfo")]
#[derive(Debug, Clone)]
enum LayerMsg {
    Core(AppMessage),
}

struct LayerShellApp {
    core: AppCore,
}

impl LayerShellApp {
    fn namespace(&self) -> String {
        "io.github.lx_snippingtool.SnippingTool".to_string()
    }

    fn update(&mut self, msg: LayerMsg) -> Task<LayerMsg> {
        match msg {
            LayerMsg::Core(core_msg) => {
                let was_overlay = matches!(self.core.phase, AppPhase::Overlay(_));
                let task = self.core.update_core(core_msg, true).map(LayerMsg::Core);
                let is_overlay = matches!(self.core.phase, AppPhase::Overlay(_));

                if !was_overlay && is_overlay {
                    let open_layer = Task::done(LayerMsg::NewLayerShell {
                        settings: iced_layershell::reexport::NewLayerShellSettings {
                            layer: Layer::Overlay,
                            keyboard_interactivity: KeyboardInteractivity::Exclusive,
                            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                            ..Default::default()
                        },
                        info: WindowInfo::Overlay(None),
                    });
                    return Task::batch([task, open_layer]);
                }
                task
            }
            _ => Task::none(),
        }
    }

    fn view(&self, id: iced::window::Id) -> Element<'_, LayerMsg> {
        self.core.view_core(id).map(LayerMsg::Core)
    }

    fn subscription(&self) -> Subscription<LayerMsg> {
        common_subscription().map(LayerMsg::Core)
    }

    fn id_info(&self, id: iced::window::Id) -> Option<WindowInfo> {
        self.core.windows.get(&id).cloned()
    }

    fn set_id_info(&mut self, id: iced::window::Id, info: WindowInfo) {
        self.core.windows.insert(id, info);
    }

    fn remove_id(&mut self, id: iced::window::Id) {
        self.core.windows.remove(&id);
    }
}

fn acquire_single_instance_lock() -> Option<File> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let lock_path = runtime_dir.join("lx-snippingtool.lock");
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(err) => {
            warn!(error = %err, path = %lock_path.display(), "failed to open lockfile");
            return None;
        }
    };

    match file.try_lock() {
        Ok(()) => Some(file),
        Err(_) => {
            info!(path = %lock_path.display(), "another lx-snippingtool capture overlay is active; exiting");
            std::process::exit(0);
        }
    }
}

fn should_use_layershell(is_wayland: bool) -> bool {
    if !is_wayland {
        return false;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    !desktop.to_ascii_uppercase().contains("GNOME")
}

fn install_gnome_shortcut() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cmd = format!("{} --mode rect", exe.display());
    let base = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/lx-snippingtool/";

    let out = std::process::Command::new("gsettings")
        .args([
            "get",
            "org.gnome.settings-daemon.plugins.media-keys",
            "custom-keybindings",
        ])
        .output()
        .map_err(|e| format!("failed to run gsettings: {e}"))?;
    let existing = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let updated = if existing == "@as []" || existing.is_empty() {
        format!("['{base}']")
    } else if existing.contains(base) {
        existing
    } else if let Some(stripped) = existing.strip_suffix(']') {
        format!("{stripped}, '{base}']")
    } else {
        format!("['{base}']")
    };

    let schema_path =
        format!("org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{base}");
    for args in [
        vec![
            "set",
            "org.gnome.settings-daemon.plugins.media-keys",
            "custom-keybindings",
            &updated,
        ],
        vec!["set", &schema_path, "name", "LX Snipping Tool"],
        vec!["set", &schema_path, "command", &cmd],
        vec!["set", &schema_path, "binding", "<Control><Shift>s"],
    ] {
        let status = std::process::Command::new("gsettings")
            .args(args)
            .status()
            .map_err(|e| format!("failed to run gsettings set: {e}"))?;
        if !status.success() {
            return Err("gsettings set returned non-zero status".to_string());
        }
    }
    println!("Registered Ctrl+Shift+S -> {cmd}");
    Ok(())
}

fn main() {
    init_telemetry();
    let cli = Cli::parse();

    if cli.install_shortcut {
        if let Err(err) = install_gnome_shortcut() {
            eprintln!("Failed to register shortcut: {err}");
            std::process::exit(1);
        }
        return;
    }

    if cli.tray {
        tray::spawn_tray_if_available();
    }

    let config = match load_or_default() {
        Ok(c) => c,
        Err(err) => {
            warn!(error = %err, "failed to load config; using defaults");
            AppConfig::default()
        }
    };

    let store = SnipStore::new_default(config.history_max_items).unwrap_or_else(|_| {
        SnipStore::new(
            std::env::temp_dir().join("lx-snips"),
            config.history_max_items,
        )
    });

    let mode = cli
        .mode
        .map(CaptureMode::from)
        .unwrap_or(config.default_mode);
    let delay_secs = cli.delay.unwrap_or(config.default_delay_secs);

    let is_capture_invocation = cli.editor.is_none() && !cli.history;
    let mut lock_guard = if is_capture_invocation {
        acquire_single_instance_lock()
    } else {
        None
    };

    let initial_phase = if let Some(path) = &cli.editor {
        match EditorState::from_png_file(path) {
            Ok(ed) => AppPhase::Editor(ed),
            Err(err) => AppPhase::Error(err),
        }
    } else if cli.history {
        AppPhase::History(HistoryState::new(store.clone()))
    } else if delay_secs > 0 {
        AppPhase::DelayPill {
            remaining_secs: delay_secs,
        }
    } else {
        AppPhase::Capturing
    };

    let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let use_layershell = is_capture_invocation && should_use_layershell(is_wayland);
    let pending_clipboard: Arc<Mutex<Option<ClipboardJob>>> = Arc::new(Mutex::new(None));

    if use_layershell {
        let core = AppCore::new(
            initial_phase,
            mode,
            delay_secs,
            config,
            store,
            pending_clipboard.clone(),
            is_wayland,
            lock_guard.take(),
        );
        let res = iced_layershell::build_pattern::daemon(
            LayerShellApp::namespace,
            LayerShellApp::update,
            LayerShellApp::view,
            LayerShellApp::id_info,
            LayerShellApp::set_id_info,
            LayerShellApp::remove_id,
        )
        .subscription(LayerShellApp::subscription)
        .layer_settings(LayerShellSettings {
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            ..Default::default()
        })
        .run_with(move || {
            let mut app = LayerShellApp { core };
            let task = app.core.initial_iced_task().map(LayerMsg::Core);
            (app, task)
        });

        if let Err(err) = res {
            error!(error = %err, "iced_layershell daemon exited with error");
        }
    } else {
        let core = AppCore::new(
            initial_phase,
            mode,
            delay_secs,
            config,
            store,
            pending_clipboard.clone(),
            is_wayland,
            lock_guard.take(),
        );
        let res = iced::daemon(IcedApp::title, IcedApp::update, IcedApp::view)
            .subscription(IcedApp::subscription)
            .run_with(move || {
                let mut app = IcedApp { core };
                let task = app.core.initial_iced_task();
                (app, task)
            });

        if let Err(err) = res {
            error!(error = %err, "iced daemon exited with error");
        }
    }

    // Release the single-instance capture lock before entering the post-copy clipboard-holder loop.
    drop(lock_guard.take());

    if let Some(job) = pending_clipboard.lock().ok().and_then(|mut g| g.take()) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build();
        if let Ok(rt) = rt {
            rt.block_on(async move {
                if !job.snip_path.as_os_str().is_empty() {
                    let path = job.snip_path.clone();
                    tokio::spawn(async move {
                        notification::notify_and_listen_for_edit(path).await;
                    });
                }
                if let Err(err) = AutoClipboardBackend::new()
                    .copy_png_and_hold(job.png_bytes)
                    .await
                {
                    warn!(error = %err, "clipboard holder exited with error");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_modes_and_flags() {
        let cli = Cli::try_parse_from([
            "lx-snippingtool",
            "--mode",
            "freeform",
            "--delay",
            "5",
            "--tray",
        ])
        .unwrap();
        assert!(matches!(cli.mode, Some(CliMode::Freeform)));
        assert_eq!(cli.delay, Some(5));
        assert!(cli.tray);
        assert!(!cli.history);
        assert!(cli.editor.is_none());
    }
}
