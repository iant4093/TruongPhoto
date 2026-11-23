use crate::MediaError;
use image::codecs::jpeg::JpegEncoder;
use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::fs::File;
use std::io::Read;
use std::path::Path;

const MAX_RAW_SCAN_BYTES: u64 = 64 * 1024 * 1024;

#[repr(C)]
struct LibRawProcessedImage {
    image_type: c_int,
    height: u16,
    width: u16,
    colors: u16,
    bits: u16,
    data_size: c_uint,
    data: [u8; 1],
}

type FnLibrawInit = unsafe extern "C" fn(flags: c_uint) -> *mut c_void;
type FnLibrawOpenFile = unsafe extern "C" fn(lr: *mut c_void, file: *const c_char) -> c_int;
#[cfg(target_os = "windows")]
type FnLibrawOpenWFile = unsafe extern "C" fn(lr: *mut c_void, file: *const u16) -> c_int;
type FnLibrawUnpack = unsafe extern "C" fn(lr: *mut c_void) -> c_int;
type FnLibrawDcrawProcess = unsafe extern "C" fn(lr: *mut c_void) -> c_int;
type FnLibrawMakeMemImage =
    unsafe extern "C" fn(lr: *mut c_void, errc: *mut c_int) -> *mut LibRawProcessedImage;
type FnLibrawClearMem = unsafe extern "C" fn(img: *mut LibRawProcessedImage);
type FnLibrawClose = unsafe extern "C" fn(lr: *mut c_void);

struct LibRawHandle {
    ptr: *mut c_void,
    close_fn: FnLibrawClose,
}

impl Drop for LibRawHandle {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.close_fn)(self.ptr) };
        }
    }
}

struct LibRawMemGuard {
    ptr: *mut LibRawProcessedImage,
    clear_fn: FnLibrawClearMem,
}

impl Drop for LibRawMemGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.clear_fn)(self.ptr) };
        }
    }
}

fn libraw_candidate_paths() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(custom) = std::env::var("TP_LIBRAW_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_string());
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.extend([
            "libraw.dylib".to_string(),
            "/opt/homebrew/lib/libraw.dylib".to_string(),
            "/usr/local/lib/libraw.dylib".to_string(),
        ]);
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for rel in ["libraw.dll", "raw.dll", "resources\\libraw.dll"] {
                    let p = dir.join(rel);
                    if p.is_file() {
                        candidates.push(p.to_string_lossy().into_owned());
                    }
                }
            }
        }
        candidates.extend(["libraw.dll".to_string(), "raw.dll".to_string()]);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        candidates.extend([
            "libraw.so".to_string(),
            "libraw.so.23".to_string(),
            "libraw.so.20".to_string(),
        ]);
    }
    candidates
}

fn load_libraw_library() -> Option<libloading::Library> {
    for candidate in libraw_candidate_paths() {
        if let Ok(lib) = unsafe { libloading::Library::new(&candidate) } {
            return Some(lib);
        }
    }
    None
}

