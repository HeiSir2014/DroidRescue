//! Per-model rescue profiles: the blind workflows are data (rescue script
//! syntax), so other devices are supported by adding a profile, not code.
//!
//! Built-in profiles can be overridden or extended in
//! `%LOCALAPPDATA%\DroidRescue\rescue-profiles.txt`:
//!
//! ```text
//! [vivo Pad3]
//! models = PA2455
//! unlock = wake; wait:500; swipe:0,-700; wait:700; text:{pw}; enter
//! enable = ...            # optional: unlock → USB debugging on
//!          continued.     # indented lines continue the value
//! usb = 2D95:6012:01, 2D95:6013:01
//! ```
//!
//! `usb` lists the USB functions (`VID:PID:MI`, or `VID:PID` for a whole
//! device) the rescue drives in each USB mode the workflow passes through;
//! on Windows those get the in-box WinUSB driver (one UAC per new port or
//! mode). A section with an existing name replaces that profile.

use crate::rescue;
use crate::usbid::UsbTarget;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// adb model codes (`ro.product.model`) this profile is for.
    pub models: Vec<String>,
    pub unlock: String,
    /// Full "debugging off → authorised adb" workflow, if known for the model.
    pub enable: Option<String>,
    /// USB functions the rescue opens, in every USB mode the workflow visits.
    pub usb: Vec<UsbTarget>,
}

pub const GENERIC: &str = "通用 Android（仅键盘解锁）";

pub fn builtin() -> Vec<Profile> {
    vec![
        Profile {
            name: "vivo Pad3".into(),
            models: vec!["PA2455".into()],
            unlock: rescue::SCRIPT_UNLOCK.into(),
            enable: Some(rescue::SCRIPT_ENABLE_DEBUGGING.into()),
            // MI_01 = vivo's virtual driver CD, unused by Windows. 6012 =
            // debugging off (rescue starts here); 6013 = with adb, so the
            // pen can tap "允许 USB 调试" while adb keeps MI_02 (and the
            // pending authorisation) open.
            usb: vec![
                UsbTarget {
                    vid: 0x2D95,
                    pid: 0x6012,
                    interface: Some(1),
                },
                UsbTarget {
                    vid: 0x2D95,
                    pid: 0x6013,
                    interface: Some(1),
                },
            ],
        },
        Profile {
            name: GENERIC.into(),
            models: Vec::new(),
            unlock: rescue::SCRIPT_UNLOCK.into(),
            enable: None,
            usb: Vec::new(),
        },
    ]
}

/// Every USB function any profile declares — what the rescue may bind and
/// open. A union, so a device is reachable whichever profile is selected.
pub fn usb_targets(profiles: &[Profile]) -> Vec<UsbTarget> {
    let mut all: Vec<UsbTarget> = profiles.iter().flat_map(|p| p.usb.clone()).collect();
    all.sort();
    all.dedup();
    all
}

/// The first profile one of whose USB functions is present — how a device
/// is recognised on a PC that has never seen it over adb.
pub fn for_usb<'a>(profiles: &'a [Profile], present: &[UsbTarget]) -> Option<&'a Profile> {
    profiles
        .iter()
        .find(|p| p.usb.iter().any(|t| present.contains(t)))
}

pub fn path() -> PathBuf {
    crate::bundle::default_base().join("rescue-profiles.txt")
}

/// Built-ins merged with the user's file (missing file = built-ins only).
pub fn load(path: &Path) -> (Vec<Profile>, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match parse(&text) {
            Ok(user) => (merge(builtin(), user), None),
            Err(e) => (builtin(), Some(format!("{}: {e}", path.display()))),
        },
        Err(_) => (builtin(), None),
    }
}

