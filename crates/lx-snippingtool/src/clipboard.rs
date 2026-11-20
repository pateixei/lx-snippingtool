//! Módulo: `clipboard`
//! Objetivos: Manter a posse da imagem PNG na área de transferência no Linux (`wl-clipboard-rs` no Wayland, `x11rb` no X11/XWayland) e publicar `PNG` + `CF_DIB` nativamente no Windows 11.
//! Principais itens: `WaylandClipboardBackend`, `X11ClipboardBackend`, `AutoClipboardBackend`, `copy_wayland_blocking` e `copy_x11_blocking`.
//! Última alteração: 2026-11-20

use lx_snip_core::platform::{ClipboardBackend, PlatformError, PlatformFuture};
#[cfg(target_os = "linux")]
use tracing::{debug, info, warn};

#[cfg(target_os = "linux")]
mod linux_clipboard {
    use super::*;
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    use x11rb::connection::Connection as X11Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as XprotoExt, CreateWindowAux, EventMask, PropMode,
        SelectionNotifyEvent, WindowClass,
    };
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    /// Implementa o backend de clipboard para compositores Wayland com suporte a `data-control`.
    /// Armazena: estrutura unitária sem campos | Principais métodos: `copy_png_and_hold`.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct WaylandClipboardBackend;

    impl ClipboardBackend for WaylandClipboardBackend {
        /// Publica `png_bytes` via `wl-clipboard-rs` em thread bloqueante até a posse mudar.
        /// Input: `&self` e `png_bytes` | Output: `PlatformFuture` indicando conclusão ou erro.
        fn copy_png_and_hold(&self, png_bytes: Vec<u8>) -> PlatformFuture<'_, ()> {
            Box::pin(async move {
                tokio::task::spawn_blocking(move || copy_wayland_blocking(png_bytes))
                    .await
                    .map_err(|e| {
                        PlatformError::ClipboardFailed(format!(
                            "Wayland clipboard task panicked: {e}"
                        ))
                    })?
            })
        }
    }

    /// Implementa o backend de clipboard X11/XWayland segurando a seleção `CLIPBOARD` via `x11rb`.
    /// Armazena: estrutura unitária sem campos | Principais métodos: `copy_png_and_hold`.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct X11ClipboardBackend;

    impl ClipboardBackend for X11ClipboardBackend {
        /// Publica `png_bytes` via servidor X11/XWayland em thread bloqueante até perder a seleção.
        /// Input: `&self` e `png_bytes` | Output: `PlatformFuture` indicando conclusão ou erro.
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

    /// Escolhe automaticamente entre `WaylandClipboardBackend` e `X11ClipboardBackend` com fallback em GNOME.
    /// Armazena: estrutura unitária sem campos | Principais métodos: `new`, `copy_png_and_hold`.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct AutoClipboardBackend;

    impl AutoClipboardBackend {
        /// Cria uma nova instância de `AutoClipboardBackend`.
        /// Input: nenhum | Output: `AutoClipboardBackend`.
        pub const fn new() -> Self {
            Self
        }
    }

    impl ClipboardBackend for AutoClipboardBackend {
        /// Publica `png_bytes` usando `wl-clipboard-rs` fora do GNOME ou fallback direto no X11/XWayland.
        /// Input: `&self` e `png_bytes` | Output: `PlatformFuture` indicando sucesso ou erro.
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

    /// Verifica pelas variáveis de ambiente se a sessão atual é GNOME (onde falta `wlr-data-control`).
    /// Input: nenhum | Output: `true` se `XDG_CURRENT_DESKTOP` contém `GNOME`.
    fn is_gnome_session() -> bool {
        std::env::var("XDG_CURRENT_DESKTOP")
            .map(|d| d.to_ascii_uppercase().contains("GNOME"))
            .unwrap_or(false)
    }

    /// Serve `image/png` em primeiro plano via `wl-clipboard-rs` até outro aplicativo assumir o clipboard.
    /// Input: vetor `png_bytes` | Output: `Result<(), PlatformError>`.
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

    /// Resolve ou registra o identificador numérico de um átomo X11 pelo nome.
    /// Input: conexão `conn` e nome em bytes `name` | Output: `Result<u32, PlatformError>` com o atom.
    fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<u32, PlatformError> {
        conn.intern_atom(false, name)
            .map_err(|e| PlatformError::ClipboardFailed(format!("intern_atom failed: {e}")))?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| PlatformError::ClipboardFailed(format!("intern_atom reply failed: {e}")))
    }

    /// Cria uma janela X11 oculta, adquire `CLIPBOARD` e atende `SelectionRequest` até receber `SelectionClear`.
    /// Input: vetor `png_bytes` | Output: `Result<(), PlatformError>`.
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
            .map_err(|e| {
                PlatformError::ClipboardFailed(format!("set_selection_owner failed: {e}"))
            })?;
        conn.flush()
            .map_err(|e| PlatformError::ClipboardFailed(format!("X11 flush failed: {e}")))?;

        let owner = conn
            .get_selection_owner(clipboard_atom)
            .map_err(|e| {
                PlatformError::ClipboardFailed(format!("get_selection_owner failed: {e}"))
            })?
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
}

#[cfg(target_os = "windows")]
mod windows_clipboard {
    use std::mem::size_of;
    use std::time::Duration;

    use super::*;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_DIB;

