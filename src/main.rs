#![cfg_attr(not(test), windows_subsystem = "windows")]

mod adb;
mod aoa;
mod audio;
mod bundle;
mod checkup;
#[cfg(windows)]
mod chrome;
#[cfg(target_os = "macos")]
#[path = "macos/chrome.rs"]
mod chrome;
mod control;
mod decoder;
#[cfg(windows)]
mod decoder_mf;
#[cfg(target_os = "macos")]
#[path = "macos/decoder_vt.rs"]
mod decoder_vt;
mod geometry;
mod input;
mod mirror;
mod profiles;
mod protocol;
mod rescue;
mod secrets;
mod skin;
#[cfg(target_os = "macos")]
#[path = "macos/surface.rs"]
mod surface;
mod theme;
#[cfg(windows)]
mod usbdev;
#[cfg(target_os = "macos")]
#[path = "macos/usbdev.rs"]
mod usbdev;
/// The rescue's USB host side: WinUSB binding on Windows, nothing on macOS.
#[cfg(target_os = "macos")]
#[path = "macos/usbhost.rs"]
mod usbhost;
mod usbid;
mod video;
mod widgets;
#[cfg(windows)]
mod winusb;
#[cfg(windows)]
use winusb as usbhost;

use adb::{Device, Tools};
use control::{KeyAction, MotionAction, POINTER_ID_FINGER, Position};
use eframe::egui::{
    self, Align, Align2, Layout, Margin, PointerButton, Rect, RichText, ViewportCommand, pos2, vec2,
};
use eframe::egui_wgpu::{self, wgpu};
use geometry::{ChromeLayout, Edges, Side};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use widgets::CaptionIcon;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_LOG_LINES: usize = 500;
const CAPTION_H: f32 = 40.0;
const RESIZE_BORDER: f32 = 6.0;
/// Control rail width (logical px).
const RAIL_W: f32 = 320.0;
const DEFAULT_SIZE: [f32; 2] = [340.0, 800.0];
/// The rail is laid out for `RAIL_W`; narrower windows clipped its widgets.
const MIN_SIZE: [f32; 2] = [RAIL_W, 520.0];
/// While mirroring, the stage keeps at least this much beside the rail.
const MIN_STAGE_W: f32 = 200.0;

/// Smallest window that shows the whole rail (and a usable stage).
fn min_window_size(mirroring: bool) -> egui::Vec2 {
    let stage = if mirroring { MIN_STAGE_W } else { 0.0 };
    vec2(MIN_SIZE[0] + stage, MIN_SIZE[1])
}
const KEY_HOME: u32 = 3;
/// Resize frame (old layout) + surface reconfigure + first correct present.
const UNCLOAK_AFTER_FRAMES: u8 = 3;

/// State written by background threads and read by the UI.
#[derive(Default)]
struct Shared {
    devices: Vec<Device>,
    selected: Option<String>,
    /// adb serial → hardware serial (`ro.serialno`), stable across USB/WiFi.
    device_ids: HashMap<String, String>,
    /// adb serial → highest panel refresh rate the device supports.
    device_hz: HashMap<String, f32>,
    /// adb serial → friendly name ("vivo Pad3" rather than "PA2455").
    friendly: HashMap<String, String>,
    awake: Option<bool>,
    locked: Option<bool>,
    /// Android display rotation 0..3.
    orientation: Option<u8>,
    /// Serials of tablets reachable for rescue (WinUSB interface present).
    usb_serials: Vec<String>,
    /// Every USB function present, driver or not: recognises a device by
    /// its profile's `usb` ids on a PC that never saw it over adb.
    usb_present: Vec<usbid::UsbTarget>,
    /// Rescue USB functions declared by the profiles (for the poller).
    usb_targets: Vec<usbid::UsbTarget>,
    tasks: Vec<String>,
    log: Vec<String>,
}

type SharedRef = Arc<Mutex<Shared>>;

/// Append-only diagnostics file: `%LOCALAPPDATA%\DroidRescue\droidrescue.log`.
fn log_file() -> &'static Mutex<Option<(std::fs::File, Instant)>> {
    static FILE: std::sync::OnceLock<Mutex<Option<(std::fs::File, Instant)>>> =
        std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let path = bundle::default_base().join("droidrescue.log");
        let _ = std::fs::create_dir_all(bundle::default_base());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        Mutex::new(file.map(|f| (f, Instant::now())))
    })
}

fn log(shared: &SharedRef, msg: impl Into<String>) {
    let msg = msg.into();
    if let Some((file, start)) = log_file().lock().unwrap().as_mut() {
        use std::io::Write as _;
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = writeln!(
            file,
            "[{unix} +{:>8.3}s] {msg}",
            start.elapsed().as_secs_f32()
        );
    }
    let mut s = shared.lock().unwrap();
    s.log.push(msg);
    let overflow = s.log.len().saturating_sub(MAX_LOG_LINES);
    s.log.drain(..overflow);
}

fn log_result(shared: &SharedRef, what: &str, result: Result<String, String>) {
    match result {
        Ok(out) if out.is_empty() => log(shared, format!("✔ {what}")),
        Ok(out) => log(shared, format!("✔ {what}: {out}")),
        Err(e) => log(shared, format!("✘ {what}: {e}")),
    }
}

/// `<Documents>\DroidRescue 备份` — never a machine-specific drive letter.
fn default_backup_dir() -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join("Documents")
        .join("DroidRescue 备份")
        .display()
        .to_string()
}

/// Window / taskbar / Dock icon (rendered from assets/icon/droidrescue.svg
/// by scripts/make-icons.sh; the exe and .app icons are separate files).
fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/droidrescue-256.png"))
        .expect("embedded icon is a valid PNG")
}

/// Window decorations. Windows: none — egui paints the caption and frame,
/// chrome.rs hit-tests them natively. macOS: the native window with a
/// transparent full-size title bar, so the traffic lights, resizing,
/// shadow and rounded corners stay native and our caption shows through.
fn native_chrome(builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
    if cfg!(target_os = "macos") {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
    } else {
        builder.with_decorations(false)
    }
}

/// Room at the caption's left for the macOS traffic lights.
const CAPTION_INSET: f32 = if cfg!(target_os = "macos") { 72.0 } else { 0.0 };
/// The caption draws its own minimise / maximise / close buttons (Windows).
const OWN_CAPTION_BUTTONS: bool = !cfg!(target_os = "macos");

/// Notepad on Windows, the default text editor (TextEdit) on macOS.
fn open_in_text_editor(path: &std::path::Path) -> std::io::Result<std::process::Child> {
    if cfg!(windows) {
        std::process::Command::new("notepad.exe").arg(path).spawn()
    } else {
        std::process::Command::new("open")
            .arg("-t")
            .arg(path)
            .spawn()
    }
}

/// Rescue profiles; writes a commented starter file on first run so users
/// can find and extend it.
fn load_profiles(shared: &SharedRef) -> Vec<profiles::Profile> {
    let path = profiles::path();
    if !path.exists() {
        let _ = std::fs::create_dir_all(bundle::default_base());
        if let Err(e) = std::fs::write(&path, profiles::template()) {
            log(
                shared,
                format!("✘ 无法创建机型配置 {}: {e}", path.display()),
            );
        }
    }
    let (list, error) = profiles::load(&path);
    if let Some(e) = error {
        log(shared, format!("✘ 机型配置有误，已只用内置配置: {e}"));
    }
    list
}

