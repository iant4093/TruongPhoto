use std::path::Path;

/// Coarse media classification used during ingestion and serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaCategory {
    Photo,
    Raw,
    Video,
    Livp,
}

impl MediaCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Raw => "raw",
            Self::Video => "video",
            Self::Livp => "livp",
        }
    }
}

const PHOTO_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff", "heic", "heif", "avif", "jxl",
];

const RAW_EXTENSIONS: &[&str] = &[
    "cr3", "cr2", "nef", "nrw", "arw", "srf", "dng", "raf", "orf", "rw2", "pef", "srw", "iiq",
    "3fr",
];

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "3gp", "mts", "m2ts", "avi",
];

fn normalize_ext_str(ext: &str) -> String {
    ext.trim().trim_start_matches('.').to_ascii_lowercase()
}

/// Extracts and normalizes a path's extension to lowercase without a leading dot.
pub fn normalized_extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let norm = normalize_ext_str(ext);
    if norm.is_empty() {
        None
    } else {
        Some(norm)
    }
}

/// Returns `true` for OS metadata, VCS, staging, and incomplete download entries that must always be ignored.
pub fn is_ignored_entry(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Dotfiles cover `.DS_Store`, `._*` AppleDouble, `.git`, `.staging`, etc.
    if trimmed.starts_with('.') {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "thumbs.db"
            | "desktop.ini"
            | "@eadir"
            | "$recycle.bin"
            | "system volume information"
            | "recovery"
            | "$windows.~bt"
            | "$windows.~ws"
    ) || lower.ends_with(".tmp")
        || lower.ends_with(".part")
        || lower.ends_with(".crdownload")
}

/// Returns `true` if the extension represents a supported camera RAW format.
pub fn is_raw_extension(ext: &str) -> bool {
    let norm = normalize_ext_str(ext);
    RAW_EXTENSIONS.contains(&norm.as_str())
}

/// Returns `true` if the extension represents HEIC or HEIF.
pub fn is_heic_extension(ext: &str) -> bool {
    let norm = normalize_ext_str(ext);
    matches!(norm.as_str(), "heic" | "heif")
}

/// Classifies a normalized or raw extension string into a `MediaCategory`.
pub fn classify_extension(ext: &str) -> Option<MediaCategory> {
    let norm = normalize_ext_str(ext);
    let s = norm.as_str();
    if PHOTO_EXTENSIONS.contains(&s) {
        Some(MediaCategory::Photo)
    } else if RAW_EXTENSIONS.contains(&s) {
        Some(MediaCategory::Raw)
    } else if VIDEO_EXTENSIONS.contains(&s) {
        Some(MediaCategory::Video)
    } else if s == "livp" {
        Some(MediaCategory::Livp)
    } else {
        None
    }
}

/// Classifies a filesystem path into a supported `MediaCategory`, returning `None` for ignored or unsupported paths.
pub fn classify_path(path: &Path) -> Option<MediaCategory> {
    let file_name = path.file_name()?.to_str()?;
    if is_ignored_entry(file_name) {
        return None;
    }
    let ext = normalized_extension(path)?;
    classify_extension(&ext)
}

/// Maps a file extension to its canonical MIME type.
pub fn mime_for_extension(ext: &str) -> &'static str {
    match normalize_ext_str(ext).as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "jxl" => "image/jxl",
        "cr3" => "image/x-canon-cr3",
        "cr2" => "image/x-canon-cr2",
        "nef" => "image/x-nikon-nef",
        "nrw" => "image/x-nikon-nrw",
        "arw" => "image/x-sony-arw",
        "srf" => "image/x-sony-srf",
        "dng" => "image/x-adobe-dng",
        "raf" => "image/x-fuji-raf",
        "orf" => "image/x-olympus-orf",
        "rw2" => "image/x-panasonic-rw2",
        "pef" => "image/x-pentax-pef",
        "srw" => "image/x-samsung-srw",
        "iiq" => "image/x-phaseone-iiq",
        "3fr" => "image/x-hasselblad-3fr",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "3gp" => "video/3gpp",
        "mts" | "m2ts" => "video/mp2t",
        "avi" => "video/x-msvideo",
        "livp" => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_all_supported_extensions() {
        for ext in PHOTO_EXTENSIONS {
            assert_eq!(
                classify_path(Path::new(&format!("sample.{ext}"))),
                Some(MediaCategory::Photo),
                "photo ext {ext}"
            );
            assert_eq!(
                classify_path(Path::new(&format!("SAMPLE.{}", ext.to_ascii_uppercase()))),
                Some(MediaCategory::Photo)
            );
        }
        for ext in RAW_EXTENSIONS {
            assert_eq!(
                classify_path(Path::new(&format!("sample.{ext}"))),
                Some(MediaCategory::Raw),
                "raw ext {ext}"
            );
            assert!(is_raw_extension(ext));
        }
        for ext in VIDEO_EXTENSIONS {
            assert_eq!(
                classify_path(Path::new(&format!("sample.{ext}"))),
                Some(MediaCategory::Video),
                "video ext {ext}"
            );
        }
        assert_eq!(
            classify_path(Path::new("live.LIVP")),
            Some(MediaCategory::Livp)
        );
        assert!(is_heic_extension(".HEIC"));
        assert!(is_heic_extension("heif"));
        assert!(!is_heic_extension("jpg"));
        assert!(!is_raw_extension("jpg"));
    }

    #[test]
    fn ignores_junk_and_partial_entries() {
        for name in [
            ".DS_Store",
            "._IMG_001.JPG",
            "Thumbs.db",
            "desktop.ini",
            "@eaDir",
            ".git",
            ".staging",
            "upload.tmp",
            "video.PART",
            "download.crdownload",
        ] {
            assert!(is_ignored_entry(name), "expected ignored: {name}");
            assert_eq!(classify_path(Path::new(name)), None);
        }
        assert!(!is_ignored_entry("IMG_0001.jpg"));
        assert_eq!(classify_path(Path::new("notes.txt")), None);
    }

    #[test]
    fn maps_mime_types() {
        assert_eq!(mime_for_extension(".JPG"), "image/jpeg");
        assert_eq!(mime_for_extension("heic"), "image/heic");
        assert_eq!(mime_for_extension("mov"), "video/quicktime");
        assert_eq!(mime_for_extension("cr3"), "image/x-canon-cr3");
        assert_eq!(mime_for_extension("livp"), "application/zip");
        assert_eq!(mime_for_extension("unknown"), "application/octet-stream");
    }
}
