//! Módulo: `recorder_linux`
//! Objetivos: Orquestrar gravação de tela contida à região ou janela no Linux via GStreamer (`pipewiresrc`/`ximagesrc` e `pulsesrc`), com supervisão `PR_SET_PDEATHSIG`, recorte RGB com preenchimento opaco `videobox` e concatenação de segmentos entre pausas.
//! Principais itens: `PreparedRecording`, `ActiveRecording`, `prepare_recording`, `probe_video_profile`, `build_segment_args` e `build_concat_args`.
//! Última alteração: 2026-11-20

use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::capture_wayland::{
    open_screencast_session_for, WaylandScreencastSession, WaylandSource, WaylandSourceBinding,
};
use crate::capture_x11::query_x11_window_size;
use crate::recorder::{
    compute_even_min_padding, compute_region_crop, next_temp_recording_path, segment_path,
    RecordingTarget, SessionLockFile, VideoProfile,
};

/// Parâmetros internos consolidados para instanciar segmentos do pipeline GStreamer no Linux.
/// Armazena: `is_wayland`, `target`, `profile`, `with_system_audio`, `with_mic`, `has_audio`, `x11_window_size`, `wayland_binding` | Principais métodos: nenhum (estrutura de dados).
#[derive(Debug, Clone)]
struct RecordingPipelineConfig {
    is_wayland: bool,
    target: RecordingTarget,
    profile: VideoProfile,
    with_system_audio: bool,
    with_mic: bool,
    has_audio: bool,
    x11_window_size: Option<(u32, u32)>,
    wayland_binding: Option<WaylandSourceBinding>,
}

/// Comando enviado à thread supervisora dedicada que possui o processo filho `gst-launch-1.0`.
/// Armazena: variantes `StopGraceful` e `ForceKill` | Principais métodos: nenhum (tipo de dados).
enum SupervisorCmd {
    StopGraceful(mpsc::Sender<Result<String, String>>),
    ForceKill,
}

