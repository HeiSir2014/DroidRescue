//! Rescue worker: owns the AOA connection on its own thread so the UI never
//! blocks on USB. The device is held only while commands are running and is
//! released after a short idle period, so MTP / adb can see the tablet again.

use crate::aoa::{self, Aoa};
use crate::usbid::UsbTarget;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Release the device after this long without commands.
const IDLE_RELEASE: Duration = Duration::from_secs(20);
/// Default settle time between steps (UI animations).
const STEP_PAUSE_MS: u32 = 350;
/// The lock screen sleeps after a few seconds; poke it at least this often
/// while a sequence runs (a bare Shift tap: wakes/keeps awake, types nothing).
const KEEP_AWAKE: Duration = Duration::from_millis(2000);
/// How often to retry opening the tablet during `reconnect`.
const RECONNECT_POLL: Duration = Duration::from_millis(500);

/// One step of a blind input sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// (modifiers, usage); usage 0 = modifier-only tap.
    Key(u8, u8),
    Text(String),
    Consumer(u16),
    Swipe(i32, i32),
    /// Absolute pen tap; coordinates in 1/10000 of the screen as displayed.
    Tap(u16, u16),
    /// Pen hovers (in range, not touching) — shows where a tap would land,
    /// with 开发者选项 → 指针位置 on. For aiming taps while writing scripts.
    Hover(u16, u16),
    /// Pen down at `from`, moves to `to` over `ms`, up. Same points = long
    /// press.
    Drag {
        from: (u16, u16),
        to: (u16, u16),
        ms: u32,
    },
    /// Android display rotation 0..3 for later taps (default 1: landscape).
    Rotation(u8),
    /// Sleep while keeping the screen awake.
    Wait(u32),
    /// The tablet re-enumerates (USB mode switch): drop the connection and
    /// reopen it within this many ms, then continue.
    Reconnect(u32),
}

// ───────────────────────── scripts ─────────────────────────
//
// Workflows are data: `;`- or newline-separated commands, `*N` repeats.
//   wake | enter | tab | space | esc | backspace | up | down | left | right
//   home | back | select | kpenter        (system / confirm keys)
//   meta+i | ctrl+a | shift+tab           (modifier + key or letter)
//   a..z 0..9                             (single key)
//   click:<x>,<y>  (pen tap, screen fractions)   rot:<0-3>
//   move:<x>,<y>   (pen hovers there without touching — aim a click)
//   drag:<x1>,<y1>,<x2>,<y2>[,<ms>]  (press, move, release; same point = long press)
//   text:<chars>   swipe:<dx>,<dy>   wait:<ms>   pause   reconnect:<ms>
// `{name}` is substituted from the variables passed to `parse`.

pub const SCRIPT_WAKE: &str = "wake; pause";
/// Wake → swipe the keyguard up → PIN → Enter.
pub const SCRIPT_UNLOCK: &str =
    "wake; wait:500; swipe:0,-700; wait:700; text:{pw}; enter; wait:800";

/// Full rescue, verified on the Pad3 (landscape) 2026-11-04 — screen and
/// USB debugging both unavailable at the start. Positions are fractions of
/// the landscape screen measured from device screenshots (2000×1406).
///  1. unlock with the saved PIN; the "USB 已连接" dialog appears
///  2. pen-tap the USB mode 连接办公套件 → the tablet itself turns USB
///     debugging on and re-enumerates (PID 6012 → 6013); no vivo software
///     needs to run on the PC (verified by the user 2026-11-04)
///  3. dismiss vivo's informational "需打开电脑的 vivo 办公套件" notice (知道了)
///  4. "允许 USB 调试吗？": tick 一律允许, tap 允许
pub const SCRIPT_ENABLE_DEBUGGING: &str = "\
    wake; wait:500; swipe:0,-700; wait:700; text:{pw}; enter; wait:2500; \
    rot:1; click:0.41,0.61; wait:500; \
    reconnect:20000; wait:1500; \
    click:0.5,0.582; wait:800; \
    click:0.3625,0.525; wait:400; \
    click:0.5,0.601; wait:600";

