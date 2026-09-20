use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

pub const DEADLOCK_APP_ID: u32 = 1_422_450;
pub const DEADLOCK_GAME_RELATIVE_PATH: &str = "game/citadel";
pub const DEADLOCK_LOG_RELATIVE_PATH: &str = "game/citadel/console.log";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Detection {
    Ready { path: PathBuf },
    NotCreated { path: PathBuf },
}

impl Detection {
    pub fn path(&self) -> &Path {
        match self {
            Self::Ready { path } | Self::NotCreated { path } => path,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DetectionError {
    UnsupportedPlatform,
    SteamNotFound,
    SteamUnreadable { path: PathBuf, details: String },
    DeadlockNotInstalled,
    InvalidInstall { paths: Vec<PathBuf> },
    MalformedMetadata { path: PathBuf, details: String },
    AmbiguousInstalls { paths: Vec<PathBuf> },
}

impl fmt::Display for DetectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => formatter
                .write_str("Deadlock auto-detection is supported only on Windows and Linux"),
            Self::SteamNotFound => formatter.write_str(
                "Steam was not found; verify that Steam is installed for the current user",
            ),
            Self::SteamUnreadable { path, details } => write!(
                formatter,
                "Steam data at {} could not be read: {details}",
                path.display()
            ),
            Self::DeadlockNotInstalled => formatter
                .write_str("Deadlock is not installed in any Steam library visible to this user"),
            Self::InvalidInstall { paths } => write!(
                formatter,
                "Deadlock metadata exists, but no valid game/citadel directory was found at {}",
                display_paths(paths)
            ),
            Self::MalformedMetadata { path, details } => write!(
                formatter,
                "Steam metadata at {} is malformed: {details}",
                path.display()
            ),
            Self::AmbiguousInstalls { paths } => write!(
                formatter,
                "multiple Deadlock installations are equally plausible ({}); enter the log path manually",
                display_paths(paths)
            ),
        }
    }
}

impl std::error::Error for DetectionError {}

#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    modified: Option<SystemTime>,
}

pub fn detect() -> Result<Detection, DetectionError> {
    let roots = platform_steam_roots()?;
    detect_in_roots(&roots)
}

