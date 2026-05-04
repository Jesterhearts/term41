use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

const BASH_HOOK: &str = r#"
if [ -z "${__TERM41_SHELL_INTEGRATION_INSTALLED:-}" ]; then
  __TERM41_SHELL_INTEGRATION_INSTALLED=1
  __term41_prompt_seen=0
  __term41_command_running=0
  __term41_in_prompt=0

  __term41_emit_osc133() {
    printf '\033]133;%s\007' "$1"
  }

  __term41_prompt_command() {
    local __term41_status=$?
    __term41_in_prompt=1
    if [ "${__term41_prompt_seen:-0}" = 1 ] && [ "${__term41_command_running:-0}" = 1 ]; then
      __term41_emit_osc133 "D;${__term41_status}"
    fi
    __term41_command_running=0
    __term41_prompt_seen=1
    __term41_in_prompt=0
    return "$__term41_status"
  }

  __term41_preexec() {
    if [ "${__term41_in_prompt:-0}" = 1 ]; then
      return
    fi
    if [ "${__term41_command_running:-0}" = 0 ]; then
      __term41_emit_osc133 C
      __term41_command_running=1
    fi
  }

  trap '__term41_preexec' DEBUG

  case "$PS1" in
    *"]133;B"*) ;;
    *) PS1='\[\033]133;A\007\]'"${PS1}"'\[\033]133;B\007\]' ;;
  esac

  __term41_prompt_decl="$(declare -p PROMPT_COMMAND 2>/dev/null)"
  case "$__term41_prompt_decl" in
    declare\ -a*|declare\ -ax*)
      PROMPT_COMMAND=(__term41_prompt_command "${PROMPT_COMMAND[@]}")
      ;;
    *)
      if [ -n "${PROMPT_COMMAND:-}" ]; then
        PROMPT_COMMAND="__term41_prompt_command; ${PROMPT_COMMAND}"
      else
        PROMPT_COMMAND=__term41_prompt_command
      fi
      ;;
  esac
  unset __term41_prompt_decl
fi
"#;

const ZSH_HOOK: &str = r#"
if [[ -z ${__TERM41_SHELL_INTEGRATION_INSTALLED:-} ]]; then
  typeset -g __TERM41_SHELL_INTEGRATION_INSTALLED=1
  typeset -g __term41_prompt_seen=0
  typeset -g __term41_command_running=0

  __term41_precmd() {
    local __term41_status=$?
    if (( __term41_prompt_seen && __term41_command_running )); then
      printf '\e]133;D;%d\a' "$__term41_status"
    fi
    __term41_command_running=0
    __term41_prompt_seen=1
    printf '\e]133;A\a'
    return "$__term41_status"
  }

  __term41_preexec() {
    printf '\e]133;C\a'
    __term41_command_running=1
  }

  autoload -Uz add-zsh-hook
  add-zsh-hook precmd __term41_precmd
  add-zsh-hook preexec __term41_preexec

  if [[ ${PROMPT:-} != *']133;B'* ]]; then
    PROMPT="${PROMPT:-}%{\e]133;B\a%}"
  fi
fi
"#;

const FISH_HOOK: &str = r#"
if not set -q __TERM41_SHELL_INTEGRATION_INSTALLED
    set -g __TERM41_SHELL_INTEGRATION_INSTALLED 1
    set -g __term41_command_running 0

    if functions -q fish_prompt
        functions -c fish_prompt __term41_original_fish_prompt
    end

    function fish_prompt
        if functions -q __term41_original_fish_prompt
            __term41_original_fish_prompt
        end
        printf '\e]133;B\a'
    end

    function __term41_preexec --on-event fish_preexec
        printf '\e]133;C\a'
        set -g __term41_command_running 1
    end

    function __term41_postexec --on-event fish_postexec
        set -l __term41_status $status
        if test "$__term41_command_running" = 1
            printf '\e]133;D;%s\a' $__term41_status
        end
        printf '\e]133;A\a'
        set -g __term41_command_running 0
        return $__term41_status
    end

    printf '\e]133;A\a'
end
"#;

