//! Phase 2 GStreamer screen recorder (`gst-launch-1.0 -e`) using PipeWire `ScreenCast`
//! on Wayland (with `restore_token`) and `ximagesrc` on X11, supporting Pause/Resume
//! via lossless segment concatenation.

use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use directories::{ProjectDirs, UserDirs};
use lx_snip_core::scene::Rect;
use tracing::{info, warn};
use uuid::Uuid;

use crate::capture_wayland::{
    open_screencast_session_for, WaylandScreencastSession, WaylandSource,
};

/// Recording target selected in Level 2 of the capture overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingTarget {
    /// Entire desktop / default monitor.
    Fullscreen,
    /// User-drawn rectangle (or selected X11 window bounds).
    Region {
        /// Crop rectangle in full-desktop image pixels.
        rect: Rect,
        /// Full desktop width in pixels.
        full_width: u32,
        /// Full desktop height in pixels.
        full_height: u32,
    },
    /// Wayland portal-selected single window.
    Window,
}

/// Detected GStreamer video/audio encoder & container profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoProfile {
    /// H.264 (`x264enc`) + Opus in MP4 (`mp4mux`).
    Mp4H264,
    /// Royalty-free VP8 (`vp8enc`) + Opus in WebM (`webmmux`).
    WebmVp8,
    /// Royalty-free VP9 (`vp9enc`) + Opus in WebM (`webmmux`).
    WebmVp9,
}

impl VideoProfile {
    /// Output file extension.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4H264 => "mp4",
            Self::WebmVp8 | Self::WebmVp9 => "webm",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RecordingPipelineConfig {
    is_wayland: bool,
    target: RecordingTarget,
    profile: VideoProfile,
    with_system_audio: bool,
    with_mic: bool,
    has_audio: bool,
}

/// Active screen recording process handle with Pause/Resume support.
pub struct ActiveRecording {
    config: RecordingPipelineConfig,
    final_output_path: PathBuf,
    segments: Vec<PathBuf>,
    current_child: Option<Child>,
    wayland_session: Option<WaylandScreencastSession>,
    accumulated_duration: Duration,
    segment_started_at: Option<Instant>,
    paused: bool,
    last_stderr: String,
}

impl ActiveRecording {
    /// Elapsed active recording time in seconds (excluding paused intervals).
    pub fn elapsed_secs(&self) -> u64 {
        let active = self
            .segment_started_at
            .map(|t| t.elapsed())
            .unwrap_or_default();
        (self.accumulated_duration + active).as_secs()
    }

    /// Pauses an active recording by cleanly finalizing the current GStreamer segment.
    pub async fn pause(&mut self) -> Result<(), String> {
        if self.paused {
            return Ok(());
        }
        if let Some(child) = self.current_child.take() {
            let err = stop_gst_child(child).await?;
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

    /// Resumes a paused recording by starting a new GStreamer segment and refreshing the PipeWire fd.
    pub async fn resume(&mut self) -> Result<(), String> {
        if !self.paused {
            return Ok(());
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
                    let old = self.wayland_session.replace(fresh);
                    if let Some(old_session) = old {
                        old_session.close().await;
                    }
                }
            } else {
                let fresh = open_screencast_session_for(source)
                    .await
                    .map_err(|e| e.to_string())?;
                self.wayland_session = Some(fresh);
            }
        }

        let seg_path = segment_path(
            &self.final_output_path,
            self.segments.len(),
            self.config.profile.ext(),
        );
        let child =
            match spawn_gst_segment(&self.config, &seg_path, self.wayland_session.as_ref()).await {
                Ok(child) => child,
                Err(err) => {
                    return Err(err);
                }
            };
        self.segments.push(seg_path);
        self.current_child = Some(child);
        self.segment_started_at = Some(Instant::now());
        self.paused = false;
        info!(segments = self.segments.len(), "resumed screen recording");
        Ok(())
    }

