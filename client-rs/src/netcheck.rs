//! Ukur kualitas jaringan tanpa ping (ping Windows berbahasa lokal sulit dibaca): waktu handshake TCP, yang mendekati
//! RTT jaringan. Port dari client/cloudpc_client/netcheck.py, dengan bentuk keluaran JSON yang sama.
use std::net::{TcpStream, ToSocketAddrs};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 }
}

pub fn tcp_probe(host: &str, port: u16, count: usize, timeout: Duration, gap: Duration) -> Value {
    let addr = (host, port).to_socket_addrs().ok().and_then(|mut a| a.next());
    let mut samples: Vec<f64> = vec![];
    let mut failed = 0;
    for _ in 0..count {
        let t0 = Instant::now();
        match addr.and_then(|a| TcpStream::connect_timeout(&a, timeout).ok()) {
            Some(_) => samples.push(t0.elapsed().as_secs_f64() * 1000.0),
            None => failed += 1,
        }
        thread::sleep(gap);
    }
    if samples.is_empty() {
        return json!({"rtt_ms": null, "jitter_ms": null, "loss_pct": 100.0, "samples": 0});
    }
    let jitter = if samples.len() > 1 {
        samples.windows(2).map(|w| (w[0] - w[1]).abs()).sum::<f64>() / (samples.len() - 1) as f64
    } else {
        0.0
    };
    let mut sorted = samples.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({"rtt_ms": round1(median(&sorted)), "jitter_ms": round1(jitter),
           "loss_pct": round1(100.0 * failed as f64 / count as f64), "samples": samples.len()})
}

const BURST_PACKET_SIZE: usize = 1200;
/// Paket hilang yang masih nyaman untuk stream (di atasnya video patah-patah walau FEC menolong).
pub const BURST_CLEAN_LOSS_PCT: f64 = 1.0;

/// Satu semburan UDP dari server (`server-rs/src/netprobe.rs`) ke aplikasi dengan laju `kbps` selama ~`millis`. Mengukur
/// paket hilang arah server -> pemain yang sebenarnya, hal yang tidak terlihat oleh ping/TCP kecil. Mengembalikan
/// `{"ok": false, "error": ...}` bila server tidak menjawab.
pub fn udp_burst(host: &str, port: u16, ticket: &str, kbps: u64, millis: u64) -> Value {
    use std::net::UdpSocket;
    let count = ((kbps * 1000 / 8) * millis / 1000 / BURST_PACKET_SIZE as u64).max(20) as usize;
    let Some(addr) = (host, port).to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return json!({"ok": false, "error": "alamat server tidak dikenal"});
    };
    let bind = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let Ok(sock) = UdpSocket::bind(bind) else { return json!({"ok": false, "error": "soket UDP tidak bisa dibuka"}) };
    if sock.connect(addr).is_err() {
        return json!({"ok": false, "error": "soket UDP tidak bisa dibuka"});
    }
    let request = format!("CPNP1 {ticket} {count} {BURST_PACKET_SIZE} {kbps}");
    let mut buf = [0u8; 2048];
    let mut seen = vec![false; count];
    let (mut received, mut ooo, mut highest) = (0usize, 0usize, -1i64);
    let mut last: Option<Instant> = None;
    let mut max_gap = 0.0f64;
    // Permintaan UDP bisa hilang: ulangi sampai tiga kali sebelum menyerah.
    let mut started = false;
    for _ in 0..3 {
        let _ = sock.send(request.as_bytes());
        let _ = sock.set_read_timeout(Some(Duration::from_millis(600)));
        if let Ok(n) = sock.recv(&mut buf) {
            if n >= 12 && &buf[..4] == b"CPNP" {
                started = true;
                handle_packet(&buf[..n], &mut seen, &mut received, &mut ooo, &mut highest, &mut last, &mut max_gap);
                break;
            }
        }
    }
    if !started {
        return json!({"ok": false, "error": "server tidak menjawab tes UDP"});
    }
    let deadline = Instant::now() + Duration::from_millis(millis + 2500);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(700)));
    while received < count && Instant::now() < deadline {
        match sock.recv(&mut buf) {
            Ok(n) if n >= 12 && &buf[..4] == b"CPNP" => handle_packet(&buf[..n], &mut seen, &mut received, &mut ooo, &mut highest, &mut last, &mut max_gap),
            Ok(_) => {}
            Err(_) => break, // 700 ms tanpa paket: semburan sudah selesai (sisanya hilang)
        }
    }
    // Rentetan paket hilang terpanjang (hilang beruntun terasa lebih parah daripada hilang tersebar).
    let (mut run, mut longest) = (0usize, 0usize);
    for s in &seen {
        run = if *s { 0 } else { run + 1 };
        longest = longest.max(run);
    }
    json!({"ok": true, "kbps": kbps, "expected": count, "received": received,
           "loss_pct": round1(100.0 * (count - received) as f64 / count as f64),
           "out_of_order": ooo, "max_gap_ms": round1(max_gap), "longest_loss_run": longest})
}