const POWERSHELL_HOOK: &str = r#"
if (-not $global:__TERM41_SHELL_INTEGRATION_INSTALLED) {
    $global:__TERM41_SHELL_INTEGRATION_INSTALLED = $true
    $global:__term41PromptSeen = $false
    $global:__term41CommandRunning = $false
    $global:__term41OriginalPrompt = (Get-Command prompt -CommandType Function).ScriptBlock

    function global:prompt {
        $term41Success = $?
        $term41Exit = if ($term41Success) {
            0
        } elseif ($global:LASTEXITCODE -is [int]) {
            $global:LASTEXITCODE
        } else {
            1
        }

        if ($global:__term41PromptSeen -and $global:__term41CommandRunning) {
            [Console]::Write("`e]133;D;$term41Exit`a")
        }
        $global:__term41PromptSeen = $true
        $global:__term41CommandRunning = $false
        [Console]::Write("`e]133;A`a")
        $term41PromptText = (& $global:__term41OriginalPrompt) -join ""
        "$term41PromptText`e]133;B`a"
    }

    if (Get-Command Set-PSReadLineOption -ErrorAction SilentlyContinue) {
        Set-PSReadLineOption -AddToHistoryHandler {
            param([string] $line)
            if (-not [string]::IsNullOrWhiteSpace($line)) {
                [Console]::Write("`e]133;C`a")
                $global:__term41CommandRunning = $true
            }
            return $true
        }
    }
}
"#;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellDetection {
    Supported { shell: Shell, path: OsString },
    Unsupported { name: String, path: OsString },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookedCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
pub struct InstalledHooks {
    command: HookedCommand,
    temp_dir: PathBuf,
}

impl InstalledHooks {
    pub fn command(&self) -> &HookedCommand {
        &self.command
    }
}

impl Drop for InstalledHooks {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

#[derive(Debug)]
pub enum HookInstallError {
    Io(io::Error),
}

impl fmt::Display for HookInstallError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "failed to install shell hooks: {error}"),
        }
    }
}

impl std::error::Error for HookInstallError {}

impl From<io::Error> for HookInstallError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn detect_current_shell() -> ShellDetection {
    detect_shell_path(current_shell_path().as_deref())
}

pub fn detect_shell_path(shell: Option<&OsStr>) -> ShellDetection {
    let Some(path) = shell else {
        return ShellDetection::Unknown;
    };
    let name = shell_name(path);
    let shell = match name.as_deref() {
        Some("bash") => Some(Shell::Bash),
        Some("zsh") => Some(Shell::Zsh),
        Some("fish") => Some(Shell::Fish),
        Some("pwsh" | "pwsh.exe" | "powershell" | "powershell.exe") => Some(Shell::PowerShell),
        _ => None,
    };
    match (shell, name) {
        (Some(shell), _) => ShellDetection::Supported {
            shell,
            path: path.to_owned(),
        },
        (None, Some(name)) => ShellDetection::Unsupported {
            name,
            path: path.to_owned(),
        },
        (None, None) => ShellDetection::Unknown,
    }
}

pub fn install_current_shell_hooks() -> Result<Option<InstalledHooks>, HookInstallError> {
    install_shell_hooks(detect_current_shell())
}

pub fn install_shell_hooks(
    detection: ShellDetection
) -> Result<Option<InstalledHooks>, HookInstallError> {
    let (shell, path) = match detection {
        ShellDetection::Supported { shell, path } => (shell, path),
        ShellDetection::Unsupported { name, .. } => {
            log::warn!("shell hooks unavailable: no hooks for shell {name:?}");
            return Ok(None);
        }
        ShellDetection::Unknown => {
            log::warn!("shell hooks unavailable: could not identify current shell");
            return Ok(None);
        }
    };

    let temp_dir = create_temp_dir()?;
    let command = match shell {
        Shell::Bash => install_bash_hooks(&temp_dir, path)?,
        Shell::Zsh => install_zsh_hooks(&temp_dir, path)?,
        Shell::Fish => install_fish_hooks(&temp_dir, path)?,
        Shell::PowerShell => install_powershell_hooks(&temp_dir, path)?,
    };
    Ok(Some(InstalledHooks { command, temp_dir }))
}

fn current_shell_path() -> Option<OsString> {
    std::env::var_os("SHELL").or_else(|| std::env::var_os("ComSpec"))
}

fn shell_name(path: &OsStr) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(OsStr::to_str)
        .map(|name| name.to_ascii_lowercase())
}

fn create_temp_dir() -> io::Result<PathBuf> {
    let pid = std::process::id();
    for _ in 0..100 {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("term41-hooks-{pid}-{id}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique hook temp directory",
    ))
}

fn install_bash_hooks(
    temp_dir: &Path,
    shell_path: OsString,
) -> io::Result<HookedCommand> {
    let hook_path = write_hook(temp_dir, "term41.bash", BASH_HOOK)?;
    let rc_path = temp_dir.join("bashrc");
    fs::write(
        &rc_path,
        format!(
            r#"if [ -r "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi
. "{}"
"#,
            shell_quote(&hook_path)
        ),
    )?;
    Ok(HookedCommand {
        program: shell_path,
        args: vec![
            OsString::from("--rcfile"),
            rc_path.into(),
            OsString::from("-i"),
        ],
        env: Vec::new(),
    })
}

