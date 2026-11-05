//! Updater, part 2: download this platform's package, verify it against
//! the release's `SHA256SUMS.txt`, and swap it in place of the running app.
//!
//! No helper process: a running exe (Windows) or `.app` (macOS) can be
//! renamed, so the app moves itself aside to `<name>.old` and puts the new
//! one under its own name. It keeps running from the moved file; the next
//! start is the new version and removes `.old`. Everything lives next to
//! the app, so a folder we can't write to (Program Files, a dmg, a
//! translocated `.app`) means "download it by hand" — never a half swap.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::update::{self, Release};

/// Why an install didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// Worth trying again at the next check (network, a bad transfer).
    Retry(String),
    /// Will never work from here: offer the release page instead.
    Manual(String),
}

/// What the updater replaces: the exe on Windows, the `.app` on macOS.
/// `None` when not running from an installable build (`cargo run`).
pub fn install_target() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(windows) {
        return Some(exe);
    }
    if cfg!(target_os = "macos") {
        // …/DroidRescue.app/Contents/MacOS/DroidRescue
        return exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(Path::to_path_buf);
    }
    None
}

/// `<target>` with `suffix` appended to its file name, in the same folder.
fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    target.with_file_name(name)
}

/// Downloads, verifies and swaps in `release`'s package for this platform.
/// `progress(done, total)` is called as bytes arrive.
pub fn install(release: &Release, progress: &mut dyn FnMut(u64, u64)) -> Result<(), InstallError> {
    let target = install_target()
        .ok_or_else(|| InstallError::Manual("当前运行方式不支持自动更新".into()))?;
    install_to(&target, release, progress)
}

/// [`install`] into an explicit `target` (the exe, or the `.app` folder).
fn install_to(
    target: &Path,
    release: &Release,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), InstallError> {
    let manual = |why: &str| InstallError::Manual(why.into());
    let package = release
        .package
        .as_ref()
        .ok_or_else(|| manual("这个版本没有本平台的更新包"))?;
    let sums = release
        .sums
        .as_ref()
        .ok_or_else(|| manual("这个版本缺少校验文件"))?;
    let sums = update::fetch_text(&sums.url)
        .map_err(InstallError::Retry)?
        .ok_or_else(|| InstallError::Retry("校验文件暂时下载不到".into()))?;
    let expected = update::sum_for(&sums, &package.name)
        .ok_or_else(|| manual("校验文件里没有本平台的更新包"))?;

    let response = update::agent()
        .get(&package.url)
        .call()
        .map_err(|e| InstallError::Retry(update::network_error(&e)))?;
    let total = response.body().content_length().unwrap_or(package.size);
    let mut body = response.into_body();
    let file = download(target, &mut body.as_reader(), total, &expected, progress)?;
    apply(target, &file)
}

/// Streams `reader` to `<target>.download`, hashing on the way. Returns
/// the file when its SHA-256 is `expected`; otherwise nothing is left.
fn download(
    target: &Path,
    reader: &mut dyn Read,
    total: u64,
    expected: &str,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, InstallError> {
    let path = sibling(target, ".download");
    let mut file = File::create(&path).map_err(|e| {
        InstallError::Manual(format!("程序所在的文件夹不能写入（{e}），请手动下载新版本"))
    })?;
    let result = copy_hashing(reader, &mut file, total, progress);
    drop(file);
    let fail = |why: String| {
        let _ = fs::remove_file(&path);
        Err(InstallError::Retry(why))
    };
    match result {
        Err(e) => fail(format!("下载中断：{e}")),
        Ok((_, hash)) if hash != expected => fail("下载的文件校验不符，已丢弃".into()),
        Ok(_) => Ok(path),
    }
}

/// Copies `reader` into `writer`; returns the byte count and its SHA-256.
fn copy_hashing(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total.max(done));
    }
    writer.flush()?;
    let hash = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((done, hash))
}

