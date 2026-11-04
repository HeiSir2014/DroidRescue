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
mod guide;
mod input;
mod mirror;
mod nav;
mod pages;
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
mod workbench;
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
/// Our own caption (title bar) height — Windows only. macOS uses the native
/// title bar: content drawn under a transparent one gets partly covered by
/// the system's title-bar layer (the app name showed only a sliver).
const CAPTION_H: f32 = if OWN_CAPTION { 40.0 } else { 0.0 };
const RESIZE_BORDER: f32 = 6.0;
/// First-launch window: sidebar + a comfortable page.
const DEFAULT_SIZE: [f32; 2] = [1120.0, 780.0];
/// While mirroring, the picture keeps at least this much width.
const MIN_STAGE_W: f32 = 200.0;
/// Pages (cards, the script editor) need at least this much beside the
/// sidebar.
const MIN_PAGE_W: f32 = 620.0;
const MIN_HEIGHT: f32 = 600.0;
/// Log drawer: closed (one line) and open heights.
const LOG_CLOSED_H: f32 = 30.0;
const LOG_OPEN_H: f32 = 200.0;
const _: () = assert!(DEFAULT_SIZE[1] >= MIN_HEIGHT && DEFAULT_SIZE[0] >= nav::NAV_W + MIN_PAGE_W);

/// Smallest window: full sidebar + a usable page, or — while showing the
/// mirror — collapsed sidebar + picture + key strip.
fn min_window_size(compact_mirror: bool) -> egui::Vec2 {
    let width = if compact_mirror {
        nav::NAV_COLLAPSED_W + MIN_STAGE_W + pages::KEYSTRIP_W
    } else {
        nav::NAV_W + MIN_PAGE_W
    };
    vec2(width, MIN_HEIGHT)
}

/// Frames to ignore the window size after asking for a resize (it lands a
/// frame or two later), so a transient size is not taken as the user's.
const RESIZE_SETTLE_FRAMES: u8 = 6;

