//! Pembaruan aplikasi dari dalam aplikasi ("Update sekarang"). Port dari client/cloudpc_client/updater.py + bagian pembaruan
//! di ClientCore, dengan satu perbedaan penting: aplikasi Rust memakai kanal rilis TERPISAH ("windows-rs"), jadi tidak
//! pernah ditawari installer aplikasi Python (dan sebaliknya).
//!
//! Alur (Windows): cek /client/latest -> unduh installer di latar belakang + cek SHA-256 -> pemain memilih "Mulai ulang" ->
//! installer dijalankan tanpa jendela (/VERYSILENT /RELAUNCH), aplikasi menutup diri, installer membuka aplikasi baru ->
//! aplikasi baru menampilkan hasil pembaruan.
//! macOS (BELUM diuji di Mac sungguhan): kanal "macos-rs", file .dmg; skrip kecil menunggu aplikasi tertutup, mengganti
//! CloudPC Rust.app, lalu membukanya lagi (sama dengan aplikasi Python).
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg_attr(target_os = "android", allow(unused_imports))]
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use reqwest::Method;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::http::{ApiError, Http};

/// Versi build resmi (CI menanamnya lewat variabel CLOUDPC_RS_VERSION, mis. "2026.10.30.7"); build lokal "0.0.0" selalu
/// dianggap lebih lama dari rilis mana pun.
pub const BUILD_VERSION: &str = match option_env!("CLOUDPC_RS_VERSION") {
    Some(v) => v,
    None => "0.0.0",
};
#[cfg(not(any(target_os = "macos", target_os = "android")))]
pub const PLATFORM: &str = "windows-rs";
#[cfg(target_os = "macos")]
pub const PLATFORM: &str = "macos-rs";
/// Aplikasi HP: kanal sendiri, berkasnya APK yang dipasang lewat pemasang sistem Android.
#[cfg(target_os = "android")]
pub const PLATFORM: &str = "android-rs";
#[cfg(not(any(target_os = "macos", target_os = "android")))]
const INSTALLER: &str = "CloudPCRustSetup.exe";
#[cfg(target_os = "macos")]
const INSTALLER: &str = "CloudPCRust.dmg";
#[cfg(target_os = "android")]
const INSTALLER: &str = "CloudPC-update.apk";

pub fn display_version() -> String {
    format!("rs-{BUILD_VERSION}")
}

#[derive(Clone)]
struct UpdateState {
    status: String, // idle -> downloading -> ready -> installing (atau error)
    progress: u64,
    done: u64,
    total: u64,
    check: Option<Value>,
    error: Option<String>,
}

pub struct Updater {
    http: Arc<Http>,
    dir: PathBuf,
    state: Mutex<UpdateState>,
    file: Mutex<Option<PathBuf>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Perintah pemasangan senyap: tanpa jendela installer, tutup aplikasi lama, buka lagi aplikasi baru.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn install_command(installer: &Path) -> (PathBuf, Vec<String>) {
    let args = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/RELAUNCH"];
    (installer.to_path_buf(), args.iter().map(|s| s.to_string()).collect())
}

/// Aplikasi hasil `cargo build` (di dalam folder target/) bukan aplikasi terpasang: tidak boleh menjalankan installer.
pub fn is_installed_build() -> bool {
    std::env::current_exe().ok().map_or(false, |p| !p.ancestors().any(|a| a.file_name().is_some_and(|n| n == "target")))
}

impl Updater {
    pub fn new(http: Arc<Http>, state_dir: &Path) -> Arc<Updater> {
        Arc::new(Updater {
            http,
            dir: state_dir.join("updates"),
            state: Mutex::new(UpdateState { status: "idle".into(), progress: 0, done: 0, total: 0, check: None, error: None }),
            file: Mutex::new(None),
            thread: Mutex::new(None),
        })
    }

    pub fn state(&self) -> Value {
        let s = self.state.lock().unwrap();
        json!({"status": s.status, "progress": s.progress, "done": s.done, "total": s.total, "check": s.check, "error": s.error})
    }

