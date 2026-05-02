use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use crate::dedupe_paths;
use crate::expand_path;
use crate::usize_opt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEditorConfig {
    pub enabled: bool,
    pub vim_mode: bool,
    pub completions: Vec<String>,
    pub binary_dirs: Vec<PathBuf>,
    pub merge_extra_dirs: bool,
    pub deep_history_integration: bool,
    pub max_history: usize,
}

impl Default for CommandEditorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            vim_mode: false,
            completions: Vec::new(),
            binary_dirs: default_binary_dirs(),
            merge_extra_dirs: true,
            deep_history_integration: false,
            max_history: 200,
        }
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct CommandEditorSettings {
    /// Enable the terminal-local command editor. Disabled by default so the
    /// normal keyboard path remains unchanged unless the user opts in.
    #[serde(default)]
    enabled: Option<bool>,
    /// Start the command editor in a vim-like normal mode and interpret
    /// unmodified keys as modal editing commands.
    #[serde(default)]
    vim_mode: Option<bool>,
    /// Static completion candidates. Recent command history is always added
    /// at runtime by the editor.
    #[serde(default)]
    completions: Option<Vec<String>>,
    /// Extra executable directories scanned by the command editor.
    #[serde(default)]
    binary_dirs: Option<Vec<PathBuf>>,
    /// When true, `binary_dirs` is appended to the platform default list.
    /// When false, `binary_dirs` replaces the default list.
    #[serde(default)]
    merge_extra_dirs: Option<bool>,
    /// When true, attempt to discover the user's active shell history and
    /// merge it into command editor history navigation/completion.
    #[serde(default)]
    deep_history_integration: Option<bool>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    max_history: Option<usize>,
}

pub(crate) fn build_command_editor(raw: Option<CommandEditorSettings>) -> CommandEditorConfig {
    let settings = raw.unwrap_or_default();
    let defaults = CommandEditorConfig::default();
    let merge_extra_dirs = settings
        .merge_extra_dirs
        .unwrap_or(defaults.merge_extra_dirs);
    let binary_dirs = build_command_editor_binary_dirs(
        defaults.binary_dirs,
        settings.binary_dirs.unwrap_or_default(),
        merge_extra_dirs,
    );
    CommandEditorConfig {
        enabled: settings.enabled.unwrap_or(defaults.enabled),
        vim_mode: settings.vim_mode.unwrap_or(defaults.vim_mode),
        completions: settings.completions.unwrap_or_default(),
        binary_dirs,
        merge_extra_dirs,
        deep_history_integration: settings
            .deep_history_integration
            .unwrap_or(defaults.deep_history_integration),
        max_history: settings.max_history.unwrap_or(defaults.max_history).max(1),
    }
}

fn build_command_editor_binary_dirs(
    default_dirs: Vec<PathBuf>,
    configured_dirs: Vec<PathBuf>,
    merge_extra_dirs: bool,
) -> Vec<PathBuf> {
    let configured_dirs = configured_dirs.into_iter().map(expand_path);
    if merge_extra_dirs {
        return dedupe_paths(default_dirs.into_iter().chain(configured_dirs));
    }
    dedupe_paths(configured_dirs)
}

fn default_binary_dirs() -> Vec<PathBuf> {
    default_binary_dirs_for(
        dirs::executable_dir(),
        dirs::home_dir().as_deref(),
        platform_binary_dirs(),
    )
}

fn default_binary_dirs_for(
    executable_dir: Option<PathBuf>,
    home: Option<&Path>,
    platform_dirs: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = executable_dir {
        dirs.push(dir);
    }
    if let Some(home) = home {
        dirs.extend([
            home.join(".cargo").join("bin"),
            home.join("bin"),
            home.join("go").join("bin"),
            home.join(".bun").join("bin"),
            home.join(".deno").join("bin"),
            home.join(".local").join("share").join("pnpm"),
        ]);
    }
    dirs.extend(platform_dirs);
    dedupe_paths(dirs)
}

#[cfg(unix)]
fn platform_binary_dirs() -> Vec<PathBuf> {
    [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/opt/local/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(windows)]
fn platform_binary_dirs() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_dirs_merge_with_defaults() {
        let home = PathBuf::from("/tmp/term41-home");
        let default_dirs = default_binary_dirs_for(
            Some(home.join(".local").join("bin")),
            Some(&home),
            [PathBuf::from("/usr/local/bin")],
        );

        assert_eq!(
            build_command_editor_binary_dirs(
                default_dirs,
                vec![home.join(".cargo").join("bin"), home.join("tools")],
                true,
            ),
            vec![
                home.join(".local").join("bin"),
                home.join(".cargo").join("bin"),
                home.join("bin"),
                home.join("go").join("bin"),
                home.join(".bun").join("bin"),
                home.join(".deno").join("bin"),
                home.join(".local").join("share").join("pnpm"),
                PathBuf::from("/usr/local/bin"),
                home.join("tools"),
            ]
        );
    }

    #[test]
    fn binary_dirs_replace_defaults_when_merge_disabled() {
        let home = PathBuf::from("/tmp/term41-home");

        assert_eq!(
            build_command_editor_binary_dirs(
                vec![
                    home.join(".cargo").join("bin"),
                    home.join(".local").join("bin")
                ],
                vec![home.join("tools"), home.join("tools")],
                false,
            ),
            vec![home.join("tools")]
        );
    }
}
