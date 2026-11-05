//! Updater, part 1: is a newer release published on GitHub, and which of
//! its files is this platform's update package.
//!
//! Only `/releases/latest` is asked: GitHub never returns a draft or a
//! pre-release there, so a draft stays a maintainer smoke test until it is
//! published. Versions are plain `X.Y.Z`; anything with a suffix
//! (`1.0.0-rc1`) is never offered.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;

/// The release feed. `DROIDRESCUE_UPDATE_FEED` (an `https://` URL or a local
/// JSON file) replaces it for tests.
const FEED_URL: &str = "https://api.github.com/repos/HeiSir2014/DroidRescue/releases/latest";
/// The checksum file every release carries (`sha256sum` output).
const SUMS_NAME: &str = "SHA256SUMS.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// `X.Y.Z` with an optional leading `v`. A pre-release or build suffix
    /// is not a version we offer, so it doesn't parse.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || -> Option<u32> {
            let part = parts.next()?;
            // `u32::from_str` would accept "+1"; only digits are a version.
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// The version this build is. `DROIDRESCUE_FAKE_VERSION` pretends to be
    /// older, to try an update against a real release.
    pub fn current() -> Self {
        std::env::var("DROIDRESCUE_FAKE_VERSION")
            .ok()
            .and_then(|v| Version::parse(&v))
            .or_else(|| Version::parse(env!("CARGO_PKG_VERSION")))
            .expect("Cargo.toml version is X.Y.Z")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The platforms CI builds an update package for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    WindowsX64,
    MacArm64,
}

impl Platform {
    pub const CURRENT: Option<Platform> = if cfg!(all(windows, target_arch = "x86_64")) {
        Some(Platform::WindowsX64)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(Platform::MacArm64)
    } else {
        None
    };

    /// The release file the updater installs (CI's package step names it).
    pub fn asset_name(self, version: Version) -> String {
        let v = format!("{}.{}.{}", version.major, version.minor, version.patch);
        match self {
            Platform::WindowsX64 => format!("DroidRescue-{v}-win-x64.exe"),
            Platform::MacArm64 => format!("DroidRescue-{v}-macos-arm64.app.zip"),
        }
    }
}

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release's web page (notes, manual download).
    pub page: String,
    /// This platform's update package. `None`: the release has none for us,
    /// so it can only be downloaded by hand.
    pub package: Option<Asset>,
    /// `SHA256SUMS.txt`; without it nothing is installed.
    pub sums: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    UpToDate,
    Available(Release),
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// What a release JSON means for a build at `current` on `platform`.
pub fn evaluate(current: Version, json: &str, platform: Option<Platform>) -> Result<Check, String> {
    let release: ReleaseJson =
        serde_json::from_str(json).map_err(|e| format!("版本信息格式不对：{e}"))?;
    let Some(version) = Version::parse(&release.tag_name) else {
        return Ok(Check::UpToDate);
    };
    if release.draft || release.prerelease || version <= current {
        return Ok(Check::UpToDate);
    }
    let find = |name: &str| {
        release
            .assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| Asset {
                name: a.name.clone(),
                url: a.browser_download_url.clone(),
                size: a.size,
            })
    };
    Ok(Check::Available(Release {
        version,
        page: release.html_url.clone(),
        package: platform.and_then(|p| find(&p.asset_name(version))),
        sums: find(SUMS_NAME),
    }))
}

