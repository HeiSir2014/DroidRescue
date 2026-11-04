//! H.264 decoding behind a small trait so the backend can be swapped
//! (CPU FFmpeg now; zero-copy D3D11 later).
//!
//! The FFmpeg backend loads the bundled avcodec/avutil at runtime and only
//! touches the long-stable leading fields of AVPacket / AVFrame.

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::Path;
use std::time::Instant;

/// Where the decoded pixels live.
pub enum Picture {
    /// Planar YUV 4:2:0 in system memory; row `r` of plane `i` starts at
    /// `r * strides[i]`.
    Cpu {
        planes: [Vec<u8>; 3],
        strides: [u32; 3],
        full_range: bool,
    },
    /// NV12 in a shared D3D11 texture (NT handle owned by the decoder whose
    /// id is `pool`). The visible picture is the top-left `width × height`
    /// of the (macroblock-aligned) `texture_size`.
    Shared {
        handle: isize,
        pool: u64,
        texture_size: (u32, u32),
    },
}

/// One decoded picture.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub picture: Picture,
    /// When the encoded packet finished arriving (for the latency HUD).
    pub received_at: Instant,
    /// When decoding finished.
    pub decoded_at: Instant,
}

/// Texture-coordinate scale that crops an aligned texture to the visible size.
pub fn crop_scale(visible: (u32, u32), texture: (u32, u32)) -> [f32; 2] {
    let s = |v: u32, t: u32| {
        if t == 0 {
            1.0
        } else {
            (v as f32 / t as f32).min(1.0)
        }
    };
    [s(visible.0, texture.0), s(visible.1, texture.1)]
}

pub trait Decoder: Send {
    /// Short label for the HUD.
    fn name(&self) -> &'static str;

    /// Visible video size from the latest session packet (decoders whose
    /// output is macroblock-aligned use it to crop).
    fn set_visible_size(&mut self, _width: u32, _height: u32) {}

    /// Optional per-stage timing breakdown for the HUD.
    fn detail(&self) -> Option<String> {
        None
    }

    /// Decode one Annex-B access unit; calls `on_frame` for each output picture.
    fn decode(
        &mut self,
        packet: &[u8],
        received_at: Instant,
        on_frame: &mut dyn FnMut(Frame),
    ) -> Result<(), String>;
}

// ───────────────────────── FFmpeg FFI ─────────────────────────

const AV_CODEC_ID_H264: c_int = 27;
const AV_PIX_FMT_YUV420P: c_int = 0;
const AV_PIX_FMT_YUVJ420P: c_int = 12;
const AVERROR_EAGAIN: c_int = -11; // AVERROR(EAGAIN) with the MSVC/mingw errno value

/// Leading fields of AVPacket (stable since FFmpeg 1.x).
#[repr(C)]
struct AvPacket {
    buf: *mut c_void,
    pts: i64,
    dts: i64,
    data: *mut u8,
    size: c_int,
}

/// Leading fields of AVFrame (stable since FFmpeg 1.x).
#[repr(C)]
struct AvFrame {
    data: [*mut u8; 8],
    linesize: [c_int; 8],
    extended_data: *mut *mut u8,
    width: c_int,
    height: c_int,
    nb_samples: c_int,
    format: c_int,
}

type FnFindDecoder = unsafe extern "C" fn(c_int) -> *const c_void;
type FnAllocContext = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type FnOpen2 = unsafe extern "C" fn(*mut c_void, *const c_void, *mut *mut c_void) -> c_int;
type FnFreeContext = unsafe extern "C" fn(*mut *mut c_void);
type FnPacketAlloc = unsafe extern "C" fn() -> *mut AvPacket;
type FnPacketFree = unsafe extern "C" fn(*mut *mut AvPacket);
type FnNewPacket = unsafe extern "C" fn(*mut AvPacket, c_int) -> c_int;
type FnPacketUnref = unsafe extern "C" fn(*mut AvPacket);
type FnSendPacket = unsafe extern "C" fn(*mut c_void, *const AvPacket) -> c_int;
type FnReceiveFrame = unsafe extern "C" fn(*mut c_void, *mut AvFrame) -> c_int;
type FnFrameAlloc = unsafe extern "C" fn() -> *mut AvFrame;
type FnFrameFree = unsafe extern "C" fn(*mut *mut AvFrame);
type FnFrameUnref = unsafe extern "C" fn(*mut AvFrame);
type FnDictSet =
    unsafe extern "C" fn(*mut *mut c_void, *const c_char, *const c_char, c_int) -> c_int;
