//! System clipboard access with a graceful in-memory fallback.
//!
//! Wraps [`arboard`] and, when initialization fails (headless systems, CI,
//! disabled Wayland protocols), falls back to a per-process buffer so
//! OSC 52 round-trips still behave sensibly without producing runtime errors.

use arboard::Clipboard as ArboardClipboard;

/// Which selection an OSC 52 or right-click-copy operation targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardKind {
    Clipboard,
    Primary,
}

pub struct Clipboard {
    backend: Backend,
}

enum Backend {
    Real(ArboardClipboard),
    InMemory { clipboard: String, primary: String },
}

impl Clipboard {
    pub fn new() -> Self {
        let backend = match ArboardClipboard::new() {
            Ok(cb) => Backend::Real(cb),
            Err(e) => {
                warn!("clipboard unavailable, falling back to in-memory: {e}");
                Backend::InMemory {
                    clipboard: String::new(),
                    primary: String::new(),
                }
            }
        };
        Self { backend }
    }

    /// Construct a Clipboard that never touches the system — used by tests
    /// so results are deterministic regardless of whether a display server
    /// happens to be reachable from the test environment.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            backend: Backend::InMemory {
                clipboard: String::new(),
                primary: String::new(),
            },
        }
    }

    pub fn set(
        &mut self,
        kind: ClipboardKind,
        text: &str,
    ) {
        match &mut self.backend {
            Backend::Real(cb) => {
                let result = set_real(cb, kind, text);
                if let Err(e) = result {
                    warn!("clipboard set failed: {e}");
                }
            }
            Backend::InMemory { clipboard, primary } => {
                let slot = match kind {
                    ClipboardKind::Clipboard => clipboard,
                    ClipboardKind::Primary => primary,
                };
                slot.clear();
                slot.push_str(text);
            }
        }
    }

    pub fn get(
        &mut self,
        kind: ClipboardKind,
    ) -> Option<String> {
        match &mut self.backend {
            Backend::Real(cb) => match get_real(cb, kind) {
                Ok(text) => Some(text),
                Err(e) => {
                    warn!("clipboard get failed: {e}");
                    None
                }
            },
            Backend::InMemory { clipboard, primary } => {
                let slot = match kind {
                    ClipboardKind::Clipboard => clipboard,
                    ClipboardKind::Primary => primary,
                };
                Some(slot.clone())
            }
        }
    }
}

impl std::fmt::Debug for Clipboard {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        let variant = match self.backend {
            Backend::Real(_) => "real",
            Backend::InMemory { .. } => "in-memory",
        };
        f.debug_struct("Clipboard")
            .field("backend", &variant)
            .finish()
    }
}

#[cfg(target_os = "linux")]
fn set_real(
    cb: &mut ArboardClipboard,
    kind: ClipboardKind,
    text: &str,
) -> Result<(), arboard::Error> {
    use arboard::LinuxClipboardKind;
    use arboard::SetExtLinux;
    let linux_kind = match kind {
        ClipboardKind::Clipboard => LinuxClipboardKind::Clipboard,
        ClipboardKind::Primary => LinuxClipboardKind::Primary,
    };
    cb.set().clipboard(linux_kind).text(text.to_owned())
}

#[cfg(target_os = "linux")]
fn get_real(
    cb: &mut ArboardClipboard,
    kind: ClipboardKind,
) -> Result<String, arboard::Error> {
    use arboard::GetExtLinux;
    use arboard::LinuxClipboardKind;
    let linux_kind = match kind {
        ClipboardKind::Clipboard => LinuxClipboardKind::Clipboard,
        ClipboardKind::Primary => LinuxClipboardKind::Primary,
    };
    cb.get().clipboard(linux_kind).text()
}

// On non-Linux targets arboard has only one clipboard; primary degrades to it.
#[cfg(not(target_os = "linux"))]
fn set_real(
    cb: &mut ArboardClipboard,
    _kind: ClipboardKind,
    text: &str,
) -> Result<(), arboard::Error> {
    cb.set_text(text.to_owned())
}

#[cfg(not(target_os = "linux"))]
fn get_real(
    cb: &mut ArboardClipboard,
    _kind: ClipboardKind,
) -> Result<String, arboard::Error> {
    cb.get_text()
}
