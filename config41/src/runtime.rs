use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;

use crate::Config;
use crate::schema::parse_config;

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

#[derive(Debug)]
enum ConfigLoadError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl std::fmt::Display for ConfigLoadError {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::Parse { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

fn load_from(path: &Path) -> Result<Config, ConfigLoadError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigLoadError::Read {
        path: path.to_owned(),
        source,
    })?;
    parse_config(&contents, &path.display()).map_err(|source| ConfigLoadError::Parse {
        path: path.to_owned(),
        source,
    })
}

fn reload_from(
    current: &Mutex<Option<Config>>,
    path: &Path,
) -> Result<(), ConfigLoadError> {
    let replacement = load_from(path)?;
    *current.lock() = Some(replacement);
    Ok(())
}

pub(crate) fn dedupe_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !path.as_os_str().is_empty() && !out.iter().any(|existing| existing == &path) {
            out.push(path);
        }
    }
    out
}

/// Resolve `~` and `$VAR` / `${VAR}` references in a config-supplied
/// path. Without this, `background_image = "~/foo.png"` is opened
/// literally and fails with ENOENT, since Rust's `PathBuf` (unlike a
/// shell) doesn't expand `~`. `shellexpand::full` also accepts
/// `${XDG_CONFIG_HOME}/term41/wall.png` and similar, which is useful
/// because terminals are exactly where users expect shell-style paths.
///
/// On a lookup failure (referenced env var unset), we log the error and
/// fall back to the literal path so the downstream loader reports a
/// clean "no such file" diagnostic against what the user actually wrote.
pub(crate) fn expand_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    match shellexpand::full(&raw) {
        Ok(expanded) => PathBuf::from(expanded.as_ref()),
        Err(e) => {
            warn!("path: failed to expand {raw:?}: {e}");
            path
        }
    }
}

pub fn scripts_dir_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("scripts"))
}

pub fn init_config(
    config_reload: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<std::thread::Thread>>,
) -> Config {
    if let Some(config) = CONFIG.lock().clone() {
        warn!("Init config called twice");
        return config;
    }

    let Some(config_path) = config_path() else {
        error!("Failed to initialize config watcher");
        *CONFIG.lock() = Some(Config::default());
        return Config::default();
    };

    let config = match load_from(&config_path) {
        Ok(config) => config,
        Err(ConfigLoadError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Config::default()
        }
        Err(error) => {
            warn!("{error}; using startup defaults");
            Config::default()
        }
    };
    *CONFIG.lock() = Some(config);

    spawn_config_watcher(config_path, config_reload, render_thread_handle);

    CONFIG.lock().clone().unwrap()
}

pub fn config() -> Config {
    CONFIG.lock().clone().unwrap_or_default()
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("config.toml"))
}

fn spawn_config_watcher(
    config_path: PathBuf,
    config_reload: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<std::thread::Thread>>,
) {
    use notify::EventKind;
    use notify::RecursiveMode;
    use notify::Watcher;

    let Some(dir) = config_path.parent().map(PathBuf::from) else {
        return;
    };

    std::thread::Builder::new()
        .name("config-watcher".into())
        .spawn(move || {
            let target = config_path.clone();
            let scripts_dir = dir.join("scripts");
            let config_reload_for_handler = config_reload.clone();
            let mut watcher = match notify::recommended_watcher(move |res| {
                let event: notify::Event = match res {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("config watcher error: {e}");
                        return;
                    }
                };
                let touches_config_or_script = event
                    .paths
                    .iter()
                    .any(|p| p == &target || p.starts_with(&scripts_dir));
                if !touches_config_or_script {
                    return;
                }
                if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    return;
                }

                if event.paths.iter().any(|path| path == &target)
                    && let Err(error) = reload_from(&CONFIG, &config_path)
                {
                    warn!("{error}; keeping the last valid configuration");
                    return;
                }

                config_reload_for_handler.store(true, Ordering::Release);
                if let Some(thread) = render_thread_handle.get() {
                    thread.unpark();
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    warn!("failed to create config watcher: {e}");
                    return;
                }
            };

            if let Err(e) = watcher.watch(&dir, RecursiveMode::Recursive) {
                warn!("failed to watch config dir {}: {e}", dir.display());
                return;
            }
            std::thread::park();
        })
        .expect("spawn config watcher");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_reload_preserves_config_and_a_later_valid_reload_applies() {
        let dir = std::env::temp_dir().join(format!("term41-config-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let current = Mutex::new(Some(Config {
            font_size: 37.0,
            gutter: false,
            ..Config::default()
        }));

        std::fs::write(&path, "font_size = [").unwrap();
        assert!(matches!(
            reload_from(&current, &path),
            Err(ConfigLoadError::Parse { .. })
        ));
        assert_eq!(current.lock().as_ref().unwrap().font_size, 37.0);
        assert!(!current.lock().as_ref().unwrap().gutter);

        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            reload_from(&current, &path),
            Err(ConfigLoadError::Read { .. })
        ));
        assert_eq!(current.lock().as_ref().unwrap().font_size, 37.0);

        std::fs::write(&path, "font_size = 19\ngutter = true").unwrap();
        reload_from(&current, &path).unwrap();
        assert_eq!(current.lock().as_ref().unwrap().font_size, 19.0);
        assert!(current.lock().as_ref().unwrap().gutter);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