type FnDictFree = unsafe extern "C" fn(*mut *mut c_void);

struct Api {
    packet_free: FnPacketFree,
    new_packet: FnNewPacket,
    packet_unref: FnPacketUnref,
    send_packet: FnSendPacket,
    receive_frame: FnReceiveFrame,
    frame_free: FnFrameFree,
    frame_unref: FnFrameUnref,
    free_context: FnFreeContext,
}

pub struct FfmpegDecoder {
    api: Api,
    ctx: *mut c_void,
    pkt: *mut AvPacket,
    frame: *mut AvFrame,
    warned_format: bool,
    // Keep the libraries loaded for as long as the function pointers live.
    _libs: Vec<libloading::Library>,
}

// SAFETY: the decoder is used from one thread at a time (moved into the demux thread).
unsafe impl Send for FfmpegDecoder {}

fn load(dir: &Path, name: &str) -> Result<libloading::Library, String> {
    let path = dir.join(name);
    // SAFETY: loading a trusted DLL that ships with this program.
    unsafe { libloading::Library::new(&path) }
        .map_err(|e| format!("加载 {} 失败: {e}", path.display()))
}

fn sym<T: Copy>(lib: &libloading::Library, name: &[u8]) -> Result<T, String> {
    // SAFETY: the type T matches the documented C signature of `name`.
    unsafe { lib.get::<T>(name) }
        .map(|s| *s)
        .map_err(|e| format!("缺少符号 {}: {e}", String::from_utf8_lossy(name)))
}

impl FfmpegDecoder {
    /// `dir` holds avutil-61.dll, swresample-7.dll and avcodec-63.dll.
    pub fn new(dir: &Path) -> Result<Self, String> {
        // Dependencies first so avcodec's imports resolve to these copies.
        let avutil = load(dir, "avutil-61.dll")?;
        let swresample = load(dir, "swresample-7.dll")?;
        let avcodec = load(dir, "avcodec-63.dll")?;

        let find_decoder: FnFindDecoder = sym(&avcodec, b"avcodec_find_decoder\0")?;
        let alloc_context: FnAllocContext = sym(&avcodec, b"avcodec_alloc_context3\0")?;
        let open2: FnOpen2 = sym(&avcodec, b"avcodec_open2\0")?;
        let packet_alloc: FnPacketAlloc = sym(&avcodec, b"av_packet_alloc\0")?;
        let frame_alloc: FnFrameAlloc = sym(&avutil, b"av_frame_alloc\0")?;
        let dict_set: FnDictSet = sym(&avutil, b"av_dict_set\0")?;
        let dict_free: FnDictFree = sym(&avutil, b"av_dict_free\0")?;
        let api = Api {
            packet_free: sym(&avcodec, b"av_packet_free\0")?,
            new_packet: sym(&avcodec, b"av_new_packet\0")?,
            packet_unref: sym(&avcodec, b"av_packet_unref\0")?,
            send_packet: sym(&avcodec, b"avcodec_send_packet\0")?,
            receive_frame: sym(&avcodec, b"avcodec_receive_frame\0")?,
            frame_free: sym(&avutil, b"av_frame_free\0")?,
            frame_unref: sym(&avutil, b"av_frame_unref\0")?,
            free_context: sym(&avcodec, b"avcodec_free_context\0")?,
        };

        // SAFETY: straightforward FFmpeg setup; every pointer is checked.
        unsafe {
            let codec = find_decoder(AV_CODEC_ID_H264);
            if codec.is_null() {
                return Err("FFmpeg 不含 H.264 解码器".into());
            }
            let ctx = alloc_context(codec);
            if ctx.is_null() {
                return Err("avcodec_alloc_context3 失败".into());
            }
            // Low latency: no frame reordering delay, slice threads only.
            let mut opts: *mut c_void = std::ptr::null_mut();
            for (k, v) in [
                ("flags", "low_delay"),
                ("thread_type", "slice"),
                ("threads", "0"),
            ] {
                let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
                dict_set(&mut opts, k.as_ptr(), v.as_ptr(), 0);
            }
            let rc = open2(ctx, codec, &mut opts);
            dict_free(&mut opts);
            let mut ctx_mut = ctx;
            if rc < 0 {
                (api.free_context)(&mut ctx_mut);
                return Err(format!("avcodec_open2 失败 ({rc})"));
            }
            let pkt = packet_alloc();
            let frame = frame_alloc();
            if pkt.is_null() || frame.is_null() {
                (api.free_context)(&mut ctx_mut);
                return Err("分配 AVPacket/AVFrame 失败".into());
            }
            Ok(Self {
                api,
                ctx,
                pkt,
                frame,
                warned_format: false,
                _libs: vec![avcodec, swresample, avutil],
            })
        }
    }

