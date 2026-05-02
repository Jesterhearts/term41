use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use config41::CommandEditorConfig;

#[derive(Debug, Clone, Default)]
pub(crate) struct CommandCatalog {
    binary_dirs: Vec<PathBuf>,
    names: Vec<String>,
}

impl CommandCatalog {
    pub(crate) fn from_config(config: &CommandEditorConfig) -> Self {
        let binary_dirs = config.binary_dirs.clone();
        Self {
            names: command_names_in_dirs(command_dirs_from_environment(&binary_dirs)),
            binary_dirs,
        }
    }

    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    pub(crate) fn refresh_for_config(
        &mut self,
        config: &CommandEditorConfig,
    ) {
        if self.binary_dirs == config.binary_dirs {
            return;
        }
        *self = Self::from_config(config);
    }
}

fn command_dirs_from_environment(binary_dirs: &[PathBuf]) -> Vec<PathBuf> {
    command_dirs(std::env::var_os("PATH").as_deref(), binary_dirs)
}

fn command_dirs(
    path: Option<&OsStr>,
    binary_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            push_unique_path(&mut out, dir);
        }
    }
    for dir in binary_dirs {
        push_unique_path(&mut out, dir.clone());
    }
    out
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

fn push_unique_path(
    out: &mut Vec<PathBuf>,
    path: PathBuf,
) {
    if !path.as_os_str().is_empty() && !out.iter().any(|existing| existing == &path) {
        out.push(path);
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

    #[test]
    fn command_dirs_merge_path_and_config_dirs() {
        let root = unique_test_dir("config-binary-dirs");
        let path_dir = root.join("path-bin");
        let config_dir = root.join("config-bin");
        let path = std::env::join_paths([path_dir.as_path()]).expect("join path");

        assert_eq!(
            command_dirs(
                Some(path.as_os_str()),
                &[path_dir.clone(), config_dir.clone()]
            ),
            vec![path_dir, config_dir]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_binary_dirs_contribute_command_names() {
        let root = unique_test_dir("config-binary-dir-commands");
        let binary_dir = root.join("home").join(".cargo").join("bin");
        fs::create_dir_all(&binary_dir).expect("create binary dir");
        write_executable(binary_dir.join("cargo"));

        let dirs = command_dirs(None, &[binary_dir]);

        assert_eq!(command_names_in_dirs(dirs), vec!["cargo"]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn catalog_refreshes_when_config_binary_dirs_change() {
        let root = unique_test_dir("catalog-refresh");
        let first_dir = root.join("first");
        let second_dir = root.join("second");
        fs::create_dir_all(&first_dir).expect("create first dir");
        fs::create_dir_all(&second_dir).expect("create second dir");
        write_executable(first_dir.join("first-tool"));
        write_executable(second_dir.join("second-tool"));

        let mut first = CommandEditorConfig::default();
        first.binary_dirs = vec![first_dir];
        let mut second = CommandEditorConfig::default();
        second.binary_dirs = vec![second_dir];
        let mut catalog = CommandCatalog::from_config(&first);

        assert!(catalog.names().contains(&"first-tool".to_owned()));
        assert!(!catalog.names().contains(&"second-tool".to_owned()));
        catalog.refresh_for_config(&second);
        assert!(!catalog.names().contains(&"first-tool".to_owned()));
        assert!(catalog.names().contains(&"second-tool".to_owned()));

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
