//! Theme engine on the Omarchy "Quattro" `colors.toml` schema.
//!
//! Resolution: explicit `config.theme` (name or path) → Omarchy current theme →
//! wallet themes directory → `terminal` (ANSI palette, adapts to any terminal theme).
//! Widgets reference semantic roles only.

use crate::commands::Ctx;
use ratatui::style::{Color, Modifier, Style};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use wallet_core::Result;

/// Semantic roles used by every widget.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub source: String,
    pub light: bool,
    pub surface: Color,
    pub raised: Color,
    pub text: Color,
    pub strong: Color,
    pub dim: Color,
    pub focus: Color,
    pub selection: Color,
    pub quai: Color,
    pub qi: Color,
    pub ok: Color,
    pub pending: Color,
    pub attention: Color,
    pub danger: Color,
    pub link: Color,
    /// Resting borders, rules and dividers: quieter than any text (1.6–2.4:1 against the
    /// surface), so the lit focus border is the brightest line on screen by a wide margin.
    pub line: Color,
    /// Boundaries a person acts on (input tracks, meter troughs): ≈3:1.
    pub line_strong: Color,
    /// A price that rose or fell. Kept apart from `ok`/`danger`, which mean state: a falling
    /// price is not an error.
    pub up: Color,
    pub down: Color,
    /// Categorical colors for series and allocations (never state colors).
    pub chart: [Color; 8],
    /// Ink on a filled accent or danger chip: whichever of black or white reads best on it.
    pub on_accent: Color,
    pub on_danger: Color,
    /// The one-cell shadow under a modal.
    pub shadow: Color,
    /// The resting node dot: `ok`, quieter.
    pub ok_soft: Color,
    /// Colors for effects/charts (hex-capable themes only).
    pub accent_rgb: Option<(u8, u8, u8)>,
    pub adjusted: bool,
    pub monochrome: bool,
    /// Which glyphs draw here (set by the app each frame; Unicode until then).
    pub icons: super::icons::Set,
}

fn hex(value: &str) -> Option<(u8, u8, u8)> {
    let v = value.trim().trim_start_matches('#');
    if v.len() != 6 {
        return None;
    }
    Some((u8::from_str_radix(&v[0..2], 16).ok()?, u8::from_str_radix(&v[2..4], 16).ok()?, u8::from_str_radix(&v[4..6], 16).ok()?))
}

fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let ch = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
}

/// WCAG contrast ratio.
pub fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round().clamp(0.0, 255.0) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Hue (degrees), saturation and lightness.
fn hsl((r, g, b): (u8, u8, u8)) -> (f64, f64, f64) {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if (max - min).abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r), to(g), to(b))
}