/// Background loop: device list, lock/screen state, rotation.
fn spawn_poller(tools: Tools, shared: SharedRef, ctx: egui::Context) {
    thread::spawn(move || {
        loop {
            let devices = tools
                .adb(None, &["devices", "-l"])
                .map(|o| adb::parse_devices(&String::from_utf8_lossy(&o.stdout)))
                .unwrap_or_default();

            let serial = {
                let mut s = shared.lock().unwrap();
                let still_there = s
                    .selected
                    .as_ref()
                    .is_some_and(|sel| devices.iter().any(|d| &d.serial == sel && d.is_ready()));
                if !still_there {
                    s.selected = devices
                        .iter()
                        .find(|d| d.is_ready())
                        .map(|d| d.serial.clone());
                }
                s.devices = devices;
                s.selected.clone()
            };

            if let Some(ser) = serial.as_deref()
                && !shared.lock().unwrap().device_ids.contains_key(ser)
                && let Ok(hw) = tools.shell(Some(ser), "getprop ro.serialno")
            {
                let id = if hw.is_empty() { ser.to_string() } else { hw };
                shared
                    .lock()
                    .unwrap()
                    .device_ids
                    .insert(ser.to_string(), id);
            }

            if let Some(ser) = serial.as_deref()
                && !shared.lock().unwrap().device_hz.contains_key(ser)
                && let Some(hz) = tools
                    .shell(Some(ser), "dumpsys display | grep -oE 'fps=[0-9.]+'")
                    .ok()
                    .and_then(|o| adb::parse_max_refresh(&o))
            {
                shared.lock().unwrap().device_hz.insert(ser.to_string(), hz);
            }

            if let Some(ser) = serial.as_deref()
                && !shared.lock().unwrap().friendly.contains_key(ser)
                && let Ok(out) = tools.shell(Some(ser), adb::FRIENDLY_NAME_CMD)
            {
                let mut s = shared.lock().unwrap();
                let model = s
                    .devices
                    .iter()
                    .find(|d| d.serial == ser)
                    .map(|d| d.model.clone())
                    .unwrap_or_default();
                s.friendly
                    .insert(ser.to_string(), adb::friendly_name(&out, &model));
            }

            let (awake, locked, orientation) = match serial.as_deref() {
                Some(ser) => (
                    tools
                        .shell(Some(ser), "dumpsys power | grep mWakefulness=")
                        .ok()
                        .and_then(|o| adb::parse_awake(&o)),
                    tools
                        .shell(Some(ser), "dumpsys window | grep isKeyguardShowing")
                        .ok()
                        .and_then(|o| adb::parse_keyguard_showing(&o)),
                    tools
                        .shell(Some(ser), "dumpsys input | grep -m1 'Viewport INTERNAL'")
                        .ok()
                        .and_then(|o| protocol::parse_orientation(&o)),
                ),
                None => (None, None, None),
            };
            let targets = shared.lock().unwrap().usb_targets.clone();
            let usb_serials = usbdev::present_serials(&targets);
            let usb_present = usbhost::survey()
                .iter()
                .filter_map(usbid::UsbEntry::target)
                .collect();
            {
                let mut s = shared.lock().unwrap();
                s.awake = awake;
                s.locked = locked;
                s.orientation = orientation;
                s.usb_serials = usb_serials;
                s.usb_present = usb_present;
            }
            ctx.request_repaint();
            thread::sleep(POLL_INTERVAL);
        }
    });
}

/// Per-frame copy of the shared state (lock held only while copying).
struct Snapshot {
    devices: Vec<Device>,
    selected: Option<String>,
    device_id: Option<String>,
    friendly: HashMap<String, String>,
    awake: Option<bool>,
    locked: Option<bool>,
    orientation: Option<u8>,
    usb_serials: Vec<String>,
    usb_present: Vec<usbid::UsbTarget>,
    tasks: Vec<String>,
    log: Vec<String>,
}

impl Snapshot {
    /// Friendly name once known, otherwise the adb model.
    fn name_of(&self, d: &Device) -> String {
        self.friendly
            .get(&d.serial)
            .cloned()
            .unwrap_or_else(|| d.model.clone())
    }

    /// adb model code of the selected device, e.g. `PA2455`.
    fn selected_model(&self) -> Option<&str> {
        let sel = self.selected.as_deref()?;
        self.devices
            .iter()
            .find(|d| d.serial == sel)
            .map(|d| d.model.as_str())
    }
}

enum MirrorState {
    Idle,
    Connecting(mpsc::Receiver<Result<mirror::Session, String>>),
    Running(Box<mirror::Session>),
}

/// Frames-per-second meter fed from the session's frame counter.
#[derive(Default)]
struct FpsMeter {
    last: Option<(Instant, u64, u64)>,
    fps: f32,
    mbps: f32,
}

impl FpsMeter {
    fn update(&mut self, frames: u64, bytes: u64) {
        let now = Instant::now();
        match self.last {
            Some((t, f, b)) if now.duration_since(t) >= Duration::from_millis(500) => {
                let dt = now.duration_since(t).as_secs_f32();
                self.fps = (frames - f) as f32 / dt;
                self.mbps = (bytes - b) as f32 * 8.0 / dt / 1_000_000.0;
                self.last = Some((now, frames, bytes));
            }
            None => self.last = Some((now, frames, bytes)),
            _ => {}
        }
    }
}

/// Pointer interaction with the drawn device.
#[derive(Default)]
struct StageInput {
    touching: bool,
    /// Physical key held with the mouse, and since when.
    pressed_key: Option<(skin::Part, Instant)>,
    /// Keyboard goes to the device after clicking its screen.
    focused: bool,
}

struct PadApp {
    tools: Option<Tools>,
    shared: SharedRef,
    mirror: MirrorState,
    opts: mirror::Options,
    password: String,
    backup_dir: String,
    side: Side,
    was_mirroring: bool,
    /// Skin extent (mm, rotated) the window was last fitted to.
    last_extent: Option<egui::Vec2>,
    stay_on: bool,
    passwords: secrets::PasswordStore,
    remember_password: bool,
    fps: FpsMeter,
    stage: StageInput,
    show_hud: bool,
    hwnd: isize,
    /// "60 fps · 平板 144 Hz · 显示器 60 Hz" for the HUD.
    capture_info: String,
    /// Render adapter LUID when zero-copy (DX12 + NV12) is available.
    gpu_luid: Option<i64>,
    /// Frames to render while cloaked after a resize before showing again.
    uncloak_in: u8,
    /// AOA HID worker: keyboard/mouse into the tablet without USB debugging.
    rescue: rescue::Rescue,
    rescue_text: String,
    /// Forward this PC's keyboard to the tablet as a USB keyboard.
    rescue_passthrough: bool,
    rescue_script_text: String,
    /// Rescue profiles (built-in + user file) and the user's explicit pick
    /// (None = automatic by model).
    profiles: Vec<profiles::Profile>,
    profile_choice: Option<String>,
    /// USB serial → adb model, learnt whenever adb sees a device.
    known_models: profiles::KnownModels,
    /// Last 设备体检 scan (taken on demand, not every frame).
    checkup: Option<Vec<checkup::Row>>,
}

