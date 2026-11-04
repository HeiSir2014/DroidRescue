//! Draws the latest decoded frame inside egui via an egui_wgpu paint callback.
//! YUV→RGB happens in the fragment shader. Two input paths:
//! * I420 planes uploaded from the CPU decoder;
//! * zero-copy: shared D3D11 NV12 textures imported into wgpu (DX12) once and
//!   sampled directly (Plane0 = Y as R8, Plane1 = UV as RG8).

use crate::decoder::{self, Frame, Picture};
use crate::mirror::Shared;
use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Shared by both pipelines: full-screen triangle + BT.601 conversion.
const COMMON: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    // One oversized triangle covering the viewport.
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var out: VsOut;
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

// params: x = full range (1) / limited (0), zw = crop scale
fn to_rgb(y_in: f32, u_in: f32, v_in: f32, full_range: f32) -> vec4<f32> {
    var y = y_in;
    var u = u_in - 0.5;
    var v = v_in - 0.5;
    if (full_range < 0.5) {
        y = (y - 16.0 / 255.0) * (255.0 / 219.0);
        u = u * (255.0 / 224.0);
        v = v * (255.0 / 224.0);
    }
    // BT.601 (what Android encoders signal by default)
    let rgb = vec3<f32>(y + 1.402 * v, y - 0.344136 * u - 0.714136 * v, y + 1.772 * u);
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

const I420: &str = r#"
@group(0) @binding(0) var tex_y: texture_2d<f32>;
@group(0) @binding(1) var tex_u: texture_2d<f32>;
@group(0) @binding(2) var tex_v: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> params: vec4<f32>;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv * params.zw;
    return to_rgb(textureSample(tex_y, samp, uv).r, textureSample(tex_u, samp, uv).r,
                  textureSample(tex_v, samp, uv).r, params.x);
}
"#;

const NV12: &str = r#"
@group(0) @binding(0) var tex_y: texture_2d<f32>;
@group(0) @binding(1) var tex_uv: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: vec4<f32>;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv * params.zw;
    let c = textureSample(tex_uv, samp, uv).rg;
    return to_rgb(textureSample(tex_y, samp, uv).r, c.x, c.y, params.x);
}
"#;

struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

struct Planes {
    size: (u32, u32),
    textures: [wgpu::Texture; 3],
    bind_group: wgpu::BindGroup,
}

#[derive(Clone, Copy, PartialEq)]
enum Current {
    None,
    I420,
    Nv12(isize),
}

struct Resources {
    i420: Pipeline,
    /// Only when the device can import NV12 (DX12 + TEXTURE_FORMAT_NV12).
    nv12: Option<Pipeline>,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    params_value: [f32; 4],
    planes: Option<Planes>,
    /// Shared handle → bind group; reset when the decoder's pool changes.
    imported: HashMap<isize, wgpu::BindGroup>,
    pool: u64,
    current: Current,
}

/// Adapter LUID for the zero-copy decoder, if wgpu runs on DX12 with NV12.
pub fn zero_copy_adapter(rs: &egui_wgpu::RenderState) -> Option<i64> {
    if !rs
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
    {
        return None;
    }
    // SAFETY: read-only query on the live DX12 device.
    unsafe {
        let hal = rs.device.as_hal::<wgpu::hal::api::Dx12>()?;
        let luid = hal.raw_device().GetAdapterLuid();
        Some((i64::from(luid.HighPart) << 32) | i64::from(luid.LowPart))
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn pipeline(rs: &egui_wgpu::RenderState, label: &str, fragment: &str, textures: u32) -> Pipeline {
    let device = &rs.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(format!("{COMMON}{fragment}").into()),
    });
    let mut entries: Vec<_> = (0..textures).map(texture_entry).collect();
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: textures,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    });
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: textures + 1,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(&layout)],
        ..Default::default()
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(rs.target_format.into())],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    Pipeline { pipeline, layout }
}

/// Create the pipelines once and park them in egui-wgpu's callback resources.
pub fn install(rs: &egui_wgpu::RenderState) {
    let device = &rs.device;
    let nv12 = rs
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
        .then(|| pipeline(rs, "droidrescue-video-nv12", NV12, 2));
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("droidrescue-video"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("droidrescue-video-params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    rs.renderer.write().callback_resources.insert(Resources {
        i420: pipeline(rs, "droidrescue-video-i420", I420, 3),
        nv12,
        sampler,
        params,
        params_value: [f32::NAN; 4],
        planes: None,
        imported: HashMap::new(),
        pool: 0,
        current: Current::None,
    });
}

/// Paint callback: takes a new frame if one arrived, then draws.
pub struct VideoCallback {
    pub shared: Arc<Shared>,
}

impl VideoCallback {
    pub fn shape(rect: egui::Rect, shared: Arc<Shared>) -> egui::Shape {
        egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
            rect,
            VideoCallback { shared },
        ))
    }
}

