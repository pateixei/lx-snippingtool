//! Phase 2 GStreamer screen recorder (`gst-launch-1.0 -e`) using PipeWire `ScreenCast`
//! on Wayland (with `restore_token`) and `ximagesrc` on X11.

use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use directories::{ProjectDirs, UserDirs};
use lx_snip_core::scene::Rect;
use tracing::{info, warn};
use uuid::Uuid;

use crate::capture_wayland::{open_screencast_session, WaylandScreencastSession};

/// Detected GStreamer video/audio encoder & container profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoProfile {
    /// H.264 (`x264enc`) + AAC/Opus in MP4 (`mp4mux`).
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

/// Active screen recording process handle.
pub struct ActiveRecording {
    child: Child,
    /// Output file path being written.
    pub output_path: PathBuf,
    wayland_session: Option<WaylandScreencastSession>,
    started_at: Instant,
}

impl ActiveRecording {
    /// Elapsed recording time in seconds.
    pub fn elapsed_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Sends `SIGINT` to `gst-launch-1.0 -e` so it emits `EOS`, finalizes the container,
    /// and closes the Wayland `ScreenCast` session.
    pub async fn stop(mut self) -> Result<(PathBuf, u64), String> {
        let elapsed = self.elapsed_secs();
        let pid = self.child.id() as libc::pid_t;
        // SAFETY: sending SIGINT to our own spawned gst-launch-1.0 child pid.
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }

        let start_wait = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) if start_wait.elapsed() < Duration::from_secs(5) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok(None) => {
                    warn!("gst-launch-1.0 did not exit on SIGINT within 5s; killing");
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Err(err) => return Err(format!("failed waiting for gst-launch-1.0: {err}")),
            }
        }

        if let Some(session) = self.wayland_session.take() {
            session.close().await;
        }

        let file_len = fs::metadata(&self.output_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if file_len == 0 {
            let mut err_text = String::new();
            if let Some(mut stderr) = self.child.stderr.take() {
                let _ = stderr.read_to_string(&mut err_text);
            }
            let _ = fs::remove_file(&self.output_path);
            let detail = err_text.trim();
            return Err(if detail.is_empty() {
                "Recording produced an empty video file".to_string()
            } else {
                format!("Recording failed (empty output): {detail}")
            });
        }

        info!(path = %self.output_path.display(), elapsed_secs = elapsed, "recording finalized");
        Ok((self.output_path, elapsed))
    }
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

/// Starts a Wayland or X11 GStreamer screen recording over the given optional `region`.
pub async fn start_recording(
    is_wayland: bool,
    region: Option<(Rect, u32, u32)>,
    with_system_audio: bool,
    with_mic: bool,
) -> Result<ActiveRecording, String> {
    let profile = probe_video_profile()?;
    let output_path = next_recording_path(profile.ext())?;

    let mut wayland_session = None;
    let mut args: Vec<String> = vec!["-e".to_string()];

    if is_wayland {
        if !gst_has_plugin("pipewiresrc") {
            return Err("GStreamer pipewiresrc plugin is missing".to_string());
        }
        let session = open_screencast_session().await.map_err(|e| e.to_string())?;
        let raw_fd = session.pw_fd.as_raw_fd();
        let node_id = session.node_id;
        wayland_session = Some(session);

        args.extend([
            "pipewiresrc".to_string(),
            format!("fd={raw_fd}"),
            format!("path={node_id}"),
            "do-timestamp=true".to_string(),
            "keepalive-time=1000".to_string(),
            "resend-last=true".to_string(),
        ]);
    } else {
        if !gst_has_plugin("ximagesrc") {
            return Err("GStreamer ximagesrc plugin is missing".to_string());
        }
        args.extend(["ximagesrc".to_string(), "use-damage=false".to_string()]);
    }

    args.extend(["!".to_string(), "videoconvert".to_string()]);

    if let Some((rect, full_w, full_h)) = region {
        let (top, bottom, left, right) = compute_videocrop(rect, full_w, full_h);
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

    match profile {
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

    if (with_system_audio || with_mic) && gst_has_plugin("pulsesrc") && gst_has_plugin("opusenc") {
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

    let mut cmd = Command::new("gst-launch-1.0");
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    if let Some(ref session) = wayland_session {
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
            if let Some(session) = wayland_session.take() {
                session.close().await;
            }
            let _ = fs::remove_file(&output_path);
            let detail = err_text.trim();
            return Err(if detail.is_empty() {
                format!("gst-launch-1.0 exited early ({status})")
            } else {
                format!("gst-launch-1.0 exited early ({status}): {detail}")
            });
        }
        Ok(None) => {}
        Err(err) => {
            if let Some(session) = wayland_session.take() {
                session.close().await;
            }
            let _ = fs::remove_file(&output_path);
            return Err(format!("failed checking gst-launch-1.0 status: {err}"));
        }
    }

    info!(path = %output_path.display(), ?profile, "started screen recording");
    Ok(ActiveRecording {
        child,
        output_path,
        wayland_session,
        started_at: Instant::now(),
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
}
