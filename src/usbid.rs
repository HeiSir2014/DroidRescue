//! USB identities of rescue targets: `VID:PID[:MI]` as written in rescue
//! profiles (`usb = 2D95:6012:01`), and parsing of Windows device instance
//! ids (`USB\VID_2D95&PID_6012&MI_01\7&37F57971&0&0001`) into them.
//!
//! A target names the USB function the rescue opens for AOA control
//! transfers: a spare interface of a composite device (`MI_01`, vivo's
//! virtual driver CD), or a whole non-composite device (no `MI`).

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsbTarget {
    pub vid: u16,
    pub pid: u16,
    /// Interface number of a composite device; `None` = the whole device.
    pub interface: Option<u8>,
}

impl UsbTarget {
    /// `2D95:6012:01` or `2D95:6012` (hex, case-insensitive).
    pub fn parse(text: &str) -> Result<UsbTarget, String> {
        let bad = || format!("USB 目标格式应为 VID:PID 或 VID:PID:MI（十六进制），而不是 “{text}”");
        let parts: Vec<&str> = text.trim().split(':').collect();
        let hex16 = |s: &str| {
            (s.len() == 4)
                .then(|| u16::from_str_radix(s, 16).ok())
                .flatten()
        };
        let (vid, pid, interface) = match parts.as_slice() {
            [v, p] => (hex16(v), hex16(p), None),
            [v, p, mi] if mi.len() == 2 => (
                hex16(v),
                hex16(p),
                Some(u8::from_str_radix(mi, 16).map_err(|_| bad())?),
            ),
            _ => return Err(bad()),
        };
        match (vid, pid) {
            (Some(vid), Some(pid)) => Ok(UsbTarget {
                vid,
                pid,
                interface,
            }),
            _ => Err(bad()),
        }
    }

    /// Comma-separated list, as in a profile's `usb =` line.
    pub fn parse_list(text: &str) -> Result<Vec<UsbTarget>, String> {
        text.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(UsbTarget::parse)
            .collect()
    }

    /// The target a Windows device instance id names, e.g.
    /// `USB\VID_2D95&PID_6012&MI_01\<instance>` → `2D95:6012:01`.
    pub fn of_instance(instance_id: &str) -> Option<UsbTarget> {
        let mut parts = instance_id.split('\\');
        let (bus, hardware, instance) = (parts.next()?, parts.next()?, parts.next()?);
        if !bus.eq_ignore_ascii_case("USB") || instance.is_empty() || parts.next().is_some() {
            return None;
        }
        let (mut vid, mut pid, mut interface) = (None, None, None);
        for token in hardware.split('&') {
            let (key, value) = token.split_once('_')?;
            match key.to_ascii_uppercase().as_str() {
                "VID" if value.len() == 4 => vid = u16::from_str_radix(value, 16).ok(),
                "PID" if value.len() == 4 => pid = u16::from_str_radix(value, 16).ok(),
                "MI" if value.len() == 2 => interface = u8::from_str_radix(value, 16).ok(),
                _ => return None,
            }
        }
        Some(UsbTarget {
            vid: vid?,
            pid: pid?,
            interface,
        })
    }

    /// Is this device instance exactly this target?
    pub fn matches_instance(&self, instance_id: &str) -> bool {
        UsbTarget::of_instance(instance_id) == Some(*self)
    }
}

impl fmt::Display for UsbTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04X}:{:04X}", self.vid, self.pid)?;
        if let Some(mi) = self.interface {
            write!(f, ":{mi:02X}")?;
        }
        Ok(())
    }
}

/// A present USB device or interface, as the platform scanner reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbEntry {
    /// Windows-style instance id (`USB\VID_…&PID_…[&MI_..]\<instance>`);
    /// the macOS scanner synthesises the same shape.
    pub id: String,
    /// Bound driver (`WinUSB`, `WUDFWpdMtp`, …; empty = none).
    pub service: String,
    pub description: String,
    /// Usable for AOA as is: WinUSB bound (Windows); always (macOS).
    pub ready: bool,
}

impl UsbEntry {
    pub fn target(&self) -> Option<UsbTarget> {
        UsbTarget::of_instance(&self.id)
    }
}

/// Target instances in `entries` that are not ready yet — on Windows, the
/// adb-mode interface the first time the device switches into it, or any
/// target on a USB port this PC has not seen it on.
#[cfg_attr(not(windows), allow(dead_code))] // macOS has nothing to bind
pub fn unbound(entries: &[UsbEntry], targets: &[UsbTarget]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| is_target(targets, &e.id) && !e.ready)
        .map(|e| e.id.clone())
        .collect()
}