/// Attempts runtime dynamic loading and full demosaic via `libraw` (`libloading`), returning `None` if unavailable.
pub fn try_libraw_demosaic_jpeg(path: &Path) -> Option<Vec<u8>> {
    let lib = load_libraw_library()?;

    unsafe {
        let init: libloading::Symbol<'_, FnLibrawInit> = lib.get(b"libraw_init\0").ok()?;
        let unpack: libloading::Symbol<'_, FnLibrawUnpack> = lib.get(b"libraw_unpack\0").ok()?;
        let process: libloading::Symbol<'_, FnLibrawDcrawProcess> =
            lib.get(b"libraw_dcraw_process\0").ok()?;
        let make_mem: libloading::Symbol<'_, FnLibrawMakeMemImage> =
            lib.get(b"libraw_dcraw_make_mem_image\0").ok()?;
        let clear_mem: libloading::Symbol<'_, FnLibrawClearMem> =
            lib.get(b"libraw_dcraw_clear_mem\0").ok()?;
        let close: libloading::Symbol<'_, FnLibrawClose> = lib.get(b"libraw_close\0").ok()?;

        let raw_ptr = init(0);
        if raw_ptr.is_null() {
            return None;
        }
        let handle = LibRawHandle {
            ptr: raw_ptr,
            close_fn: *close,
        };

        let open_rc = {
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::ffi::OsStrExt;
                if let Ok(open_wfile) = lib.get::<FnLibrawOpenWFile>(b"libraw_open_wfile\0") {
                    let wide: Vec<u16> = path
                        .as_os_str()
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();
                    open_wfile(handle.ptr, wide.as_ptr())
                } else {
                    let open_file: libloading::Symbol<'_, FnLibrawOpenFile> =
                        lib.get(b"libraw_open_file\0").ok()?;
                    let c_path = CString::new(path.to_str()?).ok()?;
                    open_file(handle.ptr, c_path.as_ptr())
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let open_file: libloading::Symbol<'_, FnLibrawOpenFile> =
                    lib.get(b"libraw_open_file\0").ok()?;
                let c_path = CString::new(path.to_str()?).ok()?;
                open_file(handle.ptr, c_path.as_ptr())
            }
        };
        if open_rc != 0 {
            return None;
        }
        if unpack(handle.ptr) != 0 {
            return None;
        }
        if process(handle.ptr) != 0 {
            return None;
        }

        let mut errc: c_int = 0;
        let mem_ptr = make_mem(handle.ptr, &mut errc);
        if mem_ptr.is_null() || errc != 0 {
            return None;
        }
        let mem_guard = LibRawMemGuard {
            ptr: mem_ptr,
            clear_fn: *clear_mem,
        };

        let img = &*mem_guard.ptr;
        let data_len = img.data_size as usize;
        if data_len == 0 {
            return None;
        }
        let pixels = std::slice::from_raw_parts(img.data.as_ptr(), data_len);

        // LIBRAW_IMAGE_JPEG = 1, LIBRAW_IMAGE_BITMAP = 2
        if img.image_type == 1 {
            return Some(pixels.to_vec());
        }
        if img.image_type == 2 && img.colors == 3 && img.bits == 8 {
            let expected = (img.width as usize) * (img.height as usize) * 3;
            if pixels.len() < expected {
                return None;
            }
            let mut jpeg_out = Vec::new();
            let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_out, 95);
            encoder
                .encode(
                    &pixels[..expected],
                    img.width as u32,
                    img.height as u32,
                    image::ExtendedColorType::Rgb8,
                )
                .ok()?;
            return Some(jpeg_out);
        }
    }
    None
}

fn read_u16_endian(bytes: &[u8], offset: usize, little: bool) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(if little {
        u16::from_le_bytes([slice[0], slice[1]])
    } else {
        u16::from_be_bytes([slice[0], slice[1]])
    })
}

fn read_u32_endian(bytes: &[u8], offset: usize, little: bool) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(if little {
        u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
    } else {
        u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]])
    })
}