fn handle_packet(p: &[u8], seen: &mut [bool], received: &mut usize, ooo: &mut usize, highest: &mut i64, last: &mut Option<Instant>, max_gap: &mut f64) {
    let seq = u32::from_be_bytes([p[4], p[5], p[6], p[7]]) as usize;
    if seq >= seen.len() || seen[seq] {
        return; // di luar rentang atau dobel
    }
    seen[seq] = true;
    *received += 1;
    if (seq as i64) < *highest {
        *ooo += 1;
    }
    *highest = (*highest).max(seq as i64);
    let now = Instant::now();
    if let Some(t) = *last {
        *max_gap = max_gap.max(now.duration_since(t).as_secs_f64() * 1000.0);
    }
    *last = Some(now);
}

/// Dari hasil tiap langkah tangga bitrate (tertinggi dulu): bitrate tertinggi yang bersih, atau yang terendah bila tak ada.
/// Mengembalikan (Mbps rekomendasi, bersih?).
pub fn recommend(steps: &[Value]) -> Option<(f64, bool)> {
    let ok: Vec<&Value> = steps.iter().filter(|s| s["ok"] == true).collect();
    let mbps = |s: &Value| s["kbps"].as_f64().unwrap_or(0.0) / 1000.0;
    if let Some(s) = ok.iter().find(|s| s["loss_pct"].as_f64().unwrap_or(100.0) <= BURST_CLEAN_LOSS_PCT) {
        return Some((mbps(s), true));
    }
    ok.last().map(|s| (mbps(s), false))
}

/// Penilaian sederhana untuk ditampilkan ke user: (verdict, saran).
pub fn verdict(r: &Value) -> (&'static str, &'static str) {
    let Some(rtt) = r["rtt_ms"].as_f64() else {
        return ("buruk", "Server tidak bisa dijangkau dari jaringanmu.");
    };
    let jit = r["jitter_ms"].as_f64().unwrap_or(0.0);
    let loss = r["loss_pct"].as_f64().unwrap_or(0.0);
    if loss >= 5.0 || rtt > 80.0 || jit > 20.0 {
        ("buruk", "Koneksi kurang stabil. Coba pakai kabel LAN atau pindah jaringan.")
    } else if loss > 0.0 || rtt > 40.0 || jit > 8.0 {
        ("cukup", "Bisa dipakai, tapi mungkin terasa delay di game cepat.")
    } else {
        ("bagus", "Koneksi bagus untuk gaming dan editing.")
    }
}

fn put_verdict(r: &mut Value) {
    let (v, a) = verdict(r);
    r["verdict"] = json!(v);
    r["advice"] = json!(a);
}

/// Ukur ke satu alamat (server API, untuk aplikasi tanpa daftar jalur).
pub fn probe_host(host: &str, port: u16) -> Value {
    let mut r = tcp_probe(host, port, 10, Duration::from_millis(1500), Duration::from_millis(150));
    put_verdict(&mut r);
    r
}

/// Port dasar bawaan host stream (HTTP: /serverinfo dan pairing). Port lain dihitung dari sini (HTTPS = dasar - 5).
pub const DEFAULT_HTTP_PORT: u16 = 47989;

/// Alamat jalur boleh membawa port dasar host (`1.2.3.4:48189`, `[2001:db8::1]:48189`), supaya beberapa PC bisa berbagi
/// satu IP relay. Tanpa port = bawaan 47989. IPv6 tanpa kurung dianggap tanpa port. Mengembalikan (host, port HTTP).
pub fn split_address(address: &str) -> (String, u16) {
    let a = address.trim();
    if let Some(rest) = a.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).filter(|p| *p != 0).unwrap_or(DEFAULT_HTTP_PORT);
            return (host.to_string(), port);
        }
    }
    if a.matches(':').count() == 1 {
        if let Some((host, port)) = a.split_once(':') {
            if let Some(port) = port.parse::<u16>().ok().filter(|p| *p != 0) {
                return (host.to_string(), port);
            }
        }
    }
    (a.to_string(), DEFAULT_HTTP_PORT)
}

/// Port HTTPS host (dipakai mengukur RTT) dari port HTTP-nya.
pub fn https_port(http_port: u16) -> u16 {
    http_port.saturating_sub(5).max(1)
}

