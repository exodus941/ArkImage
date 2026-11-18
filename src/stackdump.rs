//! Windows: a watchdog that writes the main thread's call stack to the trace when the
//! main thread has not passed `beat()` for 3 s. Frames are named from DLL exports
//! (dbghelp, no symbol files), with module offsets for the exe itself.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static BEAT: AtomicU64 = AtomicU64::new(0);
static MAIN_TID: AtomicU32 = AtomicU32::new(0);

fn now_ms() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The main thread is alive.
pub fn beat() {
    BEAT.store(now_ms(), Ordering::Relaxed);
}

#[repr(C, align(16))]
struct Context([u8; 1232]);

#[repr(C)]
struct Address64 {
    offset: u64,
    segment: u16,
    mode: u32,
}

#[repr(C)]
struct StackFrame64 {
    pc: Address64,
    ret: Address64,
    frame: Address64,
    stack: Address64,
    bstore: Address64,
    func_table: *mut c_void,
    params: [u64; 4],
    far: i32,
    virt: i32,
    reserved: [u64; 3],
    kd: [u64; 5],
}

#[repr(C)]
struct SymbolInfo {
    size_of_struct: u32,
    type_index: u32,
    reserved: [u64; 2],
    index: u32,
    size: u32,
    mod_base: u64,
    flags: u32,
    value: u64,
    address: u64,
    register: u32,
    scope: u32,
    tag: u32,
    name_len: u32,
    max_name_len: u32,
    name: [u8; 256],
}

type Callback = unsafe extern "system" fn(*mut c_void, u64) -> *mut c_void;
type BaseCallback = unsafe extern "system" fn(*mut c_void, u64) -> u64;

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentProcess() -> *mut c_void;
    fn OpenThread(access: u32, inherit: i32, tid: u32) -> *mut c_void;
    fn SuspendThread(h: *mut c_void) -> u32;
    fn ResumeThread(h: *mut c_void) -> u32;
    fn GetThreadContext(h: *mut c_void, c: *mut Context) -> i32;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn ReadProcessMemory(p: *mut c_void, addr: *const c_void, buf: *mut c_void, n: usize, got: *mut usize) -> i32;
    fn GetModuleHandleExW(flags: u32, addr: *const c_void, m: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(m: *mut c_void, buf: *mut u16, n: u32) -> u32;
}

#[link(name = "dbghelp")]
extern "system" {
    fn SymInitialize(p: *mut c_void, path: *const u8, invade: i32) -> i32;
    fn SymFromAddr(p: *mut c_void, addr: u64, disp: *mut u64, s: *mut SymbolInfo) -> i32;
    fn SymFunctionTableAccess64(p: *mut c_void, base: u64) -> *mut c_void;
    fn SymGetModuleBase64(p: *mut c_void, addr: u64) -> u64;
    fn StackWalk64(machine: u32, p: *mut c_void, t: *mut c_void, f: *mut StackFrame64, c: *mut Context, read: *const c_void, fta: Callback, gmb: BaseCallback, tr: *const c_void) -> i32;
}

const SNAP_LEN: usize = 128 << 10;
static mut SNAP: [u8; SNAP_LEN] = [0; SNAP_LEN];
static SNAP_BASE: AtomicU64 = AtomicU64::new(0);
static SNAP_GOT: AtomicU64 = AtomicU64::new(0);

/// StackWalk64's memory reader: stack bytes from the copy, code and tables live.
unsafe extern "system" fn read_mem(p: *mut c_void, addr: u64, buf: *mut c_void, size: u32, read: *mut u32) -> i32 {
    let (b, n) = (SNAP_BASE.load(Ordering::Relaxed), SNAP_GOT.load(Ordering::Relaxed));
    if addr >= b && addr + size as u64 <= b + n {
        let src = std::ptr::addr_of!(SNAP) as *const u8;
        std::ptr::copy_nonoverlapping(src.add((addr - b) as usize), buf as *mut u8, size as usize);
        *read = size;
        return 1;
    }
    let mut got = 0usize;
    let ok = ReadProcessMemory(p, addr as *const c_void, buf, size as usize, &mut got);
    *read = got as u32;
    ok
}

