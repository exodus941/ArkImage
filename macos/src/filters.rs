//! Display filter presets: a resize kernel plus an unsharp mask.
//! The built-in presets are read-only; the user can add copies and delete them.
//! `Config::filter` is an index into `all()`: built-ins first, then the user's.

/// Resize kernels, by index. 0 was "Interpolation setting" (the old Interpolation menu); it is
/// no longer offered, and a saved 0 loads as 4, Lanczos3, what that menu defaulted to.
/// 7-17 follow GPUResize's filter reference (render::kernel has the formulas); its
/// Triangle is this Bilinear, which also widens when shrinking.
pub const KERNELS: [&str; 22] = [
    "Interpolation setting",
    "None (Nearest)",
    "Bilinear / Triangle",
    "Bicubic (Keys)",
    "Lanczos3",
    "Lanczos4",
    "Mitchell",
    "Area (best shrink)",
    "Parzen (softest)",
    "Bell (soft)",
    "Cubic (Hermite)",
    "Notch (hides moiré)",
    "Simon (sharp)",
    "Rifman (sharpest cubic)",
    "Lanczos6 (widest)",
    "Kaiser",
    "Sinc (hard edge)",
    "Content-Aware (as Area)",
    "FSR 1 EASU (enlarge)",
    "EWA LanczosSharp",
    "Scale2x (pixel art)",
    "Eagle (pixel art)",
];

/// One line per kernel, by KERNELS index, for the dropdown's hover tooltip. Written from
/// GPUResize's filter reference (gpuresize.pages.dev/manual), in our own words.
pub const KERNEL_INFO: [&str; 22] = [
    "",
    "Copies the one nearest pixel, no blending. Crisper than every other kernel but blocky when enlarged; unlike Area, thin lines can vanish when shrunk. Keeps a pixel-art palette exact, as Scale2x and Eagle do, without their smoothing.",
    "Blends the 2 nearest pixels on each axis with a straight-line tent, widened when shrinking. Softer than Bicubic and Lanczos, with no halos; sharper than Bell and Parzen. GPUResize's Triangle is the same kernel.",
    "Keys cubic (Catmull-Rom, B 0, C 0.5): 4 pixels per axis, some of them weighted negative. Crisper than Bilinear and Mitchell, softer than Simon and Rifman; a slight bright or dark rim on hard edges.",
    "Sinc faded by a wider sinc, 3 pixels each side. Keeps more fine texture than the cubics; rings more than Mitchell. Lanczos4 and Lanczos6 reach farther; Kaiser fades the same sinc with a different curve.",
    "Lanczos with 4 pixels each side. Holds slightly finer patterns than Lanczos3; its ringing changes shape and spreads a little farther. Slower than Lanczos3, faster than Lanczos6.",
    "Mitchell-Netravali cubic (B 1/3, C 1/3), tuned by eye for few artefacts. A middle ground: sharper than Parzen and Bell, fewer halos than Bicubic and Lanczos. A good first choice for photos.",
    "Each screen pixel averages the image pixels under it, weighted by how much it covers. The cleanest shrink: no halos and fewer false stripes than Bilinear or the cubics. Enlarges as Bilinear.",
    "Cubic B-spline (B 1, C 0): every weight positive and wide. The softest kernel here, rounder than Bell; no halos at all, but small detail loses contrast. Never sharper than Bilinear.",
    "Quadratic B-spline, 1.5 pixels each side: three box averages stacked. Softer than Bilinear, sharper than Parzen; no halos. Easy to mistake for a Gaussian, but it ends at a fixed width.",
    "Hermite cubic (B 0, C 0): an ease-in, ease-out blend of the 2 nearest pixels. Smoother than Bilinear with the same reach and no halos; less crisp than Bicubic or Mitchell.",
    "Cubic with B 1.5, C -0.25: a broad, even blend made to quiet repeating patterns (screens, scan lines, fabric). Hides moiré better than Mitchell or Parzen, at the cost of being the second-softest here.",
    "Cubic with C 0.75, so its curve bends smoothly at x = 1. More bite than Bicubic (C 0.5), less than Rifman (C 1); halos grow in that order. From K. W. Simon's 1975 satellite-image work.",
    "Cubic with C 1, matching sinc's slope at x = 1: the strongest edge contrast of the cubics, with the brightest rims. Sharper than Simon and Bicubic; pair it with Clamp ringing to keep the bite without the halo.",
    "Lanczos with 6 pixels each side: the widest kernel here. Treats very fine patterns differently from Lanczos3 and Lanczos4, and its ringing reaches farthest from an edge. The slowest of the Lanczos set.",
    "Sinc faded by a Kaiser-Bessel window (radius 3, beta 4.12) instead of Lanczos3's sinc window. Similar detail to Lanczos3 with a differently shaped halo; worth trying when Lanczos3's rims bother you.",
    "Sinc cut off at 2 pixels each side with no fade. Narrower than Lanczos3, yet rings more, because the abrupt end leaves ripples. A deliberately hard edge; Clamp ringing limits it.",
    "In a viewer the image keeps its shape, so this resizes as Area: the same clean shrink, Bilinear when enlarging. (Seam carving would bend the picture to the window.)",
    "AMD FSR 1's edge-adaptive upscaler: reads 12 pixels, finds the edge direction and stretches a Lanczos-like kernel along it. Smoother diagonals than Lanczos3 when enlarging; shrinking uses Area.",
    "Elliptical weighted average with a jinc kernel (LanczosSharp blur): one round footprint, not rows then columns. Diagonals and curves get the same treatment as straight edges, unlike Lanczos3; can still ring.",
    "Pixel-art rule from AdvanceMAME: each enlarged pixel takes a neighbour's colour where the neighbours form a corner. Smooths stair-steps with no new colours, unlike Bilinear; gentler than Eagle. Shrinking uses Area.",
    "Pixel-art rule: a corner takes the colour its three neighbours agree on. Rounder and more joined than Scale2x, but it can erase one-pixel dots such as eyes. No new colours. Shrinking uses Area.",
];

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
    /// Clamp Ringing: no resized pixel brighter or darker than the pixels it reads.
    pub clamp: bool,
}

