//! Módulo: `recorder`
//! Objetivos: Definir tipos e matemática compartilhados de gravação de tela (contenção estrita de região, padding opaco pós-recorte, gerenciamento de arquivos pendentes/locks e caminho padrão por timestamp) despachando entre o backend Linux (`GStreamer`/`PipeWire`/`X11`) e o backend nativo Windows 11 (`WGC` + `WASAPI` + `Media Foundation`).
//! Principais itens: `RecordingTarget`, `RegionCropPlan`, `VideoProfile`, `PreparedRecording`, `ActiveRecording`, `prepare_recording`, `compute_region_crop`, `cleanup_abandoned_recordings`, `probe_video_profile` e `save_recording_to_path`.
//! Última alteração: 2026-11-20

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use directories::{ProjectDirs, UserDirs};
use lx_snip_core::scene::Rect;
use tracing::debug;
use uuid::Uuid;

#[cfg(target_os = "linux")]
#[path = "recorder_linux.rs"]
mod backend;

#[cfg(target_os = "windows")]
#[path = "recorder_windows.rs"]
mod backend;

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use backend::{build_concat_args, build_segment_args};
#[allow(unused_imports)]
pub use backend::{prepare_recording, probe_video_profile, ActiveRecording, PreparedRecording};

/// Alvo de captura escolhido no Nível 2 do overlay para gravação de vídeo.
/// Armazena: variantes `Fullscreen`, `Region`, `NativeWindow` e `Window` | Principais métodos: nenhum (enumeração de seleção).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingTarget {
    /// Entire desktop / default monitor.
    Fullscreen,
    /// User-drawn rectangle clamped rigorously inside screen bounds.
    Region {
        /// Crop rectangle in full-desktop image pixels.
        rect: Rect,
        /// Full desktop width in pixels.
        full_width: u32,
        /// Full desktop height in pixels.
        full_height: u32,
    },
    /// Specific native window handle (`u32` X11 XID on Linux, `HWND` as `u64` on Windows).
    NativeWindow {
        /// Platform native window handle.
        handle: u64,
    },
    /// Wayland portal-selected single window.
    Window,
}

/// Plano exato de recorte e preenchimento opaco pós-recorte para gravação de região ou janela.
/// Armazena: margens (`top`, `bottom`, `left`, `right`), dimensões recortadas (`crop_w`, `crop_h`) e preenchimentos (`pad_right`, `pad_bottom`) | Principais métodos: nenhum (estrutura de dados).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionCropPlan {
    /// Pixels removidos do topo antes da codificação.
    pub top: u32,
    /// Pixels removidos de baixo antes da codificação.
    pub bottom: u32,
    /// Pixels removidos da esquerda antes da codificação.
    pub left: u32,
    /// Pixels removidos da direita antes da codificação.
    pub right: u32,
    /// Largura real útil após o recorte.
    pub crop_w: u32,
    /// Altura real útil após o recorte.
    pub crop_h: u32,
    /// Preenchimento opaco à direita adicionado após o recorte quando necessário pelo codec.
    pub pad_right: u32,
    /// Preenchimento opaco inferior adicionado após o recorte quando necessário pelo codec.
    pub pad_bottom: u32,
}

/// Perfil de codificação de vídeo e contêiner suportado na plataforma ativa.
/// Armazena: variantes `Mp4H264`, `WebmVp8` e `WebmVp9` | Principais métodos: `ext`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoProfile {
    /// H.264 + AAC/Opus in MP4 (`mp4mux` on Linux / `IMFSinkWriter` on Windows).
    Mp4H264,
    /// Royalty-free VP8 (`vp8enc`) + Opus in WebM (`webmmux`).
    WebmVp8,
    /// Royalty-free VP9 (`vp9enc`) + Opus in WebM (`webmmux`).
    WebmVp9,
}

impl VideoProfile {
    /// Retorna a extensão de arquivo correspondente ao perfil de contêiner selecionado.
    /// Input: `self` | Output: `"mp4"` ou `"webm"`.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4H264 => "mp4",
            Self::WebmVp8 | Self::WebmVp9 => "webm",
        }
    }
}

/// Bloqueio exclusivo da sessão ativa no cache de gravações pendentes (`flock` em Unix ou lock de arquivo no Windows).
/// Armazena: descritor `_file` e `path` do arquivo `.lock` | Principais métodos: `acquire`, `lock_path_for`, `release_and_remove`.
pub(crate) struct SessionLockFile {
    _file: File,
    path: PathBuf,
}

impl SessionLockFile {
    /// Resolve o caminho `.{stem}.lock` associado ao arquivo final da gravação.
    /// Input: `final_path` (`&Path`) | Output: `PathBuf` oculto de lock.
    pub(crate) fn lock_path_for(final_path: &Path) -> PathBuf {
        let stem = final_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("rec");
        let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
        parent.join(format!(".{stem}.lock"))
    }

