//! Módulo: `capture_windows`
//! Objetivos: Capturar a área de trabalho multimonitor no Windows 11 via `Windows.Graphics.Capture` (com fallback GDI), configurar consciência de DPI por monitor e enumerar janelas/monitores via Win32 + DWM.
//! Principais itens: `WindowsMonitorInfo`, `WindowsCaptureBackend`, `ensure_per_monitor_dpi_aware`, `list_windows`, `query_window_size`, `virtual_desktop_origin`, `create_d3d11_device_and_winrt`, `create_capture_item_for_monitor` e `create_capture_item_for_window`.
//! Última alteração: 2026-11-20

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::{mpsc, Once};
use std::time::Duration;

use lx_snip_core::platform::{
    CaptureBackend, CapturedImage, OutputGeometry, PlatformError, PlatformFuture, WindowBounds,
};
use lx_snip_core::scene::Rect;
use tracing::{debug, warn};
use windows::core::{ComInterface, IInspectable};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GetDC, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW, SRCCOPY,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetSystemMetrics, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE,
    MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, WS_EX_TOOLWINDOW,
};

static DPI_INIT: Once = Once::new();

/// Mantém a inicialização WinRT/COM MTA vinculada ao ciclo de vida do escopo na thread atual.
/// Armazena: indicador `should_uninit` | Principais métodos: `enter_mta` e `Drop::drop`.
pub(crate) struct WinRtScope {
    should_uninit: bool,
}

impl WinRtScope {
    /// Inicializa o runtime WinRT/COM em modo MTA (`RO_INIT_MULTITHREADED`) quando ainda não ativo na thread.
    /// Input: nenhum | Output: `Result<WinRtScope, PlatformError>`.
    pub(crate) fn enter_mta() -> Result<Self, PlatformError> {
        match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => Ok(Self {
                should_uninit: true,
            }),
            Err(err) if err.code() == RPC_E_CHANGED_MODE => Ok(Self {
                should_uninit: false,
            }),
            Err(err) => Err(PlatformError::CaptureFailed(format!(
                "RoInitialize(MTA) failed: {err}"
            ))),
        }
    }
}

impl Drop for WinRtScope {
    /// Desfaz a inicialização WinRT apenas se este escopo a iniciou com sucesso.
    /// Input: `&mut self` | Output: nenhum (`()`).
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe {
                RoUninitialize();
            }
        }
    }
}

/// Representa metadados físicos e de DPI de um monitor ativo no Windows.
/// Armazena: `hmonitor`, `name`, `x`, `y`, `width`, `height`, `scale_factor` e `primary` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone)]
pub(crate) struct WindowsMonitorInfo {
    /// Handle Win32 do monitor físico.
    pub(crate) hmonitor: HMONITOR,
    /// Nome de dispositivo GDI (ex.: `\\.\DISPLAY1`).
    pub(crate) name: String,
    /// Origem horizontal no espaço virtual desktop do Windows (pixels físicos).
    pub(crate) x: i32,
    /// Origem vertical no espaço virtual desktop do Windows (pixels físicos).
    pub(crate) y: i32,
    /// Largura do monitor em pixels físicos.
    pub(crate) width: u32,
    /// Altura do monitor em pixels físicos.
    pub(crate) height: u32,
    /// Escala DPI (`dpi / 96.0`).
    pub(crate) scale_factor: f64,
    /// Indica se este é o monitor principal do Windows.
    pub(crate) primary: bool,
}

/// Backend de captura de tela nativo para Windows 11 usando `Windows.Graphics.Capture` e enumeração Win32/DWM.
/// Armazena: estrutura unitária sem campos | Principais métodos: `new`, `supports_window_mode`, `capture_desktop`, `capture_window`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsCaptureBackend;

impl WindowsCaptureBackend {
    /// Cria uma nova instância de `WindowsCaptureBackend`.
    /// Input: nenhum | Output: `WindowsCaptureBackend`.
    pub const fn new() -> Self {
        Self
    }
}

