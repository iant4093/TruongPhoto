use crate::classify::{self, MediaCategory};
use crate::color::{bake_icc_to_srgb, composite_to_rgb, compute_histogram};
use crate::metadata;
use crate::raw;
use crate::video;
use crate::MediaError;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage};
use jxl_oxide::JxlImage;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use truongphoto_core::Histogram256;

/// Thumbnail generation result containing oriented pixel dimensions, WebP size, RGB/Luma histogram,
/// and 64-bit perceptual difference hash (`dhash64`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedThumb {
    pub width: u32,
    pub height: u32,
    pub thumb_bytes_len: u64,
    pub histogram: Histogram256,
    pub dhash64: i64,
}

/// Computes a 64-bit horizontal perceptual difference hash (`dHash`) from an RGB image.
///
/// Resizes to 9x8 grayscale and sets bit `y * 8 + x` when `luma(x, y) > luma(x + 1, y)`.
/// Robust to color grading, exposure push, recompression, and moderate resolution changes.
pub fn compute_dhash64(rgb: &RgbImage) -> i64 {
    let small = image::imageops::resize(rgb, 9, 8, FilterType::Triangle);
    let mut hash: u64 = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let p1 = small.get_pixel(x, y);
            let p2 = small.get_pixel(x + 1, y);
            let l1 =
                (u32::from(p1[0]) * 299 + u32::from(p1[1]) * 587 + u32::from(p1[2]) * 114) / 1000;
            let l2 =
                (u32::from(p2[0]) * 299 + u32::from(p2[1]) * 587 + u32::from(p2[2]) * 114) / 1000;
            if l1 > l2 {
                hash |= 1u64 << (y * 8 + x);
            }
        }
    }
    hash as i64
}

/// Hamming distance (`0..=64`) between two 64-bit perceptual difference hashes.
pub const fn dhash_hamming_distance(a: i64, b: i64) -> u32 {
    ((a as u64) ^ (b as u64)).count_ones()
}

/// Computes a 64-bit perceptual difference hash from an existing thumbnail/image file on disk.
pub fn compute_dhash64_from_file(path: &Path) -> Option<i64> {
    let file = File::open(path).ok()?;
    let reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?;
    Some(compute_dhash64(&img.to_rgb8()))
}

fn decode_standard_image_reader(
    reader: ImageReader<BufReader<File>>,
) -> Result<RgbImage, MediaError> {
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| MediaError::Decode(format!("image decoder init failed: {e}")))?;

    let orientation = decoder.orientation().ok();
    let icc_profile = decoder.icc_profile().ok().flatten();

    let mut dyn_img = DynamicImage::from_decoder(decoder)
        .map_err(|e| MediaError::Decode(format!("image decode failed: {e}")))?;

    if let Some(orient) = orientation {
        dyn_img.apply_orientation(orient);
    }

    let mut rgb = composite_to_rgb(dyn_img);
    if let Some(icc) = icc_profile.as_deref() {
        if !icc.is_empty() {
            if let Err(err) = bake_icc_to_srgb(rgb.as_mut(), icc) {
                tracing::debug!(error = %err, "skipping non-RGB or unsupported ICC profile bake");
            }
        }
    }
    Ok(rgb)
}