    /// Cria e adquire um bloqueio exclusivo não bloqueante para proteger os artefatos desta sessão.
    /// Input: `final_path` (`&Path`) | Output: `Result<Self, String>`.
    pub(crate) fn acquire(final_path: &Path) -> Result<Self, String> {
        let lock_path = Self::lock_path_for(final_path);
        let mut opts = OpenOptions::new();
        opts.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            opts.mode(0o600);
        }
        let file = opts
            .open(&lock_path)
            .map_err(|e| format!("failed to open recording session lock: {e}"))?;

        #[cfg(unix)]
        {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                return Err(format!(
                    "failed to lock recording session: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        #[cfg(not(unix))]
        {
            file.try_lock()
                .map_err(|e| format!("failed to lock recording session: {e}"))?;
        }

        Ok(Self {
            _file: file,
            path: lock_path,
        })
    }

    /// Libera explicitamente e remove o arquivo de lock após finalização ou descarte seguro.
    /// Input: `self` | Output: nenhum (`()`).
    pub(crate) fn release_and_remove(self) {
        let path = self.path.clone();
        drop(self);
        let _ = fs::remove_file(path);
    }
}

/// Resolve o caminho padrão de destino para salvar uma gravação com timestamp local legível.
/// Input: `ext` (`&str`) | Output: `PathBuf` em `Vídeos/SnippingTool/gravacao_YYYY-MM-DD_HH-MM-SS.<ext>`.
pub fn default_recording_save_path(ext: &str) -> PathBuf {
    let dir = UserDirs::new()
        .and_then(|u| u.video_dir().map(|d| d.join("SnippingTool")))
        .or_else(|| {
            ProjectDirs::from("", "", "lx-snippingtool").map(|p| p.data_dir().join("recordings"))
        })
        .unwrap_or_else(std::env::temp_dir);

    #[cfg(unix)]
    let file_name = {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `localtime_r` writes into stack-allocated `tm` using valid `now`.
        let local_ptr = unsafe { libc::localtime_r(&now, &mut tm) };
        if !local_ptr.is_null() {
            format!(
                "gravacao_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.{ext}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            )
        } else {
            format!("gravacao_{now}.{ext}")
        }
    };

    #[cfg(target_os = "windows")]
    let file_name = {
        let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!(
            "gravacao_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.{ext}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    };

    #[cfg(all(not(unix), not(target_os = "windows")))]
    let file_name = {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("gravacao_{secs}.{ext}")
    };

    dir.join(file_name)
}

/// Move ou copia entre sistemas de arquivos o vídeo temporário para o destino escolhido e remove o temporário.
/// Input: `src` (`&Path`), `dest` (`&Path`) | Output: `Result<(), String>`.
pub fn save_recording_to_path(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create save directory: {e}"))?;
    }
    if fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    fs::copy(src, dest).map_err(|e| format!("failed to copy recording: {e}"))?;
    let _ = fs::remove_file(src);
    Ok(())
}

/// Resolve o diretório de cache seguro usado para gravações pendentes e segmentos.
/// Input: nenhum | Output: `PathBuf` do diretório `pending`.
fn pending_recordings_dir() -> PathBuf {
    ProjectDirs::from("", "", "lx-snippingtool")
        .map(|p| p.cache_dir().join("pending"))
        .unwrap_or_else(|| std::env::temp_dir().join("lx-snippingtool"))
}

/// Remove arquivos e segmentos abandonados de sessões anteriores em `pending/`, preservando sessões com lock ativo.
/// Input: nenhum | Output: quantidade de arquivos removidos (`usize`).
pub fn cleanup_abandoned_recordings() -> usize {
    let dir = pending_recordings_dir();
    cleanup_abandoned_recordings_in(&dir)
}

/// Extrai o identificador base (`pending_<ms>_<uuid>`) de um nome de arquivo no diretório de gravações pendentes.
/// Input: `name` (`&str`) | Output: `Option<String>` com o radical da sessão.
fn extract_pending_stem(name: &str) -> Option<String> {
    if let Some(inner) = name.strip_prefix('.') {
        if let Some(idx) = inner.find(".part") {
            let stem = &inner[..idx];
            if stem.starts_with("pending_") {
                return Some(stem.to_string());
            }
        }
        if let Some(stem) = inner.strip_suffix(".lock") {
            if stem.starts_with("pending_") {
                return Some(stem.to_string());
            }
        }
        return None;
    }
    if name.starts_with("pending_") {
        let stem = Path::new(name).file_stem()?.to_str()?;
        return Some(stem.to_string());
    }
    None
}

/// Verifica se existe um arquivo `.lock` ativo (mantido sob lock exclusivo por outro processo) para o radical `stem`.
/// Input: `dir` (`&Path`), `stem` (`&str`) | Output: `bool` verdadeiro se a sessão ainda estiver ativa.
fn is_session_lock_active(dir: &Path, stem: &str) -> bool {
    let lock_path = dir.join(format!(".{stem}.lock"));
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&lock_path) else {
        return false;
    };

    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            // Conseguimos o lock: nenhum processo dono está mais vivo.
            let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            false
        } else {
            let err = std::io::Error::last_os_error();
            matches!(err.raw_os_error(), Some(libc::EWOULDBLOCK))
        }
    }

    #[cfg(not(unix))]
    {
        match file.try_lock() {
            Ok(()) => false,
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(_) => false,
        }
    }
}

/// Implementação testável da limpeza de gravações/segmentos órfãos em `dir` respeitando locks ativos e gravações recém-concluídas.
/// Input: `dir` (`&Path`) | Output: quantidade de arquivos removidos (`usize`).
pub fn cleanup_abandoned_recordings_in(dir: &Path) -> usize {
    const MAX_FINISHED_AGE: Duration = Duration::from_secs(24 * 3600);

    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut hidden_files: Vec<(String, PathBuf)> = Vec::new();
    let mut public_files: Vec<(String, PathBuf)> = Vec::new();
    let mut stems = HashSet::new();
    let mut stems_with_session_artifacts = HashSet::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = extract_pending_stem(name) else {
            continue;
        };
        stems.insert(stem.clone());
        if name.starts_with('.') {
            stems_with_session_artifacts.insert(stem.clone());
            hidden_files.push((stem, path));
        } else {
            public_files.push((stem, path));
        }
    }

