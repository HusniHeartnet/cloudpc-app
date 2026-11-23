//! Aplikasi pemain CloudPC (Tauri). UI-nya folder yang sama dengan aplikasi Python (client/cloudpc_client/ui), dijembatani
//! oleh `shim.js` dan perintah `call`. Lihat client-rs/README.md untuk status fase.
//!
//! Mode uji tanpa jendela (dipakai tes kontrak):
//!   cloudpc-client --config client_config.json --headless-script '[["login",["a@b.id","pw",false]],["me",[]]]'
//! menjalankan panggilan berurutan pada satu Bridge (token login dipertahankan) dan mencetak array hasilnya.
//!
//! Crate ini berbentuk pustaka supaya bisa dimuat aplikasi Android (Tauri mobile, `run` di bawah); program desktop
//! (`main.rs`) hanya memanggil `desktop_main`. Yang khusus desktop (tray, satu instans, argumen baris perintah, jendela
//! berukuran) ditandai `cfg(desktop)`; yang khusus HP `cfg(mobile)`. Kedua tanda itu dipasang tauri-build.

#[cfg(target_os = "android")]
mod android_stream;
mod bridge;
mod config;
mod http;
mod moonlight;
mod netcheck;
#[cfg(desktop)]
mod paths;
mod secret_store;
mod session;
mod updater;
mod uploads;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, FilePath};
#[cfg(desktop)]
use tauri_plugin_dialog::{MessageDialogButtons, MessageDialogKind};

use bridge::Bridge;

const SHIM: &str = include_str!("shim.js");

/// Jangan memunculkan jendela konsol saat aplikasi menjalankan program lain.
pub fn no_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd;
}

/// Semua panggilan UI lewat sini. Panggilan jaringan dan dialog memblokir, jadi dijalankan di thread terpisah.
#[tauri::command]
async fn call(app: tauri::AppHandle, bridge: tauri::State<'_, Arc<Bridge>>, name: String, args: Vec<Value>) -> Result<Value, String> {
    let b = bridge.inner().clone();
    tauri::async_runtime::spawn_blocking(move || match name.as_str() {
        // Pilih file lalu tampilkan dampaknya ke kuota storage; unggah baru dimulai setelah pemain setuju.
        "pick_files" => {
            let Some(picked) = app.dialog().file().blocking_pick_files() else { return json!({"ok": true, "data": null}) };
            let paths: Vec<Value> = picked.into_iter().filter_map(|p| p.into_path().ok()).map(|p| json!(p.to_string_lossy())).collect();
            b.dispatch("_upload_preview", &[args.first().cloned().unwrap_or(json!(0)), Value::Array(paths)])
        }
        "download" => {
            let filename = args.get(1).and_then(Value::as_str).unwrap_or("file");
            let Some(dest) = app.dialog().file().set_file_name(filename).blocking_save_file() else { return json!({"ok": true, "data": null}) };
            match dest {
                FilePath::Path(p) => b.dispatch("_download", &[args.first().cloned().unwrap_or(json!(0)), json!(p.to_string_lossy())]),
                _ => json!({"ok": false, "error": "Lokasi simpan tidak didukung"}),
            }
        }
        "install_update" => {
            let r = b.dispatch("install_update", &args);
            // Desktop: tutup supaya file bisa diganti. HP: Android yang menutup aplikasi saat pemasangan berjalan (dan pemain
            // bisa membatalkan dialognya), jadi aplikasi dibiarkan hidup.
            if r["ok"] == true && cfg!(desktop) {
                // Beri waktu UI menampilkan "Memasang pembaruan", lalu tutup supaya file bisa diganti; installer membukanya lagi.
                let h = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    h.exit(0);
                });
            }
            r
        }
        other => b.dispatch(other, &args),
    })
    .await
    .map_err(|e| e.to_string())
}

