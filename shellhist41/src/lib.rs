use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

const DEFAULT_MAX_ENTRIES: usize = 1000;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellHistorySource {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Atuin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellHistoryEntry {
    pub command: String,
    pub source: ShellHistorySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellHistoryOptions {
    pub max_entries: usize,
    pub timeout: Duration,
    pub prefer_atuin: bool,
}

impl Default for ShellHistoryOptions {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            timeout: DEFAULT_TIMEOUT,
            prefer_atuin: true,
        }
    }
}

#[derive(Debug)]
pub enum ShellHistoryError {
    UnsupportedShell(Option<OsString>),
    Command(io::Error),
    CommandTimedOut(String),
    CommandFailed(String),
    HistoryFileMissing,
    HistoryFileRead { path: PathBuf, error: io::Error },
}

impl fmt::Display for ShellHistoryError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::UnsupportedShell(shell) => write!(f, "unsupported shell: {shell:?}"),
            Self::Command(error) => write!(f, "failed to run shell history command: {error}"),
            Self::CommandTimedOut(command) => {
                write!(f, "shell history command timed out: {command}")
            }
            Self::CommandFailed(command) => write!(f, "shell history command failed: {command}"),
            Self::HistoryFileMissing => write!(f, "shell did not report a history file"),
            Self::HistoryFileRead { path, error } => {
                write!(f, "failed to read history file {}: {error}", path.display())
            }
        }
    }
}

impl Error for ShellHistoryError {}

pub fn load_current_shell_history(
    options: &ShellHistoryOptions
) -> Result<Vec<ShellHistoryEntry>, ShellHistoryError> {
    let providers = current_history_providers(options);
    if providers.is_empty() {
        return Err(ShellHistoryError::UnsupportedShell(env::var_os("SHELL")));
    }

    let mut last_error = None;
    for provider in providers {
        match load_provider_history(provider, options) {
            Ok(entries) => return Ok(entries),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| ShellHistoryError::UnsupportedShell(env::var_os("SHELL"))))
}

pub fn detect_current_shell() -> Option<ShellHistorySource> {
    detect_shell_provider(env::var_os("SHELL").as_deref())
}

fn current_history_providers(options: &ShellHistoryOptions) -> Vec<ShellHistorySource> {
    let mut providers = Vec::new();
    if options.prefer_atuin && atuin_env_present() && command_on_path("atuin") {
        providers.push(ShellHistorySource::Atuin);
    }
    if let Some(provider) = detect_current_shell() {
        push_unique_provider(&mut providers, provider);
    }
    providers
}

fn push_unique_provider(
    providers: &mut Vec<ShellHistorySource>,
    provider: ShellHistorySource,
) {
    if !providers.contains(&provider) {
        providers.push(provider);
    }
}

fn detect_shell_provider(shell: Option<&OsStr>) -> Option<ShellHistorySource> {
    let shell = shell?;
    let name = Path::new(shell)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    match name {
        "bash" => Some(ShellHistorySource::Bash),
        "zsh" => Some(ShellHistorySource::Zsh),
        "fish" => Some(ShellHistorySource::Fish),
        "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe" => {
            Some(ShellHistorySource::PowerShell)
        }
        _ => None,
    }
}

fn load_provider_history(
    provider: ShellHistorySource,
    options: &ShellHistoryOptions,
) -> Result<Vec<ShellHistoryEntry>, ShellHistoryError> {
    let entries = match provider {
        ShellHistorySource::Bash => load_file_history(provider, options)?,
        ShellHistorySource::Zsh => load_file_history(provider, options)?,
        ShellHistorySource::Fish => load_fish_history(options)?,
        ShellHistorySource::PowerShell => load_powershell_history(options)?,
        ShellHistorySource::Atuin => load_atuin_history(options)?,
    };
    Ok(limit_entries(entries, options.max_entries))
}

