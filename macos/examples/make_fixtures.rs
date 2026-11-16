//! Writes test images to `tests/fixtures` (git-ignored). Run: cargo run --example make_fixtures

use std::{fs, io::Write, path::Path};

use image::{codecs::gif::GifEncoder, Delay, Frame, Rgba, RgbaImage};

fn gradient(w: u32, h: u32, alpha: bool) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let a = if alpha { ((x * 255) / w.max(1)) as u8 } else { 255 };
        Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 128, a])
    })
}

/// A JPEG whose pixels are "upright when rotated 90 CW" plus an EXIF Orientation=6 tag.
fn jpeg_with_orientation(path: &Path) {
    // Stored image is landscape 300x200 with a red bar on the left edge.
    let img = image::RgbImage::from_fn(300, 200, |x, _| if x < 30 { image::Rgb([255, 0, 0]) } else { image::Rgb([40, 40, 200]) });
    let mut jpg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 90).encode_image(&img).unwrap();
    // APP1 Exif: little-endian TIFF, one IFD entry: Orientation (0x0112) SHORT 1 = 6.
    let mut tiff = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0];
    tiff.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let len = (payload.len() + 2) as u16;
    let mut out = jpg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1, (len >> 8) as u8, len as u8]);
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpg[2..]);
    fs::write(path, out).unwrap();
}

#[cfg(not(target_os = "macos"))]
fn lossy_webp(img: &RgbaImage) -> Vec<u8> {
    unsafe {
        let mut out: *mut u8 = std::ptr::null_mut();
        let n = libwebp_sys::WebPEncodeRGBA(img.as_raw().as_ptr(), img.width() as i32, img.height() as i32, img.width() as i32 * 4, 85.0, &mut out);
        let v = std::slice::from_raw_parts(out, n).to_vec();
        libwebp_sys::WebPFree(out as *mut _);
        v
    }
}