impl CaptureBackend for WindowsCaptureBackend {
    /// Indica que o Windows suporta seleção direta de janela dentro do overlay congelado.
    /// Input: `&self` | Output: `true`.
    fn supports_window_mode(&self) -> bool {
        true
    }

    /// Captura toda a área de trabalho virtual cobrindo todos os monitores em thread bloqueante dedicada.
    /// Input: `&self` | Output: `PlatformFuture` resolvendo em `CapturedImage`.
    fn capture_desktop(&self) -> PlatformFuture<'_, CapturedImage> {
        Box::pin(async move {
            tokio::task::spawn_blocking(capture_windows_desktop_blocking)
                .await
                .map_err(|e| {
                    PlatformError::CaptureFailed(format!("Windows capture task panicked: {e}"))
                })?
        })
    }

    /// Captura a área de trabalho completa para recorte interativo de janela no overlay.
    /// Input: `&self` | Output: `PlatformFuture` resolvendo em `CapturedImage`.
    fn capture_window(&self) -> PlatformFuture<'_, CapturedImage> {
        self.capture_desktop()
    }
}

/// Configura o processo atual como Per-Monitor DPI Aware v2 de forma idempotente para obter pixels físicos reais.
/// Input: nenhum | Output: nenhum (`()`).
pub fn ensure_per_monitor_dpi_aware() {
    DPI_INIT.call_once(|| unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

/// Callback C chamado por `EnumDisplayMonitors` acumulando informações de cada monitor ativo.
/// Input: `hmonitor`, `_hdc`, `_rect` e ponteiro `lparam` para `Vec<WindowsMonitorInfo>` | Output: `BOOL(1)` para continuar enumeração.
unsafe extern "system" fn monitor_enum_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<WindowsMonitorInfo>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if !GetMonitorInfoW(hmonitor, (&mut info.monitorInfo) as *mut MONITORINFO).as_bool() {
        return BOOL(1);
    }

    let rc = info.monitorInfo.rcMonitor;
    let width = (rc.right - rc.left).max(0) as u32;
    let height = (rc.bottom - rc.top).max(0) as u32;
    if width == 0 || height == 0 {
        return BOOL(1);
    }

    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    let scale_factor = if GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y)
        .is_ok()
        && dpi_x > 0
    {
        f64::from(dpi_x) / 96.0
    } else {
        1.0
    };

    let nul_pos = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    let name = String::from_utf16_lossy(&info.szDevice[..nul_pos]);
    let primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

    list.push(WindowsMonitorInfo {
        hmonitor,
        name,
        x: rc.left,
        y: rc.top,
        width,
        height,
        scale_factor,
        primary,
    });
    BOOL(1)
}

/// Enumera todos os monitores conectados ordenando o monitor principal primeiro quando disponível.
/// Input: nenhum | Output: `Result<Vec<WindowsMonitorInfo>, PlatformError>`.
pub(crate) fn enumerate_monitors() -> Result<Vec<WindowsMonitorInfo>, PlatformError> {
    ensure_per_monitor_dpi_aware();
    let mut monitors: Vec<WindowsMonitorInfo> = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_enum_proc),
            LPARAM((&mut monitors as *mut Vec<WindowsMonitorInfo>) as isize),
        )
    };
    if !ok.as_bool() && monitors.is_empty() {
        return Err(PlatformError::CaptureFailed(
            "EnumDisplayMonitors failed with no monitors".to_string(),
        ));
    }
    monitors.sort_by_key(|m| (!m.primary, m.x, m.y));
    Ok(monitors)
}

