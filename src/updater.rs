//! Updater, part 3: the background worker and what the UI shows of it.
//!
//! One thread: a check a few seconds after start, then every 6 hours (or
//! on 立即检查). A newer release is downloaded and swapped in right away
//! (`update_swap`); the title then says so and the user restarts when it
//! suits them. Nothing restarts by itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::update::{self, Check, Version};
use crate::update_swap::{self, InstallError};

/// First check after start: let the window and the device poll settle.
const FIRST_CHECK: Duration = Duration::from_secs(8);
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// After a failure (offline, GitHub unreachable) try again sooner.
const RETRY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Not checked yet (or automatic checks are off).
    Idle,
    Checking,
    UpToDate {
        at: Instant,
    },
    Downloading {
        version: Version,
        done: u64,
        total: u64,
    },
    /// Swapped in: the next start is `version`.
    Ready {
        version: Version,
    },
    /// Newer, but it has to be downloaded by hand.
    Manual {
        version: Version,
        page: String,
        why: String,
    },
    Failed {
        at: Instant,
        why: String,
    },
}

/// What the title shows for a state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Badge {
    /// Information only.
    Progress(String),
    /// Click: restart into the new version.
    Restart(String),
    /// Click: open the release page.
    Download { text: String, page: String },
}

impl Badge {
    pub fn text(&self) -> &str {
        match self {
            Badge::Progress(t) | Badge::Restart(t) | Badge::Download { text: t, .. } => t,
        }
    }
}

/// The title badge for `state`; `None` when there is nothing to tell.
pub fn badge(state: &State) -> Option<Badge> {
    match state {
        State::Downloading {
            version,
            done,
            total,
        } => {
            let pct = done * 100 / (*total).max(1);
            Some(Badge::Progress(format!("下载新版本 {version} · {pct}%")))
        }
        State::Ready { version } => Some(Badge::Restart(format!("新版本 {version} · 重启更新"))),
        State::Manual { version, page, .. } => Some(Badge::Download {
            text: format!("新版本 {version} · 去下载"),
            page: page.clone(),
        }),
        _ => None,
    }
}

/// One line for the settings page.
pub fn describe(state: &State) -> String {
    match state {
        State::Idle => "尚未检查".into(),
        State::Checking => "正在检查…".into(),
        State::UpToDate { at } => format!("已是最新版本 · {}", crate::logbar::ago(at.elapsed())),
        State::Downloading { .. } | State::Ready { .. } | State::Manual { .. } => badge(state)
            .map(|b| b.text().to_string())
            .unwrap_or_default(),
        State::Failed { why, .. } => format!("检查失败：{why}"),
    }
}

type LogFn = Arc<dyn Fn(String) + Send + Sync>;

pub struct Updater {
    state: Arc<Mutex<State>>,
    tx: Sender<()>,
    auto: Arc<AtomicBool>,
    /// What gets replaced and relaunched (the exe / the `.app`).
    target: Option<PathBuf>,
}

