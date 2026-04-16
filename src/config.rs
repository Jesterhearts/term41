use std::path::PathBuf;

use serde::Deserialize;
use wgpu::PowerPreference;

use crate::keybindings::Keybinding;
use crate::keybindings::KeybindingConfig;
use crate::keybindings::Keybindings;
use crate::terminal::CursorShape;
use crate::terminal::CursorStyle;

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
}

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

    Config {
        opacity: file.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
        fonts: file.fonts,
        font_size: file.font_size.unwrap_or(24.0).max(1.0),
        scrollback_lines: file.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
        cursor_style,
        keybindings,
        bell: file.bell.unwrap_or_default(),
        gutter: file.gutter.unwrap_or(true),
        power_preference: file.power_preference.unwrap_or_default(),
        vsync: file.vsync.unwrap_or(VSync::Auto),
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
}
