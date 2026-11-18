//! X11 screen and window capture backend using `x11rb` (`shm`, `randr`, `_NET_CLIENT_LIST`).

use std::fs::OpenOptions;
use std::os::unix::io::OwnedFd;

use lx_snip_core::platform::{
    CaptureBackend, CapturedImage, OutputGeometry, PlatformError, PlatformFuture,
};
use lx_snip_core::scene::Rect;
use tracing::{debug, warn};
use uuid::Uuid;
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::randr::{self, ConnectionExt as RandrExt};
use x11rb::protocol::shm::{self, ConnectionExt as ShmExt};
use x11rb::protocol::xproto::{self, AtomEnum, ConnectionExt as XprotoExt, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

/// Geometry and title of a mapped X11 top-level window.
#[derive(Debug, Clone)]
pub struct X11WindowBounds {
    /// X11 window ID.
    #[allow(dead_code)]
    pub window: Window,
    /// Bounding rectangle in root coordinates.
    pub rect: Rect,
}

/// X11 capture backend using XShm (`shm`), RandR (`randr`), and `_NET_CLIENT_LIST`.
#[derive(Debug, Default, Clone, Copy)]
pub struct X11CaptureBackend;

impl X11CaptureBackend {
    /// Creates a new [`X11CaptureBackend`].
    pub const fn new() -> Self {
        Self
    }
}

impl CaptureBackend for X11CaptureBackend {
    fn supports_window_mode(&self) -> bool {
        true
    }

    fn capture_desktop(&self) -> PlatformFuture<'_, CapturedImage> {
        Box::pin(async move {
            tokio::task::spawn_blocking(capture_x11_desktop_blocking)
                .await
                .map_err(|e| {
                    PlatformError::CaptureFailed(format!("X11 capture task panicked: {e}"))
                })?
        })
    }

    fn capture_window(&self) -> PlatformFuture<'_, CapturedImage> {
        // On X11, window selection happens inside the freeze-frame overlay using `_NET_CLIENT_LIST`;
        // capture_window returns the full desktop with outputs so the overlay can crop to the selected window.
        self.capture_desktop()
    }
}

/// Enumerates visible top-level windows from `_NET_CLIENT_LIST` in root-window coordinates.
pub fn list_x11_windows() -> Result<Vec<X11WindowBounds>, PlatformError> {
    let (conn, screen_num) = RustConnection::connect(None)
        .map_err(|e| PlatformError::CaptureFailed(format!("X11 connect failed: {e}")))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let net_client_list = intern_atom(&conn, b"_NET_CLIENT_LIST")?;
    let prop = conn
        .get_property(false, root, net_client_list, AtomEnum::WINDOW, 0, 4096)
        .map_err(|e| PlatformError::CaptureFailed(format!("_NET_CLIENT_LIST request failed: {e}")))?
        .reply()
        .map_err(|e| PlatformError::CaptureFailed(format!("_NET_CLIENT_LIST reply failed: {e}")))?;

    let mut windows = Vec::new();
    let Some(win_ids) = prop.value32() else {
        return Ok(windows);
    };

    for win in win_ids {
        let Ok(attrs_cookie) = conn.get_window_attributes(win) else {
            continue;
        };
        let Ok(attrs) = attrs_cookie.reply() else {
            continue;
        };
        if attrs.map_state != xproto::MapState::VIEWABLE {
            continue;
        }
        let Ok(geom_cookie) = conn.get_geometry(win) else {
            continue;
        };
        let Ok(geom) = geom_cookie.reply() else {
            continue;
        };
        let Ok(trans_cookie) = conn.translate_coordinates(win, root, 0, 0) else {
            continue;
        };
        let Ok(trans) = trans_cookie.reply() else {
            continue;
        };
        if geom.width == 0 || geom.height == 0 {
            continue;
        }
        windows.push(X11WindowBounds {
            window: win,
            rect: Rect::new(
                f32::from(trans.dst_x),
                f32::from(trans.dst_y),
                f32::from(geom.width),
                f32::from(geom.height),
            ),
        });
    }

    Ok(windows)
}

fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<u32, PlatformError> {
    conn.intern_atom(false, name)
        .map_err(|e| PlatformError::CaptureFailed(format!("intern_atom failed: {e}")))?
        .reply()
        .map(|r| r.atom)
        .map_err(|e| PlatformError::CaptureFailed(format!("intern_atom reply failed: {e}")))
}

fn capture_x11_desktop_blocking() -> Result<CapturedImage, PlatformError> {
    let (conn, screen_num) = RustConnection::connect(None)
        .map_err(|e| PlatformError::CaptureFailed(format!("X11 connect failed: {e}")))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let width = screen.width_in_pixels;
    let height = screen.height_in_pixels;

    let bgra = match capture_via_shm(&conn, root, width, height) {
        Ok(bytes) => bytes,
        Err(err) => {
            debug!(error = %err, "XShm capture unavailable, falling back to core GetImage");
            capture_via_get_image(&conn, root, width, height)?
        }
    };

    let rgba = swizzle_bgra_to_rgba(&bgra);
    let outputs = match query_randr_outputs(&conn, root) {
        Ok(outs) if !outs.is_empty() => outs,
        Ok(_) => vec![fallback_output(u32::from(width), u32::from(height))],
        Err(err) => {
            warn!(error = %err, "RandR output enumeration failed; using root screen bounds");
            vec![fallback_output(u32::from(width), u32::from(height))]
        }
    };

    Ok(CapturedImage {
        rgba,
        width: u32::from(width),
        height: u32::from(height),
        outputs,
    })
}