impl Updater {
    /// Starts the worker. `repaint` wakes the UI when the state changes.
    pub fn spawn(auto: bool, log: LogFn, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let target = update_swap::install_target();
        if let Some(t) = target.clone() {
            // Leftovers of the previous update (its old version may still
            // be closing, so this can take a few seconds).
            thread::spawn(move || update_swap::cleanup(&t));
        }
        let state = Arc::new(Mutex::new(State::Idle));
        let auto = Arc::new(AtomicBool::new(auto));
        let (tx, rx) = mpsc::channel::<()>();
        {
            let state = state.clone();
            let auto = auto.clone();
            thread::spawn(move || {
                let mut wait = FIRST_CHECK;
                loop {
                    let asked = match rx.recv_timeout(wait) {
                        Ok(()) => true,
                        Err(RecvTimeoutError::Timeout) => false,
                        Err(RecvTimeoutError::Disconnected) => return,
                    };
                    wait = INTERVAL;
                    if !asked && !auto.load(Ordering::Relaxed) {
                        continue;
                    }
                    if matches!(*state.lock().unwrap(), State::Ready { .. }) {
                        // Already installed; nothing newer until a restart.
                        continue;
                    }
                    run_once(&state, &log, &*repaint, asked);
                    if matches!(*state.lock().unwrap(), State::Failed { .. }) {
                        wait = RETRY;
                    }
                }
            });
        }
        Updater {
            state,
            tx,
            auto,
            target,
        }
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    pub fn check_now(&self) {
        let _ = self.tx.send(());
    }

    pub fn auto(&self) -> bool {
        self.auto.load(Ordering::Relaxed)
    }

    pub fn set_auto(&self, on: bool) {
        self.auto.store(on, Ordering::Relaxed);
    }

    /// The installed app to start after this one exits, once an update is
    /// in place.
    pub fn restart_target(&self) -> Option<PathBuf> {
        matches!(self.state(), State::Ready { .. })
            .then(|| self.target.clone())
            .flatten()
    }
}

/// One check, and the download/swap if something newer is out.
fn run_once(state: &Mutex<State>, log: &LogFn, repaint: &dyn Fn(), asked: bool) {
    let set = |s: State| {
        *state.lock().unwrap() = s;
        repaint();
    };
    set(State::Checking);
    let release = match update::check() {
        Err(why) => {
            log(format!("✘ 检查更新失败：{why}"));
            return set(State::Failed {
                at: Instant::now(),
                why,
            });
        }
        Ok(Check::UpToDate) => {
            if asked {
                log(format!("✔ 已是最新版本 {}", Version::current()));
            }
            return set(State::UpToDate { at: Instant::now() });
        }
        Ok(Check::Available(release)) => release,
    };
    let version = release.version;
    log(format!("▶ 发现新版本 {version}，正在后台下载…"));
    set(State::Downloading {
        version,
        done: 0,
        total: release.package.as_ref().map_or(0, |p| p.size),
    });
    let mut shown = 0;
    let result = if cfg!(debug_assertions) {
        // A dev build must never replace itself with a release.
        Err(InstallError::Manual("开发版本不自动安装".into()))
    } else {
        update_swap::install(&release, &mut |done, total| {
            let pct = done * 100 / total.max(1);
            if pct != shown {
                shown = pct;
                set(State::Downloading {
                    version,
                    done,
                    total,
                });
            }
        })
    };
    match result {
        Ok(()) => {
            log(format!(
                "✔ 新版本 {version} 已就绪：点标题栏的「重启更新」，或下次打开时生效"
            ));
            set(State::Ready { version });
        }
        Err(InstallError::Retry(why)) => {
            log(format!(
                "✘ 下载新版本 {version} 失败：{why}（稍后自动重试）"
            ));
            set(State::Failed {
                at: Instant::now(),
                why,
            });
        }
        Err(InstallError::Manual(why)) => {
            log(format!("● 新版本 {version} 需要手动下载：{why}"));
            set(State::Manual {
                version,
                page: release.page.clone(),
                why,
            });
        }
    }
}

/// Starts the installed (new) version. Called as this one exits.
pub fn relaunch(target: &Path) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        // A new instance even though this one is still closing.
        let mut c = std::process::Command::new("open");
        c.arg("-n").arg(target);
        c
    } else {
        std::process::Command::new(target)
    };
    command.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn nothing_is_shown_while_idle_checking_up_to_date_or_failed() {
        for state in [
            State::Idle,
            State::Checking,
            State::UpToDate { at: Instant::now() },
            State::Failed {
                at: Instant::now(),
                why: "offline".into(),
            },
        ] {
            assert_eq!(badge(&state), None, "{state:?}");
        }
    }

    #[test]
    fn the_badge_follows_the_download_to_a_restart() {
        let downloading = State::Downloading {
            version: v("0.3.0"),
            done: 37,
            total: 100,
        };
        assert_eq!(
            badge(&downloading),
            Some(Badge::Progress("下载新版本 v0.3.0 · 37%".into()))
        );
        let ready = State::Ready {
            version: v("0.3.0"),
        };
        assert_eq!(
            badge(&ready),
            Some(Badge::Restart("新版本 v0.3.0 · 重启更新".into()))
        );
    }

    #[test]
    fn an_update_we_cannot_install_links_to_its_page() {
        let manual = State::Manual {
            version: v("0.3.0"),
            page: "https://example.test/r".into(),
            why: "x".into(),
        };
        assert_eq!(
            badge(&manual),
            Some(Badge::Download {
                text: "新版本 v0.3.0 · 去下载".into(),
                page: "https://example.test/r".into()
            })
        );
    }

    #[test]
    fn an_unknown_size_does_not_divide_by_zero() {
        let state = State::Downloading {
            version: v("0.3.0"),
            done: 10,
            total: 0,
        };
        assert!(badge(&state).is_some());
    }

    #[test]
    fn settings_describe_every_state() {
        assert_eq!(describe(&State::Idle), "尚未检查");
        assert!(describe(&State::UpToDate { at: Instant::now() }).starts_with("已是最新版本"));
        assert!(
            describe(&State::Failed {
                at: Instant::now(),
                why: "连接 GitHub 超时".into()
            })
            .contains("超时")
        );
    }
}
