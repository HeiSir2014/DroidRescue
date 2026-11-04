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
#[cfg(test)]
mod e2e;
mod geometry;
mod guide;
mod icons;
mod input;
mod logbar;
mod mirror;
mod nav;
mod pad;
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
/// Widest a page's content grows (pt); wider windows centre it.
const MAX_PAGE_W: f32 = 1200.0;
const MAX_LOG_LINES: usize = 500;
/// Our own caption (title bar) height — Windows only. macOS uses the native
/// title bar: content drawn under a transparent one gets partly covered by
/// the system's title-bar layer (the app name showed only a sliver).
const CAPTION_H: f32 = if OWN_CAPTION { 40.0 } else { 0.0 };
// Our caption exactly where the window has no native title bar.
const _: () = assert!(OWN_CAPTION == (CAPTION_H > 0.0));
const _: () = assert!(OWN_CAPTION != cfg!(target_os = "macos"));
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
const LOG_CLOSED_H: f32 = 34.0;
const LOG_OPEN_H: f32 = 240.0;
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
    device_hz: HashMap<String, Option<f32>>,
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
    log: Vec<logbar::Entry>,
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
    s.log.push(logbar::Entry::new(&msg));
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

/// Password for a blind unlock: typed > saved for this device (held /
/// plugged-in USB serial, adb device) > the only saved password. May
/// decrypt (DPAPI / Keychain): call when needed, not every frame.
/// The device a script's `{pw}` belongs to: the one the rescue holds, else
/// the single rescue-ready device plugged in, else the adb device.
fn password_device_id(
    held: Option<&str>,
    usb_serials: &[String],
    adb_device_id: Option<&str>,
) -> Option<String> {
    let plugged = match usb_serials {
        [one] => Some(one.as_str()),
        _ => None,
    };
    held.or(plugged).or(adb_device_id).map(str::to_string)
}

fn rescue_password_for(
    typed: &str,
    store: &secrets::PasswordStore,
    snap: &Snapshot,
    usb_serial: Option<&str>,
) -> Option<String> {
    if !typed.is_empty() {
        return Some(typed.to_string());
    }
    let plugged = match snap.usb_serials.as_slice() {
        [one] => Some(one.as_str()),
        _ => None,
    };
    [
        usb_serial,
        plugged,
        snap.device_id.as_deref(),
        store.only_id().as_deref(),
    ]
    .into_iter()
    .flatten()
    .find_map(|id| store.get(id).ok().flatten())
}

/// Window decorations. Windows: none — egui paints the caption and frame,
/// chrome.rs hit-tests them natively. macOS: the standard native window
/// (title bar with the app name and traffic lights; its light/dark follows
/// the app's theme setting, see [`apply_window_theme`]).
fn native_chrome(builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
    builder.with_decorations(!OWN_CAPTION)
}

