"use strict";
const $ = (id) => document.getElementById(id);
let ME = null, PROFILE = null, POLL = null, LAST_SESSION = null, prefs = {};
let MINIMIZED = false; // layar sesi diperkecil jadi bilah di bawah: sesi tetap berjalan, aplikasi bisa dipakai
let QUALITY_LIVE = false; // sesi hidup dan kualitasnya bisa diganti (layar PC terbuka atau baru ditutup)

async function api(name, ...args) {
  const r = await window.pywebview.api[name](...args);
  if (!r.ok) throw new Error(r.error || "Gagal");
  return r.data;
}

function toast(msg, ms = 3200) {
  const t = $("toast");
  t.textContent = msg; t.classList.remove("hidden");
  clearTimeout(toast._t); toast._t = setTimeout(() => t.classList.add("hidden"), ms);
}

const fmtDur = (s) => {
  if (s == null) return "-";
  if (s >= 1e8) return "Tanpa batas";
  s = Math.max(0, Math.floor(s));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60);
  return h ? `${h} j ${m} m` : `${m} m ${s % 60} d`;
};
// Jam reset dari server (UTC) ditampilkan dalam waktu layanan, bukan zona waktu laptop pemain.
const fmtClock = (iso) => {
  const offsetH = { WIB: 7, WITA: 8, WIT: 9 }[ME && ME.quota && ME.quota.tz_label] ?? 7;
  const d = new Date(new Date(iso).getTime() + offsetH * 3600e3);
  return `${String(d.getUTCHours()).padStart(2, "0")}.${String(d.getUTCMinutes()).padStart(2, "0")}`;
};
const fmtBytes = (b) => b > 1 << 30 ? (b / (1 << 30)).toFixed(2) + " GB" : b > 1 << 20 ? (b / (1 << 20)).toFixed(1) + " MB" : Math.ceil(b / 1024) + " KB";
const rupiah = (n) => "Rp" + Number(n).toLocaleString("id-ID");
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const IS_MAC = /Mac/.test(navigator.platform || navigator.userAgent);
// Aplikasi HP: bridge melaporkan platform "android"/"ios" lewat config. Aplikasi desktop (Python maupun Rust) tidak
// mengirimnya atau mengirim nama desktop, jadi perilakunya tidak berubah.
let PLATFORM = "desktop", IS_MOBILE = false, MOBILE_STREAM = false;
let APP_VERSION = "-", UPDATE = null, UPDATE_DISMISSED = false;

/* ---------- pembaruan aplikasi ---------- */
// Alur seperti aplikasi umumnya: versi baru diunduh di latar belakang (aplikasi tetap bisa dipakai), lalu
// "Mulai ulang sekarang" -> layar "Memasang pembaruan" -> installer tanpa jendela -> CloudPC terbuka lagi sendiri.
let UPDATE_STATE = { status: "idle" }, UPDATE_POLL = null, INSTALL_WHEN_READY = false;
const mbOf = (b) => (b / 1e6).toFixed(b < 10e6 ? 1 : 0);

async function checkUpdate(manual = false) {
  try {
    UPDATE = await api("check_update");
    if (manual && !UPDATE.update_available) toast(`CloudPC sudah versi terbaru (${APP_VERSION}).`);
    if (manual) UPDATE_DISMISSED = false;
    renderUpdate();
    if (UPDATE.update_available) downloadUpdate(false);
  } catch (err) { if (manual) toast(err.message); }
}

async function downloadUpdate(userAsked) {
  if (["downloading", "ready", "installing"].includes(UPDATE_STATE.status)) return;
  try {
    await api("download_update");
  } catch (err) {
    if (userAsked) toast(err.message, 5000);  // mis. sedang sesi: diunduh otomatis setelah sesi selesai
    return;
  }
  clearInterval(UPDATE_POLL);
  UPDATE_POLL = setInterval(async () => {
    const st = await api("update_state").catch(() => null);
    if (!st) return;
    renderUpdate(st);
    if (st.status === "ready" || st.status === "error") clearInterval(UPDATE_POLL);
    if (st.status === "ready" && INSTALL_WHEN_READY) installUpdate();
  }, 400);
}

async function installUpdate() {
  if (LAST_SESSION && !["ENDED", "FAILED"].includes(LAST_SESSION.state) && (MINIMIZED || !$("session-overlay").classList.contains("hidden"))) {
    toast("Akhiri sesi dulu, lalu mulai ulang untuk memasang pembaruan.", 5000);
    return;
  }
  // HP: Android menampilkan dialog pemasangnya sendiri di atas aplikasi (dan bisa dibatalkan), jadi tanpa layar penutup.
  if (!IS_MOBILE) $("update-overlay").classList.remove("hidden");
  try {
    await api("install_update");  // desktop: aplikasi tertutup sendiri ±2 detik lagi
  } catch (err) {
    $("update-overlay").classList.add("hidden");
    toast(err.message, 5000);
  }
}

function onUpdateButton() {
  const st = UPDATE_STATE.status;
  if (st === "ready") return installUpdate();
  INSTALL_WHEN_READY = true;  // "Update sekarang" saat belum selesai diunduh: pasang begitu siap
  renderUpdate();
  if (st !== "downloading") downloadUpdate(true);
}

// Tampil di tiga tempat: layar login (sebelum masuk), banner Beranda, dan kartu "Pembaruan aplikasi" di tab Akun.
function renderUpdate(state = UPDATE_STATE) {
  UPDATE_STATE = state || { status: "idle" };
  const st = UPDATE_STATE.status;
  const avail = !!(UPDATE && UPDATE.update_available);
  const v = avail ? UPDATE.latest.version : "";
  let text = "", button = "Update sekarang", disabled = false;
  if (avail) {
    text = UPDATE.required
      ? `Versi ${v} wajib dipasang sebelum memakai CloudPC lagi (${mbOf(UPDATE.latest.size || 0)} MB).`
      : `Versi baru CloudPC ${v} tersedia.`;
    if (UPDATE.latest.notes) text += ` ${UPDATE.latest.notes}`;
  }
  if (st === "downloading") {
    const total = UPDATE_STATE.total || (UPDATE && UPDATE.latest && UPDATE.latest.size) || 0;
    text = `Mengunduh pembaruan ${v}... ${UPDATE_STATE.progress || 0}% (${mbOf(UPDATE_STATE.done || 0)} / ${mbOf(total)} MB)`;
    button = INSTALL_WHEN_READY ? "Dipasang setelah selesai" : "Pasang setelah selesai";
    disabled = INSTALL_WHEN_READY;
  } else if (st === "ready") {
    text = IS_MOBILE ? `Pembaruan ${v} siap dipasang. Ketuk "Pasang sekarang", lalu "Update" di dialog Android.`
      : `Pembaruan ${v} siap dipasang. CloudPC akan dimulai ulang (±10 detik).`;
    button = IS_MOBILE ? "Pasang sekarang" : "Mulai ulang sekarang";
  } else if (st === "installing") {
    text = "Memasang pembaruan...";
    disabled = true;
  } else if (st === "error") {
    text = UPDATE_STATE.error;
    button = "Coba lagi";
  }
  document.querySelectorAll(".update-text").forEach((e) => (e.textContent = text));
  document.querySelectorAll(".update-now").forEach((b) => { b.textContent = button; b.disabled = disabled; });
  document.querySelectorAll(".update-bar").forEach((b) => {
    b.classList.toggle("hidden", st !== "downloading");
    b.firstElementChild.style.width = `${UPDATE_STATE.progress || 0}%`;
  });
  document.querySelectorAll(".update-version").forEach((e) => (e.textContent = v || "baru"));
  // Banner login selalu tampil bila ada versi baru; banner Beranda bisa ditunda dengan "Nanti" (kecuali wajib/siap).
  const active = st === "downloading" || st === "ready" || st === "error";
  $("login-update").classList.toggle("hidden", !avail);
  $("update-banner").classList.toggle("hidden", !(avail && (UPDATE.required || !UPDATE_DISMISSED || INSTALL_WHEN_READY || st === "ready")));
  $("update-later").classList.toggle("hidden", !avail || UPDATE.required || st === "installing");
  $("acc-update-status").textContent = !UPDATE ? "belum dicek" : avail ? text : "sudah versi terbaru";
  $("acc-update-actions").classList.toggle("hidden", !avail);
  // Versi wajib: sesi baru tidak bisa dimulai sebelum update.
  if (avail && UPDATE.required && $("start-btn")) {
    $("start-btn").disabled = true;
    $("start-hint").textContent = "Update aplikasi dulu";
  }
  return active;
}

