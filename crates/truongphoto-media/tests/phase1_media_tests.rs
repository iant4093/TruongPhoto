use image::codecs::jpeg::JpegEncoder;
use image::{ImageEncoder, Rgb, RgbImage};
use moxcms::ColorProfile;
use std::io::{Cursor, Write};
use std::path::Path;
use tempfile::TempDir;
use truongphoto_media::{
    decode_image_to_srgb, extract_livp_entry_bytes, extract_metadata, generate_thumb_and_histogram,
    inspect_livp, parse_android_motion_photo, parse_apple_makernote_content_id,
    parse_isobmff_content_id_bytes, resolve_volume_identity, video_capabilities,
};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn make_test_jpeg_bytes(width: u32, height: u32, icc_profile: Option<Vec<u8>>) -> Vec<u8> {
    let mut img = RgbImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let r = ((x * 255) / width.max(1)) as u8;
        let g = ((y * 255) / height.max(1)) as u8;
        let b = (((x + y) * 127) / (width + height).max(1)) as u8;
        *px = Rgb([r, g, b]);
    }
    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut out, 90);
    if let Some(icc) = icc_profile {
        enc.set_icc_profile(icc)
            .expect("JpegEncoder should accept ICC profile");
    }
    enc.write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgb8)
        .expect("JPEG encode should succeed");
    out
}

