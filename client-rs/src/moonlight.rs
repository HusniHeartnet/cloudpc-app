//! Menjalankan Moonlight / CloudPC Stream lewat command line. Port dari client/cloudpc_client/moonlight.py.
//!
//! Moonlight (moonlight-qt) mendukung, di Windows (Moonlight.exe) maupun macOS (Moonlight.app/Contents/MacOS/Moonlight):
//!   Moonlight pair <host> --pin <4 digit>
//!   Moonlight stream <host> <app> --resolution 1920x1080 --fps 60 --bitrate 15000 --video-codec HEVC ...
//! Template perintah bisa diubah di client_config.json tanpa mengubah kode, kalau versi Moonlight berbeda.
use std::fs::File;
use std::path::{Path, PathBuf};
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::config::ClientConfig;

fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

/// Client stream yang dibundel bersama aplikasi, urut prioritas: client CloudPC versi Rust (stream-rs, pengganti
/// Moonlight dengan argumen dan log yang sama), build bermerek (fork moonlight-qt), lalu Moonlight resmi.
fn bundled_clients() -> Vec<(&'static str, &'static str)> {
    if is_mac() {
        // Client Rust untuk Mac baru dibuild untuk Apple Silicon (arm64). Aplikasi ini universal: di Mac Intel (bagian x86_64)
        // client Rust dilewati dan CloudPC Stream (Moonlight bermerek, universal) yang dipakai.
        let mut clients = vec![("stream", "CloudPC Stream.app/Contents/MacOS/Moonlight"), ("moonlight", "Moonlight.app/Contents/MacOS/Moonlight")];
        if cfg!(target_arch = "aarch64") {
            // Sejak 20 Nov 2026 dibungkus sebagai aplikasi (supaya terdaftar di Window Capture TikTok LIVE); bentuk lama
            // (program lepas) tetap dikenali.
            clients.insert(0, ("stream-rs", "cloudpc-stream-client"));
            clients.insert(0, ("stream-rs", "CloudPC Stream Rust.app/Contents/MacOS/cloudpc-stream-client"));
        }
        clients
    } else {
        vec![("stream-rs", "cloudpc-stream-client.exe"), ("stream", "CloudPCStream.exe"), ("moonlight", "Moonlight.exe")]
    }
}

fn default_paths() -> Vec<PathBuf> {
    if is_mac() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        return vec![
            PathBuf::from("/Applications/Moonlight.app/Contents/MacOS/Moonlight"),
            home.join("Applications/Moonlight.app/Contents/MacOS/Moonlight"),
        ];
    }
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    vec![
        PathBuf::from(r"C:\Program Files\Moonlight Game Streaming\Moonlight.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Moonlight Game Streaming\Moonlight.exe"),
        PathBuf::from(local).join(r"Programs\Moonlight Game Streaming\Moonlight.exe"),
    ]
}

fn in_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|d| exts.iter().map(|e| d.join(format!("{name}{e}"))).find(|p| p.is_file()))
    })
}

/// Folder tempat file yang dibundel bersama aplikasi dicari (di samping exe; di macOS juga Contents/Resources).
fn bundle_dirs() -> Vec<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let mut dirs = vec![exe_dir.clone()];
    if is_mac() {
        dirs.push(exe_dir.parent().map(|p| p.join("Resources")).unwrap_or_default());
    }
    dirs
}

pub fn find_moonlight(cfg: &ClientConfig) -> Option<PathBuf> {
    // Android: bukan program, melainkan layar stream di dalam aplikasi (lihat android_stream.rs).
    #[cfg(target_os = "android")]
    if crate::android_stream::available() {
        return Some(PathBuf::from("android-stream"));
    }
    let mut candidates: Vec<PathBuf> = cfg.moonlight_path.iter().map(PathBuf::from).collect();
    for b in bundle_dirs() {
        for (folder, exe) in bundled_clients() {
            candidates.push(b.join(folder).join(exe));
        }
    }
    candidates.extend(default_paths());
    candidates.into_iter().find(|c| c.is_file()).or_else(|| in_path("moonlight").or_else(|| in_path("Moonlight")))
}

