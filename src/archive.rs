//! Images inside ZIP/CBZ, RAR/CBR and 7z/CB7 archives.
//! The type comes from the file's first bytes (many .cbr files are really ZIPs).
//! ZIP entries are read one at a time. RAR and 7z are often solid: reading one entry
//! means decompressing everything before it. So the first read starts one background
//! pass that extracts every image in order; each reader waits only until its own
//! entry is out. The last two archives stay in memory.

use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::SystemTime,
};

use crate::source::{is_archive_name, is_image_name};

/// Entries worth listing or extracting: images, and archives nested inside this one.
fn wanted(name: &str) -> bool {
    is_image_name(name) || is_archive_name(name)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Zip,
    Rar,
    SevenZ,
}

pub fn kind(path: &Path) -> Option<Kind> {
    let mut head = [0u8; 8];
    let n = fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let h = &head[..n];
    if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") {
        return Some(Kind::Zip);
    }
    if h.starts_with(b"Rar!\x1a\x07") {
        return Some(Kind::Rar);
    }
    if h.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(Kind::SevenZ);
    }
    None
}

/// Archive entry names use '/' whatever the archive stored.
fn norm(name: &str) -> String {
    name.replace('\\', "/")
}

/// Image and nested-archive entries: (name, uncompressed size), in archive order.
pub fn list(path: &Path) -> io::Result<Vec<(String, u64)>> {
    let mut out = Vec::new();
    match kind(path) {
        Some(Kind::Zip) => {
            let f = fs::File::open(path)?;
            let mut z = zip::ZipArchive::new(io::BufReader::new(f)).map_err(io::Error::other)?;
            for i in 0..z.len() {
                let Ok(f) = z.by_index_raw(i) else { continue };
                let name = f.name().to_string();
                if !f.is_dir() && wanted(&name) {
                    out.push((name, f.size()));
                }
            }
        }
        Some(Kind::Rar) => {
            let mut a = unrar::Archive::new(path).open_for_listing().map_err(io::Error::other)?;
            while let Some(h) = a.read_header().map_err(io::Error::other)? {
                let e = h.entry();
                let name = norm(&e.filename.to_string_lossy());
                if e.is_file() && wanted(&name) {
                    out.push((name, e.unpacked_size));
                }
                a = h.skip().map_err(io::Error::other)?;
            }
        }
        Some(Kind::SevenZ) => {
            let r = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty()).map_err(io::Error::other)?;
            for e in &r.archive().files {
                let name = norm(e.name());
                if !e.is_directory() && wanted(&name) {
                    out.push((name, e.size()));
                }
            }
        }
        None => return Err(io::Error::new(io::ErrorKind::InvalidData, "not a ZIP, RAR or 7z archive")),
    }
    out.retain(|(n, _)| !n.starts_with("__MACOSX/"));
    Ok(out)
}

/// One archive being (or already) extracted on a background thread.
struct Slot {
    path: PathBuf,
    mtime: Option<SystemTime>,
    files: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    /// None while running; Some(Ok) when complete; Some(Err) on failure.
    done: Mutex<Option<Result<(), String>>>,
    cv: Condvar,
}

fn slots() -> &'static Mutex<Vec<Arc<Slot>>> {
    static S: OnceLock<Mutex<Vec<Arc<Slot>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Archives kept in memory at once.
const KEEP: usize = 2;

fn slot_for(path: &Path) -> Arc<Slot> {
    let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut list = slots().lock().unwrap();
    if let Some(i) = list.iter().position(|s| s.path == path && s.mtime == mtime) {
        let s = list.remove(i);
        list.push(s.clone()); // most recently used last
        return s;
    }
    let s = Arc::new(Slot { path: path.to_path_buf(), mtime, files: Mutex::new(HashMap::new()), done: Mutex::new(None), cv: Condvar::new() });
    list.retain(|x| x.path != path);
    list.push(s.clone());
    while list.len() > KEEP {
        list.remove(0);
    }
    let worker = s.clone();
    std::thread::spawn(move || {
        crate::trace(&format!("archive extract start {}", worker.path.display()));
        let r = extract_all(&worker);
        crate::trace("archive extract done");
        *worker.done.lock().unwrap() = Some(r);
        worker.cv.notify_all();
    });
    s
}

fn put(s: &Slot, name: String, data: Vec<u8>) {
    // Hold `done` (the lock readers wait on) so a reader cannot miss this wake-up
    // between checking `files` and starting to wait.
    let _g = s.done.lock().unwrap();
    s.files.lock().unwrap().insert(name, Arc::new(data));
    s.cv.notify_all();
}