impl PadApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        let gpu_luid = cc.wgpu_render_state.as_ref().and_then(|rs| {
            video::install(rs);
            video::zero_copy_adapter(rs)
        });
        let hwnd = cc
            .window_handle()
            .ok()
            .and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
                _ => None,
            })
            .unwrap_or(0);
        if hwnd != 0 {
            chrome::install(hwnd);
        }

        let shared: SharedRef = Arc::default();
        let shared_for_rescue = shared.clone();
        let profile_list = load_profiles(&shared);
        shared.lock().unwrap().usb_targets = profiles::usb_targets(&profile_list);
        let tools = match Tools::locate() {
            Ok(t) => {
                log(
                    &shared,
                    format!("server {} · {}", t.server_version, t.dir.display()),
                );
                spawn_poller(t.clone(), shared.clone(), cc.egui_ctx.clone());
                Some(t)
            }
            Err(e) => {
                log(&shared, format!("✘ {e}"));
                None
            }
        };
        Self {
            tools,
            shared,
            mirror: MirrorState::Idle,
            opts: mirror::Options::default(),
            password: String::new(),
            backup_dir: default_backup_dir(),
            side: Side::Right,
            // Treated as "just stopped mirroring" on the first frame, so a size
            // persisted while mirroring (wide) snaps back to the rail size;
            // the persisted position is kept.
            was_mirroring: true,
            last_extent: None,
            stay_on: false,
            passwords: secrets::PasswordStore::load(bundle::default_base().join("passwords.dat")),
            remember_password: true,
            fps: FpsMeter::default(),
            stage: StageInput::default(),
            show_hud: true,
            hwnd,
            capture_info: String::new(),
            gpu_luid,
            uncloak_in: 0,
            rescue: {
                let shared = shared_for_rescue;
                let rescue = rescue::Rescue::spawn(Arc::new(move |line| log(&shared, line)));
                rescue.set_targets(profiles::usb_targets(&profile_list));
                rescue
            },
            rescue_text: String::new(),
            rescue_passthrough: false,
            rescue_script_text: String::new(),
            profiles: profile_list,
            checkup: None,
            profile_choice: None,
            known_models: profiles::KnownModels::load(profiles::known_models_path()),
        }
    }

    /// The rescue profile in effect: the user's pick, else by the model last
    /// seen (via adb) for the tablet on USB, else the selected adb device's,
    /// else by a USB function a profile declares, else the generic one.
    fn profile(&self, snap: &Snapshot) -> Option<&profiles::Profile> {
        if let Some(name) = &self.profile_choice
            && let Some(p) = self.profiles.iter().find(|p| &p.name == name)
        {
            return Some(p);
        }
        let usb_model = match snap.usb_serials.as_slice() {
            [one] => self.known_models.get(one),
            _ => None,
        };
        usb_model
            .or(snap.selected_model())
            .and_then(|m| profiles::by_model(&self.profiles, m))
            .or_else(|| profiles::for_usb(&self.profiles, &snap.usb_present))
            .or_else(|| profiles::pick(&self.profiles, None))
    }

    /// (Re)load the profiles and hand their USB functions to the rescue.
    fn reload_profiles(&mut self) {
        self.profiles = load_profiles(&self.shared);
        let targets = profiles::usb_targets(&self.profiles);
        self.shared.lock().unwrap().usb_targets = targets.clone();
        self.rescue.set_targets(targets);
    }

    fn snapshot(&self) -> Snapshot {
        let s = self.shared.lock().unwrap();
        Snapshot {
            devices: s.devices.clone(),
            selected: s.selected.clone(),
            device_id: s
                .selected
                .as_ref()
                .and_then(|ser| s.device_ids.get(ser).cloned()),
            friendly: s.friendly.clone(),
            awake: s.awake,
            locked: s.locked,
            orientation: s.orientation,
            usb_serials: s.usb_serials.clone(),
            usb_present: s.usb_present.clone(),
            tasks: s.tasks.clone(),
            log: s.log.clone(),
        }
    }

    fn selected(&self) -> Option<String> {
        self.shared.lock().unwrap().selected.clone()
    }

    fn session(&self) -> Option<&mirror::Session> {
        match &self.mirror {
            MirrorState::Running(s) => Some(s),
            _ => None,
        }
    }

    fn mirroring(&self) -> bool {
        !matches!(self.mirror, MirrorState::Idle)
    }

    /// Run `job` on a worker thread, tracked in the task list.
    fn run_task(
        &self,
        name: &str,
        job: impl FnOnce(&Tools, Option<&str>, &SharedRef) + Send + 'static,
    ) {
        let Some(tools) = self.tools.clone() else {
            return;
        };
        let shared = self.shared.clone();
        let serial = self.selected();
        let name = name.to_string();
        shared.lock().unwrap().tasks.push(name.clone());
        thread::spawn(move || {
            job(&tools, serial.as_deref(), &shared);
            let mut s = shared.lock().unwrap();
            if let Some(i) = s.tasks.iter().position(|t| *t == name) {
                s.tasks.remove(i);
            }
        });
    }

    fn shell_task(&self, name: &'static str, cmd: String) {
        self.run_task(name, move |t, ser, sh| {
            log_result(sh, name, t.shell(ser, &cmd))
        });
    }

    /// Instant via the control socket while mirroring, adb shell otherwise.
    fn press_key(&self, keycode: u32) {
        match self.session() {
            Some(s) => s.press_key(keycode),
            None => self.shell_task("按键", format!("input keyevent {keycode}")),
        }
    }

    /// Hardware key held with the mouse. While mirroring, DOWN goes out now and
    /// UP on release, so Android's InputDispatcher synthesises repeats and
    /// FLAG_LONG_PRESS exactly as for a real held key (power menu, volume
    /// ramp). Over adb shell a hold can't be streamed, so the release decides
    /// between a tap and `--longpress`.
    fn key_down(&self, keycode: u32) {
        if let Some(s) = self.session() {
            s.send(&control::keycode(KeyAction::Down, keycode, 0, 0));
        }
    }

    fn key_up(&self, keycode: u32, held: Duration) {
        match self.session() {
            Some(s) => s.send(&control::keycode(KeyAction::Up, keycode, 0, 0)),
            None => self.shell_task("按键", adb::keyevent_cmd(keycode, held)),
        }
    }

    fn start_mirror(&mut self, ctx: &egui::Context) {
        let Some(tools) = self.tools.clone() else {
            return;
        };
        let serial = self.selected();
        let mut opts = self.opts.clone();
        // Capture at most min(panel, monitor) fps: extra frames would only
        // cost encode time and bandwidth without ever being shown.
        let device_hz = serial
            .as_ref()
            .and_then(|s| self.shared.lock().unwrap().device_hz.get(s).copied());
        let monitor_hz = (self.hwnd != 0)
            .then(|| chrome::monitor_refresh_hz(self.hwnd))
            .flatten();
        opts.max_fps = adb::capture_fps(device_hz, monitor_hz, self.opts.max_fps);
        let hz = |v: Option<String>| v.unwrap_or_else(|| "?".into());
        self.capture_info = format!(
            "cap {} · pad {} Hz · monitor {} Hz",
            if opts.max_fps == 0 {
                "∞".to_string()
            } else {
                format!("{} fps", opts.max_fps)
            },
            hz(device_hz.map(|h| format!("{h:.0}"))),
            hz(monitor_hz.map(|h| h.to_string())),
        );
        log(&self.shared, format!("帧率策略: {}", self.capture_info));
        let shared = self.shared.clone();
        let log_fn: mirror::LogFn = Arc::new(move |line| log(&shared, line));
        let c = ctx.clone();
        let repaint: mirror::RepaintFn = Arc::new(move || c.request_repaint());
        let (tx, rx) = mpsc::channel();
        let gpu_luid = self.gpu_luid;
        log(&self.shared, "▶ 正在连接 scrcpy-server …");
        thread::spawn(move || {
            let _ = tx.send(mirror::Session::start(
                &tools,
                serial.as_deref(),
                &opts,
                gpu_luid,
                log_fn,
                repaint,
            ));
        });
        self.mirror = MirrorState::Connecting(rx);
        self.fps = FpsMeter::default();
    }

    fn stop_mirror(&mut self) {
        if !matches!(self.mirror, MirrorState::Idle) {
            self.mirror = MirrorState::Idle; // drops the session: sockets, server, tunnel
            log(&self.shared, "■ 投屏已停止");
        }
    }

    /// Advance Connecting → Running / Idle and notice ended sessions.
    fn poll_mirror(&mut self) {
        let next = match &self.mirror {
            MirrorState::Connecting(rx) => match rx.try_recv() {
                Ok(Ok(session)) => {
                    let name = session.shared.device_name.lock().unwrap().clone();
                    log(&self.shared, format!("✔ 已连接 {name}"));
                    Some(MirrorState::Running(Box::new(session)))
                }
                Ok(Err(e)) => {
                    log(&self.shared, format!("✘ 投屏失败: {e}"));
                    Some(MirrorState::Idle)
                }
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(MirrorState::Idle),
            },
            MirrorState::Running(s) => s.shared.ended.lock().unwrap().clone().map(|reason| {
                log(&self.shared, format!("■ {reason}"));
                MirrorState::Idle
            }),
            MirrorState::Idle => None,
        };
        if let Some(n) = next {
            self.mirror = n;
        }
    }

    /// Typed password wins (and is saved if "remember" is on); an empty field
    /// falls back to the password saved for this device.
    fn unlock(&mut self, device_id: Option<&str>, route: &rescue::UnlockRoute, usb_script: &str) {
        let typed = std::mem::take(&mut self.password);
        let password = if !typed.is_empty() {
            if self.remember_password
                && let Some(id) = device_id
            {
                match self.passwords.set(id, &typed) {
                    Ok(()) => log(
                        &self.shared,
                        format!("✔ 已保存 {id} 的锁屏密码（DPAPI 加密）"),
                    ),
                    Err(e) => log(&self.shared, format!("✘ 保存密码失败: {e}")),
                }
            }
            typed
        } else {
            match device_id.map(|id| self.passwords.get(id)) {
                Some(Ok(Some(saved))) => saved,
                Some(Err(e)) => return log(&self.shared, format!("✘ 读取已保存密码失败: {e}")),
                _ => return,
            }
        };
        match route {
            rescue::UnlockRoute::Adb => self.send_unlock(&password),
            rescue::UnlockRoute::Usb(_) => {
                self.rescue_script("解锁", usb_script, &[("pw", &password)])
            }
            rescue::UnlockRoute::None => {}
        }
    }

    fn forget_password(&mut self, device_id: &str) {
        match self.passwords.remove(device_id) {
            Ok(()) => log(&self.shared, format!("✔ 已删除 {device_id} 的已保存密码")),
            Err(e) => log(&self.shared, format!("✘ 删除已保存密码失败: {e}")),
        }
    }

    fn send_unlock(&self, password: &str) {
        let text = adb::quote_input_text(password);
        self.run_task("解锁", move |t, ser, sh| {
            let steps = [
                ("唤醒", "input keyevent 224".to_string()),
                ("上滑", adb::SWIPE_UP_CMD.to_string()),
                ("输入密码", format!("input text {text}")),
                ("回车", "input keyevent 66".to_string()),
            ];
            for (name, cmd) in steps {
                if let Err(e) = t.shell(ser, &cmd) {
                    // Never echo the command: it may contain the password.
                    log(
                        sh,
                        format!("✘ 解锁/{name} 失败: {}", e.replace(&text, "***")),
                    );
                    return;
                }
                thread::sleep(Duration::from_millis(600));
            }
            log(sh, "✔ 已发送解锁序列");
        });
    }

    fn enable_wireless(&self) {
        self.run_task("无线调试", |t, ser, sh| {
            let ip = match t.shell(ser, "ip -f inet addr show wlan0") {
                Ok(out) => adb::parse_wlan_ip(&out),
                Err(e) => return log(sh, format!("✘ 获取 IP 失败: {e}")),
            };
            let Some(ip) = ip else {
                return log(sh, "✘ 平板没连 WiFi（wlan0 没有 IP）");
            };
            log_result(sh, "adb tcpip 5555", run_adb(t, ser, &["tcpip", "5555"]));
            thread::sleep(Duration::from_secs(2));
            let target = format!("{ip}:5555");
            log_result(
                sh,
                &format!("adb connect {target}"),
                run_adb(t, None, &["connect", &target]),
            );
            log(sh, "拔掉 USB 后在设备列表里选 IP 那一项即可无线投屏");
        });
    }

    fn backup(&self) {
        let dest = self.backup_dir.clone();
        self.run_task("备份", move |t, ser, sh| {
            if let Err(e) = std::fs::create_dir_all(&dest) {
                return log(sh, format!("✘ 无法创建 {dest}: {e}"));
            }
            for dir in [
                "DCIM",
                "Pictures",
                "Download",
                "Documents",
                "Movies",
                "Music",
            ] {
                log(sh, format!("… 导出 /sdcard/{dir}"));
                let src = format!("/sdcard/{dir}");
                log_result(
                    sh,
                    &format!("导出 {dir}"),
                    run_adb(t, ser, &["pull", "-a", &src, &dest]),
                );
            }
            log(sh, format!("✔ 备份完成 → {dest}"));
        });
    }

    // ───────────────────────── chrome ─────────────────────────

    /// Draws the title bar; returns the left x of the caption controls.
    fn caption(&mut self, ui: &mut egui::Ui, snap: &Snapshot, maximized: bool) -> f32 {
        let (rect, drag) = ui.allocate_exact_size(
            vec2(ui.available_width(), CAPTION_H),
            egui::Sense::click_and_drag(),
        );
        // On Windows the caption is non-client (chrome.rs), so these events
        // only arrive on macOS: drag moves the window, double-click zooms.
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }
        let p = ui.painter();
        p.rect_filled(rect, 0.0, theme::c().surface);
        p.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            egui::Stroke::new(1.0, theme::c().line),
        );

        let y = rect.center().y;
        let mark = p.text(
            pos2(rect.left() + 14.0 + CAPTION_INSET, y),
            Align2::LEFT_CENTER,
            "安卓救援宝",
            theme::strong_font(15.0),
            theme::c().amber,
        );
        // The English name lives in the OS window title (taskbar, Alt-Tab):
        // the 340 px rail has no room for both next to the status.
        let sub = mark;

        let ready = snap.selected.is_some();
        let unauthorized = snap.devices.iter().any(|d| d.state == "unauthorized");
        let live = self.session().is_some();
        let lamp = if live {
            theme::c().amber
        } else if ready {
            theme::c().green
        } else if unauthorized {
            theme::c().red
        } else {
            theme::c().text_faint
        };
        let lamp_c = pos2(sub.right() + 14.0, y);
        p.circle_filled(lamp_c, 6.0, lamp.gamma_multiply(0.18));
        p.circle_filled(lamp_c, 3.0, lamp);
        let status = if let Some(task) = snap.tasks.first() {
            format!("{task}…")
        } else if matches!(self.mirror, MirrorState::Connecting(_)) {
            "CONNECTING…".into()
        } else if let Some(d) = snap
            .devices
            .iter()
            .find(|d| Some(&d.serial) == snap.selected.as_ref())
        {
            let name = snap.name_of(d);
            if live {
                format!("{name} · LIVE")
            } else {
                name
            }
        } else {
            "OFFLINE".into()
        };
        let color = if snap.tasks.is_empty() && !live {
            theme::c().text_dim
        } else {
            theme::c().amber
        };
        p.text(
            pos2(lamp_c.x + 10.0, y),
            Align2::LEFT_CENTER,
            status,
            theme::mono(10.5),
            color,
        );

        let mut controls_left = rect.right();
        let builder = egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center));
        ui.scope_builder(builder, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let mut button = |ui: &mut egui::Ui, icon, tip: &str| {
                let r = widgets::caption_button(ui, icon, CAPTION_H).on_hover_text(tip);
                controls_left = controls_left.min(r.rect.left());
                r
            };
            if OWN_CAPTION_BUTTONS {
                if button(ui, CaptionIcon::Close, "关闭").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
                let (icon, tip) = if maximized {
                    (CaptionIcon::Restore, "还原")
                } else {
                    (CaptionIcon::Maximize, "最大化")
                };
                if button(ui, icon, tip).clicked() {
                    ui.ctx()
                        .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                if button(ui, CaptionIcon::Minimize, "最小化").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                }
            }
            if self.mirroring() && button(ui, CaptionIcon::Swap, "控制栏换边").clicked() {
                self.side = self.side.flipped();
            }
        });
        controls_left
    }

    // ───────────────────────── stage ─────────────────────────

    fn stage(&mut self, ui: &mut egui::Ui, rect: Rect, snap: &Snapshot) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, theme::c().well);
        // Faint instrument grid.
        let step = 24.0;
        let mut x = rect.left() + step / 2.0;
        while x < rect.right() {
            let mut y = rect.top() + step / 2.0;
            while y < rect.bottom() {
                p.circle_filled(pos2(x, y), 0.8, theme::c().line);
                y += step;
            }
            x += step;
        }

        let session_shared = self.session().map(|s| s.shared.clone());
        let video_size = session_shared
            .as_ref()
            .map_or((0, 0), |s| *s.video_size.lock().unwrap());
        let landscape = (video_size.0 > 0).then_some(video_size.0 >= video_size.1);
        let model = skin::Model::for_device(snap.selected_model(), video_size, snap.orientation);
        let turns = skin::turns_for(&model, snap.orientation, landscape);
        self.last_extent = Some(model.extent(turns));
        let layout = skin::layout(rect.shrink(8.0), &model, turns);
        // Where the picture actually goes: aspect-fit, never stretched.
        let picture = skin::fit(layout.screen, video_size);
        let has_video = session_shared
            .as_ref()
            .is_some_and(|s| s.frame_seq.load(Ordering::Acquire) > 0);

        // Physical keys.
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hot_key = pointer.and_then(|pos| layout.key_at(pos));
        for (part, key_rect) in &layout.keys {
            let resp = ui
                .interact(
                    key_rect.expand(0.8 * layout.scale),
                    ui.id().with(("key", *part as u8)),
                    egui::Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(part.label());
            if resp.is_pointer_button_down_on() && self.stage.pressed_key.is_none() {
                self.stage.pressed_key = Some((*part, Instant::now()));
                self.key_down(part.keycode());
            }
        }
        if !ui.input(|i| i.pointer.primary_down())
            && let Some((part, since)) = self.stage.pressed_key.take()
        {
            self.key_up(part.keycode(), since.elapsed());
        }

        skin::paint::back(
            &p,
            &layout,
            hot_key,
            self.stage.pressed_key.map(|(part, _)| part),
            has_video,
        );
        if let Some(shared) = &session_shared
            && has_video
        {
            p.add(video::VideoCallback::shape(picture, shared.clone()));
        }
        skin::paint::front(&p, &layout);

        if !has_video {
            let msg = match self.mirror {
                MirrorState::Connecting(_) => "CONNECTING …",
                _ => "NO SIGNAL",
            };
            p.text(
                layout.screen.center(),
                Align2::CENTER_CENTER,
                msg,
                theme::mono(13.0),
                theme::c().text_faint,
            );
        }

        if let MirrorState::Running(session) = &self.mirror {
            let screen_resp = ui.interact(
                picture,
                ui.id().with("screen"),
                egui::Sense::click_and_drag(),
            );
            if has_video {
                forward_input(
                    ui,
                    &screen_resp,
                    picture,
                    video_size,
                    session,
                    &mut self.stage,
                );
            }
            if self.show_hud {
                hud(&p, rect, session, video_size, &self.fps, &self.capture_info);
            }
        }
    }
    // ───────────────────────── rail ─────────────────────────

    fn rail(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ready = snap.selected.is_some() && self.tools.is_some();
        let gap = ui.spacing().item_spacing.x;

        // 01 LINK
        widgets::section(ui, "01", "LINK", "连接");
        if snap.devices.len() > 1 {
            let label = snap.selected.clone().unwrap_or_else(|| "选择设备".into());
            egui::ComboBox::from_id_salt("device")
                .width(ui.available_width())
                .truncate()
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for d in &snap.devices {
                        let text = format!("{}  {}  [{}]", snap.name_of(d), d.serial, d.state);
                        let is_sel = snap.selected.as_deref() == Some(d.serial.as_str());
                        if ui
                            .add_enabled(d.is_ready(), egui::Button::selectable(is_sel, text))
                            .clicked()
                        {
                            self.shared.lock().unwrap().selected = Some(d.serial.clone());
                        }
                    }
                });
        }
        if let Some(d) = snap.devices.iter().find(|d| d.state == "unauthorized") {
            ui.label(
                RichText::new(format!("{} 未授权 · 需在平板上允许 USB 调试", d.serial))
                    .color(theme::c().red)
                    .small(),
            );
        }
        let cell = (ui.available_width() - gap * 2.0) / 3.0;
        ui.horizontal(|ui| {
            let link = match snap.selected.as_deref() {
                Some(s) if s.contains(':') => ("WIFI", theme::c().green),
                Some(_) => ("USB", theme::c().green),
                None => ("—", theme::c().text_faint),
            };
            widgets::readout(ui, cell, "LINK", link.0, link.1);
            let screen = match snap.awake {
                Some(true) => ("亮屏", theme::c().green),
                Some(false) => ("熄屏", theme::c().amber),
                None => ("—", theme::c().text_faint),
            };
            widgets::readout(ui, cell, "SCREEN", screen.0, screen.1);
            let lock = match snap.locked {
                Some(true) => ("已锁", theme::c().amber),
                Some(false) => ("解锁", theme::c().green),
                None => ("—", theme::c().text_faint),
            };
            widgets::readout(ui, cell, "LOCK", lock.0, lock.1);
        });

        // 02 MIRROR
        widgets::section(ui, "02", "MIRROR", "投屏");
        if self.mirroring() {
            if widgets::danger(ui, "■  停止投屏").clicked() {
                self.stop_mirror();
            }
        } else if widgets::primary(ui, "▶  开始投屏", ready).clicked() {
            self.start_mirror(ui.ctx());
        }
        let idle = !self.mirroring();
        egui::CollapsingHeader::new(RichText::new("投屏参数").color(theme::c().text_dim))
            .id_salt("mirror-opts")
            .show(ui, |ui| {
                ui.add_enabled_ui(idle, |ui| {
                    combo_row(
                        ui,
                        "最大分辨率",
                        "size",
                        &mut self.opts.max_size,
                        &[1280, 1600, 1920, 2400, 2800],
                        |v| format!("{v}p"),
                    );
                    combo_row(
                        ui,
                        "码率",
                        "bitrate",
                        &mut self.opts.bit_rate,
                        &[4_000_000, 8_000_000, 12_000_000, 20_000_000, 32_000_000],
                        |v| format!("{} Mbps", v / 1_000_000),
                    );
                    combo_row(
                        ui,
                        "帧率上限",
                        "fps",
                        &mut self.opts.max_fps,
                        &[0, 30, 60, 90, 120, 144],
                        |v| {
                            if v == 0 {
                                "自动".into()
                            } else {
                                format!("{v} fps")
                            }
                        },
                    );
                    widgets::switch(ui, &mut self.opts.audio, "声音转到电脑（平板静音）");
                    widgets::switch(ui, &mut self.opts.stay_awake, "保持唤醒");
                    widgets::switch(ui, &mut self.opts.turn_screen_off, "投屏时关闭平板屏幕");
                });
                widgets::switch(ui, &mut self.show_hud, "显示延迟 / 帧率");
            });

        // 03 KEYS
        widgets::section(ui, "03", "KEYS", "按键");
        let w3 = (ui.available_width() - gap * 2.0) / 3.0;
        ui.horizontal(|ui| {
            for (glyph, label, code) in [("◁", "返回", 4), ("○", "主页", 3), ("▢", "多任务", 187)]
            {
                if widgets::key(ui, glyph, label, vec2(w3, 52.0), ready).clicked() {
                    self.press_key(code);
                }
            }
        });
        let w4 = (ui.available_width() - gap * 3.0) / 4.0;
        ui.horizontal(|ui| {
            for (glyph, label, code) in [
                ("PWR", "电源", 26),
                ("VOL−", "音量", 25),
                ("VOL+", "音量", 24),
                ("WAKE", "唤醒", 224),
            ] {
                if widgets::key(ui, glyph, label, vec2(w4, 48.0), ready).clicked() {
                    self.press_key(code);
                }
            }
        });
        let live = self.session().is_some();
        let screen_off = self
            .session()
            .is_some_and(|s| s.shared.screen_off.load(Ordering::Relaxed));
        ui.horizontal(|ui| {
            let (glyph, label) = if screen_off {
                ("SCR ON", "点亮平板屏幕")
            } else {
                ("SCR OFF", "熄灭平板屏幕")
            };
            if widgets::key(ui, glyph, label, vec2(w3, 44.0), live).clicked()
                && let Some(s) = self.session()
            {
                s.set_screen_off(!screen_off);
            }
            if widgets::key(ui, "NOTIF", "通知栏", vec2(w3, 44.0), live).clicked()
                && let Some(s) = self.session()
            {
                s.send(&control::expand_notification_panel());
            }
            if widgets::key(ui, "ROTATE", "旋转屏幕", vec2(w3, 44.0), live).clicked()
                && let Some(s) = self.session()
            {
                s.send(&control::rotate_device());
            }
        });

        // 04 UNLOCK
        // Works without adb: falls back to the rescue USB keyboard, and the
        // password can be typed before anything is connected.
        widgets::section(ui, "04", "UNLOCK", "盲输解锁");
        let route = rescue::unlock_route(ready, &snap.usb_serials);
        let device_id = match &route {
            rescue::UnlockRoute::Adb => snap.device_id.clone(),
            rescue::UnlockRoute::Usb(serial) => Some(serial.clone()),
            rescue::UnlockRoute::None => None,
        };
        let device_id = device_id.as_deref();
        let saved = device_id.is_some_and(|id| self.passwords.contains(id));
        ui.horizontal(|ui| {
            let field_w = ui.available_width() - 72.0 - gap;
            let hint = match (&route, saved) {
                (_, true) => "已保存 · 留空直接解锁",
                (rescue::UnlockRoute::None, _) => "锁屏密码（可先输入，插上后使用）",
                _ => "锁屏密码",
            };
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .hint_text(hint)
                    .desired_width(field_w)
                    .margin(Margin::symmetric(8, 6)),
            );
            let can_unlock =
                route != rescue::UnlockRoute::None && (saved || !self.password.is_empty());
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked = widgets::key(ui, "ENTER", "解锁", vec2(72.0, 34.0), can_unlock).clicked();
            if (clicked || enter) && can_unlock {
                let script = self
                    .profile(snap)
                    .map_or(rescue::SCRIPT_UNLOCK.to_string(), |p| p.unlock.clone());
                self.unlock(device_id, &route, &script);
            }
        });
        let via = match &route {
            rescue::UnlockRoute::Adb => "经 adb 输入",
            rescue::UnlockRoute::Usb(_) => "经 USB 模拟键盘（无需调试）",
            rescue::UnlockRoute::None if snap.usb_serials.len() > 1 => {
                "检测到多台平板，请只连接一台"
            }
            rescue::UnlockRoute::None => "未连接设备",
        };
        ui.label(
            RichText::new(via)
                .font(theme::mono(10.0))
                .color(theme::c().text_faint),
        );
        ui.horizontal(|ui| {
            widgets::switch_inline(ui, &mut self.remember_password, "记住此设备的密码");
            if saved && let Some(id) = device_id {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::link(ui, "忘记密码").clicked() {
                        self.forget_password(id);
                    }
                    ui.label(
                        RichText::new("● 已保存")
                            .font(theme::mono(10.5))
                            .color(theme::c().green),
                    );
                });
            }
        });

        // 05 TOOLS
        widgets::section(ui, "05", "TOOLS", "工具");
        // Appearance works without a device; egui persists the choice.
        let mut preference = ui.ctx().options(|o| o.theme_preference);
        if widgets::segmented(
            ui,
            &mut preference,
            &[
                (egui::ThemePreference::System, "跟随系统"),
                (egui::ThemePreference::Light, "亮色"),
                (egui::ThemePreference::Dark, "暗色"),
            ],
        ) {
            ui.ctx().set_theme(preference);
        }
        ui.add_enabled_ui(ready, |ui| {
            if widgets::switch(ui, &mut self.stay_on, "插电常亮（不锁屏）").changed() {
                let value = if self.stay_on { 7 } else { 0 };
                self.shell_task(
                    "插电常亮",
                    format!("settings put global stay_on_while_plugged_in {value}"),
                );
            }
            let w2 = (ui.available_width() - gap) / 2.0;
            ui.horizontal(|ui| {
                if widgets::key(ui, "WIFI", "开启无线 ADB", vec2(w2, 48.0), ready).clicked() {
                    self.enable_wireless();
                }
                if widgets::key(ui, "SAVE", "导出照片/文件", vec2(w2, 48.0), ready).clicked()
                {
                    self.backup();
                }
            });
            ui.add(
                egui::TextEdit::singleline(&mut self.backup_dir)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::symmetric(8, 6))
                    .font(theme::mono(11.0)),
            );
        });

        // 06 RESCUE
        self.rescue_section(ui, snap);

        // 07 LOG
        widgets::section(ui, "07", "LOG", "日志");
        egui::Frame::NONE
            .fill(theme::c().well)
            .stroke(egui::Stroke::new(1.0, theme::c().line))
            .corner_radius(theme::RADIUS)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for line in &snap.log {
                            let color = if line.starts_with('✔') {
                                theme::c().green
                            } else if line.starts_with('✘') {
                                theme::c().red
                            } else {
                                theme::c().text_dim
                            };
                            ui.label(RichText::new(line).font(theme::mono(10.5)).color(color));
                        }
                    });
            });
        ui.add_space(8.0);
    }

    /// Rescue mode: drive the tablet as a USB keyboard / pen (AOA HID) —
    /// works with USB debugging off, on the lock screen, with no screen.
    fn rescue_section(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        widgets::section(ui, "06", "RESCUE", "救援 · 无需屏幕和调试");
        let gap = ui.spacing().item_spacing.x;
        let status = self.rescue.status.lock().unwrap().clone();
        let (text, color) = match (&status.device, status.busy) {
            (Some(d), true) => (format!("● {d} · 执行中…"), theme::c().amber),
            (Some(d), false) => (format!("● {d}"), theme::c().green),
            (None, true) => ("连接平板中…".to_string(), theme::c().amber),
            (None, false) => ("插上平板即可操作".to_string(), theme::c().text_dim),
        };
        ui.label(RichText::new(text).font(theme::mono(10.5)).color(color));
        if status.needs_setup {
            self.winusb_setup_link(ui);
        }

        // Per-model workflows (profiles.rs): automatic by model, or picked.
        let profile = self.profile(snap).cloned();
        let auto = self.profile_choice.is_none();
        ui.horizontal(|ui| {
            ui.label(RichText::new("机型").color(theme::c().text_dim));
            let shown = match (&profile, auto) {
                (Some(p), true) => format!("自动 · {}", p.name),
                (Some(p), false) => p.name.clone(),
                (None, _) => "—".into(),
            };
            // Truncate: in a horizontal row the default (Extend) lets a long
            // profile name push the whole rail wider than the window.
            egui::ComboBox::from_id_salt("rescue-profile")
                .width(ui.available_width())
                .truncate()
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.profile_choice, None, "自动（按型号）");
                    for p in &self.profiles {
                        let label = if p.models.is_empty() {
                            p.name.clone()
                        } else {
                            format!("{}  [{}]", p.name, p.models.join(", "))
                        };
                        ui.selectable_value(&mut self.profile_choice, Some(p.name.clone()), label);
                    }
                });
        });

        // Pad3: unlock → USB mode 连接办公套件 (the tablet itself turns USB
        // debugging on; no PC software needed — verified by the user) →
        // 知道了 → 一律允许 → 允许. Ends with an authorised adb device.
        let enable = profile.as_ref().and_then(|p| p.enable.clone());
        if widgets::primary(ui, "一键救援：解锁并开启 USB 调试", !status.busy && enable.is_some())
            .on_hover_text(if enable.is_some() {
                "按所选机型的流程盲操作：解锁 → 选 USB 模式 → 允许 USB 调试。无需在电脑上安装其他软件，完成后可直接投屏。"
            } else {
                "该机型还没有“开启调试”流程：可在机型配置文件里添加 enable = …"
            })
            .clicked()
            && let Some(script) = &enable
        {
            match self.rescue_password(snap, status.serial.as_deref()) {
                Some(pw) => self.rescue_script("开启调试", script, &[("pw", &pw)]),
                None => log(&self.shared, "✘ 救援: 没有锁屏密码，请先在 04 输入"),
            }
        }

        let unlock = profile
            .as_ref()
            .map_or(rescue::SCRIPT_UNLOCK.to_string(), |p| p.unlock.clone());
        let w2 = (ui.available_width() - gap) / 2.0;
        ui.horizontal(|ui| {
            if widgets::key(ui, "WAKE", "唤醒", vec2(w2, 44.0), !status.busy).clicked() {
                self.rescue_script("唤醒", rescue::SCRIPT_WAKE, &[]);
            }
            if widgets::key(ui, "UNLOCK", "解锁", vec2(w2, 44.0), !status.busy).clicked() {
                match self.rescue_password(snap, status.serial.as_deref()) {
                    Some(pw) => self.rescue_script("解锁", &unlock, &[("pw", &pw)]),
                    None => log(&self.shared, "✘ 救援: 没有锁屏密码，请先在 04 输入"),
                }
            }
        });

        egui::CollapsingHeader::new(RichText::new("高级").color(theme::c().text_dim))
            .id_salt("rescue-advanced")
            .show(ui, |ui| {
                self.rescue_advanced(ui, snap, &status);
            });
    }

    fn rescue_advanced(&mut self, ui: &mut egui::Ui, snap: &Snapshot, status: &rescue::Status) {
        use rescue::Step;
        let gap = ui.spacing().item_spacing.x;
        let w4 = (ui.available_width() - gap * 3.0) / 4.0;
        let nav: [[(&str, &str, Step); 4]; 2] = [
            [
                ("↑", "上", Step::Key(0, aoa::KEY_UP)),
                ("↓", "下", Step::Key(0, aoa::KEY_DOWN)),
                ("←", "左", Step::Key(0, aoa::KEY_LEFT)),
                ("→", "右", Step::Key(0, aoa::KEY_RIGHT)),
            ],
            [
                ("TAB", "焦点", Step::Key(0, aoa::KEY_TAB)),
                ("ENTER", "确认", Step::Key(0, aoa::KEY_ENTER)),
                ("BACK", "返回", Step::Consumer(aoa::USAGE_AC_BACK)),
                ("HOME", "主页", Step::Consumer(aoa::USAGE_AC_HOME)),
            ],
        ];
        for row in nav {
            ui.horizontal(|ui| {
                for (glyph, label, step) in row {
                    if widgets::key(ui, glyph, label, vec2(w4, 40.0), !status.busy).clicked() {
                        self.rescue.run("按键", vec![step]);
                    }
                }
            });
        }

        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.rescue_text)
                    .hint_text("文字发送到平板（英文/数字/符号）")
                    .desired_width(ui.available_width() - 72.0 - gap)
                    .margin(Margin::symmetric(8, 6)),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (widgets::key(
                ui,
                "SEND",
                "发送",
                vec2(72.0, 34.0),
                !self.rescue_text.is_empty(),
            )
            .clicked()
                || enter)
                && !self.rescue_text.is_empty()
            {
                self.rescue.run(
                    "输入文字",
                    vec![Step::Text(std::mem::take(&mut self.rescue_text))],
                );
            }
        });
        widgets::switch(
            ui,
            &mut self.rescue_passthrough,
            "键盘直通（电脑键盘 → 平板）",
        );

        // Workflow script — for adapting to other vivo dialogs / layouts.
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.rescue_script_text)
                    .font(theme::mono(11.0))
                    .hint_text("脚本，如 wake; click:0.5,0.6")
                    .desired_width(ui.available_width() - 72.0 - gap)
                    .margin(Margin::symmetric(8, 6)),
            );
            if widgets::key(ui, "RUN", "运行", vec2(72.0, 34.0), !self.rescue_script_text.trim().is_empty())
                .on_hover_text("click:x,y（笔点按，屏幕比例）· move:x,y（笔悬停不点，配合开发者选项「指针位置」对准坐标）· drag:x1,y1,x2,y2[,ms]（按住拖动；起止相同=长按）· rot:0~3 · wait:ms · text:abc · enter · home · back · {pw}")
                .clicked()
            {
                let script = self.rescue_script_text.clone();
                let pw = self.rescue_password(snap, status.serial.as_deref()).unwrap_or_default();
                self.rescue_script("脚本", &script, &[("pw", &pw)]);
            }
        });
        ui.horizontal(|ui| {
            if widgets::link(ui, "编辑机型配置")
                .on_hover_text(profiles::path().display().to_string())
                .clicked()
                && let Err(e) = open_in_text_editor(&profiles::path())
            {
                log(&self.shared, format!("✘ 打开机型配置失败: {e}"));
            }
            if widgets::link(ui, "重新加载").clicked() {
                self.reload_profiles();
                log(
                    &self.shared,
                    format!("✔ 机型配置: {} 个", self.profiles.len()),
                );
            }
            if status.device.is_some() && widgets::link(ui, "释放平板").clicked() {
                self.rescue.release();
            }
        });
        self.winusb_setup_link(ui);
        egui::CollapsingHeader::new(RichText::new("设备体检").color(theme::c().text_dim))
            .id_salt("rescue-checkup")
            .show(ui, |ui| self.checkup_view(ui, snap));
    }

    /// USB functions as Windows sees them, and whether the rescue can use them.
    fn checkup_view(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let c = theme::c();
        let adb: Vec<String> = snap
            .devices
            .iter()
            .map(|d| format!("{} {} {}", d.serial, d.state, d.model))
            .collect();
        ui.horizontal(|ui| {
            if widgets::link(ui, "扫描").clicked() || self.checkup.is_none() {
                let targets = profiles::usb_targets(&self.profiles);
                self.checkup = Some(checkup::rows(&usbhost::survey(), &targets));
            }
            if let Some(rows) = &self.checkup
                && widgets::link(ui, "复制报告").clicked()
            {
                ui.ctx().copy_text(checkup::report(rows, &adb));
                log(&self.shared, "✔ 设备体检报告已复制");
            }
        });
        let Some(rows) = &self.checkup else { return };
        ui.label(RichText::new(checkup::summary(rows)).color(c.text_dim));
        for r in rows {
            let color = match r.role {
                checkup::Role::Ready => c.green,
                checkup::Role::NeedsDriver => c.amber,
                checkup::Role::Adb | checkup::Role::Mtp => c.text,
                checkup::Role::Other => c.text_faint,
            };
            let service = if r.service.is_empty() {
                "无驱动"
            } else {
                &r.service
            };
            ui.label(
                RichText::new(format!(
                    "{:<12} {:<4} {}",
                    r.target.to_string(),
                    checkup::role_label(r.role),
                    service
                ))
                .font(theme::mono(10.5))
                .color(color),
            )
            .on_hover_text(&r.description);
        }
        let adb_line = if adb.is_empty() {
            "adb: 无设备".to_string()
        } else {
            format!("adb: {}", adb.join(" · "))
        };
        ui.label(
            RichText::new(adb_line)
                .font(theme::mono(10.5))
                .color(c.text_dim),
        );
    }

    fn winusb_setup_link(&self, ui: &mut egui::Ui) {
        if widgets::link(ui, "为救援接口安装 WinUSB（需管理员）")
            .on_hover_text("使用 Windows 自带、微软签名的 WinUsb 驱动，只作用于机型配置里声明的救援接口（如 Pad3 的虚拟光驱接口）；MTP 文件传输不受影响。设备需插着。开始救援时也会自动请求。")
            .clicked()
        {
            match usbhost::install_elevated(&profiles::usb_targets(&self.profiles)) {
                Ok(()) => log(&self.shared, "… 已请求管理员权限安装 WinUSB，结果见日志"),
                Err(e) => log(&self.shared, format!("✘ {e}")),
            }
        }
    }

    fn rescue_script(&self, name: &'static str, script: &str, vars: &[(&str, &str)]) {
        if let Err(e) = self.rescue.run_script(name, script, vars) {
            log(&self.shared, format!("✘ 救援/{name}: 脚本错误: {e}"));
        }
    }

    /// Password for a blind unlock: typed in 04 > saved for this device (held
    /// / plugged-in USB serial, adb device) > the only saved password.
    fn rescue_password(&self, snap: &Snapshot, usb_serial: Option<&str>) -> Option<String> {
        if !self.password.is_empty() {
            return Some(self.password.clone());
        }
        let plugged = match snap.usb_serials.as_slice() {
            [one] => Some(one.as_str()),
            _ => None,
        };
        [
            usb_serial,
            plugged,
            snap.device_id.as_deref(),
            self.passwords.only_id().as_deref(),
        ]
        .into_iter()
        .flatten()
        .find_map(|id| self.passwords.get(id).ok().flatten())
    }

    /// Keyboard passthrough: this frame's typing → HID keyboard on the tablet.
    fn rescue_passthrough(&self, ctx: &egui::Context) {
        if !self.rescue_passthrough || ctx.egui_wants_keyboard_input() {
            return;
        }
        let steps: Vec<rescue::Step> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) => Some(rescue::Step::Text(t.clone())),
                    egui::Event::Key {
                        key, pressed: true, ..
                    } => input::hid_usage(*key).map(|u| rescue::Step::Key(0, u)),
                    _ => None,
                })
                .collect()
        });
        if !steps.is_empty() {
            self.rescue.run("", steps);
        }
    }

    /// Widen the window so the stage fits the device (`extent`, mm incl.
    /// margins, as drawn) at the current height.
    fn fit_window(&mut self, ctx: &egui::Context, extent: egui::Vec2) {
        let height = ctx
            .input(|i| i.viewport().inner_rect)
            .map_or(800.0, |r| r.height());
        // The stage is shrunk by 8 px on each side before the skin is laid out.
        let inner_h = height - CAPTION_H - 16.0;
        self.set_width(ctx, inner_h * extent.x / extent.y + 16.0 + RAIL_W);
    }

    /// Resize horizontally without moving the rail on screen (the stage opens
    /// or closes on the far side) — no jump under the user's cursor.
    fn set_width(&mut self, ctx: &egui::Context, width: f32) {
        if self.hwnd != 0 {
            let px = (width * ctx.pixels_per_point()).round() as i32;
            // Cloak across the resize: the swapchain still holds the old-size
            // frame (DWM would stretch it) and the next egui pass lays out with
            // the old size. Show again once correct frames have been presented.
            chrome::set_cloaked(self.hwnd, true);
            chrome::resize_width_keep_rail(self.hwnd, px, self.side);
            self.uncloak_in = UNCLOAK_AFTER_FRAMES;
            ctx.request_repaint();
        } else {
            let height = ctx
                .input(|i| i.viewport().inner_rect)
                .map_or(800.0, |r| r.height());
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(width, height)));
        }
    }
}

