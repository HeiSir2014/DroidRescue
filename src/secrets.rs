//! Per-device lock-screen passwords, sealed per OS user so the file is
//! useless when copied: Windows DPAPI, or on macOS AES-256-GCM with a
//! random key kept in the login Keychain.
//!
//! File format: one `device_id<TAB>hex(sealed_blob)` per line.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

#[cfg(windows)]
use dpapi::{protect, unprotect};
#[cfg(target_os = "macos")]
use keychain::{protect, unprotect};

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

#[cfg(windows)]
mod dpapi {
    use std::io;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    /// Scopes the blobs to this app (other DPAPI users can't decrypt them blindly).
    /// Kept from the padctl era on purpose: changing it would make every PIN
    /// saved before the rename undecryptable.
    const ENTROPY: &[u8] = b"padctl/lockscreen/v1";

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

    pub fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
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

    pub fn unprotect(sealed: &[u8]) -> io::Result<Vec<u8>> {
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
}

/// macOS: AES-256-GCM; the key is 32 random bytes stored as a generic
/// password in the login Keychain (created on first use). Blob layout:
/// 12-byte nonce ‖ ciphertext+tag.
#[cfg(target_os = "macos")]
mod keychain {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use security_framework::passwords::{get_generic_password, set_generic_password};
    use security_framework::random::SecRandom;
    use std::io;

    const SERVICE: &str = "DroidRescue";
    /// Binds the ciphertext to its purpose.
    const AAD: &[u8] = b"droidrescue/lockscreen/v1";
    const NONCE_LEN: usize = 12;

    const ACCOUNT: &str = "pin-key";

    fn other(e: impl std::fmt::Display) -> io::Error {
        io::Error::other(e.to_string())
    }

    fn random<const N: usize>() -> io::Result<[u8; N]> {
        let mut bytes = [0u8; N];
        SecRandom::default().copy_bytes(&mut bytes).map_err(other)?;
        Ok(bytes)
    }

    /// The Keychain-held key, created on first use.
    /// `errSecItemNotFound`: no key yet.
    const ITEM_NOT_FOUND: i32 = -25300;

    /// Only a missing key is created. Any other failure (access denied
    /// after an update, locked keychain) is an error: replacing the key
    /// would make every saved PIN undecryptable for good.
    pub fn keychain_key() -> io::Result<Vec<u8>> {
        match get_generic_password(SERVICE, ACCOUNT) {
            Ok(key) if key.len() == 32 => Ok(key),
            Ok(_) => Err(other("钥匙串中的密钥格式不对")),
            Err(e) if e.code() == ITEM_NOT_FOUND => {
                let key = random::<32>()?;
                set_generic_password(SERVICE, ACCOUNT, &key).map_err(other)?;
                Ok(key.to_vec())
            }
            Err(e) => Err(other(format!(
                "无法读取钥匙串中的密钥（已拒绝访问？）: {e}"
            ))),
        }
    }

    /// Unit tests use a per-process key in memory: they never touch (or
    /// litter) the user's Keychain, and a rebuilt test binary never trips
    /// a Keychain access prompt.
    fn key() -> io::Result<Vec<u8>> {
        if cfg!(test) {
            static TEST_KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
            Ok(TEST_KEY
                .get_or_init(|| random::<32>().expect("random"))
                .to_vec())
        } else {
            keychain_key()
        }
    }

    fn cipher() -> io::Result<Aes256Gcm> {
        Aes256Gcm::new_from_slice(&key()?).map_err(other)
    }

    pub fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
        let nonce = random::<NONCE_LEN>()?;
        let sealed = cipher()?
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: AAD,
                },
            )
            .map_err(other)?;
        Ok([nonce.as_slice(), &sealed].concat())
    }

    pub fn unprotect(sealed: &[u8]) -> io::Result<Vec<u8>> {
        let (nonce, body) = sealed
            .split_first_chunk::<NONCE_LEN>()
            .ok_or_else(|| other("密文太短"))?;
        cipher()?
            .decrypt(
                &Nonce::from(*nonce),
                Payload {
                    msg: body,
                    aad: AAD,
                },
            )
            .map_err(|_| other("无法解密（钥匙串中的密钥已变？）"))
    }
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
        store.set("PAD-A", "pin-1111").unwrap();
        store.set("PAD-B", "it's 2 2").unwrap();

        let reloaded = PasswordStore::load(path.clone());
        assert_eq!(reloaded.get("PAD-A").unwrap().as_deref(), Some("pin-1111"));
        assert_eq!(reloaded.get("PAD-B").unwrap().as_deref(), Some("it's 2 2"));
        assert_eq!(reloaded.get("PAD-C").unwrap(), None);
        // "pin-" cannot occur by chance in the hex ciphertext (a bare
        // "1111" could, which made this test flaky).
        assert!(!std::fs::read_to_string(&path).unwrap().contains("pin-"));
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

    /// The real Keychain key (run by hand on a Mac; may show an access
    /// prompt): `cargo test keychain_key -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn keychain_key_is_created_once_and_reused() {
        let first = keychain::keychain_key().unwrap();
        assert_eq!(first.len(), 32);
        assert_eq!(keychain::keychain_key().unwrap(), first);
    }
}
