use crate::classify::{self, MediaCategory};
use crate::geocode;
use crate::video;
use crate::MediaError;
use chrono::{DateTime, FixedOffset, NaiveDateTime};
use nom_exif::{EntryValue, Exif, ExifTag, MediaParser, MediaSource, TrackInfo, TrackInfoTag};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;
use truongphoto_core::{PhotoMetaView, VideoMetaView};
use zip::ZipArchive;

/// Unified deterministic metadata extracted from photos, RAWs, videos, Motion Photos, and `.livp` archives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtractedMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub capture_at_ms: Option<i64>,
    pub capture_tz_offset_m: Option<i32>,
    pub photo_meta: Option<PhotoMetaView>,
    pub video_meta: Option<VideoMetaView>,
    pub content_identifier: Option<String>,
    pub embedded_video_offset: Option<u64>,
    pub embedded_video_length: Option<u64>,
    pub duration_ms: Option<i64>,
}

/// Entry names discovered inside an Apple `.livp` ZIP archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivpEntries {
    pub still_entry: String,
    pub video_entry: Option<String>,
}

/// Extracted inner still and video payloads from an Apple `.livp` ZIP archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivpContents {
    pub still_name: String,
    pub still_bytes: Vec<u8>,
    pub video_name: Option<String>,
    pub video_bytes: Option<Vec<u8>>,
}

fn read_u16_e(bytes: &[u8], off: usize, little: bool) -> Option<u16> {
    let s = bytes.get(off..off + 2)?;
    Some(if little {
        u16::from_le_bytes([s[0], s[1]])
    } else {
        u16::from_be_bytes([s[0], s[1]])
    })
}

fn read_u32_e(bytes: &[u8], off: usize, little: bool) -> Option<u32> {
    let s = bytes.get(off..off + 4)?;
    Some(if little {
        u32::from_le_bytes([s[0], s[1], s[2], s[3]])
    } else {
        u32::from_be_bytes([s[0], s[1], s[2], s[3]])
    })
}

/// Parses Apple iOS MakerNote tag `0x0011` (`ContentIdentifier`) from raw `ExifTag::MakerNote` bytes.
pub fn parse_apple_makernote_content_id(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"Apple iOS\0") || bytes.len() < 18 {
        return None;
    }

    let (little, ifd_start, base_offset) = match bytes.get(12..14) {
        Some(b"MM") => {
            if bytes.get(14..16) == Some(&[0x00, 0x2A]) {
                let rel = read_u32_e(bytes, 16, false)? as usize;
                (false, 12 + rel, 12usize)
            } else {
                (false, 14usize, 0usize)
            }
        }
        Some(b"II") => {
            if bytes.get(14..16) == Some(&[0x2A, 0x00]) {
                let rel = read_u32_e(bytes, 16, true)? as usize;
                (true, 12 + rel, 12usize)
            } else {
                (true, 14usize, 0usize)
            }
        }
        _ => return None,
    };

    let count = read_u16_e(bytes, ifd_start, little)? as usize;
    let entries_start = ifd_start + 2;

    for i in 0..count {
        let entry = entries_start + i * 12;
        if entry + 12 > bytes.len() {
            break;
        }
        let tag = read_u16_e(bytes, entry, little)?;
        if tag != 0x0011 {
            continue;
        }
        let field_type = read_u16_e(bytes, entry + 2, little)?;
        let val_count = read_u32_e(bytes, entry + 4, little)? as usize;
        if val_count == 0 || !matches!(field_type, 1 | 2 | 7) {
            return None;
        }
        let raw_slice = if val_count <= 4 {
            bytes.get(entry + 8..entry + 8 + val_count)?
        } else {
            let off = read_u32_e(bytes, entry + 8, little)? as usize;
            bytes.get(off..off.checked_add(val_count)?).or_else(|| {
                let adj = base_offset.checked_add(off)?;
                bytes.get(adj..adj.checked_add(val_count)?)
            })?
        };
        let text = String::from_utf8_lossy(raw_slice)
            .trim_matches('\0')
            .trim()
            .to_string();
        return if text.is_empty() { None } else { Some(text) };
    }

    None
}

fn read_box_header_slice(data: &[u8], pos: usize) -> Option<([u8; 4], usize, usize)> {
    if pos + 8 > data.len() {
        return None;
    }
    let size32 =
        u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as u64;
    let box_type = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
    let (header_len, total_size) = if size32 == 1 {
        if pos + 16 > data.len() {
            return None;
        }
        let ext = u64::from_be_bytes([
            data[pos + 8],
            data[pos + 9],
            data[pos + 10],
            data[pos + 11],
            data[pos + 12],
            data[pos + 13],
            data[pos + 14],
            data[pos + 15],
        ]);
        (16usize, ext)
    } else if size32 == 0 {
        (8usize, (data.len() - pos) as u64)
    } else {
        (8usize, size32)
    };

    if total_size < header_len as u64 {
        return None;
    }
    let end = pos.saturating_add(total_size as usize).min(data.len());
    Some((box_type, pos + header_len, end))
}

fn is_known_meta_child(tag: &[u8]) -> bool {
    matches!(
        tag,
        b"hdlr" | b"keys" | b"ilst" | b"mhdr" | b"iinf" | b"pitm" | b"iloc"
    )
}

