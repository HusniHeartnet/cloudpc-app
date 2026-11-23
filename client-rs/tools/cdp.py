"""Klien DevTools (CDP) mini dari pustaka standar, untuk memeriksa UI aplikasi di jendela WebView2 (tanpa Node/Selenium).

Aplikasi build DEBUG membuka port DevTools hanya bila CLOUDPC_DEBUG_PORT diisi. Dipakai gui_smoke.py dan update_e2e.py.
"""
import base64
import json
import os
import socket
import struct
import time
import urllib.request


def page_target(port, timeout=40):
    """Tunggu halaman aplikasi muncul; kembalikan entri target (url, webSocketDebuggerUrl) atau None."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            pages = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json", timeout=2))
            target = next((p for p in pages if p.get("type") == "page"), None)
            if target:
                return target
        except Exception:
            pass
        time.sleep(0.5)
    return None


class WS:
    def __init__(self, ws_url, port):
        path = ws_url.split(f":{port}", 1)[1]
        self.s = socket.create_connection(("127.0.0.1", port), timeout=15)
        key = base64.b64encode(os.urandom(16)).decode()
        self.s.sendall((f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                        f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.s.recv(1)
        assert b" 101 " in buf.split(b"\r\n")[0], buf
        self.n = 0

    def send(self, obj):
        data = json.dumps(obj).encode()
        hdr = bytes([0x81])
        ln = len(data)
        hdr += bytes([0x80 | ln]) if ln < 126 else bytes([0x80 | 126]) + struct.pack(">H", ln) if ln < 65536 else bytes([0x80 | 127]) + struct.pack(">Q", ln)
        mask = os.urandom(4)
        self.s.sendall(hdr + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def recv_exact(self, n):
        out = b""
        while len(out) < n:
            chunk = self.s.recv(n - len(out))
            if not chunk:
                raise EOFError
            out += chunk
        return out

    def recv(self):
        msg = b""
        while True:
            b0, b1 = self.recv_exact(2)
            ln = b1 & 0x7F
            if ln == 126:
                ln = struct.unpack(">H", self.recv_exact(2))[0]
            elif ln == 127:
                ln = struct.unpack(">Q", self.recv_exact(8))[0]
            msg += self.recv_exact(ln)
            if b0 & 0x80:
                return json.loads(msg)

    def eval(self, expr):
        """Jalankan JavaScript di halaman (menunggu Promise) dan kembalikan nilainya."""
        self.n += 1
        self.send({"id": self.n, "method": "Runtime.evaluate",
                   "params": {"expression": expr, "awaitPromise": True, "returnByValue": True}})
        while True:
            m = self.recv()
            if m.get("id") == self.n:
                r = m["result"]
                if "exceptionDetails" in r:
                    return {"EXCEPTION": r["exceptionDetails"].get("exception", {}).get("description", str(r["exceptionDetails"]))}
                return r["result"].get("value")
