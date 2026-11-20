//! Módulo: `recorder_windows`
//! Objetivos: Implementar gravação de tela 100% nativa no Windows 11 combinando `Windows.Graphics.Capture` + Direct3D11, áudio WASAPI loopback/microfone e codificação H.264/AAC MP4 via Media Foundation (`IMFSinkWriter`) sem dependências externas.
//! Principais itens: `PreparedRecording`, `ActiveRecording`, `prepare_recording` e `probe_video_profile`.
//! Última alteração: 2026-11-20

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{info, warn};
use windows::core::{ComInterface, IUnknown, HSTRING};
use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::MediaFoundation::{
    eAVEncH264VProfile_Main, IMFAttributes, IMFByteStream, IMFMediaType, IMFSinkWriter,
    MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32,
    MFVideoInterlace_Progressive, MFSTARTUP_FULL, MF_MT_AAC_PAYLOAD_TYPE,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SINK_WRITER_DISABLE_THROTTLING, MF_VERSION,
};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;

use crate::capture_windows::{
    create_capture_item_for_monitor, create_capture_item_for_window, create_d3d11_device_and_winrt,
    ensure_per_monitor_dpi_aware, enumerate_monitors, query_window_size,
    virtual_desktop_bounds_from_monitors, WinRtScope, WindowsMonitorInfo,
};
use crate::recorder::{
    compute_even_min_padding, compute_region_crop, next_temp_recording_path, RecordingTarget,
    SessionLockFile, VideoProfile,
};

const TARGET_FPS: u32 = 30;
const FRAME_DURATION_HNS: i64 = 10_000_000 / (TARGET_FPS as i64);
const AUDIO_SAMPLE_RATE: u32 = 48_000;
const AUDIO_CHANNELS: u16 = 2;
const AUDIO_BITS_PER_SAMPLE: u16 = 16;
const AUDIO_FRAMES_PER_CHUNK: usize = 960; // 20 ms em 48 kHz
const AUDIO_DURATION_HNS: i64 = 200_000;

/// Empacota dois inteiros `u32` (largura/altura ou numerador/denominador) no formato `u64` de atributos Media Foundation.
/// Input: `hi` (`u32`) e `lo` (`u32`) | Output: valor `u64` combinado.
#[inline]
const fn pack_u32_pair(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | (lo as u64)
}

/// Descreve a região física de origem dentro do desktop ou janela a ser gravada no Windows.
/// Armazena: variantes `Monitor`, `Window` e `RegionSlices` | Principais métodos: nenhum (enumeração de plano).
#[derive(Debug, Clone)]
enum CapturePlanKind {
    Monitor(WindowsMonitorInfo),
    Window { hwnd_raw: u64 },
    RegionSlices { slices: Vec<RegionMonitorSlice> },
}

/// Sub-retângulo de um monitor específico que compõe uma gravação de região retangular.
/// Armazena: `monitor`, caixa de origem (`src_x`, `src_y`, `copy_w`, `copy_h`) e destino (`dst_x`, `dst_y`) | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone)]
struct RegionMonitorSlice {
    monitor: WindowsMonitorInfo,
    src_x: u32,
    src_y: u32,
    copy_w: u32,
    copy_h: u32,
    dst_x: u32,
    dst_y: u32,
}

/// Configuração validada e dimensões finais pares (`out_w`, `out_h`) para o pipeline nativo do Windows.
/// Armazena: `plan_kind`, `target`, `out_w`, `out_h`, `with_system_audio` e `with_mic` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone)]
struct WindowsRecordingConfig {
    plan_kind: CapturePlanKind,
    target: RecordingTarget,
    out_w: u32,
    out_h: u32,
    with_system_audio: bool,
    with_mic: bool,
}

/// Pacote enviado das threads de captura WGC e WASAPI para a thread do `IMFSinkWriter`.
/// Armazena: variantes `Video`, `Audio` e `Stop` | Principais métodos: nenhum (tipo de dados).
enum EncoderPacket {
    Video {
        bgra: Vec<u8>,
        pts_hns: i64,
        dur_hns: i64,
    },
    Audio {
        pcm: Vec<u8>,
        pts_hns: i64,
        dur_hns: i64,
    },
    Stop,
}

/// Sessão de gravação já validada no Windows, aguardando a contagem regressiva para iniciar WGC/WASAPI/MF.
/// Armazena: `config`, `final_output_path` e `lock_file` | Principais métodos: `start`, `cancel`.
pub struct PreparedRecording {
    config: WindowsRecordingConfig,
    final_output_path: PathBuf,
    lock_file: Option<SessionLockFile>,
}

/// Controlador de uma sessão ativa de gravação nativa no Windows (WGC + WASAPI + Media Foundation MP4).
/// Armazena: `final_output_path`, `lock_file`, flags `stop_flag`/`paused_flag`/`exited_flag`, canais/threads e cronômetro | Principais métodos: `has_child_exited`, `elapsed_secs`, `pause`, `resume`, `stop`.
pub struct ActiveRecording {
    final_output_path: PathBuf,
    lock_file: Option<SessionLockFile>,
    stop_flag: Arc<AtomicBool>,
    paused_flag: Arc<AtomicBool>,
    exited_flag: Arc<AtomicBool>,
    enc_tx: Option<mpsc::SyncSender<EncoderPacket>>,
    video_thread: Option<JoinHandle<Result<(), String>>>,
    audio_thread: Option<JoinHandle<()>>,
    encoder_thread: Option<JoinHandle<Result<(), String>>>,
    accumulated_duration: Duration,
    segment_started_at: Option<Instant>,
    paused: bool,
}

/// Informa o perfil nativo suportado pelo Media Foundation no Windows 11 (`Mp4H264`).
/// Input: nenhum | Output: `Result<VideoProfile, String>` sempre `Ok(VideoProfile::Mp4H264)`.
#[allow(dead_code)]
pub fn probe_video_profile() -> Result<VideoProfile, String> {
    Ok(VideoProfile::Mp4H264)
}