const MOD_CTRL: u8 = 0x01;
const MOD_ALT: u8 = 0x04;
const MOD_META: u8 = 0x08;

fn named_key(name: &str) -> Option<Step> {
    let k = |u| Some(Step::Key(0, u));
    match name {
        "wake" => Some(Step::Key(aoa::MOD_SHIFT, 0)),
        "enter" => k(aoa::KEY_ENTER),
        "tab" => k(aoa::KEY_TAB),
        "space" => k(aoa::KEY_SPACE),
        "esc" => k(aoa::KEY_ESCAPE),
        "backspace" => k(aoa::KEY_BACKSPACE),
        "up" => k(aoa::KEY_UP),
        "down" => k(aoa::KEY_DOWN),
        "left" => k(aoa::KEY_LEFT),
        "right" => k(aoa::KEY_RIGHT),
        "home" => Some(Step::Consumer(aoa::USAGE_AC_HOME)),
        "back" => Some(Step::Consumer(aoa::USAGE_AC_BACK)),
        // D-pad centre (KEY_SELECT → KEYCODE_DPAD_CENTER).
        "select" | "center" => Some(Step::Consumer(aoa::USAGE_MENU_PICK)),
        "kpenter" => k(aoa::KEY_KP_ENTER),
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => aoa::ascii_to_hid(c).map(|(m, u)| Step::Key(m, u)),
                _ => None,
            }
        }
    }
}

fn parse_command(cmd: &str) -> Result<Step, String> {
    if let Some(t) = cmd.strip_prefix("text:") {
        return Ok(Step::Text(t.to_string()));
    }
    if let Some(ms) = cmd.strip_prefix("wait:") {
        return ms
            .trim()
            .parse()
            .map(Step::Wait)
            .map_err(|_| format!("wait 需要毫秒数: {cmd}"));
    }
    if let Some(ms) = cmd.strip_prefix("reconnect:") {
        return ms
            .trim()
            .parse()
            .map(Step::Reconnect)
            .map_err(|_| format!("reconnect 需要毫秒数: {cmd}"));
    }
    if cmd == "pause" {
        return Ok(Step::Wait(STEP_PAUSE_MS));
    }
    if let Some(r) = cmd.strip_prefix("rot:") {
        return match r.trim().parse::<u8>() {
            Ok(v) if v < 4 => Ok(Step::Rotation(v)),
            _ => Err(format!("rot 需要 0~3（屏幕旋转）: {cmd}")),
        };
    }
    for prefix in ["click:", "tap:"] {
        if let Some(xy) = cmd.strip_prefix(prefix) {
            let (x, y) = fraction_pair(xy, cmd)?;
            return Ok(Step::Tap(x, y));
        }
    }
    for prefix in ["move:", "hover:"] {
        if let Some(xy) = cmd.strip_prefix(prefix) {
            let (x, y) = fraction_pair(xy, cmd)?;
            return Ok(Step::Hover(x, y));
        }
    }
    if let Some(args) = cmd.strip_prefix("drag:") {
        return parse_drag(args, cmd);
    }
    if let Some(xy) = cmd.strip_prefix("swipe:") {
        let (x, y) = xy
            .split_once(',')
            .ok_or_else(|| format!("swipe 需要 dx,dy: {cmd}"))?;
        let n = |s: &str| {
            s.trim()
                .parse::<i32>()
                .map_err(|_| format!("swipe 坐标无效: {cmd}"))
        };
        return Ok(Step::Swipe(n(x)?, n(y)?));
    }
    // modifier+key chains, e.g. meta+i, ctrl+shift+tab
    let mut mods = 0u8;
    let mut parts: Vec<&str> = cmd.split('+').collect();
    let last = parts.pop().unwrap_or_default();
    for m in parts {
        mods |= match m {
            "ctrl" => MOD_CTRL,
            "shift" => aoa::MOD_SHIFT,
            "alt" => MOD_ALT,
            "meta" | "win" => MOD_META,
            _ => return Err(format!("未知修饰键 {m}: {cmd}")),
        };
    }
    match named_key(last) {
        Some(Step::Key(m, u)) => Ok(Step::Key(m | mods, u)),
        Some(other) if mods == 0 => Ok(other),
        _ => Err(format!("未知按键: {cmd}")),
    }
}

