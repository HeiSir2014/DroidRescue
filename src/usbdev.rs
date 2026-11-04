//! Native WinUSB access to the tablet's MI_01 (no libusb, no filter driver).
//!
//! libusb's WinUSB backend opens *every* WinUSB interface of a composite
//! device and fails with ACCESS when the adb server holds the ADB interface
//! (MI_02) — exactly the moment we must tap "允许 USB 调试" (killing adb would
//! cancel that dialog). Opening only MI_01 through its own device interface
//! (`winusb::INTERFACE_GUID`) side-steps that; control transfers on endpoint
//! 0 through it reach the whole device, which is all AOA needs.

use crate::winusb::INTERFACE_GUID_VALUE;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Parent, CR_SUCCESS, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Devices::Usb::{
    PIPE_TRANSFER_TIMEOUT, WINUSB_INTERFACE_HANDLE, WINUSB_SETUP_PACKET, WinUsb_ControlTransfer,
    WinUsb_Free, WinUsb_Initialize, WinUsb_SetPipePolicy,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

/// Error when no tablet interface is bound to WinUSB (unplugged, or the
/// one-time setup has not been done).
pub const NOT_FOUND: &str =
    "没有找到可用的平板接口（平板需插着；首次使用需执行一次性 WinUSB 设置）";

/// Control-transfer timeout (ms).
const TIMEOUT_MS: u32 = 500;

/// `USB\VID_2D95&PID_6012\SER0123456789AB` → `SER0123456789AB`.
pub fn serial_from_parent_id(parent: &str) -> Option<String> {
    let serial = parent.rsplit('\\').next()?;
    // Windows synthesises ids like `6&10174C10&1&2` when there is no serial.
    (!serial.is_empty() && !serial.contains('&')).then(|| serial.to_string())
}

/// A tablet MI_01 interface found via its device interface GUID.
struct Found {
    path: Vec<u16>,
    serial: Option<String>,
    parent: String,
}

pub struct UsbDevice {
    file: HANDLE,
    usb: WINUSB_INTERFACE_HANDLE,
    pub serial: String,
    /// Parent device instance id, e.g. `USB\VID_2D95&PID_6012\<serial>`.
    pub parent: String,
}

// SAFETY: the handles are only used by the thread that owns the value.
unsafe impl Send for UsbDevice {}

fn last_error(what: &str) -> String {
    // SAFETY: trivial thread-local query.
    format!("{what} 失败 (Win32 错误 0x{:08X})", unsafe {
        GetLastError()
    })
}

fn device_id(devinst: u32) -> Option<String> {
    let mut buf = [0u16; 512];
    // SAFETY: buffer is large enough for any device instance id (MAX_DEVICE_ID_LEN = 200).
    (unsafe { CM_Get_Device_IDW(devinst, buf.as_mut_ptr(), buf.len() as u32, 0) } == CR_SUCCESS)
        .then(|| {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..len])
        })
}

fn enumerate() -> Result<Vec<Found>, String> {
    // SAFETY: SetupAPI enumeration with correctly sized structs; the detail
    // buffer is sized from the first call's `required` and 8-byte aligned.
    unsafe {
        let set = SetupDiGetClassDevsW(
            &INTERFACE_GUID_VALUE,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set == INVALID_HANDLE_VALUE as _ {
            return Err(last_error("枚举 WinUSB 接口"));
        }
        let mut found = Vec::new();
        let mut index = 0;
        loop {
            let mut iface: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            iface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(
                set,
                std::ptr::null(),
                &INTERFACE_GUID_VALUE,
                index,
                &mut iface,
            ) == 0
            {
                break;
            }
            index += 1;
            let mut required = 0u32;
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &iface,
                std::ptr::null_mut(),
                0,
                &mut required,
                std::ptr::null_mut(),
            );
            let mut storage = vec![0u64; (required as usize).div_ceil(8).max(1)];
            let detail = storage
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut info: SP_DEVINFO_DATA = std::mem::zeroed();
            info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(
                set,
                &iface,
                detail,
                required,
                std::ptr::null_mut(),
                &mut info,
            ) == 0
            {
                continue;
            }
            // DevicePath is a NUL-terminated string starting after cbSize.
            let path_ptr = std::ptr::addr_of!((*detail).DevicePath).cast::<u16>();
            let max = (required as usize - 4) / 2;
            let path: Vec<u16> = std::slice::from_raw_parts(path_ptr, max)
                .iter()
                .copied()
                .take_while(|&c| c != 0)
                .chain([0])
                .collect();
            let mut parent_inst = 0u32;
            let parent = if CM_Get_Parent(&mut parent_inst, info.DevInst, 0) == CR_SUCCESS {
                device_id(parent_inst).unwrap_or_default()
            } else {
                String::new()
            };
            found.push(Found {
                path,
                serial: serial_from_parent_id(&parent),
                parent,
            });
        }
        SetupDiDestroyDeviceInfoList(set);
        Ok(found)
    }
}