/// Valida o alvo selecionado (`Fullscreen`, `Region`, `NativeWindow`), calcula dimensões pares e adquire trava da sessão.
/// Input: `_is_wayland` (`bool`), `target` (`RecordingTarget`), `with_system_audio` (`bool`), `with_mic` (`bool`), `_wayland_binding` (`Option<&()>`) | Output: `Result<PreparedRecording, String>`.
pub async fn prepare_recording(
    _is_wayland: bool,
    target: RecordingTarget,
    with_system_audio: bool,
    with_mic: bool,
    _wayland_binding: Option<&()>,
) -> Result<PreparedRecording, String> {
    ensure_per_monitor_dpi_aware();
    let monitors = enumerate_monitors().map_err(|e| e.to_string())?;
    let (vx, vy, vw, vh) = virtual_desktop_bounds_from_monitors(&monitors);

    let (plan_kind, raw_w, raw_h) = match target {
        RecordingTarget::Fullscreen => {
            let primary = monitors
                .into_iter()
                .next()
                .ok_or_else(|| "Nenhum monitor ativo encontrado para gravação.".to_string())?;
            let (w, h) = (primary.width, primary.height);
            (CapturePlanKind::Monitor(primary), w, h)
        }
        RecordingTarget::Region {
            rect,
            full_width,
            full_height,
        } => {
            if vw != full_width || vh != full_height {
                return Err(format!(
                    "Dimensões da região ({full_width}x{full_height}) divergem do desktop ({vw}x{vh})."
                ));
            }
            let plan = compute_region_crop(rect, full_width, full_height)?;
            let target_vx0 = vx + plan.left as i32;
            let target_vy0 = vy + plan.top as i32;
            let target_vx1 = target_vx0 + plan.crop_w as i32;
            let target_vy1 = target_vy0 + plan.crop_h as i32;

            let mut slices = Vec::new();
            for mon in monitors {
                let mon_x1 = mon.x + mon.width as i32;
                let mon_y1 = mon.y + mon.height as i32;
                let ix0 = target_vx0.max(mon.x);
                let iy0 = target_vy0.max(mon.y);
                let ix1 = target_vx1.min(mon_x1);
                let iy1 = target_vy1.min(mon_y1);
                if ix1 > ix0 && iy1 > iy0 {
                    slices.push(RegionMonitorSlice {
                        src_x: (ix0 - mon.x) as u32,
                        src_y: (iy0 - mon.y) as u32,
                        copy_w: (ix1 - ix0) as u32,
                        copy_h: (iy1 - iy0) as u32,
                        dst_x: (ix0 - target_vx0) as u32,
                        dst_y: (iy0 - target_vy0) as u32,
                        monitor: mon,
                    });
                }
            }
            if slices.is_empty() {
                return Err("Nenhum monitor intersecta a região selecionada.".to_string());
            }
            (
                CapturePlanKind::RegionSlices { slices },
                plan.crop_w,
                plan.crop_h,
            )
        }
        RecordingTarget::NativeWindow { handle } => {
            let (w, h) = query_window_size(handle).map_err(|e| e.to_string())?;
            (CapturePlanKind::Window { hwnd_raw: handle }, w, h)
        }
        RecordingTarget::Window => {
            return Err("Selecione uma janela específica antes de gravar.".to_string());
        }
    };

    let (pad_r, pad_b) = compute_even_min_padding(raw_w, raw_h);
    let out_w = raw_w + pad_r;
    let out_h = raw_h + pad_b;

    let (final_output_path, lock_file) = next_temp_recording_path(VideoProfile::Mp4H264.ext())?;

    let config = WindowsRecordingConfig {
        plan_kind,
        target,
        out_w,
        out_h,
        with_system_audio,
        with_mic,
    };

    Ok(PreparedRecording {
        config,
        final_output_path,
        lock_file: Some(lock_file),
    })
}

impl PreparedRecording {
    /// Inicia as threads nativas (`mf-encoder`, `wgc-video` e opcionalmente `wasapi-audio`) para gravar diretamente em MP4.
    /// Input: `mut self` | Output: `Result<ActiveRecording, String>`.
    pub async fn start(mut self) -> Result<ActiveRecording, String> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let paused_flag = Arc::new(AtomicBool::new(false));
        let exited_flag = Arc::new(AtomicBool::new(false));

        let (enc_tx, enc_rx) = mpsc::sync_channel::<EncoderPacket>(64);
        let (enc_ready_tx, enc_ready_rx) = mpsc::channel::<Result<bool, String>>();

        let mf_path = self.final_output_path.clone();
        let mf_w = self.config.out_w;
        let mf_h = self.config.out_h;
        let mf_want_audio = self.config.with_system_audio || self.config.with_mic;
        let mf_exited = Arc::clone(&exited_flag);

        let encoder_thread = std::thread::Builder::new()
            .name("mf-encoder".to_string())
            .spawn(move || {
                let res = run_mf_encoder_thread(
                    &mf_path,
                    mf_w,
                    mf_h,
                    mf_want_audio,
                    enc_rx,
                    enc_ready_tx,
                );
                if res.is_err() {
                    mf_exited.store(true, Ordering::Release);
                }
                res
            })
            .map_err(|e| format!("failed to spawn mf-encoder thread: {e}"))?;

        let enc_init = tokio::task::spawn_blocking(move || enc_ready_rx.recv())
            .await
            .map_err(|e| format!("mf-encoder readiness join error: {e}"))?
            .map_err(|_| "mf-encoder thread terminated unexpectedly".to_string())?;

        let has_audio_stream = match enc_init {
            Ok(has_audio) => has_audio,
            Err(err) => {
                let _ = encoder_thread.join();
                if let Some(lock) = self.lock_file.take() {
                    lock.release_and_remove();
                }
                let _ = fs::remove_file(&self.final_output_path);
                return Err(err);
            }
        };

        let (vid_ready_tx, vid_ready_rx) = mpsc::channel::<Result<(), String>>();
        let vid_cfg = self.config.clone();
        let vid_tx = enc_tx.clone();
        let vid_stop = Arc::clone(&stop_flag);
        let vid_pause = Arc::clone(&paused_flag);
        let vid_exited = Arc::clone(&exited_flag);

        let video_thread = std::thread::Builder::new()
            .name("wgc-video".to_string())
            .spawn(move || {
                let res = run_wgc_capture_thread(
                    vid_cfg,
                    vid_tx,
                    vid_stop,
                    vid_pause,
                    vid_exited.clone(),
                    vid_ready_tx,
                );
                if res.is_err() {
                    vid_exited.store(true, Ordering::Release);
                }
                res
            })
            .map_err(|e| format!("failed to spawn wgc-video thread: {e}"))?;

        let vid_init = tokio::task::spawn_blocking(move || vid_ready_rx.recv())
            .await
            .map_err(|e| format!("wgc-video readiness join error: {e}"))?
            .map_err(|_| "wgc-video thread terminated unexpectedly".to_string())?;

        if let Err(err) = vid_init {
            stop_flag.store(true, Ordering::Release);
            let _ = video_thread.join();
            let _ = enc_tx.send(EncoderPacket::Stop);
            let _ = encoder_thread.join();
            if let Some(lock) = self.lock_file.take() {
                lock.release_and_remove();
            }
            let _ = fs::remove_file(&self.final_output_path);
            return Err(err);
        }

        let audio_thread =
            if has_audio_stream && (self.config.with_system_audio || self.config.with_mic) {
                let aud_tx = enc_tx.clone();
                let aud_stop = Arc::clone(&stop_flag);
                let aud_pause = Arc::clone(&paused_flag);
                let want_loopback = self.config.with_system_audio;
                let want_mic = self.config.with_mic;
                Some(
                    std::thread::Builder::new()
                        .name("wasapi-audio".to_string())
                        .spawn(move || {
                            run_wasapi_audio_thread(
                                want_loopback,
                                want_mic,
                                aud_tx,
                                aud_stop,
                                aud_pause,
                            );
                        })
                        .map_err(|e| format!("failed to spawn wasapi-audio thread: {e}"))?,
                )
            } else {
                None
            };

        info!(
            path = %self.final_output_path.display(),
            target = ?self.config.target,
            width = self.config.out_w,
            height = self.config.out_h,
            "started native Windows screen recording"
        );

