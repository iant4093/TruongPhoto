//! Cross-platform child-process lifecycle helpers and folder path normalization.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Assigns `child` to an app-lifetime Windows Job Object configured with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so spawned helper processes (`llama-server`,
/// `ffmpeg`, `ffprobe`) are terminated by the kernel if TruongPhoto crashes, panics, or is killed.
#[cfg(target_os = "windows")]
pub fn assign_to_kill_on_close_job(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    static JOB_HANDLE: OnceLock<isize> = OnceLock::new();
    let job_raw = *JOB_HANDLE.get_or_init(|| unsafe {
        let Ok(job) = CreateJobObjectW(None, PCWSTR::null()) else {
            return 0;
        };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok.is_err() {
            return 0;
        }
        job.0 as isize
    });

    if job_raw != 0 {
        let job = HANDLE(job_raw as *mut core::ffi::c_void);
        let proc_handle = HANDLE(child.as_raw_handle());
        unsafe {
            let _ = AssignProcessToJobObject(job, proc_handle);
        }
    }
}

/// No-op on non-Windows platforms.
#[cfg(not(target_os = "windows"))]
pub fn assign_to_kill_on_close_job(_child: &Child) {}

/// Spawns `cmd` with piped stdout/stderr, assigns it to the Windows kill-on-close Job Object,
/// drains pipes without deadlock, and terminates the child if it exceeds `timeout`.
pub fn run_command_with_timeout(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    assign_to_kill_on_close_job(&child);

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = stdout_pipe {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = stderr_pipe {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("subprocess timed out after {}s", timeout.as_secs()),
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Normalizes a filesystem folder path consistently across Windows, macOS, and Linux:
/// - canonicalizes when the directory exists (stripping Windows `\\?\` / `\\?\UNC\` prefixes)
/// - unifies separators (`/` -> `\` on Windows) and uppercases Windows drive letters
/// - strips trailing path separators while preserving `/` and `X:\` drive roots.
pub fn normalize_folder_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let resolved = match std::fs::canonicalize(Path::new(trimmed)) {
        Ok(canon) => strip_windows_verbatim(&canon.to_string_lossy()),
        Err(_) => strip_windows_verbatim(trimmed),
    };

    #[cfg(target_os = "windows")]
    let mut normalized = resolved.replace('/', "\\");
    #[cfg(not(target_os = "windows"))]
    let mut normalized = resolved;

    if normalized.len() <= 1 {
        return normalized;
    }

    // Uppercase Windows drive letter (`c:\photos` -> `C:\photos`).
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_lowercase() && bytes[1] == b':' {
        let upper = (bytes[0] as char).to_ascii_uppercase();
        normalized.replace_range(0..1, &upper.to_string());
    }

    let stripped = normalized.trim_end_matches(['/', '\\']);
    if stripped.len() == 2
        && stripped.as_bytes()[0].is_ascii_alphabetic()
        && stripped.as_bytes()[1] == b':'
    {
        return format!("{stripped}\\");
    }
    if stripped.is_empty() {
        "/".to_string()
    } else {
        stripped.to_string()
    }
}

/// Atomically renames `from` to `to`, retrying briefly on Windows where antivirus scanners
/// or recently closed mmap/file handles can transiently hold `ERROR_SHARING_VIOLATION` / `AccessDenied`.
pub fn rename_with_windows_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let attempts = if cfg!(windows) { 5 } else { 1 };
    let mut last_err = None;
    for attempt in 0..attempts {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < attempts {
                    std::thread::sleep(Duration::from_millis(50 * (attempt as u64 + 1)));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::other(format!(
            "rename {} -> {} failed",
            from.display(),
            to.display()
        ))
    }))
}

fn strip_windows_verbatim(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        let candidate = format!(r"\\{unc}");
        if candidate.len() < 248 {
            return candidate;
        }
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        // Preserve `\\?\` extended-length prefix for Windows paths >= 248 chars.
        if rest.len() < 248 {
            return rest.to_string();
        }
        return path.to_string();
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folder_path_preserves_roots_and_strips_trailing_slashes() {
        #[cfg(not(windows))]
        assert_eq!(normalize_folder_path("/"), "/");
        #[cfg(windows)]
        assert_eq!(normalize_folder_path("c:/Users/Ian/"), r"C:\Users\Ian");
        assert_eq!(normalize_folder_path(r"d:\"), r"D:\");
        assert_eq!(normalize_folder_path(r"\\?\C:\Users\Ian\"), r"C:\Users\Ian");
        assert_eq!(
            normalize_folder_path(r"\\?\UNC\nas\photos\"),
            r"\\nas\photos"
        );
    }
}
