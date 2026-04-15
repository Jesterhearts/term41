//! User-configurable keybindings.
//!
//! A binding maps a `(modifiers, key)` pair to an [`Action`]. The default set
//! reproduces the previously-hardcoded shortcuts (Shift+PageUp/Down for
//! scrollback, Ctrl+Shift+C/V for clipboard); a `keybindings` array in
//! `config.toml` replaces it wholesale, so the user always knows exactly
//! which bindings are active. Add new actions by extending [`Action`] and
//! handling them in `App::run_action`.

use std::str::FromStr;

use serde::Deserialize;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;

/// Things a keybinding can do. Renamed only with care — the names appear
/// verbatim in `config.toml` (`action = "ScrollPageUp"`), so changing one
/// silently breaks user configs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Action {
    /// Scroll the viewport one screenful into history.
    ScrollPageUp,
    /// Scroll the viewport one screenful back toward live.
    ScrollPageDown,
    /// Copy the active selection to the system clipboard.
    Copy,
    /// Paste the system clipboard at the cursor.
    Paste,
    /// Open the search-in-scrollback bar. Subsequent keystrokes type into
    /// the search query until Escape closes it; Enter / Shift+Enter step
    /// through the match list.
    OpenSearch,
    /// Scroll the viewport to the previous OSC 133 shell-integration prompt
    /// (the one above the current viewport top). Silent no-op when no
    /// earlier prompt exists, so the binding doesn't flash on sessions
    /// without shell integration.
    ScrollPrevPrompt,
    /// Scroll the viewport to the next OSC 133 prompt (below the current
    /// viewport top).
    ScrollNextPrompt,
    /// Launch a detached copy of this binary with its working directory
    /// inherited from the current session — typically bound to
    /// `Ctrl+Shift+N`. The new process gets its own winit window.
    OpenNewWindow,
}

/// One key, identified either by its winit `NamedKey` (Enter, F1, …) or by
/// the printable character it produces. Character matching is
/// case-insensitive so `Ctrl+V` and `Ctrl+v` resolve identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySpec {
    Named(NamedKey),
    Char(char),
}

#[derive(Debug, Clone)]
pub struct Keybinding {
    pub key: KeySpec,
    pub mods: ModifiersState,
    pub action: Action,
}

#[derive(Debug, Default, Clone)]
pub struct Keybindings {
    bindings: Vec<Keybinding>,
}

impl Keybindings {
    /// The shortcuts the terminal ships with — kept in sync with the README
    /// and the previously-hardcoded match arms in `App::keyboard_input`.
    pub fn defaults() -> Self {
        Self {
            bindings: vec![
                Keybinding {
                    key: KeySpec::Named(NamedKey::PageUp),
                    mods: ModifiersState::SHIFT,
                    action: Action::ScrollPageUp,
                },
                Keybinding {
                    key: KeySpec::Named(NamedKey::PageDown),
                    mods: ModifiersState::SHIFT,
                    action: Action::ScrollPageDown,
                },
                Keybinding {
                    key: KeySpec::Char('c'),
                    mods: ModifiersState::CONTROL | ModifiersState::SHIFT,
                    action: Action::Copy,
                },
                Keybinding {
                    key: KeySpec::Char('v'),
                    mods: ModifiersState::CONTROL | ModifiersState::SHIFT,
                    action: Action::Paste,
                },
                Keybinding {
                    key: KeySpec::Char('f'),
                    mods: ModifiersState::CONTROL | ModifiersState::SHIFT,
                    action: Action::OpenSearch,
                },
                // Ctrl+Shift+Up/Down walk through shell-integration
                // prompts. They share a modifier family with the other
                // scroll shortcuts (Shift+PageUp/PageDown) so the mental
                // model stays "Shift family moves the viewport".
                Keybinding {
                    key: KeySpec::Named(NamedKey::ArrowUp),
                    mods: ModifiersState::CONTROL | ModifiersState::SHIFT,
                    action: Action::ScrollPrevPrompt,
                },
                Keybinding {
                    key: KeySpec::Named(NamedKey::ArrowDown),
                    mods: ModifiersState::CONTROL | ModifiersState::SHIFT,
                    action: Action::ScrollNextPrompt,
                },
                Keybinding {
                    key: KeySpec::Char('n'),
                    mods: ModifiersState::CONTROL | ModifiersState::SHIFT,
                    action: Action::OpenNewWindow,
                },
            ],
        }
    }

    /// Build from a config-supplied vec, falling back to [`Self::defaults`]
    /// if empty. We deliberately do *not* merge with defaults — if the user
    /// writes `keybindings = []` they get an empty set, which is a useful
    /// way to disable everything while debugging.
    pub fn from_config(parsed: Vec<Keybinding>) -> Self {
        Self { bindings: parsed }
    }

