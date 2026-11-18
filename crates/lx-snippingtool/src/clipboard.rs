//! Post-copy clipboard holder for Wayland (`wl-clipboard-rs`) and X11/XWayland (`x11rb`).

use lx_snip_core::platform::{ClipboardBackend, PlatformError, PlatformFuture};
use tracing::{debug, info, warn};
use wl_clipboard_rs::copy::{MimeType, Options, Source};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as XprotoExt, CreateWindowAux, EventMask, PropMode,
    SelectionNotifyEvent, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

/// Wayland clipboard holder backed by `wl-clipboard-rs` (`wlr-data-control` / `ext-data-control-v1`).
#[derive(Debug, Default, Clone, Copy)]
pub struct WaylandClipboardBackend;

impl ClipboardBackend for WaylandClipboardBackend {
    fn copy_png_and_hold(&self, png_bytes: Vec<u8>) -> PlatformFuture<'_, ()> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || copy_wayland_blocking(png_bytes))
                .await
                .map_err(|e| {
                    PlatformError::ClipboardFailed(format!("Wayland clipboard task panicked: {e}"))
                })?
        })
    }
}

/// X11 / XWayland clipboard holder backed by `x11rb` selection ownership.
#[derive(Debug, Default, Clone, Copy)]
pub struct X11ClipboardBackend;

impl ClipboardBackend for X11ClipboardBackend {
    fn copy_png_and_hold(&self, png_bytes: Vec<u8>) -> PlatformFuture<'_, ()> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || copy_x11_blocking(png_bytes))
                .await
                .map_err(|e| {
                    PlatformError::ClipboardFailed(format!("X11 clipboard task panicked: {e}"))
                })?
        })
    }
}

/// Selects `wl-clipboard-rs` on `data-control`-capable Wayland compositors and falls back
/// to `x11rb` X11/XWayland selection ownership on GNOME Wayland and X11.
#[derive(Debug, Default, Clone, Copy)]
pub struct AutoClipboardBackend;

impl AutoClipboardBackend {
    /// Creates a new [`AutoClipboardBackend`].
    pub const fn new() -> Self {
        Self
    }
}

impl ClipboardBackend for AutoClipboardBackend {
    fn copy_png_and_hold(&self, png_bytes: Vec<u8>) -> PlatformFuture<'_, ()> {
        Box::pin(async move {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() && !is_gnome_session() {
                match WaylandClipboardBackend
                    .copy_png_and_hold(png_bytes.clone())
                    .await
                {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        warn!(error = %err, "wl-clipboard-rs failed; falling back to X11/XWayland clipboard");
                    }
                }
            }
            X11ClipboardBackend.copy_png_and_hold(png_bytes).await
        })
    }
}

fn is_gnome_session() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|d| d.to_ascii_uppercase().contains("GNOME"))
        .unwrap_or(false)
}

fn copy_wayland_blocking(png_bytes: Vec<u8>) -> Result<(), PlatformError> {
    let mut opts = Options::new();
    opts.foreground(true);
    opts.copy(
        Source::Bytes(png_bytes.into_boxed_slice()),
        MimeType::Specific("image/png".to_string()),
    )
    .map_err(|e| PlatformError::ClipboardFailed(format!("wl-clipboard-rs copy failed: {e}")))?;
    info!("Wayland clipboard ownership released");
    Ok(())
}

fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<u32, PlatformError> {
    conn.intern_atom(false, name)
        .map_err(|e| PlatformError::ClipboardFailed(format!("intern_atom failed: {e}")))?
        .reply()
        .map(|r| r.atom)
        .map_err(|e| PlatformError::ClipboardFailed(format!("intern_atom reply failed: {e}")))
}

fn copy_x11_blocking(png_bytes: Vec<u8>) -> Result<(), PlatformError> {
    let (conn, screen_num) = RustConnection::connect(None)
        .map_err(|e| PlatformError::ClipboardFailed(format!("X11 connect failed: {e}")))?;
    let screen = &conn.setup().roots[screen_num];

    let win = conn
        .generate_id()
        .map_err(|e| PlatformError::ClipboardFailed(format!("generate_id failed: {e}")))?;

    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        win,
        screen.root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| PlatformError::ClipboardFailed(format!("create_window failed: {e}")))?;

    let clipboard_atom = intern_atom(&conn, b"CLIPBOARD")?;
    let targets_atom = intern_atom(&conn, b"TARGETS")?;
    let image_png_atom = intern_atom(&conn, b"image/png")?;
    let atom_type: u32 = AtomEnum::ATOM.into();

    conn.set_selection_owner(win, clipboard_atom, x11rb::CURRENT_TIME)
        .map_err(|e| PlatformError::ClipboardFailed(format!("set_selection_owner failed: {e}")))?;
    conn.flush()
        .map_err(|e| PlatformError::ClipboardFailed(format!("X11 flush failed: {e}")))?;

    let owner = conn
        .get_selection_owner(clipboard_atom)
        .map_err(|e| PlatformError::ClipboardFailed(format!("get_selection_owner failed: {e}")))?
        .reply()
        .map_err(|e| {
            PlatformError::ClipboardFailed(format!("get_selection_owner reply failed: {e}"))
        })?;
    if owner.owner != win {
        return Err(PlatformError::ClipboardFailed(
            "failed to acquire X11 CLIPBOARD selection ownership".to_string(),
        ));
    }

    info!("X11/XWayland clipboard holder active for image/png");

    loop {
        let event = match conn.wait_for_event() {
            Ok(ev) => ev,
            Err(err) => {
                debug!(error = %err, "X11 clipboard event loop ended");
                break;
            }
        };

        match event {
            Event::SelectionClear(ev) if ev.selection == clipboard_atom => {
                info!("X11 CLIPBOARD ownership replaced; exiting holder");
                break;
            }
            Event::SelectionRequest(req) => {
                let mut prop_to_set = x11rb::NONE;

                if req.selection == clipboard_atom && req.property != x11rb::NONE {
                    if req.target == targets_atom {
                        let targets = [targets_atom, image_png_atom];
                        let bytes: Vec<u8> =
                            targets.iter().flat_map(|atom| atom.to_ne_bytes()).collect();
                        if conn
                            .change_property(
                                PropMode::REPLACE,
                                req.requestor,
                                req.property,
                                atom_type,
                                32,
                                targets.len() as u32,
                                &bytes,
                            )
                            .is_ok()
                        {
                            prop_to_set = req.property;
                        }
                    } else if req.target == image_png_atom
                        && conn
                            .change_property(
                                PropMode::REPLACE,
                                req.requestor,
                                req.property,
                                image_png_atom,
                                8,
                                png_bytes.len() as u32,
                                &png_bytes,
                            )
                            .is_ok()
                    {
                        prop_to_set = req.property;
                    }
                }

                let notify = SelectionNotifyEvent {
                    response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: req.time,
                    requestor: req.requestor,
                    selection: req.selection,
                    target: req.target,
                    property: prop_to_set,
                };
                let _ = conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
                let _ = conn.flush();
            }
            _ => {}
        }
    }

    let _ = conn.destroy_window(win);
    let _ = conn.flush();
    Ok(())
}