        Ok(ActiveRecording {
            final_output_path: self.final_output_path.clone(),
            lock_file: self.lock_file.take(),
            stop_flag,
            paused_flag,
            exited_flag,
            enc_tx: Some(enc_tx),
            video_thread: Some(video_thread),
            audio_thread,
            encoder_thread: Some(encoder_thread),
            accumulated_duration: Duration::ZERO,
            segment_started_at: Some(Instant::now()),
            paused: false,
        })
    }

    /// Cancela a gravação preparada antes de iniciar e libera o lock da sessão.
    /// Input: `mut self` | Output: nenhum (`()`).
    pub async fn cancel(mut self) {
        if let Some(lock) = self.lock_file.take() {
            lock.release_and_remove();
        }
    }
}

impl Drop for PreparedRecording {
    /// Libera o arquivo de lock caso a gravação preparada seja descartada sem iniciar.
    /// Input: `&mut self` | Output: nenhum (`()`).
    fn drop(&mut self) {
        if let Some(lock) = self.lock_file.take() {
            lock.release_and_remove();
        }
    }
}

impl ActiveRecording {
    /// Informa se a captura foi encerrada inesperadamente (ex.: janela alvo fechada ou falha crítica).
    /// Input: `&self` | Output: `bool` indicando encerramento prematuro.
    pub fn has_child_exited(&self) -> bool {
        !self.paused && self.exited_flag.load(Ordering::Acquire)
    }

    /// Calcula o tempo total ativo de gravação em segundos desconsiderando intervalos em pausa.
    /// Input: `&self` | Output: duração acumulada em segundos (`u64`).
    pub fn elapsed_secs(&self) -> u64 {
        let active = self
            .segment_started_at
            .map(|t| t.elapsed())
            .unwrap_or_default();
        (self.accumulated_duration + active).as_secs()
    }

    /// Pausa a gravação suspendendo o envio de amostras de vídeo/áudio sem fechar o MP4.
    /// Input: `&mut self` | Output: `Result<(), String>`.
    pub async fn pause(&mut self) -> Result<(), String> {
        if self.paused {
            return Ok(());
        }
        self.paused_flag.store(true, Ordering::Release);
        if let Some(started) = self.segment_started_at.take() {
            self.accumulated_duration += started.elapsed();
        }
        self.paused = true;
        info!(
            elapsed_secs = self.elapsed_secs(),
            "paused Windows recording"
        );
        Ok(())
    }

    /// Retoma a gravação pausada reiniciando a entrega contínua de quadros e áudio com timestamps alinhados.
    /// Input: `&mut self` | Output: `Result<(), String>`.
    pub async fn resume(&mut self) -> Result<(), String> {
        if !self.paused {
            return Ok(());
        }
        if self.exited_flag.load(Ordering::Acquire) {
            return Err(
                "A gravação foi interrompida porque a origem ficou indisponível.".to_string(),
            );
        }
        self.paused_flag.store(false, Ordering::Release);
        self.segment_started_at = Some(Instant::now());
        self.paused = false;
        info!("resumed Windows recording");
        Ok(())
    }

    /// Encerra captura WGC/WASAPI, finaliza o muxer `IMFSinkWriter`, libera locks e devolve caminho + duração.
    /// Input: `mut self` | Output: `Result<(PathBuf, u64), String>`.
    pub async fn stop(mut self) -> Result<(PathBuf, u64), String> {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(started) = self.segment_started_at.take() {
            self.accumulated_duration += started.elapsed();
        }
        let elapsed = self.accumulated_duration.as_secs();

        let vid_h = self.video_thread.take();
        let aud_h = self.audio_thread.take();
        let enc_tx = self.enc_tx.take();
        let enc_h = self.encoder_thread.take();

        let join_res = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let vid_res = if let Some(h) = vid_h {
                h.join()
                    .map_err(|_| "wgc-video thread panicked".to_string())?
            } else {
                Ok(())
            };
            if let Some(h) = aud_h {
                let _ = h.join();
            }
            if let Some(tx) = enc_tx {
                let _ = tx.send(EncoderPacket::Stop);
            }
            let enc_res = if let Some(h) = enc_h {
                h.join()
                    .map_err(|_| "mf-encoder thread panicked".to_string())?
            } else {
                Ok(())
            };
            enc_res.and(vid_res)
        })
        .await
        .map_err(|e| format!("recording shutdown task panicked: {e}"))?;

        if let Some(lock) = self.lock_file.take() {
            lock.release_and_remove();
        }

        join_res?;

        let len = fs::metadata(&self.final_output_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if len == 0 {
            let _ = fs::remove_file(&self.final_output_path);
            return Err("Recording produced an empty MP4 file".to_string());
        }

        info!(
            path = %self.final_output_path.display(),
            elapsed_secs = elapsed,
            bytes = len,
            "finalized native Windows MP4 recording"
        );
        Ok((self.final_output_path.clone(), elapsed))
    }
}

impl Drop for ActiveRecording {
    /// Garante sinalização de parada, encerramento limpo das threads e liberação do lock caso descartado sem `stop`.
    /// Input: `&mut self` | Output: nenhum (`()`).
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(h) = self.video_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.audio_thread.take() {
            let _ = h.join();
        }
        if let Some(tx) = self.enc_tx.take() {
            let _ = tx.send(EncoderPacket::Stop);
        }
        if let Some(h) = self.encoder_thread.take() {
            let _ = h.join();
        }
        if let Some(lock) = self.lock_file.take() {
            lock.release_and_remove();
        }
    }
}

/// Garante `MFShutdown` determinístico ao sair da thread do codificador Media Foundation.
/// Armazena: campo marcador sem dados | Principais métodos: `Drop::drop`.
struct MfScope;

impl Drop for MfScope {
    /// Chama `MFShutdown` ao finalizar a sessão Media Foundation.
    /// Input: `&mut self` | Output: nenhum (`()`).
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

/// Configura os tipos de entrada/saída de vídeo H.264 e áudio AAC e grava amostras recebidas no `IMFSinkWriter`.
/// Input: caminho `output_path`, dimensões `(width, height)`, flag `want_audio`, canal `rx` e sinalizador `ready_tx` | Output: `Result<(), String>`.
fn run_mf_encoder_thread(
    output_path: &Path,
    width: u32,
    height: u32,
    want_audio: bool,
    rx: mpsc::Receiver<EncoderPacket>,
    ready_tx: mpsc::Sender<Result<bool, String>>,
) -> Result<(), String> {
    let _winrt = match WinRtScope::enter_mta() {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            let _ = ready_tx.send(Err(msg.clone()));
            return Err(msg);
        }
    };

