//! One-time driver setup for rescue mode: bind Windows' in-box, Microsoft-
//! signed "WinUsb Device" driver (winusb.inf) to the tablet's otherwise
//! unused interface MI_01 (vivo's virtual driver CD) — the same thing Device
//! Manager does with "Let me pick → Universal Serial Bus devices → WinUsb
//! Device". MTP (MI_00) is untouched and no filter driver is involved, so
//! re-enumeration (USB mode switches) keeps working.
//!
//! Needs administrator rights: the app re-launches itself elevated with
//! `--install-winusb`.

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DI_ENUMSINGLEINF, DI_FLAGSEX_ALLOWEXCLUDEDDRVS, DIGCF_ALLCLASSES, DIGCF_PRESENT,
    DiInstallDevice, HDEVINFO, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SP_DRVINFO_DATA_V2_W,
    SPDIT_CLASSDRIVER, SPDRP_CLASSGUID, SPDRP_SERVICE, SetupDiBuildDriverInfoList,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiEnumDriverInfoW,
    SetupDiGetClassDevsW, SetupDiGetDeviceInstallParamsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiSetDeviceInstallParamsW,
    SetupDiSetDeviceRegistryPropertyW, SetupDiSetSelectedDriverW,
};
use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};

/// MI_01 (the virtual driver CD) of the Pad3 in both USB modes:
/// 6012 = debugging off (rescue starts here), 6013 = with adb — needed so
/// AOA can tap the "允许 USB 调试" dialog while adb keeps MI_02 (and the
/// pending authorisation) open; killing adb would cancel that dialog.
pub const TARGET_PREFIXES: [&str; 2] = [
    r"USB\VID_2D95&PID_6012&MI_01\",
    r"USB\VID_2D95&PID_6013&MI_01\",
];
/// System class "USBDevice" — the class of winusb.inf.
const USBDEVICE_CLASS: &str = "{88BAE032-5A81-49F0-BC3D-A4FF138216D6}";
/// Device interface WinUSB registers for MI_01. WinUSB only exposes an
/// openable interface when the hardware key has `DeviceInterfaceGUIDs`
/// (libwdi/Zadig INFs write it; the in-box winusb.inf does not — without it
/// libusb fails with LIBUSB_ERROR_NOT_FOUND). Any fixed GUID works.
pub const INTERFACE_GUID: &str = "{6A1F7B54-2D95-4C3B-9A10-50AD7C3E5A01}";
/// The same GUID as a value, for SetupDiGetClassDevs.
pub const INTERFACE_GUID_VALUE: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x6A1F7B54_2D95_4C3B_9A10_50AD7C3E5A01);

/// REG_MULTI_SZ payload: UTF-16 strings, each NUL-terminated, plus a final NUL.
pub fn multi_sz(values: &[&str]) -> Vec<u8> {
    let mut wide: Vec<u16> = Vec::new();
    for v in values {
        wide.extend(v.encode_utf16());
        wide.push(0);
    }
    wide.push(0);
    wide.iter().flat_map(|c| c.to_le_bytes()).collect()
}

