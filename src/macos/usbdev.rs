//! macOS access to the rescue device: AOA control requests on endpoint 0
//! through IOKit (nusb). No interface is claimed, so whatever else uses the
//! device (adb, Android File Transfer) keeps working.

use crate::usbhost::device_id;
use crate::usbid::UsbTarget;
use nusb::MaybeFuture;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use std::time::Duration;

/// Error when no device of a rescue profile is attached.
pub const NOT_FOUND: &str = "没有找到可用的设备（设备需插着；机型配置需声明它的 usb = VID:PID:MI）";

/// Control-transfer timeout.
const TIMEOUT: Duration = Duration::from_millis(500);

/// Does this device carry one of the targets?
fn is_target(info: &nusb::DeviceInfo, targets: &[UsbTarget]) -> bool {
    targets.iter().any(|t| {
        t.vid == info.vendor_id()
            && t.pid == info.product_id()
            && t.interface
                .is_none_or(|mi| info.interfaces().any(|i| i.interface_number() == mi))
    })
}

fn enumerate(targets: &[UsbTarget]) -> Result<Vec<nusb::DeviceInfo>, String> {
    Ok(nusb::list_devices()
        .wait()
        .map_err(|e| format!("枚举 USB 设备失败: {e}"))?
        .filter(|info| is_target(info, targets))
        .collect())
}

fn serial(info: &nusb::DeviceInfo) -> Option<String> {
    info.serial_number()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Serials of rescue devices plugged in (enumeration only).
pub fn present_serials(targets: &[UsbTarget]) -> Vec<String> {
    enumerate(targets)
        .unwrap_or_default()
        .iter()
        .filter_map(serial)
        .collect()
}

/// Device ids (`USB\VID_2D95&PID_6012\<serial>`) of the rescue devices —
/// the PID tells USB modes apart.
pub fn present_parents(targets: &[UsbTarget]) -> Vec<String> {
    enumerate(targets)
        .unwrap_or_default()
        .iter()
        .map(device_id)
        .collect()
}

pub struct UsbDevice {
    dev: nusb::Device,
    pub serial: String,
    /// Device id, e.g. `USB\VID_2D95&PID_6012\<serial>`.
    pub parent: String,
}

/// bmRequestType → nusb's (type, recipient).
fn split_request_type(request_type: u8) -> Result<(ControlType, Recipient), String> {
    let control_type = match (request_type >> 5) & 0b11 {
        0 => ControlType::Standard,
        1 => ControlType::Class,
        2 => ControlType::Vendor,
        _ => return Err(format!("无效的 USB 请求类型 0x{request_type:02X}")),
    };
    let recipient = match request_type & 0x1F {
        0 => Recipient::Device,
        1 => Recipient::Interface,
        2 => Recipient::Endpoint,
        _ => Recipient::Other,
    };
    Ok((control_type, recipient))
}

impl UsbDevice {
    /// The single attached rescue device (optionally a specific serial).
    pub fn open(serial_wanted: Option<&str>, targets: &[UsbTarget]) -> Result<UsbDevice, String> {
        let mut all = enumerate(targets)?;
        if let Some(want) = serial_wanted {
            all.retain(|info| serial(info).as_deref() == Some(want));
        }
        let info = match all.len() {
            0 => return Err(NOT_FOUND.into()),
            1 => all.remove(0),
            n => {
                return Err(format!(
                    "检测到 {n} 台可救援的设备，为避免误操作已停止：请只连接要救援的设备"
                ));
            }
        };
        let dev = info
            .open()
            .wait()
            .map_err(|e| format!("打开 USB 设备失败: {e}"))?;
        Ok(UsbDevice {
            dev,
            serial: serial(&info).unwrap_or_default(),
            parent: device_id(&info),
        })
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
        let (control_type, recipient) = split_request_type(request_type)?;
        let failed = |e: nusb::transfer::TransferError| format!("USB 控制请求 {request} 失败: {e}");
        if request_type & 0x80 != 0 {
            let got = self
                .dev
                .control_in(
                    ControlIn {
                        control_type,
                        recipient,
                        request,
                        value,
                        index,
                        length: data.len() as u16,
                    },
                    TIMEOUT,
                )
                .wait()
                .map_err(failed)?;
            let n = got.len().min(data.len());
            data[..n].copy_from_slice(&got[..n]);
            Ok(n)
        } else {
            self.dev
                .control_out(
                    ControlOut {
                        control_type,
                        recipient,
                        request,
                        value,
                        index,
                        data,
                    },
                    TIMEOUT,
                )
                .wait()
                .map_err(failed)?;
            Ok(data.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aoa_request_types_split_into_type_and_recipient() {
        // AOA: vendor, device; IN = 0xC0, OUT = 0x40.
        for rt in [0xC0, 0x40] {
            assert!(matches!(
                split_request_type(rt),
                Ok((ControlType::Vendor, Recipient::Device))
            ));
        }
        assert!(split_request_type(0x60).is_err(), "reserved type");
    }
}