/// A commented starter file showing the built-ins in the file format.
pub fn template() -> String {
    let mut out = String::from(
        "# 安卓救援宝 救援机型配置。每节 [名称]；同名覆盖内置配置。\n\
         # models = adb 型号(逗号分隔)  unlock = 解锁脚本  enable = 一键开启调试脚本(可选)\n\
         # usb = 救援用的 USB 接口 VID:PID:MI（整机用 VID:PID），见 06 救援 → 高级 → 设备体检\n\
         # 脚本语法同 06 救援 → 高级 → 脚本；缩进的行接续上一行。\n\
         # 以下为内置配置，取消注释后修改即可。\n",
    );
    for p in builtin() {
        out.push_str(&format!(
            "\n# [{}]\n# models = {}\n# unlock = {}\n",
            p.name,
            p.models.join(", "),
            p.unlock
        ));
        if let Some(enable) = &p.enable {
            out.push_str(&format!(
                "# enable = {}\n",
                enable.split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
        if !p.usb.is_empty() {
            let usb: Vec<String> = p.usb.iter().map(UsbTarget::to_string).collect();
            out.push_str(&format!("# usb = {}\n", usb.join(", ")));
        }
    }
    out
}

pub fn parse(text: &str) -> Result<Vec<Profile>, String> {
    let mut out: Vec<Profile> = Vec::new();
    let mut last_key: Option<String> = None;
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let trimmed = line.trim();
        let indented = line.starts_with([' ', '\t']);
        // Unindented `#` = file comment; indented `#` inside a value is a
        // script comment and stays with the script.
        if trimmed.is_empty() || (trimmed.starts_with('#') && !(indented && last_key.is_some())) {
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            out.push(Profile {
                name: name.trim().into(),
                models: Vec::new(),
                unlock: String::new(),
                enable: None,
                usb: Vec::new(),
            });
            last_key = None;
            continue;
        }
        let profile = out
            .last_mut()
            .ok_or(format!("第 {n} 行: 需要先写 [名称]"))?;
        let continuation = line.starts_with([' ', '\t']);
        let (key, value) = match (continuation, trimmed.split_once('=')) {
            (true, _) if last_key.is_some() => (last_key.clone().unwrap(), trimmed),
            (_, Some((k, v))) => (k.trim().to_string(), v.trim()),
            _ => return Err(format!("第 {n} 行: 需要 key = value")),
        };
        // Continuation lines stay separate lines: newline-separated commands
        // keep working and the editor shows the script as written.
        let append = |field: &mut String| {
            if continuation && !field.is_empty() {
                field.push('\n');
            }
            field.push_str(value);
        };
        match key.as_str() {
            "models" => profile.models.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(String::from),
            ),
            "unlock" => append(&mut profile.unlock),
            "enable" => append(profile.enable.get_or_insert_with(String::new)),
            "usb" => profile
                .usb
                .extend(UsbTarget::parse_list(value).map_err(|e| format!("第 {n} 行: {e}"))?),
            other => return Err(format!("第 {n} 行: 未知字段 {other}")),
        }
        last_key = Some(key);
    }
    for p in &out {
        if p.unlock.is_empty() {
            return Err(format!("[{}] 缺少 unlock", p.name));
        }
        for script in std::iter::once(&p.unlock).chain(p.enable.as_ref()) {
            rescue::parse(script, &[("pw", "0")])
                .map_err(|e| format!("[{}] 脚本错误: {e}", p.name))?;
        }
    }
    Ok(out)
}

/// User profiles replace built-ins with the same name; new ones go first.
pub fn merge(builtin: Vec<Profile>, user: Vec<Profile>) -> Vec<Profile> {
    let mut out: Vec<Profile> = user
        .iter()
        .filter(|u| !builtin.iter().any(|b| b.name == u.name))
        .cloned()
        .collect();
    out.extend(
        builtin
            .into_iter()
            .map(|b| user.iter().find(|u| u.name == b.name).cloned().unwrap_or(b)),
    );
    out
}

/// The first profile that lists this adb model.
pub fn by_model<'a>(profiles: &'a [Profile], model: &str) -> Option<&'a Profile> {
    profiles
        .iter()
        .find(|p| p.models.iter().any(|x| x.eq_ignore_ascii_case(model)))
}

/// Profile for a model: the first that lists it, else the generic one.
pub fn pick<'a>(profiles: &'a [Profile], model: Option<&str>) -> Option<&'a Profile> {
    model
        .and_then(|m| by_model(profiles, m))
        .or_else(|| profiles.iter().find(|p| p.name == GENERIC))
        .or(profiles.first())
}

/// serial → adb model, remembered from adb sessions so the right profile is
/// chosen later when only the USB (rescue) side is visible.
pub struct KnownModels {
    path: PathBuf,
    map: BTreeMap<String, String>,
}

impl KnownModels {
    pub fn load(path: PathBuf) -> KnownModels {
        let map = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(s, m)| (s.trim().to_string(), m.trim().to_string()))
            .filter(|(s, m)| !s.is_empty() && !m.is_empty())
            .collect();
        KnownModels { path, map }
    }

    pub fn get(&self, serial: &str) -> Option<&str> {
        self.map.get(serial).map(String::as_str)
    }

    /// Record a pairing; writes the file only when something changed.
    pub fn remember(&mut self, serial: &str, model: &str) -> std::io::Result<()> {
        if serial.is_empty() || model.is_empty() || self.get(serial) == Some(model) {
            return Ok(());
        }
        self.map.insert(serial.to_string(), model.to_string());
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text: String = self
            .map
            .iter()
            .map(|(s, m)| format!("{s}\t{m}\n"))
            .collect();
        std::fs::write(&self.path, text)
    }
}

