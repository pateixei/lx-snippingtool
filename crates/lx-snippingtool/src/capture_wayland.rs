//! Módulo: `capture_wayland`
//! Objetivos: Capturar quadros e abrir sessões de gravação em Wayland via portal `ScreenCast` (`ashpd`), vinculando fonte/resolução, validando `spa_chunk` do PipeWire e sem fallback `Screenshot`.
//! Principais itens: `PortalCaptureBackend`, `WaylandSource`, `WaylandSourceBinding`, `WaylandScreencastSession`, `capture_desktop_with_binding`, `open_screencast_session_for`, `query_wayland_outputs`, `trim_transparent_border`
//! Última alteração: 2026-11-19

use std::cell::Cell;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use lx_snip_core::platform::{
    CaptureBackend, CapturedImage, OutputGeometry, PlatformError, PlatformFuture,
};
use lx_snip_core::storage::TokenStore;
use pipewire as pw;
use pw::spa;
use spa::param::video::VideoFormat;
use spa::pod::Pod;
use tracing::{debug, warn};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

const APP_ID: &str = "io.github.lx_snippingtool.SnippingTool";
const PIPEWIRE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME_RGBA_BYTES: usize = 256 * 1024 * 1024;

/// Backend de captura de tela para Wayland baseado no portal XDG `ScreenCast` e PipeWire.
/// Armazena: unidade sem estado (unit struct) | Principais métodos: `new`, `supports_window_mode`, `capture_desktop`, `capture_window`
#[derive(Debug, Default, Clone, Copy)]
pub struct PortalCaptureBackend;

impl PortalCaptureBackend {
    /// Cria uma nova instância padrão de `PortalCaptureBackend`.
    /// Input: nenhum | Output: instância de `PortalCaptureBackend`
    pub const fn new() -> Self {
        Self
    }
}

impl CaptureBackend for PortalCaptureBackend {
    /// Informa se a captura de janela dedicada é exposta pelo modo padrão de backend.
    /// Input: `&self` | Output: `false` (seleção de janela é invocada diretamente por fluxo específico)
    fn supports_window_mode(&self) -> bool {
        // Window mode uses ScreenCast source selection if invoked via `--mode window`.
        false
    }

    /// Captura um quadro estático de monitor/área de trabalho via portal Wayland ScreenCast.
    /// Input: `&self` | Output: `PlatformFuture<'_, CapturedImage>` com os pixels RGBA capturados
    fn capture_desktop(&self) -> PlatformFuture<'_, CapturedImage> {
        Box::pin(async move { capture_via_screencast(SourceType::Monitor).await })
    }

    /// Captura um quadro estático de uma janela escolhida no seletor do portal Wayland.
    /// Input: `&self` | Output: `PlatformFuture<'_, CapturedImage>` com os pixels RGBA da janela
    fn capture_window(&self) -> PlatformFuture<'_, CapturedImage> {
        Box::pin(async move { capture_via_screencast(SourceType::Window).await })
    }
}

/// Registra o aplicativo no `org.freedesktop.host.portal.Registry` via D-Bus para identificação no compositor.
/// Input: nenhum | Output: `()` (falhas são apenas registradas em log de depuração)
async fn register_host_app() {
    let Ok(app_id) = ashpd::AppID::try_from(APP_ID) else {
        return;
    };
    let conn = match ashpd::zbus::Connection::session().await {
        Ok(c) => c,
        Err(err) => {
            debug!(error = %err, "failed to open session D-Bus for portal Registry.Register");
            return;
        }
    };
    let options: HashMap<String, ashpd::zbus::zvariant::Value<'_>> = HashMap::new();
    if let Err(err) = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.host.portal.Registry"),
            "Register",
            &(app_id.as_ref(), options),
        )
        .await
    {
        debug!(error = %err, "org.freedesktop.host.portal.Registry.Register returned non-fatal error");
    }
}

/// Tipo de origem selecionado para abrir uma sessão `ScreenCast` no portal Wayland.
/// Armazena: variantes `Monitor` e `Window` | Principais métodos: `to_portal`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaylandSource {
    /// Capture an entire monitor.
    Monitor,
    /// Capture a single window chosen via the portal picker.
    Window,
}

impl WaylandSource {
    /// Converte `WaylandSource` no tipo `SourceType` correspondente da biblioteca `ashpd`.
    /// Input: `self` | Output: `SourceType::Monitor` ou `SourceType::Window`
    fn to_portal(self) -> SourceType {
        match self {
            Self::Monitor => SourceType::Monitor,
            Self::Window => SourceType::Window,
        }
    }
}

