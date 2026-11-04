//! Rescue worker: owns the AOA connection on its own thread so the UI never
//! blocks on USB. The device is held only while commands are running and is
//! released after a short idle period, so MTP / adb can see the tablet again.

use crate::aoa::{self, Aoa};
use crate::usbid::UsbTarget;
use std::sync::atomic::{AtomicU64, Ordering};
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
// Workflows are data: `;`- or newline-separated commands, `*N` repeats,
// `#` comments, `{name}` substituted from the variables passed to `parse`.
// The command reference is `COMMANDS` (also rendered in the 说明 tab).

/// One documented script command.
pub struct CommandDoc {
    pub group: &'static str,
    pub syntax: &'static str,
    pub summary: &'static str,
    /// Parses (tested); shown and insertable in the guide.
    pub example: &'static str,
}

/// The script language reference — the single source for the in-app guide.
pub const COMMANDS: &[CommandDoc] = &[
    CommandDoc {
        group: "按键",
        syntax: "wake",
        summary: "唤醒屏幕：按一下 Shift，不输入任何字符",
        example: "wake",
    },
    CommandDoc {
        group: "按键",
        syntax: "enter · tab · space · esc · backspace · up · down · left · right",
        summary: "键盘按键（方向键可在对话框里移动焦点）",
        example: "tab; enter",
    },
    CommandDoc {
        group: "按键",
        syntax: "home · back · select · kpenter",
        summary: "主页、返回、确认（方向键中心键）、小键盘回车",
        example: "back",
    },
    CommandDoc {
        group: "按键",
        syntax: "ctrl+a · shift+tab · alt+… · meta+…",
        summary: "组合键：修饰键 + 按键或字母",
        example: "shift+tab",
    },
    CommandDoc {
        group: "按键",
        syntax: "a…z · 0…9",
        summary: "单个字母或数字键",
        example: "a",
    },
    CommandDoc {
        group: "文字",
        syntax: "text:内容",
        summary: "用 USB 键盘输入文字（只支持英文、数字、符号）；{pw} 替换为锁屏密码",
        example: "text:{pw}",
    },
    CommandDoc {
        group: "触控笔",
        syntax: "click:x,y",
        summary: "笔点按一下。x、y 是屏幕比例 0~1（0,0 左上角；0.5,0.5 正中）",
        example: "click:0.5,0.88",
    },
    CommandDoc {
        group: "触控笔",
        syntax: "move:x,y",
        summary: "笔悬停在该点但不点击——配合开发者选项「指针位置」对准坐标，确认后改成 click",
        example: "move:0.5,0.88",
    },
    CommandDoc {
        group: "触控笔",
        syntax: "drag:x1,y1,x2,y2[,毫秒]",
        summary: "按住从起点拖到终点（默认 300 毫秒，最长 10 秒）；起止相同 = 长按",
        example: "drag:0.5,0.9,0.5,0.3",
    },
    CommandDoc {
        group: "触控笔",
        syntax: "rot:0~3",
        summary: "之后的 click / move / drag 按哪个屏幕方向计算：0 竖屏，1 横屏（默认），2 倒竖，3 反横",
        example: "rot:0",
    },
    CommandDoc {
        group: "鼠标",
        syntax: "swipe:dx,dy",
        summary: "鼠标按住相对移动（受指针加速影响，适合上滑解锁这类粗略手势）",
        example: "swipe:0,-700",
    },
    CommandDoc {
        group: "时序",
        syntax: "wait:毫秒 · pause",
        summary: "等待（pause = 350 毫秒）；等待期间自动保持屏幕不灭",
        example: "wait:800",
    },
    CommandDoc {
        group: "时序",
        syntax: "reconnect:毫秒",
        summary: "设备切换 USB 模式（会断开重连）后，在这里等它重新出现再继续",
        example: "reconnect:20000",
    },
    CommandDoc {
        group: "结构",
        syntax: "命令*N · ; · 换行 · # 注释",
        summary: "*N 重复 N 次；命令用分号或换行分隔；# 开头的行是注释",
        example: "# 向下移动三次\ndown*3; enter",
    },
];