pub(crate) fn detect_in_roots(roots: &[PathBuf]) -> Result<Detection, DetectionError> {
    let input_root_count = roots.len();
    let roots = deduplicate_paths(roots.iter().cloned());
    log::debug!(
        target: "companion::deadlock_path",
        "deadlock_roots discovered={} deduplicated={}",
        input_root_count,
        roots.len()
    );
    if roots.is_empty() {
        return Err(DetectionError::SteamNotFound);
    }

    let mut libraries = Vec::new();
    let mut readable_root_found = false;
    let mut first_unreadable = None;
    let mut first_malformed = None;

    for root in roots {
        match fs::metadata(&root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: root.clone(),
                    details: "the discovered Steam root is not a directory".to_owned(),
                });
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: root.clone(),
                    details: error.to_string(),
                });
                continue;
            }
        }

        let steam_dir = match steamlocate::SteamDir::from_dir(&root) {
            Ok(steam_dir) => steam_dir,
            Err(error) => {
                first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: root.clone(),
                    details: error.to_string(),
                });
                continue;
            }
        };
        readable_root_found = true;

        let library_metadata = root.join("steamapps").join("libraryfolders.vdf");
        match steam_dir.library_paths() {
            Ok(paths) => libraries.extend(paths),
            Err(error) if library_metadata.is_file() => {
                first_malformed.get_or_insert_with(|| DetectionError::MalformedMetadata {
                    path: library_metadata,
                    details: error.to_string(),
                });
            }
            Err(error) => {
                first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: library_metadata,
                    details: error.to_string(),
                });
            }
        }
    }

    if !readable_root_found {
        return Err(first_unreadable.unwrap_or(DetectionError::SteamNotFound));
    }

    let libraries = deduplicate_paths(libraries);
    log::debug!(
        target: "companion::deadlock_path",
        "deadlock_libraries count={}",
        libraries.len()
    );
    let mut candidates = Vec::new();
    let mut invalid_installs = Vec::new();
    let mut manifest_found = false;

    for library_path in libraries {
        let library = match steamlocate::Library::from_dir(&library_path) {
            Ok(library) => library,
            Err(error) => {
                first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: library_path.clone(),
                    details: error.to_string(),
                });
                continue;
            }
        };

        let Some(app) = library.app(DEADLOCK_APP_ID) else {
            continue;
        };
        manifest_found = true;
        let manifest_path = library
            .path()
            .join("steamapps")
            .join(format!("appmanifest_{DEADLOCK_APP_ID}.acf"));
        let app = match app.map_err(|error| DetectionError::MalformedMetadata {
            path: manifest_path.clone(),
            details: error.to_string(),
        }) {
            Ok(app) => app,
            Err(error) => {
                first_malformed.get_or_insert(error);
                continue;
            }
        };

        if app.app_id != DEADLOCK_APP_ID || !is_safe_install_dir(&app.install_dir) {
            first_malformed.get_or_insert_with(|| DetectionError::MalformedMetadata {
                path: manifest_path,
                details: "appid or installdir does not match the manifest location".to_owned(),
            });
            continue;
        }

        let install_path = library.resolve_app_dir(&app);
        let game_path = install_path.join(DEADLOCK_GAME_RELATIVE_PATH);
        match fs::metadata(&game_path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                invalid_installs.push(install_path);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                invalid_installs.push(install_path);
                continue;
            }
            Err(error) => {
                first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: game_path,
                    details: error.to_string(),
                });
                continue;
            }
        }

        let log_path = install_path.join(DEADLOCK_LOG_RELATIVE_PATH);
        let modified = match fs::metadata(&log_path) {
            Ok(metadata) if metadata.is_file() => match metadata.modified() {
                Ok(modified) => Some(modified),
                Err(error) => {
                    first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                        path: log_path.clone(),
                        details: format!("could not read log modification time: {error}"),
                    });
                    continue;
                }
            },
            Ok(_) => {
                invalid_installs.push(install_path);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                first_unreadable.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: log_path,
                    details: error.to_string(),
                });
                continue;
            }
        };
        candidates.push(Candidate {
            path: log_path,
            modified,
        });
    }

    candidates = deduplicate_candidates(candidates);
    invalid_installs = deduplicate_paths(invalid_installs);
    log::debug!(
        target: "companion::deadlock_path",
        "deadlock_candidates count={} invalid_installs={}",
        candidates.len(),
        invalid_installs.len()
    );
    if !candidates.is_empty() {
        return select_candidate(candidates);
    }
    if let Some(error) = first_malformed {
        return Err(error);
    }
    if let Some(error) = first_unreadable {
        return Err(error);
    }
    if manifest_found {
        return Err(DetectionError::InvalidInstall {
            paths: invalid_installs,
        });
    }
    Err(DetectionError::DeadlockNotInstalled)
}

fn select_candidate(mut candidates: Vec<Candidate>) -> Result<Detection, DetectionError> {
    let existing_count = candidates
        .iter()
        .filter(|candidate| candidate.modified.is_some())
        .count();

    if existing_count == 0 {
        if candidates.len() == 1 {
            log::debug!(
                target: "companion::deadlock_path",
                "deadlock_selection reason=only_candidate_log_missing"
            );
            return Ok(Detection::NotCreated {
                path: candidates.remove(0).path,
            });
        }
        log::debug!(
            target: "companion::deadlock_path",
            "deadlock_selection reason=ambiguous_without_logs candidates={}",
            candidates.len()
        );
        return Err(DetectionError::AmbiguousInstalls {
            paths: sorted_candidate_paths(candidates),
        });
    }

    candidates.retain(|candidate| candidate.modified.is_some());
    let newest = candidates
        .iter()
        .filter_map(|candidate| candidate.modified)
        .max()
        .expect("existing candidates have modification times");
    let mut newest_candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.modified == Some(newest))
        .collect();

    if newest_candidates.len() == 1 {
        log::debug!(
            target: "companion::deadlock_path",
            "deadlock_selection reason=newest_log"
        );
        Ok(Detection::Ready {
            path: newest_candidates.remove(0).path,
        })
    } else {
        log::debug!(
            target: "companion::deadlock_path",
            "deadlock_selection reason=tied_newest candidates={}",
            newest_candidates.len()
        );
        Err(DetectionError::AmbiguousInstalls {
            paths: sorted_candidate_paths(newest_candidates),
        })
    }
}

