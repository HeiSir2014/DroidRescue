//! One mirroring session against scrcpy-server (forward tunnel).
//!
//! Latency rules: TCP_NODELAY on both sockets, decode on the receiving thread,
//! and a single latest-frame slot — a slow UI drops frames, it never queues.

use crate::adb::Tools;
use crate::audio;
use crate::control;
#[cfg(windows)]
use crate::decoder::FfmpegDecoder;
use crate::decoder::{Decoder, Frame};
#[cfg(windows)]
use crate::decoder_mf::MfDecoder;
use crate::protocol::{self, Header};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DEVICE_SERVER_PATH: &str = "/data/local/tmp/droidrescue-server.jar";
const CONNECT_ATTEMPTS: u32 = 100;
const CONNECT_RETRY: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub struct Options {
    pub max_size: u32,
    pub bit_rate: u32,
    pub max_fps: u32,
    pub stay_awake: bool,
    /// Physical panel off while mirroring (restored on stop).
    pub turn_screen_off: bool,
    /// Forward device audio to this PC (device itself goes silent).
    pub audio: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_size: 1920,
            bit_rate: 12_000_000,
            max_fps: 0,
            stay_awake: true,
            turn_screen_off: true,
            audio: true,
        }
    }
}

/// Server launch arguments (after the class name).
pub fn server_args(version: &str, scid: u32, o: &Options) -> Vec<String> {
    let mut args = vec![
        version.to_string(),
        format!("scid={scid:08x}"),
        "log_level=info".into(),
        // Raw PCM: no encode/decode latency; ~1.5 Mbps is nothing over USB.
        if o.audio {
            "audio_codec=raw".into()
        } else {
            "audio=false".into()
        },
        "tunnel_forward=true".into(),
        format!("max_size={}", o.max_size),
        format!("video_bit_rate={}", o.bit_rate),
    ];
    if o.max_fps > 0 {
        args.push(format!("max_fps={}", o.max_fps));
    }
    if o.stay_awake {
        args.push("stay_awake=true".into());
    }
    args
}

/// Rolling stats for the HUD.
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub frames: u64,
    pub bytes: u64,
    /// Exponential moving averages (ms): packet received → decoded,
    /// decoded → uploaded to the GPU for the next present.
    pub decode_ms: f32,
    /// Time spent inside `Decoder::decode` itself. Much lower than
    /// `decode_ms` means the decoder holds frames back (reorder/pipelining).
    pub call_ms: f32,
    pub present_ms: f32,
    /// Sequence number of the last frame that reached the GPU.
    pub presented: u64,
}

/// State shared between the session threads and the UI.
pub struct Shared {
    pub device_name: Mutex<String>,
    pub video_size: Mutex<(u32, u32)>,
    pub latest: Mutex<Option<Frame>>,
    pub frame_seq: AtomicU64,
    pub stats: Mutex<Stats>,
    pub ended: Mutex<Option<String>>,
    /// Physical display currently powered off by us.
    pub screen_off: AtomicBool,
    /// Audio jitter-buffer fill (ms), for the HUD; negative = no audio.
    pub audio_ms: Mutex<f32>,
    /// Active decoder backend, for the HUD.
    pub decoder: Mutex<&'static str>,
    /// Decoder's own per-stage timing, if it reports one.
    pub decoder_detail: Mutex<Option<String>>,
    stopping: AtomicBool,
}

pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;
pub type RepaintFn = Arc<dyn Fn() + Send + Sync>;

pub struct Session {
    pub shared: Arc<Shared>,
    control: Mutex<TcpStream>,
    video: TcpStream,
    audio: Option<TcpStream>,
    server: Child,
    tools: Tools,
    serial: Option<String>,
    port: u16,
    threads: Vec<JoinHandle<()>>,
    clipboard_seq: AtomicU64,
}