pub const SCRIPT_WAKE: &str = "wake; pause";
/// Wake → swipe the keyguard up → PIN → Enter.
pub const SCRIPT_UNLOCK: &str = "\
wake; wait:500
swipe:0,-700; wait:700
text:{pw}; enter; wait:800";

/// Full rescue, verified on the Pad3 (landscape) 2026-11-04 — screen and
/// USB debugging both unavailable at the start. Positions are fractions of
/// the landscape screen measured from device screenshots (2000×1406).
///  1. unlock with the saved PIN; the "USB 已连接" dialog appears
///  2. pen-tap the USB mode 连接办公套件 → the tablet itself turns USB
///     debugging on and re-enumerates (PID 6012 → 6013); no vivo software
///     needs to run on the PC (verified by the user 2026-11-04)
///  3. dismiss vivo's informational "需打开电脑的 vivo 办公套件" notice (知道了)
///  4. "允许 USB 调试吗？": tick 一律允许, tap 允许
///
/// One action per line, so the script editor can single-step it (the 说明
/// tab annotates each line: `guide::PAD3_ANNOTATED`).
pub const SCRIPT_ENABLE_DEBUGGING: &str = "\
wake; wait:500
swipe:0,-700; wait:700
text:{pw}; enter; wait:2500
rot:1
click:0.41,0.61; wait:500
reconnect:20000; wait:1500
click:0.5,0.582; wait:800
click:0.3625,0.525; wait:400
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

/// A step and the 1-based script line it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub step: Step,
    pub line: usize,
}

/// A script error at a 1-based line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "第 {} 行: {}", self.line, self.message)
    }
}

/// Parse a workflow script, substituting `{var}` placeholders; every step
/// keeps its source line (for the debugger's line marker and errors).
pub fn parse_located(script: &str, vars: &[(&str, &str)]) -> Result<Vec<Located>, ScriptError> {
    let mut steps = Vec::new();
    for (index, source_line) in script.lines().enumerate() {
        let line = index + 1;
        for raw in source_line.split(';') {
            let mut cmd = raw.trim().to_string();
            if cmd.is_empty() || cmd.starts_with('#') {
                continue;
            }
            let written = cmd.clone();
            for (name, value) in vars {
                cmd = cmd.replace(&format!("{{{name}}}"), value);
            }
            let (body, times) = split_repeat(&cmd);
            let step = parse_command(body).map_err(|_| ScriptError {
                line,
                // Never echo substituted values — `{pw}` is the lock-screen
                // PIN and errors end up in the log: describe the line as
                // written instead.
                message: match parse_command(split_repeat(&written).0) {
                    Err(message) => message,
                    Ok(_) => format!("代入变量后无效: {written}"),
                },
            })?;
            steps.extend(std::iter::repeat_n(Located { step, line }, times as usize));
        }
    }
    Ok(steps)
}

/// `cmd*N` → (cmd, N); `text:` keeps its payload verbatim (it may contain '*').
fn split_repeat(cmd: &str) -> (&str, u32) {
    match cmd.rsplit_once('*') {
        Some((body, n)) if !cmd.starts_with("text:") => match n.trim().parse::<u32>() {
            Ok(times) => (body.trim(), times),
            Err(_) => (cmd, 1),
        },
        _ => (cmd, 1),
    }
}

/// Stand-in for `{pw}` when only checking syntax: not a single key, so a
/// bare `{pw}` (instead of `text:{pw}`) is caught while editing.
pub const PW_PLACEHOLDER: &str = "0000";

/// [`parse_located`] without line numbers (errors still name the line).
pub fn parse(script: &str, vars: &[(&str, &str)]) -> Result<Vec<Step>, String> {
    parse_located(script, vars)
        .map(|steps| steps.into_iter().map(|l| l.step).collect())
        .map_err(|e| e.to_string())
}

/// Pen rotation when a run starts: a fresh run starts in the default
/// (landscape) orientation; a continued run (single-stepping) keeps what
/// earlier `rot:` steps set.
fn start_rotation(fresh: bool, held: u8) -> u8 {
    if fresh { DEFAULT_ROTATION } else { held }
}

/// Default display rotation for pen taps (1 = landscape, the Pad3's).
pub const DEFAULT_ROTATION: u8 = 1;

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
    /// Script line being executed (runs started from the script editor).
    pub line: Option<usize>,
    /// Pen rotation the next continued run starts with.
    pub rotation: u8,
}