/// Calcula a caixa envolvente `(min_x, min_y, largura, altura)` cobrindo todos os monitores ativos.
/// Input: `monitors` (`&[WindowsMonitorInfo]`) | Output: tupla `(vx, vy, vw, vh)`.
pub(crate) fn virtual_desktop_bounds_from_monitors(
    monitors: &[WindowsMonitorInfo],
) -> (i32, i32, u32, u32) {
    if monitors.is_empty() {
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1) as u32;
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1) as u32;
        return (vx, vy, vw, vh);
    }

    let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = monitors
        .iter()
        .map(|m| m.x + m.width as i32)
        .max()
        .unwrap_or(1);
    let max_y = monitors
        .iter()
        .map(|m| m.y + m.height as i32)
        .max()
        .unwrap_or(1);

    (
        min_x,
        min_y,
        (max_x - min_x).max(1) as u32,
        (max_y - min_y).max(1) as u32,
    )
}

/// Retorna a origem `(vx, vy)` do desktop virtual Win32 em pixels físicos reais.
/// Input: nenhum | Output: `(vx, vy)`.
pub fn virtual_desktop_origin() -> (i32, i32) {
    ensure_per_monitor_dpi_aware();
    if let Ok(monitors) = enumerate_monitors() {
        if !monitors.is_empty() {
            let (vx, vy, _, _) = virtual_desktop_bounds_from_monitors(&monitors);
            return (vx, vy);
        }
    }
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        )
    }
}

/// Consulta os limites reais DWM (`DWMWA_EXTENDED_FRAME_BOUNDS`) de uma janela `HWND` com fallback `GetWindowRect`.
/// Input: `hwnd` (`HWND`) | Output: `Option<RECT>` se dimensões forem válidas.
fn query_hwnd_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    let dwm_ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT) as *mut c_void,
            size_of::<RECT>() as u32,
        )
    }
    .is_ok();
    if !dwm_ok && unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return None;
    }
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return None;
    }
    Some(rect)
}

/// Estrutura auxiliar passada ao callback `EnumWindows`.
/// Armazena: `self_pid`, origem/dimensões do desktop virtual e vetor acumulador `windows` | Principais métodos: nenhum (estado interno).
struct WindowEnumState {
    self_pid: u32,
    vx: i32,
    vy: i32,
    vw: f32,
    vh: f32,
    windows: Vec<WindowBounds>,
}

/// Callback C chamado por `EnumWindows` (em ordem Z do topo para a base) coletando janelas reais visíveis.
/// Input: `hwnd` e ponteiro `lparam` para `WindowEnumState` | Output: `BOOL(1)` para prosseguir.
unsafe extern "system" fn window_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam.0 as *mut WindowEnumState);
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return BOOL(1);
    }

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == state.self_pid {
        return BOOL(1);
    }

    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        (&mut cloaked as *mut u32) as *mut c_void,
        size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0
    {
        return BOOL(1);
    }

    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if (ex_style & WS_EX_TOOLWINDOW.0) != 0 {
        return BOOL(1);
    }

    let mut class_buf = [0u16; 128];
    let len = GetClassNameW(hwnd, &mut class_buf);
    if len > 0 {
        let class_name = String::from_utf16_lossy(&class_buf[..len as usize]);
        if matches!(
            class_name.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return BOOL(1);
        }
    }

    let Some(rc) = query_hwnd_rect(hwnd) else {
        return BOOL(1);
    };
    let w = (rc.right - rc.left) as f32;
    let h = (rc.bottom - rc.top) as f32;
    if w < 10.0 || h < 10.0 {
        return BOOL(1);
    }

    let clamped = Rect::new(
        (rc.left - state.vx) as f32,
        (rc.top - state.vy) as f32,
        w,
        h,
    )
    .clamp(state.vw, state.vh);
    if clamped.width < 10.0 || clamped.height < 10.0 {
        return BOOL(1);
    }

    state.windows.push(WindowBounds {
        id: hwnd.0 as usize as u64,
        rect: clamped,
    });
    BOOL(1)
}

