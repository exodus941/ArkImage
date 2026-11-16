//! Fast path for lossy, still WebP files.
//! Windows/Linux: libwebp. On a 6000x4000 lossy file it decodes in 220-290 ms; the
//! pure-Rust decoder in the image crate takes 540-820 ms. Tried without gain: libwebp
//! threads, skipping the deblocking filter (-12%), decoding straight to a smaller size.
//! macOS: the system ImageIO decoder (WebP since macOS 11), so no C build there.
//! Lossless and animated files return None and use the image crate, which is faster
//! for lossless (214 ms vs libwebp 262 ms).

use image::RgbaImage;

/// True for a RIFF/WEBP file whose image data is a lossy VP8 chunk and has no animation.
pub fn is_lossy_still(b: &[u8]) -> bool {
    if b.len() < 30 || &b[0..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return false;
    }
    match &b[12..16] {
        b"VP8 " => true,
        // Extended header: animation flag is bit 1 of the first flags byte.
        b"VP8X" => b[20] & 0x02 == 0 && b.windows(4).take(4096).any(|w| w == b"VP8 "),
        _ => false,
    }
}

#[cfg(target_os = "macos")]
pub fn decode_lossy_still(bytes: &[u8]) -> Option<RgbaImage> {
    if !is_lossy_still(bytes) {
        return None;
    }
    crate::heic::decode(bytes).ok()
}

#[cfg(not(target_os = "macos"))]
pub fn decode_lossy_still(bytes: &[u8]) -> Option<RgbaImage> {
    use libwebp_sys::*;
    if !is_lossy_still(bytes) {
        return None;
    }
    unsafe {
        let mut cfg: WebPDecoderConfig = std::mem::zeroed();
        if !WebPInitDecoderConfig(&mut cfg) {
            return None;
        }
        if WebPGetFeatures(bytes.as_ptr(), bytes.len(), &mut cfg.input) != VP8StatusCode::VP8_STATUS_OK {
            return None;
        }
        // format: 0 mixed/undefined, 1 lossy, 2 lossless
        if cfg.input.has_animation != 0 || cfg.input.format != 1 {
            return None;
        }
        cfg.output.colorspace = WEBP_CSP_MODE::MODE_RGBA;
        if WebPDecode(bytes.as_ptr(), bytes.len(), &mut cfg) != VP8StatusCode::VP8_STATUS_OK {
            WebPFreeDecBuffer(&mut cfg.output);
            return None;
        }
        let (w, h) = (cfg.output.width as usize, cfg.output.height as usize);
        let rgba = cfg.output.u.RGBA;
        let stride = rgba.stride as usize;
        let src = std::slice::from_raw_parts(rgba.rgba, stride * h);
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            out.extend_from_slice(&src[y * stride..y * stride + w * 4]);
        }
        WebPFreeDecBuffer(&mut cfg.output);
        RgbaImage::from_raw(w as u32, h as u32, out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn header_check() {
        let mut lossy = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        lossy.resize(40, 0);
        assert!(super::is_lossy_still(&lossy));
        let mut lossless = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
        lossless.resize(40, 0);
        assert!(!super::is_lossy_still(&lossless));
        let mut anim = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        anim.resize(40, 0);
        anim[20] = 0x02;
        anim.extend_from_slice(b"ANIMVP8 ");
        assert!(!super::is_lossy_still(&anim));
    }
}