/// Processo filho `gst-launch-1.0` supervisionado por thread OS dedicada vinculada com `PR_SET_PDEATHSIG(SIGKILL)`.
/// Armazena: canal `cmd_tx`, indicador atômico `exited` e handle da thread supervisora | Principais métodos: `has_exited`, `stop_graceful`.
struct SupervisedGstChild {
    cmd_tx: Option<mpsc::Sender<SupervisorCmd>>,
    exited: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SupervisedGstChild {
    /// Informa se o processo `gst-launch-1.0` encerrou inesperadamente por conta própria.
    /// Input: `&self` | Output: `bool` verdadeiro se o filho já terminou.
    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// Envia `SIGINT` via supervisor para emissão de EOS, aguarda encerramento e retorna o `stderr`.
    /// Input: `mut self` | Output: `Result<String, String>` com saída `stderr`.
    async fn stop_graceful(mut self) -> Result<String, String> {
        let Some(tx) = self.cmd_tx.take() else {
            return Ok(String::new());
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        if tx.send(SupervisorCmd::StopGraceful(reply_tx)).is_err() {
            if let Some(h) = self.thread.take() {
                let _ = h.join();
            }
            return Ok(String::new());
        }
        let handle = self.thread.take();
        tokio::task::spawn_blocking(move || {
            let res = reply_rx.recv().unwrap_or_else(|_| Ok(String::new()));
            if let Some(h) = handle {
                let _ = h.join();
            }
            res
        })
        .await
        .map_err(|e| format!("gst supervisor join panicked: {e}"))?
    }
}

impl Drop for SupervisedGstChild {
    /// Garante encerramento imediato da thread supervisora e do processo filho caso descartado sem `stop_graceful`.
    /// Input: `&mut self` | Output: nenhum (`()`).
    fn drop(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(SupervisorCmd::ForceKill);
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

/// Controlador de uma sessão ativa de gravação de tela com suporte a pausa e retomada em múltiplos segmentos.
/// Armazena: `config`, `final_output_path`, `segments`, `current_child`, `wayland_session`, `lock_file`, `accumulated_duration`, `segment_started_at`, `paused`, `last_stderr` | Principais métodos: `elapsed_secs`, `has_child_exited`, `pause`, `resume`, `stop`.
pub struct ActiveRecording {
    config: RecordingPipelineConfig,
    final_output_path: PathBuf,
    segments: Vec<PathBuf>,
    current_child: Option<SupervisedGstChild>,
    wayland_session: Option<WaylandScreencastSession>,
    lock_file: Option<SessionLockFile>,
    accumulated_duration: Duration,
    segment_started_at: Option<Instant>,
    paused: bool,
    last_stderr: String,
}

impl ActiveRecording {
    /// Verifica se o processo GStreamer encerrou inesperadamente enquanto a gravação estava ativa.
    /// Input: `&self` | Output: `bool` indicando queda inesperada do subprocesso.
    pub fn has_child_exited(&self) -> bool {
        !self.paused
            && self
                .current_child
                .as_ref()
                .is_some_and(SupervisedGstChild::has_exited)
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

    /// Pausa a gravação ativa finalizando de forma limpa o processo GStreamer do segmento corrente.
    /// Input: `&mut self` | Output: `Result<(), String>`.
    pub async fn pause(&mut self) -> Result<(), String> {
        if self.paused {
            return Ok(());
        }
        if let Some(child) = self.current_child.take() {
            let err = child.stop_graceful().await?;
            if !err.is_empty() {
                self.last_stderr = err;
            }
        }
        if let Some(started) = self.segment_started_at.take() {
            self.accumulated_duration += started.elapsed();
        }
        self.paused = true;
        info!(
            elapsed_secs = self.elapsed_secs(),
            "paused screen recording"
        );
        Ok(())
    }

    /// Retoma uma gravação pausada validando origem Wayland/X11 e iniciando novo segmento GStreamer.
    /// Input: `&mut self` | Output: `Result<(), String>`.
    pub async fn resume(&mut self) -> Result<(), String> {
        if !self.paused {
            return Ok(());
        }
        if let RecordingTarget::NativeWindow { handle } = self.config.target {
            let xid = u32::try_from(handle)
                .map_err(|_| format!("Identificador X11 inválido: {handle}"))?;
            let (w, h) = query_x11_window_size(xid).map_err(|e| e.to_string())?;
            self.config.x11_window_size = Some((w, h));
        }
        if self.config.is_wayland {
            let source = match self.config.target {
                RecordingTarget::Window => WaylandSource::Window,
                _ => WaylandSource::Monitor,
            };
            if let Some(session) = self.wayland_session.as_mut() {
                if let Err(err) = session.reopen_pipewire_remote().await {
                    warn!(error = %err, "reopen_pipewire_remote failed; opening fresh ScreenCast session");
                    let fresh = open_screencast_session_for(source)
                        .await
                        .map_err(|e| e.to_string())?;
                    if let Some(binding) = self.config.wayland_binding.as_ref() {
                        if let Err(verr) = binding.validate_session(&fresh) {
                            fresh.close().await;
                            return Err(verr);
                        }
                    }
                    let old = self.wayland_session.replace(fresh);
                    if let Some(old_session) = old {
                        old_session.close().await;
                    }
                }
            } else {
                let fresh = open_screencast_session_for(source)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(binding) = self.config.wayland_binding.as_ref() {
                    if let Err(verr) = binding.validate_session(&fresh) {
                        fresh.close().await;
                        return Err(verr);
                    }
                }
                self.wayland_session = Some(fresh);
            }
        }

        let seg_path = segment_path(
            &self.final_output_path,
            self.segments.len(),
            self.config.profile.ext(),
        );
        let child =
            spawn_gst_segment(&self.config, &seg_path, self.wayland_session.as_ref()).await?;
        self.segments.push(seg_path);
        self.current_child = Some(child);
        self.segment_started_at = Some(Instant::now());
        self.paused = false;
        info!(segments = self.segments.len(), "resumed screen recording");
        Ok(())
    }

    /// Encerra a gravação ativa, fecha a sessão Wayland, concatena segmentos se necessário e informa arquivo e duração.
    /// Input: `mut self` | Output: `Result<(PathBuf, u64), String>` com caminho temporário final e duração em segundos.
    pub async fn stop(mut self) -> Result<(PathBuf, u64), String> {
        if let Some(child) = self.current_child.take() {
            let err = child.stop_graceful().await?;
            if !err.is_empty() {
                self.last_stderr = err;
            }
        }
        if let Some(started) = self.segment_started_at.take() {
            self.accumulated_duration += started.elapsed();
        }
        let elapsed = self.accumulated_duration.as_secs();

        if let Some(session) = self.wayland_session.take() {
            session.close().await;
        }

        let mut valid_segments = Vec::new();
        for seg in &self.segments {
            let len = fs::metadata(seg).map(|m| m.len()).unwrap_or(0);
            if len > 0 {
                valid_segments.push(seg.clone());
            } else {
                let _ = fs::remove_file(seg);
            }
        }

        if valid_segments.is_empty() {
            if let Some(lock) = self.lock_file.take() {
                lock.release_and_remove();
            }
            let detail = self.last_stderr.trim();
            return Err(if detail.is_empty() {
                "Recording produced an empty video file".to_string()
            } else {
                format!("Recording failed (empty output): {detail}")
            });
        }

        let finalize_res = if valid_segments.len() == 1 {
            fs::rename(&valid_segments[0], &self.final_output_path)
                .map_err(|e| format!("failed to finalize recording file: {e}"))
        } else {
            let res = concat_segments(
                &valid_segments,
                &self.final_output_path,
                self.config.profile,
                self.config.has_audio,
            )
            .await;
            for seg in &valid_segments {
                let _ = fs::remove_file(seg);
            }
            res
        };

        if let Some(lock) = self.lock_file.take() {
            lock.release_and_remove();
        }
        finalize_res?;

        info!(path = %self.final_output_path.display(), elapsed_secs = elapsed, "recording finalized");
        Ok((self.final_output_path, elapsed))
    }
}

/// Envia `SIGINT` ao processo filho `gst-launch-1.0` para emissão de EOS com fallback para `SIGKILL`.
/// Input: `&mut Child` | Output: `String` com saída `stderr` coletada.
fn terminate_gst_child_graceful(child: &mut Child) -> String {
    let pid = child.id() as libc::pid_t;
    // SAFETY: sending SIGINT to our own spawned gst-launch-1.0 child pid so `-e` emits EOS.
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let start_wait = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start_wait.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                warn!("gst-launch-1.0 did not exit on SIGINT within 5s; killing");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(err) => {
                warn!(error = %err, "failed waiting for gst-launch-1.0");
                let _ = child.kill();
                break;
            }
        }
    }

    let mut err_text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err_text);
    }
    err_text
}

/// Verifica se um plugin GStreamer está instalado no sistema consultando `gst-inspect-1.0`.
/// Input: `name` (`&str`) | Output: `bool` (`true` se disponível).
fn gst_has_plugin(name: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Detecta em tempo de execução os encoders e multiplexadores GStreamer disponíveis (com fallback para WebM).
/// Input: nenhum | Output: `Result<VideoProfile, String>`.
pub fn probe_video_profile() -> Result<VideoProfile, String> {
    if !gst_has_plugin("pipewiresrc") && !gst_has_plugin("ximagesrc") {
        return Err(
            "GStreamer screen capture plugins missing (install gstreamer1.0-tools and gstreamer1.0-pipewire)"
                .to_string(),
        );
    }
    if gst_has_plugin("x264enc") && gst_has_plugin("mp4mux") {
        Ok(VideoProfile::Mp4H264)
    } else if gst_has_plugin("vp8enc") && gst_has_plugin("webmmux") {
        Ok(VideoProfile::WebmVp8)
    } else if gst_has_plugin("vp9enc") && gst_has_plugin("webmmux") {
        Ok(VideoProfile::WebmVp9)
    } else {
        Err(
            "No supported GStreamer video encoder found (need x264enc+mp4mux or vp8enc/vp9enc+webmmux)"
                .to_string(),
        )
    }
}

/// Monta a linha de argumentos do `gst-launch-1.0` garantindo contenção de origem, recorte em RGB e padding opaco pós-recorte.
/// Input: parâmetros de pipeline (`is_wayland`, `target`, `profile`, áudio, tamanho X11, fd Wayland e destino) | Output: `Result<Vec<String>, String>`.
#[allow(clippy::too_many_arguments)]
pub fn build_segment_args(
    is_wayland: bool,
    target: RecordingTarget,
    profile: VideoProfile,
    has_audio: bool,
    with_system_audio: bool,
    with_mic: bool,
    x11_window_size: Option<(u32, u32)>,
    wayland_pw: Option<(i32, u32)>,
    output_path: &Path,
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = vec!["-e".to_string()];

    if is_wayland {
        if matches!(target, RecordingTarget::NativeWindow { .. }) {
            return Err("NativeWindow target cannot be recorded on Wayland".to_string());
        }
        let (raw_fd, node_id) =
            wayland_pw.ok_or_else(|| "Wayland ScreenCast session is missing".to_string())?;
        args.extend([
            "pipewiresrc".to_string(),
            format!("fd={raw_fd}"),
            format!("path={node_id}"),
            "do-timestamp=true".to_string(),
            "keepalive-time=1000".to_string(),
            "resend-last=true".to_string(),
        ]);
    } else {
        match target {
            RecordingTarget::NativeWindow { handle } => {
                let xid = u32::try_from(handle)
                    .map_err(|_| format!("Identificador X11 inválido: {handle}"))?;
                args.extend([
                    "ximagesrc".to_string(),
                    format!("xid={xid}"),
                    "use-damage=false".to_string(),
                ]);
            }
            RecordingTarget::Window => {
                return Err(
                    "Modo Janela genérico sem seleção é inválido no X11; selecione uma janela."
                        .to_string(),
                );
            }
            RecordingTarget::Fullscreen | RecordingTarget::Region { .. } => {
                args.extend(["ximagesrc".to_string(), "use-damage=false".to_string()]);
            }
        }
    }

    match target {
        RecordingTarget::Region {
            rect,
            full_width,
            full_height,
        } => {
            let plan = compute_region_crop(rect, full_width, full_height)?;
            args.extend([
                "!".to_string(),
                "videoconvert".to_string(),
                "!".to_string(),
                format!("video/x-raw,format=BGRx,width={full_width},height={full_height}"),
            ]);
            if plan.top > 0 || plan.bottom > 0 || plan.left > 0 || plan.right > 0 {
                args.extend([
                    "!".to_string(),
                    "videocrop".to_string(),
                    format!("top={}", plan.top),
                    format!("bottom={}", plan.bottom),
                    format!("left={}", plan.left),
                    format!("right={}", plan.right),
                ]);
            }
            if plan.pad_right > 0 || plan.pad_bottom > 0 {
                args.extend([
                    "!".to_string(),
                    "videobox".to_string(),
                    "top=0".to_string(),
                    format!("bottom=-{}", plan.pad_bottom),
                    "left=0".to_string(),
                    format!("right=-{}", plan.pad_right),
                    "fill=black".to_string(),
                ]);
            }
            args.extend([
                "!".to_string(),
                "videoconvert".to_string(),
                "!".to_string(),
                "video/x-raw,format=I420".to_string(),
            ]);
        }
        RecordingTarget::NativeWindow { .. } => {
            args.extend([
                "!".to_string(),
                "videoconvert".to_string(),
                "!".to_string(),
                "video/x-raw,format=BGRx".to_string(),
            ]);
            if let Some((w, h)) = x11_window_size {
                let (pad_right, pad_bottom) = compute_even_min_padding(w, h);
                if pad_right > 0 || pad_bottom > 0 {
                    args.extend([
                        "!".to_string(),
                        "videobox".to_string(),
                        "top=0".to_string(),
                        format!("bottom=-{pad_bottom}"),
                        "left=0".to_string(),
                        format!("right=-{pad_right}"),
                        "fill=black".to_string(),
                    ]);
                }
            }
            args.extend([
                "!".to_string(),
                "videoconvert".to_string(),
                "!".to_string(),
                "video/x-raw,format=I420".to_string(),
            ]);
        }
        RecordingTarget::Fullscreen | RecordingTarget::Window => {
            args.extend(["!".to_string(), "videoconvert".to_string()]);
        }
    }

    match profile {
        VideoProfile::Mp4H264 => {
            args.extend([
                "!".to_string(),
                "x264enc".to_string(),
                "tune=zerolatency".to_string(),
                "speed-preset=veryfast".to_string(),
                "pass=quant".to_string(),
                "quantizer=22".to_string(),
                "key-int-max=60".to_string(),
                "!".to_string(),
                "video/x-h264,profile=main".to_string(),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "mp4mux".to_string(),
                "name=mux".to_string(),
            ]);
        }
        VideoProfile::WebmVp8 => {
            args.extend([
                "!".to_string(),
                "vp8enc".to_string(),
                "deadline=1".to_string(),
                "threads=4".to_string(),
                "target-bitrate=8000000".to_string(),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "webmmux".to_string(),
                "name=mux".to_string(),
            ]);
        }
        VideoProfile::WebmVp9 => {
            args.extend([
                "!".to_string(),
                "vp9enc".to_string(),
                "deadline=1".to_string(),
                "threads=4".to_string(),
                "target-bitrate=8000000".to_string(),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "webmmux".to_string(),
                "name=mux".to_string(),
            ]);
        }
    }

    args.extend([
        "!".to_string(),
        "filesink".to_string(),
        format!("location={}", output_path.display()),
    ]);

    if has_audio {
        if with_system_audio && with_mic && gst_has_plugin("audiomixer") {
            args.extend([
                "pulsesrc".to_string(),
                "device=@DEFAULT_MONITOR@".to_string(),
                "!".to_string(),
                "audioconvert".to_string(),
                "!".to_string(),
                "audioresample".to_string(),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "mix.".to_string(),
                "pulsesrc".to_string(),
                "device=@DEFAULT_SOURCE@".to_string(),
                "!".to_string(),
                "audioconvert".to_string(),
                "!".to_string(),
                "audioresample".to_string(),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "mix.".to_string(),
                "audiomixer".to_string(),
                "name=mix".to_string(),
                "!".to_string(),
                "audioconvert".to_string(),
                "!".to_string(),
                "audioresample".to_string(),
                "!".to_string(),
                "opusenc".to_string(),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "mux.".to_string(),
            ]);
        } else {
            let device = if with_system_audio {
                "@DEFAULT_MONITOR@"
            } else {
                "@DEFAULT_SOURCE@"
            };
            args.extend([
                "pulsesrc".to_string(),
                format!("device={device}"),
                "!".to_string(),
                "audioconvert".to_string(),
                "!".to_string(),
                "audioresample".to_string(),
                "!".to_string(),
                "opusenc".to_string(),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "mux.".to_string(),
            ]);
        }
    }

    Ok(args)
}

/// Inicia um processo filho `gst-launch-1.0 -e` supervisionado por thread OS dedicada com `PR_SET_PDEATHSIG`.
/// Input: `config`, `output_path` e `wayland_session` | Output: `Result<SupervisedGstChild, String>`.
async fn spawn_gst_segment(
    config: &RecordingPipelineConfig,
    output_path: &Path,
    wayland_session: Option<&WaylandScreencastSession>,
) -> Result<SupervisedGstChild, String> {
    let wayland_pw = wayland_session.map(|s| (s.pw_fd.as_raw_fd(), s.node_id));
    let args = build_segment_args(
        config.is_wayland,
        config.target,
        config.profile,
        config.has_audio,
        config.with_system_audio,
        config.with_mic,
        config.x11_window_size,
        wayland_pw,
        output_path,
    )?;

    let raw_pw_fd = wayland_pw.map(|(fd, _)| fd);
    let expected_ppid = unsafe { libc::getpid() };
    let exited = Arc::new(AtomicBool::new(false));
    let exited_worker = Arc::clone(&exited);
    let (cmd_tx, cmd_rx) = mpsc::channel::<SupervisorCmd>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let out_path_owned = output_path.to_path_buf();

    let handle = std::thread::Builder::new()
        .name("gst-supervisor".to_string())
        .spawn(move || {
            let mut cmd = Command::new("gst-launch-1.0");
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());

            // SAFETY: configura PR_SET_PDEATHSIG(SIGKILL) no filho e valida PPID esperado antes do exec,
            // além de limpar FD_CLOEXEC do descritor PipeWire quando presente.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != expected_ppid {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "parent process exited before exec",
                        ));
                    }
                    if let Some(pw_fd) = raw_pw_fd {
                        let flags = libc::fcntl(pw_fd, libc::F_GETFD);
                        if flags >= 0 {
                            libc::fcntl(pw_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                        }
                    }
                    Ok(())
                });
            }

            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(e) => {
                    exited_worker.store(true, Ordering::Release);
                    let _ = ready_tx.send(Err(format!("failed to launch gst-launch-1.0: {e}")));
                    return;
                }
            };

            std::thread::sleep(Duration::from_millis(200));
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited_worker.store(true, Ordering::Release);
                    let mut err_text = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        let _ = stderr.read_to_string(&mut err_text);
                    }
                    let _ = fs::remove_file(&out_path_owned);
                    let detail = err_text.trim();
                    let msg = if detail.is_empty() {
                        format!("gst-launch-1.0 exited early ({status})")
                    } else {
                        format!("gst-launch-1.0 exited early ({status}): {detail}")
                    };
                    let _ = ready_tx.send(Err(msg));
                    return;
                }
                Ok(None) => {
                    let _ = ready_tx.send(Ok(()));
                }
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    exited_worker.store(true, Ordering::Release);
                    let _ = fs::remove_file(&out_path_owned);
                    let _ =
                        ready_tx.send(Err(format!("failed checking gst-launch-1.0 status: {err}")));
                    return;
                }
            }

            let mut cached_stderr: Option<String> = None;
            loop {
                match cmd_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(SupervisorCmd::StopGraceful(reply_tx)) => {
                        let err_text = if let Some(s) = cached_stderr.take() {
                            s
                        } else {
                            terminate_gst_child_graceful(&mut child)
                        };
                        exited_worker.store(true, Ordering::Release);
                        let _ = reply_tx.send(Ok(err_text));
                        break;
                    }
                    Ok(SupervisorCmd::ForceKill) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if cached_stderr.is_none() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        exited_worker.store(true, Ordering::Release);
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if cached_stderr.is_none() {
                            if let Ok(Some(_)) = child.try_wait() {
                                let mut s = String::new();
                                if let Some(mut stderr) = child.stderr.take() {
                                    let _ = stderr.read_to_string(&mut s);
                                }
                                cached_stderr = Some(s);
                                exited_worker.store(true, Ordering::Release);
                            }
                        }
                    }
                }
            }
        })
        .map_err(|e| format!("failed to spawn gst supervisor thread: {e}"))?;

    let ready_res = tokio::task::spawn_blocking(move || ready_rx.recv())
        .await
        .map_err(|e| format!("gst supervisor readiness join error: {e}"))?
        .map_err(|_| "gst supervisor terminated unexpectedly".to_string())?;

    if let Err(err) = ready_res {
        let _ = handle.join();
        return Err(err);
    }

    Ok(SupervisedGstChild {
        cmd_tx: Some(cmd_tx),
        exited,
        thread: Some(handle),
    })
}

