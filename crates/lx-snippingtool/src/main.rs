//! Módulo: `main`
//! Objetivos: Ponto de entrada do binário `lx-snippingtool`, gerenciando parsing de CLI, trava de instância única, despacho entre runners (`iced::daemon` e `iced_layershell`), integração com janelas nativas GTK4/Libadwaita, abertura automática de pasta salva e manutenção de clipboard na saída.
//! Principais itens: `Cli`, `AppPhase`, `AppMessage`, `AppCore`, `IcedApp`, `LayerShellApp`, `open_saved_folder`, `main`
//! Última alteração: 2026-11-19

mod capture_wayland;
mod capture_x11;
mod clipboard;
mod editor;
mod history_view;
mod icons;
mod native_ui;
mod notification;
mod overlay;
mod recorder;
mod tray;

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use iced::keyboard;
use iced::widget::{button, container, row, text, Column};
use iced::{Alignment, Element, Length, Size, Subscription, Task};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::LayerShellSettings;
use iced_layershell::{to_layer_message, WindowInfoMarker};
use lx_snip_core::config::{load_or_default, AppConfig, CaptureMode};
use lx_snip_core::export::{encode_rgba, render_and_export, ExportFormat};
use lx_snip_core::platform::{CaptureBackend, CapturedImage, ClipboardBackend, OutputGeometry};
use lx_snip_core::scene::Point;
use lx_snip_core::storage::SnipStore;
use lx_snip_core::telemetry::init_telemetry;
use tracing::{error, info, warn};

use crate::capture_wayland::PortalCaptureBackend;
use crate::capture_x11::{list_x11_windows, X11CaptureBackend, X11WindowBounds};
use crate::clipboard::AutoClipboardBackend;
use crate::editor::{save_as_via_portal, EditorAction, EditorMessage, EditorState, UiTheme};
use crate::history_view::{HistoryAction, HistoryMessage, HistoryState};
use crate::overlay::{OverlayMessage, OverlayOutcome, OverlayState};
use crate::recorder::{ActiveRecording, PreparedRecording};
use crate::tray::{RecordingTrayHandle, TrayRecordingCmd};

/// Opções de modo de captura aceitas via argumento `--mode` da linha de comando.
/// Armazena: variantes `Rect`, `Freeform`, `Window`, `Full` e `Record` | Principais métodos: `From<CliMode> for CaptureMode`
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliMode {
    Rect,
    Freeform,
    Window,
    Full,
    Record,
}

impl From<CliMode> for CaptureMode {
    /// Converte o modo informado na linha de comando (`CliMode`) para o `CaptureMode` interno.
    /// Input: `value` (`CliMode`) | Output: variante correspondente de `CaptureMode`
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

/// Argumentos de linha de comando suportados pelo executável `lx-snippingtool`.
/// Armazena: `mode`, `delay`, `editor`, `history`, `tray`, `install_shortcut` | Principais métodos: `Parser::parse` (derivado via `clap`)
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

/// Trabalho pendente de cópia para a área de transferência executado imediatamente após o loop UI encerrar.
/// Armazena: `png_bytes`, `snip_path` | Principais métodos: nenhum (estrutura de transferência)
#[derive(Debug, Clone)]
struct ClipboardJob {
    png_bytes: Vec<u8>,
    snip_path: PathBuf,
}

/// Wrapper clonável com mutex assíncrono em torno da gravação de tela ativa.
/// Armazena: `Arc<tokio::sync::Mutex<Option<ActiveRecording>>>` | Principais métodos: `Debug::fmt`
#[derive(Clone)]
struct SharedRecording(Arc<tokio::sync::Mutex<Option<ActiveRecording>>>);

impl std::fmt::Debug for SharedRecording {
    /// Formata uma representação opaca de `SharedRecording` para logs e depuração.
    /// Input: `&self`, `f` (`&mut std::fmt::Formatter<'_>`) | Output: `std::fmt::Result`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedRecording").finish()
    }
}

/// Wrapper clonável com mutex assíncrono em torno da gravação preparada para iniciar após o aviso.
/// Armazena: `Arc<tokio::sync::Mutex<Option<PreparedRecording>>>` | Principais métodos: `Debug::fmt`
#[derive(Clone)]
struct SharedPreparedRecording(Arc<tokio::sync::Mutex<Option<PreparedRecording>>>);

impl std::fmt::Debug for SharedPreparedRecording {
    /// Formata uma representação opaca de `SharedPreparedRecording` para logs e depuração.
    /// Input: `&self`, `f` (`&mut std::fmt::Formatter<'_>`) | Output: `std::fmt::Result`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedPreparedRecording").finish()
    }
}

/// Pacote retornado ao iniciar uma gravação reunindo o controlador de vídeo e o canal da bandeja.
/// Armazena: `recording`, `tray`, `tray_rx` | Principais métodos: `Debug::fmt`
#[derive(Clone)]
struct RecordingSessionBundle {
    recording: SharedRecording,
    tray: Option<RecordingTrayHandle>,
    tray_rx:
        Option<Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<TrayRecordingCmd>>>>,
}

impl std::fmt::Debug for RecordingSessionBundle {
    /// Formata `RecordingSessionBundle` exibindo se há manipulador de bandeja acoplado.
    /// Input: `&self`, `f` (`&mut std::fmt::Formatter<'_>`) | Output: `std::fmt::Result`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingSessionBundle")
            .field("has_tray", &self.tray.is_some())
            .finish()
    }
}

/// Identificador lógico do papel de cada janela aberta pelos daemons `iced` ou `iced_layershell`.
/// Armazena: variantes `DelayPill`, `Overlay`, `Editor`, `History`, `RecordingToast`, `RecordingBar`, `RecordingDone` e `Error` | Principais métodos: nenhum (marcador `WindowInfoMarker`)
#[derive(Debug, Clone, PartialEq, Eq, WindowInfoMarker)]
enum WindowInfo {
    DelayPill,
    Overlay(Option<OutputGeometryKey>),
    Editor,
    History,
    RecordingToast,
    RecordingBar,
    RecordingDone,
    Error,
}

/// Chave equiparável usada para mapear uma janela de overlay ao nome de seu monitor físico.
/// Armazena: `name` (`String`) | Principais métodos: nenhum (tipo de chave)
#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputGeometryKey {
    name: String,
}

/// Máquina de estados de alto nível da aplicação cobrindo atraso, overlay, contagem, gravação, saída, editor e histórico.
/// Armazena: variantes `DelayPill`, `Capturing`, `Overlay`, `StartingRecording`, `RecordingCountdown`, `Recording`, `RecordingDone`, `Exiting`, `Editor`, `History` e `Error` | Principais métodos: nenhum (estado da aplicação)
#[derive(Debug, Clone)]
enum AppPhase {
    DelayPill {
        remaining_secs: u8,
        using_native: bool,
    },
    Capturing,
    Overlay(OverlayState),
    StartingRecording,
    RecordingCountdown {
        remaining_secs: u8,
    },
    Recording {
        elapsed_secs: u64,
        paused: bool,
        busy: bool,
        floating_bar_visible: bool,
    },
    RecordingDone {
        temp_path: PathBuf,
        default_save_path: PathBuf,
        duration_secs: u64,
        status: Option<String>,
        busy: bool,
        using_native: bool,
    },
    Exiting,
    Editor(EditorState),
    History(HistoryState),
    Error(String),
}

/// Eventos e comandos assíncronos trafegados no barramento central de mensagens `AppCore`.
/// Armazena: eventos de captura, overlay, editor, histórico, contagem, gravação, bandeja, janelas nativas GTK4, diálogos e encerramento | Principais métodos: nenhum (enumeração de mensagens)
#[derive(Debug, Clone)]
enum AppMessage {
    DelayTick,
    NativeDelayCancelled,
    Cancel,
    CaptureReady(Result<(CapturedImage, bool, Vec<X11WindowBounds>), String>),
    Overlay(OverlayMessage),
    Editor(EditorMessage),
    History(HistoryMessage),
    RecordingPrepared(Result<SharedPreparedRecording, String>),
    RecordingCountdownTick,
    RecordingStarted(Result<RecordingSessionBundle, String>),
    RecordingTick,
    TogglePauseRecording,
    PauseToggled(Result<bool, String>),
    TrayRecordingEvent(Option<TrayRecordingCmd>),
    NativeBarEvent(Option<native_ui::NativeBarEvent>),
    NativeDoneEvent(Option<native_ui::NativeDoneEvent>),
    NativeEditorEvent(Option<native_ui::NativeEditorEvent>),
    NativeHistoryEvent(Option<native_ui::NativeHistoryEvent>),
    StopRecording,
    RecordingStopped(Result<(PathBuf, u64), String>),
    SaveRecordingDefault,
    SaveRecordingAs,
    SaveRecordingDone(Result<Option<PathBuf>, String>),
    DiscardRecording,
    FinishExit,
    WindowClosed(iced::window::Id),
}

