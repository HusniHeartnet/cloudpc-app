//! Unggah/unduh file ke profile. Port dari bagian file di client/cloudpc_client/core.py.
//!
//! Antrean unggahan berjalan di thread sendiri (satu file per waktu), bisa dijeda/dilanjutkan/dibatalkan, bertahap
//! (potongan 8 MB, posisi dilanjutkan dari server), diverifikasi SHA-256, dicoba ulang otomatis saat jaringan putus, dan
//! tercatat di disk (`uploads.json`) supaya lanjut otomatis setelah aplikasi dibuka ulang.
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::Method;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::http::{detail, ApiError, Http};

const HASH_BLOCK: usize = 4 * 1024 * 1024;
const UPLOAD_CHUNK: usize = 8 * 1024 * 1024; // cukup kecil supaya jeda cepat terasa, cukup besar supaya efisien
const ACTIVE: [&str; 4] = ["queued", "hashing", "uploading", "retrying"];

#[derive(Clone, Debug)]
pub struct UploadJob {
    id: String,
    profile_id: i64,
    path: String,
    name: String,
    size: u64,
    status: String, // queued | hashing | uploading | retrying | paused | error | verified | done
    sha256: Option<String>,
    file_id: Option<i64>,
    sent: u64,
    hashed: u64,
    speed_bps: f64,
    eta_s: Option<f64>,
    error: Option<String>,
    result: Option<Value>,
}

impl UploadJob {
    fn view(&self) -> Value {
        json!({"id": self.id, "name": self.name, "size": self.size, "status": self.status, "sent": self.sent,
               "hashed": self.hashed, "speed_bps": self.speed_bps, "eta_s": self.eta_s, "error": self.error,
               "file_id": self.file_id})
    }

    fn persist(&self) -> Value {
        json!({"id": self.id, "profile_id": self.profile_id, "path": self.path, "name": self.name, "size": self.size,
               "status": self.status, "sha256": self.sha256, "file_id": self.file_id, "sent": self.sent})
    }
}

/// Kecepatan rata-rata bergerak (supaya angka di layar tidak loncat-loncat) dan perkiraan sisa waktu.
struct Speed {
    t: Instant,
    b: u64,
    rate: f64,
}

impl Speed {
    fn new(start: u64) -> Self {
        Speed { t: Instant::now(), b: start, rate: 0.0 }
    }

    fn update(&mut self, sent: u64, total: u64) -> (f64, Option<f64>) {
        let dt = self.t.elapsed().as_secs_f64();
        if dt > 0.0 {
            let inst = sent.saturating_sub(self.b) as f64 / dt;
            self.rate = if self.rate == 0.0 { inst } else { 0.7 * self.rate + 0.3 * inst };
        }
        self.t = Instant::now();
        self.b = sent;
        (self.rate, (self.rate > 0.0).then(|| total.saturating_sub(sent) as f64 / self.rate))
    }
}

/// Gangguan jaringan/server sementara (dicoba ulang otomatis) atau penolakan yang tidak akan berubah bila diulang.
enum UErr {
    Net(String),
    Api(String),
}