/// Metadados da fonte negociada pelo portal XDG `ScreenCast` para vincular captura inicial e gravação em Wayland.
/// Armazena: `source_type`, `stream_id`, `mapping_id`, `position`, `logical_size`, `pixel_width`, `pixel_height` | Principais métodos: `validate_session`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaylandSourceBinding {
    /// Tipo de origem reportado pelo portal (`Monitor` ou `Window`).
    pub source_type: Option<SourceType>,
    /// Identificador do stream reportado pelo portal.
    pub stream_id: Option<String>,
    /// Identificador persistente de mapeamento da fonte no compositor.
    pub mapping_id: Option<String>,
    /// Posição lógica `(x, y)` da origem, quando informada pelo portal.
    pub position: Option<(i32, i32)>,
    /// Dimensões lógicas `(width, height)`, quando informadas pelo portal.
    pub logical_size: Option<(i32, i32)>,
    /// Largura real em pixels negociada no quadro capturado.
    pub pixel_width: u32,
    /// Altura real em pixels negociada no quadro capturado.
    pub pixel_height: u32,
}

impl WaylandSourceBinding {
    /// Valida se a sessão `ScreenCast` recém-aberta corresponde à mesma fonte e geometria vinculada na seleção.
    /// Input: `&self` e `session` (`&WaylandScreencastSession`) | Output: `Result<(), String>`.
    pub fn validate_session(&self, session: &WaylandScreencastSession) -> Result<(), String> {
        if let (Some(expected), Some(actual)) = (self.source_type, session.source_type) {
            if expected != actual {
                return Err(format!(
                    "fonte Wayland incompatível: esperado {expected:?}, recebido {actual:?}"
                ));
            }
        }
        if let (Some(expected), Some(actual)) = (&self.mapping_id, &session.mapping_id) {
            if expected != actual {
                return Err(format!(
                    "monitor/origem Wayland alterado entre seleção e gravação ({expected} != {actual})"
                ));
            }
        }
        if let (Some(expected), Some(actual)) = (self.logical_size, session.logical_size) {
            if expected != actual {
                return Err(format!(
                    "resolução lógica da fonte Wayland alterada ({expected:?} != {actual:?})"
                ));
            }
        }
        if let (Some(expected), Some(actual)) = (self.position, session.position) {
            if expected != actual {
                return Err(format!(
                    "posição lógica da fonte Wayland alterada ({expected:?} != {actual:?})"
                ));
            }
        }
        Ok(())
    }
}

/// Sessão `ScreenCast` ativa no portal Wayland para gravação de tela contínua.
/// Armazena: `pw_fd`, `node_id`, `had_saved_token`, `source_type`, `stream_id`, `mapping_id`, `position`, `logical_size`, `proxy`, `session` | Principais métodos: `reopen_pipewire_remote`, `close`
pub struct WaylandScreencastSession {
    /// PipeWire remote fd to pass to `pipewiresrc`.
    pub pw_fd: OwnedFd,
    /// PipeWire node ID for the selected monitor/window stream.
    pub node_id: u32,
    /// Whether a previously saved `restore_token` existed before starting this session.
    pub had_saved_token: bool,
    /// Tipo de origem selecionado para este stream.
    pub source_type: Option<SourceType>,
    /// Identificador do stream reportado pelo portal.
    pub stream_id: Option<String>,
    /// Identificador persistente de mapeamento da fonte, quando disponível.
    pub mapping_id: Option<String>,
    /// Posição lógica da origem reportada pelo portal.
    pub position: Option<(i32, i32)>,
    /// Dimensões lógicas da origem reportadas pelo portal.
    pub logical_size: Option<(i32, i32)>,
    proxy: Screencast<'static>,
    session: ashpd::desktop::Session<'static, Screencast<'static>>,
}

impl WaylandScreencastSession {
    /// Reabre o descritor remoto do PipeWire em uma sessão `ScreenCast` ativa ao retomar gravação.
    /// Input: `&mut self` | Output: `Result<(), PlatformError>` atualizando `self.pw_fd`
    pub async fn reopen_pipewire_remote(&mut self) -> Result<(), PlatformError> {
        self.pw_fd = self
            .proxy
            .open_pipe_wire_remote(&self.session)
            .await
            .map_err(|err| {
                PlatformError::CaptureFailed(format!("failed to reopen PipeWire remote fd: {err}"))
            })?;
        Ok(())
    }

    /// Encerra a sessão `ScreenCast` no D-Bus para desligar o indicador de compartilhamento do compositor.
    /// Input: `self` | Output: `()`
    pub async fn close(self) {
        let _ = self.session.close().await;
    }
}

