use std::path::PathBuf;

use serde::Deserialize;

use crate::keybindings::Keybinding;
use crate::keybindings::KeybindingConfig;
use crate::keybindings::Keybindings;
use crate::terminal::CursorShape;
use crate::terminal::CursorStyle;

const DEFAULT_SCROLLBACK: u32 = 10_000;

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
}

pub struct Config {
    pub opacity: f32,
    pub fonts: Option<String>,
    pub font_size: f32,
    pub scrollback_lines: u32,
    pub cursor_style: CursorStyle,
    pub keybindings: Keybindings,
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

    let file: ConfigFile = match toml::from_str(&contents) {
        Ok(f) => f,
        Err(e) => {
            warn!("failed to parse {}: {e}", path.display());
            return Config::default();
        }
    };

    let cursor_style = build_cursor_style(file.cursor_shape.as_deref(), file.cursor_blink);
    let keybindings = build_keybindings(file.keybindings, &path.display());

    Config {
        opacity: file.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
        fonts: file.fonts,
        font_size: file.font_size.unwrap_or(24.0).max(1.0),
        scrollback_lines: file.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
        cursor_style,
        keybindings,
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