pub struct Uploads {
    http: Arc<Http>,
    jobs: Mutex<Vec<UploadJob>>,
    file: PathBuf,
    user_id: Mutex<Option<i64>>,
    wake: (Mutex<bool>, Condvar),
    worker: Mutex<Option<JoinHandle<()>>>,
    autostart: bool,
    counter: AtomicU64,
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut h = Sha256::new();
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; HASH_BLOCK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl Uploads {
    pub fn new(http: Arc<Http>, state_dir: &Path) -> Arc<Uploads> {
        Self::build(http, state_dir, true)
    }

    fn build(http: Arc<Http>, state_dir: &Path, autostart: bool) -> Arc<Uploads> {
        Arc::new(Uploads {
            http,
            jobs: Mutex::new(vec![]),
            file: state_dir.join("uploads.json"),
            user_id: Mutex::new(None),
            wake: (Mutex::new(false), Condvar::new()),
            worker: Mutex::new(None),
            autostart,
            counter: AtomicU64::new(0),
        })
    }

    // ---- akun ----
    /// Setelah login: unggahan yang belum selesai milik akun ini dilanjutkan.
    pub fn set_user(self: &Arc<Self>, user_id: i64) {
        *self.user_id.lock().unwrap() = Some(user_id);
        self.load_jobs();
    }

    /// Saat keluar: simpan, lalu buang antrean (unggahan yang berjalan berhenti di potongan berikutnya karena token hilang).
    pub fn clear_user(&self) {
        self.save_jobs();
        *self.user_id.lock().unwrap() = None;
        self.jobs.lock().unwrap().clear();
    }

    /// Simpan antrean sekarang (dipanggil sebelum aplikasi ditutup).
    pub fn save(&self) {
        self.save_jobs();
    }

    // ---- tampilan & kendali ----
    pub fn views(&self) -> Vec<Value> {
        self.jobs.lock().unwrap().iter().map(UploadJob::view).collect()
    }

    pub fn active_count(&self) -> usize {
        self.jobs.lock().unwrap().iter().filter(|j| ACTIVE.contains(&j.status.as_str())).count()
    }

    /// Dampak ke kuota storage sebelum unggahan dimulai.
    pub fn preview(&self, pid: i64, paths: &[String]) -> Result<Value, ApiError> {
        let prof = self.http.call(Method::GET, &format!("/profiles/{pid}"), None)?;
        let mut files = vec![];
        for p in paths {
            let size = fs::metadata(p).map_err(|e| ApiError::new(format!("{p}: {e}")))?.len();
            files.push(json!({"path": p, "name": Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()), "size": size}));
        }
        let total: u64 = files.iter().filter_map(|f| f["size"].as_u64()).sum();
        let free = prof["free_bytes"].as_i64().unwrap_or(0);
        Ok(json!({"files": files, "total_bytes": total, "free_bytes": free, "free_after_bytes": free - total as i64,
                  "storage_gb": prof["storage_gb"], "fits": (total as i64) <= free}))
    }

    pub fn queue(self: &Arc<Self>, pid: i64, paths: &[String]) -> Result<Vec<Value>, ApiError> {
        let mut new = vec![];
        for p in paths {
            new.push(self.new_job(pid, p)?);
        }
        let views = new.iter().map(UploadJob::view).collect();
        self.jobs.lock().unwrap().extend(new);
        self.save_jobs();
        self.wake_worker();
        Ok(views)
    }

    pub fn pause(&self, id: &str) {
        self.modify(id, |j| {
            if ACTIVE.contains(&j.status.as_str()) {
                j.status = "paused".into();
            }
        });
        self.save_jobs();
    }

    pub fn resume(self: &Arc<Self>, id: &str) {
        self.modify(id, |j| {
            if matches!(j.status.as_str(), "paused" | "error") {
                j.status = "queued".into();
                j.error = None;
            }
        });
        self.save_jobs();
        self.wake_worker();
    }

    pub fn cancel(&self, id: &str) {
        let removed = {
            let mut jobs = self.jobs.lock().unwrap();
            jobs.iter().position(|j| j.id == id).map(|i| jobs.remove(i))
        };
        if let Some(j) = removed {
            if let (Some(fid), None) = (j.file_id, &j.result) {
                let _ = self.http.call(Method::DELETE, &format!("/files/{fid}"), None); // dibersihkan server otomatis bila gagal
            }
        }
        self.save_jobs();
    }

    pub fn clear_finished(&self) {
        self.jobs.lock().unwrap().retain(|j| !matches!(j.status.as_str(), "verified" | "done"));
    }

    // ---- internal ----
    fn new_job(&self, pid: i64, path: &str) -> Result<UploadJob, ApiError> {
        let meta = fs::metadata(path).map_err(|e| ApiError::new(format!("{path}: {e}")))?;
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
        let id = format!("{:012x}", (nanos ^ (self.counter.fetch_add(1, Ordering::SeqCst) << 40)) & 0xFFFF_FFFF_FFFF);
        Ok(UploadJob {
            id, profile_id: pid, path: path.into(),
            name: Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            size: meta.len(), status: "queued".into(), sha256: None, file_id: None, sent: 0, hashed: 0, speed_bps: 0.0,
            eta_s: None, error: None, result: None,
        })
    }

    fn modify<R>(&self, id: &str, f: impl FnOnce(&mut UploadJob) -> R) -> Option<R> {
        self.jobs.lock().unwrap().iter_mut().find(|j| j.id == id).map(f)
    }