document.querySelectorAll(".update-now").forEach((b) => (b.onclick = onUpdateButton));
$("update-later").onclick = () => { UPDATE_DISMISSED = true; INSTALL_WHEN_READY = false; renderUpdate(); };
$("update-check").onclick = () => checkUpdate(true);

/* ---------- start ---------- */
async function boot() {
  const cfg = await api("config");
  document.title = cfg.brand_name;
  APP_VERSION = cfg.version || "-";
  PLATFORM = cfg.platform || "desktop";
  IS_MOBILE = PLATFORM === "android" || PLATFORM === "ios";
  MOBILE_STREAM = IS_MOBILE && !!cfg.moonlight_installed;
  document.body.classList.toggle("is-mobile", IS_MOBILE);
  if (IS_MOBILE) $("q-live-hint").textContent = "Sesi sedang berjalan. Perubahan di sini langsung dipakai saat layar PC dibuka lagi: tekan \"Buka sesi\" di atas, lalu \"Sambung lagi\". PC, file, dan kuotamu tidak terpengaruh.";
  $("mobile-stream-note").classList.toggle("hidden", !IS_MOBILE || MOBILE_STREAM);
  $("app-version").textContent = APP_VERSION;
  const upd = cfg.update_result;  // aplikasi baru saja dibuka lagi oleh installer pembaruan
  if (upd) toast(upd.ok ? `CloudPC berhasil diperbarui ke versi ${upd.version}.`
    : `Pembaruan ke versi ${upd.target} belum terpasang. Coba lagi dari tab Akun.`, 6000);
  checkUpdate();  // juga sebelum login, supaya pengguna lama langsung tahu ada versi baru
  document.querySelectorAll(".brand").forEach((e) => (e.textContent = cfg.brand_name));
  $("moonlight-missing").classList.toggle("hidden", cfg.moonlight_installed || IS_MOBILE);
  // Di Mac, pintasan Moonlight memakai tombol Option (posisi tombol Alt).
  if (IS_MAC) $("s-keys").innerHTML = $("s-keys").innerHTML.replaceAll("Alt+", "Option+");
  prefs = await api("load_prefs");
  // HP: layar kecil dan sering memakai kuota data, jadi bawaan pertama 720p 60 FPS 8 Mbps (bukan 1080p 20 Mbps).
  if (IS_MOBILE && !prefs.quality) prefs.quality = { resolution: "1280x720", fps: 60, bitrate_kbps: 8000, codec: "auto", display_mode: "fullscreen" };
  if (prefs.email) $("login-email").value = prefs.email;
  applyPrefs();
  // "Ingat saya": masuk otomatis (juga setelah update aplikasi). Gagal/kedaluwarsa = tetap di layar login.
  try {
    const me = await api("resume_login");
    if (me) { ME = me; showApp(); }
  } catch { /* tampilkan layar login */ }
}
window.addEventListener("pywebviewready", () => boot().catch((e) => toast(e.message)));

/* ---------- login ---------- */
$("login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  $("login-error").textContent = "";
  $("login-btn").disabled = true;
  try {
    ME = await api("login", $("login-email").value.trim(), $("login-password").value, $("login-remember").checked);
    $("login-password").value = "";
    savePrefs();
    showApp();
  } catch (err) {
    $("login-error").textContent = err.message;
  } finally {
    $("login-btn").disabled = false;
  }
});

/* ---------- daftar (kode undangan) ---------- */
// which: "login" | "register" | "forgot" (true/false = daftar/masuk, untuk pemanggil lama)
function showAuthForm(which) {
  if (which === true) which = "register";
  if (which === false || !which) which = "login";
  $("login-form").classList.toggle("hidden", which !== "login");
  $("register-form").classList.toggle("hidden", which !== "register");
  $("forgot-form").classList.toggle("hidden", which !== "forgot");
  $("login-error").textContent = ""; $("register-error").textContent = ""; $("forgot-error").textContent = "";
  ({ login: $("login-email"), register: $("reg-code"), forgot: $("fg-email") })[which].focus();
}
$("to-register").onclick = () => showAuthForm("register");
$("to-login").onclick = () => showAuthForm("login");

/* ---------- lupa password (kode dari admin) ---------- */
$("to-forgot").onclick = () => {
  $("fg-email").value = $("login-email").value.trim();
  $("fg-sent").textContent = "";
  showAuthForm("forgot");
};
$("fg-to-login").onclick = () => showAuthForm("login");
$("fg-request").onclick = async () => {
  const err = (m) => ($("forgot-error").textContent = m);
  err("");
  const email = $("fg-email").value.trim();
  if (!email || !$("fg-email").checkValidity()) return err("Isi email akunmu dengan benar.");
  $("fg-request").disabled = true;
  try {
    await api("forgot_password", email);
    $("fg-sent").textContent = "Permintaan terkirim. Admin CloudPC akan mengirim kode reset ke WhatsApp atau email akunmu. " +
      "Kode berlaku 30 menit. Belum dapat kode? Hubungi admin.";
    $("fg-code").focus();
  } catch (ex) {
    err(ex.message);
  } finally { $("fg-request").disabled = false; }
};
$("forgot-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const err = (m) => ($("forgot-error").textContent = m);
  err("");
  const email = $("fg-email").value.trim();
  if (!email) return err("Isi email akunmu.");
  if (!$("fg-code").value.trim()) return err("Isi kode reset dari admin. Belum punya? Klik \"Minta kode reset\".");
  if ($("fg-password").value.length < 8) return err("Password minimal 8 karakter.");
  if ($("fg-password").value !== $("fg-password2").value) return err("Ulangi password dengan sama persis.");
  $("fg-btn").disabled = true;
  try {
    ME = await api("reset_password", email, $("fg-code").value.trim(), $("fg-password").value, $("fg-remember").checked);
    ["fg-code", "fg-password", "fg-password2"].forEach((id) => ($(id).value = ""));
    $("login-email").value = ME.user.email;
    showAuthForm("login");
    showApp();
    savePrefs();
    toast("Password berhasil diganti. Selamat datang kembali!", 4000);
  } catch (ex) {
    err(ex.message);
  } finally { $("fg-btn").disabled = false; }
});

$("register-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const err = (m) => ($("register-error").textContent = m);
  err("");
  if ($("reg-password").value !== $("reg-password2").value) return err("Ulangi password dengan sama persis.");
  if ($("reg-password").value.length < 8) return err("Password minimal 8 karakter.");
  if (!$("reg-isp").value) return err("Pilih provider internetmu.");
  if ($("reg-isp").value === "lainnya" && !$("reg-isp-other").value.trim()) return err("Tulis nama provider internetmu.");
  $("register-btn").disabled = true;
  try {
    ME = await api("register", {
      invite_code: $("reg-code").value.trim(), name: $("reg-name").value.trim(), email: $("reg-email").value.trim(),
      password: $("reg-password").value, city: $("reg-city").value.trim(), isp: ispValue("reg-isp", "reg-isp-other"),
      remember: $("reg-remember").checked,
    });
    ["reg-password", "reg-password2"].forEach((id) => ($(id).value = ""));
    $("login-email").value = ME.user.email;
    showAuthForm(false);
    showApp();
    savePrefs();
    toast("Pendaftaran berhasil. Selamat datang!", 4000);
  } catch (ex) {
    err(ex.message);
  } finally { $("register-btn").disabled = false; }
});

$("logout-btn").onclick = $("logout-btn-m").onclick = async () => {
  await api("logout");
  ME = null;
  PROFILE = null;
  LAST_SESSION = null;
  clearAccountData();
  $("view-app").classList.add("hidden");
  $("view-login").classList.remove("hidden");
};

// Laptop bisa dipakai bergantian: jangan sampai data akun sebelumnya masih terlihat setelah ganti akun.
function clearAccountData() {
  $("files-body").innerHTML = "";
  $("files-empty").classList.add("hidden");
  $("upload-status").textContent = "";
  $("uploads-list").innerHTML = "";
  $("uploads").classList.add("hidden");
  $("upload-preview").classList.add("hidden");
  PREVIEW = null;
  document.querySelectorAll(".nav").forEach((x) => x.classList.toggle("active", x.dataset.tab === "home"));
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("hidden", t.id !== "tab-home"));
}

function showApp() {
  clearAccountData();
  $("view-login").classList.add("hidden");
  $("view-app").classList.remove("hidden");
  $("user-email").textContent = $("user-email-m").textContent = ME.user.email;
  syncLocationFromAccount();
  render();
  netCheck();
  loadBusy();
  if (!loadBusy._timer) {
    loadBusy._timer = setInterval(() => {
      if (!$("view-app").classList.contains("hidden") && !$("tab-home").classList.contains("hidden")) loadBusy();
    }, 60e3);
  }
  renderUpdate();
  if (!checkUpdate._timer) checkUpdate._timer = setInterval(() => checkUpdate(), 6 * 3600e3);  // aplikasi dibiarkan terbuka lama
  pollUploads();  // upload yang belum selesai dari sebelumnya dilanjutkan otomatis
  if (ME.live_session && !(IS_MOBILE && !MOBILE_STREAM)) {
    // Aplikasi dibuka ulang saat sesi masih berjalan: lanjutkan. (HP tanpa penampil layar PC tidak ikut membuka sesi
    // yang sedang berjalan di perangkat lain.)
    api("resume_session", ME.live_session).then(openSessionOverlay).catch((e) => toast(e.message));
  }
}

