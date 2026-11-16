//! Software rendering into a 0x00RRGGBB window buffer.

use image::RgbaImage;

pub struct Canvas<'a> {
    pub buf: &'a mut [u32],
    pub w: usize,
    pub h: usize,
}

#[inline]
fn rgb(r: u32, g: u32, b: u32) -> u32 {
    (r << 16) | (g << 8) | b
}

/// Blend `src` over `dst` with alpha `a` (0-255).
#[inline]
pub fn mix(dst: u32, src: u32, a: u32) -> u32 {
    let ia = 255 - a;
    let r = (((src >> 16) & 255) * a + ((dst >> 16) & 255) * ia + 127) / 255;
    let g = (((src >> 8) & 255) * a + ((dst >> 8) & 255) * ia + 127) / 255;
    let b = ((src & 255) * a + (dst & 255) * ia + 127) / 255;
    rgb(r, g, b)
}

#[inline]
fn bg_at(x: usize, y: usize, bg: u32, checker: bool) -> u32 {
    if checker {
        if ((x >> 3) ^ (y >> 3)) & 1 == 0 {
            0xFFFFFF
        } else {
            0xCCCCCC
        }
    } else {
        bg
    }
}

impl Canvas<'_> {
    pub fn fill(&mut self, c: u32) {
        self.buf.fill(c);
    }

    pub fn rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32, alpha: u32) {
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = ((x + w).max(0) as usize).min(self.w);
        let y1 = ((y + h).max(0) as usize).min(self.h);
        for yy in y0..y1 {
            let row = &mut self.buf[yy * self.w..(yy + 1) * self.w];
            for p in &mut row[x0.min(x1)..x1] {
                *p = mix(*p, color, alpha);
            }
        }
    }

    /// Blur a rectangle in place: three box passes each way (close to a Gaussian, sigma ~ r).
    /// The rectangle is box-downsampled by BLUR_F first; at 4x a 3440x142 strip costs 1/16 per pass.
    pub fn blur(&mut self, x: i32, y: i32, w: i32, h: i32, r: usize) {
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = ((x + w).max(0) as usize).min(self.w);
        let y1 = ((y + h).max(0) as usize).min(self.h);
        if x1 <= x0 + 2 || y1 <= y0 + 2 || r == 0 {
            return;
        }
        // box downsample by BLUR_F into planar channels
        let (sw, sh) = ((x1 - x0).div_ceil(BLUR_F), (y1 - y0).div_ceil(BLUR_F));
        let mut ch = [vec![0u32; sw * sh], vec![0u32; sw * sh], vec![0u32; sw * sh]];
        for sy in 0..sh {
            for sx in 0..sw {
                let (mut r_, mut g, mut b, mut n) = (0, 0, 0, 0);
                for dy in 0..BLUR_F {
                    for dx in 0..BLUR_F {
                        let (px, py) = (x0 + sx * BLUR_F + dx, y0 + sy * BLUR_F + dy);
                        if px < x1 && py < y1 {
                            let p = self.buf[py * self.w + px];
                            r_ += (p >> 16) & 255;
                            g += (p >> 8) & 255;
                            b += p & 255;
                            n += 1;
                        }
                    }
                }
                let i = sy * sw + sx;
                ch[0][i] = r_ / n;
                ch[1][i] = g / n;
                ch[2][i] = b / n;
            }
        }
        let rr = (r / BLUR_F).max(1);
        let mut tmp = vec![0u32; sw.max(sh)];
        for plane in ch.iter_mut() {
            for _ in 0..3 {
                for row in 0..sh {
                    box_line(plane, row * sw, 1, sw, rr, &mut tmp);
                }
                for col in 0..sw {
                    box_line(plane, col, sw, sh, rr, &mut tmp);
                }
            }
        }
        // bilinear upsample back into the canvas
        for py in y0..y1 {
            let fy = ((py - y0) as f32 + 0.5) / BLUR_F as f32 - 0.5;
            let ya = (fy.floor().max(0.0) as usize).min(sh - 1);
            let yb = (ya + 1).min(sh - 1);
            let ty = (fy - ya as f32).clamp(0.0, 1.0);
            for px in x0..x1 {
                let fx = ((px - x0) as f32 + 0.5) / BLUR_F as f32 - 0.5;
                let xa = (fx.floor().max(0.0) as usize).min(sw - 1);
                let xb = (xa + 1).min(sw - 1);
                let tx = (fx - xa as f32).clamp(0.0, 1.0);
                let s = |p: &Vec<u32>| {
                    let top = p[ya * sw + xa] as f32 * (1.0 - tx) + p[ya * sw + xb] as f32 * tx;
                    let bot = p[yb * sw + xa] as f32 * (1.0 - tx) + p[yb * sw + xb] as f32 * tx;
                    (top * (1.0 - ty) + bot * ty) as u32
                };
                self.buf[py * self.w + px] = rgb(s(&ch[0]), s(&ch[1]), s(&ch[2]));
            }
        }
    }
}

