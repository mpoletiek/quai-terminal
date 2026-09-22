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
    /// Colors for effects/charts (hex-capable themes only).
    pub accent_rgb: Option<(u8, u8, u8)>,
    pub adjusted: bool,
    pub monochrome: bool,
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
            accent_rgb: None,
            adjusted: false,
            monochrome: false,
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
        ] {
            *c = Color::Reset;
        }
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
        let state = |c: (u8, u8, u8)| ensure(c, &[background, raised], 3.0, extreme);
        let (red, green, yellow, orange, blue, magenta, cyan, accent) =
            (state(red), state(green), state(yellow), state(orange), state(blue), state(magenta), state(cyan), state(accent));
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
            accent_rgb: Some(accent),
            adjusted,
            monochrome: false,
        })
    }

    pub fn base(&self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }
    /// Body text without a background (inherits the surface or modal it sits on).
    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text)
    }
    pub fn dim_style(&self) -> Style {
        Style::default().fg(self.dim)
    }
    pub fn strong_style(&self) -> Style {
        Style::default().fg(self.strong).add_modifier(Modifier::BOLD)
    }
    pub fn border(&self, focused: bool) -> Style {
        if focused { Style::default().fg(self.focus).add_modifier(Modifier::BOLD) } else { Style::default().fg(self.dim) }
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
        "auto" | "" => {
            if let Some(path) = omarchy_colors()
                && let Some(t) = load_file(&path, "omarchy")
            {
                return (t, Some(path));
            }
            return (Theme::terminal(light_hint), None);
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
    (Theme::terminal(light_hint), None)
}

/// All discoverable theme names.
pub fn available(ctx_home: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        vec![("auto".into(), "Omarchy current theme, else terminal".into()), ("terminal".into(), "terminal ANSI palette".into())];
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
        paint(theme.pending, "◔ pending"),
        paint(theme.attention, "! refunded"),
        paint(theme.danger, "✕ failed")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ("focus", t.focus),
            ] {
                for (bg_name, back) in [("surface", (br, bg, bb)), ("raised", (rr, rg, rb))] {
                    let ratio = contrast(rgb(c), back);
                    assert!(ratio >= 3.0, "{} {role} on {bg_name} {ratio:.2}", entry.id);
                }
            }
            assert!(contrast(rgb(t.dim), (br, bg, bb)) >= 4.5, "{} dim on surface", entry.id);
            assert!(contrast(rgb(t.dim), (rr, rg, rb)) >= 4.5 - 0.3, "{} dim on raised", entry.id);
            assert!(contrast(rgb(t.dim), (sr, sg, sb)) >= 3.0, "{} dim on selection", entry.id);
            assert!(contrast(rgb(t.strong), (sr, sg, sb)) >= 4.5, "{} strong on selection", entry.id);
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
