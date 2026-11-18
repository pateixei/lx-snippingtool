//! Wayland screen capture backend using `xdg-desktop-portal` `ScreenCast` (`ashpd`),
//! persistent `restore_token` (`TokenStore`), `pipewire` single-frame grab, and
//! `wl_output` / `zxdg_output_manager_v1` output geometry enumeration.
//!
//! No `Screenshot` fallback is used: PipeWire / `ScreenCast` support is required.

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

/// Wayland capture backend backed strictly by `org.freedesktop.portal.ScreenCast` + PipeWire.
#[derive(Debug, Default, Clone, Copy)]
pub struct PortalCaptureBackend;

impl PortalCaptureBackend {
    /// Creates a new [`PortalCaptureBackend`].
    pub const fn new() -> Self {
        Self
    }
}

impl CaptureBackend for PortalCaptureBackend {
    fn supports_window_mode(&self) -> bool {
        // Window mode uses ScreenCast source selection if invoked via `--mode window`.
        false
    }

    fn capture_desktop(&self) -> PlatformFuture<'_, CapturedImage> {
        Box::pin(async move { capture_via_screencast(SourceType::Monitor).await })
    }

    fn capture_window(&self) -> PlatformFuture<'_, CapturedImage> {
        Box::pin(async move { capture_via_screencast(SourceType::Window).await })
    }
}

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

/// Active Wayland `ScreenCast` session handle for live screen recording (Phase 2).
pub struct WaylandScreencastSession {
    /// PipeWire remote fd to pass to `pipewiresrc`.
    pub pw_fd: OwnedFd,
    /// PipeWire node ID for the selected monitor/window stream.
    pub node_id: u32,
    /// Whether a previously saved `restore_token` existed before starting this session.
    pub had_saved_token: bool,
    session: ashpd::desktop::Session<'static, Screencast<'static>>,
}

impl WaylandScreencastSession {
    /// Closes the underlying D-Bus `ScreenCast` session so the compositor indicator stops.
    pub async fn close(self) {
        let _ = self.session.close().await;
    }
}

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

    let pw_fd = proxy.open_pipe_wire_remote(&session).await.map_err(|err| {
        PlatformError::CaptureFailed(format!(
            "failed to open PipeWire remote fd (PipeWire support missing or denied): {err}"
        ))
    })?;

    Ok(WaylandScreencastSession {
        pw_fd,
        node_id,
        had_saved_token,
        session,
    })
}

/// Opens a persistent `ScreenCast` stream (reusing `restore_token`) for video recording.
pub async fn open_screencast_session() -> Result<WaylandScreencastSession, PlatformError> {
    open_screencast_session_with_options(SourceType::Monitor, CursorMode::Embedded).await
}

async fn capture_via_screencast(source_type: SourceType) -> Result<CapturedImage, PlatformError> {
    let WaylandScreencastSession {
        pw_fd,
        node_id,
        had_saved_token,
        session,
    } = open_screencast_session_with_options(source_type, CursorMode::Hidden).await?;

    let frame_res = tokio::task::spawn_blocking(move || {
        grab_single_pipewire_frame(pw_fd, node_id, !had_saved_token)
    })
    .await
    .map_err(|err| PlatformError::CaptureFailed(format!("PipeWire worker panicked: {err}")))?;

    // Close ScreenCast session immediately so the compositor sharing indicator disappears.
    let _ = session.close().await;

    let (rgba, width, height) = frame_res?;

    let outputs = match query_wayland_outputs() {
        Ok(outs) if !outs.is_empty() => outs,
        Ok(_) => vec![fallback_output(width, height)],
        Err(err) => {
            warn!(error = %err, "failed to query Wayland outputs; falling back to single virtual output");
            vec![fallback_output(width, height)]
        }
    };

    Ok(CapturedImage {
        rgba,
        width,
        height,
        outputs,
    })
}

type CapturedFrame = (Vec<u8>, u32, u32);

#[derive(Default)]
struct StreamFrameState {
    format: Option<spa::param::video::VideoInfoRaw>,
    captured: Arc<Mutex<Option<CapturedFrame>>>,
    error: Arc<Mutex<Option<String>>>,
}

fn grab_single_pipewire_frame(
    pw_fd: OwnedFd,
    node_id: u32,
    had_portal_dialog: bool,
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

            let stride = datas[0].chunk().stride().max((w * 4) as i32) as usize;
            let size = datas[0].chunk().size() as usize;
            let Some(slice) = datas[0].data() else {
                return;
            };
            if slice.is_empty() || size == 0 {
                return;
            }

            match convert_spa_to_rgba(fmt.format(), slice, w, h, stride) {
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

    let obj = spa::pod::object!(
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
    );
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

fn convert_spa_to_rgba(
    fmt: VideoFormat,
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
) -> Result<Vec<u8>, String> {
    let w = width as usize;
    let h = height as usize;
    let mut rgba = vec![0u8; w * h * 4];

    for y in 0..h {
        let src_row_start = y * stride;
        let src_row_end = src_row_start + w * 4;
        if src_row_end > data.len() {
            return Err("PipeWire buffer smaller than expected video dimensions".to_string());
        }
        let src = &data[src_row_start..src_row_end];
        let dst = &mut rgba[y * w * 4..(y + 1) * w * 4];

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
            other => {
                return Err(format!("unsupported PipeWire video format: {other:?}"));
            }
        }
    }

    Ok(rgba)
}

fn map_ashpd_err(err: ashpd::Error) -> PlatformError {
    match err {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            PlatformError::Cancelled
        }
        other => PlatformError::CaptureFailed(other.to_string()),
    }
}

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

struct WaylandOutputState {
    xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    outputs: HashMap<u32, (wl_output::WlOutput, RawOutputInfo)>,
    xdg_to_global: HashMap<wayland_client::backend::ObjectId, u32>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandOutputState {
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

/// Queries active Wayland outputs and their logical/physical geometries via
/// `wl_output` and `zxdg_output_manager_v1`.
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