impl egui_wgpu::CallbackTrait for VideoCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<Resources>() else {
            return Vec::new();
        };
        let frame = self.shared.latest.lock().unwrap().take();
        if let Some(frame) = frame {
            if let Err(e) = accept(device, queue, res, &frame) {
                *self.shared.ended.lock().unwrap() = Some(format!("视频渲染失败: {e}"));
            }
            let lag = frame.decoded_at.elapsed().as_secs_f32() * 1000.0;
            let mut st = self.shared.stats.lock().unwrap();
            st.present_ms = if st.present_ms == 0.0 {
                lag
            } else {
                st.present_ms * 0.9 + lag * 0.1
            };
            st.presented = self.shared.frame_seq.load(Ordering::Acquire);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(res) = resources.get::<Resources>() else {
            return;
        };
        let (pipeline, bind_group) = match res.current {
            Current::I420 => match &res.planes {
                Some(p) => (&res.i420.pipeline, &p.bind_group),
                None => return,
            },
            Current::Nv12(handle) => match (&res.nv12, res.imported.get(&handle)) {
                (Some(p), Some(bg)) => (&p.pipeline, bg),
                _ => return,
            },
            Current::None => return,
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn set_params(queue: &wgpu::Queue, res: &mut Resources, value: [f32; 4]) {
    if res.params_value != value {
        queue.write_buffer(&res.params, 0, &f32x4_bytes(value));
        res.params_value = value;
    }
}

fn accept(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    res: &mut Resources,
    frame: &Frame,
) -> Result<(), String> {
    match &frame.picture {
        Picture::Cpu {
            planes,
            strides,
            full_range,
        } => {
            upload_i420(
                device,
                queue,
                res,
                (frame.width, frame.height),
                planes,
                strides,
            );
            set_params(
                queue,
                res,
                [if *full_range { 1.0 } else { 0.0 }, 0.0, 1.0, 1.0],
            );
            res.current = Current::I420;
        }
        Picture::Shared {
            handle,
            pool,
            texture_size,
        } => {
            if res.pool != *pool {
                res.imported.clear(); // old decoder's textures are gone
                res.pool = *pool;
            }
            if !res.imported.contains_key(handle) {
                let bind_group = import_nv12(device, res, *handle, *texture_size)?;
                res.imported.insert(*handle, bind_group);
            }
            let [sx, sy] = decoder::crop_scale((frame.width, frame.height), *texture_size);
            set_params(queue, res, [0.0, 0.0, sx, sy]);
            res.current = Current::Nv12(*handle);
        }
    }
    Ok(())
}

/// Open the decoder's shared NV12 texture on wgpu's DX12 device (once per handle).
fn import_nv12(
    device: &wgpu::Device,
    res: &Resources,
    handle: isize,
    size: (u32, u32),
) -> Result<wgpu::BindGroup, String> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

    let nv12 = res.nv12.as_ref().ok_or("渲染设备不支持 NV12")?;
    let extent = wgpu::Extent3d {
        width: size.0,
        height: size.1,
        depth_or_array_layers: 1,
    };
    // SAFETY: `handle` is a live NT handle to an NV12 texture on this adapter;
    // the opened resource holds its own reference.
    let texture = unsafe {
        let hal = device
            .as_hal::<wgpu::hal::api::Dx12>()
            .ok_or("渲染后端不是 DX12")?;
        let mut resource: Option<ID3D12Resource> = None;
        hal.raw_device()
            .OpenSharedHandle(HANDLE(handle as *mut _), &mut resource)
            .map_err(|e| format!("OpenSharedHandle: {e}"))?;
        let resource = resource.ok_or("OpenSharedHandle 返回空")?;
        let hal_texture = wgpu::hal::dx12::Device::texture_from_raw(
            resource,
            wgpu::TextureFormat::NV12,
            wgpu::TextureDimension::D2,
            extent,
            1,
            1,
        );
        device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("video-nv12-shared"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::NV12,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )
    };
    let plane = |format, aspect| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(format),
            aspect,
            ..Default::default()
        })
    };
    let y = plane(wgpu::TextureFormat::R8Unorm, wgpu::TextureAspect::Plane0);
    let uv = plane(wgpu::TextureFormat::Rg8Unorm, wgpu::TextureAspect::Plane1);
    Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("video-nv12"),
        layout: &nv12.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&y),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&uv),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&res.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: res.params.as_entire_binding(),
            },
        ],
    }))
}

fn upload_i420(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    res: &mut Resources,
    size: (u32, u32),
    planes: &[Vec<u8>; 3],
    strides: &[u32; 3],
) {
    if res.planes.as_ref().is_none_or(|p| p.size != size) {
        res.planes = Some(create_planes(device, res, size));
    }
    let tex = &res.planes.as_ref().unwrap().textures;
    let chroma = (size.0.div_ceil(2), size.1.div_ceil(2));
    for (i, texture) in tex.iter().enumerate() {
        let (w, h) = if i == 0 { size } else { chroma };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &planes[i],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(strides[i]),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
}

fn create_planes(device: &wgpu::Device, res: &Resources, size: (u32, u32)) -> Planes {
    let chroma = (size.0.div_ceil(2), size.1.div_ceil(2));
    let make = |(w, h): (u32, u32), label| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    };
    let textures = [
        make(size, "video-y"),
        make(chroma, "video-u"),
        make(chroma, "video-v"),
    ];
    let views: Vec<_> = textures
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("video-i420"),
        layout: &res.i420.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&views[0]),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&views[1]),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&views[2]),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&res.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: res.params.as_entire_binding(),
            },
        ],
    });
    Planes {
        size,
        textures,
        bind_group,
    }
}

fn f32x4_bytes(v: [f32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, f) in v.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    out
}