/// Mouse → touch / scroll / back / home; keyboard → keys + text.
fn forward_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    screen: Rect,
    size: (u32, u32),
    s: &mirror::Session,
    st: &mut StageInput,
) {
    let pos_of = |p: egui::Pos2| {
        let (x, y) = skin::screen_to_video(p, screen, size);
        Position {
            x,
            y,
            width: size.0 as u16,
            height: size.1 as u16,
        }
    };
    let hovered = resp.hovered();
    let (events, hover_pos) = ui.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    for ev in &events {
        match ev {
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                ..
            } => {
                let inside = screen.contains(*pos);
                if *pressed {
                    st.focused = inside;
                }
                match (button, pressed, inside) {
                    (PointerButton::Primary, true, true) => {
                        s.send(&control::touch(
                            MotionAction::Down,
                            POINTER_ID_FINGER,
                            pos_of(*pos),
                            1.0,
                            0,
                            0,
                        ));
                        st.touching = true;
                    }
                    (PointerButton::Primary, false, _) if st.touching => {
                        s.send(&control::touch(
                            MotionAction::Up,
                            POINTER_ID_FINGER,
                            pos_of(*pos),
                            0.0,
                            0,
                            0,
                        ));
                        st.touching = false;
                    }
                    (PointerButton::Secondary, true, true) => {
                        s.send(&control::back_or_screen_on(KeyAction::Down));
                        s.send(&control::back_or_screen_on(KeyAction::Up));
                    }
                    (PointerButton::Middle, true, true) => s.press_key(KEY_HOME),
                    _ => {}
                }
            }
            egui::Event::PointerMoved(pos) if st.touching => {
                s.send(&control::touch(
                    MotionAction::Move,
                    POINTER_ID_FINGER,
                    pos_of(*pos),
                    1.0,
                    0,
                    0,
                ));
            }
            egui::Event::MouseWheel { unit, delta, .. } if hovered => {
                if let Some(p) = hover_pos {
                    let n = input::wheel_notches(*unit, *delta);
                    s.send(&control::scroll(pos_of(p), n.x, n.y, 0));
                }
            }
            egui::Event::Text(t) if st.focused => s.send(&control::text(t)),
            egui::Event::Paste(t) if st.focused => s.paste(t),
            egui::Event::Key {
                key,
                pressed,
                repeat,
                ..
            } if st.focused => {
                if let Some(code) = input::android_keycode(*key) {
                    let action = if *pressed {
                        KeyAction::Down
                    } else {
                        KeyAction::Up
                    };
                    s.send(&control::keycode(action, code, u32::from(*repeat), 0));
                }
            }
            _ => {}
        }
    }
}

