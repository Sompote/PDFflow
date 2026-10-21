//! Image › Adjustments on picture pixels, ported from PhotoCraft
//! (github.com/storytold/photocraft, `crates/compose/src/adjust.rs`, `crates/algo/src/tone.rs`,
//! `crates/algo/src/tone/shadows_highlights.rs` and `crates/engine/src/edit_cmds.rs`,
//! MIT OR Apache-2.0). PhotoCraft's formulas are measured against Photoshop; here they are
//! reduced to what a PDF picture needs: 8-bit sRGB, straight RGBA in 0..=1, no layers.

use std::sync::Arc;

/// Which adjustment; the order and grouping follow Photoshop's Image › Adjustments menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AdjKind {
    BrightnessContrast,
    Levels,
    Curves,
    Exposure,
    Vibrance,
    HueSaturation,
    ColorBalance,
    BlackWhite,
    PhotoFilter,
    ChannelMixer,
    ColorLookup,
    Invert,
    Posterize,
    Threshold,
    GradientMap,
    SelectiveColor,
    ShadowsHighlights,
    HdrToning,
    Desaturate,
    MatchColor,
    ReplaceColor,
    Equalize,
    AutoTone,
    AutoContrast,
    AutoColor,
}

/// Modifier keys of a menu shortcut (⌥, ⇧, ⌘) and its letter.
pub type Accel = (bool, bool, char);

/// The Adjustments submenu: `None` is a separator.
pub const MENU: [Option<(AdjKind, &str, Option<Accel>)>; 25] = [
    Some((AdjKind::BrightnessContrast, "Brightness/Contrast…", None)),
    Some((AdjKind::Levels, "Levels…", Some((false, false, 'L')))),
    Some((AdjKind::Curves, "Curves…", Some((false, false, 'M')))),
    Some((AdjKind::Exposure, "Exposure…", None)),
    None,
    Some((AdjKind::Vibrance, "Vibrance…", None)),
    Some((AdjKind::HueSaturation, "Hue/Saturation…", Some((false, false, 'U')))),
    Some((AdjKind::ColorBalance, "Color Balance…", Some((false, false, 'B')))),
    Some((AdjKind::BlackWhite, "Black & White…", Some((true, true, 'B')))),
    Some((AdjKind::PhotoFilter, "Photo Filter…", None)),
    Some((AdjKind::ChannelMixer, "Channel Mixer…", None)),
    Some((AdjKind::ColorLookup, "Color Lookup…", None)),
    None,
    Some((AdjKind::Invert, "Invert", Some((false, false, 'I')))),
    Some((AdjKind::Posterize, "Posterize…", None)),
    Some((AdjKind::Threshold, "Threshold…", None)),
    Some((AdjKind::GradientMap, "Gradient Map…", None)),
    Some((AdjKind::SelectiveColor, "Selective Color…", None)),
    None,
    Some((AdjKind::ShadowsHighlights, "Shadows/Highlights…", None)),
    Some((AdjKind::HdrToning, "HDR Toning…", None)),
    None,
    Some((AdjKind::Desaturate, "Desaturate", Some((false, true, 'U')))),
    Some((AdjKind::MatchColor, "Match Color…", None)),
    Some((AdjKind::ReplaceColor, "Replace Color…", None)),
];

/// Image menu commands that apply at once, after the Adjustments submenu.
pub const AUTO: [(AdjKind, &str, Accel); 3] = [
    (AdjKind::AutoTone, "Auto Tone", (false, true, 'L')),
    (AdjKind::AutoContrast, "Auto Contrast", (true, true, 'L')),
    (AdjKind::AutoColor, "Auto Color", (false, true, 'B')),
];

impl AdjKind {
    /// Commands without a dialog.
    pub fn is_immediate(self) -> bool {
        matches!(self, AdjKind::Invert | AdjKind::Desaturate | AdjKind::Equalize | AdjKind::AutoTone | AdjKind::AutoContrast | AdjKind::AutoColor)
    }

    pub fn title(self) -> &'static str {
        if self == AdjKind::Equalize {
            return "Equalize";
        }
        MENU.iter().flatten().map(|m| (m.0, m.1)).chain(AUTO.iter().map(|a| (a.0, a.1))).find(|m| m.0 == self).map_or("Adjustment", |m| m.1.trim_end_matches('…'))
    }
}

/// One channel of Levels, all values in 0..=1 except the midtone gamma (0.1..=9.99).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LevelsCh {
    pub in_black: f32,
    pub in_white: f32,
    pub gamma: f32,
    pub out_black: f32,
    pub out_white: f32,
}

impl Default for LevelsCh {
    fn default() -> Self {
        LevelsCh { in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0 }
    }
}

/// Shadows/Highlights (Photoshop's dialog, percentages as 0..=100, radii in pixels).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ShadowsHighlights {
    pub shadow_amount: f32,
    pub shadow_tone: f32,
    pub shadow_radius: f32,
    pub highlight_amount: f32,
    pub highlight_tone: f32,
    pub highlight_radius: f32,
    /// Colour (saturation) correction in -100..=100.
    pub color: f32,
    /// Midtone contrast in -100..=100.
    pub midtone: f32,
    /// Percent of pixels clipped to black / white (0..=50).
    pub black_clip: f32,
    pub white_clip: f32,
}

impl Default for ShadowsHighlights {
    fn default() -> Self {
        ShadowsHighlights {
            shadow_amount: 35.0,
            shadow_tone: 50.0,
            shadow_radius: 30.0,
            highlight_amount: 0.0,
            highlight_tone: 50.0,
            highlight_radius: 30.0,
            color: 20.0,
            midtone: 0.0,
            black_clip: 0.01,
            white_clip: 0.01,
        }
    }
}

/// HDR Toning, Local Adaptation method.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HdrToning {
    /// Edge glow radius in pixels (1..=500).
    pub radius: f32,
    /// Edge glow strength 0.1..=4.
    pub strength: f32,
    /// 0.1..=2 (1 is neutral; lower gives more contrast).
    pub gamma: f32,
    /// -5..=5 stops.
    pub exposure: f32,
    /// -100..=300 %.
    pub detail: f32,
    pub shadow: f32,
    pub highlight: f32,
    pub vibrance: f32,
    pub saturation: f32,
}

impl Default for HdrToning {
    fn default() -> Self {
        HdrToning { radius: 30.0, strength: 0.5, gamma: 1.0, exposure: 0.0, detail: 30.0, shadow: 0.0, highlight: 0.0, vibrance: 0.0, saturation: 20.0 }
    }
}

/// A 3D colour lookup table read from a `.cube` file (`n`³ RGB triplets, red fastest).
#[derive(PartialEq, Debug)]
pub struct Lut3d {
    pub n: usize,
    pub data: Vec<f32>,
}

/// An adjustment with its settings.
#[derive(Clone, PartialEq, Debug)]
pub enum Adj {
    BrightnessContrast { brightness: f32, contrast: f32, legacy: bool },
    /// Composite RGB first, then red, green and blue.
    Levels { ch: [LevelsCh; 4] },
    /// Points (input, output) in 0..=1 for composite RGB, red, green and blue.
    Curves { ch: [Vec<[f32; 2]>; 4] },
    Exposure { exposure: f32, offset: f32, gamma: f32 },
    Vibrance { vibrance: f32, saturation: f32 },
    HueSaturation { hue: f32, saturation: f32, lightness: f32, colorize: bool },
    /// Shadows, midtones and highlights: cyan–red, magenta–green, yellow–blue in -100..=100.
    ColorBalance { tones: [[f32; 3]; 3], preserve: bool },
    /// Reds, yellows, greens, cyans, blues and magentas in percent.
    BlackWhite { weights: [f32; 6], tint: Option<[f32; 3]> },
    PhotoFilter { color: [f32; 3], density: f32, preserve: bool },
    /// Output red, green and blue: source red, green, blue and a constant, as fractions.
    ChannelMixer { matrix: [[f32; 4]; 3], mono: bool },
    ColorLookup { lut: Option<Arc<Lut3d>>, name: String },
    Invert,
    Posterize { levels: u32 },
    /// Level 1..=255.
    Threshold { level: f32 },
    GradientMap { from: [f32; 3], to: [f32; 3], reverse: bool },
    /// Reds, yellows, greens, cyans, blues, magentas, whites, neutrals, blacks: C, M, Y, K in %.
    SelectiveColor { relative: bool, adj: [[f32; 4]; 9] },
    ShadowsHighlights(ShadowsHighlights),
    HdrToning(HdrToning),
    Desaturate,
    MatchColor { luminance: f32, intensity: f32, fade: f32, neutralize: bool },
    ReplaceColor { color: [f32; 3], fuzziness: f32, hue: f32, saturation: f32, lightness: f32 },
    Equalize,
    AutoTone,
    AutoContrast,
    AutoColor,
}

/// Photo Filter presets (Photoshop's names and colours).
pub const PHOTO_FILTERS: [(&str, [u8; 3]); 20] = [
    ("Warming Filter (85)", [236, 138, 0]),
    ("Warming Filter (LBA)", [250, 150, 0]),
    ("Warming Filter (81)", [235, 177, 19]),
    ("Cooling Filter (80)", [0, 109, 255]),
    ("Cooling Filter (LBB)", [0, 93, 255]),
    ("Cooling Filter (82)", [0, 181, 255]),
    ("Red", [234, 26, 26]),
    ("Orange", [243, 132, 23]),
    ("Yellow", [249, 227, 28]),
    ("Green", [25, 201, 25]),
    ("Cyan", [29, 203, 234]),
    ("Blue", [29, 53, 234]),
    ("Violet", [155, 29, 234]),
    ("Magenta", [227, 24, 227]),
    ("Sepia", [172, 122, 51]),
    ("Deep Red", [255, 0, 0]),
    ("Deep Blue", [0, 34, 205]),
    ("Deep Emerald", [0, 140, 0]),
    ("Deep Yellow", [255, 213, 0]),
    ("Underwater", [0, 193, 177]),
];