/// Menunggu sampai fase sesi (dipisah koma: "streaming,error") tercapai; mengembalikan state terakhir (atau galat bila lewat waktu).
#[cfg(desktop)]
fn wait_phase(bridge: &Bridge, phases: &str, timeout_s: f64) -> Value {
    let wanted: Vec<&str> = phases.split(',').collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_s);
    loop {
        let st = bridge.engine.get_state();
        if wanted.contains(&st["phase"].as_str().unwrap_or("")) {
            return serde_json::json!({"ok": true, "data": st});
        }
        if std::time::Instant::now() > deadline {
            return serde_json::json!({"ok": false, "error": format!("fase {phases} tidak tercapai"), "data": st});
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// macOS: file yang disalin dari .dmg unduhan ikut bertanda karantina, termasuk client stream di dalam bundel. Pemain hanya
/// menyetujui aplikasi utama sekali ("Open Anyway"); begitu aplikasi ini berjalan, tanda karantina seluruh bundel dihapus
/// supaya CloudPC Stream tidak ikut ditahan Gatekeeper (bundel belum dinotarisasi Apple). Gagal (mis. bundel read-only karena
/// dijalankan langsung dari dmg) tidak apa-apa.
#[cfg(target_os = "macos")]
fn clear_quarantine() {
    let app = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok())
        .and_then(|p| p.ancestors().nth(3).map(std::path::Path::to_path_buf))
        .filter(|a| a.extension().is_some_and(|e| e == "app"));
    if let Some(app) = app {
        let _ = std::process::Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"]).arg(&app)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
}

#[cfg(desktop)]
fn load_bridge(config: Option<PathBuf>) -> Result<Bridge, String> {
    let path = config.unwrap_or_else(config::find_config_path);
    let cfg = config::ClientConfig::load(&path)?;
    Bridge::new(cfg, paths::app_data_dir())
}

// ---- aplikasi HP (Android) --------------------------------------------------------------------------------------
// Di HP tidak ada client_config.json di samping program: config dan sertifikat server yang dikunci ditanam saat build
// (build.rs, variabel CLOUDPC_MOBILE_CONFIG dan CLOUDPC_MOBILE_CA). Tanpa keduanya aplikasi tetap terbangun, tetapi
// menunjuk ke alamat bawaan (localhost) dan hanya berguna untuk uji tampilan.
#[cfg(mobile)]
const MOBILE_CONFIG: &str = include_str!(concat!(env!("OUT_DIR"), "/mobile_config.json"));
#[cfg(mobile)]
const MOBILE_CA: &str = include_str!(concat!(env!("OUT_DIR"), "/mobile_ca.pem"));

#[cfg(mobile)]
fn mobile_bridge(dir: PathBuf) -> Result<Bridge, String> {
    let mut cfg = config::ClientConfig::parse(MOBILE_CONFIG)?;
    cfg.verify_tls = true; // di HP tidak pernah ada alasan mematikan pemeriksaan sertifikat
    // Di HP layar PC ditutup tiap kali pemain kembali ke aplikasi atau membuka aplikasi lain ("Perkecil"), jadi waktu
    // untuk "Sambung lagi" sebelum sesi diakhiri otomatis dibuat 10 menit (desktop 2 menit: di sana jendela stream bisa
    // dikecilkan tanpa ditutup). Kuota tetap berjalan selama itu.
    cfg.reconnect_grace_seconds = cfg.reconnect_grace_seconds.max(600);
    cfg.moonlight_path = None;
    cfg.ca_file = None;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !MOBILE_CA.trim().is_empty() {
        // Klien HTTP membaca sertifikat dari file; tulis salinan yang ditanam ke folder data aplikasi tiap kali dibuka.
        let pem = dir.join("server.pem");
        std::fs::write(&pem, MOBILE_CA).map_err(|e| format!("{}: {e}", pem.display()))?;
        cfg.ca_file = Some(pem.to_string_lossy().into_owned());
    }
    Bridge::new(cfg, dir)
}

#[cfg(mobile)]
static MOBILE_BRIDGE: std::sync::OnceLock<Arc<Bridge>> = std::sync::OnceLock::new();

/// Titik masuk aplikasi HP. Tanpa tray, tanpa satu-instans, tanpa argumen: sistem operasi yang mengatur siklus hidupnya.
#[cfg(mobile)]
#[tauri::mobile_entry_point]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    // Layar PC (pairing + stream) dikerjakan komponen Android terpisah; lihat android_stream.rs.
    #[cfg(target_os = "android")]
    let builder = builder.plugin(android_stream::plugin());
    builder
        .invoke_handler(tauri::generate_handler![call])
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            // Satu Bridge (dan satu mesin sesi) per PROSES. Android boleh membuat ulang layar aplikasi tanpa mematikan
            // prosesnya; kalau tiap kali dibuat Bridge baru, mesin sesi lama (yang masih memegang sesi berjalan) jadi yatim
            // dan layar baru mengira tidak ada sesi.
            let bridge = match MOBILE_BRIDGE.get() {
                Some(existing) => Arc::clone(existing),
                None => {
                    let created = Arc::new(mobile_bridge(dir)?);
                    Arc::clone(MOBILE_BRIDGE.get_or_init(|| created))
                }
            };
            app.manage(bridge);
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into())).initialization_script(SHIM).build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("gagal menjalankan aplikasi");
}