/// Monta os argumentos do `gst-launch-1.0` para concatenar segmentos gravados entre pausas sem recodificação.
/// Input: `segments` (`&[PathBuf]`), `output_path` (`&Path`), `profile` (`VideoProfile`), `has_audio` (`bool`) | Output: `Vec<String>` com a linha de pipeline.
pub fn build_concat_args(
    segments: &[PathBuf],
    output_path: &Path,
    profile: VideoProfile,
    has_audio: bool,
) -> Vec<String> {
    let (demux, mux) = match profile {
        VideoProfile::Mp4H264 => ("qtdemux", "mp4mux"),
        VideoProfile::WebmVp8 | VideoProfile::WebmVp9 => ("matroskademux", "webmmux"),
    };

    let mut args = vec![
        "-e".to_string(),
        "concat".to_string(),
        "name=vcat".to_string(),
        "!".to_string(),
    ];
    if profile == VideoProfile::Mp4H264 {
        args.extend(["h264parse".to_string(), "!".to_string()]);
    }
    args.extend([
        "queue".to_string(),
        "!".to_string(),
        mux.to_string(),
        "name=mux".to_string(),
        "!".to_string(),
        "filesink".to_string(),
        format!("location={}", output_path.display()),
    ]);

    if has_audio {
        args.extend([
            "concat".to_string(),
            "name=acat".to_string(),
            "!".to_string(),
            "opusparse".to_string(),
            "!".to_string(),
            "queue".to_string(),
            "!".to_string(),
            "mux.".to_string(),
        ]);
    }

    for (idx, seg) in segments.iter().enumerate() {
        let d = format!("d{idx}");
        args.extend([
            "filesrc".to_string(),
            format!("location={}", seg.display()),
            "!".to_string(),
            demux.to_string(),
            format!("name={d}"),
            format!("{d}.video_0"),
            "!".to_string(),
            "queue".to_string(),
            "!".to_string(),
            "vcat.".to_string(),
        ]);
        if has_audio {
            args.extend([
                format!("{d}.audio_0"),
                "!".to_string(),
                "queue".to_string(),
                "!".to_string(),
                "acat.".to_string(),
            ]);
        }
    }

    args
}