/// Start the watchdog; call on the main thread.
pub fn start() {
    MAIN_TID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    beat();
    std::thread::spawn(|| {
        let mut dumped_at = u64::MAX;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let b = BEAT.load(Ordering::Relaxed);
            if now_ms().saturating_sub(b) > 3000 && dumped_at != b {
                // no beat yet past the first event: idle in the message wait counts too
                dumped_at = b;
                dump();
            }
        }
    });
}

fn module_of(addr: u64) -> (String, u64) {
    unsafe {
        let mut m: *mut c_void = std::ptr::null_mut();
        // FROM_ADDRESS | UNCHANGED_REFCOUNT
        if GetModuleHandleExW(4 | 2, addr as *const c_void, &mut m) == 0 {
            return ("?".into(), 0);
        }
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(m, buf.as_mut_ptr(), 260) as usize;
        let p = String::from_utf16_lossy(&buf[..n]);
        (p.rsplit('\\').next().unwrap_or("?").to_string(), m as u64)
    }
}

fn dump() {
    unsafe {
        let proc_ = GetCurrentProcess();
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            SymInitialize(proc_, std::ptr::null(), 1);
        });
        // SUSPEND_RESUME | GET_CONTEXT | QUERY_INFORMATION
        let t = OpenThread(0x0002 | 0x0008 | 0x0040, 0, MAIN_TID.load(Ordering::Relaxed));
        if t.is_null() {
            return;
        }
        // While the thread is stopped: copy its registers and the top of its stack into
        // buffers made beforehand, and nothing else. It may hold the heap lock; any
        // allocation here would then wait for it forever.
        let snap = &mut *std::ptr::addr_of_mut!(SNAP);
        SuspendThread(t);
        let mut c = Context([0; 1232]);
        // ContextFlags at offset 0x30: CONTEXT_FULL (AMD64)
        c.0[0x30..0x34].copy_from_slice(&0x0010_000Bu32.to_le_bytes());
        let ok = GetThreadContext(t, &mut c) != 0;
        let rsp = u64::from_le_bytes(c.0[0x98..0xA0].try_into().unwrap());
        let mut got = 0usize;
        if ok {
            ReadProcessMemory(proc_, rsp as *const c_void, snap.as_mut_ptr() as *mut c_void, SNAP_LEN, &mut got);
        }
        ResumeThread(t);
        CloseHandle(t);
        SNAP_BASE.store(rsp, Ordering::Relaxed);
        SNAP_GOT.store(got as u64, Ordering::Relaxed);
        let mut lines = Vec::new();
        if ok {
            let rd = |o: usize| u64::from_le_bytes(c.0[o..o + 8].try_into().unwrap());
            let (rip, rbp) = (rd(0xF8), rd(0xA0));
            let mut f: StackFrame64 = std::mem::zeroed();
            f.pc = Address64 { offset: rip, segment: 0, mode: 3 };
            f.stack = Address64 { offset: rsp, segment: 0, mode: 3 };
            f.frame = Address64 { offset: rbp, segment: 0, mode: 3 };
            for _ in 0..48 {
                // IMAGE_FILE_MACHINE_AMD64; the thread handle is only passed through
                if StackWalk64(0x8664, proc_, std::ptr::null_mut(), &mut f, &mut c, read_mem as *const c_void, SymFunctionTableAccess64, SymGetModuleBase64, std::ptr::null()) == 0 || f.pc.offset == 0 {
                    break;
                }
                let a = f.pc.offset;
                let (m, base) = module_of(a);
                let mut s: SymbolInfo = std::mem::zeroed();
                s.size_of_struct = 88;
                s.max_name_len = 255;
                let mut disp = 0u64;
                let name = if SymFromAddr(proc_, a, &mut disp, &mut s) != 0 {
                    let n = (s.name_len as usize).min(255);
                    format!("{}+0x{disp:x}", String::from_utf8_lossy(&s.name[..n]))
                } else {
                    String::new()
                };
                lines.push(format!("  {m}+0x{:x} {name}", a - base));
            }
        }
        // its own file: the trace lock may be held by the stuck thread
        if let Some(t) = std::env::var_os("ARKIMAGE_TRACE") {
            use std::io::Write;
            let p = std::path::PathBuf::from(t).with_extension("stacks.txt");
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{} ms: main thread silent 3 s; stack:\n{}", now_ms(), lines.join("\n"));
            }
        }
    }
}