fn inject_jpeg_app1_segments(jpeg: &[u8], segments: &[Vec<u8>]) -> Vec<u8> {
    assert!(jpeg.starts_with(&[0xFF, 0xD8]), "expected JPEG SOI");
    let mut out = Vec::with_capacity(jpeg.len() + 4096);
    out.extend_from_slice(&jpeg[..2]);
    for payload in segments {
        let len = (payload.len() + 2) as u16;
        out.push(0xFF);
        out.push(0xE1);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn build_apple_makernote_blob(uuid: &str) -> Vec<u8> {
    // "Apple iOS\0" (10) + version 0x0001 (2) + "MM" (2) + count=1 (2) + entry (12) + next=0 (4) = 32
    let mut blob = Vec::new();
    blob.extend_from_slice(b"Apple iOS\0");
    blob.extend_from_slice(&[0x00, 0x01]);
    blob.extend_from_slice(b"MM");
    blob.extend_from_slice(&1u16.to_be_bytes());

    let mut val = uuid.as_bytes().to_vec();
    val.push(0);
    let val_offset = 32u32;

    // Tag 0x0011, ASCII (2), count, offset
    blob.extend_from_slice(&0x0011u16.to_be_bytes());
    blob.extend_from_slice(&2u16.to_be_bytes());
    blob.extend_from_slice(&(val.len() as u32).to_be_bytes());
    blob.extend_from_slice(&val_offset.to_be_bytes());

    blob.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(blob.len(), val_offset as usize);
    blob.extend_from_slice(&val);
    blob
}

fn build_isobmff_box(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = (8 + payload.len()) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    out
}

fn build_isobmff_with_content_id(uuid: &str) -> Vec<u8> {
    let ftyp = build_isobmff_box(b"ftyp", b"qt  \x00\x00\x02\x00qt  ");

    let key_name = b"com.apple.quicktime.content.identifier";
    let key_entry_size = (8 + key_name.len()) as u32;
    let mut keys_payload = Vec::new();
    keys_payload.extend_from_slice(&0u32.to_be_bytes()); // version + flags
    keys_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
    keys_payload.extend_from_slice(&key_entry_size.to_be_bytes());
    keys_payload.extend_from_slice(b"mdta");
    keys_payload.extend_from_slice(key_name);
    let keys_box = build_isobmff_box(b"keys", &keys_payload);

    let mut data_payload = Vec::new();
    data_payload.extend_from_slice(&1u32.to_be_bytes()); // type = UTF-8 (1)
    data_payload.extend_from_slice(&0u32.to_be_bytes()); // locale
    data_payload.extend_from_slice(uuid.as_bytes());
    let data_box = build_isobmff_box(b"data", &data_payload);

    let item_box = build_isobmff_box(&1u32.to_be_bytes(), &data_box);
    let ilst_box = build_isobmff_box(b"ilst", &item_box);

    let mut meta_payload = Vec::new();
    meta_payload.extend_from_slice(&0u32.to_be_bytes()); // FullBox version+flags
    meta_payload.extend_from_slice(&keys_box);
    meta_payload.extend_from_slice(&ilst_box);
    let meta_box = build_isobmff_box(b"meta", &meta_payload);
    let moov_box = build_isobmff_box(b"moov", &meta_box);

    let mut out = Vec::new();
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov_box);
    out
}

/// Builds a little-endian EXIF APP1 payload (`Exif\0\0` + TIFF) containing camera, exposure,
/// orientation=6, Tokyo GPS (35.6762 N, 139.6503 E), and optional MakerNote.
fn build_synthetic_exif_app1(makernote: Option<&[u8]>) -> Vec<u8> {
    struct Entry {
        tag: u16,
        ty: u16,
        count: u32,
        inline_or_data: EntryData,
    }
    enum EntryData {
        Inline([u8; 4]),
        Bytes(Vec<u8>),
    }

    fn write_ifd(buf: &mut Vec<u8>, entries: Vec<Entry>, next_ifd: u32) {
        let start = buf.len();
        let data_start = start + 2 + entries.len() * 12 + 4;
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());

        let mut extra = Vec::new();
        for e in entries {
            buf.extend_from_slice(&e.tag.to_le_bytes());
            buf.extend_from_slice(&e.ty.to_le_bytes());
            buf.extend_from_slice(&e.count.to_le_bytes());
            match e.inline_or_data {
                EntryData::Inline(val) => buf.extend_from_slice(&val),
                EntryData::Bytes(bytes) => {
                    if bytes.len() <= 4 {
                        let mut padded = [0u8; 4];
                        padded[..bytes.len()].copy_from_slice(&bytes);
                        buf.extend_from_slice(&padded);
                    } else {
                        let offset = (data_start + extra.len()) as u32;
                        buf.extend_from_slice(&offset.to_le_bytes());
                        extra.extend_from_slice(&bytes);
                        if extra.len() % 2 == 1 {
                            extra.push(0);
                        }
                    }
                }
            }
        }
        buf.extend_from_slice(&next_ifd.to_le_bytes());
        buf.extend_from_slice(&extra);
    }

    fn rational_le(num: u32, den: u32) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[..4].copy_from_slice(&num.to_le_bytes());
        out[4..8].copy_from_slice(&den.to_le_bytes());
        out
    }

    // Tokyo: 35.6762° = 35° 40' 34.32" N, 139.6503° = 139° 39' 1.08" E
    let mut gps_lat_bytes = Vec::with_capacity(24);
    gps_lat_bytes.extend_from_slice(&rational_le(35, 1));
    gps_lat_bytes.extend_from_slice(&rational_le(40, 1));
    gps_lat_bytes.extend_from_slice(&rational_le(3432, 100));

    let mut gps_lon_bytes = Vec::with_capacity(24);
    gps_lon_bytes.extend_from_slice(&rational_le(139, 1));
    gps_lon_bytes.extend_from_slice(&rational_le(39, 1));
    gps_lon_bytes.extend_from_slice(&rational_le(108, 100));

    // Build TIFF starting at offset 0
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at 8

    // Reserve IFD0 with 5 entries: Make(0x010F), Model(0x0110), Orientation(0x0112), ExifIFD(0x8769), GPSIFD(0x8825)
    // First compute offsets for ExifIFD and GPSIFD by placing IFD0 first with placeholder pointers,
    // or lay them out sequentially after IFD0.
    // IFD0 header+entries+next = 2 + 5*12 + 4 = 66 bytes (8..74).
    // Extra strings in IFD0: "Apple\0" (6 bytes), "iPhone 16 Pro\0" (14 bytes) = 20 bytes -> IFD0 ends at 94.
    let exif_ifd_offset = 94u32;

    let ifd0_entries = vec![
        Entry {
            tag: 0x010F,
            ty: 2,
            count: 6,
            inline_or_data: EntryData::Bytes(b"Apple\0".to_vec()),
        },
        Entry {
            tag: 0x0110,
            ty: 2,
            count: 14,
            inline_or_data: EntryData::Bytes(b"iPhone 16 Pro\0".to_vec()),
        },
        Entry {
            tag: 0x0112,
            ty: 3,
            count: 1,
            inline_or_data: EntryData::Inline([6, 0, 0, 0]), // Orientation = 6 (90 deg CW)
        },
        Entry {
            tag: 0x8769,
            ty: 4,
            count: 1,
            inline_or_data: EntryData::Inline(exif_ifd_offset.to_le_bytes()),
        },
        Entry {
            tag: 0x8825,
            ty: 4,
            count: 1,
            inline_or_data: EntryData::Inline([0, 0, 0, 0]), // patched once ExifIFD written
        },
    ];
    write_ifd(&mut tiff, ifd0_entries, 0);
    assert_eq!(tiff.len(), exif_ifd_offset as usize);

    let mut exif_entries = vec![
        Entry {
            tag: 0x829A, // ExposureTime 1/250s
            ty: 5,
            count: 1,
            inline_or_data: EntryData::Bytes(rational_le(1, 250).to_vec()),
        },
        Entry {
            tag: 0x829D, // FNumber f/1.8
            ty: 5,
            count: 1,
            inline_or_data: EntryData::Bytes(rational_le(18, 10).to_vec()),
        },
        Entry {
            tag: 0x8827, // ISOSpeedRatings = 100
            ty: 3,
            count: 1,
            inline_or_data: EntryData::Inline([100, 0, 0, 0]),
        },
        Entry {
            tag: 0x9003, // DateTimeOriginal
            ty: 2,
            count: 20,
            inline_or_data: EntryData::Bytes(b"2026:11:19 14:30:00\0".to_vec()),
        },
        Entry {
            tag: 0x920A, // FocalLength 24mm
            ty: 5,
            count: 1,
            inline_or_data: EntryData::Bytes(rational_le(24, 1).to_vec()),
        },
        Entry {
            tag: 0xA434, // LensModel
            ty: 2,
            count: 13,
            inline_or_data: EntryData::Bytes(b"24mm f/1.78\0\0".to_vec()),
        },
    ];
    if let Some(mn) = makernote {
        exif_entries.push(Entry {
            tag: 0x927C,
            ty: 7,
            count: mn.len() as u32,
            inline_or_data: EntryData::Bytes(mn.to_vec()),
        });
    }
    exif_entries.sort_by_key(|e| e.tag);
    write_ifd(&mut tiff, exif_entries, 0);

    let gps_ifd_offset = tiff.len() as u32;
    // Patch GPSIFD pointer in IFD0 entry index 4 (offset: 8 + 2 + 4*12 + 8 = 66)
    tiff[66..70].copy_from_slice(&gps_ifd_offset.to_le_bytes());

    let gps_entries = vec![
        Entry {
            tag: 0x0001, // GPSLatitudeRef = "N\0"
            ty: 2,
            count: 2,
            inline_or_data: EntryData::Inline([b'N', 0, 0, 0]),
        },
        Entry {
            tag: 0x0002, // GPSLatitude
            ty: 5,
            count: 3,
            inline_or_data: EntryData::Bytes(gps_lat_bytes),
        },
        Entry {
            tag: 0x0003, // GPSLongitudeRef = "E\0"
            ty: 2,
            count: 2,
            inline_or_data: EntryData::Inline([b'E', 0, 0, 0]),
        },
        Entry {
            tag: 0x0004, // GPSLongitude
            ty: 5,
            count: 3,
            inline_or_data: EntryData::Bytes(gps_lon_bytes),
        },
    ];
    write_ifd(&mut tiff, gps_entries, 0);

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    app1
}

