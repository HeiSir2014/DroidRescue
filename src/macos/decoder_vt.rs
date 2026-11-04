//! macOS H.264 decoding with VideoToolbox (hardware on every Apple Silicon
//! and recent Intel Mac). The decoded NV12 picture is read back into
//! planar YUV for the existing upload path; zero-copy IOSurface → Metal is
//! the next step (IMPLEMENTATION_PLAN.md, Stage 11).
//!
//! A deliberately small hand-written FFI surface (CoreFoundation /
//! CoreMedia / CoreVideo / VideoToolbox C APIs) instead of generated
//! bindings: every call and ownership rule is visible here.

use crate::decoder::{Decoder, Frame, Picture, annexb_nals};
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::Instant;

type OsStatus = i32;
type CfRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CmTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

type OutputCallback = unsafe extern "C" fn(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: OsStatus,
    info_flags: u32,
    image: CfRef,
    pts: CmTime,
    duration: CmTime,
);

#[repr(C)]
struct OutputCallbackRecord {
    callback: OutputCallback,
    refcon: *mut c_void,
}

/// Opaque CFDictionary{Key,Value}CallBacks; only their address is used.
#[repr(C)]
struct DictCallbacks {
    _opaque: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorNull: CfRef;
    static kCFBooleanTrue: CfRef;
    static kCFTypeDictionaryKeyCallBacks: DictCallbacks;
    static kCFTypeDictionaryValueCallBacks: DictCallbacks;
    fn CFRetain(cf: CfRef) -> CfRef;
    fn CFRelease(cf: CfRef);
    fn CFDictionaryCreate(
        allocator: CfRef,
        keys: *const CfRef,
        values: *const CfRef,
        count: isize,
        key_callbacks: *const DictCallbacks,
        value_callbacks: *const DictCallbacks,
    ) -> CfRef;
}

/// CFDictionary with CF-typed keys/values (both retained by the dictionary).
///
/// SAFETY (caller): every key and value is a live CF object.
unsafe fn cf_dictionary(pairs: &[(CfRef, CfRef)]) -> CfRef {
    let keys: Vec<CfRef> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<CfRef> = pairs.iter().map(|p| p.1).collect();
    unsafe {
        CFDictionaryCreate(
            std::ptr::null_mut(),
            keys.as_ptr(),
            values.as_ptr(),
            pairs.len() as isize,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    }
}

/// Destination attributes for zero-copy: IOSurface-backed and usable as
/// Metal textures. Caller releases the dictionary.
fn metal_image_attributes() -> CfRef {
    // SAFETY: CoreVideo / CoreFoundation constants; the empty dictionary is
    // retained by the outer one and released here.
    unsafe {
        let iosurface_props = cf_dictionary(&[]);
        let attrs = cf_dictionary(&[
            (kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue),
            (kCVPixelBufferIOSurfacePropertiesKey, iosurface_props),
        ]);
        CFRelease(iosurface_props);
        attrs
    }
}

/// A retained CVPixelBuffer (IOSurface-backed NV12) handed to the renderer.
pub struct PixelBuffer(CfRef);

// SAFETY: CVPixelBuffers are reference-counted and thread-safe to retain,
// release and read; the renderer only reads.
unsafe impl Send for PixelBuffer {}

impl PixelBuffer {
    pub fn as_ptr(&self) -> CfRef {
        self.0
    }

    pub fn size(&self) -> (u32, u32) {
        // SAFETY: live buffer.
        unsafe {
            (
                CVPixelBufferGetWidth(self.0) as u32,
                CVPixelBufferGetHeight(self.0) as u32,
            )
        }
    }
}

impl Clone for PixelBuffer {
    fn clone(&self) -> Self {
        // SAFETY: live buffer; the clone owns one more reference.
        PixelBuffer(unsafe { CFRetain(self.0) })
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference.
        unsafe { CFRelease(self.0) }
    }
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CfRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_length_size: i32,
        out: *mut CfRef,
    ) -> OsStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CfRef,
        memory: *mut c_void,
        block_length: usize,
        block_allocator: CfRef,
        custom_source: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut CfRef,
    ) -> OsStatus;
    fn CMSampleBufferCreateReady(
        allocator: CfRef,
        data: CfRef,
        format: CfRef,
        num_samples: isize,
        num_timing: isize,
        timing: *const c_void,
        num_sizes: isize,
        sizes: *const usize,
        out: *mut CfRef,
    ) -> OsStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferMetalCompatibilityKey: CfRef;
    static kCVPixelBufferIOSurfacePropertiesKey: CfRef;
    fn CVPixelBufferLockBaseAddress(buffer: CfRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CfRef, flags: u64) -> i32;
    fn CVPixelBufferGetWidth(buffer: CfRef) -> usize;
    fn CVPixelBufferGetHeight(buffer: CfRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(buffer: CfRef) -> u32;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: CfRef, plane: usize) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CfRef, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(buffer: CfRef, plane: usize) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTDecompressionSessionCreate(
        allocator: CfRef,
        format: CfRef,
        decoder_spec: CfRef,
        image_attributes: CfRef,
        callback: *const OutputCallbackRecord,
        out: *mut CfRef,
    ) -> OsStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: CfRef,
        sample: CfRef,
        flags: u32,
        frame_refcon: *mut c_void,
        info_out: *mut u32,
    ) -> OsStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: CfRef) -> OsStatus;
    fn VTDecompressionSessionInvalidate(session: CfRef);
}

