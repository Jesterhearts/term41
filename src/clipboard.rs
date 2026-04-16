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

    /// Read the system clipboard as raw RGBA pixel data. Returns `None`
    /// when the clipboard doesn't hold an image (the common case — most
    /// pastes are text), the in-memory test fallback is in use, or the
    /// platform backend reports an error. Errors get a `debug!` log
    /// rather than `warn!` because "no image on the clipboard" lands here
    /// too, and that's the expected path for a regular paste.
    pub fn get_image(&mut self) -> Option<ClipboardImage> {
        let Backend::Real(cb) = &mut self.backend else {
            return None;
        };
        match cb.get_image() {
            Ok(img) => Some(ClipboardImage {
                width: img.width as u32,
                height: img.height as u32,
                rgba: img.bytes.into_owned(),
            }),
            Err(e) => {
                debug!("clipboard image not available: {e}");
                None
            }
        }
    }
}

/// Owned RGBA pixel data from the system clipboard. `width * height * 4`
/// bytes, row-major, top-down — same layout as `DecodedImage` everywhere
/// else in the codebase, so the bytes can be re-encoded or passed
/// through to the renderer without conversion.
pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
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