async function refresh() {
  ME = await api("me");
  render();
}

/* ---------- beranda ---------- */
function render() {
  PROFILE = ME.profiles[0] || null;
  const q = ME.quota;
  $("plan-name").textContent = q.plan_name || "Tanpa paket";
  $("profile-name").textContent = PROFILE ? `Profile: ${PROFILE.name}` : "Belum ada profile";

  const unlimited = !q.daily_seconds;
  $("quota-left").textContent = unlimited ? "Tanpa batas" : fmtDur(q.remaining_today);
  const pct = unlimited ? 0 : Math.min(100, (q.used_today / q.daily_seconds) * 100);
  setBar("quota-bar", pct);
  $("quota-detail").textContent = q.has_plan
    ? `Terpakai ${fmtDur(q.used_today)} dari ${unlimited ? "tanpa batas" : fmtDur(q.daily_seconds)}` +
      (q.window_left != null ? ` · jam paket aktif` : "") +
      (!unlimited && q.resets_at ? ` · reset ${fmtClock(q.resets_at)} ${q.tz_label || ""}`.trimEnd() : "") +
      ` · paket berakhir dalam ${Math.floor(q.subscription_left / 86400)} hari`
    : "Hubungi admin untuk mengaktifkan paket.";

  if (PROFILE) {
    const used = PROFILE.used_gb, total = PROFILE.storage_gb;
    $("storage-used").textContent = `${used.toFixed(1)} GB terpakai`;
    $("storage-total").textContent = `${total} GB`;
    setBar("storage-bar", (used / total) * 100);
    const pend = PROFILE.pending_addons.map((a) => `+${a.gb} GB menunggu pembayaran`).join(", ");
    $("storage-detail").textContent = [
      `Sisa ${(total - used).toFixed(1)} GB`,
      PROFILE.last_backup_at ? `backup terakhir ${new Date(PROFILE.last_backup_at).toLocaleString("id-ID")}` : "belum ada backup",
      pend,
    ].filter(Boolean).join(" · ");
    $("addons").innerHTML = ME.storage_addons
      .map((a) => `<button class="btn small" data-gb="${a.gb}">+${a.gb} GB · ${rupiah(a.price_idr)}/bln</button>`).join("");
  }

  renderModeSelect();
  const live = ME.live_session;
  // Sesi masih berjalan di server: tombol ini membuka layar sesinya (Sambung lagi / Akhiri sesi), jadi sesi tidak pernah
  // "terkunci" tanpa cara mengakhirinya walau layar sesi sempat tertutup.
  const canStart = (q.can_start && PROFILE && !live) || !!live;
  $("start-btn").disabled = !canStart;
  $("start-btn").textContent = live ? "Buka sesi" : "Mulai sesi";
  $("start-hint").textContent = live ? "Sesi sedang berjalan. Buka untuk menyambung lagi atau mengakhirinya." : q.can_start ? `Bisa dipakai ${fmtDur(q.seconds_left)} lagi hari ini` : (q.reason || "");
  $("hero-title").textContent = q.can_start ? "PC kamu siap dipakai" : "Belum bisa memulai sesi";
  if (UPDATE && UPDATE.required && UPDATE.update_available) {
    $("start-btn").disabled = true;
    $("start-hint").textContent = "Update aplikasi dulu";
  }
  // HP tanpa penampil layar PC: jangan sampai PC dipesan (dan direstart di akhir sesi) untuk sesi yang tidak bisa dibuka.
  if (IS_MOBILE && !MOBILE_STREAM) {
    $("start-btn").disabled = true;
    $("start-hint").textContent = "Layar PC belum tersedia di HP";
  }
  renderFeedbackReminder();
}

function setBar(id, pct) {
  const el = $(id);
  el.style.width = `${Math.max(0, Math.min(100, pct))}%`;
  el.className = pct > 90 ? "danger" : pct > 75 ? "warn" : "";
}

$("addons").addEventListener("click", async (e) => {
  const gb = e.target.dataset.gb;
  if (!gb || !PROFILE) return;
  try {
    const r = await api("request_addon", PROFILE.id, Number(gb));
    toast(r.message, 5000);
    refresh();
  } catch (err) { toast(err.message); }
});

/* ---------- jam ramai ---------- */
// Data agregat dari /public/occupancy (tanpa nama pemain): kapan PC paling sering dipakai, supaya pemain memilih jam yang pas.
// Skala sama dengan halaman /jam-ramai: hitam (kosong) -> biru -> merah (penuh); sel tanpa data = kotak putus-putus.
const HEAT_STOPS = [[0, [10, 12, 18]], [50, [37, 99, 235]], [100, [225, 29, 36]]];
function heatColor(v) {
  v = Math.max(0, Math.min(100, v));
  for (let i = 1; i < HEAT_STOPS.length; i++) {
    if (v <= HEAT_STOPS[i][0]) {
      const [a, ca] = HEAT_STOPS[i - 1], [b, cb] = HEAT_STOPS[i], t = (v - a) / (b - a);
      return "rgb(" + ca.map((c, k) => Math.round(c + (cb[k] - c) * t)).join(",") + ")";
    }
  }
}
const escHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

async function loadBusy() {
  try {
    renderBusy(await api("occupancy", "gaming"));
  } catch (_) {
    $("busy-card").classList.add("hidden");  // server lama tanpa endpoint, atau belum ada PC di pool ini
  }
}
$("busy-btn").onclick = loadBusy;

function renderBusy(d) {
  $("busy-card").classList.remove("hidden");
  const n = d.now;
  $("busy-now").textContent = !n ? "Belum ada PC aktif"
    : n.available ? `Tersedia · ${n.in_use}/${n.capacity} PC terpakai`
    : `Penuh · ${n.in_use}/${n.capacity} PC terpakai, antre ${n.waiting}`;
  const X0 = 26, CW = 10, CH = 8, G = 1;
  let s = "";
  for (let h = 0; h < 24; h += 6) s += `<text x="${X0 + h * (CW + G)}" y="7">${String(h).padStart(2, "0")}</text>`;
  d.heatmap.forEach((row, di) => {
    const y = 11 + di * (CH + G + 1);
    s += `<text x="${X0 - 4}" y="${y + CH - 1}" text-anchor="end">${escHtml(d.days[di].slice(0, 3))}</text>`;
    row.forEach((v, h) => {
      const x = X0 + h * (CW + G), tip = `${d.days[di]} ${String(h).padStart(2, "0")}:00`;
      s += v == null
        ? `<rect x="${x + .5}" y="${y + .5}" width="${CW - 1}" height="${CH - 1}" rx="2" fill="none" stroke="#9aa3b2" stroke-dasharray="2 2"><title>${escHtml(tip)}: belum ada data</title></rect>`
        : `<rect x="${x}" y="${y}" width="${CW}" height="${CH}" rx="2" fill="${heatColor(v)}"><title>${escHtml(tip)}: ${v}% terpakai</title></rect>`;
    });
  });
  if (d.now_slot) {
    const nx = X0 + d.now_slot.hour * (CW + G), ny = 11 + d.now_slot.day * (CH + G + 1);
    const box = `x="${nx - 1}" y="${ny - 1}" width="${CW + 2}" height="${CH + 2}" rx="3" fill="none"`;
    s += `<rect ${box} stroke="#000" stroke-width="3"/><rect ${box} stroke="#fff" stroke-width="1.4"/>`;
  }
  $("busy-map").innerHTML = s;
  const wins = (list) => list.slice(0, 2).map((w) => `${w.day} ${w.from}–${w.to} (${w.avg_pct}%)`).join("; ");
  $("busy-quiet").textContent = d.quietest.length ? `Paling sepi: ${wins(d.quietest)}` : "Belum cukup data untuk rekomendasi jam.";
  $("busy-busy").textContent = d.busiest.length ? `Paling ramai: ${wins(d.busiest)} · jam ${d.tz}` : "";
}