fn linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn gamma(c: f64) -> u8 {
    let c = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// OKLab: a space where equal distances look equally different.
fn oklab((r, g, b): (u8, u8, u8)) -> (f64, f64, f64) {
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    (
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    )
}

fn from_oklab((l, a, b): (f64, f64, f64)) -> (u8, u8, u8) {
    let l_ = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m_ = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s_ = (l - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    (
        gamma(4.076_741_662_1 * l_ - 3.307_711_591_3 * m_ + 0.230_969_929_2 * s_),
        gamma(-1.268_438_004_6 * l_ + 2.609_757_401_1 * m_ - 0.341_319_396_5 * s_),
        gamma(-0.004_196_086_3 * l_ - 0.703_418_614_7 * m_ + 1.707_614_701 * s_),
    )
}

/// How different two colors look (OKLab distance; about 0.1 reads as clearly different side by
/// side).
pub fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (x, y) = (oklab(a), oklab(b));
    ((x.0 - y.0).powi(2) + (x.1 - y.1).powi(2) + (x.2 - y.2).powi(2)).sqrt()
}

/// Turn `c` around the hue circle, away from every color in `from`, until it is at least `min`
/// from each (keeping its lightness and chroma), at most 60°. Palettes often put red and orange
/// a hair apart; an error and a warning must not look alike.
fn separate(c: (u8, u8, u8), from: &[(u8, u8, u8)], min: f64) -> (u8, u8, u8) {
    let near = |c: (u8, u8, u8)| from.iter().copied().min_by(|x, y| distance(c, *x).total_cmp(&distance(c, *y)));
    let Some(anchor) = near(c) else { return c };
    if distance(c, anchor) >= min {
        return c;
    }
    let (l, a, b) = oklab(c);
    let chroma = a.hypot(b).max(0.04);
    let hue = b.atan2(a);
    let (_, aa, ab) = oklab(anchor);
    // Away from the anchor's hue: the shorter way round points at it.
    let toward = (ab.atan2(aa) - hue + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    let step = if toward > 0.0 { -1.0f64 } else { 1.0 }.to_radians() * 3.0;
    // A muted palette has little hue to turn: past 60° the color gains chroma instead.
    let mut best = c;
    for i in 1..=30u32 {
        let h = hue + step * i.min(20) as f64;
        let chroma = chroma * (1.0 + 0.1 * i.saturating_sub(20) as f64);
        let turned = from_oklab((l, chroma * h.cos(), chroma * h.sin()));
        best = turned;
        if from.iter().all(|f| distance(turned, *f) >= min) {
            break;
        }
    }
    best
}

/// Parsed palette keys (canonical Quattro names, legacy aliases resolved).
pub fn parse_colors(text: &str) -> std::result::Result<BTreeMap<String, String>, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for (k, v) in table {
        if let Some(s) = v.as_str() {
            out.insert(k.to_lowercase(), s.to_string());
        }
    }
    for (legacy, canonical) in [("bg", "background"), ("fg", "foreground"), ("dark_bg", "dark_background")] {
        if !out.contains_key(canonical)
            && let Some(v) = out.get(legacy).cloned()
        {
            out.insert(canonical.into(), v);
        }
    }
    Ok(out)
}

impl Theme {
    /// ANSI palette theme: follows the terminal's own colors.
    /// The terminal theme, fitted to what the terminal said about itself: lines and the
    /// selection move off palette color 8 where it is too faint to see against the background
    /// (solarized-dark's is the background itself), and with truecolor and a known background
    /// modals get a raised surface and a shadow of their own.
    pub fn fit_to_terminal(&mut self, background: Option<(u8, u8, u8)>, ansi8: Option<(u8, u8, u8)>, truecolor: bool) {
        if self.name != "terminal" {
            return;
        }
        if let (Some(bg), Some(a8)) = (background, ansi8)
            && contrast(a8, bg) < 1.6
        {
            self.line = Color::Gray;
            self.line_strong = Color::White;
            self.selection = if self.light { Color::Gray } else { Color::Blue };
        }
        if let (true, Some((r, g, b))) = (truecolor, background) {
            let lift = |c: u8, k: f32| {
                let c = f32::from(c);
                (if self.light { c * (1.0 - k) } else { c + (255.0 - c) * k }).round() as u8
            };
            let sink = |c: u8| (f32::from(c) * 0.55).round() as u8;
            self.raised = Color::Rgb(lift(r, 0.07), lift(g, 0.07), lift(b, 0.07));
            self.shadow = Color::Rgb(sink(r), sink(g), sink(b));
        }
    }

    pub fn terminal(light: bool) -> Theme {
        Theme {
            name: "terminal".into(),
            source: "terminal palette".into(),
            light,
            surface: Color::Reset,
            raised: Color::Reset,
            text: Color::Reset,
            strong: Color::Reset,
            dim: Color::DarkGray,
            focus: Color::Cyan,
            selection: if light { Color::Gray } else { Color::DarkGray },
            quai: Color::Blue,
            qi: Color::Magenta,
            ok: Color::Green,
            pending: Color::Yellow,
            attention: Color::LightRed,
            danger: Color::Red,
            link: Color::Cyan,
            line: Color::DarkGray,
            line_strong: Color::Gray,
            up: Color::Green,
            down: Color::Red,
            chart: [
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                Color::Yellow,
                Color::Green,
                Color::LightBlue,
                Color::LightMagenta,
                Color::LightCyan,
            ],
            on_accent: Color::Reset,
            on_danger: Color::Reset,
            shadow: Color::Reset,
            ok_soft: Color::Green,
            accent_rgb: None,
            adjusted: false,
            monochrome: false,
            icons: super::icons::Set::Unicode,
        }
    }

    /// Monochrome (NO_COLOR): emphasis through modifiers only.
    pub fn mono() -> Theme {
        let mut t = Theme::terminal(false);
        t.name = "mono".into();
        t.source = "NO_COLOR".into();
        for c in [
            &mut t.dim,
            &mut t.focus,
            &mut t.selection,
            &mut t.quai,
            &mut t.qi,
            &mut t.ok,
            &mut t.pending,
            &mut t.attention,
            &mut t.danger,
            &mut t.link,
            &mut t.line,
            &mut t.line_strong,
            &mut t.up,
            &mut t.down,
            &mut t.ok_soft,
        ] {
            *c = Color::Reset;
        }
        t.chart = [Color::Reset; 8];
        t.monochrome = true;
        t
    }

    /// Build from a Quattro palette.
    pub fn from_palette(name: &str, source: &str, keys: &BTreeMap<String, String>) -> Option<Theme> {
        let get = |k: &str| keys.get(k).and_then(|v| hex(v));
        let background = get("background")?;
        let mut foreground = get("foreground")?;
        let light = match keys.get("mode").map(|s| s.as_str()) {
            Some("light") => true,
            Some("dark") => false,
            _ => luminance(background) > 0.5,
        };
        let pick = |k: &str, fallback: (u8, u8, u8)| get(k).unwrap_or(fallback);
        let red = pick("red", (0xe0, 0x6c, 0x75));
        let green = pick("green", (0x98, 0xc3, 0x79));
        let yellow = pick("yellow", (0xe5, 0xc0, 0x7b));
        let blue = pick("blue", (0x61, 0xaf, 0xef));
        let magenta = pick("magenta", (0xc6, 0x78, 0xdd));
        let cyan = pick("cyan", (0x56, 0xb6, 0xc2));
        let orange = get("orange").unwrap_or_else(|| mix(red, yellow, 0.5));
        let accent = get("accent").unwrap_or(blue);
        let mut adjusted = false;
        // Safety-critical text must stay readable whatever the theme says.
        let bright = get("bright_foreground").unwrap_or(foreground);
        let best = if contrast(bright, background) > contrast(foreground, background) { bright } else { foreground };
        if contrast(foreground, background) < 4.5 {
            foreground = best;
            adjusted = true;
        }
        let strong = if contrast(best, background) >= 7.0 {
            best
        } else {
            adjusted = true;
            if light { (0, 0, 0) } else { (255, 255, 255) }
        };
        let muted = get("muted").unwrap_or_else(|| mix(foreground, background, 0.45));
        let dim = if contrast(muted, background) < 2.5 { mix(foreground, background, 0.35) } else { muted };
        let raised = get("lighter_background").unwrap_or_else(|| mix(background, foreground, 0.08));
        let selection = get("selection").unwrap_or_else(|| mix(background, accent, 0.30));
        // Legibility guards against every background a color is drawn on: the page,
        // raised surfaces (header, modals) and the selection highlight.
        let extreme = if light { (0, 0, 0) } else { (255, 255, 255) };
        let selection = {
            let mut sel = selection;
            for _ in 0..10 {
                if contrast(sel, background) >= 1.3 && contrast(sel, raised) >= 1.2 {
                    break;
                }
                sel = mix(sel, accent, 0.25);
            }
            sel
        };
        let ensure = |c: (u8, u8, u8), backs: &[(u8, u8, u8)], min: f64, toward: (u8, u8, u8)| {
            let mut c = c;
            for _ in 0..20 {
                if backs.iter().all(|b| contrast(c, *b) >= min) {
                    break;
                }
                c = mix(c, toward, 0.12);
            }
            c
        };
        let dim = ensure(ensure(dim, &[background, raised], 4.5, extreme), &[selection], 3.0, extreme);
        // State colors are read as text (errors, warnings, amounts), so they hold text contrast
        // on every surface they are drawn on, the raised header and modals included. At 3:1
        // `danger` read 3.16:1 on nightfox's modals.
        let state = |c: (u8, u8, u8)| ensure(c, &[background, raised], 4.5, extreme);
        let (red, green, yellow, orange, blue, magenta, cyan) =
            (state(red), state(green), state(yellow), state(orange), state(blue), state(magenta), state(cyan));
        // The accent is lines and marks as much as text: 3:1 keeps a light accent light.
        let accent = ensure(accent, &[background, raised], 3.0, extreme);
        // Meanings that must not be confused, settled in order of what is at stake: an error
        // never looks like the focus, a warning never looks like an error, Qi never looks like
        // QUAI, pending never looks like a warning. Each turn is checked for contrast again.
        let red = state(separate(red, &[accent], 0.1));
        let orange = state(separate(orange, &[red, accent], 0.1));
        let magenta = state(separate(magenta, &[blue, accent], 0.1));
        let yellow = state(separate(yellow, &[orange, green], 0.08));
        // A resting line sits between the surface and dim text: present, never competing.
        let line = {
            let mut l = mix(foreground, background, 0.78);
            for _ in 0..20 {
                let c = contrast(l, background);
                if c < 1.6 {
                    l = mix(l, extreme, 0.08);
                } else if c > 2.4 {
                    l = mix(l, background, 0.08);
                } else {
                    break;
                }
            }
            l
        };
        let line_strong = ensure(mix(foreground, background, 0.6), &[background], 3.0, extreme);
        let ink =
            |fill: (u8, u8, u8)| if contrast((0, 0, 0), fill) >= contrast((255, 255, 255), fill) { (0, 0, 0) } else { (255, 255, 255) };
        let shadow = if light { mix(background, (0, 0, 0), 0.08) } else { mix(background, (0, 0, 0), 0.45) };
        // Eight categorical hues around the color wheel from the accent, each held to
        // 3:1 on the page so a thin bar still reads.
        let chart = {
            let (h, s, l) = hsl(accent);
            let mut out = [Color::Reset; 8];
            for (i, slot) in out.iter_mut().enumerate() {
                // Starting a sixth of the way round: the accent itself means focus.
                let c = from_hsl((h + 60.0 + 360.0 * i as f64 / 8.0 + 20.0 * (i % 2) as f64) % 360.0, s.max(0.45), l.clamp(0.45, 0.65));
                *slot = rgb(ensure(c, &[background], 3.0, extreme));
            }
            out
        };
        Some(Theme {
            name: name.into(),
            source: source.into(),
            light,
            surface: rgb(background),
            raised: rgb(raised),
            text: rgb(foreground),
            strong: rgb(strong),
            dim: rgb(dim),
            focus: rgb(accent),
            selection: rgb(selection),
            quai: rgb(blue),
            qi: rgb(magenta),
            ok: rgb(green),
            pending: rgb(yellow),
            attention: rgb(orange),
            danger: rgb(red),
            link: rgb(cyan),
            line: rgb(line),
            line_strong: rgb(line_strong),
            up: rgb(green),
            down: rgb(red),
            chart,
            on_accent: rgb(ink(accent)),
            on_danger: rgb(ink(red)),
            shadow: rgb(shadow),
            ok_soft: rgb(mix(green, background, 0.25)),
            accent_rgb: Some(accent),
            adjusted,
            monochrome: false,
            icons: super::icons::Set::Unicode,
        })
    }

    /// An icon in this terminal's glyph set.
    pub fn icon(&self, icon: super::icons::Icon) -> &'static str {
        icon.glyph(self.icons)
    }

    /// An icon and its trailing space, or nothing where the set has no glyph for it.
    pub fn lead(&self, icon: super::icons::Icon) -> String {
        icon.lead(self.icons)
    }

    pub fn base(&self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }
    /// Body text without a background (inherits the surface or modal it sits on).
    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text)
    }
    /// Quiet text. On the terminal palette that is the terminal's own faint foreground: ANSI 8
    /// "bright black" is invisible on some palettes (1.00:1 on Solarized dark), faint never is.
    pub fn dim_style(&self) -> Style {
        match self.dim {
            Color::Rgb(..) => Style::default().fg(self.dim),
            _ => Style::default().fg(Color::Reset).add_modifier(Modifier::DIM),
        }
    }
    pub fn strong_style(&self) -> Style {
        Style::default().fg(self.strong).add_modifier(Modifier::BOLD)
    }
    /// A filled chip: `color` as the background, the surface as ink. Where the surface is the
    /// terminal's own default (the terminal palette and NO_COLOR themes) that ink would be the
    /// terminal's foreground — pale text on a pale ANSI green, about 1:1 — so the chip is drawn
    /// in reverse video instead, which always inks with the terminal's own background.
    pub fn chip(&self, color: Color) -> Style {
        match (self.surface, color) {
            (Color::Rgb(..), Color::Rgb(..)) => {
                let ink = if color == self.danger { self.on_danger } else { self.ink_on(color) };
                Style::default().fg(ink).bg(color).add_modifier(Modifier::BOLD)
            }
            _ => Style::default().fg(color).add_modifier(Modifier::BOLD | Modifier::REVERSED),
        }
    }
    /// Black or white, whichever reads best on `fill`.
    pub fn ink_on(&self, fill: Color) -> Color {
        match fill {
            Color::Rgb(r, g, b) => {
                if contrast((0, 0, 0), (r, g, b)) >= contrast((255, 255, 255), (r, g, b)) {
                    Color::Rgb(0, 0, 0)
                } else {
                    Color::Rgb(255, 255, 255)
                }
            }
            _ => self.on_accent,
        }
    }
    /// A panel's border: the lit accent when focused, the quiet `line` at rest.
    pub fn border(&self, focused: bool) -> Style {
        if focused { Style::default().fg(self.focus).add_modifier(Modifier::BOLD) } else { Style::default().fg(self.line) }
    }
    /// Body-text contrast ratio against the surface (hex themes only).
    pub fn text_contrast(&self) -> Option<f64> {
        match (self.text, self.surface) {
            (Color::Rgb(a, b, c), Color::Rgb(d, e, f)) => Some(contrast((a, b, c), (d, e, f))),
            _ => None,
        }
    }
    /// Hex of a role color for effect arguments (`rrggbb`).
    pub fn hex(c: Color) -> Option<String> {
        match c {
            Color::Rgb(r, g, b) => Some(format!("{r:02x}{g:02x}{b:02x}")),
            _ => None,
        }
    }
    pub fn selected(&self) -> Style {
        if self.monochrome {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(self.selection).fg(self.strong).add_modifier(Modifier::BOLD)
        }
    }
}

