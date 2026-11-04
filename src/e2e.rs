//! End-to-end tests against a real Android device or emulator (adb
//! authorised). They drive the same code the app uses — the poller's status
//! queries and parsers, adb unlock, export, a full mirroring session — so
//! a pass means the feature works on that device, not just that units do.
//!
//! ```text
//! cargo test e2e_ -- --ignored --test-threads=1
//! DROIDRESCUE_SERIAL=<serial>   pick a device (default: the first ready one)
//! DROIDRESCUE_E2E_PIN=1         allow the lock-screen test on a real phone
//!                               (it sets and then clears a test PIN; it
//!                               always runs on emulators)
//! ```
//!
//! USB rescue (AOA) needs physical USB and is not covered here.

use crate::adb::{self, Tools};
use crate::mirror;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Bundled tools and the device under test.
fn device() -> (Tools, String) {
    let tools = Tools::locate().expect("bundled adb / scrcpy-server");
    let out = tools.adb(None, &["devices", "-l"]).expect("adb runs");
    let devices = adb::parse_devices(&String::from_utf8_lossy(&out.stdout));
    let serial = match std::env::var("DROIDRESCUE_SERIAL") {
        Ok(s) => s,
        Err(_) => devices
            .iter()
            .find(|d| d.is_ready())
            .map(|d| d.serial.clone())
            .expect("an authorised adb device or emulator"),
    };
    assert!(
        devices.iter().any(|d| d.serial == serial && d.is_ready()),
        "{serial} is not an authorised adb device: {devices:?}"
    );
    (tools, serial)
}

fn sh(tools: &Tools, serial: &str, cmd: &str) -> String {
    tools
        .shell(Some(serial), cmd)
        .unwrap_or_else(|e| panic!("`{cmd}`: {e}"))
}

/// Poll `check` until true or `timeout`.
fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// The poller's exact queries, parsed by the app's parsers, on real output.
#[test]
#[ignore]
fn e2e_status_queries_parse_real_output() {
    let (tools, serial) = device();
    let q = |cmd| sh(&tools, &serial, cmd);
    assert!(
        adb::parse_awake(&q(adb::AWAKE_CMD)).is_some(),
        "mWakefulness"
    );
    assert!(
        adb::parse_keyguard_showing(&q(adb::KEYGUARD_CMD)).is_some(),
        "isKeyguardShowing"
    );
    let orientation = crate::protocol::parse_orientation(&q(adb::ORIENTATION_CMD));
    assert!(
        matches!(orientation, Some(0..=3)),
        "orientation {orientation:?}"
    );
    let hz = adb::parse_max_refresh(&q(adb::REFRESH_CMD));
    assert!(hz.is_some_and(|hz| hz >= 30.0), "refresh {hz:?}");
    assert!(!q(adb::SERIALNO_CMD).is_empty(), "serial number");

    let out = tools.adb(None, &["devices", "-l"]).unwrap();
    let devices = adb::parse_devices(&String::from_utf8_lossy(&out.stdout));
    let me = devices.iter().find(|d| d.serial == serial).unwrap();
    assert!(!me.model.is_empty(), "model from `adb devices -l`");
    let name = adb::friendly_name(&q(adb::FRIENDLY_NAME_CMD), &me.model);
    assert!(!name.is_empty() && name != "null");
}

/// Clears the test PIN however the test ends.
struct PinGuard<'a> {
    tools: &'a Tools,
    serial: &'a str,
    pin: &'a str,
}

impl Drop for PinGuard<'_> {
    fn drop(&mut self) {
        let _ = self.tools.shell(
            Some(self.serial),
            &format!("locksettings clear --old {}", self.pin),
        );
    }
}

/// 04 解锁 over adb: lock the device with a PIN, unlock it with the app's
/// sequence, check the keyguard is gone.
#[test]
#[ignore]
fn e2e_adb_unlock_with_pin() {
    let (tools, serial) = device();
    if !serial.starts_with("emulator-") && std::env::var("DROIDRESCUE_E2E_PIN").is_err() {
        eprintln!("skipped: sets a lock PIN; set DROIDRESCUE_E2E_PIN=1 to allow on {serial}");
        return;
    }
    let pin = "2580";
    sh(&tools, &serial, &format!("locksettings set-pin {pin}"));
    let _guard = PinGuard {
        tools: &tools,
        serial: &serial,
        pin,
    };
    let keyguard = || adb::parse_keyguard_showing(&sh(&tools, &serial, adb::KEYGUARD_CMD));
    // From awake, so SLEEP really locks with the new PIN (an already
    // sleeping device would ignore it and show no keyguard).
    sh(&tools, &serial, "input keyevent 224"); // WAKEUP
    assert!(
        wait_until(Duration::from_secs(5), || adb::parse_awake(&sh(
            &tools,
            &serial,
            adb::AWAKE_CMD
        )) == Some(true)),
        "device did not wake"
    );
    sh(&tools, &serial, "input keyevent 223"); // SLEEP
    assert!(
        wait_until(Duration::from_secs(5), || keyguard() == Some(true)),
        "device did not lock"
    );

    adb::unlock(&tools, Some(&serial), pin).expect("unlock sequence");
    assert!(
        wait_until(Duration::from_secs(5), || keyguard() == Some(false)),
        "still locked after the unlock sequence"
    );
}