/// Abre uma sessão ScreenCast no portal Wayland configurando tipo de origem e modo de cursor.
/// Input: `source_type` (`SourceType`), `cursor_mode` (`CursorMode`) | Output: `Result<WaylandScreencastSession, PlatformError>`
async fn open_screencast_session_with_options(
    source_type: SourceType,
    cursor_mode: CursorMode,
) -> Result<WaylandScreencastSession, PlatformError> {
    register_host_app().await;

    let proxy = Screencast::new().await.map_err(|err| {
        PlatformError::CaptureFailed(format!(
            "PipeWire / ScreenCast portal unavailable: {err}. Ensure xdg-desktop-portal and PipeWire are running."
        ))
    })?;

    let session = proxy.create_session().await.map_err(|err| {
        PlatformError::CaptureFailed(format!("failed to create ScreenCast session: {err}"))
    })?;

    let mut token_store = TokenStore::load_default();
    let saved_token = if source_type == SourceType::Monitor {
        token_store.screencast_restore_token.clone()
    } else {
        None
    };
    let had_saved_token = saved_token.is_some();

    proxy
        .select_sources(
            &session,
            cursor_mode,
            source_type.into(),
            false,
            saved_token.as_deref(),
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(map_ashpd_err)?;

    let response = proxy
        .start(&session, None)
        .await
        .map_err(map_ashpd_err)?
        .response()
        .map_err(map_ashpd_err)?;

    if source_type == SourceType::Monitor {
        if let Some(new_token) = response.restore_token() {
            token_store.screencast_restore_token = Some(new_token.to_string());
            if let Err(err) = token_store.save_default() {
                warn!(error = %err, "failed to persist ScreenCast restore_token");
            }
        }
    }

    let stream = response.streams().first().ok_or_else(|| {
        PlatformError::CaptureFailed(
            "ScreenCast portal returned no PipeWire streams (PipeWire support may be missing)"
                .to_string(),
        )
    })?;
    let node_id = stream.pipe_wire_node_id();
    let stream_source_type = stream.source_type();
    let stream_id = stream.id().map(ToString::to_string);
    let mapping_id = stream.mapping_id().map(ToString::to_string);
    let position = stream.position();
    let logical_size = stream.size();

    let pw_fd = proxy.open_pipe_wire_remote(&session).await.map_err(|err| {
        PlatformError::CaptureFailed(format!(
            "failed to open PipeWire remote fd (PipeWire support missing or denied): {err}"
        ))
    })?;

    Ok(WaylandScreencastSession {
        pw_fd,
        node_id,
        had_saved_token,
        source_type: stream_source_type,
        stream_id,
        mapping_id,
        position,
        logical_size,
        proxy,
        session,
    })
}

/// Captura o monitor em Wayland retornando tanto a imagem quanto o vínculo (`WaylandSourceBinding`) negociado no portal.
/// Input: nenhum | Output: `Result<(CapturedImage, WaylandSourceBinding), PlatformError>`.
pub async fn capture_desktop_with_binding(
) -> Result<(CapturedImage, WaylandSourceBinding), PlatformError> {
    capture_via_screencast_with_binding(SourceType::Monitor).await
}

/// Abre uma sessão `ScreenCast` persistente no portal (reaproveitando `restore_token` em monitores) para gravação de vídeo com cursor embutido.
/// Input: `source` (`WaylandSource`) | Output: `Result<WaylandScreencastSession, PlatformError>`
pub async fn open_screencast_session_for(
    source: WaylandSource,
) -> Result<WaylandScreencastSession, PlatformError> {
    open_screencast_session_with_options(source.to_portal(), CursorMode::Embedded).await
}

/// Realiza a captura de um único quadro via sessão temporária do portal ScreenCast e PipeWire.
/// Input: `source_type` (`SourceType`) | Output: `Result<CapturedImage, PlatformError>`
async fn capture_via_screencast(source_type: SourceType) -> Result<CapturedImage, PlatformError> {
    let (img, _binding) = capture_via_screencast_with_binding(source_type).await?;
    Ok(img)
}

/// Realiza captura via portal `ScreenCast` retornando a imagem e os metadados de vinculação da fonte.
/// Input: `source_type` (`SourceType`) | Output: `Result<(CapturedImage, WaylandSourceBinding), PlatformError>`.
async fn capture_via_screencast_with_binding(
    source_type: SourceType,
) -> Result<(CapturedImage, WaylandSourceBinding), PlatformError> {
    let WaylandScreencastSession {
        pw_fd,
        node_id,
        had_saved_token,
        source_type: stream_source_type,
        stream_id,
        mapping_id,
        position,
        logical_size,
        session,
        ..
    } = open_screencast_session_with_options(source_type, CursorMode::Hidden).await?;

    let prefer_alpha = source_type == SourceType::Window;
    let frame_res = tokio::task::spawn_blocking(move || {
        grab_single_pipewire_frame(pw_fd, node_id, !had_saved_token, prefer_alpha)
    })
    .await
    .map_err(|err| PlatformError::CaptureFailed(format!("PipeWire worker panicked: {err}")))?;

    // Close ScreenCast session immediately so the compositor sharing indicator disappears.
    let _ = session.close().await;

    let (mut rgba, mut width, mut height) = frame_res?;
    if prefer_alpha {
        if let Some((trimmed, w, h)) = trim_transparent_border(&rgba, width, height) {
            rgba = trimmed;
            width = w;
            height = h;
        }
    }

    let binding = WaylandSourceBinding {
        source_type: stream_source_type,
        stream_id,
        mapping_id,
        position,
        logical_size,
        pixel_width: width,
        pixel_height: height,
    };

    let outputs = match query_wayland_outputs() {
        Ok(outs) if !outs.is_empty() => outs,
        Ok(_) => vec![fallback_output(width, height)],
        Err(err) => {
            warn!(error = %err, "failed to query Wayland outputs; falling back to single virtual output");
            vec![fallback_output(width, height)]
        }
    };

    Ok((
        CapturedImage {
            rgba,
            width,
            height,
            outputs,
        },
        binding,
    ))
}

type CapturedFrame = (Vec<u8>, u32, u32);

/// Remove a margem transparente (sombras/bordas CSD) de um quadro RGBA devolvendo apenas a área opaca da janela.
/// Input: `rgba` (`&[u8]`), `width` (`u32`), `height` (`u32`) | Output: `Option<(Vec<u8>, u32, u32)>` recortado, ou `None` se nada a recortar (sem alfa ou sem margem)
fn trim_transparent_border(rgba: &[u8], width: u32, height: u32) -> Option<CapturedFrame> {
    const OPAQUE_MIN: u8 = 250;
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0usize, 0usize);
    for y in 0..h {
        let row = &rgba[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            if row[x * 4 + 3] >= OPAQUE_MIN {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    if min_x > max_x || min_y > max_y {
        return None;
    }
    let (cw, ch) = (max_x - min_x + 1, max_y - min_y + 1);
    if cw == w && ch == h {
        return None;
    }
    let mut out = Vec::with_capacity(cw * ch * 4);
    for y in min_y..=max_y {
        let start = (y * w + min_x) * 4;
        out.extend_from_slice(&rgba[start..start + cw * 4]);
    }
    Some((out, cw as u32, ch as u32))
}

/// Estado compartilhado nos callbacks PipeWire durante a captura de um único quadro.
/// Armazena: `format`, `captured`, `error` | Principais métodos: nenhum (tipo de estado interno)
#[derive(Default)]
struct StreamFrameState {
    format: Option<spa::param::video::VideoInfoRaw>,
    captured: Arc<Mutex<Option<CapturedFrame>>>,
    error: Arc<Mutex<Option<String>>>,
}

/// Conecta-se a um nó PipeWire existente e extrai um único quadro convertido para RGBA.
/// Input: `pw_fd` (`OwnedFd`), `node_id` (`u32`), `had_portal_dialog` (`bool`), `prefer_alpha` (`bool`, prioriza formatos com canal alfa para janelas) | Output: `Result<(Vec<u8>, u32, u32), PlatformError>` com buffer RGBA, largura e altura
fn grab_single_pipewire_frame(
    pw_fd: OwnedFd,
    node_id: u32,
    had_portal_dialog: bool,
    prefer_alpha: bool,
) -> Result<(Vec<u8>, u32, u32), PlatformError> {
    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|err| {
        PlatformError::CaptureFailed(format!("PipeWire MainLoop init failed: {err}"))
    })?;
    let context = pw::context::Context::new(&mainloop).map_err(|err| {
        PlatformError::CaptureFailed(format!("PipeWire Context init failed: {err}"))
    })?;
    let core = context.connect_fd(pw_fd, None).map_err(|err| {
        PlatformError::CaptureFailed(format!("PipeWire connect_fd failed: {err}"))
    })?;

    let state = StreamFrameState::default();
    let captured_ref = Arc::clone(&state.captured);
    let error_ref = Arc::clone(&state.error);

    let stream = pw::stream::Stream::new(
        &core,
        "lx-snippingtool-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|err| PlatformError::CaptureFailed(format!("PipeWire stream create failed: {err}")))?;

    let mainloop_weak = mainloop.downgrade();
    let mainloop_timeout = mainloop.downgrade();
    let done = Rc::new(Cell::new(false));
    let done_process = Rc::clone(&done);
    let started_at = Instant::now();
    let first_frame_at: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let first_frame_timer = Rc::clone(&first_frame_at);
    let first_frame_process = Rc::clone(&first_frame_at);
    let frame_count = Rc::new(Cell::new(0u32));
    let frame_count_process = Rc::clone(&frame_count);

    let settle_duration = if had_portal_dialog {
        Duration::from_millis(180)
    } else {
        Duration::from_millis(40)
    };
    let target_frames: u32 = if had_portal_dialog { 6 } else { 2 };

    let _timer = mainloop.loop_().add_timer(move |_| {
        let settled = first_frame_timer
            .get()
            .is_some_and(|t| t.elapsed() >= settle_duration);
        if settled || started_at.elapsed() >= PIPEWIRE_TIMEOUT {
            if let Some(ml) = mainloop_timeout.upgrade() {
                ml.quit();
            }
        }
    });
    let _ = _timer.update_timer(
        Some(Duration::from_millis(20)),
        Some(Duration::from_millis(20)),
    );

    let _listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(move |_, _, old, new| {
            debug!(?old, ?new, "PipeWire stream state changed");
        })
        .param_changed(move |_, user_data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let (media_type, media_subtype) = match spa::param::format_utils::parse_format(param) {
                Ok(v) => v,
                Err(_) => return,
            };
            if media_type != spa::param::format::MediaType::Video
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let mut info = spa::param::video::VideoInfoRaw::new();
            if info.parse(param).is_ok() {
                user_data.format = Some(info);
            }
        })
        .process(move |stream, user_data| {
            if done_process.get() || started_at.elapsed() > PIPEWIRE_TIMEOUT {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let Some(fmt) = user_data.format else {
                return;
            };
            let w = fmt.size().width;
            let h = fmt.size().height;
            if w == 0 || h == 0 {
                return;
            }

            let data_type_invalid = datas[0].type_() == spa::buffer::DataType::Invalid;
            let chunk = datas[0].chunk();
            let corrupted = chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED);
            let chunk_offset = chunk.offset();
            let chunk_size = chunk.size();
            let stride = chunk.stride();
            let Some(slice) = datas[0].data() else {
                return;
            };

            match extract_and_convert_spa_frame(
                fmt.format(),
                slice,
                data_type_invalid,
                corrupted,
                chunk_offset,
                chunk_size,
                stride,
                w,
                h,
            ) {
                Ok(rgba) => {
                    if let Ok(mut guard) = user_data.captured.lock() {
                        *guard = Some((rgba, w, h));
                    }
                    let count = frame_count_process.get().saturating_add(1);
                    frame_count_process.set(count);
                    if first_frame_process.get().is_none() {
                        first_frame_process.set(Some(Instant::now()));
                    }
                    let settled = first_frame_process
                        .get()
                        .is_some_and(|t| t.elapsed() >= settle_duration);
                    if count >= target_frames || settled {
                        done_process.set(true);
                        if let Some(ml) = mainloop_weak.upgrade() {
                            ml.quit();
                        }
                    }
                }
                Err(msg) => {
                    debug!(reason = %msg, "ignoring invalid or incomplete PipeWire frame");
                    if let Ok(mut guard) = user_data.error.lock() {
                        *guard = Some(msg);
                    }
                }
            }
        })
        .register()
        .map_err(|err| {
            PlatformError::CaptureFailed(format!("PipeWire listener register failed: {err}"))
        })?;

    let obj = if prefer_alpha {
        spa::pod::object!(
            spa::utils::SpaTypes::ObjectParamFormat,
            spa::param::ParamType::EnumFormat,
            spa::pod::property!(
                spa::param::format::FormatProperties::MediaType,
                Id,
                spa::param::format::MediaType::Video
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::MediaSubtype,
                Id,
                spa::param::format::MediaSubtype::Raw
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                VideoFormat::BGRA,
                VideoFormat::BGRA,
                VideoFormat::RGBA,
                VideoFormat::BGRx,
                VideoFormat::RGBx
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                spa::utils::Rectangle {
                    width: 1920,
                    height: 1080
                },
                spa::utils::Rectangle {
                    width: 1,
                    height: 1
                },
                spa::utils::Rectangle {
                    width: 16384,
                    height: 16384
                }
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                spa::utils::Fraction { num: 60, denom: 1 },
                spa::utils::Fraction { num: 0, denom: 1 },
                spa::utils::Fraction { num: 144, denom: 1 }
            ),
        )
    } else {
        spa::pod::object!(
            spa::utils::SpaTypes::ObjectParamFormat,
            spa::param::ParamType::EnumFormat,
            spa::pod::property!(
                spa::param::format::FormatProperties::MediaType,
                Id,
                spa::param::format::MediaType::Video
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::MediaSubtype,
                Id,
                spa::param::format::MediaSubtype::Raw
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                VideoFormat::BGRx,
                VideoFormat::BGRx,
                VideoFormat::BGRA,
                VideoFormat::RGBx,
                VideoFormat::RGBA
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                spa::utils::Rectangle {
                    width: 1920,
                    height: 1080
                },
                spa::utils::Rectangle {
                    width: 1,
                    height: 1
                },
                spa::utils::Rectangle {
                    width: 16384,
                    height: 16384
                }
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                spa::utils::Fraction { num: 60, denom: 1 },
                spa::utils::Fraction { num: 0, denom: 1 },
                spa::utils::Fraction { num: 144, denom: 1 }
            ),
        )
    };
    let pod_bytes: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|err| PlatformError::CaptureFailed(format!("SPA pod serialize failed: {err}")))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&pod_bytes)
        .ok_or_else(|| PlatformError::CaptureFailed("invalid SPA format pod bytes".to_string()))?];

    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|err| {
            PlatformError::CaptureFailed(format!("PipeWire stream connect failed: {err}"))
        })?;

    mainloop.run();

    if let Ok(mut guard) = captured_ref.lock() {
        if let Some(frame) = guard.take() {
            return Ok(frame);
        }
    }
    if let Ok(mut err_guard) = error_ref.lock() {
        if let Some(err) = err_guard.take() {
            return Err(PlatformError::CaptureFailed(err));
        }
    }
    Err(PlatformError::CaptureFailed(
        "timed out waiting for PipeWire ScreenCast frame (PipeWire support may be unavailable)"
            .to_string(),
    ))
}

