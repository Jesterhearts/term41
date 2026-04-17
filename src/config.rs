use std::path::PathBuf;
use std::str::FromStr;

use palette::Srgb;
use serde::Deserialize;
use terminal41::ColorPalette;
use terminal41::CursorShape;
use terminal41::CursorStyle;
use wgpu::PowerPreference;

use crate::keybindings::Keybinding;
use crate::keybindings::KeybindingConfig;
use crate::keybindings::Keybindings;

pub const DEFAULT_SCROLLBACK: u32 = 10_000;

/// VSync mode for frame presentation. See the `vsync` config key and the
/// `Config::vsync` field for details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VSync {
    /// Let the OS decide when to present frames. This is the default and
    /// usually means "sync to the display's refresh rate", but some
    /// platforms may choose a different strategy.
    #[default]
    Auto,
    /// Try using fast-vsync or similar techniques to present frames immediately
    /// when they're ready, without screen tearing.
    Fast,
    /// Present frames as soon as they're ready, even if that means
    /// tearing.
    Off,
    /// Wait for the next vertical blanking interval before presenting each
    /// frame. Eliminates tearing at the cost of increased latency and
    /// potential stuttering if the render time exceeds the display's refresh
    /// period.
    On,
}

/// What to do when the foreground app rings the bell (BEL / `\x07`).
///
/// Default is [`BellMode::Off`] because shells like bash ring the bell on
/// completion-ambiguity by default — most users find that surprising
/// rather than useful out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BellMode {
    /// Ignore the bell entirely.
    #[default]
    #[serde(alias = "none", alias = "false")]
    Off,
    /// Briefly flash the screen.
    #[serde(alias = "flash")]
    Visual,
    /// Ask the compositor to mark the window as needing attention
    /// (taskbar bobbing on macOS, urgency hint on X11/Wayland). Quiet
    /// when the window is focused; eye-catching when it isn't.
    #[serde(alias = "attention")]
    Urgent,
}

// ---------------------------------------------------------------------------
// [colors] — Rio/Alacritty-format palette config
// ---------------------------------------------------------------------------

/// Top-level `[colors]` table in the config file.
#[derive(Deserialize, Default)]
struct ColorsConfig {
    /// Cursor color override (hex string, e.g. `"#009fff"`).
    cursor: Option<String>,
    /// `[colors.primary]` — default foreground / background.
    primary: Option<PrimaryColors>,
    /// `[colors.selection]` — selection highlight colors.
    selection: Option<SelectionColors>,
    /// `[colors.normal]` — the 8 standard ANSI colors.
    normal: Option<AnsiColors>,
    /// `[colors.bright]` — the 8 bright ANSI colors.
    bright: Option<AnsiColors>,
}

#[derive(Deserialize, Default)]
struct PrimaryColors {
    foreground: Option<String>,
    background: Option<String>,
}