    /// Stops recording, closes the Wayland session, concatenates segments if paused/resumed,
    /// and returns `(final_path, elapsed_secs)`.
    pub async fn stop(mut self) -> Result<(PathBuf, u64), String> {
        if let Some(child) = self.current_child.take() {
            let err = stop_gst_child(child).await?;
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
            let detail = self.last_stderr.trim();
            return Err(if detail.is_empty() {
                "Recording produced an empty video file".to_string()
            } else {
                format!("Recording failed (empty output): {detail}")
            });
        }

        if valid_segments.len() == 1 {
            fs::rename(&valid_segments[0], &self.final_output_path)
                .map_err(|e| format!("failed to finalize recording file: {e}"))?;
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
            res?;
        }

        info!(path = %self.final_output_path.display(), elapsed_secs = elapsed, "recording finalized");
        Ok((self.final_output_path, elapsed))
    }
}

async fn stop_gst_child(mut child: Child) -> Result<String, String> {
    let pid = child.id() as libc::pid_t;
    // SAFETY: sending SIGINT to our own spawned gst-launch-1.0 child pid so `-e` emits EOS.
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let start_wait = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if start_wait.elapsed() < Duration::from_secs(5) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(None) => {
                warn!("gst-launch-1.0 did not exit on SIGINT within 5s; killing");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(err) => return Err(format!("failed waiting for gst-launch-1.0: {err}")),
        }
    }

    let mut err_text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err_text);
    }
    Ok(err_text)
}