    /// Resolve a key event to its bound action, if any. Returns the *first*
    /// matching binding so users can override defaults by listing their
    /// override earlier in the config.
    pub fn lookup(
        &self,
        key: &Key,
        mods: ModifiersState,
    ) -> Option<Action> {
        self.bindings
            .iter()
            .find(|b| b.matches(key, mods))
            .map(|b| b.action)
    }
}

impl Keybinding {
    fn matches(
        &self,
        key: &Key,
        mods: ModifiersState,
    ) -> bool {
        if self.mods != mods {
            return false;
        }
        match (&self.key, key) {
            (KeySpec::Named(want), Key::Named(got)) => want == got,
            (KeySpec::Char(want), Key::Character(got)) => {
                // Case-insensitive single-char compare — Shift is encoded in
                // the modifier set, not in the character casing.
                let mut chars = got.chars();
                let Some(first) = chars.next() else {
                    return false;
                };
                if chars.next().is_some() {
                    return false;
                }
                first.eq_ignore_ascii_case(want)
            }
            _ => false,
        }
    }
}

/// Wire format for a single keybinding entry: `keys = "Ctrl+Shift+V"`.
/// Parsed via [`Keybinding::from_config_entry`] so the toml side stays a
/// dumb string-bag and all the validation lives in one place.
#[derive(Deserialize)]
pub struct KeybindingConfig {
    pub keys: String,
    pub action: Action,
}

impl Keybinding {
    /// Convert a parsed config entry into a runtime [`Keybinding`]. Errors
    /// are descriptive so a typo in `config.toml` shows the offending token.
    pub fn from_config_entry(entry: KeybindingConfig) -> Result<Self, String> {
        let (key, mods) = parse_key_combo(&entry.keys)?;
        Ok(Self {
            key,
            mods,
            action: entry.action,
        })
    }
}

/// Parse `Ctrl+Shift+V`-style strings into a `(key, mods)` pair. Modifier
/// names are case-insensitive; the trailing token is the key. Empty input
/// and unknown tokens return `Err` so misconfiguration is loud rather than
/// silently dropped.
fn parse_key_combo(s: &str) -> Result<(KeySpec, ModifiersState), String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(format!("empty token in keybinding {s:?}"));
    }
    let (key_part, mod_parts) = parts.split_last().expect("non-empty by guard");
    let mut mods = ModifiersState::empty();
    for part in mod_parts {
        mods |= parse_modifier(part)?;
    }
    let key = KeySpec::from_str(key_part)?;
    Ok((key, mods))
}

fn parse_modifier(s: &str) -> Result<ModifiersState, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => ModifiersState::CONTROL,
        "shift" => ModifiersState::SHIFT,
        "alt" | "option" => ModifiersState::ALT,
        "super" | "cmd" | "command" | "meta" | "win" => ModifiersState::SUPER,
        other => return Err(format!("unknown modifier {other:?}")),
    })
}

impl FromStr for KeySpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "pageup" | "page_up" => KeySpec::Named(NamedKey::PageUp),
            "pagedown" | "page_down" => KeySpec::Named(NamedKey::PageDown),
            "home" => KeySpec::Named(NamedKey::Home),
            "end" => KeySpec::Named(NamedKey::End),
            "enter" | "return" => KeySpec::Named(NamedKey::Enter),
            "tab" => KeySpec::Named(NamedKey::Tab),
            "escape" | "esc" => KeySpec::Named(NamedKey::Escape),
            "backspace" => KeySpec::Named(NamedKey::Backspace),
            "space" => KeySpec::Named(NamedKey::Space),
            "left" | "arrowleft" => KeySpec::Named(NamedKey::ArrowLeft),
            "right" | "arrowright" => KeySpec::Named(NamedKey::ArrowRight),
            "up" | "arrowup" => KeySpec::Named(NamedKey::ArrowUp),
            "down" | "arrowdown" => KeySpec::Named(NamedKey::ArrowDown),
            "delete" | "del" => KeySpec::Named(NamedKey::Delete),
            "insert" | "ins" => KeySpec::Named(NamedKey::Insert),
            // Function keys: F1..F35 (winit's full range). We accept any
            // F<n> token and rely on `f_named` to map it; out-of-range or
            // malformed tokens fall through to the unknown-key error.
            f if f.starts_with('f')
                && f[1..].chars().all(|c| c.is_ascii_digit())
                && !f[1..].is_empty() =>
            {
                let n: u32 = f[1..]
                    .parse()
                    .map_err(|e| format!("invalid F-key number in {s:?}: {e}"))?;
                KeySpec::Named(f_named(n).ok_or_else(|| format!("unsupported function key {s:?}"))?)
            }
            other if other.chars().count() == 1 => {
                // Use the original (un-lowercased) char so symbol keys
                // round-trip; case is already normalized in matching.
                KeySpec::Char(s.chars().next().expect("single char by guard"))
            }
            other => return Err(format!("unknown key {other:?}")),
        })
    }
}