/* ---------- koneksi ---------- */
// Jalur ke PC: Otomatis (default) memilih yang tercepat; pemain juga bisa memilih sendiri. Ping diukur sampai
// port PC (bukan ke server API), jadi angkanya sama dengan yang dirasakan saat stream.
let NET = null;
const selectedRoute = () => prefs.route || "auto";
/* Mode sesi hanya ditawarkan bila server mendukung (server lama tidak mengirim session_modes). */
const modesSupported = () => !!(ME && (ME.session_modes || []).includes("ephemeral"));
const selectedMode = () => (modesSupported() && $("mode-select").value === "ephemeral" ? "ephemeral" : "profile");
function renderModeSelect() {
  $("mode-wrap").classList.toggle("hidden", !modesSupported());
  const eph = selectedMode() === "ephemeral";
  $("hero-sub").textContent = eph
    ? "Sesi bersih: PC dibersihkan setelah selesai dan tidak ada yang tersimpan. Simpan progres lewat cloud save game."
    : "Semua file dan game tersimpan otomatis di drive P: milikmu.";
}
$("mode-select").addEventListener("change", () => { renderModeSelect(); savePrefs(); });

async function netCheck() {
  $("net-rtt").textContent = "Mengukur...";
  $("net-detail").textContent = "";
  try {
    // PROFILE.local = kandidat jalur LAN dari server; aplikasi mengecek sendiri dan hanya memunculkannya bila PC itu terjangkau.
    const cands = PROFILE && PROFILE.routes && PROFILE.routes.length ? PROFILE.routes.concat(PROFILE.local ? [PROFILE.local] : []) : null;
    NET = await api("net_check", cands);
    renderNet();
  } catch (err) {
    NET = null;
    $("net-rtt").textContent = "-";
    $("net-detail").textContent = err.message;
    $("net-routes").classList.add("hidden");
  }
}

function renderNet() {
  if (!NET) return;
  const routes = NET.routes || [];
  let sel = selectedRoute();
  if (sel !== "auto" && !routes.some((r) => r.id === sel)) sel = "auto";  // jalur pilihan sudah tidak ada
  const cur = routes.find((r) => r.id === (sel === "auto" ? NET.auto : sel)) || NET;
  $("net-rtt").innerHTML = cur.rtt_ms == null ? "Tidak terjangkau" : `${cur.rtt_ms} ms`;
  $("net-rtt").className = `big-num verdict-${cur.verdict}`;
  $("net-detail").textContent = `${cur.advice} Jitter ${cur.jitter_ms ?? "-"} ms, paket hilang ${cur.loss_pct ?? "-"}%.`;
  wifiHint(cur);
  $("net-routes").classList.toggle("hidden", !routes.length);
  if (!routes.length) return;
  const autoLabel = (routes.find((r) => r.id === NET.auto) || {}).label;
  const item = (id, title, info, disabled) => `<label class="route${disabled ? " disabled" : ""}">
      <input type="radio" name="route" value="${esc(id)}"${id === sel ? " checked" : ""}${disabled ? " disabled" : ""}>
      <span class="route-name">${esc(title)}</span><span class="muted small">${esc(info)}</span></label>`;
  const info = (r) => r.rtt_ms == null ? "tidak tersedia" : `${r.rtt_ms} ms · ${r.verdict === "bagus" ? "stabil" : r.verdict}`;
  // Jalur lokal hanya ada di daftar bila terdeteksi: tampilkan perbandingannya dengan relay supaya terlihat untungnya.
  const lan = routes.find((r) => r.kind === "local" && r.rtt_ms != null);
  const relay = routes.find((r) => r.kind === "relay" && r.rtt_ms != null);
  const lanBanner = lan
    ? `<div class="route-lan">Kamu satu jaringan dengan PC ini. Koneksi lokal terdeteksi: <b>${lan.rtt_ms} ms</b>` +
      (relay ? ` (lewat ${esc(relay.label)}: ${relay.rtt_ms} ms)` : "") + `</div>`
    : "";
  $("net-routes").innerHTML = lanBanner +
    item("auto","Otomatis (disarankan)", autoLabel ? `sekarang: ${autoLabel}` : "tidak ada jalur terjangkau", false) +
    routes.map((r) => item(r.id, r.label, info(r), r.rtt_ms == null)).join("");
  $("net-routes").querySelectorAll("input").forEach((el) => el.addEventListener("change", () => {
    prefs.route = el.value;
    savePrefs();
    renderNet();
  }));
}
$("net-btn").onclick = netCheck;

/* ---------- kualitas & preferensi ---------- */
// 120/144 FPS mengirim 2x lebih banyak frame: dengan bitrate rendah tiap frame jadi lebih buram.
function fpsHint() {
  const fps = Number($("q-fps").value), mbps = Number($("q-bitrate").value);
  // Bitrate rendah (internet lambat/seluler): tiap frame hanya dapat sedikit data, jadi kurangi jumlah frame dan piksel.
  const pixels = $("q-res").value.split("x").reduce((a, b) => a * Number(b), 1);
  const low = mbps < 5 && (fps > 30 || pixels > 960 * 540);
  const show = low || (fps >= 120 && mbps < 30);
  $("q-fps-hint").classList.toggle("hidden", !show);
  if (low) $("q-fps-hint").textContent = `${mbps} Mbps terlalu kecil untuk ${$("q-res").value} ${fps} FPS: gambar akan pecah. Pilih 30 FPS dan resolusi ${mbps <= 2 ? "854x480" : "960x540"}.`;
  else if (show) $("q-fps-hint").textContent = `${fps} FPS dengan ${mbps} Mbps bisa terlihat buram; naikkan bitrate ke 30-40 Mbps.`;
  syncBitratePick();
}

/* HP: bitrate dipilih dari daftar (#q-bitrate-pick), bukan slider. Slider berubah nilainya begitu tersentuh jari yang
   sedang menggulir halaman: sesi #113 (23 Nov 2026) diminta 28 Mbps padahal pemain merasa memilih 1 Mbps. Slider tetap
   menjadi sumber nilai (disembunyikan lewat CSS), daftar hanya menyetelnya; baris "Kualitas: ..." di dekat tombol Mulai
   memperlihatkan nilai yang benar-benar akan dikirim. */
const BITRATE_PICKS = [1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 25, 30, 40, 50];
const bitrateLabel = (m) => `${m} Mbps` + (m <= 2 ? " (internet sangat lambat)" : m <= 5 ? " (internet lambat / hemat data)" : "");
$("q-bitrate-pick").innerHTML = BITRATE_PICKS.map((m) => `<option value="${m}">${bitrateLabel(m)}</option>`).join("");
function syncBitratePick() {
  const pick = $("q-bitrate-pick"), v = String(Number($("q-bitrate").value));
  if (![...pick.options].some((o) => o.value === v)) pick.add(new Option(bitrateLabel(Number(v)), v));
  pick.value = v;
  const [, h] = $("q-res").value.split("x");
  $("start-quality").textContent = `Kualitas: ${h}p · ${$("q-fps").value} FPS · ${v} Mbps`;
}
$("q-bitrate-pick").addEventListener("change", () => {
  $("q-bitrate").value = $("q-bitrate-pick").value;
  // Pendengar lama (label, petunjuk, simpan preferensi) tetap terpasang di slider.
  ["input", "change"].forEach((t) => $("q-bitrate").dispatchEvent(new Event(t, { bubbles: true })));
});
["q-fps", "q-bitrate", "q-res"].forEach((id) => $(id).addEventListener("input", fpsHint));

// Preset "koneksi tidak stabil": jitter tinggi atau paket hilang (mis. Wi-Fi kafe) membuat video datang bergerombol dan
// kursor terasa patah. Tawarkan 60 FPS / 12 Mbps hanya bila pengaturan sekarang di atas itu. Ambang dari hasil cek koneksi.
const WIFI_PRESET = { fps: 60, mbps: 12 };
const WIFI_JITTER_MS = 8, WIFI_LOSS_PCT = 1;
let LAST_NET_ROUTE = null;
function wifiHint(cur = LAST_NET_ROUTE) {
  LAST_NET_ROUTE = cur;
  if (!cur) return;
  const why = [];
  if (cur.jitter_ms != null && cur.jitter_ms >= WIFI_JITTER_MS) why.push(`jitter ${cur.jitter_ms} ms`);
  if (cur.loss_pct != null && cur.loss_pct >= WIFI_LOSS_PCT) why.push(`paket hilang ${cur.loss_pct}%`);
  const heavy = Number($("q-fps").value) > WIFI_PRESET.fps || Number($("q-bitrate").value) > WIFI_PRESET.mbps + 3;
  $("q-wifi-hint").classList.toggle("hidden", !(why.length && heavy));
  $("q-wifi-why").textContent = why.join(", ");
}
["q-fps", "q-bitrate"].forEach((id) => $(id).addEventListener("input", () => wifiHint()));
$("q-wifi-preset").onclick = () => {
  $("q-fps").value = String(WIFI_PRESET.fps);
  $("q-bitrate").value = WIFI_PRESET.mbps;
  $("q-bitrate-val").textContent = WIFI_PRESET.mbps;
  fpsHint(); wifiHint(); savePrefs();
  // Sesi sedang berjalan: langsung terapkan (stream dibuka ulang sebentar); kalau belum, berlaku saat mulai sesi.
  if (QUALITY_LIVE) applyQuality();
  else toast("Pengaturan koneksi tidak stabil dipakai untuk sesi berikutnya.");
};