// ---- menjalankan ------------------------------------------------------------------------------------------------

pub const PAIR_ARGS: [&str; 4] = ["pair", "{host}", "--pin", "{pin}"];
// --capture-system-keys: bawaan Moonlight "never", jadi Alt+Tab/tombol Windows lari ke laptop pemain, bukan ke PC cloud.
// "fullscreen" = dikirim ke PC cloud saat layar penuh/borderless; mode jendela tetap bisa Alt+Tab ke laptop.
// --no-absolute-mouse: paksa mode mouse RELATIF (game). Tanpa ini yang berlaku setelan Moonlight tiap pemain; mode mutlak
// ("remote desktop") membuat kamera game 3D mentok di tepi layar (ditemukan 1 Nov 2026 di Deadlock). Pemain tetap bisa
// berpindah dengan Ctrl+Alt+Shift+M.
pub const STREAM_ARGS: [&str; 16] = [
    "stream", "{host}", "{app}", "--resolution", "{resolution}", "--fps", "{fps}", "--bitrate", "{bitrate_kbps}",
    "--video-codec", "{codec}", "--display-mode", "{display_mode}", "--capture-system-keys", "fullscreen", "--no-absolute-mouse",
];

/// Ganti `{nama}` pada template dengan nilainya (hanya nama yang dikenal; kurung lain dibiarkan).
pub fn build_args(template: &[String], vals: &[(&str, String)]) -> Vec<String> {
    template
        .iter()
        .map(|part| vals.iter().fold(part.clone(), |acc, (k, v)| acc.replace(&format!("{{{k}}}"), v)))
        .collect()
}

/// Proses Moonlight beserta log-nya. Output ke FILE, bukan pipe: Moonlight menulis log beberapa KB per menit, dan kalau
/// pipe tidak dibaca Moonlight membeku saat pipe penuh (terbukti: pairing macet sampai output dibaca).
pub struct Proc {
    runner: Runner,
    pub log_path: PathBuf,
    pub stats_reported: bool,
    /// Stdin client stream CloudPC versi Rust yang berjalan dengan `--persist` (jendela tetap sama saat ganti kualitas dan
    /// saat koneksi putus). `None` untuk Moonlight.
    pub control: Option<ChildStdin>,
}

/// Yang menjalankan pairing/stream: proses terpisah (desktop) atau layar stream di dalam aplikasi (Android, yang tidak
/// bisa menjalankan program berjendela sebagai proses anak).
enum Runner {
    Child(Child),
    #[cfg(target_os = "android")]
    Android(crate::android_stream::Job),
}

impl Proc {
    #[cfg(target_os = "android")]
    pub(crate) fn android(job: crate::android_stream::Job, log_path: PathBuf) -> Proc {
        Proc { runner: Runner::Android(job), log_path, stats_reported: false, control: None }
    }

    pub fn running(&mut self) -> bool {
        match &mut self.runner {
            Runner::Child(child) => matches!(child.try_wait(), Ok(None)),
            #[cfg(target_os = "android")]
            Runner::Android(job) => job.running(),
        }
    }

    pub fn exit_code(&mut self) -> Option<i32> {
        match &mut self.runner {
            Runner::Child(child) => child.try_wait().ok().flatten().map(|s| s.code().unwrap_or(-1)),
            #[cfg(target_os = "android")]
            Runner::Android(job) => job.exit_code(),
        }
    }

    /// Menunggu sampai `timeout`; true bila proses sudah selesai.
    pub fn wait_for(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if !self.running() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        !self.running()
    }

    /// Ganti kualitas stream yang sedang berjalan TANPA menutup jendela (hanya client Rust dengan `--persist`): jendela
    /// tetap sama, jadi Window Capture (TikTok LIVE, OBS) tidak kehilangan sumbernya. False = tidak didukung/gagal, pemanggil
    /// memakai cara lama (tutup lalu buka lagi).
    pub fn send_quality(&mut self, quality: &Value) -> bool {
        let Some(stdin) = self.control.as_mut() else { return false };
        let line = quality_line(quality);
        stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_ok()
    }