/// Drag duration when the script gives none.
const DRAG_DEFAULT_MS: u32 = 300;
/// Longest drag / long press a script may ask for.
const DRAG_MAX_MS: u32 = 10_000;

/// "x1,y1,x2,y2[,ms]".
fn parse_drag(args: &str, cmd: &str) -> Result<Step, String> {
    let parts: Vec<&str> = args.split(',').collect();
    let usage = || format!("drag 需要 x1,y1,x2,y2[,毫秒]（坐标为 0~1 的比例）: {cmd}");
    let (points, ms) = match parts.as_slice() {
        [a, b, c, d] => ([*a, *b, *c, *d], DRAG_DEFAULT_MS),
        [a, b, c, d, ms] => (
            [*a, *b, *c, *d],
            match ms.trim().parse::<u32>() {
                Ok(v) if (1..=DRAG_MAX_MS).contains(&v) => v,
                _ => return Err(format!("drag 时长需在 1~{DRAG_MAX_MS} 毫秒: {cmd}")),
            },
        ),
        _ => return Err(usage()),
    };
    let from = fraction_pair(&format!("{},{}", points[0], points[1]), cmd)?;
    let to = fraction_pair(&format!("{},{}", points[2], points[3]), cmd)?;
    Ok(Step::Drag { from, to, ms })
}

/// "0.41,0.61" → (4100, 6100) in 1/10000 of the screen.
fn fraction_pair(xy: &str, cmd: &str) -> Result<(u16, u16), String> {
    let (x, y) = xy
        .split_once(',')
        .ok_or_else(|| format!("需要 x,y（0~1 的比例）: {cmd}"))?;
    let frac = |s: &str| match s.trim().parse::<f32>() {
        Ok(v) if (0.0..=1.0).contains(&v) => Ok((v * 10_000.0).round() as u16),
        _ => Err(format!("坐标必须是 0~1 的比例: {cmd}")),
    };
    Ok((frac(x)?, frac(y)?))
}

/// Parse a workflow script, substituting `{var}` placeholders.
pub fn parse(script: &str, vars: &[(&str, &str)]) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for raw in script.split([';', '\n']) {
        let mut cmd = raw.trim().to_string();
        if cmd.is_empty() || cmd.starts_with('#') {
            continue;
        }
        for (name, value) in vars {
            cmd = cmd.replace(&format!("{{{name}}}"), value);
        }
        // `text:` keeps its payload verbatim (it may contain '*').
        let (body, times) = match cmd.rsplit_once('*') {
            Some((b, n)) if !cmd.starts_with("text:") && n.trim().parse::<u32>().is_ok() => {
                (b.trim().to_string(), n.trim().parse::<u32>().unwrap())
            }
            _ => (cmd.clone(), 1),
        };
        let step = parse_command(&body)?;
        steps.extend(std::iter::repeat_n(step, times as usize));
    }
    Ok(steps)
}

/// How a blind unlock reaches the device.
#[derive(Debug, PartialEq, Eq)]
pub enum UnlockRoute {
    /// adb `input` (debugging on and authorised).
    Adb,
    /// USB keyboard (AOA HID) to the tablet with this serial.
    Usb(String),
    /// Nothing usable connected (or several USB tablets: ambiguous).
    None,
}

pub fn unlock_route(adb_ready: bool, usb_serials: &[String]) -> UnlockRoute {
    match (adb_ready, usb_serials) {
        (true, _) => UnlockRoute::Adb,
        (false, [one]) => UnlockRoute::Usb(one.clone()),
        _ => UnlockRoute::None,
    }
}

pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Default, Clone)]
pub struct Status {
    /// Description of the held device, if connected.
    pub device: Option<String>,
    pub serial: Option<String>,
    pub busy: bool,
    /// The last attempt found no WinUSB-bound tablet: offer the setup link.
    pub needs_setup: bool,
}

enum Cmd {
    Run(&'static str, Vec<Step>),
    Release,
}

pub struct Rescue {
    tx: Sender<Cmd>,
    pub status: Arc<Mutex<Status>>,
    /// USB functions the rescue may bind and open (from the profiles).
    targets: Arc<Mutex<Vec<UsbTarget>>>,
}

impl Rescue {
    pub fn spawn(log: LogFn) -> Rescue {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let targets = Arc::new(Mutex::new(Vec::new()));
        let (st, tg) = (status.clone(), targets.clone());
        thread::spawn(move || worker(rx, st, tg, log));
        Rescue {
            tx,
            status,
            targets,
        }
    }

    /// Set after (re)loading the profiles.
    pub fn set_targets(&self, targets: Vec<UsbTarget>) {
        *self.targets.lock().unwrap() = targets;
    }

    pub fn run(&self, name: &'static str, steps: Vec<Step>) {
        let _ = self.tx.send(Cmd::Run(name, steps));
    }

    /// Parse and queue a workflow script (see the script syntax above).
    pub fn run_script(
        &self,
        name: &'static str,
        script: &str,
        vars: &[(&str, &str)],
    ) -> Result<(), String> {
        self.run(name, parse(script, vars)?);
        Ok(())
    }

    pub fn release(&self) {
        let _ = self.tx.send(Cmd::Release);
    }
}

/// Open the tablet and register the HID devices.
fn open(targets: &[UsbTarget]) -> Result<Aoa, String> {
    let mut aoa = Aoa::open(None, targets)?;
    aoa.register_all()?;
    Ok(aoa)
}

/// [`open`], first binding WinUSB to a target that has no driver yet —
/// first use on this PC, USB port or mode. One UAC prompt (the helper is
/// elevated, never this process), then wait for the driver to come up.
/// (macOS needs no driver: nothing is ever unbound there.)
fn connect(targets: &[UsbTarget], log: &LogFn) -> Result<Aoa, String> {
    match open(targets) {
        Err(e) if e.contains(crate::usbdev::NOT_FOUND) => {}
        other => return other,
    }
    let unbound = crate::usbhost::unbound_targets(targets);
    let Some(id) = unbound.first() else {
        return Err(crate::usbdev::NOT_FOUND.into());
    };
    log(format!(
        "… 救援: {id} 还没有 WinUSB，正在请求管理员权限安装 —— 请在电脑上点“是”"
    ));
    crate::usbhost::install_elevated(targets)?;
    let deadline = Instant::now() + INSTALL_GRACE;
    while Instant::now() < deadline {
        thread::sleep(RECONNECT_POLL);
        if !crate::usbdev::present_parents(targets).is_empty() {
            return open(targets);
        }
    }
    Err("等待 WinUSB 安装超时（管理员授权被拒绝？详情见日志）".into())
}

fn worker(
    rx: Receiver<Cmd>,
    status: Arc<Mutex<Status>>,
    targets: Arc<Mutex<Vec<UsbTarget>>>,
    log: LogFn,
) {
    let mut held: Option<Aoa> = None;
    let set = |device: &Option<Aoa>, busy: bool| {
        let mut s = status.lock().unwrap();
        s.device = device.as_ref().map(|a| a.description.clone());
        s.serial = device.as_ref().map(|a| a.serial.clone());
        s.busy = busy;
        if device.is_some() {
            s.needs_setup = false;
        }
    };
    loop {
        let cmd = match rx.recv_timeout(IDLE_RELEASE) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => Cmd::Release,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match cmd {
            Cmd::Release => {
                if held.take().is_some() {
                    log("救援: 已释放平板".into());
                }
                set(&held, false);
            }
            Cmd::Run(name, steps) => {
                set(&held, true);
                let targets = targets.lock().unwrap().clone();
                match run_workflow(&mut held, &steps, &targets, &log) {
                    Ok(o) if o.skipped > 0 => log(format!(
                        "✘ 救援/{name}: {} 个非 ASCII 字符无法用 USB 键盘输入",
                        o.skipped
                    )),
                    Ok(o) if o.device_left => {
                        log(format!("✔ 救援/{name}: 平板已切换 USB 模式并重新连接"));
                        held = None;
                    }
                    Ok(_) if name.is_empty() => {} // keyboard passthrough: too chatty to log
                    Ok(_) => log(format!("✔ 救援/{name}")),
                    Err(e) => {
                        status.lock().unwrap().needs_setup = e.contains(crate::usbdev::NOT_FOUND);
                        log(format!("✘ 救援/{name}: {e}"));
                        held = None; // the device went away or re-enumerated
                    }
                }
                set(&held, false);
            }
        }
    }
}

/// What a finished sequence observed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    /// Characters `text:` could not type (non-ASCII).
    skipped: usize,
    /// The device dropped off USB after the last input — the expected
    /// result of choosing a new USB mode (it re-enumerates).
    device_left: bool,
}