/* Tes semburan UDP (net_burst): ping/TCP kecil tidak melihat paket hilang pada semburan video di Wi-Fi. Mencari bitrate
   tertinggi yang paket hilangnya <= 1%, lalu menawarkan memakainya. Server atau aplikasi lama tanpa fitur ini: disembunyikan. */
let BURST_REC = null;
$("burst-btn").onclick = async () => {
  if (QUALITY_LIVE) { toast("Tes tidak bisa dijalankan saat sesi berjalan; akhiri sesi dulu."); return; }
  const btn = $("burst-btn"), out = $("burst-out"), apply = $("burst-apply");
  btn.disabled = true; apply.classList.add("hidden");
  out.classList.remove("hidden"); out.textContent = "Mengetes... jangan pakai internet untuk hal lain dulu.";
  try {
    const r = await api("net_burst", Math.max(Number($("q-bitrate").value) || 20, 10));
    if (!r.available) { $("burst-wrap").classList.add("hidden"); return; }
    const lines = r.steps.filter((s) => s.ok).map((s) => `${s.kbps / 1000} Mbps: ${s.loss_pct}% paket hilang${s.longest_loss_run > 3 ? ` (hilang beruntun hingga ${s.longest_loss_run})` : ""}`);
    out.textContent = lines.join(" · ") + ". " + (r.clean
      ? `Aman sampai ${r.recommended_mbps} Mbps.`
      : "Paket hilang tetap tinggi walau bitrate rendah: coba kabel LAN, Wi-Fi 5 GHz dekat router, atau hotspot HP.");
    BURST_REC = r.recommended_mbps;
    apply.textContent = `Pakai ${r.recommended_mbps} Mbps`;
    apply.classList.remove("hidden");
  } catch (err) {
    if (/tidak dikenal/i.test(err.message)) $("burst-wrap").classList.add("hidden");  // aplikasi Python lama
    else out.textContent = err.message;
  } finally {
    btn.disabled = false;
  }
};
$("burst-apply").onclick = () => {
  if (BURST_REC == null) return;
  $("q-bitrate").value = BURST_REC;
  $("q-bitrate-val").textContent = BURST_REC;
  fpsHint(); wifiHint(); savePrefs();
  toast(`Bitrate ${BURST_REC} Mbps dipakai untuk sesi berikutnya.`);
};

function quality() {
  return {
    resolution: $("q-res").value, fps: Number($("q-fps").value),
    bitrate_kbps: Number($("q-bitrate").value) * 1000, codec: $("q-codec").value, display_mode: $("q-mode").value,
  };
}
/* ---------- lokasi & provider ---------- */
// Lewat Tailscale server hanya melihat IP 100.x, jadi kota dan provider ditanyakan langsung (tanpa geolokasi IP).
const ISPS = [
  ["", "Pilih provider..."], ["IndiHome", "IndiHome (Telkom)"], ["Biznet", "Biznet"], ["First Media", "First Media"],
  ["MyRepublic", "MyRepublic"], ["CBN", "CBN"], ["Iconnet", "Iconnet (PLN)"], ["XL Home", "XL Home / XL Satu"],
  ["MNC Play", "MNC Play"], ["Oxygen", "Oxygen.id"], ["Telkomsel", "Telkomsel / Orbit (seluler)"],
  ["Indosat", "Indosat / IM3 (seluler)"], ["XL Axis", "XL / Axis (seluler)"], ["Tri", "Tri (seluler)"],
  ["Smartfren", "Smartfren (seluler)"], ["lainnya", "Lainnya..."],
];
document.querySelectorAll(".isp-select").forEach((s) => {
  s.innerHTML = ISPS.map(([v, t]) => `<option value="${esc(v)}">${esc(t)}</option>`).join("");
});
const bindOther = (sel, wrap) => $(sel).addEventListener("change", () => $(wrap).classList.toggle("hidden", $(sel).value !== "lainnya"));
bindOther("isp", "isp-other-wrap");
bindOther("setup-isp", "setup-isp-other-wrap");
bindOther("reg-isp", "reg-isp-other-wrap");
bindOther("acc-isp", "acc-isp-other-wrap");

function ispValue(sel = "isp", other = "isp-other") {
  const v = $(sel).value;
  return v === "lainnya" ? ($(other).value.trim() || "lainnya") : v;
}
const locationComplete = () => $("city").value.trim() !== "" && $("isp").value !== "";

function applyPrefs() {
  const q = prefs.quality || {};
  if (q.resolution) $("q-res").value = q.resolution;
  if (q.fps) $("q-fps").value = String(q.fps);
  if (q.bitrate_kbps) $("q-bitrate").value = q.bitrate_kbps / 1000;
  if (q.codec) $("q-codec").value = q.codec;
  if (q.display_mode) $("q-mode").value = q.display_mode;
  if (prefs.mode === "ephemeral") $("mode-select").value = "ephemeral";
  if (prefs.connection) $("conn-type").value = prefs.connection;
  if (prefs.city) $("city").value = prefs.city;
  if (prefs.isp && ISPS.some(([v]) => v === prefs.isp)) $("isp").value = prefs.isp;
  if (prefs.isp_other) $("isp-other").value = prefs.isp_other;
  $("isp-other-wrap").classList.toggle("hidden", $("isp").value !== "lainnya");
  $("q-bitrate-val").textContent = $("q-bitrate").value;
  fpsHint();
  dataHint();
}
// HP: tampilkan perkiraan pemakaian data (Mbps x 3600 dtk / 8 / 1000 = Mbps x 0,45 GB per jam).
function dataHint() {
  const el = $("q-data-hint");
  el.classList.toggle("hidden", !IS_MOBILE);
  if (!IS_MOBILE) return;
  const gb = (Number($("q-bitrate").value) * 0.45).toFixed(1).replace(".", ",");
  el.textContent = `Stream memakai sekitar ${gb} GB data per jam.`
    + ($("conn-type").value === "seluler" ? " Kamu memakai data seluler: perhatikan kuota internetmu." : "");
}
["q-bitrate", "conn-type"].forEach((id) => $(id).addEventListener("input", dataHint));
function savePrefs() {
  prefs = {
    quality: quality(), connection: $("conn-type").value, city: $("city").value.trim(), email: $("login-email").value.trim(),
    isp: $("isp").value, isp_other: $("isp-other").value.trim(), route: selectedRoute(), mode: selectedMode(),
  };
  api("save_prefs", prefs).catch(() => {});
}
["q-res", "q-fps", "q-bitrate", "q-codec", "q-mode", "conn-type", "city", "isp", "isp-other"].forEach((id) => $(id).addEventListener("change", savePrefs));
$("q-bitrate").addEventListener("input", () => ($("q-bitrate-val").textContent = $("q-bitrate").value));

function openSetup() {
  $("setup-city").value = $("city").value.trim();
  $("setup-isp").value = $("isp").value;
  $("setup-isp-other").value = $("isp-other").value;
  $("setup-conn").value = $("conn-type").value;
  $("setup-isp-other-wrap").classList.toggle("hidden", $("setup-isp").value !== "lainnya");
  $("setup-error").textContent = "";
  $("setup-overlay").classList.remove("hidden");
  $("setup-city").focus();
}
$("setup-cancel").onclick = () => $("setup-overlay").classList.add("hidden");
$("setup-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const city = $("setup-city").value.trim();
  if (!city || !$("setup-isp").value) { $("setup-error").textContent = "Isi kota dan pilih provider internet dulu."; return; }
  if ($("setup-isp").value === "lainnya" && !$("setup-isp-other").value.trim()) {
    $("setup-error").textContent = "Tulis nama provider internetmu."; return;
  }
  $("city").value = city;
  $("isp").value = $("setup-isp").value;
  $("isp-other").value = $("setup-isp-other").value.trim();
  $("conn-type").value = $("setup-conn").value;
  $("isp-other-wrap").classList.toggle("hidden", $("isp").value !== "lainnya");
  $("setup-overlay").classList.add("hidden");
  // Simpan juga ke akun, supaya tidak ditanya lagi di laptop lain.
  api("update_profile", { city, isp: ispValue() }).then((me) => (ME = me)).catch(() => {});
  startSession();
});

