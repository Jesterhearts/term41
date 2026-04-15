use std::path::PathBuf;

use serde::Deserialize;
use wgpu::PowerPreference;

use crate::keybindings::Keybinding;
use crate::keybindings::KeybindingConfig;
use crate::keybindings::Keybindings;
use crate::terminal::CursorShape;
use crate::terminal::CursorStyle;

const DEFAULT_SCROLLBACK: u32 = 10_000;

/// What to do when the foreground app rings the bell (BEL / `\x07`).
///
/// Default is [`BellMode::Off`] because shells like bash ring the bell on
/// completion-ambiguity by default — most users find that surprising
/// rather than useful out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BellMode {
    /// Ignore the bell entirely.
    #[default]
    Off,
    /// Briefly flash the screen.
    Visual,
    /// Ask the compositor to mark the window as needing attention
    /// (taskbar bobbing on macOS, urgency hint on X11/Wayland). Quiet
    /// when the window is focused; eye-catching when it isn't.
    Urgent,
}

#[derive(Deserialize)]
struct ConfigFile {
    opacity: Option<f32>,
    fonts: Option<String>,
    font_size: Option<f32>,
    scrollback_lines: Option<u32>,
    /// Cursor shape: `block`, `underline`, or `beam`.
    cursor_shape: Option<String>,
    /// Whether the cursor blinks. Defaults to true.
    cursor_blink: Option<bool>,
    /// Replace the default keybindings entirely. Setting an empty array
    /// disables all bindings — useful for debugging conflicts.
    keybindings: Option<Vec<KeybindingConfig>>,
    /// Bell behaviour: `off`, `visual`, or `urgent`.
    bell: Option<String>,
    /// Show the shell-integration gutter on the left edge — a thin strip
    /// where OSC 133 prompt rows get a coloured dot marking the last
    /// command's exit status. Defaults to on; disable for a pure
    /// terminal-text view or when the shell doesn't emit OSC 133 at all.
    gutter: Option<bool>,
    /// Preferred power mode for the GPU. See wgpu::PowerPreference docs for
    /// details.
    power_preference: Option<PowerPreference>,
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

    let cursor_style = build_cursor_style(file.cursor_shape.as_deref(), file.cursor_blink);
    let keybindings = build_keybindings(file.keybindings, source);
    let bell = build_bell_mode(file.bell.as_deref());

    Config {
        opacity: file.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
        fonts: file.fonts,
        font_size: file.font_size.unwrap_or(24.0).max(1.0),
        scrollback_lines: file.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
        cursor_style,
        keybindings,
        bell,
        gutter: file.gutter.unwrap_or(true),
        power_preference: file.power_preference.unwrap_or_default(),
    }
}

/// Map the optional `bell = "..."` toml field onto a [`BellMode`]. Unknown
/// values warn and default to off so a typo can't silently turn the bell
/// on (or off) against the user's intent.
fn build_bell_mode(raw: Option<&str>) -> BellMode {
    let Some(s) = raw else {
        return BellMode::default();
    };
    match s.to_ascii_lowercase().as_str() {
        "off" | "none" | "false" => BellMode::Off,
        "visual" | "flash" => BellMode::Visual,
        "urgent" | "attention" => BellMode::Urgent,
        other => {
            warn!("unknown bell mode {other:?}; falling back to off");
            BellMode::Off
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
    shape: Option<&str>,
    blink: Option<bool>,
) -> CursorStyle {
    let mut style = CursorStyle::default();
    if let Some(s) = shape {
        style.shape = match s.to_ascii_lowercase().as_str() {
            "block" => CursorShape::Block,
            "underline" | "underscore" => CursorShape::Underline,
            "beam" | "bar" | "ibeam" => CursorShape::Beam,
            other => {
                warn!("unknown cursor_shape {other:?}; falling back to block");
                CursorShape::Block
            }
        };
    }
    if let Some(b) = blink {
        style.blink = b;
    }
    style
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("config.toml"))
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
