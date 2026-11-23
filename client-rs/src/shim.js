// Menjembatani UI lama (dibuat untuk pywebview: window.pywebview.api.<nama>(...args) -> {ok, data|error}) ke Tauri,
// supaya folder UI yang SAMA dipakai aplikasi Python dan Rust tanpa disalin atau diubah.
(function () {
  const call = (name) => (...args) =>
    window.__TAURI__.core.invoke("call", { name, args }).catch((e) => ({ ok: false, error: String(e) }));
  window.pywebview = { api: new Proxy({}, { get: (_, name) => call(String(name)) }) };
  // app.js mendaftarkan listener "pywebviewready" saat skripnya dimuat; kirim setelah halaman selesai dimuat.
  window.addEventListener("load", () => setTimeout(() => window.dispatchEvent(new Event("pywebviewready")), 0));
})();