/// Run a workflow, splitting it at `reconnect` steps: each segment runs on a
/// fresh connection once the tablet has come back.
fn run_workflow(
    held: &mut Option<Aoa>,
    steps: &[Step],
    targets: &[UsbTarget],
    log: &LogFn,
) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    // Rotation carries across segments.
    let mut rotation: u8 = 1;
    for segment in steps.split_inclusive(|s| matches!(s, Step::Reconnect(_))) {
        let (body, reconnect) = match segment.split_last() {
            Some((Step::Reconnect(ms), body)) => (body, Some(*ms)),
            _ => (segment, None),
        };
        if !body.is_empty() {
            if held.is_none() {
                let aoa = connect(targets, log)?;
                log(format!("✔ 救援: 已接管 {}", aoa.description));
                *held = Some(aoa);
            }
            let left = run_segment(
                held.as_ref().unwrap(),
                body,
                &mut rotation,
                &mut outcome.skipped,
            )?;
            outcome.device_left = left;
        }
        if let Some(ms) = reconnect {
            let previous = held
                .take()
                .map(|a| a.parent().to_string())
                .unwrap_or_default();
            let aoa = wait_for_device(&previous, ms, targets, log)?;
            log(format!("✔ 救援: 平板重新连接 ({})", aoa.parent()));
            *held = Some(aoa);
            outcome.device_left = false;
        }
    }
    Ok(outcome)
}

/// Extra time to wait while the user answers the UAC prompt for a WinUSB
/// install of the new USB mode's interface.
const INSTALL_GRACE: Duration = Duration::from_secs(90);

/// Is a present tablet the *new* USB configuration? The old one lingers on
/// the bus for a while after the tap that switches modes; opening it would
/// "reconnect" to a device that is about to vanish. So accept a device only
/// once the old one has been seen gone, or when its parent id (PID) differs.
fn is_new_configuration(previous: &str, old_left: bool, present: &[String]) -> bool {
    if old_left {
        !present.is_empty()
    } else {
        present.iter().any(|p| p != previous)
    }
}

fn wait_for_device(
    previous: &str,
    timeout_ms: u32,
    targets: &[UsbTarget],
    log: &LogFn,
) -> Result<Aoa, String> {
    let mut deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    let mut old_left = previous.is_empty();
    let mut install_requested = false;
    let mut last = String::from("平板尚未重新出现");
    while Instant::now() < deadline {
        let present = crate::usbdev::present_parents(targets);
        old_left |= !present.iter().any(|p| p == previous);
        if is_new_configuration(previous, old_left, &present) {
            match open(targets) {
                Ok(aoa) => return Ok(aoa),
                Err(e) => last = e,
            }
        } else if !install_requested
            && let [id, ..] = crate::usbhost::unbound_targets(targets).as_slice()
        {
            // First time in this USB mode on this PC: its interface has no
            // WinUSB yet. Install it now (UAC on the PC screen) and keep waiting.
            install_requested = true;
            log(format!(
                "… 救援: 新 USB 模式 {id} 还没有 WinUSB，正在请求管理员权限安装 —— 请在电脑上点“是”"
            ));
            match crate::usbhost::install_elevated(targets) {
                Ok(()) => deadline += INSTALL_GRACE,
                Err(e) => log(format!("✘ 救援: {e}")),
            }
        }
        thread::sleep(RECONNECT_POLL);
    }
    Err(format!("等待平板重新连接超时: {last}"))
}

