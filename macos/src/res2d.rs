//! Resize filters that are not separable into a row pass and a column pass, on the
//! processor. The GPU runs the same code as the `resample2d` shader in gpu.rs; keep the two
//! in step. Codes as render::resize:
//! - 18 FSR 1 EASU: AMD's edge-adaptive upscaler (FidelityFX Super Resolution 1, MIT).
//!   Enlarging only; any axis that shrinks makes the whole resize Area, as GPUResize does.
//! - 19 EWA LanczosSharp: a round jinc-windowed-jinc kernel (Heckbert's elliptical weighted
//!   average, Robidoux's LanczosSharp blur). Past 2x reduction an Area pass first brings
//!   the image to twice the target, so the footprint stays at most 13x13 taps.
//! - 20 Scale2x and 21 Eagle: pixel-art rules, adapted to any enlargement by the quadrant
//!   of the source pixel each output pixel falls in. Shrinking is Area.
//! Colour is premultiplied by alpha while filtering.

use image::RgbaImage;

/// Edge sensitivity 50%: two colours match when their channels differ by at most this in
/// total (0-1 per channel, four channels).
pub const PIXEL_TOL: f32 = 0.1;
/// EWA LanczosSharp: the jinc's third zero as the support, the Robidoux blur.
pub const EWA_SUPPORT: f32 = 3.238_315_5;
pub const EWA_BLUR: f32 = 0.981_250_56;
/// The jinc's first zero, to stretch the window to the support.
pub const JINC_ZERO1: f32 = 1.219_669_9;

/// Bessel J1 (Numerical Recipes' rational and asymptotic forms).
pub fn bessel_j1(x: f32) -> f32 {
    let ax = x.abs();
    if ax < 8.0 {
        let y = x * x;
        let a1 = x * (72362614232.0 + y * (-7895059235.0 + y * (242396853.1 + y * (-2972611.439 + y * (15704.48260 + y * (-30.16036606))))));
        let a2 = 144725228442.0 + y * (2300535178.0 + y * (18583304.74 + y * (99447.43394 + y * (376.9991397 + y))));
        a1 / a2
    } else {
        let z = 8.0 / ax;
        let y = z * z;
        let xx = ax - 2.356194491;
        let a1 = 1.0 + y * (0.183105e-2 + y * (-0.3516396496e-4 + y * (0.2457520174e-5 + y * (-0.240337019e-6))));
        let a2 = 0.04687499995 + y * (-0.2002690873e-3 + y * (0.8449199096e-5 + y * (-0.88228987e-6 + y * 0.105787412e-6)));
        let r = (0.636619772 / ax).sqrt() * (xx.cos() * a1 - z * xx.sin() * a2);
        if x < 0.0 { -r } else { r }
    }
}

/// jinc(x) = 2 J1(pi x) / (pi x), 1 at 0.
pub fn jinc(x: f32) -> f32 {
    if x.abs() < 1e-4 {
        return 1.0;
    }
    let p = std::f32::consts::PI * x;
    2.0 * bessel_j1(p) / p
}

/// The EWA LanczosSharp weight at distance `r` (in filter units).
pub fn ewa_weight(r: f32) -> f32 {
    let x = r / EWA_BLUR;
    if x >= EWA_SUPPORT {
        return 0.0;
    }
    jinc(x) * jinc(x * JINC_ZERO1 / EWA_SUPPORT)
}

/// `ewa_weight` from a 4096-step table, linearly interpolated. Computing the Bessel
/// function per tap took about a minute for an 11 MP enlargement; the table differs from
/// it by under 1e-5.
fn ewa_weight_fast(r: f32) -> f32 {
    const N: usize = 4096;
    static T: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| (0..=N + 1).map(|i| ewa_weight(i as f32 / N as f32 * EWA_SUPPORT * EWA_BLUR)).collect());
    let x = r / (EWA_SUPPORT * EWA_BLUR) * N as f32;
    if x >= N as f32 {
        return 0.0;
    }
    let i = x as usize;
    let f = x - i as f32;
    t[i] + (t[i + 1] - t[i]) * f
}

type Px = [f32; 4];

/// The source, premultiplied, 0-1, read with clamping at the edges.
struct Src {
    w: i32,
    h: i32,
    px: Vec<Px>,
}