impl Session {
    /// Blocking: pushes the server, opens the tunnel and connects (≈1 s).
    /// `gpu_luid`: adapter wgpu renders on, when it can import shared NV12
    /// textures (zero-copy); `None` → CPU decoding.
    pub fn start(
        tools: &Tools,
        serial: Option<&str>,
        opts: &Options,
        gpu_luid: Option<i64>,
        log: LogFn,
        repaint: RepaintFn,
    ) -> Result<Session, String> {
        let server_path = tools.server.to_string_lossy().to_string();
        let push = tools
            .adb(serial, &["push", &server_path, DEVICE_SERVER_PATH])
            .map_err(|e| e.to_string())?;
        if !push.status.success() {
            return Err(format!(
                "推送 server 失败: {}",
                String::from_utf8_lossy(&push.stderr).trim()
            ));
        }

        // Tunnels left behind by a killed session would leak ports forever.
        if let (Some(ser), Ok(list)) = (serial, tools.adb(None, &["forward", "--list"])) {
            for local in
                crate::adb::stale_scrcpy_forwards(&String::from_utf8_lossy(&list.stdout), ser)
            {
                let _ = tools.adb(serial, &["forward", "--remove", &local]);
            }
        }

        let scid = random_scid();
        let socket = format!("localabstract:scrcpy_{scid:08x}");
        let fwd = tools
            .adb(serial, &["forward", "tcp:0", &socket])
            .map_err(|e| e.to_string())?;
        let port: u16 = String::from_utf8_lossy(&fwd.stdout)
            .trim()
            .parse()
            .map_err(|_| {
                format!(
                    "adb forward 失败: {}",
                    String::from_utf8_lossy(&fwd.stderr).trim()
                )
            })?;

        let mut cmd =
            format!("CLASSPATH={DEVICE_SERVER_PATH} app_process / com.genymobile.scrcpy.Server");
        for a in server_args(&tools.server_version, scid, opts) {
            cmd.push(' ');
            cmd.push_str(&a);
        }
        let mut server = tools
            .adb_command(serial, &["shell", &cmd])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 server 失败: {e}"))?;
        for stream in [
            server
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            server
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let log = log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        log(line);
                    }
                }
            });
        }

        let cleanup = |server: &mut Child| {
            let _ = server.kill();
            let _ = tools.adb(serial, &["forward", "--remove", &format!("tcp:{port}")]);
        };

        // The forward tunnel accepts before the server listens: a connection is
        // only real once the server's dummy byte arrives.
        let mut video = match connect_with_dummy(port) {
            Ok(s) => s,
            Err(e) => {
                cleanup(&mut server);
                return Err(e);
            }
        };
        // Socket order is fixed by the server: video, audio, control.
        let audio_sock = if opts.audio {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => Some(s),
                Err(e) => {
                    cleanup(&mut server);
                    return Err(format!("连接音频通道失败: {e}"));
                }
            }
        } else {
            None
        };
        let control = match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => {
                cleanup(&mut server);
                return Err(format!("连接控制通道失败: {e}"));
            }
        };
        let _ = control.set_nodelay(true);

        let mut name = [0u8; protocol::DEVICE_NAME_LEN];
        let mut codec = [0u8; 4];
        if let Err(e) = video
            .read_exact(&mut name)
            .and_then(|_| video.read_exact(&mut codec))
        {
            cleanup(&mut server);
            return Err(format!("读取设备信息失败: {e}"));
        }
        if u32::from_be_bytes(codec) != protocol::CODEC_H264 {
            cleanup(&mut server);
            return Err(format!(
                "不支持的视频编码 {:?}",
                String::from_utf8_lossy(&codec)
            ));
        }

        let shared = Arc::new(Shared {
            device_name: Mutex::new(protocol::parse_device_name(&name)),
            video_size: Mutex::new((0, 0)),
            latest: Mutex::new(None),
            frame_seq: AtomicU64::new(0),
            stats: Mutex::new(Stats::default()),
            ended: Mutex::new(None),
            screen_off: AtomicBool::new(false),
            audio_ms: Mutex::new(-1.0),
            decoder: Mutex::new("—"),
            decoder_detail: Mutex::new(None),
            stopping: AtomicBool::new(false),
        });

        // Every handle the threads need, before any thread starts: a failure
        // here must still kill the server and remove the tunnel.
        let clones = (|| {
            let audio = audio_sock.as_ref().map(TcpStream::try_clone).transpose()?;
            Ok::<_, std::io::Error>((video.try_clone()?, audio, control.try_clone()?))
        })();
        let (demux_sock, audio_reader, mut drain) = match clones {
            Ok(c) => c,
            Err(e) => {
                cleanup(&mut server);
                return Err(format!("复制连接句柄失败: {e}"));
            }
        };

        let mut threads = Vec::new();
        let s = shared.clone();
        let r = repaint.clone();
        let (dll_dir, demux_log) = (tools.dir.clone(), log.clone());
        threads.push(std::thread::spawn(move || {
            // Created on the thread that uses it (COM apartment / D3D11 context).
            let decoder = match open_decoder(gpu_luid, &dll_dir, &demux_log) {
                Ok(d) => d,
                Err(e) => {
                    *s.ended.lock().unwrap() = Some(format!("无法创建解码器: {e}"));
                    return r();
                }
            };
            *s.decoder.lock().unwrap() = decoder.name();
            demux_log(format!("✔ 解码器: {}", decoder.name()));
            let reason = run_demuxer(demux_sock, decoder, &s, &r);
            if !s.stopping.load(Ordering::Relaxed) {
                *s.ended.lock().unwrap() = Some(reason);
                r();
            }
        }));

        if let Some(sock) = audio_reader {
            let (s, log) = (shared.clone(), log.clone());
            threads.push(std::thread::spawn(move || run_audio(sock, &s, &log)));
        }

        // Device → client messages (clipboard etc.): drain so the socket never stalls.
        threads.push(std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while matches!(drain.read(&mut buf), Ok(n) if n > 0) {}
        }));

        let session = Session {
            shared,
            control: Mutex::new(control),
            video,
            audio: audio_sock,
            server,
            tools: tools.clone(),
            serial: serial.map(str::to_string),
            port,
            threads,
            clipboard_seq: AtomicU64::new(1),
        };
        if opts.turn_screen_off {
            session.set_screen_off(true);
        }
        Ok(session)
    }

    /// Fire-and-forget control message.
    pub fn send(&self, msg: &[u8]) {
        let _ = self.control.lock().unwrap().write_all(msg);
    }

    pub fn press_key(&self, keycode: u32) {
        self.send(&control::keycode(control::KeyAction::Down, keycode, 0, 0));
        self.send(&control::keycode(control::KeyAction::Up, keycode, 0, 0));
    }

    /// Power the physical panel off/on; mirroring is unaffected.
    pub fn set_screen_off(&self, off: bool) {
        self.send(&control::set_display_power(!off));
        self.shared.screen_off.store(off, Ordering::Relaxed);
    }

    pub fn paste(&self, text: &str) {
        let seq = self.clipboard_seq.fetch_add(1, Ordering::Relaxed);
        self.send(&control::set_clipboard(seq, true, text));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Relaxed);
        // Leave the tablet usable: panel back on before we disconnect.
        if self.shared.screen_off.load(Ordering::Relaxed) {
            self.set_screen_off(false);
        }
        let _ = self.video.shutdown(Shutdown::Both);
        if let Some(a) = &self.audio {
            let _ = a.shutdown(Shutdown::Both);
        }
        let _ = self.control.lock().unwrap().shutdown(Shutdown::Both);
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = self.tools.adb(
            self.serial.as_deref(),
            &["forward", "--remove", &format!("tcp:{}", self.port)],
        );
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn connect_with_dummy(port: u16) -> Result<TcpStream, String> {
    for _ in 0..CONNECT_ATTEMPTS {
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = s.set_nodelay(true);
            let mut dummy = [0u8; 1];
            if s.read_exact(&mut dummy).is_ok() {
                return Ok(s);
            }
        }
        std::thread::sleep(CONNECT_RETRY);
    }
    Err("连接 scrcpy-server 超时（平板上 server 未启动？）".into())
}

