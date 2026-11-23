//! Jembatan antara UI (JavaScript) dan logika klien. UI memanggil `window.pywebview.api.<nama>(...args)` dan menerima
//! `{ok, data}` atau `{ok:false, error}`; `shim.js` meneruskan semua panggilan itu ke `dispatch` di sini, jadi UI Python
//! dan UI Rust adalah folder yang sama.
//!
//! FASE K1: akun (login, daftar, ganti password, profil), data (me, file, add-on, feedback), cek jaringan, preferensi.
//! FASE K2a: sesi (antrean, pairing, stream, sambung ulang) lewat `SessionEngine`.
//! FASE K2b: unggah/unduh file. FASE K3: "Ingat saya" (DPAPI) dan pembaruan otomatis kanal "windows-rs".
use std::path::PathBuf;
use std::sync::Arc;

use reqwest::Method;
use serde_json::{json, Map, Value};

use crate::config::ClientConfig;
use crate::http::{ApiError, Http};
use crate::secret_store::SecretStore;
use crate::session::SessionEngine;
use crate::updater::{self, Updater};
use crate::uploads::Uploads;
use crate::{moonlight, netcheck};

const SUNSHINE_TCP_PORT: u16 = 47984; // port HTTPS Sunshine yang dipakai Moonlight; dipakai untuk mengukur RTT
const PREF_KEYS: [&str; 8] = ["quality", "connection", "city", "isp", "isp_other", "email", "route", "mode"];

pub struct Bridge {
    pub cfg: ClientConfig,
    http: Arc<Http>,
    state_dir: PathBuf,
    pub engine: Arc<SessionEngine>,
    pub uploads: Arc<Uploads>,
    pub updater: Arc<Updater>,
    secrets: SecretStore,
}

fn platform() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn arg_str<'a>(args: &'a [Value], i: usize) -> &'a str {
    args.get(i).and_then(Value::as_str).unwrap_or("")
}

