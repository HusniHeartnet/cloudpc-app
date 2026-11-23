//! Simpan token login "Ingat saya" dengan perlindungan sistem operasi. Port dari client/cloudpc_client/secret_store.py.
//!
//! Windows: DPAPI (CryptProtectData), hanya bisa dibuka oleh akun Windows yang sama di komputer yang sama, disimpan di
//! folder data aplikasi. File-nya `login-rs.dat`, TERPISAH dari `login.dat` aplikasi Python, supaya keduanya bisa dipasang
//! berdampingan tanpa saling menimpa login. Yang disimpan hanya token (bukan password); token batal sendiri bila
//! password diganti atau 30 hari tidak dipakai.
//! macOS: Keychain (`security`), BELUM diuji di Mac sungguhan. Android: file di folder data privat aplikasi (lihat modul
//! `platform` Android di bawah). Sistem lain: tidak disimpan, login tiap aplikasi dibuka.
use std::path::{Path, PathBuf};

use serde_json::Value;

pub struct SecretStore {
    file: PathBuf,
}

impl SecretStore {
    pub fn new(state_dir: &Path) -> Self {
        SecretStore { file: state_dir.join("login-rs.dat") }
    }

    /// True bila tersimpan.
    pub fn save(&self, value: &Value) -> bool {
        #[cfg(target_os = "macos")]
        return keychain::save(&value.to_string());
        #[cfg(not(target_os = "macos"))]
        self.save_file(value)
    }

    pub fn load(&self) -> Option<Value> {
        #[cfg(target_os = "macos")]
        return serde_json::from_str(&keychain::load()?).ok();
        #[cfg(not(target_os = "macos"))]
        self.load_file()
    }

    pub fn clear(&self) {
        #[cfg(target_os = "macos")]
        keychain::clear();
        let _ = std::fs::remove_file(&self.file);
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn save_file(&self, value: &Value) -> bool {
        let Some(blob) = platform::protect(value.to_string().as_bytes()) else { return false };
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&self.file, blob).is_ok()
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn load_file(&self) -> Option<Value> {
        let blob = std::fs::read(&self.file).ok()?;
        serde_json::from_slice(&platform::unprotect(&blob)?).ok()
    }
}

/// macOS Keychain lewat perintah `security` (sama dengan aplikasi Python, tapi service berbeda supaya tidak saling menimpa).
#[cfg(target_os = "macos")]
mod keychain {
    use std::process::Command;

    const SERVICE: &str = "CloudPC Rust";
    const ACCOUNT: &str = "login";

    pub fn save(secret: &str) -> bool {
        Command::new("security")
            .args(["add-generic-password", "-U", "-s", SERVICE, "-a", ACCOUNT, "-w", secret])
            .output()
            .map_or(false, |o| o.status.success())
    }

    pub fn load() -> Option<String> {
        let o = Command::new("security").args(["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"]).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    pub fn clear() {
        let _ = Command::new("security").args(["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT]).output();
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(data_in: *const DataBlob, descr: *const u16, entropy: *const DataBlob, reserved: *mut c_void,
                            prompt: *mut c_void, flags: u32, data_out: *mut DataBlob) -> i32;
        fn CryptUnprotectData(data_in: *const DataBlob, descr: *mut *mut u16, entropy: *const DataBlob, reserved: *mut c_void,
                              prompt: *mut c_void, flags: u32, data_out: *mut DataBlob) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
    const ENTROPY: &[u8] = b"CloudPC Rust login v1"; // tambahan rahasia: blob dari aplikasi lain tidak bisa dibuka di sini

    fn blob(data: &[u8]) -> DataBlob {
        DataBlob { cb_data: data.len() as u32, pb_data: data.as_ptr() as *mut u8 }
    }

    fn take(out: DataBlob) -> Vec<u8> {
        // SAFETY: out diisi oleh DPAPI (pb_data menunjuk cb_data byte yang dialokasikan LocalAlloc); disalin lalu dibebaskan.
        unsafe {
            let v = std::slice::from_raw_parts(out.pb_data, out.cb_data as usize).to_vec();
            LocalFree(out.pb_data as *mut c_void);
            v
        }
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let (src, entropy) = (blob(data), blob(ENTROPY));
        let mut out = DataBlob { cb_data: 0, pb_data: std::ptr::null_mut() };
        let descr: Vec<u16> = "CloudPC".encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: semua pointer hidup selama pemanggilan; DPAPI hanya membaca src/entropy dan mengisi out.
        let ok = unsafe {
            CryptProtectData(&src, descr.as_ptr(), &entropy, std::ptr::null_mut(), std::ptr::null_mut(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let (src, entropy) = (blob(data), blob(ENTROPY));
        let mut out = DataBlob { cb_data: 0, pb_data: std::ptr::null_mut() };
        // SAFETY: lihat protect.
        let ok = unsafe {
            CryptUnprotectData(&src, std::ptr::null_mut(), &entropy, std::ptr::null_mut(), std::ptr::null_mut(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }
}

/// Android (versi percobaan): token disimpan apa adanya di folder data aplikasi, yang hanya bisa dibaca aplikasi ini
/// (sandbox per aplikasi) dan dikecualikan dari cadangan (`allowBackup="false"` di AndroidManifest). HP yang di-root
/// bisa membacanya; enkripsi lewat Android Keystore menyusul sebelum rilis ke umum.
#[cfg(target_os = "android")]
mod platform {
    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        Some(data.to_vec())
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        Some(data.to_vec())
    }
}

#[cfg(not(any(windows, target_os = "android")))]
mod platform {
    pub fn protect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn unprotect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dpapi_roundtrip_hides_token_and_rejects_tampering() {
        let t = tempfile::tempdir().unwrap();
        let s = SecretStore::new(t.path());
        assert_eq!(s.load(), None);
        let secret = json!({"token": "eyJ.rahasia.token", "email": "a@b.id"});
        assert!(s.save(&secret));
        let raw = std::fs::read(t.path().join("login-rs.dat")).unwrap();
        assert!(!raw.windows(5).any(|w| w == b"eyJ.r") && !raw.windows(6).any(|w| w == b"a@b.id")); // terenkripsi
        assert_eq!(s.load(), Some(secret));

        let mut bad = raw.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xFF;
        std::fs::write(t.path().join("login-rs.dat"), bad).unwrap();
        assert_eq!(s.load(), None); // dirusak -> dianggap tidak ada, bukan crash
        s.clear();
        assert!(!t.path().join("login-rs.dat").exists());
    }

    #[test]
    fn python_login_file_is_not_touched() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("login.dat"), b"punya aplikasi python").unwrap();
        let s = SecretStore::new(t.path());
        s.save(&json!({"token": "x"}));
        s.clear();
        assert_eq!(std::fs::read(t.path().join("login.dat")).unwrap(), b"punya aplikasi python");
    }
}