/// Is `instance_id` one of `targets`?
pub fn is_target(targets: &[UsbTarget], instance_id: &str) -> bool {
    targets.iter().any(|t| t.matches_instance(instance_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD3_OFF: UsbTarget = UsbTarget {
        vid: 0x2D95,
        pid: 0x6012,
        interface: Some(1),
    };

    #[test]
    fn parses_profile_notation() {
        assert_eq!(UsbTarget::parse("2D95:6012:01"), Ok(PAD3_OFF));
        assert_eq!(UsbTarget::parse(" 2d95:6012:01 "), Ok(PAD3_OFF));
        assert_eq!(
            UsbTarget::parse("18D1:4EE1").unwrap().interface,
            None,
            "whole device"
        );
        for bad in [
            "",
            "2D95",
            "2D95:6012:1",
            "2D95:60123",
            "XYZW:6012",
            "2D95:6012:01:02",
        ] {
            assert!(UsbTarget::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn display_round_trips() {
        for text in ["2D95:6012:01", "18D1:4EE1"] {
            assert_eq!(UsbTarget::parse(text).unwrap().to_string(), text);
        }
    }

    #[test]
    fn parses_lists_and_reports_the_bad_entry() {
        let list = UsbTarget::parse_list("2D95:6012:01, 2D95:6013:01,").unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            UsbTarget::parse_list("2D95:6012:01, nope")
                .unwrap_err()
                .contains("nope")
        );
    }

    #[test]
    fn reads_windows_instance_ids() {
        assert_eq!(
            UsbTarget::of_instance(r"USB\VID_2D95&PID_6012&MI_01\7&37F57971&0&0001"),
            Some(PAD3_OFF)
        );
        assert_eq!(
            UsbTarget::of_instance(r"usb\vid_2d95&pid_6012&mi_01\7&37f57971&0&0001"),
            Some(PAD3_OFF),
            "case-insensitive"
        );
        assert_eq!(
            UsbTarget::of_instance(r"USB\VID_2D95&PID_6012\SER0123456789AB")
                .unwrap()
                .interface,
            None,
            "the composite parent is the whole device"
        );
        assert_eq!(UsbTarget::of_instance(r"USB\ROOT_HUB30\4&1A2B&0&0"), None);
        assert_eq!(UsbTarget::of_instance(r"HID\VID_046D&PID_C52B\x"), None);
        assert_eq!(
            UsbTarget::of_instance(r"USB\VID_2D95&PID_6012&MI_01\"),
            None,
            "needs an instance suffix"
        );
    }

    fn entry(id: &str, ready: bool) -> UsbEntry {
        UsbEntry {
            id: id.into(),
            service: String::new(),
            description: String::new(),
            ready,
        }
    }

    /// The second-PC failure: the adb-mode interface had no driver yet.
    #[test]
    fn finds_targets_that_are_not_ready_only() {
        let targets = UsbTarget::parse_list("2D95:6012:01, 2D95:6013:01").unwrap();
        let entries = [
            entry(r"USB\VID_2D95&PID_6013\SER0123456789AB", false),
            entry(r"USB\VID_2D95&PID_6013&MI_00\7&24A3E323&0&0000", false),
            entry(r"USB\VID_2D95&PID_6013&MI_01\7&24A3E323&0&0001", false),
            entry(r"USB\VID_2D95&PID_6013&MI_02\7&24A3E323&0&0002", true),
            entry(r"USB\VID_2D95&PID_6012&MI_01\7&37F57971&0&0001", true),
        ];
        assert_eq!(
            unbound(&entries, &targets),
            [r"USB\VID_2D95&PID_6013&MI_01\7&24A3E323&0&0001"]
        );
        assert!(unbound(&entries, &[]).is_empty(), "no targets, no binds");
    }

    #[test]
    fn matches_only_the_exact_interface() {
        let targets = [PAD3_OFF];
        assert!(is_target(
            &targets,
            r"USB\VID_2D95&PID_6012&MI_01\7&37F57971&0&0001"
        ));
        assert!(
            !is_target(&targets, r"USB\VID_2D95&PID_6012&MI_00\7&37F57971&0&0000"),
            "never MTP"
        );
        assert!(
            !is_target(&targets, r"USB\VID_2D95&PID_6013&MI_02\7&24A3E323&0&0002"),
            "never the ADB interface"
        );
        assert!(
            !is_target(&targets, r"USB\VID_2D95&PID_6012\SER0123456789AB"),
            "never the parent"
        );
    }
}