/// Is this device instance the interface we bind WinUSB to?
pub fn is_target(instance_id: &str) -> bool {
    let id = instance_id.to_ascii_uppercase();
    TARGET_PREFIXES
        .iter()
        .any(|p| id.len() > p.len() && id.starts_with(p))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> String {
    // SAFETY: trivial thread-local query.
    format!("{what} 失败 (Win32 错误 0x{:08X})", unsafe {
        GetLastError()
    })
}

struct DevInfoList(HDEVINFO);

impl Drop for DevInfoList {
    fn drop(&mut self) {
        // SAFETY: the list was created by SetupDiGetClassDevsW.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// A present USB device element: instance id + bound service.
struct Present {
    id: String,
    service: String,
    dev: SP_DEVINFO_DATA,
}

/// Every present device under the USB enumerator.
fn present_usb() -> Result<(DevInfoList, Vec<Present>), String> {
    let enumerator = wide("USB");
    // SAFETY: SetupAPI calls with correctly sized, zero-initialised structs;
    // every BOOL result is checked.
    unsafe {
        let set = SetupDiGetClassDevsW(
            std::ptr::null(),
            enumerator.as_ptr(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        );
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(last_error("枚举 USB 设备"));
        }
        let set = DevInfoList(set);
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            let mut dev: SP_DEVINFO_DATA = std::mem::zeroed();
            dev.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set.0, i, &mut dev) == 0 {
                break;
            }
            i += 1;
            let mut buf = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(
                set.0,
                &dev,
                buf.as_mut_ptr(),
                buf.len() as u32,
                std::ptr::null_mut(),
            ) == 0
            {
                continue;
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let id = String::from_utf16_lossy(&buf[..len]);
            let service = registry_string(set.0, &dev, SPDRP_SERVICE).unwrap_or_default();
            out.push(Present { id, service, dev });
        }
        Ok((set, out))
    }
}

/// SPDRP_* string property of a device element.
unsafe fn registry_string(set: HDEVINFO, dev: &SP_DEVINFO_DATA, prop: u32) -> Option<String> {
    let mut buf = [0u16; 256];
    // SAFETY (caller): live set/element; buffer size passed in bytes.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            dev,
            prop,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            (buf.len() * 2) as u32,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then(|| {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    })
}

/// Tablet MI_01 interfaces present right now that do not have WinUSB yet —
/// e.g. the adb-mode (6013) interface the first time the tablet switches
/// into it. Read-only; no elevation needed.
pub fn unbound_targets() -> Vec<String> {
    present_usb()
        .map(|(_, all)| {
            all.into_iter()
                .filter(|p| is_target(&p.id) && !p.service.eq_ignore_ascii_case("WinUSB"))
                .map(|p| p.id)
                .collect()
        })
        .unwrap_or_default()
}

/// Bind the in-box WinUSB driver to every present MI_01 of the tablet.
/// Returns the device instance ids handled. Must run elevated. Idempotent.
pub fn install_for_tablet() -> Result<Vec<String>, String> {
    let (set, all) = present_usb()?;
    let vivo: Vec<String> = all
        .iter()
        .filter(|p| p.id.to_ascii_uppercase().contains("VID_2D95"))
        .map(|p| p.id.clone())
        .collect();
    let targets: Vec<Present> = all.into_iter().filter(|p| is_target(&p.id)).collect();
    if targets.is_empty() {
        let seen = if vivo.is_empty() {
            "无".to_string()
        } else {
            vivo.join(", ")
        };
        return Err(format!(
            "没有找到平板的 MI_01 接口（平板需插着）。当前 vivo USB 设备: {seen}"
        ));
    }
    let mut done = Vec::new();
    for mut p in targets {
        // SAFETY: `p.dev` is an element of the live `set`.
        unsafe { install_one(set.0, &mut p.dev) }.map_err(|e| format!("{}: {e}", p.id))?;
        done.push(p.id);
    }
    Ok(done)
}

/// Bind winusb.inf's "WinUsb Device" to one device element.
unsafe fn install_one(set: HDEVINFO, dev: &mut SP_DEVINFO_DATA) -> Result<(), String> {
    // SAFETY (caller): `set`/`dev` are a live device info set and element.
    unsafe {
        // Before (re)installing, so the driver start below picks it up.
        set_interface_guid(set, dev)?;

        // Make it a "USB device" so winusb.inf's driver applies to it.
        let class = wide(USBDEVICE_CLASS);
        if SetupDiSetDeviceRegistryPropertyW(
            set,
            dev,
            SPDRP_CLASSGUID,
            class.as_ptr().cast(),
            (class.len() * 2) as u32,
        ) == 0
        {
            return Err(last_error("设置设备类 USBDevice"));
        }

        // Search only winusb.inf, including drivers not shown by default.
        let mut params: SP_DEVINSTALL_PARAMS_W = std::mem::zeroed();
        params.cbSize = size_of::<SP_DEVINSTALL_PARAMS_W>() as u32;
        if SetupDiGetDeviceInstallParamsW(set, dev, &mut params) == 0 {
            return Err(last_error("读取安装参数"));
        }
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let inf = wide(&format!(r"{windir}\INF\winusb.inf"));
        params.DriverPath[..inf.len()].copy_from_slice(&inf);
        params.Flags |= DI_ENUMSINGLEINF;
        params.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
        if SetupDiSetDeviceInstallParamsW(set, dev, &params) == 0 {
            return Err(last_error("设置安装参数"));
        }
        if SetupDiBuildDriverInfoList(set, dev, SPDIT_CLASSDRIVER) == 0 {
            return Err(last_error("读取 winusb.inf 驱动列表"));
        }
        let mut drv: SP_DRVINFO_DATA_V2_W = std::mem::zeroed();
        drv.cbSize = size_of::<SP_DRVINFO_DATA_V2_W>() as u32;
        if SetupDiEnumDriverInfoW(set, dev, SPDIT_CLASSDRIVER, 0, &mut drv) == 0 {
            return Err(last_error("winusb.inf 中没有可用驱动"));
        }
        if SetupDiSetSelectedDriverW(set, dev, &mut drv) == 0 {
            return Err(last_error("选择 WinUSB 驱动"));
        }
        let mut reboot = 0;
        if DiInstallDevice(std::ptr::null_mut(), set, dev, &drv, 0, &mut reboot) == 0 {
            return Err(last_error("安装 WinUSB 驱动"));
        }
        Ok(())
    }
}

/// Write `DeviceInterfaceGUIDs` into the device's hardware key
/// (…\Enum\USB\<id>\Device Parameters), as an INF `HKR,,…,0x10000` would.
unsafe fn set_interface_guid(set: HDEVINFO, dev: &mut SP_DEVINFO_DATA) -> Result<(), String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DICS_FLAG_GLOBAL, DIREG_DEV, SetupDiOpenDevRegKey,
    };
    use windows_sys::Win32::System::Registry::{
        KEY_SET_VALUE, REG_MULTI_SZ, RegCloseKey, RegSetValueExW,
    };
    // SAFETY (caller): `set`/`dev` are a live device info set and element.
    unsafe {
        let key = SetupDiOpenDevRegKey(set, dev, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_SET_VALUE);
        if std::ptr::eq(key, INVALID_HANDLE_VALUE) {
            return Err(last_error("打开设备注册表项"));
        }
        let name = wide("DeviceInterfaceGUIDs");
        let data = multi_sz(&[INTERFACE_GUID]);
        let rc = RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_MULTI_SZ,
            data.as_ptr(),
            data.len() as u32,
        );
        RegCloseKey(key);
        if rc != 0 {
            return Err(format!("写入 DeviceInterfaceGUIDs 失败 (错误 {rc})"));
        }
    }
    Ok(())
}