fn collect_tiff_ifd_jpegs<'a>(
    data: &'a [u8],
    ifd_offset: usize,
    little: bool,
    depth: u8,
    best: &mut Option<&'a [u8]>,
) {
    if depth > 4 || ifd_offset + 2 > data.len() {
        return;
    }
    let Some(count) = read_u16_endian(data, ifd_offset, little) else {
        return;
    };

    let mut jpg_offset: Option<usize> = None;
    let mut jpg_len: Option<usize> = None;
    let mut strip_offset: Option<usize> = None;
    let mut strip_len: Option<usize> = None;

    for idx in 0..count as usize {
        let entry = ifd_offset + 2 + idx * 12;
        if entry + 12 > data.len() {
            break;
        }
        let Some(tag) = read_u16_endian(data, entry, little) else {
            break;
        };
        let Some(field_type) = read_u16_endian(data, entry + 2, little) else {
            break;
        };
        let Some(val_count) = read_u32_endian(data, entry + 4, little) else {
            break;
        };
        let Some(val_or_off) = read_u32_endian(data, entry + 8, little) else {
            break;
        };

        match tag {
            0x0201 => jpg_offset = Some(val_or_off as usize),
            0x0202 => jpg_len = Some(val_or_off as usize),
            0x0111 if val_count == 1 => strip_offset = Some(val_or_off as usize),
            0x0117 if val_count == 1 => strip_len = Some(val_or_off as usize),
            // SubIFDs (0x014A) or ExifIFD (0x8769)
            0x014A | 0x8769 => {
                if val_count == 1 {
                    collect_tiff_ifd_jpegs(data, val_or_off as usize, little, depth + 1, best);
                } else if field_type == 4 && val_count <= 16 {
                    let table_off = val_or_off as usize;
                    for i in 0..val_count as usize {
                        if let Some(sub_off) = read_u32_endian(data, table_off + i * 4, little) {
                            collect_tiff_ifd_jpegs(data, sub_off as usize, little, depth + 1, best);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for (off_opt, len_opt) in [(jpg_offset, jpg_len), (strip_offset, strip_len)] {
        if let (Some(off), Some(len)) = (off_opt, len_opt) {
            if len >= 128 && off.saturating_add(len) <= data.len() {
                let candidate = &data[off..off + len];
                if candidate.starts_with(&[0xFF, 0xD8, 0xFF])
                    && best.is_none_or(|cur| candidate.len() > cur.len())
                {
                    *best = Some(candidate);
                }
            }
        }
    }

    // Follow NextIFD pointer
    let next_ptr_off = ifd_offset + 2 + (count as usize) * 12;
    if let Some(next_off) = read_u32_endian(data, next_ptr_off, little) {
        if next_off != 0 && (next_off as usize) != ifd_offset {
            collect_tiff_ifd_jpegs(data, next_off as usize, little, depth + 1, best);
        }
    }
}

/// Walks a JPEG bitstream starting at `start` (`FF D8 FF`) by skipping segment lengths
/// (so embedded EXIF APP1 thumbnails' `FF D9` markers do not prematurely truncate the stream)
/// and scanning entropy-coded data after `FF DA` (SOS) until the true `FF D9` (EOI).
fn find_jpeg_end_offset(data: &[u8], start: usize) -> Option<usize> {
    if start + 4 > data.len() || data[start] != 0xFF || data[start + 1] != 0xD8 {
        return None;
    }
    let mut i = start + 2;
    while i + 1 < data.len() {
        // Advance to next marker prefix 0xFF (skip padding 0xFF bytes).
        if data[i] != 0xFF {
            return None;
        }
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let marker = data[i];
        i += 1;
        match marker {
            0xD9 => return Some(i),         // EOI
            0x01 | 0xD0..=0xD8 => continue, // Standalone markers (TEM, RST0..7, SOI)
            0xDA => {
                // SOS header length + entropy-coded scan until unstuffed non-RST marker.
                if i + 2 > data.len() {
                    return None;
                }
                let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                if seg_len < 2 || i + seg_len > data.len() {
                    return None;
                }
                i += seg_len;
                while i + 1 < data.len() {
                    if data[i] == 0xFF {
                        let next = data[i + 1];
                        if next == 0x00 || (0xD0..=0xD7).contains(&next) {
                            i += 2;
                            continue;
                        }
                        if next == 0xFF {
                            i += 1;
                            continue;
                        }
                        if next == 0xD9 {
                            return Some(i + 2);
                        }
                        // Another marker (e.g., progressive JPEG DHT/SOS/DRI): resume outer marker loop.
                        break;
                    }
                    i += 1;
                }
            }
            _ => {
                if i + 2 > data.len() {
                    return None;
                }
                let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                if seg_len < 2 || i + seg_len > data.len() {
                    return None;
                }
                i += seg_len;
            }
        }
    }
    None
}

fn scan_largest_jpeg_stream(data: &[u8]) -> Option<&[u8]> {
    let mut best: Option<&[u8]> = None;
    let mut pos = 0usize;
    while pos + 4 < data.len() {
        let Some(rel_start) = data[pos..].windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) else {
            break;
        };
        let start = pos + rel_start;
        if let Some(end) = find_jpeg_end_offset(data, start) {
            let candidate = &data[start..end];
            if best.is_none_or(|cur| candidate.len() > cur.len()) {
                best = Some(candidate);
            }
            pos = end;
        } else if let Some(end_rel) = data[start + 3..].windows(2).position(|w| w == [0xFF, 0xD9]) {
            let end = start + 3 + end_rel + 2;
            let candidate = &data[start..end];
            if best.is_none_or(|cur| candidate.len() > cur.len()) {
                best = Some(candidate);
            }
            pos = end;
        } else {
            pos = start + 3;
        }
    }
    best
}

/// Extracts the largest embedded JPEG preview from RAW bytes (TIFF IFD `JpgFromRaw`/`PreviewImage` or embedded stream).
pub fn extract_embedded_raw_jpeg_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let mut best: Option<&[u8]> = None;
    if data.len() >= 8 {
        let little = match &data[0..4] {
            [b'I', b'I', 0x2A, 0x00] => Some(true),
            [b'M', b'M', 0x00, 0x2A] => Some(false),
            _ => None,
        };
        if let Some(little) = little {
            if let Some(ifd0) = read_u32_endian(data, 4, little) {
                collect_tiff_ifd_jpegs(data, ifd0 as usize, little, 0, &mut best);
            }
        }
    }

    if let Some(found) = best {
        if found.len() >= 1024 {
            return Some(found.to_vec());
        }
    }

    if let Some(stream) = scan_largest_jpeg_stream(data) {
        if best.is_none_or(|b| stream.len() > b.len()) {
            return Some(stream.to_vec());
        }
    }

    best.map(|b| b.to_vec())
}

/// Extracts the largest embedded JPEG preview from a RAW file on disk.
pub fn extract_embedded_raw_jpeg(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_RAW_SCAN_BYTES).read_to_end(&mut buf).ok()?;
    extract_embedded_raw_jpeg_bytes(&buf)
}

#[cfg(target_os = "macos")]
pub(crate) fn sips_convert_to_jpeg(path: &Path, quality: u8) -> Result<Vec<u8>, MediaError> {
    let tmp = tempfile::Builder::new()
        .prefix("tp_sips_")
        .suffix(".jpg")
        .tempfile()?;
    let out_path = tmp.path();

    let srgb_icc = Path::new("/System/Library/ColorSync/Profiles/sRGB Profile.icc");
    let q_str = quality.to_string();

    let status = if srgb_icc.exists() {
        std::process::Command::new("/usr/bin/sips")
            .args([
                "-s",
                "format",
                "jpeg",
                "-s",
                "formatOptions",
                &q_str,
                "--matchTo",
                "/System/Library/ColorSync/Profiles/sRGB Profile.icc",
            ])
            .arg(path)
            .arg("--out")
            .arg(out_path)
            .output()
    } else {
        std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "-s", "formatOptions", &q_str])
            .arg(path)
            .arg("--out")
            .arg(out_path)
            .output()
    };

    let output = match status {
        Ok(out) if out.status.success() => out,
        Ok(out) => {
            // Retry without --matchTo in case sips rejects profile matching on some inputs
            let retry = std::process::Command::new("/usr/bin/sips")
                .args(["-s", "format", "jpeg", "-s", "formatOptions", &q_str])
                .arg(path)
                .arg("--out")
                .arg(out_path)
                .output()?;
            if !retry.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(MediaError::ExternalTool {
                    tool: "sips",
                    message: stderr,
                });
            }
            retry
        }
        Err(e) => {
            return Err(MediaError::ExternalTool {
                tool: "sips",
                message: e.to_string(),
            });
        }
    };
    let _ = output;
    let bytes = std::fs::read(out_path)?;
    if bytes.is_empty() {
        return Err(MediaError::Decode("sips produced empty JPEG".to_string()));
    }
    Ok(bytes)
}

