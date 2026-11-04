//! Embeds the runtime pieces from vendor/ (adb, scrcpy-server, and FFmpeg on
//! Windows) into the executable. Generates `$OUT_DIR/bundle.rs` with
//! `BUNDLE_ID`, `BUNDLE_FILES` and `SERVER_VERSION`.
//!
//! Fetched by `scripts/fetch-vendor.ps1` (Windows → vendor/scrcpy) or
//! `scripts/fetch-vendor.sh` (macOS → vendor/scrcpy-macos).

use std::fmt::Write as _;
use std::path::PathBuf;

/// Only what the native client needs (no scrcpy.exe / SDL).
const WINDOWS_FILES: &[&str] = &[
    "adb.exe",
    "AdbWinApi.dll",
    "AdbWinUsbApi.dll",
    "scrcpy-server",
    "avcodec-63.dll",
    "avutil-61.dll",
    "swresample-7.dll",
    "LICENSE.txt",
];
/// macOS decodes with VideoToolbox: no FFmpeg.
const MACOS_FILES: &[&str] = &["adb", "scrcpy-server", "LICENSE.txt"];

fn main() {
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let (vendor, files) = if macos {
        ("scrcpy-macos", MACOS_FILES)
    } else {
        ("scrcpy", WINDOWS_FILES)
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join(vendor);
    println!("cargo:rerun-if-changed={}", dir.display());

    let entries: Vec<PathBuf> = files.iter().map(|f| dir.join(f)).collect();
    for p in &entries {
        assert!(p.is_file(), "missing bundle file {}", p.display());
    }
    let version_file = dir.join("VERSION.txt");
    println!("cargo:rerun-if-changed={}", version_file.display());
    let version = std::fs::read_to_string(&version_file)
        .expect("vendor/scrcpy/VERSION.txt")
        .trim()
        .trim_start_matches('v')
        .to_string();

    // FNV-1a over names + contents: changes whenever the bundle changes,
    // so a new build extracts into a fresh directory.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };

    let mut files = String::new();
    for path in &entries {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        feed(name.as_bytes());
        feed(&std::fs::read(path).unwrap());
        writeln!(
            files,
            "    ({name:?}, include_bytes!({:?})),",
            path.display().to_string()
        )
        .unwrap();
    }

    let code = format!(
        "pub const BUNDLE_ID: &str = \"{hash:016x}\";\npub const SERVER_VERSION: &str = {version:?};\npub const BUNDLE_FILES: &[(&str, &[u8])] = &[\n{files}];\n"
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("bundle.rs");
    std::fs::write(out, code).unwrap();
}