/// Serials of tablets currently plugged in with the rescue interface ready
/// (enumeration only — nothing is opened, so it never disturbs MTP/adb).
pub fn present_serials() -> Vec<String> {
    enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| f.serial)
        .collect()
}

/// Parent instance ids (`USB\VID_2D95&PID_6012\<serial>`) of the tablets
/// with the rescue interface ready — the PID tells USB modes apart.
pub fn present_parents() -> Vec<String> {
    enumerate()
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.parent)
        .collect()
}

impl UsbDevice {
    /// The single tablet MI_01 bound to WinUSB (optionally a specific serial).
    pub fn open(serial: Option<&str>) -> Result<UsbDevice, String> {
        let mut all = enumerate()?;
        if let Some(want) = serial {
            all.retain(|f| f.serial.as_deref() == Some(want));
        }
        let target = match all.len() {
            0 => return Err(NOT_FOUND.into()),
            1 => all.remove(0),
            n => {
                return Err(format!(
                    "检测到 {n} 台已设置的平板，为避免误操作已停止：请只连接要救援的平板"
                ));
            }
        };
        // SAFETY: path is NUL-terminated; handles are checked and freed in Drop.
        unsafe {
            let file = CreateFileW(
                target.path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            );
            if file == INVALID_HANDLE_VALUE {
                return Err(last_error("打开平板 WinUSB 接口"));
            }
            let mut usb: WINUSB_INTERFACE_HANDLE = std::ptr::null_mut();
            if WinUsb_Initialize(file, &mut usb) == 0 {
                let e = last_error("WinUsb_Initialize");
                CloseHandle(file);
                return Err(e);
            }
            let timeout = TIMEOUT_MS;
            WinUsb_SetPipePolicy(
                usb,
                0,
                PIPE_TRANSFER_TIMEOUT,
                4,
                (&raw const timeout).cast(),
            );
            Ok(UsbDevice {
                file,
                usb,
                serial: target.serial.unwrap_or_default(),
                parent: target.parent,
            })
        }
    }

    /// Control transfer on endpoint 0; returns bytes transferred.
    pub fn control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize, String> {
        let setup = WINUSB_SETUP_PACKET {
            RequestType: request_type,
            Request: request,
            Value: value,
            Index: index,
            Length: data.len() as u16,
        };
        let mut transferred = 0u32;
        // SAFETY: `data` is valid for `data.len()` bytes; synchronous call (no OVERLAPPED).
        let ok = unsafe {
            WinUsb_ControlTransfer(
                self.usb,
                setup,
                data.as_mut_ptr(),
                data.len() as u32,
                &mut transferred,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            Err(last_error(&format!("USB 控制请求 {request}")))
        } else {
            Ok(transferred as usize)
        }
    }
}

impl Drop for UsbDevice {
    fn drop(&mut self) {
        // SAFETY: both handles were created in `open` and are released once.
        unsafe {
            WinUsb_Free(self.usb);
            CloseHandle(self.file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_comes_from_the_parent_instance_id() {
        assert_eq!(
            serial_from_parent_id(r"USB\VID_2D95&PID_6012\SER0123456789AB").as_deref(),
            Some("SER0123456789AB")
        );
        assert_eq!(
            serial_from_parent_id(r"USB\VID_0000&PID_0002\6&10174C10&1&2"),
            None,
            "synthesised id"
        );
        assert_eq!(serial_from_parent_id(""), None);
    }

    #[test]
    fn guid_value_matches_the_registry_string() {
        let g = INTERFACE_GUID_VALUE;
        let s = format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{}}}",
            g.data1,
            g.data2,
            g.data3,
            g.data4[0],
            g.data4[1],
            g.data4[2..]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>()
        );
        assert_eq!(s, crate::winusb::INTERFACE_GUID);
    }
}
