use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize)]
struct ConfigFile {
    opacity: Option<f32>,
    fonts: Option<String>,
}

pub struct Config {
    pub opacity: f32,
    pub fonts: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            fonts: None,
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
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("config.toml"))
}