/// One queued run.
struct Run {
    /// Shown in the log: "✔ 救援/<name>".
    name: String,
    steps: Vec<Step>,
    /// Source line per step (empty when not run from the editor).
    lines: Vec<usize>,
    /// Start with the default rotation (a whole script) or continue with
    /// the held device's rotation (single-stepping).
    fresh: bool,
    /// Stop epoch when queued; a later [`Rescue::stop`] cancels the run.
    epoch: u64,
}

enum Cmd {
    Run(Run),
    Release,
    /// The coordinate pad moved: hover at the latest [`Pointer`].
    Pointer,
}

/// Where the coordinate pad wants the pen to hover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// 1/10000 of the screen, as in `move:` / `click:`.
    pub x: u16,
    pub y: u16,
    /// The pad's rotation; the scripts' rotation state is not touched.
    pub rotation: u8,
    /// Take the device first if none is held (the 接管设备 button);
    /// plain moves never open it, so they cannot fail noisily.
    pub connect: bool,
}

/// The latest pointer request: a new one replaces an unsent one, so a
/// fast-moving mouse never builds a backlog.
#[derive(Default)]
struct PointerSlot(Mutex<Option<Pointer>>);

impl PointerSlot {
    fn put(&self, p: Pointer) {
        let mut slot = self.0.lock().unwrap();
        // A connect request survives being overtaken by a plain move.
        let connect = p.connect || slot.is_some_and(|old| old.connect);
        *slot = Some(Pointer { connect, ..p });
    }

    fn take(&self) -> Option<Pointer> {
        self.0.lock().unwrap().take()
    }
}

/// Error text of a run cancelled with [`Rescue::stop`].
pub const STOPPED: &str = "已停止";

pub struct Rescue {
    tx: Sender<Cmd>,
    pub status: Arc<Mutex<Status>>,
    /// USB functions the rescue may bind and open (from the profiles).
    targets: Arc<Mutex<Vec<UsbTarget>>>,
    /// Stop epoch: bumped by [`Rescue::stop`].
    stop: Arc<AtomicU64>,
    pointer: Arc<PointerSlot>,
}

/// What the worker thread shares with the UI.
struct Worker {
    status: Arc<Mutex<Status>>,
    targets: Arc<Mutex<Vec<UsbTarget>>>,
    stop: Arc<AtomicU64>,
    pointer: Arc<PointerSlot>,
    log: LogFn,
}

/// Has `epoch`'s run been cancelled?
fn cancelled(stop: &AtomicU64, epoch: u64) -> bool {
    stop.load(Ordering::Acquire) != epoch
}

impl Rescue {
    pub fn spawn(log: LogFn) -> Rescue {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status {
            rotation: DEFAULT_ROTATION,
            ..Status::default()
        }));
        let targets = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicU64::new(0));
        let pointer = Arc::new(PointerSlot::default());
        let shared = Worker {
            status: status.clone(),
            targets: targets.clone(),
            stop: stop.clone(),
            pointer: pointer.clone(),
            log,
        };
        thread::spawn(move || worker(rx, shared));
        Rescue {
            tx,
            status,
            targets,
            stop,
            pointer,
        }
    }

    /// Hover the pen at `p` (coordinate pad). Latest wins: calls faster than
    /// the device takes them collapse into the newest position.
    pub fn pointer(&self, p: Pointer) {
        self.pointer.put(p);
        let _ = self.tx.send(Cmd::Pointer);
    }

    /// Set after (re)loading the profiles.
    pub fn set_targets(&self, targets: Vec<UsbTarget>) {
        *self.targets.lock().unwrap() = targets;
    }

    pub fn run(&self, name: &str, steps: Vec<Step>) {
        let _ = self.tx.send(Cmd::Run(Run {
            name: name.to_string(),
            steps,
            lines: Vec::new(),
            fresh: true,
            epoch: self.stop.load(Ordering::Acquire),
        }));
    }

    /// Run steps from the script editor: the status reports the executing
    /// line; `fresh` = a whole script (default rotation), else continue
    /// where the previous single step left the pen rotation.
    pub fn run_located(&self, name: String, steps: Vec<Located>, fresh: bool) {
        let (steps, lines) = steps.into_iter().map(|l| (l.step, l.line)).unzip();
        let _ = self.tx.send(Cmd::Run(Run {
            name,
            steps,
            lines,
            fresh,
            epoch: self.stop.load(Ordering::Acquire),
        }));
    }

    /// Cancel the running sequence and everything queued before this call
    /// (at the next step, or within a wait). Runs queued later are not
    /// affected, so a stop can never swallow the next command.
    pub fn stop(&self) {
        self.stop.fetch_add(1, Ordering::AcqRel);
    }

    /// Parse and queue a workflow script (see the script syntax above).
    pub fn run_script(
        &self,
        name: &str,
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
    Err("WinUSB 已安装，但设备没有就绪：请重新插拔数据线后再试".into())
}

