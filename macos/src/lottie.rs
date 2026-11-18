//! Lottie animations (.json, .lottie) drawn with rlottie, linked statically from vcpkg.
//! Frames are drawn when shown (`Player`), not up front.

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

/// A Lottie animation kept as its vector data: each frame is drawn when it is shown, into
/// one reused buffer. (Drawing every frame up front held them all: 709 MB in RAM, or
/// 950 MB of GPU textures, for a 30-frame test file at 2048 px.)
pub struct Player {
    anim: *mut Anim,
    pub w: u32,
    pub h: u32,
    pub frames: usize,
    pub delay_ms: u32,
    /// the last frame drawn, and the drawing buffer (one caller at a time)
    last: std::sync::Mutex<(usize, Option<std::sync::Arc<RgbaImage>>, Vec<u32>)>,
    /// GPU mode: each frame drawn once and kept as a texture (all of them within
    /// GPU_FRAMES bytes: the frames are drawn smaller when needed); later loops replay
    /// them without drawing. Measured on a 30-frame 2048 px file: drawing every frame as
    /// it played took 49% CPU; replaying textures 0.6%.
    tex: std::sync::Mutex<Vec<Option<std::sync::Arc<crate::gpu::Tex>>>>,
}

/// GPU mode: all of an animation's frame textures, mipmaps included, stay under this.
const GPU_FRAMES: f64 = 256.0 * 1024.0 * 1024.0;

// rlottie's animation is used behind the mutex only
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { lottie_animation_destroy(self.anim) };
    }
}

impl Player {
    /// `gpu`: frames will be kept as textures, so they are drawn at a size where all of
    /// them fit in GPU_FRAMES.
    pub fn new(bytes: &[u8], ext: &str, gpu: bool) -> Result<Player, String> {
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
            // vector: draw the longer side at 2048 px like SVG (at most 8192)
            let long = w.max(h).max(1) as f64;
            let mut s = (long.max(2048.0).min(8192.0)) / long;
            if gpu {
                // 4 bytes a pixel, a third more for the mipmaps
                let need = (w * h * 4 * n) as f64 * s * s * 4.0 / 3.0;
                if need > GPU_FRAMES {
                    s *= (GPU_FRAMES / need).sqrt();
                }
            }
            let (w, h) = (((w as f64 * s) as usize).max(1), ((h as f64 * s) as usize).max(1));
            Ok(Player {
                anim: a,
                w: w as u32,
                h: h as u32,
                frames: n,
                delay_ms: delay,
                last: std::sync::Mutex::new((usize::MAX, None, Vec::new())),
                tex: std::sync::Mutex::new(vec![None; n]),
            })
        }
    }

    /// GPU mode: frame `i` as a texture, drawn and uploaded the first time only. None when
    /// the GPU path is off.
    pub fn frame_tex(&self, i: usize) -> Option<std::sync::Arc<crate::gpu::Tex>> {
        let i = i.min(self.frames - 1);
        if let Some(t) = self.tex.lock().unwrap()[i].clone() {
            return Some(t);
        }
        let t = crate::gpu::upload_shared(&self.frame(i))?;
        self.tex.lock().unwrap()[i] = Some(t.clone());
        Some(t)
    }

    /// GPU memory its frame textures hold.
    pub fn tex_bytes(&self) -> u64 {
        self.tex.lock().unwrap().iter().flatten().map(|t| t.bytes).sum()
    }

    /// Frame `i`, drawn now (or the same frame again, if it was the last one drawn).
    pub fn frame(&self, i: usize) -> std::sync::Arc<RgbaImage> {
        let mut g = self.last.lock().unwrap();
        if g.0 == i {
            if let Some(img) = &g.1 {
                return img.clone();
            }
        }
        let (w, h) = (self.w as usize, self.h as usize);
        g.2.resize(w * h, 0);
        unsafe { lottie_animation_render(self.anim, i.min(self.frames - 1), g.2.as_mut_ptr(), w, h, w * 4) };
        // premultiplied BGRA words to straight RGBA bytes
        let mut px = Vec::with_capacity(w * h * 4);
        for &p in &g.2 {
            let (a8, r, gg, b) = ((p >> 24) as u32, (p >> 16) & 255, (p >> 8) & 255, p & 255);
            let un = |c: u32| if a8 == 0 { 0 } else { ((c * 255 + a8 / 2) / a8).min(255) as u8 };
            px.extend_from_slice(&[un(r), un(gg), un(b), a8 as u8]);
        }
        let img = std::sync::Arc::new(RgbaImage::from_raw(self.w, self.h, px).unwrap());
        *g = (i, Some(img.clone()), std::mem::take(&mut g.2));
        img
    }
}

#[cfg(test)]
mod tests {
    /// A 30-frame spinning square: 30 frames at 33 ms, a cyan centre, a clear corner.
    #[test]
    fn decodes_spin() {
        let b = std::fs::read("tests/fixtures/spin.json").unwrap();
        assert!(super::is_lottie_json(&b));
        let pl = super::Player::new(&b, "json", false).unwrap();
        let f = [pl.frame(0), pl.frame(15)];
        // 100 px source drawn at 2048 px: scale 20.48
        assert_eq!((pl.frames, pl.delay_ms, f[0].width(), f[0].height()), (30, 33, 2048, 2048));
        let p = |v: f32| (v * 20.48) as u32;
        assert_eq!(f[0].get_pixel(p(50.0), p(50.0)).0, [0, 204, 255, 255]);
        assert_eq!(f[0].get_pixel(p(2.0), p(2.0)).0[3], 0);
        // (50, 75) is outside the square at 0 degrees, inside the diamond at 45
        assert_eq!(f[0].get_pixel(p(50.0), p(75.0)).0[3], 0);
        assert_eq!(f[1].get_pixel(p(50.0), p(75.0)).0[3], 255);
    }
}