/// Executa `gst-launch-1.0` para mesclar múltiplos arquivos de segmento em um único vídeo final.
/// Input: `segments` (`&[PathBuf]`), `output_path` (`&Path`), `profile` (`VideoProfile`), `has_audio` (`bool`) | Output: `Result<(), String>`.
async fn concat_segments(
    segments: &[PathBuf],
    output_path: &Path,
    profile: VideoProfile,
    has_audio: bool,
) -> Result<(), String> {
    let args = build_concat_args(segments, output_path, profile, has_audio);
    let output = tokio::process::Command::new("gst-launch-1.0")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to launch gst-launch-1.0 concat: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "failed to merge recording segments ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(())
}

/// Sessão de gravação já preparada e autorizada no Linux, aguardando a contagem regressiva para iniciar o GStreamer.
/// Armazena: `config`, `final_output_path`, `wayland_session` e `lock_file` | Principais métodos: `start`, `cancel`.
pub struct PreparedRecording {
    config: RecordingPipelineConfig,
    final_output_path: PathBuf,
    wayland_session: Option<WaylandScreencastSession>,
    lock_file: Option<SessionLockFile>,
}

impl PreparedRecording {
    /// Inicia efetivamente o processo `gst-launch-1.0` do primeiro segmento após a contagem regressiva.
    /// Input: `mut self` | Output: `Result<ActiveRecording, String>`.
    pub async fn start(mut self) -> Result<ActiveRecording, String> {
        let seg0 = segment_path(&self.final_output_path, 0, self.config.profile.ext());
        let child =
            match spawn_gst_segment(&self.config, &seg0, self.wayland_session.as_ref()).await {
                Ok(child) => child,
                Err(err) => {
                    if let Some(session) = self.wayland_session.take() {
                        session.close().await;
                    }
                    if let Some(lock) = self.lock_file.take() {
                        lock.release_and_remove();
                    }
                    return Err(err);
                }
            };

        info!(
            path = %self.final_output_path.display(),
            profile = ?self.config.profile,
            target = ?self.config.target,
            "started screen recording"
        );
        Ok(ActiveRecording {
            config: self.config,
            final_output_path: self.final_output_path,
            segments: vec![seg0],
            current_child: Some(child),
            wayland_session: self.wayland_session,
            lock_file: self.lock_file,
            accumulated_duration: Duration::ZERO,
            segment_started_at: Some(Instant::now()),
            paused: false,
            last_stderr: String::new(),
        })
    }