    let active_stems: HashSet<String> = stems
        .into_iter()
        .filter(|stem| is_session_lock_active(dir, stem))
        .collect();

    let mut removed = 0usize;
    for (stem, path) in hidden_files {
        if active_stems.contains(&stem) {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    for (stem, path) in public_files {
        if active_stems.contains(&stem) {
            continue;
        }
        let is_old = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
            .is_some_and(|age| age > MAX_FINISHED_AGE);
        if (stems_with_session_artifacts.contains(&stem) || is_old)
            && fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    if removed > 0 {
        debug!(removed, dir = %dir.display(), "cleaned up abandoned recording files");
    }
    removed
}

/// Gera um caminho temporário exclusivo no diretório de cache seguro (`0o700` em Unix) e adquire o lock da sessão.
/// Input: `ext` (`&str`) | Output: `Result<(PathBuf, SessionLockFile), String>`.
pub(crate) fn next_temp_recording_path(ext: &str) -> Result<(PathBuf, SessionLockFile), String> {
    let dir = pending_recordings_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let final_path = dir.join(format!("pending_{ms}_{}.{ext}", Uuid::new_v4()));
    let lock = SessionLockFile::acquire(&final_path)?;
    Ok((final_path, lock))
}

/// Constrói o caminho de um arquivo de segmento parcial associado à gravação final.
/// Input: `final_path` (`&Path`), `index` (`usize`), `ext` (`&str`) | Output: `PathBuf` oculto `.part<index>`.
#[cfg(target_os = "linux")]
pub(crate) fn segment_path(final_path: &Path, index: usize, ext: &str) -> PathBuf {
    let stem = final_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rec");
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{stem}.part{index}.{ext}"))
}

/// Calcula limites internos estritos (`ceil`/`floor`) para recorte RGB sem jamais expandir para fora da seleção ou da tela.
/// Input: `rect` (`Rect`), `full_w` (`u32`), `full_h` (`u32`) | Output: `Result<RegionCropPlan, String>`.
pub fn compute_region_crop(rect: Rect, full_w: u32, full_h: u32) -> Result<RegionCropPlan, String> {
    if full_w == 0 || full_h == 0 {
        return Err("Dimensões da tela de origem são inválidas para gravação.".to_string());
    }
    let r = rect.normalized();
    if !r.x.is_finite()
        || !r.y.is_finite()
        || !r.width.is_finite()
        || !r.height.is_finite()
        || r.width <= 0.0
        || r.height <= 0.0
    {
        return Err("Região selecionada para gravação é vazia ou inválida.".to_string());
    }

    let x0 = (r.x.max(0.0).ceil() as u32).min(full_w);
    let y0 = (r.y.max(0.0).ceil() as u32).min(full_h);
    let x1 = ((r.x + r.width).min(full_w as f32).floor() as u32).min(full_w);
    let y1 = ((r.y + r.height).min(full_h as f32).floor() as u32).min(full_h);

    if x0 >= x1 || y0 >= y1 {
        return Err(
            "Região selecionada para gravação é vazia após alinhamento rigoroso à área interna."
                .to_string(),
        );
    }

    let crop_w = x1 - x0;
    let crop_h = y1 - y0;
    let (pad_right, pad_bottom) = compute_even_min_padding(crop_w, crop_h);

    Ok(RegionCropPlan {
        top: y0,
        bottom: full_h.saturating_sub(y1),
        left: x0,
        right: full_w.saturating_sub(x1),
        crop_w,
        crop_h,
        pad_right,
        pad_bottom,
    })
}

/// Calcula o preenchimento mínimo `(pad_right, pad_bottom)` para atingir dimensões pares `>= 16` quando exigido pelo encoder.
/// Input: `w` (`u32`) e `h` (`u32`) | Output: `(pad_right, pad_bottom)`.
pub(crate) fn compute_even_min_padding(w: u32, h: u32) -> (u32, u32) {
    let target_w = if w < 16 {
        16
    } else if !w.is_multiple_of(2) {
        w + 1
    } else {
        w
    };
    let target_h = if h < 16 {
        16
    } else if !h.is_multiple_of(2) {
        h + 1
    } else {
        h
    };
    (target_w.saturating_sub(w), target_h.saturating_sub(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_crop_inward_bounds_no_fullscreen_expansion_and_even_padding() {
        // Odd narrow strip inside a 1920x1080 screen
        let plan = compute_region_crop(Rect::new(100.3, 50.2, 5.4, 33.6), 1920, 1080)
            .expect("valid narrow region");
        // ceil(100.3)=101, floor(105.7)=105 -> crop_w = 4; ceil(50.2)=51, floor(83.8)=83 -> crop_h = 32
        assert_eq!(plan.left, 101);
        assert_eq!(plan.top, 51);
        assert_eq!(plan.crop_w, 4);
        assert_eq!(plan.crop_h, 32);
        assert_eq!(plan.right, 1920 - 105);
        assert_eq!(plan.bottom, 1080 - 83);
        // Must pad narrow width (< 16) via padding rather than expanding crop to 1920!
        assert_eq!(plan.pad_right, 12);
        assert_eq!(plan.pad_bottom, 0);

        // Edge strip touching right/bottom screen border stays strictly clamped without expanding left/top
        let edge_plan =
            compute_region_crop(Rect::new(1915.0, 1070.0, 20.0, 20.0), 1920, 1080).unwrap();
        assert_eq!(edge_plan.left, 1915);
        assert_eq!(edge_plan.right, 0);
        assert_eq!(edge_plan.crop_w, 5);
        assert_eq!(edge_plan.pad_right, 11);

        // Empty / subpixel-empty after inward rounding must return Err, never Fullscreen!
        assert!(compute_region_crop(Rect::new(10.6, 10.6, 0.2, 10.0), 1920, 1080).is_err());
        assert!(compute_region_crop(Rect::new(0.0, 0.0, 0.0, 0.0), 1920, 1080).is_err());
    }

    #[test]
    fn cleanup_abandoned_recordings_removes_stale_and_preserves_locked_active_sessions() {
        let dir = std::env::temp_dir().join(format!("lx_snip_cleanup_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        // Active locked session (should be preserved completely)
        let active_final = dir.join("pending_1000_active.mp4");
        let _active_lock = SessionLockFile::acquire(&active_final).unwrap();
        let active_part = dir.join(".pending_1000_active.part0.mp4");
        fs::write(&active_part, b"active-data").unwrap();

        // Fresh finished recording awaiting user Save dialog in another live instance (no .part/.lock, recent mtime -> preserved)
        let finished_final = dir.join("pending_1001_finished.mp4");
        fs::write(&finished_final, b"finished-video").unwrap();

        // Abandoned unlocked session (should be removed)
        let stale_final = dir.join("pending_900_stale.mp4");
        let stale_part = dir.join(".pending_900_stale.part0.mp4");
        let stale_lock = SessionLockFile::lock_path_for(&stale_final);
        fs::write(&stale_part, b"stale-data").unwrap();
        fs::write(&stale_lock, b"").unwrap();
        fs::write(&stale_final, b"orphan").unwrap();

        let removed = cleanup_abandoned_recordings_in(&dir);
        assert_eq!(removed, 3);
        assert!(active_part.exists(), "active session segment must stay");
        assert!(
            SessionLockFile::lock_path_for(&active_final).exists(),
            "active session lock must stay"
        );
        assert!(
            finished_final.exists(),
            "fresh finished pending recording without lock/part must be preserved"
        );
        assert!(!stale_part.exists());
        assert!(!stale_lock.exists());
        assert!(!stale_final.exists());

        drop(_active_lock);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_save_path_uses_readable_timestamp() {
        let path = default_recording_save_path("mp4");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("mp4"));
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assert!(name.starts_with("gravacao_"));
    }
}