/// Núcleo de estado e lógica compartilhado entre os daemons `IcedApp` e `LayerShellApp`.
/// Armazena: `phase`, `mode`, `delay_secs`, `config`, `store`, `windows`, `outputs`, `pending_clipboard`, `is_wayland`, `system_dark`, `capture_lock`, `prepared_recording`, `active_recording`, `recording_tray`, `recording_tray_rx`, `native_delay`, `native_bar`, `native_bar_rx`, `native_done`, `native_done_rx`, `native_editor`, `native_editor_rx`, `native_history`, `native_history_rx` | Principais métodos: `new`, `initial_iced_task`, `update_core`, `view_core`, `open_editor_window`, `open_history_window`, `open_error_window`, `current_dark_theme`
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
    system_dark: bool,
    capture_lock: Option<File>,
    prepared_recording: Option<SharedPreparedRecording>,
    active_recording: Option<SharedRecording>,
    recording_tray: Option<RecordingTrayHandle>,
    recording_tray_rx:
        Option<Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<TrayRecordingCmd>>>>,
    native_delay: Option<native_ui::NativeDelayHandle>,
    native_bar: Option<native_ui::NativeBarHandle>,
    native_bar_rx: Option<
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeBarEvent>>>,
    >,
    native_done: Option<native_ui::NativeDoneHandle>,
    native_done_rx: Option<
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeDoneEvent>>>,
    >,
    native_editor: Option<native_ui::NativeEditorHandle>,
    native_editor_rx: Option<
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeEditorEvent>>>,
    >,
    native_history: Option<native_ui::NativeHistoryHandle>,
    native_history_rx: Option<
        Arc<
            tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeHistoryEvent>>,
        >,
    >,
}