fn is_safe_install_dir(install_dir: &str) -> bool {
    let mut components = Path::new(install_dir).components();
    let has_component = matches!(components.next(), Some(Component::Normal(_)));
    has_component && components.all(|component| matches!(component, Component::Normal(_)))
}

fn deduplicate_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut unique = Vec::<(Candidate, PathBuf)>::new();
    for candidate in candidates {
        let comparison_path = canonicalize_for_comparison(&candidate.path);
        if let Some((existing, _)) = unique
            .iter_mut()
            .find(|(_, existing_path)| paths_equal(existing_path, &comparison_path))
        {
            if candidate.modified > existing.modified {
                existing.modified = candidate.modified;
            }
        } else {
            unique.push((candidate, comparison_path));
        }
    }
    let mut unique: Vec<_> = unique.into_iter().map(|(candidate, _)| candidate).collect();
    unique.sort_by(|left, right| left.path.cmp(&right.path));
    unique
}

fn deduplicate_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::<(PathBuf, PathBuf)>::new();
    for path in paths {
        let comparison_path = canonicalize_for_comparison(&path);
        if !unique
            .iter()
            .any(|(_, existing_path)| paths_equal(existing_path, &comparison_path))
        {
            unique.push((path, comparison_path));
        }
    }
    let mut unique: Vec<_> = unique.into_iter().map(|(path, _)| path).collect();
    unique.sort();
    unique
}

fn canonicalize_for_comparison(path: &Path) -> PathBuf {
    if path.exists() {
        return fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    }
    let Some(file_name) = path.file_name() else {
        return path.to_owned();
    };
    path.parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .map(|parent| parent.join(file_name))
        .unwrap_or_else(|| path.to_owned())
}

#[cfg(target_os = "windows")]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(target_os = "windows"))]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

