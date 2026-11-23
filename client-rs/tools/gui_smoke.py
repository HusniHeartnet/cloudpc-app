"""Uji asap jendela Tauri: buka aplikasi, tunggu UI lama selesai boot lewat jembatan, periksa lewat DevTools (CDP).

Pakai: cargo build -p cloudpc-client; python client-rs/tools/gui_smoke.py
Membuka jendela sebentar (config dan folder data sementara; tidak menyentuh preferensimu atau server).
"""
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cdp import WS, page_target  # noqa: E402

EXE = Path(__file__).resolve().parents[2] / "target" / "debug" / "cloudpc-client.exe"
PORT = 9333
tmp = Path(tempfile.mkdtemp(prefix="cpc-gui-"))
(tmp / "client_config.json").write_text(json.dumps({"api_url": "http://127.0.0.1:9", "brand_name": "CloudPC"}))
env = {**os.environ, "CLOUDPC_APP_DATA": str(tmp / "appdata"), "CLOUDPC_CLIENT_CONFIG": str(tmp / "client_config.json"),
       "CLOUDPC_DEBUG_PORT": str(PORT)}
proc = subprocess.Popen([str(EXE)], env=env)
try:
    target = page_target(PORT)
    assert target, "jendela/halaman tidak muncul dalam 40 detik"
    print("halaman  :", target["url"])
    time.sleep(3)  # beri waktu boot() menyelesaikan panggilan jembatan
    ws = WS(target["webSocketDebuggerUrl"], PORT)
    checks = {
        "judul dokumen": "document.title",
        "jembatan pywebview": "typeof window.pywebview.api.config",
        "tauri global": "typeof window.__TAURI__.core.invoke",
        "versi tampil (boot() lewat jembatan)": "document.getElementById('app-version').textContent",
        "layar login ada": "!!document.getElementById('login-email')",
        "config via jembatan": "window.pywebview.api.config().then(r => JSON.stringify(r))",
        "login ke server mati": "window.pywebview.api.login('x','y',false).then(r => JSON.stringify(r))",
        "pembaruan belum diunduh": "window.pywebview.api.install_update().then(r => JSON.stringify(r))",
    }
    for label, expr in checks.items():
        print(f"{label:38}:", ws.eval(expr))
finally:
    proc.kill()
    proc.wait()
