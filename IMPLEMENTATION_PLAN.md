# padctl v3 — native low-latency client + Pad3 device skin

No scrcpy.exe/SDL window any more. padctl talks the scrcpy-server v5.0.1
protocol directly (server jar pushed via adb), decodes H.264 itself and draws
the video inside egui as the screen of a drawn vivo Pad3 body.

Protocol (verified on device 2026-11-04): forward tunnel → video socket:
dummy byte, 64-byte device name, u32 codec id ("h264"), then 12-byte headers:
session (MSB set: w/h at 4..12) or media (bit62 config, bit61 key, pts, u32
len) + Annex-B payload. Control socket: big-endian messages (control.rs).

Latency rules: latest-frame-wins (never queue frames), decoder low-delay,
repaint immediately on new frame, no-vsync present, on-screen latency HUD.

## Stage 1: Protocol + control encoding (pure, tested)
**Goal**: `protocol.rs` (header parsing, orientation parsing), `control.rs`
(keycode/text/touch/scroll/back/power/clipboard byte layouts).
**Tests**: byte-exact layouts and header parsing against captured bytes.
**Status**: Complete

## Stage 2: Session + decoder (CPU path)
**Goal**: `mirror.rs` (push server, forward, start, sockets, demux thread,
control writer, clean stop), `decoder.rs` trait + FFmpeg software decoder
(dynamic load, minimal FFI), latest-frame slot.
**Tests**: e2e on device: frames arrive, size matches session packet.
**Status**: Complete

## Stage 3: GPU video renderer
**Goal**: `video.rs` — egui_wgpu callback, Y/U/V R8 textures, WGSL BT.601
conversion, no-vsync present, latency/fps HUD.
**Status**: Complete

## Stage 4: Device skin + input
**Goal**: `skin.rs` (mm geometry, rotation, fit; tested), keys at real
positions → keycodes; mouse → touch, wheel → scroll, keyboard → keycode/text,
paste → clipboard; one window: caption + stage + rail.
**Status**: Complete

## Stage 4b: Audio forwarding + physical screen off
**Goal**: `audio_codec=raw` (PCM s16le 48 kHz stereo, verified on device:
4096-byte packets, no config) on its own socket (order: video, audio,
control); WASAPI playback via cpal owned by the audio thread; jitter buffer
target 40 ms / cap 120 ms (drop oldest when over cap, re-prebuffer on
underrun); linear resampler to the device mix rate. Device audio is muted by
the `output` source (Android 11+). Screen off by default via
SET_DISPLAY_POWER(false), runtime toggle, restored on stop.
**Tests**: latency policy, resampler, s16→f32 conversion, server args.
Device: audio audible on PC, SurfaceFlinger power mode off while mirroring.
**Status**: Complete (verified: powerMode Off→On, audio buffer steady 40 ms,
stale forward tunnels cleaned on start)

## Stage 4c: Window persistence + frame-rate policy
**Goal**: persist window position (eframe `persistence`; centre only on first
launch since `centered` overrides saved state); capture cap
= min(panel max Hz, monitor Hz) via server `max_fps`; Mailbox present.
**Tests**: refresh parsing, cap policy; device: relaunch restores position,
HUD shows `cap 60 fps · pad 144 Hz · monitor 60 Hz`.
**Status**: Complete

## Stage 5: Zero-copy (texture passthrough)
**Goal**: D3D11 hardware decode (Media Foundation, low-latency mode) → NV12
texture → shared NT handle → D3D12 → wgpu texture (Plane0/Plane1 views).
Compare HUD latency vs stage 3.
**Design**:
- wgpu forced to DX12 + `TEXTURE_FORMAT_NV12`; adapter LUID handed to the
  decoder so D3D11 runs on the same GPU.
- `decoder_mf.rs`: MS H.264 MFT, D3D11-aware (DXGI device manager),
  `MF_LOW_LATENCY`; output texture slice → GPU copy into a ring of 3 shared
  NV12 textures (`SHARED | SHARED_NTHANDLE`); event query guarantees the copy
  finished before the frame is published (no cross-API fence needed).