/// Omarchy's active theme palette file.
pub fn omarchy_colors() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let path = Path::new(&home).join(".local/state/omarchy/current/theme/colors.toml");
    path.exists().then_some(path)
}

/// Theme directories searched for named themes.
fn theme_dirs(ctx_home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![ctx_home.join("themes")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".config/omarchy/themes"));
        dirs.push(Path::new(&home).join(".local/share/omarchy/themes"));
    }
    dirs
}

/// Built-in palettes shipped with the wallet (see `themes::CATALOG`).
pub fn builtin(name: &str) -> Option<BTreeMap<String, String>> {
    super::themes::find(name).and_then(|t| parse_colors(t.colors).ok())
}

/// Resolve the configured theme. `light_hint` comes from the terminal background probe.
pub fn resolve(ctx_home: &Path, setting: &str, light_hint: bool, no_color: bool) -> (Theme, Option<PathBuf>) {
    if no_color {
        return (Theme::mono(), None);
    }
    let load_file = |path: &Path, name: &str| -> Option<Theme> {
        let text = std::fs::read_to_string(path).ok()?;
        let keys = parse_colors(&text).ok()?;
        Theme::from_palette(name, &path.display().to_string(), &keys)
    };
    match setting {
        "terminal" => return (Theme::terminal(light_hint), None),
        // No color at all: every meaning carried by a glyph or a word.
        "monochrome" => return (Theme::mono(), None),
        "auto" | "" => {
            if let Some(path) = omarchy_colors()
                && let Some(t) = load_file(&path, "omarchy")
            {
                return (t, Some(path));
            }
            // Without Omarchy, the house theme for the terminal's light or dark background. The
            // terminal palette is a choice (`terminal`), never a default: its greys and ANSI
            // colors have unknown contrast, and a wallet can't guess at what an Approve reads as.
            return (house(light_hint), None);
        }
        other => {
            if let Some(keys) = builtin(other)
                && let Some(t) = Theme::from_palette(other, "built-in", &keys)
            {
                return (t, None);
            }
            let as_path = Path::new(other);
            if as_path.exists() {
                let file = if as_path.is_dir() { as_path.join("colors.toml") } else { as_path.to_path_buf() };
                if let Some(t) = load_file(&file, other) {
                    return (t, Some(file));
                }
            }
            for dir in theme_dirs(ctx_home) {
                let file = dir.join(other).join("colors.toml");
                if let Some(t) = load_file(&file, other) {
                    return (t, Some(file));
                }
            }
        }
    }
    (house(light_hint), None)
}