pub fn known_models_path() -> PathBuf {
    crate::bundle::default_base().join("known-devices.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_include_pad3_and_generic() {
        let b = builtin();
        for p in &b {
            rescue::parse(&p.unlock, &[("pw", "1")]).unwrap();
        }
        assert_eq!(pick(&b, Some("PA2455")).unwrap().name, "vivo Pad3");
        assert_eq!(pick(&b, Some("pa2455")).unwrap().name, "vivo Pad3");
        assert_eq!(pick(&b, Some("V2405A")).unwrap().name, GENERIC);
        assert_eq!(pick(&b, None).unwrap().name, GENERIC);
    }

    #[test]
    fn parses_sections_lists_and_continuations() {
        let text = "# comment\n[My Phone]\nmodels = V2405A, V2406A\nunlock = wake; enter\nenable = wake\n  click:0.5,0.5\n  wait:100\n";
        let p = parse(text).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].models, vec!["V2405A", "V2406A"]);
        assert_eq!(p[0].unlock, "wake; enter");
        assert_eq!(
            p[0].enable.as_deref(),
            Some("wake\nclick:0.5,0.5\nwait:100"),
            "one command per line, no ';' needed"
        );
        assert_eq!(
            rescue::parse(p[0].enable.as_ref().unwrap(), &[])
                .unwrap()
                .len(),
            3
        );

        let commented =
            parse("[X]\nunlock = wake\n  # 输入密码\n  text:1\n# file comment\n").unwrap();
        assert_eq!(commented[0].unlock, "wake\n# 输入密码\ntext:1");
    }

    #[test]
    fn usb_targets_come_from_profiles() {
        let p = parse("[X]\nunlock = wake\nusb = 18D1:4EE1, 2D95:6012:01\n").unwrap();
        assert_eq!(p[0].usb[0].to_string(), "18D1:4EE1");
        let mut all = builtin();
        all.extend(p);
        let union: Vec<String> = usb_targets(&all).iter().map(|t| t.to_string()).collect();
        assert_eq!(
            union,
            ["18D1:4EE1", "2D95:6012:01", "2D95:6013:01"],
            "sorted, no duplicates"
        );
        assert!(
            parse("[X]\nunlock = wake\nusb = 2D95")
                .unwrap_err()
                .contains("第 3 行")
        );
    }

    #[test]
    fn recognises_a_device_by_its_usb_function() {
        let b = builtin();
        let pad3_off = UsbTarget::parse("2D95:6012:01").unwrap();
        let other = UsbTarget::parse("18D1:4EE1").unwrap();
        assert_eq!(for_usb(&b, &[other, pad3_off]).unwrap().name, "vivo Pad3");
        assert_eq!(for_usb(&b, &[other]), None);
    }

    #[test]
    fn rejects_bad_files_with_line_numbers() {
        assert!(parse("unlock = wake").unwrap_err().contains("第 1 行"));
        assert!(parse("[X]\nfoo = 1").unwrap_err().contains("未知字段"));
        assert!(
            parse("[X]\nmodels = A")
                .unwrap_err()
                .contains("缺少 unlock")
        );
        assert!(
            parse("[X]\nunlock = nonsense-key")
                .unwrap_err()
                .contains("脚本错误")
        );
    }

    #[test]
    fn user_profiles_override_by_name_and_add_new_ones_first() {
        let user = parse(
            "[vivo Pad3]\nmodels = PA2455\nunlock = wake\n[Other]\nmodels = Z1\nunlock = enter\n",
        )
        .unwrap();
        let merged = merge(builtin(), user);
        assert_eq!(merged[0].name, "Other");
        let pad = merged.iter().find(|p| p.name == "vivo Pad3").unwrap();
        assert_eq!(pad.unlock, "wake");
        assert_eq!(pad.enable, None, "an override replaces the whole profile");
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn template_is_all_comments_so_it_changes_nothing() {
        assert_eq!(parse(&template()).unwrap(), Vec::new());
    }

    #[test]
    fn known_models_persist() {
        let path =
            std::env::temp_dir().join(format!("droidrescue-known-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut k = KnownModels::load(path.clone());
        k.remember("SER0123456789AB", "PA2455").unwrap();
        assert_eq!(
            KnownModels::load(path.clone()).get("SER0123456789AB"),
            Some("PA2455")
        );
        std::fs::remove_file(&path).unwrap();
    }
}
