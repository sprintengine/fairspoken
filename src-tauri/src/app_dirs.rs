//! Where Fairspoken keeps its files, how its environment variables are read,
//! and the one-time move of data written by builds that used the former name.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Bundle identifier in tauri.conf.json; Tauri keys webview storage by it.
const IDENTIFIER: &str = "ie.fairspoken.desktop";
const LEGACY_IDENTIFIER: &str = "com.multicodelabs.multivoice.tauri";

/// Reads a `FAIRSPOKEN_*` variable. Builds before the rename used the
/// `MULTIVOICE_*` prefix (`MULTIVOICE_TAURI_*` for the desktop path
/// overrides); that name is still honoured when the new one is unset.
pub(crate) fn env_var_os(name: &str) -> Option<OsString> {
    env::var_os(name).or_else(|| legacy_env_name(name).and_then(env::var_os))
}

pub(crate) fn env_var(name: &str) -> Option<String> {
    env_var_os(name).and_then(|value| value.into_string().ok())
}

fn legacy_env_name(name: &str) -> Option<String> {
    let suffix = name.strip_prefix("FAIRSPOKEN_")?;
    let prefix = match suffix {
        "SETTINGS_PATH" | "NOTES_PATH" | "HISTORY_PATH" | "SPEED_TEST_PATH"
        | "USAGE_STATS_PATH" | "MODEL_DIR" => "MULTIVOICE_TAURI_",
        _ => "MULTIVOICE_",
    };
    Some(format!("{prefix}{suffix}"))
}

/// Settings and the small JSON stores (notes, history, stats, host config).
pub(crate) fn config_dir() -> Option<PathBuf> {
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return Some(PathBuf::from(local_app_data).join("Fairspoken"));
    }
    env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("fairspoken"))
}

/// Downloaded speech and polish models.
pub(crate) fn data_dir() -> Option<PathBuf> {
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return Some(PathBuf::from(local_app_data).join("Fairspoken"));
    }
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("fairspoken")
    })
}

/// Replaces `path` with `bytes` so a crash or full disk leaves either the old
/// file or the new one, never a truncated mix: the bytes go to a uniquely
/// named file in the same directory, are flushed, and are renamed over the
/// target. The parent directory is created when missing. On unix the file is
/// readable only by the user, since several stores hold tokens or dictations.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let write = || -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    };
    let result = write();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Reads a JSON store. A missing file is `None`. A file that exists but cannot
/// be read or parsed is renamed to `<name>.corrupt-<unix millis>` and is also
/// `None`: the caller starts empty, and its next save cannot overwrite data
/// that may still be recoverable by hand.
pub(crate) fn read_json_or_back_up<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let failure = match fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(value) => return Some(value),
            Err(err) => err.to_string(),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => err.to_string(),
    };
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt-{millis}"));
    let backup = path.with_file_name(name);
    match fs::rename(path, &backup) {
        Ok(()) => eprintln!(
            "Could not read {} ({failure}); moved it to {} and started empty",
            path.display(),
            backup.display()
        ),
        Err(err) => eprintln!(
            "Could not read {} ({failure}) or move it aside ({err}); started empty",
            path.display()
        ),
    }
    None
}

/// Each directory earlier builds wrote, paired with where that data lives now:
/// the app's own stores and models, plus the webview storage (localStorage)
/// Tauri keys by bundle identifier.
fn legacy_locations(local_app_data: Option<&Path>, home: Option<&Path>) -> Vec<(PathBuf, PathBuf)> {
    if let Some(local) = local_app_data {
        return vec![
            (local.join("Multivoice Tauri"), local.join("Fairspoken")),
            (local.join(LEGACY_IDENTIFIER), local.join(IDENTIFIER)),
        ];
    }
    let Some(home) = home else {
        return Vec::new();
    };
    let config = home.join(".config");
    let share = home.join(".local").join("share");
    let webview = if cfg!(target_os = "macos") {
        home.join("Library").join("WebKit")
    } else {
        share.clone()
    };
    vec![
        (config.join("multivoice-tauri"), config.join("fairspoken")),
        (share.join("multivoice-tauri"), share.join("fairspoken")),
        (webview.join(LEGACY_IDENTIFIER), webview.join(IDENTIFIER)),
    ]
}

#[derive(Debug, PartialEq, Eq)]
enum MigrationPlan {
    /// Nothing was ever written at the old location.
    NothingToMove,
    /// The new location already holds data; the old copy is left untouched.
    AlreadyMigrated,
    Move,
}