    pub fn check(&self) -> Result<Value, ApiError> {
        let r = self.http.call(Method::GET, &format!("/client/latest?platform={PLATFORM}&version={BUILD_VERSION}"), None)
            .map_err(|_| ApiError::new("Tidak bisa memeriksa pembaruan. Periksa koneksi internet."))?;
        let mut info = r;
        info["current"] = json!(BUILD_VERSION);
        self.state.lock().unwrap().check = Some(info.clone());
        Ok(info)
    }

    /// Sekali setelah aplikasi dibuka lagi oleh installer: {"ok": true/false, "version", "target"}.
    pub fn take_result(&self) -> Option<Value> {
        let f = self.dir.join("pending.json");
        let data: Value = serde_json::from_str(&fs::read_to_string(&f).ok()?).ok()?;
        let _ = fs::remove_file(&f);
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let _ = fs::remove_file(e.path()); // installer lama tidak diperlukan lagi
            }
        }
        let target = data["to"].as_str().map(str::to_string);
        Some(json!({"ok": target.as_deref() == Some(BUILD_VERSION), "version": BUILD_VERSION, "target": target}))
    }

    /// Unduh versi terbaru di latar belakang (aplikasi tetap bisa dipakai). Tidak saat sesi berjalan, supaya unduhan
    /// tidak mengganggu stream.
    pub fn download(self: &Arc<Self>, busy: bool) -> Result<(), ApiError> {
        if busy {
            return Err(ApiError::new("Pembaruan diunduh setelah sesi selesai, supaya stream tidak terganggu."));
        }
        if self.thread.lock().unwrap().as_ref().is_some_and(|t| !t.is_finished()) {
            return Ok(());
        }
        if self.state.lock().unwrap().status == "ready" && self.file.lock().unwrap().as_ref().is_some_and(|f| f.exists()) {
            return Ok(());
        }
        let cached = self.state.lock().unwrap().check.clone();
        let info = match cached {
            Some(c) => c,
            None => self.check()?,
        };
        if info["update_available"] != true {
            return Err(ApiError::new("Aplikasi sudah versi terbaru."));
        }
        let latest = info["latest"].clone();
        {
            let mut s = self.state.lock().unwrap();
            *s = UpdateState { status: "downloading".into(), progress: 0, done: 0, total: latest["size"].as_u64().unwrap_or(0),
                               check: s.check.clone(), error: None };
        }
        let me = Arc::clone(self);
        *self.thread.lock().unwrap() = Some(thread::spawn(move || match me.fetch(&latest) {
            Ok(path) => {
                *me.file.lock().unwrap() = Some(path);
                let mut s = me.state.lock().unwrap();
                s.status = "ready".into();
                s.progress = 100;
            }
            Err(e) => {
                let mut s = me.state.lock().unwrap();
                s.status = "error".into();
                s.error = Some(e);
            }
        }));
        Ok(())
    }

    /// Unduh installer dan pastikan SHA-256 sama dengan yang diumumkan server.
    fn fetch(&self, latest: &Value) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let (part, fin) = (self.dir.join(format!("{INSTALLER}.part")), self.dir.join(INSTALLER));
        let mut r = self.http.get_stream(&format!("/client/download/{PLATFORM}"), None).map_err(|e| e.message)?;
        if r.status().as_u16() >= 400 {
            return Err("Installer pembaruan belum tersedia di server.".into());
        }
        let total = r.content_length().or(latest["size"].as_u64()).unwrap_or(0);
        let (mut out, mut h, mut done) = (fs::File::create(&part).map_err(|e| e.to_string())?, Sha256::new(), 0u64);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = r.read(&mut buf).map_err(|_| "Gagal mengunduh pembaruan. Periksa koneksi lalu coba lagi.".to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            h.update(&buf[..n]);
            done += n as u64;
            let mut s = self.state.lock().unwrap();
            s.done = done;
            s.total = total;
            s.progress = if total > 0 { done * 100 / total } else { 0 };
        }
        drop(out);
        if Some(hex(&h.finalize()).as_str()) != latest["sha256"].as_str() {
            let _ = fs::remove_file(&part);
            return Err("File pembaruan rusak saat diunduh (checksum tidak cocok). Coba lagi.".into());
        }
        fs::rename(&part, &fin).map_err(|e| e.to_string())?;
        Ok(fin)
    }

    /// Pasang pembaruan yang sudah diunduh. Pemanggil harus menutup aplikasi sesudahnya (installer membukanya lagi).
    pub fn install(&self, busy: bool) -> Result<(), ApiError> {
        if busy {
            return Err(ApiError::new("Akhiri sesi dulu sebelum memasang pembaruan."));
        }
        let file = self.file.lock().unwrap().clone().filter(|f| f.exists());
        let (status, check) = {
            let s = self.state.lock().unwrap();
            (s.status.clone(), s.check.clone())
        };
        let Some(installer) = file.filter(|_| status == "ready") else {
            return Err(ApiError::new("Pembaruan belum selesai diunduh."));
        };
        if !is_installed_build() {
            return Err(ApiError::new("Pembaruan otomatis hanya untuk aplikasi yang terpasang (bukan mode pengembangan)."));
        }
        if !cfg!(windows) && !cfg!(target_os = "macos") && !cfg!(target_os = "android") {
            return Err(ApiError::new("Sistem operasi ini belum didukung untuk pembaruan otomatis."));
        }
        // Catatan untuk aplikasi yang terbuka lagi: berhasil (versi sama dengan target) atau gagal.
        let target = check.as_ref().and_then(|c| c["latest"]["version"].as_str().map(str::to_string));
        fs::create_dir_all(&self.dir).map_err(|e| ApiError::new(e.to_string()))?;
        fs::write(self.dir.join("pending.json"), json!({"from": BUILD_VERSION, "to": target}).to_string())
            .map_err(|e| ApiError::new(e.to_string()))?;
        // Android: serahkan APK ke pemasang sistem. Pemain masih harus menekan "Update" di dialog Android (pemasangan
        // diam-diam tidak diizinkan), dan bisa membatalkannya, jadi status tetap "ready" supaya bisa dicoba lagi. Aplikasi
        // ditutup Android sendiri saat pemasangan benar-benar berjalan.
        #[cfg(target_os = "android")]
        {
            crate::android_stream::install_apk(&installer).map_err(ApiError::new)?;
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            mac::install(&installer).map_err(ApiError::new)?;
            self.state.lock().unwrap().status = "installing".into();
            return Ok(());
        }
        #[cfg(not(any(target_os = "macos", target_os = "android")))]
        let (exe, args) = install_command(&installer);
        #[cfg(not(any(target_os = "macos", target_os = "android")))]
        let mut cmd = Command::new(exe);
        #[cfg(not(any(target_os = "macos", target_os = "android")))]
        cmd.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        }
        #[cfg(not(any(target_os = "macos", target_os = "android")))]
        {
            cmd.spawn().map_err(|e| ApiError::new(format!("Gagal menjalankan installer: {e}")))?;
            self.state.lock().unwrap().status = "installing".into();
            Ok(())
        }
    }
}