    fn snapshot(&self, id: &str) -> Option<UploadJob> {
        self.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    fn stopped(&self, id: &str) -> bool {
        self.http.token().is_none() || self.jobs.lock().unwrap().iter().find(|j| j.id == id).map_or(true, |j| j.status == "paused")
    }

    fn wake_worker(self: &Arc<Self>) {
        if !self.autostart {
            return;
        }
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_all();
        let mut w = self.worker.lock().unwrap();
        if w.as_ref().map_or(true, |t| t.is_finished()) {
            let me = Arc::clone(self);
            *w = thread::Builder::new().name("uploads".into()).spawn(move || me.worker_loop()).ok();
        }
    }

    fn worker_loop(self: Arc<Self>) {
        loop {
            let next = self.jobs.lock().unwrap().iter().find(|j| j.status == "queued").map(|j| j.id.clone());
            match next {
                None => {
                    let (lock, cv) = &self.wake;
                    let mut flag = lock.lock().unwrap();
                    if !*flag {
                        flag = cv.wait_timeout(flag, Duration::from_secs(5)).unwrap().0;
                    }
                    *flag = false;
                }
                Some(id) => {
                    self.run_job(&id);
                    self.save_jobs();
                }
            }
        }
    }

    fn run_job(&self, id: &str) {
        let mut backoff = 5u64;
        loop {
            match self.upload_job(id) {
                Ok(()) => return,
                Err(UErr::Net(e)) => {
                    if self.stopped(id) {
                        return;
                    }
                    if backoff > 120 {
                        self.modify(id, |j| {
                            j.status = "error".into();
                            j.error = Some(format!("{e}. Klik Lanjutkan untuk mencoba lagi."));
                        });
                        return;
                    }
                    self.modify(id, |j| {
                        j.status = "retrying".into();
                        j.error = Some(format!("Koneksi terputus, mencoba lagi dalam {backoff} detik..."));
                    });
                    self.sleep_unless_stopped(id, Duration::from_secs(backoff));
                    backoff *= 2;
                    if self.stopped(id) {
                        return;
                    }
                }
                Err(UErr::Api(e)) => {
                    if !self.stopped(id) {
                        self.modify(id, |j| {
                            j.status = "error".into();
                            j.error = Some(e);
                        });
                    }
                    return;
                }
            }
        }
    }

    fn sleep_unless_stopped(&self, id: &str, d: Duration) {
        let end = Instant::now() + d;
        while Instant::now() < end && !self.stopped(id) {
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Seperti Http::call, tapi kegagalan jaringan/server sibuk dibedakan supaya unggahan dicoba ulang otomatis.
    fn net(&self, method: Method, path: &str, body: Option<Vec<u8>>, headers: &[(&str, String)], timeout: Duration, allow: &[u16])
        -> Result<(u16, Value), UErr> {
        let (code, val) = self.http.request_raw(method, path, body, headers, Some(timeout))
            .map_err(|_| UErr::Net("Tidak bisa terhubung ke server".into()))?;
        if code >= 500 && code != 507 {
            return Err(UErr::Net(format!("Server sibuk ({code})")));
        }
        if code >= 400 && !allow.contains(&code) {
            return Err(UErr::Api(detail(&val, code)));
        }
        Ok((code, val))
    }

    fn upload_job(&self, id: &str) -> Result<(), UErr> {
        let Some(job) = self.snapshot(id) else { return Ok(()) };
        let path = PathBuf::from(&job.path);
        let meta = fs::metadata(&path).map_err(|_| UErr::Api("File di laptop sudah tidak ada (dipindah atau dihapus)".into()))?;
        if meta.len() != job.size {
            return Err(UErr::Api("File berubah sejak dipilih. Hapus dari antrean lalu unggah ulang.".into()));
        }
        let api = |e: std::io::Error| UErr::Api(e.to_string());
        let sha = match job.sha256.clone() {
            Some(s) => s,
            None => {
                self.modify(id, |j| {
                    j.status = "hashing".into();
                    j.error = None;
                    j.hashed = 0;
                });
                let mut h = Sha256::new();
                let mut f = File::open(&path).map_err(api)?;
                let mut buf = vec![0u8; HASH_BLOCK];
                loop {
                    let n = f.read(&mut buf).map_err(api)?;
                    if n == 0 {
                        break;
                    }
                    if self.stopped(id) {
                        return Ok(());
                    }
                    h.update(&buf[..n]);
                    self.modify(id, |j| j.hashed += n as u64);
                }
                let s: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
                self.modify(id, |j| j.sha256 = Some(s.clone()));
                self.save_jobs();
                s
            }
        };
        if self.stopped(id) {
            return Ok(());
        }
        self.modify(id, |j| {
            j.status = "uploading".into();
            j.error = None;
        });
        let start = serde_json::to_vec(&json!({"filename": job.name, "size_bytes": job.size, "sha256": sha})).unwrap();
        let (_, mut info) = self.net(Method::POST, &format!("/profiles/{}/uploads", job.profile_id), Some(start),
                                     &[("Content-Type", "application/json".into())], Duration::from_secs(30), &[])?;
        let file_id = info["id"].as_i64().ok_or_else(|| UErr::Api("Server tidak mengirim id upload".into()))?;
        let received = info["received_bytes"].as_u64().unwrap_or(0);
        self.modify(id, |j| {
            j.file_id = Some(file_id);
            j.sent = received;
        });
        let mut speed = Speed::new(received);
        let mut fh = File::open(&path).map_err(api)?;
        while info["status"] == "UPLOADING" {
            if self.stopped(id) {
                return Ok(());
            }
            let offset = info["received_bytes"].as_u64().unwrap_or(0);
            fh.seek(SeekFrom::Start(offset)).map_err(api)?;
            let mut data = vec![0u8; UPLOAD_CHUNK];
            let mut got = 0;
            while got < UPLOAD_CHUNK {
                let n = fh.read(&mut data[got..]).map_err(api)?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            if got == 0 {
                return Err(UErr::Api("File berubah saat diunggah. Unggah ulang.".into()));
            }
            data.truncate(got);
            let (code, val) = self.net(Method::PUT, &format!("/uploads/{file_id}"), Some(data),
                                       &[("Upload-Offset", offset.to_string())], Duration::from_secs(600), &[409])?;
            if code == 409 {
                match val["detail"].as_object().and_then(|d| d.get("received_bytes")).and_then(Value::as_u64) {
                    Some(rb) => {
                        info["received_bytes"] = json!(rb);
                        continue;
                    }
                    None => return Err(UErr::Api(val["detail"].to_string())),
                }
            }
            info = val;
            let sent = info["received_bytes"].as_u64().unwrap_or(0);
            let (rate, eta) = speed.update(sent, job.size);
            self.modify(id, |j| {
                j.sent = sent;
                j.speed_bps = rate;
                j.eta_s = eta;
            });
        }
        let verified = info["verified"].as_bool().unwrap_or(false);
        self.modify(id, |j| {
            j.result = Some(info.clone());
            j.status = if verified { "verified" } else { "done" }.into();
            j.speed_bps = 0.0;
            j.eta_s = None;
        });
        Ok(())
    }

    // ---- penyimpanan antrean ----
    fn save_jobs(&self) {
        let Some(uid) = *self.user_id.lock().unwrap() else { return };
        let mut data: Map<String, Value> =
            fs::read_to_string(&self.file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let list: Vec<Value> = self.jobs.lock().unwrap().iter()
            .filter(|j| !matches!(j.status.as_str(), "verified" | "done" | "cancelled")).map(UploadJob::persist).collect();
        data.insert(uid.to_string(), json!(list));
        if let Some(dir) = self.file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&self.file, Value::Object(data).to_string());
    }

    fn load_jobs(self: &Arc<Self>) {
        let Some(uid) = *self.user_id.lock().unwrap() else { return };
        let Some(data) = fs::read_to_string(&self.file).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { return };
        let saved = data[uid.to_string()].as_array().cloned().unwrap_or_default();
        {
            let mut jobs = self.jobs.lock().unwrap();
            for d in &saved {
                let (Some(id), Some(path)) = (d["id"].as_str(), d["path"].as_str()) else { continue };
                if jobs.iter().any(|j| j.id == id) {
                    continue;
                }
                jobs.push(UploadJob {
                    id: id.into(), profile_id: d["profile_id"].as_i64().unwrap_or(0), path: path.into(),
                    name: d["name"].as_str().unwrap_or("").into(), size: d["size"].as_u64().unwrap_or(0),
                    status: if d["status"] == "paused" { "paused" } else { "queued" }.into(),
                    sha256: d["sha256"].as_str().map(str::to_string), file_id: d["file_id"].as_i64(),
                    sent: d["sent"].as_u64().unwrap_or(0), hashed: 0, speed_bps: 0.0, eta_s: None, error: None, result: None,
                });
            }
        }
        if !saved.is_empty() {
            self.wake_worker();
        }
    }

    // ---- unduh ----
    /// Unduh lewat file .partial; unduhan yang terputus dilanjutkan dengan Range, lalu checksum dicek.
    pub fn download(&self, fid: i64, dest: &str) -> Result<Value, ApiError> {
        let tmp = PathBuf::from(format!("{dest}.partial"));
        let have = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        let mut r = self.http.get_stream(&format!("/files/{fid}/download"), (have > 0).then(|| format!("bytes={have}-")))?;
        let code = r.status().as_u16();
        let expected = r.headers().get("x-content-sha256").and_then(|v| v.to_str().ok()).map(str::to_string);
        if code != 416 {
            if code >= 400 {
                return Err(ApiError::new("File tidak bisa diunduh"));
            }
            let append = code == 206;
            let mut out = fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&tmp)
                .map_err(|e| ApiError::new(e.to_string()))?;
            let mut buf = vec![0u8; 1024 * 1024];
            loop {
                let n = r.read(&mut buf).map_err(|_| ApiError::new("Unduhan terputus. Coba unduh lagi; bagian yang sudah ada akan dilanjutkan."))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).map_err(|e| ApiError::new(e.to_string()))?;
            }
        }
        if let Some(sha) = &expected {
            if sha256_file(&tmp).map_err(|e| ApiError::new(e.to_string()))? != *sha {
                let _ = fs::remove_file(&tmp);
                return Err(ApiError::new("File rusak saat diunduh. Silakan unduh ulang."));
            }
        }
        fs::rename(&tmp, dest).map_err(|e| ApiError::new(e.to_string()))?;
        Ok(json!({"path": dest, "verified": expected.is_some()}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;

    fn uploads(dir: &Path) -> Arc<Uploads> {
        let http = Arc::new(Http::new(&ClientConfig { api_url: "http://127.0.0.1:9".into(), ..ClientConfig::default() }).unwrap());
        http.set_token(Some("t".into()));
        Uploads::build(http, dir, false) // tanpa thread pekerja: status berubah hanya oleh pemanggil
    }

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> String {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn queue_pause_resume_cancel_and_clear() {
        let t = tempfile::tempdir().unwrap();
        let u = uploads(t.path());
        let views = u.queue(7, &[file(t.path(), "a.zip", b"aaa"), file(t.path(), "b.zip", b"bbbb")]).unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!((views[0]["name"].as_str(), views[0]["size"].as_u64(), views[0]["status"].as_str()), (Some("a.zip"), Some(3), Some("queued")));
        let (a, b) = (views[0]["id"].as_str().unwrap().to_string(), views[1]["id"].as_str().unwrap().to_string());
        assert_ne!(a, b);
        assert_eq!(u.active_count(), 2);

        u.pause(&a);
        assert_eq!(u.views()[0]["status"], "paused");
        assert_eq!(u.active_count(), 1);
        u.resume(&a);
        assert_eq!(u.views()[0]["status"], "queued");
        u.cancel(&b);
        assert_eq!(u.views().len(), 1);
        u.modify(&a, |j| j.status = "verified".into());
        u.clear_finished();
        assert!(u.views().is_empty());
    }

    #[test]
    fn jobs_survive_restart_and_start_queued() {
        let t = tempfile::tempdir().unwrap();
        let u = uploads(t.path());
        u.set_user(5);
        let id = u.queue(7, &[file(t.path(), "film.mp4", &[1u8; 100])]).unwrap()[0]["id"].as_str().unwrap().to_string();
        u.modify(&id, |j| {
            j.status = "uploading".into();
            j.sha256 = Some("ab".repeat(32));
            j.sent = 50;
        });
        u.save_jobs();

        let again = uploads(t.path());
        again.set_user(6); // akun lain tidak melihat antrean ini
        assert!(again.views().is_empty());
        let again = uploads(t.path());
        again.set_user(5);
        let v = again.views();
        assert_eq!(v.len(), 1);
        assert_eq!((v[0]["status"].as_str(), v[0]["sent"].as_u64()), (Some("queued"), Some(50))); // dilanjutkan otomatis
    }

    #[test]
    fn speed_is_smoothed_and_eta_computed() {
        let mut s = Speed::new(0);
        thread::sleep(Duration::from_millis(50));
        let (rate, eta) = s.update(1_000_000, 3_000_000);
        assert!(rate > 0.0 && eta.unwrap() > 0.0);
    }

    #[test]
    fn missing_file_is_rejected_when_queued() {
        let t = tempfile::tempdir().unwrap();
        assert!(uploads(t.path()).queue(1, &["C:/tidak/ada.bin".into()]).is_err());
    }
}