/// macOS: VideoToolbox; zero-copy into Metal when the renderer can import
/// IOSurfaces (`gpu_luid` is then `Some`), CPU upload otherwise.
#[cfg(target_os = "macos")]
fn open_decoder(
    gpu_luid: Option<i64>,
    _dll_dir: &std::path::Path,
    _log: &LogFn,
) -> Result<Box<dyn Decoder>, String> {
    Ok(Box::new(crate::decoder_vt::VtDecoder::new(
        gpu_luid.is_some(),
    )))
}

/// Hardware zero-copy when possible, FFmpeg on the CPU otherwise.
#[cfg(windows)]
fn open_decoder(
    gpu_luid: Option<i64>,
    dll_dir: &std::path::Path,
    log: &LogFn,
) -> Result<Box<dyn Decoder>, String> {
    if let Some(luid) = gpu_luid {
        match MfDecoder::new(luid) {
            Ok(d) => return Ok(Box::new(d)),
            Err(e) => log(format!("✘ 硬件解码不可用，回退 CPU: {e}")),
        }
    }
    Ok(Box::new(FfmpegDecoder::new(dll_dir)?))
}

/// Reads headers + packets until the socket closes; returns the reason.
fn run_demuxer(
    mut sock: TcpStream,
    mut decoder: Box<dyn Decoder>,
    shared: &Shared,
    repaint: &RepaintFn,
) -> String {
    let mut header = [0u8; protocol::HEADER_LEN];
    let mut config: Vec<u8> = Vec::new();
    let mut payload: Vec<u8> = Vec::new();
    loop {
        if let Err(e) = sock.read_exact(&mut header) {
            return format!("视频流结束: {e}");
        }
        match protocol::parse_header(&header) {
            Header::Session { width, height } => {
                *shared.video_size.lock().unwrap() = (width, height);
                decoder.set_visible_size(width, height);
            }
            Header::Media {
                config: is_config,
                len,
                ..
            } => {
                payload.resize(len as usize, 0);
                if let Err(e) = sock.read_exact(&mut payload) {
                    return format!("视频流结束: {e}");
                }
                let received_at = Instant::now();
                if is_config {
                    // Like scrcpy: SPS/PPS are prepended to the next frame.
                    config = std::mem::take(&mut payload);
                    continue;
                }
                let data = if config.is_empty() {
                    &payload[..]
                } else {
                    config.extend_from_slice(&payload);
                    &config[..]
                };
                let bytes = data.len() as u64;
                let result = decoder.decode(data, received_at, &mut |frame| {
                    {
                        let mut st = shared.stats.lock().unwrap();
                        let ms = frame
                            .decoded_at
                            .duration_since(frame.received_at)
                            .as_secs_f32()
                            * 1000.0;
                        st.decode_ms = if st.frames == 0 {
                            ms
                        } else {
                            st.decode_ms * 0.9 + ms * 0.1
                        };
                        st.frames += 1;
                    }
                    *shared.latest.lock().unwrap() = Some(frame);
                    shared.frame_seq.fetch_add(1, Ordering::Release);
                    repaint();
                });
                {
                    let mut st = shared.stats.lock().unwrap();
                    st.bytes += bytes;
                    let call = received_at.elapsed().as_secs_f32() * 1000.0;
                    st.call_ms = if st.call_ms == 0.0 {
                        call
                    } else {
                        st.call_ms * 0.9 + call * 0.1
                    };
                }
                if let Some(detail) = decoder.detail() {
                    *shared.decoder_detail.lock().unwrap() = Some(detail);
                }
                config.clear();
                if let Err(e) = result {
                    return format!("解码失败: {e}");
                }
            }
        }
    }
}