/// Enumera todas as janelas visíveis no Windows 11 com geometria DWM exata ordenada da base para o topo (`Z-order`).
/// Input: nenhum | Output: `Result<Vec<WindowBounds>, PlatformError>`.
pub fn list_windows() -> Result<Vec<WindowBounds>, PlatformError> {
    ensure_per_monitor_dpi_aware();
    let monitors = enumerate_monitors().unwrap_or_default();
    let (vx, vy, vw, vh) = virtual_desktop_bounds_from_monitors(&monitors);

    let mut state = WindowEnumState {
        self_pid: std::process::id(),
        vx,
        vy,
        vw: vw as f32,
        vh: vh as f32,
        windows: Vec::new(),
    };

    unsafe {
        let _ = EnumWindows(
            Some(window_enum_proc),
            LPARAM((&mut state as *mut WindowEnumState) as isize),
        );
    }

    // `EnumWindows` enumera do topo para a base; invertemos para que `.iter().rev()` no overlay encontre primeiro a janela mais acima.
    state.windows.reverse();
    Ok(state.windows)
}

/// Consulta as dimensões atuais em pixels físicos de uma janela Win32 pelo seu `HWND` codificado como `u64`.
/// Input: `handle` (`u64`) | Output: `Result<(u32, u32), PlatformError>`.
pub fn query_window_size(handle: u64) -> Result<(u32, u32), PlatformError> {
    ensure_per_monitor_dpi_aware();
    let hwnd = HWND(handle as usize as isize);
    if hwnd.0 == 0 || !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(PlatformError::CaptureFailed(format!(
            "A janela selecionada (0x{handle:x}) não está mais disponível."
        )));
    }
    let Some(rc) = query_hwnd_rect(hwnd) else {
        return Err(PlatformError::CaptureFailed(format!(
            "Falha ao obter dimensões da janela 0x{handle:x}."
        )));
    };
    let w = (rc.right - rc.left).max(0) as u32;
    let h = (rc.bottom - rc.top).max(0) as u32;
    if w == 0 || h == 0 {
        return Err(PlatformError::CaptureFailed(format!(
            "A janela selecionada (0x{handle:x}) possui dimensões nulas."
        )));
    }
    Ok((w, h))
}

/// Cria par de dispositivo/contexto `Direct3D 11` (hardware com fallback `WARP`) e o wrapper WinRT `IDirect3DDevice`.
/// Input: nenhum | Output: `Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice), PlatformError>`.
pub(crate) fn create_d3d11_device_and_winrt(
) -> Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice), PlatformError> {
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device = None;
        let mut context = None;
        let res = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        if res.is_ok() {
            if let (Some(d3d_dev), Some(d3d_ctx)) = (device, context) {
                let dxgi_dev: IDXGIDevice = d3d_dev
                    .cast()
                    .map_err(|e| PlatformError::CaptureFailed(format!("IDXGIDevice cast: {e}")))?;
                let inspectable: IInspectable =
                    unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_dev) }.map_err(|e| {
                        PlatformError::CaptureFailed(format!(
                            "CreateDirect3D11DeviceFromDXGIDevice failed: {e}"
                        ))
                    })?;
                let winrt_dev: IDirect3DDevice = inspectable.cast().map_err(|e| {
                    PlatformError::CaptureFailed(format!("IDirect3DDevice cast: {e}"))
                })?;
                return Ok((d3d_dev, d3d_ctx, winrt_dev));
            }
        }
    }

    Err(PlatformError::CaptureFailed(
        "Unable to initialize Direct3D 11 device for screen capture".to_string(),
    ))
}

/// Cria um `GraphicsCaptureItem` ligado a um monitor físico `HMONITOR` via interoperação COM.
/// Input: `hmonitor` (`HMONITOR`) | Output: `Result<GraphicsCaptureItem, PlatformError>`.
pub(crate) fn create_capture_item_for_monitor(
    hmonitor: HMONITOR,
) -> Result<GraphicsCaptureItem, PlatformError> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(
            |e| PlatformError::CaptureFailed(format!("IGraphicsCaptureItemInterop factory: {e}")),
        )?;
    unsafe { interop.CreateForMonitor(hmonitor) }
        .map_err(|e| PlatformError::CaptureFailed(format!("CreateForMonitor failed: {e}")))
}