    if let Err(err) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
        let msg = format!("MFStartup failed: {err}");
        let _ = ready_tx.send(Err(msg.clone()));
        return Err(msg);
    }
    let _mf_scope = MfScope;

    let setup_res = (|| -> Result<(IMFSinkWriter, Option<u32>), String> {
        let mut attrs_opt: Option<IMFAttributes> = None;
        unsafe {
            MFCreateAttributes(&mut attrs_opt, 2)
                .map_err(|e| format!("MFCreateAttributes failed: {e}"))?;
        }
        let attrs = attrs_opt.ok_or_else(|| "MFCreateAttributes returned None".to_string())?;
        unsafe {
            let _ = attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1);
            let _ = attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1);
        }

        let url = HSTRING::from(output_path.to_string_lossy().as_ref());
        let writer = unsafe {
            MFCreateSinkWriterFromURL(&url, None::<&IMFByteStream>, Some(&attrs))
                .map_err(|e| format!("MFCreateSinkWriterFromURL failed: {e}"))?
        };

        // Stream 0: H.264 output type
        let out_video_type: IMFMediaType = unsafe {
            MFCreateMediaType().map_err(|e| format!("MFCreateMediaType(video out): {e}"))?
        };
        unsafe {
            out_video_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            out_video_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(|e| e.to_string())?;
            out_video_type
                .SetUINT32(&MF_MT_AVG_BITRATE, 8_000_000)
                .map_err(|e| e.to_string())?;
            out_video_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| e.to_string())?;
            out_video_type
                .SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(width, height))
                .map_err(|e| e.to_string())?;
            out_video_type
                .SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(TARGET_FPS, 1))
                .map_err(|e| e.to_string())?;
            out_video_type
                .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32_pair(1, 1))
                .map_err(|e| e.to_string())?;
            let _ =
                out_video_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32);
        }

        let video_stream_idx = unsafe {
            writer
                .AddStream(&out_video_type)
                .map_err(|e| format!("IMFSinkWriter::AddStream(video) failed: {e}"))?
        };

        // Stream 0: RGB32 top-down input type (stride positivo explicitado)
        let in_video_type: IMFMediaType = unsafe {
            MFCreateMediaType().map_err(|e| format!("MFCreateMediaType(video in): {e}"))?
        };
        unsafe {
            in_video_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            in_video_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .map_err(|e| e.to_string())?;
            in_video_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| e.to_string())?;
            in_video_type
                .SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(width, height))
                .map_err(|e| e.to_string())?;
            in_video_type
                .SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(TARGET_FPS, 1))
                .map_err(|e| e.to_string())?;
            in_video_type
                .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32_pair(1, 1))
                .map_err(|e| e.to_string())?;
            let _ = in_video_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4);
            writer
                .SetInputMediaType(video_stream_idx, &in_video_type, None::<&IMFAttributes>)
                .map_err(|e| format!("SetInputMediaType(video) failed: {e}"))?;
        }

        // Stream 1: opcional AAC 192 kbps (melhor esforço: se faltar codec AAC, prossegue só vídeo)
        let audio_stream_idx = if want_audio {
            match configure_mf_audio_stream(&writer) {
                Ok(idx) => Some(idx),
                Err(err) => {
                    warn!(error = %err, "MF AAC audio stream setup failed; recording video-only");
                    None
                }
            }
        } else {
            None
        };

        unsafe {
            writer
                .BeginWriting()
                .map_err(|e| format!("IMFSinkWriter::BeginWriting failed: {e}"))?;
        }

        Ok((writer, audio_stream_idx))
    })();

    let (writer, audio_stream_idx) = match setup_res {
        Ok(pair) => {
            let _ = ready_tx.send(Ok(pair.1.is_some()));
            pair
        }
        Err(err) => {
            let _ = ready_tx.send(Err(err.clone()));
            return Err(err);
        }
    };

    let mut wrote_any_video = false;
    while let Ok(pkt) = rx.recv() {
        match pkt {
            EncoderPacket::Video {
                bgra,
                pts_hns,
                dur_hns,
            } => {
                if let Err(err) = write_mf_sample(&writer, 0, &bgra, pts_hns, dur_hns) {
                    return Err(format!("WriteSample(video) failed: {err}"));
                }
                wrote_any_video = true;
            }
            EncoderPacket::Audio {
                pcm,
                pts_hns,
                dur_hns,
            } => {
                if let Some(aidx) = audio_stream_idx {
                    let _ = write_mf_sample(&writer, aidx, &pcm, pts_hns, dur_hns);
                }
            }
            EncoderPacket::Stop => break,
        }
    }

    if !wrote_any_video {
        return Err("Nenhum quadro de vídeo foi gravado.".to_string());
    }

    unsafe {
        writer
            .Finalize()
            .map_err(|e| format!("IMFSinkWriter::Finalize failed: {e}"))?;
    }
    Ok(())
}

/// Adiciona e configura a faixa de áudio AAC (saída) + PCM 48 kHz estéreo (entrada) no `IMFSinkWriter`.
/// Input: `writer` (`&IMFSinkWriter`) | Output: `Result<u32, String>` com o índice da stream de áudio.
fn configure_mf_audio_stream(writer: &IMFSinkWriter) -> Result<u32, String> {
    let out_audio: IMFMediaType =
        unsafe { MFCreateMediaType().map_err(|e| format!("MFCreateMediaType(audio out): {e}"))? };
    unsafe {
        out_audio
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(|e| e.to_string())?;
        out_audio
            .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)
            .map_err(|e| e.to_string())?;
        out_audio
            .SetUINT32(
                &MF_MT_AUDIO_BITS_PER_SAMPLE,
                u32::from(AUDIO_BITS_PER_SAMPLE),
            )
            .map_err(|e| e.to_string())?;
        out_audio
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE)
            .map_err(|e| e.to_string())?;
        out_audio
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(AUDIO_CHANNELS))
            .map_err(|e| e.to_string())?;
        out_audio
            .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)
            .map_err(|e| e.to_string())?;
        let _ = out_audio.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0);
    }

    let idx = unsafe {
        writer
            .AddStream(&out_audio)
            .map_err(|e| format!("AddStream(audio) failed: {e}"))?
    };

    let in_audio: IMFMediaType =
        unsafe { MFCreateMediaType().map_err(|e| format!("MFCreateMediaType(audio in): {e}"))? };
    let block_align = u32::from(AUDIO_CHANNELS * (AUDIO_BITS_PER_SAMPLE / 8));
    unsafe {
        in_audio
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(|e| e.to_string())?;
        in_audio
            .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
            .map_err(|e| e.to_string())?;
        in_audio
            .SetUINT32(
                &MF_MT_AUDIO_BITS_PER_SAMPLE,
                u32::from(AUDIO_BITS_PER_SAMPLE),
            )
            .map_err(|e| e.to_string())?;
        in_audio
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE)
            .map_err(|e| e.to_string())?;
        in_audio
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(AUDIO_CHANNELS))
            .map_err(|e| e.to_string())?;
        in_audio
            .SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)
            .map_err(|e| e.to_string())?;
        in_audio
            .SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                AUDIO_SAMPLE_RATE * block_align,
            )
            .map_err(|e| e.to_string())?;
        writer
            .SetInputMediaType(idx, &in_audio, None::<&IMFAttributes>)
            .map_err(|e| format!("SetInputMediaType(audio) failed: {e}"))?;
    }

    Ok(idx)
}

