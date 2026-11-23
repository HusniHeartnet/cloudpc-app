//! Layar PC di Android. Di desktop pairing dan stream dijalankan sebagai PROSES client stream (Moonlight atau
//! `cloudpc-stream-client`); di Android tidak ada proses berjendela, jadi keduanya dilakukan komponen Android
//! `id.cloudpc.stream` (layar `StreamActivity` + pustaka `libcloudpc_stream.so` dari `stream-rs/android`).
//!
//! Aplikasi ini TIDAK menautkan kode stream-rs (GPL): ia hanya meminta lewat plugin Kotlin `StreamPlugin`
//! (`gen/android/.../StreamPlugin.kt`), sama seperti desktop yang hanya menjalankan program lain. Bagi mesin sesi
//! (`session.rs`) hasilnya tetap sebuah `moonlight::Proc`: "berjalan", "kode keluar", dan file log untuk statistik.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::Wry;

use crate::moonlight::Proc;

/// Pegangan plugin TERBARU. Diganti tiap `run` (lib.rs) dipanggil: Android bisa membuat ulang layar aplikasi dalam proses
/// yang sama, dan pegangan lama menunjuk layar yang sudah tidak ada.
static PLUGIN: Mutex<Option<Arc<PluginHandle<Wry>>>> = Mutex::new(None);

/// Plugin Tauri yang memuat `StreamPlugin` Kotlin. Didaftarkan di `run` (lib.rs).
pub fn plugin() -> TauriPlugin<Wry> {
    Builder::new("cloudpc-stream")
        .setup(|_app, api| {
            let handle = api.register_android_plugin("id.cloudpc.app", "StreamPlugin")?;
            *PLUGIN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(handle));
            Ok(())
        })
        .build()
}

pub fn available() -> bool {
    PLUGIN.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

fn call(command: &str, payload: Value) -> Result<Value, String> {
    // Pegangan disalin lalu kunci dilepas: pairing memblokir lama, dan panggilan lain (tanya keadaan) tidak boleh ikut menunggu.
    let handle = PLUGIN.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or("Layar PC belum siap di HP ini.")?;
    handle.run_mobile_plugin::<Value>(command, payload).map_err(|e| format!("Layar PC: {e}"))
}

/// Serahkan APK pembaruan yang sudah diunduh (dan dicek SHA-256-nya) ke pemasang sistem Android.
pub fn install_apk(path: &std::path::Path) -> Result<(), String> {
    call("installApk", json!({"path": path.to_string_lossy()})).map(|_| ())
}

pub enum Job {
    /// Pairing berjalan di thread sendiri (memblokir sampai PC menerima PIN); hasilnya kode keluar 0/1.
    Pair { thread: Option<JoinHandle<i32>>, code: Option<i32> },
    /// Layar stream terbuka; keadaannya ditanyakan ke plugin.
    Stream { code: Option<i32> },
}

impl Job {
    pub fn running(&mut self) -> bool {
        match self {
            Job::Pair { thread, code } => {
                if thread.as_ref().is_some_and(|t| t.is_finished()) {
                    *code = Some(thread.take().and_then(|t| t.join().ok()).unwrap_or(1));
                }
                code.is_none()
            }
            Job::Stream { code } => {
                if code.is_some() {
                    return false;
                }
                // Plugin yang tidak menjawab dianggap layar sudah tertutup (aplikasi sedang dimatikan).
                let state = call("state", json!({})).unwrap_or_else(|_| json!({"running": false, "error": "tidak ada jawaban"}));
                if state["running"].as_bool().unwrap_or(false) {
                    return true;
                }
                *code = Some(if state["error"].as_str().unwrap_or("").is_empty() { 0 } else { 1 });
                false
            }
        }
    }

    pub fn exit_code(&mut self) -> Option<i32> {
        if self.running() {
            return None;
        }
        match self {
            Job::Pair { code, .. } | Job::Stream { code } => *code,
        }
    }

    /// Minta berhenti. Pairing tidak bisa dibatalkan di tengah jalan (selesai sendiri saat PC menjawab atau waktu habis).
    pub fn stop(&mut self) {
        if let Job::Stream { .. } = self {
            let _ = call("stop", json!({}));
        }
    }
}

/// Pairing dengan PIN. Galat ditulis ke `log` supaya mesin sesi menampilkannya seperti log Moonlight.
pub fn pair(host: &str, pin: &str, log: PathBuf) -> Result<Proc, String> {
    if !available() {
        return Err("Layar PC belum siap di HP ini.".into());
    }
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&log, "");
    let (host, pin, path) = (host.to_string(), pin.to_string(), log.clone());
    let thread = std::thread::spawn(move || {
        let error = match call("pair", json!({"host": host, "pin": pin})) {
            Ok(result) => result["error"].as_str().unwrap_or("").to_string(),
            Err(e) => e,
        };
        if error.is_empty() {
            0
        } else {
            let _ = std::fs::write(&path, &error);
            1
        }
    });
    Ok(Proc::android(Job::Pair { thread: Some(thread), code: None }, log))
}

/// Buka layar stream. `resolution` "1280x720", sisanya angka sebagai teks (sama dengan argumen client desktop).
pub fn stream(host: &str, app: &str, resolution: &str, fps: &str, bitrate_kbps: &str, codec: &str, log: PathBuf) -> Result<Proc, String> {
    let (width, height) = resolution.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))).unwrap_or((1280, 720));
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(&log); // statistik stream sebelumnya tidak boleh terbaca sebagai milik stream ini
    call(
        "start",
        json!({
            "host": host, "app": app, "width": width, "height": height,
            "fps": fps.parse::<u32>().unwrap_or(60), "bitrateKbps": bitrate_kbps.parse::<u32>().unwrap_or(8000),
            "codec": codec, "logPath": log.to_string_lossy(),
        }),
    )?;
    Ok(Proc::android(Job::Stream { code: None }, log))
}