/// `'420v'` / `'420f'`: bi-planar 4:2:0 (NV12), video / full range.
const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
const NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
const LOCK_READ_ONLY: u64 = 1;

const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

/// Decoded pictures handed over by the output callback (retained).
#[derive(Default)]
struct Outputs(Mutex<Vec<Result<CfRef, OsStatus>>>);

unsafe extern "C" fn on_output(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: OsStatus,
    _info_flags: u32,
    image: CfRef,
    _pts: CmTime,
    _duration: CmTime,
) {
    // SAFETY: refcon is the `Outputs` boxed in VtDecoder, alive for the
    // session's lifetime; a delivered image is retained before keeping it.
    let outputs = unsafe { &*refcon.cast::<Outputs>() };
    let item = if status == 0 && !image.is_null() {
        Ok(unsafe { CFRetain(image) })
    } else {
        Err(status)
    };
    outputs.0.lock().unwrap().push(item);
}

/// AVCC sample: each NAL prefixed with its 4-byte big-endian length.
fn to_avcc<'a>(nals: impl Iterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

fn nal_type(nal: &[u8]) -> u8 {
    nal[0] & 0x1F
}

pub struct VtDecoder {
    session: CfRef,
    format: CfRef,
    /// SPS + PPS the session was built for (rebuilt when they change, e.g.
    /// on rotation).
    params: Option<(Vec<u8>, Vec<u8>)>,
    outputs: Box<Outputs>,
    /// Hand IOSurface-backed buffers to the renderer (Metal) instead of
    /// reading them back to the CPU.
    zero_copy: bool,
}

// SAFETY: the CF objects are only used from the thread that owns the decoder.
unsafe impl Send for VtDecoder {}

impl VtDecoder {
    pub fn new(zero_copy: bool) -> VtDecoder {
        VtDecoder {
            session: std::ptr::null_mut(),
            format: std::ptr::null_mut(),
            params: None,
            outputs: Box::default(),
            zero_copy,
        }
    }

    fn release_session(&mut self) {
        // SAFETY: both were created by us and are released once.
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
            }
            if !self.format.is_null() {
                CFRelease(self.format);
            }
        }
        self.session = std::ptr::null_mut();
        self.format = std::ptr::null_mut();
    }

    fn configure(&mut self, sps: &[u8], pps: &[u8]) -> Result<(), String> {
        self.release_session();
        let pointers = [sps.as_ptr(), pps.as_ptr()];
        let sizes = [sps.len(), pps.len()];
        // SAFETY: parameter sets outlive the call; out-pointers are ours.
        unsafe {
            let status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                std::ptr::null_mut(),
                2,
                pointers.as_ptr(),
                sizes.as_ptr(),
                4,
                &mut self.format,
            );
            if status != 0 {
                return Err(format!("H.264 参数集无效 (CoreMedia {status})"));
            }
            let record = OutputCallbackRecord {
                callback: on_output,
                refcon: (&raw const *self.outputs).cast_mut().cast(),
            };
            // The decoder's native NV12; for zero-copy also Metal-compatible
            // IOSurfaces.
            let attributes = if self.zero_copy {
                metal_image_attributes()
            } else {
                std::ptr::null_mut()
            };
            let status = VTDecompressionSessionCreate(
                std::ptr::null_mut(),
                self.format,
                std::ptr::null_mut(),
                attributes,
                &record,
                &mut self.session,
            );
            if !attributes.is_null() {
                CFRelease(attributes);
            }
            if status != 0 {
                self.session = std::ptr::null_mut();
                return Err(format!("无法创建 VideoToolbox 解码会话 ({status})"));
            }
        }
        self.params = Some((sps.to_vec(), pps.to_vec()));
        Ok(())
    }

    /// Decode one AVCC sample synchronously; outputs land in `self.outputs`.
    fn decode_sample(&mut self, sample: &mut [u8]) -> Result<(), String> {
        // SAFETY: the block buffer wraps `sample` without copying or freeing
        // it (kCFAllocatorNull) and is released before `sample` goes away;
        // decoding is synchronous (no async flag) and waited for.
        unsafe {
            let mut block: CfRef = std::ptr::null_mut();
            let status = CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null_mut(),
                sample.as_mut_ptr().cast(),
                sample.len(),
                kCFAllocatorNull,
                std::ptr::null(),
                0,
                sample.len(),
                0,
                &mut block,
            );
            if status != 0 {
                return Err(format!("CMBlockBuffer 失败 ({status})"));
            }
            let size = sample.len();
            let mut buffer: CfRef = std::ptr::null_mut();
            let status = CMSampleBufferCreateReady(
                std::ptr::null_mut(),
                block,
                self.format,
                1,
                0,
                std::ptr::null(),
                1,
                &size,
                &mut buffer,
            );
            CFRelease(block);
            if status != 0 {
                return Err(format!("CMSampleBuffer 失败 ({status})"));
            }
            let mut info = 0u32;
            let status = VTDecompressionSessionDecodeFrame(
                self.session,
                buffer,
                0,
                std::ptr::null_mut(),
                &mut info,
            );
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            CFRelease(buffer);
            if status != 0 {
                return Err(format!("VideoToolbox 解码失败 ({status})"));
            }
        }
        Ok(())
    }
}