#[derive(Deserialize, Default)]
struct SelectionColors {
    background: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct AnsiColors {
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
}

/// Try to parse a hex color, logging a warning on failure and returning the
/// provided fallback.
fn parse_color_or_default(
    s: &Option<String>,
    fallback: Srgb<u8>,
    label: &str,
) -> Srgb<u8> {
    match s {
        Some(string) => match palette::Srgb::from_str(string)
            .ok()
            .or_else(|| palette::named::from_str(string))
        {
            Some(c) => c,
            None => {
                warn!("invalid color for {label}: {string:?}; using default");
                fallback
            }
        },
        None => fallback,
    }
}

/// Build a [`ColorPalette`] from the deserialized `[colors]` config,
/// falling back to hardcoded defaults for any value not specified.
fn build_palette(colors: Option<ColorsConfig>) -> ColorPalette {
    let mut pal = ColorPalette::default();
    let Some(c) = colors else {
        return pal;
    };

    if let Some(ref p) = c.primary {
        pal.fg = parse_color_or_default(&p.foreground, pal.fg, "colors.primary.foreground");
        pal.bg = parse_color_or_default(&p.background, pal.bg, "colors.primary.background");
    }

    pal.cursor = c.cursor.as_ref().and_then(|s| {
        Srgb::from_str(s)
            .ok()
            .or_else(|| palette::named::from_str(s))
            .or_else(|| {
                warn!("invalid hex color for colors.cursor: {s:?}; ignoring");
                None
            })
    });

    if let Some(ref sel) = c.selection {
        pal.selection_bg = sel.background.as_ref().and_then(|s| {
            Srgb::from_str(s)
                .ok()
                .or_else(|| palette::named::from_str(s))
                .or_else(|| {
                    warn!("invalid hex color for colors.selection.background: {s:?}; ignoring");
                    None
                })
        });
        pal.selection_fg = sel.text.as_ref().and_then(|s| {
            Srgb::from_str(s)
                .ok()
                .or_else(|| palette::named::from_str(s))
                .or_else(|| {
                    warn!("invalid hex color for colors.selection.text: {s:?}; ignoring");
                    None
                })
        });
    }

    if let Some(ref n) = c.normal {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let fields = [
            &n.black, &n.red, &n.green, &n.yellow, &n.blue, &n.magenta, &n.cyan, &n.white,
        ];
        for (i, (field, name)) in fields.iter().zip(names.iter()).enumerate() {
            pal.ansi[i] =
                parse_color_or_default(field, pal.ansi[i], &format!("colors.normal.{name}"));
        }
    }

    if let Some(ref b) = c.bright {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let fields = [
            &b.black, &b.red, &b.green, &b.yellow, &b.blue, &b.magenta, &b.cyan, &b.white,
        ];
        for (i, (field, name)) in fields.iter().zip(names.iter()).enumerate() {
            pal.ansi[8 + i] =
                parse_color_or_default(field, pal.ansi[8 + i], &format!("colors.bright.{name}"));
        }
    }

    pal
}

#[derive(Deserialize)]
struct ConfigFile {
    #[serde(deserialize_with = "float_opt_clamp_0_1")]
    #[serde(default)]
    opacity: Option<f32>,
    #[serde(default)]
    fonts: Option<String>,
    #[serde(deserialize_with = "float_opt")]
    #[serde(default)]
    font_size: Option<f32>,
    #[serde(deserialize_with = "u32_opt")]
    #[serde(default)]
    scrollback_lines: Option<u32>,
    /// Cursor shape: `block`, `underline`, or `beam`.
    #[serde(deserialize_with = "cursor_shape_opt")]
    #[serde(default)]
    cursor_shape: Option<CursorShape>,
    /// Whether the cursor blinks. Defaults to true.
    #[serde(deserialize_with = "cursor_blink_opt")]
    #[serde(default)]
    cursor_blink: Option<bool>,
    /// Replace the default keybindings entirely. Setting an empty array
    /// disables all bindings — useful for debugging conflicts.
    #[serde(deserialize_with = "keybindings_opt")]
    #[serde(default)]
    keybindings: Option<Vec<KeybindingConfig>>,
    /// Bell behaviour: `off`, `visual`, or `urgent`.
    #[serde(deserialize_with = "bell_mode_opt")]
    #[serde(default)]
    bell: Option<BellMode>,
    /// Show the shell-integration gutter on the left edge — a thin strip
    /// where OSC 133 prompt rows get a coloured dot marking the last
    /// command's exit status. Defaults to on; disable for a pure
    /// terminal-text view or when the shell doesn't emit OSC 133 at all.
    #[serde(deserialize_with = "gutter_opt")]
    #[serde(default)]
    gutter: Option<bool>,
    /// Preferred power mode for the GPU. See wgpu::PowerPreference docs for
    /// details.
    #[serde(deserialize_with = "power_preference_opt")]
    #[serde(default)]
    power_preference: Option<PowerPreference>,
    /// Whether to enable vsync.
    #[serde(deserialize_with = "vsync_opt")]
    #[serde(default)]
    vsync: Option<VSync>,
    /// Override the monitor's DPI scale factor. When absent, the system
    /// scale factor is used automatically. Set to `1.0` to disable DPI
    /// scaling entirely.
    #[serde(deserialize_with = "float_opt")]
    #[serde(default)]
    dpi_scale: Option<f32>,
    /// Path to an image file to draw behind the terminal cells. PNG is
    /// always supported; GIF (including animated) requires the `ffmpeg`
    /// cargo feature. Cells with the default background colour become
    /// transparent over the image so it shows through; cells with an
    /// explicit SGR background still paint over the image.
    #[serde(default)]
    background_image: Option<PathBuf>,
    /// Multiplier applied to the background image's RGB. `1.0` paints the
    /// image at full brightness; `0.0` makes it invisible. Useful for
    /// dimming a busy wallpaper enough that text remains readable. The
    /// image's own alpha channel is preserved either way.
    #[serde(deserialize_with = "float_opt_clamp_0_1")]
    #[serde(default)]
    background_opacity: Option<f32>,