/// Split the window body into (sidebar, page).
fn split_body(body: Rect, nav_w: f32) -> (Rect, Rect) {
    let nav = Rect::from_min_max(body.min, pos2(body.left() + nav_w, body.bottom()));
    (
        nav,
        Rect::from_min_max(pos2(nav.right(), body.top()), body.max),
    )
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
/// chrome.rs hit-tests them natively. macOS: the standard native window
/// (title bar with the app name, traffic lights, light/dark from the system).
fn native_chrome(builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
    if OWN_CAPTION {
        builder.with_decorations(false)
    } else {
        builder
    }
}

/// We draw the title bar, frame and window buttons ourselves (Windows,
/// hit-tested natively by chrome.rs). macOS keeps the native title bar.
const OWN_CAPTION: bool = !cfg!(target_os = "macos");

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
    /// The mirror (collapsed sidebar + picture) was shown last frame.
    was_compact: bool,
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
    /// Rescue profiles (built-in + user file) and the user's explicit pick
    /// (None = automatic by model).
    profiles: Vec<profiles::Profile>,
    profile_choice: Option<String>,
    /// USB serial → adb model, learnt whenever adb sees a device.
    known_models: profiles::KnownModels,
    /// Last 设备体检 scan (taken on demand, not every frame).
    checkup: Option<Vec<checkup::Row>>,
    /// Page shown (sidebar selection).
    page: nav::Page,
    /// 连接设备 dialog, when open.
    connect: Option<pages::ConnectTab>,
    connect_form: pages::ConnectForm,
    /// Log drawer expanded.
    log_open: bool,
    workbench: workbench::Workbench,
    /// Window width when not showing the mirror (the user's choice),
    /// restored after it.
    idle_width: f32,
    /// The window has been checked against the page minimum once (older
    /// versions saved a one-column 340 px window).
    sized_once: bool,
    /// Frames left before the window size is trusted again after a resize.
    settle: u8,
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
            was_compact: false,
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
            profiles: profile_list,
            checkup: None,
            profile_choice: None,
            known_models: profiles::KnownModels::load(profiles::known_models_path()),
            page: cc
                .storage
                .and_then(|s| s.get_string(PAGE_KEY))
                .map_or(nav::Page::Home, |k| nav::Page::from_key(&k)),
            connect: None,
            connect_form: pages::ConnectForm::default(),
            log_open: false,
            workbench: workbench::Workbench::default(),
            idle_width: DEFAULT_SIZE[0],
            sized_once: false,
            settle: 0,
        }
    }

    /// The current page (everything except the mirror view).
    fn page_content(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        match self.page {
            nav::Page::Home => self.home_page(ui, snap),
            nav::Page::Mirror => self.mirror_page(ui, snap),
            nav::Page::Rescue => self.rescue_page(ui, snap),
            nav::Page::Script => self.script_page(ui, snap),
            nav::Page::Guide => self.guide_page(ui),
            nav::Page::Tools => self.tools_page(ui, snap),
            nav::Page::Settings => self.settings_page(ui),
        }
    }

    /// Bottom drawer: the last log line, or the full log when open.
    fn log_drawer(&mut self, ui: &mut egui::Ui, rect: Rect, snap: &Snapshot) {
        let c = theme::c();
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, c.surface);
        p.line_segment(
            [rect.left_top(), rect.right_top()],
            egui::Stroke::new(1.0, c.line),
        );
        let color_of = |line: &str| {
            if line.starts_with('✔') {
                c.green
            } else if line.starts_with('✘') {
                c.red
            } else {
                c.text_dim
            }
        };
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink2(vec2(16.0, 4.0))),
            |ui| {
                ui.set_clip_rect(rect);
                ui.horizontal(|ui| {
                    let arrow = if self.log_open {
                        "日志 ▾"
                    } else {
                        "日志 ▴"
                    };
                    if widgets::link(ui, arrow).clicked() {
                        self.log_open = !self.log_open;
                    }
                    if let Some(task) = snap.tasks.first() {
                        ui.label(
                            RichText::new(format!("● {task}…"))
                                .font(theme::mono(10.5))
                                .color(c.amber),
                        );
                    }
                    if !self.log_open
                        && let Some(last) = snap.log.last()
                    {
                        ui.add(
                            egui::Label::new(
                                RichText::new(last)
                                    .font(theme::mono(10.5))
                                    .color(color_of(last)),
                            )
                            .truncate(),
                        );
                    }
                });
                if self.log_open {
                    egui::ScrollArea::vertical()
                        .id_salt("log")
                        .stick_to_bottom(true)
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            for line in &snap.log {
                                ui.label(
                                    RichText::new(line)
                                        .font(theme::mono(10.5))
                                        .color(color_of(line)),
                                );
                            }
                        });
                }
            },
        );
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

    /// Draws our title bar (Windows; non-client, hit-tested by chrome.rs);
    /// returns the left x of the caption controls.
    fn caption(&mut self, ui: &mut egui::Ui, snap: &Snapshot, maximized: bool) -> f32 {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), CAPTION_H), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 0.0, theme::c().surface);
        p.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            egui::Stroke::new(1.0, theme::c().line),
        );

        let y = rect.center().y;
        let mark = p.text(
            pos2(rect.left() + 14.0, y),
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
        // The stage is shrunk by 8 px on each side before the skin is laid
        // out; the log drawer (closed) takes its line below the picture.
        let inner_h = height - CAPTION_H - LOG_CLOSED_H - 16.0;
        self.set_width(
            ctx,
            inner_h * extent.x / extent.y + 16.0 + nav::NAV_COLLAPSED_W + pages::KEYSTRIP_W,
        );
    }

    /// Remember the user's window width while not mirroring (to restore it
    /// after mirroring), and widen a window saved by the old one-column
    /// layout once.
    fn track_idle_width(
        &mut self,
        ctx: &egui::Context,
        mirroring: bool,
        maximized: bool,
        inner: Option<Rect>,
    ) {
        if self.settle > 0 {
            self.settle -= 1;
            return;
        }
        let Some(inner) = inner else { return };
        if mirroring || maximized || self.uncloak_in > 0 {
            return;
        }
        if !self.sized_once {
            self.sized_once = true;
            if inner.width() + 0.5 < min_window_size(false).x {
                self.set_width(ctx, DEFAULT_SIZE[0]);
                return;
            }
        }
        self.idle_width = inner.width();
    }

    /// Resize horizontally without moving the rail on screen (the stage opens
    /// or closes on the far side) — no jump under the user's cursor.
    fn set_width(&mut self, ctx: &egui::Context, width: f32) {
        self.settle = RESIZE_SETTLE_FRAMES;
        if self.hwnd != 0 {
            let px = (width * ctx.pixels_per_point()).round() as i32;
            // Cloak across the resize: the swapchain still holds the old-size
            // frame (DWM would stretch it) and the next egui pass lays out with
            // the old size. Show again once correct frames have been presented.
            chrome::set_cloaked(self.hwnd, true);
            // The sidebar is on the left: keep the left edge, grow rightwards.
            chrome::resize_width_keep_rail(self.hwnd, px, Side::Left);
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
        // Compact = the mirror is on screen: collapsed sidebar, window fitted
        // to the picture. Other pages keep the user's window width (the
        // mirror keeps running in the background).
        let compact = self.mirroring() && self.page == nav::Page::Mirror;
        if compact != self.was_compact {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min_window_size(
                compact,
            )));
        }
        if compact && !self.was_compact {
            // No stream yet: best guess from the model and current rotation;
            // the stage refits once the first frame reveals the real shape.
            let model = skin::Model::for_device(snap.selected_model(), (0, 0), snap.orientation);
            let extent = model.extent(skin::turns_for(&model, snap.orientation, None));
            self.fit_window(&ctx, self.last_extent.unwrap_or(extent));
        } else if !compact && self.was_compact {
            self.set_width(&ctx, self.idle_width);
            self.last_extent = None;
        }
        self.was_compact = compact;

        let (maximized, focused, inner) = ctx.input(|i| {
            (
                i.viewport().maximized.unwrap_or(false),
                i.viewport().focused.unwrap_or(true),
                i.viewport().inner_rect,
            )
        });
        self.track_idle_width(&ctx, compact, maximized, inner);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::c().bg))
            .show(root, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if OWN_CAPTION {
                    let controls_left = self.caption(ui, &snap, maximized);
                    let ppp = ctx.pixels_per_point();
                    chrome::set_layout(ChromeLayout {
                        caption_height: (CAPTION_H * ppp).round() as i32,
                        controls_left: (controls_left * ppp).round() as i32,
                        border: (RESIZE_BORDER * ppp).round() as i32,
                        resizable: if maximized { Edges::NONE } else { Edges::ALL },
                    });
                }

                let body = ui.available_rect_before_wrap();
                let nav_w = if compact {
                    nav::NAV_COLLAPSED_W
                } else {
                    nav::NAV_W
                };
                let (nav_rect, page_rect) = split_body(body, nav_w);
                if let Some(page) = nav::sidebar(ui, nav_rect, self.page, compact) {
                    self.page = page;
                }
                let log_h = if self.log_open {
                    LOG_OPEN_H
                } else {
                    LOG_CLOSED_H
                };
                let content = Rect::from_min_max(
                    page_rect.min,
                    pos2(page_rect.right(), page_rect.bottom() - log_h),
                );
                let log_rect =
                    Rect::from_min_max(pos2(page_rect.left(), content.bottom()), page_rect.max);
                if compact {
                    self.mirror_view(ui, content, &snap);
                } else if content.width() >= MIN_PAGE_W / 2.0 {
                    // (Narrower only for the frames until an old one-column
                    // window has been widened.)
                    ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
                        // Whatever a page lays out, it never paints elsewhere.
                        ui.set_clip_rect(content);
                        egui::Frame::NONE
                            .inner_margin(Margin::symmetric(24, 18))
                            .show(ui, |ui| {
                                ui.set_min_size(ui.available_size());
                                ui.spacing_mut().item_spacing.y = 8.0;
                                self.page_content(ui, &snap);
                            });
                    });
                }
                self.log_drawer(ui, log_rect, &snap);
                if OWN_CAPTION {
                    // macOS draws its own window border.
                    widgets::window_frame(ui, focused);
                }
            });
        self.connect_dialog(&ctx, &snap);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(PAGE_KEY, self.page.key().to_string());
    }

    fn on_exit(&mut self) {
        self.stop_mirror();
    }
}

/// eframe storage key of the last page.
const PAGE_KEY: &str = "page";

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
                .with_min_inner_size(min_window_size(false)),
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
    fn pages_keep_room_beside_the_full_sidebar() {
        assert_eq!(min_window_size(false).x, nav::NAV_W + MIN_PAGE_W);
        assert!(DEFAULT_SIZE[0] >= min_window_size(false).x);
    }

    #[test]
    fn body_splits_into_sidebar_and_page() {
        let body = Rect::from_min_max(pos2(0.0, 40.0), pos2(1000.0, 800.0));
        let (nav_rect, page) = split_body(body, nav::NAV_W);
        assert_eq!((nav_rect.left(), nav_rect.width()), (0.0, nav::NAV_W));
        assert_eq!((page.left(), page.right()), (nav::NAV_W, 1000.0));
        assert_eq!(page.top(), 40.0);
    }

    #[test]
    fn app_icon_decodes() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
    }

    #[test]
    fn mirroring_keeps_room_for_the_picture_and_keys() {
        assert_eq!(
            min_window_size(true).x,
            nav::NAV_COLLAPSED_W + MIN_STAGE_W + pages::KEYSTRIP_W
        );
    }
}