/// Full range for `'420f'`, limited for `'420v'`; anything else is not NV12.
fn nv12_full_range(format: u32) -> Result<bool, String> {
    match format {
        NV12_VIDEO => Ok(false),
        NV12_FULL => Ok(true),
        other => Err(format!(
            "不支持的像素格式 {:?}",
            String::from_utf8_lossy(&other.to_be_bytes())
        )),
    }
}

/// Zero-copy: hand the IOSurface-backed buffer itself to the renderer.
///
/// SAFETY (caller): `buffer` wraps a live CVPixelBuffer.
unsafe fn surface(buffer: PixelBuffer) -> Result<(u32, u32, Picture), String> {
    let full_range = nv12_full_range(unsafe { CVPixelBufferGetPixelFormatType(buffer.as_ptr()) })?;
    let (width, height) = buffer.size();
    Ok((width, height, Picture::Surface { buffer, full_range }))
}

/// Copy an NV12 pixel buffer into planar Y / U / V.
///
/// SAFETY (caller): `image` is a live CVPixelBuffer.
unsafe fn read_nv12(image: CfRef) -> Result<(u32, u32, Picture), String> {
    unsafe {
        let full_range = nv12_full_range(CVPixelBufferGetPixelFormatType(image))?;
        if CVPixelBufferLockBaseAddress(image, LOCK_READ_ONLY) != 0 {
            return Err("无法读取解码画面".into());
        }
        let (width, height) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        let plane = |i: usize| {
            let rows = CVPixelBufferGetHeightOfPlane(image, i);
            let stride = CVPixelBufferGetBytesPerRowOfPlane(image, i);
            let base = CVPixelBufferGetBaseAddressOfPlane(image, i);
            (std::slice::from_raw_parts(base, rows * stride), stride)
        };
        let (y, y_stride) = plane(0);
        let (uv, uv_stride) = plane(1);
        let planes = split_nv12(y, y_stride, uv, uv_stride, width, height);
        CVPixelBufferUnlockBaseAddress(image, LOCK_READ_ONLY);
        let (cw, w) = (width.div_ceil(2) as u32, width as u32);
        Ok((
            w,
            height as u32,
            Picture::Cpu {
                planes,
                strides: [w, cw, cw],
                full_range,
            },
        ))
    }
}

/// NV12 (Y plane + interleaved UV) → tightly packed I420 planes.
fn split_nv12(
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    width: usize,
    height: usize,
) -> [Vec<u8>; 3] {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut luma = Vec::with_capacity(width * height);
    for row in y.chunks(y_stride).take(height) {
        luma.extend_from_slice(&row[..width]);
    }
    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for row in uv.chunks(uv_stride).take(ch) {
        for &[cb, cr] in row[..cw * 2].as_chunks::<2>().0 {
            u.push(cb);
            v.push(cr);
        }
    }
    [luma, u, v]
}