const BLUR_F: usize = 4;

/// One box-blur pass over `n` samples starting at `start`, `step` apart, radius `r` (edges clamp).
fn box_line(p: &mut [u32], start: usize, step: usize, n: usize, r: usize, tmp: &mut [u32]) {
    for i in 0..n {
        tmp[i] = p[start + i * step];
    }
    let win = (2 * r + 1) as u32;
    let at = |i: isize| tmp[i.clamp(0, n as isize - 1) as usize];
    let mut sum: u32 = (-(r as isize)..=r as isize).map(at).sum();
    for i in 0..n {
        p[start + i * step] = sum / win;
        sum += at(i as isize + r as isize + 1);
        sum -= at(i as isize - r as isize);
    }
}

impl Canvas<'_> {
    /// Horizontal inside-span of a rounded rect at pixel-centre row `py`, or None.
    pub fn rrect_span(py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<(f32, f32)> {
        if py < y || py > y + h {
            return None;
        }
        let dy = if py < y + r { y + r - py } else if py > y + h - r { py - (y + h - r) } else { 0.0 };
        if dy > r {
            return None;
        }
        let inset = r - (r * r - dy * dy).max(0.0).sqrt();
        Some((x + inset, x + w - inset))
    }

    /// Fill a rounded rect fast: solid row spans, antialiasing only near the edges.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: u32, alpha: f32) {
        let y0 = (y - 1.0).floor().max(0.0) as usize;
        let y1 = ((y + h + 1.0).ceil().max(0.0) as usize).min(self.h);
        let a_full = (alpha * 255.0) as u32;
        for py in y0..y1 {
            let cy = py as f32 + 0.5;
            // first and last rows: exact per-pixel coverage
            if cy < y + 1.0 || cy > y + h - 1.0 {
                self.sdf((x - 1.0, py as f32, x + w + 1.0, py as f32 + 1.0), color, alpha, |px, pyy| sd_rrect(px, pyy, x, y, w, h, r));
                continue;
            }
            let Some((l, rt)) = Self::rrect_span(cy, x, y, w, h, r) else { continue };
            // near the top/bottom of the caps the edge is shallow: widen the AA band there
            let band = if (l - x) > r * 0.3 { (r * 0.5).max(1.5) } else { 1.5 };
            let sl = (l - band).floor().max(0.0) as usize;
            let el = ((l + band).ceil().max(0.0) as usize).min(self.w);
            // clip to the canvas: a shape may extend past the right edge
            let sr = ((rt - band).floor().max(0.0) as usize).min(self.w);
            let er = ((rt + band).ceil().max(0.0) as usize).min(self.w);
            for px in sl..el.min(sr) {
                self.aa_px(px, py, color, alpha, sd_rrect(px as f32 + 0.5, cy, x, y, w, h, r));
            }
            for px in sr.max(el)..er {
                self.aa_px(px, py, color, alpha, sd_rrect(px as f32 + 0.5, cy, x, y, w, h, r));
            }
            if el < sr {
                let row = &mut self.buf[py * self.w..(py + 1) * self.w];
                for p in &mut row[el..sr] {
                    *p = mix(*p, color, a_full);
                }
            }
        }
    }

    #[inline]
    fn aa_px(&mut self, x: usize, y: usize, color: u32, alpha: f32, sd: f32) {
        let cov = (0.5 - sd).clamp(0.0, 1.0) * alpha;
        if cov > 0.0 {
            let i = y * self.w + x;
            self.buf[i] = mix(self.buf[i], color, (cov * 255.0) as u32);
        }
    }

    /// Outline of a rounded rect, `t` px thick, centred on its edge. Only edge regions are visited.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, t: f32, color: u32, alpha: f32) {
        let e = t + 2.0;
        let f = |px: f32, py: f32| sd_rrect(px, py, x, y, w, h, r).abs() - t / 2.0;
        self.sdf((x + r, y - e, x + w - r, y + e), color, alpha, f);
        self.sdf((x + r, y + h - e, x + w - r, y + h + e), color, alpha, f);
        self.sdf((x - e, y - e, x + r, y + h + e), color, alpha, f);
        self.sdf((x + w - r, y - e, x + w + e, y + h + e), color, alpha, f);
    }

    /// Antialiased fill of the region where `sd(x, y) < 0` (signed distance in pixels).
    pub fn sdf(&mut self, bbox: (f32, f32, f32, f32), color: u32, alpha: f32, sd: impl Fn(f32, f32) -> f32) {
        let x0 = bbox.0.floor().max(0.0) as usize;
        let y0 = bbox.1.floor().max(0.0) as usize;
        let x1 = (bbox.2.ceil().max(0.0) as usize).min(self.w);
        let y1 = (bbox.3.ceil().max(0.0) as usize).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                let cov = (0.5 - sd(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0) * alpha;
                if cov > 0.0 {
                    let i = y * self.w + x;
                    self.buf[i] = mix(self.buf[i], color, (cov * 255.0) as u32);
                }
            }
        }
    }
}