impl Src {
    fn new(img: &RgbaImage) -> Src {
        let px = img
            .pixels()
            .map(|p| {
                let a = p[3] as f32 / 255.0;
                [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a]
            })
            .collect();
        Src { w: img.width() as i32, h: img.height() as i32, px }
    }
    fn at(&self, x: i32, y: i32) -> Px {
        self.px[(y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize]
    }
}

fn luma(p: Px) -> f32 {
    0.5 * p[2] + p[1] + 0.5 * p[0]
}

/// EASU's direction and length from one 2x2 corner: weight `w`, the cross of luma values
/// a (up), b (left), c (centre), d (right), e (down).
fn easu_set(dir: &mut [f32; 2], len: &mut f32, w: f32, a: f32, b: f32, c: f32, d: f32, e: f32) {
    let lx = (d - c).abs().max((c - b).abs());
    let dx = d - b;
    dir[0] += dx * w;
    let lx = if lx > 0.0 { (dx.abs() / lx).clamp(0.0, 1.0) } else { 0.0 };
    *len += lx * lx * w;
    let ly = (e - c).abs().max((c - a).abs());
    let dy = e - a;
    dir[1] += dy * w;
    let ly = if ly > 0.0 { (dy.abs() / ly).clamp(0.0, 1.0) } else { 0.0 };
    *len += ly * ly * w;
}

/// Where output pixel o of out falls in a source of inn pixels, exactly: the source
/// pixel and the fraction past its left edge. centred measures from pixel centres (EASU)
/// instead of edges. Integer arithmetic, so the GPU (the same formula in u32/i32) picks the
/// same pixel when a position lands on an edge: at 2400 to 4080 px every 17th one does.
pub fn exact(o: u32, inn: u32, out: u32, centred: bool) -> (i32, f32) {
    let num = (2 * o as i64 + 1) * inn as i64 - if centred { out as i64 } else { 0 };
    let den = 2 * out as i64;
    (num.div_euclid(den) as i32, num.rem_euclid(den) as f32 / den as f32)
}

/// One EASU output pixel: source pixel (ix, iy) and the fractions (x, y) past it, measured
/// from pixel centres.
fn easu_px(s: &Src, ix: i32, iy: i32, x: f32, y: f32) -> Px {
    // the 12 taps, by offset from (ix, iy)
    const OFF: [(i32, i32); 12] = [(0, -1), (1, -1), (-1, 0), (0, 0), (1, 0), (2, 0), (-1, 1), (0, 1), (1, 1), (2, 1), (0, 2), (1, 2)];
    let t: Vec<Px> = OFF.iter().map(|&(dx, dy)| s.at(ix + dx, iy + dy)).collect();
    let l: Vec<f32> = t.iter().map(|&p| luma(p)).collect();
    // b c / e f g h / i j k l / n o  ->  0 1 / 2 3 4 5 / 6 7 8 9 / 10 11
    let (mut dir, mut len) = ([0.0f32; 2], 0.0f32);
    easu_set(&mut dir, &mut len, (1.0 - x) * (1.0 - y), l[0], l[2], l[3], l[4], l[7]);
    easu_set(&mut dir, &mut len, x * (1.0 - y), l[1], l[3], l[4], l[5], l[8]);
    easu_set(&mut dir, &mut len, (1.0 - x) * y, l[3], l[6], l[7], l[8], l[10]);
    easu_set(&mut dir, &mut len, x * y, l[4], l[7], l[8], l[9], l[11]);
    let dr = dir[0] * dir[0] + dir[1] * dir[1];
    let (dx, dy) = if dr < 1.0 / 32768.0 { (1.0, 0.0) } else { (dir[0] / dr.sqrt(), dir[1] / dr.sqrt()) };
    let len = (len * 0.5) * (len * 0.5);
    let stretch = (dx * dx + dy * dy) / dx.abs().max(dy.abs());
    let (l2x, l2y) = (1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    let lob = 0.5 + (1.0 / 4.0 - 0.04 - 0.5) * len;
    let clp = 1.0 / lob;
    let (mut ac, mut aw) = ([0.0f32; 4], 0.0f32);
    for (k, &(ox, oy)) in OFF.iter().enumerate() {
        let (offx, offy) = (ox as f32 - x, oy as f32 - y);
        let vx = (offx * dx + offy * dy) * l2x;
        let vy = (offx * -dy + offy * dx) * l2y;
        let d2 = (vx * vx + vy * vy).min(clp);
        let wb = 2.0 / 5.0 * d2 - 1.0;
        let wa = lob * d2 - 1.0;
        let wb = 25.0 / 16.0 * wb * wb - (25.0 / 16.0 - 1.0);
        let w = wb * wa * wa;
        for c in 0..4 {
            ac[c] += t[k][c] * w;
        }
        aw += w;
    }
    // no ring past the 2x2 around the sample
    let mut o = [0.0f32; 4];
    for c in 0..4 {
        let (mn, mx) = (t[3][c].min(t[4][c]).min(t[7][c]).min(t[8][c]), t[3][c].max(t[4][c]).max(t[7][c]).max(t[8][c]));
        o[c] = if aw != 0.0 { (ac[c] / aw).clamp(mn, mx) } else { t[3][c] };
    }
    o
}

/// One EWA LanczosSharp output pixel at source position (px, py); the kernel is widened by
/// (fx, fy) on each axis when shrinking.
fn ewa_px(s: &Src, px: f32, py: f32, fx: f32, fy: f32, clamp: bool) -> Px {
    let (rx, ry) = (EWA_SUPPORT * EWA_BLUR * fx, EWA_SUPPORT * EWA_BLUR * fy);
    let (x0, x1) = ((px - rx).floor() as i32, (px + rx).ceil() as i32);
    let (y0, y1) = ((py - ry).floor() as i32, (py + ry).ceil() as i32);
    let (mut ac, mut aw) = ([0.0f32; 4], 0.0f32);
    let (mut mn, mut mx) = ([1.0f32; 4], [0.0f32; 4]);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = ((x as f32 + 0.5 - px) / fx, (y as f32 + 0.5 - py) / fy);
            let w = ewa_weight_fast((dx * dx + dy * dy).sqrt());
            if w == 0.0 {
                continue;
            }
            let p = s.at(x, y);
            for c in 0..4 {
                ac[c] += p[c] * w;
                mn[c] = mn[c].min(p[c]);
                mx[c] = mx[c].max(p[c]);
            }
            aw += w;
        }
    }
    let mut o = [0.0f32; 4];
    for c in 0..4 {
        let v = if aw != 0.0 { ac[c] / aw } else { 0.0 };
        o[c] = if clamp { v.clamp(mn[c], mx[c]) } else { v.clamp(0.0, 1.0) };
    }
    o
}

