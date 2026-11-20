//! Módulo: `capture_x11`
//! Objetivos: Capturar a área de trabalho e enumerar janelas e monitores no servidor X11 via `memfd_create`+XShm, GetImage, RandR e `_NET_CLIENT_LIST`.
//! Principais itens: `X11WindowBounds`, `X11CaptureBackend`, `list_x11_windows`, `query_x11_window_size`, `capture_via_shm` e `query_randr_outputs`.
//! Última alteração: 2026-11-20

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::io::{FromRawFd, OwnedFd};

use lx_snip_core::platform::{
    CaptureBackend, CapturedImage, OutputGeometry, PlatformError, PlatformFuture, WindowBounds,
};
use lx_snip_core::scene::Rect;
use tracing::{debug, warn};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::randr::{self, ConnectionExt as RandrExt};
use x11rb::protocol::shm::{self, ConnectionExt as ShmExt};
use x11rb::protocol::xproto::{self, AtomEnum, ConnectionExt as XprotoExt, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

/// Reexporta `WindowBounds` como alias compatível para janelas enumeradas no X11.
pub type X11WindowBounds = WindowBounds;

/// Implementa o backend de captura de tela X11 com XShm, RandR e lista de janelas.
/// Armazena: estrutura unitária sem campos | Principais métodos: `new`, `supports_window_mode`, `capture_desktop`, `capture_window`.
#[derive(Debug, Default, Clone, Copy)]
pub struct X11CaptureBackend;

impl X11CaptureBackend {
    /// Cria uma nova instância de `X11CaptureBackend`.
    /// Input: nenhum | Output: `X11CaptureBackend`.
    pub const fn new() -> Self {
        Self
    }
}

impl CaptureBackend for X11CaptureBackend {
    /// Informa que o X11 suporta escolha interativa de janela dentro do overlay congelado.
    /// Input: `&self` | Output: `true`.
    fn supports_window_mode(&self) -> bool {
        true
    }

    /// Captura a tela inteira X11 em uma thread bloqueante dedicada.
    /// Input: `&self` | Output: `PlatformFuture` resolvendo em `CapturedImage`.
    fn capture_desktop(&self) -> PlatformFuture<'_, CapturedImage> {
        Box::pin(async move {
            tokio::task::spawn_blocking(capture_x11_desktop_blocking)
                .await
                .map_err(|e| {
                    PlatformError::CaptureFailed(format!("X11 capture task panicked: {e}"))
                })?
        })
    }

    /// Captura a tela cheia X11 para permitir o recorte pela janela escolhida no overlay.
    /// Input: `&self` | Output: `PlatformFuture` resolvendo em `CapturedImage`.
    fn capture_window(&self) -> PlatformFuture<'_, CapturedImage> {
        // On X11, window selection happens inside the freeze-frame overlay using `_NET_CLIENT_LIST`;
        // capture_window returns the full desktop with outputs so the overlay can crop to the selected window.
        self.capture_desktop()
    }
}

