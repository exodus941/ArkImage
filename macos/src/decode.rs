//! Decoding to RGBA frames, EXIF reading, and view transforms.

use std::{io::Cursor, sync::Arc, time::SystemTime};

use exif::{In, Tag};
use image::{
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
    imageops, AnimationDecoder, ImageFormat, ImageReader, RgbaImage,
};

use crate::source::Entry;

pub struct Frame {
    /// The image, or with `gpu` set only a thumbnail of it (at most THUMB px a side).
    pub img: Arc<RgbaImage>,
    pub delay_ms: u32,
    /// GPU path: the full image as a shared texture; RAM keeps only the thumbnail.
    pub gpu: Option<Arc<crate::gpu::Tex>>,
}

/// The long side of the thumbnail kept in RAM when the image lives on the GPU.
pub const THUMB: u32 = 512;

/// A thumbnail of at most THUMB px a side.
pub fn thumb(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let k = (THUMB as f64 / w.max(h) as f64).min(1.0);
    let (tw, th) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    if (tw, th) == (w, h) {
        return img.clone();
    }
    crate::render::resize(img, tw, th, 1)
}

pub struct Decoded {
    pub frames: Vec<Frame>,
    pub format: String,
    pub file_size: u64,
    pub modified: Option<SystemTime>,
    pub exif: Vec<(&'static str, String)>,
    pub orig_w: u32,
    pub orig_h: u32,
    /// EXIF orientation (1-8) still to apply when shown: the GPU path uploads a turned
    /// photo as stored and turns it in the shader. 1: nothing to turn (the processor path
    /// turns the pixels while decoding). `orig_w` x `orig_h` is the size once turned; the
    /// thumbnail is stored turned.
    pub orient: u8,
    /// A Lottie animation: frames are drawn when shown; `frames` holds only the first.
    pub lottie: Option<Arc<crate::lottie::Player>>,
}

impl Decoded {
    /// Frames in the animation (1 for a still image).
    pub fn frame_count(&self) -> usize {
        self.lottie.as_ref().map(|p| p.frames).unwrap_or(self.frames.len())
    }

    /// Delay after frame `i`, ms.
    pub fn frame_delay(&self, i: usize) -> u32 {
        match &self.lottie {
            Some(p) => p.delay_ms,
            None => self.frames[i.min(self.frames.len().saturating_sub(1))].delay_ms,
        }
    }

    /// Memory it holds: RAM, plus GPU memory for frames that live on the GPU (one budget,
    /// the cache size setting, for both).
    pub fn bytes(&self) -> u64 {
        self.frames.iter().map(|f| f.img.as_raw().len() as u64 + f.gpu.as_ref().map(|t| t.bytes).unwrap_or(0)).sum::<u64>()
            + self.lottie.as_ref().map(|p| p.tex_bytes()).unwrap_or(0)
    }
}

fn es<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn collect<'a, D: AnimationDecoder<'a>>(d: D) -> Result<Vec<Frame>, String> {
    let frames = d.into_frames().collect_frames().map_err(es)?;
    Ok(frames
        .into_iter()
        .map(|f| {
            let (n, dn) = f.delay().numer_denom_ms();
            let ms = if dn == 0 { 100 } else { n / dn };
            Frame {
                img: Arc::new(f.into_buffer()),
                // Browsers treat 0-10 ms frame delays as 100 ms.
                delay_ms: if ms <= 10 { 100 } else { ms },
                gpu: None,
            }
        })
        .collect())
}

fn single(img: RgbaImage) -> Vec<Frame> {
    vec![Frame { img: Arc::new(img), delay_ms: 0, gpu: None }]
}

/// Rasterise an SVG. A vector has no pixel size, so the longer side is drawn at
/// 2048 px at least (sharp at zoom), and at most 8192 px; aspect is kept.
fn svg(bytes: &[u8]) -> Result<RgbaImage, String> {
    use resvg::{tiny_skia, usvg};
    // system fonts for <text>, loaded once per process
    static FONTS: std::sync::OnceLock<std::sync::Arc<usvg::fontdb::Database>> = std::sync::OnceLock::new();
    let mut opt = usvg::Options::default();
    // loading every system font costs about 2.5 s: only for text, or a compressed file
    let has_text = bytes.windows(5).any(|w| w == b"<text") || !bytes.starts_with(b"<");
    if has_text { opt.fontdb = FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            std::sync::Arc::new(db)
        })
        .clone(); }
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(es)?;
    let size = tree.size();
    let long = size.width().max(size.height());
    let scale = (long.max(2048.0).min(8192.0)) / long;
    let (w, h) = ((size.width() * scale).ceil() as u32, (size.height() * scale).ceil() as u32);
    let mut pm = tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or("SVG too large")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    // tiny-skia stores premultiplied alpha; the viewer wants straight alpha
    let mut px = pm.take();
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(w.max(1), h.max(1), px).ok_or_else(|| "bad SVG data".into())
}