/// Pemasangan di macOS: buka .dmg, salin .app ke folder sementara, lalu skrip terpisah menggantinya setelah aplikasi ini tertutup.
#[cfg(target_os = "macos")]
mod mac {
    use super::*;

    const HELPER: &str = r#"#!/bin/bash
# Dijalankan updater CloudPC: tunggu aplikasi lama tertutup, ganti, lalu buka versi baru.
pid="$1"; target="$2"; staged="$3"
while kill -0 "$pid" 2>/dev/null; do sleep 0.5; done
rm -rf "$target.old"
if mv "$target" "$target.old" && ditto "$staged" "$target"; then
    rm -rf "$target.old"
else
    rm -rf "$target"; mv "$target.old" "$target"   # gagal: kembalikan versi lama
fi
xattr -cr "$target" 2>/dev/null
rm -rf "$(dirname "$staged")"
open "$target"
"#;

    /// CloudPC Rust.app/Contents/MacOS/<exe> -> CloudPC Rust.app
    fn app_path() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?.canonicalize().map_err(|e| e.to_string())?;
        exe.ancestors().nth(3).filter(|a| a.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
            .ok_or_else(|| "Lokasi aplikasi tidak dikenali; pasang ulang dari file dmg.".to_string())
    }

    pub fn install(dmg: &Path) -> Result<(), String> {
        let target = app_path()?;
        let work = std::env::temp_dir().join(format!("cloudpc-rs-update-{}", std::process::id()));
        let mnt = work.join("mnt");
        fs::create_dir_all(&mnt).map_err(|e| e.to_string())?;
        let staged = work.join("CloudPC Rust.app");
        let run = |prog: &str, args: &[&std::ffi::OsStr]| Command::new(prog).args(args).status().map(|s| s.success()).unwrap_or(false);
        let attached = run("hdiutil", &["attach".as_ref(), "-quiet".as_ref(), "-nobrowse".as_ref(), "-readonly".as_ref(),
                                        "-mountpoint".as_ref(), mnt.as_os_str(), dmg.as_os_str()]);
        let copied = attached && run("ditto", &[mnt.join("CloudPC Rust.app").as_os_str(), staged.as_os_str()]);
        if attached {
            run("hdiutil", &["detach".as_ref(), "-quiet".as_ref(), mnt.as_os_str()]);
        }
        if !copied {
            let _ = fs::remove_dir_all(&work);
            return Err("Gagal membuka file pembaruan.".into());
        }
        let helper = work.join("pasang.sh");
        fs::write(&helper, HELPER).map_err(|e| e.to_string())?;
        Command::new("/bin/bash").arg(&helper).arg(std::process::id().to_string()).arg(&target).arg(&staged)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().map_err(|e| format!("Gagal menjalankan pemasang: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;

    fn updater(dir: &Path) -> Arc<Updater> {
        Updater::new(Arc::new(Http::new(&ClientConfig { api_url: "http://127.0.0.1:9".into(), ..ClientConfig::default() }).unwrap()), dir)
    }

    #[test]
    fn install_command_is_silent_and_relaunches() {
        let (exe, args) = install_command(Path::new(r"C:\x\CloudPCRustSetup.exe"));
        assert_eq!(exe, PathBuf::from(r"C:\x\CloudPCRustSetup.exe"));
        assert_eq!(args, ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/RELAUNCH"]);
    }

    #[test]
    fn cargo_build_output_is_not_an_installed_app() {
        assert!(!is_installed_build()); // tes berjalan dari <repo>/target/debug
    }

    #[test]
    fn local_build_version_is_oldest_and_display_is_prefixed() {
        assert_eq!(BUILD_VERSION, "0.0.0");
        assert_eq!(display_version(), "rs-0.0.0");
    }

    #[test]
    fn update_result_reports_success_only_when_versions_match_then_cleans_up() {
        let t = tempfile::tempdir().unwrap();
        let u = updater(t.path());
        assert_eq!(u.take_result(), None);
        fs::create_dir_all(t.path().join("updates")).unwrap();
        fs::write(t.path().join("updates").join("pending.json"), r#"{"from":"0.0.0","to":"0.0.0"}"#).unwrap();
        fs::write(t.path().join("updates").join(INSTALLER), b"lama").unwrap();
        assert_eq!(u.take_result().unwrap()["ok"], true);
        assert!(!t.path().join("updates").join(INSTALLER).exists() && u.take_result().is_none()); // sekali saja

        fs::write(t.path().join("updates").join("pending.json"), r#"{"from":"0.0.0","to":"2026.10.30.1"}"#).unwrap();
        let r = u.take_result().unwrap();
        assert_eq!((r["ok"].clone(), r["target"].clone()), (json!(false), json!("2026.10.30.1")));
    }

    #[test]
    fn install_requires_downloaded_update_and_no_session() {
        let t = tempfile::tempdir().unwrap();
        let u = updater(t.path());
        assert!(u.install(true).unwrap_err().message.contains("Akhiri sesi"));
        assert!(u.install(false).unwrap_err().message.contains("belum selesai diunduh"));
        assert!(u.download(true).unwrap_err().message.contains("setelah sesi selesai"));
    }
}
