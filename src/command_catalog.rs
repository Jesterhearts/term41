use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub(crate) struct CommandCatalog {
    names: Vec<String>,
}

impl CommandCatalog {
    pub(crate) fn from_environment() -> Self {
        let dirs = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .unwrap_or_default();
        Self {
            names: command_names_in_dirs(dirs),
        }
    }

    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }
}

fn command_names_in_dirs<I, P>(dirs: I) -> Vec<String>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut names = BTreeSet::new();
    for dir in dirs {
        collect_command_names(dir.as_ref(), &mut names);
    }
    names.into_iter().collect()
}

fn collect_command_names(
    dir: &Path,
    names: &mut BTreeSet<String>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !is_executable_file(&entry) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if let Some(command) = command_name(&name) {
            names.insert(command.to_owned());
        }
    }
}

#[cfg(unix)]
fn is_executable_file(entry: &fs::DirEntry) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = entry.metadata() else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable_file(entry: &fs::DirEntry) -> bool {
    let Ok(metadata) = entry.metadata() else {
        return false;
    };
    metadata.is_file()
}

#[cfg(unix)]
fn command_name(name: &str) -> Option<&str> {
    Some(name)
}

#[cfg(windows)]
fn command_name(name: &str) -> Option<&str> {
    let pathext = std::env::var("PATHEXT").ok()?;
    let extension = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))?;
    let executable_extension = pathext
        .split(';')
        .any(|ext| ext.eq_ignore_ascii_case(&extension));
    if executable_extension {
        Path::new(name).file_stem().and_then(|stem| stem.to_str())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn command_names_include_executable_files_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_test_dir("path-commands");
        fs::create_dir_all(root.join("bin")).expect("create bin dir");
        fs::write(root.join("bin/rg"), "").expect("write executable");
        fs::write(root.join("bin/readme"), "").expect("write non-executable");
        let mut permissions = fs::metadata(root.join("bin/rg"))
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(root.join("bin/rg"), permissions).expect("set permissions");

        assert_eq!(command_names_in_dirs([root.join("bin")]), vec!["rg"]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_names_are_sorted_and_deduplicated() {
        let root = unique_test_dir("path-dedupe");
        fs::create_dir_all(root.join("a")).expect("create dir");
        fs::create_dir_all(root.join("b")).expect("create dir");
        write_executable(root.join("a/git"));
        write_executable(root.join("b/cargo"));
        write_executable(root.join("b/git"));

        assert_eq!(
            command_names_in_dirs([root.join("a"), root.join("b")]),
            vec!["cargo", "git"]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn write_executable(path: std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(&path, "").expect("write executable");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("set permissions");
    }

    #[cfg(windows)]
    fn write_executable(path: std::path::PathBuf) {
        fs::write(path.with_extension("exe"), "").expect("write executable");
    }

    fn unique_test_dir(label: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "term41-command-catalog-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }
}
