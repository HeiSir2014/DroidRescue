//! adb / scrcpy helpers: tool discovery, command execution and output parsing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Prevents a console window from flashing for each spawned process.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// adb's file name in the bundle.
const ADB: &str = if cfg!(windows) { "adb.exe" } else { "adb" };

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub model: String,
}

impl Device {
    pub fn is_ready(&self) -> bool {
        self.state == "device"
    }
}

/// Parse the output of `adb devices -l`.
pub fn parse_devices(out: &str) -> Vec<Device> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            let serial = tokens.next()?.to_string();
            let state = tokens.next()?.to_string();
            let model = tokens
                .find_map(|t| t.strip_prefix("model:"))
                .unwrap_or("")
                .to_string();
            Some(Device {
                serial,
                state,
                model,
            })
        })
        .collect()
}

/// Quote text for `adb shell input text`: spaces become `%s` (input's own
/// space escape) and the whole thing is single-quoted for the device shell.
pub fn quote_input_text(text: &str) -> String {
    let escaped = text.replace(' ', "%s").replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// Extract the IPv4 address from `ip -f inet addr show wlan0`.
pub fn parse_wlan_ip(out: &str) -> Option<String> {
    let rest = out.split("inet ").nth(1)?;
    let ip: String = rest
        .chars()
        .take_while(|c| *c != '/' && !c.is_whitespace())
        .collect();
    (!ip.is_empty()).then_some(ip)
}

/// Local ports of scrcpy forward tunnels for `serial` in `adb forward --list`
/// (leftovers from a session whose process was killed).
pub fn stale_scrcpy_forwards(list: &str, serial: &str) -> Vec<String> {
    list.lines()
        .filter_map(|line| {
            let mut t = line.split_whitespace();
            let (ser, local, remote) = (t.next()?, t.next()?, t.next()?);
            (ser == serial && remote.starts_with("localabstract:scrcpy_"))
                .then(|| local.to_string())
        })
        .collect()
}

/// Shell command whose three output lines feed `friendly_name`.
pub const FRIENDLY_NAME_CMD: &str =
    "settings get global device_name; getprop ro.product.marketname; getprop ro.vivo.market.name";

/// Human name for the caption: a user-chosen device name wins; otherwise the
/// marketing name (standard property, then vivo's); the model as fallback.
pub fn friendly_name(output: &str, model: &str) -> String {
    let mut lines = output.lines().map(str::trim);
    let device_name = lines.next().unwrap_or_default();
    let market = lines.next().unwrap_or_default();
    let vivo_market = lines.next().unwrap_or_default();
    let usable = |s: &str| !s.is_empty() && s != "null";
    if usable(device_name) && device_name != model {
        device_name.to_string()
    } else if usable(market) {
        market.to_string()
    } else if usable(vivo_market) {
        vivo_market.to_string()
    } else {
        model.to_string()
    }
}

/// Highest refresh rate the device panel supports, from the `fps=` /
/// `refreshRate=` entries of `dumpsys display`.
pub fn parse_max_refresh(dumpsys: &str) -> Option<f32> {
    dumpsys
        .split(['\n', ' ', ',', '{', '}'])
        .filter_map(|tok| {
            tok.strip_prefix("fps=")
                .or_else(|| tok.strip_prefix("refreshRate="))
        })
        .filter_map(|v| v.parse::<f32>().ok())
        .filter(|hz| hz.is_finite() && *hz > 0.0)
        .reduce(f32::max)
}

/// Capture cap: never encode frames the monitor can't show nor the panel
/// can't produce. `user` overrides when non-zero; 0 = unlimited if unknown.
pub fn capture_fps(device_hz: Option<f32>, monitor_hz: Option<u32>, user: u32) -> u32 {
    if user > 0 {
        return user;
    }
    let device = device_hz.map(|hz| hz.round() as u32);
    match (device, monitor_hz) {
        (Some(d), Some(m)) => d.min(m),
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => 0,
    }
}

/// `isKeyguardShowing=true|false` from `dumpsys window`.
pub fn parse_keyguard_showing(out: &str) -> Option<bool> {
    parse_flag(out, "isKeyguardShowing=")
}

/// `mWakefulness=Awake|Asleep|Dozing` from `dumpsys power`.
pub fn parse_awake(out: &str) -> Option<bool> {
    let rest = out.split("mWakefulness=").nth(1)?;
    Some(rest.starts_with("Awake"))
}

fn parse_flag(out: &str, key: &str) -> Option<bool> {
    let rest = out.split(key).nth(1)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// adb, scrcpy-server and the FFmpeg DLLs, all from the embedded bundle.
#[derive(Clone, Debug)]
pub struct Tools {
    pub adb: PathBuf,
    /// scrcpy-server jar (pushed to the device).
    pub server: PathBuf,
    /// Must match the server exactly (protocol is version-locked).
    pub server_version: String,
    /// Directory with the FFmpeg DLLs.
    pub dir: PathBuf,
}

impl Tools {
    /// Use `<exe dir>\bundle\` if present (portable override), otherwise the
    /// bundle embedded in this executable.
    pub fn locate() -> Result<Tools, String> {
        let override_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("bundle")));
        if let Some(tools) = override_dir.as_deref().and_then(Tools::in_dir) {
            return Ok(tools);
        }
        let base = crate::bundle::default_base();
        let dir = crate::bundle::extract_to(&base)
            .map_err(|e| format!("解压内置组件到 {} 失败: {e}", base.display()))?;
        Tools::in_dir(&dir).ok_or_else(|| format!("{} 中缺少 {ADB} / scrcpy-server", dir.display()))
    }

    fn in_dir(dir: &Path) -> Option<Tools> {
        let adb = dir.join(ADB);
        let server = dir.join("scrcpy-server");
        (adb.is_file() && server.is_file()).then(|| Tools {
            adb,
            server,
            server_version: crate::bundle::SERVER_VERSION.to_string(),
            dir: dir.to_path_buf(),
        })
    }

    pub fn adb_command(&self, serial: Option<&str>, args: &[&str]) -> Command {
        let mut cmd = hidden_command(&self.adb);
        if let Some(s) = serial {
            cmd.args(["-s", s]);
        }
        cmd.args(args);
        cmd
    }

    pub fn adb(&self, serial: Option<&str>, args: &[&str]) -> std::io::Result<Output> {
        self.adb_command(serial, args).stdin(Stdio::null()).output()
    }

    /// Run `adb shell <cmd>` and return stdout (stderr appended on failure).
    pub fn shell(&self, serial: Option<&str>, cmd: &str) -> Result<String, String> {
        let out = self
            .adb(serial, &["shell", cmd])
            .map_err(|e| format!("无法运行 adb: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(format!("`{cmd}` 失败: {stderr} {stdout}"))
        }
    }
}

/// Swipe up the keyguard on any device: vertical stroke along the short
/// side of `wm size`, so it stays on screen in portrait and landscape
/// (`input` coordinates follow the current rotation; `wm size` is natural).
pub const SWIPE_UP_CMD: &str = "s=$(wm size | grep -oE '[0-9]+x[0-9]+' | tail -n 1); \
    w=${s%x*}; h=${s#*x}; m=$(( w < h ? w : h )); \
    input swipe $(( m / 2 )) $(( m * 8 / 10 )) $(( m / 2 )) $(( m * 2 / 10 )) 300";

/// Android's default long-press timeout (ViewConfiguration.DEFAULT_LONG_PRESS_TIMEOUT).
pub const LONG_PRESS: std::time::Duration = std::time::Duration::from_millis(400);

/// `input keyevent` for a key the user held for `held` (shell can't stream a hold).
pub fn keyevent_cmd(keycode: u32, held: std::time::Duration) -> String {
    if held >= LONG_PRESS {
        format!("input keyevent --longpress {keycode}")
    } else {
        format!("input keyevent {keycode}")
    }
}

fn hidden_command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hold_is_a_tap() {
        assert_eq!(
            keyevent_cmd(26, std::time::Duration::from_millis(120)),
            "input keyevent 26"
        );
    }

    #[test]
    fn long_hold_is_a_longpress() {
        assert_eq!(
            keyevent_cmd(26, std::time::Duration::from_millis(900)),
            "input keyevent --longpress 26"
        );
    }

    #[test]
    fn parses_ready_usb_device_with_model() {
        let out = "List of devices attached\nSER0123456789AB        device product:DPD2345 model:PA2455 device:DPD2345 transport_id:2\n\n";
        assert_eq!(
            parse_devices(out),
            vec![Device {
                serial: "SER0123456789AB".into(),
                state: "device".into(),
                model: "PA2455".into()
            }]
        );
    }

    #[test]
    fn parses_unauthorized_device_and_skips_daemon_banner() {
        let out = "* daemon not running; starting now at tcp:5037\n* daemon started successfully\nList of devices attached\nABC unauthorized usb:1-1 transport_id:3\n";
        let devices = parse_devices(out);
        assert_eq!(devices.len(), 1);
        assert!(!devices[0].is_ready());
    }

    #[test]
    fn parses_empty_device_list() {
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }

    #[test]
    fn quotes_input_text_spaces_and_quotes() {
        assert_eq!(quote_input_text("1234"), "'1234'");
        assert_eq!(quote_input_text("a b"), "'a%sb'");
        assert_eq!(quote_input_text("it's"), r"'it'\''s'");
        assert_eq!(quote_input_text("a&b;c"), "'a&b;c'");
    }

    #[test]
    fn parses_wlan_ip() {
        let out = "47: wlan0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n    inet 192.168.1.23/24 brd 192.168.1.255 scope global wlan0\n";
        assert_eq!(parse_wlan_ip(out), Some("192.168.1.23".into()));
        assert_eq!(parse_wlan_ip(""), None);
    }

    #[test]
    fn friendly_name_prefers_custom_then_market_then_model() {
        // Captured from the Pad3: device_name = model, no standard market name.
        assert_eq!(
            friendly_name("PA2455\n\nvivo Pad3\n", "PA2455"),
            "vivo Pad3"
        );
        assert_eq!(
            friendly_name("我的平板\n\nvivo Pad3\n", "PA2455"),
            "我的平板",
            "user-chosen name wins"
        );
        assert_eq!(friendly_name("null\nPixel 9\n\n", "GKWS6"), "Pixel 9");
        assert_eq!(friendly_name("", "PA2455"), "PA2455");
    }

    #[test]
    fn parses_highest_supported_refresh_rate() {
        // Captured from the vivo Pad3 (dumpsys display).
        let out = "fps=120.00001\nfps=144.00002\nfps=30.000002\n{id=1, width=1968, fps=60.000004, x}\nrefreshRate=90.0";
        assert_eq!(parse_max_refresh(out), Some(144.00002));
        assert_eq!(parse_max_refresh("nothing here"), None);
    }

    #[test]
    fn capture_fps_is_min_of_panel_and_monitor() {
        assert_eq!(capture_fps(Some(144.00002), Some(60), 0), 60);
        assert_eq!(capture_fps(Some(60.000004), Some(165), 0), 60);
        assert_eq!(capture_fps(None, Some(75), 0), 75);
        assert_eq!(capture_fps(Some(90.0), None, 0), 90);
        assert_eq!(capture_fps(None, None, 0), 0, "unknown → unlimited");
        assert_eq!(
            capture_fps(Some(144.0), Some(60), 30),
            30,
            "explicit user cap wins"
        );
    }

    #[test]
    fn finds_stale_scrcpy_forwards_for_one_device_only() {
        let list = "401E tcp:59902 localabstract:scrcpy_3635732c\n\
                    401E tcp:8080 tcp:8080\n\
                    OTHER tcp:55799 localabstract:scrcpy_16e67374\n";
        assert_eq!(
            stale_scrcpy_forwards(list, "401E"),
            vec!["tcp:59902".to_string()]
        );
        assert!(stale_scrcpy_forwards("", "401E").is_empty());
    }

    #[test]
    fn parses_keyguard_and_wakefulness() {
        assert_eq!(
            parse_keyguard_showing("    isKeyguardShowing=false\n"),
            Some(false)
        );
        assert_eq!(parse_keyguard_showing("isKeyguardShowing=true"), Some(true));
        assert_eq!(parse_keyguard_showing("nothing"), None);
        assert_eq!(parse_awake("  mWakefulness=Awake\n"), Some(true));
        assert_eq!(parse_awake("  mWakefulness=Asleep\n"), Some(false));
    }
}