fn hud(
    p: &egui::Painter,
    stage: Rect,
    session: &mirror::Session,
    size: (u32, u32),
    fps: &FpsMeter,
    cap: &str,
) {
    let st = *session.shared.stats.lock().unwrap();
    let lines = [
        format!("{:>5.1} fps   {:>5.1} Mbps", fps.fps, fps.mbps),
        // to-frame: decoded → picked up by the next rendered frame.
        format!(
            "decode {:>4.1} (call {:>4.1}) ms  to-frame {:>4.1} ms",
            st.decode_ms, st.call_ms, st.present_ms
        ),
        format!(
            "{}×{}  H.264  {}",
            size.0,
            size.1,
            *session.shared.decoder.lock().unwrap()
        ),
        session
            .shared
            .decoder_detail
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default(),
        match *session.shared.audio_ms.lock().unwrap() {
            ms if ms >= 0.0 => format!("audio PCM 48k  buffer {ms:>4.0} ms"),
            _ => "audio off".to_string(),
        },
        cap.to_string(),
    ];
    // Bottom-left: keeps the device's top-edge keys and camera visible.
    let origin = stage.left_bottom() + vec2(12.0, -106.0);
    let bg = Rect::from_min_size(origin - vec2(6.0, 4.0), vec2(300.0, 104.0));
    p.rect_filled(bg, 3.0, egui::Color32::from_black_alpha(150));
    for (i, line) in lines.iter().enumerate() {
        // Overlay on video with a black scrim: always the dark palette.
        let color = if i == 0 {
            theme::DARK.amber
        } else {
            theme::DARK.text_dim
        };
        p.text(
            origin + vec2(0.0, i as f32 * 16.0),
            Align2::LEFT_TOP,
            line,
            theme::mono(11.0),
            color,
        );
    }
}