    pub fn kill(&mut self) {
        if !self.running() {
            return;
        }
        match &mut self.runner {
            Runner::Child(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            #[cfg(target_os = "android")]
            Runner::Android(job) => job.stop(),
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Baris perintah `quality` untuk client Rust (`persist.rs` di stream-rs).
pub fn quality_line(quality: &Value) -> String {
    let (resolution, fps, bitrate, codec) = stream_values(quality);
    format!("quality --resolution {resolution} --fps {fps} --bitrate {bitrate} --video-codec {codec}\n")
}

fn stream_values(quality: &Value) -> (String, String, String, String) {
    let get = |k: &str, d: &str| match &quality[k] {
        Value::String(s) => s.clone(),
        Value::Null => d.to_string(),
        other => other.to_string(),
    };
    let codec = match get("codec", "auto").as_str() {
        c @ ("auto" | "H.264" | "HEVC" | "AV1") => c.to_string(),
        _ => "auto".to_string(),
    };
    (get("resolution", "1920x1080"), get("fps", "60"), get("bitrate_kbps", "20000"), codec)
}

/// Client stream CloudPC versi Rust (bukan Moonlight): mendukung `--persist`.
fn is_rust_client(exe: &Path) -> bool {
    exe.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("cloudpc-stream-client"))
}

pub struct Moonlight {
    pub exe: PathBuf,
    pair_args: Vec<String>,
    stream_args: Vec<String>,
    app_name: String,
    log_dir: PathBuf,
}

impl Moonlight {
    pub fn new(exe: PathBuf, cfg: &ClientConfig, log_dir: PathBuf) -> Moonlight {
        let own = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Moonlight {
            exe,
            pair_args: cfg.pair_args.clone().unwrap_or_else(|| own(&PAIR_ARGS)),
            stream_args: cfg.stream_args.clone().unwrap_or_else(|| own(&STREAM_ARGS)),
            app_name: cfg.moonlight_app.clone(),
            log_dir,
        }
    }

    fn popen(&self, args: Vec<String>, name: &str) -> Result<Proc, String> {
        self.popen_with(args, name, false)
    }

    fn popen_with(&self, args: Vec<String>, name: &str, control: bool) -> Result<Proc, String> {
        std::fs::create_dir_all(&self.log_dir).map_err(|e| e.to_string())?;
        let log_path = self.log_dir.join(format!("moonlight-{name}.log"));
        let out = File::create(&log_path).map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&self.exe);
        cmd.args(args).stdout(Stdio::from(out)).stderr(Stdio::from(err));
        if control {
            cmd.stdin(Stdio::piped());
        }
        crate::no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("Moonlight tidak bisa dijalankan: {e}"))?;
        #[cfg(windows)]
        tie_to_app(&child);
        let control = child.stdin.take();
        Ok(Proc { runner: Runner::Child(child), log_path, stats_reported: false, control })
    }

    pub fn pair(&self, host: &str, pin: &str) -> Result<Proc, String> {
        #[cfg(target_os = "android")]
        return crate::android_stream::pair(host, pin, self.log_dir.join("moonlight-pair.log"));
        #[cfg(not(target_os = "android"))]
        self.popen(build_args(&self.pair_args, &[("host", host.into()), ("pin", pin.into())]), "pair")
    }

    pub fn stream(&self, host: &str, quality: &Value) -> Result<Proc, String> {
        let (resolution, fps, bitrate, codec) = stream_values(quality);
        #[cfg(target_os = "android")]
        return crate::android_stream::stream(host, &self.app_name, &resolution, &fps, &bitrate, &codec, self.log_dir.join("moonlight-stream.log"));
        #[cfg(not(target_os = "android"))]
        self.stream_process(host, quality, (resolution, fps, bitrate, codec))
    }

    #[cfg_attr(target_os = "android", allow(dead_code))]
    fn stream_process(&self, host: &str, quality: &Value, (resolution, fps, bitrate, codec): (String, String, String, String)) -> Result<Proc, String> {
        let display_mode = match &quality["display_mode"] {
            Value::String(s) => s.clone(),
            _ => "fullscreen".to_string(),
        };
        let vals = [
            ("host", host.to_string()), ("app", self.app_name.clone()), ("resolution", resolution),
            ("fps", fps), ("bitrate_kbps", bitrate), ("codec", codec), ("display_mode", display_mode),
        ];
        let mut args = build_args(&self.stream_args, &vals);
        // Client Rust: satu jendela untuk seluruh sesi (sambung ulang otomatis + ganti kualitas lewat stdin), supaya
        // Window Capture TikTok LIVE/OBS tidak kehilangan jendelanya (temuan tester Mac 20 Nov 2026).
        let persistent = is_rust_client(&self.exe);
        if persistent {
            args.push("--persist".into());
        }
        self.popen_with(args, "stream", persistent)
    }
}

/// Masukkan proses Moonlight ke Job Object milik aplikasi dengan KILL_ON_JOB_CLOSE: kalau aplikasi berhenti dengan cara apa
/// pun (ditutup, crash, dimatikan dari Task Manager), Windows ikut mematikan jendela stream. Tanpa ini jendela stream lama
/// tertinggal, dan saat aplikasi dibuka lagi lalu sesi disambung ulang muncul jendela stream KEDUA (bug 16 Nov 2026).
/// Gagal (mis. kebijakan job dari luar) tidak menggagalkan stream; penutupan normal tetap ditangani `SessionEngine::shutdown`.
#[cfg(windows)]
fn tie_to_app(child: &Child) {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimit {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimit {
        basic: BasicLimit,
        io: [u64; 6], // IO_COUNTERS
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *mut c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    }
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    // Handle job sengaja tidak pernah ditutup: baru tertutup (dan mematikan isinya) saat proses aplikasi berakhir.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut info = ExtendedLimit::default();
        info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(job, JOB_OBJECT_EXTENDED_LIMIT_INFORMATION, &mut info as *mut _ as *mut c_void,
                                         std::mem::size_of::<ExtendedLimit>() as u32);
        if ok == 0 { 0 } else { job as usize }
    });
    if job != 0 && unsafe { AssignProcessToJobObject(job as *mut c_void, child.as_raw_handle() as *mut c_void) } == 0 {
        eprintln!("proses Moonlight tidak bisa diikat ke aplikasi: {}", std::io::Error::last_os_error());
    }
}

