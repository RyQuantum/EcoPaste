//! Persistent state: our identity and the peers we trust.
//!
//! One JSON file. Private keys are PKCS#8 DER, wrapped with DPAPI on Windows
//! (bound to the current Windows user) and stored as plain base64 elsewhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};

use crate::crypto::KeyPair;
use crate::model::SyncInfo;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedPeer {
    /// Peer's ECDH public key (SPKI DER, base64).
    pub crypt_public_key: String,
    /// Last SyncInfo we saw for the peer (mDNS or heartbeat).
    #[serde(default)]
    pub sync_info: Option<SyncInfo>,
    pub paired_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    app_instance_id: String,
    /// "dpapi:" or "plain:" followed by base64.
    sign_key: String,
    crypt_key: String,
    #[serde(default)]
    peers: BTreeMap<String, TrustedPeer>,
}

pub struct Store {
    path: PathBuf,
    pub app_instance_id: String,
    pub keys: KeyPair,
    pub peers: BTreeMap<String, TrustedPeer>,
}

impl Store {
    /// Loads `dir/crosspaste.json`, creating a fresh identity when it is missing.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("crosspaste.json");
        if path.exists() {
            let raw: StoreFile = serde_json::from_slice(&std::fs::read(&path)?)?;
            let keys = KeyPair::from_pkcs8(
                &unwrap_secret(&raw.sign_key)?,
                &unwrap_secret(&raw.crypt_key)?,
            )?;
            return Ok(Self {
                path,
                app_instance_id: raw.app_instance_id,
                keys,
                peers: raw.peers,
            });
        }
        let store = Self {
            path,
            app_instance_id: new_instance_id(),
            keys: KeyPair::generate(),
            peers: BTreeMap::new(),
        };
        store.save()?;
        Ok(store)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let raw = StoreFile {
            app_instance_id: self.app_instance_id.clone(),
            sign_key: wrap_secret(&self.keys.sign_pkcs8())?,
            crypt_key: wrap_secret(&self.keys.crypt_pkcs8())?,
            peers: self.peers.clone(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&raw)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn peer_crypt_key(&self, app_instance_id: &str) -> Option<Vec<u8>> {
        self.peers
            .get(app_instance_id)
            .and_then(|p| B64.decode(&p.crypt_public_key).ok())
    }
}

fn new_instance_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn wrap_secret(der: &[u8]) -> anyhow::Result<String> {
    #[cfg(windows)]
    {
        Ok(format!("dpapi:{}", B64.encode(dpapi::protect(der)?)))
    }
    #[cfg(not(windows))]
    {
        Ok(format!("plain:{}", B64.encode(der)))
    }
}

fn unwrap_secret(s: &str) -> anyhow::Result<Vec<u8>> {
    if let Some(rest) = s.strip_prefix("plain:") {
        return Ok(B64.decode(rest)?);
    }
    if let Some(rest) = s.strip_prefix("dpapi:") {
        #[cfg(windows)]
        {
            return dpapi::unprotect(&B64.decode(rest)?);
        }
        #[cfg(not(windows))]
        {
            let _ = rest;
            anyhow::bail!("DPAPI-protected key can only be read on Windows");
        }
    }
    anyhow::bail!("unknown key encoding")
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn call(
        input: &[u8],
        f: unsafe extern "system" fn(*const CRYPT_INTEGER_BLOB, *mut CRYPT_INTEGER_BLOB) -> i32,
    ) -> anyhow::Result<Vec<u8>> {
        let inp = CRYPT_INTEGER_BLOB {
            cbData: input.len() as u32,
            pbData: input.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = unsafe { f(&inp, &mut out) };
        if ok == 0 {
            anyhow::bail!("DPAPI call failed: {}", std::io::Error::last_os_error());
        }
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        Ok(bytes)
    }

    unsafe extern "system" fn protect_raw(
        i: *const CRYPT_INTEGER_BLOB,
        o: *mut CRYPT_INTEGER_BLOB,
    ) -> i32 {
        CryptProtectData(
            i,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            o,
        )
    }

    unsafe extern "system" fn unprotect_raw(
        i: *const CRYPT_INTEGER_BLOB,
        o: *mut CRYPT_INTEGER_BLOB,
    ) -> i32 {
        CryptUnprotectData(
            i,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            o,
        )
    }

    pub fn protect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        call(data, protect_raw)
    }

    pub fn unprotect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        call(data, unprotect_raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).unwrap();
        let second = Store::open(dir.path()).unwrap();
        assert_eq!(first.app_instance_id, second.app_instance_id);
        assert_eq!(
            first.keys.crypt_public_der(),
            second.keys.crypt_public_der()
        );
        assert_eq!(first.app_instance_id.len(), 36);
    }

    #[test]
    fn private_keys_are_not_stored_as_plain_der_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path()).unwrap();
        let raw = std::fs::read_to_string(dir.path().join("crosspaste.json")).unwrap();
        if cfg!(windows) {
            assert!(raw.contains("\"dpapi:"));
            assert!(!raw.contains("\"plain:"));
        }
    }
}