fn load_file_history(
    provider: ShellHistorySource,
    options: &ShellHistoryOptions,
) -> Result<Vec<ShellHistoryEntry>, ShellHistoryError> {
    let shell = env::var_os("SHELL").ok_or(ShellHistoryError::UnsupportedShell(None))?;
    let history_file = ask_shell_for_history_file(&shell, options.timeout)?;
    let raw =
        fs::read_to_string(&history_file).map_err(|error| ShellHistoryError::HistoryFileRead {
            path: history_file,
            error,
        })?;
    let commands = match provider {
        ShellHistorySource::Bash => parse_bash_history(&raw),
        ShellHistorySource::Zsh => parse_zsh_history(&raw),
        _ => Vec::new(),
    };
    Ok(entries(provider, commands))
}

fn ask_shell_for_history_file(
    shell: &OsStr,
    timeout: Duration,
) -> Result<PathBuf, ShellHistoryError> {
    let output = run_command(shell, ["-i", "-c", "printf '%s\\n' \"$HISTFILE\""], timeout)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(PathBuf::from)
        .ok_or(ShellHistoryError::HistoryFileMissing)
}

fn load_fish_history(
    options: &ShellHistoryOptions
) -> Result<Vec<ShellHistoryEntry>, ShellHistoryError> {
    let max = options.max_entries.max(1).to_string();
    let script = format!("history search --reverse --null --max {max}");
    let shell = env::var_os("SHELL").unwrap_or_else(|| OsString::from("fish"));
    let output = run_command(shell, ["-c", script.as_str()], options.timeout)?;
    Ok(entries(
        ShellHistorySource::Fish,
        parse_null_or_line_separated_history(&output.stdout),
    ))
}

fn load_powershell_history(
    options: &ShellHistoryOptions
) -> Result<Vec<ShellHistoryEntry>, ShellHistoryError> {
    let shell = env::var_os("SHELL").ok_or(ShellHistoryError::UnsupportedShell(None))?;
    let output = run_command(
        shell,
        [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-PSReadLineOption).HistorySavePath",
        ],
        options.timeout,
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let history_file = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(PathBuf::from)
        .ok_or(ShellHistoryError::HistoryFileMissing)?;
    let raw =
        fs::read_to_string(&history_file).map_err(|error| ShellHistoryError::HistoryFileRead {
            path: history_file,
            error,
        })?;
    Ok(entries(
        ShellHistorySource::PowerShell,
        parse_powershell_history(&raw),
    ))
}

fn load_atuin_history(
    options: &ShellHistoryOptions
) -> Result<Vec<ShellHistoryEntry>, ShellHistoryError> {
    let output = run_command(
        "atuin",
        [
            "history",
            "list",
            "--reverse",
            "--print0",
            "--format",
            "{command}",
        ],
        options.timeout,
    )?;
    Ok(entries(
        ShellHistorySource::Atuin,
        parse_null_or_line_separated_history(&output.stdout),
    ))
}

fn entries(
    source: ShellHistorySource,
    commands: Vec<String>,
) -> Vec<ShellHistoryEntry> {
    let mut out = Vec::new();
    for command in commands {
        push_latest(&mut out, ShellHistoryEntry { command, source });
    }
    out
}

fn parse_bash_history(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = raw.lines().peekable();
    while let Some(line) = lines.next() {
        if is_bash_timestamp(line) {
            if let Some(command) = lines.next() {
                push_history_command(&mut out, command);
            }
        } else {
            push_history_command(&mut out, line);
        }
    }
    out
}

fn parse_zsh_history(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let command = line
            .strip_prefix(": ")
            .and_then(|line| line.split_once(';').map(|(_, command)| command))
            .unwrap_or(line);
        push_history_command(&mut out, command);
    }
    out
}