/// Tutup jendela stream Moonlight seperti menekan tombol X, baru di-kill kalau tidak menurut.
///
/// Moonlight 6.2 hanya menulis ringkasan "Global video stats" ke log saat stream ditutup normal ("Quit event received");
/// TerminateProcess membuat statistik sesi hilang (terbukti di log uji 9 Okt 2026). Di macOS/Linux SIGTERM ditangani
/// Moonlight sebagai quit normal, jadi statistik tetap tertulis.
pub fn close_gracefully(proc: &mut Proc, timeout: Duration) {
    if !proc.running() {
        return;
    }
    let pid = match &mut proc.runner {
        Runner::Child(child) => child.id(),
        // Layar stream Android: minta ditutup (log statistik ditulis saat stream berhenti), tunggu, selesai.
        #[cfg(target_os = "android")]
        Runner::Android(job) => {
            job.stop();
            proc.wait_for(timeout);
            return;
        }
    };
    #[cfg(windows)]
    {
        if post_close(pid) && proc.wait_for(timeout) {
            return;
        }
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
        if proc.wait_for(timeout) {
            return;
        }
    }
    let _ = pid;
    proc.kill();
}

/// Kirim WM_CLOSE ke jendela SDL milik proses `pid`. True bila ada jendela yang dikirimi.
#[cfg(windows)]
fn post_close(pid: u32) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn GetClassNameW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, w: usize, l: isize) -> i32;
    }
    struct Search {
        pid: u32,
        found: Vec<isize>,
    }
    extern "system" fn cb(hwnd: isize, lparam: isize) -> i32 {
        // SAFETY: lparam adalah pointer ke Search milik post_close, yang hidup selama EnumWindows berjalan.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut owner = 0u32;
        let mut buf = [0u16; 64];
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut owner);
            if owner == search.pid {
                let n = GetClassNameW(hwnd, buf.as_mut_ptr(), 64) as usize;
                // Jendela stream = "SDL_app"; jendela Qt ditutup tanpa menulis statistik.
                if String::from_utf16_lossy(&buf[..n]) == "SDL_app" {
                    search.found.push(hwnd);
                }
            }
        }
        1
    }
    let mut s = Search { pid, found: vec![] };
    // SAFETY: lihat cb; PostMessageW hanya memakai handle yang baru ditemukan.
    unsafe {
        EnumWindows(cb, &mut s as *mut Search as isize);
        for h in &s.found {
            PostMessageW(*h, 0x0010, 0, 0); // WM_CLOSE
        }
    }
    !s.found.is_empty()
}