    /// Supersampling factor for font rasterization. Higher values produce
    /// smoother results at the cost of increased CPU usage and memory
    /// consumption. Default is 4.
    #[serde(deserialize_with = "u32_opt_clamp_1_16")]
    #[serde(default)]
    font_supersampling: Option<u32>,

    /// Color palette in Rio format.
    #[serde(default)]
    colors: Option<ColorsConfig>,
}

#[derive(Debug)]
pub struct Config {
    pub opacity: f32,
    pub fonts: Option<String>,
    pub font_size: f32,
    pub scrollback_lines: u32,
    pub cursor_style: CursorStyle,
    pub keybindings: Keybindings,
    pub bell: BellMode,
    pub gutter: bool,
    pub power_preference: PowerPreference,
    pub vsync: VSync,
    /// Override the monitor's DPI scale factor. `None` = automatic (use the
    /// system scale factor). `Some(x)` = use `x` regardless of monitor.
    pub dpi_scale: Option<f32>,
    /// Optional wallpaper image painted behind terminal cells.
    pub background_image: Option<PathBuf>,
    /// RGB multiplier applied to the background image. Always in `[0.0, 1.0]`.
    pub background_opacity: f32,
    /// Supersampling factor for font rasterization. Higher values produce
    /// smoother results at the cost of increased CPU usage and memory
    /// consumption. Default is 4.
    pub font_supersampling: i32,
    /// Color palette (ANSI 16 colors, default fg/bg, cursor, selection).
    pub palette: ColorPalette,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            fonts: None,
            font_size: 24.0,
            scrollback_lines: DEFAULT_SCROLLBACK,
            cursor_style: CursorStyle::default(),
            keybindings: Keybindings::defaults(),
            bell: BellMode::default(),
            gutter: true,
            power_preference: PowerPreference::default(),
            vsync: VSync::Auto,
            dpi_scale: None,
            background_image: None,
            background_opacity: 1.0,
            font_supersampling: 4,
            palette: ColorPalette::default(),
        }
    }
}

/// Read and parse the config at `path`, falling back to defaults on any
/// I/O or parse failure. Used both by the startup loader and the
/// live-reload watcher (which already knows the path it's watching).
pub fn load_from(path: &std::path::Path) -> Config {
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Config::default(),
    };
    parse_config(&contents, &path.display())
}