/// Empacota `bytes` em um `IMFSample` com timestamp/duração e envia ao `IMFSinkWriter`.
/// Input: `writer`, índice `stream_idx`, `bytes`, `pts_hns` e `dur_hns` | Output: `Result<(), String>`.
fn write_mf_sample(
    writer: &IMFSinkWriter,
    stream_idx: u32,
    bytes: &[u8],
    pts_hns: i64,
    dur_hns: i64,
) -> Result<(), String> {
    let buffer = unsafe {
        MFCreateMemoryBuffer(bytes.len() as u32)
            .map_err(|e| format!("MFCreateMemoryBuffer: {e}"))?
    };
    let mut dst_ptr: *mut u8 = std::ptr::null_mut();
    unsafe {
        buffer
            .Lock(&mut dst_ptr, None, None)
            .map_err(|e| format!("IMFMediaBuffer::Lock: {e}"))?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst_ptr, bytes.len());
        let _ = buffer.Unlock();
        buffer
            .SetCurrentLength(bytes.len() as u32)
            .map_err(|e| format!("SetCurrentLength: {e}"))?;
    }

    let sample = unsafe { MFCreateSample().map_err(|e| format!("MFCreateSample: {e}"))? };
    unsafe {
        sample
            .AddBuffer(&buffer)
            .map_err(|e| format!("AddBuffer: {e}"))?;
        sample
            .SetSampleTime(pts_hns)
            .map_err(|e| format!("SetSampleTime: {e}"))?;
        sample
            .SetSampleDuration(dur_hns)
            .map_err(|e| format!("SetSampleDuration: {e}"))?;
        writer
            .WriteSample(stream_idx, &sample)
            .map_err(|e| format!("WriteSample: {e}"))?;
    }
    Ok(())
}

/// Fonte individual de captura WGC + textura staging D3D11 reutilizada entre quadros.
/// Armazena: `item`, `pool`, `session`, `closed_token`, dimensões atuais, textura `staging` e geometria de recorte/destino | Principais métodos: `open`, `drain_latest_into`.
struct WgcCaptureSource {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: windows::Graphics::Capture::GraphicsCaptureSession,
    closed_token: Option<EventRegistrationToken>,
    pool_size: SizeInt32,
    staging: Option<(u32, u32, ID3D11Texture2D)>,
    src_x: u32,
    src_y: u32,
    max_copy_w: u32,
    max_copy_h: u32,
    dst_x: u32,
    dst_y: u32,
}

/// Referências aos dispositivos Direct3D11/WinRT reutilizados ao drenar cada fonte de captura.
/// Armazena: `dev` (`&ID3D11Device`), `ctx` (`&ID3D11DeviceContext`) e `winrt` (`&IDirect3DDevice`) | Principais métodos: nenhum (contexto de chamada).
struct D3dCaptureContext<'a> {
    dev: &'a ID3D11Device,
    ctx: &'a ID3D11DeviceContext,
    winrt: &'a IDirect3DDevice,
}

impl WgcCaptureSource {
    /// Inicializa pool/sessão `Windows.Graphics.Capture` para monitor ou janela e assina fechamento se janela.
    /// Input: dispositivo `winrt_dev`, `item`, região origem/destino e flags opcionais de encerramento | Output: `Result<Self, String>`.
    #[allow(clippy::too_many_arguments)]
    fn open(
        winrt_dev: &IDirect3DDevice,
        item: GraphicsCaptureItem,
        src_x: u32,
        src_y: u32,
        max_copy_w: u32,
        max_copy_h: u32,
        dst_x: u32,
        dst_y: u32,
        window_close_signals: Option<(Arc<AtomicBool>, Arc<AtomicBool>)>,
    ) -> Result<Self, String> {
        let size = item
            .Size()
            .map_err(|e| format!("GraphicsCaptureItem::Size: {e}"))?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err("GraphicsCaptureItem reportou tamanho nulo.".to_string());
        }

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            winrt_dev,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(|e| format!("CreateFreeThreaded failed: {e}"))?;

        let closed_token = if let Some((stop_flag, exited_flag)) = window_close_signals {
            Some(
                item.Closed(&TypedEventHandler::new(move |_, _| {
                    exited_flag.store(true, Ordering::Release);
                    stop_flag.store(true, Ordering::Release);
                    Ok(())
                }))
                .map_err(|e| format!("GraphicsCaptureItem::Closed subscribe failed: {e}"))?,
            )
        } else {
            None
        };

        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("CreateCaptureSession failed: {e}"))?;
        let _ = session.SetIsCursorCaptureEnabled(true);
        let _ = session.SetIsBorderRequired(false);
        session
            .StartCapture()
            .map_err(|e| format!("StartCapture failed: {e}"))?;

        Ok(Self {
            item,
            pool,
            session,
            closed_token,
            pool_size: size,
            staging: None,
            src_x,
            src_y,
            max_copy_w,
            max_copy_h,
            dst_x,
            dst_y,
        })
    }

    /// Drena todos os quadros pendentes no pool mantendo apenas o mais recente e copia a sub-região exata para `dst_bgra`.
    /// Input: contexto Direct3D/WinRT `gpu`, largura total `out_w`, altura `out_h`, buffer `dst_bgra` e modo `drop_only` | Output: `Result<bool, String>` (`true` se houve quadro novo copiado).
    fn drain_latest_into(
        &mut self,
        gpu: &D3dCaptureContext<'_>,
        out_w: u32,
        out_h: u32,
        dst_bgra: &mut [u8],
        drop_only: bool,
    ) -> Result<bool, String> {
        let mut latest_frame = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            if let Some(prev) = latest_frame.replace(frame) {
                let _ = prev.Close();
            }
        }
        let Some(frame) = latest_frame else {
            return Ok(false);
        };

        let content_size = frame.ContentSize().unwrap_or(self.pool_size);
        let need_recreate = content_size.Width != self.pool_size.Width
            || content_size.Height != self.pool_size.Height;

        let mut copied = false;
        if !drop_only && content_size.Width > 0 && content_size.Height > 0 {
            let surface = frame
                .Surface()
                .map_err(|e| format!("Frame::Surface: {e}"))?;
            let access: IDirect3DDxgiInterfaceAccess = surface
                .cast()
                .map_err(|e| format!("DXGI access cast: {e}"))?;
            let src_tex: ID3D11Texture2D = unsafe { access.GetInterface() }
                .map_err(|e| format!("GetInterface texture2d: {e}"))?;

            let mut src_desc = D3D11_TEXTURE2D_DESC::default();
            unsafe {
                src_tex.GetDesc(&mut src_desc);
            }

            let avail_w = src_desc
                .Width
                .min(content_size.Width as u32)
                .saturating_sub(self.src_x);
            let avail_h = src_desc
                .Height
                .min(content_size.Height as u32)
                .saturating_sub(self.src_y);
            let copy_w = avail_w
                .min(self.max_copy_w)
                .min(out_w.saturating_sub(self.dst_x));
            let copy_h = avail_h
                .min(self.max_copy_h)
                .min(out_h.saturating_sub(self.dst_y));

            if copy_w > 0 && copy_h > 0 {
                let reuse_valid = self
                    .staging
                    .as_ref()
                    .is_some_and(|(sw, sh, _)| *sw == copy_w && *sh == copy_h);
                if !reuse_valid {
                    let st_desc = D3D11_TEXTURE2D_DESC {
                        Width: copy_w,
                        Height: copy_h,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: src_desc.Format,
                        SampleDesc: src_desc.SampleDesc,
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        MiscFlags: 0,
                    };
                    let mut st_opt = None;
                    unsafe {
                        gpu.dev
                            .CreateTexture2D(&st_desc, None, Some(&mut st_opt))
                            .map_err(|e| format!("CreateTexture2D staging: {e}"))?;
                    }
                    let st_tex = st_opt.ok_or_else(|| "staging texture None".to_string())?;
                    self.staging = Some((copy_w, copy_h, st_tex));
                }

                if let Some((_, _, st_tex)) = &self.staging {
                    let src_res: ID3D11Resource = src_tex
                        .cast()
                        .map_err(|e| format!("src ID3D11Resource: {e}"))?;
                    let dst_res: ID3D11Resource = st_tex
                        .cast()
                        .map_err(|e| format!("dst ID3D11Resource: {e}"))?;
                    let box_src = D3D11_BOX {
                        left: self.src_x,
                        top: self.src_y,
                        front: 0,
                        right: self.src_x + copy_w,
                        bottom: self.src_y + copy_h,
                        back: 1,
                    };
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    unsafe {
                        gpu.ctx.CopySubresourceRegion(
                            &dst_res,
                            0,
                            0,
                            0,
                            0,
                            &src_res,
                            0,
                            Some(&box_src),
                        );
                        gpu.ctx
                            .Map(&dst_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                            .map_err(|e| format!("D3D11 Map: {e}"))?;
                    }

                    let row_pitch = mapped.RowPitch as usize;
                    let base_ptr = mapped.pData as *const u8;
                    for row in 0..copy_h {
                        let src_row = unsafe {
                            std::slice::from_raw_parts(
                                base_ptr.add((row as usize) * row_pitch),
                                (copy_w as usize) * 4,
                            )
                        };
                        let dst_off = (((self.dst_y + row) as usize) * (out_w as usize)
                            + (self.dst_x as usize))
                            * 4;
                        let dst_row = &mut dst_bgra[dst_off..dst_off + (copy_w as usize) * 4];
                        dst_row.copy_from_slice(src_row);
                        for px in dst_row.as_chunks_mut::<4>().0 {
                            px[3] = 255;
                        }
                    }

                    unsafe {
                        gpu.ctx.Unmap(&dst_res, 0);
                    }
                    copied = true;
                }
            }
        }

        let _ = frame.Close();
        if need_recreate
            && content_size.Width > 0
            && content_size.Height > 0
            && self
                .pool
                .Recreate(
                    gpu.winrt,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    content_size,
                )
                .is_ok()
        {
            self.pool_size = content_size;
        }
        Ok(copied)
    }
}