fn parse_powershell_history(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim_end)
        .filter(|command| !command.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_null_or_line_separated_history(raw: &[u8]) -> Vec<String> {
    if raw.contains(&0) {
        return raw
            .split(|byte| *byte == 0)
            .filter_map(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .filter(|command| !command.trim().is_empty())
            .collect();
    }
    String::from_utf8_lossy(raw)
        .lines()
        .map(str::to_owned)
        .filter(|command| !command.trim().is_empty())
        .collect()
}

fn is_bash_timestamp(line: &str) -> bool {
    line.strip_prefix('#').is_some_and(|timestamp| {
        !timestamp.is_empty() && timestamp.chars().all(|ch| ch.is_ascii_digit())
    })
}

fn push_history_command(
    out: &mut Vec<String>,
    command: &str,
) {
    if !command.trim().is_empty() {
        out.push(command.to_owned());
    }
}

fn limit_entries(
    entries: Vec<ShellHistoryEntry>,
    max_entries: usize,
) -> Vec<ShellHistoryEntry> {
    let max_entries = max_entries.max(1);
    let start = entries.len().saturating_sub(max_entries);
    entries.into_iter().skip(start).collect()
}

fn push_latest(
    out: &mut Vec<ShellHistoryEntry>,
    entry: ShellHistoryEntry,
) {
    if let Some(idx) = out
        .iter()
        .position(|existing| existing.command == entry.command)
    {
        out.remove(idx);
    }
    out.push(entry);
}

fn atuin_env_present() -> bool {
    env::var_os("ATUIN_SESSION").is_some() || env::var_os("ATUIN_HISTORY_ID").is_some()
}

fn command_on_path(command: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|path| path.join(command).is_file())
}

#[derive(Debug)]
struct CommandOutput {
    stdout: Vec<u8>,
}

fn run_command<I, S>(
    command: impl AsRef<OsStr>,
    args: I,
    timeout: Duration,
) -> Result<CommandOutput, ShellHistoryError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let command_name = command.as_ref().to_string_lossy().to_string();
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ShellHistoryError::Command)?;

    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .map_err(ShellHistoryError::Command)?
            .is_some()
        {
            let output = child
                .wait_with_output()
                .map_err(ShellHistoryError::Command)?;
            if output.status.success() {
                return Ok(CommandOutput {
                    stdout: output.stdout,
                });
            }
            return Err(ShellHistoryError::CommandFailed(command_name));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ShellHistoryError::CommandTimedOut(command_name));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bash_history_with_optional_timestamps() {
        assert_eq!(
            parse_bash_history("cargo check\n#1710000000\ncargo test\n"),
            ["cargo check", "cargo test"]
        );
    }

    #[test]
    fn parses_zsh_extended_and_plain_history() {
        assert_eq!(
            parse_zsh_history(": 1710000000:0;cargo check\ncargo test\n"),
            ["cargo check", "cargo test"]
        );
    }

    #[test]
    fn parses_powershell_psreadline_history() {
        assert_eq!(
            parse_powershell_history("Get-ChildItem\r\ncargo test  \r\n\n"),
            ["Get-ChildItem", "cargo test"]
        );
    }

    #[test]
    fn parses_null_separated_history() {
        assert_eq!(
            parse_null_or_line_separated_history(b"cargo check\0cargo test\0"),
            ["cargo check", "cargo test"]
        );
    }

    #[test]
    fn limits_to_most_recent_entries() {
        let entries = entries(
            ShellHistorySource::Bash,
            vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
        );
        assert_eq!(
            limit_entries(entries, 2)
                .into_iter()
                .map(|entry| entry.command)
                .collect::<Vec<_>>(),
            ["two", "three"]
        );
    }

    #[test]
    fn dedupes_to_latest_command_position() {
        let entries = entries(
            ShellHistorySource::Bash,
            vec!["one".to_owned(), "two".to_owned(), "one".to_owned()],
        );
        assert_eq!(
            entries
                .into_iter()
                .map(|entry| entry.command)
                .collect::<Vec<_>>(),
            ["two", "one"]
        );
    }

    #[test]
    fn detects_supported_shell_from_path() {
        assert_eq!(
            detect_shell_provider(Some(OsStr::new("/usr/bin/fish"))),
            Some(ShellHistorySource::Fish)
        );
        assert_eq!(
            detect_shell_provider(Some(OsStr::new("/usr/bin/pwsh"))),
            Some(ShellHistorySource::PowerShell)
        );
        assert_eq!(
            detect_shell_provider(Some(OsStr::new("powershell.exe"))),
            Some(ShellHistorySource::PowerShell)
        );
    }
}