/// Cria um `GraphicsCaptureItem` ligado a uma janela específica `HWND` via interoperação COM.
/// Input: `hwnd` (`HWND`) | Output: `Result<GraphicsCaptureItem, PlatformError>`.
pub(crate) fn create_capture_item_for_window(
    hwnd: HWND,
) -> Result<GraphicsCaptureItem, PlatformError> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(
            |e| PlatformError::CaptureFailed(format!("IGraphicsCaptureItemInterop factory: {e}")),
        )?;
    unsafe { interop.CreateForWindow(hwnd) }
        .map_err(|e| PlatformError::CaptureFailed(format!("CreateForWindow failed: {e}")))
}

/// Captura um quadro único de um `HMONITOR` via `Windows.Graphics.Capture` e o copia para o buffer RGBA8 do desktop.
/// Input: dispositivos D3D11/WinRT, monitor `mon`, origem `(vx, vy)`, dimensões `(vw, vh)` e destino `dst_rgba` | Output: `Result<(), PlatformError>`.
#[allow(clippy::too_many_arguments)]
fn capture_monitor_wgc_into_rgba(
    d3d_dev: &ID3D11Device,
    d3d_ctx: &ID3D11DeviceContext,
    winrt_dev: &IDirect3DDevice,
    mon: &WindowsMonitorInfo,
    vx: i32,
    vy: i32,
    vw: u32,
    vh: u32,
    dst_rgba: &mut [u8],
) -> Result<(), PlatformError> {
    let item = create_capture_item_for_monitor(mon.hmonitor)?;
    let size = item
        .Size()
        .map_err(|e| PlatformError::CaptureFailed(format!("GraphicsCaptureItem::Size: {e}")))?;
    if size.Width <= 0 || size.Height <= 0 {
        return Err(PlatformError::CaptureFailed(
            "GraphicsCaptureItem reported zero size".to_string(),
        ));
    }

    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        winrt_dev,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        size,
    )
    .map_err(|e| PlatformError::CaptureFailed(format!("CreateFreeThreaded failed: {e}")))?;

    let (tx, rx) = mpsc::sync_channel::<()>(1);
    let token = pool
        .FrameArrived(&TypedEventHandler::new(move |_pool, _| {
            let _ = tx.try_send(());
            Ok(())
        }))
        .map_err(|e| PlatformError::CaptureFailed(format!("FrameArrived subscribe failed: {e}")))?;

    let session = pool
        .CreateCaptureSession(&item)
        .map_err(|e| PlatformError::CaptureFailed(format!("CreateCaptureSession failed: {e}")))?;
    let _ = session.SetIsCursorCaptureEnabled(false);
    let _ = session.SetIsBorderRequired(false);

    let capture_res = (|| -> Result<(), PlatformError> {
        session
            .StartCapture()
            .map_err(|e| PlatformError::CaptureFailed(format!("StartCapture failed: {e}")))?;

        let frame = loop {
            if rx.recv_timeout(Duration::from_secs(3)).is_err() {
                return Err(PlatformError::CaptureFailed(
                    "Timed out waiting for WGC monitor frame".to_string(),
                ));
            }
            if let Ok(f) = pool.TryGetNextFrame() {
                break f;
            }
        };

        let surface = frame
            .Surface()
            .map_err(|e| PlatformError::CaptureFailed(format!("Frame::Surface: {e}")))?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| {
            PlatformError::CaptureFailed(format!("IDirect3DDxgiInterfaceAccess cast: {e}"))
        })?;
        let src_tex: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|e| PlatformError::CaptureFailed(format!("GetInterface texture2d: {e}")))?;

        let mut src_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            src_tex.GetDesc(&mut src_desc);
        }

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: src_desc.Width,
            Height: src_desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: src_desc.Format,
            SampleDesc: src_desc.SampleDesc,
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging_opt = None;
        unsafe {
            d3d_dev
                .CreateTexture2D(&staging_desc, None, Some(&mut staging_opt))
                .map_err(|e| {
                    PlatformError::CaptureFailed(format!("CreateTexture2D staging: {e}"))
                })?;
        }
        let staging_tex = staging_opt.ok_or_else(|| {
            PlatformError::CaptureFailed("CreateTexture2D returned None".to_string())
        })?;

        let src_res: ID3D11Resource = src_tex
            .cast()
            .map_err(|e| PlatformError::CaptureFailed(format!("ID3D11Resource src cast: {e}")))?;
        let dst_res: ID3D11Resource = staging_tex
            .cast()
            .map_err(|e| PlatformError::CaptureFailed(format!("ID3D11Resource dst cast: {e}")))?;

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            d3d_ctx.CopyResource(&dst_res, &src_res);
            d3d_ctx
                .Map(&dst_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| PlatformError::CaptureFailed(format!("ID3D11 Map: {e}")))?;
        }

        let dst_x0 = (mon.x - vx).max(0) as u32;
        let dst_y0 = (mon.y - vy).max(0) as u32;
        let copy_w = mon.width.min(src_desc.Width).min(vw.saturating_sub(dst_x0));
        let copy_h = mon
            .height
            .min(src_desc.Height)
            .min(vh.saturating_sub(dst_y0));

        let row_pitch = mapped.RowPitch as usize;
        let base_ptr = mapped.pData as *const u8;
        for row in 0..copy_h {
            let src_row = unsafe {
                std::slice::from_raw_parts(
                    base_ptr.add(row as usize * row_pitch),
                    (copy_w as usize) * 4,
                )
            };
            let dst_off = (((dst_y0 + row) as usize) * (vw as usize) + (dst_x0 as usize)) * 4;
            let dst_row = &mut dst_rgba[dst_off..dst_off + (copy_w as usize) * 4];
            for (s, d) in src_row
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst_row.as_chunks_mut::<4>().0)
            {
                d[0] = s[2]; // R
                d[1] = s[1]; // G
                d[2] = s[0]; // B
                d[3] = 255;
            }
        }

        unsafe {
            d3d_ctx.Unmap(&dst_res, 0);
        }
        let _ = frame.Close();
        Ok(())
    })();

    let _ = pool.RemoveFrameArrived(token);
    let _ = <GraphicsCaptureSession as Clone>::clone(&session).Close();
    let _ = pool.Close();
    capture_res
}