fn capture_via_shm(
    conn: &RustConnection,
    root: Window,
    width: u16,
    height: u16,
) -> Result<Vec<u8>, PlatformError> {
    let shm_ver = conn
        .shm_query_version()
        .map_err(|e| PlatformError::CaptureFailed(format!("shm_query_version failed: {e}")))?
        .reply()
        .map_err(|e| {
            PlatformError::CaptureFailed(format!("shm_query_version reply failed: {e}"))
        })?;

    if !shm_ver.shared_pixmaps && shm_ver.major_version == 0 {
        return Err(PlatformError::Unsupported("XShm not supported".to_string()));
    }

    let byte_len = usize::from(width) * usize::from(height) * 4;
    let shm_path = std::env::temp_dir().join(format!("lx-snip-shm-{}", Uuid::new_v4()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&shm_path)
        .map_err(|e| {
            PlatformError::CaptureFailed(format!("failed to create shm backing file: {e}"))
        })?;
    // Unlink immediately so the backing file is cleaned up even on crash.
    let _ = std::fs::remove_file(&shm_path);
    file.set_len(byte_len as u64)
        .map_err(|e| PlatformError::CaptureFailed(format!("failed to size shm file: {e}")))?;

    let dup_fd = file
        .try_clone()
        .map_err(|e| PlatformError::CaptureFailed(format!("failed to clone shm fd: {e}")))?;
    let owned_fd: OwnedFd = dup_fd.into();

    let shmseg = conn
        .generate_id()
        .map_err(|e| PlatformError::CaptureFailed(format!("X11 generate_id failed: {e}")))?;

    conn.shm_attach_fd(shmseg, owned_fd, false)
        .map_err(|e| PlatformError::CaptureFailed(format!("shm_attach_fd failed: {e}")))?;

    let reply = conn
        .shm_get_image(
            root,
            0,
            0,
            width,
            height,
            u32::MAX,
            ImageFormat::Z_PIXMAP.into(),
            shmseg,
            0,
        )
        .map_err(|e| PlatformError::CaptureFailed(format!("shm_get_image failed: {e}")))?
        .reply()
        .map_err(|e| PlatformError::CaptureFailed(format!("shm_get_image reply failed: {e}")))?;

    let _ = conn.shm_detach(shmseg);
    let _ = conn.flush();

    if reply.size as usize != byte_len {
        return Err(PlatformError::CaptureFailed(format!(
            "XShm image size mismatch: expected {byte_len}, got {}",
            reply.size
        )));
    }

    use std::io::Read;
    let mut file_reader = file;
    let mut buf = vec![0u8; byte_len];
    file_reader
        .read_exact(&mut buf)
        .map_err(|e| PlatformError::CaptureFailed(format!("failed to read shm buffer: {e}")))?;

    let _ = shm::CompletionEvent::default();
    Ok(buf)
}

fn capture_via_get_image(
    conn: &RustConnection,
    root: Window,
    width: u16,
    height: u16,
) -> Result<Vec<u8>, PlatformError> {
    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, root, 0, 0, width, height, u32::MAX)
        .map_err(|e| PlatformError::CaptureFailed(format!("get_image failed: {e}")))?
        .reply()
        .map_err(|e| PlatformError::CaptureFailed(format!("get_image reply failed: {e}")))?;

    Ok(reply.data)
}

fn swizzle_bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.as_chunks::<4>().0 {
        rgba.push(px[2]); // R
        rgba.push(px[1]); // G
        rgba.push(px[0]); // B
        rgba.push(255); // Opaque alpha for root desktop capture
    }
    rgba
}

fn query_randr_outputs(
    conn: &RustConnection,
    root: Window,
) -> Result<Vec<OutputGeometry>, PlatformError> {
    let resources = conn
        .randr_get_screen_resources_current(root)
        .map_err(|e| {
            PlatformError::CaptureFailed(format!("randr_get_screen_resources_current: {e}"))
        })?
        .reply()
        .map_err(|e| PlatformError::CaptureFailed(format!("randr resources reply: {e}")))?;

    let mut outputs = Vec::new();
    for output in resources.outputs {
        let Ok(info_cookie) = conn.randr_get_output_info(output, resources.config_timestamp) else {
            continue;
        };
        let Ok(info) = info_cookie.reply() else {
            continue;
        };
        if info.connection != randr::Connection::CONNECTED || info.crtc == 0 {
            continue;
        }
        let Ok(crtc_cookie) = conn.randr_get_crtc_info(info.crtc, resources.config_timestamp)
        else {
            continue;
        };
        let Ok(crtc) = crtc_cookie.reply() else {
            continue;
        };
        if crtc.width == 0 || crtc.height == 0 {
            continue;
        }
        let name = String::from_utf8_lossy(&info.name).into_owned();
        outputs.push(OutputGeometry {
            name,
            x: i32::from(crtc.x),
            y: i32::from(crtc.y),
            width: u32::from(crtc.width),
            height: u32::from(crtc.height),
            scale_factor: 1.0,
        });
    }

    Ok(outputs)
}

fn fallback_output(width: u32, height: u32) -> OutputGeometry {
    OutputGeometry {
        name: "x11-0".to_string(),
        x: 0,
        y: 0,
        width,
        height,
        scale_factor: 1.0,
    }
}
