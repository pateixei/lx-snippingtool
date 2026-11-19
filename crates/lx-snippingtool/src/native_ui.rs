//! Módulo: `native_ui`
//! Objetivos: Fornecer janelas nativas GTK4 / Libadwaita para todas as janelas e diálogos do aplicativo (`RecordingDone`, `RecordingBar`, `RecordingToast`, `DelayPill`, `Error`, `History` e `Editor`) e detectar o tema escuro/claro do sistema operacional.
//! Principais itens: `NativeDoneEvent`, `NativeBarEvent`, `NativeHistoryEvent`, `NativeEditorEvent`, `NativeDoneHandle`, `NativeBarHandle`, `NativeDelayHandle`, `NativeHistoryHandle`, `NativeEditorHandle`, `should_use_native_gtk`, `should_use_native_gtk_editor`, `detect_system_dark_theme`, `spawn_recording_done`, `spawn_recording_bar`, `run_recording_toast`, `parse_toast_outcome`, `spawn_delay_dialog`, `run_error_dialog`, `spawn_history_window`, `spawn_editor_window`
//! Última alteração: 2026-11-19

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use directories::ProjectDirs;
use lx_snip_core::export::{encode_rgba, ExportFormat};
use lx_snip_core::scene::{AnnotationOp, AnnotationScene, Rect};
use lx_snip_core::storage::SnipEntry;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

const NATIVE_GTK_SCRIPT: &str = include_str!("native_ui_gtk.py");

/// Eventos de ação emitidos pelo diálogo nativo `Gravação concluída`.
/// Armazena: variantes `Save`, `SaveAs` e `Discard` | Principais métodos: nenhum (enumeração de eventos)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDoneEvent {
    /// Salvar diretamente no caminho padrão sugerido.
    Save,
    /// Abrir o seletor nativo para escolher nome e pasta de destino.
    SaveAs,
    /// Descartar a gravação temporária ou fechar o diálogo sem salvar.
    Discard,
}

/// Eventos de controle emitidos pela janela nativa da barra de gravação ativa.
/// Armazena: variantes `TogglePause`, `Stop` e `Closed` | Principais métodos: nenhum (enumeração de eventos)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeBarEvent {
    /// Alternar entre pausar e retomar a gravação em andamento.
    TogglePause,
    /// Encerrar a gravação atual.
    Stop,
    /// Usuário fechou a janela de controles pelo botão do sistema.
    Closed,
}

/// Eventos de interação emitidos pela janela nativa GTK4 de histórico de capturas.
/// Armazena: variantes `Refresh`, `Copy`, `Edit`, `Delete` e `Closed` | Principais métodos: nenhum (enumeração de eventos)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeHistoryEvent {
    /// Recarregar a lista de capturas armazenadas.
    Refresh,
    /// Copiar o arquivo PNG indicado para a área de transferência e encerrar.
    Copy(PathBuf),
    /// Abrir o arquivo PNG indicado no editor de anotações.
    Edit(PathBuf),
    /// Excluir o arquivo PNG indicado do histórico local.
    Delete(PathBuf),
    /// Usuário fechou a janela de histórico.
    Closed,
}

/// Eventos de desenho e ações emitidos pela janela nativa GTK4 do editor de anotações.
/// Armazena: variantes `PushOp`, `Erase`, `SetCrop`, `Undo`, `Redo`, `Copy`, `Save`, `NewCapture`, `OpenHistory` e `Closed` | Principais métodos: nenhum (enumeração de eventos)
#[derive(Debug, Clone, PartialEq)]
pub enum NativeEditorEvent {
    /// Adiciona uma operação vetorial concluída à cena.
    PushOp(AnnotationOp),
    /// Aplica borracha nas coordenadas da imagem com o raio informado.
    Erase {
        /// Coordenada X em pixels da imagem.
        x: f32,
        /// Coordenada Y em pixels da imagem.
        y: f32,
        /// Raio de colisão em pixels da imagem.
        radius: f32,
    },
    /// Define ou limpa (`None`) o retângulo de recorte da cena.
    SetCrop(Option<Rect>),
    /// Desfaz a última alteração na cena.
    Undo,
    /// Refaz a última alteração desfeita na cena.
    Redo,
    /// Exporta a imagem anotada e copia para a área de transferência.
    Copy,
    /// Exporta a imagem anotada via seletor `Salvar Como`.
    Save,
    /// Inicia uma nova captura fechando o editor atual.
    NewCapture,
    /// Abre a janela de histórico de capturas.
    OpenHistory,
    /// Usuário fechou a janela do editor.
    Closed,
}