pub fn rgb3(c: [u8; 3]) -> [f32; 3] {
    c.map(|v| v as f32 / 255.0)
}

impl Adj {
    /// Photoshop's defaults (all of them leave the picture unchanged, except the commands
    /// without a dialog and Shadows/Highlights, which opens with a 35 % shadow lift).
    pub fn new(kind: AdjKind) -> Adj {
        match kind {
            AdjKind::BrightnessContrast => Adj::BrightnessContrast { brightness: 0.0, contrast: 0.0, legacy: false },
            AdjKind::Levels => Adj::Levels { ch: [LevelsCh::default(); 4] },
            AdjKind::Curves => Adj::Curves { ch: std::array::from_fn(|_| vec![[0.0, 0.0], [1.0, 1.0]]) },
            AdjKind::Exposure => Adj::Exposure { exposure: 0.0, offset: 0.0, gamma: 1.0 },
            AdjKind::Vibrance => Adj::Vibrance { vibrance: 0.0, saturation: 0.0 },
            AdjKind::HueSaturation => Adj::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false },
            AdjKind::ColorBalance => Adj::ColorBalance { tones: [[0.0; 3]; 3], preserve: true },
            AdjKind::BlackWhite => Adj::BlackWhite { weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0], tint: None },
            AdjKind::PhotoFilter => Adj::PhotoFilter { color: rgb3(PHOTO_FILTERS[0].1), density: 0.25, preserve: true },
            AdjKind::ChannelMixer => Adj::ChannelMixer { matrix: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], mono: false },
            AdjKind::ColorLookup => Adj::ColorLookup { lut: None, name: String::new() },
            AdjKind::Invert => Adj::Invert,
            AdjKind::Posterize => Adj::Posterize { levels: 4 },
            AdjKind::Threshold => Adj::Threshold { level: 128.0 },
            AdjKind::GradientMap => Adj::GradientMap { from: [0.0; 3], to: [1.0; 3], reverse: false },
            AdjKind::SelectiveColor => Adj::SelectiveColor { relative: true, adj: [[0.0; 4]; 9] },
            AdjKind::ShadowsHighlights => Adj::ShadowsHighlights(ShadowsHighlights::default()),
            AdjKind::HdrToning => Adj::HdrToning(HdrToning::default()),
            AdjKind::Desaturate => Adj::Desaturate,
            AdjKind::MatchColor => Adj::MatchColor { luminance: 100.0, intensity: 100.0, fade: 0.0, neutralize: false },
            AdjKind::ReplaceColor => Adj::ReplaceColor { color: [1.0, 0.0, 0.0], fuzziness: 40.0, hue: 0.0, saturation: 0.0, lightness: 0.0 },
            AdjKind::Equalize => Adj::Equalize,
            AdjKind::AutoTone => Adj::AutoTone,
            AdjKind::AutoContrast => Adj::AutoContrast,
            AdjKind::AutoColor => Adj::AutoColor,
        }
    }
}

// ------------------------------------------------------------------------------ applying

/// Applies `adj` to `px` (`w × h`, straight RGBA in 0..=1). `inside` marks the pixels the
/// statistics of Auto, Equalize and Match Color are taken from (the selection); the caller
/// keeps only the inside pixels of the result.
pub fn apply(adj: &Adj, px: &mut [[f32; 4]], w: usize, h: usize, inside: &[bool]) {
    match adj {
        Adj::Invert => map_rgb(px, |c| c.map(|v| 1.0 - v)),
        Adj::Desaturate => map_rgb(px, |c| [(c[0].max(c[1]).max(c[2]) + c[0].min(c[1]).min(c[2])) / 2.0; 3]),
        Adj::Threshold { level } => map_rgb(px, |c| [if threshold_white(c, *level) { 1.0 } else { 0.0 }; 3]),
        Adj::Posterize { levels } => map_rgb(px, |c| c.map(|v| posterize(v, *levels))),
        Adj::BrightnessContrast { brightness, contrast, legacy: true } => {
            let f = legacy_brightness_contrast(*brightness, *contrast);
            map_rgb(px, |c| c.map(&f))
        }
        Adj::BrightnessContrast { brightness, contrast, .. } => {
            let table = modern_lut(*brightness, *contrast);
            map_rgb(px, |c| c.map(|v| lut(&table, v)))
        }
        Adj::Exposure { exposure, offset, gamma } => {
            let m = 2f32.powf(*exposure);
            let g = gamma.max(0.01);
            let table: Vec<f32> = (0..LUT_SIZE)
                .map(|k| {
                    let lin = (srgb_to_linear(k as f32 / (LUT_SIZE - 1) as f32) * m + offset).max(0.0);
                    let lin = if g == 1.0 { lin } else { lin.powf(1.0 / g) };
                    linear_to_srgb(lin).clamp(0.0, 1.0)
                })
                .collect();
            map_rgb(px, |c| c.map(|v| lut(&table, v)))
        }
        Adj::Levels { ch } => {
            let luts = levels_luts(ch);
            map_rgb(px, |c| std::array::from_fn(|i| lut(&luts[i], c[i])))
        }
        Adj::Curves { ch } => {
            let master = curve_lut(&ch[0]);
            let luts: [Vec<f32>; 3] = std::array::from_fn(|i| curve_lut(&ch[i + 1]).iter().map(|&v| lut(&master, v)).collect());
            map_rgb(px, |c| std::array::from_fn(|i| lut(&luts[i], c[i])))
        }
        Adj::HueSaturation { hue, saturation, lightness, colorize } => {
            let (s, l) = ((*saturation / 100.0).clamp(-1.0, 1.0), (*lightness / 100.0).clamp(-1.0, 1.0));
            map_rgb(px, |c| hue_saturation(c, *hue, s, l, *colorize))
        }
        Adj::Vibrance { vibrance, saturation } => {
            let (v, s) = (*vibrance / 100.0, *saturation / 100.0);
            map_rgb(px, |c| vibrance_px(c, v, s))
        }
        Adj::ColorBalance { tones, preserve } => {
            let luts: [Vec<f32>; 3] = std::array::from_fn(|ch| (0..LUT_SIZE).map(|k| color_balance_channel(*tones, *preserve, ch, k as f32 / (LUT_SIZE - 1) as f32)).collect());
            map_rgb(px, |c| std::array::from_fn(|i| lut(&luts[i], c[i])))
        }
        Adj::BlackWhite { weights, tint } => map_rgb(px, |c| {
            let g = black_white_gray(c, weights);
            match tint {
                Some(t) => set_lum(*t, g).map(|v| v.clamp(0.0, 1.0)),
                None => [g; 3],
            }
        }),
        Adj::PhotoFilter { color, density, preserve } => {
            let m = photo_filter_matrix(*color, *density);
            map_rgb(px, |c| {
                let f = mat3_mul(&m, c.map(srgb_to_linear)).map(linear_to_srgb);
                let f = if *preserve { set_lum(f, lum(c)) } else { f };
                f.map(|v| v.clamp(0.0, 1.0))
            })
        }
        Adj::ChannelMixer { matrix, mono } => map_rgb(px, |c| {
            let mix = |row: &[f32; 4]| (row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3]).clamp(0.0, 1.0);
            if *mono { [mix(&matrix[0]); 3] } else { [mix(&matrix[0]), mix(&matrix[1]), mix(&matrix[2])] }
        }),
        Adj::ColorLookup { lut: Some(t), .. } => map_rgb(px, |c| lut3d_sample(&t.data, t.n, c).map(|v| v.clamp(0.0, 1.0))),
        Adj::ColorLookup { lut: None, .. } => {}
        Adj::GradientMap { from, to, reverse } => map_rgb(px, |c| {
            let t = lum(c).clamp(0.0, 1.0);
            let t = if *reverse { 1.0 - t } else { t };
            std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
        }),
        Adj::SelectiveColor { relative, adj } => map_rgb(px, |c| selective_color(c, *relative, adj)),
        Adj::ShadowsHighlights(p) => shadows_highlights(px, w, h, p),
        Adj::HdrToning(p) => hdr_toning(px, w, h, p),
        Adj::MatchColor { luminance, intensity, fade, neutralize } => {
            if let Some(t) = lab_stats(px, inside) {
                match_color(px, &t, *luminance, *intensity, *fade, *neutralize);
            }
        }
        Adj::ReplaceColor { color, fuzziness, hue, saturation, lightness } => {
            let mask = color_range(px, *color, *fuzziness);
            replace_color_masked(px, &mask, *hue, *saturation, *lightness);
        }
        Adj::Equalize => equalize(px, inside),
        Adj::AutoTone | Adj::AutoContrast | Adj::AutoColor => {
            let ch = auto_levels(adj, px, inside);
            let luts = levels_luts(&ch);
            map_rgb(px, |c| std::array::from_fn(|i| lut(&luts[i], c[i])));
        }
    }
}

const LUT_SIZE: usize = 4096;

