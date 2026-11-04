//! Per-device lock-screen passwords, encrypted with Windows DPAPI
//! (bound to the current Windows user; useless if the file is copied).
//!
//! File format: one `device_id<TAB>hex(dpapi_blob)` per line.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

/// Scopes the blobs to this app (other DPAPI users can't decrypt them blindly).
/// Kept from the padctl era on purpose: changing it would make every PIN
/// saved before the rename undecryptable.
const ENTROPY: &[u8] = b"padctl/lockscreen/v1";

pub struct PasswordStore {
    path: PathBuf,
    entries: BTreeMap<String, Vec<u8>>,
}

impl PasswordStore {
    /// Missing or unreadable file → empty store (never fatal for the UI).
    pub fn load(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .map(|t| parse(&t))
            .unwrap_or_default();
        Self { path, entries }
    }

    pub fn contains(&self, device_id: &str) -> bool {
        self.entries.contains_key(device_id)
    }

    /// The device id when exactly one password is saved.
    pub fn only_id(&self) -> Option<String> {
        (self.entries.len() == 1)
            .then(|| self.entries.keys().next().cloned())
            .flatten()
    }

    pub fn get(&self, device_id: &str) -> io::Result<Option<String>> {
        let Some(blob) = self.entries.get(device_id) else {
            return Ok(None);
        };
        let plain = unprotect(blob)?;
        String::from_utf8(plain)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn set(&mut self, device_id: &str, password: &str) -> io::Result<()> {
        self.entries
            .insert(device_id.to_string(), protect(password.as_bytes())?);
        self.save()
    }

    pub fn remove(&mut self, device_id: &str) -> io::Result<()> {
        self.entries.remove(device_id);
        self.save()
    }

    /// Atomic write: temp file + rename.
    fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serialize(&self.entries))?;
        std::fs::rename(&tmp, &self.path)
    }
}

fn parse(text: &str) -> BTreeMap<String, Vec<u8>> {
    text.lines()
        .filter_map(|line| {
            let (id, hex) = line.split_once('\t')?;
            Some((id.to_string(), from_hex(hex.trim())?))
        })
        .filter(|(id, _)| !id.is_empty())
        .collect()
}

fn serialize(entries: &BTreeMap<String, Vec<u8>>) -> String {
    entries
        .iter()
        .map(|(id, blob)| format!("{id}\t{}\n", to_hex(blob)))
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    }
}

/// Copy a DPAPI output blob and free it with LocalFree as the API requires.
fn take_blob(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: DPAPI returned a valid buffer of cbData bytes that we own.
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe { LocalFree(out.pbData.cast()) };
    bytes
}

fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(plain);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: all pointers reference live buffers for the duration of the call.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_blob(out))
}

fn unprotect(sealed: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(sealed);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: as above.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_blob(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "droidrescue-secrets-{tag}-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn dpapi_round_trips_and_hides_plaintext() {
        let sealed = protect(b"1234 abc").unwrap();
        assert!(!sealed.windows(8).any(|w| w == b"1234 abc"));
        assert_eq!(unprotect(&sealed).unwrap(), b"1234 abc");
    }

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        assert_eq!(
            from_hex(&to_hex(&[0, 1, 0xAB, 0xFF])),
            Some(vec![0, 1, 0xAB, 0xFF])
        );
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn parse_skips_malformed_lines() {
        let map = parse("dev1\t0a0b\nbroken line\n\tff\ndev2\tzz\n");
        assert_eq!(map.len(), 1);
        assert_eq!(map["dev1"], vec![0x0a, 0x0b]);
    }

    #[test]
    fn passwords_are_kept_per_device_and_persist() {
        let path = temp_file("persist");
        let mut store = PasswordStore::load(path.clone());
        store.set("PAD-A", "1111").unwrap();
        store.set("PAD-B", "it's 2 2").unwrap();

        let reloaded = PasswordStore::load(path.clone());
        assert_eq!(reloaded.get("PAD-A").unwrap().as_deref(), Some("1111"));
        assert_eq!(reloaded.get("PAD-B").unwrap().as_deref(), Some("it's 2 2"));
        assert_eq!(reloaded.get("PAD-C").unwrap(), None);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("1111"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn forgetting_a_device_removes_only_it() {
        let path = temp_file("forget");
        let mut store = PasswordStore::load(path.clone());
        store.set("PAD-A", "1111").unwrap();
        store.set("PAD-B", "2222").unwrap();
        store.remove("PAD-A").unwrap();

        let reloaded = PasswordStore::load(path.clone());
        assert!(!reloaded.contains("PAD-A"));
        assert!(reloaded.contains("PAD-B"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn only_id_is_set_for_a_single_saved_device() {
        let path = temp_file("only");
        let mut store = PasswordStore::load(path.clone());
        assert_eq!(store.only_id(), None);
        store.set("PAD-A", "1").unwrap();
        assert_eq!(store.only_id().as_deref(), Some("PAD-A"));
        store.set("PAD-B", "2").unwrap();
        assert_eq!(store.only_id(), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_an_empty_store() {
        let store = PasswordStore::load(temp_file("missing"));
        assert!(!store.contains("anything"));
    }
}