fn main() {
    let dir = Path::new("tests/fixtures");
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir.join("set_a")).unwrap();
    fs::create_dir_all(dir.join("set_b")).unwrap();
    let a = dir.join("set_a");

    gradient(640, 480, true).save(a.join("01_alpha.png")).unwrap();
    image::DynamicImage::ImageRgba8(gradient(6000, 4000, false)).to_rgb8().save(a.join("02_big.jpg")).unwrap();
    gradient(320, 240, false).save(a.join("10_tga.tga")).unwrap();
    image::DynamicImage::ImageRgba8(gradient(320, 240, false)).to_rgb8().save(a.join("2_bmp.bmp")).unwrap();
    gradient(400, 300, true).save(a.join("03_lossless.webp")).unwrap();
    jpeg_with_orientation(&a.join("04_exif_rot6.jpg"));
    fs::write(a.join("05_broken.jpg"), b"not an image").unwrap();

    // Animated GIF: 4 frames, 200 ms each, a moving white square.
    let f = fs::File::create(a.join("06_anim.gif")).unwrap();
    let mut enc = GifEncoder::new(f);
    enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
    for i in 0..4u32 {
        let img = RgbaImage::from_fn(160, 120, |x, y| {
            if x / 40 == i && y < 40 { Rgba([255, 255, 255, 255]) } else { Rgba([20, 120, 60, 255]) }
        });
        enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(200, 1))).unwrap();
    }
    drop(enc);

    // Portrait pages for double-page view, in set_b, plus a CBZ.
    let b = dir.join("set_b");
    for i in 1..=4u32 {
        let img = RgbaImage::from_fn(600, 900, |x, _| Rgba([(i * 50) as u8, (x / 3) as u8, 90, 255]));
        img.save(b.join(format!("page{i}.png"))).unwrap();
    }
    let zf = fs::File::create(dir.join("set_c.cbz")).unwrap();
    let mut z = zip::ZipWriter::new(zf);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for i in [1u32, 2, 10] {
        let mut png = Vec::new();
        RgbaImage::from_fn(300, 450, |_, y| Rgba([200, (y / 2) as u8, (i * 20) as u8, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        z.start_file(format!("chapter/p{i}.png"), opts).unwrap();
        z.write_all(&png).unwrap();
    }
    z.finish().unwrap();

    // Same three pages as a 7z (set_c.cb7), via a staging folder.
    let stage = dir.join("stage_c");
    fs::create_dir_all(stage.join("chapter")).unwrap();
    for i in [1u32, 2, 10] {
        RgbaImage::from_fn(300, 450, |_, y| Rgba([200, (y / 2) as u8, (i * 20) as u8, 255])).save(stage.join(format!("chapter/p{i}.png"))).unwrap();
    }
    sevenz_rust2::compress_to_path(&stage, dir.join("set_c.cb7")).unwrap();
    // Solid RAR when WinRAR is installed (RAR cannot be written from Rust).
    let rar = std::path::Path::new(r"C:\Program Files\WinRAR\Rar.exe");
    if rar.exists() {
        let out = std::path::absolute(dir.join("set_c.cbr")).unwrap();
        let _ = std::process::Command::new(rar).current_dir(&stage).args(["a", "-s", "-r", "-idq"]).arg(&out).arg("chapter").status();
        let ns = std::path::absolute(dir.join("set_c_nonsolid.cbr")).unwrap();
        let _ = std::process::Command::new(rar).current_dir(&stage).args(["a", "-s-", "-r", "-idq"]).arg(&ns).arg("chapter").status();
    }
    let _ = fs::remove_dir_all(&stage);

    // An archive of archives: issue1.cbz and issue2.cb7 inside nested.zip.
    let nf = fs::File::create(dir.join("nested.zip")).unwrap();
    let mut nz = zip::ZipWriter::new(nf);
    for (inner, src) in [("issue2.cb7", "set_c.cb7"), ("issue1.cbz", "set_c.cbz")] {
        nz.start_file(inner, opts).unwrap();
        nz.write_all(&fs::read(dir.join(src)).unwrap()).unwrap();
    }
    nz.finish().unwrap();
    #[cfg(not(target_os = "macos"))]
    heic(&a.join("07_photo.heic"));

    // Large photo-like files for speed tests (noise defeats trivial compression).
    let perf = dir.join("perf");
    fs::create_dir_all(&perf).unwrap();
    let mut seed = 12345u32;
    let noisy = RgbaImage::from_fn(6000, 4000, |x, y| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let n = (seed & 31) as u32;
        Rgba([((x * 255 / 6000) + n).min(255) as u8, ((y * 255 / 4000) + n).min(255) as u8, (100 + n) as u8, 255])
    });
    image::DynamicImage::ImageRgba8(noisy.clone()).to_rgb8().save(perf.join("big.png")).unwrap();
    image::DynamicImage::ImageRgba8(noisy.clone()).to_rgb8().save(perf.join("big.jpg")).unwrap();
    noisy.save(perf.join("big.webp")).unwrap();
    #[cfg(not(target_os = "macos"))]
    {
    let photo = image::open(perf.join("big.jpg")).unwrap().into_rgba8();
    fs::write(perf.join("big_lossy.webp"), lossy_webp(&photo)).unwrap();
    // small lossy WebP: red bar on top, blue below
    let small = RgbaImage::from_fn(320, 240, |_, y| if y < 40 { Rgba([255, 0, 0, 255]) } else { Rgba([40, 40, 200, 255]) });
    fs::write(a.join("08_lossy.webp"), lossy_webp(&small)).unwrap();
    }
    println!("fixtures written to {}", dir.display());
}

/// 320x240 HEIC: red bar across the top 40 rows, blue below.
#[cfg(not(target_os = "macos"))]
fn heic(path: &Path) {
    use libheif_rs::{Channel, ColorSpace, CompressionFormat, EncoderQuality, HeifContext, Image, LibHeif, RgbChroma};
    let (w, h) = (320u32, 240u32);
    let mut img = Image::new(w, h, ColorSpace::Rgb(RgbChroma::Rgb)).unwrap();
    img.create_plane(Channel::Interleaved, w, h, 8).unwrap();
    {
        let planes = img.planes_mut();
        let p = planes.interleaved.unwrap();
        let stride = p.stride;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let px = if y < 40 { [255u8, 0, 0] } else { [40, 40, 200] };
                p.data[y * stride + x * 3..y * stride + x * 3 + 3].copy_from_slice(&px);
            }
        }
    }
    let lib = LibHeif::new();
    let mut ctx = HeifContext::new().unwrap();
    let mut enc = lib.encoder_for_format(CompressionFormat::Hevc).unwrap();
    enc.set_quality(EncoderQuality::Lossy(90)).unwrap();
    ctx.encode_image(&img, &mut enc, None).unwrap();
    ctx.write_to_file(path.to_str().unwrap()).unwrap();
}