- `video.rs`: import each shared handle once (`OpenSharedHandle` →
  `texture_from_raw` → `create_texture_from_hal`), NV12 shader with
  Plane0 (R8) / Plane1 (RG8) views, crop to the visible size.
- Graceful degradation: any failure → FFmpeg CPU path (kept as fallback).
**Tests**: picture/crop math unit tests; device: HUD shows `D3D11 zero-copy`,
decode ms vs CPU path.
**Status**: In Progress — zero-copy works on device (hw decode 0.6–1.5 ms).
Open: decode thread blocks on GPU event queries; totals swing 3–14 ms between
runs (whichever wait comes first absorbs it — not the shared texture, verified
with a private-copy control). Next: replace CPU waits with a shared
ID3D11Fence signalled after the copy and waited on by wgpu's DX12 queue.

## Stage 6: Rescue mode — AOA HID without USB debugging
**Why**: broken screen + USB debugging off (device enumerates as VID 2D95 /
PID 6012: MTP + mass storage, no ADB interface — observed 2026-11-04).
**Approach**: Android Open Accessory v2 HID over USB control transfers
(what `scrcpy --otg` does; needs no adb / no authorisation, works on the
lock screen). Register: boot keyboard, absolute touch digitizer (exact blind
taps), consumer control (Home/Back). Flows: wake → swipe → saved PIN → Enter;
"USB 用途" dialog 4th option (↓×3, Enter); "允许 USB 调试" (always-allow + OK);
free typing.
**Blocker**: Windows needs a libusb-capable driver on the device.
Options: (a) UsbDk filter driver (no driver replacement, MTP keeps working);
(b) WinUSB via libwdi on the MTP interface (MTP lost until reverted).
**Tests**: HID report encoding unit tests; device: keyboard input reaches the
lock screen with debugging off.
**Status**: In Progress — driver: UsbDk (user choice). First install without a
reboot + a probe that opened every USB device → BSOD 0x10D WDF_VIOLATION.
Fixed: reboot after install; only VID 2D95 is ever opened. Verified
2026-11-04 on Pad3 (PID 6012, debugging off): AOA v2, keyboard/mouse/consumer
registered, key sent, MTP intact.
**End-to-end verified 2026-11-04 03:01** (debugging off, screen unused):
keyboard unlock with the DPAPI-saved PIN → pen tap on 连接办公套件 → vivo PC
suite enables USB debugging → tablet re-enumerates as PID 6013, adb
`device` (authorised). Key facts (sourced, not guessed): single-touch HID
"Touch Screen" gets no INPUT_PROP_DIRECT in hid-input.c → Android treats it
as a pointer; the "Pen" application gets INPUT_PROP_DIRECT → touch screen
with absolute coords; raw coords are natural-orientation, mapped per
TouchInputMapper::computeInputTransforms (ROTATION_90: send (1−fy, fx)).
Relative mouse is unusable for positioning (acceleration/scaling).
**WinUSB verified 2026-11-04 03:08**: in-box winusb.inf bound to MI_01 via
SetupAPI (DiInstallDevice) + `DeviceInterfaceGUIDs` in the hardware key
(without it libusb fails with NOT_FOUND). WinUSB-only flow: unlock → pen tap
→ vivo suite enabled debugging (so it does not need MI_01) → 6012→6013 with
no enumeration failure. AOA via the ADB interface also works, but adb server
holds it exclusively (Access) — irrelevant in practice.
**Native WinUSB verified 2026-11-04 03:2x**: only MI_01 opened (usbdev.rs),
coexists with adb on MI_02; full workflow incl. reconnect after 6012→6013,
知道了, 一律允许 + 允许 → adb `device`. UsbDk removed (UsbDk.sys pending
deletion after reboot). Correction (user-verified): the vivo PC suite does
NOT need to run — choosing the 连接办公套件 USB mode makes the tablet enable
debugging by itself.
**Status**: Complete