impl Drop for WgcCaptureSource {
    /// Fecha a sessão/pool `Windows.Graphics.Capture` e remove event handler `Closed` ao encerrar.
    /// Input: `&mut self` | Output: nenhum (`()`).
    fn drop(&mut self) {
        if let Some(tok) = self.closed_token.take() {
            let _ = self.item.RemoveClosed(tok);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// Preenche o buffer BGRA com preto totalmente opaco (`[0, 0, 0, 255]`) garantindo padding/bordas limpas.
/// Input: `buf` (`&mut [u8]`) | Output: nenhum (`()`).
fn clear_opaque_black_bgra(buf: &mut [u8]) {
    for px in buf.as_chunks_mut::<4>().0 {
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
        px[3] = 255;
    }
}

/// Executa o loop de captura `Windows.Graphics.Capture` a 30 fps enviando quadros recortados/preenchidos ao `mf-encoder`.
/// Input: `config`, canal `enc_tx`, flags de controle e canal `ready_tx` | Output: `Result<(), String>`.
fn run_wgc_capture_thread(
    config: WindowsRecordingConfig,
    enc_tx: mpsc::SyncSender<EncoderPacket>,
    stop_flag: Arc<AtomicBool>,
    paused_flag: Arc<AtomicBool>,
    exited_flag: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let _winrt = match WinRtScope::enter_mta() {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            let _ = ready_tx.send(Err(msg.clone()));
            return Err(msg);
        }
    };

    let init_res = (|| -> Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice, Vec<WgcCaptureSource>), String> {
        let (d3d_dev, d3d_ctx, winrt_dev) =
            create_d3d11_device_and_winrt().map_err(|e| e.to_string())?;

        let mut sources = Vec::new();
        match &config.plan_kind {
            CapturePlanKind::Monitor(mon) => {
                let item = create_capture_item_for_monitor(mon.hmonitor)
                    .map_err(|e| e.to_string())?;
                sources.push(WgcCaptureSource::open(
                    &winrt_dev,
                    item,
                    0,
                    0,
                    mon.width,
                    mon.height,
                    0,
                    0,
                    None,
                )?);
            }
            CapturePlanKind::Window { hwnd_raw } => {
                let hwnd = HWND(*hwnd_raw as usize as isize);
                let item =
                    create_capture_item_for_window(hwnd).map_err(|e| e.to_string())?;
                sources.push(WgcCaptureSource::open(
                    &winrt_dev,
                    item,
                    0,
                    0,
                    config.out_w,
                    config.out_h,
                    0,
                    0,
                    Some((Arc::clone(&stop_flag), Arc::clone(&exited_flag))),
                )?);
            }
            CapturePlanKind::RegionSlices { slices } => {
                for slice in slices {
                    let item = create_capture_item_for_monitor(slice.monitor.hmonitor)
                        .map_err(|e| e.to_string())?;
                    sources.push(WgcCaptureSource::open(
                        &winrt_dev,
                        item,
                        slice.src_x,
                        slice.src_y,
                        slice.copy_w,
                        slice.copy_h,
                        slice.dst_x,
                        slice.dst_y,
                        None,
                    )?);
                }
            }
        }
        Ok((d3d_dev, d3d_ctx, winrt_dev, sources))
    })();

    let (d3d_dev, d3d_ctx, winrt_dev, mut sources) = match init_res {
        Ok(v) => {
            let _ = ready_tx.send(Ok(()));
            v
        }
        Err(err) => {
            let _ = ready_tx.send(Err(err.clone()));
            return Err(err);
        }
    };

    let gpu = D3dCaptureContext {
        dev: &d3d_dev,
        ctx: &d3d_ctx,
        winrt: &winrt_dev,
    };
    let frame_bytes = (config.out_w as usize) * (config.out_h as usize) * 4;
    let mut work_buf = vec![0u8; frame_bytes];
    clear_opaque_black_bgra(&mut work_buf);
    let mut last_valid_buf: Option<Vec<u8>> = None;
    let mut pts_hns: i64 = 0;
    let tick_interval = Duration::from_micros(1_000_000 / u64::from(TARGET_FPS));
    let mut next_tick = Instant::now();

    // Aguarda até 1s pelo primeiro quadro real antes de começar repetição estática.
    let first_wait_start = Instant::now();
    while !stop_flag.load(Ordering::Acquire) && first_wait_start.elapsed() < Duration::from_secs(1)
    {
        let mut any_new = false;
        for src in &mut sources {
            if src.drain_latest_into(&gpu, config.out_w, config.out_h, &mut work_buf, false)? {
                any_new = true;
            }
        }
        if any_new {
            last_valid_buf = Some(work_buf.clone());
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    while !stop_flag.load(Ordering::Acquire) {
        let is_paused = paused_flag.load(Ordering::Acquire);
        if is_paused {
            for src in &mut sources {
                let _ =
                    src.drain_latest_into(&gpu, config.out_w, config.out_h, &mut work_buf, true);
            }
            next_tick = Instant::now() + tick_interval;
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }

        let mut updated = false;
        if let CapturePlanKind::Window { .. } = config.plan_kind {
            // Limpa antes quando captura janela para não manter lixo ao redimensionar menor.
            clear_opaque_black_bgra(&mut work_buf);
        }
        for src in &mut sources {
            if src.drain_latest_into(&gpu, config.out_w, config.out_h, &mut work_buf, false)? {
                updated = true;
            }
        }
        if updated {
            last_valid_buf = Some(work_buf.clone());
        } else if let Some(prev) = &last_valid_buf {
            work_buf.copy_from_slice(prev);
        } else {
            clear_opaque_black_bgra(&mut work_buf);
        }

        if let Some(to_send) = last_valid_buf.as_ref().cloned() {
            if enc_tx
                .send(EncoderPacket::Video {
                    bgra: to_send,
                    pts_hns,
                    dur_hns: FRAME_DURATION_HNS,
                })
                .is_err()
            {
                break;
            }
            pts_hns += FRAME_DURATION_HNS;
        }

        next_tick += tick_interval;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else if now.duration_since(next_tick) > tick_interval * 4 {
            next_tick = now;
        }
    }

    Ok(())
}

/// Formato de amostra decodificado de um dispositivo WASAPI para conversão normalizada.
/// Armazena: `channels`, `sample_rate`, `is_float`, `bits_per_sample` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, Copy)]
struct WasapiSourceFormat {
    channels: usize,
    sample_rate: u32,
    is_float: bool,
    bits_per_sample: u16,
}

/// Fluxo individual WASAPI (`eRender` loopback ou `eCapture` microfone) com fila PCM estéreo 48 kHz reamostrada.
/// Armazena: `_client`, `capture_client`, `src_fmt` e buffer `pending` | Principais métodos: `open`, `pull_into_queue`.
struct WasapiCaptureStream {
    _client: IAudioClient,
    capture_client: IAudioCaptureClient,
    src_fmt: WasapiSourceFormat,
    pending: VecDeque<i16>,
}

impl WasapiCaptureStream {
    /// Inicializa um fluxo WASAPI em modo compartilhado usando `AUTOCONVERTPCM` quando aceito ou fallback no mix format nativo.
    /// Input: `is_loopback` (`bool`) | Output: `Result<Self, String>`.
    fn open(is_loopback: bool) -> Result<Self, String> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None::<&IUnknown>, CLSCTX_ALL) }
                .map_err(|e| format!("MMDeviceEnumerator failed: {e}"))?;

        let flow = if is_loopback { eRender } else { eCapture };
        let device: IMMDevice = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }
            .map_err(|e| format!("GetDefaultAudioEndpoint failed: {e}"))?;

        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|e| format!("IMMDevice::Activate(IAudioClient) failed: {e}"))?;

        let base_flags = if is_loopback {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };
        let target_wfx = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: AUDIO_CHANNELS,
            nSamplesPerSec: AUDIO_SAMPLE_RATE,
            nAvgBytesPerSec: AUDIO_SAMPLE_RATE
                * u32::from(AUDIO_CHANNELS * (AUDIO_BITS_PER_SAMPLE / 8)),
            nBlockAlign: AUDIO_CHANNELS * (AUDIO_BITS_PER_SAMPLE / 8),
            wBitsPerSample: AUDIO_BITS_PER_SAMPLE,
            cbSize: 0,
        };

        let autoconv_flags = base_flags
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        let buffer_hns = 2_000_000i64; // 200 ms

        let src_fmt = if unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                autoconv_flags,
                buffer_hns,
                0,
                &target_wfx,
                None,
            )
        }
        .is_ok()
        {
            WasapiSourceFormat {
                channels: usize::from(AUDIO_CHANNELS),
                sample_rate: AUDIO_SAMPLE_RATE,
                is_float: false,
                bits_per_sample: AUDIO_BITS_PER_SAMPLE,
            }
        } else {
            let mix_ptr = unsafe {
                client
                    .GetMixFormat()
                    .map_err(|e| format!("GetMixFormat failed: {e}"))?
            };
            let parsed = unsafe { parse_wave_format(mix_ptr) };
            let init_res = unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    base_flags,
                    buffer_hns,
                    0,
                    mix_ptr,
                    None,
                )
            };
            unsafe {
                CoTaskMemFree(Some(mix_ptr as *const _));
            }
            init_res.map_err(|e| format!("IAudioClient::Initialize failed: {e}"))?;
            parsed?
        };

        let capture_client: IAudioCaptureClient = unsafe {
            client
                .GetService()
                .map_err(|e| format!("GetService(IAudioCaptureClient) failed: {e}"))?
        };
        unsafe {
            client
                .Start()
                .map_err(|e| format!("IAudioClient::Start failed: {e}"))?;
        }

        Ok(Self {
            _client: client,
            capture_client,
            src_fmt,
            pending: VecDeque::with_capacity(AUDIO_FRAMES_PER_CHUNK * 8),
        })
    }

    /// Drena todos os pacotes disponíveis do endpoint WASAPI convertendo-os para PCM 48 kHz estéreo na fila `pending`.
    /// Input: `&mut self` e `drop_only` (`bool`) | Output: nenhum (`()`).
    fn pull_into_queue(&mut self, drop_only: bool) {
        loop {
            let Ok(pkt_frames) = (unsafe { self.capture_client.GetNextPacketSize() }) else {
                break;
            };
            if pkt_frames == 0 {
                break;
            }
            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            if unsafe {
                self.capture_client.GetBuffer(
                    &mut data_ptr,
                    &mut num_frames,
                    &mut flags,
                    None,
                    None,
                )
            }
            .is_err()
            {
                break;
            }

            if !drop_only && num_frames > 0 {
                if (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) != 0 || data_ptr.is_null() {
                    let silent_frames = (((u64::from(num_frames) * u64::from(AUDIO_SAMPLE_RATE))
                        / u64::from(self.src_fmt.sample_rate.max(1)))
                    .max(1)) as usize;
                    for _ in 0..(silent_frames * 2) {
                        self.pending.push_back(0);
                    }
                } else {
                    let bytes_per_frame =
                        self.src_fmt.channels * usize::from(self.src_fmt.bits_per_sample / 8);
                    let slice = unsafe {
                        std::slice::from_raw_parts(
                            data_ptr,
                            (num_frames as usize) * bytes_per_frame,
                        )
                    };
                    append_converted_audio(slice, self.src_fmt, &mut self.pending);
                }
            }

            let _ = unsafe { self.capture_client.ReleaseBuffer(num_frames) };
        }
        if drop_only {
            self.pending.clear();
        }
    }
}