/// Captura um retângulo do desktop Win32 via GDI `BitBlt` como fallback caso `Windows.Graphics.Capture` falhe.
/// Input: retângulo `(src_x, src_y, cap_w, cap_h)`, destino `(dst_x0, dst_y0, vw, vh)` e `dst_rgba` | Output: `Result<(), PlatformError>`.
#[allow(clippy::too_many_arguments)]
fn capture_region_gdi_into_rgba(
    src_x: i32,
    src_y: i32,
    cap_w: u32,
    cap_h: u32,
    dst_x0: u32,
    dst_y0: u32,
    vw: u32,
    vh: u32,
    dst_rgba: &mut [u8],
) -> Result<(), PlatformError> {
    let copy_w = cap_w.min(vw.saturating_sub(dst_x0));
    let copy_h = cap_h.min(vh.saturating_sub(dst_y0));
    if copy_w == 0 || copy_h == 0 {
        return Ok(());
    }

    unsafe {
        let screen_dc = GetDC(HWND::default());
        if screen_dc.is_invalid() {
            return Err(PlatformError::CaptureFailed(
                "GetDC(screen) failed".to_string(),
            ));
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_invalid() {
            let _ = ReleaseDC(HWND::default(), screen_dc);
            return Err(PlatformError::CaptureFailed(
                "CreateCompatibleDC failed".to_string(),
            ));
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: copy_w as i32,
                biHeight: -(copy_h as i32), // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits_ptr: *mut c_void = std::ptr::null_mut();
        let hbitmap: HBITMAP =
            match CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) {
                Ok(hb) => hb,
                Err(e) => {
                    let _ = DeleteDC(mem_dc);
                    let _ = ReleaseDC(HWND::default(), screen_dc);
                    return Err(PlatformError::CaptureFailed(format!(
                        "CreateDIBSection failed: {e}"
                    )));
                }
            };

        let old_obj = SelectObject(mem_dc, hbitmap);
        let blt_res = BitBlt(
            mem_dc,
            0,
            0,
            copy_w as i32,
            copy_h as i32,
            screen_dc,
            src_x,
            src_y,
            SRCCOPY,
        );

        if blt_res.is_ok() && !bits_ptr.is_null() {
            let total = (copy_w as usize) * (copy_h as usize) * 4;
            let src_bytes = std::slice::from_raw_parts(bits_ptr as *const u8, total);
            let row_bytes = (copy_w as usize) * 4;
            for row in 0..copy_h {
                let s_off = (row as usize) * row_bytes;
                let d_off = (((dst_y0 + row) as usize) * (vw as usize) + (dst_x0 as usize)) * 4;
                for (s, d) in src_bytes[s_off..s_off + row_bytes]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst_rgba[d_off..d_off + row_bytes].as_chunks_mut::<4>().0)
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = 255;
                }
            }
        }

        let _ = SelectObject(mem_dc, old_obj);
        let _ = DeleteObject(hbitmap);
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);

        blt_res.map_err(|e| PlatformError::CaptureFailed(format!("BitBlt failed: {e}")))
    }
}