fn map_rgb(px: &mut [[f32; 4]], f: impl Fn([f32; 3]) -> [f32; 3]) {
    for p in px {
        if p[3] <= 0.0 {
            continue;
        }
        let r = f([p[0], p[1], p[2]]);
        p[..3].copy_from_slice(&r);
    }
}

#[inline]
fn lut(table: &[f32], v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0) * (table.len() - 1) as f32;
    let i = x.floor() as usize;
    let j = (i + 1).min(table.len() - 1);
    let f = x - i as f32;
    table[i] * (1.0 - f) + table[j] * f
}

// ------------------------------------------------------------------------------ colour helpers

pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 }
}

const D50: [f32; 3] = [0.964_22, 1.0, 0.825_21];
const SRGB_TO_XYZ_D50: [[f32; 3]; 3] = [[0.436_074, 0.385_064, 0.143_080], [0.222_504, 0.716_878, 0.060_618], [0.013_932, 0.097_104, 0.714_173]];
const XYZ_D50_TO_SRGB: [[f32; 3]; 3] = [[3.133_856, -1.616_867, -0.490_615], [-0.978_768, 1.916_142, 0.033_454], [0.071_945, -0.228_991, 1.405_243]];

fn mat3_mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

/// Encoded sRGB → CIE L*a*b* (D50).
fn srgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    let xyz = mat3_mul(&SRGB_TO_XYZ_D50, rgb.map(srgb_to_linear));
    let f = |t: f32| if t > (6.0f32 / 29.0).powi(3) { t.cbrt() } else { t / (3.0 * (6.0f32 / 29.0).powi(2)) + 4.0 / 29.0 };
    let (fx, fy, fz) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_srgb(lab: [f32; 3]) -> [f32; 3] {
    let [l, a, b] = lab;
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let finv = |t: f32| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    let xyz = [finv(fx) * D50[0], finv(fy) * D50[1], finv(fz) * D50[2]];
    mat3_mul(&XYZ_D50_TO_SRGB, xyz).map(|lin| linear_to_srgb(lin.clamp(0.0, 1.0)))
}

/// The blend modes' luminosity (W3C weights, as Photoshop uses for Gradient Map and Threshold).
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// ClipColor: moves `c` towards its grey until every channel is within 0..=1.
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        let d = l - n;
        for v in &mut out {
            *v = if l > 0.0 && d > 0.0 { l + (*v - l) * l / d } else { l };
        }
    }
    if x > 1.0 {
        let d = x - l;
        for v in &mut out {
            *v = if l < 1.0 && d > 0.0 { l + (*v - l) * (1.0 - l) / d } else { l };
        }
    }
    out
}

/// `c` with its luminosity moved to `l` (the Luminosity blend mode's SetLum + ClipColor).
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

pub fn rgb_to_hsl(c: [f32; 3]) -> (f32, f32, f32) {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-7 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    (h / 6.0, s, l)
}

pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [l; 3];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

// ------------------------------------------------------------------------------ tone

/// Legacy Brightness/Contrast: contrast scales around 128/255, with brightness added before it
/// when contrast is raised and after it when contrast is lowered.
fn legacy_brightness_contrast(brightness: f32, contrast: f32) -> impl Fn(f32) -> f32 {
    const PIVOT: f32 = 128.0 / 255.0;
    let b = brightness / 255.0;
    let c = contrast.clamp(-100.0, 99.0);
    let k = if c >= 0.0 { 1.0 / (1.0 - c / 100.0) } else { 1.0 + c / 100.0 };
    let (pre, post) = if c >= 0.0 { (b, 0.0) } else { (0.0, b) };
    move |v: f32| ((v + pre - PIVOT) * k + PIVOT + post).clamp(0.0, 1.0)
}

/// Output level of the modern Brightness curve's knee.
const BRIGHTNESS_KNEE: f32 = 126.0 / 255.0;

/// The modern (CS3+) Brightness curve raised by `b` (0..=150): a line of slope 1.375^(b/50) up to
/// a knee at 126/255, then a cubic Hermite to (1, 1) whose end slope is 1 / (1 + 0.084 b), or a
/// power roll-off where no such cubic stays below white. Lowering inverts the raised curve.
struct Brightness {
    s: f32,
    e: f32,
    x0: f32,
    alpha: f32,
    hermite: bool,
}

impl Brightness {
    fn new(b: f32) -> Self {
        let b = b.abs();
        let s = 1.375f32.powf(b / 50.0);
        let e = 1.0 / (1.0 + 0.084 * b);
        let x0 = (BRIGHTNESS_KNEE / s).min(1.0);
        let secant = (1.0 - BRIGHTNESS_KNEE) / (1.0 - x0).max(1e-6);
        let (alpha, beta) = (s / secant, e / secant);
        Brightness { s, e, x0, alpha, hermite: alpha * alpha + beta * beta <= 9.0 }
    }

    fn raised(&self, x: f32) -> f32 {
        let Brightness { s, e, x0, alpha, hermite } = *self;
        let y0 = BRIGHTNESS_KNEE;
        if x <= x0 {
            return y0.min(s * x);
        }
        let h = (1.0 - x0).max(1e-6);
        let t = ((x - x0) / h).clamp(0.0, 1.0);
        let y = if hermite {
            let (t2, t3) = (t * t, t * t * t);
            (2.0 * t3 - 3.0 * t2 + 1.0) * y0 + (t3 - 2.0 * t2 + t) * h * s + (-2.0 * t3 + 3.0 * t2) + (t3 - t2) * h * e
        } else {
            1.0 - (1.0 - y0) * (1.0 - t).powf(alpha)
        };
        y.clamp(0.0, 1.0)
    }
}

/// Modern Brightness/Contrast as one lookup table: brightness, then contrast.
pub fn modern_lut(brightness: f32, contrast: f32) -> Vec<f32> {
    let step = |k: usize, n: usize| k as f32 / (n - 1) as f32;
    let bright: Vec<f32> = if brightness == 0.0 || !brightness.is_finite() {
        (0..LUT_SIZE).map(|k| step(k, LUT_SIZE)).collect()
    } else if brightness > 0.0 {
        let curve = Brightness::new(brightness);
        (0..LUT_SIZE).map(|k| curve.raised(step(k, LUT_SIZE))).collect()
    } else {
        // Lowered: invert the raised curve, sampled four times finer, in one monotone pass.
        let curve = Brightness::new(-brightness);
        let fine = 4 * LUT_SIZE;
        let ys: Vec<f32> = (0..fine).map(|j| curve.raised(step(j, fine))).collect();
        let mut j = 0;
        (0..LUT_SIZE)
            .map(|k| {
                let y = step(k, LUT_SIZE);
                while j + 2 < fine && ys[j + 1] < y {
                    j += 1;
                }
                let (y0, y1) = (ys[j], ys[j + 1]);
                let f = if y1 > y0 { ((y - y0) / (y1 - y0)).clamp(0.0, 1.0) } else { 0.0 };
                (step(j, fine) + f / (fine - 1) as f32).clamp(0.0, 1.0)
            })
            .collect()
    };
    bright.into_iter().map(|b| modern_contrast(b, contrast).clamp(0.0, 1.0)).collect()
}

/// Modern Contrast (-50..=100): a symmetric cubic-Hermite S-curve pivoting at 0.5, endpoint
/// slope 1 − c/128 and centre slope 1 + c/128.
fn modern_contrast(v: f32, contrast: f32) -> f32 {
    if contrast == 0.0 {
        return v;
    }
    let k = contrast / 128.0;
    let (end, mid) = (1.0 - k, 1.0 + k);
    let half = |t: f32| end * 0.5 * (t * t * t - 2.0 * t * t + t) + (-2.0 * t * t * t + 3.0 * t * t) * 0.5 + mid * 0.5 * (t * t * t - t * t);
    let v = v.clamp(0.0, 1.0);
    if v <= 0.5 { half(v / 0.5) } else { 1.0 - half((1.0 - v) / 0.5) }
}

/// Photoshop Posterize: `n` equal input bins over 0..=255, output levels ⌊k · 255 / (n − 1)⌋.
pub fn posterize(v: f32, levels: u32) -> f32 {
    let n = levels.clamp(2, 255) as f32;
    let x = (v.clamp(0.0, 1.0) * 255.0).round();
    let bin = (x * n / 256.0).floor().min(n - 1.0);
    (bin * 255.0 / (n - 1.0)).floor() / 255.0
}

/// Threshold: white when the luminosity 0.30 R + 0.59 G + 0.11 B, rounded to a level with halves
/// down, reaches `level`.
pub fn threshold_white(c: [f32; 3], level: f32) -> bool {
    let l = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round());
    let sum = 30.0 * l[0] + 59.0 * l[1] + 11.0 * l[2];
    ((sum - 50.0) / 100.0).ceil() >= level.round()
}

/// One Levels channel on whole 8-bit levels: input stretch, midtone gamma, output range, each
/// rounded half up as Photoshop does.
pub fn levels(ch: &LevelsCh, v: f32) -> f32 {
    let q = 255.0;
    let range = (ch.in_white - ch.in_black).max(1e-6);
    let whole = |x: f32| (x * q + 0.5).floor() / q;
    let v = (v * q).round() / q;
    let u = (((v - ch.in_black) / range).clamp(0.0, 1.0) * q + 0.5 + 1e-3).floor() / q;
    let t = whole(levels_gamma(u, ch.gamma.max(0.01)));
    whole(ch.out_black + t * (ch.out_white - ch.out_black))
}