/// Puts the verified download in place of `target`.
fn apply(target: &Path, download: &Path) -> Result<(), InstallError> {
    if cfg!(target_os = "macos") {
        return apply_app_zip(target, download);
    }
    swap_into(target, download)
}

/// macOS: unzip the `.app` next to the running one, check its signature,
/// swap it in.
fn apply_app_zip(target: &Path, zip: &Path) -> Result<(), InstallError> {
    let dir = sibling(target, ".update");
    let _ = remove_any(&dir);
    let retry = |why: String| {
        let _ = remove_any(&dir);
        let _ = fs::remove_file(zip);
        Err(InstallError::Retry(why))
    };
    let unzip = std::process::Command::new("ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(&dir)
        .status();
    if !matches!(unzip, Ok(s) if s.success()) {
        return retry("更新包解压失败".into());
    }
    let app = fs::read_dir(&dir).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
    });
    let Some(app) = app else {
        return retry("更新包里没有应用".into());
    };
    let signed = std::process::Command::new("codesign")
        .args(["--verify", "--strict"])
        .arg(&app)
        .status();
    if !matches!(signed, Ok(s) if s.success()) {
        return retry("更新包签名校验失败".into());
    }
    let result = swap_into(target, &app);
    let _ = remove_any(&dir);
    let _ = fs::remove_file(zip);
    result
}

/// `target` → `<target>.old`, `new` → `target`. If the second rename
/// fails the first is undone, so the app is never left missing.
fn swap_into(target: &Path, new: &Path) -> Result<(), InstallError> {
    let old = sibling(target, ".old");
    if let Err(e) = remove_any(&old) {
        // A previous version still running from `.old` holds it open.
        return Err(InstallError::Retry(format!("旧版本文件还在使用中：{e}")));
    }
    fs::rename(target, &old)
        .map_err(|e| InstallError::Manual(format!("无法替换程序（{e}），请手动下载新版本")))?;
    if let Err(e) = fs::rename(new, target) {
        let _ = fs::rename(&old, target);
        return Err(InstallError::Manual(format!(
            "无法替换程序（{e}），请手动下载新版本"
        )));
    }
    Ok(())
}

