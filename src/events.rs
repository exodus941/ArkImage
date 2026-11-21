//! A small permanent log of starts, exits, freezes, kills, takeovers and restarts.
//!
//! It is always on (the trace is opt-in), so a restart the user saw can be explained
//! afterwards. Lines go to `events.log` next to the config:
//! "<date time> <pid> <message>" (local time on Windows, UTC elsewhere). Past MAX_BYTES
//! the older half is cut.

use std::io::Write;

const MAX_BYTES: u64 = 256 * 1024;

pub fn path() -> std::path::PathBuf {
    crate::config::Config::dir().join("events.log")
}

/// Append one line, and copy it to the trace.
pub fn log(msg: &str) {
    crate::trace(&format!("event: {msg}"));
    let p = path();
    let _ = std::fs::create_dir_all(crate::config::Config::dir());
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > MAX_BYTES) {
        trim(&p);
    }
    let line = format!("{} {} {msg}\n", now_text(), std::process::id());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Keep the newer half, cut at a line start.
fn trim(p: &std::path::Path) {
    let Ok(b) = std::fs::read(p) else { return };
    let half = b.len() / 2;
    let start = b[half..].iter().position(|&c| c == b'\n').map(|i| half + i + 1).unwrap_or(b.len());
    let _ = std::fs::write(p, &b[start..]);
}

/// Windows: the local time the user's clock shows, "2026-11-21 06:36:53". Elsewhere UTC.
#[cfg(windows)]
fn now_text() -> String {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        y: u16,
        mo: u16,
        _dow: u16,
        d: u16,
        h: u16,
        mi: u16,
        s: u16,
        _ms: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.y, t.mo, t.d, t.h, t.mi, t.s)
}

#[cfg(not(windows))]
fn now_text() -> String {
    stamp(std::time::SystemTime::now())
}

/// UTC: "2026-11-21 02:21:13Z"
#[cfg_attr(windows, allow(dead_code))]
pub fn stamp(t: std::time::SystemTime) -> String {
    let s = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn stamp_is_utc_civil_time() {
        assert_eq!(stamp(UNIX_EPOCH), "1970-01-01 00:00:00Z");
        // 2026-11-21 02:21:13 UTC
        assert_eq!(stamp(UNIX_EPOCH + Duration::from_secs(1_795_227_673)), "2026-11-21 02:21:13Z");
        // a leap day
        assert_eq!(stamp(UNIX_EPOCH + Duration::from_secs(1_709_164_800)), "2024-02-29 00:00:00Z");
    }

    #[test]
    fn trim_keeps_whole_newer_lines() {
        let d = std::env::temp_dir().join(format!("ark-ev-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let p = d.join("e.log");
        std::fs::write(&p, "aaaa\nbbbb\ncccc\ndddd\n").unwrap();
        trim(&p);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "dddd\n");
        let _ = std::fs::remove_dir_all(&d);
    }
}