/// Audio socket: codec id, then 12-byte headers + raw PCM payloads.
fn run_audio(mut sock: TcpStream, shared: &Shared, log: &LogFn) {
    let mut codec = [0u8; 4];
    if sock.read_exact(&mut codec).is_err() {
        return;
    }
    if u32::from_be_bytes(codec) != protocol::CODEC_RAW {
        log(format!(
            "✘ 平板未提供音频（codec {:08x}），仅投视频",
            u32::from_be_bytes(codec)
        ));
        return;
    }
    // The cpal stream is owned by this thread and dropped when the socket closes.
    let player = match audio::Player::open(log.clone()) {
        Ok(p) => p,
        Err(e) => {
            log(format!("✘ 电脑无法播放声音，仅投视频: {e}"));
            // Keep reading: an unread socket would stall the server's audio.
            let mut sink = [0u8; 8192];
            while matches!(sock.read(&mut sink), Ok(n) if n > 0) {}
            return;
        }
    };
    log(format!("✔ 声音输出 → {}", player.description));
    let mut header = [0u8; protocol::HEADER_LEN];
    let mut payload = Vec::new();
    loop {
        if sock.read_exact(&mut header).is_err() {
            break;
        }
        let Header::Media { config, len, .. } = protocol::parse_header(&header) else {
            continue;
        };
        payload.resize(len as usize, 0);
        if sock.read_exact(&mut payload).is_err() {
            break;
        }
        if config {
            continue; // raw PCM has no codec config
        }
        let mut buf = player.buffer.lock().unwrap();
        buf.push(audio::s16le_to_frames(&payload));
        *shared.audio_ms.lock().unwrap() = buf.buffered_ms();
    }
    *shared.audio_ms.lock().unwrap() = -1.0;
}

/// 31-bit random id (server requires it to fit a signed int).
fn random_scid() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos ^ std::process::id().rotate_left(13)) & 0x7fff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_args_use_forward_tunnel_and_hex_scid() {
        let args = server_args("5.0.1", 0x1a2b, &Options::default());
        assert_eq!(args[0], "5.0.1");
        assert!(args.contains(&"scid=00001a2b".to_string()));
        assert!(args.contains(&"tunnel_forward=true".to_string()));
        assert!(args.contains(&"audio_codec=raw".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("max_fps")));
    }

    #[test]
    fn audio_can_be_disabled() {
        let opts = Options {
            audio: false,
            ..Options::default()
        };
        let args = server_args("5.0.1", 1, &opts);
        assert!(args.contains(&"audio=false".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("audio_codec")));
    }

    #[test]
    fn defaults_forward_audio_and_blank_the_panel() {
        let o = Options::default();
        assert!(o.audio && o.turn_screen_off);
    }

    #[test]
    fn scid_is_31_bit() {
        assert!(random_scid() <= 0x7fff_ffff);
    }
}