/// Valida o contrato de `spa_chunk` (`DataType`, flags, `offset`, `size`, `stride` e limites) antes de alocar e converter para RGBA8.
/// Input: formato, bytes mapeados, sinalizadores de erro, `chunk_offset`, `chunk_size`, `stride`, `width` e `height` | Output: `Result<Vec<u8>, String>`.
#[allow(clippy::too_many_arguments)]
fn extract_and_convert_spa_frame(
    fmt: VideoFormat,
    mapped: &[u8],
    data_type_invalid: bool,
    corrupted: bool,
    chunk_offset: u32,
    chunk_size: u32,
    stride: i32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    if data_type_invalid {
        return Err("PipeWire SPA buffer has Invalid data type".to_string());
    }
    if corrupted {
        return Err("PipeWire SPA chunk is flagged CORRUPTED".to_string());
    }
    if !matches!(
        fmt,
        VideoFormat::BGRx | VideoFormat::BGRA | VideoFormat::RGBx | VideoFormat::RGBA
    ) {
        return Err(format!("unsupported PipeWire video format: {fmt:?}"));
    }

    let w = usize::try_from(width).map_err(|_| "frame width overflow".to_string())?;
    let h = usize::try_from(height).map_err(|_| "frame height overflow".to_string())?;
    if w == 0 || h == 0 {
        return Err("PipeWire frame has zero width or height".to_string());
    }

    let row_bytes = w
        .checked_mul(4)
        .ok_or_else(|| "PipeWire row byte size overflow".to_string())?;
    let rgba_len = row_bytes
        .checked_mul(h)
        .ok_or_else(|| "PipeWire frame RGBA size overflow".to_string())?;
    if rgba_len > MAX_FRAME_RGBA_BYTES {
        return Err(format!(
            "PipeWire frame exceeds max limit ({rgba_len} > {MAX_FRAME_RGBA_BYTES})"
        ));
    }

    let stride_usize = usize::try_from(stride)
        .ok()
        .filter(|&s| s >= row_bytes)
        .ok_or_else(|| format!("invalid PipeWire stride {stride} for row_bytes {row_bytes}"))?;

    if mapped.is_empty() || chunk_size == 0 {
        return Err("empty PipeWire SPA chunk or mapped buffer".to_string());
    }

    let offset = (chunk_offset as usize) % mapped.len();
    let size =
        usize::try_from(chunk_size).map_err(|_| "PipeWire chunk_size overflow".to_string())?;
    let valid_end = offset
        .checked_add(size)
        .ok_or_else(|| "PipeWire chunk offset+size overflow".to_string())?;
    if valid_end > mapped.len() {
        return Err(format!(
            "PipeWire chunk bounds [{offset}..{valid_end}] exceed mapped len {}",
            mapped.len()
        ));
    }

    let valid_slice = &mapped[offset..valid_end];
    let required_bytes = (h - 1)
        .checked_mul(stride_usize)
        .and_then(|base| base.checked_add(row_bytes))
        .ok_or_else(|| "PipeWire required frame bytes overflow".to_string())?;
    if valid_slice.len() < required_bytes {
        return Err(format!(
            "PipeWire chunk valid bytes ({}) smaller than required frame layout ({required_bytes})",
            valid_slice.len()
        ));
    }

    let mut rgba = vec![0u8; rgba_len];
    for y in 0..h {
        let src_row_start = y * stride_usize;
        let src = &valid_slice[src_row_start..src_row_start + row_bytes];
        let dst = &mut rgba[y * row_bytes..(y + 1) * row_bytes];

        match fmt {
            VideoFormat::BGRx => {
                for (s, d) in src
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst.as_chunks_mut::<4>().0.iter_mut())
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = 255;
                }
            }
            VideoFormat::BGRA => {
                for (s, d) in src
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst.as_chunks_mut::<4>().0.iter_mut())
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
            }
            VideoFormat::RGBx => {
                for (s, d) in src
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst.as_chunks_mut::<4>().0.iter_mut())
                {
                    d[0] = s[0];
                    d[1] = s[1];
                    d[2] = s[2];
                    d[3] = 255;
                }
            }
            VideoFormat::RGBA => {
                dst.copy_from_slice(src);
            }
            _ => unreachable!("validated supported VideoFormat upfront"),
        }
    }

    Ok(rgba)
}

