//! macOS zero-copy video: a VideoToolbox CVPixelBuffer (IOSurface-backed
//! NV12) becomes two wgpu textures — Y as R8Unorm, CbCr as Rg8Unorm — that
//! alias the decoder's memory through CVMetalTextureCache. Nothing is
//! copied; the NV12 shader in video.rs samples them directly, like the
//! D3D12 shared-texture path on Windows.

use crate::decoder_vt::PixelBuffer;
use eframe::egui_wgpu::wgpu;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLTexture, MTLTextureType};
use std::ffi::c_void;
use wgpu::hal::api::Metal;

type CfRef = *mut c_void;

/// `MTLPixelFormatR8Unorm` / `MTLPixelFormatRG8Unorm`.
const MTL_R8_UNORM: usize = 10;
const MTL_RG8_UNORM: usize = 30;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CfRef);
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVMetalTextureCacheCreate(
        allocator: CfRef,
        cache_attributes: CfRef,
        metal_device: *mut c_void,
        texture_attributes: CfRef,
        out: *mut CfRef,
    ) -> i32;
    fn CVMetalTextureCacheCreateTextureFromImage(
        allocator: CfRef,
        cache: CfRef,
        image: CfRef,
        texture_attributes: CfRef,
        pixel_format: usize,
        width: usize,
        height: usize,
        plane: usize,
        out: *mut CfRef,
    ) -> i32;
    fn CVMetalTextureCacheFlush(cache: CfRef, options: u64);
    fn CVMetalTextureGetTexture(texture: CfRef) -> *mut c_void;
    fn CVPixelBufferGetWidthOfPlane(buffer: CfRef, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(buffer: CfRef, plane: usize) -> usize;
}

/// A CVMetalTextureCache bound to wgpu's Metal device.
pub struct TextureCache(CfRef);

// SAFETY: CVMetalTextureCache is thread-safe; it lives in egui-wgpu's
// callback resources, used from the render thread.
unsafe impl Send for TextureCache {}
unsafe impl Sync for TextureCache {}

/// One imported frame: the wgpu textures and everything they alias, kept
/// alive together until the GPU is done with the frame.
pub struct Imported {
    pub y: wgpu::Texture,
    pub uv: wgpu::Texture,
    metal_textures: [CfRef; 2],
    _buffer: PixelBuffer,
}

// SAFETY: as TextureCache; the CF objects are only released in Drop.
unsafe impl Send for Imported {}
unsafe impl Sync for Imported {}

impl Drop for Imported {
    fn drop(&mut self) {
        for texture in self.metal_textures {
            // SAFETY: created by CVMetalTextureCacheCreateTextureFromImage.
            unsafe { CFRelease(texture) };
        }
    }
}

impl TextureCache {
    /// `None` when wgpu does not run on Metal.
    pub fn new(device: &wgpu::Device) -> Option<TextureCache> {
        // SAFETY: read-only access to the live Metal device; the cache keeps
        // its own reference to it.
        unsafe {
            let hal = device.as_hal::<Metal>()?;
            let mtl_device = Retained::as_ptr(hal.raw_device()).cast_mut().cast();
            let mut cache: CfRef = std::ptr::null_mut();
            let status = CVMetalTextureCacheCreate(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                mtl_device,
                std::ptr::null_mut(),
                &mut cache,
            );
            (status == 0 && !cache.is_null()).then_some(TextureCache(cache))
        }
    }