/* ---------- sesi ---------- */
$("start-btn").onclick = () => {
  if (ME && ME.live_session) {
    api("resume_session", ME.live_session).then(openSessionOverlay).catch((e) => toast(e.message, 5000));
    return;
  }
  if (!PROFILE) return;
  if (!locationComplete()) { openSetup(); return; }
  startSession();
};

// Kembali ke aplikasi (di HP: dari layar PC; di desktop: jendela dimunculkan lagi). Selama aplikasi tidak terlihat
// timer halaman bisa berhenti, jadi keadaan sesi diperiksa ulang: bila mesin sesi masih memegang sesi, layar sesi dibuka
// lagi dan dipantau; bila tidak, data akun disegarkan (kuota, "Buka sesi").
document.addEventListener("visibilitychange", async () => {
  if (document.hidden || !ME) return;
  try {
    const st = await api("state");
    if (!["idle", "ended", "error"].includes(st.phase)) { if (!MINIMIZED) openSessionOverlay(); }
    else if ($("session-overlay").classList.contains("hidden")) await refresh();
  } catch { /* coba lagi saat kembali berikutnya */ }
});

async function startSession() {
  savePrefs();
  try {
    await api("start_session", PROFILE.id, quality(), {
      connection: $("conn-type").value, city: $("city").value.trim(), isp: ispValue(),
      os: IS_MOBILE ? PLATFORM : navigator.userAgent.includes("Windows") ? "windows" : IS_MAC ? "macos" : "other",
    }, selectedRoute(), selectedMode());
    openSessionOverlay();
  } catch (err) { toast(err.message, 5000); }
}

const STEP_ORDER = ["preparing", "pairing", "connecting", "streaming"];
const TITLES = {
  queued: "Menunggu giliran", idle: "Keluar dari antrean",
  starting: "Memesan PC...", preparing: "Menyiapkan PC...", pairing: "Menghubungkan...", connecting: "Membuka layar...",
  streaming: "Selamat bermain!", stream_closed: "Layar tertutup", ending: "Menutup sesi...", ended: "Sesi selesai", error: "Gagal memulai",
};

function setMinimized(on) {
  MINIMIZED = on;
  $("session-overlay").classList.toggle("hidden", on);
  $("session-bar").classList.toggle("hidden", !on);
  document.body.classList.toggle("has-session-bar", on);
}

function openSessionOverlay() {
  setMinimized(false);
  $("session-overlay").classList.remove("hidden");
  $("s-error").textContent = "";
  ["s-reconnect", "s-close"].forEach((id) => $(id).classList.add("hidden"));
  $("s-end").classList.remove("hidden");
  clearInterval(POLL);
  POLL = setInterval(pollSession, 1000);
  pollSession();
}

async function pollSession() {
  let st;
  try { st = await api("state"); } catch { return; }
  const phase = st.phase;
  if (st.session) LAST_SESSION = st.session;
  $("s-title").textContent = phase === "ended" && st.error ? "Sesi berakhir dengan masalah" : (TITLES[phase] || phase);
  let msg = st.message || "";
  if (phase === "stream_closed" && st.reconnect_seconds_left != null) msg += ` Sesi otomatis diakhiri dalam ${st.reconnect_seconds_left > 90 ? fmtDur(st.reconnect_seconds_left) : st.reconnect_seconds_left + " detik"}.`;
  $("s-msg").textContent = msg;
  $("s-error").textContent = st.error || "";

  // Langkah persiapan hanya relevan sampai tersambung; saat menutup tampilkan progres, saat selesai ringkasan.
  const closing = phase === "ending";
  const done = phase === "ended" || phase === "error" || phase === "idle";
  const queued = phase === "queued";
  $("s-steps").classList.toggle("hidden", closing || done || queued);
  $("s-busy").classList.toggle("hidden", !closing);
  $("s-queue").classList.toggle("hidden", !(queued && st.queue));
  if (queued && st.queue) {
    $("s-q-pos").textContent = st.queue.position > 0 ? `${st.queue.position} dari ${st.queue.waiting}` : "Giliranmu!";
    // Perkiraan kasar dari server (sisa kuota pemain yang sedang main); pemain bisa selesai jauh lebih cepat.
    const etaMin = st.queue.eta_seconds != null ? Math.max(1, Math.ceil(st.queue.eta_seconds / 60)) : null;
    $("s-q-eta").textContent = etaMin == null ? "-" : etaMin < 60 ? `± ${etaMin} menit` : `± ${Math.floor(etaMin / 60)} j ${etaMin % 60} m`;
  }
  $("s-end").textContent = queued ? "Keluar dari antrean" : "Akhiri sesi";
  const idx = STEP_ORDER.indexOf(phase === "stream_closed" ? "streaming" : phase);
  document.querySelectorAll("#s-steps li").forEach((li, i) => {
    li.className = idx > i || (phase === "streaming" && i === idx) ? "done" : i === idx ? "current" : "";
  });

  const s = st.session;
  const ranSecs = s && s.started_at && s.ended_at ? (new Date(s.ended_at) - new Date(s.started_at)) / 1000 : null;
  $("s-summary").classList.toggle("hidden", !(done && ranSecs != null));
  if (done && ranSecs != null) {
    $("s-summary").innerHTML = `<div><div class="muted small">Durasi sesi</div><div class="big-num">${fmtDur(ranSecs)}</div></div>
      <div><div class="muted small">Selesai</div><div class="big-num">${new Date(s.ended_at).toLocaleTimeString("id-ID", { hour: "2-digit", minute: "2-digit" })}</div></div>`;
  }
  const live = s && s.state === "ACTIVE";
  $("s-timer").classList.toggle("hidden", !live);
  $("s-keys").classList.toggle("hidden", phase !== "streaming" && phase !== "stream_closed");
  if (live) {
    const grace = s.grace_seconds_left != null;
    $("s-left-label").textContent = grace ? "Waktu habis · waktu tambahan"
      : s.limited_by_session ? "Sisa waktu sesi ini" : "Sisa waktu hari ini";
    $("s-left").textContent = fmtDur(grace ? s.grace_seconds_left : s.seconds_left);
    $("s-left").classList.toggle("warn-text", grace || s.seconds_left <= 600);
    $("s-elapsed").textContent = fmtDur(s.elapsed_seconds);
  }
  $("s-reconnect").classList.toggle("hidden", phase !== "stream_closed");
  const finished = phase === "ended" || phase === "error" || phase === "idle";
  // Tombol beranda ("Mulai sesi"/"Buka sesi") dibaca dari ME, yang diambil sebelum sesi ini dimulai. Selaraskan dengan
  // keadaan mesin sesi supaya saat layar sesi diperkecil beranda tidak menawarkan "Mulai sesi" untuk sesi yang berjalan.
  if (ME && phase !== "queued") {
    const liveNow = finished ? null : (s || ME.live_session || null);
    const changed = !!liveNow !== !!ME.live_session;
    ME.live_session = liveNow;
    if (changed) render();
  }
  // Perkecil hanya saat stream berjalan; kualitas hanya bisa diganti saat sesi hidup. Selain "streaming"/"connecting"
  // (layar tertutup, mengakhiri, selesai) layar sesi dikembalikan supaya tombol dan ringkasannya terlihat.
  const inStream = phase === "streaming" || (phase === "connecting" && live);
  $("s-minimize").classList.toggle("hidden", phase !== "streaming");
  QUALITY_LIVE = !!(live && (inStream || phase === "stream_closed"));
  // HP: tanpa tombol "Terapkan"; perubahan langsung dikirim (lihat autoApplyQuality) dan dipakai saat layar PC dibuka lagi.
  $("q-apply").classList.toggle("hidden", !QUALITY_LIVE || IS_MOBILE);
  $("q-live-hint").classList.toggle("hidden", !QUALITY_LIVE);
  // Kualitas yang SEDANG dipakai sesi ini (dari server), supaya salah setel langsung terlihat.
  const sq = live && s.quality && s.quality.bitrate_kbps ? s.quality : null;
  $("s-quality").classList.toggle("hidden", !sq);
  if (sq) $("s-quality").textContent = `Kualitas sesi ini: ${sq.resolution || "-"} · ${sq.fps || "-"} FPS · ${sq.bitrate_kbps / 1000} Mbps`;
  // HP: layar PC menutupi aplikasi selama stream, jadi kualitas diganti setelah layar ditutup (X): "Ubah kualitas"
  // memperkecil layar sesi ini jadi bilah, kartu Kualitas stream terbuka, lalu "Buka sesi" -> "Sambung lagi".
  const closedOnPhone = IS_MOBILE && phase === "stream_closed";
  $("s-quality-btn").classList.toggle("hidden", !closedOnPhone);
  if (MINIMIZED) {
    if (!inStream && !closedOnPhone) setMinimized(false);
    else $("sb-text").textContent = closedOnPhone ? `Layar PC tertutup · sesi berakhir ${fmtDur(st.reconnect_seconds_left || 0)} lagi`
      : phase === "connecting" ? "Menerapkan kualitas baru..."
      : `Sesi berjalan · sisa ${fmtDur(s && s.seconds_left)}`;
  }
  $("s-end").classList.toggle("hidden", finished || phase === "ending");
  $("s-close").classList.toggle("hidden", !finished);
  if (finished) {
    clearInterval(POLL);
    // Pembaruan yang tertunda karena sesi (unduhan tidak berjalan saat stream) diunduh sekarang.
    if (UPDATE && UPDATE.update_available) downloadUpdate(false);
  }
}