fn arg_i64(args: &[Value], i: usize) -> i64 {
    args.get(i).and_then(Value::as_i64).unwrap_or(0)
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

impl Bridge {
    pub fn new(cfg: ClientConfig, state_dir: PathBuf) -> Result<Bridge, String> {
        let http = Arc::new(Http::new(&cfg)?);
        let engine = SessionEngine::new(cfg.clone(), Arc::clone(&http), state_dir.join("logs"));
        let uploads = Uploads::new(Arc::clone(&http), &state_dir);
        let updater = Updater::new(Arc::clone(&http), &state_dir);
        let secrets = SecretStore::new(&state_dir);
        Ok(Bridge { cfg, http, state_dir, engine, uploads, updater, secrets })
    }

    pub fn dispatch(&self, name: &str, args: &[Value]) -> Value {
        match self.run(name, args) {
            Ok(v) => json!({"ok": true, "data": v}),
            Err(e) => json!({"ok": false, "error": e.message}),
        }
    }

    /// Kendali satu unggahan berdasarkan id (argumen pertama).
    fn job(&self, a: &[Value], f: impl FnOnce(&Arc<Uploads>, &str)) -> Value {
        f(&self.uploads, arg_str(a, 0));
        Value::Null
    }

    fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, ApiError> {
        self.http.call(method, path, body.as_ref())
    }

    /// `remember_email`: Some = simpan token "Ingat saya"; None = hapus login yang diingat sebelumnya.
    fn after_auth(&self, r: Value, remember_email: Option<&str>) -> Result<Value, ApiError> {
        let token = r["token"].as_str().ok_or_else(|| ApiError::new("Server tidak mengirim token login"))?;
        self.http.set_token(Some(token.to_string()));
        let me = self.call(Method::GET, "/me", None)?;
        match remember_email {
            Some(email) => {
                self.secrets.save(&json!({"token": token, "email": email}));
            }
            None => self.secrets.clear(), // login tanpa "Ingat saya" menimpa login yang diingat sebelumnya
        }
        if let Some(uid) = me["user"]["id"].as_i64() {
            self.uploads.set_user(uid); // unggahan yang belum selesai milik akun ini dilanjutkan
        }
        Ok(me)
    }

    /// Login otomatis dengan token "Ingat saya" (aplikasi dibuka lagi, termasuk setelah update). Token diperpanjang setiap
    /// kali; token yang ditolak server (kedaluwarsa, password diganti) dihapus.
    fn resume_login(&self) -> Value {
        let Some(stored) = self.secrets.load() else { return Value::Null };
        let Some(token) = stored["token"].as_str().filter(|t| !t.is_empty()) else { return Value::Null };
        self.http.set_token(Some(token.to_string()));
        let email = stored["email"].as_str().unwrap_or("").to_string();
        match self.call(Method::POST, "/auth/refresh", None).and_then(|r| self.after_auth(r, Some(&email))) {
            Ok(me) => me,
            Err(e) => {
                self.http.set_token(None);
                if matches!(e.status, Some(401 | 403)) {
                    self.secrets.clear();
                }
                Value::Null // server tidak terjangkau: token tetap disimpan, coba lagi saat dibuka berikutnya
            }
        }
    }

    fn run(&self, name: &str, a: &[Value]) -> Result<Value, ApiError> {
        match name {
            // `platform`: UI memakai tata letak dan batasan HP bila "android"/"ios" (lihat IS_MOBILE di app.js).
            "config" => Ok(json!({"brand_name": self.cfg.brand_name, "version": updater::display_version(),
                                   "platform": platform(),
                                   "update_result": self.updater.take_result(),
                                   "moonlight_installed": moonlight::find_moonlight(&self.cfg).is_some()})),
            // Kanal rilis TERPISAH per platform ("windows-rs", "macos-rs", "android-rs"): aplikasi Rust tidak pernah ditawari
            // installer aplikasi Python, dan HP tidak pernah ditawari installer desktop.
            "check_update" => Ok(self.updater.check()?),
            "update_state" => Ok(self.updater.state()),
            "download_update" => {
                self.updater.download(self.engine.busy())?;
                Ok(Value::Null)
            }
            "install_update" => {
                self.updater.install(self.engine.busy())?;
                Ok(Value::Null) // main.rs menutup aplikasi beberapa detik kemudian; installer membukanya lagi
            }
            "login" => {
                let remember = a.get(2).and_then(Value::as_bool).unwrap_or(true);
                let body = json!({"email": arg_str(a, 0), "password": arg_str(a, 1), "remember": remember});
                let r = self.call(Method::POST, "/auth/login", Some(body))?;
                self.after_auth(r, remember.then(|| arg_str(a, 0)))
            }
            "resume_login" => Ok(self.resume_login()),
            // Lupa password: minta kode (admin mengirimnya lewat WhatsApp/email), lalu ganti password dengan kode itu dan masuk.
            "forgot_password" => match self.call(Method::POST, "/auth/forgot", Some(json!({"email": arg_str(a, 0)}))) {
                Err(e) if e.status == Some(404) => Err(ApiError::new("Server ini belum mendukung lupa password. Hubungi admin CloudPC.")),
                r => r.map(|_| Value::Null),
            },
            "reset_password" => {
                let remember = a.get(3).and_then(Value::as_bool).unwrap_or(true);
                let body = json!({"email": arg_str(a, 0), "code": arg_str(a, 1), "new_password": arg_str(a, 2), "remember": remember});
                let r = self.call(Method::POST, "/auth/reset", Some(body))?;
                self.after_auth(r, remember.then(|| arg_str(a, 0)))
            }
            "register" => {
                let d = a.first().cloned().unwrap_or(Value::Null);
                let mut body = Map::new();
                for k in ["invite_code", "email", "name", "password", "city", "isp", "phone"] {
                    body.insert(k.into(), json!(d[k].as_str().unwrap_or("")));
                }
                body.insert("remember".into(), json!(d["remember"].as_bool().unwrap_or(true)));
                let remember = d["remember"].as_bool().unwrap_or(true);
                let r = self.call(Method::POST, "/auth/register", Some(Value::Object(body)))?;
                self.after_auth(r, remember.then(|| d["email"].as_str().unwrap_or("")))
            }
            "change_password" => {
                let r = self.call(Method::POST, "/me/password",
                                  Some(json!({"old_password": arg_str(a, 0), "new_password": arg_str(a, 1)})))?;
                let token = r["token"].as_str().map(str::to_string); // token lama tidak berlaku lagi
                self.http.set_token(token.clone());
                if let (Some(mut stored), Some(t)) = (self.secrets.load(), token) {
                    stored["token"] = json!(t);
                    self.secrets.save(&stored);
                }
                Ok(Value::Null)
            }
            "update_profile" => {
                let d = a.first().cloned().unwrap_or(Value::Null);
                let allowed: Map<String, Value> = ["name", "city", "isp", "phone"]
                    .iter()
                    .filter_map(|k| d.get(*k).map(|v| ((*k).to_string(), v.clone())))
                    .collect();
                self.call(Method::PATCH, "/me/profile", Some(Value::Object(allowed)))
            }
            "logout" => {
                self.secrets.clear();
                self.uploads.clear_user();
                self.http.set_token(None);
                self.engine.reset(); // buang status sesi & hasil cek koneksi milik akun sebelumnya
                Ok(Value::Null)
            }
            "me" => self.call(Method::GET, "/me", None),
            "occupancy" => {
                // Nama pool dibatasi huruf kecil/angka/-/_ (sama dengan aturan `set-pool`), jadi aman ditaruh di URL.
                let pool = a.first().and_then(Value::as_str).filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')).unwrap_or("gaming");
                self.call(Method::GET, &format!("/public/occupancy?pool={pool}"), None)
            }
            "net_check" => Ok(self.net_check(a.first())),
            "net_burst" => self.net_burst(a.first().and_then(Value::as_f64)),
            "state" => Ok(self.engine.get_state()),
            "start_session" => {
                let route = a.get(3).and_then(Value::as_str).filter(|r| !r.is_empty()).unwrap_or("auto");
                let mode = if a.get(4).and_then(Value::as_str) == Some("ephemeral") { "ephemeral" } else { "profile" };
                self.engine.start(arg_i64(a, 0), a.get(1).cloned().unwrap_or(json!({})), a.get(2).cloned().unwrap_or(json!({})), route, mode)?;
                Ok(Value::Null)
            }
            "resume_session" => {
                self.engine.resume_existing(a.first().cloned().unwrap_or(Value::Null))?;
                Ok(Value::Null)
            }
            "end_session" => {
                self.engine.end();
                Ok(Value::Null)
            }
            "apply_quality" => self.engine.apply_quality(a.first().cloned().unwrap_or(json!({}))),
            "reconnect" => {
                self.engine.reconnect();
                Ok(Value::Null)
            }
            "list_files" => self.call(Method::GET, &format!("/profiles/{}/files", arg_i64(a, 0)), None),
            "uploads" => Ok(json!(self.uploads.views())),
            "start_uploads" => Ok(json!(self.uploads.queue(arg_i64(a, 0), &str_list(a.get(1)))?)),
            "pause_upload" => Ok(self.job(a, |u, id| u.pause(id))),
            "resume_upload" => Ok(self.job(a, |u, id| u.resume(id))),
            "cancel_upload" => Ok(self.job(a, |u, id| u.cancel(id))),
            "clear_finished_uploads" => {
                self.uploads.clear_finished();
                Ok(Value::Null)
            }
            "delete_file" => self.call(Method::DELETE, &format!("/files/{}", arg_i64(a, 0)), None),
            // Dialog file (memilih file, menyimpan unduhan) ditangani main.rs, lalu memanggil dua perintah internal ini.
            "_upload_preview" => Ok(self.uploads.preview(arg_i64(a, 0), &str_list(a.get(1)))?),
            "_download" => Ok(self.uploads.download(arg_i64(a, 0), arg_str(a, 1))?),
            "pick_files" | "download" => Err(ApiError::new("Fitur ini memerlukan dialog file dan hanya tersedia di jendela aplikasi.")),
            "request_addon" => self.call(Method::POST, &format!("/profiles/{}/addons", arg_i64(a, 0)),
                                         Some(json!({"gb": arg_i64(a, 1)}))),
            "submit_feedback" => self.call(Method::POST, &format!("/sessions/{}/feedback", arg_i64(a, 0)),
                                           a.get(1).cloned()),
            "load_prefs" => Ok(std::fs::read_to_string(self.state_dir.join("prefs.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}))),
            "save_prefs" => {
                let d = a.first().cloned().unwrap_or(Value::Null);
                let safe: Map<String, Value> =
                    PREF_KEYS.iter().filter_map(|k| d.get(*k).map(|v| ((*k).to_string(), v.clone()))).collect();
                std::fs::create_dir_all(&self.state_dir).map_err(|e| ApiError::new(e.to_string()))?;
                std::fs::write(self.state_dir.join("prefs.json"), Value::Object(safe).to_string())
                    .map_err(|e| ApiError::new(e.to_string()))?;
                Ok(Value::Null)
            }
            other => Err(ApiError::new(format!("Perintah tidak dikenal: {other}"))),
        }
    }

    /// Tes semburan UDP ke server: mencari bitrate tertinggi yang paket hilangnya masih kecil. Tangga turun dari `max_mbps`
    /// (bawaan 20) sampai bersih; tiap langkah 2 detik. Server lama / tanpa fitur ini (404) -> `{"available": false}`.
    fn net_burst(&self, max_mbps: Option<f64>) -> Result<Value, ApiError> {
        let t = match self.call(Method::POST, "/netprobe/ticket", None) {
            Ok(t) => t,
            Err(e) if e.status == Some(404) => return Ok(json!({"available": false})),
            Err(e) => return Err(e),
        };
        let host = reqwest::Url::parse(&self.cfg.api_url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let port = t["port"].as_u64().and_then(|p| u16::try_from(p).ok()).ok_or_else(|| ApiError::new("Server mengirim port tes yang tidak valid"))?;
        let ticket = t["ticket"].as_str().unwrap_or("").to_string();
        let top = max_mbps.unwrap_or(20.0).clamp(6.0, 30.0);
        let mut steps: Vec<Value> = vec![];
        // Turun sampai ±2 Mbps: bitrate stream bisa diatur serendah 1 Mbps untuk internet HP yang lambat.
        for factor in [1.0, 0.6, 0.4, 0.25, 0.1] {
            let mbps = (top * factor).round().max(1.0);
            if steps.iter().any(|s| s["kbps"].as_f64() == Some(mbps * 1000.0)) {
                continue;
            }
            let r = netcheck::udp_burst(&host, port, &ticket, (mbps * 1000.0) as u64, 2000);
            let clean = r["ok"] == true && r["loss_pct"].as_f64().unwrap_or(100.0) <= netcheck::BURST_CLEAN_LOSS_PCT;
            steps.push(r);
            if clean || steps.last().is_some_and(|s| s["ok"] == false) {
                break; // bersih = ketemu; server tidak menjawab = tidak ada gunanya mencoba lagi
            }
        }
        match netcheck::recommend(&steps) {
            Some((mbps, clean)) => Ok(json!({"available": true, "steps": steps, "recommended_mbps": mbps, "clean": clean})),
            None => Err(ApiError::new("Tes UDP tidak dijawab server. Mungkin jaringanmu memblokir UDP.")),
        }
    }

    /// Dengan `routes` (dari profile.routes): ukur tiap jalur sampai port Sunshine di PC, jadi angkanya sama dengan yang
    /// dirasakan saat stream. Tanpa itu: ukur ke server API.
    fn net_check(&self, routes: Option<&Value>) -> Value {
        let result = match routes.and_then(Value::as_array).filter(|r| !r.is_empty()) {
            Some(routes) => {
                let res = netcheck::visible(netcheck::probe_routes(routes, SUNSHINE_TCP_PORT, 6)); // Lokal yang tidak terdeteksi disembunyikan
                let auto = netcheck::best_route(&res);
                let best = res.iter().find(|r| r["id"].as_str() == auto.as_deref()).or(res.first()).cloned().unwrap_or(Value::Null);
                let mut out = Map::new();
                for k in ["rtt_ms", "jitter_ms", "loss_pct", "samples", "verdict", "advice"] {
                    out.insert(k.into(), best.get(k).cloned().unwrap_or(Value::Null));
                }
                out.insert("routes".into(), json!(res));
                out.insert("auto".into(), json!(auto));
                Value::Object(out)
            }
            None => {
                let (host, port) = reqwest::Url::parse(&self.cfg.api_url)
                    .ok()
                    .map(|u| (u.host_str().unwrap_or("").to_string(), u.port_or_known_default().unwrap_or(SUNSHINE_TCP_PORT)))
                    .unwrap_or_default();
                netcheck::probe_host(&host, port)
            }
        };
        self.engine.set_net(result.clone());
        result
    }
}