impl Decoder for VtDecoder {
    fn name(&self) -> &'static str {
        if self.zero_copy {
            "VideoToolbox → Metal (zero-copy)"
        } else {
            "VideoToolbox (CPU upload)"
        }
    }

    fn decode(
        &mut self,
        packet: &[u8],
        received_at: Instant,
        on_frame: &mut dyn FnMut(Frame),
    ) -> Result<(), String> {
        let nals = annexb_nals(packet);
        let find = |t| nals.iter().find(|n| nal_type(n) == t).copied();
        if let (Some(sps), Some(pps)) = (find(NAL_SPS), find(NAL_PPS))
            && self.params.as_ref() != Some(&(sps.to_vec(), pps.to_vec()))
        {
            self.configure(sps, pps)?;
        }
        let mut sample = to_avcc(
            nals.iter()
                .copied()
                .filter(|n| !matches!(nal_type(n), NAL_SPS | NAL_PPS | NAL_AUD)),
        );
        if sample.is_empty() || self.session.is_null() {
            return Ok(()); // nothing to show yet (waiting for SPS/PPS)
        }
        self.decode_sample(&mut sample)?;
        let outputs = std::mem::take(&mut *self.outputs.0.lock().unwrap());
        let mut result = Ok(());
        for output in outputs {
            match output {
                Ok(image) => {
                    // SAFETY: retained by the callback; the reference moves
                    // into a PixelBuffer (zero-copy) or is released here.
                    let picture = if self.zero_copy {
                        unsafe { surface(PixelBuffer(image)) }
                    } else {
                        let picture = unsafe { read_nv12(image) };
                        unsafe { CFRelease(image) };
                        picture
                    };
                    match picture {
                        Ok((width, height, picture)) => on_frame(Frame {
                            width,
                            height,
                            picture,
                            received_at,
                            decoded_at: Instant::now(),
                        }),
                        Err(e) => result = Err(e),
                    }
                }
                Err(status) => result = Err(format!("VideoToolbox 输出错误 ({status})")),
            }
        }
        result
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        self.release_session();
        for image in std::mem::take(&mut *self.outputs.0.lock().unwrap())
            .into_iter()
            .flatten()
        {
            // SAFETY: retained by the callback, never released yet.
            unsafe { CFRelease(image) };
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn nv12_splits_into_planar_chroma_and_drops_row_padding() {
        // 4×2 luma with stride 6; one chroma row: U0 V0 U1 V1 (+2 padding).
        let y = [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        let uv = [10, 20, 11, 21, 0, 0];
        let [luma, u, v] = split_nv12(&y, 6, &uv, 6, 4, 2);
        assert_eq!(luma, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(u, [10, 11]);
        assert_eq!(v, [20, 21]);
    }

    #[test]
    fn avcc_prefixes_each_nal_with_its_length() {
        let out = to_avcc([&[0x65, 1, 2][..], &[0x41]].into_iter());
        assert_eq!(out, [0, 0, 0, 3, 0x65, 1, 2, 0, 0, 0, 1, 0x41]);
    }

    /// The fixture as scrcpy sends it: SPS + PPS ride with the first access
    /// unit, one slice per frame.
    fn fixture_units() -> Vec<Vec<u8>> {
        let stream = include_bytes!("../../tests/fixtures/testsrc-320x240.h264");
        let mut units: Vec<Vec<u8>> = Vec::new();
        let mut pending: Vec<u8> = Vec::new();
        for nal in annexb_nals(stream) {
            pending.extend_from_slice(&[0, 0, 0, 1]);
            pending.extend_from_slice(nal);
            if matches!(nal_type(nal), 1 | 5) {
                units.push(std::mem::take(&mut pending));
            }
        }
        units
    }

    /// Decode the 320×240, 3-frame fixture.
    pub fn decode_fixture(zero_copy: bool) -> Vec<Frame> {
        let units = fixture_units();
        let mut decoder = VtDecoder::new(zero_copy);
        let mut frames = Vec::new();
        for unit in &units {
            decoder
                .decode(unit, Instant::now(), &mut |f| frames.push(f))
                .unwrap();
        }
        assert_eq!(frames.len(), units.len());
        frames
    }

    #[test]
    fn decodes_a_real_h264_stream() {
        let frames = decode_fixture(false);
        assert_eq!(frames.len(), 3);
        let f = &frames[0];
        assert_eq!((f.width, f.height), (320, 240));
        let Picture::Cpu { planes, .. } = &f.picture else {
            panic!("CPU picture expected");
        };
        assert_eq!(planes[0].len(), 320 * 240);
        assert_eq!(planes[1].len(), 160 * 120);
        // testsrc is colourful: luma is neither black nor flat.
        let (min, max) = (
            planes[0].iter().min().unwrap(),
            planes[0].iter().max().unwrap(),
        );
        assert!(max - min > 100, "luma range {min}..{max}");
    }

    #[test]
    fn zero_copy_hands_over_iosurface_buffers() {
        let frames = decode_fixture(true);
        let Picture::Surface { buffer, .. } = &frames[0].picture else {
            panic!("surface picture expected");
        };
        assert_eq!(buffer.size(), (320, 240));
    }
}
