use std::path::PathBuf;

use serde::Deserialize;

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
}

pub struct Config {
    pub opacity: f32,
    pub fonts: Option<String>,
    pub font_size: f32,
    pub scrollback_lines: u32,
    pub cursor_style: CursorStyle,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            fonts: None,
            font_size: 24.0,
            scrollback_lines: DEFAULT_SCROLLBACK,
            cursor_style: CursorStyle::default(),
        }
    }
}

pub fn load() -> Config {
    let path = match config_path() {
        Some(p) => p,
        None => return Config::default(),
    };

    let contents = match std::fs::read_to_string(&path) {
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

    Config {
        opacity: file.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
        fonts: file.fonts,
        font_size: file.font_size.unwrap_or(24.0).max(1.0),
        scrollback_lines: file.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
        cursor_style,
    }
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