fn extract_all(s: &Slot) -> Result<(), String> {
    match kind(&s.path) {
        Some(Kind::Rar) => {
            let mut a = unrar::Archive::new(&s.path).open_for_processing().map_err(|e| e.to_string())?;
            while let Some(h) = a.read_header().map_err(|e| e.to_string())? {
                let name = norm(&h.entry().filename.to_string_lossy());
                if h.entry().is_file() && wanted(&name) {
                    let (data, rest) = h.read().map_err(|e| e.to_string())?;
                    put(s, name, data);
                    a = rest;
                } else {
                    a = h.skip().map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        }
        Some(Kind::SevenZ) => {
            let mut r = sevenz_rust2::ArchiveReader::open(&s.path, sevenz_rust2::Password::empty()).map_err(|e| e.to_string())?;
            r.for_each_entries(|e, rd| {
                let name = norm(e.name());
                if !e.is_directory() && wanted(&name) {
                    let mut v = Vec::with_capacity(e.size() as usize);
                    rd.read_to_end(&mut v)?;
                    put(s, name, v);
                }
                Ok(true)
            })
            .map_err(|e| e.to_string())
        }
        _ => Err("not a RAR or 7z archive".into()),
    }
}

/// An archive stored inside `outer`, written once to <cache dir>/ArkImage/nested so it
/// can be opened like any archive on disk. The file name hashes the outer path, size,
/// modified time and inner name, so a changed outer archive gets fresh copies.
/// Only the 16 most recently used copies are kept.
pub fn extract_nested(outer: &Path, inner: &str) -> io::Result<PathBuf> {
    let md = fs::metadata(outer)?;
    let mtime = md.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let key = format!("{}|{}|{}|{}", outer.display(), md.len(), mtime, inner);
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("ArkImage").join("nested");
    let ext = crate::source::ext_of(inner);
    let path = dir.join(format!("{h:016x}.{ext}"));
    if path.exists() {
        let _ = fs::File::options().write(true).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
        return Ok(path);
    }
    let bytes = read(outer, inner)?;
    fs::create_dir_all(&dir)?;
    let tmp = path.with_extension("part");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &path)?;
    // keep the 16 most recent copies
    if let Ok(rd) = fs::read_dir(&dir) {
        let mut files: Vec<(SystemTime, PathBuf)> = rd.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
        if files.len() > 16 {
            files.sort_by_key(|f| std::cmp::Reverse(f.0));
            for (_, p) in files.into_iter().skip(16) {
                if p != path {
                    let _ = fs::remove_file(p);
                }
            }
        }
    }
    Ok(path)
}

/// One entry from a non-solid RAR or 7z without touching the others.
/// None means the archive is solid (or could not be opened): use the in-order pass.
fn read_direct(k: Kind, path: &Path, name: &str) -> Option<io::Result<Vec<u8>>> {
    match k {
        Kind::Rar => {
            let mut a = unrar::Archive::new(path).open_for_processing().ok()?;
            if a.is_solid() {
                return None;
            }
            loop {
                let h = match a.read_header() {
                    Ok(Some(h)) => h,
                    Ok(None) => return Some(Err(io::Error::new(io::ErrorKind::NotFound, format!("{name} not in archive")))),
                    Err(e) => return Some(Err(io::Error::other(e))),
                };
                if norm(&h.entry().filename.to_string_lossy()) == name {
                    return Some(h.read().map(|(d, _)| d).map_err(io::Error::other));
                }
                a = match h.skip() {
                    Ok(a) => a,
                    Err(e) => return Some(Err(io::Error::other(e))),
                };
            }
        }
        Kind::SevenZ => {
            let mut r = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty()).ok()?;
            let with_data = r.archive().files.iter().filter(|f| f.has_stream).count();
            // solid: fewer blocks than files with data
            if r.archive().blocks.len() < with_data {
                return None;
            }
            let raw = r.archive().files.iter().find(|f| norm(f.name()) == name)?.name().to_string();
            Some(r.read_file(&raw).map_err(io::Error::other))
        }
        Kind::Zip => None,
    }
}

/// Bytes of one entry.
pub fn read(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    match kind(path) {
        Some(Kind::Zip) => {
            let f = fs::File::open(path)?;
            let mut z = zip::ZipArchive::new(io::BufReader::new(f)).map_err(io::Error::other)?;
            let mut zf = z.by_name(name).map_err(io::Error::other)?;
            let mut v = Vec::with_capacity(zf.size() as usize);
            zf.read_to_end(&mut v)?;
            Ok(v)
        }
        Some(k) => {
            // Non-solid archives: read the one entry directly. (A comic stored with its
            // cover last waited 516 ms for the in-order pass before this.)
            if let Some(r) = read_direct(k, path, name) {
                return r;
            }
            let s = slot_for(path);
            let mut done = s.done.lock().unwrap();
            loop {
                if let Some(d) = s.files.lock().unwrap().get(name) {
                    return Ok(d.as_ref().clone());
                }
                match &*done {
                    Some(Ok(())) => return Err(io::Error::new(io::ErrorKind::NotFound, format!("{name} not in archive"))),
                    Some(Err(e)) => return Err(io::Error::other(e.clone())),
                    None => done = s.cv.wait(done).unwrap(),
                }
            }
        }
        None => Err(io::Error::new(io::ErrorKind::InvalidData, "not a ZIP, RAR or 7z archive")),
    }
}