/// Removes a file or a folder; missing is fine.
fn remove_any(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// On start: delete what an earlier update left beside `target` (the
/// previous version, a partial download). The previous process may still
/// be closing, so a locked `.old` is retried a few times.
pub fn cleanup(target: &Path) {
    for suffix in [".download", ".update"] {
        let _ = remove_any(&sibling(target, suffix));
    }
    let old = sibling(target, ".old");
    for _ in 0..20 {
        if remove_any(&old).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty folder for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("droidrescue-swap-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn copying_hashes_and_reports_progress() {
        let mut out = Vec::new();
        let mut seen = Vec::new();
        let (n, hash) =
            copy_hashing(&mut &b"abc"[..], &mut out, 3, &mut |d, t| seen.push((d, t))).unwrap();
        assert_eq!(
            (n, hash.as_str(), out.as_slice()),
            (3, ABC_SHA256, &b"abc"[..])
        );
        assert_eq!(seen.last(), Some(&(3, 3)));
    }

    #[test]
    fn a_verified_download_is_kept() {
        let dir = scratch("ok");
        let target = dir.join("DroidRescue.exe");
        let file = download(&target, &mut &b"abc"[..], 3, ABC_SHA256, &mut |_, _| {}).unwrap();
        assert_eq!(file, dir.join("DroidRescue.exe.download"));
        assert_eq!(fs::read(&file).unwrap(), b"abc");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_that_fails_its_checksum_is_dropped_and_the_app_untouched() {
        let dir = scratch("bad");
        let target = dir.join("DroidRescue.exe");
        fs::write(&target, b"current").unwrap();
        let err = download(&target, &mut &b"abd"[..], 3, ABC_SHA256, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, InstallError::Retry(_)));
        assert!(!dir.join("DroidRescue.exe.download").exists());
        assert_eq!(fs::read(&target).unwrap(), b"current");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_we_cannot_write_means_download_by_hand() {
        let dir = scratch("nowrite");
        let target = dir.join("missing-folder").join("DroidRescue.exe");
        let err = download(&target, &mut &b"abc"[..], 3, ABC_SHA256, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, InstallError::Manual(_)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn swapping_moves_the_app_aside_and_puts_the_new_one_in_place() {
        let dir = scratch("swap");
        let target = dir.join("DroidRescue.exe");
        fs::write(&target, b"v1").unwrap();
        fs::write(dir.join("new1"), b"v2").unwrap();
        swap_into(&target, &dir.join("new1")).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"v2");
        assert_eq!(fs::read(dir.join("DroidRescue.exe.old")).unwrap(), b"v1");
        assert!(!dir.join("new1").exists());

        // Updating again before a restart replaces the earlier `.old`.
        fs::write(dir.join("new2"), b"v3").unwrap();
        swap_into(&target, &dir.join("new2")).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"v3");
        assert_eq!(fs::read(dir.join("DroidRescue.exe.old")).unwrap(), b"v2");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn swapping_works_for_app_bundles_too() {
        let dir = scratch("bundle");
        let target = dir.join("DroidRescue.app");
        fs::create_dir_all(target.join("Contents")).unwrap();
        fs::write(target.join("Contents/v"), b"v1").unwrap();
        let new = dir.join("DroidRescue.app.update").join("DroidRescue.app");
        fs::create_dir_all(new.join("Contents")).unwrap();
        fs::write(new.join("Contents/v"), b"v2").unwrap();
        swap_into(&target, &new).unwrap();
        assert_eq!(fs::read(target.join("Contents/v")).unwrap(), b"v2");
        assert_eq!(
            fs::read(dir.join("DroidRescue.app.old/Contents/v")).unwrap(),
            b"v1"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_swap_leaves_the_app_where_it_was() {
        let dir = scratch("undo");
        let target = dir.join("DroidRescue.exe");
        fs::write(&target, b"v1").unwrap();
        let err = swap_into(&target, &dir.join("does-not-exist")).unwrap_err();
        assert!(matches!(err, InstallError::Manual(_)));
        assert_eq!(fs::read(&target).unwrap(), b"v1");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cleanup_removes_leftovers_and_keeps_the_app() {
        let dir = scratch("cleanup");
        let target = dir.join("DroidRescue.exe");
        fs::write(&target, b"v2").unwrap();
        fs::write(dir.join("DroidRescue.exe.old"), b"v1").unwrap();
        fs::write(dir.join("DroidRescue.exe.download"), b"part").unwrap();
        fs::create_dir_all(dir.join("DroidRescue.exe.update/x")).unwrap();
        cleanup(&target);
        let left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["DroidRescue.exe"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The whole install against the real feed, into a stand-in app in a
    /// scratch folder. Pretend to be old so the latest release is offered:
    /// `DROIDRESCUE_FAKE_VERSION=0.0.1 cargo test real_install -- --ignored --nocapture`
    /// (needs a published release).
    #[test]
    #[ignore]
    fn real_install_from_the_latest_release() {
        let dir = scratch("real");
        let target = if cfg!(target_os = "macos") {
            let app = dir.join("DroidRescue.app");
            fs::create_dir_all(&app).unwrap();
            app
        } else {
            let exe = dir.join("DroidRescue.exe");
            fs::write(&exe, b"stand-in").unwrap();
            exe
        };
        match update::check() {
            Ok(update::Check::Available(release)) => {
                let mut last = 0;
                let result = install_to(&target, &release, &mut |done, total| {
                    let pct = done * 100 / total.max(1);
                    if pct >= last + 10 {
                        last = pct;
                        println!("{pct}% of {total}");
                    }
                });
                println!("{} → {result:?} in {}", release.version, dir.display());
            }
            other => println!("nothing to install: {other:?}"),
        }
    }
}