## Stage 7: Generic mirroring, per-model rescue, offline password entry
### 7a Generic skin (bug: a portrait phone was stretched into the Pad3 frame)
**Goal**: `skin::Model` — Pad3 (by adb model PA2455) or a generic body built
from the video's natural size; video always aspect-fit inside the screen
(letterbox) for drawing *and* input mapping; window fit uses the same model.
**Tests**: generic screen aspect == video aspect for all rotations; letterbox
fit keeps aspect; screen→video mapping inside the fitted rect; Pad3 unchanged.
**Status**: Implemented + unit-tested; awaiting check with a phone.
### 7b Rescue profiles per model
**Goal**: `rescue::Profile` (name, models, unlock / enable-debugging scripts):
built-in Pad3 + generic Android (keyboard-only unlock); user profiles in
`%LOCALAPPDATA%\padctl\rescue-profiles.txt` (same script syntax); profile
picker in 06, auto-chosen from the adb model last seen for that USB serial.
**Tests**: profile file parsing, override by name, auto-selection.
**Status**: Implemented + unit-tested; awaiting device check.
### 7b-fix Reconnect on a fresh PC (found on a second PC 2026-11-04)
Two bugs: (1) reconnect opened the *lingering* 6012 device right after the
mode-switch tap → AOA 57 failed (0x16/0x1F) as it vanished; now a device
counts only after the old one was seen gone or the parent PID differs.
(2) one-time setup only binds the mode present at the time; the 6013 MI_01
stayed on usbstor → "没有找到可用的平板接口". Reconnect now detects an
unbound target and requests the elevated WinUSB install itself (+90 s).
**Status**: Complete (0.1.1, verified end-to-end on the second PC).
### 7c Password entry with nothing connected
**Goal**: 04 always accepts a password; unlock goes via adb when available,
otherwise via the rescue keyboard; saved per serial (USB iSerial ==
ro.serialno), so it is usable before any adb connection.
**Tests**: target choice (adb / rescue / none); id choice.
**Status**: Implemented + unit-tested; awaiting device check (incl. the new
`wm size`-based adb swipe, not yet run on a device).

## Stage 8: Rename → 安卓救援宝 / DroidRescue + public repo + CI
**Goal**: package/exe `droidrescue`, title 安卓救援宝; data dir
`%LOCALAPPDATA%\DroidRescue` with one-time migration from `padctl` (DPAPI
entropy kept so saved PINs still decrypt); public GitHub repo
`HeiSir2014/DroidRescue`; no third-party binaries in git —
`scripts/fetch-vendor.ps1` pins scrcpy-win64-v5.0.1.zip by SHA256
(B12A2C4E…27A5, files verified identical to the current vendor dir); personal
data (serials, user paths) scrubbed from tests/docs; GitHub Actions: fmt,
clippy -D warnings, test, release build artifact on windows-latest.
**Tests**: migration unit test; CI green.
**Status**: Complete — repo public, toolchain pinned (rust-toolchain.toml
1.99.0), CI green (fmt, clippy, test, packaged artifact); migration verified
on the dev PC (3 files carried over).

## Stage 9: Light / dark theme
**Goal**: follow the OS theme (eframe system theme) with a manual
override (跟随系统/亮色/暗色, persisted); every colour from a palette
token, both palettes checked for contrast (text ≥ 4.5:1).
**Tests**: palette contrast test; both themes render (screenshot check).
**Status**: Complete — contrast tests caught dark `text_faint` at 2.81:1
(raised to ≥3:1); both themes screenshot-checked; HUD stays dark (it sits
on a black scrim over video); migrated users reset to "follow system"
(padctl had forced dark and egui persisted it).

