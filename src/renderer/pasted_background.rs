use std::path::Path;
use std::path::PathBuf;

use config41::Config;

/// Directory where `PasteAsBackground` persists images.
/// `~/.local/share/term41/` on Linux, `~/Library/Application Support/term41/`
/// on macOS, `%APPDATA%\term41\` on Windows. Returns `None` on platforms
/// where `dirs` can't resolve a data dir (rare — usually broken environment).
pub(super) fn pasted_background_dir() -> Option<PathBuf> {
    term41_data_dir()
}

pub(crate) fn term41_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("term41"))
}

/// Build the full pasted-background path for a given file extension.
pub(super) fn pasted_background_path(ext: &str) -> Option<PathBuf> {
    pasted_background_dir().map(|d| d.join(format!("pasted_background.{ext}")))
}

/// Find an existing pasted-background file, regardless of extension.
/// Returns the first match found; there should only ever be one because
/// `clear_pasted_backgrounds` deletes all variants before a new save.
pub(super) fn find_pasted_background() -> Option<PathBuf> {
    let dir = pasted_background_dir()?;
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("pasted_background."))
        {
            return Some(entry.path());
        }
    }
    None
}

/// Delete every `pasted_background.*` file in the data directory so a
/// fresh paste doesn't leave a stale file from a previous format.
pub(super) fn clear_pasted_backgrounds() {
    let Some(dir) = pasted_background_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("pasted_background."))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Resolve which background image to actually load: pasted-image-on-disk
/// always wins over the config-supplied path. The "pasted always wins
/// until cleared" rule keeps the precedence one-line debuggable —
/// "does a pasted file exist?" is the whole question.
pub(crate) fn effective_bg_path(config: &Config) -> Option<PathBuf> {
    find_pasted_background().or_else(|| config.background_image.clone())
}

/// Encode an RGBA byte buffer to PNG at `path`. Always RGBA8 — the
/// clipboard hands us pixels in that layout and the renderer reads them
/// back the same way, so there's no need for a more flexible encoder.
pub(super) fn encode_png_rgba(
    path: &Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer
        .write_image_data(rgba)
        .map_err(std::io::Error::other)?;
    Ok(())
}
