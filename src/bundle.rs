//! scrcpy + adb embedded at build time (see build.rs), extracted on first run.

use std::io;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/bundle.rs"));

const COMPLETE_MARKER: &str = ".complete";

/// Per-user data directory name (`%LOCALAPPDATA%\DroidRescue`).
pub const APP_DIR: &str = "DroidRescue";
/// The directory used before the rename to DroidRescue.
const LEGACY_DIR: &str = "padctl";
/// User data carried over from the legacy directory (not logs or bundles).
const MIGRATED: &[&str] = &[
    "passwords.dat",
    "known-devices.txt",
    "rescue-profiles.txt",
    "window.ron",
];

/// Per-user application data root: `%LOCALAPPDATA%` on Windows,
/// `~/Library/Application Support` on macOS (falls back to the temp dir).
fn local_app_data() -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    };
    dir.unwrap_or_else(std::env::temp_dir)
}

/// `%LOCALAPPDATA%\DroidRescue` / `~/Library/Application Support/DroidRescue`.
pub fn default_base() -> PathBuf {
    local_app_data().join(APP_DIR)
}

/// One-time move of user data from `%LOCALAPPDATA%\padctl` (saved PINs,
/// profiles, window position). Runs only while the new directory does not
/// exist yet; the legacy directory is left in place. Returns files copied.
pub fn migrate_legacy_data() -> io::Result<usize> {
    let root = local_app_data();
    migrate(&root.join(LEGACY_DIR), &root.join(APP_DIR))
}

fn migrate(old: &Path, new: &Path) -> io::Result<usize> {
    if new.exists() || !old.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(new)?;
    let mut copied = 0;
    for name in MIGRATED {
        let from = old.join(name);
        if from.is_file() {
            std::fs::copy(&from, new.join(name))?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Extract the bundle under `base` (once per bundle version) and return its directory.
pub fn extract_to(base: &Path) -> io::Result<PathBuf> {
    extract_files(base, BUNDLE_ID, BUNDLE_FILES)
}

fn extract_files(base: &Path, id: &str, files: &[(&str, &[u8])]) -> io::Result<PathBuf> {
    let dir = base.join(format!("scrcpy-{id}"));
    if dir.join(COMPLETE_MARKER).is_file() {
        return Ok(dir);
    }

    // Write into a private temp dir, then rename into place, so a crash or a
    // second instance never sees a half-written bundle.
    let tmp = base.join(format!("scrcpy-{id}.tmp-{}", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;
    for (name, bytes) in files {
        let path = tmp.join(name);
        std::fs::write(&path, bytes)?;
        // adb must be executable (include_bytes! keeps no file modes).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    std::fs::write(tmp.join(COMPLETE_MARKER), id)?;

    if dir.exists() && !dir.join(COMPLETE_MARKER).is_file() {
        std::fs::remove_dir_all(&dir)?; // leftover from an old interrupted extract
    }
    match std::fs::rename(&tmp, &dir) {
        Ok(()) => Ok(dir),
        // Another instance finished first: use theirs.
        Err(_) if dir.join(COMPLETE_MARKER).is_file() => {
            let _ = std::fs::remove_dir_all(&tmp);
            Ok(dir)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(tag: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("droidrescue-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        base
    }

    #[test]
    fn migrates_user_data_once_and_skips_logs_and_bundles() {
        let base = temp_base("migrate");
        let (old, new) = (base.join("padctl"), base.join("DroidRescue"));
        std::fs::create_dir_all(old.join("scrcpy-abc")).unwrap();
        std::fs::write(old.join("passwords.dat"), b"pw").unwrap();
        std::fs::write(old.join("window.ron"), b"w").unwrap();
        std::fs::write(old.join("padctl.log"), b"log").unwrap();

        assert_eq!(migrate(&old, &new).unwrap(), 2);
        assert_eq!(std::fs::read(new.join("passwords.dat")).unwrap(), b"pw");
        assert!(!new.join("padctl.log").exists());
        assert!(!new.join("scrcpy-abc").exists());
        assert!(
            old.join("passwords.dat").exists(),
            "legacy data is left in place"
        );

        // Second run: the new dir exists → nothing is overwritten.
        std::fs::write(old.join("passwords.dat"), b"newer").unwrap();
        assert_eq!(migrate(&old, &new).unwrap(), 0);
        assert_eq!(std::fs::read(new.join("passwords.dat")).unwrap(), b"pw");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn nothing_to_migrate_without_a_legacy_dir() {
        let base = temp_base("migrate-none");
        assert_eq!(
            migrate(&base.join("padctl"), &base.join("DroidRescue")).unwrap(),
            0
        );
        assert!(!base.join("DroidRescue").exists());
    }

    #[test]
    fn real_bundle_contains_runtime_pieces_only() {
        let names: Vec<_> = BUNDLE_FILES.iter().map(|(n, _)| *n).collect();
        let needed: &[&str] = if cfg!(windows) {
            &[
                "adb.exe",
                "scrcpy-server",
                "avcodec-63.dll",
                "avutil-61.dll",
            ]
        } else {
            &["adb", "scrcpy-server"]
        };
        for needed in needed {
            assert!(names.contains(needed), "missing {needed}");
        }
        for unwanted in ["scrcpy.exe", "scrcpy", "SDL3.dll"] {
            assert!(!names.contains(&unwanted), "{unwanted} is not needed");
        }
        assert_eq!(SERVER_VERSION, "5.0.1");
    }

    #[test]
    fn extracts_all_files_with_marker() {
        let base = temp_base("extract");
        let dir =
            extract_files(&base, "v1", &[("a.txt", b"hello"), ("b.bin", b"\x00\x01")]).unwrap();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), b"\x00\x01");
        assert!(dir.join(COMPLETE_MARKER).is_file());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn second_extract_reuses_existing_dir() {
        let base = temp_base("reuse");
        let first = extract_files(&base, "v1", &[("a.txt", b"one")]).unwrap();
        let second = extract_files(&base, "v1", &[("a.txt", b"two")]).unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read(second.join("a.txt")).unwrap(), b"one");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn replaces_incomplete_previous_extract() {
        let base = temp_base("incomplete");
        let stale = base.join("scrcpy-v1");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("a.txt"), b"partial").unwrap();
        let dir = extract_files(&base, "v1", &[("a.txt", b"full")]).unwrap();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"full");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
