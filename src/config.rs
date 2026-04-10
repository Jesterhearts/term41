use std::path::PathBuf;

use serde::Deserialize;

const DEFAULT_SCROLLBACK: u32 = 10_000;

#[derive(Deserialize)]
struct ConfigFile {
    opacity: Option<f32>,
    fonts: Option<String>,
    scrollback_lines: Option<u32>,
}

pub struct Config {
    pub opacity: f32,
    pub fonts: Option<String>,
    pub scrollback_lines: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            fonts: None,
            scrollback_lines: DEFAULT_SCROLLBACK,
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
            log::warn!("failed to parse {}: {e}", path.display());
            return Config::default();
        }
    };

    Config {
        opacity: file.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
        fonts: file.fonts,
        scrollback_lines: file.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("config.toml"))
}
