//! Media classification, metadata extraction, color management, RAW/HEIC/JXL decoding,
//! thumbnail/preview WebP generation, offline geocoding, OS volume identity, and FFmpeg video pipelines.

pub mod classify;
pub mod color;
pub mod decode;
pub mod geocode;
pub mod metadata;
pub mod raw;
pub mod video;
pub mod volume;

use std::path::Path;

pub use classify::{
    classify_extension, classify_path, is_heic_extension, is_ignored_entry, is_raw_extension,
    mime_for_extension, normalized_extension, MediaCategory,
};
pub use color::{bake_icc_to_srgb, composite_to_rgb, compute_histogram};
pub use decode::{
    compute_dhash64, compute_dhash64_from_file, decode_image_to_srgb, dhash_hamming_distance,
    encode_webp_resized, generate_preview_webp, generate_thumb_and_histogram, GeneratedThumb,
};
pub use geocode::{reverse_geocode, GeoLocation};
pub use metadata::{
    extract_livp, extract_livp_entry_bytes, extract_metadata, inspect_livp, is_editing_software,
    parse_android_motion_photo, parse_apple_makernote_content_id, parse_isobmff_content_id,
    parse_isobmff_content_id_bytes, parse_subsec_ms, ExtractedMetadata, LivpContents, LivpEntries,
};
pub use raw::{demosaic_raw_to_jpeg, extract_embedded_raw_jpeg, try_libraw_demosaic_jpeg};
pub use video::{
    decide_playback_mode, extract_video_poster_frame, transcode_to_hls, video_capabilities,
    VideoCapabilities,
};
pub use volume::{resolve_volume_identity, VolumeInfo};

/// Errors returned by media decoding and thumbnail pipelines.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported media format: {0}")]
    UnsupportedFormat(String),

    #[error("not implemented in Phase 0: {0}")]
    Unimplemented(&'static str),

    #[error("decode error: {0}")]
    Decode(String),

    #[error("metadata error: {0}")]
    Metadata(String),

    #[error("color transform error: {0}")]
    ColorTransform(String),

    #[error("external tool `{tool}` failed: {message}")]
    ExternalTool { tool: &'static str, message: String },

    #[error("archive error: {0}")]
    Archive(String),
}

/// Decodes image/video metadata and frames.
pub trait MediaDecoder: Send + Sync {
    fn is_supported_extension(&self, ext: &str) -> bool;
    fn probe_dimensions(&self, path: &Path) -> Result<(u32, u32), MediaError>;
}

/// Generates WebP thumbnails for indexed media assets.
pub trait ThumbnailGenerator: Send + Sync {
    fn generate_thumbnail(
        &self,
        source: &Path,
        dest: &Path,
        max_edge: u32,
    ) -> Result<(), MediaError>;
}

/// Resolves stable OS-level volume identity for removable/external storage tracking.
pub trait VolumeIdentity: Send + Sync {
    fn resolve(&self, path: &Path) -> VolumeInfo;
}

/// Default Phase 1 `MediaDecoder` implementation backed by `classify` and `metadata`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultMediaDecoder;

impl MediaDecoder for DefaultMediaDecoder {
    fn is_supported_extension(&self, ext: &str) -> bool {
        classify_extension(ext).is_some()
    }

    fn probe_dimensions(&self, path: &Path) -> Result<(u32, u32), MediaError> {
        let meta = extract_metadata(path)?;
        match (meta.width, meta.height) {
            (Some(w), Some(h)) => Ok((w, h)),
            _ => {
                let rgb = decode_image_to_srgb(path)?;
                Ok(rgb.dimensions())
            }
        }
    }
}

/// Default Phase 1 `ThumbnailGenerator` implementation backed by `decode::generate_preview_webp`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultThumbnailGenerator;

impl ThumbnailGenerator for DefaultThumbnailGenerator {
    fn generate_thumbnail(
        &self,
        source: &Path,
        dest: &Path,
        max_edge: u32,
    ) -> Result<(), MediaError> {
        let _ = generate_preview_webp(source, dest, max_edge)?;
        Ok(())
    }
}

/// Phase 0 stub implementation of `MediaDecoder` (retained for backwards compatibility).
#[derive(Debug, Default, Clone, Copy)]
pub struct StubMediaDecoder;

impl MediaDecoder for StubMediaDecoder {
    fn is_supported_extension(&self, ext: &str) -> bool {
        matches!(
            ext.trim()
                .trim_start_matches('.')
                .to_ascii_lowercase()
                .as_str(),
            "jpg"
                | "jpeg"
                | "png"
                | "webp"
                | "heic"
                | "heif"
                | "avif"
                | "cr3"
                | "cr2"
                | "nef"
                | "arw"
                | "dng"
                | "raf"
                | "mp4"
                | "mov"
                | "m4v"
        )
    }

    fn probe_dimensions(&self, _path: &Path) -> Result<(u32, u32), MediaError> {
        Err(MediaError::Unimplemented("MediaDecoder::probe_dimensions"))
    }
}

/// Phase 0 stub implementation of `ThumbnailGenerator` (retained for backwards compatibility).
#[derive(Debug, Default, Clone, Copy)]
pub struct StubThumbnailGenerator;

impl ThumbnailGenerator for StubThumbnailGenerator {
    fn generate_thumbnail(
        &self,
        _source: &Path,
        _dest: &Path,
        _max_edge: u32,
    ) -> Result<(), MediaError> {
        Err(MediaError::Unimplemented(
            "ThumbnailGenerator::generate_thumbnail",
        ))
    }
}

/// Default `VolumeIdentity` implementation backed by `volume::resolve_volume_identity`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemVolumeIdentity;

impl VolumeIdentity for SystemVolumeIdentity {
    fn resolve(&self, path: &Path) -> VolumeInfo {
        resolve_volume_identity(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_media_decoder_recognizes_extensions_and_returns_unimplemented() {
        let decoder = StubMediaDecoder;
        assert!(decoder.is_supported_extension("HEIC"));
        assert!(decoder.is_supported_extension(".cr3"));
        assert!(decoder.is_supported_extension("mp4"));
        assert!(!decoder.is_supported_extension("txt"));

        let err = decoder
            .probe_dimensions(Path::new("photo.jpg"))
            .unwrap_err();
        assert!(matches!(err, MediaError::Unimplemented(_)));
    }

    #[test]
    fn stub_thumbnail_generator_returns_unimplemented() {
        let generator = StubThumbnailGenerator;
        let err = generator
            .generate_thumbnail(Path::new("in.jpg"), Path::new("out.webp"), 400)
            .unwrap_err();
        assert!(matches!(err, MediaError::Unimplemented(_)));
    }

    #[test]
    fn system_volume_identity_does_not_panic_on_missing_paths() {
        let resolver = SystemVolumeIdentity;
        let info = resolver.resolve(Path::new("/nonexistent/truongphoto/folder"));
        assert_eq!(info, VolumeInfo::default());
    }
}