$("s-end").onclick = async () => {
  $("s-end").disabled = true;
  try { await api("end_session"); } finally { setTimeout(() => ($("s-end").disabled = false), 1500); }
};
$("s-reconnect").onclick = () => api("reconnect");
$("s-minimize").onclick = () => setMinimized(true);
$("sb-open").onclick = () => setMinimized(false);
$("sb-end").onclick = async () => { setMinimized(false); await api("end_session").catch((e) => toast(e.message)); };
async function applyQuality() {
  const btn = $("q-apply");
  btn.disabled = true;
  try {
    savePrefs();
    const q = await api("apply_quality", quality());
    const mbps = ((q && q.bitrate_kbps) || quality().bitrate_kbps) / 1000;
    toast(IS_MOBILE ? `Kualitas baru (${mbps} Mbps) dipakai saat layar PC dibuka lagi: Buka sesi, lalu Sambung lagi.`
      : "Kualitas diterapkan. Layar PC dibuka ulang sebentar...", 4500);
  } catch (err) { toast(err.message, 5000); }
  setTimeout(() => (btn.disabled = false), IS_MOBILE ? 500 : 8000);
}
$("q-apply").onclick = applyQuality;
// HP: mengubah kualitas selama sesi langsung dikirim (jeda singkat supaya beberapa perubahan beruntun jadi satu kiriman).
let AUTO_APPLY = null;
["q-res", "q-fps", "q-bitrate", "q-codec"].forEach((id) => $(id).addEventListener("change", () => {
  if (!IS_MOBILE || !QUALITY_LIVE) return;
  clearTimeout(AUTO_APPLY);
  AUTO_APPLY = setTimeout(applyQuality, 700);
}));
$("s-quality-btn").onclick = () => {
  setMinimized(true);
  document.querySelector('.nav[data-tab="home"]').click();
  setTimeout(() => $("q-res").closest(".card").scrollIntoView({ block: "start" }), 50);
};
$("s-close").onclick = async () => {
  setMinimized(false);
  $("session-overlay").classList.add("hidden");
  await refresh().catch(() => {});
  if (LAST_SESSION && LAST_SESSION.started_at && !LAST_SESSION.feedback_submitted) openFeedback(LAST_SESSION.id);
};

/* ---------- feedback ---------- */
// Inti 3 pertanyaan (delay, gambar, harga); sisanya opsional di "Tambah detail". Kalau dilewati,
// pengingat muncul di Beranda sampai diisi (sekali "Nanti" = disembunyikan sampai aplikasi dibuka lagi).
let FEEDBACK_SID = null, REMINDER_HIDDEN_FOR = null, FB_PRICE = null;

document.querySelectorAll(".rating").forEach((r) => {
  const box = r.querySelector("div");
  for (let i = 1; i <= 5; i++) {
    const b = document.createElement("button");
    b.type = "button"; b.textContent = i; b.dataset.v = i;
    b.onclick = () => { r.dataset.value = i; box.querySelectorAll("button").forEach((x) => x.classList.toggle("on", Number(x.dataset.v) <= i)); };
    box.appendChild(b);
  }
});
$("fb-price").addEventListener("click", (e) => {
  const v = e.target.dataset.v;
  if (v == null) return;
  FB_PRICE = v;
  $("fb-price").querySelectorAll("button").forEach((b) => b.classList.toggle("on", b === e.target));
  $("fb-price-other").classList.toggle("hidden", v !== "other");
  if (v === "other") $("fb-price-other").focus();
});

function openFeedback(sid) {
  FEEDBACK_SID = sid;
  $("feedback-form").reset();
  FB_PRICE = null;
  document.querySelectorAll(".rating").forEach((r) => { delete r.dataset.value; r.querySelectorAll("button").forEach((b) => b.classList.remove("on")); });
  $("fb-price").querySelectorAll("button").forEach((b) => b.classList.remove("on"));
  $("fb-price-other").classList.add("hidden");
  $("feedback-form").querySelector("details").open = false;
  $("fb-error").textContent = "";
  $("feedback-overlay").classList.remove("hidden");
}
$("fb-skip").onclick = () => { $("feedback-overlay").classList.add("hidden"); refresh().catch(() => {}); };
$("feedback-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const data = Object.fromEntries(new FormData(e.target));
  for (const r of document.querySelectorAll(".rating")) {
    if (r.dataset.value) data[r.dataset.name] = Number(r.dataset.value);
    else if (r.dataset.required) { $("fb-error").textContent = "Beri nilai 1-5 untuk delay dan kualitas gambar dulu."; return; }
  }
  if (FB_PRICE == null) { $("fb-error").textContent = "Pilih kisaran harga yang mau kamu bayar."; return; }
  if (FB_PRICE === "other") {
    const v = Number($("fb-price-other").value);
    if (!$("fb-price-other").value || !(v >= 0)) { $("fb-error").textContent = "Tulis harga per bulan dalam Rupiah."; return; }
    data.fair_price_idr = v;
  } else data.fair_price_idr = Number(FB_PRICE);
  try {
    await api("submit_feedback", FEEDBACK_SID, data);
    $("feedback-overlay").classList.add("hidden");
    toast("Terima kasih atas masukanmu!");
    refresh().catch(() => {});
  } catch (err) { $("fb-error").textContent = err.message; }
});

function renderFeedbackReminder() {
  const p = ME && ME.pending_feedback;
  const show = !!p && REMINDER_HIDDEN_FOR !== p.id;
  $("fb-reminder").classList.toggle("hidden", !show);
  if (!show) return;
  const when = new Date(p.started_at).toLocaleString("id-ID", { weekday: "long", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  $("fb-reminder-text").textContent = `Sesi terakhirmu (${when}, ${fmtDur(p.duration_seconds)}) belum dinilai. Bantu kami dengan 3 pertanyaan singkat.`;
}
$("fb-reminder-open").onclick = () => { if (ME && ME.pending_feedback) openFeedback(ME.pending_feedback.id); };
$("fb-reminder-later").onclick = () => { REMINDER_HIDDEN_FOR = ME && ME.pending_feedback && ME.pending_feedback.id; renderFeedbackReminder(); };

/* ---------- tab & file ---------- */
document.querySelectorAll(".nav").forEach((b) => (b.onclick = () => {
  document.querySelectorAll(".nav").forEach((x) => x.classList.toggle("active", x === b));
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("hidden", t.id !== `tab-${b.dataset.tab}`));
  if (b.dataset.tab === "files") { loadFiles(); pollUploads(); }
  else if (b.dataset.tab === "account") refresh().then(fillAccount).catch(() => fillAccount());
  else refresh().catch(() => {});
}));

/* ---------- akun: profil & password ---------- */
// Pilih provider di <select>; nama di luar daftar ditampilkan sebagai "Lainnya" + isian bebas.
function setIsp(sel, other, wrap, value) {
  const known = ISPS.some(([v]) => v && v === value);
  $(sel).value = known ? value : (value ? "lainnya" : "");
  $(other).value = known ? "" : (value || "");
  $(wrap).classList.toggle("hidden", $(sel).value !== "lainnya");
}

// Profil akun (server) adalah sumber utama kota/provider; isian di Beranda ikut disamakan.
function syncLocationFromAccount() {
  const u = ME && ME.user;
  if (!u) return;
  if (u.city) $("city").value = u.city;
  if (u.isp) setIsp("isp", "isp-other", "isp-other-wrap", u.isp);
}

function fillAccount() {
  const u = ME.user;
  $("acc-email").value = u.email;
  $("acc-name").value = u.name || "";
  $("acc-city").value = u.city || "";
  setIsp("acc-isp", "acc-isp-other", "acc-isp-other-wrap", u.isp || "");
  $("acc-phone").value = u.phone || "";
  $("profile-status").textContent = "";
  $("pw-status").textContent = "";
}

$("profile-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if ($("acc-isp").value === "lainnya" && !$("acc-isp-other").value.trim()) {
    $("profile-status").textContent = "Tulis nama provider internetmu."; return;
  }
  $("profile-save").disabled = true;
  try {
    ME = await api("update_profile", {
      name: $("acc-name").value.trim(), city: $("acc-city").value.trim(),
      isp: ispValue("acc-isp", "acc-isp-other"), phone: $("acc-phone").value.trim(),
    });
    syncLocationFromAccount();
    savePrefs();
    $("profile-status").innerHTML = '<span class="ok-text">Profil tersimpan.</span>';
  } catch (err) { $("profile-status").textContent = err.message; }
  finally { $("profile-save").disabled = false; }
});