/// 导出照片/文件: a file in Download comes back with its content.
#[test]
#[ignore]
fn e2e_backup_copies_device_folders() {
    let (tools, serial) = device();
    let marker = format!("droidrescue-e2e-{}.txt", std::process::id());
    let remote = format!("/sdcard/Download/{marker}");
    sh(
        &tools,
        &serial,
        &format!("mkdir -p /sdcard/Download && echo e2e-ok > {remote}"),
    );
    let dest = std::env::temp_dir().join(format!("droidrescue-e2e-backup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);

    let lines = std::sync::Mutex::new(Vec::new());
    let result = adb::backup(&tools, Some(&serial), &dest, &|l| {
        lines.lock().unwrap().push(l)
    });
    let copied = result.copied;
    assert_eq!(result.failed, 0, "log: {:?}", lines.lock().unwrap());
    let _ = tools.shell(Some(&serial), &format!("rm -f {remote}"));
    let got = std::fs::read_to_string(dest.join("Download").join(&marker));
    let _ = std::fs::remove_dir_all(&dest);

    assert!(
        copied >= 1,
        "folders copied: {copied}; log: {:?}",
        lines.lock().unwrap()
    );
    assert_eq!(got.expect("exported file").trim(), "e2e-ok");
    assert!(
        !lines.lock().unwrap().iter().any(|l| l.starts_with('✘')),
        "no folder failed: {:?}",
        lines.lock().unwrap()
    );
}

/// 投屏: server push + tunnel + decode → frames of the announced size; the
/// control channel works (HOME leaves Settings); stopping removes the tunnel.
#[test]
#[ignore]
fn e2e_mirror_session_streams_and_controls() {
    let (tools, serial) = device();
    let opts = mirror::Options {
        max_size: 1280,
        turn_screen_off: false,
        ..mirror::Options::default()
    };
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log: mirror::LogFn = {
        let lines = lines.clone();
        Arc::new(move |l| lines.lock().unwrap().push(l))
    };
    // Awake and past a (non-secure) keyguard: frames keep coming and an
    // activity started below can take focus.
    sh(&tools, &serial, "input keyevent 224");
    sh(&tools, &serial, "wm dismiss-keyguard");
    let session = mirror::Session::start(&tools, Some(&serial), &opts, None, log, Arc::new(|| {}))
        .unwrap_or_else(|e| panic!("start: {e}; log: {:?}", lines.lock().unwrap()));
    let port_line = |list: &str| list.contains("localabstract:scrcpy_");

    let shared = session.shared.clone();
    assert!(
        wait_until(Duration::from_secs(15), || shared
            .frame_seq
            .load(Ordering::Acquire)
            >= 3),
        "no frames; ended: {:?}; log: {:?}",
        shared.ended.lock().unwrap(),
        lines.lock().unwrap()
    );
    let (w, h) = *shared.video_size.lock().unwrap();
    assert!(w > 0 && h > 0 && w.max(h) <= 1280, "video size {w}×{h}");
    if let Some(frame) = shared.latest.lock().unwrap().as_ref() {
        assert_eq!((frame.width, frame.height), (w, h), "decoded size");
    }
    assert!(!shared.device_name.lock().unwrap().is_empty());
    assert!(shared.ended.lock().unwrap().is_none());

    // Control channel: open Settings, press HOME through the session.
    // The top resumed activity (mCurrentFocus is per display, and the
    // server's mirror display lists first with focus=null).
    let focus = || {
        sh(
            &tools,
            &serial,
            "dumpsys activity activities | grep -m1 topResumedActivity",
        )
    };
    sh(&tools, &serial, "am start -W -a android.settings.SETTINGS");
    assert!(
        wait_until(Duration::from_secs(5), || focus().contains("settings")),
        "Settings did not open: {}",
        focus()
    );
    session.press_key(3); // HOME via the scrcpy control socket
    assert!(
        wait_until(Duration::from_secs(5), || !focus().contains("settings")),
        "HOME over the control channel had no effect: {}",
        focus()
    );

    let forwards = || {
        String::from_utf8_lossy(&tools.adb(None, &["forward", "--list"]).unwrap().stdout)
            .to_string()
    };
    assert!(port_line(&forwards()), "tunnel open while mirroring");
    drop(session);
    assert!(
        !port_line(&forwards()),
        "tunnel removed after stop: {}",
        forwards()
    );
}