/// Levels' midtone gamma: u^(1/g), except that above 1 Photoshop starts along a toe a·u − b·u^2.2
/// of slope 2^g that joins the power curve with the same value and slope.
fn levels_gamma(u: f32, g: f32) -> f32 {
    let c = 1.0 / g;
    if g <= 1.0 {
        return u.powf(c);
    }
    let (a, q) = (2f32.powf(g), 2.2f32);
    let u1 = ((1.0 - c / q) / (a * (1.0 - 1.0 / q))).powf(1.0 / (1.0 - c));
    if u >= u1 || u1 <= 0.0 {
        return u.powf(c);
    }
    let b = (a * u1 - u1.powf(c)) / u1.powf(q);
    a * u - b * u.powf(q)
}

/// Per-channel tables of Levels: the channel's own levels, then the composite's.
fn levels_luts(ch: &[LevelsCh; 4]) -> [Vec<f32>; 3] {
    std::array::from_fn(|i| (0..256).map(|k| levels(&ch[0], levels(&ch[i + 1], k as f32 / 255.0))).collect())
}

/// Photoshop curve: a natural cubic spline through the points, constant outside the first and
/// last point, clamped to 0..=1, sampled into a table.
pub fn curve_lut(points: &[[f32; 2]]) -> Vec<f32> {
    let mut pts: Vec<(f32, f32)> = points.iter().map(|p| (p[0], p[1])).collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    if pts.len() < 2 {
        return (0..LUT_SIZE).map(|i| i as f32 / (LUT_SIZE - 1) as f32).collect();
    }
    let n = pts.len();
    let x: Vec<f64> = pts.iter().map(|p| f64::from(p.0)).collect();
    let y: Vec<f64> = pts.iter().map(|p| f64::from(p.1)).collect();
    // Second derivatives of the natural spline (tridiagonal solve).
    let mut m2 = vec![0.0f64; n];
    if n > 2 {
        let mut c = vec![0.0f64; n];
        let mut d = vec![0.0f64; n];
        for i in 1..n - 1 {
            let (h0, h1) = (x[i] - x[i - 1], x[i + 1] - x[i]);
            let (a, b, cc) = (h0 / 6.0, (h0 + h1) / 3.0, h1 / 6.0);
            let r = (y[i + 1] - y[i]) / h1 - (y[i] - y[i - 1]) / h0;
            let denom = b - a * c[i - 1];
            c[i] = cc / denom;
            d[i] = (r - a * d[i - 1]) / denom;
        }
        for i in (1..n - 1).rev() {
            m2[i] = d[i] - c[i] * m2[i + 1];
        }
    }
    (0..LUT_SIZE)
        .map(|k| {
            let xv = k as f32 / (LUT_SIZE - 1) as f32;
            if xv <= pts[0].0 {
                return pts[0].1.clamp(0.0, 1.0);
            }
            if xv >= pts[n - 1].0 {
                return pts[n - 1].1.clamp(0.0, 1.0);
            }
            let i = pts.windows(2).position(|w| xv <= w[1].0).unwrap_or(n - 2);
            let h = x[i + 1] - x[i];
            let t = f64::from(xv);
            let (a, b) = ((x[i + 1] - t) / h, (t - x[i]) / h);
            let v = a * y[i] + b * y[i + 1] + ((a * a * a - a) * m2[i] + (b * b * b - b) * m2[i + 1]) * h * h / 6.0;
            (v as f32).clamp(0.0, 1.0)
        })
        .collect()
}

/// The curve's value at `x` (for drawing it in the Curves dialog).
pub fn curve_at(table: &[f32], x: f32) -> f32 {
    lut(table, x)
}

// ------------------------------------------------------------------------------ colour

/// Hue/Saturation: hue shift in degrees, saturation and lightness in -1..=1. Lightness first, then
/// the hue turns and the saturation scales; Colorize sets hue and saturation instead.
pub fn hue_saturation(c: [f32; 3], hue: f32, s: f32, l: f32, colorize: bool) -> [f32; 3] {
    let lighten = |v: f32| if l > 0.0 { v + (1.0 - v) * l } else { v * (1.0 + l) };
    if colorize {
        let (_, _, ll) = rgb_to_hsl(c);
        return hsl_to_rgb(hue.rem_euclid(360.0) / 360.0, s.abs().min(1.0), lighten(ll));
    }
    let (hh, ss, ll) = rgb_to_hsl(c.map(lighten));
    let hh = (hh + hue / 360.0).rem_euclid(1.0);
    // Raising saturation divides by 1 − s, lowering it multiplies by 1 + s.
    let ss = if s > 0.0 { ss / (1.0 - s).max(1.0 / 512.0) } else { ss * (1.0 + s) }.clamp(0.0, 1.0);
    hsl_to_rgb(hh, ss, ll)
}

/// Vibrance, then Saturation, both in linear light (Photoshop measured on colour charts).
fn vibrance_px(c: [f32; 3], v: f32, s: f32) -> [f32; 3] {
    let mut l = c.map(srgb_to_linear);
    if v != 0.0 {
        l = vibrance_lin(l, v);
    }
    if s != 0.0 {
        let g = 0.288 * l[0] + 0.712 * l[1];
        l = l.map(|x| (g + (1.0 + s) * (x - g)).clamp(0.0, 1.0));
    }
    l.map(|x| linear_to_srgb(x.clamp(0.0, 1.0)))
}

const VIBRANCE_BOOST: [f32; 7] = [0.5001, 0.0887, 0.4374, 0.7573, -0.8113, 0.5154, 2.8026];

/// The Vibrance slider (-1..=1) on a linear-light pixel: an HSV change that keeps the hue, which
/// saturates dull colours more than vivid ones and spares skin tones.
fn vibrance_lin(c: [f32; 3], v: f32) -> [f32; 3] {
    let mx = c[0].max(c[1]).max(c[2]);
    let mn = c[0].min(c[1]).min(c[2]);
    if mx <= 0.0 || mx - mn <= 1e-9 {
        return c;
    }
    let t = 1.0 - mn / mx;
    let shape = t * (1.0 - t) * (2.0 - t + t * t) * mx * (1.0 - mx).max(0.0);
    let (t2, v2) = if v < 0.0 {
        let a = -v;
        (t * (1.0 - a / 4.0) * (1.0 - a + a * t * (1.0 + t) / 2.0), mx - a * shape)
    } else {
        let [c0, c1, p, q, r0, ra, rt] = VIBRANCE_BOOST;
        let w = vibrance_skin(hsv_hue(c, mx, mn));
        let a = v * (1.0 - w) + 0.64 * v.powf(1.6) * w;
        let boost = (c0 * a + c1 * a * a) * t.powf(p) * (1.0 - t).powf(q) * ((r0 + ra * a + rt * (1.0 - t)) * (1.0 - mx).max(0.0)).exp();
        ((t * (1.0 + boost)).min(1.0), mx + v / 4.0 * shape)
    };
    let m2 = v2 * (1.0 - t2);
    let k = (v2 - m2) / (mx - mn);
    c.map(|x| m2 + (x - mn) * k)
}

fn vibrance_skin(h: f32) -> f32 {
    if h <= 30.0 {
        1.0
    } else if h < 45.0 {
        (45.0 - h) / 15.0
    } else if h < 300.0 {
        0.0
    } else {
        (h - 300.0) / 60.0
    }
}

fn hsv_hue(c: [f32; 3], mx: f32, mn: f32) -> f32 {
    let d = mx - mn;
    let h = if mx == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    h * 60.0
}

/// Color Balance on channel `ch` of `v`: the three tones fold into one Levels per channel (black
/// point, white point, midtone gamma), reverse-engineered from Photoshop.
pub fn color_balance_channel(tones: [[f32; 3]; 3], preserve: bool, ch: usize, v: f32) -> f32 {
    let [shadows, midtones, highlights] = tones.map(|t| t.map(|s| s.clamp(-100.0, 100.0)));
    let max = |t: [f32; 3]| t[0].max(t[1]).max(t[2]);
    let min = |t: [f32; 3]| t[0].min(t[1]).min(t[2]);
    let (black, white, m) = if preserve {
        (max(shadows) - shadows[ch], highlights[ch] - min(highlights), midtones[ch] - (max(midtones) + min(midtones)) / 2.0)
    } else {
        ((-shadows[ch]).max(0.0), highlights[ch].max(0.0), midtones[ch] + (shadows[ch] + highlights[ch]) / 2.0)
    };
    let (b, w) = (black / 255.0, 1.0 - white / 255.0);
    let v = v.clamp(0.0, 1.0);
    if v >= w {
        return 1.0;
    }
    if v <= b {
        return 0.0;
    }
    balance_gamma((v - b) / (w - b), 2f32.powf(m / 100.0))
}

/// Color Balance's midtone gamma: u^(1/gamma), except that a gamma above 1 starts at slope 2^gamma
/// and joins the power curve in a cubic Hermite.
fn balance_gamma(u: f32, gamma: f32) -> f32 {
    let e = 1.0 / gamma.max(0.01);
    let power = u.powf(e);
    if gamma <= 1.0 {
        return power;
    }
    let s0 = 2f32.powf(gamma);
    let u0 = (2.0 * s0.powf(1.0 / (e - 1.0))).min(1.0);
    if u0.is_nan() || u0 <= 0.0 || u >= u0 {
        return power;
    }
    let (p, d, t) = (u0.powf(e), e * u0.powf(e - 1.0), u / u0);
    let (t2, t3) = (t * t, t * t * t);
    ((t3 - 2.0 * t2 + t) * u0 * s0 + (3.0 * t2 - 2.0 * t3) * p + (t3 - t2) * u0 * d).clamp(0.0, 1.0)
}

