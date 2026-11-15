//! Decoding to RGBA frames, EXIF reading, and view transforms.

use std::{io::Cursor, sync::Arc, time::SystemTime};

use exif::{In, Tag};
use image::{
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
    imageops, AnimationDecoder, ImageFormat, ImageReader, RgbaImage,
};

use crate::source::Entry;

pub struct Frame {
    pub img: Arc<RgbaImage>,
    pub delay_ms: u32,
}

pub struct Decoded {
    pub frames: Vec<Frame>,
    pub format: String,
    pub file_size: u64,
    pub modified: Option<SystemTime>,
    pub exif: Vec<(&'static str, String)>,
    pub orig_w: u32,
    pub orig_h: u32,
}

impl Decoded {
    pub fn bytes(&self) -> u64 {
        self.frames.iter().map(|f| f.img.as_raw().len() as u64).sum()
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
            }
        })
        .collect())
}

fn single(img: RgbaImage) -> Vec<Frame> {
    vec![Frame { img: Arc::new(img), delay_ms: 0 }]
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

fn decode_frames(bytes: &[u8], ext: &str) -> Result<(Vec<Frame>, String), String> {
    if matches!(ext, "psd" | "psb") || bytes.starts_with(b"8BPS") {
        if let Some(img) = crate::psd_fast::decode(bytes) {
            return Ok((single(img), if bytes.get(4..6) == Some(&[0, 2]) { "PSB" } else { "PSD" }.into()));
        }
        let p = psd::Psd::from_bytes(bytes).map_err(es)?;
        let img = RgbaImage::from_raw(p.width(), p.height(), p.rgba()).ok_or("bad PSD data")?;
        return Ok((single(img), "PSD".into()));
    }
    #[cfg(windows)]
    if ext == "lottie" || (ext == "json" && crate::lottie::is_lottie_json(bytes)) {
        let (imgs, delay) = crate::lottie::decode(bytes, ext)?;
        let name = if imgs.len() > 1 { "Lottie (animated)" } else { "Lottie" };
        return Ok((imgs.into_iter().map(|i| Frame { img: Arc::new(i), delay_ms: delay }).collect(), name.into()));
    }
    if matches!(ext, "svg" | "svgz") {
        return Ok((single(svg(bytes)?), "SVG".into()));
    }
    if crate::heic::is_heif(bytes) || matches!(ext, "heic" | "heif" | "hif") {
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

pub fn decode(e: &Entry, exif_rotate: bool) -> Result<Decoded, String> {
    crate::trace(&format!("decode start {}", e.name()));
    let bytes = e.read_bytes().map_err(es)?;
    crate::trace(&format!("read {} bytes", bytes.len()));
    let ext = e.ext();
    let modified = match e {
        Entry::File(p) => std::fs::metadata(p).ok().and_then(|m| m.modified().ok()),
        Entry::Zip { .. } => None,
    };
    let (mut frames, format) = decode_frames(&bytes, &ext)?;
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
    Ok(Decoded {
        frames,
        format,
        file_size: bytes.len() as u64,
        modified,
        exif,
        orig_w,
        orig_h,
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
        for e in &pl.entries {
            let r = decode(e, true);
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
        assert_eq!(decode(&n.entries[5], true).unwrap().frames[0].img.get_pixel(5, 100).0, [200, 50, 200, 255]);
        let z = crate::source::open(Path::new("tests/fixtures/set_c.cbz"), 0, false).unwrap();
        let zn: Vec<String> = z.entries.iter().map(|e| e.name()).collect();
        assert_eq!(zn, ["p1.png", "p2.png", "p10.png"]);
        assert_eq!(decode(&z.entries[2], true).unwrap().orig_h, 450);
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
            let d = decode(&a.entries[2], true).unwrap();
            assert_eq!((d.orig_w, d.orig_h), (300, 450), "{arc}");
            assert_eq!(d.frames[0].img.get_pixel(5, 100).0, [200, 50, 200, 255], "{arc}");
        }
        let sib = crate::source::sibling(Path::new("tests/fixtures/set_a").canonicalize().as_deref().unwrap_or(dir), true, 0, false);
        assert!(sib.is_some());
    }
}

/// Two pages side by side, scaled to the same height.
pub fn compose(left: &RgbaImage, right: &RgbaImage) -> RgbaImage {
    let h = left.height().max(right.height());
    let fit = |i: &RgbaImage| -> RgbaImage {
        if i.height() == h {
            i.clone()
        } else {
            let w = ((i.width() as f64) * h as f64 / i.height() as f64).round().max(1.0) as u32;
            crate::render::resize(i, w, h, 2)
        }
    };
    let l = fit(left);
    let r = fit(right);
    let mut out = RgbaImage::new(l.width() + r.width(), h);
    imageops::replace(&mut out, &l, 0, 0);
    imageops::replace(&mut out, &r, l.width() as i64, 0);
    out
}