/// Quai Dark, or Quai Light on a light terminal.
fn house(light: bool) -> Theme {
    let id = if light { "quai-light" } else { "quai-dark" };
    builtin(id).and_then(|keys| Theme::from_palette(id, "built-in", &keys)).unwrap_or_else(|| Theme::terminal(light))
}

/// All discoverable theme names.
pub fn available(ctx_home: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        vec![("auto".into(), "Omarchy current theme, else Quai Dark or Light".into()), ("terminal".into(), "terminal ANSI palette".into())];
    out.extend(super::themes::CATALOG.iter().map(|t| (t.id.to_string(), format!("built-in · {}", t.family))));
    for dir in theme_dirs(ctx_home) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if e.path().join("colors.toml").exists() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if !out.iter().any(|(n, _)| *n == name) {
                        out.push((name, dir.display().to_string()));
                    }
                }
            }
        }
    }
    out
}

pub fn list_cmd(ctx: &Ctx) -> Result<()> {
    let themes = available(ctx.paths.root());
    if ctx.out.json() {
        ctx.out.emit("theme list", &themes.iter().map(|(n, s)| serde_json::json!({"name": n, "source": s})).collect::<Vec<_>>());
        return Ok(());
    }
    for (name, source) in themes {
        let mark = if name == ctx.config.theme { "*" } else { " " };
        println!("{mark} {name:<18} {}", ctx.out.dim(&source));
    }
    Ok(())
}