/// Black & White grey: the smallest channel, plus the secondary part (middle − smallest) and the
/// primary part (largest − middle), each weighted by its colour's slider.
pub fn black_white_gray(c: [f32; 3], weights: &[f32; 6]) -> f32 {
    let (r, g, b) = (c[0], c[1], c[2]);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let mid = r + g + b - max - min;
    let primary = if r >= g && r >= b {
        0
    } else if g >= b {
        2
    } else {
        4
    };
    let secondary = if b <= r && b <= g {
        1
    } else if r <= g {
        3
    } else {
        5
    };
    (min + (mid - min) * weights[secondary] / 100.0 + (max - mid) * weights[primary] / 100.0).clamp(0.0, 1.0)
}

/// Photo Filter's linear-light matrix: the pixel's D50 XYZ, relative to white, multiplied by the
/// filter colour's (mixed in by `density`).
fn photo_filter_matrix(color: [f32; 3], density: f32) -> [[f32; 3]; 3] {
    let d = density.clamp(0.0, 1.0);
    let xyz = mat3_mul(&SRGB_TO_XYZ_D50, color.map(|v| srgb_to_linear(v.clamp(0.0, 1.0))));
    let s: [f32; 3] = std::array::from_fn(|i| 1.0 - d + d * xyz[i] / D50[i]);
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| XYZ_D50_TO_SRGB[r][k] * s[k] * SRGB_TO_XYZ_D50[k][c]).sum()))
}

/// Samples a 3D table at `c` (tetrahedral interpolation).
fn lut3d_sample(lut: &[f32], n: usize, c: [f32; 3]) -> [f32; 3] {
    if n < 2 || lut.len() < n * n * n * 3 {
        return c;
    }
    let m = (n - 1) as f32;
    let pos = c.map(|v| v.clamp(0.0, 1.0) * m);
    let i0 = pos.map(|p| (p.floor() as usize).min(n - 2));
    let f: [f32; 3] = std::array::from_fn(|k| pos[k] - i0[k] as f32);
    let at = |dr: usize, dg: usize, db: usize| -> [f32; 3] {
        let idx = (((i0[2] + db) * n + i0[1] + dg) * n + i0[0] + dr) * 3;
        [lut[idx], lut[idx + 1], lut[idx + 2]]
    };
    let mix = |w: &[(f32, [f32; 3])]| -> [f32; 3] { std::array::from_fn(|k| w.iter().map(|(a, v)| a * v[k]).sum()) };
    let (fr, fg, fb) = (f[0], f[1], f[2]);
    let (c000, c111) = (at(0, 0, 0), at(1, 1, 1));
    if fr > fg {
        if fg > fb {
            mix(&[(1.0 - fr, c000), (fr - fg, at(1, 0, 0)), (fg - fb, at(1, 1, 0)), (fb, c111)])
        } else if fr > fb {
            mix(&[(1.0 - fr, c000), (fr - fb, at(1, 0, 0)), (fb - fg, at(1, 0, 1)), (fg, c111)])
        } else {
            mix(&[(1.0 - fb, c000), (fb - fr, at(0, 0, 1)), (fr - fg, at(1, 0, 1)), (fg, c111)])
        }
    } else if fb > fg {
        mix(&[(1.0 - fb, c000), (fb - fg, at(0, 0, 1)), (fg - fr, at(0, 1, 1)), (fr, c111)])
    } else if fb > fr {
        mix(&[(1.0 - fg, c000), (fg - fb, at(0, 1, 0)), (fb - fr, at(0, 1, 1)), (fr, c111)])
    } else {
        mix(&[(1.0 - fg, c000), (fg - fr, at(0, 1, 0)), (fr - fb, at(1, 1, 0)), (fb, c111)])
    }
}

/// Reads a `.cube` 3D table (Adobe/Resolve format; inputs in 0..=1).
pub fn parse_cube(text: &str) -> Result<Lut3d, String> {
    let mut n = 0usize;
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
            n = rest.trim().parse().map_err(|_| "bad LUT_3D_SIZE".to_string())?;
            continue;
        }
        if line.starts_with("LUT_1D_SIZE") {
            return Err("1D tables are not supported; choose a 3D LUT.".into());
        }
        if line.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            continue; // TITLE, DOMAIN_MIN, DOMAIN_MAX, …
        }
        let v: Vec<f32> = line.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if v.len() == 3 {
            data.extend(v);
        }
    }
    if !(2..=256).contains(&n) {
        return Err("The file has no LUT_3D_SIZE.".into());
    }
    if data.len() != n * n * n * 3 {
        return Err(format!("Expected {} entries, found {}.", n * n * n, data.len() / 3));
    }
    Ok(Lut3d { n, data })
}

/// Selective Color: each range shifts the cyan, magenta and yellow ink (1 − R, G, B) by its
/// percentage plus its black, weighted by how much the colour belongs to the range.
pub fn selective_color(c: [f32; 3], relative: bool, adj: &[[f32; 4]; 9]) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let mid = c[0] + c[1] + c[2] - max - min;
    let top = |i: usize| if c[i] >= max { max - mid } else { 0.0 };
    let bottom = |i: usize| if c[i] <= min { mid - min } else { 0.0 };
    let w = [
        top(0),
        bottom(2),
        top(1),
        bottom(0),
        top(2),
        bottom(1),
        ((min - 0.5) * 2.0).max(0.0),
        (1.0 - (max - 0.5).abs() - (min - 0.5).abs()).clamp(0.0, 1.0),
        ((0.5 - max) * 2.0).max(0.0),
    ];
    let mut delta = [0.0f32; 3];
    for (r, wr) in w.iter().enumerate() {
        if *wr <= 0.0 {
            continue;
        }
        let a = adj[r].map(|v| v / 100.0);
        for i in 0..3 {
            let ink = 1.0 - c[i];
            let d = a[i] + a[3];
            let d = if relative {
                d * ink
            } else if d > 0.0 {
                d * (2.0 * (1.0 - ink)).min(1.0)
            } else {
                d * (2.0 * ink).min(1.0)
            };
            delta[i] += d * wr;
        }
    }
    std::array::from_fn(|i| (c[i] - delta[i]).clamp(0.0, 1.0))
}

/// Replace Color's selection: 1 at `color`, falling to 0 at `fuzziness` levels away.
fn color_range(px: &[[f32; 4]], color: [f32; 3], fuzziness: f32) -> Vec<f32> {
    let f = fuzziness.max(0.0) + 1.0;
    px.iter()
        .map(|p| {
            if p[3] <= 0.0 {
                return 0.0;
            }
            let d = (0..3).map(|c| (p[c] - color[c]).abs() * 255.0).fold(0.0f32, f32::max);
            (1.0 - d / f).clamp(0.0, 1.0)
        })
        .collect()
}

