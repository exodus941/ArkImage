//! Image lists: a folder or a ZIP/CBZ archive, sorted, with sibling navigation.

use std::{
    cmp::Ordering,
    collections::HashMap,
    fs,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
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
    /// In folder order, shared by every window in the folder (see `Slot`).
    pub entries: Arc<Vec<Entry>>,
    /// Read the list backwards (an archive read right to left). Every read of the list
    /// goes through `get`, `len` and `iter`, so `index` is always in reading order.
    pub rev: bool,
    /// Position in reading order.
    pub index: usize,
}

impl Playlist {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Reading-order position to folder-order position, and back (the map is its own inverse).
    pub fn flip(&self, i: usize) -> usize {
        if self.rev { self.entries.len().saturating_sub(1).saturating_sub(i) } else { i }
    }
    pub fn get(&self, i: usize) -> Option<&Entry> {
        if i >= self.entries.len() {
            return None;
        }
        self.entries.get(self.flip(i))
    }
    pub fn iter(&self) -> Box<dyn Iterator<Item = &Entry> + '_> {
        if self.rev { Box::new(self.entries.iter().rev()) } else { Box::new(self.entries.iter()) }
    }
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
    #[cfg(target_os = "macos")]
    {
        // Finder's order.
        use objc2::msg_send;
        use objc2_foundation::NSString;
        let (na, nb) = (NSString::from_str(a), NSString::from_str(b));
        let r: isize = unsafe { msg_send![&*na, localizedStandardCompare: &*nb] };
        r.cmp(&0)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
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

/// Entry to position in the list (folder order).
pub type PosMap = HashMap<Entry, usize>;

/// The master's one list per folder. Every window showing an image from the folder
/// holds the same `Arc`; the slot keeps only a weak reference, so a list goes away with
/// its last window. A new listing that differs replaces the list, and `changed()` hands
/// the folder to the master, which gives the new list to every window in that folder.
struct Slot {
    sort: (u8, bool),
    /// Counts listings, so a window that waited for another's listing reuses it.
    gen: u64,
    list: Weak<Vec<Entry>>,
    pos: Arc<PosMap>,
}

static LISTS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<Slot>>>>> = Mutex::new(None);
static CHANGED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn build_pos(l: &[Entry]) -> PosMap {
    l.iter().cloned().enumerate().map(|(i, e)| (e, i)).collect()
}

fn slot(dir: &Path) -> Arc<Mutex<Slot>> {
    let mut g = LISTS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    // drop folders no window shows any more (a slot being filled is locked: keep it)
    m.retain(|_, s| s.try_lock().map(|s| s.list.strong_count() > 0).unwrap_or(true));
    m.entry(dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(Slot { sort: (u8::MAX, false), gen: 0, list: Weak::new(), pos: Arc::default() })))
        .clone()
}

/// The folder's list in this sort order. `fresh` reads the folder even when a list is
/// held (a window opening an image, a file gone); otherwise a held list is used as is.
/// Windows asking at the same moment share one read.
fn list_dir_shared(dir: &Path, sort: u8, desc: bool, fresh: bool) -> io::Result<Arc<Vec<Entry>>> {
    let slot = slot(dir);
    let seen = slot.try_lock().map(|s| s.gen).ok();
    let mut s = slot.lock().unwrap();
    let held = s.list.upgrade().filter(|_| s.sort == (sort, desc));
    if let Some(l) = &held {
        // not asked to re-read, or another window read it while this one waited
        if !fresh || seen != Some(s.gen) {
            crate::trace("list shared");
            return Ok(l.clone());
        }
    }
    let new = list_dir(dir, sort, desc)?;
    s.gen += 1;
    if let Some(l) = held {
        if *l == new {
            return Ok(l);
        }
    }
    let had = s.list.strong_count() > 0;
    let l = Arc::new(new);
    s.pos = Arc::new(build_pos(&l));
    s.list = Arc::downgrade(&l);
    s.sort = (sort, desc);
    if had {
        CHANGED.lock().unwrap().push(dir.to_path_buf());
    }
    Ok(l)
}

/// The held list of `dir` and its position map, if any window holds one. Err: the slot
/// is busy (a folder being read); the main thread never waits for it, so ask again later.
/// Waiting here, with LISTS held, froze the master behind a queue of 5 s listings.
pub fn held(dir: &Path) -> Result<Option<(Arc<Vec<Entry>>, Arc<PosMap>)>, ()> {
    let slot = { LISTS.lock().unwrap().as_ref().and_then(|m| m.get(dir).cloned()) };
    let Some(slot) = slot else { return Ok(None) };
    let Ok(s) = slot.try_lock() else { return Err(()) };
    Ok(s.list.upgrade().map(|l| (l, s.pos.clone())))
}

/// Hand `dir` to `changed()` again (its slot was busy).
pub fn retry_changed(dir: &Path) {
    CHANGED.lock().unwrap().push(dir.to_path_buf());
}

/// Folders whose list was replaced since the last call.
pub fn changed() -> Vec<PathBuf> {
    std::mem::take(&mut *CHANGED.lock().unwrap())
}

/// Remove `e` from the held list of `dir`, for every window in it (a file deleted).
pub fn remove(dir: &Path, e: &Entry) -> Option<Arc<Vec<Entry>>> {
    let slot = slot(dir);
    let mut s = slot.lock().unwrap();
    let l = s.list.upgrade()?;
    if !l.contains(e) {
        return None;
    }
    let l = Arc::new(l.iter().filter(|x| *x != e).cloned().collect::<Vec<_>>());
    s.gen += 1;
    s.pos = Arc::new(build_pos(&l));
    s.list = Arc::downgrade(&l);
    // the caller takes it at once; the other windows through changed()
    CHANGED.lock().unwrap().push(dir.to_path_buf());
    Some(l)
}