fn decode_jxl_to_srgb(path: &Path) -> Result<RgbImage, MediaError> {
    let image = JxlImage::builder()
        .open(path)
        .map_err(|e| MediaError::Decode(format!("JXL open failed: {e}")))?;
    let icc = image.original_icc().map(|b| b.to_vec());
    let render = image
        .render_frame(0)
        .map_err(|e| MediaError::Decode(format!("JXL render failed: {e}")))?;

    let mut stream = render.stream_no_alpha();
    let width = stream.width();
    let height = stream.height();
    let channels = stream.channels() as usize;

    const MAX_DECODE_PIXELS: usize = 150_000_000;
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| MediaError::Decode("JXL dimensions overflow".to_string()))?;
    if pixel_count == 0 || pixel_count > MAX_DECODE_PIXELS {
        return Err(MediaError::Decode(format!(
            "JXL dimensions {width}x{height} exceed safety limit"
        )));
    }
    let sample_len = pixel_count
        .checked_mul(channels)
        .ok_or_else(|| MediaError::Decode("JXL buffer length overflow".to_string()))?;

    let mut raw_samples = vec![0u8; sample_len];
    stream.write_to_buffer(&mut raw_samples);

    let mut rgb_bytes = if channels == 3 {
        raw_samples
    } else if channels == 1 {
        let mut expanded = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for g in raw_samples {
            expanded.extend_from_slice(&[g, g, g]);
        }
        expanded
    } else if channels >= 4 {
        let mut dropped = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for chunk in raw_samples.chunks_exact(channels) {
            dropped.extend_from_slice(&[chunk[0], chunk[1], chunk[2]]);
        }
        dropped
    } else {
        return Err(MediaError::Decode(format!(
            "unsupported JXL channel count {channels}"
        )));
    };

    if let Some(icc_bytes) = icc.as_deref() {
        if !icc_bytes.is_empty() {
            let _ = bake_icc_to_srgb(&mut rgb_bytes, icc_bytes);
        }
    }

    RgbImage::from_raw(width, height, rgb_bytes)
        .ok_or_else(|| MediaError::Decode("JXL buffer size mismatch".to_string()))
}

fn decode_via_ffmpeg_png(path: &Path) -> Result<RgbImage, MediaError> {
    let ffmpeg = video::video_capabilities()
        .ffmpeg_path
        .as_deref()
        .ok_or_else(|| MediaError::ExternalTool {
            tool: "ffmpeg",
            message: "ffmpeg executable not found for fallback decode".to_string(),
        })?;
    let mut cmd = std::process::Command::new(ffmpeg);
    video::hide_console_window(&mut cmd);
    cmd.args(["-v", "error", "-i"]).arg(path).args([
        "-frames:v",
        "1",
        "-f",
        "image2pipe",
        "-vcodec",
        "png",
        "-",
    ]);
    let out = truongphoto_core::process::run_command_with_timeout(
        &mut cmd,
        std::time::Duration::from_secs(30),
    )?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(MediaError::ExternalTool {
            tool: "ffmpeg",
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let dyn_img = image::load_from_memory(&out.stdout)
        .map_err(|e| MediaError::Decode(format!("ffmpeg PNG fallback decode failed: {e}")))?;
    Ok(composite_to_rgb(dyn_img))
}

fn decode_jpeg_bytes_with_exif_and_icc(
    jpeg_bytes: &[u8],
    fallback_orientation: Option<image::metadata::Orientation>,
) -> Result<RgbImage, MediaError> {
    let cursor_reader = ImageReader::new(std::io::Cursor::new(jpeg_bytes))
        .with_guessed_format()
        .map_err(|e| MediaError::Decode(format!("JPEG reader init failed: {e}")))?;
    let mut decoder = cursor_reader
        .into_decoder()
        .map_err(|e| MediaError::Decode(format!("JPEG decoder init failed: {e}")))?;
    let embedded_orient = decoder.orientation().ok();
    let icc = decoder.icc_profile().ok().flatten();
    let mut dyn_img = DynamicImage::from_decoder(decoder)
        .map_err(|e| MediaError::Decode(format!("JPEG pixel decode failed: {e}")))?;
    let effective_orient = match embedded_orient {
        Some(o) => Some(o),
        None => fallback_orientation,
    };
    if let Some(orient) = effective_orient {
        dyn_img.apply_orientation(orient);
    }
    let mut rgb = composite_to_rgb(dyn_img);
    if let Some(icc_bytes) = icc.as_deref() {
        let _ = bake_icc_to_srgb(rgb.as_mut(), icc_bytes);
    }
    Ok(rgb)
}

fn exif_orientation_from_file(path: &Path) -> Option<image::metadata::Orientation> {
    let mut parser = nom_exif::MediaParser::new();
    let ms = nom_exif::MediaSource::open(path).ok()?;
    let iter = parser.parse_exif(ms).ok()?;
    let exif: nom_exif::Exif = iter.into();
    let raw_val = match exif.get(nom_exif::ExifTag::Orientation)? {
        nom_exif::EntryValue::U16(v) => *v as u8,
        nom_exif::EntryValue::U8(v) => *v,
        nom_exif::EntryValue::U32(v) => u8::try_from(*v).ok()?,
        _ => return None,
    };
    image::metadata::Orientation::from_exif(raw_val)
}

fn decode_heic_to_srgb(path: &Path) -> Result<RgbImage, MediaError> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(jpeg_bytes) = raw::sips_convert_to_jpeg(path, 92) {
            let fallback = exif_orientation_from_file(path);
            if let Ok(rgb) = decode_jpeg_bytes_with_exif_and_icc(&jpeg_bytes, fallback) {
                return Ok(rgb);
            }
        }
    }

    decode_via_ffmpeg_png(path)
}