fn combo_row<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    label: &str,
    id: &str,
    value: &mut T,
    options: &[T],
    fmt: impl Fn(T) -> String,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(theme::c().text_dim));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            egui::ComboBox::from_id_salt(id)
                .width(110.0)
                .selected_text(fmt(*value))
                .show_ui(ui, |ui| {
                    for &opt in options {
                        ui.selectable_value(value, opt, fmt(opt));
                    }
                });
        });
    });
}

fn run_adb(t: &Tools, serial: Option<&str>, args: &[&str]) -> Result<String, String> {
    let out = t.adb(serial, args).map_err(|e| e.to_string())?;
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
    .trim()
    .lines()
    .last()
    .unwrap_or("")
    .to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

impl eframe::App for PadApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(500));
        theme::sync(&ctx);
        self.poll_mirror();
        self.rescue_passthrough(&ctx);
        if let Some(s) = self.session() {
            let st = *s.shared.stats.lock().unwrap();
            self.fps.update(st.frames, st.bytes);
        }

        if self.uncloak_in > 0 {
            self.uncloak_in -= 1;
            if self.uncloak_in == 0 {
                chrome::set_cloaked(self.hwnd, false);
            } else {
                ctx.request_repaint();
            }
        }

        let snap = self.snapshot();
        // USB serial == adb serial over USB; remember its model for rescue.
        for d in snap
            .devices
            .iter()
            .filter(|d| d.is_ready() && !d.serial.contains(':'))
        {
            if let Err(e) = self.known_models.remember(&d.serial, &d.model) {
                log(&self.shared, format!("✘ 记录设备型号失败: {e}"));
            }
        }
        let mirroring = self.mirroring();
        if mirroring != self.was_mirroring {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min_window_size(
                mirroring,
            )));
        }
        if mirroring && !self.was_mirroring {
            // No stream yet: best guess from the model and current rotation;
            // the stage refits once the first frame reveals the real shape.
            let model = skin::Model::for_device(snap.selected_model(), (0, 0), snap.orientation);
            let extent = model.extent(skin::turns_for(&model, snap.orientation, None));
            self.fit_window(&ctx, self.last_extent.unwrap_or(extent));
        } else if !mirroring && self.was_mirroring {
            self.set_width(&ctx, DEFAULT_SIZE[0]);
            self.last_extent = None;
        }
        self.was_mirroring = mirroring;

        let (maximized, focused) = ctx.input(|i| {
            (
                i.viewport().maximized.unwrap_or(false),
                i.viewport().focused.unwrap_or(true),
            )
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::c().bg))
            .show(root, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let controls_left = self.caption(ui, &snap, maximized);

                let ppp = ctx.pixels_per_point();
                chrome::set_layout(ChromeLayout {
                    caption_height: (CAPTION_H * ppp).round() as i32,
                    controls_left: (controls_left * ppp).round() as i32,
                    border: (RESIZE_BORDER * ppp).round() as i32,
                    resizable: if maximized { Edges::NONE } else { Edges::ALL },
                });

                let body = ui.available_rect_before_wrap();
                let rail_rect = if mirroring {
                    let (stage, rail) = match self.side {
                        Side::Right => {
                            let rail = Rect::from_min_max(
                                pos2(body.right() - RAIL_W, body.top()),
                                body.max,
                            );
                            (
                                Rect::from_min_max(body.min, pos2(rail.left(), body.bottom())),
                                rail,
                            )
                        }
                        Side::Left => {
                            let rail = Rect::from_min_max(
                                body.min,
                                pos2(body.left() + RAIL_W, body.bottom()),
                            );
                            (
                                Rect::from_min_max(pos2(rail.right(), body.top()), body.max),
                                rail,
                            )
                        }
                    };
                    let before = self.last_extent;
                    self.stage(ui, stage, &snap);
                    if let Some(extent) = self.last_extent
                        && before.is_some()
                        && before != self.last_extent
                    {
                        self.fit_window(&ctx, extent);
                    }
                    let edge_x = if self.side == Side::Right {
                        rail.left()
                    } else {
                        rail.right()
                    };
                    ui.painter().line_segment(
                        [pos2(edge_x, rail.top()), pos2(edge_x, rail.bottom())],
                        egui::Stroke::new(1.0, theme::c().line),
                    );
                    rail
                } else {
                    body
                };

                ui.scope_builder(egui::UiBuilder::new().max_rect(rail_rect), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("rail")
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            egui::Frame::NONE
                                .inner_margin(Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 8.0;
                                    self.rail(ui, &snap);
                                });
                        });
                });
                if OWN_CAPTION_BUTTONS {
                    // macOS draws its own window border.
                    widgets::window_frame(ui, focused);
                }
            });
    }

    fn on_exit(&mut self) {
        self.stop_mirror();
    }
}

