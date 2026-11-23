use crate::color::composite_to_rgb;
use crate::MediaError;
use image::RgbImage;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use truongphoto_core::{PlaybackMode, VideoMetaView};

/// Discovered FFmpeg/FFprobe binaries and hardware/filter capabilities.
#[derive(Debug, Clone)]
pub struct VideoCapabilities {
    pub ffmpeg_path: Option<PathBuf>,
    pub ffprobe_path: Option<PathBuf>,
    pub h264_encoder: String,
    pub has_zscale_tonemap: bool,
}

static VIDEO_CAPS: OnceLock<VideoCapabilities> = OnceLock::new();

/// Prevents flashing black console windows when spawning external CLI tools on Windows.
pub fn hide_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

fn resolve_tool_binary(env_key: &str, name: &str) -> Option<PathBuf> {
    if let Ok(val) = std::env::var(env_key) {
        let p = PathBuf::from(val.trim());
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sidecar = dir.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_string()
            });
            if sidecar.is_file() {
                return Some(sidecar);
            }
        }
    }

    let bin_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };

    let mut search_dirs: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ]
    } else if cfg!(target_os = "windows") {
        let mut dirs = vec![
            PathBuf::from(r"C:\ffmpeg\bin"),
            PathBuf::from(r"C:\Program Files\ffmpeg\bin"),
            PathBuf::from(r"C:\ProgramData\chocolatey\bin"),
        ];
        if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local_app)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links"),
            );
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            dirs.push(PathBuf::from(profile).join("scoop").join("shims"));
        }
        dirs
    } else {
        vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")]
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            search_dirs.push(dir.join("resources"));
            if cfg!(target_os = "macos") {
                if let Some(contents_dir) = dir.parent() {
                    search_dirs.push(contents_dir.join("Resources"));
                }
            }
        }
    }

    for dir in &search_dirs {
        let candidate = dir.join(&bin_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(&bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn is_hardware_encoder(enc: &str) -> bool {
    matches!(
        enc,
        "h264_nvenc" | "h264_qsv" | "h264_amf" | "h264_videotoolbox"
    )
}

fn smoke_test_encoder(ffmpeg: &Path, encoder: &str) -> bool {
    let mut cmd = Command::new(ffmpeg);
    hide_console_window(&mut cmd);
    cmd.args([
        "-hide_banner",
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=64x64:d=0.1",
        "-frames:v",
        "1",
        "-c:v",
        encoder,
        "-f",
        "null",
        "-",
    ]);
    truongphoto_core::process::run_command_with_timeout(
        &mut cmd,
        std::time::Duration::from_secs(15),
    )
    .is_ok_and(|out| out.status.success())
}

fn probe_h264_encoder(ffmpeg: &Path) -> String {
    let preferred: &[&str] = if cfg!(target_os = "macos") {
        &["h264_videotoolbox", "libopenh264", "libx264"]
    } else if cfg!(target_os = "windows") {
        &[
            "h264_nvenc",
            "h264_qsv",
            "h264_amf",
            "libx264",
            "libopenh264",
        ]
    } else {
        &["libx264", "libopenh264"]
    };

    let mut cmd = Command::new(ffmpeg);
    hide_console_window(&mut cmd);
    cmd.args(["-hide_banner", "-encoders"]);
    let Ok(out) = truongphoto_core::process::run_command_with_timeout(
        &mut cmd,
        std::time::Duration::from_secs(15),
    ) else {
        return "libx264".to_string();
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    for enc in preferred {
        if stdout.contains(enc) {
            if is_hardware_encoder(enc) && !smoke_test_encoder(ffmpeg, enc) {
                continue;
            }
            return (*enc).to_string();
        }
    }
    "libx264".to_string()
}

fn probe_zscale_tonemap(ffmpeg: &Path) -> bool {
    let mut cmd = Command::new(ffmpeg);
    hide_console_window(&mut cmd);
    cmd.args(["-hide_banner", "-filters"]);
    let Ok(out) = truongphoto_core::process::run_command_with_timeout(
        &mut cmd,
        std::time::Duration::from_secs(15),
    ) else {
        return false;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout.contains("zscale") && stdout.contains("tonemap")
}

/// Discovers and caches FFmpeg/FFprobe binaries and encoding capabilities once per process.
pub fn video_capabilities() -> &'static VideoCapabilities {
    VIDEO_CAPS.get_or_init(|| {
        let ffmpeg_path = resolve_tool_binary("TP_FFMPEG_PATH", "ffmpeg");
        let ffprobe_path = resolve_tool_binary("TP_FFPROBE_PATH", "ffprobe");
        let (h264_encoder, has_zscale_tonemap) = match ffmpeg_path.as_deref() {
            Some(ffmpeg) => (probe_h264_encoder(ffmpeg), probe_zscale_tonemap(ffmpeg)),
            None => ("libx264".to_string(), false),
        };
        VideoCapabilities {
            ffmpeg_path,
            ffprobe_path,
            h264_encoder,
            has_zscale_tonemap,
        }
    })
}

/// Chooses `Direct` HTTP Range vs. `Hls` transcoding based on container, codec, bit depth, and HDR status.
pub fn decide_playback_mode(extension: &str, video_meta: Option<&VideoMetaView>) -> PlaybackMode {
    let ext = extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let direct_container = matches!(ext.as_str(), "mp4" | "m4v" | "webm");
    if !direct_container {
        return PlaybackMode::Hls;
    }

    let Some(meta) = video_meta else {
        return PlaybackMode::Hls;
    };

    if meta.is_hdr || meta.bit_depth.unwrap_or(8) > 8 {
        return PlaybackMode::Hls;
    }

    let codec = meta
        .video_codec
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if matches!(codec.as_str(), "h264" | "avc1" | "vp8" | "vp9") {
        PlaybackMode::Direct
    } else {
        PlaybackMode::Hls
    }
}

fn require_ffmpeg() -> Result<&'static Path, MediaError> {
    video_capabilities()
        .ffmpeg_path
        .as_deref()
        .ok_or_else(|| MediaError::ExternalTool {
            tool: "ffmpeg",
            message: "ffmpeg executable not found in TP_FFMPEG_PATH, sidecar, or PATH".to_string(),
        })
}

/// Extracts a poster frame from a video file using FFmpeg.
pub fn extract_video_poster_frame(src: &Path) -> Result<RgbImage, MediaError> {
    let ffmpeg = require_ffmpeg()?;
    for seek in [Some("00:00:00.500"), None] {
        let mut cmd = Command::new(ffmpeg);
        hide_console_window(&mut cmd);
        cmd.args(["-v", "error"]);
        if let Some(ts) = seek {
            cmd.args(["-ss", ts]);
        }
        cmd.arg("-i")
            .arg(src)
            .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"]);
        let output = truongphoto_core::process::run_command_with_timeout(
            &mut cmd,
            std::time::Duration::from_secs(30),
        )?;

        if output.status.success() && !output.stdout.is_empty() {
            let dyn_img = image::load_from_memory(&output.stdout).map_err(|e| {
                MediaError::Decode(format!("failed to decode video poster PNG: {e}"))
            })?;
            return Ok(composite_to_rgb(dyn_img));
        }
    }

    Err(MediaError::ExternalTool {
        tool: "ffmpeg",
        message: format!("failed to extract poster frame from {}", src.display()),
    })
}

fn dir_size_bytes(dir: &Path) -> Result<u64, MediaError> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

/// Transcodes `src` into an atomic VOD HLS directory (`index.m3u8` + `seg_%04d.ts`), returning total bytes written.
pub fn transcode_to_hls(src: &Path, output_dir: &Path, is_hdr: bool) -> Result<u64, MediaError> {
    let caps = video_capabilities();
    let ffmpeg = require_ffmpeg()?;

    let parent = output_dir
        .parent()
        .ok_or_else(|| MediaError::Decode("invalid HLS output directory".to_string()))?;
    std::fs::create_dir_all(parent)?;

    let dir_name = output_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("hls");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_dir = parent.join(format!(".{dir_name}.tmp.{}.{nonce}", std::process::id()));
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    std::fs::create_dir_all(&tmp_dir)?;

    const VF_SDR: &str = "scale=w='trunc(min(1920\\,iw)/2)*2':h=-2,format=yuv420p";
    const VF_HDR_TONEMAP: &str = "scale=w='trunc(min(1920\\,iw)/2)*2':h=-2,zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p";

    let use_hdr_vf = is_hdr && caps.has_zscale_tonemap;
    let initial_vf = if use_hdr_vf { VF_HDR_TONEMAP } else { VF_SDR };

    let seg_pattern = tmp_dir.join("seg_%04d.ts");
    let playlist_path = tmp_dir.join("index.m3u8");

    let run_hls = |encoder: &str, filter: &str| {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let _ = std::fs::create_dir_all(&tmp_dir);
        let mut cmd = Command::new(ffmpeg);
        hide_console_window(&mut cmd);
        cmd.args(["-y", "-v", "error", "-i"])
            .arg(src)
            .args([
                "-vf",
                filter,
                "-c:v",
                encoder,
                "-b:v",
                "6M",
                "-maxrate",
                "8M",
                "-bufsize",
                "12M",
                "-c:a",
                "aac",
                "-ac",
                "2",
                "-b:a",
                "128k",
                "-hls_time",
                "4",
                "-hls_playlist_type",
                "vod",
                "-hls_flags",
                "independent_segments",
                "-hls_segment_filename",
            ])
            .arg(&seg_pattern)
            .arg(&playlist_path);
        truongphoto_core::process::run_command_with_timeout(
            &mut cmd,
            std::time::Duration::from_secs(15 * 60),
        )
    };

    let mut output = match run_hls(&caps.h264_encoder, initial_vf) {
        Ok(out) => out,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(MediaError::Io(e));
        }
    };

    // Automatic software fallback if a hardware encoder fails at runtime (e.g. driver/profile limits).
    if (!output.status.success() || !playlist_path.is_file())
        && is_hardware_encoder(&caps.h264_encoder)
    {
        if let Ok(fallback) = run_hls("libx264", initial_vf) {
            output = fallback;
        }
    }

    // If HDR zscale tonemapping failed (e.g. missing/unusual colorspace tags), fall back to SDR filter chain.
    if (!output.status.success() || !playlist_path.is_file()) && use_hdr_vf {
        if let Ok(sdr_fallback) = run_hls("libx264", VF_SDR) {
            output = sdr_fallback;
        }
    }

    if !output.status.success() || !playlist_path.is_file() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(MediaError::ExternalTool {
            tool: "ffmpeg",
            message: if stderr.is_empty() {
                "ffmpeg HLS transcode failed".to_string()
            } else {
                stderr
            },
        });
    }

    let total_bytes = dir_size_bytes(&tmp_dir)?;

    // Safely move aside any existing output_dir first so Windows Defender or an active reader
    // does not leave output_dir half-deleted and break rename().
    let old_backup = if output_dir.exists() {
        let backup = parent.join(format!(".hls.old.{}.{}", std::process::id(), nonce));
        let mut moved = false;
        let attempts = if cfg!(windows) { 5 } else { 1 };
        for attempt in 0..attempts {
            match std::fs::rename(output_dir, &backup) {
                Ok(()) => {
                    moved = true;
                    break;
                }
                Err(_) if attempt + 1 < attempts => {
                    std::thread::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1)));
                }
                Err(_) => break,
            }
        }
        if moved {
            Some(backup)
        } else {
            let _ = std::fs::remove_dir_all(output_dir);
            None
        }
    } else {
        None
    };

    let mut rename_err: Option<std::io::Error> = None;
    let attempts = if cfg!(windows) { 5 } else { 1 };
    for attempt in 0..attempts {
        match std::fs::rename(&tmp_dir, output_dir) {
            Ok(()) => {
                rename_err = None;
                break;
            }
            Err(e) => {
                rename_err = Some(e);
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1)));
                }
            }
        }
    }

    if let Some(e) = rename_err {
        if let Some(backup) = old_backup {
            let _ = std::fs::rename(&backup, output_dir);
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(MediaError::Io(e));
    }

    if let Some(backup) = old_backup {
        let _ = std::fs::remove_dir_all(&backup);
    }

    Ok(total_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta(codec: &str, bit_depth: u8, is_hdr: bool) -> VideoMetaView {
        VideoMetaView {
            duration_ms: 2000,
            video_codec: Some(codec.to_string()),
            bit_depth: Some(bit_depth),
            is_hdr,
            ..Default::default()
        }
    }

    #[test]
    fn playback_mode_rules_match_spec() {
        assert_eq!(
            decide_playback_mode("mp4", Some(&sample_meta("h264", 8, false))),
            PlaybackMode::Direct
        );
        assert_eq!(
            decide_playback_mode("webm", Some(&sample_meta("vp9", 8, false))),
            PlaybackMode::Direct
        );
        assert_eq!(
            decide_playback_mode("mov", Some(&sample_meta("h264", 8, false))),
            PlaybackMode::Hls
        );
        assert_eq!(
            decide_playback_mode("mp4", Some(&sample_meta("hevc", 10, false))),
            PlaybackMode::Hls
        );
        assert_eq!(
            decide_playback_mode("mp4", Some(&sample_meta("h264", 8, true))),
            PlaybackMode::Hls
        );
        assert_eq!(decide_playback_mode("mp4", None), PlaybackMode::Hls);
    }
}
