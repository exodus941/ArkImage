//! Image lists: a folder or a ZIP/CBZ archive, sorted, with sibling navigation.

use std::{
    cmp::Ordering,
    fs,
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "bmp", "dib", "tif", "tiff",
    "tga", "dds", "ico", "pnm", "pbm", "pgm", "ppm", "pam", "hdr", "exr", "qoi", "psd", "psb", "heic", "heif", "hif",
    "svg", "svgz", "lottie", "json",
];
/// Extensions the app opens but never registers for: other programs own them.
pub const NOT_ASSOCIATED: &[&str] = &["json"];
pub const ARCHIVE_EXTS: &[&str] = &["zip", "cbz", "rar", "cbr", "7z", "cb7"];

pub fn ext_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn is_archive_name(name: &str) -> bool {
    ARCHIVE_EXTS.contains(&ext_of(name).as_str())
}

pub fn is_image_name(name: &str) -> bool {
    IMAGE_EXTS.contains(&ext_of(name).as_str())
}

/// Reads the first 64 KB of a .json file and checks for Lottie keys.
fn json_file_is_lottie(p: &Path) -> bool {
    use std::io::Read;
    let mut head = vec![0u8; 64 * 1024];
    let Ok(mut f) = fs::File::open(p) else { return false };
    let n = f.read(&mut head).unwrap_or(0);
    head.truncate(n);
    let has = |k: &[u8]| head.windows(k.len()).any(|w| w == k);
    head.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') && has(b"\"layers\"") && has(b"\"fr\"")
}

