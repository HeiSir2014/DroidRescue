//! Hardware H.264 decoding: Media Foundation's H.264 MFT on a D3D11 device
//! (DXVA), output copied GPU→GPU into a ring of shared NV12 textures that the
//! wgpu/DX12 renderer samples directly. No pixel ever touches the CPU.

use crate::decoder::{Decoder, Frame, Picture};
use std::mem::ManuallyDrop;
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11Query, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter,
    IDXGIFactory4, IDXGIResource1,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample, IMFTransform,
    MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_LOW_LATENCY, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE,
    MF_VERSION, MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::core::{Interface, PCWSTR};

/// Shared textures in flight: the renderer samples one while we write the next.
const POOL: usize = 3;

struct Slot {
    texture: ID3D11Texture2D,
    handle: HANDLE,
}

pub struct MfDecoder {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    _manager: IMFDXGIDeviceManager,
    mft: IMFTransform,
    query: ID3D11Query,
    query_decode: ID3D11Query,
    slots: Vec<Slot>,
    slot_size: (u32, u32),
    next: usize,
    pool_id: u64,
    visible: (u32, u32),
    /// EMA (ms): ProcessInput, ProcessOutput, GPU decode done, copy done.
    timing: [f32; 4],
}

fn ema(slot: &mut f32, since: Instant) {
    let ms = since.elapsed().as_secs_f32() * 1000.0;
    *slot = if *slot == 0.0 {
        ms
    } else {
        *slot * 0.9 + ms * 0.1
    };
}

// SAFETY: all COM objects live in the MTA and the decoder is used by one
// thread at a time (it is moved into the demux thread).
unsafe impl Send for MfDecoder {}

fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl MfDecoder {
    /// `adapter_luid` must be the GPU wgpu renders on (shared handles are per-adapter).
    pub fn new(adapter_luid: i64) -> Result<Self, String> {
        // SAFETY: plain COM / D3D11 / MF initialisation; every result is checked.
        unsafe {
            // S_FALSE / RPC_E_CHANGED_MODE are fine: some apartment already exists.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).map_err(err("MFStartup"))?;

            let factory: IDXGIFactory4 = CreateDXGIFactory1().map_err(err("CreateDXGIFactory1"))?;
            let luid = LUID {
                LowPart: adapter_luid as u32,
                HighPart: (adapter_luid >> 32) as i32,
            };
            let adapter: IDXGIAdapter = factory
                .EnumAdapterByLuid(luid)
                .map_err(err("EnumAdapterByLuid"))?;

            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(err("D3D11CreateDevice"))?;
            let device: ID3D11Device = device.ok_or("D3D11 device missing")?;
            let context: ID3D11DeviceContext = context.ok_or("D3D11 context missing")?;
            // Media Foundation touches the device from its own threads.
            let mt: ID3D11Multithread = device.cast().map_err(err("ID3D11Multithread"))?;
            let _ = mt.SetMultithreadProtected(true);

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)
                .map_err(err("MFCreateDXGIDeviceManager"))?;
            let manager: IMFDXGIDeviceManager = manager.ok_or("DXGI device manager missing")?;
            manager
                .ResetDevice(&device, token)
                .map_err(err("ResetDevice"))?;

            let mft: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(err("H.264 MFT"))?;
            let attrs = mft.GetAttributes().map_err(err("MFT attributes"))?;
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err("H.264 MFT 不支持 D3D11".into());
            }
            // Output each frame as soon as it is decoded (no reorder delay).
            attrs
                .SetUINT32(&MF_LOW_LATENCY, 1)
                .map_err(err("MF_LOW_LATENCY"))?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .map_err(err("SET_D3D_MANAGER"))?;

            let input = MFCreateMediaType().map_err(err("MFCreateMediaType"))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(err("input major"))?;
            input
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(err("input subtype"))?;
            input
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(err("input interlace"))?;
            mft.SetInputType(0, &input, 0)
                .map_err(err("SetInputType"))?;
            select_nv12(&mft)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(err("BEGIN_STREAMING"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(err("START_OF_STREAM"))?;

            let event = D3D11_QUERY_DESC {
                Query: D3D11_QUERY_EVENT,
                MiscFlags: 0,
            };
            let (mut query, mut query_decode) = (None, None);
            device
                .CreateQuery(&event, Some(&mut query))
                .map_err(err("CreateQuery"))?;
            device
                .CreateQuery(&event, Some(&mut query_decode))
                .map_err(err("CreateQuery"))?;

            Ok(Self {
                device,
                context,
                _manager: manager,
                mft,
                query: query.ok_or("query missing")?,
                query_decode: query_decode.ok_or("query missing")?,
                slots: Vec::new(),
                slot_size: (0, 0),
                next: 0,
                pool_id: 0,
                visible: (0, 0),
                timing: [0.0; 4],
            })
        }
    }

    /// (Re)create the shared NV12 ring for a new decoded texture size.
    fn ensure_slots(&mut self, size: (u32, u32)) -> Result<(), String> {
        if self.slot_size == size && !self.slots.is_empty() {
            return Ok(());
        }
        self.close_slots();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                as u32,
        };
        for _ in 0..POOL {
            // SAFETY: valid descriptor; the handle is closed in close_slots.
            unsafe {
                let mut texture = None;
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut texture))
                    .map_err(err("CreateTexture2D NV12"))?;
                let texture: ID3D11Texture2D = texture.ok_or("texture missing")?;
                let resource: IDXGIResource1 = texture.cast().map_err(err("IDXGIResource1"))?;
                let handle = resource
                    .CreateSharedHandle(
                        None,
                        DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                        PCWSTR::null(),
                    )
                    .map_err(err("CreateSharedHandle"))?;
                self.slots.push(Slot { texture, handle });
            }
        }
        self.slot_size = size;
        self.pool_id = next_pool_id();
        Ok(())
    }

    fn close_slots(&mut self) {
        for slot in self.slots.drain(..) {
            // SAFETY: handle came from CreateSharedHandle and is closed once.
            unsafe {
                let _ = CloseHandle(slot.handle);
            }
        }
    }

    /// Block until all GPU work submitted on our context so far has finished.
    fn wait_gpu(&self, query: &ID3D11Query) {
        // SAFETY: event query on our own device/context.
        unsafe {
            self.context.End(query);
            self.context.Flush();
            let mut done = windows::core::BOOL(0);
            while self
                .context
                .GetData(
                    query,
                    Some((&raw mut done).cast()),
                    size_of_val(&done) as u32,
                    0,
                )
                .is_ok()
                && done.0 == 0
            {
                std::thread::yield_now();
            }
        }
    }

    /// Copy the decoder's output slice into the next shared texture.
    fn publish(
        &mut self,
        sample: &IMFSample,
        received_at: Instant,
        on_frame: &mut dyn FnMut(Frame),
    ) -> Result<(), String> {
        // SAFETY: COM calls on live objects; the copy completes before we return.
        unsafe {
            let buffer = sample
                .GetBufferByIndex(0)
                .map_err(err("GetBufferByIndex"))?;
            let dxgi: IMFDXGIBuffer = buffer.cast().map_err(err("IMFDXGIBuffer"))?;
            let mut raw = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)
                .map_err(err("GetResource"))?;
            let source = ID3D11Texture2D::from_raw(raw);
            let index = dxgi
                .GetSubresourceIndex()
                .map_err(err("GetSubresourceIndex"))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            source.GetDesc(&mut desc);
            let size = (desc.Width, desc.Height);
            self.ensure_slots(size)?;

            // Diagnostics: how long until the hardware decode itself is done…
            let gpu_start = Instant::now();
            self.wait_gpu(&self.query_decode);
            ema(&mut self.timing[2], gpu_start);

            // …then the copy into the shared texture. Waiting here guarantees
            // the DX12 side never reads a half-written texture.
            let copy_start = Instant::now();
            let slot = &self.slots[self.next];
            self.next = (self.next + 1) % POOL;
            self.context
                .CopySubresourceRegion(&slot.texture, 0, 0, 0, 0, &source, index, None);
            self.wait_gpu(&self.query);
            let handle = slot.handle.0 as isize;
            ema(&mut self.timing[3], copy_start);

            let (w, h) = if self.visible.0 > 0 {
                self.visible
            } else {
                size
            };
            on_frame(Frame {
                width: w,
                height: h,
                picture: Picture::Shared {
                    handle,
                    pool: self.pool_id,
                    texture_size: size,
                },
                received_at,
                decoded_at: Instant::now(),
            });
        }
        Ok(())
    }

    fn drain(
        &mut self,
        received_at: Instant,
        on_frame: &mut dyn FnMut(Frame),
    ) -> Result<(), String> {
        loop {
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let out_start = Instant::now();
            // SAFETY: `out` is a valid one-element array; we take ownership of
            // whatever the MFT put into it right after the call.
            let result = unsafe { self.mft.ProcessOutput(0, &mut out, &mut status) };
            if result.is_ok() {
                ema(&mut self.timing[1], out_start);
            }
            let sample = unsafe { ManuallyDrop::take(&mut out[0].pSample) };
            drop(unsafe { ManuallyDrop::take(&mut out[0].pEvents) });
            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        self.publish(&sample, received_at, on_frame)?;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => select_nv12(&self.mft)?,
                Err(e) => return Err(format!("ProcessOutput: {e}")),
            }
        }
    }

    fn make_sample(packet: &[u8]) -> Result<IMFSample, String> {
        // SAFETY: the buffer is locked for exactly `packet.len()` bytes while copying.
        unsafe {
            let buffer =
                MFCreateMemoryBuffer(packet.len() as u32).map_err(err("MFCreateMemoryBuffer"))?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None).map_err(err("Lock"))?;
            std::ptr::copy_nonoverlapping(packet.as_ptr(), ptr, packet.len());
            buffer.Unlock().map_err(err("Unlock"))?;
            buffer
                .SetCurrentLength(packet.len() as u32)
                .map_err(err("SetCurrentLength"))?;
            let sample = MFCreateSample().map_err(err("MFCreateSample"))?;
            sample.AddBuffer(&buffer).map_err(err("AddBuffer"))?;
            Ok(sample)
        }
    }
}