$("password-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const pw = $("pw-new").value;
  if (pw !== $("pw-new2").value) { $("pw-status").textContent = "Ulangi password baru dengan sama persis."; return; }
  $("pw-save").disabled = true;
  try {
    await api("change_password", $("pw-old").value, pw);
    ["pw-old", "pw-new", "pw-new2"].forEach((id) => ($(id).value = ""));
    $("pw-status").innerHTML = '<span class="ok-text">Password diganti. Login di perangkat lain sudah dikeluarkan.</span>';
  } catch (err) { $("pw-status").textContent = err.message; }
  finally { $("pw-save").disabled = false; }
});

function fileStatus(f) {
  const pct = f.size_bytes ? Math.floor((f.received_bytes / f.size_bytes) * 100) : 0;
  const ok = f.verified ? `<span class="tag ok" title="Checksum SHA-256 cocok">Terverifikasi</span>` : "";
  if (f.direction === "IN") {
    if (f.status === "UPLOADING") return `Upload belum selesai (${pct}%)`;
    if (f.status === "DELIVERED") return `Sudah di P:\\Inbox${ok}`;
    return (ME && ME.live_session ? "Sedang dikirim ke P:\\Inbox" : "Masuk ke P:\\Inbox saat sesi dimulai") + ok;
  }
  if (f.status === "UPLOADING") return `Sedang dikirim dari PC (${pct}%)`;
  return `Siap diunduh${ok}`;
}

async function loadFiles() {
  if (!PROFILE) return;
  const files = await api("list_files", PROFILE.id);
  $("files-empty").classList.toggle("hidden", files.length > 0);
  $("files-body").innerHTML = files.map((f) => `
    <tr>
      <td>${esc(f.filename)}</td>
      <td>${fmtBytes(f.size_bytes)}</td>
      <td><span class="tag ${f.direction === "IN" ? "in" : "out"}">${f.direction === "IN" ? "Ke PC" : "Dari PC"}</span></td>
      <td>${fileStatus(f)}</td>
      <td>${f.direction === "OUT" && f.status === "DELIVERED" ? `<button class="btn small" data-dl="${f.id}" data-name="${esc(f.filename)}">Unduh</button>` : ""}
          ${(f.direction === "IN" && f.status !== "DELIVERED") || (f.direction === "OUT" && f.status === "DELIVERED")
            ? `<button class="btn small ghost" data-del="${f.id}">Hapus</button>` : ""}</td>
    </tr>`).join("");
}

/* ---------- upload: pratinjau kuota, lalu antrean di background ---------- */
let PREVIEW = null, UPLOAD_POLL = null, LAST_UPLOAD_SIG = "";

$("upload-btn").onclick = async () => {
  if (!PROFILE) return;
  $("upload-status").textContent = "";
  try {
    const p = await api("pick_files", PROFILE.id);
    if (!p) return;
    PREVIEW = p;
    const usedAfter = p.storage_gb * (1 << 30) - p.free_after_bytes;
    $("preview-text").innerHTML = `<b>${p.files.length} file</b> · ${fmtBytes(p.total_bytes)}`;
    setBar("preview-bar", (usedAfter / (p.storage_gb * (1 << 30))) * 100);
    $("preview-detail").textContent = p.fits
      ? `Sisa penyimpanan setelah upload: ${fmtBytes(p.free_after_bytes)} dari ${p.storage_gb} GB. Upload tidak memotong jam main.`
      : `Penyimpanan tidak cukup: kurang ${fmtBytes(-p.free_after_bytes)}. Hapus file di P: atau tambah storage di Beranda.`;
    $("preview-ok").disabled = !p.fits;
    $("upload-preview").classList.remove("hidden");
  } catch (err) { $("upload-status").textContent = err.message; }
};
$("preview-cancel").onclick = () => { PREVIEW = null; $("upload-preview").classList.add("hidden"); };
$("preview-ok").onclick = async () => {
  if (!PREVIEW) return;
  try {
    await api("start_uploads", PROFILE.id, PREVIEW.files.map((f) => f.path));
    PREVIEW = null;
    $("upload-preview").classList.add("hidden");
    pollUploads();
  } catch (err) { $("upload-status").textContent = err.message; }
};

const JOB_TEXT = {
  queued: "Menunggu giliran", hashing: "Menghitung checksum", uploading: "Mengunggah", retrying: "Mencoba lagi",
  paused: "Dijeda", error: "Gagal", verified: "Terverifikasi", done: "Selesai",
};
const fmtEta = (s) => s == null ? "" : s < 60 ? `${Math.ceil(s)} d` : s < 3600 ? `${Math.ceil(s / 60)} m` : `${Math.floor(s / 3600)} j ${Math.ceil((s % 3600) / 60)} m`;

function renderJobs(jobs) {
  $("uploads").classList.toggle("hidden", jobs.length === 0);
  $("uploads-list").innerHTML = jobs.map((j) => {
    const done = j.status === "verified" || j.status === "done";
    const prog = j.status === "hashing" ? j.hashed : j.sent;
    const pct = j.size ? (done ? 100 : (prog / j.size) * 100) : 100;
    let info = `${JOB_TEXT[j.status] || j.status} · ${fmtBytes(prog)} / ${fmtBytes(j.size)}`;
    if (j.status === "uploading" && j.speed_bps) info += ` · ${fmtBytes(j.speed_bps)}/s · sisa ${fmtEta(j.eta_s)}`;
    const btns = [];
    if (["queued", "hashing", "uploading", "retrying"].includes(j.status)) btns.push(`<button class="btn small" data-pause="${j.id}">Jeda</button>`);
    if (["paused", "error"].includes(j.status)) btns.push(`<button class="btn small" data-resume="${j.id}">Lanjutkan</button>`);
    if (!done) btns.push(`<button class="btn small ghost" data-cancel="${j.id}">Batal</button>`);
    return `<div class="job">
      <div class="job-name">${esc(j.name)} ${j.status === "verified" ? `<span class="tag ok">Terverifikasi</span>` : ""}</div>
      <div class="actions">${btns.join("")}</div>
      <div class="bar"><span style="width:${pct.toFixed(1)}%" class="${j.status === "error" ? "danger" : j.status === "paused" ? "warn" : ""}"></span></div>
      <div class="muted small">${info}${j.error ? ` · <span class="err">${esc(j.error)}</span>` : ""}</div>
    </div>`;
  }).join("");
}

async function pollUploads() {
  let jobs;
  try { jobs = await api("uploads"); } catch { return; }
  renderJobs(jobs);
  const active = jobs.some((j) => ["queued", "hashing", "uploading", "retrying"].includes(j.status));
  // Daftar file di server ikut diperbarui saat ada upload yang baru selesai.
  const sig = jobs.filter((j) => j.status === "verified" || j.status === "done").map((j) => j.id).join();
  if (sig !== LAST_UPLOAD_SIG) { LAST_UPLOAD_SIG = sig; loadFiles().catch(() => {}); }
  clearTimeout(UPLOAD_POLL);
  if (active) UPLOAD_POLL = setTimeout(pollUploads, 1000);
}

$("uploads-list").addEventListener("click", async (e) => {
  const d = e.target.dataset;
  try {
    if (d.pause) await api("pause_upload", d.pause);
    else if (d.resume) await api("resume_upload", d.resume);
    else if (d.cancel) await api("cancel_upload", d.cancel);
    else return;
    pollUploads();
  } catch (err) { toast(err.message); }
});
$("uploads-clear").onclick = async () => { await api("clear_finished_uploads").catch(() => {}); pollUploads(); };

$("files-body").addEventListener("click", async (e) => {
  const t = e.target;
  try {
    if (t.dataset.dl) {
      t.disabled = true;
      t.textContent = "Mengunduh...";
      const r = await api("download", t.dataset.dl, t.dataset.name);
      if (r) toast(`Tersimpan${r.verified ? " dan terverifikasi" : ""}: ${r.path}`, 5000);
      loadFiles();
    } else if (t.dataset.del) {
      await api("delete_file", t.dataset.del);
      loadFiles();
    }
  } catch (err) { toast(err.message, 5000); loadFiles(); }
});