/// Re-launch this executable elevated to perform the install (UAC prompt).
pub fn install_elevated() -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (verb, file, args) = (
        wide("runas"),
        wide(&exe.to_string_lossy()),
        wide(INSTALL_FLAG),
    );
    // SAFETY: plain ShellExecute with NUL-terminated wide strings.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            args.as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        )
    };
    // ShellExecute returns a value > 32 on success.
    if (r as isize) > 32 {
        Ok(())
    } else {
        Err(format!(
            "无法以管理员身份运行（已取消？代码 {}）",
            r as isize
        ))
    }
}

/// Command-line flag handled in `main` before the UI starts.
pub const INSTALL_FLAG: &str = "--install-winusb";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz_is_utf16_with_double_nul_terminator() {
        let data = multi_sz(&["{A}"]);
        let wide: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        assert_eq!(
            wide,
            vec![u16::from(b'{'), u16::from(b'A'), u16::from(b'}'), 0, 0]
        );
        assert_eq!(INTERFACE_GUID.len(), 38, "braced GUID");
    }

    #[test]
    fn matches_only_the_debugging_off_mi01_interface() {
        assert!(is_target(r"USB\VID_2D95&PID_6012&MI_01\7&37F57971&0&0001"));
        assert!(
            is_target(r"usb\vid_2d95&pid_6012&mi_01\7&37f57971&0&0001"),
            "case-insensitive"
        );
        assert!(
            !is_target(r"USB\VID_2D95&PID_6012&MI_00\7&37F57971&0&0000"),
            "never MTP"
        );
        assert!(
            is_target(r"USB\VID_2D95&PID_6013&MI_01\7&24A3E323&0&0001"),
            "adb mode too"
        );
        assert!(
            !is_target(r"USB\VID_2D95&PID_6013&MI_02\7&24A3E323&0&0002"),
            "never the ADB interface"
        );
        assert!(
            !is_target(r"USB\VID_2D95&PID_6012\SER0123456789AB"),
            "never the parent"
        );
        assert!(!is_target(TARGET_PREFIXES[0]), "needs an instance suffix");
    }
}