pub fn show_cmd(ctx: &Ctx) -> Result<()> {
    let (theme, file) = resolve(ctx.paths.root(), &ctx.config.theme, false, ctx.global.no_color);
    let fmt = |c: Color| format!("{c:?}");
    let value = serde_json::json!({
        "name": theme.name, "source": theme.source, "file": file, "light": theme.light,
        "contrast_adjusted": theme.adjusted,
        "roles": {"surface": fmt(theme.surface), "text": fmt(theme.text), "strong": fmt(theme.strong), "dim": fmt(theme.dim),
            "focus": fmt(theme.focus), "selection": fmt(theme.selection), "ledger.quai": fmt(theme.quai), "ledger.qi": fmt(theme.qi),
            "state.ok": fmt(theme.ok), "state.pending": fmt(theme.pending), "state.attention": fmt(theme.attention), "state.danger": fmt(theme.danger)}
    });
    if ctx.out.json() {
        ctx.out.emit("theme show", &value);
    } else {
        println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
    }
    Ok(())
}

pub fn preview_cmd(ctx: &Ctx, name: Option<&str>) -> Result<()> {
    let setting = name.unwrap_or(&ctx.config.theme);
    let (theme, _) = resolve(ctx.paths.root(), setting, false, ctx.global.no_color);
    let paint = |c: Color, label: &str| -> String {
        match c {
            Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m{label}\x1b[0m"),
            Color::Reset => label.to_string(),
            other => format!("{label} ({other:?})"),
        }
    };
    println!("theme {} ({}){}", theme.name, theme.source, if theme.adjusted { " — contrast adjusted" } else { "" });
    println!(
        "  {}  {}  {}  {}",
        paint(theme.quai, "QUAI 1,204.5"),
        paint(theme.qi, "Qi 386.286"),
        paint(theme.focus, "▌focus"),
        paint(theme.dim, "muted")
    );
    println!(
        "  {}  {}  {}  {}",
        paint(theme.ok, "✓ confirmed"),
        paint(theme.pending, "◌ pending"),
        paint(theme.attention, "! refunded"),
        paint(theme.danger, "✕ failed")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a palette whose color 8 is the background (solarized-dark), lines and the selection
    /// move to colors that show; with truecolor, modals get a raised surface and a shadow.
    #[test]
    fn the_terminal_theme_fits_a_faint_palette() {
        let mut t = Theme::terminal(false);
        let base03 = (0x00, 0x2b, 0x36);
        t.fit_to_terminal(Some(base03), Some(base03), false);
        assert_eq!((t.line, t.selection), (Color::Gray, Color::Blue));
        assert_eq!(t.raised, Color::Reset, "no truecolor, no painted surface");
        let mut t = Theme::terminal(false);
        t.fit_to_terminal(Some((0x1e, 0x1e, 0x2e)), Some((0x58, 0x5b, 0x70)), true);
        assert_eq!(t.line, Color::DarkGray, "a color 8 that shows is kept");
        assert!(matches!(t.raised, Color::Rgb(..)) && matches!(t.shadow, Color::Rgb(..)));
        let mut named = resolve(std::path::Path::new("/nonexistent"), "quai-red", false, false).0;
        let before = named.clone().raised;
        named.fit_to_terminal(Some(base03), Some(base03), true);
        assert_eq!(named.raised, before, "only the terminal theme is fitted");
    }

    #[test]
    fn builtins_parse_and_meet_contrast() {
        for name in super::super::themes::CATALOG.iter().map(|t| t.id) {
            let keys = builtin(name).unwrap();
            let t = Theme::from_palette(name, "test", &keys).unwrap();
            if let (Color::Rgb(sr, sg, sb), Color::Rgb(br, bgc, bb)) = (t.strong, t.surface) {
                assert!(contrast((sr, sg, sb), (br, bgc, bb)) >= 7.0, "{name}");
            }
        }
    }

    #[test]
    fn catalog_state_colors_are_legible() {
        for entry in super::super::themes::CATALOG {
            let t = Theme::from_palette(entry.id, "test", &builtin(entry.id).unwrap()).unwrap();
            let Color::Rgb(br, bg, bb) = t.surface else { panic!() };
            let text = t.text_contrast().unwrap();
            assert!(text >= 4.5, "{} text {text:.2}", entry.id);
            let Color::Rgb(rr, rg, rb) = t.raised else { panic!() };
            let Color::Rgb(sr, sg, sb) = t.selection else { panic!() };
            let rgb = |c: Color| match c {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => panic!(),
            };
            for (role, c) in [
                ("quai", t.quai),
                ("qi", t.qi),
                ("ok", t.ok),
                ("pending", t.pending),
                ("attention", t.attention),
                ("danger", t.danger),
                ("link", t.link),
                ("focus", t.focus),
            ] {
                // State colors are text; the accent is also lines and marks.
                let min = if role == "focus" { 3.0 } else { 4.5 };
                for (bg_name, back) in [("surface", (br, bg, bb)), ("raised", (rr, rg, rb))] {
                    let ratio = contrast(rgb(c), back);
                    assert!(ratio >= min, "{} {role} on {bg_name} {ratio:.2}", entry.id);
                }
            }
            assert!(contrast(rgb(t.dim), (br, bg, bb)) >= 4.5, "{} dim on surface", entry.id);
            assert!(contrast(rgb(t.dim), (rr, rg, rb)) >= 4.5 - 0.3, "{} dim on raised", entry.id);
            assert!(contrast(rgb(t.dim), (sr, sg, sb)) >= 3.0, "{} dim on selection", entry.id);
            assert!(contrast(rgb(t.strong), (sr, sg, sb)) >= 4.5, "{} strong on selection", entry.id);
        }
    }

    /// Every filled chip (the focused button, Approve & sign, a danger pill) carries ink that
    /// reads on its fill, in every theme.
    #[test]
    fn catalog_fills_carry_readable_ink() {
        for entry in super::super::themes::CATALOG {
            let t = Theme::from_palette(entry.id, "test", &builtin(entry.id).unwrap()).unwrap();
            for (role, fill) in
                [("focus", t.focus), ("ok", t.ok), ("danger", t.danger), ("attention", t.attention), ("quai", t.quai), ("qi", t.qi)]
            {
                let style = t.chip(fill);
                let (Some(Color::Rgb(fr, fg, fb)), Some(Color::Rgb(br, bg, bb))) = (style.fg, style.bg) else {
                    panic!("{} {role}", entry.id)
                };
                let ratio = contrast((fr, fg, fb), (br, bg, bb));
                assert!(ratio >= 4.5, "{} ink on {role} {ratio:.2}", entry.id);
            }
        }
    }

    /// Colors that mean different things look different, in every theme.
    #[test]
    fn catalog_meanings_are_distinguishable() {
        let rgb = |c: Color| match c {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => panic!(),
        };
        for entry in super::super::themes::CATALOG {
            let t = Theme::from_palette(entry.id, "test", &builtin(entry.id).unwrap()).unwrap();
            for (a, an, b, bn, min) in [
                (t.danger, "danger", t.focus, "focus", 0.1),
                (t.attention, "attention", t.danger, "danger", 0.1),
                (t.qi, "qi", t.quai, "quai", 0.1),
                (t.qi, "qi", t.focus, "focus", 0.1),
                (t.pending, "pending", t.attention, "attention", 0.08),
                (t.danger, "danger", t.ok, "ok", 0.1),
            ] {
                let d = distance(rgb(a), rgb(b));
                // Contrast guards may pull a turned color back a little.
                assert!(d >= min - 0.015, "{}: {an} and {bn} look alike ({d:.3})", entry.id);
            }
        }
    }

    /// Quiet lines are quieter than dim text, and dim text quieter than text.
    #[test]
    fn catalog_lines_sit_below_text() {
        for entry in super::super::themes::CATALOG {
            let t = Theme::from_palette(entry.id, "test", &builtin(entry.id).unwrap()).unwrap();
            let (Color::Rgb(lr, lg, lb), Color::Rgb(br, bg, bb), Color::Rgb(dr, dg, db)) = (t.line, t.surface, t.dim) else { panic!() };
            let line = contrast((lr, lg, lb), (br, bg, bb));
            assert!((1.5..=2.5).contains(&line), "{} line {line:.2}", entry.id);
            assert!(contrast((dr, dg, db), (br, bg, bb)) > line + 1.5, "{} dim vs line", entry.id);
        }
    }

    #[test]
    fn low_contrast_theme_is_adjusted() {
        let keys = parse_colors("background = \"#222222\"\nforeground = \"#333333\"\n").unwrap();
        let t = Theme::from_palette("bad", "test", &keys).unwrap();
        assert!(t.adjusted);
        if let (Color::Rgb(r, g, b), Color::Rgb(br, bg, bb)) = (t.strong, t.surface) {
            assert!(contrast((r, g, b), (br, bg, bb)) >= 7.0);
        }
    }

    #[test]
    fn legacy_aliases_and_mode() {
        let keys = parse_colors("mode = \"light\"\nbg = \"#ffffff\"\nfg = \"#000000\"\n").unwrap();
        let t = Theme::from_palette("x", "t", &keys).unwrap();
        assert!(t.light);
    }
}