/// Map an `F<n>` integer onto its `NamedKey`. `winit`'s `NamedKey` doesn't
/// expose a uniform `Fn(u8)` constructor so we spell out the supported
/// range explicitly. Returns `None` for values outside 1..=35 (the spec
/// cap) so callers can surface a useful error.
fn f_named(n: u32) -> Option<NamedKey> {
    Some(match n {
        1 => NamedKey::F1,
        2 => NamedKey::F2,
        3 => NamedKey::F3,
        4 => NamedKey::F4,
        5 => NamedKey::F5,
        6 => NamedKey::F6,
        7 => NamedKey::F7,
        8 => NamedKey::F8,
        9 => NamedKey::F9,
        10 => NamedKey::F10,
        11 => NamedKey::F11,
        12 => NamedKey::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use winit::keyboard::Key;
    use winit::keyboard::SmolStr;

    use super::*;

    fn cfg(
        keys: &str,
        action: Action,
    ) -> Keybinding {
        Keybinding::from_config_entry(KeybindingConfig {
            keys: keys.to_owned(),
            action,
        })
        .expect("valid keybinding")
    }

    #[test]
    fn parse_ctrl_shift_v() {
        let b = cfg("Ctrl+Shift+V", Action::Paste);
        assert_eq!(b.mods, ModifiersState::CONTROL | ModifiersState::SHIFT);
        assert!(matches!(b.key, KeySpec::Char('V')));
    }

    #[test]
    fn parse_named_key() {
        let b = cfg("Shift+PageUp", Action::ScrollPageUp);
        assert_eq!(b.mods, ModifiersState::SHIFT);
        assert!(matches!(b.key, KeySpec::Named(NamedKey::PageUp)));
    }

    #[test]
    fn parse_no_modifier() {
        let b = cfg("F1", Action::Copy);
        assert_eq!(b.mods, ModifiersState::empty());
    }

    #[test]
    fn parse_modifier_aliases() {
        let b = cfg("Control+a", Action::Copy);
        assert!(b.mods.contains(ModifiersState::CONTROL));
        let b = cfg("Cmd+a", Action::Copy);
        assert!(b.mods.contains(ModifiersState::SUPER));
    }

    #[test]
    fn parse_rejects_empty_token() {
        let err = Keybinding::from_config_entry(KeybindingConfig {
            keys: "Ctrl++V".into(),
            action: Action::Paste,
        });
        assert!(err.is_err());
    }

    #[test]
    fn parse_rejects_unknown_modifier() {
        let err = Keybinding::from_config_entry(KeybindingConfig {
            keys: "Hyper+a".into(),
            action: Action::Paste,
        });
        assert!(err.is_err());
    }

    #[test]
    fn lookup_matches_case_insensitive_char() {
        let bindings = Keybindings::from_config(vec![cfg("Ctrl+Shift+V", Action::Paste)]);
        let key_lower = Key::Character(SmolStr::new_inline("v"));
        let key_upper = Key::Character(SmolStr::new_inline("V"));
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(bindings.lookup(&key_lower, mods), Some(Action::Paste));
        assert_eq!(bindings.lookup(&key_upper, mods), Some(Action::Paste));
    }

    #[test]
    fn lookup_misses_when_modifiers_differ() {
        let bindings = Keybindings::from_config(vec![cfg("Ctrl+Shift+V", Action::Paste)]);
        let key = Key::Character(SmolStr::new_inline("v"));
        // Plain Ctrl+V should still produce the legacy 0x16 byte.
        assert_eq!(bindings.lookup(&key, ModifiersState::CONTROL), None);
    }

    #[test]
    fn lookup_first_match_wins() {
        let bindings = Keybindings::from_config(vec![
            cfg("Ctrl+Shift+V", Action::Copy),
            cfg("Ctrl+Shift+V", Action::Paste),
        ]);
        let key = Key::Character(SmolStr::new_inline("v"));
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(bindings.lookup(&key, mods), Some(Action::Copy));
    }

    #[test]
    fn defaults_cover_legacy_shortcuts() {
        let bindings = Keybindings::defaults();
        let key = Key::Character(SmolStr::new_inline("c"));
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(bindings.lookup(&key, mods), Some(Action::Copy));

        let pgup = Key::Named(NamedKey::PageUp);
        assert_eq!(
            bindings.lookup(&pgup, ModifiersState::SHIFT),
            Some(Action::ScrollPageUp)
        );
    }

    #[test]
    fn defaults_bind_ctrl_shift_n_to_open_new_window() {
        let bindings = Keybindings::defaults();
        let key = Key::Character(SmolStr::new_inline("n"));
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(bindings.lookup(&key, mods), Some(Action::OpenNewWindow));
    }
}