## Stage 10: Rescue that works on any PC / port / mode
**Why**: WinUSB binds per device *instance*, and instance ids depend on the
USB port and USB mode → a new port or a first-seen mode had no driver
(second PC). Never run the GUI elevated (UIPI, least privilege): only the
driver bind is elevated, as a short-lived helper.
**Goal**: rescue start and reconnect detect any unbound target interface
and bind it (one UAC per new port/mode, automatic); target interfaces come
from the profile (`usb = 2D95:6012:01, 2D95:6013:01`), not code; a 设备体检
(diagnostics) view: USB mode, interfaces + drivers, AOA protocol, adb state.
**Tests**: target matching from profiles; unbound detection logic.
**Done**: `usbid::UsbTarget` (`VID:PID[:MI]`, whole non-composite devices
too); profiles `usb =`; union of all profiles' targets drives binding;
`rescue::connect` binds unbound targets at rescue start (one UAC, helper
gets the targets on its command line); profile auto-pick by present USB ids
(fresh PC, never seen over adb); `checkup` 设备体检 view + copyable report.
Window minimum width = rail width (+ stage while mirroring).
**Pending**: device verification on a fresh PC / new port.
**Status**: Complete (code), device check pending

## Stage 11: macOS port (Mac mini, ~/Documents/projects/DroidRescue)
**Goal**: same crate, platform modules behind `cfg`: VideoToolbox decode →
IOSurface → Metal texture (zero-copy, wgpu Metal hal); rescue AOA through
IOKit/libusb (no driver binding on macOS); Keychain for PINs; plain
native title bar (a full-size content view hid the drawn caption under the
title-bar layer); bundled darwin adb; signed .app/.dmg
via CI macos-latest.
**Tests**: core tests pass on macOS; mirror + rescue on device.
**Done**: platform modules (`src/macos/`: usbhost/usbdev via nusb, chrome,
decoder_vt); Keychain-keyed AES-GCM PIN store; Metal backend; native
traffic-light title bar; darwin adb bundle (`fetch-vendor.sh`);
`package-macos.sh` → ad-hoc signed .app in an LZMA .dmg; CI macos job
(fmt, clippy, tests incl. a real VideoToolbox decode, package).
**Pending**: device test (mirror + rescue) on a Mac with a phone attached;
zero-copy IOSurface → Metal (Stage 13); Developer ID signing +
notarisation (needs an Apple Developer account — owner's decision).
**Status**: In Progress (runs; device verification pending)

## Stage 12: Release pipeline + app icon
**Goal**: one SVG icon → .ico (PNG entries) / .icns / window PNG
(`scripts/make-icons.sh`); exe icon + version info (winresource); tag
`vX.Y.Z` → CI builds both platforms, checks tag == Cargo version, adds
SHA256SUMS, creates a **draft** release with notes from CHANGELOG.md.
**Tests**: icon decodes (unit test); exe VersionInfo/icon checked; draft
release v0.2.0 contains win 7z/zip + mac dmg + SHA256SUMS.
**Done**: draft v0.2.0 built by CI (first macOS run hit an hdiutil flake →
retry added); Windows zip copied to YANTAO G:\soft, SHA256 verified.
**Pending**: owner smoke-tests the draft's builds on devices, then publishes.
**Status**: Complete

## Stage 13: macOS zero-copy video
**Goal**: VideoToolbox CVPixelBuffer (IOSurface) → Metal NV12 textures via
wgpu's Metal hal, like the D3D12 path on Windows; CPU read-back stays as
fallback.
**Tests**: decode fixture → imported texture; HUD shows decoder path.
**Done**: VT asks for Metal-compatible '420v' IOSurfaces; `macos/surface.rs`
aliases both planes via CVMetalTextureCache → `texture_from_raw` →
`create_texture_from_hal`; the shared NV12 shader samples them; the last 3
frames stay alive while in flight. Test: imported planes read back
byte-identical to the CPU path's planes.
**Status**: Complete (device check with a phone pending)

## Stage 14: Sidebar navigation + pages
**Why**: the 320 px single column made sense next to a video stage, not on
its own; a rail + tabs split put controls in two places. Owner's direction:
left navigation, every feature a page (layout language borrowed from
desktop phone suites — cards, device hero, connect dialog — keeping our
own palette and type, no third-party branding).
**Goal**: sidebar (icon + label; collapses to icons while mirroring):
首页 · 投屏 · 救援 · 脚本调试 · 脚本说明 | 工具 · 设置. 首页 = device hero card
(name, status tags, actions, drawn device) + 解锁 card; 投屏 = stage + key
strip; 救援 = rescue/manual/checkup cards; 工具 = wireless adb, export,
stay-on; 设置 = theme + mirror options. Bottom log drawer on every page.
Connect dialog (USB / 无线 / 屏幕坏了→救援). Pages live in `src/pages/`.
**Tests**: window minimums per mode; page key round-trip; body split.
**Status**: Complete (device check of the mirror page pending)

## Stage 15: Script engine for debugging
**Goal**: parse keeps source line numbers (errors say 第 N 行; steps map to
lines); one command table (`rescue::COMMANDS`: syntax, meaning, example)
feeds the parser's help and the 说明 tab — its examples are tested to parse;
worker: stop (cancel flag, also inside waits), progress (current step),
pen rotation kept across runs while the device is held (single-stepping
after `rot:0` works), runs can start fresh (rotation reset) or continue.
**Tests**: line numbers in errors/steps; every COMMANDS example parses;
cancel/rotation logic as pure functions.
**Status**: Complete

## Stage 16: Script workbench
**Goal**: 脚本 tab: profile + field (解锁 / 一键救援) picker, code editor
with line numbers, live validation, 运行全部 / 单步 / 运行当前行 / 停止,
executing line marker, 保存到机型配置 (rewrites only that section of
rescue-profiles.txt), 新建机型 (name, adb model, USB ids prefilled from 设备体检).
**Tests**: `profiles::render` + `replace_section` (replace, append, keep
other sections and comments); new-profile prefill.
**Status**: Complete (device run of single-step pending)

## Stage 17: In-app script guide (说明)
**Goal**: complete guide in the UI: concepts, command reference (from
COMMANDS), development workflow (measure coordinates, 指针位置 + move,
single-step, reconnect across USB-mode switches, save), debugging tips /
FAQ, the Pad3 script annotated line by line; "插入" buttons put examples
into the editor.
**Tests**: guide examples parse; Pad3 annotation covers every line.
**Status**: Complete

## Stage 18: Coordinate pad (坐标拾取板)
**Goal**: beside the script editor, a pad shaped like the device screen
(aspect preset, rotation rot:0–3 = the script's coordinate system). The
mouse over it shows the normalised x,y live and — device held, sync on —
hovers the pen at that point in real time; a click pins the point; insert
`move:` / `click:` after the editor's cursor line, or copy.
**Design**: hovering bypasses the run queue: `Rescue::pointer` keeps only
the latest position (latest wins, never a backlog), the worker sends one
HID report for it with the pad's rotation (the script's rotation state is
untouched) and never opens the device on its own; 接管设备 does. Pointer
traffic keeps the device from the idle release. UI sends ≤ 30/s, only on
change.
**Tests**: normalise (clamp, rotation-independent), pad size per
aspect/rotation, coordinate formatting, command text, insert after a line;
pointer coalescing (latest wins); ignored device test
`device_pointer_takes_the_device_and_hovers`.
**Done**: pad, readout, pin, insert / copy, sync + 接管设备; checked on the
Mac (hover readout, pin, insert after the cursor line).
**Pending**: live hover on a real device (phone was unplugged): run the
device test / try it in the app.
**Status**: In Progress

## Stage 5b: Window/rail stability
**Goal**: start/stop mirroring resizes in one SetWindowPos keeping the rail's
screen edge fixed (stage opens on the far side), clamped to the work area.
**Tests**: `resize_keep_rail` unit tests; device: right edge 1571 before,
during and after mirroring.
**Status**: Complete

## Updater (自动更新) — Stages 19–22
**Flow**: background check of the latest published GitHub Release →
download this platform's package → verify SHA-256 → swap it in place of
the running app → the title says "新版本 vX.Y.Z 已就绪 · 重启生效". Nothing
restarts by itself: the user clicks the title badge (or just quits and
reopens) and the new version starts. Drafts and pre-releases are never
offered (`/releases/latest` skips both), so a draft stays a maintainer-only
smoke test until "Publish".
**Decisions**:
- Feed: `GET api.github.com/repos/HeiSir2014/DroidRescue/releases/latest`
  (unauthenticated, 60/h — we ask on start + every 6 h). Override with env
  `DROIDRESCUE_UPDATE_FEED` (URL or local JSON file) for tests.
- Packages the updater fetches (CI adds them to the release beside the
  existing 7z/zip/dmg): `DroidRescue-X.Y.Z-win-x64.exe` (the bare exe —
  everything else is embedded) and `DroidRescue-X.Y.Z-macos-arm64.app.zip`
  (`ditto -c -k --keepParent`, so signatures/modes survive). Integrity:
  hash listed in the release's `SHA256SUMS.txt`; a mismatch is dropped.
- Swap without a helper process: Windows can rename a running exe →
  `DroidRescue.exe` → `DroidRescue.exe.old`, new file → `DroidRescue.exe`;
  macOS moves the running `.app` aside the same way. `.old` is removed on
  the next start. Not writable (Program Files, translocated/read-only
  `.app`, dmg) → no swap; the badge offers 下载页 instead.
- HTTP: one small blocking client on a background thread (`ureq` with the
  OS TLS stack: schannel / Security.framework — system trust store, no
  bundled certs); `serde_json` for the release JSON; `sha2` for hashing.
- Never during work: no swap and no restart while mirroring or a rescue
  run is active — the restart button says why it is disabled.
- Settings: 自动检查更新 (on by default, persisted) + 立即检查 + last result.

## Stage 19: Release check
**Goal**: `update.rs` decides "is there something newer for me, and which
file": version parse/compare (`X.Y.Z`, `v` prefix, pre-release suffix →
not offered), release JSON → (version, notes URL, my asset URL, sums URL),
`SHA256SUMS.txt` parse. CI uploads the two updater packages.
**Tests**: compare (0.2.0 < 0.10.0, equal → none, older → none, `-rc1` →
none); asset pick per platform (missing asset → "manual" result); sums
parse (two-space and `*` binary-mode lines); feed override reads a file.
**Done**: `src/update.rs` (+ ignored `real_feed_answers`: our feed → 404 →
up to date; `cli/cli` with a fake 0.0.1 → offered, over schannel); CI
uploads `*.exe` / `*.app.zip`.
**Status**: Complete

