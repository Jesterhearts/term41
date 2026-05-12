use std::path::PathBuf;

use percent_encoding::percent_decode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DirectoryAction {
    Clear,
    Set(PathBuf),
}

pub(super) fn parse_file_uri(rest: &[u8]) -> Option<DirectoryAction> {
    if rest.is_empty() {
        return Some(DirectoryAction::Clear);
    }

    let uri = std::str::from_utf8(rest).ok()?;
    let rest = uri.strip_prefix("file://")?;
    let path_start = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..path_start];
    let encoded_path = &rest[path_start..];
    if encoded_path.is_empty() {
        return None;
    }

    let decoded = percent_decode(encoded_path.as_bytes()).collect::<Vec<u8>>();
    let path = std::str::from_utf8(&decoded).ok()?;
    absolute_directory_action(normalize_file_uri_path(authority, path))
}

pub(super) fn parse_absolute_or_file(value: &[u8]) -> Option<DirectoryAction> {
    if value.is_empty() {
        return Some(DirectoryAction::Clear);
    }

    let path = std::str::from_utf8(value).ok()?;
    if path.starts_with("file://") {
        return parse_file_uri(value);
    }
    absolute_directory_action(PathBuf::from(path))
}

pub(super) fn apply(
    action: DirectoryAction,
    current_directory: &mut Option<PathBuf>,
) {
    match action {
        DirectoryAction::Clear => *current_directory = None,
        DirectoryAction::Set(path) => *current_directory = Some(path),
    }
}

fn absolute_directory_action(path: PathBuf) -> Option<DirectoryAction> {
    path.is_absolute().then_some(DirectoryAction::Set(path))
}

fn normalize_file_uri_path(
    authority: &str,
    path: &str,
) -> PathBuf {
    #[cfg(unix)]
    {
        // PowerShell snippets commonly build OSC 7 as
        // `file://$hostName/$escapedPath`. On Unix, `$escapedPath` already
        // starts with `/`, producing `file://host//home/...`. Treat that as
        // the local absolute path the shell intended to report.
        if !authority.is_empty() && path.starts_with("//") {
            return PathBuf::from(&path[1..]);
        }
    }

    PathBuf::from(path)
}