fn parse_meta_box_content_id(meta_payload: &[u8]) -> Option<String> {
    // ISO BMFF `meta` is a FullBox (4-byte version+flags), whereas Apple QuickTime `meta` omits version+flags.
    let start_offset = if meta_payload.len() >= 12
        && meta_payload[0..4] == [0, 0, 0, 0]
        && is_known_meta_child(&meta_payload[8..12])
    {
        4
    } else if meta_payload.len() >= 8 && is_known_meta_child(&meta_payload[4..8]) {
        0
    } else if meta_payload.len() >= 12 {
        4
    } else {
        0
    };

    let mut keys_payload: Option<&[u8]> = None;
    let mut ilst_payload: Option<&[u8]> = None;

    let mut pos = start_offset;
    while let Some((box_type, content_start, box_end)) = read_box_header_slice(meta_payload, pos) {
        if &box_type == b"keys" {
            keys_payload = Some(&meta_payload[content_start..box_end]);
        } else if &box_type == b"ilst" {
            ilst_payload = Some(&meta_payload[content_start..box_end]);
        }
        if box_end <= pos {
            break;
        }
        pos = box_end;
    }

    let keys = keys_payload?;
    let ilst = ilst_payload?;

    // `keys` box: 4 bytes version/flags + 4 bytes entry_count
    if keys.len() < 8 {
        return None;
    }
    let entry_count = u32::from_be_bytes([keys[4], keys[5], keys[6], keys[7]]) as usize;
    let mut k_pos = 8usize;
    let mut target_index: Option<u32> = None;

    for idx in 1..=entry_count {
        if k_pos + 8 > keys.len() {
            break;
        }
        let k_size = u32::from_be_bytes([
            keys[k_pos],
            keys[k_pos + 1],
            keys[k_pos + 2],
            keys[k_pos + 3],
        ]) as usize;
        if k_size < 8 || k_pos + k_size > keys.len() {
            break;
        }
        let key_name = &keys[k_pos + 8..k_pos + k_size];
        if key_name == b"com.apple.quicktime.content.identifier" {
            target_index = Some(idx as u32);
            break;
        }
        k_pos += k_size;
    }

    let target_tag = target_index?.to_be_bytes();
    let mut i_pos = 0usize;
    while let Some((item_tag, item_start, item_end)) = read_box_header_slice(ilst, i_pos) {
        if item_tag == target_tag {
            let item_slice = &ilst[item_start..item_end];
            let mut d_pos = 0usize;
            while let Some((sub_tag, sub_start, sub_end)) = read_box_header_slice(item_slice, d_pos)
            {
                if &sub_tag == b"data" && sub_start + 8 <= sub_end {
                    let payload = &item_slice[sub_start + 8..sub_end];
                    let text = String::from_utf8_lossy(payload)
                        .trim_matches('\0')
                        .trim()
                        .to_string();
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
                if sub_end <= d_pos {
                    break;
                }
                d_pos = sub_end;
            }
        }
        if item_end <= i_pos {
            break;
        }
        i_pos = item_end;
    }

    None
}

fn parse_moov_box_content_id(moov_payload: &[u8]) -> Option<String> {
    let mut pos = 0usize;
    while let Some((box_type, content_start, box_end)) = read_box_header_slice(moov_payload, pos) {
        if &box_type == b"meta" {
            if let Some(id) = parse_meta_box_content_id(&moov_payload[content_start..box_end]) {
                return Some(id);
            }
        } else if &box_type == b"udta" {
            let udta = &moov_payload[content_start..box_end];
            let mut u_pos = 0usize;
            while let Some((u_tag, u_start, u_end)) = read_box_header_slice(udta, u_pos) {
                if &u_tag == b"meta" {
                    if let Some(id) = parse_meta_box_content_id(&udta[u_start..u_end]) {
                        return Some(id);
                    }
                }
                if u_end <= u_pos {
                    break;
                }
                u_pos = u_end;
            }
        }
        if box_end <= pos {
            break;
        }
        pos = box_end;
    }
    None
}

/// Scans in-memory ISOBMFF/QuickTime bytes (`moov -> meta -> keys / ilst`) for `com.apple.quicktime.content.identifier`.
pub fn parse_isobmff_content_id_bytes(data: &[u8]) -> Option<String> {
    let mut pos = 0usize;
    while let Some((box_type, content_start, box_end)) = read_box_header_slice(data, pos) {
        if &box_type == b"moov" {
            if let Some(id) = parse_moov_box_content_id(&data[content_start..box_end]) {
                return Some(id);
            }
        }
        if box_end <= pos {
            break;
        }
        pos = box_end;
    }
    None
}

/// Streams top-level ISOBMFF boxes from disk to find `moov -> meta -> keys / ilst` `com.apple.quicktime.content.identifier`.
pub fn parse_isobmff_content_id(path: &Path) -> Option<String> {
    const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;
    let file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut reader = BufReader::new(file);
    let mut pos = 0u64;

    while pos + 8 <= file_len {
        reader.seek(SeekFrom::Start(pos)).ok()?;
        let mut hdr = [0u8; 8];
        reader.read_exact(&mut hdr).ok()?;
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let box_type = &hdr[4..8];

        let (hdr_len, total_size) = if size32 == 1 {
            if pos + 16 > file_len {
                break;
            }
            let mut ext_buf = [0u8; 8];
            reader.read_exact(&mut ext_buf).ok()?;
            (16u64, u64::from_be_bytes(ext_buf))
        } else if size32 == 0 {
            (8u64, file_len - pos)
        } else {
            (8u64, size32)
        };

        if total_size < hdr_len {
            break;
        }
        let payload_len = total_size - hdr_len;

        if box_type == b"moov" && payload_len <= MAX_MOOV_BYTES {
            let mut moov = vec![0u8; payload_len as usize];
            reader.read_exact(&mut moov).ok()?;
            return parse_moov_box_content_id(&moov);
        }

        let Some(next) = pos.checked_add(total_size) else {
            break;
        };
        if next <= pos {
            break;
        }
        pos = next;
    }
    None
}

fn extract_xmp_slice(data: &[u8]) -> Option<&str> {
    let find_bytes =
        |needle: &[u8]| -> Option<usize> { data.windows(needle.len()).position(|w| w == needle) };

    if let Some(start) = find_bytes(b"<x:xmpmeta") {
        let end_tag = b"</x:xmpmeta>";
        let end = data[start..]
            .windows(end_tag.len())
            .position(|w| w == end_tag)
            .map(|rel| start + rel + end_tag.len())
            .unwrap_or(data.len());
        return std::str::from_utf8(&data[start..end]).ok();
    }

    if let Some(start) = find_bytes(b"<rdf:RDF") {
        let end_tag = b"</rdf:RDF>";
        let end = data[start..]
            .windows(end_tag.len())
            .position(|w| w == end_tag)
            .map(|rel| start + rel + end_tag.len())
            .unwrap_or(data.len());
        return std::str::from_utf8(&data[start..end]).ok();
    }

    None
}

fn parse_motion_photo_length_from_xmp(xmp: &str) -> Option<u64> {
    let mut reader = Reader::from_str(xmp);
    reader.config_mut().trim_text(true);

    let mut micro_video_offset: Option<u64> = None;
    let mut container_motion_length: Option<u64> = None;
    let mut in_micro_video_offset = false;
    let mut in_item_semantic = false;
    let mut in_item_length = false;
    let mut current_item_semantic: Option<String> = None;
    let mut current_item_length: Option<u64> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name_bytes = e.name();
                let qname = name_bytes.as_ref();

                let mut item_semantic_attr: Option<String> = None;
                let mut item_length_attr: Option<u64> = None;

                for attr in e.attributes().flatten() {
                    let key = attr.key.as_ref();
                    let val = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .ok()?;
                    if key.ends_with("MicroVideoOffset") {
                        if let Ok(v) = val.trim().parse::<u64>() {
                            micro_video_offset = Some(v);
                        }
                    } else if key.ends_with("Semantic") {
                        item_semantic_attr = Some(val.trim().to_string());
                    } else if key.ends_with("Length") {
                        if let Ok(v) = val.trim().parse::<u64>() {
                            item_length_attr = Some(v);
                        }
                    }
                }

                if item_semantic_attr.as_deref() == Some("MotionPhoto") {
                    if let Some(len) = item_length_attr {
                        container_motion_length = Some(len);
                    }
                }

                if qname.ends_with("MicroVideoOffset") {
                    in_micro_video_offset = true;
                } else if qname.ends_with(":Item") || qname == "Item" || qname.ends_with(":li") {
                    if let Some(sem) = item_semantic_attr {
                        current_item_semantic = Some(sem);
                    }
                    if let Some(len) = item_length_attr {
                        current_item_length = Some(len);
                    }
                } else if qname.ends_with("Semantic") {
                    in_item_semantic = true;
                } else if qname.ends_with("Length") {
                    in_item_length = true;
                }
            }
            Ok(Event::Text(t)) => {
                let text = t.trim().to_string();
                if in_micro_video_offset {
                    if let Ok(v) = text.parse::<u64>() {
                        micro_video_offset = Some(v);
                    }
                } else if in_item_semantic {
                    current_item_semantic = Some(text);
                } else if in_item_length {
                    if let Ok(v) = text.parse::<u64>() {
                        current_item_length = Some(v);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name_bytes = e.name();
                let qname = name_bytes.as_ref();
                if qname.ends_with("MicroVideoOffset") {
                    in_micro_video_offset = false;
                } else if qname.ends_with("Semantic") {
                    in_item_semantic = false;
                } else if qname.ends_with("Length") {
                    in_item_length = false;
                } else if qname.ends_with(":Item") || qname == "Item" || qname.ends_with(":li") {
                    if current_item_semantic.as_deref() == Some("MotionPhoto") {
                        if let Some(len) = current_item_length {
                            container_motion_length = Some(len);
                        }
                    }
                    current_item_semantic = None;
                    current_item_length = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    container_motion_length.or(micro_video_offset)
}

/// Parses Android/Google Motion Photo XMP (`GCamera:MicroVideoOffset` or `Container:Directory` `Item:Semantic="MotionPhoto"`)
/// and verifies ISOBMFF `ftyp` magic at `offset + 4..offset + 8`.
pub fn parse_android_motion_photo(data: &[u8]) -> Option<(u64, u64)> {
    let xmp = extract_xmp_slice(data)?;
    let length = parse_motion_photo_length_from_xmp(xmp)?;
    let total_len = data.len() as u64;
    if length < 8 || length >= total_len {
        return None;
    }
    let offset = total_len - length;
    let off_usize = offset as usize;
    if data.get(off_usize + 4..off_usize + 8) != Some(b"ftyp") {
        return None;
    }
    Some((offset, length))
}

fn detect_android_motion_photo_file(path: &Path) -> Option<(u64, u64)> {
    const HEADER_SCAN_BYTES: u64 = 512 * 1024;
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut header = Vec::new();
    file.by_ref()
        .take(HEADER_SCAN_BYTES)
        .read_to_end(&mut header)
        .ok()?;
    let xmp = extract_xmp_slice(&header)?;
    let length = parse_motion_photo_length_from_xmp(xmp)?;
    if length < 8 || length >= file_len {
        return None;
    }
    let offset = file_len - length;
    file.seek(SeekFrom::Start(offset + 4)).ok()?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).ok()?;
    if &magic != b"ftyp" {
        return None;
    }
    Some((offset, length))
}

/// Inspects an Apple `.livp` ZIP archive without extracting its bytes, returning inner still and video entry names.
pub fn inspect_livp(path: &Path) -> Result<LivpEntries, MediaError> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|e| MediaError::Archive(format!("invalid .livp archive: {e}")))?;

    let mut still_entry: Option<String> = None;
    let mut video_entry: Option<String> = None;

    for idx in 0..archive.len() {
        let entry = archive
            .by_index(idx)
            .map_err(|e| MediaError::Archive(format!("failed reading .livp entry: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        let Ok(name_cow) = entry.name() else {
            continue;
        };
        let name = name_cow.into_owned();
        if name.starts_with("__MACOSX/") {
            continue;
        }
        let base_name = Path::new(&name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&name);
        if classify::is_ignored_entry(base_name) {
            continue;
        }
        match classify::classify_path(Path::new(base_name)) {
            Some(MediaCategory::Photo | MediaCategory::Raw) if still_entry.is_none() => {
                still_entry = Some(name);
            }
            Some(MediaCategory::Video) if video_entry.is_none() => {
                video_entry = Some(name);
            }
            _ => {}
        }
    }

    let still_entry = still_entry.ok_or_else(|| {
        MediaError::Archive(format!(
            "no supported still image found inside {}",
            path.display()
        ))
    })?;

    Ok(LivpEntries {
        still_entry,
        video_entry,
    })
}

const MAX_LIVP_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// Extracts a named entry from an Apple `.livp` ZIP archive into memory (bounded to 64 MiB).
pub fn extract_livp_entry_bytes(path: &Path, entry_name: &str) -> Result<Vec<u8>, MediaError> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|e| MediaError::Archive(format!("invalid .livp archive: {e}")))?;
    let entry = archive
        .by_name(entry_name)
        .map_err(|e| MediaError::Archive(format!("missing .livp entry '{entry_name}': {e}")))?;
    let cap = entry.size().min(MAX_LIVP_ENTRY_BYTES) as usize;
    let mut out = Vec::with_capacity(cap);
    entry.take(MAX_LIVP_ENTRY_BYTES + 1).read_to_end(&mut out)?;
    if out.len() as u64 > MAX_LIVP_ENTRY_BYTES {
        return Err(MediaError::Archive(format!(
            ".livp entry '{entry_name}' exceeds maximum allowed size of 64 MiB"
        )));
    }
    Ok(out)
}

/// Extracts both the inner still and optional inner video from an Apple `.livp` archive.
pub fn extract_livp(path: &Path) -> Result<LivpContents, MediaError> {
    let entries = inspect_livp(path)?;
    let still_bytes = extract_livp_entry_bytes(path, &entries.still_entry)?;
    let video_bytes = match entries.video_entry.as_deref() {
        Some(vname) => Some(extract_livp_entry_bytes(path, vname)?),
        None => None,
    };
    Ok(LivpContents {
        still_name: entries.still_entry,
        still_bytes,
        video_name: entries.video_entry,
        video_bytes,
    })
}

fn entry_as_str(exif: &Exif, tag: ExifTag) -> Option<String> {
    match exif.get(tag)? {
        EntryValue::Text(s) => {
            let trimmed = s.trim_matches('\0').trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

fn entry_as_f64(exif: &Exif, tag: ExifTag) -> Option<f64> {
    match exif.get(tag)? {
        EntryValue::URational(r) => r.to_f64(),
        EntryValue::IRational(r) => r.to_f64(),
        EntryValue::F32(v) => Some(*v as f64),
        EntryValue::F64(v) => Some(*v),
        EntryValue::U8(v) => Some(*v as f64),
        EntryValue::U16(v) => Some(*v as f64),
        EntryValue::U32(v) => Some(*v as f64),
        EntryValue::U64(v) => Some(*v as f64),
        EntryValue::I16(v) => Some(*v as f64),
        EntryValue::I32(v) => Some(*v as f64),
        _ => None,
    }
}

fn entry_as_u32(exif: &Exif, tag: ExifTag) -> Option<u32> {
    match exif.get(tag)? {
        EntryValue::U8(v) => Some(*v as u32),
        EntryValue::U16(v) => Some(*v as u32),
        EntryValue::U32(v) => Some(*v),
        EntryValue::U64(v) => u32::try_from(*v).ok(),
        EntryValue::U16Array(arr) => arr.first().copied().map(u32::from),
        EntryValue::U32Array(arr) => arr.first().copied(),
        _ => None,
    }
}

/// Parses an EXIF fractional-second string (`SubSecTimeOriginal` / `SubSecTimeDigitized`) into milliseconds (0..=999).
///
/// EXIF fractional seconds are decimal fractions right-padded with zeros:
/// `"9"` -> `900`, `"09"` -> `90`, `"095"` -> `95`, `"123456"` -> `123`, `"00"` -> `0`.
pub fn parse_subsec_ms(raw: &str) -> Option<i64> {
    let digits: String = raw
        .trim()
        .trim_start_matches('.')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    let ms = match digits.len() {
        1 => digits.parse::<i64>().ok()? * 100,
        2 => digits.parse::<i64>().ok()? * 10,
        _ => digits[..3].parse::<i64>().ok()?,
    };
    Some(ms.clamp(0, 999))
}

/// Returns `true` when an EXIF `Software` / `ProcessingSoftware` string identifies a photo editor
/// rather than in-camera firmware.
pub fn is_editing_software(software: &str) -> bool {
    let low = software.trim().to_ascii_lowercase();
    if low.is_empty() {
        return false;
    }
    const EDITOR_KEYWORDS: &[&str] = &[
        "lightroom",
        "photoshop",
        "adobe",
        "capture one",
        "darktable",
        "rawtherapee",
        "dxo",
        "luminar",
        "affinity",
        "pixelmator",
        "photomator",
        "on1",
        "acorn",
        "gimp",
        "snapseed",
        "vsco",
        "polarr",
        "skylum",
        "topaz",
        "nik collection",
        "silkypix",
        "digital photo professional",
        "imaging edge",
        "nx studio",
        "x raw studio",
        "om workspace",
        "aftershot",
        "acdsee",
        "paintshop",
        "canva",
        "figma",
        "imagemagick",
        "exiftool",
        "apple photos",
        "photos ",
        "preview",
    ];
    EDITOR_KEYWORDS.iter().any(|kw| low.contains(kw))
}

fn parse_single_exif_timestamp(
    exif: &Exif,
    tag: ExifTag,
    subsec_tag: Option<ExifTag>,
    offset_tags: &[ExifTag],
) -> (Option<i64>, Option<i32>, bool) {
    let Some(val) = exif.get(tag) else {
        return (None, None, false);
    };
    let subsec_ms = subsec_tag
        .and_then(|st| entry_as_str(exif, st))
        .and_then(|s| parse_subsec_ms(&s));
    let has_subsec = subsec_ms.is_some();

    let apply_subsec = |base_ms: i64| -> i64 {
        if let Some(ms) = subsec_ms {
            if base_ms % 1000 == 0 {
                return base_ms.saturating_add(ms);
            }
        }
        base_ms
    };

    let resolve_offset = || -> Option<FixedOffset> {
        for ot in offset_tags {
            if let Some(offset_str) = entry_as_str(exif, *ot) {
                if let Ok(offset) = offset_str.parse::<FixedOffset>() {
                    return Some(offset);
                }
            }
        }
        None
    };

    match val {
        EntryValue::DateTime(dt) => (
            Some(apply_subsec(dt.timestamp_millis())),
            Some(dt.offset().local_minus_utc() / 60),
            has_subsec || (dt.timestamp_millis() % 1000 != 0),
        ),
        EntryValue::NaiveDateTime(naive) => {
            if let Some(offset) = resolve_offset() {
                if let Some(dt) = naive.and_local_timezone(offset).single() {
                    return (
                        Some(apply_subsec(dt.timestamp_millis())),
                        Some(offset.local_minus_utc() / 60),
                        has_subsec,
                    );
                }
            }
            (
                Some(apply_subsec(naive.and_utc().timestamp_millis())),
                None,
                has_subsec,
            )
        }
        EntryValue::Text(s) => {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s.trim()) {
                return (
                    Some(apply_subsec(dt.timestamp_millis())),
                    Some(dt.offset().local_minus_utc() / 60),
                    has_subsec || (dt.timestamp_millis() % 1000 != 0),
                );
            }
            if let Ok(naive) = NaiveDateTime::parse_from_str(s.trim(), "%Y:%m:%d %H:%M:%S") {
                if let Some(offset) = resolve_offset() {
                    if let Some(dt) = naive.and_local_timezone(offset).single() {
                        return (
                            Some(apply_subsec(dt.timestamp_millis())),
                            Some(offset.local_minus_utc() / 60),
                            has_subsec,
                        );
                    }
                }
                return (
                    Some(apply_subsec(naive.and_utc().timestamp_millis())),
                    None,
                    has_subsec,
                );
            }
            (None, None, false)
        }
        _ => (None, None, false),
    }
}

fn parse_exif_timestamp(exif: &Exif) -> (Option<i64>, Option<i32>, bool) {
    let candidates: [(ExifTag, Option<ExifTag>, &[ExifTag]); 3] = [
        (
            ExifTag::DateTimeOriginal,
            Some(ExifTag::SubSecTimeOriginal),
            &[ExifTag::OffsetTimeOriginal, ExifTag::OffsetTime],
        ),
        (
            ExifTag::CreateDate,
            Some(ExifTag::SubSecTimeDigitized),
            &[
                ExifTag::OffsetTimeDigitized,
                ExifTag::OffsetTimeOriginal,
                ExifTag::OffsetTime,
            ],
        ),
        (
            ExifTag::ModifyDate,
            Some(ExifTag::SubSecTime),
            &[ExifTag::OffsetTime, ExifTag::OffsetTimeOriginal],
        ),
    ];
    for (tag, subsec_tag, offset_tags) in candidates {
        let (ts, tz, has_subsec) = parse_single_exif_timestamp(exif, tag, subsec_tag, offset_tags);
        if ts.is_some() {
            return (ts, tz, has_subsec);
        }
    }
    (None, None, false)
}

fn parse_exif_modify_timestamp(exif: &Exif) -> Option<i64> {
    parse_single_exif_timestamp(
        exif,
        ExifTag::ModifyDate,
        Some(ExifTag::SubSecTime),
        &[ExifTag::OffsetTime, ExifTag::OffsetTimeOriginal],
    )
    .0
}

fn populate_from_exif(
    exif: &Exif,
) -> (
    PhotoMetaView,
    Option<u32>,
    Option<u32>,
    Option<i32>,
    Option<String>,
) {
    let (taken_at_ms, tz_offset_m, taken_at_has_subsec) = parse_exif_timestamp(exif);
    let exif_modified_at_ms = parse_exif_modify_timestamp(exif);
    let software = entry_as_str(exif, ExifTag::Software)
        .or_else(|| entry_as_str(exif, ExifTag::ProcessingSoftware));
    let orientation = entry_as_u32(exif, ExifTag::Orientation).and_then(|v| u16::try_from(v).ok());

    let color_space = match entry_as_u32(exif, ExifTag::ColorSpace) {
        Some(1) => Some("sRGB".to_string()),
        Some(2) => Some("Adobe RGB".to_string()),
        Some(65535) => Some("Uncalibrated".to_string()),
        _ => None,
    };

    let (gps_lat, gps_lon, gps_alt_m, geo_city, geo_admin1, geo_country, geo_country_code) =
        if let Some(gps) = exif.gps_info() {
            let raw_lat = gps.latitude_decimal();
            let raw_lon = gps.longitude_decimal();
            let (lat, lon) = match (raw_lat, raw_lon) {
                (Some(la), Some(lo))
                    if la.is_finite()
                        && lo.is_finite()
                        && (-90.0..=90.0).contains(&la)
                        && (-180.0..=180.0).contains(&lo)
                        && !(la.abs() < 1e-6 && lo.abs() < 1e-6) =>
                {
                    (Some(la), Some(lo))
                }
                _ => (None, None),
            };
            let alt = if lat.is_some() && lon.is_some() {
                gps.altitude_meters().filter(|a| a.is_finite())
            } else {
                None
            };
            let geo = match (lat, lon) {
                (Some(la), Some(lo)) => geocode::reverse_geocode(la, lo),
                _ => None,
            };
            (
                lat,
                lon,
                alt,
                geo.as_ref().map(|g| g.city.clone()),
                geo.as_ref().map(|g| g.admin1.clone()),
                geo.as_ref().map(|g| g.country.clone()),
                geo.as_ref().map(|g| g.country_code.clone()),
            )
        } else {
            (None, None, None, None, None, None, None)
        };

    let width = entry_as_u32(exif, ExifTag::ExifImageWidth)
        .or_else(|| entry_as_u32(exif, ExifTag::ImageWidth));
    let height = entry_as_u32(exif, ExifTag::ExifImageHeight)
        .or_else(|| entry_as_u32(exif, ExifTag::ImageHeight));

    let content_identifier = match exif.get(ExifTag::MakerNote) {
        Some(EntryValue::Undefined(bytes)) | Some(EntryValue::U8Array(bytes)) => {
            parse_apple_makernote_content_id(bytes)
        }
        _ => None,
    };

    let positive_f64 = |v: Option<f64>| v.filter(|x| x.is_finite() && *x > 0.0);
    let positive_u32 = |v: Option<u32>| v.filter(|x| *x > 0);
    let view = PhotoMetaView {
        camera_make: entry_as_str(exif, ExifTag::Make),
        camera_model: entry_as_str(exif, ExifTag::Model),
        lens_model: entry_as_str(exif, ExifTag::LensModel),
        focal_length_mm: positive_f64(entry_as_f64(exif, ExifTag::FocalLength)),
        focal_length_35mm: positive_u32(entry_as_u32(exif, ExifTag::FocalLengthIn35mmFilm)),
        aperture_f: positive_f64(entry_as_f64(exif, ExifTag::FNumber)),
        shutter_speed_sec: positive_f64(entry_as_f64(exif, ExifTag::ExposureTime)),
        iso: positive_u32(entry_as_u32(exif, ExifTag::ISOSpeedRatings)),
        orientation,
        color_space,
        gps_lat,
        gps_lon,
        gps_alt_m,
        geo_city,
        geo_admin1,
        geo_country,
        geo_country_code,
        taken_at_ms,
        software,
        exif_modified_at_ms,
        taken_at_has_subsec,
    };

    (view, width, height, tz_offset_m, content_identifier)
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    format: Option<FfprobeFormat>,
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    bit_rate: Option<String>,
    bits_per_raw_sample: Option<String>,
    pix_fmt: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
}

fn parse_ffprobe_rate(rate: &str) -> Option<f64> {
    let trimmed = rate.trim();
    if let Some((num_s, den_s)) = trimmed.split_once('/') {
        let num = num_s.parse::<f64>().ok()?;
        let den = den_s.parse::<f64>().ok()?;
        if den > 0.0 && num >= 0.0 {
            return Some(num / den);
        }
        return None;
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}

fn infer_bit_depth(stream: &FfprobeStream) -> Option<u8> {
    if let Some(raw_bits) = stream
        .bits_per_raw_sample
        .as_deref()
        .and_then(|s| s.trim().parse::<u8>().ok())
    {
        if raw_bits > 0 {
            return Some(raw_bits);
        }
    }
    let pix_fmt = stream.pix_fmt.as_deref()?.to_ascii_lowercase();
    if pix_fmt.contains("12le") || pix_fmt.contains("12be") || pix_fmt.contains("p012") {
        Some(12)
    } else if pix_fmt.contains("10le") || pix_fmt.contains("10be") || pix_fmt.contains("p010") {
        Some(10)
    } else {
        Some(8)
    }
}

fn is_hdr_stream(stream: &FfprobeStream, bit_depth: Option<u8>) -> bool {
    let transfer = stream
        .color_transfer
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let primaries = stream
        .color_primaries
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if matches!(
        transfer.as_str(),
        "smpte2084" | "arib-std-b67" | "pq" | "hlg"
    ) {
        return true;
    }

    if primaries == "bt2020" && bit_depth.unwrap_or(8) >= 10 {
        return true;
    }

    stream.side_data_list.iter().any(|sd| {
        sd.get("side_data_type")
            .and_then(|v| v.as_str())
            .is_some_and(|s| {
                let low = s.to_ascii_lowercase();
                low.contains("dovi")
                    || low.contains("dolby vision")
                    || low.contains("mastering display")
                    || low.contains("content light level")
            })
    })
}

fn run_ffprobe(path: &Path) -> Option<FfprobeOutput> {
    let ffprobe = video::video_capabilities().ffprobe_path.as_deref()?;
    let mut cmd = Command::new(ffprobe);
    video::hide_console_window(&mut cmd);
    cmd.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ])
    .arg(path);
    let out = truongphoto_core::process::run_command_with_timeout(
        &mut cmd,
        std::time::Duration::from_secs(30),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn track_str(track: &TrackInfo, tag: TrackInfoTag) -> Option<String> {
    match track.get(tag)? {
        EntryValue::Text(s) => {
            let t = s.trim_matches('\0').trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        _ => None,
    }
}

fn extract_video_metadata(path: &Path) -> ExtractedMetadata {
    let mut meta = ExtractedMetadata::default();
    let mut video_meta = VideoMetaView::default();

    let mut parser = MediaParser::new();
    if let Ok(ms) = MediaSource::open(path) {
        if let Ok(track) = parser.parse_track(ms) {
            if let Some(EntryValue::U64(dur)) = track.get(TrackInfoTag::DurationMs) {
                let d = i64::try_from(*dur).unwrap_or(i64::MAX);
                video_meta.duration_ms = d;
                meta.duration_ms = Some(d);
            }
            if let Some(EntryValue::U32(w)) = track.get(TrackInfoTag::Width) {
                meta.width = Some(*w);
            }
            if let Some(EntryValue::U32(h)) = track.get(TrackInfoTag::Height) {
                meta.height = Some(*h);
            }
            match track.get(TrackInfoTag::CreateDate) {
                Some(EntryValue::DateTime(dt)) => {
                    let ms = dt.timestamp_millis();
                    if ms > 0 {
                        video_meta.taken_at_ms = Some(ms);
                        meta.capture_at_ms = Some(ms);
                        meta.capture_tz_offset_m = Some(dt.offset().local_minus_utc() / 60);
                    }
                }
                Some(EntryValue::NaiveDateTime(naive)) => {
                    let ms = naive.and_utc().timestamp_millis();
                    if ms > 0 {
                        video_meta.taken_at_ms = Some(ms);
                        meta.capture_at_ms = Some(ms);
                    }
                }
                _ => {}
            }
            if let Some(gps) = track.gps_info() {
                if let (Some(la), Some(lo)) = (gps.latitude_decimal(), gps.longitude_decimal()) {
                    let is_valid = la.is_finite()
                        && lo.is_finite()
                        && (-90.0..=90.0).contains(&la)
                        && (-180.0..=180.0).contains(&lo)
                        && !(la.abs() < 1e-4 && lo.abs() < 1e-4);
                    if is_valid {
                        video_meta.gps_lat = Some(la);
                        video_meta.gps_lon = Some(lo);
                        if let Some(geo) = geocode::reverse_geocode(la, lo) {
                            video_meta.geo_city = Some(geo.city);
                            video_meta.geo_admin1 = Some(geo.admin1);
                            video_meta.geo_country = Some(geo.country);
                            video_meta.geo_country_code = Some(geo.country_code);
                        }
                    }
                }
            }
            let _ = track_str(&track, TrackInfoTag::Make);
        }
    }

    meta.content_identifier = parse_isobmff_content_id(path);

    if let Some(probe) = run_ffprobe(path) {
        if let Some(fmt) = &probe.format {
            if video_meta.duration_ms == 0 {
                if let Some(dur_ms) = fmt
                    .duration
                    .as_deref()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .filter(|d| d.is_finite() && *d >= 0.0)
                    .map(|d| (d * 1000.0).round() as i64)
                {
                    video_meta.duration_ms = dur_ms;
                    meta.duration_ms = Some(dur_ms);
                }
            }
            video_meta.bitrate_bps = fmt
                .bit_rate
                .as_deref()
                .and_then(|s| s.trim().parse::<u64>().ok());

            if meta.capture_at_ms.is_none() {
                let raw_creation = fmt
                    .tags
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("com.apple.quicktime.creationdate"))
                    .or_else(|| {
                        fmt.tags
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("creation_time"))
                    })
                    .map(|(_, v)| v.trim());
                if let Some(ts_str) = raw_creation.filter(|s| !s.is_empty()) {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts_str)
                        .or_else(|_| {
                            chrono::DateTime::parse_from_str(ts_str, "%Y-%m-%dT%H:%M:%S%z")
                        })
                        .or_else(|_| {
                            chrono::DateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S%z")
                        })
                    {
                        let ms = dt.timestamp_millis();
                        if ms > 0 {
                            video_meta.taken_at_ms = Some(ms);
                            meta.capture_at_ms = Some(ms);
                            meta.capture_tz_offset_m = Some(dt.offset().local_minus_utc() / 60);
                        }
                    } else if let Ok(naive) =
                        chrono::NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%dT%H:%M:%S%.f")
                            .or_else(|_| {
                                chrono::NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S")
                            })
                    {
                        let ms = naive.and_utc().timestamp_millis();
                        if ms > 0 {
                            video_meta.taken_at_ms = Some(ms);
                            meta.capture_at_ms = Some(ms);
                        }
                    }
                }
            }

            if meta.content_identifier.is_none() {
                meta.content_identifier = fmt.tags.iter().find_map(|(k, v)| {
                    if k.eq_ignore_ascii_case("com.apple.quicktime.content.identifier") {
                        let trimmed = v.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    } else {
                        None
                    }
                });
            }
        }

        if let Some(v_stream) = probe
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("video"))
        {
            video_meta.video_codec = v_stream.codec_name.clone();
            video_meta.fps = v_stream
                .avg_frame_rate
                .as_deref()
                .and_then(parse_ffprobe_rate)
                .or_else(|| {
                    v_stream
                        .r_frame_rate
                        .as_deref()
                        .and_then(parse_ffprobe_rate)
                });
            if video_meta.bitrate_bps.is_none() {
                video_meta.bitrate_bps = v_stream
                    .bit_rate
                    .as_deref()
                    .and_then(|s| s.trim().parse::<u64>().ok());
            }
            video_meta.bit_depth = infer_bit_depth(v_stream);
            video_meta.color_transfer = v_stream.color_transfer.clone();
            video_meta.color_primaries = v_stream.color_primaries.clone();
            video_meta.is_hdr = is_hdr_stream(v_stream, video_meta.bit_depth);

            let rot_tag = v_stream
                .tags
                .get("rotate")
                .and_then(|s| s.trim().parse::<i32>().ok());
            let rot_side = v_stream.side_data_list.iter().find_map(|sd| {
                sd.get("rotation")
                    .and_then(|v| v.as_i64())
                    .and_then(|r| i32::try_from(r).ok())
            });
            video_meta.rotation = rot_side.or(rot_tag).unwrap_or(0);

            if let (Some(w), Some(h)) = (v_stream.width, v_stream.height) {
                let normalized_rot = video_meta.rotation.rem_euclid(360);
                if normalized_rot == 90 || normalized_rot == 270 {
                    meta.width = Some(h);
                    meta.height = Some(w);
                } else {
                    meta.width = Some(w);
                    meta.height = Some(h);
                }
            }
        }

        if let Some(a_stream) = probe
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("audio"))
        {
            video_meta.audio_codec = a_stream.codec_name.clone();
        }
    }

    meta.video_meta = Some(video_meta);
    meta
}