/// Enumera janelas mapeadas e visíveis via `_NET_CLIENT_LIST` convertidas para coordenadas globais.
/// Input: nenhum | Output: `Result<Vec<X11WindowBounds>, PlatformError>`.
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
        windows.push(WindowBounds {
            id: u64::from(win),
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

/// Consulta ou cria o identificador numérico de um átomo X11.
/// Input: conexão `conn` e nome `name` | Output: `Result<u32, PlatformError>`.
fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<u32, PlatformError> {
    conn.intern_atom(false, name)
        .map_err(|e| PlatformError::CaptureFailed(format!("intern_atom failed: {e}")))?
        .reply()
        .map(|r| r.atom)
        .map_err(|e| PlatformError::CaptureFailed(format!("intern_atom reply failed: {e}")))
}

/// Captura toda a janela raiz X11 em modo bloqueante usando XShm com fallback para `GetImage`.
/// Input: nenhum | Output: `Result<CapturedImage, PlatformError>`.
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

/// Captura pixels BGRA da janela raiz utilizando memória compartilhada `MIT-SHM`.
/// Input: conexão `conn`, janela `root`, `width` e `height` | Output: `Result<Vec<u8>, PlatformError>`.
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
    let mut file = create_anonymous_shm_file(byte_len)?;

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

    file.seek(SeekFrom::Start(0))
        .map_err(|e| PlatformError::CaptureFailed(format!("failed to rewind shm buffer: {e}")))?;
    let mut buf = vec![0u8; byte_len];
    file.read_exact(&mut buf)
        .map_err(|e| PlatformError::CaptureFailed(format!("failed to read shm buffer: {e}")))?;

    let _ = shm::CompletionEvent::default();
    Ok(buf)
}

/// Cria buffer de memória anônimo via `memfd_create` com `MFD_CLOEXEC` e tamanho `byte_len`.
/// Input: tamanho `byte_len` em bytes | Output: `Result<File, PlatformError>` sem entrada no sistema de arquivos.
fn create_anonymous_shm_file(byte_len: usize) -> Result<File, PlatformError> {
    let raw_fd = unsafe { libc::memfd_create(c"lx-snip-shm".as_ptr(), libc::MFD_CLOEXEC) };
    if raw_fd < 0 {
        return Err(PlatformError::CaptureFailed(format!(
            "memfd_create failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    let owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let file = File::from(owned);
    file.set_len(byte_len as u64).map_err(|e| {
        PlatformError::CaptureFailed(format!("failed to size anonymous shm buffer: {e}"))
    })?;
    Ok(file)
}

/// Consulta os atributos e a geometria atual de uma janela X11 pelo seu `xid`.
/// Input: `xid` (`u32`) da janela | Output: `Result<(u32, u32), PlatformError>` com largura e altura ou erro se indisponível.
pub fn query_x11_window_size(xid: u32) -> Result<(u32, u32), PlatformError> {
    let (conn, _screen_num) = x11rb::connect(None).map_err(|e| {
        PlatformError::CaptureFailed(format!("failed to connect X11 for window query: {e}"))
    })?;
    let attrs = conn
        .get_window_attributes(xid)
        .map_err(|e| PlatformError::CaptureFailed(format!("get_window_attributes failed: {e}")))?
        .reply()
        .map_err(|e| PlatformError::CaptureFailed(format!("window 0x{xid:x} unavailable: {e}")))?;
    if attrs.map_state != xproto::MapState::VIEWABLE {
        return Err(PlatformError::CaptureFailed(format!(
            "X11 window 0x{xid:x} is no longer viewable"
        )));
    }
    let geom = conn
        .get_geometry(xid)
        .map_err(|e| PlatformError::CaptureFailed(format!("get_geometry failed: {e}")))?
        .reply()
        .map_err(|e| {
            PlatformError::CaptureFailed(format!("window 0x{xid:x} geometry error: {e}"))
        })?;
    if geom.width == 0 || geom.height == 0 {
        return Err(PlatformError::CaptureFailed(format!(
            "X11 window 0x{xid:x} has zero dimensions"
        )));
    }
    Ok((u32::from(geom.width), u32::from(geom.height)))
}

/// Realiza captura de fallback via chamada X11 padrão `GetImage` quando XShm falha.
/// Input: conexão `conn`, janela `root`, `width` e `height` | Output: `Result<Vec<u8>, PlatformError>`.
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

/// Converte pixels BGRx do X11 em RGBA8 opaco descartando lixo no canal alfa.
/// Input: slice `bgra` bruto | Output: `Vec<u8>` em RGBA8 reto.
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

/// Consulta monitores conectados e seus limites CRTC ativos usando a extensão X11 RandR.
/// Input: conexão `conn` e janela `root` | Output: `Result<Vec<OutputGeometry>, PlatformError>`.
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

/// Cria geometria virtual única cobrindo toda a tela quando RandR não retorna saídas válidas.
/// Input: `width` e `height` totais da tela | Output: instância única de `OutputGeometry`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn anonymous_shm_memfd_has_cloexec_and_exact_size_without_tmp_file() {
        let before_count = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("lx-snip-shm-"))
            .count();

        let mut file = create_anonymous_shm_file(4096).expect("memfd_create should succeed");
        assert_eq!(file.metadata().unwrap().len(), 4096);

        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "FD_CLOEXEC must be set");

        file.write_all(&[0xAA; 16]).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut head = [0u8; 16];
        file.read_exact(&mut head).unwrap();
        assert_eq!(head, [0xAA; 16]);

        let after_count = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("lx-snip-shm-"))
            .count();
        assert_eq!(before_count, after_count);
    }
}