fn worker(rx: Receiver<Cmd>, w: Worker) {
    let log = &w.log;
    let mut held: Option<Aoa> = None;
    // Survives idle releases on purpose: single-stepping after a `rot:`
    // keeps that orientation however long the user looks at the device;
    // a fresh run resets it (`start_rotation`).
    let mut rotation = DEFAULT_ROTATION;
    // Device id of the last device held: a `reconnect` stepped on its own
    // (no device held) must still wait for the *new* configuration.
    let mut last_parent = String::new();
    let set = |device: &Option<Aoa>, busy: bool, rotation: u8| {
        let mut s = w.status.lock().unwrap();
        s.device = device.as_ref().map(|a| a.description.clone());
        s.serial = device.as_ref().map(|a| a.serial.clone());
        s.busy = busy;
        s.rotation = rotation;
        if !busy {
            s.line = None;
        }
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
            // Already sent by an earlier wake-up (latest wins).
            Cmd::Pointer => {
                let Some(p) = w.pointer.take() else { continue };
                if held.is_none() && p.connect {
                    let targets = w.targets.lock().unwrap().clone();
                    match connect(&targets, log) {
                        Ok(aoa) => {
                            log(format!("✔ 救援: 已接管 {}", aoa.description));
                            last_parent = aoa.parent().to_string();
                            held = Some(aoa);
                        }
                        Err(e) => {
                            w.status.lock().unwrap().needs_setup =
                                e.contains(crate::usbdev::NOT_FOUND);
                            log(format!("✘ 救援/接管: {e}"));
                        }
                    }
                }
                if let Some(aoa) = &held
                    && let Err(e) = aoa.hover(frac(p.x), frac(p.y), p.rotation)
                {
                    log(format!("✘ 救援/悬停: {e}"));
                    held = None; // gone or re-enumerated
                }
                set(&held, false, rotation);
            }
            Cmd::Release => {
                if held.take().is_some() {
                    log("救援: 已释放平板".into());
                }
                set(&held, false, rotation);
            }
            Cmd::Run(run) if cancelled(&w.stop, run.epoch) => {} // stopped while queued
            Cmd::Run(run) => {
                let name = run.name;
                rotation = start_rotation(run.fresh, rotation);
                set(&held, true, rotation);
                let targets = w.targets.lock().unwrap().clone();
                let hooks = Hooks {
                    cancelled: &|| cancelled(&w.stop, run.epoch),
                    on_step: &|index| {
                        w.status.lock().unwrap().line = run.lines.get(index).copied();
                    },
                };
                let mut device = DeviceState {
                    held: &mut held,
                    last_parent: &mut last_parent,
                };
                let result = run_workflow(
                    &mut device,
                    &run.steps,
                    &mut rotation,
                    &targets,
                    log,
                    &hooks,
                );
                match result {
                    Ok(o) => {
                        if o.skipped > 0 {
                            log(format!(
                                "✘ 救援/{name}: {} 个非 ASCII 字符无法用 USB 键盘输入",
                                o.skipped
                            ));
                        }
                        if o.device_left {
                            // Checked: the device stopped answering (USB mode
                            // switch). The next run reconnects.
                            log(format!(
                                "✔ 救援/{name}: 设备已切换 USB 模式（断开后会自动重新连接）"
                            ));
                            held = None;
                        } else if o.skipped == 0 && !name.is_empty() {
                            // (keyboard passthrough has no name: too chatty)
                            log(format!("✔ 救援/{name}"));
                        }
                    }
                    // The device is fine: keep holding it for the next run.
                    Err(e) if e == STOPPED => log(format!("■ 救援/{name}: 已停止")),
                    Err(e) => {
                        w.status.lock().unwrap().needs_setup = e.contains(crate::usbdev::NOT_FOUND);
                        log(format!("✘ 救援/{name}: {e}"));
                        held = None; // the device went away or re-enumerated
                    }
                }
                set(&held, false, rotation);
            }
        }
    }
}

