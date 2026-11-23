//! Mesin sesi: antre -> pesan PC -> pairing -> stream -> sambung ulang -> akhiri. Port dari ClientCore di
//! client/cloudpc_client/core.py (bagian sesi). Semua logika bisnis ada di server; ini hanya menjalankan alurnya.
//!
//! Satu sesi berjalan di satu thread latar. UI memantau lewat `get_state()` (dipanggil tiap detik) dan mengendalikan
//! lewat `end()` / `reconnect()` (hanya menyalakan bendera yang dibaca thread sesi).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{json, Map, Value};

use crate::config::ClientConfig;
use crate::http::{ApiError, Http};
use crate::moonlight::{self, Moonlight, Proc};
use crate::netcheck;

const SUNSHINE_TCP_PORT: u16 = 47984; // port HTTPS Sunshine yang dipakai Moonlight; dipakai untuk mengukur RTT
const SUNSHINE_PAIR_PORT: u16 = 47989; // port HTTP Sunshine untuk pairing
const RELAY_ROUTE: &str = "relay"; // jalur cadangan yang selalu ada (VPS + WireGuard), lihat server routes.py
const QUICK_FAIL_SECONDS: f64 = 20.0; // stream jalur langsung yang tertutup secepat ini dianggap jalurnya gagal

fn queue_lost(reason: &str) -> &'static str {
    match reason {
        "offer_timeout" => "Giliranmu terlewat karena tidak diambil dalam waktunya. Antre lagi untuk mendapat giliran.",
        "not_polling" => "Kamu keluar dari antrean karena aplikasi lama tidak terhubung. Antre lagi untuk mendapat giliran.",
        "quota" => "Kuota waktumu tidak cukup untuk memulai sesi, jadi kamu dikeluarkan dari antrean.",
        _ => "Kamu keluar dari antrean. Coba antre lagi.",
    }
}

/// Alasan sesi berhenti selain selesai normal.
enum Stop {
    UserEnded,
    Api(ApiError),
}

impl From<ApiError> for Stop {
    fn from(e: ApiError) -> Self {
        Stop::Api(e)
    }
}

type R<T> = Result<T, Stop>;

/// Lama (detik) jaringan pemain boleh putus berturut-turut tanpa menghentikan sesi. Server baru memutus sesi bila tidak ada
/// detak klien selama `CLIENT_HB_TIMEOUT` (180 dtk), jadi 120 dtk aman. Ditemukan di sesi #61 (Wi-Fi kafe): satu detak yang gagal
/// langsung mengakhiri sesi dengan `client_error` walau streamnya masih berjalan.
const NET_BLIP_TOLERANCE_SECONDS: u64 = 120;

/// Melacak sudah berapa lama galat jaringan terjadi berturut-turut.
#[derive(Default)]
struct BlipTracker {
    since: Mutex<Option<Instant>>,
}