// A Lottie animation, set by decode_frames for the caller (frames are drawn when shown).
thread_local! {
    static LOTTIE: std::cell::RefCell<Option<Arc<crate::lottie::Player>>> = const { std::cell::RefCell::new(None) };
}

fn decode_frames(bytes: &[u8], ext: &str) -> Result<(Vec<Frame>, String), String> {
    if matches!(ext, "psd" | "psb") || bytes.starts_with(b"8BPS") {
        if let Some(img) = crate::psd_fast::decode(bytes) {
            return Ok((single(img), if bytes.get(4..6) == Some(&[0, 2]) { "PSB" } else { "PSD" }.into()));
        }
        let p = psd::Psd::from_bytes(bytes).map_err(es)?;
        let img = RgbaImage::from_raw(p.width(), p.height(), p.rgba()).ok_or("bad PSD data")?;
        return Ok((single(img), "PSD".into()));
    }
    #[cfg(lottie)]
    if ext == "lottie" || (ext == "json" && crate::lottie::is_lottie_json(bytes)) {
        let gpu = crate::gpu::DECODE_TO_GPU.load(std::sync::atomic::Ordering::Relaxed);
        let p = Arc::new(crate::lottie::Player::new(bytes, ext, gpu)?);
        let name = if p.frames > 1 { "Lottie (animated)" } else { "Lottie" };
        let first = Frame { img: p.frame(0), delay_ms: p.delay_ms, gpu: None };
        LOTTIE.with(|l| *l.borrow_mut() = Some(p));
        return Ok((vec![first], name.into()));
    }
    if matches!(ext, "svg" | "svgz") {
        return Ok((single(svg(bytes)?), "SVG".into()));
    }
    if crate::heic::is_heif(bytes) || matches!(ext, "heic" | "heif" | "hif" | "heics" | "heifs" | "avif" | "avifs") {
        return Ok((single(crate::heic::decode(bytes)?), "HEIF".into()));
    }
    match image::guess_format(bytes).ok() {
        Some(ImageFormat::Gif) => {
            let d = GifDecoder::new(Cursor::new(bytes)).map_err(es)?;
            let f = collect(d)?;
            if !f.is_empty() {
                let name = if f.len() > 1 { "GIF (animated)" } else { "GIF" };
                return Ok((f, name.into()));
            }
        }
        Some(ImageFormat::WebP) => {
            if let Some(img) = crate::webp_fast::decode_lossy_still(bytes) {
                return Ok((single(img), "WebP (lossy)".into()));
            }
            let d = WebPDecoder::new(Cursor::new(bytes)).map_err(es)?;
            if d.has_animation() {
                return Ok((collect(d)?, "WebP (animated)".into()));
            }
        }
        Some(ImageFormat::Png) => {
            let d = PngDecoder::new(Cursor::new(bytes)).map_err(es)?;
            if d.is_apng().unwrap_or(false) {
                let f = collect(d.apng().map_err(es)?)?;
                if f.len() > 1 {
                    return Ok((f, "APNG".into()));
                }
            }
        }
        _ => {}
    }
    // JPEG: libjpeg-turbo, the decoder the GPU path reads bands from, so both paths show
    // the same pixels (the image crate's decoder differed by up to 5 levels)
    if image::guess_format(bytes).ok() == Some(ImageFormat::Jpeg) {
        let run = || -> Option<RgbaImage> {
            let d = mozjpeg::Decompress::new_mem(bytes).ok()?;
            let mut s = d.rgba().ok()?;
            let (w, h) = (s.width() as u32, s.height() as u32);
            let mut buf = vec![0u8; (w * h * 4) as usize];
            s.read_scanlines_into(&mut buf[..]).ok()?;
            let _ = s.finish();
            RgbaImage::from_raw(w, h, buf)
        };
        if let Ok(Some(img)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
            return Ok((single(img), "JPEG".into()));
        }
    }
    let mut r = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(es)?;
    if r.format().is_none() {
        if let Some(f) = ImageFormat::from_extension(ext) {
            r.set_format(f);
        }
    }
    r.no_limits();
    let name = r
        .format()
        .map(|f| format!("{f:?}").to_uppercase())
        .unwrap_or_else(|| ext.to_uppercase());
    let img = r.decode().map_err(es)?.into_rgba8();
    Ok((single(img), name))
}