/// Akhir log Moonlight untuk pesan error.
/// Isi log Moonlight. Di macOS (terlihat di runner CI 13 Nov 2026) Moonlight tidak menulis log ke stdout tetapi hanya
/// mencetak "Redirecting log output to /tmp/Moonlight-<angka>.log"; log sebenarnya dibaca dari file itu.
pub fn read_log(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let redirected = s.lines().find_map(|l| l.trim().strip_prefix("Redirecting log output to ")).map(str::trim);
    match redirected.and_then(|r| std::fs::read_to_string(r).ok()) {
        Some(real) => Some(format!("{s}
{real}")),
        None => Some(s),
    }
}

pub fn log_tail(path: &Path, chars: usize) -> String {
    let s = read_log(path).unwrap_or_default();
    let s = s.trim();
    let skip = s.chars().count().saturating_sub(chars);
    s.chars().skip(skip).collect()
}

// ---- statistik stream dari log Moonlight --------------------------------------------------------------------------
// Format dari log Moonlight 6.2.0 sungguhan (client/tests/data). Saat stream ditutup, Moonlight menulis satu blok
// "Global video stats" per dekoder (dibuat ulang saat ganti layar penuh/jendela). FPS di desktop bisa jauh di bawah 60:
// Sunshine hanya mengirim frame saat layar berubah.

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

const AVG_KEYS: [&str; 11] = [
    "fps_received", "fps_decoded", "fps_rendered", "host_latency_avg_ms", "drop_network_pct", "drop_jitter_pct",
    "net_latency_ms", "net_latency_var_ms", "decode_ms", "queue_ms", "render_ms",
];

fn codec_name(fmt: u32) -> String {
    const TEN_BIT: u32 = 0x0200 | 0x0800 | 0x2000 | 0x8000;
    for (mask, name) in [(0x000Fu32, "H.264"), (0x0F00, "HEVC"), (0xF000, "AV1")] {
        if fmt & mask != 0 {
            return format!("{name}{}", if fmt & TEN_BIT != 0 { " 10-bit" } else { "" });
        }
    }
    format!("format {fmt:#x}")
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Ringkas log stream Moonlight. Selalu mengembalikan objek; `has_stats` false bila Moonlight tidak sempat menulis
/// statistik (di-kill, atau tidak ada frame yang diterima).
pub fn parse_stream_log(text: &str) -> Value {
    static TS: OnceLock<Regex> = OnceLock::new();
    static VIDEO: OnceLock<Regex> = OnceLock::new();
    static BITRATE: OnceLock<Regex> = OnceLock::new();
    static RENDERER: OnceLock<Regex> = OnceLock::new();
    static FIRST: OnceLock<Regex> = OnceLock::new();
    static VERSION: OnceLock<Regex> = OnceLock::new();
    static TERM: OnceLock<Regex> = OnceLock::new();
    static FPS_IN: OnceLock<Regex> = OnceLock::new();
    static FPS_DEC: OnceLock<Regex> = OnceLock::new();
    static FPS_REN: OnceLock<Regex> = OnceLock::new();
    static HOST_LAT: OnceLock<Regex> = OnceLock::new();
    static DROP_NET: OnceLock<Regex> = OnceLock::new();
    static DROP_JIT: OnceLock<Regex> = OnceLock::new();
    static NET_LAT: OnceLock<Regex> = OnceLock::new();
    static DECODE: OnceLock<Regex> = OnceLock::new();
    static QUEUE: OnceLock<Regex> = OnceLock::new();
    static RENDER: OnceLock<Regex> = OnceLock::new();
    let ts = re(&TS, r"^(\d+):(\d\d):(\d\d) - ");
    let mut info = Map::new();
    info.insert("has_stats".into(), json!(false));
    info.insert("segments".into(), json!(0));
    let mut rfi = 0u64;
    let mut blocks: Vec<Map<String, Value>> = vec![];
    let (mut last_t, mut seg_start, mut cur): (i64, Option<i64>, Option<Map<String, Value>>) = (0, None, None);
    let num = |m: &regex::Captures, i: usize| m[i].parse::<f64>().unwrap_or(0.0);

    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim_end();
        if let Some(m) = ts.captures(line) {
            last_t = m[1].parse::<i64>().unwrap_or(0) * 3600 + m[2].parse::<i64>().unwrap_or(0) * 60 + m[3].parse::<i64>().unwrap_or(0);
            if let Some(b) = cur.take() {
                blocks.push(b); // baris bertimestamp = blok statistik selesai
            }
            if let Some(mm) = re(&VIDEO, r"Video stream is (\d+x\d+)x(\d+) \(format (0x[0-9a-fA-F]+)\)").captures(line) {
                let fmt = u32::from_str_radix(&mm[3][2..], 16).unwrap_or(0);
                info.insert("resolution".into(), json!(&mm[1]));
                info.insert("fps_target".into(), json!(mm[2].parse::<i64>().unwrap_or(0)));
                info.insert("video_format".into(), json!(&mm[3]));
                info.insert("codec".into(), json!(codec_name(fmt)));
            }
            if let Some(mm) = re(&BITRATE, r"Video bitrate: (\d+) kbps").captures(line) {
                info.insert("bitrate_kbps".into(), json!(mm[1].parse::<i64>().unwrap_or(0)));
            }
            if let Some(mm) = re(&RENDERER, r"Renderer '([^']+)' chosen").captures(line) {
                info.insert("renderer".into(), json!(&mm[1]));
            }
            if let Some(mm) = re(&FIRST, r"Received first video packet after (\d+) ms").captures(line) {
                info.insert("first_frame_ms".into(), json!(mm[1].parse::<i64>().unwrap_or(0)));
                seg_start = Some(last_t);
            }
            if let Some(mm) = re(&VERSION, r#"Current Moonlight version: "([^"]+)""#).captures(line) {
                info.insert("moonlight_version".into(), json!(&mm[1]));
            }
            if let Some(mm) = re(&TERM, r"Connection terminated: (-?\d+)").captures(line) {
                info.insert("terminated_code".into(), json!(mm[1].parse::<i64>().unwrap_or(0)));
            }
            if line.contains("Sending RFI request") {
                rfi += 1; // frame rusak karena paket hilang, minta frame pemulihan
            }
            continue;
        }
        if line == "Global video stats" {
            let start = seg_start.unwrap_or(last_t);
            let mut b = Map::new();
            b.insert("t_end_s".into(), json!(last_t));
            b.insert("duration_s".into(), json!((last_t - start).max(0)));
            cur = Some(b);
            seg_start = Some(last_t);
            continue;
        }
        let Some(b) = cur.as_mut() else { continue };
        let mut put = |k: &str, v: f64| {
            b.insert(k.into(), json!(v));
        };
        if let Some(m) = re(&FPS_IN, r"^Incoming frame rate from network: ([\d.]+) FPS").captures(line) { put("fps_received", num(&m, 1)); }
        else if let Some(m) = re(&FPS_DEC, r"^Decoding frame rate: ([\d.]+) FPS").captures(line) { put("fps_decoded", num(&m, 1)); }
        else if let Some(m) = re(&FPS_REN, r"^Rendering frame rate: ([\d.]+) FPS").captures(line) { put("fps_rendered", num(&m, 1)); }
        else if let Some(m) = re(&HOST_LAT, r"^Host processing latency min/max/average: ([\d.]+)/([\d.]+)/([\d.]+) ms").captures(line) {
            put("host_latency_min_ms", num(&m, 1));
            put("host_latency_max_ms", num(&m, 2));
            put("host_latency_avg_ms", num(&m, 3));
        }
        else if let Some(m) = re(&DROP_NET, r"^Frames dropped by your network connection: ([\d.]+)%").captures(line) { put("drop_network_pct", num(&m, 1)); }
        else if let Some(m) = re(&DROP_JIT, r"^Frames dropped due to network jitter: ([\d.]+)%").captures(line) { put("drop_jitter_pct", num(&m, 1)); }
        else if let Some(m) = re(&NET_LAT, r"^Average network latency: ([\d.]+) ms \(variance: ([\d.]+) ms\)").captures(line) {
            put("net_latency_ms", num(&m, 1));
            put("net_latency_var_ms", num(&m, 2));
        }
        else if let Some(m) = re(&DECODE, r"^Average decoding time: ([\d.]+) ms").captures(line) { put("decode_ms", num(&m, 1)); }
        else if let Some(m) = re(&QUEUE, r"^Average frame queue delay: ([\d.]+) ms").captures(line) { put("queue_ms", num(&m, 1)); }
        else if let Some(m) = re(&RENDER, r"^Average rendering time \(including monitor V-sync latency\): ([\d.]+) ms").captures(line) { put("render_ms", num(&m, 1)); }
    }
    if let Some(b) = cur.take() {
        blocks.push(b);
    }
    info.insert("rfi_requests".into(), json!(rfi));
    blocks.retain(|b| b.len() > 2);
    if blocks.is_empty() {
        return Value::Object(info);
    }
    info.insert("has_stats".into(), json!(true));
    info.insert("segments".into(), json!(blocks.len()));
    let dur = |b: &Map<String, Value>| b["duration_s"].as_i64().unwrap_or(0);
    info.insert("stats_duration_s".into(), json!(blocks.iter().map(dur).sum::<i64>()));
    for key in AVG_KEYS {
        let pairs: Vec<(f64, f64)> = blocks.iter().filter_map(|b| b.get(key).and_then(Value::as_f64).map(|v| (v, dur(b).max(1) as f64))).collect();
        if !pairs.is_empty() {
            let total_w: f64 = pairs.iter().map(|(_, w)| w).sum();
            info.insert(key.into(), json!(round2(pairs.iter().map(|(v, w)| v * w).sum::<f64>() / total_w)));
        }
    }
    let col = |k: &str| blocks.iter().filter_map(|b| b.get(k).and_then(Value::as_f64)).collect::<Vec<_>>();
    let (mins, maxs) = (col("host_latency_min_ms"), col("host_latency_max_ms"));
    if !mins.is_empty() {
        info.insert("host_latency_min_ms".into(), json!(mins.iter().cloned().fold(f64::MAX, f64::min)));
        info.insert("host_latency_max_ms".into(), json!(maxs.iter().cloned().fold(f64::MIN, f64::max)));
    }
    let tail = blocks.len().saturating_sub(10);
    info.insert("blocks".into(), json!(blocks[tail..]));
    Value::Object(info)
}

pub fn parse_stream_log_file(path: &Path) -> Option<Value> {
    Some(parse_stream_log(&read_log(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_path_wins_when_it_exists() {
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("Moonlight.exe");
        std::fs::write(&exe, b"x").unwrap();
        let cfg = ClientConfig { moonlight_path: Some(exe.to_string_lossy().into_owned()), ..ClientConfig::default() };
        assert_eq!(find_moonlight(&cfg), Some(exe));
        let missing = ClientConfig { moonlight_path: Some("C:/tidak/ada.exe".into()), ..ClientConfig::default() };
        let _ = find_moonlight(&missing); // tidak panik walau jalur salah
    }

    #[test]
    fn follows_mac_log_redirect() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("Moonlight-123.log");
        std::fs::write(&real, "00:00:01 - SDL Info (0): isi log sebenarnya").unwrap();
        let captured = t.path().join("moonlight-stream.log");
        std::fs::write(&captured, format!("Redirecting log output to {}
", real.display())).unwrap();
        assert!(read_log(&captured).unwrap().contains("isi log sebenarnya"));
        assert!(log_tail(&captured, 200).contains("isi log sebenarnya"));
        std::fs::write(&captured, "tanpa pengalihan").unwrap();
        assert_eq!(read_log(&captured).unwrap(), "tanpa pengalihan");
    }

    #[test]
    fn build_args_replaces_only_known_placeholders() {
        let tpl: Vec<String> = ["stream", "{host}", "--note", "{ini bukan placeholder}", "{fps}"].iter().map(|s| s.to_string()).collect();
        let out = build_args(&tpl, &[("host", "10.0.0.1".into()), ("fps", "60".into())]);
        assert_eq!(out, ["stream", "10.0.0.1", "--note", "{ini bukan placeholder}", "60"]);
    }

    #[test]
    fn rust_client_gets_persist_and_quality_line() {
        assert!(is_rust_client(Path::new("C:/x/stream-rs/cloudpc-stream-client.exe")));
        assert!(is_rust_client(Path::new("/Applications/CloudPC Rust.app/Contents/Resources/stream-rs/cloudpc-stream-client")));
        assert!(!is_rust_client(Path::new("C:/x/stream/CloudPCStream.exe")));
        let q = serde_json::json!({"resolution": "1280x720", "fps": 120, "bitrate_kbps": 8000, "codec": "HEVC"});
        assert_eq!(quality_line(&q), "quality --resolution 1280x720 --fps 120 --bitrate 8000 --video-codec HEVC
");
        assert_eq!(quality_line(&serde_json::json!({"codec": "x"})), "quality --resolution 1920x1080 --fps 60 --bitrate 20000 --video-codec auto
");
    }

    #[test]
    fn stream_command_line_matches_python_defaults() {
        let t = tempfile::tempdir().unwrap();
        let m = Moonlight::new(PathBuf::from("x"), &ClientConfig::default(), t.path().to_path_buf());
        let q = json!({"resolution": "2560x1440", "fps": 120, "bitrate_kbps": 30000, "codec": "AV1", "display_mode": "borderless"});
        let want: Vec<String> = ["stream", "203.0.113.10", "Desktop", "--resolution", "2560x1440", "--fps", "120", "--bitrate", "30000",
            "--video-codec", "AV1", "--display-mode", "borderless", "--capture-system-keys", "fullscreen", "--no-absolute-mouse"].iter().map(|s| s.to_string()).collect();
        let vals = [("host", "203.0.113.10".to_string()), ("app", m.app_name.clone()), ("resolution", "2560x1440".into()), ("fps", "120".into()),
            ("bitrate_kbps", "30000".into()), ("codec", "AV1".into()), ("display_mode", "borderless".into())];
        assert_eq!(build_args(&m.stream_args, &vals), want);
        let _ = q;
    }

    #[test]
    fn parses_synthetic_log() {
        let log = "00:00:01 - SDL Info (0): Video stream is 1920x1080x60 (format 0x100)\n\
                   00:00:01 - SDL Info (0): Video bitrate: 20000 kbps\n\
                   00:00:02 - SDL Info (0): Received first video packet after 900 ms\n\
                   00:00:12 - SDL Info (0):\nGlobal video stats\n-------------------\n\
                   Incoming frame rate from network: 30.00 FPS\nAverage network latency: 4 ms (variance: 1 ms)\n\
                   00:00:13 - SDL Info (0): Sending RFI request\n";
        let v = parse_stream_log(log);
        assert_eq!(v["has_stats"], true);
        assert_eq!(v["codec"], "HEVC");
        assert_eq!(v["resolution"], "1920x1080");
        assert_eq!(v["rfi_requests"], 1);
        assert_eq!(v["fps_received"], 30.0);
        assert_eq!(parse_stream_log("")["has_stats"], false);
    }
}