fn same(a: Px, b: Px) -> bool {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() + (a[3] - b[3]).abs() <= PIXEL_TOL
}

/// One Scale2x (`eagle` false) or Eagle output pixel: source pixel (ix, iy) and the
/// fractions (fx, fy) across it.
fn pixel_px(s: &Src, ix: i32, iy: i32, fx: f32, fy: f32, eagle: bool) -> Px {
    let (left, top) = (fx < 0.5, fy < 0.5);
    let g = |dx: i32, dy: i32| s.at(ix + dx, iy + dy);
    let (a, b, c) = (g(-1, -1), g(0, -1), g(1, -1));
    let (d, e, f) = (g(-1, 0), g(0, 0), g(1, 0));
    let (gg, h, i) = (g(-1, 1), g(0, 1), g(1, 1));
    if eagle {
        match (top, left) {
            (true, true) if same(a, b) && same(b, d) => a,
            (true, false) if same(b, c) && same(c, f) => c,
            (false, true) if same(d, gg) && same(gg, h) => gg,
            (false, false) if same(f, i) && same(i, h) => i,
            _ => e,
        }
    } else {
        match (top, left) {
            (true, true) if same(b, d) && !same(b, f) && !same(d, h) => d,
            (true, false) if same(b, f) && !same(b, d) && !same(f, h) => f,
            (false, true) if same(d, h) && !same(d, b) && !same(h, f) => d,
            (false, false) if same(h, f) && !same(d, h) && !same(b, f) => f,
            _ => e,
        }
    }
}