/// Demosaics/converts a camera RAW file to JPEG Q95, reporting the decoder backend used (`libraw` | `imageio` | `embedded_preview`).
pub fn demosaic_raw_to_jpeg(path: &Path) -> Result<(Vec<u8>, &'static str), MediaError> {
    if let Some(bytes) = try_libraw_demosaic_jpeg(path) {
        return Ok((bytes, "libraw"));
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(bytes) = sips_convert_to_jpeg(path, 95) {
            return Ok((bytes, "imageio"));
        }
    }

    if let Some(bytes) = extract_embedded_raw_jpeg(path) {
        return Ok((bytes, "embedded_preview"));
    }

    Err(MediaError::Decode(format!(
        "unable to demosaic or extract preview from RAW file {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_embedded_tiff_ifd_jpeg_from_synthetic_raw() {
        let mut fake_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        fake_jpeg.extend_from_slice(&vec![0xAB; 2048]);
        fake_jpeg.extend_from_slice(&[0xFF, 0xD9]);

        // Minimal little-endian TIFF with IFD0 containing JpgFromRaw offset (0x0201) + length (0x0202)
        // Header: 8 bytes ("II*\0" + IFD0 offset 8)
        // IFD0: 2 (count=2) + 2*12 (entries) + 4 (next_ifd=0) = 30 bytes -> JPEG starts at 38
        let jpeg_offset = 38u32;
        let jpeg_len = fake_jpeg.len() as u32;

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());

        // Tag 0x0201 (LONG=4, count=1, value=jpeg_offset)
        tiff.extend_from_slice(&0x0201u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&jpeg_offset.to_le_bytes());

        // Tag 0x0202 (LONG=4, count=1, value=jpeg_len)
        tiff.extend_from_slice(&0x0202u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&jpeg_len.to_le_bytes());

        tiff.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(tiff.len(), jpeg_offset as usize);
        tiff.extend_from_slice(&fake_jpeg);

        let extracted = extract_embedded_raw_jpeg_bytes(&tiff).expect("should extract preview");
        assert_eq!(extracted, fake_jpeg);
    }

    #[test]
    fn missing_libraw_returns_none_cleanly() {
        let res = try_libraw_demosaic_jpeg(Path::new("/nonexistent/sample.cr3"));
        assert!(res.is_none());
    }
}