const EXIF_TAGS: &[(&str, Tag)] = &[
    ("Make", Tag::Make),
    ("Camera Model", Tag::Model),
    ("Lens", Tag::LensModel),
    ("Software", Tag::Software),
    ("Artist", Tag::Artist),
    ("Copyright", Tag::Copyright),
    ("Shooting Date/Time", Tag::DateTimeOriginal),
    ("Flash", Tag::Flash),
    ("Focal Length", Tag::FocalLength),
    ("35mm Equivalent", Tag::FocalLengthIn35mmFilm),
    ("Exposure Time", Tag::ExposureTime),
    ("Exposure Bias", Tag::ExposureBiasValue),
    ("Aperture", Tag::FNumber),
    ("ISO Speed", Tag::PhotographicSensitivity),
    ("Metering Mode", Tag::MeteringMode),
    ("Exposure Program", Tag::ExposureProgram),
    ("Focus Distance", Tag::SubjectDistance),
    ("GPS Latitude", Tag::GPSLatitude),
    ("GPS Lat. Ref", Tag::GPSLatitudeRef),
    ("GPS Longitude", Tag::GPSLongitude),
    ("GPS Long. Ref", Tag::GPSLongitudeRef),
    ("Comment", Tag::UserComment),
];

fn read_exif(bytes: &[u8]) -> (Vec<(&'static str, String)>, u32) {
    let Ok(ex) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) else {
        return (Vec::new(), 1);
    };
    let orient = ex
        .get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(1);
    let mut out = Vec::new();
    for (label, tag) in EXIF_TAGS {
        if let Some(f) = ex.get_field(*tag, In::PRIMARY) {
            let v = f.display_value().with_unit(&ex).to_string();
            let v = v.trim_matches('"').trim().to_string();
            if !v.is_empty() {
                out.push((*label, v));
            }
        }
    }
    (out, orient)
}

pub fn orient(i: &RgbaImage, o: u32) -> RgbaImage {
    match o {
        2 => imageops::flip_horizontal(i),
        3 => imageops::rotate180(i),
        4 => imageops::flip_vertical(i),
        5 => imageops::flip_horizontal(&imageops::rotate90(i)),
        6 => imageops::rotate90(i),
        7 => imageops::flip_horizontal(&imageops::rotate270(i)),
        8 => imageops::rotate270(i),
        _ => i.clone(),
    }
}

/// `to_gpu`: upload each frame as a shared texture and keep only a thumbnail in RAM (when
/// the GPU path is on; the thumbnail workers pass false).
pub fn decode(e: &Entry, exif_rotate: bool, to_gpu: bool) -> Result<Decoded, String> {
    let start = heap_note(None);
    let r = decode_inner(e, exif_rotate, to_gpu);
    heap_note(Some((e, start)));
    r
}