/// Representa o formato JSON recebido via `stdout` da janela nativa de histórico.
/// Armazena: variantes `Refresh`, `Copy`, `Edit`, `Delete` e `Close` | Principais métodos: nenhum (desserialização interna)
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum NativeHistoryWire {
    Refresh,
    Copy { path: PathBuf },
    Edit { path: PathBuf },
    Delete { path: PathBuf },
    Close,
}

/// Representa o formato JSON recebido via `stdout` da janela nativa do editor.
/// Armazena: variantes `PushOp`, `Erase`, `SetCrop`, `ResetCrop`, `Undo`, `Redo`, `Copy`, `Save`, `New`, `History` e `Close` | Principais métodos: nenhum (desserialização interna)
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum NativeEditorWire {
    PushOp {
        op: AnnotationOp,
    },
    Erase {
        x: f32,
        y: f32,
        #[serde(default = "default_erase_radius")]
        radius: f32,
    },
    SetCrop {
        crop: Option<Rect>,
    },
    ResetCrop,
    Undo,
    Redo,
    Copy,
    Save,
    New,
    History,
    Close,
}

/// Retorna o raio padrão em pixels usado pela borracha caso não seja especificado no JSON.
/// Input: nenhum | Output: `10.0` (`f32`)
const fn default_erase_radius() -> f32 {
    10.0
}

/// Item serializado enviado via `stdin` para popular cartões da janela nativa de histórico.
/// Armazena: `path`, `name` e `kb` | Principais métodos: nenhum (carga útil JSON)
#[derive(Serialize)]
struct NativeHistoryItemPayload {
    path: String,
    name: String,
    kb: f64,
}

/// Carga JSON completa enviada ao atualizar a lista de capturas da janela nativa de histórico.
/// Armazena: `items` e `status` | Principais métodos: nenhum (carga útil JSON)
#[derive(Serialize)]
struct NativeHistorySyncPayload<'a> {
    items: Vec<NativeHistoryItemPayload>,
    status: Option<&'a str>,
}

/// Carga JSON completa enviada ao sincronizar a cena retida com a janela nativa do editor.
/// Armazena: `ops`, `crop_rect`, `can_undo`, `can_redo` e `status` | Principais métodos: nenhum (carga útil JSON)
#[derive(Serialize)]
struct NativeEditorScenePayload<'a> {
    ops: &'a [AnnotationOp],
    crop_rect: Option<Rect>,
    can_undo: bool,
    can_redo: bool,
    status: Option<&'a str>,
}

/// Processo filho gerenciado de janela GTK4/Libadwaita com canal `stdin` para comandos do host.
/// Armazena: `stdin`, `child` e `cleanup_path` | Principais métodos: `send_line`, `close_and_wait`, `cleanup_temp`, `Drop::drop`
struct NativeChild {
    stdin: Option<ChildStdin>,
    child: Child,
    cleanup_path: Option<PathBuf>,
}

impl NativeChild {
    /// Escreve uma linha de comando para o processo filho nativo e força flush imediato.
    /// Input: `&mut self`, `line` (`&str`) | Output: `()`
    fn send_line(&mut self, line: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{line}");
            let _ = stdin.flush();
        }
    }

    /// Remove qualquer arquivo temporário associado ao subprocesso nativo caso ainda exista.
    /// Input: `&mut self` | Output: `()`
    fn cleanup_temp(&mut self) {
        if let Some(path) = self.cleanup_path.take() {
            let _ = fs::remove_file(path);
        }
    }

    /// Solicita o encerramento gracioso via `close`, fecha `stdin`, recolhe o subprocesso e limpa temporários.
    /// Input: `&mut self` | Output: `()`
    fn close_and_wait(&mut self) {
        self.send_line("close");
        self.stdin.take();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            std::thread::sleep(std::time::Duration::from_millis(80));
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
        self.cleanup_temp();
    }
}

impl Drop for NativeChild {
    /// Garante fechamento de `stdin`, término do subprocesso nativo e remoção de arquivo temporário.
    /// Input: `&mut self` | Output: `()`
    fn drop(&mut self) {
        self.stdin.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        self.cleanup_temp();
    }
}

/// Handle compartilhável para controlar o diálogo nativo `Gravação concluída` após sua abertura.
/// Armazena: `inner` (`Arc<Mutex<Option<NativeChild>>>`) | Principais métodos: `send_close`, `send_idle`, `send_error`
#[derive(Clone)]
pub struct NativeDoneHandle {
    inner: Arc<Mutex<Option<NativeChild>>>,
}

