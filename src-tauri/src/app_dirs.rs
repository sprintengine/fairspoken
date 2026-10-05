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
    copy_dir_all(old, new)?;
    Ok(MigrationOutcome::Copied)
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
}