/// Extrai parâmetros de canais, taxa e tipo de amostra de um ponteiro `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE`.
/// Input: ponteiro `wfx_ptr` (`*const WAVEFORMATEX`) | Output: `Result<WasapiSourceFormat, String>`.
unsafe fn parse_wave_format(wfx_ptr: *const WAVEFORMATEX) -> Result<WasapiSourceFormat, String> {
    if wfx_ptr.is_null() {
        return Err("WAVEFORMATEX nulo retornado por GetMixFormat".to_string());
    }
    let wfx = std::ptr::read_unaligned(wfx_ptr);
    let format_tag = wfx.wFormatTag;
    let cb_size = wfx.cbSize;
    let n_channels = wfx.nChannels;
    let n_samples_per_sec = wfx.nSamplesPerSec;
    let bits_per_sample = wfx.wBitsPerSample;
    let is_float = if u32::from(format_tag) == WAVE_FORMAT_IEEE_FLOAT {
        true
    } else if u32::from(format_tag) == WAVE_FORMAT_PCM {
        false
    } else if u32::from(format_tag) == WAVE_FORMAT_EXTENSIBLE && cb_size >= 22 {
        let ext = std::ptr::read_unaligned(wfx_ptr as *const WAVEFORMATEXTENSIBLE);
        let subformat = ext.SubFormat;
        if subformat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            true
        } else if subformat == KSDATAFORMAT_SUBTYPE_PCM {
            false
        } else {
            return Err("Subformato WASAPI não suportado".to_string());
        }
    } else {
        return Err(format!("Formato WASAPI {format_tag} não suportado"));
    };

    Ok(WasapiSourceFormat {
        channels: usize::from(n_channels.max(1)),
        sample_rate: n_samples_per_sec.max(1),
        is_float,
        bits_per_sample,
    })
}