#[cfg(desktop)]
pub fn desktop_main() {
    let mut config: Option<PathBuf> = None;
    let mut script: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config = args.next().map(PathBuf::from),
            "--headless-script" => script = args.next(),
            // Pengait uji: cetak hasil parser statistik Moonlight untuk satu file log, lalu keluar.
            "--parse-log" => {
                let path = args.next().unwrap_or_default();
                match moonlight::parse_stream_log_file(std::path::Path::new(&path)) {
                    Some(v) => println!("{v}"),
                    None => eprintln!("log tidak terbaca: {path}"),
                }
                return;
            }
            _ => {}
        }
    }

    let bridge = match load_bridge(config) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    if let Some(script) = script {
        let steps: Vec<Value> = serde_json::from_str(&script).unwrap_or_default();
        let results: Vec<Value> = steps
            .iter()
            .map(|s| {
                let (name, args) = (s[0].as_str().unwrap_or(""), s[1].as_array().map(Vec::as_slice).unwrap_or(&[]));
                // Pengait uji (hanya mode tanpa jendela): tunggu sampai fase sesi tercapai, mis. ["_wait_phase", ["streaming", 30]].
                if name == "_wait_phase" {
                    return wait_phase(&bridge, args[0].as_str().unwrap_or(""), args.get(1).and_then(Value::as_f64).unwrap_or(30.0));
                }
                // Pengait uji: diam sekian detik (mis. menunggu stream dibuka ulang setelah ganti kualitas).
                if name == "_sleep" {
                    std::thread::sleep(std::time::Duration::from_secs_f64(args.first().and_then(Value::as_f64).unwrap_or(1.0)));
                    return json!({"ok": true, "data": null});
                }
                // Pengait uji: tunggu unduhan pembaruan selesai (ready/error); mengembalikan update_state.
                if name == "_wait_update" {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(args.first().and_then(Value::as_f64).unwrap_or(30.0));
                    while std::time::Instant::now() < deadline && !matches!(bridge.updater.state()["status"].as_str(), Some("ready" | "error")) {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    return json!({"ok": true, "data": bridge.updater.state()});
                }
                // Pengait uji: tunggu sampai tidak ada unggahan yang berjalan; mengembalikan daftar unggahan.
                if name == "_wait_uploads" {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(args.first().and_then(Value::as_f64).unwrap_or(60.0));
                    while bridge.uploads.active_count() > 0 && std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    return json!({"ok": bridge.uploads.active_count() == 0, "data": bridge.uploads.views()});
                }
                bridge.dispatch(name, args)
            })
            .collect();
        println!("{}", Value::Array(results));
        return;
    }

    #[cfg(target_os = "macos")]
    std::thread::spawn(clear_quarantine);

    let brand = bridge.cfg.brand_name.clone();
    let builder = tauri::Builder::default();
    // Satu aplikasi saja: membuka CloudPC lagi saat ia tersembunyi di tray cukup menampilkan jendela yang ada. Dua aplikasi
    // bersamaan = dua mesin sesi yang sama-sama membuka stream. Build debug dilewati supaya alat uji bisa jalan berdampingan
    // dengan aplikasi terpasang (identifier-nya sama).
    #[cfg(not(debug_assertions))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| show_main(app)));
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .manage(bridge)
        .invoke_handler(tauri::generate_handler![call])
        .setup(move |app| {
            let bridge = app.state::<Arc<Bridge>>().inner().clone();
            let wb = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title(brand.as_str())
                .inner_size(1100.0, 720.0)
                .min_inner_size(900.0, 620.0)
                .initialization_script(SHIM);
            // Pengait uji (HANYA build debug, HANYA bila variabel diisi): port DevTools WebView2 untuk memeriksa UI.
            #[cfg(all(windows, debug_assertions))]
            let wb = match std::env::var("CLOUDPC_DEBUG_PORT") {
                Ok(port) => wb.additional_browser_args(&format!(
                    "--remote-debugging-port={port} --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection")),
                Err(_) => wb,
            };
            let window = wb.build()?;
            build_tray(app, &brand)?;
            // Tombol X saat sesi atau unggahan berjalan = sembunyikan ke tray (hidden icons), bukan keluar. Dulu aplikasi keluar
            // tapi jendela stream tertinggal, lalu saat dibuka lagi dan disambung ulang stream-nya jadi dobel (16 Nov 2026).
            // Keluar sungguhan lewat menu tray "Keluar".
            let w = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if bridge.engine.busy() || bridge.uploads.active_count() > 0 {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("gagal menjalankan aplikasi");
    app.run(|handle, event| match event {
        // Jalan keluar apa pun (menu Keluar, pembaruan, jendela terakhir ditutup): jendela stream tidak boleh tertinggal.
        tauri::RunEvent::Exit => {
            let b = handle.state::<Arc<Bridge>>();
            b.uploads.save();
            b.engine.shutdown();
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show_main(handle),
        _ => {}
    });
}

/// Tampilkan lagi jendela utama (dari tray, dari ikon Dock, atau saat aplikasi dibuka kedua kali).
#[cfg(desktop)]
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg(desktop)]
fn build_tray(app: &tauri::App, brand: &str) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let open = MenuItem::with_id(app, "open", format!("Buka {brand}"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Keluar", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip(brand)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Menu tray "Keluar". Sesi yang masih berjalan diakhiri dulu dengan benar (disk dilepas, pairing dicabut, kuota berhenti
/// dihitung); tanpa itu server baru mengakhirinya setelah detak berhenti ±3 menit.
#[cfg(desktop)]
fn quit_app(app: &tauri::AppHandle) {
    let bridge = app.state::<Arc<Bridge>>().inner().clone();
    let brand = bridge.cfg.brand_name.clone();
    let session = bridge.engine.busy();
    let uploads = bridge.uploads.active_count();
    if !session && uploads == 0 {
        app.exit(0);
        return;
    }
    show_main(app);
    let mut msg = String::new();
    if session {
        msg.push_str("Sesi masih berjalan. Keluar akan MENGAKHIRI sesi (data di drive P: tetap tersimpan).

");
    }
    if uploads > 0 {
        msg.push_str(&format!("{uploads} file masih diunggah; unggahan dijeda dan otomatis dilanjutkan saat aplikasi dibuka lagi.

"));
    }
    msg.push_str(if session { "Akhiri sesi dan keluar?" } else { "Keluar sekarang?" });
    let h = app.clone();
    app.dialog().message(msg).title(brand.as_str()).kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNo)
        .show(move |yes| {
            if !yes {
                return;
            }
            if !session {
                h.exit(0);
                return;
            }
            // Layar sesi menampilkan "Menyimpan data dan menutup sesi..."; keluar setelah selesai (paling lama 60 dtk).
            bridge.engine.end();
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while bridge.engine.busy() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                h.exit(0);
            });
        });
}