fn extract_image_metadata(path: &Path, ext: &str, is_raw: bool) -> ExtractedMetadata {
    let mut meta = ExtractedMetadata::default();
    let mut parser = MediaParser::new();

    if let Ok(ms) = MediaSource::open(path) {
        if let Ok(iter) = parser.parse_exif(ms) {
            let exif: Exif = iter.into();
            let (mut photo_meta, exif_w, exif_h, tz_offset, content_id) = populate_from_exif(&exif);
            meta.capture_at_ms = photo_meta.taken_at_ms;
            meta.capture_tz_offset_m = tz_offset;
            meta.content_identifier = content_id;

            // For TIFF-based camera RAWs (CR2/NEF/ARW/DNG), `image::image_dimensions` reads IFD0
            // which is frequently a 160×120 thumbnail. Prefer EXIF full-resolution dimensions first.
            let (mut w, mut h) = if is_raw && exif_w.is_some() && exif_h.is_some() {
                (exif_w, exif_h)
            } else {
                match image::image_dimensions(path) {
                    Ok((iw, ih)) => (Some(iw), Some(ih)),
                    Err(_) => (exif_w, exif_h),
                }
            };
            if photo_meta.orientation.is_some_and(|o| (5..=8).contains(&o)) {
                std::mem::swap(&mut w, &mut h);
            }
            meta.width = w;
            meta.height = h;
            if photo_meta.orientation.is_none() {
                photo_meta.orientation = Some(1);
            }
            meta.photo_meta = Some(photo_meta);
        }
    }

    if meta.width.is_none() || meta.height.is_none() {
        if let Ok((iw, ih)) = image::image_dimensions(path) {
            meta.width = Some(iw);
            meta.height = Some(ih);
        }
    }

    if matches!(ext, "jpg" | "jpeg" | "heic" | "heif") {
        if let Some((offset, length)) = detect_android_motion_photo_file(path) {
            meta.embedded_video_offset = Some(offset);
            meta.embedded_video_length = Some(length);
            if length <= 64 * 1024 * 1024 {
                if let Ok(mut f) = File::open(path) {
                    if f.seek(SeekFrom::Start(offset)).is_ok() {
                        let mut trailer = vec![0u8; length as usize];
                        if f.read_exact(&mut trailer).is_ok() {
                            if let Ok(ms) = MediaSource::from_memory(trailer) {
                                if let Ok(track) = parser.parse_track(ms) {
                                    if let Some(EntryValue::U64(dur)) =
                                        track.get(TrackInfoTag::DurationMs)
                                    {
                                        meta.duration_ms = i64::try_from(*dur).ok();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    meta
}

fn extract_livp_metadata(path: &Path) -> Result<ExtractedMetadata, MediaError> {
    let contents = extract_livp(path)?;
    let mut meta = ExtractedMetadata::default();
    let mut parser = MediaParser::new();

    if let Ok(ms) = MediaSource::from_memory(contents.still_bytes.clone()) {
        if let Ok(iter) = parser.parse_exif(ms) {
            let exif: Exif = iter.into();
            let (photo_meta, exif_w, exif_h, tz_offset, content_id) = populate_from_exif(&exif);
            meta.capture_at_ms = photo_meta.taken_at_ms;
            meta.capture_tz_offset_m = tz_offset;
            meta.content_identifier = content_id;

            let (mut w, mut h) = match image::load_from_memory(&contents.still_bytes) {
                Ok(img) => (Some(img.width()), Some(img.height())),
                Err(_) => (exif_w, exif_h),
            };
            if photo_meta.orientation.is_some_and(|o| (5..=8).contains(&o)) {
                std::mem::swap(&mut w, &mut h);
            }
            meta.width = w;
            meta.height = h;
            meta.photo_meta = Some(photo_meta);
        }
    }

    if meta.width.is_none() || meta.height.is_none() {
        if let Ok(img) = image::load_from_memory(&contents.still_bytes) {
            meta.width = Some(img.width());
            meta.height = Some(img.height());
        }
    }

    if let Some(vbytes) = contents.video_bytes {
        if meta.content_identifier.is_none() {
            meta.content_identifier = parse_isobmff_content_id_bytes(&vbytes);
        }
        if let Ok(ms) = MediaSource::from_memory(vbytes) {
            if let Ok(track) = parser.parse_track(ms) {
                if let Some(EntryValue::U64(dur)) = track.get(TrackInfoTag::DurationMs) {
                    let dur_i64 = i64::try_from(*dur).unwrap_or(i64::MAX);
                    meta.duration_ms = Some(dur_i64);
                    meta.video_meta = Some(VideoMetaView {
                        duration_ms: dur_i64,
                        taken_at_ms: meta.capture_at_ms,
                        ..Default::default()
                    });
                }
            }
        }
    }

    Ok(meta)
}

/// Extracts deterministic metadata for any supported image, RAW, video, Motion Photo, or `.livp` file.
pub fn extract_metadata(path: &Path) -> Result<ExtractedMetadata, MediaError> {
    if !path.is_file() {
        return Err(MediaError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("media file not found: {}", path.display()),
        )));
    }

    let category = classify::classify_path(path)
        .ok_or_else(|| MediaError::UnsupportedFormat(path.display().to_string()))?;
    let ext = classify::normalized_extension(path).unwrap_or_default();

    match category {
        MediaCategory::Photo => Ok(extract_image_metadata(path, &ext, false)),
        MediaCategory::Raw => Ok(extract_image_metadata(path, &ext, true)),
        MediaCategory::Video => Ok(extract_video_metadata(path)),
        MediaCategory::Livp => extract_livp_metadata(path),
    }
}

#[cfg(test)]
mod subsec_and_software_tests {
    use super::*;

    #[test]
    fn parses_exif_subsec_fractional_milliseconds() {
        assert_eq!(parse_subsec_ms("9"), Some(900));
        assert_eq!(parse_subsec_ms("09"), Some(90));
        assert_eq!(parse_subsec_ms("095"), Some(95));
        assert_eq!(parse_subsec_ms("123456"), Some(123));
        assert_eq!(parse_subsec_ms("00"), Some(0));
        assert_eq!(parse_subsec_ms(".40"), Some(400));
        assert_eq!(parse_subsec_ms(""), None);
        assert_eq!(parse_subsec_ms("   "), None);
    }

    #[test]
    fn distinguishes_editing_software_from_camera_firmware() {
        assert!(is_editing_software(
            "Adobe Photoshop Lightroom Classic 15.3 (Windows)"
        ));
        assert!(is_editing_software("Capture One 23 Macintosh"));
        assert!(is_editing_software("darktable 4.8.1"));
        assert!(is_editing_software("Affinity Photo 2"));
        assert!(!is_editing_software("Ver.1.4.0"));
        assert!(!is_editing_software("18.1.1"));
        assert!(!is_editing_software(""));
    }
}