/// Resize with code 18-21 (bit 7: Clamp Ringing, used by EWA). `None` when the code is
/// not one of these.
pub fn resize(img: &RgbaImage, w: u32, h: u32, code: u8) -> Option<RgbaImage> {
    let mode = code & 0x7F;
    if !(18..=21).contains(&mode) {
        return None;
    }
    let (sw, sh) = img.dimensions();
    let shrinks = w < sw || h < sh;
    // EASU and the pixel rules only enlarge: a shrink is Area
    if mode != 19 && shrinks {
        return Some(crate::render::resize_table(img, w, h, 7 | (code & 0x80)));
    }
    // EWA past 2x reduction: Area to twice the target first
    let pre;
    let img = if mode == 19 && (sw > 2 * w || sh > 2 * h) {
        pre = crate::render::resize_table(img, (2 * w).min(sw), (2 * h).min(sh), 7);
        &pre
    } else {
        img
    };
    let s = Src::new(img);
    let (sx, sy) = (img.width() as f32 / w as f32, img.height() as f32 / h as f32);
    let clamp = code & 0x80 != 0;
    let mut out = vec![0u8; w as usize * h as usize * 4];
    let row = w as usize * 4;
    let n = h as usize;
    let k = std::thread::available_parallelism().map_or(4, |v| v.get()).min(n.max(1));
    let per = n.div_ceil(k.max(1)).max(1);
    std::thread::scope(|sc| {
        for (ci, chunk) in out.chunks_mut(per * row).enumerate() {
            let s = &s;
            sc.spawn(move || {
                for (j, r) in chunk.chunks_mut(row).enumerate() {
                    let y = ci * per + j;
                    for x in 0..w as usize {
                        let (px, py) = ((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy);
                        let (iw, ih) = (s.w as u32, s.h as u32);
                        let p = match mode {
                            18 => {
                                let ((ix, fx), (iy, fy)) = (exact(x as u32, iw, w, true), exact(y as u32, ih, h, true));
                                easu_px(s, ix, iy, fx, fy)
                            }
                            19 => ewa_px(s, px, py, sx.max(1.0), sy.max(1.0), clamp),
                            _ => {
                                let ((ix, fx), (iy, fy)) = (exact(x as u32, iw, w, false), exact(y as u32, ih, h, false));
                                pixel_px(s, ix, iy, fx, fy, mode == 21)
                            }
                        };
                        let a = p[3].clamp(0.0, 1.0);
                        let un = |v: f32| if a > 0.0 { ((v / a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
                        r[x * 4..x * 4 + 4].copy_from_slice(&[un(p[0]), un(p[1]), un(p[2]), (a * 255.0).round() as u8]);
                    }
                }
            });
        }
    });
    RgbaImage::from_raw(w, h, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jinc_zeros_and_weight() {
        assert!((jinc(0.0) - 1.0).abs() < 1e-6);
        assert!(jinc(JINC_ZERO1).abs() < 1e-3, "jinc first zero: {}", jinc(JINC_ZERO1));
        assert!(ewa_weight(0.0) > 0.99 && ewa_weight(EWA_SUPPORT * EWA_BLUR + 0.01) == 0.0);
        let worst = (0..10000).map(|i| i as f32 / 10000.0 * 3.3).map(|r| (ewa_weight(r) - ewa_weight_fast(r)).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "table error {worst}");
    }

    /// EWA enlarging 2400x1600 to 1.7x stays fast enough to view (was about a minute).
    #[test]
    fn ewa_enlarge_time() {
        let img = RgbaImage::from_fn(2400, 1600, |x, y| image::Rgba([(x ^ y) as u8, x as u8, y as u8, 255]));
        let t = std::time::Instant::now();
        let o = resize(&img, 4080, 2720, 19).unwrap();
        let ms = t.elapsed().as_millis();
        eprintln!("EWA 2400x1600 -> 4080x2720: {ms} ms");
        assert_eq!(o.dimensions(), (4080, 2720));
        assert!(ms < 15000, "{ms} ms");
    }

    /// A hard diagonal edge, enlarged 4x: Scale2x and Eagle keep only source colours;
    /// EASU and EWA blend, with EASU inside the 2x2 range by construction.
    #[test]
    fn modes_enlarge_a_diagonal() {
        let img = RgbaImage::from_fn(16, 16, |x, y| if x > y { image::Rgba([250, 30, 30, 255]) } else { image::Rgba([20, 20, 200, 255]) });
        for mode in [20u8, 21] {
            let o = resize(&img, 64, 64, mode).unwrap();
            assert!(o.pixels().all(|p| *p == image::Rgba([250, 30, 30, 255]) || *p == image::Rgba([20, 20, 200, 255])), "mode {mode} made a new colour");
        }
        let easu = resize(&img, 64, 64, 18).unwrap();
        assert!(easu.pixels().all(|p| p[0] >= 20 && p[0] <= 250 && p[2] >= 30 && p[2] <= 200));
        let ewa = resize(&img, 64, 64, 19).unwrap();
        let mid = ewa.pixels().filter(|p| p[0] > 40 && p[0] < 230).count();
        assert!(mid > 0, "EWA blends the edge");
        // Scale2x smooths the staircase: it differs from nearest
        let near = crate::render::resize(&img, 64, 64, 0);
        assert_ne!(resize(&img, 64, 64, 20).unwrap().into_raw(), near.into_raw());
    }
}