fn sorted_candidate_paths(candidates: Vec<Candidate>) -> Vec<PathBuf> {
    let mut paths: Vec<_> = candidates
        .into_iter()
        .map(|candidate| candidate.path)
        .collect();
    paths.sort();
    paths
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn platform_steam_roots() -> Result<Vec<PathBuf>, DetectionError> {
    let located = steamlocate::locate_all();
    let roots: Vec<PathBuf> = located
        .as_ref()
        .map(|dirs| dirs.iter().map(|dir| dir.path().to_owned()).collect())
        .unwrap_or_default();

    #[cfg(target_os = "windows")]
    let roots = {
        let mut roots = roots;
        roots.extend(windows_steam_roots()?);
        roots
    };
    let roots = deduplicate_paths(roots);
    if !roots.is_empty() {
        return Ok(roots);
    }

    match located {
        Ok(_) => Err(DetectionError::SteamNotFound),
        Err(steamlocate::Error::Io { path, inner }) => Err(DetectionError::SteamUnreadable {
            path,
            details: inner.to_string(),
        }),
        Err(_) => Err(DetectionError::SteamNotFound),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn platform_steam_roots() -> Result<Vec<PathBuf>, DetectionError> {
    Err(DetectionError::UnsupportedPlatform)
}

#[cfg(target_os = "windows")]
fn windows_steam_roots() -> Result<Vec<PathBuf>, DetectionError> {
    use winreg::RegKey;
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };

    let mut roots = Vec::new();
    let mut meaningful_error = None;
    let registry_locations = [
        (
            HKEY_CURRENT_USER,
            "Software\\Valve\\Steam",
            KEY_READ,
            &["SteamPath", "InstallPath"][..],
        ),
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Valve\\Steam",
            KEY_READ | KEY_WOW64_32KEY,
            &["InstallPath"][..],
        ),
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Valve\\Steam",
            KEY_READ | KEY_WOW64_64KEY,
            &["InstallPath"][..],
        ),
    ];

    for (hive, subkey, flags, value_names) in registry_locations {
        let key = match RegKey::predef(hive).open_subkey_with_flags(subkey, flags) {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                meaningful_error.get_or_insert_with(|| DetectionError::SteamUnreadable {
                    path: PathBuf::from(format!("registry:{subkey}")),
                    details: error.to_string(),
                });
                continue;
            }
        };
        for value_name in value_names {
            match key.get_value::<String, _>(value_name) {
                Ok(value) if !value.trim().is_empty() => {
                    roots.push(PathBuf::from(value));
                    break;
                }
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    meaningful_error.get_or_insert_with(|| DetectionError::SteamUnreadable {
                        path: PathBuf::from(format!("registry:{subkey}\\{value_name}")),
                        details: error.to_string(),
                    });
                }
            }
        }
    }

    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
            roots.push(PathBuf::from(value).join("Steam"));
        }
    }

    if roots.is_empty()
        && let Some(error) = meaningful_error
    {
        return Err(error);
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::{Duration, UNIX_EPOCH};
    use tempfile::TempDir;

    /// Sets a file's modified time. The file must be opened for writing:
    /// Windows' `SetFileTime` needs `FILE_WRITE_ATTRIBUTES`, which a read-only
    /// `File::open` handle does not carry, so it fails there with "Access is
    /// denied".
    fn set_modified(path: &Path, time: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .expect("open log for mtime update")
            .set_modified(time)
            .expect("set log mtime");
    }

    struct TestSteam {
        _temp: TempDir,
        root: PathBuf,
    }

    impl TestSteam {
        fn new(libraries: usize) -> Self {
            let temp = tempfile::tempdir().expect("create temporary Steam tree");
            let root = temp.path().join("Steam");
            fs::create_dir_all(root.join("steamapps")).expect("create Steam root");
            let library_paths: Vec<_> = (0..libraries)
                .map(|index| {
                    if index == 0 {
                        root.clone()
                    } else {
                        temp.path().join(format!("library-{index}"))
                    }
                })
                .collect();
            for library in &library_paths {
                fs::create_dir_all(library.join("steamapps")).expect("create library");
            }
            write_library_folders(&root, &library_paths);
            Self { _temp: temp, root }
        }

        fn library(&self, index: usize) -> PathBuf {
            if index == 0 {
                self.root.clone()
            } else {
                self._temp.path().join(format!("library-{index}"))
            }
        }

        fn install(&self, library: usize, name: &str, log: bool) -> PathBuf {
            let library = self.library(library);
            write_manifest(&library, name);
            let install = library.join("steamapps").join("common").join(name);
            fs::create_dir_all(install.join(DEADLOCK_GAME_RELATIVE_PATH))
                .expect("create Deadlock install");
            let log_path = install.join(DEADLOCK_LOG_RELATIVE_PATH);
            if log {
                File::create(&log_path).expect("create console log");
            }
            log_path
        }
    }

    fn write_library_folders(root: &Path, libraries: &[PathBuf]) {
        let entries = libraries
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let escaped_path = path
                    .to_string_lossy()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"");
                format!("\"{index}\" {{ \"path\" \"{escaped_path}\" \"apps\" {{}} }}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            root.join("steamapps").join("libraryfolders.vdf"),
            format!("\"libraryfolders\" {{\n{entries}\n}}"),
        )
        .expect("write library metadata");
    }

    fn write_manifest(library: &Path, install_dir: &str) {
        fs::write(
            library
                .join("steamapps")
                .join(format!("appmanifest_{DEADLOCK_APP_ID}.acf")),
            format!(
                "\"AppState\" {{ \"appid\" \"{DEADLOCK_APP_ID}\" \"installdir\" \"{install_dir}\" \"name\" \"Deadlock\" }}"
            ),
        )
        .expect("write app manifest");
    }

    #[test]
    fn resolves_primary_library_log() {
        let steam = TestSteam::new(1);
        let expected = steam.install(0, "Deadlock", true);

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Ok(Detection::Ready { path: expected })
        );
    }

    #[test]
    fn resolves_secondary_library_log() {
        let steam = TestSteam::new(2);
        let expected = steam.install(1, "Deadlock", true);

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Ok(Detection::Ready { path: expected })
        );
    }

    #[test]
    fn installed_game_without_log_is_not_created() {
        let steam = TestSteam::new(1);
        let expected = steam.install(0, "Deadlock", false);

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Ok(Detection::NotCreated { path: expected })
        );
    }

    #[test]
    fn stale_manifest_is_an_invalid_install() {
        let steam = TestSteam::new(1);
        write_manifest(&steam.root, "DeletedDeadlock");
        let expected = steam
            .root
            .join("steamapps")
            .join("common")
            .join("DeletedDeadlock");

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Err(DetectionError::InvalidInstall {
                paths: vec![expected]
            })
        );
    }

    #[test]
    fn malformed_app_manifest_is_reported() {
        let steam = TestSteam::new(1);
        let manifest = steam
            .root
            .join("steamapps")
            .join(format!("appmanifest_{DEADLOCK_APP_ID}.acf"));
        fs::write(&manifest, "not vdf").expect("write malformed manifest");

        assert!(matches!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Err(DetectionError::MalformedMetadata { path, .. }) if path == manifest
        ));
    }
    #[test]
    fn broken_root_does_not_hide_valid_root() {
        let steam = TestSteam::new(1);
        let expected = steam.install(0, "Deadlock", true);
        let broken_root = steam._temp.path().join("not-a-steam-directory");
        File::create(&broken_root).expect("create broken root");

        assert_eq!(
            detect_in_roots(&[broken_root, steam.root]),
            Ok(Detection::Ready { path: expected })
        );
    }

    #[test]
    fn broken_library_does_not_hide_valid_library() {
        let steam = TestSteam::new(3);
        let broken_library = steam.library(1);
        fs::remove_dir_all(&broken_library).expect("remove broken library");
        File::create(&broken_library).expect("create broken library");
        let expected = steam.install(2, "Deadlock", true);

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Ok(Detection::Ready { path: expected })
        );
    }

    #[test]
    fn malformed_metadata_precedes_other_accumulated_errors() {
        let steam = TestSteam::new(2);
        let malformed_manifest = steam
            .root
            .join("steamapps")
            .join(format!("appmanifest_{DEADLOCK_APP_ID}.acf"));
        fs::write(&malformed_manifest, "not vdf").expect("write malformed manifest");
        let broken_library = steam.library(1);
        fs::remove_dir_all(&broken_library).expect("remove broken library");
        File::create(&broken_library).expect("create broken library");

        assert!(matches!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Err(DetectionError::MalformedMetadata { path, .. }) if path == malformed_manifest
        ));
    }

    #[test]
    fn existing_log_beats_an_install_without_a_log() {
        let steam = TestSteam::new(2);
        steam.install(0, "DeadlockPrimary", false);
        let expected = steam.install(1, "DeadlockSecondary", true);

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Ok(Detection::Ready { path: expected })
        );
    }

    #[test]
    fn newest_existing_log_wins_deterministically() {
        let steam = TestSteam::new(2);
        let older = steam.install(0, "DeadlockPrimary", true);
        let newer = steam.install(1, "DeadlockSecondary", true);
        set_modified(&older, UNIX_EPOCH + Duration::from_secs(10));
        set_modified(&newer, UNIX_EPOCH + Duration::from_secs(20));

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Ok(Detection::Ready { path: newer })
        );
    }

    #[test]
    fn tied_existing_logs_are_ambiguous_and_sorted() {
        let steam = TestSteam::new(2);
        let first = steam.install(0, "DeadlockPrimary", true);
        let second = steam.install(1, "DeadlockSecondary", true);
        let timestamp = UNIX_EPOCH + Duration::from_secs(10);
        set_modified(&first, timestamp);
        set_modified(&second, timestamp);
        let mut expected = vec![first, second];
        expected.sort();

        assert_eq!(
            detect_in_roots(std::slice::from_ref(&steam.root)),
            Err(DetectionError::AmbiguousInstalls { paths: expected })
        );
    }
}