/// The SHA-256 (lowercase hex) that `sha256sum` output lists for `name`.
/// Text mode (`hash  name`) and binary mode (`hash *name`) both count.
pub fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, rest) = line.trim_end().split_once(' ')?;
        let file = rest.trim_start_matches([' ', '*']);
        let hex = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        (hex && file == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Asks the feed whether something newer is out for this build.
pub fn check() -> Result<Check, String> {
    let feed = std::env::var("DROIDRESCUE_UPDATE_FEED").unwrap_or_else(|_| FEED_URL.into());
    let Some(json) = fetch_text(&feed)? else {
        // 404: nothing published yet.
        return Ok(Check::UpToDate);
    };
    evaluate(Version::current(), &json, Platform::CURRENT)
}

/// A small text resource: an `http(s)://` URL, or a local file (tests).
/// `Ok(None)` when the server says it doesn't exist.
pub fn fetch_text(source: &str) -> Result<Option<String>, String> {
    if !source.starts_with("https://") && !source.starts_with("http://") {
        return std::fs::read_to_string(source)
            .map(Some)
            .map_err(|e| format!("读取 {source} 失败：{e}"));
    }
    let mut request = agent().get(source);
    if source.starts_with("https://api.github.com/") {
        request = request.header("Accept", "application/vnd.github+json");
    }
    match request.call() {
        Ok(mut r) => r
            .body_mut()
            .read_to_string()
            .map(Some)
            .map_err(|e| format!("读取 {source} 失败：{e}")),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(network_error(&e)),
    }
}

/// The one HTTP client: OS TLS + trust store, generous timeouts for slow
/// links (GitHub from China), our name and version as the user agent.
pub fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // A read that stalls this long is a dead link, not a slow one.
        .timeout_per_read(Some(Duration::from_secs(60)))
        .user_agent(concat!("DroidRescue/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

/// A network failure in words a user can act on.
pub fn network_error(e: &ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(403 | 429) => "GitHub 暂时限制了访问次数，稍后会再试".into(),
        ureq::Error::StatusCode(code) => format!("GitHub 返回 HTTP {code}"),
        ureq::Error::Timeout(_) => "连接 GitHub 超时".into(),
        other => format!("连接 GitHub 失败：{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(v("0.2.0") < v("0.10.0"));
        assert!(v("v1.0.0") > v("0.99.99"));
        assert_eq!(v("v0.3.1"), v("0.3.1"));
    }

    #[test]
    fn suffixed_or_malformed_versions_are_not_versions() {
        for bad in [
            "0.3.0-rc1",
            "0.3",
            "0.3.0.1",
            "0.3.x",
            "+1.0.0",
            "1.+2.3",
            "",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn this_build_has_a_version() {
        assert_eq!(
            Version::current().to_string(),
            concat!("v", env!("CARGO_PKG_VERSION"))
        );
    }

    fn release_json(tag: &str, draft: bool, prerelease: bool) -> String {
        format!(
            r#"{{
              "tag_name": "{tag}",
              "html_url": "https://github.com/HeiSir2014/DroidRescue/releases/tag/{tag}",
              "draft": {draft},
              "prerelease": {prerelease},
              "assets": [
                {{"name": "DroidRescue-0.3.0-win-x64.exe", "size": 34000000,
                  "browser_download_url": "https://example.test/win.exe"}},
                {{"name": "DroidRescue-0.3.0-win-x64.zip", "size": 12000000,
                  "browser_download_url": "https://example.test/win.zip"}},
                {{"name": "DroidRescue-0.3.0-macos-arm64.app.zip", "size": 20000000,
                  "browser_download_url": "https://example.test/mac.zip"}},
                {{"name": "SHA256SUMS.txt", "size": 500,
                  "browser_download_url": "https://example.test/sums"}}
              ]
            }}"#
        )
    }

    #[test]
    fn a_newer_release_offers_this_platforms_package() {
        let json = release_json("v0.3.0", false, false);
        let Check::Available(r) = evaluate(v("0.2.0"), &json, Some(Platform::WindowsX64)).unwrap()
        else {
            panic!("expected an update");
        };
        assert_eq!(r.version, v("0.3.0"));
        assert_eq!(r.package.unwrap().url, "https://example.test/win.exe");
        assert_eq!(r.sums.unwrap().name, "SHA256SUMS.txt");
        assert!(r.page.ends_with("/tag/v0.3.0"));

        let Check::Available(r) = evaluate(v("0.2.0"), &json, Some(Platform::MacArm64)).unwrap()
        else {
            panic!("expected an update");
        };
        assert_eq!(r.package.unwrap().url, "https://example.test/mac.zip");
    }

    #[test]
    fn same_or_older_releases_are_not_offered() {
        let json = release_json("v0.3.0", false, false);
        for current in ["0.3.0", "0.4.0"] {
            let check = evaluate(v(current), &json, Some(Platform::WindowsX64)).unwrap();
            assert_eq!(check, Check::UpToDate, "{current}");
        }
    }

    #[test]
    fn drafts_and_prereleases_are_not_offered() {
        for json in [
            release_json("v0.3.0", true, false),
            release_json("v0.3.0", false, true),
            release_json("v0.3.0-rc1", false, false),
        ] {
            let check = evaluate(v("0.2.0"), &json, Some(Platform::WindowsX64)).unwrap();
            assert_eq!(check, Check::UpToDate);
        }
    }

    #[test]
    fn a_release_without_our_package_can_only_be_downloaded_by_hand() {
        let json = release_json("v0.3.0", false, false);
        let Check::Available(r) = evaluate(v("0.2.0"), &json, None).unwrap() else {
            panic!("expected an update");
        };
        assert_eq!(r.package, None);
    }

    #[test]
    fn a_broken_feed_is_an_error_not_an_update() {
        assert!(evaluate(v("0.2.0"), "<html>rate limited</html>", None).is_err());
    }

    #[test]
    fn checksums_are_found_in_text_and_binary_mode_lines() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums =
            format!("{a}  DroidRescue-0.3.0-win-x64.zip\n{b} *DroidRescue-0.3.0-win-x64.exe\n");
        assert_eq!(
            sum_for(&sums, "DroidRescue-0.3.0-win-x64.zip").as_deref(),
            Some(a.as_str())
        );
        assert_eq!(
            sum_for(&sums, "DroidRescue-0.3.0-win-x64.exe"),
            Some(b.to_ascii_lowercase())
        );
        assert_eq!(sum_for(&sums, "DroidRescue-0.3.0-win-x64"), None);
        assert_eq!(sum_for("short  DroidRescue-0.3.0-win-x64.zip", "x"), None);
    }

    #[test]
    fn the_feed_can_be_a_local_file() {
        let dir = std::env::temp_dir().join(format!("droidrescue-feed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("latest.json");
        std::fs::write(&file, release_json("v9.0.0", false, false)).unwrap();
        let text = fetch_text(file.to_str().unwrap()).unwrap().unwrap();
        assert!(text.contains("v9.0.0"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The real feed (network). `cargo test real_feed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_feed_answers() {
        println!("{:?}", check());
    }
}
