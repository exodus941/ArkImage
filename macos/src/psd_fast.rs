//! Reads only the flattened composite that Photoshop stores at the end of a PSD/PSB,
//! skipping the layers. The `psd` crate parses every layer first: 1.0-1.15 s for a
//! 130 MB 6000x4000 file. Channels decode in parallel. Unsupported variants return
//! None and fall back to the `psd` crate.
//! Supported: PSD and PSB, 8 or 16 bits, Grayscale / RGB / CMYK, raw or RLE (PackBits).

use image::RgbaImage;

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|s| u64::from_be_bytes(s.try_into().unwrap()))
    }
    fn skip_section(&mut self, big: bool) -> Option<()> {
        let n = if big { self.u64()? as usize } else { self.u32()? as usize };
        self.take(n).map(|_| ())
    }
}

/// PackBits: decode `src` into exactly `out.len()` bytes.
fn unpack(src: &[u8], out: &mut [u8]) -> Option<()> {
    let (mut i, mut o) = (0, 0);
    while o < out.len() {
        let n = *src.get(i)? as i8;
        i += 1;
        if n >= 0 {
            let len = n as usize + 1;
            out.get_mut(o..o + len)?.copy_from_slice(src.get(i..i + len)?);
            i += len;
            o += len;
        } else if n != -128 {
            let len = (-(n as isize)) as usize + 1;
            let v = *src.get(i)?;
            i += 1;
            out.get_mut(o..o + len)?.fill(v);
            o += len;
        }
    }
    Some(())
}

pub fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let mut r = R { b: bytes, p: 0 };
    if r.take(4)? != b"8BPS" {
        return None;
    }
    let big = match r.u16()? {
        1 => false,
        2 => true,
        _ => return None,
    };
    r.take(6)?;
    let channels = r.u16()? as usize;
    let h = r.u32()? as usize;
    let w = r.u32()? as usize;
    let depth = r.u16()?;
    let mode = r.u16()?;
    let need = match mode {
        1 => 1, // grayscale
        3 => 3, // RGB
        4 => 4, // CMYK
        _ => return None,
    };
    if !(depth == 8 || depth == 16) || channels < need || w == 0 || h == 0 {
        return None;
    }
    let n = r.u32()? as usize;
    r.take(n)?; // colour mode data
    let n = r.u32()? as usize;
    r.take(n)?; // image resources
    r.skip_section(big)?; // layer and mask info
    let comp = r.u16()?;
    let bpc = (depth / 8) as usize;
    let row_bytes = w * bpc;
    // Channels we use: colour (+ alpha for RGB/gray when present).
    let use_n = (need + if mode != 4 && channels > need { 1 } else { 0 }).min(channels);

    // Locate each channel's data, then decode the channels on parallel threads.
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(use_n);
    match comp {
        0 => {
            for _ in 0..use_n {
                planes.push(r.take(row_bytes * h)?.to_vec());
            }
        }
        1 => {
            let mut counts = Vec::with_capacity(channels * h);
            for _ in 0..channels * h {
                counts.push(if big { r.u32()? as usize } else { r.u16()? as usize });
            }
            let mut starts = Vec::with_capacity(use_n);
            let mut pos = r.p;
            for c in 0..use_n {
                starts.push(pos);
                pos += counts[c * h..(c + 1) * h].iter().sum::<usize>();
            }
            if pos > bytes.len() {
                return None;
            }
            let ok = std::thread::scope(|s| {
                let handles: Vec<_> = (0..use_n)
                    .map(|c| {
                        let counts = &counts[c * h..(c + 1) * h];
                        let start = starts[c];
                        s.spawn(move || {
                            let mut plane = vec![0u8; row_bytes * h];
                            let mut p = start;
                            for (y, &n) in counts.iter().enumerate() {
                                unpack(bytes.get(p..p + n)?, &mut plane[y * row_bytes..(y + 1) * row_bytes])?;
                                p += n;
                            }
                            Some(plane)
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().ok().flatten()).collect::<Option<Vec<_>>>()
            });
            planes = ok?;
        }
        _ => return None,
    }

    // 16-bit: keep the high byte (big-endian, so the first byte of each pair).
    let px = |plane: &[u8], i: usize| plane[i * bpc];
    let mut out = vec![255u8; w * h * 4];
    for (i, o) in out.chunks_exact_mut(4).enumerate() {
        match mode {
            1 => {
                let v = px(&planes[0], i);
                o[0] = v;
                o[1] = v;
                o[2] = v;
                if use_n > 1 {
                    o[3] = px(&planes[1], i);
                }
            }
            3 => {
                o[0] = px(&planes[0], i);
                o[1] = px(&planes[1], i);
                o[2] = px(&planes[2], i);
                if use_n > 3 {
                    o[3] = px(&planes[3], i);
                }
            }
            _ => {
                // CMYK is stored inverted (255 = no ink); plain conversion, no ICC.
                let (c, m, y, k) = (px(&planes[0], i) as u32, px(&planes[1], i) as u32, px(&planes[2], i) as u32, px(&planes[3], i) as u32);
                o[0] = (c * k / 255) as u8;
                o[1] = (m * k / 255) as u8;
                o[2] = (y * k / 255) as u8;
            }
        }
    }
    RgbaImage::from_raw(w as u32, h as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 2x2 RGB PSD with RLE data and check the pixels come back.
    #[test]
    fn rle_rgb() {
        let mut f = b"8BPS".to_vec();
        f.extend([0, 1, 0, 0, 0, 0, 0, 0, 0, 3]); // version 1, reserved, 3 channels
        f.extend(2u32.to_be_bytes()); // height
        f.extend(2u32.to_be_bytes()); // width
        f.extend([0, 8, 0, 3]); // depth 8, RGB
        f.extend([0u8; 12]); // three empty sections
        f.extend([0, 1]); // RLE
        // each row: literal run of 2 bytes -> [1, a, b] = 3 bytes
        for _ in 0..6 {
            f.extend(3u16.to_be_bytes());
        }
        for (a, b) in [(10, 20), (30, 40), (50, 60), (70, 80), (90, 100), (110, 120)] {
            f.extend([1, a, b]);
        }
        let img = decode(&f).expect("decodes");
        assert_eq!(img.dimensions(), (2, 2));
        assert_eq!(img.get_pixel(0, 0).0, [10, 50, 90, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [40, 80, 120, 255]);
    }
}