impl NativeDoneHandle {
    /// Fecha imediatamente a janela nativa `Gravação concluída` e aguarda a saída do processo.
    /// Input: `&self` | Output: `()`
    pub fn send_close(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut proc_handle) = guard.take() {
                proc_handle.close_and_wait();
            }
        }
    }

    /// Reabilita os botões do diálogo nativo após cancelamento de `Salvar como...`.
    /// Input: `&self` | Output: `()`
    pub fn send_idle(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(proc_handle) = guard.as_mut() {
                proc_handle.send_line("idle");
            }
        }
    }

    /// Exibe uma mensagem de erro no diálogo nativo e reabilita os botões de ação.
    /// Input: `&self`, `msg` (`&str`) | Output: `()`
    pub fn send_error(&self, msg: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(proc_handle) = guard.as_mut() {
                proc_handle.send_line(&format_done_error_cmd(msg));
            }
        }
    }
}

/// Handle compartilhável para atualizar ou fechar a barra nativa de gravação em andamento.
/// Armazena: `inner` (`Arc<Mutex<Option<NativeChild>>>`) | Principais métodos: `update`, `close`
#[derive(Clone)]
pub struct NativeBarHandle {
    inner: Arc<Mutex<Option<NativeChild>>>,
}

impl NativeBarHandle {
    /// Atualiza o cronômetro, estado de pausa e ocupação dos botões na barra nativa.
    /// Input: `&self`, `elapsed_secs` (`u64`), `paused` (`bool`), `busy` (`bool`) | Output: `()`
    pub fn update(&self, elapsed_secs: u64, paused: bool, busy: bool) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(proc_handle) = guard.as_mut() {
                proc_handle.send_line(&format_bar_state_cmd(elapsed_secs, paused, busy));
            }
        }
    }

    /// Fecha a barra nativa de gravação e encerra seu processo associado.
    /// Input: `&self` | Output: `()`
    pub fn close(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut proc_handle) = guard.take() {
                proc_handle.close_and_wait();
            }
        }
    }
}

/// Handle compartilhável para atualizar ou fechar o diálogo nativo de contagem regressiva antes da captura.
/// Armazena: `inner` (`Arc<Mutex<Option<NativeChild>>>`) | Principais métodos: `update_secs`, `close`
#[derive(Clone)]
pub struct NativeDelayHandle {
    inner: Arc<Mutex<Option<NativeChild>>>,
}

impl NativeDelayHandle {
    /// Atualiza o contador de segundos exibido na janela nativa de atraso.
    /// Input: `&self`, `remaining_secs` (`u8`) | Output: `()`
    pub fn update_secs(&self, remaining_secs: u8) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(proc_handle) = guard.as_mut() {
                proc_handle.send_line(&format_delay_tick_cmd(remaining_secs));
            }
        }
    }

    /// Fecha a janela nativa de atraso e encerra seu subprocesso associado.
    /// Input: `&self` | Output: `()`
    pub fn close(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut proc_handle) = guard.take() {
                proc_handle.close_and_wait();
            }
        }
    }
}

/// Handle compartilhável para atualizar a listagem ou fechar a janela nativa de histórico.
/// Armazena: `inner` (`Arc<Mutex<Option<NativeChild>>>`) | Principais métodos: `sync_items`, `close`
#[derive(Clone)]
pub struct NativeHistoryHandle {
    inner: Arc<Mutex<Option<NativeChild>>>,
}

impl NativeHistoryHandle {
    /// Serializa a lista de capturas e mensagem de status enviando comando `items:` à janela nativa.
    /// Input: `&self`, `entries` (`&[SnipEntry]`), `status` (`Option<&str>`) | Output: `()`
    pub fn sync_items(&self, entries: &[SnipEntry], status: Option<&str>) {
        let items = entries
            .iter()
            .map(|entry| {
                let name = entry
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("snip.png")
                    .to_string();
                NativeHistoryItemPayload {
                    path: entry.path.to_string_lossy().into_owned(),
                    name,
                    kb: (entry.byte_size as f64) / 1024.0,
                }
            })
            .collect();
        let payload = NativeHistorySyncPayload { items, status };
        if let Ok(json) = serde_json::to_string(&payload) {
            if let Ok(mut guard) = self.inner.lock() {
                if let Some(proc_handle) = guard.as_mut() {
                    proc_handle.send_line(&format!("items:{json}"));
                }
            }
        }
    }

    /// Fecha a janela nativa de histórico e encerra seu processo associado.
    /// Input: `&self` | Output: `()`
    pub fn close(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut proc_handle) = guard.take() {
                proc_handle.close_and_wait();
            }
        }
    }
}

