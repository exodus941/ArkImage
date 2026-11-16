//! Thumbnails on disk, so the Select Image grid fills fast in a folder seen before.
//! One small file per thumbnail under <cache dir>/ArkImage/thumbs/<2 hex>/<16 hex>.
//! The key covers the path, file size, modified time and thumbnail size, so an edited
//! file gets a new thumbnail. Opaque thumbnails are JPEG (quality 85); thumbnails with
//! transparency are QOI. A size cap trims the oldest files at startup.

use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use image::{ImageFormat, RgbaImage};

use crate::source::Entry;

pub fn dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("ArkImage").join("thumbs")
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Cache file for `e` at thumbnail size `size` (EXIF rotation on/off is part of the key),
/// or None if the source cannot be stat'ed.
pub fn path_for(e: &Entry, size: u32, exif_rotate: bool) -> Option<PathBuf> {
    let md = fs::metadata(e.fs_path()).ok()?;
    let mtime = md.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    let key = format!("{}|{}|{}|{}|{}", e.label(), md.len(), mtime, size, exif_rotate as u8);
    let h = fnv(&key);
    Some(dir().join(format!("{:02x}", h >> 56)).join(format!("{h:016x}")))
}

pub fn load(p: &Path) -> Option<RgbaImage> {
    let bytes = fs::read(p).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.into_rgba8();
    // touch it so trimming keeps recently used thumbnails
    let _ = fs::File::options().write(true).open(p).and_then(|f| f.set_modified(SystemTime::now()));
    Some(img)
}

pub fn store(p: &Path, img: &RgbaImage) {
    let opaque = img.pixels().all(|px| px[3] == 255);
    let mut buf = Vec::new();
    let ok = if opaque {
        let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85).encode_image(&rgb).is_ok()
    } else {
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Qoi).is_ok()
    };
    if !ok {
        return;
    }
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // write to a temp name, then rename, so a reader never sees half a file
    let tmp = p.with_extension("tmp");
    if fs::write(&tmp, &buf).is_ok() {
        let _ = fs::rename(&tmp, p);
    }
}

/// Total bytes and file count on disk.
pub fn usage() -> (u64, usize) {
    let mut total = 0;
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(dir()) {
        for sub in rd.flatten() {
            if let Ok(files) = fs::read_dir(sub.path()) {
                for f in files.flatten() {
                    if let Ok(md) = f.metadata() {
                        total += md.len();
                        n += 1;
                    }
                }
            }
        }
    }
    (total, n)
}

/// Delete the least recently used thumbnails until the cache is under 90% of `cap_mb`.
pub fn trim(cap_mb: u64) {
    let cap = cap_mb * 1024 * 1024;
    let mut files = Vec::new();
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(dir()) {
        for sub in rd.flatten() {
            if let Ok(fs_) = fs::read_dir(sub.path()) {
                for f in fs_.flatten() {
                    if let Ok(md) = f.metadata() {
                        total += md.len();
                        files.push((md.modified().unwrap_or(UNIX_EPOCH), md.len(), f.path()));
                    }
                }
            }
        }
    }
    if total <= cap {
        return;
    }
    files.sort_by_key(|f| f.0);
    let target = cap / 10 * 9;
    for (_, len, p) in files {
        if total <= target {
            break;
        }
        if fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

pub fn clear() {
    let _ = fs::remove_dir_all(dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_changes_with_file_and_settings() {
        let p = std::env::temp_dir().join("arkimage_thumbkey_test.bin");
        fs::write(&p, b"one").unwrap();
        let e = Entry::File(p.clone());
        let a = path_for(&e, 160, true).unwrap();
        assert_eq!(a, path_for(&e, 160, true).unwrap(), "same input, same key");
        assert_ne!(a, path_for(&e, 192, true).unwrap(), "thumbnail size is in the key");
        assert_ne!(a, path_for(&e, 160, false).unwrap(), "EXIF setting is in the key");
        fs::write(&p, b"edited, longer").unwrap();
        assert_ne!(a, path_for(&e, 160, true).unwrap(), "an edited file gets a new key");
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn store_and_load_round_trip() {
        let p = std::env::temp_dir().join("arkimage_thumb_rt").join("t");
        let opaque = RgbaImage::from_pixel(8, 6, image::Rgba([200, 40, 40, 255]));
        store(&p, &opaque);
        let back = load(&p).unwrap();
        assert_eq!(back.dimensions(), (8, 6));
        assert!((back.get_pixel(3, 3)[0] as i32 - 200).abs() < 8, "JPEG keeps the colour");
        let clear_px = RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 0]));
        store(&p, &clear_px);
        assert_eq!(load(&p).unwrap().get_pixel(1, 1).0, [10, 20, 30, 0], "QOI keeps alpha exactly");
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}