    /// Backend de área de transferência nativo para Windows 11 publicando PNG e `CF_DIB`.
    /// Armazena: estrutura unitária sem campos | Principais métodos: `new`, `copy_png_and_hold`.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct AutoClipboardBackend;

    impl AutoClipboardBackend {
        /// Cria uma nova instância de `AutoClipboardBackend` para Windows.
        /// Input: nenhum | Output: `AutoClipboardBackend`.
        pub const fn new() -> Self {
            Self
        }
    }

    impl ClipboardBackend for AutoClipboardBackend {
        /// Copia os bytes PNG (formato `"PNG"` + fallback `CF_DIB`) para o clipboard do Windows.
        /// Input: `&self` e `png_bytes` | Output: `PlatformFuture` indicando sucesso ou erro.
        fn copy_png_and_hold(&self, png_bytes: Vec<u8>) -> PlatformFuture<'_, ()> {
            Box::pin(async move {
                tokio::task::spawn_blocking(move || copy_windows_blocking(&png_bytes))
                    .await
                    .map_err(|e| {
                        PlatformError::ClipboardFailed(format!(
                            "Windows clipboard task panicked: {e}"
                        ))
                    })?
            })
        }
    }

    /// Aloca um bloco `GMEM_MOVEABLE`, copia `data` e chama `SetClipboardData` transferindo posse ao Windows.
    /// Input: identificador de formato `fmt` (`u32`) e slice `data` | Output: `Result<(), PlatformError>`.
    unsafe fn set_clipboard_hglobal(fmt: u32, data: &[u8]) -> Result<(), PlatformError> {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, data.len())
            .map_err(|e| PlatformError::ClipboardFailed(format!("GlobalAlloc failed: {e}")))?;
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            return Err(PlatformError::ClipboardFailed(
                "GlobalLock returned null".to_string(),
            ));
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        let _ = GlobalUnlock(hmem);
        SetClipboardData(fmt, HANDLE(hmem.0 as isize))
            .map_err(|e| PlatformError::ClipboardFailed(format!("SetClipboardData failed: {e}")))?;
        Ok(())
    }

    /// Abre o clipboard Win32 com breves tentativas, publica `"PNG"` e `CF_DIB` bottom-up e fecha o clipboard.
    /// Input: slice `png_bytes` | Output: `Result<(), PlatformError>`.
    fn copy_windows_blocking(png_bytes: &[u8]) -> Result<(), PlatformError> {
        let dib_bytes = build_cf_dib_from_png(png_bytes)?;

        let mut opened = false;
        for _ in 0..5 {
            if unsafe { OpenClipboard(HWND::default()) }.is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !opened {
            return Err(PlatformError::ClipboardFailed(
                "OpenClipboard failed after retries".to_string(),
            ));
        }

        let res = (|| -> Result<(), PlatformError> {
            unsafe {
                EmptyClipboard().map_err(|e| {
                    PlatformError::ClipboardFailed(format!("EmptyClipboard failed: {e}"))
                })?;
                let png_fmt = RegisterClipboardFormatW(&HSTRING::from("PNG"));
                if png_fmt != 0 {
                    set_clipboard_hglobal(png_fmt, png_bytes)?;
                }
                set_clipboard_hglobal(u32::from(CF_DIB.0), &dib_bytes)?;
            }
            Ok(())
        })();

        unsafe {
            let _ = CloseClipboard();
        }
        res
    }

    /// Decodifica um PNG e monta o buffer `CF_DIB` (`BITMAPINFOHEADER` + pixels BGRx de baixo para cima).
    /// Input: slice `png_bytes` | Output: `Result<Vec<u8>, PlatformError>`.
    fn build_cf_dib_from_png(png_bytes: &[u8]) -> Result<Vec<u8>, PlatformError> {
        let img = image::load_from_memory(png_bytes)
            .map_err(|e| PlatformError::ClipboardFailed(format!("PNG decode failed: {e}")))?
            .to_rgba8();
        let (w, h) = img.dimensions();
        let raw = img.into_raw();

        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: h as i32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: w * h * 4,
            ..Default::default()
        };

        let header_size = size_of::<BITMAPINFOHEADER>();
        let mut out = Vec::with_capacity(header_size + (w as usize) * (h as usize) * 4);
        let header_bytes = unsafe {
            std::slice::from_raw_parts(
                (&header as *const BITMAPINFOHEADER) as *const u8,
                header_size,
            )
        };
        out.extend_from_slice(header_bytes);

        let stride = (w as usize) * 4;
        for y in (0..h as usize).rev() {
            let row = &raw[y * stride..(y + 1) * stride];
            for px in row.as_chunks::<4>().0 {
                let a = u16::from(px[3]);
                let inv = 255 - a;
                let r = ((u16::from(px[0]) * a + 255 * inv + 127) / 255) as u8;
                let g = ((u16::from(px[1]) * a + 255 * inv + 127) / 255) as u8;
                let b = ((u16::from(px[2]) * a + 255 * inv + 127) / 255) as u8;
                out.extend_from_slice(&[b, g, r, 255]);
            }
        }
        Ok(out)
    }
}

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use linux_clipboard::{AutoClipboardBackend, WaylandClipboardBackend, X11ClipboardBackend};

#[cfg(target_os = "windows")]
pub use windows_clipboard::AutoClipboardBackend;