#[test]
fn synthetic_jpeg_exif_tokyo_geocoding_orientation_thumb_and_histogram() {
    let dir = TempDir::new().unwrap();
    let img_path = dir.path().join("tokyo_portrait.jpg");
    let thumb_path = dir.path().join("thumbs/aa/bb/tokyo_portrait.webp");

    // Raw coded dimensions: 640x480, Orientation=6 (90° CW) -> oriented dimensions: 480x640
    let base_jpeg = make_test_jpeg_bytes(640, 480, None);
    let exif_app1 = build_synthetic_exif_app1(None);
    let jpeg_with_exif = inject_jpeg_app1_segments(&base_jpeg, &[exif_app1]);
    std::fs::write(&img_path, jpeg_with_exif).unwrap();

    let meta = extract_metadata(&img_path).expect("metadata extraction should succeed");
    assert_eq!(meta.width, Some(480));
    assert_eq!(meta.height, Some(640));
    assert!(meta.capture_at_ms.is_some());

    let photo = meta.photo_meta.expect("photo_meta should be populated");
    assert_eq!(photo.camera_make.as_deref(), Some("Apple"));
    assert_eq!(photo.camera_model.as_deref(), Some("iPhone 16 Pro"));
    assert_eq!(photo.lens_model.as_deref(), Some("24mm f/1.78"));
    assert_eq!(photo.iso, Some(100));
    assert_eq!(photo.orientation, Some(6));
    assert_eq!(photo.geo_country_code.as_deref(), Some("JP"));
    assert_eq!(photo.geo_country.as_deref(), Some("Japan"));
    assert!(photo.geo_city.is_some());

    let decoded = decode_image_to_srgb(&img_path).expect("decode should apply orientation");
    assert_eq!(decoded.dimensions(), (480, 640));

    let thumb = generate_thumb_and_histogram(&img_path, &thumb_path)
        .expect("thumbnail + histogram should succeed");
    assert_eq!(thumb.width, 480);
    assert_eq!(thumb.height, 640);
    assert!(thumb.thumb_bytes_len > 0);
    assert!(thumb.histogram.luma.iter().any(|&v| v > 0));
    assert!(thumb.histogram.r.iter().any(|&v| v > 0));

    let webp_bytes = std::fs::read(&thumb_path).unwrap();
    assert!(webp_bytes.starts_with(b"RIFF"));
    assert_eq!(&webp_bytes[8..12], b"WEBP");

    let decoded_thumb = image::load_from_memory(&webp_bytes).unwrap();
    assert_eq!(decoded_thumb.width().max(decoded_thumb.height()), 400);
    assert_eq!(decoded_thumb.width(), 300);
    assert_eq!(decoded_thumb.height(), 400);
}