/// Converte um erro da biblioteca `ashpd` em `PlatformError` preservando cancelamento pelo usuário.
/// Input: `err` (`ashpd::Error`) | Output: `PlatformError` equivalente
fn map_ashpd_err(err: ashpd::Error) -> PlatformError {
    match err {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            PlatformError::Cancelled
        }
        other => PlatformError::CaptureFailed(other.to_string()),
    }
}

/// Gera uma geometria de saída sintética única quando o compositor Wayland não informa monitores.
/// Input: `width` (`u32`), `height` (`u32`) | Output: `OutputGeometry` cobrindo toda a imagem
fn fallback_output(width: u32, height: u32) -> OutputGeometry {
    OutputGeometry {
        name: "wayland-0".to_string(),
        x: 0,
        y: 0,
        width,
        height,
        scale_factor: 1.0,
    }
}

/// Metadados brutos acumulados dos eventos Wayland `wl_output` e `zxdg_output_v1` para um monitor.
/// Armazena: `global_name`, `name`, `scale_factor`, `mode_width`, `mode_height`, `logical_x`, `logical_y`, `logical_w`, `logical_h` | Principais métodos: nenhum (tipo de dados)
#[derive(Debug, Default)]
struct RawOutputInfo {
    global_name: u32,
    name: Option<String>,
    scale_factor: i32,
    mode_width: i32,
    mode_height: i32,
    logical_x: Option<i32>,
    logical_y: Option<i32>,
    logical_w: Option<i32>,
    logical_h: Option<i32>,
}

