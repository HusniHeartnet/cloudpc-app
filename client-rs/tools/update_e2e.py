"""Uji end-to-end pembaruan aplikasi TERPASANG (client-rs): pasang versi A, terbitkan versi B di server lokal, perbarui lewat
aplikasi itu sendiri (cek -> unduh -> pasang senyap -> installer membuka aplikasi baru), lalu periksa hasilnya.

Pakai (dari root repo, Inno Setup 6 terpasang):
    .venv\\Scripts\\python.exe client-rs\\tools\\update_e2e.py

Yang disentuh di komputer ini: folder sementara di %TEMP%, entri "CloudPC (Rust)" di Settings > Apps (dihapus lagi di akhir),
dan jendela aplikasi yang terbuka sebentar saat installer membukanya kembali. Data aplikasi memakai folder sementara lewat
CLOUDPC_APP_DATA, jadi preferensi/login aslimu dan aplikasi Python tidak tersentuh. Build debug dikompilasi dengan versi A dan B,
lalu DIKEMBALIKAN ke build tanpa versi (0.0.0) di akhir supaya tes lain tidak terganggu.
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "server"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import uvicorn  # noqa: E402

from cloudpc_api import db as dbmod  # noqa: E402
from cloudpc_api.config import reset_settings  # noqa: E402
from cloudpc_api.routers.client_update import publish  # noqa: E402
from cdp import WS, page_target  # noqa: E402

VER_A, VER_B = "2026.10.30.1", "2026.10.30.2"
DEBUG_EXE = ROOT / "target" / "debug" / "cloudpc-client.exe"
ISS = ROOT / "client-rs" / "installer" / "CloudPC-Rust.iss"
results: list[tuple[str, bool, str]] = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"  [{'OK' if ok else 'GAGAL'}] {name}" + (f" - {detail}" if detail and not ok else ""))
    return ok


def find_iscc():
    for p in (os.path.expandvars(r"%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe"),
              os.path.expandvars(r"%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"), os.path.expandvars(r"%ProgramFiles%\Inno Setup 6\ISCC.exe")):
        if Path(p).exists():
            return p
    sys.exit("Inno Setup 6 belum terpasang (ISCC.exe)")


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def build_debug(version):
    env = {k: v for k, v in os.environ.items() if k != "CLOUDPC_RS_VERSION"}
    if version:
        env["CLOUDPC_RS_VERSION"] = version
    r = subprocess.run(["cargo", "build", "-p", "cloudpc-client"], cwd=ROOT, env=env, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"cargo build gagal:\n{r.stderr[-800:]}")


def run_headless(exe, env, steps, timeout=180):
    r = subprocess.run([str(exe), "--headless-script", json.dumps(steps)], capture_output=True, text=True, timeout=timeout, env=env)
    return json.loads(r.stdout.strip().splitlines()[-1]) if r.stdout.strip() else [{"ok": False, "error": r.stderr[-300:]}]


def procs_running(exe):
    out = subprocess.run(["powershell", "-NoProfile", "-Command",
                          f"(Get-CimInstance Win32_Process | Where-Object {{ $_.ExecutablePath -eq '{exe}' }} | Measure-Object).Count"],
                         capture_output=True, text=True).stdout.strip()
    return int(out or 0)


def kill_app(exe):
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    f"Get-CimInstance Win32_Process | Where-Object {{ $_.ExecutablePath -eq '{exe}' }} | "
                    "ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }"], capture_output=True)


def wait_for(fn, timeout, step=0.5):
    end = time.time() + timeout
    while time.time() < end:
        if fn():
            return True
        time.sleep(step)
    return False


def main():
    iscc = find_iscc()
    tmp = Path(tempfile.mkdtemp(prefix="cpc-upd-"))
    app_dir, appdata = tmp / "app", tmp / "appdata"
    installed = app_dir / "CloudPCRust.exe"
    print(f"Folder kerja: {tmp}")
    try:
        # 1. Server lokal (hanya kanal pembaruan yang dibutuhkan).
        reset_settings(data_dir=tmp / "data", backup_hour_local=-1)
        dbmod.init_engine()
        from cloudpc_api.main import create_app
        port = free_port()
        server = uvicorn.Server(uvicorn.Config(create_app(start_ticker=False), host="127.0.0.1", port=port, log_level="warning"))
        threading.Thread(target=server.run, daemon=True).start()
        wait_for(lambda: server.started, 10)

        # 2. Dua build berversi A dan B -> dua installer.
        setups = {}
        for ver in (VER_A, VER_B):
            print(f"Membangun versi {ver} ...")
            build_debug(ver)
            stage = tmp / f"stage-{ver}"
            stage.mkdir()
            shutil.copy2(DEBUG_EXE, stage / "CloudPCRust.exe")
            (stage / "client_config.json").write_text(json.dumps({"api_url": f"http://127.0.0.1:{port}"}))
            r = subprocess.run([iscc, "/Q", f"/DAppVersion={ver}", f"/DSourceDir={stage}", f"/DOutDir={tmp}",
                                f"/DOutName=Setup-{ver}", str(ISS)], capture_output=True, text=True)
            if r.returncode != 0:
                sys.exit(f"ISCC gagal:\n{r.stdout[-600:]}{r.stderr[-300:]}")
            setups[ver] = tmp / f"Setup-{ver}.exe"
        build_debug(None)  # kembalikan build debug ke "tanpa versi" (0.0.0) untuk tes lain
        print("Rilis & pemasangan:")
        publish("windows-rs", setups[VER_B], VER_B, notes="Uji E2E")

        # 3. Pasang versi A.
        p = subprocess.run([str(setups[VER_A]), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", f"/DIR={app_dir}"], timeout=300)
        check("installer versi A selesai", p.returncode == 0, f"exit {p.returncode}")
        check("aplikasi A terpasang", installed.exists())
        dbg = free_port()  # aplikasi yang dibuka ulang installer mewarisi variabel ini -> port DevTools terbuka
        env = {**os.environ, "CLOUDPC_APP_DATA": str(appdata), "CLOUDPC_DEBUG_PORT": str(dbg)}

        # 4. Perbarui lewat aplikasi terpasang.
        cfg, chk, dl, waited, inst = run_headless(installed, env, [
            ["config", []], ["check_update", []], ["download_update", []], ["_wait_update", [120]], ["install_update", []]])
        check("aplikasi A melapor versinya", cfg.get("data", {}).get("version") == f"rs-{VER_A}", str(cfg))
        check("cek: ada versi B", chk.get("data", {}).get("update_available") is True
              and chk["data"]["latest"]["version"] == VER_B, str(chk))
        check("unduhan B selesai & checksum cocok", waited.get("data", {}).get("status") == "ready", str(waited)[:200])
        check("install_update diterima (aplikasi terpasang, bukan build dev)", inst.get("ok") is True, str(inst))
        check("pending.json ditulis", (appdata / "updates" / "pending.json").exists())

        # 5. Installer B berjalan senyap, mengganti file, lalu membuka aplikasi baru.
        has_b = lambda: installed.exists() and VER_B.encode() in installed.read_bytes()
        check("file aplikasi diganti menjadi versi B", wait_for(has_b, 120), "exe belum berisi versi B")
        check("installer membuka aplikasi lagi (RELAUNCH)", wait_for(lambda: procs_running(installed) > 0, 60))

        # 6. Jendela aplikasi baru sendiri menampilkan hasil pembaruan (boot() mengambilnya sekali lewat `config`).
        target = page_target(dbg, timeout=60)
        check("jendela aplikasi baru terbuka", target is not None)
        if target:
            ws = WS(target["webSocketDebuggerUrl"], dbg)
            check("jendela menampilkan versi B", wait_for(lambda: ws.eval("document.getElementById('app-version').textContent") == f"rs-{VER_B}", 30),
                  str(ws.eval("document.getElementById('app-version').textContent")))
            seen = []
            wait_for(lambda: seen.append(ws.eval("document.getElementById('toast').textContent")) or f"{VER_B}" in str(seen[-1]), 15, step=0.3)
            check("pemberitahuan di layar: berhasil diperbarui ke versi B",
                  "berhasil diperbarui" in str(seen[-1]) and VER_B in str(seen[-1]), str(seen[-1]) if seen else "tidak ada")
        kill_app(installed)
        time.sleep(2)
        d = run_headless(installed, env, [["config", []]])[0].get("data", {})
        check("hasil pembaruan sudah diambil (hanya sekali)", d.get("update_result") is None, str(d.get("update_result")))
        check("installer lama dibersihkan", not (appdata / "updates" / "CloudPCRustSetup.exe").exists())

        # 7. Uninstall bersih.
        u = subprocess.run([str(app_dir / "unins000.exe"), "/VERYSILENT", "/SUPPRESSMSGBOXES"], timeout=120)
        time.sleep(3)
        check("uninstall bersih", u.returncode == 0 and not installed.exists(), f"exit {u.returncode}")
        server.should_exit = True
    finally:
        kill_app(installed)
        shutil.rmtree(tmp, ignore_errors=True)
    bad = [n for n, ok, _ in results if not ok]
    print(f"\n{len(results) - len(bad)}/{len(results)} pemeriksaan lolos." + (f" GAGAL: {bad}" if bad else ""))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
