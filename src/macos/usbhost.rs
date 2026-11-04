//! macOS USB host side of the rescue: nothing to install. IOKit lets a
//! user process send device requests to any USB device, so every target is
//! ready as is — no driver binding, no elevation, no helper process.
//!
//! [`survey`] reports devices and interfaces in the same shape as the
//! Windows scanner (instance ids `USB\VID_…&PID_…[&MI_..]\<location>`,
//! Windows-style driver names for classes), so profiles, 设备体检 and the
//! profile auto-pick work unchanged.

use crate::usbid::{UsbEntry, UsbTarget};
use nusb::MaybeFuture;

/// Only parsed by the Windows build; kept so `main` reads the same.
pub const INSTALL_FLAG: &str = "--install-winusb";

/// Driver-like label for a USB class, named like the Windows services so
/// the shared 设备体检 filters apply (hubs, HID, audio, video, Bluetooth).
fn class_service(class: u8, subclass: u8, protocol: u8) -> String {
    match (class, subclass, protocol) {
        (0xFF, 0x42, 0x01) => "adb".into(),
        (0x06, _, _) => "WpdMtp".into(),
        (0x01, _, _) => "usbaudio".into(),
        (0x03, _, _) => "HidUsb".into(),
        (0x08, _, _) => "USBSTOR".into(),
        (0x09, _, _) => "usbhub".into(),
        (0x0E, _, _) => "usbvideo".into(),
        (0xE0, _, _) => "BTHUSB".into(),
        _ => format!("class {class:02X}/{subclass:02X}/{protocol:02X}"),
    }
}

/// The instance part of a synthesised id: the serial, else the location.
fn instance(info: &nusb::DeviceInfo) -> String {
    info.serial_number()
        .filter(|s| !s.is_empty() && !s.contains('\\'))
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:08X}", info.location_id()))
}

/// `USB\VID_2D95&PID_6012\<instance>` — what Windows calls the parent.
pub fn device_id(info: &nusb::DeviceInfo) -> String {
    format!(
        "USB\\VID_{:04X}&PID_{:04X}\\{}",
        info.vendor_id(),
        info.product_id(),
        instance(info)
    )
}

/// Every present USB device and interface (read-only).
pub fn survey() -> Vec<UsbEntry> {
    let Ok(devices) = nusb::list_devices().wait() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for info in devices {
        let (vid, pid, inst) = (info.vendor_id(), info.product_id(), instance(&info));
        let product = info.product_string().unwrap_or("").to_string();
        out.push(UsbEntry {
            id: device_id(&info),
            service: if info.class() == 0x09 {
                "usbhub".into()
            } else {
                "usbccgp".into()
            },
            description: product.clone(),
            ready: true,
        });
        for iface in info.interfaces() {
            let service = class_service(iface.class(), iface.subclass(), iface.protocol());
            let description = match (iface.interface_string(), service.as_str()) {
                (Some(s), _) => s.to_string(),
                (None, "adb") => "ADB Interface".into(),
                (None, "WpdMtp") => "MTP".into(),
                (None, _) => product.clone(),
            };
            out.push(UsbEntry {
                id: format!(
                    "USB\\VID_{vid:04X}&PID_{pid:04X}&MI_{:02X}\\{inst}&{:02X}",
                    iface.interface_number(),
                    iface.interface_number()
                ),
                service,
                description,
                ready: true,
            });
        }
    }
    out
}

/// Nothing is ever unbound on macOS.
pub fn unbound_targets(_targets: &[UsbTarget]) -> Vec<String> {
    Vec::new()
}

pub fn install_elevated(_targets: &[UsbTarget]) -> Result<(), String> {
    Err("macOS 不需要安装驱动".into())
}

pub fn install(_targets: &[UsbTarget]) -> Result<Vec<String>, String> {
    install_elevated(&[]).map(|()| Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_map_to_the_shared_driver_names() {
        assert_eq!(class_service(0xFF, 0x42, 0x01), "adb");
        assert_eq!(class_service(0x06, 0x01, 0x01), "WpdMtp");
        assert_eq!(class_service(0x09, 0, 0), "usbhub");
        assert_eq!(class_service(0xFF, 0xFF, 0x00), "class FF/FF/00");
    }

    #[test]
    fn synthesised_interface_ids_parse_as_targets() {
        let id = format!(
            "USB\\VID_{:04X}&PID_{:04X}&MI_{:02X}\\{}&{:02X}",
            0x2D95, 0x6012, 1, "SER", 1
        );
        assert_eq!(
            UsbTarget::of_instance(&id),
            UsbTarget::parse("2D95:6012:01").ok()
        );
    }
}
