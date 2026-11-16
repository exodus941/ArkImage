//! Display filter presets: a resize kernel plus an unsharp mask.
//! The built-in presets are read-only; the user can add copies and delete them.
//! `Config::filter` is an index into `all()`: built-ins first, then the user's.

/// Resize kernels, by index. 0 uses the Interpolation setting.
pub const KERNELS: [&str; 7] = ["Interpolation setting", "None (Nearest)", "Bilinear", "Bicubic", "Lanczos3", "Lanczos4 (sharpest)", "Mitchell (no halos)"];

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    /// Index into KERNELS.
    pub kernel: u8,
    /// Sharpen strength, in hundredths (60 = 0.60). 0 = no sharpening.
    pub amount: u16,
    /// Blur radius (Gaussian sigma) of the mask, in hundredths of a pixel.
    pub radius: u16,
    /// Differences at or below this (0-255) are left alone, so flat areas keep no noise.
    pub threshold: u8,
}

pub const AMOUNT_MAX: u16 = 400;
pub const RADIUS_MIN: u16 = 30;
pub const RADIUS_MAX: u16 = 300;
pub const THRESHOLD_MAX: u8 = 64;

fn p(name: &str, kernel: u8, amount: u16, radius: u16, threshold: u8) -> Preset {
    Preset { name: name.into(), kernel, amount, radius, threshold }
}

/// Read-only presets. The first three match Honeyview's filter menu (hotkeys U, I, S).
pub fn builtins() -> Vec<Preset> {
    vec![
        p("No Filter", 1, 0, 100, 0),
        p("Soften", 0, 0, 100, 0),
        p("Soften+Sharpen", 0, 60, 100, 0),
        p("Sharpen More", 0, 120, 100, 2),
        p("Sharpen Fine Detail", 0, 80, 60, 0),
    ]
}

pub fn builtin_count() -> usize {
    builtins().len()
}

pub fn all(user: &[Preset]) -> Vec<Preset> {
    let mut v = builtins();
    v.extend(user.iter().cloned());
    v
}

/// Preset at index `i`; an index past the end falls back to Soften+Sharpen.
pub fn get(user: &[Preset], i: u8) -> Preset {
    all(user).into_iter().nth(i as usize).unwrap_or_else(|| builtins().swap_remove(2))
}

pub fn is_builtin(i: u8) -> bool {
    (i as usize) < builtin_count()
}

impl Preset {
    /// One ini value: name|kernel|amount|radius|threshold
    pub fn format(&self) -> String {
        format!("{}|{}|{}|{}|{}", self.name.replace('|', "/"), self.kernel, self.amount, self.radius, self.threshold)
    }

    pub fn parse(v: &str) -> Option<Self> {
        let mut it = v.split('|');
        let name = it.next()?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some(Self {
            name,
            kernel: it.next()?.trim().parse::<u8>().ok()?.min(KERNELS.len() as u8 - 1),
            amount: it.next()?.trim().parse::<u16>().ok()?.min(AMOUNT_MAX),
            radius: it.next()?.trim().parse::<u16>().ok()?.clamp(RADIUS_MIN, RADIUS_MAX),
            threshold: it.next()?.trim().parse::<u8>().ok()?.min(THRESHOLD_MAX),
        })
    }

    /// Changes whenever the rendered result would change; part of the scaled-image key.
    pub fn fingerprint(&self, interp: u8) -> u64 {
        let k = if self.kernel == 0 { 10 + interp as u64 } else { self.kernel as u64 };
        k | (self.amount as u64) << 8 | (self.radius as u64) << 24 | (self.threshold as u64) << 40
    }

    /// The `render::resize` filter code for this preset.
    pub fn resize_code(&self, interp: u8) -> u8 {
        let k = if self.kernel == 0 { 2 + interp } else { self.kernel };
        match k {
            1 => 0,     // nearest
            2 => 1,     // bilinear
            3 => 4,     // bicubic (Catmull-Rom)
            5 => 5,     // Lanczos4
            6 => 6,     // Mitchell-Netravali
            _ => 2,     // Lanczos3
        }
    }

    pub fn smooth(&self) -> bool {
        self.kernel != 1
    }
}

/// A name for a new copy: "Custom 1", "Custom 2", ... not yet used.
pub fn new_name(user: &[Preset]) -> String {
    (1..).map(|n| format!("Custom {n}")).find(|s| !user.iter().any(|p| &p.name == s)).unwrap()
}