/// Parse a config TOML string into a [`Config`]. Split out from
/// [`load_from`] so tests can exercise the mapping logic without touching
/// the filesystem.
fn parse_config(
    contents: &str,
    source: &dyn std::fmt::Display,
) -> Config {
    let file: ConfigFile = match toml::from_str(contents) {
        Ok(f) => f,
        Err(e) => {
            warn!("failed to parse {source}: {e}");
            return Config::default();
        }
    };

    let cursor_style = build_cursor_style(file.cursor_shape, file.cursor_blink);
    let keybindings = build_keybindings(file.keybindings, source);
    let palette = build_palette(file.colors);

    Config {
        opacity: file.opacity.unwrap_or(1.0),
        fonts: file.fonts,
        font_size: file.font_size.unwrap_or(24.0).max(1.0),
        scrollback_lines: file.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
        cursor_style,
        keybindings,
        bell: file.bell.unwrap_or_default(),
        gutter: file.gutter.unwrap_or(true),
        power_preference: file.power_preference.unwrap_or_default(),
        vsync: file.vsync.unwrap_or(VSync::Auto),
        dpi_scale: file.dpi_scale.map(|v| v.max(0.25)),
        background_image: file.background_image.map(expand_path),
        background_opacity: file.background_opacity.unwrap_or(1.0),
        font_supersampling: file.font_supersampling.unwrap_or(4) as i32,
        palette,
    }
}

/// Resolve `~` and `$VAR` / `${VAR}` references in a config-supplied
/// path. Without this, `background_image = "~/foo.png"` is opened
/// literally and fails with ENOENT, since Rust's `PathBuf` (unlike a
/// shell) doesn't expand `~`. `shellexpand::full` also accepts
/// `${XDG_CONFIG_HOME}/term41/wall.png` and similar — handy because
/// terminals are exactly where users expect shell-style paths to work.
///
/// On a lookup failure (referenced env var unset), we log the error and
/// fall back to the literal path so the downstream loader reports a
/// clean "no such file" diagnostic against what the user actually wrote.
fn expand_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    match shellexpand::full(&raw) {
        Ok(expanded) => PathBuf::from(expanded.as_ref()),
        Err(e) => {
            warn!("path: failed to expand {raw:?}: {e}");
            path
        }
    }
}

/// Public so `main.rs` can hand the watcher the same path the loader uses.
pub fn config_file_path() -> Option<PathBuf> {
    config_path()
}

/// Map the optional `keybindings = [...]` toml field onto a
/// [`Keybindings`]. Returns [`Keybindings::defaults`] when the key is
/// absent; an empty array (`keybindings = []`) is honoured as "no
/// bindings" so users can disable them all to debug a conflict.
fn build_keybindings(
    raw: Option<Vec<KeybindingConfig>>,
    path: &dyn std::fmt::Display,
) -> Keybindings {
    let Some(entries) = raw else {
        return Keybindings::defaults();
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        match Keybinding::from_config_entry(entry) {
            Ok(b) => out.push(b),
            Err(e) => warn!("invalid keybinding in {path}: {e}"),
        }
    }
    Keybindings::from_config(out)
}

/// Map the optional shape + blink toml fields onto a [`CursorStyle`]. Falls
/// back to [`CursorStyle::default`] when both are absent. Unknown shape names
/// log a warning and default to block.
fn build_cursor_style(
    shape: Option<CursorShape>,
    blink: Option<bool>,
) -> CursorStyle {
    let mut style = CursorStyle::default();
    if let Some(s) = shape {
        style.shape = s;
    }
    if let Some(b) = blink {
        style.blink = b;
    }
    style
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("config.toml"))
}

fn float_opt_clamp_0_1<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<f32>::deserialize(deserializer) {
        Ok(opt) => {
            if let Some(f) = opt {
                if !(0.0..=1.0).contains(&f) {
                    warn!("float value {f} out of range [0.0, 1.0]; clamping");
                }
                Ok(Some(f.clamp(0.0, 1.0)))
            } else {
                Ok(None)
            }
        }
        Err(e) => {
            warn!("failed to parse float in config: {e}");
            Ok(None)
        }
    }
}

fn float_opt<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<f32>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse float in config: {e}");
            Ok(None)
        }
    }
}

fn u32_opt<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<u32>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse integer in config: {e}");
            Ok(None)
        }
    }
}

fn cursor_shape_opt<'de, D>(deserializer: D) -> Result<Option<CursorShape>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<CursorShape>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse cursor shape in config: {e}");
            Ok(None)
        }
    }
}

