use crate::AiError;
use image::RgbImage;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Extracted video keyframe with its exact presentation timestamp in milliseconds.
#[derive(Debug, Clone)]
pub struct SampledKeyframe {
    pub timestamp_ms: i64,
    pub image: RgbImage,
}

fn resolve_binary(env_key: &str, name: &str) -> Option<PathBuf> {
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
    let common_dirs: &[&str] = if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
    } else if cfg!(target_os = "windows") {
        &[r"C:\ffmpeg\bin", r"C:\Program Files\ffmpeg\bin"]
    } else {
        &["/usr/local/bin", "/usr/bin"]
    };
    let bin_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    for dir in common_dirs {
        let candidate = Path::new(dir).join(&bin_name);
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

fn parse_showinfo_pts_ms(stderr: &str) -> Vec<i64> {
    let mut timestamps = Vec::new();
    for line in stderr.lines() {
        if !line.contains("showinfo") {
            continue;
        }
        if let Some(idx) = line.find("pts_time:") {
            let rest = &line[idx + "pts_time:".len()..];
            let token = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
            if let Ok(secs) = token.parse::<f64>() {
                if secs.is_finite() && secs >= 0.0 {
                    timestamps.push((secs * 1000.0).round() as i64);
                }
            }
        }
    }
    timestamps
}

fn probe_duration_ms(ffprobe: &Path, video_path: &Path) -> Option<i64> {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(video_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let secs = text.trim().parse::<f64>().ok()?;
    if secs.is_finite() && secs > 0.0 {
        Some((secs * 1000.0).round() as i64)
    } else {
        None
    }
}

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new() -> Result<Self, AiError> {
        let path = std::env::temp_dir().join(format!("tp-kf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Samples scene-change + interval-capped keyframes from `video_path` using `ffmpeg`.
pub fn sample_video_keyframes(
    video_path: &Path,
    max_frames: u32,
) -> Result<Vec<SampledKeyframe>, AiError> {
    if max_frames == 0 {
        return Ok(Vec::new());
    }
    let ffmpeg = resolve_binary("TP_FFMPEG_PATH", "ffmpeg").ok_or_else(|| {
        AiError::ExternalTool("ffmpeg binary not found on system or TP_FFMPEG_PATH".to_string())
    })?;

    let tmp_dir = TempDirGuard::new()?;
    let pattern = tmp_dir.path().join("kf_%03d.jpg");

    let output = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "info", "-i"])
        .arg(video_path)
        .args([
            "-vf",
            "select='eq(n,0)+gt(scene,0.3)*gte(t-prev_selected_t,5)+gte(t-prev_selected_t,30)',showinfo",
            "-fps_mode",
            "vfr",
            "-frames:v",
        ])
        .arg(max_frames.to_string())
        .arg(&pattern)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AiError::ExternalTool(format!(
            "ffmpeg keyframe sampling failed for {}: {}",
            video_path.display(),
            stderr.trim()
        )));
    }

    let mut frame_paths: Vec<PathBuf> = std::fs::read_dir(tmp_dir.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jpg"))
        .collect();
    frame_paths.sort();

    // Fallback to frame 0 if the select expression yielded zero frames.
    if frame_paths.is_empty() {
        let fallback_out = tmp_dir.path().join("kf_001.jpg");
        let fb_status = Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(video_path)
            .args(["-frames:v", "1"])
            .arg(&fallback_out)
            .status()?;
        if fb_status.success() && fallback_out.is_file() {
            frame_paths.push(fallback_out);
        }
    }

    if frame_paths.is_empty() {
        return Ok(Vec::new());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut pts_ms = parse_showinfo_pts_ms(&stderr);
    if pts_ms.len() != frame_paths.len() {
        let duration_ms = resolve_binary("TP_FFPROBE_PATH", "ffprobe")
            .and_then(|ffprobe| probe_duration_ms(&ffprobe, video_path))
            .unwrap_or_else(|| (frame_paths.len() as i64) * 5_000);
        let n = frame_paths.len();
        pts_ms = (0..n)
            .map(|i| {
                if n <= 1 {
                    0
                } else {
                    (i as i64) * duration_ms / (n as i64)
                }
            })
            .collect();
    }

    let mut keyframes = Vec::with_capacity(frame_paths.len());
    for (path, timestamp_ms) in frame_paths.into_iter().zip(pts_ms.into_iter()) {
        let img = image::open(&path)
            .map_err(|e| AiError::Image(format!("open keyframe {}: {e}", path.display())))?
            .to_rgb8();
        keyframes.push(SampledKeyframe {
            timestamp_ms,
            image: img,
        });
    }

    Ok(keyframes)
}
