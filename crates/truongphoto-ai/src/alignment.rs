use image::{Rgb, RgbImage};

/// Standard ArcFace 112x112 reference 5-point facial landmarks:
/// `[left_eye, right_eye, nose_tip, left_mouth_corner, right_mouth_corner]`.
pub const ARCFACE_REF_LANDMARKS: [(f32, f32); 5] = [
    (38.2946, 51.6963),
    (73.5318, 51.5014),
    (56.0252, 71.7366),
    (41.5493, 92.3655),
    (70.7299, 92.2041),
];

/// Estimates the optimal 2D similarity transform `[[a, -b, tx], [b, a, ty]]` mapping
/// `src` 5-point landmarks to `dst` 5-point landmarks (Umeyama least-squares similarity).
pub fn estimate_similarity_transform(
    src: &[(f32, f32); 5],
    dst: &[(f32, f32); 5],
) -> [[f32; 3]; 2] {
    let n = 5.0_f64;
    let mut mean_sx = 0.0_f64;
    let mut mean_sy = 0.0_f64;
    let mut mean_dx = 0.0_f64;
    let mut mean_dy = 0.0_f64;

    for i in 0..5 {
        mean_sx += src[i].0 as f64;
        mean_sy += src[i].1 as f64;
        mean_dx += dst[i].0 as f64;
        mean_dy += dst[i].1 as f64;
    }
    mean_sx /= n;
    mean_sy /= n;
    mean_dx /= n;
    mean_dy /= n;

    let mut sum_src_sq = 0.0_f64;
    let mut trace_term = 0.0_f64;
    let mut cross_term = 0.0_f64;

    for i in 0..5 {
        let sx = src[i].0 as f64 - mean_sx;
        let sy = src[i].1 as f64 - mean_sy;
        let dx = dst[i].0 as f64 - mean_dx;
        let dy = dst[i].1 as f64 - mean_dy;

        sum_src_sq += sx * sx + sy * sy;
        trace_term += sx * dx + sy * dy;
        cross_term += sx * dy - sy * dx;
    }

    if sum_src_sq <= 1e-12 {
        let tx = (mean_dx - mean_sx) as f32;
        let ty = (mean_dy - mean_sy) as f32;
        return [[1.0, 0.0, tx], [0.0, 1.0, ty]];
    }

    let r = trace_term.hypot(cross_term);
    if r <= 1e-12 {
        let tx = (mean_dx - mean_sx) as f32;
        let ty = (mean_dy - mean_sy) as f32;
        return [[1.0, 0.0, tx], [0.0, 1.0, ty]];
    }

    let cos_theta = trace_term / r;
    let sin_theta = cross_term / r;
    let scale = r / sum_src_sq;

    let a = scale * cos_theta;
    let b = scale * sin_theta;

    let tx = mean_dx - (a * mean_sx - b * mean_sy);
    let ty = mean_dy - (b * mean_sx + a * mean_sy);

    [
        [a as f32, -b as f32, tx as f32],
        [b as f32, a as f32, ty as f32],
    ]
}

/// Applies the 5-point Umeyama similarity transform to warp `img` into a `112x112`
/// ArcFace/AuraFace-aligned RGB crop using bilinear interpolation.
pub fn align_face_112(img: &RgbImage, landmarks: &[(f32, f32); 5]) -> RgbImage {
    const OUT_SIZE: u32 = 112;
    let m = estimate_similarity_transform(landmarks, &ARCFACE_REF_LANDMARKS);
    let a00 = m[0][0] as f64;
    let a01 = m[0][1] as f64;
    let tx = m[0][2] as f64;
    let a10 = m[1][0] as f64;
    let a11 = m[1][1] as f64;
    let ty = m[1][2] as f64;

    let det = a00 * a11 - a01 * a10;
    let mut out = RgbImage::new(OUT_SIZE, OUT_SIZE);
    if det.abs() <= 1e-12 {
        return out;
    }

    let inv00 = a11 / det;
    let inv01 = -a01 / det;
    let inv10 = -a10 / det;
    let inv11 = a00 / det;

    let w = img.width() as i32;
    let h = img.height() as i32;
    if w == 0 || h == 0 {
        return out;
    }

    for oy in 0..OUT_SIZE {
        let dy = oy as f64 - ty;
        for ox in 0..OUT_SIZE {
            let dx = ox as f64 - tx;
            let sx = inv00 * dx + inv01 * dy;
            let sy = inv10 * dx + inv11 * dy;

            if sx < -0.5 || sy < -0.5 || sx > (w as f64 - 0.5) || sy > (h as f64 - 0.5) {
                continue;
            }

            let sx_clamped = sx.clamp(0.0, (w - 1) as f64);
            let sy_clamped = sy.clamp(0.0, (h - 1) as f64);

            let x0 = sx_clamped.floor() as i32;
            let y0 = sy_clamped.floor() as i32;
            let x1 = (x0 + 1).min(w - 1);
            let y1 = (y0 + 1).min(h - 1);

            let fx = (sx_clamped - x0 as f64) as f32;
            let fy = (sy_clamped - y0 as f64) as f32;

            let p00 = img.get_pixel(x0 as u32, y0 as u32).0;
            let p10 = img.get_pixel(x1 as u32, y0 as u32).0;
            let p01 = img.get_pixel(x0 as u32, y1 as u32).0;
            let p11 = img.get_pixel(x1 as u32, y1 as u32).0;

            let mut rgb = [0u8; 3];
            for c in 0..3 {
                let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
                let bot = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
                let val = top * (1.0 - fy) + bot * fy;
                rgb[c] = val.round().clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(ox, oy, Rgb(rgb));
        }
    }

    out
}