#[test]
fn synthetic_display_p3_icc_image_converts_cleanly() {
    let dir = TempDir::new().unwrap();
    let img_path = dir.path().join("display_p3.jpg");
    let p3_icc = ColorProfile::new_display_p3().encode().unwrap();
    let jpeg = make_test_jpeg_bytes(120, 80, Some(p3_icc));
    std::fs::write(&img_path, jpeg).unwrap();

    let rgb =
        decode_image_to_srgb(&img_path).expect("Display P3 decode + moxcms bake must succeed");
    assert_eq!(rgb.dimensions(), (120, 80));
}

#[test]
fn synthetic_android_motion_photo_extracts_exact_video_bounds() {
    let dir = TempDir::new().unwrap();
    let mp_path = dir.path().join("PXL_motion.jpg");

    let base_jpeg = make_test_jpeg_bytes(320, 240, None);
    let mp4_trailer = build_isobmff_with_content_id("ANDROID-MOTION-TEST");
    let trailer_len = mp4_trailer.len() as u64;

    let xmp = format!(
        "http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:GCamera=\"http://ns.google.com/photos/1.0/camera/\" xmlns:Container=\"http://ns.google.com/photos/1.0/container/\" xmlns:Item=\"http://ns.google.com/photos/1.0/container/item/\" GCamera:MotionPhoto=\"1\" GCamera:MicroVideoOffset=\"{trailer_len}\"><Container:Directory><rdf:Seq><rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"Primary\" Item:Mime=\"image/jpeg\"/></rdf:li><rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"MotionPhoto\" Item:Mime=\"video/mp4\" Item:Length=\"{trailer_len}\"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description></rdf:RDF></x:xmpmeta>"
    );

    let mut motion_bytes = inject_jpeg_app1_segments(&base_jpeg, &[xmp.into_bytes()]);
    let expected_offset = motion_bytes.len() as u64;
    motion_bytes.extend_from_slice(&mp4_trailer);

    let parsed = parse_android_motion_photo(&motion_bytes).expect("in-memory motion photo parse");
    assert_eq!(parsed, (expected_offset, trailer_len));

    std::fs::write(&mp_path, &motion_bytes).unwrap();
    let meta = extract_metadata(&mp_path).unwrap();
    assert_eq!(meta.embedded_video_offset, Some(expected_offset));
    assert_eq!(meta.embedded_video_length, Some(trailer_len));
}

#[test]
fn synthetic_apple_makernote_and_isobmff_parse_identical_uuid() {
    let uuid = "7A9C4E12-3B8D-4F11-9D2A-1234567890AB";
    let makernote = build_apple_makernote_blob(uuid);
    let isobmff = build_isobmff_with_content_id(uuid);

    let mn_id = parse_apple_makernote_content_id(&makernote).expect("MakerNote UUID should parse");
    let mov_id = parse_isobmff_content_id_bytes(&isobmff).expect("ISOBMFF UUID should parse");
    assert_eq!(mn_id, uuid);
    assert_eq!(mov_id, uuid);

    // Also verify end-to-end through a synthetic JPEG carrying the Apple MakerNote
    let dir = TempDir::new().unwrap();
    let jpg_path = dir.path().join("IMG_1024.jpg");
    let base_jpeg = make_test_jpeg_bytes(200, 150, None);
    let app1 = build_synthetic_exif_app1(Some(&makernote));
    std::fs::write(&jpg_path, inject_jpeg_app1_segments(&base_jpeg, &[app1])).unwrap();

    let meta = extract_metadata(&jpg_path).unwrap();
    assert_eq!(meta.content_identifier.as_deref(), Some(uuid));
}