/// The OS-drawn window parts (macOS title bar) follow the app's theme
/// choice, not only the system's, so title bar and content always match.
fn apply_window_theme(ctx: &egui::Context, preference: egui::ThemePreference) {
    let theme = match preference {
        egui::ThemePreference::System => egui::SystemTheme::SystemDefault,
        egui::ThemePreference::Light => egui::SystemTheme::Light,
        egui::ThemePreference::Dark => egui::SystemTheme::Dark,
    };
    ctx.send_viewport_cmd(ViewportCommand::SetTheme(theme));
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
                && let Ok(hw) = tools.shell(Some(ser), adb::SERIALNO_CMD)
            {
                let id = if hw.is_empty() { ser.to_string() } else { hw };
                shared
                    .lock()
                    .unwrap()
                    .device_ids
                    .insert(ser.to_string(), id);
            }

            // Asked once per device: a device without the line would
            // otherwise run dumpsys on every poll.
            if let Some(ser) = serial.as_deref()
                && !shared.lock().unwrap().device_hz.contains_key(ser)
            {
                let hz = tools
                    .shell(Some(ser), adb::REFRESH_CMD)
                    .ok()
                    .and_then(|o| adb::parse_max_refresh(&o));
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
                        .shell(Some(ser), adb::AWAKE_CMD)
                        .ok()
                        .and_then(|o| adb::parse_awake(&o)),
                    tools
                        .shell(Some(ser), adb::KEYGUARD_CMD)
                        .ok()
                        .and_then(|o| adb::parse_keyguard_showing(&o)),
                    tools
                        .shell(Some(ser), adb::ORIENTATION_CMD)
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
    log: Vec<logbar::Entry>,
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
    /// Last log entry seen with the drawer open: newer errors get a badge.
    log_seen: u64,
    workbench: workbench::Workbench,
    /// Window width when not showing the mirror (the user's choice),
    /// restored after it.
    idle_width: f32,
    /// The window has been checked against the page minimum once (older
    /// versions saved a one-column 340 px window).
    sized_once: bool,
    /// Frames left before the window size is trusted again after a resize.
    settle: u8,
    /// Fit the window to the picture next frame (after the compact minimum
    /// size has been applied).
    fit_pending: bool,
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
                // Readable in the status bar (the folder is in 设置 → 数据).
                log(&shared, format!("就绪 · 投屏组件 {}", t.server_version));
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
            log_seen: 0,
            workbench: workbench::Workbench::default(),
            idle_width: DEFAULT_SIZE[0],
            sized_once: false,
            settle: 0,
            fit_pending: false,
        }
    }

    /// Lamp + one-line device status (caption on Windows, sidebar on macOS).
    fn device_status(&self, snap: &Snapshot) -> nav::Status {
        let c = theme::c();
        let live = self.session().is_some();
        let selected = snap
            .devices
            .iter()
            .find(|d| Some(&d.serial) == snap.selected.as_ref());
        let unauthorized = snap.devices.iter().any(|d| d.state == "unauthorized");
        let (lamp, text) = if let Some(task) = snap.tasks.first() {
            (c.amber, format!("{task}…"))
        } else if matches!(self.mirror, MirrorState::Connecting(_)) {
            (c.amber, "连接中…".to_string())
        } else if let Some(d) = selected {
            let name = snap.name_of(d);
            if live {
                (c.amber, format!("{name} · 投屏中"))
            } else {
                (c.green, name)
            }
        } else if unauthorized {
            (c.red, "未授权 · 请在设备上允许 USB 调试".to_string())
        } else {
            (c.text_faint, "未连接".to_string())
        };
        let text_color = if lamp == c.green || lamp == c.text_faint {
            c.text_dim
        } else {
            lamp
        };
        nav::Status {
            lamp,
            text,
            text_color,
        }
    }

    /// The current page (everything except the mirror view).
    fn page_content(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        // Readable line lengths and cards that stay together on wide or
        // maximised windows: content is capped and centred (the mirror uses
        // all the room it gets while live).
        let full = ui.max_rect();
        let live_mirror = self.page == nav::Page::Mirror && self.mirroring();
        if !live_mirror && full.width() > MAX_PAGE_W {
            let rect = Rect::from_center_size(full.center(), vec2(MAX_PAGE_W, full.height()));
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                self.page_by_kind(ui, snap)
            });
        } else {
            self.page_by_kind(ui, snap);
        }
    }

    fn page_by_kind(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
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

    /// Bottom status bar: a log toggle, what is happening now (running task
    /// or latest message, with a level icon and how long ago), unseen
    /// errors; expanded, the whole log with copy / clear / open-file.
    fn log_drawer(&mut self, ui: &mut egui::Ui, rect: Rect, snap: &Snapshot) {
        use logbar::Level;
        let c = theme::c();
        let p = ui.painter_at(rect);
        // The page's own background (not the sidebar's surface): the bar
        // belongs to the content area instead of merging with the sidebar
        // into one L-shaped block.
        p.rect_filled(rect, 0.0, c.bg);
        p.line_segment(
            [rect.left_top(), rect.right_top()],
            egui::Stroke::new(1.0, c.line),
        );
        if self.log_open
            && let Some(last) = snap.log.last()
        {
            self.log_seen = self.log_seen.max(last.seq);
        }
        let unseen_errors = snap
            .log
            .iter()
            .filter(|e| e.seq > self.log_seen && e.level == Level::Error)
            .count();
        let now = Instant::now();

        let header = Rect::from_min_size(rect.min, vec2(rect.width(), LOG_CLOSED_H));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(header.shrink2(vec2(16.0, 4.0)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.set_clip_rect(header);
                ui.spacing_mut().item_spacing.x = 10.0;
                if self.log_toggle(ui).clicked() {
                    self.log_open = !self.log_open;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    if self.log_open {
                        if widgets::link(ui, "打开日志文件").clicked()
                            && let Err(e) =
                                open_in_text_editor(&bundle::default_base().join("droidrescue.log"))
                        {
                            log(&self.shared, format!("✘ 打开日志文件失败: {e}"));
                        }
                        if widgets::link(ui, "清空").clicked() {
                            self.shared.lock().unwrap().log.clear();
                        }
                        if widgets::link(ui, "复制").clicked() {
                            let text: Vec<String> = snap.log.iter().map(|e| e.line()).collect();
                            ui.ctx().copy_text(text.join("\n"));
                        }
                    } else if unseen_errors > 0 && logbar::error_badge(ui, unseen_errors).clicked()
                    {
                        self.log_open = true;
                    }
                    // What is happening now, in the room that is left (the
                    // open list already shows the latest message).
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if self.log_open && snap.tasks.is_empty() {
                            ui.label(
                                RichText::new(format!("共 {} 条", snap.log.len()))
                                    .font(theme::ui(12.0))
                                    .color(c.text_faint),
                            );
                        } else {
                            status_line(ui, snap, now);
                        }
                    });
                });
            },
        );

        if self.log_open {
            let list = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
            ui.painter_at(rect).line_segment(
                [
                    pos2(list.left() + 18.0, list.top()),
                    pos2(list.right() - 18.0, list.top()),
                ],
                egui::Stroke::new(1.0, c.line),
            );
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(list.shrink2(vec2(18.0, 6.0))),
                |ui| {
                    ui.set_clip_rect(list);
                    egui::ScrollArea::vertical()
                        .id_salt("log")
                        .stick_to_bottom(true)
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            if snap.log.is_empty() {
                                ui.label(RichText::new("暂无日志").color(c.text_faint));
                            }
                            for entry in &snap.log {
                                logbar::row(ui, entry, now);
                            }
                        });
                },
            );
        }
        // Relative times and the spinner move on their own.
        let repaint = if snap.tasks.is_empty() {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(50)
        };
        ui.ctx().request_repaint_after(repaint);
    }

    /// "≡ 日志 ⌃": list icon, label, chevron pointing where it will go.
    /// (Kept beside the drawer; the row pieces live in `logbar`.)
    fn log_toggle(&self, ui: &mut egui::Ui) -> egui::Response {
        let c = theme::c();
        let (rect, resp) = ui.allocate_exact_size(vec2(68.0, 24.0), egui::Sense::click());
        let resp = resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(if self.log_open {
                "收起日志"
            } else {
                "展开日志"
            });
        let p = ui.painter();
        let hovered = resp.hovered();
        if hovered || self.log_open {
            p.rect_filled(rect, 6.0, c.surface_hover);
        }
        let fg = if hovered || self.log_open {
            c.text
        } else {
            c.text_dim
        };
        let stroke = egui::Stroke::new(1.4, fg);
        let x = rect.left() + 9.0;
        for dy in [-4.0, 0.0, 4.0] {
            let y = rect.center().y + dy;
            p.line_segment([pos2(x, y), pos2(x + 9.0, y)], stroke);
        }
        p.text(
            pos2(x + 15.0, rect.center().y),
            Align2::LEFT_CENTER,
            "日志",
            theme::ui(12.5),
            fg,
        );
        let cx = rect.right() - 10.0;
        let cy = rect.center().y;
        let dy = if self.log_open { 1.8 } else { -1.8 };
        p.line_segment([pos2(cx - 3.5, cy - dy), pos2(cx, cy + dy)], stroke);
        p.line_segment([pos2(cx, cy + dy), pos2(cx + 3.5, cy - dy)], stroke);
        resp
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
        let device_hz = serial.as_ref().and_then(|s| {
            self.shared
                .lock()
                .unwrap()
                .device_hz
                .get(s)
                .copied()
                .flatten()
        });
        // Windows reads the window's monitor; macOS the main display.
        let monitor_hz = (self.hwnd != 0 || !OWN_CAPTION)
            .then(|| chrome::monitor_refresh_hz(self.hwnd))
            .flatten();
        opts.max_fps = adb::capture_fps(device_hz, monitor_hz, self.opts.max_fps);
        // Unknown rates are left out rather than shown as "?".
        let mut parts = vec![if opts.max_fps == 0 {
            "cap ∞".to_string()
        } else {
            format!("cap {} fps", opts.max_fps)
        }];
        parts.extend(device_hz.map(|h| format!("pad {h:.0} Hz")));
        parts.extend(monitor_hz.map(|h| format!("monitor {h} Hz")));
        self.capture_info = parts.join(" · ");
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
        self.replace_mirror(MirrorState::Connecting(rx));
        self.fps = FpsMeter::default();
    }

    fn stop_mirror(&mut self) {
        if !matches!(self.mirror, MirrorState::Idle) {
            self.replace_mirror(MirrorState::Idle);
            log(&self.shared, "■ 投屏已停止");
        }
    }

    /// Ending a session joins its threads, kills the server and removes the
    /// tunnel (adb round trips): done on a thread so the UI never stalls.
    /// A dropped `Connecting` receiver makes the connecting thread drop the
    /// session it is about to deliver.
    fn replace_mirror(&mut self, next: MirrorState) {
        if let MirrorState::Running(old) = std::mem::replace(&mut self.mirror, next) {
            thread::spawn(move || drop(old));
        }
    }

    /// On exit nothing may outlive the process half-done: the panel must be
    /// back on and the tunnel gone, so end the session here and now, and
    /// give a connection in progress a moment to arrive and be ended too.
    fn end_mirror_now(&mut self) {
        match std::mem::replace(&mut self.mirror, MirrorState::Idle) {
            MirrorState::Running(session) => drop(session),
            MirrorState::Connecting(rx) => {
                if let Ok(Ok(session)) = rx.recv_timeout(Duration::from_secs(3)) {
                    drop(session);
                }
            }
            MirrorState::Idle => {}
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
            self.replace_mirror(n);
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
        let password = password.to_string();
        self.run_task("解锁", move |t, ser, sh| {
            match adb::unlock(t, ser, &password) {
                Ok(()) => log(sh, "✔ 已发送解锁序列"),
                Err(e) => log(sh, format!("✘ {e}")),
            }
        });
    }

    fn enable_wireless(&self) {
        self.run_task("无线调试", |t, ser, sh| {
            match adb::enable_wireless(t, ser) {
                Ok(target) => log(
                    sh,
                    format!("✔ 已无线连接 {target}：拔掉数据线后在首页选这台设备即可"),
                ),
                Err(e) => log(sh, format!("✘ 无线 ADB: {e}")),
            }
        });
    }

    fn backup(&self) {
        let dest = std::path::PathBuf::from(&self.backup_dir);
        self.run_task("备份", move |t, ser, sh| {
            let result = adb::backup(t, ser, &dest, &|line| log(sh, line));
            log(sh, result.summary(&dest));
        });
    }

    // ───────────────────────── chrome ─────────────────────────

    /// Draws our title bar (Windows; non-client, hit-tested by chrome.rs);
    /// returns the left x of the caption controls.
    fn caption(&mut self, ui: &mut egui::Ui, snap: &Snapshot, maximized: bool) -> f32 {
        let (rect, drag) = ui.allocate_exact_size(
            vec2(ui.available_width(), CAPTION_H),
            egui::Sense::click_and_drag(),
        );
        // Normally the caption is non-client (chrome.rs hit-testing) and no
        // pointer events reach egui. Without a Win32 handle chrome.rs is not
        // installed: move / zoom the undecorated window from here instead.
        if self.hwnd == 0 {
            if drag.drag_started() {
                ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                ui.ctx()
                    .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
            }
        }
        let p = ui.painter();
        p.rect_filled(rect, 0.0, theme::c().surface);
        p.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            egui::Stroke::new(1.0, theme::c().line),
        );

        let y = rect.center().y;
        let logo = Rect::from_center_size(pos2(rect.left() + 12.0 + 11.0, y), vec2(22.0, 22.0));
        icons::brand_mark(p, logo);
        let mark = p.text(
            pos2(logo.right() + 8.0, y),
            Align2::LEFT_CENTER,
            "安卓救援宝",
            theme::strong_font(15.0),
            theme::c().text,
        );
        let version = widgets::version_tag(p, pos2(mark.right() + 8.0, y), APP_VERSION);
        // The English name lives in the OS window title (taskbar, Alt-Tab).
        let status = self.device_status(snap);
        let lamp_c = pos2(version.right() + 16.0, y);
        nav::paint_status(p, lamp_c, &status);

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

    /// See [`rescue_password_for`].
    fn rescue_password(&self, snap: &Snapshot, usb_serial: Option<&str>) -> Option<String> {
        rescue_password_for(&self.password, &self.passwords, snap, usb_serial)
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
        // A maximized window keeps its size: resizing it would leave a
        // window that is no longer full-screen yet still in the maximized
        // state (Restore button, no resize edges).
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
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

/// The status bar's "now": the running task with a spinner, else the
/// latest message (level icon, text, how long ago), else 就绪. Fills the
/// room it is given; the full text is the tooltip.
fn status_line(ui: &mut egui::Ui, snap: &Snapshot, now: Instant) {
    let c = theme::c();
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 24.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let icon_at = pos2(rect.left() + 7.0, rect.center().y);
    let (text, color, tail, tip) = if let Some(task) = snap.tasks.first() {
        let time = ui.input(|i| i.time);
        logbar::paint_spinner(&p, icon_at, 13.0, time, c.amber);
        (format!("{task}…"), c.amber, String::new(), None)
    } else if let Some(last) = snap.log.last() {
        logbar::paint_level(&p, last.level, icon_at, 12.0);
        let color = match last.level {
            logbar::Level::Error => c.red,
            logbar::Level::Info => c.text_dim,
            _ => c.text,
        };
        let ago = logbar::ago(now.saturating_duration_since(last.at));
        (last.text.clone(), color, ago, Some(last.text.clone()))
    } else {
        logbar::paint_level(&p, logbar::Level::Info, icon_at, 12.0);
        ("就绪".to_string(), c.text_faint, String::new(), None)
    };
    let tail = (!tail.is_empty()).then(|| p.layout_no_wrap(tail, theme::ui(11.0), c.text_faint));
    let tail_w = tail.as_ref().map_or(0.0, |g| g.size().x + 12.0);
    let text_left = rect.left() + 20.0;
    let mut job = egui::text::LayoutJob::single_section(
        text,
        egui::TextFormat::simple(theme::ui(12.0), color),
    );
    job.wrap =
        egui::text::TextWrapping::truncate_at_width((rect.right() - text_left - tail_w).max(0.0));
    let galley = p.layout_job(job);
    // The full text as a tooltip only when it did not fit.
    let tip = tip.filter(|_| galley.elided);
    let text_right = text_left + galley.size().x;
    p.galley(
        pos2(text_left, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if let Some(tail) = tail {
        p.galley(
            pos2(text_right + 12.0, rect.center().y - tail.size().y / 2.0),
            tail,
            c.text_faint,
        );
    }
    if let Some(tip) = tip {
        resp.on_hover_text(tip);
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
                modifiers,
                ..
            } if st.focused => {
                if let Some(code) = input::android_keycode(*key, *modifiers) {
                    let action = if *pressed {
                        KeyAction::Down
                    } else {
                        KeyAction::Up
                    };
                    let meta = input::metastate(*modifiers);
                    s.send(&control::keycode(action, code, u32::from(*repeat), meta));
                }
            }
            // The PC turns Ctrl+C / Ctrl+X into these instead of keys.
            egui::Event::Copy | egui::Event::Cut if st.focused => {
                let code = if matches!(ev, egui::Event::Copy) {
                    31
                } else {
                    52
                }; // C / X
                let meta = input::metastate(egui::Modifiers::CTRL);
                s.send(&control::keycode(KeyAction::Down, code, 0, meta));
                s.send(&control::keycode(KeyAction::Up, code, 0, meta));
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
            // Fit next frame: the lower minimum size above is applied after
            // this frame, and Windows clamps a resize to the minimum in force
            // (the picture would sit in a page-wide window).
            self.fit_pending = true;
            ctx.request_repaint();
        } else if compact && std::mem::take(&mut self.fit_pending) {
            // No stream yet: best guess from the model and current rotation;
            // the stage refits once the first frame reveals the real shape.
            let model = skin::Model::for_device(snap.selected_model(), (0, 0), snap.orientation);
            let extent = model.extent(skin::turns_for(&model, snap.orientation, None));
            self.fit_window(&ctx, self.last_extent.unwrap_or(extent));
        } else if !compact && self.was_compact {
            self.fit_pending = false;
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
                // Our caption shows the device status; without one (macOS)
                // the sidebar does.
                let status = (!OWN_CAPTION).then(|| self.device_status(&snap));
                if let Some(page) = nav::sidebar(ui, nav_rect, self.page, compact, status.as_ref())
                {
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
        self.end_mirror_now();
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

/// Product name (Chinese name first, English name for search). Also
/// eframe's app id — it names where window state persists, so it must not
/// change between versions.
const APP_TITLE: &str = "安卓救援宝 DroidRescue";
/// The version as shown to users ("v0.2.0").
const APP_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// The OS window title (taskbar, Alt-Tab, macOS title bar): name + version.
fn window_title() -> String {
    format!("{APP_TITLE} {APP_VERSION}")
}

fn main() -> eframe::Result {
    // Before anything reads the data dir (log, PINs, window state).
    let migrated = bundle::migrate_legacy_data();

    // Elevated helper mode (started by the rescue via UAC): bind the in-box
    // WinUSB driver to the given rescue functions, log the outcome, exit.
    if std::env::args().nth(1).as_deref() == Some(usbhost::INSTALL_FLAG) {
        let shared: SharedRef = Arc::default();
        let targets = usbid::UsbTarget::parse_list(&std::env::args().nth(2).unwrap_or_default());
        // The exit code is the result the waiting parent reports.
        match targets.and_then(|t| usbhost::install(&t)) {
            Ok(ids) => log(&shared, format!("✔ WinUSB 已安装到 {}", ids.join(", "))),
            Err(e) => {
                log(&shared, format!("✘ WinUSB 安装失败: {e}"));
                std::process::exit(1);
            }
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
                .with_title(window_title())
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
            // The persisted choice also applies to the OS-drawn title bar.
            apply_window_theme(&cc.egui_ctx, cc.egui_ctx.options(|o| o.theme_preference));
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
    fn script_password_belongs_to_the_held_then_plugged_then_adb_device() {
        let one = vec!["USB1".to_string()];
        let two = vec!["USB1".to_string(), "USB2".to_string()];
        assert_eq!(
            password_device_id(Some("HELD"), &one, Some("ADB")).as_deref(),
            Some("HELD")
        );
        assert_eq!(
            password_device_id(None, &one, Some("ADB")).as_deref(),
            Some("USB1")
        );
        assert_eq!(
            password_device_id(None, &two, Some("ADB")).as_deref(),
            Some("ADB"),
            "several plugged: not guessed"
        );
        assert_eq!(password_device_id(None, &[], None), None);
    }

    #[test]
    fn window_title_shows_the_version_but_the_app_id_stays_fixed() {
        assert_eq!(
            window_title(),
            format!("安卓救援宝 DroidRescue v{}", env!("CARGO_PKG_VERSION"))
        );
        // eframe stores window state under this name: never versioned.
        assert_eq!(APP_TITLE, "安卓救援宝 DroidRescue");
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