/// The position map for `l`: the held one when `l` is a folder's held list, else a new one.
pub fn pos_map(l: &Arc<Vec<Entry>>) -> Arc<PosMap> {
    if let Some(m) = LISTS.lock().unwrap().as_ref() {
        for s in m.values() {
            if let Ok(s) = s.try_lock() {
                if std::ptr::eq(s.list.as_ptr(), Arc::as_ptr(l)) {
                    return s.pos.clone();
                }
            }
        }
    }
    Arc::new(build_pos(l))
}

/// Open a folder, an archive, or an image file (its folder becomes the list), using the
/// folder's held list when there is one.
pub fn open(path: &Path, sort: u8, desc: bool) -> io::Result<Playlist> {
    open_list(path, sort, desc, false)
}

/// As `open`, but read the folder again; a changed list goes to every window in it.
pub fn open_fresh(path: &Path, sort: u8, desc: bool) -> io::Result<Playlist> {
    open_list(path, sort, desc, true)
}

fn open_list(path: &Path, sort: u8, desc: bool, fresh: bool) -> io::Result<Playlist> {
    let path = std::path::absolute(path)?;
    if path.is_dir() {
        let entries = list_dir_shared(&path, sort, desc, fresh)?;
        return Ok(Playlist { container: path, entries, rev: false, index: 0 });
    }
    if is_archive(&path) {
        let entries = Arc::new(list_zip(&path, sort, desc)?);
        return Ok(Playlist { container: path, entries, rev: false, index: 0 });
    }
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let mut entries = list_dir_shared(&dir, sort, desc, fresh).unwrap_or_default();
    let index = match entries
        .iter()
        .position(|e| matches!(e, Entry::File(p) if same_name(p, &path)))
    {
        Some(i) => i,
        None => {
            Arc::make_mut(&mut entries).insert(0, Entry::File(path.clone()));
            0
        }
    };
    Ok(Playlist { container: dir, entries, rev: false, index })
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
mod store_tests {
    use super::*;

    fn folder(tag: &str, names: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ark-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        for n in names {
            fs::write(d.join(n), b"x").unwrap();
        }
        d
    }

    #[test]
    fn one_list_per_folder() {
        let d = folder("share", &["a.png", "b.png", "c.png"]);
        let a = open_fresh(&d.join("a.png"), 0, false).unwrap();
        let b = open_fresh(&d.join("c.png"), 0, false).unwrap();
        let c = open(&d, 0, false).unwrap();
        assert!(Arc::ptr_eq(&a.entries, &b.entries) && Arc::ptr_eq(&a.entries, &c.entries));
        assert_eq!((a.index, b.index), (0, 2));
        assert!(Arc::ptr_eq(&pos_map(&a.entries), &pos_map(&b.entries)));
        let _ = changed();
        // a new file: the next fresh read replaces the list and names the folder
        fs::write(d.join("d.png"), b"x").unwrap();
        let n = open_fresh(&d.join("a.png"), 0, false).unwrap();
        assert_eq!(n.len(), 4);
        assert!(changed().contains(&d));
        assert!(Arc::ptr_eq(&held(&d).unwrap().unwrap().0, &n.entries));
        // a delete: one list without it, for everyone
        let e = n.get(1).unwrap().clone();
        let r = remove(&d, &e).unwrap();
        assert_eq!(r.len(), 3);
        assert!(!r.contains(&e) && changed().contains(&d));
        drop((a, b, c, n));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn newest_sort_replaces_the_list() {
        let d = folder("sort", &["a.png", "b.png"]);
        let a = open(&d, 0, false).unwrap();
        let z = open(&d, 0, true).unwrap();
        assert!(!Arc::ptr_eq(&a.entries, &z.entries));
        assert_eq!(z.get(0).unwrap().name(), "b.png");
        assert!(Arc::ptr_eq(&held(&d).unwrap().unwrap().0, &z.entries));
        drop((a, z));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reversed_reading() {
        let d = folder("rev", &["a.png", "b.png", "c.png"]);
        let mut p = open(&d, 0, false).unwrap();
        p.rev = true;
        let names: Vec<String> = p.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["c.png", "b.png", "a.png"]);
        assert_eq!(p.get(0).unwrap().name(), "c.png");
        assert_eq!((p.flip(0), p.flip(2)), (2, 0));
        assert!(p.get(3).is_none());
        drop(p);
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering::*;

    #[test]
    fn finder_order() {
        use super::explorer_cmp;
        assert_eq!(explorer_cmp("img2.jpg", "img10.jpg"), Less);
        assert_eq!(explorer_cmp("b.jpg", "A.jpg"), Greater);
        assert_eq!(explorer_cmp("a.jpg", "a.jpg"), Equal);
    }

    #[test]
    fn natural() {
        assert_eq!(natural_cmp("img2.jpg", "img10.jpg"), Less);
        assert_eq!(natural_cmp("img10.jpg", "img2.jpg"), Greater);
        assert_eq!(natural_cmp("A.jpg", "a.jpg"), Equal);
        assert_eq!(natural_cmp("p001", "p1"), Greater);
        assert_eq!(natural_cmp("ch 9 p3", "ch 10 p1"), Less);
    }
}