#[test]
fn synthetic_livp_zip_archive_extracts_and_thumbnails() {
    let dir = TempDir::new().unwrap();
    let livp_path = dir.path().join("live_bundle.livp");
    let thumb_path = dir.path().join("live_bundle.webp");

    let uuid = "AABBCCDD-1122-3344-5566-77889900AABB";
    let base_jpeg = make_test_jpeg_bytes(300, 200, None);
    let makernote = build_apple_makernote_blob(uuid);
    let jpg_bytes =
        inject_jpeg_app1_segments(&base_jpeg, &[build_synthetic_exif_app1(Some(&makernote))]);
    let mov_bytes = build_isobmff_with_content_id(uuid);

    let mut zip_buf = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut zip_buf);
        let opts = SimpleFileOptions::default();
        writer.start_file("IMG_0001.jpg", opts).unwrap();
        writer.write_all(&jpg_bytes).unwrap();
        writer.start_file("IMG_0001.mov", opts).unwrap();
        writer.write_all(&mov_bytes).unwrap();
        writer.finish().unwrap();
    }
    std::fs::write(&livp_path, zip_buf.into_inner()).unwrap();

    let entries = inspect_livp(&livp_path).expect("inspect_livp should find both entries");
    assert_eq!(entries.still_entry, "IMG_0001.jpg");
    assert_eq!(entries.video_entry.as_deref(), Some("IMG_0001.mov"));

    let extracted_still = extract_livp_entry_bytes(&livp_path, &entries.still_entry).unwrap();
    assert_eq!(extracted_still, jpg_bytes);

    let meta = extract_metadata(&livp_path).unwrap();
    assert_eq!(meta.content_identifier.as_deref(), Some(uuid));
    assert_eq!(meta.width, Some(200));
    assert_eq!(meta.height, Some(300));

    let thumb = generate_thumb_and_histogram(&livp_path, &thumb_path).unwrap();
    assert_eq!(thumb.width, 200);
    assert_eq!(thumb.height, 300);
    assert!(thumb_path.is_file());
}

#[test]
fn volume_uuid_resolves_for_temp_dir_and_none_for_missing_path() {
    let dir = TempDir::new().unwrap();
    let info = resolve_volume_identity(dir.path());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert!(
        info.volume_uuid.is_some(),
        "expected real OS volume UUID, got {info:?}"
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = info;

    let missing = resolve_volume_identity(Path::new("/nonexistent/truongphoto/volume/check"));
    assert!(missing.volume_uuid.is_none());
    assert!(missing.volume_label.is_none());
}

#[test]
fn video_pipeline_probes_capabilities_and_transcodes_when_ffmpeg_present() {
    let caps = video_capabilities();
    if let Some(ffmpeg) = caps.ffmpeg_path.as_deref() {
        let dir = TempDir::new().unwrap();
        let mp4_path = dir.path().join("sample.mp4");
        let hls_dir = dir.path().join("hls_out");
        let thumb_path = dir.path().join("video_thumb.webp");

        let status = std::process::Command::new(ffmpeg)
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=160x120:rate=24",
                "-c:v",
                &caps.h264_encoder,
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&mp4_path)
            .status()
            .expect("ffmpeg command should run");

        if status.success() && mp4_path.is_file() {
            let meta = extract_metadata(&mp4_path).unwrap();
            assert_eq!(meta.width, Some(160));
            assert_eq!(meta.height, Some(120));
            assert!(meta.duration_ms.unwrap_or(0) > 0);

            let thumb = generate_thumb_and_histogram(&mp4_path, &thumb_path).unwrap();
            assert_eq!(thumb.width, 160);
            assert_eq!(thumb.height, 120);

            let hls_bytes =
                truongphoto_media::transcode_to_hls(&mp4_path, &hls_dir, false).unwrap();
            assert!(hls_bytes > 0);
            assert!(hls_dir.join("index.m3u8").is_file());
            assert!(hls_dir.join("seg_0000.ts").is_file());
        }
    }
}