/// Progress / cancellation callbacks of one run.
struct Hooks<'a> {
    cancelled: &'a dyn Fn() -> bool,
    /// Called before each step with its index in the run.
    on_step: &'a dyn Fn(usize),
}

impl Hooks<'_> {
    /// No progress, never cancelled (tests, the manual harness).
    #[cfg(test)]
    const NONE: Hooks<'static> = Hooks {
        cancelled: &|| false,
        on_step: &|_| {},
    };
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
/// The worker's device: the held connection, and the id of the last device
/// held (what a `reconnect` waits to see replaced, even when stepped alone).
struct DeviceState<'a> {
    held: &'a mut Option<Aoa>,
    last_parent: &'a mut String,
}

/// Device id a `reconnect` must see go away (or change): the held device,
/// else the last one held.
fn previous_parent(held: Option<&str>, last: &str) -> String {
    held.unwrap_or(last).to_string()
}

fn run_workflow(
    device: &mut DeviceState,
    steps: &[Step],
    rotation: &mut u8,
    targets: &[UsbTarget],
    log: &LogFn,
    hooks: &Hooks,
) -> Result<Outcome, String> {
    let held = &mut *device.held;
    let mut outcome = Outcome::default();
    // Index of the segment's first step in `steps` (for progress).
    let mut offset = 0;
    for segment in steps.split_inclusive(|s| matches!(s, Step::Reconnect(_))) {
        let (body, reconnect) = match segment.split_last() {
            Some((Step::Reconnect(ms), body)) => (body, Some(*ms)),
            _ => (segment, None),
        };
        if body.iter().all(|s| matches!(s, Step::Rotation(_))) {
            // Nothing to send (e.g. single-stepping a `rot:` line): no device needed.
            for (i, step) in body.iter().enumerate() {
                (hooks.on_step)(offset + i);
                if let Step::Rotation(r) = step {
                    *rotation = *r;
                }
            }
        } else {
            if held.is_none() {
                let aoa = connect(targets, log)?;
                log(format!("✔ 救援: 已接管 {}", aoa.description));
                *device.last_parent = aoa.parent().to_string();
                *held = Some(aoa);
            }
            let left = run_segment(
                held.as_ref().unwrap(),
                body,
                offset,
                rotation,
                &mut outcome.skipped,
                hooks,
            )?;
            outcome.device_left = left;
        }
        if let Some(ms) = reconnect {
            (hooks.on_step)(offset + body.len());
            let previous =
                previous_parent(held.take().as_ref().map(Aoa::parent), device.last_parent);
            let aoa = wait_for_device(&previous, ms, targets, log, hooks)?;
            log(format!("✔ 救援: 平板重新连接 ({})", aoa.parent()));
            *device.last_parent = aoa.parent().to_string();
            *held = Some(aoa);
            outcome.device_left = false;
        }
        offset += segment.len();
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
    hooks: &Hooks,
) -> Result<Aoa, String> {
    let mut deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    let mut old_left = previous.is_empty();
    let mut install_requested = false;
    let mut last = String::from("平板尚未重新出现");
    while Instant::now() < deadline {
        if (hooks.cancelled)() {
            return Err(STOPPED.into());
        }
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
    offset: usize,
    rotation: &mut u8,
    skipped: &mut usize,
    hooks: &Hooks,
) -> Result<bool, String> {
    match run_steps(aoa, steps, offset, rotation, skipped, hooks) {
        Ok(()) => Ok(false),
        // Only when the device really stopped answering: otherwise a failed
        // last step (any single step, key or passthrough key) would be
        // reported as a mode switch while nothing reached the device.
        Err((index, e)) if e != STOPPED && only_waits_after(steps, index) && !aoa.alive() => {
            Ok(true)
        }
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

/// Err((step index, error)); `offset` = index of `steps[0]` in the run.
fn run_steps(
    aoa: &Aoa,
    steps: &[Step],
    offset: usize,
    rotation: &mut u8,
    skipped: &mut usize,
    hooks: &Hooks,
) -> Result<(), (usize, String)> {
    let mut last_input = Instant::now();
    for (index, step) in steps.iter().enumerate() {
        let at = |e: String| (index, e);
        if (hooks.cancelled)() {
            return Err(at(STOPPED.into()));
        }
        (hooks.on_step)(offset + index);
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
                    if (hooks.cancelled)() {
                        return Err(at(STOPPED.into()));
                    }
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

    fn pointer(x: u16, connect: bool) -> Pointer {
        Pointer {
            x,
            y: 0,
            rotation: 1,
            connect,
        }
    }

    #[test]
    fn pointer_requests_collapse_to_the_latest() {
        let slot = PointerSlot::default();
        for x in [1, 2, 3] {
            slot.put(pointer(x, false));
        }
        assert_eq!(slot.take(), Some(pointer(3, false)));
        assert_eq!(slot.take(), None, "taken once");
    }

    /// Device check for the coordinate pad: take the plugged-in device and
    /// hover (never tap) at a few points through the pointer channel.
    /// `DROIDRESCUE_USB_TARGETS="2D95:6013:00,2D95:6013:01" cargo test
    /// device_pointer -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn device_pointer_takes_the_device_and_hovers() {
        let list = std::env::var("DROIDRESCUE_USB_TARGETS").expect("targets");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let rescue = Rescue::spawn(Arc::new(move |l: String| {
            eprintln!("{l}");
            sink.lock().unwrap().push(l);
        }));
        rescue.set_targets(UsbTarget::parse_list(&list).expect("valid targets"));
        let wait_idle = || {
            let deadline = Instant::now() + Duration::from_secs(15);
            thread::sleep(Duration::from_millis(300));
            while Instant::now() < deadline && rescue.status.lock().unwrap().device.is_none() {
                thread::sleep(Duration::from_millis(100));
            }
        };
        rescue.pointer(Pointer {
            x: 5000,
            y: 5000,
            rotation: 0,
            connect: true,
        });
        wait_idle();
        assert!(
            rescue.status.lock().unwrap().device.is_some(),
            "not taken: {:?}",
            lines.lock().unwrap()
        );
        // A burst like a fast mouse: collapses, never errors.
        for i in 0..=50u16 {
            rescue.pointer(Pointer {
                x: 2000 + i * 120,
                y: 8800,
                rotation: 0,
                connect: false,
            });
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(500));
        let log = lines.lock().unwrap();
        assert!(!log.iter().any(|l| l.starts_with('✘')), "{log:?}");
        assert!(rescue.status.lock().unwrap().device.is_some());
    }

    #[test]
    fn a_connect_request_survives_later_moves() {
        let slot = PointerSlot::default();
        slot.put(pointer(1, true));
        slot.put(pointer(2, false));
        assert_eq!(slot.take(), Some(pointer(2, true)));
    }

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

    /// Errors end up in the log: they must never contain a substituted PIN.
    #[test]
    fn script_errors_never_reveal_substituted_values() {
        let secret = "735919";
        for script in ["wake; {pw}; enter", "rot:{pw}", "click:{pw},0.5"] {
            let err = parse(script, &[("pw", secret)]).unwrap_err();
            assert!(!err.contains(secret), "{script}: {err}");
            assert!(err.contains("{pw}"), "describes the line as written: {err}");
        }
        assert!(
            parse("wake; {pw}", &[("pw", PW_PLACEHOLDER)]).is_err(),
            "a bare {{pw}} is caught by validation"
        );
        parse("text:{pw}; enter", &[("pw", PW_PLACEHOLDER)]).unwrap();
    }

    #[test]
    fn steps_remember_their_source_line() {
        let script = "# setup\nwake; wait:100\n\nclick:0.5,0.5\nenter*2";
        let located = parse_located(script, &[]).unwrap();
        let lines: Vec<usize> = located.iter().map(|l| l.line).collect();
        assert_eq!(lines, [2, 2, 4, 5, 5]);
        assert_eq!(located[2].step, Step::Tap(5000, 5000));
    }

    #[test]
    fn errors_name_the_line() {
        let err = parse_located("wake\nwait:100\nclick:2,0", &[]).unwrap_err();
        assert_eq!(err.line, 3);
        assert!(err.to_string().starts_with("第 3 行"), "{err}");
        assert!(
            parse("wake\nnonsense", &[])
                .unwrap_err()
                .contains("第 2 行"),
            "plain parse reports it too"
        );
    }

    #[test]
    fn every_documented_example_parses() {
        for doc in COMMANDS {
            parse(doc.example, &[("pw", "1234")])
                .unwrap_or_else(|e| panic!("{} example {:?}: {e}", doc.syntax, doc.example));
        }
        // Each step kind is documented at least once.
        let documented: Vec<Step> = COMMANDS
            .iter()
            .flat_map(|d| parse(d.example, &[("pw", "1")]).unwrap())
            .collect();
        let has = |f: fn(&Step) -> bool| documented.iter().any(f);
        assert!(has(|s| matches!(s, Step::Key(..))));
        assert!(has(|s| matches!(s, Step::Text(_))));
        assert!(has(|s| matches!(s, Step::Consumer(_))));
        assert!(has(|s| matches!(s, Step::Swipe(..))));
        assert!(has(|s| matches!(s, Step::Tap(..))));
        assert!(has(|s| matches!(s, Step::Hover(..))));
        assert!(has(|s| matches!(s, Step::Drag { .. })));
        assert!(has(|s| matches!(s, Step::Rotation(_))));
        assert!(has(|s| matches!(s, Step::Wait(_))));
        assert!(has(|s| matches!(s, Step::Reconnect(_))));
    }

    #[test]
    fn stop_cancels_earlier_runs_only() {
        let stop = AtomicU64::new(0);
        let queued_before = stop.load(Ordering::Acquire);
        stop.fetch_add(1, Ordering::AcqRel); // Rescue::stop
        let queued_after = stop.load(Ordering::Acquire);
        assert!(cancelled(&stop, queued_before));
        assert!(
            !cancelled(&stop, queued_after),
            "a stop never swallows the next command"
        );
    }

    #[test]
    fn rotation_only_runs_need_no_device() {
        let mut held = None;
        let mut rotation = DEFAULT_ROTATION;
        let lines = std::sync::Mutex::new(Vec::new());
        let hooks = Hooks {
            cancelled: &|| false,
            on_step: &|i| lines.lock().unwrap().push(i),
        };
        let log: LogFn = Arc::new(|_| {});
        let mut last_parent = String::new();
        let mut device = DeviceState {
            held: &mut held,
            last_parent: &mut last_parent,
        };
        let outcome = run_workflow(
            &mut device,
            &[Step::Rotation(0)],
            &mut rotation,
            &[],
            &log,
            &hooks,
        )
        .unwrap();
        assert_eq!(outcome, Outcome::default());
        assert_eq!(rotation, 0);
        assert_eq!(*lines.lock().unwrap(), [0]);
        assert!(held.is_none());
    }

    /// A `reconnect` single-stepped after the device dropped (nothing held)
    /// still waits for the configuration that replaces the last one.
    #[test]
    fn reconnect_without_a_held_device_waits_for_a_new_configuration() {
        assert_eq!(previous_parent(None, OLD), OLD);
        assert_eq!(previous_parent(Some(NEW), OLD), NEW);
        // …so the lingering old device is not taken as "reconnected":
        let previous = previous_parent(None, OLD);
        assert!(!is_new_configuration(&previous, false, &[OLD.into()]));
    }

    #[test]
    fn rotation_resets_for_fresh_runs_and_carries_over_for_steps() {
        assert_eq!(start_rotation(true, 0), DEFAULT_ROTATION);
        assert_eq!(start_rotation(false, 0), 0, "single-step keeps rot:0");
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
        let mut rotation = DEFAULT_ROTATION;
        let mut last_parent = String::new();
        let outcome = run_workflow(
            &mut DeviceState {
                held: &mut held,
                last_parent: &mut last_parent,
            },
            &steps,
            &mut rotation,
            &targets,
            &log,
            &Hooks::NONE,
        )
        .expect("run");
        println!("ran {} steps ({outcome:?})", steps.len());
    }
}