    /// Wrap both planes of `buffer` as wgpu textures (no copy).
    pub fn import(&self, device: &wgpu::Device, buffer: &PixelBuffer) -> Result<Imported, String> {
        let mut metal_textures: [CfRef; 2] = [std::ptr::null_mut(); 2];
        let mut wrapped = Vec::with_capacity(2);
        let planes = [
            (MTL_R8_UNORM, wgpu::TextureFormat::R8Unorm),
            (MTL_RG8_UNORM, wgpu::TextureFormat::Rg8Unorm),
        ];
        for (plane, (mtl_format, format)) in planes.into_iter().enumerate() {
            // SAFETY: live buffer and cache; the CVMetalTexture is kept in
            // `Imported` (released in Drop) for as long as the wgpu texture
            // that aliases its MTLTexture.
            let texture = unsafe {
                let (width, height) = (
                    CVPixelBufferGetWidthOfPlane(buffer.as_ptr(), plane),
                    CVPixelBufferGetHeightOfPlane(buffer.as_ptr(), plane),
                );
                let mut cv_texture: CfRef = std::ptr::null_mut();
                let status = CVMetalTextureCacheCreateTextureFromImage(
                    std::ptr::null_mut(),
                    self.0,
                    buffer.as_ptr(),
                    std::ptr::null_mut(),
                    mtl_format,
                    width,
                    height,
                    plane,
                    &mut cv_texture,
                );
                if status != 0 || cv_texture.is_null() {
                    release_all(&metal_textures);
                    return Err(format!("无法把解码画面映射为 Metal 纹理 ({status})"));
                }
                metal_textures[plane] = cv_texture;
                let raw =
                    CVMetalTextureGetTexture(cv_texture).cast::<ProtocolObject<dyn MTLTexture>>();
                let Some(raw) = Retained::retain(raw) else {
                    release_all(&metal_textures);
                    return Err("Metal 纹理为空".into());
                };
                let size = wgpu::Extent3d {
                    width: width as u32,
                    height: height as u32,
                    depth_or_array_layers: 1,
                };
                let hal_texture = wgpu::hal::metal::Device::texture_from_raw(
                    raw,
                    format,
                    MTLTextureType::Type2D,
                    1,
                    1,
                    wgpu::hal::CopyExtent {
                        width: size.width,
                        height: size.height,
                        depth: 1,
                    },
                );
                device.create_texture_from_hal::<Metal>(
                    hal_texture,
                    &wgpu::TextureDescriptor {
                        label: Some("droidrescue-video-surface"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    },
                )
            };
            wrapped.push(texture);
        }
        let uv = wrapped.pop().expect("two planes");
        let y = wrapped.pop().expect("two planes");
        Ok(Imported {
            y,
            uv,
            metal_textures,
            _buffer: buffer.clone(),
        })
    }

    /// Let the cache recycle textures no longer referenced.
    pub fn flush(&self) {
        // SAFETY: live cache.
        unsafe { CVMetalTextureCacheFlush(self.0, 0) }
    }
}

/// SAFETY (caller): non-null entries are owned CVMetalTextures.
unsafe fn release_all(textures: &[CfRef; 2]) {
    for &t in textures.iter().filter(|t| !t.is_null()) {
        unsafe { CFRelease(t) };
    }
}

impl Drop for TextureCache {
    fn drop(&mut self) {
        // SAFETY: created by CVMetalTextureCacheCreate, released once.
        unsafe { CFRelease(self.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::Picture;

    fn metal_device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });
        let adapter = pollster_block_on(instance.request_adapter(&Default::default()))
            .expect("a Metal adapter");
        pollster_block_on(adapter.request_device(&Default::default())).expect("device")
    }

    /// Minimal executor: wgpu's native futures resolve without a reactor.
    fn pollster_block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        // SAFETY: the vtable's functions do nothing with the null data pointer.
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let Poll::Ready(out) = future.as_mut().poll(&mut cx) {
                return out;
            }
            std::thread::yield_now();
        }
    }

    /// Read a texture back (row padding removed).
    fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        bpp: u32,
    ) -> Vec<u8> {
        let (w, h) = (texture.width(), texture.height());
        let padded = (w * bpp).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(padded * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            texture.size(),
        );
        queue.submit([encoder.finish()]);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = buffer.slice(..).get_mapped_range();
        data.chunks(padded as usize)
            .flat_map(|row| &row[..(w * bpp) as usize])
            .copied()
            .collect()
    }

    /// The imported planes hold exactly what the CPU path reads back.
    #[test]
    fn imported_planes_match_the_cpu_read_back() {
        let (device, queue) = metal_device();
        let cache = TextureCache::new(&device).expect("Metal texture cache");

        let surface = crate::decoder_vt::tests::decode_fixture(true).remove(0);
        let Picture::Surface { buffer, .. } = &surface.picture else {
            panic!("surface picture expected");
        };
        let imported = cache.import(&device, buffer).unwrap();
        assert_eq!(imported.y.size().width, 320);
        assert_eq!(imported.uv.size().width, 160);

        let cpu = crate::decoder_vt::tests::decode_fixture(false).remove(0);
        let Picture::Cpu { planes, .. } = &cpu.picture else {
            panic!("CPU picture expected");
        };
        assert_eq!(read_back(&device, &queue, &imported.y, 1), planes[0]);
        let uv = read_back(&device, &queue, &imported.uv, 2);
        let (u, v): (Vec<u8>, Vec<u8>) = uv.as_chunks::<2>().0.iter().map(|&[a, b]| (a, b)).unzip();
        assert_eq!(u, planes[1]);
        assert_eq!(v, planes[2]);
    }
}