impl BlipTracker {
    /// Hasil panggilan: galat jaringan (tanpa status HTTP) ditoleransi sampai `limit`; galat lain dan galat yang kelamaan diteruskan.
    fn filter<T>(&self, r: Result<T, ApiError>, limit: Duration) -> Result<Option<T>, ApiError> {
        let mut since = self.since.lock().unwrap();
        match r {
            Ok(v) => {
                *since = None;
                Ok(Some(v))
            }
            Err(e) if e.status.is_none() => {
                if since.get_or_insert_with(Instant::now).elapsed() < limit {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }
}

#[derive(Default, Clone)]
struct SessionState {
    phase: String,
    message: String,
    session: Option<Value>,
    error: Option<String>,
    reconnect_deadline: Option<Instant>,
    net: Option<Value>,
    steps: Vec<String>,
    queue: Option<Value>, // status antrean terakhir dari server (fase "queued")
}

impl SessionState {
    fn fresh(net: Option<Value>) -> Self {
        SessionState { phase: "idle".into(), net, ..Default::default() }
    }

    fn as_value(&self) -> Value {
        json!({
            "phase": self.phase, "message": self.message, "session": self.session, "error": self.error,
            "net": self.net, "steps": self.steps, "queue": self.queue,
            "reconnect_seconds_left": self.reconnect_deadline
                .map(|d| d.saturating_duration_since(Instant::now()).as_secs()),
        })
    }
}

pub struct SessionEngine {
    cfg: ClientConfig,
    http: Arc<Http>,
    log_dir: std::path::PathBuf,
    state: Mutex<SessionState>,
    end: AtomicBool,
    reconnect: AtomicBool,
    /// Kualitas baru dari pemain saat sesi berjalan; dipakai thread sesi pada pembukaan stream berikutnya.
    pending_quality: Mutex<Option<Value>>,
    /// Menyuruh `watch_stream` menutup jendela stream yang sedang berjalan supaya dibuka lagi dengan kualitas baru.
    restart_stream: AtomicBool,
    blips: BlipTracker,
    thread: Mutex<Option<JoinHandle<()>>>,
    proc: Mutex<Option<Proc>>,
    moonlight: Mutex<Option<Arc<Moonlight>>>,
}

fn is_finished(state: &str) -> bool {
    matches!(state, "ENDING" | "ENDED" | "FAILED")
}

impl SessionEngine {
    pub fn new(cfg: ClientConfig, http: Arc<Http>, log_dir: std::path::PathBuf) -> Arc<SessionEngine> {
        Arc::new(SessionEngine {
            cfg,
            http,
            log_dir,
            state: Mutex::new(SessionState::fresh(None)),
            end: AtomicBool::new(false),
            reconnect: AtomicBool::new(false),
            pending_quality: Mutex::new(None),
            restart_stream: AtomicBool::new(false),
            blips: BlipTracker::default(),
            thread: Mutex::new(None),
            proc: Mutex::new(None),
            moonlight: Mutex::new(None),
        })
    }

    // ---- status ----
    pub fn get_state(&self) -> Value {
        self.state.lock().unwrap().as_value()
    }

    pub fn set_net(&self, net: Value) {
        self.state.lock().unwrap().net = Some(net);
    }

    /// Dipanggil saat keluar akun: buang status sesi dan hasil cek koneksi milik akun sebelumnya.
    pub fn reset(&self) {
        *self.state.lock().unwrap() = SessionState::fresh(None);
    }

    fn set(&self, phase: Option<&str>, message: Option<&str>, f: impl FnOnce(&mut SessionState)) {
        let mut st = self.state.lock().unwrap();
        if let Some(p) = phase {
            st.phase = p.into();
            st.steps.push(p.into());
        }
        if let Some(m) = message {
            st.message = m.into();
        }
        f(&mut st);
    }

    fn set_session(&self, s: &Value) {
        self.state.lock().unwrap().session = Some(s.clone());
    }

    pub fn busy(&self) -> bool {
        self.thread.lock().unwrap().as_ref().is_some_and(|t| !t.is_finished())
    }

    fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, ApiError> {
        self.http.call(method, path, body.as_ref())
    }

    fn moonlight(&self) -> Result<Arc<Moonlight>, ApiError> {
        let mut slot = self.moonlight.lock().unwrap();
        if slot.is_none() {
            let exe = moonlight::find_moonlight(&self.cfg).ok_or_else(|| {
                ApiError::new("Moonlight belum terpasang. Pasang dari moonlight-stream.org lalu buka lagi aplikasi ini.")
            })?;
            *slot = Some(Arc::new(Moonlight::new(exe, &self.cfg, self.log_dir.clone())));
        }
        Ok(Arc::clone(slot.as_ref().unwrap()))
    }

    /// Tidur sampai `d` atau sampai pemain mengakhiri sesi; true bila diakhiri pemain.
    fn wait_end(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while Instant::now() < end {
            if self.end.load(Ordering::SeqCst) {
                return true;
            }
            thread::sleep(Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())));
        }
        self.end.load(Ordering::SeqCst)
    }

    fn poll(&self) -> Duration {
        Duration::from_secs_f64(self.cfg.poll_interval)
    }

    // ---- kendali dari UI ----
    pub fn end(&self) {
        self.end.store(true, Ordering::SeqCst);
    }

    pub fn reconnect(&self) {
        self.reconnect.store(true, Ordering::SeqCst);
    }

    /// Ganti kualitas stream (bitrate, FPS, resolusi, codec) tanpa mengakhiri sesi: jendela stream ditutup baik-baik lalu
    /// dibuka lagi dengan nilai baru (±5 detik). PC, disk, dan kuota tidak tersentuh. Bila layar sedang tertutup, nilai baru
    /// dipakai saat "Sambung lagi". Server lama tanpa endpoint kualitas (404/405) tetap dilayani, hanya tanpa pencatatan.
    pub fn apply_quality(&self, quality: Value) -> Result<Value, ApiError> {
        let (sid, phase) = {
            let st = self.state.lock().unwrap();
            (st.session.as_ref().and_then(|s| s["id"].as_i64()), st.phase.clone())
        };
        let sid = sid.filter(|_| self.busy() && matches!(phase.as_str(), "streaming" | "stream_closed" | "connecting"))
            .ok_or_else(|| ApiError::new("Kualitas hanya bisa diganti saat sesi sedang berjalan."))?;
        let applied = match self.call(Method::POST, &format!("/sessions/{sid}/quality"), Some(quality.clone())) {
            Ok(s) if s["quality"].is_object() => s["quality"].clone(),
            Ok(_) => quality,
            Err(e) if matches!(e.status, Some(404 | 405)) => quality,
            Err(e) => return Err(e),
        };
        *self.pending_quality.lock().unwrap() = Some(applied.clone());
        if matches!(phase.as_str(), "streaming" | "connecting") {
            self.restart_stream.store(true, Ordering::SeqCst); // layar tertutup/sedang dibuka: nilai dipakai pada pembukaan berikutnya
        }
        Ok(applied)
    }

    /// `route`: "auto" (jalur terbaik dari cek koneksi terakhir) atau id jalur pilihan pemain.
    /// `mode`: "profile" (disk profile persisten) atau "ephemeral" (sesi bersih, disk dihapus di akhir sesi).
    pub fn start(self: &Arc<Self>, profile_id: i64, quality: Value, client_info: Value, route: &str, mode: &str) -> Result<(), ApiError> {
        if self.busy() {
            return Err(ApiError::new("Sesi sedang berjalan"));
        }
        self.moonlight()?; // pastikan Moonlight ada sebelum memesan GPU
        self.end.store(false, Ordering::SeqCst);
        self.reconnect.store(false, Ordering::SeqCst);
        let net = {
            let mut st = self.state.lock().unwrap();
            let net = st.net.clone();
            *st = SessionState::fresh(net.clone());
            // Fase diisi DI SINI, bukan baru di thread: UI langsung menanyakan status setelah perintah ini kembali, dan
            // "idle" (walau sepersekian detik) dianggapnya sesi selesai -> layar sesi membeku di "Keluar dari antrean"
            // dengan tombol Tutup saja, padahal sesi berjalan (laporan pemilik 22 Nov 2026 di HP).
            st.phase = "starting".into();
            st.message = "Memesan PC...".into();
            net.unwrap_or(Value::Null)
        };
        let mut info = client_info.as_object().cloned().unwrap_or_default();
        let chosen: Option<String> = if route == "auto" { net["auto"].as_str().map(str::to_string) } else { Some(route.to_string()) };
        let measured: Vec<&Value> = net["routes"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
        let metrics = measured.iter().find(|r| r["id"].as_str() == chosen.as_deref()).copied().unwrap_or(&net);
        for k in ["rtt_ms", "jitter_ms", "loss_pct"] {
            if let Some(v) = metrics.get(k) {
                info.insert(format!("pre_{k}"), v.clone());
            }
        }
        if !measured.is_empty() {
            info.insert("route_mode".into(), json!(if route == "auto" { "otomatis" } else { "pilihan" }));
            let rtts: Vec<String> = measured
                .iter()
                .map(|r| format!("{} {} ms", r["label"].as_str().unwrap_or("?"), r["rtt_ms"].as_f64().map(|v| v.to_string()).unwrap_or("-".into())))
                .collect();
            info.insert("route_rtts".into(), json!(rtts.join(", ")));
        }
        let me = Arc::clone(self);
        let mode = mode.to_string();
        let handle = thread::spawn(move || me.run(profile_id, quality, Value::Object(info), chosen, &mode));
        *self.thread.lock().unwrap() = Some(handle);
        Ok(())
    }

    /// Dipanggil saat aplikasi dibuka ulang dan masih ada sesi berjalan di server.
    pub fn resume_existing(self: &Arc<Self>, session: Value) -> Result<(), ApiError> {
        if self.busy() {
            return Ok(());
        }
        self.end.store(false, Ordering::SeqCst);
        self.reconnect.store(false, Ordering::SeqCst);
        {
            let mut st = self.state.lock().unwrap();
            let net = st.net.clone();
            *st = SessionState::fresh(net);
            st.session = Some(session.clone());
            // Jangan tinggalkan fase "idle" walau sesaat: UI menganggap "idle" = selesai, berhenti memantau, dan hanya
            // menampilkan "Keluar dari antrean" + Tutup, padahal sesi masih berjalan (laporan pemilik 22 Nov 2026 di HP).
            st.phase = "connecting".into();
            st.message = "Menyambung lagi ke sesi yang masih berjalan...".into();
        }
        let me = Arc::clone(self);
        *self.thread.lock().unwrap() = Some(thread::spawn(move || me.run_existing(session)));
        Ok(())
    }

    // ---- alur ----
    fn get_session(&self, sid: i64) -> Result<Value, ApiError> {
        let s = self.call(Method::GET, &format!("/sessions/{sid}"), None)?;
        self.set_session(&s);
        Ok(s)
    }

    fn wait_state(&self, sid: i64, wanted: &[&str], timeout: Duration) -> R<Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let s = self.get_session(sid)?;
            let st = s["state"].as_str().unwrap_or("");
            if wanted.contains(&st) {
                return Ok(s);
            }
            if is_finished(st) {
                return Err(ApiError::new(s["error"].as_str().unwrap_or("Sesi dihentikan server")).into());
            }
            if self.wait_end(self.poll()) {
                return Err(Stop::UserEnded);
            }
        }
        Err(ApiError::new("Server terlalu lama merespons").into())
    }

