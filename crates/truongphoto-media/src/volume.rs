use std::path::Path;

/// OS volume identity metadata used to detect offline/remounted external drives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeInfo {
    pub volume_uuid: Option<String>,
    pub volume_label: Option<String>,
}

/// Resolves stable OS-level volume identity for `path`.
///
/// Returns `VolumeInfo { volume_uuid: None, volume_label: None }` if `path` does not exist.
pub fn resolve_volume_identity(path: &Path) -> VolumeInfo {
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return VolumeInfo::default();
    };

    #[cfg(target_os = "macos")]
    {
        macos::resolve_macos(&canonical)
    }

    #[cfg(target_os = "windows")]
    {
        windows_impl::resolve_windows(&canonical)
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = canonical;
        VolumeInfo::default()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::VolumeInfo;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::ffi::{CStr, CString};
    use std::io::Cursor;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::process::Command;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    const CACHE_TTL: Duration = Duration::from_secs(10);

    static VOLUME_CACHE: OnceLock<Mutex<HashMap<String, (Instant, VolumeInfo)>>> = OnceLock::new();

    fn cache() -> &'static Mutex<HashMap<String, (Instant, VolumeInfo)>> {
        VOLUME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn mount_point_for_path(canonical: &Path) -> Option<String> {
        let c_path = CString::new(canonical.as_os_str().as_bytes()).ok()?;
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return None;
        }
        let mnt = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) };
        let mnt_str = mnt.to_string_lossy().into_owned();
        if mnt_str.is_empty() {
            None
        } else {
            Some(mnt_str)
        }
    }

    fn query_diskutil(mount_point: &str) -> Option<VolumeInfo> {
        let output = Command::new("/usr/sbin/diskutil")
            .args(["info", "-plist", mount_point])
            .output()
            .or_else(|_| {
                Command::new("diskutil")
                    .args(["info", "-plist", mount_point])
                    .output()
            })
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let value = plist::Value::from_reader(Cursor::new(&output.stdout)).ok()?;
        let dict = value.as_dictionary()?;

        let non_empty = |key: &str| -> Option<String> {
            dict.get(key)
                .and_then(|v| v.as_string())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        };

        let volume_uuid = non_empty("VolumeUUID").or_else(|| non_empty("DiskUUID"));
        let volume_label = non_empty("VolumeName");

        Some(VolumeInfo {
            volume_uuid,
            volume_label,
        })
    }

    fn fallback_volumes_label(canonical: &Path) -> VolumeInfo {
        use std::path::Component;
        let mut comps = canonical.components();
        if matches!(comps.next(), Some(Component::RootDir))
            && matches!(comps.next(), Some(Component::Normal(s)) if s == "Volumes")
        {
            if let Some(Component::Normal(label)) = comps.next() {
                return VolumeInfo {
                    volume_uuid: None,
                    volume_label: Some(label.to_string_lossy().into_owned()),
                };
            }
        }
        VolumeInfo::default()
    }

    pub(super) fn resolve_macos(canonical: &Path) -> VolumeInfo {
        let Some(mount_point) = mount_point_for_path(canonical) else {
            return fallback_volumes_label(canonical);
        };

        let now = Instant::now();
        {
            let guard = cache().lock();
            if let Some((fetched_at, info)) = guard.get(&mount_point) {
                if now.duration_since(*fetched_at) < CACHE_TTL {
                    return info.clone();
                }
            }
        }

        let resolved =
            query_diskutil(&mount_point).unwrap_or_else(|| fallback_volumes_label(canonical));

        let mut guard = cache().lock();
        guard.insert(mount_point, (now, resolved.clone()));
        resolved
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::VolumeInfo;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };

    fn utf16_to_string(buf: &[u16]) -> Option<String> {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = OsString::from_wide(&buf[..end])
            .to_string_lossy()
            .trim()
            .to_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    pub(super) fn resolve_windows(canonical: &Path) -> VolumeInfo {
        // Strip extended-length `\\?\` prefix only when within Win32 MAX_PATH (< 248 chars),
        // preserving UNC shares (`\\?\UNC\server\share` -> `\\server\share`) and long paths.
        let path_str = canonical.as_os_str().to_string_lossy();
        let trimmed: std::borrow::Cow<'_, str> =
            if let Some(unc_rest) = path_str.strip_prefix(r"\\?\UNC\") {
                if unc_rest.len() + 2 < 248 {
                    std::borrow::Cow::Owned(format!(r"\\{unc_rest}"))
                } else {
                    std::borrow::Cow::Borrowed(&path_str)
                }
            } else if let Some(local_rest) = path_str.strip_prefix(r"\\?\") {
                if local_rest.len() < 248 {
                    std::borrow::Cow::Borrowed(local_rest)
                } else {
                    std::borrow::Cow::Borrowed(&path_str)
                }
            } else {
                std::borrow::Cow::Borrowed(&path_str)
            };
        let wide_path: Vec<u16> = OsString::from(trimmed.as_ref())
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut mount_point = vec![0u16; 512];
        let ok = unsafe { GetVolumePathNameW(PCWSTR(wide_path.as_ptr()), &mut mount_point) };
        if ok.is_err() {
            return VolumeInfo::default();
        }

        let mut volume_guid = vec![0u16; 260];
        let volume_uuid = unsafe {
            GetVolumeNameForVolumeMountPointW(PCWSTR(mount_point.as_ptr()), &mut volume_guid)
                .ok()
                .and_then(|()| utf16_to_string(&volume_guid))
        };

        let mut label_buf = vec![0u16; 260];
        let volume_label = unsafe {
            GetVolumeInformationW(
                PCWSTR(mount_point.as_ptr()),
                Some(&mut label_buf),
                None,
                None,
                None,
                None,
            )
            .ok()
            .and_then(|()| utf16_to_string(&label_buf))
        };

        VolumeInfo {
            volume_uuid,
            volume_label,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_volume_identity_handles_existing_temp_dir() {
        let info = resolve_volume_identity(&std::env::temp_dir());
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert!(
            info.volume_uuid.is_some(),
            "expected non-empty volume UUID on macOS/Windows, got {info:?}"
        );
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = info;
    }

    #[test]
    fn resolve_volume_identity_returns_none_for_missing_path() {
        let info = resolve_volume_identity(Path::new("/nonexistent/truongphoto/missing/folder"));
        assert_eq!(
            info,
            VolumeInfo {
                volume_uuid: None,
                volume_label: None
            }
        );
    }
}