/// Handle compartilhável para sincronizar a cena de anotações ou fechar a janela nativa do editor.
/// Armazena: `inner` (`Arc<Mutex<Option<NativeChild>>>`) | Principais métodos: `sync_scene`, `close`
#[derive(Clone)]
pub struct NativeEditorHandle {
    inner: Arc<Mutex<Option<NativeChild>>>,
}

impl NativeEditorHandle {
    /// Serializa as operações, recorte e estado desfazer/refazer enviando comando `state:` ao editor nativo.
    /// Input: `&self`, `scene` (`&AnnotationScene`), `status` (`Option<&str>`) | Output: `()`
    pub fn sync_scene(&self, scene: &AnnotationScene, status: Option<&str>) {
        let payload = NativeEditorScenePayload {
            ops: scene.ops(),
            crop_rect: scene.crop_rect,
            can_undo: scene.can_undo(),
            can_redo: scene.can_redo(),
            status,
        };
        if let Ok(json) = serde_json::to_string(&payload) {
            if let Ok(mut guard) = self.inner.lock() {
                if let Some(proc_handle) = guard.as_mut() {
                    proc_handle.send_line(&format!("state:{json}"));
                }
            }
        }
    }

    /// Fecha a janela nativa do editor, encerra o subprocesso e remove o PNG temporário de prévia.
    /// Input: `&self` | Output: `()`
    pub fn close(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut proc_handle) = guard.take() {
                proc_handle.close_and_wait();
            }
        }
    }
}

/// Converte uma linha recebida em `stdout` do diálogo nativo para `NativeDoneEvent`.
/// Input: `line` (`&str`) | Output: `Option<NativeDoneEvent>`
pub fn parse_done_event(line: &str) -> Option<NativeDoneEvent> {
    match line.trim() {
        "save" => Some(NativeDoneEvent::Save),
        "save_as" => Some(NativeDoneEvent::SaveAs),
        "discard" => Some(NativeDoneEvent::Discard),
        _ => None,
    }
}

/// Converte uma linha recebida em `stdout` da barra nativa para `NativeBarEvent`.
/// Input: `line` (`&str`) | Output: `Option<NativeBarEvent>`
pub fn parse_bar_event(line: &str) -> Option<NativeBarEvent> {
    match line.trim() {
        "toggle_pause" => Some(NativeBarEvent::TogglePause),
        "stop" => Some(NativeBarEvent::Stop),
        "closed" => Some(NativeBarEvent::Closed),
        _ => None,
    }
}

/// Converte uma linha JSON recebida em `stdout` da janela nativa de histórico para `NativeHistoryEvent`.
/// Input: `line` (`&str`) | Output: `Option<NativeHistoryEvent>`
pub fn parse_history_event(line: &str) -> Option<NativeHistoryEvent> {
    let parsed: NativeHistoryWire = serde_json::from_str(line.trim()).ok()?;
    Some(match parsed {
        NativeHistoryWire::Refresh => NativeHistoryEvent::Refresh,
        NativeHistoryWire::Copy { path } => NativeHistoryEvent::Copy(path),
        NativeHistoryWire::Edit { path } => NativeHistoryEvent::Edit(path),
        NativeHistoryWire::Delete { path } => NativeHistoryEvent::Delete(path),
        NativeHistoryWire::Close => NativeHistoryEvent::Closed,
    })
}

/// Converte uma linha JSON recebida em `stdout` da janela nativa do editor para `NativeEditorEvent`.
/// Input: `line` (`&str`) | Output: `Option<NativeEditorEvent>`
pub fn parse_editor_event(line: &str) -> Option<NativeEditorEvent> {
    let parsed: NativeEditorWire = serde_json::from_str(line.trim()).ok()?;
    Some(match parsed {
        NativeEditorWire::PushOp { op } => NativeEditorEvent::PushOp(op),
        NativeEditorWire::Erase { x, y, radius } => NativeEditorEvent::Erase { x, y, radius },
        NativeEditorWire::SetCrop { crop } => NativeEditorEvent::SetCrop(crop),
        NativeEditorWire::ResetCrop => NativeEditorEvent::SetCrop(None),
        NativeEditorWire::Undo => NativeEditorEvent::Undo,
        NativeEditorWire::Redo => NativeEditorEvent::Redo,
        NativeEditorWire::Copy => NativeEditorEvent::Copy,
        NativeEditorWire::Save => NativeEditorEvent::Save,
        NativeEditorWire::New => NativeEditorEvent::NewCapture,
        NativeEditorWire::History => NativeEditorEvent::OpenHistory,
        NativeEditorWire::Close => NativeEditorEvent::Closed,
    })
}