/// Replace Color's hue (±180°), saturation and lightness (±100) shift, as far as `mask` covers.
fn replace_color_masked(px: &mut [[f32; 4]], mask: &[f32], hue: f32, saturation: f32, lightness: f32) {
    for (q, &k) in px.iter_mut().zip(mask) {
        if k <= 0.0 {
            continue;
        }
        let c = [q[0], q[1], q[2]];
        let (hh, ss, ll) = rgb_to_hsl(c);
        let s = if saturation >= 0.0 { ss + (1.0 - ss) * saturation / 100.0 } else { ss * (1.0 + saturation / 100.0) };
        let l = if lightness >= 0.0 { ll + (1.0 - ll) * lightness / 100.0 } else { ll * (1.0 + lightness / 100.0) };
        let o = hsl_to_rgb((hh + hue / 360.0).rem_euclid(1.0), s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
        for i in 0..3 {
            q[i] = (c[i] + (o[i] - c[i]) * k).clamp(0.0, 1.0);
        }
    }
}

/// Replace Color's Result swatch: `color` after the shift.
pub fn replace_color_result(color: [f32; 3], hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let mut px = [[color[0], color[1], color[2], 1.0]];
    replace_color_masked(&mut px, &[1.0], hue, saturation, lightness);
    [px[0][0], px[0][1], px[0][2]]
}

/// Lab mean of the opaque inside pixels.
struct LabStats {
    mean: [f32; 3],
}

fn lab_stats(px: &[[f32; 4]], inside: &[bool]) -> Option<LabStats> {
    let (mut sum, mut wsum) = ([0.0f64; 3], 0.0f64);
    for (q, _) in px.iter().zip(inside).filter(|(q, i)| **i && q[3] > 0.0) {
        let w = f64::from(q[3]);
        let lab = srgb_to_lab([q[0], q[1], q[2]]);
        for k in 0..3 {
            sum[k] += w * f64::from(lab[k]);
        }
        wsum += w;
    }
    if wsum <= 0.0 {
        return None;
    }
    Some(LabStats { mean: std::array::from_fn(|k| (sum[k] / wsum) as f32) })
}

/// Match Color on the picture's own statistics (no source image): Luminance and Color Intensity
/// scale L and a/b, Neutralize moves the mean colour to grey, Fade blends back.
fn match_color(px: &mut [[f32; 4]], t: &LabStats, luminance: f32, intensity: f32, fade: f32, neutralize: bool) {
    let (lum_k, inten, fade) = (luminance / 100.0, intensity / 100.0, (fade / 100.0).clamp(0.0, 1.0));
    let mut dst = t.mean;
    if neutralize {
        dst[1] = 0.0;
        dst[2] = 0.0;
    }
    for q in px.iter_mut().filter(|q| q[3] > 0.0) {
        let c = [q[0], q[1], q[2]];
        let lab = srgb_to_lab(c);
        let l = (lab[0] - t.mean[0] + dst[0]) * lum_k;
        let a = (lab[1] - t.mean[1] + dst[1]) * inten;
        let b = (lab[2] - t.mean[2] + dst[2]) * inten;
        let o = lab_to_srgb([l.clamp(0.0, 100.0), a, b]);
        for i in 0..3 {
            q[i] = (o[i] + (c[i] - o[i]) * fade).clamp(0.0, 1.0);
        }
    }
}

// ------------------------------------------------------------------------------ histograms

/// Red, green and blue histograms of the opaque inside pixels.
pub fn histograms(px: &[[f32; 4]], inside: &[bool]) -> [[u64; 256]; 3] {
    let mut h = [[0u64; 256]; 3];
    for (p, _) in px.iter().zip(inside).filter(|(p, i)| **i && p[3] > 0.0) {
        for c in 0..3 {
            h[c][(p[c].clamp(0.0, 1.0) * 255.0).round() as usize] += 1;
        }
    }
    h
}

/// Equalize: one cumulative table from the three channels' histogram, applied to each.
fn equalize(px: &mut [[f32; 4]], inside: &[bool]) {
    let hs = histograms(px, inside);
    let hist: Vec<f64> = (0..256).map(|i| (hs[0][i] + hs[1][i] + hs[2][i]) as f64).collect();
    let total: f64 = hist.iter().sum();
    if total == 0.0 {
        return;
    }
    let first = hist.iter().copied().find(|v| *v > 0.0).unwrap_or(0.0);
    let mut table = [0f32; 256];
    let mut acc = 0.0;
    for (i, h) in hist.iter().enumerate() {
        acc += h;
        table[i] = if total > first { ((acc - first) / (total - first)).clamp(0.0, 1.0) as f32 } else { i as f32 / 255.0 };
    }
    map_rgb(px, |c| c.map(|v| table[(v.clamp(0.0, 1.0) * 255.0).round() as usize]));
}

/// Black and white points with `clip` (fraction) of the pixels clipped at each end.
fn clip_points(h: &[u64; 256], clip: f64) -> (f32, f32) {
    let total: u64 = h.iter().sum();
    if total == 0 {
        return (0.0, 1.0);
    }
    let lim = (total as f64 * clip) as u64;
    let (mut acc, mut lo) = (0u64, 0usize);
    while lo < 254 && acc + h[lo] <= lim {
        acc += h[lo];
        lo += 1;
    }
    let (mut acc, mut hi) = (0u64, 255usize);
    while hi > lo + 1 && acc + h[hi] <= lim {
        acc += h[hi];
        hi -= 1;
    }
    (lo as f32 / 255.0, hi as f32 / 255.0)
}

fn median(h: &[u64; 256]) -> f32 {
    let total: u64 = h.iter().sum();
    let mut acc = 0;
    for (i, v) in h.iter().enumerate() {
        acc += v;
        if acc * 2 >= total {
            return i as f32 / 255.0;
        }
    }
    0.5
}

/// The Levels that Auto Tone, Auto Contrast and Auto Color apply (0.1 % clipping, Photoshop's
/// default): Auto Tone stretches each channel, Auto Contrast all three together (keeping the
/// colour), and Auto Color also moves each channel's median onto their average.
pub fn auto_levels(kind: &Adj, px: &[[f32; 4]], inside: &[bool]) -> [LevelsCh; 4] {
    let h = histograms(px, inside);
    let clip = 0.001;
    let mut ch = [LevelsCh::default(); 4];
    if *kind == Adj::AutoContrast {
        let mut all = [0u64; 256];
        for c in &h {
            for (a, v) in all.iter_mut().zip(c) {
                *a += v;
            }
        }
        let (lo, hi) = clip_points(&all, clip);
        for p in &mut ch[1..] {
            (p.in_black, p.in_white) = (lo, hi);
        }
        return ch;
    }
    for c in 0..3 {
        (ch[c + 1].in_black, ch[c + 1].in_white) = clip_points(&h[c], clip);
    }
    if *kind == Adj::AutoColor {
        let meds: Vec<f32> = (0..3).map(|c| median(&h[c])).collect();
        let target = meds.iter().sum::<f32>() / 3.0;
        for c in 0..3 {
            let p = &mut ch[c + 1];
            let span = (p.in_white - p.in_black).max(1e-3);
            let t = ((meds[c] - p.in_black) / span).clamp(0.02, 0.98);
            let want = ((target - p.in_black) / span).clamp(0.02, 0.98);
            p.gamma = (t.ln() / want.ln()).clamp(0.1, 9.99);
        }
    }
    ch
}

// ------------------------------------------------------------------------------ blur

/// In-place running-sum box blur of every row of a `w × h × n` interleaved buffer.
fn box_rows_n(buf: &mut [f32], w: usize, n: usize, r: usize, scratch: &mut Vec<f32>) {
    if r == 0 || w == 0 {
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f64;
    for row in buf.chunks_exact_mut(w * n) {
        scratch.clear();
        scratch.extend_from_slice(row);
        let at = |x: isize, c: usize| scratch[(x.clamp(0, w as isize - 1) as usize) * n + c] as f64;
        for c in 0..n {
            let mut acc: f64 = (-(r as isize)..=r as isize).map(|x| at(x, c)).sum();
            for x in 0..w {
                row[x * n + c] = (acc * norm) as f32;
                acc += at(x as isize + r as isize + 1, c) - at(x as isize - r as isize, c);
            }
        }
    }
}

fn box_cols_n(buf: &mut [f32], w: usize, h: usize, n: usize, r: usize, scratch: &mut Vec<f32>) {
    if r == 0 || h == 0 {
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f64;
    for x in 0..w {
        scratch.clear();
        for y in 0..h {
            scratch.extend_from_slice(&buf[(y * w + x) * n..(y * w + x + 1) * n]);
        }
        let at = |y: isize, c: usize| scratch[(y.clamp(0, h as isize - 1) as usize) * n + c] as f64;
        for c in 0..n {
            let mut acc: f64 = (-(r as isize)..=r as isize).map(|y| at(y, c)).sum();
            for y in 0..h {
                buf[(y * w + x) * n + c] = (acc * norm) as f32;
                acc += at(y as isize + r as isize + 1, c) - at(y as isize - r as isize, c);
            }
        }
    }
}

/// Odd box widths whose 3-fold convolution approximates a Gaussian of `sigma` (Kovesi 2010).
fn gauss_box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0f32;
    let w_ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n) / (-4.0 * wl as f32 - 4.0)).round().clamp(0.0, n) as usize;
    let mut radii = std::array::from_fn(|i| if i < m { (wl as usize - 1) / 2 } else { (wu as usize - 1) / 2 });
    if radii.iter().all(|&r| r == 0) {
        radii[0] = 1;
    }
    radii
}

/// Approximate Gaussian blur (three box passes) of an interleaved buffer.
fn gauss_blur_n(buf: &mut [f32], w: usize, h: usize, n: usize, sigma: f32) {
    if sigma < 0.3 {
        return;
    }
    let mut scratch = Vec::with_capacity(w.max(h) * n);
    for r in gauss_box_radii(sigma) {
        box_rows_n(buf, w, n, r, &mut scratch);
    }
    for r in gauss_box_radii(sigma) {
        box_cols_n(buf, w, h, n, r, &mut scratch);
    }
}

// ------------------------------------------------------------------------------ Shadows/Highlights

/// Neighbourhood σ per pixel of Radius, the guide blur, the edge range and the brightness levels
/// of the edge-aware average (fitted to Photoshop 25.4).
const SPATIAL: f32 = 1.1424;
const GUIDE: f32 = 1.3559;
const RANGE: f32 = 1.0152;
const LEVELS: usize = 17;