    /// Cancela a gravação preparada antes do início do GStreamer e fecha a sessão Wayland, se existir.
    /// Input: `mut self` | Output: nenhum (`()`).
    pub async fn cancel(mut self) {
        if let Some(session) = self.wayland_session.take() {
            session.close().await;
        }
        if let Some(lock) = self.lock_file.take() {
            lock.release_and_remove();
        }
    }
}

/// Prepara perfil, caminho temporário, vínculo de origem e sessão Wayland/X11 de gravação sem disparar o pipeline ainda.
/// Input: `is_wayland` (`bool`), `target` (`RecordingTarget`), `with_system_audio` (`bool`), `with_mic` (`bool`), `wayland_binding` (`Option<&WaylandSourceBinding>`) | Output: `Result<PreparedRecording, String>`.
pub async fn prepare_recording(
    is_wayland: bool,
    target: RecordingTarget,
    with_system_audio: bool,
    with_mic: bool,
    wayland_binding: Option<&WaylandSourceBinding>,
) -> Result<PreparedRecording, String> {
    let mut x11_window_size = None;
    let mut stored_binding = None;

    match target {
        RecordingTarget::Region {
            rect,
            full_width,
            full_height,
        } => {
            compute_region_crop(rect, full_width, full_height)?;
            if is_wayland {
                let binding = wayland_binding.ok_or_else(|| {
                    "Vínculo da fonte Wayland ausente para gravação de região.".to_string()
                })?;
                if binding.pixel_width != full_width || binding.pixel_height != full_height {
                    return Err(format!(
                        "Dimensões da região ({full_width}x{full_height}) divergem do stream negociado ({}x{}).",
                        binding.pixel_width, binding.pixel_height
                    ));
                }
                stored_binding = Some(binding.clone());
            }
        }
        RecordingTarget::NativeWindow { handle } => {
            if is_wayland {
                return Err("Alvo NativeWindow não é compatível com sessão Wayland.".to_string());
            }
            let xid = u32::try_from(handle)
                .map_err(|_| format!("Identificador X11 inválido: {handle}"))?;
            let size = query_x11_window_size(xid).map_err(|e| e.to_string())?;
            x11_window_size = Some(size);
        }
        RecordingTarget::Window if !is_wayland => {
            return Err("Selecione uma janela X11 específica antes de gravar.".to_string());
        }
        RecordingTarget::Fullscreen | RecordingTarget::Window => {}
    }

    let profile = probe_video_profile()?;
    let (final_output_path, lock_file) = next_temp_recording_path(profile.ext())?;

    let wayland_session = if is_wayland {
        if !gst_has_plugin("pipewiresrc") {
            lock_file.release_and_remove();
            return Err("GStreamer pipewiresrc plugin is missing".to_string());
        }
        let source = match target {
            RecordingTarget::Window => WaylandSource::Window,
            _ => WaylandSource::Monitor,
        };
        let session = match open_screencast_session_for(source).await {
            Ok(s) => s,
            Err(e) => {
                lock_file.release_and_remove();
                return Err(e.to_string());
            }
        };
        if let Some(binding) = stored_binding.as_ref() {
            if let Err(err) = binding.validate_session(&session) {
                session.close().await;
                lock_file.release_and_remove();
                return Err(err);
            }
        }
        Some(session)
    } else {
        if !gst_has_plugin("ximagesrc") {
            lock_file.release_and_remove();
            return Err("GStreamer ximagesrc plugin is missing".to_string());
        }
        None
    };

    let has_audio =
        (with_system_audio || with_mic) && gst_has_plugin("pulsesrc") && gst_has_plugin("opusenc");

    let config = RecordingPipelineConfig {
        is_wayland,
        target,
        profile,
        with_system_audio,
        with_mic,
        has_audio,
        x11_window_size,
        wayland_binding: stored_binding,
    };

    Ok(PreparedRecording {
        config,
        final_output_path,
        wayland_session,
        lock_file: Some(lock_file),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lx_snip_core::scene::Rect;
    use uuid::Uuid;

    #[test]
    fn build_segment_args_crops_in_rgb_then_pads_before_i420_and_passes_x11_xid() {
        let region_args = build_segment_args(
            false,
            RecordingTarget::Region {
                rect: Rect::new(11.0, 21.0, 7.0, 19.0),
                full_width: 200,
                full_height: 100,
            },
            VideoProfile::Mp4H264,
            false,
            false,
            false,
            None,
            None,
            Path::new("/tmp/out.mp4"),
        )
        .expect("region pipeline args");

        let caps_idx = region_args
            .iter()
            .position(|a| a == "video/x-raw,format=BGRx,width=200,height=100")
            .expect("RGB BGRx caps with exact full size");
        let crop_idx = region_args
            .iter()
            .position(|a| a == "videocrop")
            .expect("videocrop element");
        let box_idx = region_args
            .iter()
            .position(|a| a == "videobox")
            .expect("videobox padding element");
        let i420_idx = region_args
            .iter()
            .position(|a| a == "video/x-raw,format=I420")
            .expect("I420 caps after videobox");
        assert!(caps_idx < crop_idx && crop_idx < box_idx && box_idx < i420_idx);
        assert!(region_args.contains(&"right=-9".to_string()));
        assert!(region_args.contains(&"bottom=-1".to_string()));
        assert!(region_args.contains(&"fill=black".to_string()));

        // NativeWindow on X11 must pass explicit xid=<id> and reject generic Window without id
        let win_args = build_segment_args(
            false,
            RecordingTarget::NativeWindow {
                handle: 0x02A0_0007,
            },
            VideoProfile::Mp4H264,
            false,
            false,
            false,
            Some((101, 50)),
            None,
            Path::new("/tmp/win.mp4"),
        )
        .expect("X11 window args");
        assert!(win_args.contains(&format!("xid={}", 0x02A0_0007_u32)));
        assert!(win_args.contains(&"videobox".to_string()));
        assert!(win_args.contains(&"right=-1".to_string()));

        assert!(build_segment_args(
            false,
            RecordingTarget::Window,
            VideoProfile::Mp4H264,
            false,
            false,
            false,
            None,
            None,
            Path::new("/tmp/bad.mp4"),
        )
        .is_err());
    }

    #[test]
    fn pdeathsig_terminates_child_when_parent_process_is_killed() {
        let pid_file = std::env::temp_dir().join(format!("lx_snip_pdeath_{}", Uuid::new_v4()));
        let py_script = r#"
import ctypes, os, signal, subprocess, sys, time
libc = ctypes.CDLL("libc.so.6")
PR_SET_PDEATHSIG = 1
expected_ppid = os.getpid()
def preexec():
    libc.prctl(PR_SET_PDEATHSIG, signal.SIGKILL)
    if os.getppid() != expected_ppid:
        os._exit(1)
child = subprocess.Popen(["sleep", "30"], preexec_fn=preexec)
with open(sys.argv[1], "w") as f:
    f.write(str(child.pid))
time.sleep(30)
"#;
        let mut parent = Command::new("python3")
            .args(["-c", py_script, pid_file.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 available for pdeathsig test");

        let mut child_pid_opt = None;
        for _ in 0..40 {
            if let Ok(text) = fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<libc::pid_t>() {
                    child_pid_opt = Some(pid);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let child_pid = child_pid_opt.expect("grandchild pid should be recorded");
        assert!(
            Path::new(&format!("/proc/{child_pid}")).exists(),
            "grandchild must be alive before parent SIGKILL"
        );

        let _ = parent.kill();
        let _ = parent.wait();

        let mut exited = false;
        for _ in 0..40 {
            if !Path::new(&format!("/proc/{child_pid}")).exists() {
                exited = true;
                break;
            }
            if let Ok(stat) = fs::read_to_string(format!("/proc/{child_pid}/stat")) {
                if stat.contains(") Z ") {
                    exited = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = fs::remove_file(&pid_file);
        if !exited {
            unsafe { libc::kill(child_pid, libc::SIGKILL) };
        }
        assert!(
            exited,
            "PR_SET_PDEATHSIG must terminate child when parent dies abruptly"
        );
    }

    #[test]
    fn concat_args_mp4_with_and_without_audio() {
        let segs = vec![PathBuf::from("/tmp/p0.mp4"), PathBuf::from("/tmp/p1.mp4")];
        let out = PathBuf::from("/tmp/final.mp4");

        let with_audio = build_concat_args(&segs, &out, VideoProfile::Mp4H264, true);
        assert!(with_audio.contains(&"h264parse".to_string()));
        assert!(with_audio.contains(&"qtdemux".to_string()));
        assert!(with_audio.contains(&"opusparse".to_string()));
        assert!(with_audio.contains(&"d0.audio_0".to_string()));
        assert!(with_audio.contains(&"d1.audio_0".to_string()));

        let video_only = build_concat_args(&segs, &out, VideoProfile::Mp4H264, false);
        assert!(video_only.contains(&"h264parse".to_string()));
        assert!(!video_only.contains(&"opusparse".to_string()));
        assert!(!video_only.contains(&"d0.audio_0".to_string()));
    }

    #[test]
    fn concat_args_webm_with_audio() {
        let segs = vec![PathBuf::from("/tmp/w0.webm"), PathBuf::from("/tmp/w1.webm")];
        let out = PathBuf::from("/tmp/final.webm");
        let args = build_concat_args(&segs, &out, VideoProfile::WebmVp8, true);
        assert!(args.contains(&"matroskademux".to_string()));
        assert!(args.contains(&"webmmux".to_string()));
        assert!(!args.contains(&"h264parse".to_string()));
        assert!(args.contains(&"opusparse".to_string()));
    }
}