## Stage 20: Download, verify, swap
**Goal**: stream the asset to `<exe>.new` with progress, verify SHA-256,
swap (Windows exe rename; macOS unzip with `ditto -x -k` next to the app,
then move); cleanup `.old` / stale `.new` on start; re-check skips a
version already staged.
**Tests**: swap in a temp dir (old renamed, new in place, rerun is
idempotent); hash mismatch leaves the app untouched and deletes `.new`;
unwritable dir → manual result; ignored network test against the real
release (`DROIDRESCUE_FAKE_VERSION=0.0.1`).
**Status**: Not Started

## Stage 21: Title badge + settings
**Goal**: caption (Windows) / sidebar brand row + native title (macOS)
show a green pill beside the version tag: 检查中 is silent; 下载中 37%;
"新版本 v0.3.0 · 重启更新" (click → restart: spawn the new exe/app, exit
cleanly through `end_mirror_now`); failures only in the log. Settings page
row: switch, 立即检查, "已是最新 · 2 分钟前" / error text.
**Tests**: `window_title` with a staged update; badge text per state;
restart disabled while mirroring/running; settings persistence.
**Status**: Not Started

## Stage 22: End-to-end
**Goal**: publish a real release (v0.3.0) and update a v0.2.x build to it
on Windows and on the Mac; README/CHANGELOG/使用说明 explain it.
**Tests**: manual: old build → badge appears → restart → new version in
the title; `.old` gone on the following start.
**Status**: Not Started