    /// Antre sampai giliran ditawarkan server. Server hanya mengeluarkan pemain yang berhenti menanyakan status, jadi
    /// loop ini juga yang menjaga posisi di antrean. Server lama tanpa antrean (404): langsung lanjut.
    fn wait_turn(&self, profile_id: i64, mode: &str) -> R<()> {
        let mut q = match self.call(Method::POST, "/queue/join", Some(json!({"profile_id": profile_id, "mode": mode}))) {
            Ok(q) => q,
            Err(e) if e.status == Some(404) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let result = (|| -> R<()> {
            self.set(Some("queued"), None, |_| {});
            while q["status"] != "OFFERED" {
                if q["in_queue"] != true {
                    let reason = q["last_result"]["reason"].as_str().unwrap_or("");
                    return Err(ApiError::new(queue_lost(reason)).into());
                }
                let snapshot = q.clone();
                self.set(None, Some("Menunggu giliran..."), |st| st.queue = Some(snapshot));
                if self.wait_end(self.poll()) {
                    return Err(Stop::UserEnded);
                }
                q = self.call(Method::GET, "/queue/status", None)?;
            }
            let snapshot = q.clone();
            self.set(None, Some("Giliranmu tiba! Memesan PC..."), |st| st.queue = Some(snapshot));
            Ok(())
        })();
        if result.is_err() {
            self.leave_queue();
        }
        result
    }

    /// Lepas entri antrean (dan tawaran giliran yang mencadangkan PC); kegagalan jaringan tidak penting, server tetap
    /// mengeluarkan entri yang tidak dipantau.
    fn leave_queue(&self) {
        let _ = self.call(Method::POST, "/queue/leave", None);
    }

    fn run(&self, profile_id: i64, quality: Value, info: Value, route: Option<String>, mode: &str) {
        let mut sid: Option<i64> = None;
        let result = (|| -> R<()> {
            self.set(Some("starting"), Some("Memesan PC..."), |_| {}); // sebelum antre: fase "idle" dianggap UI sebagai selesai
            self.wait_turn(profile_id, mode)?;
            self.set(Some("starting"), Some("Memesan PC..."), |_| {});
            let body = json!({"profile_id": profile_id, "mode": mode, "quality": quality, "client_info": info, "route": route});
            let s = match self.call(Method::POST, "/sessions", Some(body)) {
                Ok(s) => s,
                Err(e) => {
                    self.leave_queue(); // tawaran giliran jangan menahan PC bila sesi gagal dibuat
                    return Err(e.into());
                }
            };
            let id = s["id"].as_i64().ok_or_else(|| ApiError::new("Server tidak mengirim id sesi"))?;
            sid = Some(id);
            let msg = if mode == "ephemeral" { "Menyiapkan PC bersih untukmu..." } else { "Menyiapkan PC dan memasang penyimpananmu..." };
            let first = s.clone();
            self.set(Some("preparing"), Some(msg), |st| st.session = Some(first));
            let s = self.wait_state(id, &["AWAITING_PAIR"], Duration::from_secs(240))?;
            let s = self.ensure_reachable(s)?;
            self.pair(&s)?;
            self.stream_loop(id, &quality)
        })();
        self.settle(result, sid);
    }

    fn run_existing(&self, s: Value) {
        let sid = s["id"].as_i64().unwrap_or(0);
        let result = (|| -> R<()> {
            let quality = s.get("quality").cloned().filter(|q| q.is_object()).unwrap_or_else(|| json!({}));
            match s["state"].as_str().unwrap_or("") {
                "AWAITING_PAIR" => self.pair(&s)?,
                // Status setengah jalan tidak bisa dilanjutkan dengan aman; akhiri dan mulai baru.
                "PREPARING" | "PAIRING" | "READY" => {
                    return Err(ApiError::new("Sesi sebelumnya terputus saat disiapkan. Silakan mulai ulang.").into())
                }
                _ => {}
            }
            self.stream_loop(sid, &quality)
        })();
        self.settle(result, Some(sid));
    }

    fn settle(&self, result: R<()>, sid: Option<i64>) {
        match result {
            Ok(()) | Err(Stop::UserEnded) => {}
            Err(Stop::Api(e)) => self.set(None, None, |st| st.error = Some(e.message)),
        }
        self.finish(sid);
    }

    fn switch_to_relay(&self, sid: i64, why: &str) -> Result<Value, ApiError> {
        let s = self.call(Method::POST, &format!("/sessions/{sid}/route"), Some(json!({"route": RELAY_ROUTE})))?;
        eprintln!("jalur {why} gagal ({}), pindah ke relay", s["route"]);
        let shown = s.clone();
        self.set(None, Some("Jalur langsung tidak bisa dipakai, pindah ke server relay..."), |st| st.session = Some(shown));
        Ok(s)
    }

    /// Jalur langsung bisa saja tertutup (router/ISP); cek port pairing sebelum pairing, kalau tidak terjangkau pindah ke
    /// relay supaya pairing tidak gagal dan sesi tidak hangus.
    fn ensure_reachable(&self, s: Value) -> R<Value> {
        let route = s["route"].as_str().unwrap_or("");
        if route.is_empty() || route == RELAY_ROUTE {
            return Ok(s);
        }
        let host = s["host_address"].as_str().unwrap_or("");
        let probe = netcheck::tcp_probe(host, SUNSHINE_PAIR_PORT, 2, Duration::from_secs(2), Duration::from_millis(150));
        if probe["rtt_ms"].is_null() {
            let id = s["id"].as_i64().unwrap_or(0);
            return Ok(self.switch_to_relay(id, "pairing")?);
        }
        Ok(s)
    }

    fn pair(&self, s: &Value) -> R<()> {
        let sid = s["id"].as_i64().unwrap_or(0);
        let (host, pin) = (s["host_address"].as_str().unwrap_or(""), s["pin"].as_str().unwrap_or(""));
        self.set(Some("pairing"), Some("Menghubungkan perangkatmu dengan PC (pairing)..."), |_| {});
        let mut proc = self.moonlight()?.pair(host, pin).map_err(ApiError::new)?; // dimatikan otomatis saat keluar (Drop)
        self.call(Method::POST, &format!("/sessions/{sid}/pair-started"), None)?;
        // READY = agent sudah memastikan Sunshine menerima PIN, jadi pairing selesai di sisi PC.
        self.wait_state(sid, &["READY"], Duration::from_secs(150))?;
        // Moonlight 6.2 `pair` tidak keluar sendiri setelah sukses (tetap menampilkan jendela). Beri waktu sebentar untuk
        // menyimpan sertifikat, lalu tutup prosesnya.
        if proc.wait_for(Duration::from_secs(10)) {
            if let Some(code) = proc.exit_code().filter(|c| *c != 0) {
                let _ = code;
                let tail = moonlight::log_tail(&proc.log_path, 200);
                return Err(ApiError::new(format!("Pairing Moonlight gagal: {tail}")).into());
            }
        }
        Ok(())
    }

    fn stream_loop(&self, sid: i64, quality: &Value) -> R<()> {
        let s = self.get_session(sid)?;
        let mut host = s["host_address"].as_str().unwrap_or("").to_string();
        let (mut first, mut fell_back) = (true, false);
        let mut quality = quality.clone();
        *self.pending_quality.lock().unwrap() = None;
        self.restart_stream.store(false, Ordering::SeqCst);
        loop {
            let mut msg = "Membuka layar PC...";
            if let Some(q) = self.pending_quality.lock().unwrap().take() {
                quality = q;
                msg = "Menerapkan kualitas baru...";
            }
            self.set(Some("connecting"), Some(msg), |_| {});
            let opened = Instant::now();
            *self.proc.lock().unwrap() = Some(self.moonlight()?.stream(&host, &quality).map_err(ApiError::new)?);
            if first {
                let s = self.call(Method::POST, &format!("/sessions/{sid}/stream-started"), None)?;
                self.set_session(&s);
                first = false;
            }
            self.set(Some("streaming"), Some("Tersambung"), |st| st.reconnect_deadline = None);
            let ended_by_server = self.watch_stream(sid, &host)?;
            self.report_stream_stats(sid);
            if ended_by_server || self.end.load(Ordering::SeqCst) {
                return Ok(());
            }
            if self.restart_stream.swap(false, Ordering::SeqCst) {
                continue; // pemain mengganti kualitas: buka stream lagi segera, tanpa masa tenggang "layar tertutup"
            }
            let route =self.state.lock().unwrap().session.as_ref().and_then(|s| s["route"].as_str().map(str::to_string));
            // Android: layar stream melaporkan kenapa ia tertutup. Ditutup pemain sendiri (Perkecil, tombol kembali, pindah
            // aplikasi = kode keluar 0) bukan tanda jalurnya gagal, jadi jangan dibuka lagi lewat relay. Di desktop kode
            // keluar Moonlight tidak bisa diandalkan, jadi aturan lama (tertutup cepat = jalur gagal) tetap berlaku.
            let closed_by_player = cfg!(target_os = "android") && self.proc.lock().unwrap().as_mut().is_some_and(|p| p.exit_code() == Some(0));
            if !fell_back && !closed_by_player && route.as_deref().is_some_and(|r| r != RELAY_ROUTE) && opened.elapsed().as_secs_f64() < QUICK_FAIL_SECONDS {
                // Pairing lewat TCP bisa lolos tapi video (UDP) diblokir di jalur langsung: coba sekali lewat relay.
                fell_back = true;
                host = self.switch_to_relay(sid, "stream")?["host_address"].as_str().unwrap_or("").to_string();
                continue;
            }
            // Moonlight ditutup (sengaja atau karena jaringan). Beri waktu untuk sambung lagi.
            let deadline = Instant::now() + Duration::from_secs(self.cfg.reconnect_grace_seconds);
            self.set(Some("stream_closed"), Some("Layar PC tertutup. Sambung lagi atau akhiri sesi."), |st| st.reconnect_deadline = Some(deadline));
            self.reconnect.store(false, Ordering::SeqCst);
            let mut last_hb: Option<Instant> = None;
            let mut reconnecting = false;
            while Instant::now() < deadline {
                if self.end.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if self.reconnect.load(Ordering::SeqCst) {
                    reconnecting = true;
                    break;
                }
                if last_hb.map_or(true, |t| t.elapsed().as_secs_f64() > self.cfg.heartbeat_interval) {
                    let hb = self.blips.filter(self.heartbeat(sid, None), Duration::from_secs(NET_BLIP_TOLERANCE_SECONDS))?;
                    last_hb = Some(Instant::now());
                    if hb.is_some_and(|s| s["state"] != "ACTIVE") {
                        return Ok(());
                    }
                }
                thread::sleep(Duration::from_millis(500));
            }
            if !reconnecting {
                return Ok(()); // waktu habis, sesi diakhiri
            }
        }
    }

    fn heartbeat(&self, sid: i64, metrics: Option<Value>) -> Result<Value, ApiError> {
        let s = self.call(Method::POST, &format!("/sessions/{sid}/heartbeat"), Some(metrics.unwrap_or_else(|| json!({}))))?;
        self.set_session(&s);
        Ok(s)
    }

    fn proc_running(&self) -> bool {
        self.proc.lock().unwrap().as_mut().is_some_and(|p| p.running())
    }

    /// Pantau Moonlight. Kembalikan true bila server yang mengakhiri sesi (kuota habis, admin).
    fn watch_stream(&self, sid: i64, host: &str) -> R<bool> {
        let (mut last_hb, mut last_poll): (Option<Instant>, Instant) = (None, Instant::now());
        while self.proc_running() {
            if self.end.load(Ordering::SeqCst) {
                self.kill_stream();
                return Ok(false);
            }
            if self.restart_stream.load(Ordering::SeqCst) {
                // Client Rust: kualitas baru diterapkan di jendela yang sama (Window Capture tidak hilang).
                if self.apply_quality_live() {
                    continue;
                }
                self.kill_stream(); // tutup baik-baik (statistik tertulis), stream_loop membukanya lagi
                return Ok(false);
            }
            if last_hb.map_or(true, |t| t.elapsed().as_secs_f64() > self.cfg.heartbeat_interval) {
                let m = netcheck::tcp_probe(host, SUNSHINE_TCP_PORT, 5, Duration::from_millis(1500), Duration::from_millis(100));
                let metrics: Map<String, Value> = ["rtt_ms", "jitter_ms", "loss_pct"].iter().map(|k| ((*k).to_string(), m[*k].clone())).collect();
                // Jaringan putus sebentar (Wi-Fi kafe) tidak boleh mengakhiri sesi: lihat NET_BLIP_TOLERANCE_SECONDS.
                let hb = self.blips.filter(self.heartbeat(sid, Some(Value::Object(metrics))), Duration::from_secs(NET_BLIP_TOLERANCE_SECONDS))?;
                last_hb = Some(Instant::now());
                last_poll = Instant::now();
                if hb.is_some_and(|s| s["state"] != "ACTIVE") {
                    self.kill_stream();
                    return Ok(true);
                }
            } else if last_poll.elapsed() > Duration::from_secs(10) {
                let s = self.blips.filter(self.get_session(sid), Duration::from_secs(NET_BLIP_TOLERANCE_SECONDS))?;
                last_poll = Instant::now();
                if s.is_some_and(|s| s["state"] != "ACTIVE") {
                    self.kill_stream();
                    return Ok(true);
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
        Ok(false)
    }

    /// Kirim kualitas baru ke client stream yang sedang berjalan tanpa menutup jendelanya. False = client tidak mendukung
    /// (Moonlight) atau gagal; kualitas tetap menunggu di `pending_quality` untuk cara lama.
    fn apply_quality_live(&self) -> bool {
        let Some(quality) = self.pending_quality.lock().unwrap().clone() else { return false };
        let sent = self.proc.lock().unwrap().as_mut().is_some_and(|p| p.running() && p.send_quality(&quality));
        if sent {
            self.pending_quality.lock().unwrap().take();
            self.restart_stream.store(false, Ordering::SeqCst);
            self.set(Some("streaming"), Some("Kualitas baru diterapkan"), |_| {});
        }
        sent
    }

    /// Dipanggil saat aplikasi keluar: tutup jendela stream supaya tidak tertinggal (lalu terbuka dobel saat sambung ulang).
    /// Sesi di server tidak diakhiri di sini (itu tugas "Keluar" di tray); server mengakhirinya bila detak berhenti.
    pub fn shutdown(&self) {
        self.kill_stream();
    }

    fn kill_stream(&self) {
        if let Some(p) = self.proc.lock().unwrap().as_mut() {
            moonlight::close_gracefully(p, Duration::from_secs(6)); // supaya Moonlight sempat menulis statistik ke log
        }
    }

    /// Kirim ringkasan statistik Moonlight dari log stream yang baru selesai (sekali per proses). Harus dipanggil sebelum
    /// Moonlight dijalankan lagi, karena log ditimpa setiap kali stream dibuka.
    fn report_stream_stats(&self, sid: i64) {
        let (path, code) = {
            let mut guard = self.proc.lock().unwrap();
            let Some(p) = guard.as_mut() else { return };
            if p.running() || p.stats_reported {
                return;
            }
            p.stats_reported = true;
            (p.log_path.clone(), p.exit_code())
        };
        let Some(mut stats) = moonlight::parse_stream_log_file(&path) else { return };
        if let Some(o) = stats.as_object_mut() {
            o.insert("exit_code".into(), json!(code));
        }
        if let Err(e) = self.call(Method::POST, &format!("/sessions/{sid}/stream-stats"), Some(stats)) {
            eprintln!("gagal mengirim statistik stream: {}", e.message);
        }
    }

    fn finish(&self, sid: Option<i64>) {
        self.kill_stream();
        if let Some(id) = sid {
            self.report_stream_stats(id);
        }
        let Some(id) = sid else {
            let failed = self.state.lock().unwrap().error.is_some();
            self.set(Some(if failed { "error" } else { "idle" }), None, |_| {});
            return;
        };
        self.set(Some("ending"), Some("Menyimpan data dan menutup sesi..."), |_| {});
        let outcome = (|| -> Result<(), ApiError> {
            let s = self.get_session(id)?;
            if !is_finished(s["state"].as_str().unwrap_or("")) {
                let err = self.state.lock().unwrap().error.clone();
                let reason = if err.is_some() { "client_error" } else { "user" };
                self.call(Method::POST, &format!("/sessions/{id}/end"), Some(json!({"reason": reason, "error": err})))?;
            }
            let deadline = Instant::now() + Duration::from_secs(300);
            while Instant::now() < deadline {
                let s = self.get_session(id)?;
                if matches!(s["state"].as_str(), Some("ENDED" | "FAILED")) {
                    break;
                }
                thread::sleep(self.poll());
            }
            Ok(())
        })();
        if let Err(e) = outcome {
            self.set(None, None, |st| {
                if st.error.is_none() {
                    st.error = Some(e.message);
                }
            });
        }
        let reason = self.state.lock().unwrap().session.as_ref().and_then(|s| s["end_reason"].as_str().map(str::to_string)).unwrap_or_default();
        let msg = match reason.as_str() {
            "quota_exhausted" => "Kuota jam hari ini habis. Sesi diakhiri dan datamu sudah tersimpan.",
            "session_limit" => "Batas durasi satu sesi tercapai. Datamu tersimpan; kamu bisa langsung mulai sesi baru.",
            "idle" => "Sesi diakhiri karena PC tidak dipakai beberapa saat. Datamu tersimpan dan sisa kuotamu aman.",
            "client_lost" => "Koneksi terputus terlalu lama. Sesi diakhiri, datamu aman.",
            "admin" => "Sesi diakhiri oleh admin.",
            "pair_timeout" => "Gagal menyambungkan perangkatmu ke PC. Coba mulai sesi lagi; kalau tetap gagal, hubungi admin.",
            _ => "Sesi selesai. Datamu tersimpan.",
        };
        self.set(Some("ended"), Some(msg), |_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_blips_are_tolerated_until_the_limit_and_reset_on_success() {
        let t = BlipTracker::default();
        let net = || Err::<i32, _>(ApiError::new("Tidak bisa terhubung ke server"));
        let long = Duration::from_secs(60);
        assert_eq!(t.filter(net(), long).unwrap(), None); // galat jaringan pertama: ditoleransi
        assert_eq!(t.filter(net(), long).unwrap(), None);
        assert_eq!(t.filter(Ok(7), long).unwrap(), Some(7)); // berhasil: hitungan diulang
        assert!(t.since.lock().unwrap().is_none());
        assert_eq!(t.filter(net(), long).unwrap(), None);
        assert!(t.filter(net(), Duration::ZERO).is_err()); // sudah melewati batas: diteruskan
        let http = Err::<i32, _>(ApiError { message: "Sesi tidak ada".into(), status: Some(404) });
        assert!(t.filter(http, long).is_err()); // galat dari server tidak pernah ditoleransi
    }
}
