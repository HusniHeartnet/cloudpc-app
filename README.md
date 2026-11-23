# CloudPC App

Aplikasi pemain [CloudPC](https://github.com/HusniHeartnet/cloudpc-stream-rs) (layanan cloud gaming di Indonesia), ditulis
dalam Rust dengan [Tauri](https://tauri.app). Satu kode untuk **Android** dan desktop (Windows, macOS): masuk akun, kuota,
antrean giliran, cek koneksi, lalu membuka layar PC.

*English: the CloudPC player app (Rust + Tauri) for Android and desktop. It talks to the CloudPC API and opens the game
stream using [cloudpc-stream-rs](https://github.com/HusniHeartnet/cloudpc-stream-rs), a GameStream-compatible client.
Licensed GPL-3.0.*

## Isi

| Folder | Fungsi |
|---|---|
| `client-rs` | Aplikasi: jembatan UI (`bridge.rs`), mesin sesi (`session.rs`), antrean, cek jaringan, pembaruan aplikasi, dan untuk Android `android_stream.rs` + proyek Gradle di `gen/android` |
| `client/cloudpc_client/ui` | Antarmuka HTML/CSS/JS (dipakai di semua platform) |
| `common-rs` | Pemeriksaan sertifikat server (pin per sertifikat) |

Layar PC di Android dikerjakan pustaka `libcloudpc_stream.so` dan layar `StreamActivity` dari repo
[cloudpc-stream-rs](https://github.com/HusniHeartnet/cloudpc-stream-rs) (folder `android/`): decode MediaCodec, suara
AAudio, layar sentuh sebagai trackpad, stik layar, bidik gyro. Semua aturan layanan (kuota, antrean, batas sesi) ada di
server, bukan di aplikasi ini.

## Build Android

Butuh Rust stabil + target `aarch64-linux-android`, Android SDK dan NDK, JDK 17, Node (CLI Tauri diambil lewat `npx`),
`cargo-ndk`, dan Windows PowerShell.

```
git clone https://github.com/HusniHeartnet/cloudpc-app
cd cloudpc-app
git clone https://github.com/HusniHeartnet/cloudpc-stream-rs stream-rs
powershell -File client-rs\build-android.ps1 -ApiUrl https://server-kamu.example -NoPin
```

- `-ApiUrl` = alamat server API. Alamat server produksi CloudPC tidak disertakan di repo ini.
- Tanpa `-NoPin`, skrip menanam sertifikat server yang dikunci dari `deploy\cloudpc-server.pem` (sediakan sendiri).
- Kunci tanda tangan: bila ada `%USERPROFILE%\.cloudpc\android\keystore.properties` (`storeFile`, `storePassword`,
  `keyAlias`, `keyPassword`) APK ditandatangani kunci itu; tanpa itu dipakai kunci debug Android bawaan mesinmu. APK
  buatanmu tidak bisa dipasang di atas APK resmi (kunci berbeda); copot dulu yang resmi.

Hasilnya `client-rs\dist\android\CloudPC-Android-<versi>-debug.apk`.

## Build desktop

```
cargo build -p cloudpc-client
cargo test -p cloudpc-client
```

## Lisensi

GPL-3.0-only (lihat `LICENSE`). APK CloudPC memuat aplikasi ini bersama client stream cloudpc-stream-rs (GPL-3.0), jadi
keduanya dibagikan dengan lisensi yang sama. Tauri dan pustaka Rust lain yang dipakai berlisensi MIT/Apache-2.0 (lihat
`Cargo.lock`).

Proyek ini tidak berafiliasi dengan LizardByte (Sunshine), Moonlight Game Streaming Project, maupun NVIDIA.