fn install_zsh_hooks(
    temp_dir: &Path,
    shell_path: OsString,
) -> io::Result<HookedCommand> {
    let hook_path = write_hook(temp_dir, "term41.zsh", ZSH_HOOK)?;
    fs::write(
        temp_dir.join(".zshenv"),
        r#"if [[ -r "$HOME/.zshenv" ]]; then
  source "$HOME/.zshenv"
fi
"#,
    )?;
    fs::write(
        temp_dir.join(".zprofile"),
        r#"if [[ -r "$HOME/.zprofile" ]]; then
  source "$HOME/.zprofile"
fi
"#,
    )?;
    fs::write(
        temp_dir.join(".zshrc"),
        format!(
            r#"if [[ -r "$HOME/.zshrc" ]]; then
  source "$HOME/.zshrc"
fi
source "{}"
unset ZDOTDIR
"#,
            shell_quote(&hook_path)
        ),
    )?;
    Ok(HookedCommand {
        program: shell_path,
        args: vec![OsString::from("-l")],
        env: vec![(OsString::from("ZDOTDIR"), temp_dir.into())],
    })
}

fn install_fish_hooks(
    temp_dir: &Path,
    shell_path: OsString,
) -> io::Result<HookedCommand> {
    let hook_path = write_hook(temp_dir, "term41.fish", FISH_HOOK)?;
    Ok(HookedCommand {
        program: shell_path,
        args: vec![
            OsString::from("--init-command"),
            OsString::from(format!("source {}", fish_quote(&hook_path))),
        ],
        env: Vec::new(),
    })
}

fn install_powershell_hooks(
    temp_dir: &Path,
    shell_path: OsString,
) -> io::Result<HookedCommand> {
    let hook_path = write_hook(temp_dir, "term41.ps1", POWERSHELL_HOOK)?;
    Ok(HookedCommand {
        program: shell_path,
        args: vec![
            OsString::from("-NoLogo"),
            OsString::from("-NoExit"),
            OsString::from("-Command"),
            OsString::from(format!(". {}", powershell_quote(&hook_path))),
        ],
        env: Vec::new(),
    })
}

fn write_hook(
    temp_dir: &Path,
    name: &str,
    contents: &str,
) -> io::Result<PathBuf> {
    let path = temp_dir.join(name);
    fs::write(&path, contents.trim_start())?;
    Ok(path)
}

fn shell_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', r#"'\''"#)
}

fn fish_quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
    )
}

fn powershell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_shells_from_paths() {
        assert_eq!(
            detect_shell_path(Some(OsStr::new("/bin/bash"))),
            ShellDetection::Supported {
                shell: Shell::Bash,
                path: OsString::from("/bin/bash")
            }
        );
        assert_eq!(
            detect_shell_path(Some(OsStr::new("pwsh.exe"))),
            ShellDetection::Supported {
                shell: Shell::PowerShell,
                path: OsString::from("pwsh.exe")
            }
        );
    }

    #[test]
    fn reports_named_unsupported_shell() {
        assert_eq!(
            detect_shell_path(Some(OsStr::new("/usr/bin/nu"))),
            ShellDetection::Unsupported {
                name: "nu".to_owned(),
                path: OsString::from("/usr/bin/nu")
            }
        );
    }

    #[test]
    fn installs_bash_with_temp_rcfile() {
        let installed = install_shell_hooks(ShellDetection::Supported {
            shell: Shell::Bash,
            path: OsString::from("/bin/bash"),
        })
        .unwrap()
        .unwrap();

        assert_eq!(installed.command.program, OsString::from("/bin/bash"));
        assert_eq!(installed.command.args[0], OsString::from("--rcfile"));
        let rc = fs::read_to_string(&installed.command.args[1]).unwrap();
        assert!(rc.contains(".bashrc"));
        assert!(rc.contains("term41.bash"));
    }

    #[test]
    fn hook_snippets_emit_all_lifecycle_markers() {
        for hook in [BASH_HOOK, ZSH_HOOK, FISH_HOOK, POWERSHELL_HOOK] {
            assert!(hook_emits_marker(hook, "A"));
            assert!(hook_emits_marker(hook, "B"));
            assert!(hook_emits_marker(hook, "C"));
            assert!(hook_emits_marker(hook, "D"));
        }
    }

    #[test]
    fn powershell_appends_command_start_to_prompt_text() {
        assert!(POWERSHELL_HOOK.contains(r#""$term41PromptText`e]133;B`a""#));
        assert!(!POWERSHELL_HOOK.contains(r#"[Console]::Write("`e]133;B`a")"#));
    }

    fn hook_emits_marker(
        hook: &str,
        marker: &str,
    ) -> bool {
        hook.contains(&format!("133;{marker}"))
            || hook.contains(&format!("__term41_emit_osc133 {marker}"))
            || hook.contains(&format!("__term41_emit_osc133 \"{marker};"))
    }
}
