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

    /// The file's bytes. On a spinning disk one read at a time: decoders reading several
    /// files at once made the head seek between them, and a turned page waited up to
    /// 1.9 s for its read (4 TB HDD, 46,000-file folder; 50 ms at most on an SSD).
    pub fn read_bytes(&self) -> io::Result<Vec<u8>> {
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = seek_penalty(self.fs_path()).then(|| ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner()));
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
    let mut items = dir_items(dir)?;
    sort_items(&mut items, sort, desc);
    Ok(items.into_iter().map(|i| i.entry).collect())
}

/// The images in `dir`, unsorted.
fn dir_items(dir: &Path) -> io::Result<Vec<Item>> {
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
    Ok(items)
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
    /// The folder's own modified time when it was last read. Adding, removing or
    /// renaming a file changes it, so an equal time means no new file.
    seen: Option<SystemTime>,
    /// New files still growing at the last poll, with their size then. A file joins the
    /// list once its size holds for one poll, so a half-copied file never decodes.
    growing: HashMap<PathBuf, u64>,
}

/// How often the master checks each open folder for new files.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// The folder time to keep, or None when it is under 1 s old. A file added in the same
/// clock tick as the read leaves the time unchanged; the test failed 2 runs in 6 that way.
fn settled(t: SystemTime) -> Option<SystemTime> {
    let age = SystemTime::now().duration_since(t).unwrap_or_default();
    (age >= std::time::Duration::from_secs(1)).then_some(t)
}

/// Check every folder a window shows for new or removed files, and replace the list of
/// each one that changed (see `changed()`). Runs on its own thread. True: a list changed.
pub fn poll() -> bool {
    poll_only(None)
}

/// As `poll`, for one folder only when `only` names it (tests run beside other tests).
fn poll_only(only: Option<&Path>) -> bool {
    let slots: Vec<(PathBuf, Arc<Mutex<Slot>>)> = LISTS
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.iter().filter(|(d, _)| only.is_none_or(|o| o == *d)).map(|(d, s)| (d.clone(), s.clone())).collect())
        .unwrap_or_default();
    let mut any = false;
    for (dir, slot) in slots {
        let mut s = slot.lock().unwrap();
        let Some(held) = s.list.upgrade() else { continue };
        // read the time before the folder, so a file added during the read is seen next time
        let Ok(now) = fs::metadata(&dir).and_then(|m| m.modified()) else { continue };
        if s.seen == Some(now) && s.growing.is_empty() {
            continue;
        }
        let Ok(mut items) = dir_items(&dir) else { continue };
        s.seen = settled(now);
        let known: std::collections::HashSet<&Entry> = held.iter().collect();
        let mut growing = HashMap::new();
        items.retain(|i| {
            if known.contains(&i.entry) {
                return true;
            }
            let Entry::File(p) = &i.entry else { return true };
            let still = s.growing.get(p) == Some(&i.size) && i.size > 0;
            if !still {
                growing.insert(p.clone(), i.size);
            }
            still
        });
        s.growing = growing;
        let (sort, desc) = s.sort;
        sort_items(&mut items, sort, desc);
        let new: Vec<Entry> = items.into_iter().map(|i| i.entry).collect();
        if new == *held || new.is_empty() {
            continue;
        }
        let l = Arc::new(new);
        s.gen += 1;
        s.pos = Arc::new(build_pos(&l));
        s.list = Arc::downgrade(&l);
        // the windows take it from changed(); until then this thread keeps it alive
        KEEP.lock().unwrap().push(l);
        CHANGED.lock().unwrap().push(dir);
        any = true;
    }
    any
}

/// Lists made by `poll()`, kept until the master hands them to the windows.
static KEEP: Mutex<Vec<Arc<Vec<Entry>>>> = Mutex::new(Vec::new());

/// As `remove`, from another thread: the new list stays alive until the master hands it
/// to the windows (send `UserEvent::Polled`).
pub fn remove_polled(dir: &Path, e: &Entry) {
    if let Some(l) = remove(dir, e) {
        KEEP.lock().unwrap().push(l);
    }
}