/// Ukur waktu handshake TCP ke port HTTPS host pada alamat jalur (dengan atau tanpa port).
pub fn probe_stream_port(address: &str, count: usize, timeout: Duration, gap: Duration) -> Value {
    let (host, http) = split_address(address);
    tcp_probe(&host, https_port(http), count, timeout, gap)
}

/// Sama, tetapi ke port HTTP (pairing).
pub fn probe_pair_port(address: &str, count: usize, timeout: Duration, gap: Duration) -> Value {
    let (host, http) = split_address(address);
    tcp_probe(&host, http, count, timeout, gap)
}

pub fn parse_uniqueid(xml: &str) -> Option<String> {
    let id = xml.split("<uniqueid>").nth(1)?.split("</uniqueid>").next()?.trim();
    (!id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| id.to_string())
}

/// Alamat LAN (mis. 192.168.0.10) ada di jutaan rumah: pastikan yang menjawab benar-benar Sunshine PC ini lewat `uniqueid`
/// di /serverinfo (HTTP tanpa login). Pakai soket polos supaya tidak menambah dependensi.
pub fn is_same_host(address: &str, port: u16, uniqueid: &str) -> bool {
    use std::io::{Read, Write};
    let Some(addr) = (address, port).to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(1500)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(1500)));
    if s.write_all(format!("GET /serverinfo HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.take(65536).read_to_end(&mut buf);
    parse_uniqueid(&String::from_utf8_lossy(&buf)).map(|g| g.eq_ignore_ascii_case(uniqueid)).unwrap_or(false) && !uniqueid.is_empty()
}

/// Ukur semua jalur koneksi bersamaan (masing-masing beberapa detik) ke port Sunshine di PC host.
/// Jalur `kind == "local"` hanya kandidat: bila bukan PC yang sama (atau tidak terjangkau) ditandai `hidden` dan
/// dibuang oleh `visible` supaya tidak ditampilkan ke pemain.
pub fn probe_routes(routes: &[Value], count: usize) -> Vec<Value> {
    probe_routes_with(routes, None, count)
}

/// `ports` = (port ukur, port HTTP) paksa untuk tes; `None` = dari alamat tiap jalur.
fn probe_routes_with(routes: &[Value], ports: Option<(u16, u16)>, count: usize) -> Vec<Value> {
    let handles: Vec<_> = routes
        .iter()
        .cloned()
        .map(|route| {
            thread::spawn(move || {
                let mut res = route.clone();
                let (host, http) = split_address(route["address"].as_str().unwrap_or(""));
                let (port, http_port) = ports.unwrap_or((https_port(http), http));
                let address = host.as_str();
                let local = route["kind"] == "local";
                if local && !is_same_host(address, http_port, route["uniqueid"].as_str().unwrap_or("")) {
                    res["rtt_ms"] = Value::Null;
                    res["hidden"] = json!(true);
                    return res;
                }
                let probe = tcp_probe(address, port, count, Duration::from_millis(1500), Duration::from_millis(150));
                if let (Some(dst), Some(src)) = (res.as_object_mut(), probe.as_object()) {
                    dst.extend(src.clone().into_iter().collect::<Map<String, Value>>());
                }
                if local && res["rtt_ms"].is_null() {
                    res["hidden"] = json!(true);
                }
                put_verdict(&mut res);
                res
            })
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

pub fn visible(results: Vec<Value>) -> Vec<Value> {
    results.into_iter().filter(|r| r["hidden"] != true).collect()
}

/// Jalur terbaik untuk mode Otomatis: latensi + ketidakstabilan; jalur yang hampir tidak terjangkau dilewati.
/// Relay dimenangkan bila selisihnya kecil, karena jalurnya paling teruji.
pub fn best_route(results: &[Value]) -> Option<String> {
    let score = |r: &Value| {
        r["rtt_ms"].as_f64().unwrap_or(f64::MAX) + 2.0 * r["jitter_ms"].as_f64().unwrap_or(0.0)
            + 5.0 * r["loss_pct"].as_f64().unwrap_or(0.0) + if r["kind"] == "relay" { 0.0 } else { 3.0 }
    };
    results
        .iter()
        .filter(|r| r["rtt_ms"].is_number() && r["loss_pct"].as_f64().unwrap_or(0.0) < 20.0)
        .min_by(|a, b| score(a).partial_cmp(&score(b)).unwrap())
        .and_then(|r| r["id"].as_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn verdict_thresholds_match_python() {
        assert_eq!(verdict(&json!({"rtt_ms": 20.0, "jitter_ms": 1.0, "loss_pct": 0.0})).0, "bagus");
        assert_eq!(verdict(&json!({"rtt_ms": 41.0, "jitter_ms": 1.0, "loss_pct": 0.0})).0, "cukup");
        assert_eq!(verdict(&json!({"rtt_ms": 20.0, "jitter_ms": 9.0, "loss_pct": 0.0})).0, "cukup");
        assert_eq!(verdict(&json!({"rtt_ms": 20.0, "jitter_ms": 1.0, "loss_pct": 5.0})).0, "buruk");
        assert_eq!(verdict(&json!({"rtt_ms": 81.0, "jitter_ms": 1.0, "loss_pct": 0.0})).0, "buruk");
        assert_eq!(verdict(&json!({"rtt_ms": null})).0, "buruk");
    }

    #[test]
    fn median_of_even_and_odd() {
        assert_eq!(median(&[1.0, 2.0, 9.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 10.0]), 2.5);
    }

    #[test]
    fn probes_open_and_closed_ports() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let ok = tcp_probe("127.0.0.1", port, 4, Duration::from_millis(500), Duration::from_millis(5));
        assert_eq!(ok["samples"], 4);
        assert_eq!(ok["loss_pct"], 0.0);
        drop(l);
        let closed = tcp_probe("127.0.0.1", port, 3, Duration::from_millis(300), Duration::from_millis(5));
        assert_eq!(closed["samples"], 0);
        assert_eq!(closed["loss_pct"], 100.0);
        assert!(closed["rtt_ms"].is_null());
    }

    /// Server semburan tiruan: membalas permintaan `CPNP1` dengan paket bernomor, membuang yang `seq % drop_every == 1`.
    fn fake_burst_server(drop_every: u32) -> u16 {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = s.local_addr().unwrap().port();
        thread::spawn(move || {
            let mut buf = [0u8; 256];
            while let Ok((n, peer)) = s.recv_from(&mut buf) {
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                let f: Vec<&str> = text.split_whitespace().collect();
                let (count, size): (u32, usize) = (f[2].parse().unwrap(), f[3].parse().unwrap());
                for seq in 0..count {
                    if drop_every > 0 && seq % drop_every == 1 {
                        continue;
                    }
                    let mut p = vec![0u8; size];
                    p[..4].copy_from_slice(b"CPNP");
                    p[4..8].copy_from_slice(&seq.to_be_bytes());
                    p[8..12].copy_from_slice(&count.to_be_bytes());
                    let _ = s.send_to(&p, peer);
                    if seq % 20 == 19 {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        });
        port
    }

    #[test]
    fn udp_burst_counts_loss_and_reports_unanswered_server() {
        let clean = udp_burst("127.0.0.1", fake_burst_server(0), &"a".repeat(32), 4000, 500);
        assert_eq!(clean["ok"], true);
        assert_eq!(clean["loss_pct"], 0.0);
        assert_eq!(clean["expected"], clean["received"]);
        let lossy = udp_burst("127.0.0.1", fake_burst_server(10), &"a".repeat(32), 4000, 500);
        let loss = lossy["loss_pct"].as_f64().unwrap();
        assert!((9.0..=11.0).contains(&loss), "loss {loss}");
        // Port tanpa server: tidak ada jawaban sama sekali.
        let closed = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let none = udp_burst("127.0.0.1", closed, &"a".repeat(32), 4000, 300);
        assert_eq!(none["ok"], false);
    }

    #[test]
    fn recommend_picks_highest_clean_step_or_lowest() {
        let step = |mbps: f64, loss: f64| json!({"ok": true, "kbps": mbps * 1000.0, "loss_pct": loss});
        assert_eq!(recommend(&[step(20.0, 6.0), step(12.0, 0.4), step(8.0, 0.0)]), Some((12.0, true)));
        assert_eq!(recommend(&[step(20.0, 6.0), step(12.0, 3.0), step(8.0, 2.0)]), Some((8.0, false)));
        assert_eq!(recommend(&[json!({"ok": false})]), None);
    }

    /// Sunshine tiruan: menjawab /serverinfo dengan uniqueid tertentu pada satu port.
    fn fake_sunshine(uid: &'static str) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            use std::io::{Read, Write};
            for s in l.incoming().flatten() {
                let mut s = s;
                let mut b = [0u8; 512];
                let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = s.read(&mut b);
                let body = format!("<root><uniqueid>{uid}</uniqueid></root>");
                let _ = s.write_all(format!("HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
            }
        });
        port
    }

    #[test]
    fn local_route_shows_only_when_same_sunshine_answers() {
        let p = fake_sunshine("AAAA-1111");
        let relay = json!({"id": "relay", "kind": "relay", "label": "Jakarta", "address": "127.0.0.1"});
        let local = |uid: &str| json!({"id": "local", "kind": "local", "label": "Jaringan lokal", "address": "127.0.0.1", "uniqueid": uid});
        let seen = |routes: Vec<Value>, http: u16| visible(probe_routes_with(&routes, Some((p, http)), 2)).iter().map(|r| r["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(seen(vec![relay.clone(), local("aaaa-1111")], p), vec!["relay", "local"]); // uniqueid cocok (huruf besar/kecil bebas)
        assert_eq!(seen(vec![relay.clone(), local("BEDA-2222")], p), vec!["relay"]); // perangkat lain di alamat yang sama
        let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // port tertutup
        assert_eq!(seen(vec![relay, local("AAAA-1111")], dead), vec!["relay"]); // tidak terjangkau = tidak ditampilkan
    }

    #[test]
    fn address_may_carry_the_host_base_port() {
        assert_eq!(split_address("203.0.113.10"), ("203.0.113.10".into(), 47989));
        assert_eq!(split_address("203.0.113.10:48189"), ("203.0.113.10".into(), 48189));
        assert_eq!(split_address("pc.example:48189"), ("pc.example".into(), 48189));
        assert_eq!(split_address("[2001:db8::1]:48189"), ("2001:db8::1".into(), 48189));
        assert_eq!(split_address("[2001:db8::1]"), ("2001:db8::1".into(), 47989));
        assert_eq!(split_address("2001:db8::1"), ("2001:db8::1".into(), 47989)); // IPv6 tanpa kurung = tanpa port
        assert_eq!(split_address("203.0.113.10:abc"), ("203.0.113.10:abc".into(), 47989));
        assert_eq!((https_port(47989), https_port(48189)), (47984, 48184));
    }

    /// Dua PC di satu IP relay: tiap jalur diukur ke port PC-nya sendiri, dan jalur Lokal dicek ke port HTTP PC itu.
    #[test]
    fn routes_on_one_address_are_probed_on_their_own_ports() {
        // "HTTPS" = port HTTP - 5: cari sepasang port bebas berjarak 5.
        let (https, http) = loop {
            let a = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = a.local_addr().unwrap().port();
            if port > 60000 { continue; }
            if let Ok(b) = TcpListener::bind(("127.0.0.1", port + 5)) {
                break (a, b);
            }
        };
        let http_port = http.local_addr().unwrap().port();
        let _keep = https;
        // Port mati: port itu DAN port - 5 (yang ikut diukur) sama-sama bebas. macOS membagi port berurutan, jadi
        // port - 5 bisa jatuh ke pendengar uji di atas (gagal di CI Mac 26 Nov 2026).
        let dead = loop {
            let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            if port > 1029 && TcpListener::bind(("127.0.0.1", port - 5)).is_ok() {
                break port;
            }
        };
        let routes = vec![
            json!({"id": "pc2", "kind": "relay", "address": format!("127.0.0.1:{http_port}")}),
            json!({"id": "mati", "kind": "relay", "address": format!("127.0.0.1:{dead}")}),
        ];
        let res = probe_routes(&routes, 2);
        let rtt = |id: &str| res.iter().find(|r| r["id"] == id).unwrap()["rtt_ms"].clone();
        assert!(rtt("pc2").is_number());
        assert!(rtt("mati").is_null());
        assert!(probe_pair_port(&format!("127.0.0.1:{http_port}"), 1, Duration::from_millis(500), Duration::from_millis(1))["rtt_ms"].is_number());
        assert!(probe_stream_port(&format!("127.0.0.1:{http_port}"), 1, Duration::from_millis(500), Duration::from_millis(1))["rtt_ms"].is_number());
    }

    #[test]
    fn best_route_prefers_relay_on_small_difference_and_skips_dead_routes() {
        let rs = vec![
            json!({"id": "direct", "kind": "direct", "rtt_ms": 20.0, "jitter_ms": 0.0, "loss_pct": 0.0}),
            json!({"id": "relay", "kind": "relay", "rtt_ms": 22.0, "jitter_ms": 0.0, "loss_pct": 0.0}),
            json!({"id": "mati", "kind": "relay", "rtt_ms": 5.0, "jitter_ms": 0.0, "loss_pct": 40.0}),
        ];
        assert_eq!(best_route(&rs).as_deref(), Some("relay")); // 22 < 20 + 3
        assert_eq!(best_route(&[json!({"id": "x", "rtt_ms": null, "loss_pct": 100.0})]), None);
    }
}