fn decode_raw_preview_to_srgb(path: &Path) -> Result<RgbImage, MediaError> {
    let outer_orient = exif_orientation_from_file(path);
    if let Some(jpeg_bytes) = raw::extract_embedded_raw_jpeg(path) {
        if let Ok(rgb) = decode_jpeg_bytes_with_exif_and_icc(&jpeg_bytes, outer_orient) {
            return Ok(rgb);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(jpeg_bytes) = raw::sips_convert_to_jpeg(path, 92) {
            if let Ok(rgb) = decode_jpeg_bytes_with_exif_and_icc(&jpeg_bytes, outer_orient) {
                return Ok(rgb);
            }
        }
    }

    if let Some(jpeg_bytes) = raw::try_libraw_demosaic_jpeg(path) {
        // LibRaw's `dcraw_process` already applies EXIF orientation to the output RGB pixels;
        // do not pass `outer_orient` or portrait RAWs would be rotated twice.
        if let Ok(rgb) = decode_jpeg_bytes_with_exif_and_icc(&jpeg_bytes, None) {
            return Ok(rgb);
        }
    }

    decode_via_ffmpeg_png(path)
}

fn decode_livp_to_srgb(path: &Path) -> Result<RgbImage, MediaError> {
    let entries = metadata::inspect_livp(path)?;
    let bytes = metadata::extract_livp_entry_bytes(path, &entries.still_entry)?;
    let ext = classify::normalized_extension(Path::new(&entries.still_entry)).unwrap_or_default();

    if classify::is_heic_extension(&ext) || ext == "jxl" || classify::is_raw_extension(&ext) {
        let mut tmp = tempfile::Builder::new()
            .prefix("tp_livp_")
            .suffix(&format!(".{ext}"))
            .tempfile()?;
        std::io::Write::write_all(&mut tmp, &bytes)?;
        std::io::Write::flush(&mut tmp)?;
        // Close the open write handle before spawning external decoders (required on Windows).
        let tmp_path = tmp.into_temp_path();
        return decode_image_to_srgb(&tmp_path);
    }

    let reader = ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| MediaError::Decode(format!("livp still format guess failed: {e}")))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| MediaError::Decode(format!("livp still decoder failed: {e}")))?;
    let orientation = decoder.orientation().ok();
    let icc = decoder.icc_profile().ok().flatten();
    let mut dyn_img = DynamicImage::from_decoder(decoder)
        .map_err(|e| MediaError::Decode(format!("livp still decode failed: {e}")))?;
    if let Some(orient) = orientation {
        dyn_img.apply_orientation(orient);
    }
    let mut rgb = composite_to_rgb(dyn_img);
    if let Some(icc_bytes) = icc.as_deref() {
        let _ = bake_icc_to_srgb(rgb.as_mut(), icc_bytes);
    }
    Ok(rgb)
}