/// Drop the lists `poll()` kept; the windows hold them now.
pub fn release_polled() {
    // a busy window asks again later, so keep the lists until none waits
    if CHANGED.lock().unwrap().is_empty() {
        KEEP.lock().unwrap().clear();
    }
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
        .or_insert_with(|| Arc::new(Mutex::new(Slot { sort: (u8::MAX, false), gen: 0, list: Weak::new(), pos: Arc::default(), seen: None, growing: HashMap::new() })))
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
    let seen = fs::metadata(dir).and_then(|m| m.modified()).ok();
    let new = list_dir(dir, sort, desc)?;
    s.seen = seen.and_then(settled);
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
    fn poll_adds_new_files_once_whole() {
        let dir = std::env::temp_dir().join(format!("arkimage-poll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        let pl = super::open(&dir.join("a.png"), 0, false).unwrap();
        assert_eq!(pl.entries.len(), 1);
        // other tests share the lists, so read only this folder's held list
        let len = || super::held(&dir).unwrap().unwrap().0.len();
        super::poll_only(Some(&dir));
        assert_eq!(len(), 1);
        std::fs::write(dir.join("b.png"), b"xy").unwrap();
        // first poll sees b growing, second sees its size hold
        super::poll_only(Some(&dir));
        assert_eq!(len(), 1);
        super::poll_only(Some(&dir));
        assert_eq!(len(), 2);
        // a deleted file leaves on the next poll
        std::fs::remove_file(dir.join("b.png")).unwrap();
        super::poll_only(Some(&dir));
        assert_eq!(len(), 1);
        drop(pl);
        super::release_polled();
        let _ = std::fs::remove_dir_all(&dir);
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

/// True when the drive holding `p` has a seek penalty (a spinning disk), as Windows reports
/// it (IOCTL_STORAGE_QUERY_PROPERTY, StorageDeviceSeekPenaltyProperty). Asked once per drive.
#[cfg(windows)]
pub fn seek_penalty(p: &Path) -> bool {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::path::Component;
    static SEEN: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
    let letter = std::path::absolute(p).ok().and_then(|a| match a.components().next() {
        Some(Component::Prefix(x)) => match x.kind() {
            std::path::Prefix::Disk(d) | std::path::Prefix::VerbatimDisk(d) => Some(d as char),
            _ => None,
        },
        _ => None,
    });
    let Some(letter) = letter else { return false };
    let key = letter.to_ascii_uppercase().to_string();
    if let Some(v) = SEEN.lock().unwrap().as_ref().and_then(|m| m.get(&key)) {
        return *v;
    }
    #[repr(C)]
    struct Query {
        property: u32,
        query: u32,
        extra: [u8; 4],
    }
    #[repr(C)]
    struct Penalty {
        version: u32,
        size: u32,
        incurs: u8,
        pad: [u8; 3],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *mut c_void, disp: u32, flags: u32, tmpl: *mut c_void) -> isize;
        fn DeviceIoControl(h: isize, code: u32, inb: *const c_void, insz: u32, outb: *mut c_void, outsz: u32, ret: *mut u32, ov: *mut c_void) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    let name: Vec<u16> = format!(r"\\.\{key}:").encode_utf16().chain(std::iter::once(0)).collect();
    // no access needed to ask the property; share read and write; OPEN_EXISTING
    let h = unsafe { CreateFileW(name.as_ptr(), 0, 3, std::ptr::null_mut(), 3, 0, std::ptr::null_mut()) };
    let mut v = false;
    if h != -1 {
        let q = Query { property: 7, query: 0, extra: [0; 4] };
        let mut out = Penalty { version: 0, size: 0, incurs: 0, pad: [0; 3] };
        let mut got = 0u32;
        // IOCTL_STORAGE_QUERY_PROPERTY
        if unsafe { DeviceIoControl(h, 0x002D_1400, &q as *const Query as *const c_void, std::mem::size_of::<Query>() as u32, &mut out as *mut Penalty as *mut c_void, std::mem::size_of::<Penalty>() as u32, &mut got, std::ptr::null_mut()) } != 0 {
            v = out.incurs != 0;
        }
        unsafe { CloseHandle(h) };
    }
    crate::trace(&format!("drive {key}: seek penalty {v}"));
    SEEN.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, v);
    v
}

/// True when the volume holding `p` is on a spinning disk (an external HDD, say), as
/// `diskutil` reports it ("SolidState" false). Asked once per volume (its device id).
#[cfg(target_os = "macos")]
pub fn seek_penalty(p: &Path) -> bool {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;
    static SEEN: Mutex<Option<HashMap<u64, bool>>> = Mutex::new(None);
    let Ok(dev) = std::fs::metadata(p).map(|m| m.dev()) else { return false };
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = g.as_ref().and_then(|m| m.get(&dev)) {
        return *v;
    }
    let out = std::process::Command::new("/usr/sbin/diskutil").args(["info", "-plist"]).arg(p).output();
    let text = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    // <key>SolidState</key> then <true/> or <false/>; missing (a network volume): not spinning
    let v = text.split("<key>SolidState</key>").nth(1).is_some_and(|r| r.trim_start().starts_with("<false/>"));
    crate::trace(&format!("volume {dev}: seek penalty {v}"));
    g.get_or_insert_with(HashMap::new).insert(dev, v);
    v
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn seek_penalty(_p: &Path) -> bool {
    false
}
