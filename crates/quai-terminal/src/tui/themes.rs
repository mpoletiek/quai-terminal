//! Built-in theme catalog in the Omarchy `colors.toml` schema.
//!
//! Palettes follow each project's published colors. The wallet maps them onto semantic roles:
//! `blue` → QUAI, `magenta` → Qi, `green`/`yellow`/`orange`/`red` → states, `accent` → focus.
//! Contrast is still enforced by `Theme::from_palette`, so muted comment colors stay readable.

/// One catalog entry.
pub struct BuiltinTheme {
    pub id: &'static str,
    pub name: &'static str,
    pub family: &'static str,
    pub colors: &'static str,
}

macro_rules! theme {
    ($id:literal, $name:literal, $family:literal, $colors:literal) => {
        BuiltinTheme { id: $id, name: $name, family: $family, colors: $colors }
    };
}

pub const CATALOG: &[BuiltinTheme] = &[
    // Okabe–Ito: every meaning pair stays apart for the common color-vision deficiencies. Up
    // and ok are sky blue, down and danger vermillion, never red against green.
    theme!(
        "colorblind-safe",
        "Colorblind safe",
        "Accessibility",
        r##"
mode = "dark"
accent = "#e69f00"
selection = "#243447"
muted = "#8a93a0"
background = "#0d1117"
lighter_background = "#161b22"
foreground = "#e6edf3"
bright_foreground = "#ffffff"
red = "#ff7a33"
orange = "#e69f00"
yellow = "#f0e442"
green = "#56b4e9"
cyan = "#a6d8f5"
blue = "#3a9bdc"
magenta = "#cc79a7"
"##
    ),
    theme!(
        "quai-red",
        "Quai Red",
        "Quai",
        r##"
mode = "dark"
accent = "#ff3a14"
selection = "#3d120b"
muted = "#8f817d"
background = "#080606"
lighter_background = "#151010"
foreground = "#efe6e3"
bright_foreground = "#ffffff"
red = "#ff4f8b"
orange = "#ffb000"
yellow = "#f5dc6b"
green = "#5fd38d"
cyan = "#6cb6ff"
blue = "#ff5a2e"
magenta = "#b48cff"
"##
    ),
    theme!(
        "quai-dark",
        "Quai Dark",
        "Quai",
        r##"
mode = "dark"
accent = "#4fd1c5"
selection = "#23394a"
muted = "#7a8699"
background = "#0f141a"
lighter_background = "#161d26"
foreground = "#d7dde5"
bright_foreground = "#f4f7fa"
red = "#f0626d"
orange = "#f39a4a"
yellow = "#e7c56b"
green = "#7bd88f"
cyan = "#5ccfe6"
blue = "#6ea8fe"
magenta = "#c792ea"
"##
    ),
    theme!(
        "quai-light",
        "Quai Light",
        "Quai",
        r##"
mode = "light"
accent = "#0f766e"
selection = "#cfe8e5"
muted = "#5b6675"
background = "#fbfbf8"
lighter_background = "#f0f1ec"
foreground = "#1f2933"
bright_foreground = "#05080b"
red = "#c0343f"
orange = "#b75a0b"
yellow = "#8a6500"
green = "#2f7d3a"
cyan = "#0b7285"
blue = "#1d4ed8"
magenta = "#8b3dbf"
"##
    ),
    theme!(
        "high-contrast",
        "High Contrast",
        "Accessibility",
        r##"
mode = "dark"
accent = "#00ffff"
selection = "#003a4d"
muted = "#bbbbbb"
background = "#000000"
foreground = "#ffffff"
bright_foreground = "#ffffff"
red = "#ff5555"
orange = "#ffaa00"
yellow = "#ffff55"
green = "#55ff55"
cyan = "#55ffff"
blue = "#8888ff"
magenta = "#ff77ff"
"##
    ),
    theme!(
        "tokyo-night",
        "Tokyo Night",
        "Tokyo Night",
        r##"
mode = "dark"
accent = "#7aa2f7"
selection = "#283457"
muted = "#737aa2"
background = "#1a1b26"
lighter_background = "#1f2335"
foreground = "#c0caf5"
bright_foreground = "#d5daf7"
red = "#f7768e"
orange = "#ff9e64"
yellow = "#e0af68"
green = "#9ece6a"
cyan = "#7dcfff"
blue = "#7aa2f7"
magenta = "#bb9af7"
"##
    ),
    theme!(
        "tokyo-night-storm",
        "Tokyo Night Storm",
        "Tokyo Night",
        r##"
mode = "dark"
accent = "#7aa2f7"
selection = "#2e3c64"
muted = "#737aa2"
background = "#24283b"
lighter_background = "#292e42"
foreground = "#c0caf5"
bright_foreground = "#d5daf7"
red = "#f7768e"
orange = "#ff9e64"
yellow = "#e0af68"
green = "#9ece6a"
cyan = "#7dcfff"
blue = "#7aa2f7"
magenta = "#bb9af7"
"##
    ),
    theme!(
        "tokyo-night-moon",
        "Tokyo Night Moon",
        "Tokyo Night",
        r##"
mode = "dark"
accent = "#82aaff"
selection = "#2d3f76"
muted = "#828bb8"
background = "#222436"
lighter_background = "#2f334d"
foreground = "#c8d3f5"
bright_foreground = "#e0e6fb"
red = "#ff757f"
orange = "#ff966c"
yellow = "#ffc777"
green = "#c3e88d"
cyan = "#86e1fc"
blue = "#82aaff"
magenta = "#c099ff"
"##
    ),
    theme!(
        "tokyo-night-day",
        "Tokyo Night Day",
        "Tokyo Night",
        r##"
mode = "light"
accent = "#2e7de9"
selection = "#b6bfe2"
muted = "#6172b0"
background = "#e1e2e7"
lighter_background = "#d0d5e3"
foreground = "#3760bf"
bright_foreground = "#1f3a7a"
red = "#c51f4f"
orange = "#a14f00"
yellow = "#7a5a2a"
green = "#4a6630"
cyan = "#006a8e"
blue = "#2e62c9"
magenta = "#8440e0"
"##
    ),
    theme!(
        "catppuccin-mocha",
        "Catppuccin Mocha",
        "Catppuccin",
        r##"
mode = "dark"
accent = "#b4befe"
selection = "#45475a"
muted = "#9399b2"
background = "#1e1e2e"
lighter_background = "#313244"
foreground = "#cdd6f4"
bright_foreground = "#e6ecff"
red = "#f38ba8"
orange = "#fab387"
yellow = "#f9e2af"
green = "#a6e3a1"
cyan = "#94e2d5"
blue = "#89b4fa"
magenta = "#cba6f7"
"##
    ),
    theme!(
        "catppuccin-macchiato",
        "Catppuccin Macchiato",
        "Catppuccin",
        r##"
mode = "dark"
accent = "#b7bdf8"
selection = "#494d64"
muted = "#939ab7"
background = "#24273a"
lighter_background = "#363a4f"
foreground = "#cad3f5"
bright_foreground = "#e4e9fb"
red = "#ed8796"
orange = "#f5a97f"
yellow = "#eed49f"
green = "#a6da95"
cyan = "#8bd5ca"
blue = "#8aadf4"
magenta = "#c6a0f6"
"##
    ),
    theme!(
        "catppuccin-frappe",
        "Catppuccin Frappé",
        "Catppuccin",
        r##"
mode = "dark"
accent = "#babbf1"
selection = "#51576d"
muted = "#949cbb"
background = "#303446"
lighter_background = "#414559"
foreground = "#c6d0f5"
bright_foreground = "#e3e8fb"
red = "#e78284"
orange = "#ef9f76"
yellow = "#e5c890"
green = "#a6d189"
cyan = "#81c8be"
blue = "#8caaee"
magenta = "#ca9ee6"
"##
    ),
    theme!(
        "catppuccin-latte",
        "Catppuccin Latte",
        "Catppuccin",
        r##"
mode = "light"
accent = "#5467e0"
selection = "#bcc0cc"
muted = "#6c6f85"
background = "#eff1f5"
lighter_background = "#e6e9ef"
foreground = "#4c4f69"
bright_foreground = "#2c2e40"
red = "#d20f39"
orange = "#c64f06"
yellow = "#a36612"
green = "#3a8f26"
cyan = "#137c82"
blue = "#1e66f5"
magenta = "#8839ef"
"##
    ),
    theme!(
        "gruvbox-dark",
        "Gruvbox Dark",
        "Gruvbox",
        r##"
mode = "dark"
accent = "#fabd2f"
selection = "#504945"
muted = "#a89984"
background = "#282828"
lighter_background = "#3c3836"
foreground = "#ebdbb2"
bright_foreground = "#fbf1c7"
red = "#fb4934"
orange = "#fe8019"
yellow = "#fabd2f"
green = "#b8bb26"
cyan = "#8ec07c"
blue = "#83a598"
magenta = "#d3869b"
"##
    ),
    theme!(
        "gruvbox-light",
        "Gruvbox Light",
        "Gruvbox",
        r##"
mode = "light"
accent = "#076678"
selection = "#d5c4a1"
muted = "#665c54"
background = "#fbf1c7"
lighter_background = "#ebdbb2"
foreground = "#3c3836"
bright_foreground = "#1d2021"
red = "#9d0006"
orange = "#af3a03"
yellow = "#8a5a0e"
green = "#5f5c0a"
cyan = "#427b58"
blue = "#076678"
magenta = "#8f3f71"
"##
    ),
    theme!(
        "nord",
        "Nord",
        "Nord",
        r##"
mode = "dark"
accent = "#88c0d0"
selection = "#434c5e"
muted = "#8a93a6"
background = "#2e3440"
lighter_background = "#3b4252"
foreground = "#d8dee9"
bright_foreground = "#eceff4"
red = "#bf616a"
orange = "#d08770"
yellow = "#ebcb8b"
green = "#a3be8c"
cyan = "#8fbcbb"
blue = "#81a1c1"
magenta = "#b48ead"
"##
    ),
    theme!(
        "dracula",
        "Dracula",
        "Dracula",
        r##"
mode = "dark"
accent = "#bd93f9"
selection = "#44475a"
muted = "#7f8bbd"
background = "#282a36"
lighter_background = "#343746"
foreground = "#f8f8f2"
bright_foreground = "#ffffff"
red = "#ff5555"
orange = "#ffb86c"
yellow = "#f1fa8c"
green = "#50fa7b"
cyan = "#8be9fd"
blue = "#8be9fd"
magenta = "#ff79c6"
"##
    ),
    theme!(
        "rose-pine",
        "Rosé Pine",
        "Rosé Pine",
        r##"
mode = "dark"
accent = "#ebbcba"
selection = "#403d52"
muted = "#908caa"
background = "#191724"
lighter_background = "#1f1d2e"
foreground = "#e0def4"
bright_foreground = "#f4f2ff"
red = "#eb6f92"
orange = "#ebbcba"
yellow = "#f6c177"
green = "#9ccfd8"
cyan = "#9ccfd8"
blue = "#5ba3c0"
magenta = "#c4a7e7"
"##
    ),
    theme!(
        "rose-pine-moon",
        "Rosé Pine Moon",
        "Rosé Pine",
        r##"
mode = "dark"
accent = "#ea9a97"
selection = "#44415a"
muted = "#908caa"
background = "#232136"
lighter_background = "#2a273f"
foreground = "#e0def4"
bright_foreground = "#f4f2ff"
red = "#eb6f92"
orange = "#ea9a97"
yellow = "#f6c177"
green = "#9ccfd8"
cyan = "#9ccfd8"
blue = "#5aa7cb"
magenta = "#c4a7e7"
"##
    ),
    theme!(
        "rose-pine-dawn",
        "Rosé Pine Dawn",
        "Rosé Pine",
        r##"
mode = "light"
accent = "#b4637a"
selection = "#dfdad9"
muted = "#6e6a86"
background = "#faf4ed"
lighter_background = "#fffaf3"
foreground = "#575279"
bright_foreground = "#393552"
red = "#b4637a"
orange = "#b8602e"
yellow = "#9a6212"
green = "#286983"
cyan = "#3f7680"
blue = "#286983"
magenta = "#7c6598"
"##
    ),
    theme!(
        "kanagawa-wave",
        "Kanagawa Wave",
        "Kanagawa",
        r##"
mode = "dark"
accent = "#7e9cd8"
selection = "#2d4f67"
muted = "#8a8980"
background = "#1f1f28"
lighter_background = "#2a2a37"
foreground = "#dcd7ba"
bright_foreground = "#f2ecce"
red = "#e46876"
orange = "#ffa066"
yellow = "#e6c384"
green = "#98bb6c"
cyan = "#7aa89f"
blue = "#7e9cd8"
magenta = "#957fb8"
"##
    ),
    theme!(
        "kanagawa-dragon",
        "Kanagawa Dragon",
        "Kanagawa",
        r##"
mode = "dark"
accent = "#8ba4b0"
selection = "#2d4f67"
muted = "#8a9189"
background = "#181616"
lighter_background = "#282727"
foreground = "#c5c9c5"
bright_foreground = "#e6e8e6"
red = "#c4746e"
orange = "#b6927b"
yellow = "#c4b28a"
green = "#8a9a7b"
cyan = "#8ea4a2"
blue = "#8ba4b0"
magenta = "#a292a3"
"##
    ),
    theme!(
        "everforest-dark",
        "Everforest Dark",
        "Everforest",
        r##"
mode = "dark"
accent = "#a7c080"
selection = "#475258"
muted = "#9da9a0"
background = "#2d353b"
lighter_background = "#343f44"
foreground = "#d3c6aa"
bright_foreground = "#ece0c6"
red = "#e67e80"
orange = "#e69875"
yellow = "#dbbc7f"
green = "#a7c080"
cyan = "#83c092"
blue = "#7fbbb3"
magenta = "#d699b6"
"##
    ),
    theme!(
        "everforest-light",
        "Everforest Light",
        "Everforest",
        r##"
mode = "light"
accent = "#6b8a26"
selection = "#e6e2cc"
muted = "#707b74"
background = "#fdf6e3"
lighter_background = "#f4f0d9"
foreground = "#5c6a72"
bright_foreground = "#3a454a"
red = "#d13d3d"
orange = "#b75d15"
yellow = "#8f6c00"
green = "#5f7a14"
cyan = "#2f7d5a"
blue = "#2f7390"
magenta = "#b04f84"
"##
    ),
    theme!(
        "one-dark",
        "One Dark",
        "Atom",
        r##"
mode = "dark"
accent = "#61afef"
selection = "#3e4451"
muted = "#848b98"
background = "#282c34"
lighter_background = "#2c313a"
foreground = "#abb2bf"
bright_foreground = "#d7dae0"
red = "#e06c75"
orange = "#d19a66"
yellow = "#e5c07b"
green = "#98c379"
cyan = "#56b6c2"
blue = "#61afef"
magenta = "#c678dd"
"##
    ),
    theme!(
        "solarized-dark",
        "Solarized Dark",
        "Solarized",
        r##"
mode = "dark"
accent = "#268bd2"
selection = "#073642"
muted = "#7c8f91"
background = "#002b36"
lighter_background = "#073642"
foreground = "#93a1a1"
bright_foreground = "#eee8d5"
red = "#dc322f"
orange = "#cb4b16"
yellow = "#b58900"
green = "#859900"
cyan = "#2aa198"
blue = "#268bd2"
magenta = "#d33682"
"##
    ),
    theme!(
        "solarized-light",
        "Solarized Light",
        "Solarized",
        r##"
mode = "light"
accent = "#1f6fae"
selection = "#eee8d5"
muted = "#5f7178"
background = "#fdf6e3"
lighter_background = "#eee8d5"
foreground = "#4f6068"
bright_foreground = "#073642"
red = "#c42b28"
orange = "#a93c10"
yellow = "#846400"
green = "#5f6e00"
cyan = "#1d7a73"
blue = "#1f6fae"
magenta = "#b02a6b"
"##
    ),
    theme!(
        "nightfox",
        "Nightfox",
        "Nightfox",
        r##"
mode = "dark"
accent = "#719cd6"
selection = "#2b3b51"
muted = "#8a96a5"
background = "#192330"
lighter_background = "#212e3f"
foreground = "#cdcecf"
bright_foreground = "#e4e4e5"
red = "#c94f6d"
orange = "#f4a261"
yellow = "#dbc074"
green = "#81b29a"
cyan = "#63cdcf"
blue = "#719cd6"
magenta = "#9d79d6"
"##
    ),
    theme!(
        "monokai-pro",
        "Monokai Pro",
        "Monokai",
        r##"
mode = "dark"
accent = "#ffd866"
selection = "#403e41"
muted = "#939293"
background = "#2d2a2e"
lighter_background = "#363337"
foreground = "#fcfcfa"
bright_foreground = "#ffffff"
red = "#ff6188"
orange = "#fc9867"
yellow = "#ffd866"
green = "#a9dc76"
cyan = "#78dce8"
blue = "#78dce8"
magenta = "#ab9df2"
"##
    ),
    theme!(
        "ayu-mirage",
        "Ayu Mirage",
        "Ayu",
        r##"
mode = "dark"
accent = "#ffcc66"
selection = "#33415e"
muted = "#8a9199"
background = "#1f2430"
lighter_background = "#242936"
foreground = "#cccac2"
bright_foreground = "#e6e4dd"
red = "#f28779"
orange = "#ffad66"
yellow = "#ffd173"
green = "#d5ff80"
cyan = "#95e6cb"
blue = "#73d0ff"
magenta = "#dfbfff"
"##
    ),
    theme!(
        "flexoki-light",
        "Flexoki Light",
        "Flexoki",
        r##"
mode = "light"
accent = "#205ea6"
selection = "#e6e4d9"
muted = "#6f6e69"
background = "#fffcf0"
lighter_background = "#f2f0e5"
foreground = "#100f0f"
bright_foreground = "#000000"
red = "#af3029"
orange = "#bc5215"
yellow = "#8e6b01"
green = "#66800b"
cyan = "#24837b"
blue = "#205ea6"
magenta = "#a02f6f"
"##
    ),
];

pub fn find(id: &str) -> Option<&'static BuiltinTheme> {
    CATALOG.iter().find(|t| t.id == id)
}

/// Genesis: the session-only theme the Konami code gives (in Help). Quai red on true black, the
/// lines drawn in red. Never saved; the next start is the chosen theme again.
pub fn genesis(from: &super::theme::Theme) -> super::theme::Theme {
    use ratatui::style::Color::Rgb;
    let mut t = super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-red", false, false).0;
    t.name = "Genesis".into();
    t.surface = Rgb(0, 0, 0);
    t.raised = Rgb(14, 4, 7);
    t.selection = Rgb(52, 10, 22);
    t.line = Rgb(74, 18, 32);
    t.line_strong = Rgb(122, 28, 52);
    t.shadow = Rgb(0, 0, 0);
    t.icons = from.icons;
    t
}