/// Formata o comando de atualização de estado enviado via `stdin` para a barra nativa.
/// Input: `elapsed_secs` (`u64`), `paused` (`bool`), `busy` (`bool`) | Output: `String` no formato `state:<seg>:<0|1>:<0|1>`
pub fn format_bar_state_cmd(elapsed_secs: u64, paused: bool, busy: bool) -> String {
    format!(
        "state:{}:{}:{}",
        elapsed_secs,
        u8::from(paused),
        u8::from(busy)
    )
}

/// Formata o comando de erro enviado via `stdin` ao diálogo nativo sanando quebras de linha.
/// Input: `msg` (`&str`) | Output: `String` em linha única prefixada por `error:`
pub fn format_done_error_cmd(msg: &str) -> String {
    let single_line = msg.replace(['\r', '\n'], " ");
    format!("error:{single_line}")
}

/// Formata o comando de atualização de contagem regressiva enviado ao diálogo nativo de atraso.
/// Input: `remaining_secs` (`u8`) | Output: `String` no formato `tick:<segundos>`
pub fn format_delay_tick_cmd(remaining_secs: u8) -> String {
    format!("tick:{remaining_secs}")
}

/// Verifica de forma preguiçosa e armazenada em cache se GTK4 + Libadwaita estão disponíveis para janelas nativas.
/// Input: nenhum | Output: `bool` (`true` fora de desktops puramente KDE quando `gi`/`Gtk 4.0`/`Adw 1` carregam)
pub fn should_use_native_gtk() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if desktop.to_ascii_uppercase().contains("KDE") {
        return false;
    }

    static PROBE: OnceLock<bool> = OnceLock::new();
    *PROBE.get_or_init(|| {
        Command::new("python3")
            .args([
                "-c",
                "import gi; gi.require_version('Gtk', '4.0'); gi.require_version('Adw', '1'); from gi.repository import Gtk, Adw",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Verifica de forma preguiçosa e em cache se GTK4 + Libadwaita + PyCairo estão disponíveis para o editor nativo.
/// Input: nenhum | Output: `bool` (`true` quando `should_use_native_gtk()` e `import cairo` têm sucesso)
pub fn should_use_native_gtk_editor() -> bool {
    if !should_use_native_gtk() {
        return false;
    }

    static CAIRO_PROBE: OnceLock<bool> = OnceLock::new();
    *CAIRO_PROBE.get_or_init(|| {
        Command::new("python3")
            .args(["-c", "import cairo"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Detecta se o ambiente de desktop prefere tema escuro consultando o portal XDG ou `gsettings`.
/// Input: nenhum | Output: `bool` (`true` quando `color-scheme` indica preferência por modo escuro)
pub fn detect_system_dark_theme() -> bool {
    if let Ok(out) = Command::new("dbus-send")
        .args([
            "--reply-timeout=100",
            "--print-reply=literal",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings.Read",
            "string:org.freedesktop.appearance",
            "string:color-scheme",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            return stdout.trim().ends_with("uint32 1");
        }
    }

    Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| {
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("prefer-dark")
        })
}

/// Abre o diálogo nativo GTK4/Libadwaita `Gravação concluída` retornando handle de controle e receptor de eventos.
/// Input: `duration_secs` (`u64`), `ext` (`&str`), `default_save_path` (`&Path`) | Output: `Result<(NativeDoneHandle, mpsc::UnboundedReceiver<NativeDoneEvent>), String>`
pub fn spawn_recording_done(
    duration_secs: u64,
    ext: &str,
    default_save_path: &Path,
) -> Result<(NativeDoneHandle, mpsc::UnboundedReceiver<NativeDoneEvent>), String> {
    let duration_arg = duration_secs.to_string();
    let default_arg = default_save_path.to_string_lossy().into_owned();
    let mut child = Command::new("python3")
        .args([
            "-u",
            "-c",
            NATIVE_GTK_SCRIPT,
            "done",
            &duration_arg,
            ext,
            &default_arg,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn native GTK done dialog: {e}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "missing stdin pipe on native done dialog".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing stdout pipe on native done dialog".to_string())?;

    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(ev) = parse_done_event(&line) {
                let _ = tx.send(ev);
            }
        }
    });

    let handle = NativeDoneHandle {
        inner: Arc::new(Mutex::new(Some(NativeChild {
            stdin: Some(stdin),
            child,
            cleanup_path: None,
        }))),
    };
    Ok((handle, rx))
}

/// Abre a janela nativa GTK4/Libadwaita da barra flutuante de controle durante a gravação.
/// Input: `elapsed_secs` (`u64`), `paused` (`bool`), `busy` (`bool`) | Output: `Result<(NativeBarHandle, mpsc::UnboundedReceiver<NativeBarEvent>), String>`
pub fn spawn_recording_bar(
    elapsed_secs: u64,
    paused: bool,
    busy: bool,
) -> Result<(NativeBarHandle, mpsc::UnboundedReceiver<NativeBarEvent>), String> {
    let elapsed_arg = elapsed_secs.to_string();
    let paused_arg = if paused { "1" } else { "0" };
    let busy_arg = if busy { "1" } else { "0" };
    let mut child = Command::new("python3")
        .args([
            "-u",
            "-c",
            NATIVE_GTK_SCRIPT,
            "bar",
            &elapsed_arg,
            paused_arg,
            busy_arg,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn native GTK recording bar: {e}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "missing stdin pipe on native recording bar".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing stdout pipe on native recording bar".to_string())?;

    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(ev) = parse_bar_event(&line) {
                let _ = tx.send(ev);
            }
        }
    });

    let handle = NativeBarHandle {
        inner: Arc::new(Mutex::new(Some(NativeChild {
            stdin: Some(stdin),
            child,
            cleanup_path: None,
        }))),
    };
    Ok((handle, rx))
}

/// Interpreta a saída padrão emitida pela janela pré-gravação indicando se a contagem completou ou foi cancelada.
/// Input: `stdout` (`&str`) | Output: `bool` (`true` quando recebeu `"ready"`, `false` para `"cancel"` ou vazio)
pub fn parse_toast_outcome(stdout: &str) -> bool {
    stdout.lines().any(|line| line.trim() == "ready")
}

/// Exibe a janela nativa GTK4/Libadwaita de contagem regressiva pré-gravação (2 → 1 → 0 s) e aguarda sua conclusão ou cancelamento.
/// Input: nenhum | Output: `Result<bool, String>` (`Ok(true)` para iniciar gravação, `Ok(false)` se cancelada pelo usuário)
pub fn run_recording_toast() -> Result<bool, String> {
    let output = Command::new("python3")
        .args(["-u", "-c", NATIVE_GTK_SCRIPT, "toast"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run native GTK toast: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "native GTK toast exited with status {}",
            output.status
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_toast_outcome(&stdout))
}

/// Abre o diálogo nativo GTK4/Libadwaita de contagem regressiva retornando handle e canal de cancelamento.
/// Input: `remaining_secs` (`u8`) | Output: `Result<(NativeDelayHandle, mpsc::UnboundedReceiver<()>), String>`
pub fn spawn_delay_dialog(
    remaining_secs: u8,
) -> Result<(NativeDelayHandle, mpsc::UnboundedReceiver<()>), String> {
    let secs_arg = remaining_secs.to_string();
    let mut child = Command::new("python3")
        .args(["-u", "-c", NATIVE_GTK_SCRIPT, "delay", &secs_arg])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn native GTK delay dialog: {e}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "missing stdin pipe on native delay dialog".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing stdout pipe on native delay dialog".to_string())?;

    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if line.trim() == "cancel" {
                let _ = tx.send(());
                break;
            }
        }
    });

    let handle = NativeDelayHandle {
        inner: Arc::new(Mutex::new(Some(NativeChild {
            stdin: Some(stdin),
            child,
            cleanup_path: None,
        }))),
    };
    Ok((handle, rx))
}

/// Exibe de forma síncrona o diálogo nativo GTK4/Libadwaita de erro até ser fechado pelo usuário.
/// Input: `msg` (`&str`) | Output: `Result<(), String>`
pub fn run_error_dialog(msg: &str) -> Result<(), String> {
    let status = Command::new("python3")
        .args(["-u", "-c", NATIVE_GTK_SCRIPT, "error", msg])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("failed to run native GTK error dialog: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("native GTK error dialog exited with {status}"))
    }
}

/// Abre a janela nativa GTK4/Libadwaita de histórico retornando handle de atualização e canal de eventos.
/// Input: nenhum | Output: `Result<(NativeHistoryHandle, mpsc::UnboundedReceiver<NativeHistoryEvent>), String>`
pub fn spawn_history_window() -> Result<
    (
        NativeHistoryHandle,
        mpsc::UnboundedReceiver<NativeHistoryEvent>,
    ),
    String,
> {
    let mut child = Command::new("python3")
        .args(["-u", "-c", NATIVE_GTK_SCRIPT, "history"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn native GTK history window: {e}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "missing stdin pipe on native history window".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing stdout pipe on native history window".to_string())?;

    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(ev) = parse_history_event(&line) {
                let _ = tx.send(ev);
            }
        }
    });

    let handle = NativeHistoryHandle {
        inner: Arc::new(Mutex::new(Some(NativeChild {
            stdin: Some(stdin),
            child,
            cleanup_path: None,
        }))),
    };
    Ok((handle, rx))
}

/// Grava a imagem base RGBA em um arquivo PNG temporário restrito (`0o600`) para exibição no editor nativo.
/// Input: `base_rgba` (`&[u8]`), `width` (`u32`), `height` (`u32`) | Output: `Result<PathBuf, String>`
fn write_editor_preview_png(base_rgba: &[u8], width: u32, height: u32) -> Result<PathBuf, String> {
    let png_bytes =
        encode_rgba(base_rgba, width, height, ExportFormat::Png).map_err(|e| e.to_string())?;
    let dir = ProjectDirs::from("io.github", "lx_snippingtool", "lx-snippingtool")
        .map(|p| p.cache_dir().join("preview"))
        .unwrap_or_else(|| std::env::temp_dir().join("lx-snippingtool-preview"));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let path = dir.join(format!("editor_{}.png", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(&png_bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Abre a janela nativa GTK4/Libadwaita do editor de anotações para a imagem RGBA informada.
/// Input: `base_rgba` (`&[u8]`), `width` (`u32`), `height` (`u32`), `dark` (`bool`) | Output: `Result<(NativeEditorHandle, mpsc::UnboundedReceiver<NativeEditorEvent>), String>`
pub fn spawn_editor_window(
    base_rgba: &[u8],
    width: u32,
    height: u32,
    dark: bool,
) -> Result<
    (
        NativeEditorHandle,
        mpsc::UnboundedReceiver<NativeEditorEvent>,
    ),
    String,
> {
    let preview_png = write_editor_preview_png(base_rgba, width, height)?;
    let png_arg = preview_png.to_string_lossy().into_owned();
    let w_arg = width.to_string();
    let h_arg = height.to_string();
    let dark_arg = if dark { "1" } else { "0" };

    let mut child = match Command::new("python3")
        .args([
            "-u",
            "-c",
            NATIVE_GTK_SCRIPT,
            "editor",
            &png_arg,
            &w_arg,
            &h_arg,
            dark_arg,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&preview_png);
            return Err(format!("failed to spawn native GTK editor window: {e}"));
        }
    };

    let Some(stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(&preview_png);
        return Err("missing stdin pipe on native editor window".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(&preview_png);
        return Err("missing stdout pipe on native editor window".to_string());
    };

    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(ev) = parse_editor_event(&line) {
                let _ = tx.send(ev);
            }
        }
    });

    let handle = NativeEditorHandle {
        inner: Arc::new(Mutex::new(Some(NativeChild {
            stdin: Some(stdin),
            child,
            cleanup_path: Some(preview_png),
        }))),
    };
    Ok((handle, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lx_snip_core::scene::{Color, Point, ShapeKind};

    #[test]
    fn parses_done_events() {
        assert_eq!(parse_done_event("save\n"), Some(NativeDoneEvent::Save));
        assert_eq!(parse_done_event("save_as"), Some(NativeDoneEvent::SaveAs));
        assert_eq!(
            parse_done_event(" discard "),
            Some(NativeDoneEvent::Discard)
        );
        assert_eq!(parse_done_event("unknown"), None);
    }

    #[test]
    fn parses_bar_events() {
        assert_eq!(
            parse_bar_event("toggle_pause"),
            Some(NativeBarEvent::TogglePause)
        );
        assert_eq!(parse_bar_event("stop\n"), Some(NativeBarEvent::Stop));
        assert_eq!(parse_bar_event("closed"), Some(NativeBarEvent::Closed));
        assert_eq!(parse_bar_event(""), None);
    }

    #[test]
    fn parses_history_events() {
        assert_eq!(
            parse_history_event(r#"{"cmd":"refresh"}"#),
            Some(NativeHistoryEvent::Refresh)
        );
        assert_eq!(
            parse_history_event(r#"{"cmd":"copy","path":"/tmp/a.png"}"#),
            Some(NativeHistoryEvent::Copy(PathBuf::from("/tmp/a.png")))
        );
        assert_eq!(
            parse_history_event(r#"{"cmd":"edit","path":"/tmp/b.png"}"#),
            Some(NativeHistoryEvent::Edit(PathBuf::from("/tmp/b.png")))
        );
        assert_eq!(
            parse_history_event(r#"{"cmd":"delete","path":"/tmp/c.png"}"#),
            Some(NativeHistoryEvent::Delete(PathBuf::from("/tmp/c.png")))
        );
        assert_eq!(
            parse_history_event(r#"{"cmd":"close"}"#),
            Some(NativeHistoryEvent::Closed)
        );
        assert_eq!(parse_history_event("invalid"), None);
    }

    #[test]
    fn parses_editor_events() {
        let pen_json = r#"{"cmd":"push_op","op":{"Pen":{"points":[{"x":1.0,"y":2.0}],"color":{"r":220,"g":38,"b":38,"a":255},"width":4.0}}}"#;
        assert_eq!(
            parse_editor_event(pen_json),
            Some(NativeEditorEvent::PushOp(AnnotationOp::Pen {
                points: vec![Point::new(1.0, 2.0)],
                color: Color::RED,
                width: 4.0,
            }))
        );

        let shape_json = r#"{"cmd":"push_op","op":{"Shape":{"kind":"Arrow","start":{"x":0.0,"y":0.0},"end":{"x":20.0,"y":10.0},"color":{"r":0,"g":0,"b":0,"a":255},"width":2.0}}}"#;
        assert_eq!(
            parse_editor_event(shape_json),
            Some(NativeEditorEvent::PushOp(AnnotationOp::Shape {
                kind: ShapeKind::Arrow,
                start: Point::new(0.0, 0.0),
                end: Point::new(20.0, 10.0),
                color: Color::BLACK,
                width: 2.0,
            }))
        );

        let redact_json = r#"{"cmd":"push_op","op":{"RedactBox":{"rect":{"x":5.0,"y":6.0,"width":30.0,"height":15.0}}}}"#;
        assert_eq!(
            parse_editor_event(redact_json),
            Some(NativeEditorEvent::PushOp(AnnotationOp::RedactBox {
                rect: Rect::new(5.0, 6.0, 30.0, 15.0),
            }))
        );

        assert_eq!(
            parse_editor_event(r#"{"cmd":"erase","x":12.0,"y":24.0}"#),
            Some(NativeEditorEvent::Erase {
                x: 12.0,
                y: 24.0,
                radius: 10.0,
            })
        );
        assert_eq!(
            parse_editor_event(
                r#"{"cmd":"set_crop","crop":{"x":1.0,"y":2.0,"width":40.0,"height":30.0}}"#
            ),
            Some(NativeEditorEvent::SetCrop(Some(Rect::new(
                1.0, 2.0, 40.0, 30.0
            ))))
        );
        assert_eq!(
            parse_editor_event(r#"{"cmd":"reset_crop"}"#),
            Some(NativeEditorEvent::SetCrop(None))
        );
        assert_eq!(
            parse_editor_event(r#"{"cmd":"copy"}"#),
            Some(NativeEditorEvent::Copy)
        );
        assert_eq!(
            parse_editor_event(r#"{"cmd":"save"}"#),
            Some(NativeEditorEvent::Save)
        );
        assert_eq!(
            parse_editor_event(r#"{"cmd":"history"}"#),
            Some(NativeEditorEvent::OpenHistory)
        );
        assert_eq!(
            parse_editor_event(r#"{"cmd":"new"}"#),
            Some(NativeEditorEvent::NewCapture)
        );
        assert_eq!(
            parse_editor_event(r#"{"cmd":"close"}"#),
            Some(NativeEditorEvent::Closed)
        );
    }

    #[test]
    fn formats_bar_and_error_commands() {
        assert_eq!(format_bar_state_cmd(75, true, false), "state:75:1:0");
        assert_eq!(format_bar_state_cmd(0, false, true), "state:0:0:1");
        assert_eq!(
            format_done_error_cmd("falha linha 1\nlinha 2\r"),
            "error:falha linha 1 linha 2 "
        );
        assert_eq!(format_delay_tick_cmd(3), "tick:3");
    }

    #[test]
    fn parses_toast_outcome() {
        assert!(parse_toast_outcome("ready\n"));
        assert!(!parse_toast_outcome("cancel\n"));
        assert!(!parse_toast_outcome(""));
    }

    #[test]
    fn embedded_python_script_parses_cleanly() {
        let out = Command::new("python3")
            .args(["-c", "import ast, sys; ast.parse(sys.stdin.read())"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();

        if let Ok(mut child) = out {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(NATIVE_GTK_SCRIPT.as_bytes());
            }
            let status = child.wait_with_output().unwrap();
            assert!(
                status.status.success(),
                "python syntax error: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
    }
}