/// Captura todos os monitores no Windows de forma síncrona compondo um único buffer RGBA8 do desktop virtual.
/// Input: nenhum | Output: `Result<CapturedImage, PlatformError>`.
fn capture_windows_desktop_blocking() -> Result<CapturedImage, PlatformError> {
    ensure_per_monitor_dpi_aware();
    let _winrt = WinRtScope::enter_mta()?;

    let monitors = enumerate_monitors()?;
    let (vx, vy, vw, vh) = virtual_desktop_bounds_from_monitors(&monitors);
    let mut rgba = vec![0u8; (vw as usize) * (vh as usize) * 4];

    if monitors.is_empty() {
        capture_region_gdi_into_rgba(vx, vy, vw, vh, 0, 0, vw, vh, &mut rgba)?;
        return Ok(CapturedImage {
            rgba,
            width: vw,
            height: vh,
            outputs: vec![OutputGeometry {
                name: "Display 1".to_string(),
                x: 0,
                y: 0,
                width: vw,
                height: vh,
                scale_factor: 1.0,
            }],
        });
    }

    let d3d_tuple = create_d3d11_device_and_winrt();
    let mut outputs = Vec::with_capacity(monitors.len());

    for mon in &monitors {
        outputs.push(OutputGeometry {
            name: mon.name.clone(),
            x: mon.x - vx,
            y: mon.y - vy,
            width: mon.width,
            height: mon.height,
            scale_factor: mon.scale_factor,
        });

        let wgc_ok = match &d3d_tuple {
            Ok((d3d_dev, d3d_ctx, winrt_dev)) => {
                match capture_monitor_wgc_into_rgba(
                    d3d_dev, d3d_ctx, winrt_dev, mon, vx, vy, vw, vh, &mut rgba,
                ) {
                    Ok(()) => {
                        debug!(monitor = %mon.name, width = mon.width, height = mon.height, "captured monitor via WGC");
                        true
                    }
                    Err(err) => {
                        warn!(monitor = %mon.name, error = %err, "WGC monitor capture failed; falling back to GDI BitBlt");
                        false
                    }
                }
            }
            Err(err) => {
                warn!(error = %err, "D3D11 init failed; falling back to GDI BitBlt");
                false
            }
        };

        if !wgc_ok {
            let dst_x0 = (mon.x - vx).max(0) as u32;
            let dst_y0 = (mon.y - vy).max(0) as u32;
            capture_region_gdi_into_rgba(
                mon.x, mon.y, mon.width, mon.height, dst_x0, dst_y0, vw, vh, &mut rgba,
            )?;
        }
    }

    Ok(CapturedImage {
        rgba,
        width: vw,
        height: vh,
        outputs,
    })
}