fn cursor_blink_opt<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<bool>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse cursor blink in config: {e}");
            Ok(None)
        }
    }
}

fn keybindings_opt<'de, D>(deserializer: D) -> Result<Option<Vec<KeybindingConfig>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<Vec<KeybindingConfig>>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse keybindings in config: {e}");
            Ok(None)
        }
    }
}

fn bell_mode_opt<'de, D>(deserializer: D) -> Result<Option<BellMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<BellMode>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse bell mode in config: {e}");
            Ok(None)
        }
    }
}

fn gutter_opt<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<bool>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse gutter setting in config: {e}");
            Ok(None)
        }
    }
}

fn power_preference_opt<'de, D>(deserializer: D) -> Result<Option<PowerPreference>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<PowerPreference>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse power preference in config: {e}");
            Ok(None)
        }
    }
}

fn vsync_opt<'de, D>(deserializer: D) -> Result<Option<VSync>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<VSync>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse vsync setting in config: {e}");
            Ok(None)
        }
    }
}

fn u32_opt_clamp_1_16<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<u32>::deserialize(deserializer) {
        Ok(opt) => {
            if let Some(v) = opt {
                if !(1..=16).contains(&v) {
                    warn!("integer value {v} out of range [1, 16]; clamping");
                }
                Ok(Some(v.clamp(1, 16)))
            } else {
                Ok(None)
            }
        }
        Err(e) => {
            warn!("failed to parse integer in config: {e}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        parse_config(s, &"<test>")
    }

    #[test]
    fn gutter_defaults_to_enabled_when_absent() {
        assert!(parse("").gutter);
    }

    #[test]
    fn gutter_honours_explicit_false() {
        assert!(!parse("gutter = false").gutter);
    }

    #[test]
    fn gutter_honours_explicit_true() {
        assert!(parse("gutter = true").gutter);
    }

    #[test]
    fn malformed_toml_falls_back_to_defaults_with_gutter_on() {
        // A typo shouldn't silently leave gutter off; the whole config
        // resets to defaults, and the default gutter state is on.
        let cfg = parse("gutter = \"yes-please\"");
        assert!(cfg.gutter);
    }

    #[test]
    fn background_image_defaults_to_none() {
        let cfg = parse("");
        assert!(cfg.background_image.is_none());
        // 1.0 is the no-op multiplier — image painted at full brightness
        // when set, but doesn't affect anything when unset.
        assert_eq!(cfg.background_opacity, 1.0);
    }

    #[test]
    fn background_image_and_opacity_round_trip() {
        let cfg = parse("background_image = \"/tmp/wallpaper.png\"\nbackground_opacity = 0.6\n");
        assert_eq!(
            cfg.background_image.as_deref(),
            Some(std::path::Path::new("/tmp/wallpaper.png"))
        );
        assert_eq!(cfg.background_opacity, 0.6);
    }

    #[test]
    fn background_opacity_clamps_to_unit_range() {
        let cfg = parse("background_opacity = 1.7");
        assert_eq!(cfg.background_opacity, 1.0);
        let cfg = parse("background_opacity = -0.3");
        assert_eq!(cfg.background_opacity, 0.0);
    }

    #[test]
    fn background_image_path_expands_tilde() {
        let cfg = parse("background_image = \"~/wallpapers/forest.png\"");
        let bg = cfg.background_image.expect("path parsed");
        // dirs::home_dir() can return None in some sandboxed test envs;
        // when it does, shellexpand leaves `~` literal and we just check
        // the path round-tripped untouched.
        if let Some(home) = dirs::home_dir() {
            assert_eq!(bg, home.join("wallpapers/forest.png"));
        } else {
            assert_eq!(bg, std::path::Path::new("~/wallpapers/forest.png"));
        }
    }

    #[test]
    fn background_image_path_expands_env_var() {
        // SAFETY: setting an env var in a single-threaded test context.
        // The risk would be other threads reading mid-mutation; this
        // test only sets it.
        unsafe {
            std::env::set_var("TERM41_TEST_WALLPAPER_DIR", "/srv/walls");
        }
        let cfg = parse("background_image = \"$TERM41_TEST_WALLPAPER_DIR/foo.png\"");
        assert_eq!(
            cfg.background_image.as_deref(),
            Some(std::path::Path::new("/srv/walls/foo.png"))
        );
    }

    #[test]
    fn background_image_absolute_path_passes_through() {
        let cfg = parse("background_image = \"/srv/share/wall.png\"");
        assert_eq!(
            cfg.background_image.as_deref(),
            Some(std::path::Path::new("/srv/share/wall.png"))
        );
    }

    // ---- color palette ----

    #[test]
    fn empty_config_uses_default_palette() {
        let cfg = parse("");
        assert_eq!(cfg.palette.fg, Srgb::new(204, 204, 204));
        assert_eq!(cfg.palette.bg, Srgb::new(0, 0, 0));
        assert!(cfg.palette.cursor.is_none());
        assert!(cfg.palette.selection_bg.is_none());
        // Default ANSI red
        assert_eq!(cfg.palette.ansi[1], Srgb::new(205, 0, 0));
    }

    #[test]
    fn full_rio_palette_parses() {
        let cfg = parse(
            r##"
[colors]
cursor = "#009fff"

[colors.primary]
foreground = "#fbfbfb"
background = "#070707"

[colors.selection]
background = "#fbfbfb"
text = "#070707"

[colors.normal]
black = "#141415"
red = "#ff2e3f"
green = "#0dbe4e"
yellow = "#ffca00"
blue = "#009fff"
magenta = "#c635e4"
cyan = "#08c0ef"
white = "#c6c6c8"

[colors.bright]
black = "#6c6c71"
red = "#ff6762"
green = "#5ecc71"
yellow = "#ffd452"
blue = "#69b1ff"
magenta = "#d568ea"
cyan = "#68cdf2"
white = "#fbfbfb"
"##,
        );
        assert_eq!(cfg.palette.fg, Srgb::new(0xfb, 0xfb, 0xfb));
        assert_eq!(cfg.palette.bg, Srgb::new(0x07, 0x07, 0x07));
        assert_eq!(cfg.palette.cursor, Some(Srgb::new(0x00, 0x9f, 0xff)));
        assert_eq!(cfg.palette.selection_bg, Some(Srgb::new(0xfb, 0xfb, 0xfb)));
        assert_eq!(cfg.palette.selection_fg, Some(Srgb::new(0x07, 0x07, 0x07)));
        // Spot-check a few ANSI colors
        assert_eq!(cfg.palette.ansi[0], Srgb::new(0x14, 0x14, 0x15)); // normal black
        assert_eq!(cfg.palette.ansi[1], Srgb::new(0xff, 0x2e, 0x3f)); // normal red
        assert_eq!(cfg.palette.ansi[8], Srgb::new(0x6c, 0x6c, 0x71)); // bright black
        assert_eq!(cfg.palette.ansi[15], Srgb::new(0xfb, 0xfb, 0xfb)); // bright white
    }

    #[test]
    fn partial_palette_keeps_defaults_for_unspecified() {
        let cfg = parse(
            r##"
[colors.primary]
foreground = "#ff0000"
"##,
        );
        assert_eq!(cfg.palette.fg, Srgb::new(255, 0, 0));
        // Background stays default
        assert_eq!(cfg.palette.bg, Srgb::new(0, 0, 0));
        // ANSI colors stay default
        assert_eq!(cfg.palette.ansi[1], Srgb::new(205, 0, 0));
    }

    #[test]
    fn invalid_hex_color_falls_back_to_default() {
        let cfg = parse(
            r##"
[colors.primary]
foreground = "not-a-color"
"##,
        );
        // Should fall back to default fg
        assert_eq!(cfg.palette.fg, Srgb::new(204, 204, 204));
    }
}
