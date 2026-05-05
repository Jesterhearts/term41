use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Deserializer;

use crate::dedupe_paths;
use crate::expand_path;
use crate::usize_opt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEditorConfig {
    pub enabled: bool,
    pub vim_mode: bool,
    pub completions: Vec<String>,
    pub completion_files: Vec<PathBuf>,
    pub command_completions: Vec<CommandCompletionConfig>,
    pub binary_dirs: Vec<PathBuf>,
    pub merge_extra_dirs: bool,
    pub deep_history_integration: bool,
    pub max_history: usize,
    pub max_persistent_history_per_dir: usize,
}

impl Default for CommandEditorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            vim_mode: false,
            completions: Vec::new(),
            completion_files: Vec::new(),
            command_completions: Vec::new(),
            binary_dirs: default_binary_dirs(),
            merge_extra_dirs: true,
            deep_history_integration: false,
            max_history: 200,
            max_persistent_history_per_dir: 200,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCompletionConfig {
    pub command: String,
    pub subcommands: Vec<SubcommandCompletionConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubcommandCompletionConfig {
    pub name: String,
    pub arguments: Vec<String>,
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
    /// JSON files containing command-specific subcommand and argument
    /// completion definitions.
    #[serde(default)]
    completion_files: Option<Vec<PathBuf>>,
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
    /// Maximum persisted command-editor entries retained for one cwd key.
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    max_persistent_history_per_dir: Option<usize>,
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
    let completion_files = settings
        .completion_files
        .unwrap_or_default()
        .into_iter()
        .map(expand_path)
        .collect::<Vec<_>>();
    let command_completions = load_command_completion_files(&completion_files);
    CommandEditorConfig {
        enabled: settings.enabled.unwrap_or(defaults.enabled),
        vim_mode: settings.vim_mode.unwrap_or(defaults.vim_mode),
        completions: settings.completions.unwrap_or_default(),
        completion_files,
        command_completions,
        binary_dirs,
        merge_extra_dirs,
        deep_history_integration: settings
            .deep_history_integration
            .unwrap_or(defaults.deep_history_integration),
        max_history: settings.max_history.unwrap_or(defaults.max_history).max(1),
        max_persistent_history_per_dir: settings
            .max_persistent_history_per_dir
            .unwrap_or(defaults.max_persistent_history_per_dir)
            .max(1),
    }
}

fn load_command_completion_files(paths: &[PathBuf]) -> Vec<CommandCompletionConfig> {
    let mut completions = Vec::new();
    for path in paths {
        let loaded = match load_command_completion_file(path) {
            Ok(loaded) => loaded,
            Err(error) => {
                warn!(
                    "failed to load command completion file {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        for completion in loaded.into_command_completions() {
            push_command_completion(&mut completions, completion);
        }
    }
    completions
}

fn load_command_completion_file(
    path: &Path
) -> Result<CommandCompletionFile, CommandCompletionLoadError> {
    let contents = fs::read_to_string(path).map_err(CommandCompletionLoadError::Io)?;
    serde_json::from_str(&contents).map_err(CommandCompletionLoadError::Json)
}

#[derive(Debug)]
enum CommandCompletionLoadError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for CommandCompletionLoadError {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CommandCompletionFile {
    Wrapped {
        commands: Vec<JsonCommandCompletion>,
    },
    List(Vec<JsonCommandCompletion>),
    Single(JsonCommandCompletion),
}

impl CommandCompletionFile {
    fn into_command_completions(self) -> Vec<CommandCompletionConfig> {
        match self {
            Self::Wrapped { commands } | Self::List(commands) => {
                normalize_command_completions(commands)
            }
            Self::Single(command) => normalize_command_completions([command]),
        }
    }
}

#[derive(Deserialize)]
struct JsonCommandCompletion {
    command: String,
    #[serde(default, deserialize_with = "deserialize_json_subcommands")]
    subcommands: Vec<JsonSubcommandCompletion>,
}

#[derive(Debug, Clone)]
struct JsonSubcommandCompletion {
    name: String,
    arguments: Vec<String>,
}

fn deserialize_json_subcommands<'de, D>(
    deserializer: D
) -> Result<Vec<JsonSubcommandCompletion>, D::Error>
where
    D: Deserializer<'de>,
{
    let subcommands = JsonSubcommands::deserialize(deserializer)?;
    Ok(match subcommands {
        JsonSubcommands::List(items) => items
            .into_iter()
            .map(JsonSubcommandListEntry::into_subcommand)
            .collect(),
        JsonSubcommands::Map(items) => items
            .into_iter()
            .map(|(name, value)| value.into_subcommand(name))
            .collect(),
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonSubcommands {
    List(Vec<JsonSubcommandListEntry>),
    Map(BTreeMap<String, JsonSubcommandValue>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonSubcommandListEntry {
    Detail {
        name: String,
        #[serde(default)]
        arguments: Vec<String>,
    },
    Name(String),
}

impl JsonSubcommandListEntry {
    fn into_subcommand(self) -> JsonSubcommandCompletion {
        match self {
            Self::Detail { name, arguments } => JsonSubcommandCompletion { name, arguments },
            Self::Name(name) => JsonSubcommandCompletion {
                name,
                arguments: Vec::new(),
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonSubcommandValue {
    Arguments(Vec<String>),
    Detail {
        #[serde(default)]
        arguments: Vec<String>,
    },
}

impl JsonSubcommandValue {
    fn into_subcommand(
        self,
        name: String,
    ) -> JsonSubcommandCompletion {
        match self {
            Self::Arguments(arguments) | Self::Detail { arguments } => {
                JsonSubcommandCompletion { name, arguments }
            }
        }
    }
}

fn normalize_command_completions(
    commands: impl IntoIterator<Item = JsonCommandCompletion>
) -> Vec<CommandCompletionConfig> {
    let mut out = Vec::new();
    for command in commands {
        let command_name = command.command.trim();
        if command_name.is_empty() {
            continue;
        }
        push_command_completion(
            &mut out,
            CommandCompletionConfig {
                command: command_name.to_owned(),
                subcommands: normalize_subcommands(command.subcommands),
            },
        );
    }
    out
}

fn normalize_subcommands(
    subcommands: impl IntoIterator<Item = JsonSubcommandCompletion>
) -> Vec<SubcommandCompletionConfig> {
    let mut out = Vec::new();
    for subcommand in subcommands {
        let name = subcommand.name.trim();
        if name.is_empty() {
            continue;
        }
        push_subcommand_completion(
            &mut out,
            SubcommandCompletionConfig {
                name: name.to_owned(),
                arguments: dedupe_strings(subcommand.arguments),
            },
        );
    }
    out
}

fn push_command_completion(
    out: &mut Vec<CommandCompletionConfig>,
    completion: CommandCompletionConfig,
) {
    if let Some(existing) = out
        .iter_mut()
        .find(|existing| existing.command == completion.command)
    {
        for subcommand in completion.subcommands {
            push_subcommand_completion(&mut existing.subcommands, subcommand);
        }
    } else {
        out.push(completion);
    }
}

fn push_subcommand_completion(
    out: &mut Vec<SubcommandCompletionConfig>,
    completion: SubcommandCompletionConfig,
) {
    if let Some(existing) = out
        .iter_mut()
        .find(|existing| existing.name == completion.name)
    {
        for argument in completion.arguments {
            push_string(&mut existing.arguments, argument);
        }
    } else {
        out.push(completion);
    }
}

fn dedupe_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        push_string(&mut out, value);
    }
    out
}

fn push_string(
    out: &mut Vec<String>,
    value: String,
) {
    if !value.is_empty() && !out.iter().any(|existing| existing == &value) {
        out.push(value);
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

    #[test]
    fn history_limits_are_clamped_to_one() {
        let config = build_command_editor(Some(CommandEditorSettings {
            max_history: Some(0),
            max_persistent_history_per_dir: Some(0),
            ..CommandEditorSettings::default()
        }));

        assert_eq!(config.max_history, 1);
        assert_eq!(config.max_persistent_history_per_dir, 1);
    }

    #[test]
    fn completion_files_load_wrapped_json_commands() {
        let root = unique_test_dir("completion-wrapped");
        fs::create_dir_all(&root).expect("create temp dir");
        let path = root.join("commands.json");
        fs::write(
            &path,
            r#"
{
  "commands": [
    {
      "command": "cargo",
      "subcommands": [
        { "name": "build", "arguments": ["--release", "--workspace"] }
      ]
    }
  ]
}
"#,
        )
        .expect("write completion file");

        let config = build_command_editor(Some(CommandEditorSettings {
            completion_files: Some(vec![path.clone()]),
            ..CommandEditorSettings::default()
        }));

        assert_eq!(config.completion_files, [path]);
        assert_eq!(
            config.command_completions,
            [CommandCompletionConfig {
                command: "cargo".to_owned(),
                subcommands: vec![SubcommandCompletionConfig {
                    name: "build".to_owned(),
                    arguments: vec!["--release".to_owned(), "--workspace".to_owned()],
                }],
            }]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completion_files_load_map_subcommands_and_merge_duplicates() {
        let root = unique_test_dir("completion-map");
        fs::create_dir_all(&root).expect("create temp dir");
        let path = root.join("commands.json");
        fs::write(
            &path,
            r#"
[
  {
    "command": "cargo",
    "subcommands": {
      "build": ["--release"],
      "test": { "arguments": ["--workspace"] }
    }
  },
  {
    "command": "cargo",
    "subcommands": {
      "build": ["--release", "--all-targets"]
    }
  }
]
"#,
        )
        .expect("write completion file");

        let config = build_command_editor(Some(CommandEditorSettings {
            completion_files: Some(vec![path]),
            ..CommandEditorSettings::default()
        }));

        assert_eq!(
            config.command_completions,
            [CommandCompletionConfig {
                command: "cargo".to_owned(),
                subcommands: vec![
                    SubcommandCompletionConfig {
                        name: "build".to_owned(),
                        arguments: vec!["--release".to_owned(), "--all-targets".to_owned()],
                    },
                    SubcommandCompletionConfig {
                        name: "test".to_owned(),
                        arguments: vec!["--workspace".to_owned()],
                    },
                ],
            }]
        );

        let _ = fs::remove_dir_all(root);
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("config41-{label}-{nonce}"))
    }
}