/// Decodes any supported photo, RAW, `.livp`, or video poster frame into an oriented, sRGB-baked `RgbImage`.
pub fn decode_image_to_srgb(path: &Path) -> Result<RgbImage, MediaError> {
    let category = classify::classify_path(path)
        .ok_or_else(|| MediaError::UnsupportedFormat(path.display().to_string()))?;
    let ext = classify::normalized_extension(path).unwrap_or_default();

    match category {
        MediaCategory::Video => video::extract_video_poster_frame(path),
        MediaCategory::Livp => decode_livp_to_srgb(path),
        MediaCategory::Raw => decode_raw_preview_to_srgb(path),
        MediaCategory::Photo => {
            if ext == "jxl" {
                decode_jxl_to_srgb(path)
            } else if classify::is_heic_extension(&ext) {
                decode_heic_to_srgb(path)
            } else {
                let file = File::open(path)?;
                let reader = ImageReader::new(BufReader::new(file))
                    .with_guessed_format()
                    .map_err(|e| MediaError::Decode(format!("format guess failed: {e}")))?;
                match decode_standard_image_reader(reader) {
                    Ok(rgb) => Ok(rgb),
                    Err(err) if ext == "avif" => {
                        tracing::debug!(error = %err, "trying ffmpeg fallback for AVIF");
                        decode_via_ffmpeg_png(path)
                    }
                    Err(err) => Err(err),
                }
            }
        }
    }
}

/// Resizes `rgb` proportionally if its longest edge exceeds `max_edge` and encodes lossy WebP.
pub fn encode_webp_resized(
    rgb: &RgbImage,
    max_edge: u32,
    quality: f32,
) -> Result<Vec<u8>, MediaError> {
    let (w, h) = rgb.dimensions();
    if w == 0 || h == 0 {
        return Err(MediaError::Decode(
            "cannot encode empty image to WebP".to_string(),
        ));
    }

    let max_dim = w.max(h);
    let owned_resized: RgbImage;
    let target: &RgbImage = if max_edge > 0 && max_dim > max_edge {
        let scale = (max_edge as f64) / (max_dim as f64);
        let new_w = ((w as f64) * scale).round().max(1.0) as u32;
        let new_h = ((h as f64) * scale).round().max(1.0) as u32;
        owned_resized = image::imageops::resize(rgb, new_w, new_h, FilterType::Lanczos3);
        &owned_resized
    } else {
        rgb
    };

    let encoder = webp::Encoder::from_rgb(target.as_raw(), target.width(), target.height());
    let webp_mem = encoder.encode(quality);
    Ok(webp_mem.to_vec())
}

fn atomic_write_bytes(dest: &Path, bytes: &[u8]) -> Result<(), MediaError> {
    truongphoto_core::process::atomic_write_bytes(dest, bytes).map_err(MediaError::Io)
}

/// Generates a persistent 400px lossy WebP (Q80) thumbnail atomically and computes the 256-bin RGB+Luma histogram.
pub fn generate_thumb_and_histogram(
    src: &Path,
    thumb_out: &Path,
) -> Result<GeneratedThumb, MediaError> {
    let rgb = decode_image_to_srgb(src)?;
    let histogram = compute_histogram(&rgb);
    let dhash64 = compute_dhash64(&rgb);
    let webp_bytes = encode_webp_resized(&rgb, 400, 80.0)?;
    atomic_write_bytes(thumb_out, &webp_bytes)?;
    Ok(GeneratedThumb {
        width: rgb.width(),
        height: rgb.height(),
        thumb_bytes_len: webp_bytes.len() as u64,
        histogram,
        dhash64,
    })
}

/// Generates a lossy WebP (Q85) lightbox preview capped at `max_edge` atomically, returning written byte length.
pub fn generate_preview_webp(
    src: &Path,
    preview_out: &Path,
    max_edge: u32,
) -> Result<u64, MediaError> {
    let rgb = decode_image_to_srgb(src)?;
    let webp_bytes = encode_webp_resized(&rgb, max_edge, 85.0)?;
    atomic_write_bytes(preview_out, &webp_bytes)?;
    Ok(webp_bytes.len() as u64)
}