/// Edge-aware neighbourhood brightness of `lum` for Photoshop's Radius (Durand and Dorsey's fast
/// bilateral filter: the average at a few brightness levels, interpolated at the pixel's own).
fn neighbourhood(lum: &[f32], w: usize, h: usize, radius: f32) -> Vec<f32> {
    let mut guide = lum.to_vec();
    gauss_blur_n(&mut guide, w, h, 1, GUIDE);
    let sigma = SPATIAL * radius;
    if sigma < 0.3 {
        return guide;
    }
    let d = ((sigma / 4.0).floor() as usize).max(1);
    let (rw, rh) = (w.div_ceil(d), h.div_ceil(d));
    let cells = rw * rh;
    let step = 1.0 / (LEVELS - 1) as f32;
    let mut avg = vec![0.0f32; LEVELS * cells];
    let mut buf = vec![0.0f32; 2 * cells];
    for (k, out) in avg.chunks_exact_mut(cells).enumerate() {
        let g = k as f32 * step;
        buf.fill(0.0);
        for (i, &l) in lum.iter().enumerate() {
            let wgt = (1.0 - (l - g).abs() / RANGE).max(0.0);
            let c = 2 * ((i / w) / d * rw + (i % w) / d);
            buf[c] += wgt * l;
            buf[c + 1] += wgt;
        }
        gauss_blur_n(&mut buf, rw, rh, 2, sigma / d as f32);
        for (o, b) in out.iter_mut().zip(buf.chunks_exact(2)) {
            *o = if b[1] > 1e-6 { b[0] / b[1] } else { g };
        }
    }
    let mut out = vec![0.0f32; lum.len()];
    for y in 0..h {
        let fy = ((y as f32 + 0.5) / d as f32 - 0.5).clamp(0.0, (rh - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(rh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) / d as f32 - 0.5).clamp(0.0, (rw - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(rw - 1);
            let at = |k: usize| {
                let a = &avg[k * cells..(k + 1) * cells];
                let top = a[y0 * rw + x0] + (a[y0 * rw + x1] - a[y0 * rw + x0]) * tx;
                let bot = a[y1 * rw + x0] + (a[y1 * rw + x1] - a[y1 * rw + x0]) * tx;
                top + (bot - top) * ty
            };
            let t = (guide[y * w + x].clamp(0.0, 1.0) / step).min((LEVELS - 1) as f32 - 1e-4);
            let k = t.floor() as usize;
            let f = t - k as f32;
            out[y * w + x] = at(k) * (1.0 - f) + at(k + 1) * f;
        }
    }
    out
}

/// Beta(α, β) of the tonal mask for Tone 0, 5, …, 95 (fitted to Photoshop's masks within 2/255).
const MASK_BETA: [(f32, f32); 20] = [
    (1.9468, 22.9366),
    (1.7770, 18.3471),
    (1.6403, 14.7559),
    (1.5270, 11.9346),
    (1.4269, 9.6467),
    (1.3470, 7.8430),
    (1.2807, 6.4038),
    (1.2240, 5.2384),
    (1.1772, 4.2967),
    (1.1373, 3.5350),
    (1.1021, 2.9109),
    (1.0755, 2.4186),
    (1.0508, 2.0189),
    (1.0307, 1.7034),
    (1.0140, 1.4583),
    (1.0015, 1.2758),
    (1.0, 1.1403),
    (1.0, 1.0510),
    (1.0, 1.0002),
    (1.0, 1.0),
];

/// The tonal mask for `tone` (0..=100) at the 256 neighbourhood levels: 1 − I_L(α, β).
fn tonal_mask(tone: f32) -> [f32; 256] {
    let t = tone.clamp(0.0, 95.0) / 5.0;
    let i = (t.floor() as usize).min(MASK_BETA.len() - 2);
    let f = t - i as f32;
    let (a0, b0) = MASK_BETA[i];
    let (a1, b1) = MASK_BETA[i + 1];
    let (a, b) = ((a0 + (a1 - a0) * f) as f64, (b0 + (b1 - b0) * f) as f64);
    const SUB: usize = 16;
    let n = 255 * SUB;
    let dens = |x: f64| ((a - 1.0) * x.ln() + (b - 1.0) * (1.0 - x).ln()).exp();
    let mut cum = [0.0f64; 256];
    let mut acc = 0.0;
    for j in 0..n {
        acc += dens((j as f64 + 0.5) / n as f64);
        if (j + 1) % SUB == 0 {
            cum[(j + 1) / SUB] = acc;
        }
    }
    let total = acc.max(f64::MIN_POSITIVE);
    std::array::from_fn(|k| (1.0 - cum[k] / total) as f32)
}

/// Curve strength for Amount (0..=100): 0 at 0, 1 near 15, 2 at 50, 3 at 100.
fn strength(a: f32) -> f32 {
    let u = (a.clamp(0.0, 100.0) / 100.0).powf(0.6216);
    if u <= 0.3 { u / 0.3 } else { (1.0 + 20.0 * u) / 7.0 }
}

/// The full-strength curve of strength `s`: 1 − (1 − v)^q between q = 2^⌊s⌋ and 2^(⌊s⌋+1).
fn lift(s: f32, v: f32) -> f32 {
    let n = (s.floor() as i32).clamp(0, 2);
    let t = (s - n as f32).clamp(0.0, 1.0);
    let lo = (1.0 - v).clamp(0.0, 1.0).powi(1 << n);
    1.0 - ((1.0 - t) * lo + t * lo * lo)
}

fn interp(table: &[(f32, f32)], x: f32) -> f32 {
    let Some(&(x0, y0)) = table.first() else { return 0.0 };
    if x <= x0 {
        return y0;
    }
    for p in table.windows(2) {
        let ((xa, ya), (xb, yb)) = (p[0], p[1]);
        if x <= xb {
            return ya + (yb - ya) * (x - xa) / (xb - xa);
        }
    }
    table.last().map_or(0.0, |p| p.1)
}

const COLOR_UP: [(f32, f32); 12] = [
    (0.0, 0.0),
    (1.0, 0.1566),
    (2.0, 0.2271),
    (5.0, 0.3776),
    (10.0, 0.5653),
    (20.0, 0.8669),
    (30.0, 1.1299),
    (40.0, 1.3748),
    (50.0, 1.6093),
    (60.0, 1.8375),
    (75.0, 2.1721),
    (100.0, 2.7193),
];
const COLOR_DOWN: [(f32, f32); 9] =
    [(0.0, 0.0), (1.0, -0.0145), (5.0, -0.0709), (10.0, -0.1373), (20.0, -0.2570), (30.0, -0.3612), (50.0, -0.5312), (75.0, -0.6866), (100.0, -0.7968)];
/// Midtone: (value, k, p) of k · (Y − ½) · (4Y(1 − Y))^p.
const MIDTONE: [(f32, f32, f32); 11] = [
    (-100.0, -0.6955, 2.267),
    (-75.0, -0.5873, 2.362),
    (-50.0, -0.4429, 2.492),
    (-25.0, -0.2514, 2.667),
    (-10.0, -0.1088, 2.803),
    (0.0, 0.0, 2.9),
    (10.0, 0.1206, 3.016),
    (25.0, 0.3262, 3.216),
    (50.0, 0.7423, 3.625),
    (75.0, 1.2635, 4.160),
    (100.0, 1.9032, 4.850),
];

fn midtone(m: f32, y: f32) -> f32 {
    if m == 0.0 {
        return y;
    }
    let ks: Vec<(f32, f32)> = MIDTONE.iter().map(|r| (r.0, r.1)).collect();
    let ps: Vec<(f32, f32)> = MIDTONE.iter().map(|r| (r.0, r.2)).collect();
    let (k, p) = (interp(&ks, m), interp(&ps, m));
    (y + k * (y - 0.5) * (4.0 * y * (1.0 - y)).max(0.0).powf(p)).clamp(0.0, 1.0)
}

fn level(v: f32) -> usize {
    (v.clamp(0.0, 1.0) * 255.0).round() as usize
}

/// Shadows/Highlights as Photoshop 25.4 does it (measured as a black box by PhotoCraft): an
/// edge-aware neighbourhood brightness, a Beta tonal mask, a self-screening lift curve, the
/// colour differences added back scaled by Color, and a midtone S-curve.
pub fn shadows_highlights(px: &mut [[f32; 4]], w: usize, h: usize, p: &ShadowsHighlights) {
    if px.is_empty() || px.len() != w * h {
        return;
    }
    let (sa, ha) = (p.shadow_amount, p.highlight_amount);
    let lum: Vec<f32> = px.iter().map(|q| (q[0].max(q[1]).max(q[2]) + luma([q[0], q[1], q[2]])) / 2.0).collect();
    let ls = (sa > 0.0).then(|| neighbourhood(&lum, w, h, p.shadow_radius));
    let lh = (ha > 0.0).then(|| match &ls {
        Some(l) if (p.highlight_radius - p.shadow_radius).abs() < 1e-3 => l.clone(),
        _ => neighbourhood(&lum, w, h, p.highlight_radius),
    });
    let (ms, mh) = (tonal_mask(p.shadow_tone), tonal_mask(p.highlight_tone));
    let (ss, hs) = (strength(sa), strength(ha));
    let mut y2 = vec![0.0f32; px.len()];
    for (i, out) in y2.iter_mut().enumerate() {
        let q = px[i];
        let mut adj = [q[0], q[1], q[2]];
        if let Some(l) = &ls {
            let m = ms[level(l[i])];
            for v in &mut adj {
                *v += m * (lift(ss, *v) - *v);
            }
        }
        if let Some(l) = &lh {
            let m = mh[255 - level(l[i])];
            for (v, &o) in adj.iter_mut().zip(&q[..3]) {
                *v -= m * (lift(hs, 1.0 - o) - (1.0 - o));
            }
        }
        *out = midtone(p.midtone, luma(adj).clamp(0.0, 1.0));
    }
    clip_levels(&mut y2, px, p.black_clip, p.white_clip);
    let gain = |f: f32| {
        if p.color >= 0.0 { (interp(&COLOR_UP, p.color) * f).exp() } else { 1.0 + interp(&COLOR_DOWN, -p.color) * f }
    };
    for (q, &yn) in px.iter_mut().zip(&y2) {
        if q[3] <= 0.0 {
            continue;
        }
        let y1 = luma([q[0], q[1], q[2]]);
        let f = (yn - y1).abs() / yn.max(y1).max(1e-6);
        let g = gain(f);
        let d = [(q[0] - y1) * g, (q[1] - y1) * g, (q[2] - y1) * g];
        // Shorten the colour difference where it would leave 0..=1.
        let mut k = 1.0f32;
        for v in d {
            if v > 1e-9 {
                k = k.min((1.0 - yn) / v);
            } else if v < -1e-9 {
                k = k.min(yn / -v);
            }
        }
        let k = k.clamp(0.0, 1.0);
        for c in 0..3 {
            q[c] = (yn + k * d[c]).clamp(0.0, 1.0);
        }
    }
}

/// Black and White Clip: stretches the new luma so `black` % of pixels reach 0 and `white` % 1.
fn clip_levels(y2: &mut [f32], px: &[[f32; 4]], black: f32, white: f32) {
    if black <= 0.0 && white <= 0.0 {
        return;
    }
    let mut hist = vec![0u32; 4096];
    let mut n = 0u32;
    for (&y, q) in y2.iter().zip(px) {
        if q[3] > 0.0 {
            hist[(y.clamp(0.0, 1.0) * 4095.0).round() as usize] += 1;
            n += 1;
        }
    }
    if n == 0 {
        return;
    }
    let find = |pct: f32, rev: bool| -> f32 {
        let target = (pct.clamp(0.0, 50.0) / 100.0 * n as f32) as u32;
        let mut acc = 0;
        for k in 0..4096 {
            let i = if rev { 4095 - k } else { k };
            acc += hist[i];
            if acc > target {
                return i as f32 / 4095.0;
            }
        }
        if rev { 0.0 } else { 1.0 }
    };
    let (lo, hi) = (if black > 0.0 { find(black, false) } else { 0.0 }, if white > 0.0 { find(white, true) } else { 1.0 });
    if hi - lo < 1e-3 {
        return;
    }
    for y in y2.iter_mut() {
        *y = ((*y - lo) / (hi - lo)).clamp(0.0, 1.0);
    }
}

/// HDR Toning, Local Adaptation: the blurred log luminance is the base layer, compressed toward
/// its mean by Strength; detail is boosted; then exposure, gamma, shadow/highlight lift and
/// vibrance/saturation around the new luminance.
pub fn hdr_toning(px: &mut [[f32; 4]], w: usize, h: usize, p: &HdrToning) {
    if px.is_empty() || px.len() != w * h {
        return;
    }
    const EPS: f32 = 1e-4;
    let logl: Vec<f32> = px.iter().map(|q| (luma([q[0], q[1], q[2]]).max(0.0) + EPS).ln()).collect();
    let mut base = logl.clone();
    gauss_blur_n(&mut base, w, h, 1, p.radius.max(1.0) / 2.0);
    let n = px.iter().filter(|q| q[3] > 0.0).count().max(1) as f32;
    let mean = base.iter().zip(px.iter()).filter(|(_, q)| q[3] > 0.0).map(|(b, _)| *b).sum::<f32>() / n;
    let compress = 1.0 / (1.0 + p.strength.max(0.0));
    let detail = 1.0 + p.detail / 100.0;
    let exposure = 2f32.powf(p.exposure);
    let gamma = p.gamma.clamp(0.1, 4.0);
    for (i, q) in px.iter_mut().enumerate() {
        if q[3] <= 0.0 {
            continue;
        }
        let c = [q[0], q[1], q[2]];
        let l0 = luma(c).max(0.0) + EPS;
        let new_log = mean + (base[i] - mean) * compress + (logl[i] - base[i]) * detail;
        let mut l1 = (new_log.exp() * exposure).max(0.0).powf(1.0 / gamma);
        l1 += p.shadow / 100.0 * (1.0 - l1).powi(3) * 0.5 - p.highlight / 100.0 * l1.powi(3) * 0.5;
        let l1 = l1.clamp(0.0, 1.0);
        let mut out = c.map(|v| v * l1 / l0);
        let ll = luma(out);
        let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
        let sat_now = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
        let s = 1.0 + p.saturation / 100.0 + p.vibrance / 100.0 * (1.0 - sat_now);
        out = out.map(|v| ll + (v - ll) * s.max(0.0));
        for k in 0..3 {
            q[k] = out[k].clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(adj: &Adj, c: [u8; 3]) -> [u8; 3] {
        let mut px = vec![[c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]];
        apply(adj, &mut px, 1, 1, &[true]);
        [0, 1, 2].map(|i| (px[0][i] * 255.0).round() as u8)
    }

    #[test]
    fn defaults_leave_pixels_alone() {
        let colours = [[0, 0, 0], [255, 255, 255], [200, 120, 40], [12, 130, 250], [128, 128, 128]];
        for kind in [
            AdjKind::BrightnessContrast,
            AdjKind::Levels,
            AdjKind::Curves,
            AdjKind::Exposure,
            AdjKind::Vibrance,
            AdjKind::HueSaturation,
            AdjKind::ColorBalance,
            AdjKind::ChannelMixer,
            AdjKind::ColorLookup,
            AdjKind::SelectiveColor,
            AdjKind::MatchColor,
            AdjKind::ReplaceColor,
        ] {
            for c in colours {
                let o = run(&Adj::new(kind), c);
                assert!(o.iter().zip(c).all(|(a, b)| (*a as i32 - b as i32).abs() <= 1), "{kind:?} changed {c:?} to {o:?}");
            }
        }
    }

    #[test]
    fn invert_desaturate_threshold_posterize() {
        assert_eq!(run(&Adj::Invert, [10, 100, 250]), [245, 155, 5]);
        assert_eq!(run(&Adj::Desaturate, [200, 100, 0]), [100, 100, 100]);
        assert_eq!(run(&Adj::Threshold { level: 128.0 }, [200, 200, 200]), [255; 3]);
        assert_eq!(run(&Adj::Threshold { level: 128.0 }, [60, 60, 60]), [0; 3]);
        // Photoshop corpus: 3 levels map to 0, 127, 255.
        assert_eq!(run(&Adj::Posterize { levels: 3 }, [10, 128, 250]), [0, 127, 255]);
    }

    #[test]
    fn levels_curves_and_brightness_move_tones_the_right_way() {
        let mut ch = [LevelsCh::default(); 4];
        ch[0].in_black = 50.0 / 255.0;
        ch[0].in_white = 200.0 / 255.0;
        assert_eq!(run(&Adj::Levels { ch }, [50, 125, 200]), [0, 128, 255]);
        let mut curves = Adj::new(AdjKind::Curves);
        if let Adj::Curves { ch } = &mut curves {
            ch[0] = vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]];
        }
        assert!(run(&curves, [128, 128, 128])[0] > 180);
        let up = run(&Adj::BrightnessContrast { brightness: 50.0, contrast: 0.0, legacy: false }, [100, 100, 100]);
        let down = run(&Adj::BrightnessContrast { brightness: -50.0, contrast: 0.0, legacy: false }, [100, 100, 100]);
        assert!(up[0] > 120 && down[0] < 85, "{up:?} {down:?}");
        // Brightness pins black and white.
        assert_eq!(run(&Adj::BrightnessContrast { brightness: 80.0, contrast: 40.0, legacy: false }, [255, 0, 255]), [255, 0, 255]);
    }

    #[test]
    fn hue_saturation_matches_photoshop() {
        // Photoshop 27.11: Hue +40, Saturation +25, Lightness −10 turns (240, 160, 30) into (173, 243, 0).
        let o = run(&Adj::HueSaturation { hue: 40.0, saturation: 25.0, lightness: -10.0, colorize: false }, [240, 160, 30]);
        assert!(o.iter().zip([173, 243, 0]).all(|(a, b)| (*a as i32 - b).abs() <= 2), "{o:?}");
    }

    #[test]
    fn black_white_and_gradient_map_give_greys_and_ramps() {
        let o = run(&Adj::new(AdjKind::BlackWhite), [255, 0, 0]);
        assert_eq!(o, [102, 102, 102], "reds at 40 %");
        let o = run(&Adj::GradientMap { from: [1.0, 0.0, 0.0], to: [0.0, 0.0, 1.0], reverse: false }, [0, 0, 0]);
        assert_eq!(o, [255, 0, 0]);
    }

    #[test]
    fn auto_contrast_stretches_and_equalize_spreads() {
        let mut px: Vec<[f32; 4]> = (0..100).map(|i| {
            let v = 0.3 + 0.4 * i as f32 / 99.0;
            [v, v, v, 1.0]
        }).collect();
        let inside = vec![true; px.len()];
        let mut q = px.clone();
        apply(&Adj::AutoContrast, &mut q, 100, 1, &inside);
        assert!(q[0][0] < 0.02 && q[99][0] > 0.98, "{:?} {:?}", q[0], q[99]);
        apply(&Adj::Equalize, &mut px, 100, 1, &inside);
        assert!(px[99][0] > 0.99);
    }

    #[test]
    fn shadows_lift_dark_pixels() {
        let (w, h) = (40, 40);
        let mut px = vec![[0.1, 0.1, 0.1, 1.0]; w * h];
        shadows_highlights(&mut px, w, h, &ShadowsHighlights { color: 0.0, black_clip: 0.0, white_clip: 0.0, ..Default::default() });
        assert!(px[20 * w + 20][0] > 0.15, "{:?}", px[20 * w + 20]);
    }

    #[test]
    fn cube_files_parse_and_identity_is_identity() {
        let mut s = String::from("TITLE \"id\"\nLUT_3D_SIZE 2\n");
        for i in 0..8 {
            s += &format!("{} {} {}\n", i & 1, (i >> 1) & 1, (i >> 2) & 1);
        }
        let t = Arc::new(parse_cube(&s).unwrap());
        assert_eq!(run(&Adj::ColorLookup { lut: Some(t), name: "id".into() }, [30, 140, 220]), [30, 140, 220]);
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }
}