/// A failure is reinterpreted as "the device left" (mode switch →
/// re-enumeration) when nothing but waits remains after the failing step:
/// e.g. the tap on 连接办公套件 itself, whose last reports race the
/// re-enumeration it triggers.
fn run_segment(
    aoa: &Aoa,
    steps: &[Step],
    rotation: &mut u8,
    skipped: &mut usize,
) -> Result<bool, String> {
    match run_steps(aoa, steps, rotation, skipped) {
        Ok(()) => Ok(false),
        Err((index, _)) if only_waits_after(steps, index) => Ok(true),
        Err((_, e)) => Err(e),
    }
}

fn only_waits_after(steps: &[Step], index: usize) -> bool {
    steps[index + 1..]
        .iter()
        .all(|s| matches!(s, Step::Wait(_)))
}

/// 1/10000 of the screen → fraction.
fn frac(v: u16) -> f32 {
    f32::from(v) / 10_000.0
}

/// Err((step index, error)).
fn run_steps(
    aoa: &Aoa,
    steps: &[Step],
    rotation: &mut u8,
    skipped: &mut usize,
) -> Result<(), (usize, String)> {
    let mut last_input = Instant::now();
    for (index, step) in steps.iter().enumerate() {
        let at = |e: String| (index, e);
        match step {
            Step::Key(m, k) => aoa.tap_key(*m, *k).map_err(at)?,
            Step::Text(t) => *skipped += aoa.type_text(t).map_err(at)?,
            Step::Consumer(u) => aoa.consumer(*u).map_err(at)?,
            Step::Swipe(dx, dy) => aoa.mouse_drag(*dx, *dy).map_err(at)?,
            Step::Tap(x, y) => aoa.tap(frac(*x), frac(*y), *rotation).map_err(at)?,
            Step::Hover(x, y) => aoa.hover(frac(*x), frac(*y), *rotation).map_err(at)?,
            Step::Drag { from, to, ms } => aoa
                .drag(
                    (frac(from.0), frac(from.1)),
                    (frac(to.0), frac(to.1)),
                    Duration::from_millis(u64::from(*ms)),
                    *rotation,
                )
                .map_err(at)?,
            Step::Rotation(r) => {
                *rotation = *r;
                continue;
            }
            Step::Reconnect(_) => unreachable!("segments are split at reconnect"),
            Step::Wait(ms) => {
                // Sleep in slices, tapping Shift so the screen never times out.
                let end = Instant::now() + Duration::from_millis(u64::from(*ms));
                while Instant::now() < end {
                    if last_input.elapsed() >= KEEP_AWAKE {
                        aoa.tap_key(aoa::MOD_SHIFT, 0).map_err(at)?;
                        last_input = Instant::now();
                    }
                    thread::sleep(Duration::from_millis(100).min(end - Instant::now()));
                }
                continue;
            }
        }
        last_input = Instant::now();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = r"USB\VID_2D95&PID_6012\SER0123456789AB";
    const NEW: &str = r"USB\VID_2D95&PID_6013\SER0123456789AB";

    /// The failure first seen on a second PC: right after the mode-switch tap the old 6012
    /// device is still enumerated; reconnecting to it fails moments later.
    #[test]
    fn reconnect_ignores_the_lingering_old_configuration() {
        assert!(!is_new_configuration(OLD, false, &[OLD.into()]));
        assert!(!is_new_configuration(OLD, false, &[]));
    }

    #[test]
    fn reconnect_accepts_a_new_pid_at_once() {
        assert!(is_new_configuration(OLD, false, &[NEW.into()]));
        assert!(is_new_configuration(OLD, false, &[OLD.into(), NEW.into()]));
    }

    #[test]
    fn reconnect_accepts_the_same_id_after_it_was_gone() {
        // Same PID after a real re-enumeration (e.g. a mode without a PID change).
        assert!(is_new_configuration(OLD, true, &[OLD.into()]));
        assert!(!is_new_configuration(OLD, true, &[]));
    }

    #[test]
    fn unlock_prefers_adb_then_a_single_usb_tablet() {
        let one = vec!["SER1".to_string()];
        let two = vec!["SER1".to_string(), "SER2".to_string()];
        assert_eq!(unlock_route(true, &one), UnlockRoute::Adb);
        assert_eq!(unlock_route(false, &one), UnlockRoute::Usb("SER1".into()));
        assert_eq!(
            unlock_route(false, &two),
            UnlockRoute::None,
            "never guess between tablets"
        );
        assert_eq!(unlock_route(false, &[]), UnlockRoute::None);
    }

    fn p(script: &str) -> Vec<Step> {
        parse(script, &[]).unwrap()
    }

    #[test]
    fn parses_named_keys_repeats_and_modifiers() {
        assert_eq!(p("enter"), vec![Step::Key(0, aoa::KEY_ENTER)]);
        assert_eq!(p("down*3").len(), 3);
        assert_eq!(p("meta+i"), vec![Step::Key(MOD_META, 0x0C)]);
        assert_eq!(
            p("ctrl+shift+tab"),
            vec![Step::Key(MOD_CTRL | aoa::MOD_SHIFT, aoa::KEY_TAB)]
        );
        assert_eq!(p("wake"), vec![Step::Key(aoa::MOD_SHIFT, 0)]);
        assert_eq!(p("home"), vec![Step::Consumer(aoa::USAGE_AC_HOME)]);
        assert_eq!(p("select"), vec![Step::Consumer(0x41)]);
        assert_eq!(p("kpenter"), vec![Step::Key(0, 0x58)]);
    }

    #[test]
    fn parses_text_swipe_wait_reconnect_and_comments() {
        let s = p("# comment\ntext:a*b c; swipe:0,-700; wait:250; pause; reconnect:5000");
        assert_eq!(
            s,
            vec![
                Step::Text("a*b c".into()),
                Step::Swipe(0, -700),
                Step::Wait(250),
                Step::Wait(STEP_PAUSE_MS),
                Step::Reconnect(5000)
            ]
        );
    }

    #[test]
    fn parses_pen_taps_and_rotation() {
        assert_eq!(p("click:0.41,0.61"), vec![Step::Tap(4100, 6100)]);
        assert_eq!(p("tap:0,1"), vec![Step::Tap(0, 10_000)]);
        assert_eq!(p("rot:0"), vec![Step::Rotation(0)]);
        assert!(parse("click:1.5,0.2", &[]).is_err());
        assert!(parse("click:0.5", &[]).is_err());
        assert!(parse("rot:4", &[]).is_err());
    }

    #[test]
    fn parses_pen_hover_for_aiming_taps() {
        assert_eq!(p("move:0.5,0.25"), vec![Step::Hover(5000, 2500)]);
        assert_eq!(p("hover:0,1"), vec![Step::Hover(0, 10_000)]);
        assert!(parse("move:0.5", &[]).is_err());
        assert!(parse("move:1.2,0.5", &[]).is_err());
    }

    #[test]
    fn parses_pen_drags_with_optional_duration() {
        assert_eq!(
            p("drag:0.1,0.9,0.1,0.2"),
            vec![Step::Drag {
                from: (1000, 9000),
                to: (1000, 2000),
                ms: DRAG_DEFAULT_MS
            }]
        );
        assert_eq!(
            p("drag:0.5,0.5,0.5,0.5,800"),
            vec![Step::Drag {
                from: (5000, 5000),
                to: (5000, 5000),
                ms: 800
            }],
            "same start and end = long press"
        );
        for bad in [
            "drag:0.1,0.2,0.3",
            "drag:0.1,0.2,0.3,1.5",
            "drag:0.1,0.2,0.3,0.4,0",
            "drag:0.1,0.2,0.3,0.4,99999",
        ] {
            assert!(parse(bad, &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn substitutes_variables() {
        let s = parse("text:{pw}; enter", &[("pw", "1234")]).unwrap();
        assert_eq!(s[0], Step::Text("1234".into()));
    }

    #[test]
    fn rejects_unknown_commands() {
        assert!(parse("frobnicate", &[]).is_err());
        assert!(parse("hyper+x", &[]).is_err());
        assert!(parse("swipe:1", &[]).is_err());
    }

    #[test]
    fn unlock_script_swipes_before_typing() {
        let unlock = parse(SCRIPT_UNLOCK, &[("pw", "1234")]).unwrap();
        let swipe = unlock
            .iter()
            .position(|x| matches!(x, Step::Swipe(..)))
            .unwrap();
        let text = unlock
            .iter()
            .position(|x| *x == Step::Text("1234".into()))
            .unwrap();
        assert!(swipe < text);
        assert_eq!(unlock[text + 1], Step::Key(0, aoa::KEY_ENTER));
        assert!(parse(SCRIPT_WAKE, &[]).is_ok());
    }

    #[test]
    fn enable_debugging_crosses_the_mode_switch_in_order() {
        let s = parse(SCRIPT_ENABLE_DEBUGGING, &[("pw", "1234")]).unwrap();
        let pos = |want: &Step| s.iter().position(|x| x == want).unwrap();
        let suite = pos(&Step::Tap(4100, 6100));
        let reconnect = s
            .iter()
            .position(|x| matches!(x, Step::Reconnect(_)))
            .unwrap();
        let notice = pos(&Step::Tap(5000, 5820));
        let always = pos(&Step::Tap(3625, 5250));
        let allow = pos(&Step::Tap(5000, 6010));
        assert!(pos(&Step::Text("1234".into())) < suite);
        assert!(suite < reconnect && reconnect < notice && notice < always && always < allow);
    }

    #[test]
    fn failures_followed_only_by_waits_mean_the_device_switched_mode() {
        let s = p("enter; click:0.41,0.61; wait:500; wait:100");
        assert!(
            only_waits_after(&s, 1),
            "tap failing as the tablet re-enumerates"
        );
        assert!(only_waits_after(&s, 3), "final wait");
        assert!(
            !only_waits_after(&s, 0),
            "a key with more input after it is a real failure"
        );
    }

    /// Exploration harness (run manually, watching the tablet):
    /// `$env:RESCUE_SCRIPT="wake; click:0.5,0.5"; cargo test rescue_script -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn rescue_script() {
        let script = std::env::var("RESCUE_SCRIPT").expect("set RESCUE_SCRIPT");
        let target = std::env::var("RESCUE_SERIAL").ok();
        let targets = crate::profiles::usb_targets(&crate::profiles::builtin());
        let aoa = Aoa::open(target.as_deref(), &targets).expect("open");
        // {pw} comes from the DPAPI store for this device — never printed.
        let store = crate::secrets::PasswordStore::load(
            crate::bundle::default_base().join("passwords.dat"),
        );
        let pw = store.get(&aoa.serial).ok().flatten().unwrap_or_default();
        println!(
            "saved password for {}: {}",
            aoa.serial,
            if pw.is_empty() { "none" } else { "found" }
        );
        let steps = parse(&script, &[("pw", &pw)]).expect("parse");
        drop(aoa);
        let log: LogFn = Arc::new(|l| println!("{l}"));
        let mut held = None;
        let outcome = run_workflow(&mut held, &steps, &targets, &log).expect("run");
        println!("ran {} steps ({outcome:?})", steps.len());
    }
}
