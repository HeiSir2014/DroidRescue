//! 设备体检: what the PC sees of the USB devices, and whether the rescue can
//! reach them — the first thing to look at when a rescue fails on a new PC,
//! port or model, and where the `usb =` ids for a new profile come from.

use crate::usbid::{self, UsbTarget};
use crate::winusb::UsbEntry;

/// Declared in display order (most rescue-relevant first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// A profile's rescue function with WinUSB: ready.
    Ready,
    /// A profile's rescue function without WinUSB yet: one UAC away.
    NeedsDriver,
    /// adb interface (Android USB debugging is on).
    Adb,
    /// MTP / file transfer.
    Mtp,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub target: UsbTarget,
    pub role: Role,
    pub service: String,
    pub description: String,
}

pub fn role(entry: &UsbEntry, targets: &[UsbTarget]) -> Role {
    let service = entry.service.to_ascii_lowercase();
    let description = entry.description.to_ascii_lowercase();
    if usbid::is_target(targets, &entry.id) {
        if entry.has_winusb() {
            Role::Ready
        } else {
            Role::NeedsDriver
        }
    } else if description.contains("adb") || description.contains("android composite") {
        Role::Adb
    } else if service.contains("wpdmtp") || description.contains("mtp") {
        Role::Mtp
    } else {
        Role::Other
    }
}

/// Drivers of things that are never the phone: hubs, mice and keyboards,
/// webcams, headsets, Bluetooth radios, and composite parents (their
/// interfaces are listed instead).
const NOT_A_PHONE: [&str; 6] = ["hub", "hidusb", "usbaudio", "usbvideo", "bthusb", "usbccgp"];

fn worth_showing(entry: &UsbEntry, targets: &[UsbTarget]) -> bool {
    let service = entry.service.to_ascii_lowercase();
    usbid::is_target(targets, &entry.id) || !NOT_A_PHONE.iter().any(|s| service.contains(s))
}

/// USB functions worth showing (see [`NOT_A_PHONE`]), rescue targets first.
pub fn rows(entries: &[UsbEntry], targets: &[UsbTarget]) -> Vec<Row> {
    let mut out: Vec<Row> = entries
        .iter()
        .filter(|e| worth_showing(e, targets))
        .filter_map(|e| {
            Some(Row {
                target: e.target()?,
                role: role(e, targets),
                service: e.service.clone(),
                description: e.description.clone(),
            })
        })
        .collect();
    out.sort_by_key(|r| (r.role, r.target));
    out
}

pub fn role_label(role: Role) -> &'static str {
    match role {
        Role::Ready => "救援就绪",
        Role::NeedsDriver => "需装驱动",
        Role::Adb => "ADB",
        Role::Mtp => "MTP",
        Role::Other => "",
    }
}

/// One-line verdict for the rescue.
pub fn summary(rows: &[Row]) -> String {
    let count = |want: Role| rows.iter().filter(|r| r.role == want).count();
    match (count(Role::Ready), count(Role::NeedsDriver)) {
        (0, 0) => "没有找到机型配置里声明的救援接口：确认设备已插好；新机型请照下表在配置里加 usb = VID:PID:MI".into(),
        (_, n) if n > 0 => format!("{n} 个救援接口还没有驱动：开始救援时会自动请求管理员权限安装"),
        (n, _) => format!("{n} 个救援接口就绪，可以开始救援"),
    }
}

/// Plain-text report to copy into a bug report or chat.
pub fn report(rows: &[Row], adb: &[String]) -> String {
    let mut out = format!("安卓救援宝 设备体检\n{}\n\nUSB:\n", summary(rows));
    for r in rows {
        out.push_str(&format!(
            "  {:<13} {:<8} {:<12} {}\n",
            r.target.to_string(),
            role_label(r.role),
            if r.service.is_empty() {
                "(无驱动)"
            } else {
                &r.service
            },
            r.description
        ));
    }
    out.push_str("\nadb:\n");
    if adb.is_empty() {
        out.push_str("  (无设备)\n");
    }
    for line in adb {
        out.push_str(&format!("  {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, service: &str, description: &str) -> UsbEntry {
        UsbEntry {
            id: id.into(),
            service: service.into(),
            description: description.into(),
        }
    }

    fn pad3() -> Vec<UsbEntry> {
        vec![
            entry(
                r"USB\ROOT_HUB30\4&1A2B&0&0",
                "USBHUB3",
                "USB Root Hub (USB 3.0)",
            ),
            entry(r"USB\VID_05E3&PID_0610\5&1&1", "USBHUB3", "Generic USB Hub"),
            entry(r"USB\VID_046D&PID_C52B\6&2&3", "HidUsb", "USB Input Device"),
            entry(
                r"USB\VID_2D95&PID_6013\SER0123456789AB",
                "usbccgp",
                "USB Composite Device",
            ),
            entry(
                r"USB\VID_2D95&PID_6013&MI_00\7&1&0",
                "WUDFWpdMtp",
                "MTP USB Device",
            ),
            entry(r"USB\VID_2D95&PID_6013&MI_01\7&1&1", "", "USB Mass Storage"),
            entry(
                r"USB\VID_2D95&PID_6013&MI_02\7&1&2",
                "WinUSB",
                "ADB Interface",
            ),
        ]
    }

    #[test]
    fn classifies_the_pad3_in_adb_mode() {
        let targets = UsbTarget::parse_list("2D95:6012:01, 2D95:6013:01").unwrap();
        let rows = rows(&pad3(), &targets);
        let role_of = |t: &str| {
            rows.iter()
                .find(|r| r.target.to_string() == t)
                .map(|r| r.role)
        };
        assert_eq!(role_of("2D95:6013:01"), Some(Role::NeedsDriver));
        assert_eq!(role_of("2D95:6013:02"), Some(Role::Adb));
        assert_eq!(role_of("2D95:6013:00"), Some(Role::Mtp));
        assert_eq!(role_of("05E3:0610"), None, "hubs are hidden");
        assert_eq!(role_of("046D:C52B"), None, "so are mice");
        assert_eq!(role_of("2D95:6013"), None, "and composite parents");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].role,
            Role::NeedsDriver,
            "rescue-relevant rows first"
        );
        assert!(summary(&rows).contains("1 个救援接口还没有驱动"));
    }

    #[test]
    fn says_what_to_do_when_no_target_is_known() {
        let rows = rows(&pad3(), &[]);
        assert!(summary(&rows).contains("usb = VID:PID:MI"));
    }

    #[test]
    fn report_lists_usb_and_adb() {
        let targets = UsbTarget::parse_list("2D95:6013:01").unwrap();
        let text = report(&rows(&pad3(), &targets), &["SER0123456789AB device".into()]);
        assert!(text.contains("2D95:6013:01  需装驱动"));
        assert!(text.contains("(无驱动)"));
        assert!(text.contains("SER0123456789AB device"));
    }
}
