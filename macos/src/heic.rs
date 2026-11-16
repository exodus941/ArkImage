//! HEIC/HEIF decoding. Windows/Linux: libheif (+libde265). macOS: system ImageIO.
//! Both apply the container's rotation/mirror, so callers must not apply EXIF orientation again.

use image::RgbaImage;

/// True for an ISO-BMFF file with a HEIF brand.
pub fn is_heif(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = size.clamp(12, bytes.len().min(256));
    const BRANDS: [&[u8; 4]; 8] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1"];
    // major brand at 8..12, compatible brands from 16 on
    std::iter::once(8)
        .chain((16..end).step_by(4))
        .filter(|&i| i + 4 <= end)
        .any(|i| BRANDS.iter().any(|b| &bytes[i..i + 4] == *b))
}

#[cfg(not(target_os = "macos"))]
pub fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_bytes(bytes).map_err(|e| e.to_string())?;
    let handle = ctx.primary_image_handle().map_err(|e| e.to_string())?;
    let img = lib
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)
        .map_err(|e| e.to_string())?;
    let planes = img.planes();
    let p = planes.interleaved.ok_or("no interleaved plane")?;
    let (w, h, stride) = (p.width as usize, p.height as usize, p.stride);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        out.extend_from_slice(&p.data[y * stride..y * stride + w * 4]);
    }
    RgbaImage::from_raw(w as u32, h as u32, out).ok_or_else(|| "bad HEIF buffer".to_string())
}

#[cfg(target_os = "macos")]
pub fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    mac::decode(bytes)
}

#[cfg(target_os = "macos")]
mod mac {
    use image::RgbaImage;
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;

    #[repr(C)]
    struct CGRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataCreate(alloc: CFTypeRef, bytes: *const u8, len: isize) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
    }
    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageSourceCreateWithData(data: CFTypeRef, opts: CFTypeRef) -> CFTypeRef;
        fn CGImageSourceCreateImageAtIndex(src: CFTypeRef, i: usize, opts: CFTypeRef) -> CFTypeRef;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGImageGetWidth(img: CFTypeRef) -> usize;
        fn CGImageGetHeight(img: CFTypeRef) -> usize;
        fn CGColorSpaceCreateDeviceRGB() -> CFTypeRef;
        fn CGBitmapContextCreate(
            data: *mut c_void,
            w: usize,
            h: usize,
            bpc: usize,
            bpr: usize,
            cs: CFTypeRef,
            info: u32,
        ) -> CFTypeRef;
        fn CGContextDrawImage(ctx: CFTypeRef, rect: CGRect, img: CFTypeRef);
    }

    pub fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
        unsafe {
            let data = CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize);
            if data.is_null() {
                return Err("CFDataCreate failed".into());
            }
            let src = CGImageSourceCreateWithData(data, std::ptr::null());
            CFRelease(data);
            if src.is_null() {
                return Err("ImageIO can't read this file".into());
            }
            let img = CGImageSourceCreateImageAtIndex(src, 0, std::ptr::null());
            CFRelease(src);
            if img.is_null() {
                return Err("ImageIO can't decode this file".into());
            }
            let (w, h) = (CGImageGetWidth(img), CGImageGetHeight(img));
            let mut buf = vec![0u8; w * h * 4];
            let cs = CGColorSpaceCreateDeviceRGB();
            // kCGImageAlphaPremultipliedLast = 1, default (big-endian RGBA) byte order.
            let ctx = CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, cs, 1);
            CFRelease(cs);
            if ctx.is_null() {
                CFRelease(img);
                return Err("CGBitmapContextCreate failed".into());
            }
            CGContextDrawImage(ctx, CGRect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 }, img);
            CFRelease(ctx);
            CFRelease(img);
            for p in buf.chunks_exact_mut(4) {
                let a = p[3] as u32;
                if a != 0 && a != 255 {
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
            RgbaImage::from_raw(w as u32, h as u32, buf).ok_or_else(|| "bad buffer".into())
        }
    }
}