fn gst_has_plugin(name: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Probes available GStreamer encoders at runtime (royalty-free WebM fallback if H.264 is absent).
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

fn next_recording_path(ext: &str) -> Result<PathBuf, String> {
    let (dir, restricted) = if let Some(video_dir) =
        UserDirs::new().and_then(|u| u.video_dir().map(|d| d.join("SnippingTool")))
    {
        (video_dir, false)
    } else {
        let proj = ProjectDirs::from("", "", "lx-snippingtool")
            .ok_or_else(|| "unable to resolve XDG data dir".to_string())?;
        (proj.data_dir().join("recordings"), true)
    };
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if restricted {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(dir.join(format!("rec_{ms}_{}.{ext}", Uuid::new_v4())))
}

fn segment_path(final_path: &Path, index: usize, ext: &str) -> PathBuf {
    let stem = final_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rec");
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{stem}.part{index}.{ext}"))
}

/// Computes `(top, bottom, left, right)` even-aligned `videocrop` margins for GStreamer.
pub fn compute_videocrop(rect: Rect, full_w: u32, full_h: u32) -> (u32, u32, u32, u32) {
    let r = rect.normalized().clamp(full_w as f32, full_h as f32);
    let mut left = (r.x.round() as u32) & !1;
    let mut top = (r.y.round() as u32) & !1;
    let mut w = (r.width.round() as u32).max(16) & !1;
    let mut h = (r.height.round() as u32).max(16) & !1;
    if left + w > full_w {
        w = (full_w.saturating_sub(left)) & !1;
    }
    if top + h > full_h {
        h = (full_h.saturating_sub(top)) & !1;
    }
    if w < 16 {
        left = 0;
        w = full_w & !1;
    }
    if h < 16 {
        top = 0;
        h = full_h & !1;
    }
    let right = full_w.saturating_sub(left + w);
    let bottom = full_h.saturating_sub(top + h);
    (top, bottom, left, right)
}

async fn spawn_gst_segment(
    config: &RecordingPipelineConfig,
    output_path: &Path,
    wayland_session: Option<&WaylandScreencastSession>,
) -> Result<Child, String> {
    let mut args: Vec<String> = vec!["-e".to_string()];

    if config.is_wayland {
        let session =
            wayland_session.ok_or_else(|| "Wayland ScreenCast session is missing".to_string())?;
        let raw_fd = session.pw_fd.as_raw_fd();
        let node_id = session.node_id;

        args.extend([
            "pipewiresrc".to_string(),
            format!("fd={raw_fd}"),
            format!("path={node_id}"),
            "do-timestamp=true".to_string(),
            "keepalive-time=1000".to_string(),
            "resend-last=true".to_string(),
        ]);
    } else {
        args.extend(["ximagesrc".to_string(), "use-damage=false".to_string()]);
    }

    args.extend(["!".to_string(), "videoconvert".to_string()]);

    if let RecordingTarget::Region {
        rect,
        full_width,
        full_height,
    } = config.target
    {
        let (top, bottom, left, right) = compute_videocrop(rect, full_width, full_height);
        if top > 0 || bottom > 0 || left > 0 || right > 0 {
            args.extend([
                "!".to_string(),
                "videocrop".to_string(),
                format!("top={top}"),
                format!("bottom={bottom}"),
                format!("left={left}"),
                format!("right={right}"),
            ]);
        }
        args.extend(["!".to_string(), "videoconvert".to_string()]);
    }

    match config.profile {
        VideoProfile::Mp4H264 => {
            args.extend([
                "!".to_string(),
                "x264enc".to_string(),
                "tune=zerolatency".to_string(),
                "speed-preset=veryfast".to_string(),
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

    if config.has_audio {
        if config.with_system_audio && config.with_mic && gst_has_plugin("audiomixer") {
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
            let device = if config.with_system_audio {
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

    let mut cmd = Command::new("gst-launch-1.0");
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    if let Some(session) = wayland_session {
        let raw_fd = session.pw_fd.as_raw_fd();
        // SAFETY: pre_exec clears FD_CLOEXEC on the PipeWire fd for the child gst-launch-1.0 process.
        unsafe {
            cmd.pre_exec(move || {
                let flags = libc::fcntl(raw_fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(raw_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                }
                Ok(())
            });
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch gst-launch-1.0: {e}"))?;

    tokio::time::sleep(Duration::from_millis(200)).await;
    match child.try_wait() {
        Ok(Some(status)) => {
            let mut err_text = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err_text);
            }
            let _ = fs::remove_file(output_path);
            let detail = err_text.trim();
            Err(if detail.is_empty() {
                format!("gst-launch-1.0 exited early ({status})")
            } else {
                format!("gst-launch-1.0 exited early ({status}): {detail}")
            })
        }
        Ok(None) => Ok(child),
        Err(err) => {
            let _ = fs::remove_file(output_path);
            Err(format!("failed checking gst-launch-1.0 status: {err}"))
        }
    }
}

/// Builds `gst-launch-1.0` arguments to losslessly concatenate paused/resumed recording segments.
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

/// Starts a Wayland or X11 GStreamer screen recording for the given [`RecordingTarget`].
pub async fn start_recording(
    is_wayland: bool,
    target: RecordingTarget,
    with_system_audio: bool,
    with_mic: bool,
) -> Result<ActiveRecording, String> {
    let profile = probe_video_profile()?;
    let final_output_path = next_recording_path(profile.ext())?;

    let mut wayland_session = if is_wayland {
        if !gst_has_plugin("pipewiresrc") {
            return Err("GStreamer pipewiresrc plugin is missing".to_string());
        }
        let source = match target {
            RecordingTarget::Window => WaylandSource::Window,
            _ => WaylandSource::Monitor,
        };
        let session = open_screencast_session_for(source)
            .await
            .map_err(|e| e.to_string())?;
        Some(session)
    } else {
        if !gst_has_plugin("ximagesrc") {
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
    };

    let seg0 = segment_path(&final_output_path, 0, profile.ext());
    let child = match spawn_gst_segment(&config, &seg0, wayland_session.as_ref()).await {
        Ok(child) => child,
        Err(err) => {
            if let Some(session) = wayland_session.take() {
                session.close().await;
            }
            return Err(err);
        }
    };

    info!(path = %final_output_path.display(), ?profile, ?target, "started screen recording");
    Ok(ActiveRecording {
        config,
        final_output_path,
        segments: vec![seg0],
        current_child: Some(child),
        wayland_session,
        accumulated_duration: Duration::ZERO,
        segment_started_at: Some(Instant::now()),
        paused: false,
        last_stderr: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn videocrop_even_aligns_and_clamps() {
        let r = Rect::new(101.0, 51.0, 641.0, 481.0);
        let (top, bottom, left, right) = compute_videocrop(r, 1920, 1080);
        assert_eq!(left % 2, 0);
        assert_eq!(top % 2, 0);
        assert_eq!((1920 - left - right) % 2, 0);
        assert_eq!((1080 - top - bottom) % 2, 0);
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