pub const AMOUNT_MAX: u16 = 400;
pub const RADIUS_MIN: u16 = 30;
pub const RADIUS_MAX: u16 = 300;
pub const THRESHOLD_MAX: u8 = 64;

fn p(name: &str, kernel: u8, amount: u16, radius: u16, threshold: u8) -> Preset {
    Preset { name: name.into(), kernel, amount, radius, threshold, clamp: false }
}

/// Read-only presets. The first three match Honeyview's filter menu (hotkeys U, I, S).
pub fn builtins() -> Vec<Preset> {
    vec![
        p("No Filter", 1, 0, 100, 0),
        p("Soften", 4, 0, 100, 0),
        p("Soften+Sharpen", 4, 60, 100, 0),
        p("Sharpen More", 4, 120, 100, 2),
        p("Sharpen Fine Detail", 4, 80, 60, 0),
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
    /// One ini value: name|kernel|amount|radius|threshold|clamp (clamp 0 or 1; an older
    /// line without it reads as 0)
    pub fn format(&self) -> String {
        format!("{}|{}|{}|{}|{}|{}", self.name.replace('|', "/"), self.kernel, self.amount, self.radius, self.threshold, self.clamp as u8)
    }

    pub fn parse(v: &str) -> Option<Self> {
        let mut it = v.split('|');
        let name = it.next()?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some(Self {
            name,
            kernel: match it.next()?.trim().parse::<u8>().ok()?.min(KERNELS.len() as u8 - 1) { 0 => 4, k => k },
            amount: it.next()?.trim().parse::<u16>().ok()?.min(AMOUNT_MAX),
            radius: it.next()?.trim().parse::<u16>().ok()?.clamp(RADIUS_MIN, RADIUS_MAX),
            threshold: it.next()?.trim().parse::<u8>().ok()?.min(THRESHOLD_MAX),
            clamp: it.next().is_some_and(|v| v.trim() == "1"),
        })
    }

    /// Changes whenever the rendered result would change; part of the scaled-image key.
    pub fn fingerprint(&self, interp: u8) -> u64 {
        let k = if self.kernel == 0 { 100 + interp as u64 } else { self.kernel as u64 };
        k | (self.amount as u64) << 8 | (self.radius as u64) << 24 | (self.threshold as u64) << 40 | (self.clamp as u64) << 48
    }

    /// The `render::resize` filter code for this preset.
    pub fn resize_code(&self, interp: u8) -> u8 {
        let k = if self.kernel == 0 { 2 + interp } else { self.kernel };
        let code = match k {
            1 => 0,       // nearest
            2 => 1,       // bilinear
            3 => 4,       // bicubic (Catmull-Rom)
            5 => 5,       // Lanczos4
            6 => 6,       // Mitchell-Netravali
            7..=16 => k,  // render::kernel's codes 7-16
            17 => 7,      // Content-Aware keeps the shape: Area
            18..=21 => k, // res2d: EASU, EWA, Scale2x, Eagle
            _ => 2,       // Lanczos3
        };
        // bit 7: Clamp Ringing (nearest has nothing to clamp)
        if self.clamp && code != 0 { code | 0x80 } else { code }
    }

    /// The kernel changes an enlargement (anything but Nearest and Bilinear), so an
    /// enlarged image goes through the resize, not per-pixel sampling.
    pub fn enlarges(&self, interp: u8) -> bool {
        !matches!(self.resize_code(interp) & 0x7F, 0 | 1)
    }

    pub fn smooth(&self) -> bool {
        self.kernel != 1
    }
}

/// A name for a new copy: "Custom 1", "Custom 2", ... not yet used.
pub fn new_name(user: &[Preset]) -> String {
    (1..).map(|n| format!("Custom {n}")).find(|s| !user.iter().any(|p| &p.name == s)).unwrap()
}