    /// Copy the current AVFrame into an owned `Frame`.
    unsafe fn take_frame(&mut self, received_at: Instant) -> Option<Frame> {
        let f = unsafe { &*self.frame };
        let full_range = match f.format {
            AV_PIX_FMT_YUV420P => false,
            AV_PIX_FMT_YUVJ420P => true,
            _ => {
                self.warned_format = true;
                return None;
            }
        };
        let (w, h) = (f.width as u32, f.height as u32);
        let rows = [h, h.div_ceil(2), h.div_ceil(2)];
        let mut planes: [Vec<u8>; 3] = Default::default();
        let mut strides = [0u32; 3];
        for i in 0..3 {
            let stride = f.linesize[i] as usize;
            let len = stride * rows[i] as usize;
            // SAFETY: FFmpeg guarantees `linesize * rows` readable bytes per plane.
            planes[i] = unsafe { std::slice::from_raw_parts(f.data[i], len) }.to_vec();
            strides[i] = stride as u32;
        }
        Some(Frame {
            width: w,
            height: h,
            picture: Picture::Cpu {
                planes,
                strides,
                full_range,
            },
            received_at,
            decoded_at: Instant::now(),
        })
    }
}

impl Decoder for FfmpegDecoder {
    fn name(&self) -> &'static str {
        "FFmpeg CPU"
    }

    fn decode(
        &mut self,
        packet: &[u8],
        received_at: Instant,
        on_frame: &mut dyn FnMut(Frame),
    ) -> Result<(), String> {
        // SAFETY: pkt/frame/ctx are valid for the decoder's lifetime.
        unsafe {
            if (self.api.new_packet)(self.pkt, packet.len() as c_int) < 0 {
                return Err("av_new_packet 失败".into());
            }
            std::ptr::copy_nonoverlapping(packet.as_ptr(), (*self.pkt).data, packet.len());
            let rc = (self.api.send_packet)(self.ctx, self.pkt);
            (self.api.packet_unref)(self.pkt);
            if rc < 0 && rc != AVERROR_EAGAIN {
                return Err(format!("avcodec_send_packet 失败 ({rc})"));
            }
            loop {
                let rc = (self.api.receive_frame)(self.ctx, self.frame);
                if rc < 0 {
                    break; // EAGAIN (needs more input) or EOF
                }
                let frame = self.take_frame(received_at);
                (self.api.frame_unref)(self.frame);
                match frame {
                    Some(f) => on_frame(f),
                    None if self.warned_format => {
                        return Err("不支持的像素格式（需要 YUV420P）".into());
                    }
                    None => {}
                }
            }
        }
        Ok(())
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        // SAFETY: each pointer was allocated by FFmpeg and is freed exactly once.
        unsafe {
            (self.api.frame_free)(&mut self.frame);
            (self.api.packet_free)(&mut self.pkt);
            (self.api.free_context)(&mut self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_scale_hides_macroblock_padding() {
        // 1348 visible rows decoded into a 1360-row (16-aligned) texture.
        let [sx, sy] = crop_scale((1920, 1348), (1920, 1360));
        assert_eq!(sx, 1.0);
        assert!((sy - 1348.0 / 1360.0).abs() < 1e-6);
        assert_eq!(crop_scale((10, 10), (0, 0)), [1.0, 1.0]);
        assert_eq!(
            crop_scale((20, 20), (10, 10)),
            [1.0, 1.0],
            "never sample outside"
        );
    }
}