/// Estado do cliente Wayland usado durante o roundtrip de enumeração de monitores.
/// Armazena: `xdg_output_manager`, `outputs`, `xdg_to_global` | Principais métodos: `Dispatch::event`
struct WaylandOutputState {
    xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    outputs: HashMap<u32, (wl_output::WlOutput, RawOutputInfo)>,
    xdg_to_global: HashMap<wayland_client::backend::ObjectId, u32>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandOutputState {
    /// Processa anúncios globais do registro Wayland vinculando `wl_output` e `zxdg_output_manager_v1`.
    /// Input: `state`, `registry`, `event`, `_`, `_`, `qh` | Output: `()` atualizando `state`
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_output" => {
                    let bind_ver = version.min(4);
                    let output =
                        registry.bind::<wl_output::WlOutput, _, _>(name, bind_ver, qh, name);
                    let info = RawOutputInfo {
                        global_name: name,
                        scale_factor: 1,
                        ..Default::default()
                    };
                    if let Some(mgr) = &state.xdg_output_manager {
                        let xdg = mgr.get_xdg_output(&output, qh, name);
                        state.xdg_to_global.insert(xdg.id(), name);
                    }
                    state.outputs.insert(name, (output, info));
                }
                "zxdg_output_manager_v1" => {
                    let bind_ver = version.min(3);
                    let mgr = registry.bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, _, _>(
                        name,
                        bind_ver,
                        qh,
                        (),
                    );
                    for (&global_name, (output, _)) in &state.outputs {
                        let xdg = mgr.get_xdg_output(output, qh, global_name);
                        state.xdg_to_global.insert(xdg.id(), global_name);
                    }
                    state.xdg_output_manager = Some(mgr);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for WaylandOutputState {
    /// Recebe eventos de modo atual, fator de escala e nome de cada `wl_output` registrado.
    /// Input: `state`, `_`, `event`, `global_name`, `_`, `_` | Output: `()` atualizando `RawOutputInfo`
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global_name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some((_, info)) = state.outputs.get_mut(global_name) {
            match event {
                wl_output::Event::Mode {
                    flags: WEnum::Value(f),
                    width,
                    height,
                    ..
                } if f.contains(wl_output::Mode::Current) => {
                    info.mode_width = width;
                    info.mode_height = height;
                }
                wl_output::Event::Scale { factor } => {
                    info.scale_factor = factor.max(1);
                }
                wl_output::Event::Name { name } => {
                    info.name = Some(name);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for WaylandOutputState {
    /// Trata eventos do gerenciador `zxdg_output_manager_v1` (protocolo sem eventos no cliente).
    /// Input: `_` (estado, proxy, evento, dados, conexão, fila) | Output: `()`
    fn event(
        _: &mut Self,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for WaylandOutputState {
    /// Coleta posição lógica, tamanho lógico e nome estendido reportados por `zxdg_output_v1`.
    /// Input: `state`, `_`, `event`, `global_name`, `_`, `_` | Output: `()` atualizando `RawOutputInfo`
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        global_name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some((_, info)) = state.outputs.get_mut(global_name) {
            match event {
                zxdg_output_v1::Event::LogicalPosition { x, y } => {
                    info.logical_x = Some(x);
                    info.logical_y = Some(y);
                }
                zxdg_output_v1::Event::LogicalSize { width, height } => {
                    info.logical_w = Some(width);
                    info.logical_h = Some(height);
                }
                zxdg_output_v1::Event::Name { name } if info.name.is_none() => {
                    info.name = Some(name);
                }
                _ => {}
            }
        }
    }
}

/// Consulta os monitores ativos e suas geometrias lógicas/físicas no compositor Wayland via `wl_output` e `zxdg_output_manager_v1`.
/// Input: nenhum | Output: `Result<Vec<OutputGeometry>, PlatformError>` ordenado por `global_name`
pub fn query_wayland_outputs() -> Result<Vec<OutputGeometry>, PlatformError> {
    let conn = Connection::connect_to_env()
        .map_err(|e| PlatformError::CaptureFailed(format!("wayland connect failed: {e}")))?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let mut state = WaylandOutputState {
        xdg_output_manager: None,
        outputs: HashMap::new(),
        xdg_to_global: HashMap::new(),
    };

    let _registry = display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| PlatformError::CaptureFailed(format!("wayland roundtrip 1 failed: {e}")))?;
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| PlatformError::CaptureFailed(format!("wayland roundtrip 2 failed: {e}")))?;

    let mut raw: Vec<RawOutputInfo> = state.outputs.into_values().map(|(_, i)| i).collect();
    raw.sort_by_key(|i| i.global_name);

    let geometries = raw
        .into_iter()
        .filter_map(|info| {
            let logical_w = info.logical_w.unwrap_or(info.mode_width);
            let logical_h = info.logical_h.unwrap_or(info.mode_height);
            if logical_w <= 0 || logical_h <= 0 {
                return None;
            }
            let scale_factor = if info.mode_width > 0 && info.logical_w.is_some_and(|w| w > 0) {
                f64::from(info.mode_width) / f64::from(logical_w)
            } else {
                f64::from(info.scale_factor.max(1))
            };
            Some(OutputGeometry {
                name: info
                    .name
                    .unwrap_or_else(|| format!("output-{}", info.global_name)),
                x: info.logical_x.unwrap_or(0),
                y: info.logical_y.unwrap_or(0),
                width: logical_w as u32,
                height: logical_h as u32,
                scale_factor,
            })
        })
        .collect();

    Ok(geometries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_transparent_shadow_around_opaque_window() {
        let (w, h) = (6u32, 5u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 1..4usize {
            for x in 2..5usize {
                let i = (y * w as usize + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[10, 20, 30, 255]);
            }
        }
        let (out, cw, ch) = trim_transparent_border(&rgba, w, h).unwrap();
        assert_eq!((cw, ch), (3, 3));
        assert_eq!(out.len(), 3 * 3 * 4);
        assert!(out.chunks(4).all(|p| p == [10, 20, 30, 255]));

        let opaque = vec![255u8; (w * h * 4) as usize];
        assert!(trim_transparent_border(&opaque, w, h).is_none());
    }

    #[test]
    fn spa_chunk_validation_respects_offset_bounds_flags_and_stride() {
        // 2x2 BGRx with stride=12 (4 padding bytes per row), placed at offset=8 in a 40-byte map
        let mut mapped = vec![0xEEu8; 40];
        let valid_offset = 8usize;
        // Row 0 (BGRx): pixel0=(10,20,30,0), pixel1=(40,50,60,0) -> RGB(30,20,10) & RGB(60,50,40)
        mapped[valid_offset..valid_offset + 8].copy_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);
        // Row 1 at valid_offset + 12
        mapped[valid_offset + 12..valid_offset + 20]
            .copy_from_slice(&[70, 80, 90, 0, 100, 110, 120, 0]);

        // Valid size = 12 (row0 stride) + 8 (row1) = 20
        let rgba = extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            false,
            false,
            valid_offset as u32,
            20,
            12,
            2,
            2,
        )
        .expect("valid offset+size should succeed");
        assert_eq!(
            rgba,
            vec![
                30, 20, 10, 255, 60, 50, 40, 255, // row 0
                90, 80, 70, 255, 120, 110, 100, 255, // row 1
            ]
        );

        // Wrapped ring offset (40 + 8 = 48 -> 48 % 40 = 8) still works
        assert!(extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            false,
            false,
            48,
            20,
            12,
            2,
            2,
        )
        .is_ok());

        // Reject Invalid DataType
        assert!(extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            true,
            false,
            8,
            20,
            12,
            2,
            2,
        )
        .is_err());

        // Reject CORRUPTED chunk flag
        assert!(extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            false,
            true,
            8,
            20,
            12,
            2,
            2,
        )
        .is_err());

        // Reject offset+size exceeding mapped len even though mapped has room at offset 0
        assert!(extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            false,
            false,
            32,
            16,
            8,
            2,
            2,
        )
        .is_err());

        // Reject chunk_size smaller than last row end even though mapped has remaining bytes
        assert!(extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            false,
            false,
            8,
            19,
            12,
            2,
            2,
        )
        .is_err());

        // Reject stride < width * 4
        assert!(extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            false,
            false,
            8,
            20,
            4,
            2,
            2,
        )
        .is_err());

        // Reject oversized dimensions (> 256 MiB ceiling) before allocating
        assert!(extract_and_convert_spa_frame(
            VideoFormat::BGRx,
            &mapped,
            false,
            false,
            0,
            20,
            100_000,
            20_000,
            20_000,
        )
        .is_err());
    }
}