/// Converte amostras float32/PCM16 de qualquer taxa/canalização para PCM `i16` estéreo a 48 kHz com downmix seguro.
/// Input: `bytes`, formato `fmt` e fila destino `out` | Output: nenhum (preenche `out`).
fn append_converted_audio(bytes: &[u8], fmt: WasapiSourceFormat, out: &mut VecDeque<i16>) {
    let bytes_per_sample = usize::from(fmt.bits_per_sample / 8).max(1);
    let frame_stride = fmt.channels * bytes_per_sample;
    let in_frames = bytes.len() / frame_stride.max(1);
    if in_frames == 0 {
        return;
    }

    let read_stereo_f32 = |frame_idx: usize| -> (f32, f32) {
        let base = frame_idx * frame_stride;
        let sample_at = |ch: usize| -> f32 {
            let off = base + ch * bytes_per_sample;
            if fmt.is_float && fmt.bits_per_sample == 32 && off + 4 <= bytes.len() {
                f32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
                    .clamp(-1.0, 1.0)
            } else if !fmt.is_float && fmt.bits_per_sample == 16 && off + 2 <= bytes.len() {
                f32::from(i16::from_le_bytes([bytes[off], bytes[off + 1]])) / 32768.0
            } else {
                0.0
            }
        };
        if fmt.channels == 1 {
            let v = sample_at(0);
            (v, v)
        } else {
            (sample_at(0), sample_at(1))
        }
    };

    let to_i16 = |v: f32| -> i16 { (v * 32767.0).clamp(-32768.0, 32767.0) as i16 };

    if fmt.sample_rate == AUDIO_SAMPLE_RATE {
        for i in 0..in_frames {
            let (l, r) = read_stereo_f32(i);
            out.push_back(to_i16(l));
            out.push_back(to_i16(r));
        }
        return;
    }

    let out_frames = ((in_frames as u64 * u64::from(AUDIO_SAMPLE_RATE))
        / u64::from(fmt.sample_rate))
    .max(1) as usize;
    let ratio = (fmt.sample_rate as f64) / f64::from(AUDIO_SAMPLE_RATE);
    for oi in 0..out_frames {
        let src_pos = (oi as f64) * ratio;
        let i0 = (src_pos.floor() as usize).min(in_frames - 1);
        let i1 = (i0 + 1).min(in_frames - 1);
        let frac = (src_pos - (i0 as f64)) as f32;
        let (l0, r0) = read_stereo_f32(i0);
        let (l1, r1) = read_stereo_f32(i1);
        out.push_back(to_i16(l0 + (l1 - l0) * frac));
        out.push_back(to_i16(r0 + (r1 - r0) * frac));
    }
}

/// Captura e mixa áudio do sistema (WASAPI loopback) e/ou microfone em blocos contínuos de 20 ms a 48 kHz estéreo.
/// Input: `want_loopback`, `want_mic`, canal `enc_tx`, `stop_flag` e `paused_flag` | Output: nenhum (`()`).
fn run_wasapi_audio_thread(
    want_loopback: bool,
    want_mic: bool,
    enc_tx: mpsc::SyncSender<EncoderPacket>,
    stop_flag: Arc<AtomicBool>,
    paused_flag: Arc<AtomicBool>,
) {
    let Ok(_winrt) = WinRtScope::enter_mta() else {
        return;
    };

    let mut loopback = if want_loopback {
        match WasapiCaptureStream::open(true) {
            Ok(s) => Some(s),
            Err(err) => {
                warn!(error = %err, "WASAPI loopback open failed");
                None
            }
        }
    } else {
        None
    };

    let mut mic = if want_mic {
        match WasapiCaptureStream::open(false) {
            Ok(s) => Some(s),
            Err(err) => {
                warn!(error = %err, "WASAPI microphone open failed");
                None
            }
        }
    } else {
        None
    };

    let samples_per_chunk = AUDIO_FRAMES_PER_CHUNK * usize::from(AUDIO_CHANNELS);
    let chunk_bytes = samples_per_chunk * 2;
    let chunk_interval = Duration::from_millis(20);
    let mut pts_hns: i64 = 0;
    let mut next_tick = Instant::now();

    while !stop_flag.load(Ordering::Acquire) {
        let is_paused = paused_flag.load(Ordering::Acquire);
        if let Some(s) = loopback.as_mut() {
            s.pull_into_queue(is_paused);
        }
        if let Some(s) = mic.as_mut() {
            s.pull_into_queue(is_paused);
        }

        if is_paused {
            next_tick = Instant::now() + chunk_interval;
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }

        let mut pcm = Vec::with_capacity(chunk_bytes);
        for _ in 0..samples_per_chunk {
            let l_sample = loopback
                .as_mut()
                .and_then(|s| s.pending.pop_front())
                .unwrap_or(0);
            let m_sample = mic
                .as_mut()
                .and_then(|s| s.pending.pop_front())
                .unwrap_or(0);
            let mixed = l_sample.saturating_add(m_sample);
            pcm.extend_from_slice(&mixed.to_le_bytes());
        }

        if enc_tx
            .send(EncoderPacket::Audio {
                pcm,
                pts_hns,
                dur_hns: AUDIO_DURATION_HNS,
            })
            .is_err()
        {
            break;
        }
        pts_hns += AUDIO_DURATION_HNS;

        next_tick += chunk_interval;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else if now.duration_since(next_tick) > chunk_interval * 5 {
            next_tick = now;
        }
    }
}