// ---- signed distance functions (negative inside) ----

pub fn sd_circle(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> f32 {
    (px - cx).hypot(py - cy) - r
}

pub fn sd_rrect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (px - (x + w / 2.0)).abs() - (w / 2.0 - r);
    let qy = (py - (y + h / 2.0)).abs() - (h / 2.0 - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}




/// Draw `img` at (ox, oy), blending its alpha over what is already on the canvas.
pub fn blit_over(c: &mut Canvas, img: &RgbaImage, ox: i64, oy: i64) {
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    for y in oy.max(0)..(oy + ih).min(c.h as i64) {
        for x in ox.max(0)..(ox + iw).min(c.w as i64) {
            let p = img.get_pixel((x - ox) as u32, (y - oy) as u32);
            if p[3] > 0 {
                let i = y as usize * c.w + x as usize;
                c.buf[i] = mix(c.buf[i], rgb(p[0] as u32, p[1] as u32, p[2] as u32), p[3] as u32);
            }
        }
    }
}

/// Copy an already-scaled image at integer offset (ox, oy).
pub fn blit(c: &mut Canvas, img: &RgbaImage, ox: i64, oy: i64, bg: u32, checker: bool) {
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let x0 = ox.max(0);
    let y0 = oy.max(0);
    let x1 = (ox + iw).min(c.w as i64);
    let y1 = (oy + ih).min(c.h as i64);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let raw = img.as_raw();
    let stride = iw as usize * 4;
    for y in y0..y1 {
        let sy = (y - oy) as usize;
        let srow = &raw[sy * stride..(sy + 1) * stride];
        let drow = &mut c.buf[y as usize * c.w..(y as usize + 1) * c.w];
        for x in x0..x1 {
            let sx = (x - ox) as usize * 4;
            let a = srow[sx + 3] as u32;
            let px = rgb(srow[sx] as u32, srow[sx + 1] as u32, srow[sx + 2] as u32);
            drow[x as usize] = if a == 255 {
                px
            } else {
                mix(bg_at(x as usize, y as usize, bg, checker), px, a)
            };
        }
    }
}

/// Draw `img` scaled by `z` (z >= 1) with its top-left at (ox, oy), sampling only the visible part.
#[allow(clippy::too_many_arguments)]
pub fn blit_sample(
    c: &mut Canvas,
    img: &RgbaImage,
    ox: f64,
    oy: f64,
    z: f64,
    bilinear: bool,
    bg: u32,
    checker: bool,
) {
    let (iw, ih) = (img.width() as usize, img.height() as usize);
    if iw == 0 || ih == 0 {
        return;
    }
    let sw = iw as f64 * z;
    let sh = ih as f64 * z;
    let x0 = ox.max(0.0).floor() as usize;
    let y0 = oy.max(0.0).floor() as usize;
    let x1 = ((ox + sw).min(c.w as f64).ceil().max(0.0) as usize).min(c.w);
    let y1 = ((oy + sh).min(c.h as f64).ceil().max(0.0) as usize).min(c.h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let raw = img.as_raw();
    let stride = iw * 4;
    let put = |drow: &mut [u32], x: usize, y: usize, r: u32, g: u32, b: u32, a: u32| {
        let px = rgb(r, g, b);
        drow[x] = if a >= 255 {
            px
        } else {
            mix(bg_at(x, y, bg, checker), px, a)
        };
    };
    if !bilinear {
        let xs: Vec<usize> = (x0..x1)
            .map(|x| (((x as f64 + 0.5 - ox) / z) as isize).clamp(0, iw as isize - 1) as usize * 4)
            .collect();
        for y in y0..y1 {
            let sy = (((y as f64 + 0.5 - oy) / z) as isize).clamp(0, ih as isize - 1) as usize;
            let srow = &raw[sy * stride..(sy + 1) * stride];
            let drow = &mut c.buf[y * c.w..(y + 1) * c.w];
            for (i, x) in (x0..x1).enumerate() {
                let s = xs[i];
                put(drow, x, y, srow[s] as u32, srow[s + 1] as u32, srow[s + 2] as u32, srow[s + 3] as u32);
            }
        }
        return;
    }
    // Bilinear: precompute per-column source indices and weights (8-bit fixed point).
    let cols: Vec<(usize, usize, u32)> = (x0..x1)
        .map(|x| {
            let fx = ((x as f64 + 0.5 - ox) / z - 0.5).clamp(0.0, (iw - 1) as f64);
            let a = fx.floor() as usize;
            let b = (a + 1).min(iw - 1);
            (a * 4, b * 4, ((fx - a as f64) * 256.0) as u32)
        })
        .collect();
    // rows split across the cores: a full-window frame is 7 MP on a Retina display
    let cw = c.w;
    let rows = &mut c.buf[y0 * cw..y1 * cw];
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let per = (y1 - y0).div_ceil(n).max(1);
    let cols = &cols;
    std::thread::scope(|sc| {
        for (k, chunk) in rows.chunks_mut(per * cw).enumerate() {
            sc.spawn(move || {
                for (j, drow) in chunk.chunks_mut(cw).enumerate() {
                    let y = y0 + k * per + j;
                    let fy = ((y as f64 + 0.5 - oy) / z - 0.5).clamp(0.0, (ih - 1) as f64);
                    let ya = fy.floor() as usize;
                    let yb = (ya + 1).min(ih - 1);
                    let ty = ((fy - ya as f64) * 256.0) as u32;
                    let ra = &raw[ya * stride..(ya + 1) * stride];
                    let rb = &raw[yb * stride..(yb + 1) * stride];
                    for (i, x) in (x0..x1).enumerate() {
                        let (xa, xb, tx) = cols[i];
                        let ch = |k: usize| -> u32 {
                            let top = ra[xa + k] as u32 * (256 - tx) + ra[xb + k] as u32 * tx;
                            let bot = rb[xa + k] as u32 * (256 - tx) + rb[xb + k] as u32 * tx;
                            (top * (256 - ty) + bot * ty) >> 16
                        };
                        put(drow, x, y, ch(0), ch(1), ch(2), ch(3));
                    }
                }
            });
        }
    });
}

/// Resize. filter: 0 nearest, 1 bilinear, 2 Lanczos3, 3 quick preview, 4 bicubic.
/// Unsharp mask: out = px + amount * (px - Gaussian(px, sigma)) per colour channel,
/// only where |px - blur| > threshold; alpha kept. Separable blur, half-width 2.5 sigma.
pub fn sharpen(img: &mut RgbaImage, amount: f32, sigma: f32, threshold: u8) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 2 || h < 2 || amount <= 0.0 {
        return;
    }
    let r = (sigma * 2.5).ceil().max(1.0) as usize;
    let mut k: Vec<f32> = (0..=2 * r).map(|i| (-(i as f32 - r as f32).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
    let s: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    let src = img.as_raw().clone();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let band = h.div_ceil(threads);
    // horizontal pass into planar f32, in bands of rows
    let mut tmp = vec![0f32; w * h * 3];
    std::thread::scope(|s| {
        for (b, out) in tmp.chunks_mut(band * w * 3).enumerate() {
            let (src, k) = (&src, &k);
            s.spawn(move || {
                for (yy, row) in out.chunks_mut(w * 3).enumerate() {
                    let y = b * band + yy;
                    for x in 0..w {
                        for c in 0..3 {
                            let mut a = 0.0;
                            for (i, kv) in k.iter().enumerate() {
                                let sx = (x + i).saturating_sub(r).min(w - 1);
                                a += kv * src[(y * w + sx) * 4 + c] as f32;
                            }
                            row[x * 3 + c] = a;
                        }
                    }
                }
            });
        }
    });
    // vertical pass, writing the result
    let dst: &mut [u8] = img;
    let thr = threshold as f32;
    std::thread::scope(|s| {
        for (b, out) in dst.chunks_mut(band * w * 4).enumerate() {
            let (src, k, tmp) = (&src, &k, &tmp);
            s.spawn(move || {
                for (yy, row) in out.chunks_mut(w * 4).enumerate() {
                    let y = b * band + yy;
                    for x in 0..w {
                        for c in 0..3 {
                            let mut blur = 0.0;
                            for (i, kv) in k.iter().enumerate() {
                                let sy = (y + i).saturating_sub(r).min(h - 1);
                                blur += kv * tmp[(sy * w + x) * 3 + c];
                            }
                            let p = src[(y * w + x) * 4 + c] as f32;
                            let d = p - blur;
                            if d.abs() > thr {
                                row[x * 4 + c] = (p + amount * d).round().clamp(0.0, 255.0) as u8;
                            }
                        }
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharpen_edges_not_flats() {
        // left half 100, right half 150
        let mut img = RgbaImage::from_fn(64, 64, |x, _| if x < 32 { image::Rgba([100, 100, 100, 255]) } else { image::Rgba([150, 150, 150, 255]) });
        let t = std::time::Instant::now();
        sharpen(&mut img, 0.6, 1.0, 0);
        eprintln!("64x64 in {:?}", t.elapsed());
        assert_eq!(img.get_pixel(5, 5)[0], 100, "flat area unchanged");
        assert!(img.get_pixel(31, 5)[0] < 100, "dark side of the edge darker");
        assert!(img.get_pixel(32, 5)[0] > 150, "light side of the edge lighter");
        assert_eq!(img.get_pixel(32, 5)[3], 255, "alpha kept");
        let mut none = RgbaImage::from_pixel(8, 8, image::Rgba([7, 7, 7, 255]));
        sharpen(&mut none, 0.0, 1.0, 0);
        assert_eq!(none.get_pixel(3, 3)[0], 7);
    }

    #[test]
    fn shapes_past_the_edges_are_clipped() {
        // regression: a rect 8 px past the right edge panicked (range end 884 of 880)
        let mut buf = vec![0u32; 880 * 100];
        let mut c = Canvas { buf: &mut buf, w: 880, h: 100 };
        c.fill_rrect(232.0, -10.0, 656.0, 130.0, 8.0, 0xFFFFFF, 1.0);
        c.fill_rrect(-20.0, 10.0, 40.0, 40.0, 8.0, 0xFFFFFF, 1.0);
        c.stroke_rrect(860.0, 50.0, 60.0, 80.0, 8.0, 1.0, 0xFFFFFF, 1.0);
        assert_eq!(c.buf[50 * 880 + 879] & 0xFFFFFF, 0xFFFFFF);
    }

    #[test]
    fn extra_kernels_differ_from_lanczos3() {
        let img = RgbaImage::from_fn(200, 200, |x, y| image::Rgba([((x * 7) ^ (y * 3)) as u8, (x * 5) as u8, (y * 11) as u8, 255]));
        let l3 = resize(&img, 73, 73, 2);
        let diff = |o: &RgbaImage| o.as_raw().iter().zip(l3.as_raw()).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64).sum::<u64>();
        let (l4, mi) = (resize(&img, 73, 73, 5), resize(&img, 73, 73, 6));
        eprintln!("sum |diff| vs Lanczos3: Lanczos4 {}, Mitchell {}", diff(&l4), diff(&mi));
        assert!(diff(&l4) > 0 && diff(&mi) > 0);
    }

    #[test]
    fn threshold_skips_small_steps() {
        let mut img = RgbaImage::from_fn(32, 8, |x, _| if x < 16 { image::Rgba([100, 100, 100, 255]) } else { image::Rgba([102, 102, 102, 255]) });
        sharpen(&mut img, 1.0, 1.0, 4);
        assert_eq!(img.get_pixel(15, 2)[0], 100);
        assert_eq!(img.get_pixel(16, 2)[0], 102);
    }

    #[test]
    fn sharpen_cost_1280x860() {
        let mut img = RgbaImage::from_fn(1440, 3404, |x, y| image::Rgba([(x ^ y) as u8, x as u8, y as u8, 255]));
        let t = std::time::Instant::now();
        sharpen(&mut img, 0.6, 1.0, 0);
        eprintln!("sharpen 1440x3404 sigma 1: {:?}", t.elapsed());
    }
}

/// Lanczos with a 4-lobe window: sharper than Lanczos3, with slightly more ringing.
fn lanczos4(x: f64) -> f64 {
    let sinc = |x: f64| if x == 0.0 { 1.0 } else { (x * std::f64::consts::PI).sin() / (x * std::f64::consts::PI) };
    if (-4.0..4.0).contains(&x) { sinc(x) * sinc(x / 4.0) } else { 0.0 }
}

pub fn resize(img: &RgbaImage, w: u32, h: u32, filter: u8) -> RgbaImage {
    use fast_image_resize as fr;
    let w = w.max(1);
    let h = h.max(1);
    let fallback = || image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle);
    let alg = match filter {
        0 => fr::ResizeAlg::Nearest,
        1 => fr::ResizeAlg::Convolution(fr::FilterType::Bilinear),
        // Quick preview for the first frame of a large image.
        3 => fr::ResizeAlg::SuperSampling(fr::FilterType::Bilinear, 2),
        4 => fr::ResizeAlg::Convolution(fr::FilterType::CatmullRom),
        5 => match fr::Filter::new("Lanczos4", lanczos4, 4.0) {
            Ok(f) => fr::ResizeAlg::Convolution(fr::FilterType::Custom(f)),
            Err(_) => fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3),
        },
        6 => fr::ResizeAlg::Convolution(fr::FilterType::Mitchell),
        _ => fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3),
    };
    let Ok(src) = fr::images::ImageRef::new(img.width(), img.height(), img.as_raw(), fr::PixelType::U8x4) else {
        return fallback();
    };
    let mut dst = fr::images::Image::new(w, h, fr::PixelType::U8x4);
    let mut r = fr::Resizer::new();
    if r.resize(&src, &mut dst, &fr::ResizeOptions::new().resize_alg(alg)).is_err() {
        return fallback();
    }
    RgbaImage::from_raw(w, h, dst.into_vec()).unwrap_or_else(fallback)
}