impl AppCore {
    /// Constrói a instância inicial de `AppCore` a partir dos argumentos resolvidos, configuração e travas.
    /// Input: `initial_phase`, `mode`, `delay_secs`, `config`, `store`, `pending_clipboard`, `is_wayland`, `system_dark`, `capture_lock` | Output: instância de `AppCore`
    #[allow(clippy::too_many_arguments)]
    fn new(
        initial_phase: AppPhase,
        mode: CaptureMode,
        delay_secs: u8,
        config: AppConfig,
        store: SnipStore,
        pending_clipboard: Arc<Mutex<Option<ClipboardJob>>>,
        is_wayland: bool,
        system_dark: bool,
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
            system_dark,
            capture_lock,
            prepared_recording: None,
            active_recording: None,
            recording_tray: None,
            recording_tray_rx: None,
            native_delay: None,
            native_bar: None,
            native_bar_rx: None,
            native_done: None,
            native_done_rx: None,
            native_editor: None,
            native_editor_rx: None,
            native_history: None,
            native_history_rx: None,
        }
    }

    /// Dispara as janelas e tarefas assíncronas iniciais ao subir o runner `iced::daemon`.
    /// Input: `&mut self` | Output: `Task<AppMessage>` de boot
    fn initial_iced_task(&mut self) -> Task<AppMessage> {
        let initial_delay = match &self.phase {
            AppPhase::DelayPill { remaining_secs, .. } => Some(*remaining_secs),
            _ => None,
        };
        if let Some(secs) = initial_delay {
            if native_ui::should_use_native_gtk() {
                if let Ok((handle, rx)) = native_ui::spawn_delay_dialog(secs) {
                    if let AppPhase::DelayPill { using_native, .. } = &mut self.phase {
                        *using_native = true;
                    }
                    self.native_delay = Some(handle);
                    return Task::batch([schedule_delay_tick(), listen_native_delay(rx)]);
                }
            }
            let (id, open_task) = iced::window::open(iced::window::Settings {
                size: Size::new(280.0, 68.0),
                decorations: true,
                resizable: false,
                level: iced::window::Level::AlwaysOnTop,
                ..Default::default()
            });
            self.windows.insert(id, WindowInfo::DelayPill);
            return Task::batch([open_task.discard(), schedule_delay_tick()]);
        }

        let startup_err = match &self.phase {
            AppPhase::Error(err) => Some(err.clone()),
            _ => None,
        };
        if let Some(err) = startup_err {
            return self.open_error_window(err);
        }

        if matches!(self.phase, AppPhase::Editor(_)) {
            if let AppPhase::Editor(editor) =
                std::mem::replace(&mut self.phase, AppPhase::Capturing)
            {
                return self.open_editor_window(editor);
            }
        }
        if matches!(self.phase, AppPhase::History(_)) {
            return self.open_history_window();
        }

        match &self.phase {
            AppPhase::Capturing => start_capture_task(self.mode, self.is_wayland),
            AppPhase::DelayPill { .. }
            | AppPhase::Editor(_)
            | AppPhase::History(_)
            | AppPhase::Overlay(_)
            | AppPhase::StartingRecording
            | AppPhase::RecordingCountdown { .. }
            | AppPhase::Recording { .. }
            | AppPhase::RecordingDone { .. }
            | AppPhase::Exiting
            | AppPhase::Error(_) => Task::none(),
        }
    }

    /// Função de transição central que processa `AppMessage` e coordena janelas, captura, gravação, editor e histórico.
    /// Input: `&mut self`, `msg` (`AppMessage`), `use_layershell` (`bool`) | Output: `Task<AppMessage>` com comandos assíncronos resultantes
    fn update_core(&mut self, msg: AppMessage, use_layershell: bool) -> Task<AppMessage> {
        match msg {
            AppMessage::DelayTick => {
                if let AppPhase::DelayPill { remaining_secs, .. } = &mut self.phase {
                    if *remaining_secs > 1 {
                        *remaining_secs -= 1;
                        if let Some(handle) = &self.native_delay {
                            handle.update_secs(*remaining_secs);
                        }
                        return schedule_delay_tick();
                    }
                    self.phase = AppPhase::Capturing;
                    self.close_native_delay();
                    let close_all = self.close_all_windows_task();
                    let cap = start_capture_task(self.mode, self.is_wayland);
                    return Task::batch([close_all, cap]);
                }
                Task::none()
            }
            AppMessage::NativeDelayCancelled => {
                if matches!(self.phase, AppPhase::DelayPill { .. }) {
                    self.close_native_delay();
                    return iced::exit();
                }
                Task::none()
            }
            AppMessage::Cancel => {
                if matches!(self.phase, AppPhase::Recording { .. } | AppPhase::Exiting) {
                    // Do not abort active recordings or interrupt folder opening on Escape.
                    Task::none()
                } else if matches!(self.phase, AppPhase::RecordingCountdown { .. }) {
                    self.cancel_prepared_recording_task()
                } else {
                    self.close_native_delay();
                    self.close_native_editor();
                    self.close_native_history();
                    self.discard_recording_task()
                }
            }
            AppMessage::WindowClosed(id) => {
                let closed_info = self.windows.remove(&id);
                if matches!(self.phase, AppPhase::RecordingCountdown { .. }) {
                    if matches!(closed_info, Some(WindowInfo::RecordingToast)) {
                        return self.cancel_prepared_recording_task();
                    }
                    return Task::none();
                }
                if matches!(self.phase, AppPhase::Recording { .. }) {
                    if matches!(closed_info, Some(WindowInfo::RecordingToast)) {
                        return Task::none();
                    }
                    let has_bar = self.native_bar.is_some()
                        || self
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
                let has_native_window = self.native_done.is_some()
                    || self.native_delay.is_some()
                    || self.native_editor.is_some()
                    || self.native_history.is_some();
                if self.windows.is_empty()
                    && !has_native_window
                    && !matches!(
                        self.phase,
                        AppPhase::Capturing | AppPhase::StartingRecording | AppPhase::Exiting
                    )
                {
                    if let AppPhase::RecordingDone { temp_path, .. } = &self.phase {
                        let _ = std::fs::remove_file(temp_path);
                    }
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
                    )
                    .with_dark_theme(self.system_dark);
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
                        self.open_error_window(err)
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
                        let prepare_task = Task::perform(
                            async move {
                                let rec = recorder::prepare_recording(
                                    is_wayland,
                                    target,
                                    with_system_audio,
                                    with_mic,
                                )
                                .await?;
                                Ok(SharedPreparedRecording(Arc::new(tokio::sync::Mutex::new(
                                    Some(rec),
                                ))))
                            },
                            AppMessage::RecordingPrepared,
                        );
                        Task::batch([close_old, prepare_task])
                    }
                    Some(OverlayOutcome::Cancelled) => iced::exit(),
                    None => Task::none(),
                }
            }
            AppMessage::Editor(ed_msg) => {
                let is_save_completed = matches!(ed_msg, EditorMessage::SaveCompleted(_));
                let action = {
                    let AppPhase::Editor(editor) = &mut self.phase else {
                        return Task::none();
                    };
                    editor.update(ed_msg)
                };
                if is_save_completed {
                    if let (AppPhase::Editor(editor), Some(handle)) =
                        (&self.phase, &self.native_editor)
                    {
                        handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                    }
                }
                match action {
                    Some(EditorAction::CopyAndExit(png_bytes)) => {
                        self.copy_editor_png_task(png_bytes);
                        if let AppPhase::Editor(editor) = &mut self.phase {
                            editor.status_text = Some("Copied to clipboard".to_string());
                        }
                        Task::none()
                    }
                    Some(EditorAction::NewCapture) => {
                        self.close_native_editor();
                        let close_old = self.close_all_windows_task();
                        self.phase = AppPhase::Capturing;
                        Task::batch([close_old, start_capture_task(self.mode, self.is_wayland)])
                    }
                    Some(EditorAction::OpenHistory) => self.open_history_window(),
                    Some(EditorAction::RunSaveAs {
                        base_rgba,
                        width,
                        height,
                        scene,
                    }) => Task::perform(
                        async move {
                            let res = save_as_via_portal(base_rgba, width, height, scene).await;
                            if let Ok(Some(ref saved_path)) = res {
                                open_saved_folder(saved_path.clone()).await;
                            }
                            res
                        },
                        |res| AppMessage::Editor(EditorMessage::SaveCompleted(res)),
                    ),
                    None => Task::none(),
                }
            }
            AppMessage::NativeEditorEvent(Some(ev)) => {
                if !matches!(self.phase, AppPhase::Editor(_)) {
                    return Task::none();
                }
                let mut rearm = true;
                let task = match ev {
                    native_ui::NativeEditorEvent::PushOp(op) => {
                        if let AppPhase::Editor(editor) = &mut self.phase {
                            editor.scene.push_op(op);
                            editor.status_text = None;
                            if let Some(handle) = &self.native_editor {
                                handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                            }
                        }
                        Task::none()
                    }
                    native_ui::NativeEditorEvent::Erase { x, y, radius } => {
                        if let AppPhase::Editor(editor) = &mut self.phase {
                            if editor.scene.erase_at(Point::new(x, y), radius) {
                                editor.status_text = None;
                                if let Some(handle) = &self.native_editor {
                                    handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                                }
                            }
                        }
                        Task::none()
                    }
                    native_ui::NativeEditorEvent::SetCrop(crop) => {
                        if let AppPhase::Editor(editor) = &mut self.phase {
                            editor.scene.set_crop(crop);
                            editor.status_text = None;
                            if let Some(handle) = &self.native_editor {
                                handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                            }
                        }
                        Task::none()
                    }
                    native_ui::NativeEditorEvent::Undo => {
                        if let AppPhase::Editor(editor) = &mut self.phase {
                            if editor.scene.undo() {
                                if let Some(handle) = &self.native_editor {
                                    handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                                }
                            }
                        }
                        Task::none()
                    }
                    native_ui::NativeEditorEvent::Redo => {
                        if let AppPhase::Editor(editor) = &mut self.phase {
                            if editor.scene.redo() {
                                if let Some(handle) = &self.native_editor {
                                    handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                                }
                            }
                        }
                        Task::none()
                    }
                    native_ui::NativeEditorEvent::Copy => {
                        let rendered = if let AppPhase::Editor(editor) = &self.phase {
                            render_and_export(
                                &editor.base_rgba,
                                editor.width,
                                editor.height,
                                &editor.scene,
                                ExportFormat::Png,
                            )
                        } else {
                            return Task::none();
                        };
                        match rendered {
                            Ok(png_bytes) => {
                                self.copy_editor_png_task(png_bytes);
                                if let AppPhase::Editor(editor) = &mut self.phase {
                                    editor.status_text = Some("Copied to clipboard".to_string());
                                }
                            }
                            Err(err) => {
                                if let AppPhase::Editor(editor) = &mut self.phase {
                                    editor.status_text = Some(format!("Export failed: {err}"));
                                }
                            }
                        }
                        if let (AppPhase::Editor(editor), Some(handle)) =
                            (&self.phase, &self.native_editor)
                        {
                            handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                        }
                        Task::none()
                    }
                    native_ui::NativeEditorEvent::Save => {
                        let AppPhase::Editor(editor) = &self.phase else {
                            return Task::none();
                        };
                        let base_rgba = editor.base_rgba.clone();
                        let width = editor.width;
                        let height = editor.height;
                        let scene = editor.scene.clone();
                        Task::perform(
                            async move {
                                let res = save_as_via_portal(base_rgba, width, height, scene).await;
                                if let Ok(Some(ref saved_path)) = res {
                                    open_saved_folder(saved_path.clone()).await;
                                }
                                res
                            },
                            |res| AppMessage::Editor(EditorMessage::SaveCompleted(res)),
                        )
                    }
                    native_ui::NativeEditorEvent::NewCapture => {
                        rearm = false;
                        self.close_native_editor();
                        let close_old = self.close_all_windows_task();
                        self.phase = AppPhase::Capturing;
                        Task::batch([close_old, start_capture_task(self.mode, self.is_wayland)])
                    }
                    native_ui::NativeEditorEvent::OpenHistory => {
                        rearm = false;
                        self.open_history_window()
                    }
                    native_ui::NativeEditorEvent::Closed => {
                        rearm = false;
                        self.close_native_editor();
                        iced::exit()
                    }
                };
                if rearm {
                    if let Some(rx) = self.native_editor_rx.clone() {
                        return Task::batch([task, listen_native_editor(rx)]);
                    }
                }
                task
            }
            AppMessage::NativeEditorEvent(None) => {
                if self.native_editor.is_some() && matches!(self.phase, AppPhase::Editor(_)) {
                    self.close_native_editor();
                    return iced::exit();
                }
                Task::none()
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
                            Ok(editor) => self.open_editor_window(
                                editor.with_theme(UiTheme::from_dark(self.system_dark)),
                            ),
                            Err(err) => self.open_error_window(err),
                        }
                    }
                    None => Task::none(),
                }
            }
            AppMessage::NativeHistoryEvent(Some(ev)) => {
                if !matches!(self.phase, AppPhase::History(_)) {
                    return Task::none();
                }
                let mut rearm = true;
                let task = match ev {
                    native_ui::NativeHistoryEvent::Refresh => {
                        if let Some(handle) = &self.native_history {
                            sync_history_to_native(handle, &self.store, None);
                        }
                        Task::none()
                    }
                    native_ui::NativeHistoryEvent::Copy(path) => match std::fs::read(&path) {
                        Ok(png_bytes) => {
                            rearm = false;
                            if let Ok(mut guard) = self.pending_clipboard.lock() {
                                *guard = Some(ClipboardJob {
                                    png_bytes,
                                    snip_path: path,
                                });
                            }
                            self.close_native_history();
                            iced::exit()
                        }
                        Err(err) => {
                            if let Some(handle) = &self.native_history {
                                sync_history_to_native(
                                    handle,
                                    &self.store,
                                    Some(&format!("Failed to read {}: {err}", path.display())),
                                );
                            }
                            Task::none()
                        }
                    },
                    native_ui::NativeHistoryEvent::Edit(path) => {
                        rearm = false;
                        match EditorState::from_png_file(&path) {
                            Ok(editor) => self.open_editor_window(
                                editor.with_theme(UiTheme::from_dark(self.system_dark)),
                            ),
                            Err(err) => {
                                self.close_native_history();
                                self.open_error_window(err)
                            }
                        }
                    }
                    native_ui::NativeHistoryEvent::Delete(path) => {
                        if let Err(err) = self.store.delete(&path) {
                            if let Some(handle) = &self.native_history {
                                sync_history_to_native(
                                    handle,
                                    &self.store,
                                    Some(&format!("Failed to delete: {err}")),
                                );
                            }
                        } else if let Some(handle) = &self.native_history {
                            sync_history_to_native(handle, &self.store, None);
                        }
                        Task::none()
                    }
                    native_ui::NativeHistoryEvent::Closed => {
                        rearm = false;
                        self.close_native_history();
                        iced::exit()
                    }
                };
                if rearm {
                    if let Some(rx) = self.native_history_rx.clone() {
                        return Task::batch([task, listen_native_history(rx)]);
                    }
                }
                task
            }
            AppMessage::NativeHistoryEvent(None) => {
                if self.native_history.is_some() && matches!(self.phase, AppPhase::History(_)) {
                    self.close_native_history();
                    return iced::exit();
                }
                Task::none()
            }
            AppMessage::RecordingPrepared(res) => match res {
                Ok(prepared) => {
                    if native_ui::should_use_native_gtk() {
                        self.phase = AppPhase::StartingRecording;
                        return Task::perform(
                            async move {
                                let toast_res =
                                    tokio::task::spawn_blocking(native_ui::run_recording_toast)
                                        .await;
                                match toast_res {
                                    Ok(Ok(false)) => {
                                        if let Some(rec) = prepared.0.lock().await.take() {
                                            rec.cancel().await;
                                        }
                                        return Err("cancelled".to_string());
                                    }
                                    Ok(Ok(true)) => {
                                        // Wait briefly so the compositor finishes unmapping the countdown window before frame #1.
                                        tokio::time::sleep(Duration::from_millis(180)).await;
                                    }
                                    Ok(Err(err)) => {
                                        warn!(error = %err, "native countdown toast failed; starting recording immediately");
                                    }
                                    Err(err) => {
                                        warn!(error = %err, "native countdown toast task panicked; starting recording immediately");
                                    }
                                }
                                start_prepared_recording_bundle(prepared).await
                            },
                            AppMessage::RecordingStarted,
                        );
                    }
                    self.prepared_recording = Some(prepared);
                    self.phase = AppPhase::RecordingCountdown { remaining_secs: 2 };
                    let (id, open_task) = iced::window::open(iced::window::Settings {
                        size: Size::new(480.0, 84.0),
                        decorations: true,
                        resizable: false,
                        level: iced::window::Level::AlwaysOnTop,
                        ..Default::default()
                    });
                    self.windows.insert(id, WindowInfo::RecordingToast);
                    Task::batch([open_task.discard(), schedule_recording_countdown_tick(1000)])
                }
                Err(err) => {
                    warn!(error = %err, "recording failed to prepare");
                    if err.to_ascii_lowercase().contains("cancelled") {
                        iced::exit()
                    } else {
                        self.open_error_window(err)
                    }
                }
            },
            AppMessage::RecordingCountdownTick => {
                let AppPhase::RecordingCountdown { remaining_secs } = &mut self.phase else {
                    return Task::none();
                };
                if *remaining_secs > 1 {
                    *remaining_secs -= 1;
                    schedule_recording_countdown_tick(1000)
                } else if *remaining_secs == 1 {
                    *remaining_secs = 0;
                    schedule_recording_countdown_tick(250)
                } else {
                    let close_toast =
                        self.close_windows_where(|info| matches!(info, WindowInfo::RecordingToast));
                    self.phase = AppPhase::StartingRecording;
                    let Some(prepared) = self.prepared_recording.take() else {
                        return iced::exit();
                    };
                    let start_task = Task::perform(
                        async move {
                            tokio::time::sleep(Duration::from_millis(180)).await;
                            start_prepared_recording_bundle(prepared).await
                        },
                        AppMessage::RecordingStarted,
                    );
                    Task::batch([close_toast, start_task])
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
                    if has_tray {
                        tasks.push(self.update_tray_task());
                    } else {
                        tasks.push(self.open_recording_bar_window());
                    }
                    Task::batch(tasks)
                }
                Err(err) => {
                    warn!(error = %err, "recording failed to start");
                    if err.to_ascii_lowercase().contains("cancelled") {
                        iced::exit()
                    } else {
                        self.open_error_window(err)
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
                    self.sync_recording_bar();
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
                    self.sync_recording_bar();
                    self.update_tray_task()
                }
                Err(err) => {
                    warn!(error = %err, "failed to toggle recording pause");
                    if let AppPhase::Recording { busy, .. } = &mut self.phase {
                        *busy = false;
                    }
                    self.sync_recording_bar();
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
                            tasks.push(self.close_recording_bar());
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
            AppMessage::NativeBarEvent(Some(ev)) => {
                if !matches!(self.phase, AppPhase::Recording { .. }) {
                    return Task::none();
                }
                match ev {
                    native_ui::NativeBarEvent::TogglePause => {
                        let task = self.toggle_pause_task();
                        if let Some(rx) = self.native_bar_rx.clone() {
                            Task::batch([task, listen_native_bar(rx)])
                        } else {
                            task
                        }
                    }
                    native_ui::NativeBarEvent::Stop => self.stop_recording_task(),
                    native_ui::NativeBarEvent::Closed => {
                        self.native_bar = None;
                        self.native_bar_rx = None;
                        if self.recording_tray.is_some() {
                            if let AppPhase::Recording {
                                floating_bar_visible,
                                ..
                            } = &mut self.phase
                            {
                                *floating_bar_visible = false;
                            }
                            self.update_tray_task()
                        } else {
                            self.stop_recording_task()
                        }
                    }
                }
            }
            AppMessage::NativeBarEvent(None) => {
                if self.native_bar.is_some() && matches!(self.phase, AppPhase::Recording { .. }) {
                    self.native_bar = None;
                    self.native_bar_rx = None;
                    if self.recording_tray.is_some() {
                        if let AppPhase::Recording {
                            floating_bar_visible,
                            ..
                        } = &mut self.phase
                        {
                            *floating_bar_visible = false;
                        }
                        return self.update_tray_task();
                    } else {
                        return self.stop_recording_task();
                    }
                }
                Task::none()
            }
            AppMessage::NativeDoneEvent(Some(ev)) => {
                if !matches!(self.phase, AppPhase::RecordingDone { .. }) {
                    return Task::none();
                }
                let task = match ev {
                    native_ui::NativeDoneEvent::Save => self.save_recording_default_task(),
                    native_ui::NativeDoneEvent::SaveAs => self.save_recording_as_task(),
                    native_ui::NativeDoneEvent::Discard => self.discard_recording_task(),
                };
                if let Some(rx) = self.native_done_rx.clone() {
                    Task::batch([task, listen_native_done(rx)])
                } else {
                    task
                }
            }
            AppMessage::NativeDoneEvent(None) => {
                if let AppPhase::RecordingDone {
                    using_native: true, ..
                } = &self.phase
                {
                    return self.discard_recording_task();
                }
                Task::none()
            }
            AppMessage::StopRecording => self.stop_recording_task(),
            AppMessage::RecordingStopped(res) => {
                self.close_native_done();
                match res {
                    Ok((temp_path, duration_secs)) => {
                        let close_old = self.close_all_windows_task();
                        let ext = temp_path
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("mp4");
                        let default_save_path = recorder::default_recording_save_path(ext);

                        if native_ui::should_use_native_gtk() {
                            if let Ok((handle, rx)) = native_ui::spawn_recording_done(
                                duration_secs,
                                ext,
                                &default_save_path,
                            ) {
                                let rx = Arc::new(tokio::sync::Mutex::new(rx));
                                self.native_done = Some(handle);
                                self.native_done_rx = Some(rx.clone());
                                self.phase = AppPhase::RecordingDone {
                                    temp_path,
                                    default_save_path,
                                    duration_secs,
                                    status: None,
                                    busy: false,
                                    using_native: true,
                                };
                                return Task::batch([close_old, listen_native_done(rx)]);
                            }
                        }

                        self.phase = AppPhase::RecordingDone {
                            temp_path,
                            default_save_path,
                            duration_secs,
                            status: None,
                            busy: false,
                            using_native: false,
                        };
                        let (id, open_task) = iced::window::open(iced::window::Settings {
                            size: Size::new(540.0, 210.0),
                            decorations: true,
                            resizable: false,
                            ..Default::default()
                        });
                        self.windows.insert(id, WindowInfo::RecordingDone);
                        Task::batch([close_old, open_task.discard()])
                    }
                    Err(err) => self.open_error_window(err),
                }
            }
            AppMessage::SaveRecordingDefault => self.save_recording_default_task(),
            AppMessage::SaveRecordingAs => self.save_recording_as_task(),
            AppMessage::SaveRecordingDone(res) => {
                let AppPhase::RecordingDone { temp_path, .. } = &self.phase else {
                    return Task::none();
                };
                match res {
                    Ok(Some(dest)) => {
                        if *temp_path != dest {
                            let _ = std::fs::remove_file(temp_path);
                        }
                        self.close_native_done();
                        let close_old = self.close_all_windows_task();
                        self.phase = AppPhase::Exiting;
                        Task::batch([
                            close_old,
                            Task::perform(open_saved_folder(dest), |()| AppMessage::FinishExit),
                        ])
                    }
                    Ok(None) => {
                        if let AppPhase::RecordingDone { busy, status, .. } = &mut self.phase {
                            *busy = false;
                            *status = None;
                        }
                        if let Some(done) = &self.native_done {
                            done.send_idle();
                        }
                        Task::none()
                    }
                    Err(err) => {
                        let msg = format!("Erro ao salvar: {err}");
                        if let AppPhase::RecordingDone { busy, status, .. } = &mut self.phase {
                            *busy = false;
                            *status = Some(msg.clone());
                        }
                        if let Some(done) = &self.native_done {
                            done.send_error(&msg);
                        }
                        Task::none()
                    }
                }
            }
            AppMessage::DiscardRecording => self.discard_recording_task(),
            AppMessage::FinishExit => iced::exit(),
        }
    }

    /// Cancela a gravação preparada durante a contagem regressiva e libera a sessão de portal antes de sair.
    /// Input: `&mut self` | Output: `Task<AppMessage>` com `AppMessage::FinishExit`
    fn cancel_prepared_recording_task(&mut self) -> Task<AppMessage> {
        let close_old = self.close_all_windows_task();
        self.phase = AppPhase::Exiting;
        let prepared = self.prepared_recording.take();
        Task::batch([
            close_old,
            Task::perform(
                async move {
                    if let Some(p) = prepared {
                        if let Some(rec) = p.0.lock().await.take() {
                            rec.cancel().await;
                        }
                    }
                },
                |()| AppMessage::FinishExit,
            ),
        ])
    }

    /// Abre a janela de controles de gravação (`RecordingBar`) via GTK4 nativo ou janela de sistema fallback.
    /// Input: `&mut self` | Output: `Task<AppMessage>` (ou `Task::none()` se já aberta)
    fn open_recording_bar_window(&mut self) -> Task<AppMessage> {
        let (elapsed_secs, paused, busy) = match self.phase {
            AppPhase::Recording {
                elapsed_secs,
                paused,
                busy,
                ..
            } => (elapsed_secs, paused, busy),
            _ => (0, false, false),
        };
        if native_ui::should_use_native_gtk() {
            if self.native_bar.is_some() {
                return Task::none();
            }
            if let Ok((handle, rx)) = native_ui::spawn_recording_bar(elapsed_secs, paused, busy) {
                let rx = Arc::new(tokio::sync::Mutex::new(rx));
                self.native_bar = Some(handle);
                self.native_bar_rx = Some(rx.clone());
                return listen_native_bar(rx);
            }
        }
        if self
            .windows
            .values()
            .any(|info| matches!(info, WindowInfo::RecordingBar))
        {
            return Task::none();
        }
        let (id, open_task) = iced::window::open(iced::window::Settings {
            size: Size::new(310.0, 52.0),
            decorations: true,
            resizable: false,
            level: iced::window::Level::AlwaysOnTop,
            ..Default::default()
        });
        self.windows.insert(id, WindowInfo::RecordingBar);
        open_task.discard()
    }

    /// Fecha a barra flutuante de gravação ativa (janela nativa GTK4 ou janela Iced fallback).
    /// Input: `&mut self` | Output: `Task<AppMessage>` fechando eventuais janelas Iced
    fn close_recording_bar(&mut self) -> Task<AppMessage> {
        if let Some(handle) = self.native_bar.take() {
            handle.close();
        }
        self.native_bar_rx = None;
        self.close_windows_where(|info| matches!(info, WindowInfo::RecordingBar))
    }

    /// Sincroniza cronômetro, estado de pausa e ocupação na barra nativa GTK4, quando ativa.
    /// Input: `&self` | Output: `()`
    fn sync_recording_bar(&self) {
        let Some(handle) = &self.native_bar else {
            return;
        };
        if let AppPhase::Recording {
            elapsed_secs,
            paused,
            busy,
            ..
        } = self.phase
        {
            handle.update(elapsed_secs, paused, busy);
        }
    }

    /// Fecha imediatamente o diálogo nativo `Gravação concluída` e limpa seus manipuladores.
    /// Input: `&mut self` | Output: `()`
    fn close_native_done(&mut self) {
        if let Some(handle) = self.native_done.take() {
            handle.send_close();
        }
        self.native_done_rx = None;
    }

    /// Salva a gravação no caminho padrão, abre a pasta de destino e encerra a interface sem tela secundária.
    /// Input: `&mut self` | Output: `Task<AppMessage>`
    fn save_recording_default_task(&mut self) -> Task<AppMessage> {
        let AppPhase::RecordingDone {
            temp_path,
            default_save_path,
            status,
            busy,
            ..
        } = &mut self.phase
        else {
            return Task::none();
        };
        if *busy {
            return Task::none();
        }
        match recorder::save_recording_to_path(temp_path, default_save_path) {
            Ok(()) => {
                let dest = default_save_path.clone();
                self.close_native_done();
                let close_old = self.close_all_windows_task();
                self.phase = AppPhase::Exiting;
                Task::batch([
                    close_old,
                    Task::perform(open_saved_folder(dest), |()| AppMessage::FinishExit),
                ])
            }
            Err(err) => {
                let msg = format!("Erro ao salvar: {err}");
                *status = Some(msg.clone());
                if let Some(done) = &self.native_done {
                    done.send_error(&msg);
                }
                Task::none()
            }
        }
    }

    /// Dispara o seletor nativo `Salvar como...` bloqueando ações concorrentes até a resposta do portal.
    /// Input: `&mut self` | Output: `Task<AppMessage>` com `AppMessage::SaveRecordingDone`
    fn save_recording_as_task(&mut self) -> Task<AppMessage> {
        let AppPhase::RecordingDone {
            temp_path,
            default_save_path,
            busy,
            ..
        } = &mut self.phase
        else {
            return Task::none();
        };
        if *busy {
            return Task::none();
        }
        *busy = true;
        let src = temp_path.clone();
        let default_dest = default_save_path.clone();
        Task::perform(
            save_recording_via_portal(src, default_dest),
            AppMessage::SaveRecordingDone,
        )
    }

    /// Remove a gravação temporária não salva, fecha o diálogo nativo e encerra o aplicativo.
    /// Input: `&mut self` | Output: `Task<AppMessage>` com `iced::exit()`
    fn discard_recording_task(&mut self) -> Task<AppMessage> {
        if let AppPhase::RecordingDone { temp_path, .. } = &self.phase {
            let _ = std::fs::remove_file(temp_path);
        }
        self.close_native_done();
        iced::exit()
    }

    /// Fecha o diálogo nativo de contagem regressiva antes da captura, quando estiver aberto.
    /// Input: `&mut self` | Output: `()`
    fn close_native_delay(&mut self) {
        if let Some(handle) = self.native_delay.take() {
            handle.close();
        }
    }

    /// Fecha a janela nativa do editor de anotações e limpa seus canais associados.
    /// Input: `&mut self` | Output: `()`
    fn close_native_editor(&mut self) {
        if let Some(handle) = self.native_editor.take() {
            handle.close();
        }
        self.native_editor_rx = None;
    }

    /// Fecha a janela nativa de histórico e limpa seus canais associados.
    /// Input: `&mut self` | Output: `()`
    fn close_native_history(&mut self) {
        if let Some(handle) = self.native_history.take() {
            handle.close();
        }
        self.native_history_rx = None;
    }

    /// Grava a captura PNG no repositório local, agenda cópia na saída e atualiza clipboard em segundo plano.
    /// Input: `&mut self`, `png_bytes` (`Vec<u8>`) | Output: `()`
    fn copy_editor_png_task(&mut self, png_bytes: Vec<u8>) {
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
    }

    /// Abre o editor de anotações usando janela nativa GTK4/Libadwaita quando disponível ou fallback Iced.
    /// Input: `&mut self`, `editor` (`EditorState`) | Output: `Task<AppMessage>`
    fn open_editor_window(&mut self, editor: EditorState) -> Task<AppMessage> {
        self.close_native_editor();
        self.close_native_history();
        self.close_native_delay();
        let close_old = self.close_all_windows_task();

        if native_ui::should_use_native_gtk_editor() {
            let dark = editor.theme == UiTheme::Dark;
            if let Ok((handle, rx)) =
                native_ui::spawn_editor_window(&editor.base_rgba, editor.width, editor.height, dark)
            {
                handle.sync_scene(&editor.scene, editor.status_text.as_deref());
                let rx = Arc::new(tokio::sync::Mutex::new(rx));
                self.native_editor = Some(handle);
                self.native_editor_rx = Some(rx.clone());
                self.phase = AppPhase::Editor(editor);
                return Task::batch([close_old, listen_native_editor(rx)]);
            }
        }

        self.phase = AppPhase::Editor(editor);
        let (id, open_task) = iced::window::open(iced::window::Settings {
            size: Size::new(980.0, 680.0),
            decorations: true,
            ..Default::default()
        });
        self.windows.insert(id, WindowInfo::Editor);
        Task::batch([close_old, open_task.discard()])
    }

    /// Abre a janela de histórico de capturas usando GTK4/Libadwaita nativo quando disponível ou fallback Iced.
    /// Input: `&mut self` | Output: `Task<AppMessage>`
    fn open_history_window(&mut self) -> Task<AppMessage> {
        self.close_native_editor();
        self.close_native_history();
        let close_old = self.close_all_windows_task();
        let history = HistoryState::new(self.store.clone());

        if native_ui::should_use_native_gtk() {
            if let Ok((handle, rx)) = native_ui::spawn_history_window() {
                sync_history_to_native(&handle, &self.store, None);
                let rx = Arc::new(tokio::sync::Mutex::new(rx));
                self.native_history = Some(handle);
                self.native_history_rx = Some(rx.clone());
                self.phase = AppPhase::History(history);
                return Task::batch([close_old, listen_native_history(rx)]);
            }
        }

        self.phase = AppPhase::History(history);
        let (id, open_task) = iced::window::open(iced::window::Settings {
            size: Size::new(720.0, 560.0),
            ..Default::default()
        });
        self.windows.insert(id, WindowInfo::History);
        Task::batch([close_old, open_task.discard()])
    }

    /// Exibe uma mensagem de erro em janela nativa GTK4/Libadwaita ou fallback Iced com decorações do sistema.
    /// Input: `&mut self`, `err` (`String`) | Output: `Task<AppMessage>` encerrando após fechar o diálogo
    fn open_error_window(&mut self, err: String) -> Task<AppMessage> {
        self.close_native_editor();
        self.close_native_history();
        self.close_native_delay();
        let close_old = self.close_all_windows_task();
        if native_ui::should_use_native_gtk() {
            self.phase = AppPhase::Exiting;
            let show_task = Task::perform(
                async move {
                    let _ = tokio::task::spawn_blocking(move || native_ui::run_error_dialog(&err))
                        .await;
                },
                |()| AppMessage::FinishExit,
            );
            return Task::batch([close_old, show_task]);
        }
        self.phase = AppPhase::Error(err);
        let (id, open_task) = iced::window::open(iced::window::Settings {
            size: Size::new(560.0, 180.0),
            decorations: true,
            resizable: false,
            ..Default::default()
        });
        self.windows.insert(id, WindowInfo::Error);
        Task::batch([close_old, open_task.discard()])
    }

    /// Retorna se a interface Iced ativa deve usar paleta escura combinando tema do sistema e alternância do editor.
    /// Input: `&self` | Output: `bool` (`true` para modo escuro)
    fn current_dark_theme(&self) -> bool {
        if let AppPhase::Editor(editor) = &self.phase {
            editor.theme == UiTheme::Dark
        } else {
            self.system_dark
        }
    }

    /// Fecha todas as janelas registradas que satisfaçam o predicado `pred` e remove seus IDs do mapa.
    /// Input: `&mut self`, `pred` (`impl Fn(&WindowInfo) -> bool`) | Output: lote `Task<AppMessage>` de fechamento
    fn close_windows_where(&mut self, pred: impl Fn(&WindowInfo) -> bool) -> Task<AppMessage> {
        let ids: Vec<_> = self
            .windows
            .iter()
            .filter(|(_, info)| pred(info))
            .map(|(&id, _)| id)
            .collect();
        for id in &ids {
            self.windows.remove(id);
        }
        Task::batch(ids.into_iter().map(iced::window::close))
    }

    /// Sincroniza o estado visual da bandeja SNI com duração decorrida, pausa e visibilidade da barra flutuante.
    /// Input: `&self` | Output: `Task<AppMessage>` assíncrona atualizando o ícone da bandeja
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
            tray.update(elapsed_secs, paused, floating_bar_visible)
                .await;
        })
        .discard()
    }

    /// Alterna entre pausa e retomada da gravação ativa protegendo contra cliques concorrentes via flag `busy`.
    /// Input: `&mut self` | Output: `Task<AppMessage>` disparando `AppMessage::PauseToggled`
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
        self.sync_recording_bar();
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

    /// Encerra a barra flutuante, desliga a bandeja de gravação e finaliza os segmentos GStreamer em andamento.
    /// Input: `&mut self` | Output: `Task<AppMessage>` disparando `AppMessage::RecordingStopped`
    fn stop_recording_task(&mut self) -> Task<AppMessage> {
        if let AppPhase::Recording { busy, .. } = &mut self.phase {
            *busy = true;
        }
        let tray = self.recording_tray.take();
        self.recording_tray_rx = None;
        let close_native_bar = self.close_recording_bar();
        let close_bars = Task::batch([
            close_native_bar,
            self.close_windows_where(|info| matches!(info, WindowInfo::RecordingToast)),
        ]);
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

    /// Libera a trava de captura, salva/prepara a cópia automática da imagem RGBA8 e abre a janela do editor.
    /// Input: `&mut self`, `rgba` (`Vec<u8>`), `width` (`u32`), `height` (`u32`) | Output: `Task<AppMessage>` transicionando para `AppPhase::Editor`
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
        self.open_editor_window(
            EditorState::new(rgba, width, height).with_theme(UiTheme::from_dark(self.system_dark)),
        )
    }

    /// Abre uma janela em tela cheia do overlay por monitor detectado (ou uma única janela padrão se não houver geometria).
    /// Input: `&mut self` | Output: lote `Task<AppMessage>` de criação e transição em tela cheia
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

    /// Solicita o fechamento de todas as janelas atualmente rastreadas pelo `AppCore`.
    /// Input: `&mut self` | Output: lote `Task<AppMessage>` com os comandos `window::close`
    fn close_all_windows_task(&mut self) -> Task<AppMessage> {
        let ids: Vec<_> = self.windows.keys().copied().collect();
        self.windows.clear();
        Task::batch(ids.into_iter().map(iced::window::close))
    }

    /// Renderiza a interface correspondente ao `WindowInfo` e à fase corrente em `AppPhase` para a janela informada.
    /// Input: `&self`, `id` (`iced::window::Id`) | Output: `Element<'_, AppMessage>`
    fn view_core(&self, id: iced::window::Id) -> Element<'_, AppMessage> {
        match &self.phase {
            AppPhase::DelayPill { remaining_secs, .. } => {
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
            AppPhase::RecordingCountdown { remaining_secs } => {
                let dot = container(text(" ").size(8))
                    .width(Length::Fixed(10.0))
                    .height(Length::Fixed(10.0))
                    .style(|_theme| container::Style {
                        background: Some(iced::Background::Color(iced::Color::from_rgb8(
                            239, 68, 68,
                        ))),
                        border: iced::Border {
                            radius: 5.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                let title_row = row![
                    dot,
                    text(format!("Iniciando a gravação em {remaining_secs} s")).size(15),
                ]
                .spacing(10)
                .align_y(Alignment::Center);
                let hint = text(
                    "Barra superior: clique do meio pausa • duplo clique para • clique abre o menu",
                )
                .size(12);
                let content = Column::new()
                    .spacing(6)
                    .align_x(Alignment::Center)
                    .push(title_row)
                    .push(hint);
                container(content)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .padding([10, 16])
                    .into()
            }
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

                let timer = row![dot, text(format!("{mins:02}:{secs:02}")).size(15)]
                    .spacing(8)
                    .align_y(Alignment::Center);

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

                let bar = row![timer, pause_btn, stop_btn]
                    .spacing(10)
                    .align_y(Alignment::Center);

                container(bar)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .padding([6, 12])
                    .into()
            }
            AppPhase::RecordingDone {
                temp_path,
                default_save_path,
                duration_secs,
                status,
                busy,
                ..
            } => {
                let mins = duration_secs / 60;
                let secs = duration_secs % 60;
                let ext = temp_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("mp4")
                    .to_ascii_uppercase();
                let mut col = Column::new()
                    .push(text("Gravação concluída").size(18))
                    .push(text(format!("Duração: {mins:02}:{secs:02}  |  Formato: {ext}")).size(13))
                    .push(text(format!("Destino padrão: {}", default_save_path.display())).size(12))
                    .width(Length::Fill)
                    .spacing(10);

                if let Some(st) = status {
                    col = col.push(text(st).size(12));
                }

                let mut save_btn =
                    button(text("Salvar").size(13))
                        .padding([6, 14])
                        .style(|_theme, status| {
                            let bg = match status {
                                button::Status::Hovered => iced::Color::from_rgb8(0, 90, 170),
                                button::Status::Pressed => iced::Color::from_rgb8(0, 76, 145),
                                _ => iced::Color::from_rgb8(0, 103, 192),
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
                let mut save_as_btn = button(text("Salvar como...").size(13)).padding([6, 14]);
                let mut discard_btn =
                    button(text("Descartar").size(13))
                        .padding([6, 14])
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
                    save_btn = save_btn.on_press(AppMessage::SaveRecordingDefault);
                    save_as_btn = save_as_btn.on_press(AppMessage::SaveRecordingAs);
                    discard_btn = discard_btn.on_press(AppMessage::DiscardRecording);
                }
                let actions = container(row![discard_btn, save_as_btn, save_btn].spacing(10))
                    .width(Length::Fill)
                    .center_x(Length::Fill);
                col = col.push(actions);
                container(col)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(24)
                    .into()
            }
            AppPhase::Exiting => container(text("").size(12))
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            AppPhase::Editor(editor) => editor.view().map(AppMessage::Editor),
            AppPhase::History(history) => history.view().map(AppMessage::History),
            AppPhase::Error(err) => container(
                Column::new()
                    .push(text(format!("Error: {err}")).size(14))
                    .push(
                        container(
                            button(text("Close"))
                                .padding([6, 16])
                                .on_press(AppMessage::Cancel),
                        )
                        .width(Length::Fill)
                        .center_x(Length::Fill),
                    )
                    .width(Length::Fill)
                    .spacing(12),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(24)
            .into(),
        }
    }
}

/// Agenda o próximo pulso de 1 segundo da contagem regressiva antes da captura (`DelayTick`).
/// Input: nenhum | Output: `Task<AppMessage>`
fn schedule_delay_tick() -> Task<AppMessage> {
    Task::perform(
        async {
            tokio::time::sleep(Duration::from_secs(1)).await;
        },
        |()| AppMessage::DelayTick,
    )
}

/// Agenda o pulso de 1 segundo para atualizar o cronômetro da gravação ativa (`RecordingTick`).
/// Input: nenhum | Output: `Task<AppMessage>`
fn schedule_recording_tick() -> Task<AppMessage> {
    Task::perform(
        async {
            tokio::time::sleep(Duration::from_secs(1)).await;
        },
        |()| AppMessage::RecordingTick,
    )
}

/// Agenda o próximo passo da contagem regressiva pré-gravação no fallback Iced (`RecordingCountdownTick`).
/// Input: `delay_ms` (`u64`) em milissegundos | Output: `Task<AppMessage>`
fn schedule_recording_countdown_tick(delay_ms: u64) -> Task<AppMessage> {
    Task::perform(
        async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        },
        |()| AppMessage::RecordingCountdownTick,
    )
}

/// Inicia a gravação preparada e registra o indicador de status na barra superior do sistema.
/// Input: `prepared` (`SharedPreparedRecording`) | Output: `Result<RecordingSessionBundle, String>`
async fn start_prepared_recording_bundle(
    prepared: SharedPreparedRecording,
) -> Result<RecordingSessionBundle, String> {
    let rec_opt = prepared.0.lock().await.take();
    let Some(rec_prep) = rec_opt else {
        return Err("Recording preparation was cancelled".to_string());
    };
    let rec = rec_prep.start().await?;
    let (tray, tray_rx) = match tray::spawn_recording_tray().await {
        Ok((handle, rx)) => (Some(handle), Some(Arc::new(tokio::sync::Mutex::new(rx)))),
        Err(err) => {
            warn!(error = %err, "recording top-bar tray unavailable; falling back to floating bar");
            (None, None)
        }
    };
    Ok(RecordingSessionBundle {
        recording: SharedRecording(Arc::new(tokio::sync::Mutex::new(Some(rec)))),
        tray,
        tray_rx,
    })
}

/// Constrói o tema `iced::Theme` customizado alinhado ao esquema escuro ou claro Adwaita do sistema.
/// Input: `dark` (`bool`) | Output: instância de `iced::Theme`
fn system_iced_theme(dark: bool) -> iced::Theme {
    let palette = if dark {
        iced::theme::Palette {
            background: iced::Color::from_rgb8(36, 36, 36),
            text: iced::Color::WHITE,
            primary: iced::Color::from_rgb8(53, 132, 228),
            success: iced::Color::from_rgb8(46, 194, 126),
            danger: iced::Color::from_rgb8(224, 27, 36),
        }
    } else {
        iced::theme::Palette {
            background: iced::Color::from_rgb8(250, 250, 250),
            text: iced::Color::from_rgb8(46, 46, 46),
            primary: iced::Color::from_rgb8(53, 132, 228),
            success: iced::Color::from_rgb8(38, 162, 105),
            danger: iced::Color::from_rgb8(192, 28, 40),
        }
    };
    iced::Theme::custom("System".to_string(), palette)
}

/// Abre a pasta que contém `saved_file` no gerenciador de arquivos do sistema via portal `OpenDirectory` ou fallback `xdg-open`.
/// Input: `saved_file` (`PathBuf`) | Output: `()`
async fn open_saved_folder(saved_file: PathBuf) {
    let opened_via_portal = match std::fs::File::open(&saved_file) {
        Ok(file) => match ashpd::desktop::open_uri::OpenDirectoryRequest::default()
            .send(&file.as_fd())
            .await
        {
            Ok(req) => req.response().is_ok(),
            Err(err) => {
                warn!(error = %err, "portal OpenDirectory failed; trying xdg-open");
                false
            }
        },
        Err(err) => {
            warn!(error = %err, path = %saved_file.display(), "failed to open saved file for portal");
            false
        }
    };

    if !opened_via_portal {
        let dir = saved_file.parent().unwrap_or(&saved_file);
        if let Err(err) = std::process::Command::new("xdg-open").arg(dir).spawn() {
            warn!(error = %err, dir = %dir.display(), "xdg-open fallback failed");
        }
    }
}

/// Aguarda o próximo comando emitido pelo ícone SNI na bandeja do sistema e encaminha para o loop principal.
/// Input: `rx` (`Arc<Mutex<UnboundedReceiver<TrayRecordingCmd>>>`) | Output: `Task<AppMessage>`
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

/// Aguarda o próximo evento de controle emitido pela janela nativa GTK4 da barra de gravação.
/// Input: `rx` (`Arc<Mutex<UnboundedReceiver<NativeBarEvent>>>`) | Output: `Task<AppMessage>`
fn listen_native_bar(
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeBarEvent>>>,
) -> Task<AppMessage> {
    Task::perform(
        async move {
            let mut guard = rx.lock().await;
            guard.recv().await
        },
        AppMessage::NativeBarEvent,
    )
}

/// Aguarda o próximo evento de ação emitido pelo diálogo nativo GTK4 `Gravação concluída`.
/// Input: `rx` (`Arc<Mutex<UnboundedReceiver<NativeDoneEvent>>>`) | Output: `Task<AppMessage>`
fn listen_native_done(
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeDoneEvent>>>,
) -> Task<AppMessage> {
    Task::perform(
        async move {
            let mut guard = rx.lock().await;
            guard.recv().await
        },
        AppMessage::NativeDoneEvent,
    )
}

/// Aguarda o sinal de cancelamento emitido pelo diálogo nativo GTK4 de atraso antes da captura.
/// Input: `rx` (`tokio::sync::mpsc::UnboundedReceiver<()>`) | Output: `Task<AppMessage>` com `NativeDelayCancelled`
fn listen_native_delay(mut rx: tokio::sync::mpsc::UnboundedReceiver<()>) -> Task<AppMessage> {
    Task::perform(
        async move {
            let _ = rx.recv().await;
        },
        |()| AppMessage::NativeDelayCancelled,
    )
}

/// Aguarda o próximo evento de desenho ou ação emitido pela janela nativa GTK4 do editor.
/// Input: `rx` (`Arc<Mutex<UnboundedReceiver<NativeEditorEvent>>>`) | Output: `Task<AppMessage>`
fn listen_native_editor(
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeEditorEvent>>>,
) -> Task<AppMessage> {
    Task::perform(
        async move {
            let mut guard = rx.lock().await;
            guard.recv().await
        },
        AppMessage::NativeEditorEvent,
    )
}

/// Aguarda o próximo evento de interação emitido pela janela nativa GTK4 de histórico.
/// Input: `rx` (`Arc<Mutex<UnboundedReceiver<NativeHistoryEvent>>>`) | Output: `Task<AppMessage>`
fn listen_native_history(
    rx: Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<native_ui::NativeHistoryEvent>>,
    >,
) -> Task<AppMessage> {
    Task::perform(
        async move {
            let mut guard = rx.lock().await;
            guard.recv().await
        },
        AppMessage::NativeHistoryEvent,
    )
}

/// Lê as capturas armazenadas em `store` e envia a listagem atualizada para a janela nativa de histórico.
/// Input: `handle` (`&NativeHistoryHandle`), `store` (`&SnipStore`), `error_override` (`Option<&str>`) | Output: `()`
fn sync_history_to_native(
    handle: &native_ui::NativeHistoryHandle,
    store: &SnipStore,
    error_override: Option<&str>,
) {
    match store.list() {
        Ok(entries) => handle.sync_items(&entries, error_override),
        Err(err) => {
            let msg = format!("Failed to load snips: {err}");
            handle.sync_items(&[], Some(error_override.unwrap_or(&msg)));
        }
    }
}

/// Abre o diálogo nativo `SaveFileRequest` do portal XDG para copiar o vídeo gravado ao destino escolhido pelo usuário.
/// Input: `src_path` (`PathBuf`), `default_dest` (`PathBuf`) | Output: `Result<Option<PathBuf>, String>`
async fn save_recording_via_portal(
    src_path: PathBuf,
    default_dest: PathBuf,
) -> Result<Option<PathBuf>, String> {
    let default_name = default_dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("gravacao.mp4")
        .to_string();
    let build_base = || {
        ashpd::desktop::file_chooser::SaveFileRequest::default()
            .title("Salvar gravação de tela")
            .current_name(default_name.as_str())
    };
    let req = if let Some(parent) = default_dest.parent() {
        let _ = std::fs::create_dir_all(parent);
        build_base()
            .current_folder(parent)
            .unwrap_or_else(|_| build_base())
    } else {
        build_base()
    };
    let request = req
        .send()
        .await
        .map_err(|e| format!("file chooser failed: {e}"))?;
    let resp = match request.response() {
        Ok(r) => r,
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => return Ok(None),
        Err(e) => return Err(format!("file chooser response failed: {e}")),
    };
    let Some(uri) = resp.uris().first() else {
        return Ok(None);
    };
    let dest = uri
        .to_file_path()
        .map_err(|()| format!("non-file URI returned: {uri}"))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create save directory: {e}"))?;
    }
    std::fs::copy(&src_path, &dest).map_err(|e| format!("failed to copy recording: {e}"))?;
    Ok(Some(dest))
}

/// Executa a captura inicial da tela ou janela usando o backend apropriado (Wayland portal ou X11) em tarefa assíncrona.
/// Input: `mode` (`CaptureMode`), `is_wayland` (`bool`) | Output: `Task<AppMessage>` emitindo `CaptureReady`
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

/// Assinatura compartilhada que escuta a tecla `Escape` (cancelamento) e eventos de fechamento de janelas.
/// Input: nenhum | Output: `Subscription<AppMessage>` combinada
fn common_subscription() -> Subscription<AppMessage> {
    let keys = keyboard::on_key_press(|key, _mods| match key {
        keyboard::Key::Named(keyboard::key::Named::Escape) => Some(AppMessage::Cancel),
        _ => None,
    });
    let closed = iced::window::close_events().map(AppMessage::WindowClosed);
    Subscription::batch([keys, closed])
}

// --- Runner A: iced::daemon (GNOME Wayland + X11) ---

/// Adaptador do daemon padrão `iced::daemon` usado em sessões X11 e GNOME Wayland.
/// Armazena: `core` (`AppCore`) | Principais métodos: `title`, `update`, `view`, `theme`, `subscription`
struct IcedApp {
    core: AppCore,
}

impl IcedApp {
    /// Retorna o título contextual exibido nas decorações padrão do sistema conforme o tipo da janela `id`.
    /// Input: `&self`, `id` (`iced::window::Id`) | Output: `String` com o título da janela
    fn title(&self, id: iced::window::Id) -> String {
        match self.core.windows.get(&id) {
            Some(WindowInfo::History) => "Histórico — LX Snipping Tool".to_string(),
            Some(WindowInfo::RecordingBar) => "Gravando tela".to_string(),
            Some(WindowInfo::RecordingToast) => "Iniciando gravação".to_string(),
            _ => "LX Snipping Tool".to_string(),
        }
    }

    /// Encaminha mensagens recebidas ao controlador central `AppCore::update_core` sem `layershell`.
    /// Input: `&mut self`, `msg` (`AppMessage`) | Output: `Task<AppMessage>`
    fn update(&mut self, msg: AppMessage) -> Task<AppMessage> {
        self.core.update_core(msg, false)
    }

    /// Delega a renderização da janela `id` para `AppCore::view_core`.
    /// Input: `&self`, `id` (`iced::window::Id`) | Output: `Element<'_, AppMessage>`
    fn view(&self, id: iced::window::Id) -> Element<'_, AppMessage> {
        self.core.view_core(id)
    }

    /// Retorna o tema claro ou escuro do Iced sincronizado com o sistema e alternância do editor.
    /// Input: `&self`, `_id` (`iced::window::Id`) | Output: `iced::Theme`
    fn theme(&self, _id: iced::window::Id) -> iced::Theme {
        system_iced_theme(self.core.current_dark_theme())
    }

    /// Conecta o runner à assinatura comum de teclado e eventos de janela.
    /// Input: `&self` | Output: `Subscription<AppMessage>`
    fn subscription(&self) -> Subscription<AppMessage> {
        common_subscription()
    }
}

// --- Runner B: iced_layershell::build_pattern::daemon (KDE/wlroots Wayland) ---

/// Mensagem wrapper gerada pela macro `to_layer_message` para o runner `iced_layershell`.
/// Armazena: variante `Core(AppMessage)` e variantes de controle layer-shell | Principais métodos: nenhum (enumeração de mensagens)
#[to_layer_message(multi, info_name = "WindowInfo")]
#[derive(Debug, Clone)]
enum LayerMsg {
    Core(AppMessage),
}

/// Adaptador de daemon `iced_layershell` para compositores Wayland baseados em wlroots/KDE.
/// Armazena: `core` (`AppCore`) | Principais métodos: `namespace`, `update`, `view`, `theme`, `subscription`, `id_info`, `set_id_info`, `remove_id`
struct LayerShellApp {
    core: AppCore,
}

impl LayerShellApp {
    /// Informa ao compositor Wayland o identificador namespace `wlr-layer-shell` da aplicação.
    /// Input: `&self` | Output: `String` com o App ID reverso
    fn namespace(&self) -> String {
        "io.github.lx_snippingtool.SnippingTool".to_string()
    }

    /// Processa mensagens `LayerMsg::Core` e solicita abertura de superfície layer-shell quando o overlay se torna ativo.
    /// Input: `&mut self`, `msg` (`LayerMsg`) | Output: `Task<LayerMsg>`
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

    /// Renderiza a janela `id` via `AppCore::view_core` mapeando mensagens para `LayerMsg::Core`.
    /// Input: `&self`, `id` (`iced::window::Id`) | Output: `Element<'_, LayerMsg>`
    fn view(&self, id: iced::window::Id) -> Element<'_, LayerMsg> {
        self.core.view_core(id).map(LayerMsg::Core)
    }

    /// Retorna o tema claro ou escuro do Iced sincronizado com o ambiente e alternância do editor para o runner layer-shell.
    /// Input: `&self` | Output: `iced::Theme`
    fn theme(&self) -> iced::Theme {
        system_iced_theme(self.core.current_dark_theme())
    }

    /// Expõe a assinatura comum de eventos mapeada para `LayerMsg::Core`.
    /// Input: `&self` | Output: `Subscription<LayerMsg>`
    fn subscription(&self) -> Subscription<LayerMsg> {
        common_subscription().map(LayerMsg::Core)
    }

    /// Consulta os metadados `WindowInfo` associados a um ID de superfície/janela.
    /// Input: `&self`, `id` (`iced::window::Id`) | Output: `Option<WindowInfo>`
    fn id_info(&self, id: iced::window::Id) -> Option<WindowInfo> {
        self.core.windows.get(&id).cloned()
    }

    /// Registra ou atualiza o `WindowInfo` associado a uma janela gerenciada pelo `iced_layershell`.
    /// Input: `&mut self`, `id` (`iced::window::Id`), `info` (`WindowInfo`) | Output: `()`
    fn set_id_info(&mut self, id: iced::window::Id, info: WindowInfo) {
        self.core.windows.insert(id, info);
    }

    /// Remove o registro de `WindowInfo` ao destruir a janela informada.
    /// Input: `&mut self`, `id` (`iced::window::Id`) | Output: `()`
    fn remove_id(&mut self, id: iced::window::Id) {
        self.core.windows.remove(&id);
    }
}