fn plan_migration(old: &Path, new: &Path) -> MigrationPlan {
    if !old.is_dir() {
        return MigrationPlan::NothingToMove;
    }
    let new_is_empty = match fs::read_dir(new) {
        Ok(mut entries) => entries.next().is_none(),
        Err(err) => err.kind() == io::ErrorKind::NotFound,
    };
    if new_is_empty {
        MigrationPlan::Move
    } else {
        MigrationPlan::AlreadyMigrated
    }
}

#[derive(Debug, PartialEq, Eq)]
enum MigrationOutcome {
    Moved,
    /// A rename was impossible (e.g. across volumes); the old data remains.
    Copied,
}

fn migrate(old: &Path, new: &Path) -> io::Result<MigrationOutcome> {
    if new.is_dir() {
        // Only ever an empty directory here (see plan_migration).
        fs::remove_dir(new)?;
    }
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(old, new).is_ok() {
        return Ok(MigrationOutcome::Moved);
    }
    copy_into_place(old, new)?;
    Ok(MigrationOutcome::Copied)
}

/// Copies `old` to a sibling staging directory and renames it into place, so
/// `new` only ever appears complete. A copy that fails part way leaves `new`
/// absent, and the next launch plans the migration again.
fn copy_into_place(old: &Path, new: &Path) -> io::Result<()> {
    let mut staging_name = new.file_name().unwrap_or_default().to_os_string();
    staging_name.push(".migrating");
    let staging = new.with_file_name(staging_name);
    if staging.exists() {
        // Left by an interrupted earlier attempt.
        fs::remove_dir_all(&staging)?;
    }
    let result = copy_dir_all(old, &staging).and_then(|()| fs::rename(&staging, new));
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn copy_dir_all(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Moves data from the pre-rename locations on first launch. Runs before any
/// service opens its files; failures are logged and the app starts fresh.
pub(crate) fn migrate_legacy_dirs() {
    let local_app_data = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let home = env::var_os("HOME").map(PathBuf::from);
    for (old, new) in legacy_locations(local_app_data.as_deref(), home.as_deref()) {
        if plan_migration(&old, &new) != MigrationPlan::Move {
            continue;
        }
        match migrate(&old, &new) {
            Ok(MigrationOutcome::Moved) => {
                eprintln!("Moved data from {} to {}", old.display(), new.display())
            }
            Ok(MigrationOutcome::Copied) => eprintln!(
                "Copied data from {} to {}; the old directory was left in place",
                old.display(),
                new.display()
            ),
            Err(err) => eprintln!(
                "Could not move data from {} to {}: {err}",
                old.display(),
                new.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = env::temp_dir().join(format!("fairspoken-app-dirs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn legacy_env_names_keep_the_former_prefixes() {
        assert_eq!(
            legacy_env_name("FAIRSPOKEN_HOST_TOKEN").as_deref(),
            Some("MULTIVOICE_HOST_TOKEN")
        );
        assert_eq!(
            legacy_env_name("FAIRSPOKEN_CLOUD_URL").as_deref(),
            Some("MULTIVOICE_CLOUD_URL")
        );
        assert_eq!(
            legacy_env_name("FAIRSPOKEN_SETTINGS_PATH").as_deref(),
            Some("MULTIVOICE_TAURI_SETTINGS_PATH")
        );
        assert_eq!(
            legacy_env_name("FAIRSPOKEN_MODEL_DIR").as_deref(),
            Some("MULTIVOICE_TAURI_MODEL_DIR")
        );
        assert_eq!(legacy_env_name("PATH"), None);
    }

    #[test]
    fn legacy_locations_pair_old_and_new_directories() {
        let windows = legacy_locations(Some(Path::new("C:/Local")), Some(Path::new("C:/Users/a")));
        assert_eq!(
            windows,
            vec![
                (
                    PathBuf::from("C:/Local/Multivoice Tauri"),
                    PathBuf::from("C:/Local/Fairspoken")
                ),
                (
                    PathBuf::from("C:/Local/com.multicodelabs.multivoice.tauri"),
                    PathBuf::from("C:/Local/ie.fairspoken.desktop")
                ),
            ]
        );
        let unix = legacy_locations(None, Some(Path::new("/home/a")));
        assert_eq!(
            unix[0],
            (
                PathBuf::from("/home/a/.config/multivoice-tauri"),
                PathBuf::from("/home/a/.config/fairspoken")
            )
        );
        assert_eq!(
            unix[1],
            (
                PathBuf::from("/home/a/.local/share/multivoice-tauri"),
                PathBuf::from("/home/a/.local/share/fairspoken")
            )
        );
        assert!(unix[2].0.ends_with(LEGACY_IDENTIFIER) && unix[2].1.ends_with(IDENTIFIER));
        assert!(legacy_locations(None, None).is_empty());
    }

    #[test]
    fn plan_moves_only_when_old_has_data_and_new_does_not() {
        let root = scratch();
        let (old, new) = (root.join("old"), root.join("new"));
        assert_eq!(plan_migration(&old, &new), MigrationPlan::NothingToMove);

        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("settings.json"), "{}").unwrap();
        assert_eq!(plan_migration(&old, &new), MigrationPlan::Move);

        fs::create_dir_all(&new).unwrap();
        assert_eq!(
            plan_migration(&old, &new),
            MigrationPlan::Move,
            "an empty new dir is not data"
        );

        fs::write(new.join("settings.json"), "{}").unwrap();
        assert_eq!(plan_migration(&old, &new), MigrationPlan::AlreadyMigrated);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migrate_moves_nested_data_and_is_idempotent() {
        let root = scratch();
        let (old, new) = (root.join("old"), root.join("nested").join("new"));
        fs::create_dir_all(old.join("models").join("parakeet")).unwrap();
        fs::write(old.join("notes.json"), "[1]").unwrap();
        fs::write(
            old.join("models").join("parakeet").join("model.onnx"),
            "weights",
        )
        .unwrap();

        assert_eq!(migrate(&old, &new).unwrap(), MigrationOutcome::Moved);
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(new.join("notes.json")).unwrap(), "[1]");
        assert_eq!(
            fs::read_to_string(new.join("models/parakeet/model.onnx")).unwrap(),
            "weights"
        );
        assert_eq!(plan_migration(&old, &new), MigrationPlan::NothingToMove);

        let copy = root.join("copy");
        copy_dir_all(&new, &copy).unwrap();
        assert_eq!(
            fs::read_to_string(copy.join("models/parakeet/model.onnx")).unwrap(),
            "weights"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copied_migration_appears_whole_or_not_at_all() {
        let root = scratch();
        let (old, new) = (root.join("old"), root.join("new"));
        let staging = root.join("new.migrating");

        // A failed copy (here: nothing to read) leaves neither the target nor
        // the staging directory, so the next launch still plans a move.
        assert!(copy_into_place(&old, &new).is_err());
        assert!(!new.exists() && !staging.exists());

        fs::create_dir_all(old.join("models")).unwrap();
        fs::write(old.join("models").join("model.onnx"), "weights").unwrap();
        // Debris from an interrupted attempt is not merged into the result.
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("stale.json"), "{}").unwrap();
        copy_into_place(&old, &new).unwrap();
        assert_eq!(fs::read_to_string(new.join("models/model.onnx")).unwrap(), "weights");
        assert!(!new.join("stale.json").exists());
        assert!(!staging.exists());
        assert!(old.join("models/model.onnx").exists(), "a copy keeps the old data");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unreadable_stores_are_moved_aside_and_missing_ones_are_not() {
        let root = scratch();
        let path = root.join("notes.json");
        assert_eq!(read_json_or_back_up::<Vec<u32>>(&path), None);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);

        fs::write(&path, "[1, 2]").unwrap();
        assert_eq!(read_json_or_back_up::<Vec<u32>>(&path), Some(vec![1, 2]));

        fs::write(&path, "[1, 2").unwrap();
        assert_eq!(read_json_or_back_up::<Vec<u32>>(&path), None);
        assert!(!path.exists());
        let backup = fs::read_dir(&root).unwrap().next().unwrap().unwrap();
        assert!(backup.file_name().to_string_lossy().starts_with("notes.json.corrupt-"));
        assert_eq!(fs::read_to_string(backup.path()).unwrap(), "[1, 2");

        // Bytes that are not text count as unreadable too.
        fs::write(&path, [0xff, 0xfe]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(read_json_or_back_up::<Vec<u32>>(&path), None);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_replaces_the_file_privately_and_leaves_no_temporaries() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let path = root.join("nested").join("store.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);

        // A target that cannot be replaced fails without leaving debris.
        let blocked = root.join("blocked");
        fs::create_dir_all(blocked.join("inner")).unwrap();
        assert!(write_atomic(&blocked, b"x").is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }
}