/// Trace the heap's peak during this decode (mimalloc's committed memory; the counter is
/// reset per decode). Decodes on other threads can add to it.
#[cfg(target_os = "macos")]
fn heap_note(end: Option<(&Entry, usize)>) -> usize {
    extern "C" {
        fn mi_process_info(elapsed: *mut usize, user: *mut usize, system: *mut usize, rss: *mut usize, peak_rss: *mut usize, commit: *mut usize, peak_commit: *mut usize, faults: *mut usize);
        fn mi_stats_reset();
    }
    let mut v = [0usize; 8];
    unsafe {
        mi_process_info(&mut v[0], &mut v[1], &mut v[2], &mut v[3], &mut v[4], &mut v[5], &mut v[6], &mut v[7]);
    }
    match end {
        None => {
            // start: the peak counts from here
            unsafe { mi_stats_reset() };
            v[5]
        }
        Some((e, start)) => {
            crate::trace(&format!("heap during decode of {}: rose {} MB at most (peak {} MB, start {} MB)", e.name(), v[6].saturating_sub(start) >> 20, v[6] >> 20, start >> 20));
            0
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn heap_note(_end: Option<(&Entry, usize)>) -> usize {
    0
}

fn decode_inner(e: &Entry, exif_rotate: bool, to_gpu: bool) -> Result<Decoded, String> {
    crate::trace(&format!("decode start {}", e.name()));
    let bytes = e.read_bytes().map_err(es)?;
    crate::trace(&format!("read {} bytes", bytes.len()));
    let ext = e.ext();
    let modified = match e {
        Entry::File(p) => std::fs::metadata(p).ok().and_then(|m| m.modified().ok()),
        Entry::Zip { .. } => None,
    };
    // GPU path, row decoders: straight to the GPU in bands (not when EXIF orientation must
    // turn the image: that path still needs the whole image)
    if to_gpu {
        let (exif, o) = read_exif(&bytes);
        // an EXIF-turned photo goes up as stored; the GPU turns it (HEIF decoders apply
        // the container rotation themselves)
        let turn = if exif_rotate && o > 1 && o <= 8 && !crate::heic::is_heif(&bytes) { o as u8 } else { 1 };
        if let Some(r) = decode_bands(&bytes, &ext) {
            let (t, thumb, format) = r?;
            let swaps = turn >= 5;
            let (orig_w, orig_h) = if swaps { (t.h, t.w) } else { (t.w, t.h) };
            let thumb = if turn > 1 { orient(&thumb, turn as u32) } else { thumb };
            crate::trace(&format!("decoded {} to the GPU in bands (orientation {turn})", e.name()));
            release_freed_memory();
            return Ok(Decoded {
                frames: vec![Frame { img: Arc::new(thumb), delay_ms: 0, gpu: Some(t) }],
                format,
                file_size: bytes.len() as u64,
                modified,
                exif,
                orig_w,
                orig_h,
                orient: turn,
                lottie: None,
            });
        }
    }
    LOTTIE.with(|l| *l.borrow_mut() = None);
    let (mut frames, format) = decode_frames(&bytes, &ext)?;
    let lottie = LOTTIE.with(|l| l.borrow_mut().take());
    if frames.is_empty() {
        return Err("no frames".into());
    }
    crate::trace(&format!("decoded {}", e.name()));
    let (exif, o) = read_exif(&bytes);
    // HEIF decoders already apply the container rotation.
    if exif_rotate && o > 1 && o <= 8 && format != "HEIF" {
        for f in &mut frames {
            f.img = Arc::new(orient(&f.img, o));
        }
    }
    let (orig_w, orig_h) = (frames[0].img.width(), frames[0].img.height());
    // past the texture limit: the window draws it with the processor, so it stays in RAM
    let lim = crate::gpu::get().map(|g| g.max_tex()).unwrap_or(u32::MAX);
    if to_gpu && lottie.is_none() && orig_w <= lim && orig_h <= lim {
        for f in &mut frames {
            if let Some(t) = crate::gpu::upload_shared(&f.img) {
                f.img = Arc::new(thumb(&f.img));
                f.gpu = Some(t);
                crate::trace(&format!("decoded to the GPU; RAM keeps a {}x{} thumbnail", f.img.width(), f.img.height()));
            } else {
                crate::trace("decoded to RAM (GPU off or missing)");
            }
        }
    }
    if to_gpu {
        // the full-size buffers are freed by now, but the system allocator keeps their pages
        // (measured: 430 MB stayed after one 6016 px photo); hand them back
        release_freed_memory();
    }
    Ok(Decoded {
        frames,
        format,
        file_size: bytes.len() as u64,
        modified,
        exif,
        orig_w,
        orig_h,
        orient: 1,
        lottie,
    })
}

/// Rotation (quarter turns clockwise), flips and colour effects for display.
pub fn transform(
    src: &Arc<RgbaImage>,
    rot: u8,
    flip_h: bool,
    flip_v: bool,
    negative: bool,
    gamma: bool,
) -> Arc<RgbaImage> {
    if rot == 0 && !flip_h && !flip_v && !negative && !gamma {
        return src.clone();
    }
    let mut img = match rot % 4 {
        1 => imageops::rotate90(&**src),
        2 => imageops::rotate180(&**src),
        3 => imageops::rotate270(&**src),
        _ => (**src).clone(),
    };
    if flip_h {
        imageops::flip_horizontal_in_place(&mut img);
    }
    if flip_v {
        imageops::flip_vertical_in_place(&mut img);
    }
    if negative || gamma {
        let lut: [u8; 256] = core::array::from_fn(|v| {
            let mut x = v as f64 / 255.0;
            if gamma {
                x = x.powf(1.0 / 1.6);
            }
            if negative {
                x = 1.0 - x;
            }
            (x * 255.0).round() as u8
        });
        for p in img.chunks_exact_mut(4) {
            p[0] = lut[p[0] as usize];
            p[1] = lut[p[1] as usize];
            p[2] = lut[p[2] as usize];
        }
    }
    Arc::new(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The band path's thumbnail (running row, folded per thumbnail row) against a
    /// per-pixel sum of every pixel into its cell: real photos and 1 px stripes. Must be
    /// identical.
    #[test]
    fn thumb_running_row() {
        let compare = |name: &str, w: u32, h: u32, rows: &mut dyn FnMut(u32, &mut Vec<u8>)| {
            let mut t = Thumb::new(w, h);
            let (tw, th) = (t.tw as u64, t.th as u64);
            let (mut acc, mut cnt) = (vec![0u32; (tw * th * 4) as usize], vec![0u32; (tw * th) as usize]);
            let mut row = vec![0u8; w as usize * 4];
            let (mut tr, mut tt) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
            for y in 0..h {
                rows(y, &mut row);
                let t0 = std::time::Instant::now();
                let ty = y as u64 * th / h as u64;
                for x in 0..w as u64 {
                    let c = (ty * tw + x * tw / w as u64) as usize;
                    for k in 0..4 {
                        acc[c * 4 + k] += row[x as usize * 4 + k] as u32;
                    }
                    cnt[c] += 1;
                }
                let t1 = std::time::Instant::now();
                t.row(y, &row);
                tr += t1 - t0;
                tt += t1.elapsed();
            }
            let a = t.finish();
            let same = a.pixels().enumerate().all(|(i, p)| {
                let n = cnt[i].max(1);
                (0..4).all(|k| p.0[k] == ((acc[i * 4 + k] + n / 2) / n) as u8)
            });
            eprintln!("thumb {name} {w}x{h}: per-pixel {} ms, running row {} ms, identical {same}", tr.as_millis(), tt.as_millis());
            assert!(same, "{name}");
        };
        for f in ["tests/fixtures/perf/big.jpg", "tests/fixtures/set_a/02_big.jpg"] {
            let Ok(bytes) = std::fs::read(f) else { continue };
            let d = mozjpeg::Decompress::new_mem(&bytes).unwrap();
            let mut s = d.rgba().unwrap();
            let (w, h) = (s.width() as u32, s.height() as u32);
            compare(f, w, h, &mut |_, row| {
                s.read_scanlines_into(&mut row[..]).unwrap();
            });
        }
        compare("stripes", 6000, 4000, &mut |y, row| {
            let v = if y % 2 == 0 { 0 } else { 255 };
            for p in row.chunks_exact_mut(4) {
                p.copy_from_slice(&[v, v, v, 255]);
            }
        });
        compare("odd size", 1001, 333, &mut |y, row| {
            for (x, p) in row.chunks_exact_mut(4).enumerate() {
                p.copy_from_slice(&[(x * 7 + y as usize) as u8, (x * 3) as u8, y as u8, (x ^ y as usize) as u8]);
            }
        });
    }

    /// Needs `cargo run --example make_fixtures` first; skips otherwise.
    #[test]
    fn fixtures_decode() {
        let dir = Path::new("tests/fixtures/set_a");
        if !dir.exists() {
            eprintln!("no fixtures; skipped");
            return;
        }
        let pl = crate::source::open(dir, 0, false).unwrap();
        let names: Vec<String> = pl.entries.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["01_alpha.png", "02_big.jpg", "2_bmp.bmp", "03_lossless.webp", "04_exif_rot6.jpg", "05_broken.jpg", "06_anim.gif", "07_photo.heic", "08_lossy.webp", "10_tga.tga"]);
        for e in pl.entries.iter() {
            let r = decode(e, true, false);
            match e.name().as_str() {
                "05_broken.jpg" => assert!(r.is_err()),
                "04_exif_rot6.jpg" => {
                    let d = r.unwrap();
                    // Stored 300x200; orientation 6 rotates to 200x300, red bar moves to the top.
                    assert_eq!((d.frames[0].img.width(), d.frames[0].img.height()), (200, 300));
                    assert_eq!((d.orig_w, d.orig_h), (200, 300));
                    let p = d.frames[0].img.get_pixel(100, 5);
                    assert!(p[0] > 200 && p[2] < 80, "top should be red, got {p:?}");
                }
                "06_anim.gif" => {
                    let d = r.unwrap();
                    assert_eq!(d.frames.len(), 4);
                    assert_eq!(d.frames[0].delay_ms, 200);
                }
                "02_big.jpg" => assert_eq!(r.unwrap().orig_w, 6000),
                "08_lossy.webp" => {
                    let d = r.unwrap();
                    assert_eq!((d.orig_w, d.orig_h, d.format.as_str()), (320, 240, "WebP (lossy)"));
                    let top = d.frames[0].img.get_pixel(160, 10);
                    let low = d.frames[0].img.get_pixel(160, 200);
                    assert!(top[0] > 200 && top[2] < 60, "top should be red, got {top:?}");
                    assert!(low[2] > 160 && low[0] < 80, "lower should be blue, got {low:?}");
                }
                "03_lossless.webp" => assert_eq!(r.unwrap().format, "WEBP"),
                "07_photo.heic" => {
                    let d = r.unwrap();
                    assert_eq!((d.orig_w, d.orig_h, d.format.as_str()), (320, 240, "HEIF"));
                    let top = d.frames[0].img.get_pixel(160, 10);
                    let low = d.frames[0].img.get_pixel(160, 200);
                    assert!(top[0] > 200 && top[2] < 60, "top should be red, got {top:?}");
                    assert!(low[2] > 160 && low[0] < 80, "lower should be blue, got {low:?}");
                }
                n => assert!(r.is_ok(), "{n}: {:?}", r.err()),
            }
        }
        // archive of archives: pages of issue1, then issue2, each in page order
        let n = crate::source::open(Path::new("tests/fixtures/nested.zip"), 0, false).unwrap();
        let nn: Vec<String> = n.entries.iter().map(|e| e.name()).collect();
        assert_eq!(nn, ["p1.png", "p2.png", "p10.png", "p1.png", "p2.png", "p10.png"]);
        assert!(n.entries[0].fs_path().extension().unwrap() == "cbz" && n.entries[3].fs_path().extension().unwrap() == "cb7");
        assert_eq!(decode(&n.entries[5], true, false).unwrap().frames[0].img.get_pixel(5, 100).0, [200, 50, 200, 255]);
        let z = crate::source::open(Path::new("tests/fixtures/set_c.cbz"), 0, false).unwrap();
        let zn: Vec<String> = z.entries.iter().map(|e| e.name()).collect();
        assert_eq!(zn, ["p1.png", "p2.png", "p10.png"]);
        assert_eq!(decode(&z.entries[2], true, false).unwrap().orig_h, 450);
        // 7z and (solid) RAR copies of the same pages: same order, same pixels.
        for arc in ["tests/fixtures/set_c.cb7", "tests/fixtures/set_c.cbr", "tests/fixtures/set_c_nonsolid.cbr"] {
            let p = Path::new(arc);
            if !p.exists() {
                eprintln!("{arc} missing; skipped");
                continue;
            }
            let a = crate::source::open(p, 0, false).unwrap();
            let names: Vec<String> = a.entries.iter().map(|e| e.name()).collect();
            assert_eq!(names, ["p1.png", "p2.png", "p10.png"], "{arc}");
            let d = decode(&a.entries[2], true, false).unwrap();
            assert_eq!((d.orig_w, d.orig_h), (300, 450), "{arc}");
            assert_eq!(d.frames[0].img.get_pixel(5, 100).0, [200, 50, 200, 255], "{arc}");
        }
        let sib = crate::source::sibling(Path::new("tests/fixtures/set_a").canonicalize().as_deref().unwrap_or(dir), true, 0, false, true);
        assert!(sib.is_some());
    }
}

/// Return freed heap pages to the system (macOS keeps large freed blocks cached).
#[cfg(target_os = "macos")]
pub fn release_freed_memory() {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        // mimalloc (the app's heap on the Mac, see main.rs): free this thread's empty pages
        fn mi_collect(force: bool);
    }
    unsafe {
        mi_collect(true);
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn release_freed_memory() {}

// ---- band decoding (GPU path): the image goes to the GPU in bands of rows, so RAM holds
// one band, never the whole image ----

/// Rows from a decoder, sent to a new texture BAND rows at a time through one reused
/// buffer; a thumbnail is averaged on the way.
/// The 512 px thumbnail of an image that arrives row by row: each cell is the average of
/// every pixel it covers. Rows are added into one running row (a plain vector add), which
/// is folded into the thumbnail once per thumbnail row. (A per-pixel sum took 80 ms at
/// 6000x4000, more than the decode; summing 4 rows per thumbnail row instead differed by
/// up to 52 levels on 1 px stripes.)
struct Thumb {
    w: u32,
    h: u32,
    tw: u32,
    th: u32,
    acc: Vec<u32>,
    cnt: Vec<u32>,
    /// the thumbnail column each image column falls in
    cols: Vec<u32>,
    /// the rows of the current thumbnail row, summed per image column and channel
    run: Vec<u32>,
    run_rows: u32,
    run_ty: usize,
}

impl Thumb {
    fn new(w: u32, h: u32) -> Thumb {
        let k = (THUMB as f64 / w.max(h) as f64).min(1.0);
        let (tw, th) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
        let n = (tw * th) as usize;
        let cols = (0..w).map(|x| (x as u64 * tw as u64 / w as u64) as u32).collect();
        Thumb { w, h, tw, th, acc: vec![0; n * 4], cnt: vec![0; n], cols, run: vec![0; w as usize * 4], run_rows: 0, run_ty: 0 }
    }

    /// Row `y`, RGBA8 (rows arrive in order).
    fn row(&mut self, y: u32, rgba: &[u8]) {
        let ty = (y as u64 * self.th as u64 / self.h as u64) as usize;
        if ty != self.run_ty {
            self.fold();
            self.run_ty = ty;
        }
        for (a, &b) in self.run.iter_mut().zip(&rgba[..self.w as usize * 4]) {
            *a += b as u32;
        }
        self.run_rows += 1;
    }

    /// The running row into its thumbnail row.
    fn fold(&mut self) {
        if self.run_rows == 0 {
            return;
        }
        let row0 = self.run_ty * self.tw as usize;
        for (x, &c) in self.cols.iter().enumerate() {
            let at = (row0 + c as usize) * 4;
            for k in 0..4 {
                self.acc[at + k] += self.run[x * 4 + k];
            }
            self.cnt[row0 + c as usize] += self.run_rows;
        }
        self.run.fill(0);
        self.run_rows = 0;
    }

    fn finish(&mut self) -> RgbaImage {
        self.fold();
        let mut thumb = RgbaImage::new(self.tw, self.th);
        for (i, p) in thumb.pixels_mut().enumerate() {
            let n = self.cnt[i].max(1);
            p.0 = core::array::from_fn(|k| ((self.acc[i * 4 + k] + n / 2) / n) as u8);
        }
        thumb
    }
}

struct BandSink {
    g: Arc<crate::gpu::Gpu>,
    tex: wgpu::Texture,
    w: u32,
    h: u32,
    band: Vec<u8>,
    rows: u32,
    y: u32,
    thumb: Thumb,
    /// the last band's upload, waited for when the next one goes up
    prev: Option<wgpu::SubmissionIndex>,
}

impl BandSink {
    /// None when the GPU is off or missing, or a side passes its texture limit (then the
    /// whole-image path shrinks it first).
    fn new(w: u32, h: u32) -> Option<BandSink> {
        if !crate::gpu::DECODE_TO_GPU.load(std::sync::atomic::Ordering::Relaxed) || w == 0 || h == 0 {
            return None;
        }
        let g = crate::gpu::get()?;
        if w > g.max_tex() || h > g.max_tex() {
            return None;
        }
        let tex = g.new_tex(w, h);
        let band = vec![0u8; (crate::gpu::BAND.min(h) * w * 4) as usize];
        Some(BandSink { g, tex, w, h, band, rows: 0, y: 0, thumb: Thumb::new(w, h), prev: None })
    }

    /// One row, RGBA8.
    fn push_row(&mut self, rgba: &[u8]) {
        let w = self.w as usize;
        let at = self.rows as usize * w * 4;
        self.band[at..at + w * 4].copy_from_slice(&rgba[..w * 4]);
        self.thumb.row(self.y + self.rows, rgba);
        self.rows += 1;
        if self.rows as usize * w * 4 == self.band.len() || self.y + self.rows == self.h {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.rows == 0 {
            return;
        }
        let n = (self.rows * self.w * 4) as usize;
        self.prev = Some(self.g.write_band(&self.tex, &self.band[..n], self.w, self.y, self.rows, self.prev.take()));
        self.y += self.rows;
        self.rows = 0;
    }

    fn finish(mut self) -> Result<(Arc<crate::gpu::Tex>, RgbaImage), String> {
        self.flush();
        if self.y != self.h {
            return Err(format!("image ended at row {} of {}", self.y, self.h));
        }
        let thumb = self.thumb.finish();
        let t = self.g.finish_tex(self.tex, self.w, self.h, 1.0);
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        Ok((Arc::new(t), thumb))
    }
}

/// PNG through the band path (not interlaced, not animated); None to use the whole-image
/// path.
fn png_bands(bytes: &[u8]) -> Option<Result<(Arc<crate::gpu::Tex>, RgbaImage), String>> {
    let mut d = png::Decoder::new(Cursor::new(bytes));
    d.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = d.read_info().ok()?;
    if r.info().interlaced || r.info().animation_control.is_some() {
        return None;
    }
    let (w, h) = (r.info().width, r.info().height);
    let (ct, _) = r.output_color_type();
    let mut sink = BandSink::new(w, h)?;
    let mut rgba = vec![0u8; w as usize * 4];
    loop {
        let row = match r.next_row() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(e) => return Some(Err(e.to_string())),
        };
        let d = row.data();
        for x in 0..w as usize {
            let p = match ct {
                png::ColorType::Rgba => [d[x * 4], d[x * 4 + 1], d[x * 4 + 2], d[x * 4 + 3]],
                png::ColorType::Rgb => [d[x * 3], d[x * 3 + 1], d[x * 3 + 2], 255],
                png::ColorType::GrayscaleAlpha => [d[x * 2], d[x * 2], d[x * 2], d[x * 2 + 1]],
                _ => [d[x], d[x], d[x], 255],
            };
            rgba[x * 4..x * 4 + 4].copy_from_slice(&p);
        }
        sink.push_row(&rgba);
    }
    Some(sink.finish())
}

/// JPEG through the band path: libjpeg-turbo's scanline reader. None (whole-image path)
/// when it cannot give RGBA (CMYK, say) or libjpeg fails; the crate reports libjpeg errors
/// as panics, so they are caught here.
fn jpeg_bands(bytes: &[u8]) -> Option<Result<(Arc<crate::gpu::Tex>, RgbaImage), String>> {
    let run = || -> Option<Result<(Arc<crate::gpu::Tex>, RgbaImage), String>> {
        let d = mozjpeg::Decompress::new_mem(bytes).ok()?;
        let mut s = d.rgba().ok()?;
        let (w, h) = (s.width() as u32, s.height() as u32);
        let mut sink = BandSink::new(w, h)?;
        let mut row = vec![0u8; w as usize * 4];
        for _ in 0..h {
            if let Err(e) = s.read_scanlines_into(&mut row[..]) {
                return Some(Err(e.to_string()));
            }
            sink.push_row(&row);
        }
        let _ = s.finish();
        Some(sink.finish())
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).ok().flatten()
}

/// WebP (still, lossy or lossless) for the GPU path. libwebp has no row-by-row output,
/// so it decodes straight into one buffer of the image's size, which goes up in bands and
/// is freed: the one temporary copy of a whole-image decoder. (Before: ImageIO for lossy
/// and the image crate for lossless, with extra copies; +292 to +314 MB at 6000x4000.)
/// None for animated WebP and anything libwebp refuses.
fn webp_bands(bytes: &[u8]) -> Option<Result<(Arc<crate::gpu::Tex>, RgbaImage), String>> {
    use libwebp_sys as w;
    let mut f: w::WebPBitstreamFeatures = unsafe { std::mem::zeroed() };
    if unsafe { w::WebPGetFeatures(bytes.as_ptr(), bytes.len(), &mut f) } != w::VP8StatusCode::VP8_STATUS_OK || f.has_animation != 0 {
        return None;
    }
    let (wd, ht) = (f.width as u32, f.height as u32);
    let mut sink = BandSink::new(wd, ht)?;
    let stride = wd as usize * 4;
    let mut buf = vec![0u8; stride * ht as usize];
    let ok = unsafe { w::WebPDecodeRGBAInto(bytes.as_ptr(), bytes.len(), buf.as_mut_ptr(), buf.len(), stride as i32) };
    if ok.is_null() {
        return Some(Err("libwebp could not decode the file".into()));
    }
    for row in buf.chunks_exact(stride) {
        sink.push_row(row);
    }
    drop(buf);
    Some(sink.finish())
}

/// HEIF through the band path (Windows: libheif): its decoded plane goes up row by row,
/// so RAM holds libheif's one copy, not a second one of the image's size (145 MB each at
/// 6016x6016).
#[cfg(not(target_os = "macos"))]
fn heif_bands(bytes: &[u8]) -> Option<Result<(Arc<crate::gpu::Tex>, RgbaImage), String>> {
    let r = crate::heic::with_plane(bytes, |w, h, stride, data| {
        let mut sink = BandSink::new(w as u32, h as u32)?;
        for y in 0..h {
            sink.push_row(&data[y * stride..y * stride + w * 4]);
        }
        Some(sink.finish())
    });
    match r {
        Ok(Some(r)) => Some(r),
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    }
}

/// The band path for formats that decode row by row; None when it does not apply.
fn decode_bands(bytes: &[u8], ext: &str) -> Option<Result<(Arc<crate::gpu::Tex>, RgbaImage, String), String>> {
    #[cfg(not(target_os = "macos"))]
    if crate::heic::is_heif(bytes) {
        return heif_bands(bytes).map(|r| r.map(|(t, th)| (t, th, "HEIF".to_string())));
    }
    match image::guess_format(bytes).ok() {
        Some(ImageFormat::Png) => png_bands(bytes).map(|r| r.map(|(t, th)| (t, th, "PNG".to_string()))),
        Some(ImageFormat::Jpeg) => jpeg_bands(bytes).map(|r| r.map(|(t, th)| (t, th, "JPEG".to_string()))),
        Some(ImageFormat::WebP) => {
            let name = if crate::webp_fast::is_lossy_still(bytes) { "WebP (lossy)" } else { "WebP" };
            webp_bands(bytes).map(|r| r.map(|(t, th)| (t, th, name.to_string())))
        }
        _ => {
            let _ = ext;
            None
        }
    }
}