/// Tenta adquirir trava exclusiva não bloqueante em `$XDG_RUNTIME_DIR/lx-snippingtool.lock` e encerra se outra captura estiver ativa.
/// Input: nenhum | Output: `Option<File>` mantendo o file lock ativo enquanto o overlay rodar
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

/// Determina se deve usar o runner `iced_layershell` (Wayland fora do GNOME).
/// Input: `is_wayland` (`bool`) | Output: `bool` (`true` quando em Wayland não-GNOME)
fn should_use_layershell(is_wayland: bool) -> bool {
    if !is_wayland {
        return false;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    !desktop.to_ascii_uppercase().contains("GNOME")
}

/// Registra o atalho global `Ctrl+Shift+S` nas configurações personalizadas do GNOME via `gsettings`.
/// Input: nenhum | Output: `Result<(), String>`
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

/// Ponto de entrada principal que inicializa telemetria, interpreta CLI, executa o daemon gráfico e mantém clipboard pós-saída.
/// Input: nenhum | Output: `()` (ou término via `std::process::exit` em erro fatal)
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

    let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let system_dark = native_ui::detect_system_dark_theme();
    let initial_phase = if let Some(path) = &cli.editor {
        match EditorState::from_png_file(path) {
            Ok(ed) => AppPhase::Editor(ed.with_theme(UiTheme::from_dark(system_dark))),
            Err(err) => AppPhase::Error(err),
        }
    } else if cli.history {
        AppPhase::History(HistoryState::new(store.clone()))
    } else if delay_secs > 0 {
        AppPhase::DelayPill {
            remaining_secs: delay_secs,
            using_native: false,
        }
    } else {
        AppPhase::Capturing
    };

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
            system_dark,
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
        .theme(LayerShellApp::theme)
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
            system_dark,
            lock_guard.take(),
        );
        let res = iced::daemon(IcedApp::title, IcedApp::update, IcedApp::view)
            .theme(IcedApp::theme)
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