pub fn is_archive(p: &Path) -> bool {
    ARCHIVE_EXTS.contains(&ext_of(&p.to_string_lossy()).as_str()) && p.is_file()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Entry {
    File(PathBuf),
    Zip { archive: PathBuf, name: String },
}

impl Entry {
    pub fn name(&self) -> String {
        match self {
            Entry::File(p) => p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Entry::Zip { name, .. } => name.rsplit('/').next().unwrap_or(name).to_string(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Entry::File(p) => p.display().to_string(),
            Entry::Zip { archive, name } => format!("{}|{}", archive.display(), name),
        }
    }

    pub fn ext(&self) -> String {
        match self {
            Entry::File(p) => ext_of(&p.to_string_lossy()),
            Entry::Zip { name, .. } => ext_of(name),
        }
    }

    /// The file on disk: the image itself, or the archive that holds it.
    pub fn fs_path(&self) -> &Path {
        match self {
            Entry::File(p) => p,
            Entry::Zip { archive, .. } => archive,
        }
    }

    pub fn read_bytes(&self) -> io::Result<Vec<u8>> {
        match self {
            Entry::File(p) => fs::read(p),
            Entry::Zip { archive, name } => crate::archive::read(archive, name),
        }
    }
}

#[derive(Clone)]
pub struct Playlist {
    /// The folder or archive the entries come from.
    pub container: PathBuf,
    pub entries: Vec<Entry>,
    pub index: usize,
}

struct Item {
    entry: Entry,
    key: String,
    size: u64,
    mtime: SystemTime,
}

/// Case-insensitive compare where digit runs compare by value: "2" < "10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    // Leading-zero difference ("02" vs "2") decides only when all else is equal, as in Explorer.
    let mut zeros = Ordering::Equal;
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return zeros,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    bi.next();
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
                if zeros == Ordering::Equal {
                    zeros = nb.len().cmp(&na.len()).reverse();
                }
            }
            (Some(ca), Some(cb)) => {
                if ca != cb {
                    return ca.cmp(&cb);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Explorer's order: the column (reversed when descending), then, for equal values, the
/// name in Explorer's own comparison, always ascending. Sorting by name reverses the name.
fn sort_items(v: &mut [Item], sort: u8, desc: bool) {
    let flip = |o: Ordering| if desc { o.reverse() } else { o };
    v.sort_by(|a, b| {
        let col = match sort {
            1 => a.mtime.cmp(&b.mtime),
            2 => a.size.cmp(&b.size),
            3 => ext_of(&a.key).to_lowercase().cmp(&ext_of(&b.key).to_lowercase()),
            _ => return flip(explorer_cmp(&a.key, &b.key)),
        };
        flip(col).then_with(|| explorer_cmp(&a.key, &b.key))
    });
}

/// Names as Explorer orders them: StrCmpLogicalW on Windows, natural_cmp elsewhere.
pub fn explorer_cmp(a: &str, b: &str) -> Ordering {
    #[cfg(windows)]
    {
        #[link(name = "shlwapi")]
        extern "system" {
            fn StrCmpLogicalW(a: *const u16, b: *const u16) -> i32;
        }
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (wa, wb) = (w(a), w(b));
        unsafe { StrCmpLogicalW(wa.as_ptr(), wb.as_ptr()) }.cmp(&0)
    }
    #[cfg(not(windows))]
    natural_cmp(a, b)
}

fn list_dir(dir: &Path, sort: u8, desc: bool) -> io::Result<Vec<Entry>> {
    let mut items = Vec::new();
    for de in fs::read_dir(dir)? {
        let Ok(de) = de else { continue };
        let name = de.file_name().to_string_lossy().into_owned();
        if !is_image_name(&name) {
            continue;
        }
        let p = de.path();
        // most .json files are not animations: list only those that look like Lottie
        if ext_of(&name) == "json" && !json_file_is_lottie(&p) {
            continue;
        }
        // DirEntry carries type/size/date from the directory scan on Windows; a full
        // fs::metadata per file costs an extra open for each of e.g. 46,000 files.
        let Ok(ft) = de.file_type() else { continue };
        let md = if ft.is_symlink() { fs::metadata(&p) } else { de.metadata() };
        let Ok(md) = md else { continue };
        if !md.is_file() {
            continue;
        }
        items.push(Item {
            key: name,
            size: md.len(),
            mtime: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            entry: Entry::File(p),
        });
    }
    sort_items(&mut items, sort, desc);
    Ok(items.into_iter().map(|i| i.entry).collect())
}

/// Images in a ZIP/CBZ, RAR/CBR or 7z/CB7 archive (entries keep the `Zip` variant name).
fn list_zip(archive: &Path, sort: u8, desc: bool) -> io::Result<Vec<Entry>> {
    let mut items = Vec::new();
    for (name, size) in crate::archive::list(archive)? {
        if is_image_name(&name) && ext_of(&name) != "json" {
            items.push(Item { key: name.clone(), size, mtime: SystemTime::UNIX_EPOCH, entry: Entry::Zip { archive: archive.to_path_buf(), name } });
            continue;
        }
        // An archive inside this one (e.g. 4 issues packed in one .rar): copy it out once,
        // then list its images under "<inner name>/<page>" so issues stay in order.
        // One level only; deeper nesting is skipped.
        let Ok(inner) = crate::archive::extract_nested(archive, &name) else { continue };
        let Ok(pages) = crate::archive::list(&inner) else { continue };
        for (page, psize) in pages.into_iter().filter(|(p, _)| is_image_name(p)) {
            items.push(Item {
                key: format!("{name}/{page}"),
                size: psize,
                mtime: SystemTime::UNIX_EPOCH,
                entry: Entry::Zip { archive: inner.clone(), name: page },
            });
        }
    }
    // a comic book reads in page order: the sort setting does not apply inside an archive
    let _ = (sort, desc);
    sort_items(&mut items, 0, false);
    Ok(items.into_iter().map(|i| i.entry).collect())
}

fn same_name(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) if cfg!(windows) => {
            x.to_string_lossy().to_lowercase() == y.to_string_lossy().to_lowercase()
        }
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Open a folder, an archive, or an image file (its folder becomes the list).
pub fn open(path: &Path, sort: u8, desc: bool) -> io::Result<Playlist> {
    let path = std::path::absolute(path)?;
    if path.is_dir() {
        let entries = list_dir(&path, sort, desc)?;
        return Ok(Playlist { container: path, entries, index: 0 });
    }
    if is_archive(&path) {
        let entries = list_zip(&path, sort, desc)?;
        return Ok(Playlist { container: path, entries, index: 0 });
    }
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let mut entries = list_dir(&dir, sort, desc).unwrap_or_default();
    let index = match entries
        .iter()
        .position(|e| matches!(e, Entry::File(p) if same_name(p, &path)))
    {
        Some(i) => i,
        None => {
            entries.insert(0, Entry::File(path.clone()));
            0
        }
    };
    Ok(Playlist { container: dir, entries, index })
}

/// The next or previous folder (and archive, when `archives`) beside `container` that
/// holds images. Moving on by itself from a folder passes false: a folder ignores archives.
pub fn sibling(container: &Path, forward: bool, sort: u8, desc: bool, archives: bool) -> Option<Playlist> {
    let parent = container.parent()?;
    let mut cands: Vec<PathBuf> = fs::read_dir(parent)
        .ok()?
        .filter_map(|d| d.ok())
        .map(|d| d.path())
        .filter(|p| p.is_dir() || (archives && is_archive(p)) || same_name(p, container))
        .collect();
    cands.sort_by(|a, b| {
        natural_cmp(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    let pos = cands.iter().position(|p| same_name(p, container))?;
    let order: Vec<usize> = if forward {
        (pos + 1..cands.len()).collect()
    } else {
        (0..pos).rev().collect()
    };
    for i in order {
        if let Ok(pl) = open(&cands[i], sort, desc) {
            if !pl.entries.is_empty() {
                return Some(pl);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering::*;

    #[test]
    fn natural() {
        assert_eq!(natural_cmp("img2.jpg", "img10.jpg"), Less);
        assert_eq!(natural_cmp("img10.jpg", "img2.jpg"), Greater);
        assert_eq!(natural_cmp("A.jpg", "a.jpg"), Equal);
        assert_eq!(natural_cmp("p001", "p1"), Greater);
        assert_eq!(natural_cmp("ch 9 p3", "ch 10 p1"), Less);
    }
}
