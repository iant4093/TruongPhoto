use crate::MediaError;
use image::{DynamicImage, RgbImage};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use truongphoto_core::models::HISTOGRAM_BINS;
use truongphoto_core::Histogram256;

const ALPHA_CANVAS_RGB: [u8; 3] = [0x20, 0x20, 0x20];

/// Converts an image buffer in-place from an embedded ICC profile to sRGB using `moxcms`.
pub fn bake_icc_to_srgb(rgb: &mut [u8], icc_bytes: &[u8]) -> Result<(), MediaError> {
    if !rgb.len().is_multiple_of(3) {
        return Err(MediaError::ColorTransform(
            "RGB buffer length is not a multiple of 3".to_string(),
        ));
    }
    if rgb.is_empty() || icc_bytes.is_empty() {
        return Ok(());
    }

    let src = ColorProfile::new_from_slice(icc_bytes)
        .map_err(|e| MediaError::ColorTransform(format!("invalid source ICC profile: {e}")))?;

    if src.color_space != DataColorSpace::Rgb {
        return Err(MediaError::ColorTransform(format!(
            "unsupported non-RGB ICC color space: {:?}",
            src.color_space
        )));
    }

    let dst = ColorProfile::new_srgb();
    let xform = src
        .create_transform_8bit(Layout::Rgb, &dst, Layout::Rgb, TransformOptions::default())
        .map_err(|e| MediaError::ColorTransform(format!("failed to create ICC transform: {e}")))?;

    let src_copy = rgb.to_vec();
    xform
        .transform(&src_copy, rgb)
        .map_err(|e| MediaError::ColorTransform(format!("ICC transform failed: {e}")))?;
    Ok(())
}

/// Converts any `DynamicImage` into an 8-bit RGB image, compositing alpha onto `#202020`.
pub fn composite_to_rgb(img: DynamicImage) -> RgbImage {
    match img {
        DynamicImage::ImageRgb8(rgb) => rgb,
        DynamicImage::ImageRgba8(_)
        | DynamicImage::ImageRgba16(_)
        | DynamicImage::ImageRgba32F(_)
        | DynamicImage::ImageLumaA8(_)
        | DynamicImage::ImageLumaA16(_) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            let mut out = Vec::with_capacity((width as usize) * (height as usize) * 3);
            for px in rgba.pixels() {
                let [r, g, b, a] = px.0;
                if a == 255 {
                    out.extend_from_slice(&[r, g, b]);
                } else if a == 0 {
                    out.extend_from_slice(&ALPHA_CANVAS_RGB);
                } else {
                    let alpha = a as u16;
                    let inv = 255 - alpha;
                    out.push(
                        ((r as u16 * alpha + ALPHA_CANVAS_RGB[0] as u16 * inv + 127) / 255) as u8,
                    );
                    out.push(
                        ((g as u16 * alpha + ALPHA_CANVAS_RGB[1] as u16 * inv + 127) / 255) as u8,
                    );
                    out.push(
                        ((b as u16 * alpha + ALPHA_CANVAS_RGB[2] as u16 * inv + 127) / 255) as u8,
                    );
                }
            }
            RgbImage::from_raw(width, height, out).unwrap_or_else(|| RgbImage::new(width, height))
        }
        other => other.into_rgb8(),
    }
}

fn normalize_channel(counts: &[u64; HISTOGRAM_BINS]) -> Vec<u16> {
    let interior_max = counts[1..HISTOGRAM_BINS - 1]
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    let full_max = counts.iter().copied().max().unwrap_or(0);
    let scale_max = if interior_max > 0 {
        interior_max
    } else {
        full_max
    };

    if scale_max == 0 {
        return vec![0; HISTOGRAM_BINS];
    }

    counts
        .iter()
        .map(|&count| {
            let scaled = (count.saturating_mul(u16::MAX as u64)) / scale_max;
            scaled.min(u16::MAX as u64) as u16
        })
        .collect()
}

/// Computes a 256-bin R, G, B, and Rec.709 Luma histogram normalized to `0..=65535`.
pub fn compute_histogram(rgb: &RgbImage) -> Histogram256 {
    let mut r_counts = [0u64; HISTOGRAM_BINS];
    let mut g_counts = [0u64; HISTOGRAM_BINS];
    let mut b_counts = [0u64; HISTOGRAM_BINS];
    let mut luma_counts = [0u64; HISTOGRAM_BINS];

    for px in rgb.as_raw().chunks_exact(3) {
        let r = px[0];
        let g = px[1];
        let b = px[2];
        r_counts[r as usize] += 1;
        g_counts[g as usize] += 1;
        b_counts[b as usize] += 1;
        let y = ((2126 * r as u32 + 7152 * g as u32 + 722 * b as u32 + 5000) / 10000).min(255);
        luma_counts[y as usize] += 1;
    }

    Histogram256 {
        r: normalize_channel(&r_counts),
        g: normalize_channel(&g_counts),
        b: normalize_channel(&b_counts),
        luma: normalize_channel(&luma_counts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn composites_alpha_onto_dark_neutral_canvas() {
        let mut rgba = RgbaImage::new(2, 1);
        rgba.put_pixel(0, 0, Rgba([200, 100, 50, 0]));
        rgba.put_pixel(1, 0, Rgba([200, 100, 50, 255]));
        let rgb = composite_to_rgb(DynamicImage::ImageRgba8(rgba));
        assert_eq!(rgb.get_pixel(0, 0).0, ALPHA_CANVAS_RGB);
        assert_eq!(rgb.get_pixel(1, 0).0, [200, 100, 50]);
    }

    #[test]
    fn converts_display_p3_icc_to_srgb_and_computes_histogram() {
        let p3_icc = ColorProfile::new_display_p3()
            .encode()
            .expect("Display P3 ICC should encode");
        let mut rgb = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 128, 128, 128];
        bake_icc_to_srgb(&mut rgb, &p3_icc).expect("P3 to sRGB transform should succeed");

        let img = RgbImage::from_raw(2, 2, rgb).unwrap();
        let hist = compute_histogram(&img);
        assert_eq!(hist.r.len(), 256);
        assert_eq!(hist.g.len(), 256);
        assert_eq!(hist.b.len(), 256);
        assert_eq!(hist.luma.len(), 256);
        assert!(hist.luma.iter().any(|&v| v > 0));
    }
}
