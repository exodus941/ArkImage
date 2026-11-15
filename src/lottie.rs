//! Lottie animations (.json, .lottie) drawn with rlottie, linked statically from vcpkg.
//! Every frame is drawn up front into the same frame list a GIF uses.

use std::{ffi::c_char, io::Read, sync::atomic::AtomicU64};

use image::RgbaImage;

#[repr(C)]
struct Anim {
    _p: [u8; 0],
}

extern "C" {
    fn lottie_animation_from_data(data: *const c_char, key: *const c_char, res: *const c_char) -> *mut Anim;
    fn lottie_animation_destroy(a: *mut Anim);
    fn lottie_animation_get_size(a: *const Anim, w: *mut usize, h: *mut usize);
    fn lottie_animation_get_totalframe(a: *const Anim) -> usize;
    fn lottie_animation_get_framerate(a: *const Anim) -> f64;
    fn lottie_animation_render(a: *mut Anim, frame: usize, buf: *mut u32, w: usize, h: usize, stride: usize);
}

/// All frames together stay under this many bytes; bigger animations draw smaller.
const BUDGET: f64 = 512.0 * 1024.0 * 1024.0;

/// True if `bytes` look like Lottie JSON: an object with "layers" and a frame rate "fr".
pub fn is_lottie_json(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1 << 20)];
    let s = head.iter().position(|b| !b.is_ascii_whitespace()).map(|i| head[i]);
    let has = |k: &[u8]| head.windows(k.len()).any(|w| w == k);
    s == Some(b'{') && has(b"\"layers\"") && has(b"\"fr\"")
}

/// The first animation JSON inside a .lottie (zip) file.
fn from_dotlottie(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = z.file_names().filter(|n| n.starts_with("animations/") && n.ends_with(".json")).map(String::from).collect();
    names.sort();
    let name = names.first().ok_or("no animation in the .lottie file")?.clone();
    let mut out = Vec::new();
    z.by_name(&name).map_err(|e| e.to_string())?.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Draw every frame. Returns (frames, delay per frame in ms).
pub fn decode(bytes: &[u8], ext: &str) -> Result<(Vec<RgbaImage>, u32), String> {
    let mut json = if ext == "lottie" || bytes.starts_with(b"PK") { from_dotlottie(bytes)? } else { bytes.to_vec() };
    json.retain(|&b| b != 0);
    json.push(0);
    // rlottie caches by key; a fresh key per decode keeps two files apart
    static N: AtomicU64 = AtomicU64::new(0);
    let key = format!("arkimage-{}\0", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    unsafe {
        let a = lottie_animation_from_data(json.as_ptr() as _, key.as_ptr() as _, b"\0".as_ptr() as _);
        if a.is_null() {
            return Err("not a Lottie animation".into());
        }
        let (mut w, mut h) = (0usize, 0usize);
        lottie_animation_get_size(a, &mut w, &mut h);
        // rlottie reports op - ip + 1; frame op is past the end and would stutter the loop
        let n = lottie_animation_get_totalframe(a).saturating_sub(1).max(1);
        let fps = lottie_animation_get_framerate(a);
        let delay = if fps > 0.0 { (1000.0 / fps).round().max(10.0) as u32 } else { 33 };
        // vector: draw the longer side at 2048 px like SVG, then shrink to fit the budget
        let long = w.max(h).max(1) as f64;
        let mut s = (long.max(2048.0).min(8192.0)) / long;
        let need = (w * h * 4 * n) as f64 * s * s;
        if need > BUDGET {
            s *= (BUDGET / need).sqrt();
        }
        let (w, h) = (((w as f64 * s) as usize).max(1), ((h as f64 * s) as usize).max(1));
        let mut frames = Vec::with_capacity(n);
        let mut buf = vec![0u32; w * h];
        for i in 0..n {
            lottie_animation_render(a, i, buf.as_mut_ptr(), w, h, w * 4);
            // premultiplied BGRA words to straight RGBA bytes
            let mut px = Vec::with_capacity(w * h * 4);
            for &p in &buf {
                let (a8, r, g, b) = ((p >> 24) as u32, (p >> 16) & 255, (p >> 8) & 255, p & 255);
                let un = |c: u32| if a8 == 0 { 0 } else { ((c * 255 + a8 / 2) / a8).min(255) as u8 };
                px.extend_from_slice(&[un(r), un(g), un(b), a8 as u8]);
            }
            frames.push(RgbaImage::from_raw(w as u32, h as u32, px).unwrap());
        }
        lottie_animation_destroy(a);
        Ok((frames, delay))
    }
}

#[cfg(test)]
mod tests {
    /// A 30-frame spinning square: 30 frames at 33 ms, a cyan centre, a clear corner.
    #[test]
    fn decodes_spin() {
        let b = std::fs::read("tests/fixtures/spin.json").unwrap();
        assert!(super::is_lottie_json(&b));
        let (f, d) = super::decode(&b, "json").unwrap();
        // 100 px source drawn at 2048 px: scale 20.48
        assert_eq!((f.len(), d, f[0].width(), f[0].height()), (30, 33, 2048, 2048));
        let p = |v: f32| (v * 20.48) as u32;
        assert_eq!(f[0].get_pixel(p(50.0), p(50.0)).0, [0, 204, 255, 255]);
        assert_eq!(f[0].get_pixel(p(2.0), p(2.0)).0[3], 0);
        // (50, 75) is outside the square at 0 degrees, inside the diamond at 45
        assert_eq!(f[0].get_pixel(p(50.0), p(75.0)).0[3], 0);
        assert_eq!(f[15].get_pixel(p(50.0), p(75.0)).0[3], 255);
    }
}