/// Pick NV12 among the decoder's output types (again after a stream change).
fn select_nv12(mft: &IMFTransform) -> Result<(), String> {
    for i in 0.. {
        // SAFETY: enumerating output types until the MFT reports no more.
        let Ok(t) = (unsafe { mft.GetOutputAvailableType(0, i) }) else {
            return Err("H.264 MFT 不提供 NV12 输出".into());
        };
        if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.is_ok_and(|g| g == MFVideoFormat_NV12) {
            return unsafe { mft.SetOutputType(0, &t, 0) }.map_err(err("SetOutputType NV12"));
        }
    }
    unreachable!()
}

fn next_pool_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Decoder for MfDecoder {
    fn name(&self) -> &'static str {
        "D3D11 zero-copy"
    }

    fn set_visible_size(&mut self, width: u32, height: u32) {
        self.visible = (width, height);
    }

    fn detail(&self) -> Option<String> {
        let [i, o, g, c] = self.timing;
        Some(format!("in {i:.1} out {o:.1} hw {g:.1} copy {c:.1} ms"))
    }

    fn decode(
        &mut self,
        packet: &[u8],
        received_at: Instant,
        on_frame: &mut dyn FnMut(Frame),
    ) -> Result<(), String> {
        let sample = Self::make_sample(packet)?;
        let in_start = Instant::now();
        // SAFETY: sample is a valid IMFSample.
        match unsafe { self.mft.ProcessInput(0, &sample, 0) } {
            Ok(()) => ema(&mut self.timing[0], in_start),
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                self.drain(received_at, on_frame)?;
                unsafe { self.mft.ProcessInput(0, &sample, 0) }.map_err(err("ProcessInput"))?;
            }
            Err(e) => return Err(format!("ProcessInput: {e}")),
        }
        self.drain(received_at, on_frame)
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        // SAFETY: tearing down objects we own, in reverse order of creation.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            self.close_slots();
            let _ = MFShutdown();
        }
    }
}