/// DX12 on Windows (so decoder textures can be shared in), Metal on macOS;
/// NV12 sampling when the adapter supports it, otherwise video uses the
/// CPU (I420 upload) path.
fn zero_copy_wgpu_setup() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = if cfg!(windows) {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::METAL
    };
    let base = setup.device_descriptor.clone();
    setup.device_descriptor = Arc::new(move |adapter| {
        let mut desc = base(adapter);
        desc.required_features |= adapter.features() & wgpu::Features::TEXTURE_FORMAT_NV12;
        desc
    });
    egui_wgpu::WgpuSetup::CreateNew(setup)
}

/// Window title / product name (Chinese name first, English name for search).
const APP_TITLE: &str = "安卓救援宝 DroidRescue";

fn main() -> eframe::Result {
    // Before anything reads the data dir (log, PINs, window state).
    let migrated = bundle::migrate_legacy_data();

    // Elevated helper mode (started by the rescue via UAC): bind the in-box
    // WinUSB driver to the given rescue functions, log the outcome, exit.
    if std::env::args().nth(1).as_deref() == Some(usbhost::INSTALL_FLAG) {
        let shared: SharedRef = Arc::default();
        let targets = usbid::UsbTarget::parse_list(&std::env::args().nth(2).unwrap_or_default());
        match targets.and_then(|t| usbhost::install(&t)) {
            Ok(ids) => log(&shared, format!("✔ WinUSB 已安装到 {}", ids.join(", "))),
            Err(e) => log(&shared, format!("✘ WinUSB 安装失败: {e}")),
        }
        return Ok(());
    }

    let window_state = bundle::default_base().join("window.ron");
    // eframe's `centered` overrides a persisted position, so only centre on
    // the very first launch (no saved state yet).
    let first_launch = !window_state.exists();
    let options = eframe::NativeOptions {
        viewport: native_chrome(
            egui::ViewportBuilder::default()
                .with_title(APP_TITLE)
                .with_icon(app_icon())
                .with_inner_size(DEFAULT_SIZE)
                .with_min_inner_size(MIN_SIZE),
        ),
        // Remember position/size across launches; centre on first launch.
        persist_window: true,
        persistence_path: Some(window_state),
        centered: first_launch,
        // Latency: present as soon as a frame is ready, at most one frame queued.
        wgpu_options: egui_wgpu::WgpuConfiguration {
            surface: egui_wgpu::SurfaceConfig {
                // Windows: vsync'd but never blocks — each vblank shows the
                // newest frame (no tearing like Immediate, no queueing like
                // Fifo). Metal has no Mailbox; Fifo with one frame in flight.
                present_mode: if cfg!(windows) {
                    wgpu::PresentMode::Mailbox
                } else {
                    wgpu::PresentMode::Fifo
                },
                desired_maximum_frame_latency: Some(1),
            },
            wgpu_setup: zero_copy_wgpu_setup(),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(move |cc| {
            let app = PadApp::new(cc);
            match migrated {
                Ok(0) => {}
                Ok(n) => {
                    // padctl forced dark mode and egui persisted that as if
                    // it were the user's choice: start migrated users on
                    // "follow the system" instead.
                    cc.egui_ctx.set_theme(egui::ThemePreference::System);
                    log(
                        &app.shared,
                        format!("✔ 已从旧版 padctl 迁移 {n} 个数据文件（密码、配置、窗口位置）"),
                    );
                }
                Err(e) => log(&app.shared, format!("✘ 迁移旧版 padctl 数据失败: {e}")),
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window can never get narrower than the rail's layout width.
    #[test]
    fn window_minimum_fits_the_rail() {
        assert!(min_window_size(false).x >= RAIL_W);
        assert!(DEFAULT_SIZE[0] >= min_window_size(false).x);
    }

    #[test]
    fn app_icon_decodes() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
    }

    #[test]
    fn mirroring_keeps_room_for_the_stage() {
        assert_eq!(min_window_size(true).x, RAIL_W + MIN_STAGE_W);
    }
}
