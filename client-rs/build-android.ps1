# Build aplikasi CloudPC untuk Android (APK), termasuk layar PC (client stream-rs untuk Android). VERSI PERCOBAAN.
#
#   client-rs\build-android.ps1                    # APK debug arm64 (HP sungguhan), menunjuk server produksi
#   client-rs\build-android.ps1 -Target x86_64     # untuk emulator
#   client-rs\build-android.ps1 -ApiUrl http://10.0.2.2:8000 -NoPin   # server lokal dari emulator
#
# Butuh: Android SDK (ANDROID_HOME), NDK (ANDROID_NDK_HOME atau NDK_HOME), JDK 17, Node (CLI Tauri diambil lewat npx),
# cargo-ndk. Config dan sertifikat server yang dikunci DITANAM ke aplikasi saat build (build.rs: CLOUDPC_MOBILE_CONFIG /
# _CA), karena di HP tidak ada client_config.json di samping program.
param([string]$Version = '', [string]$ApiUrl = 'https://server.example', [string]$Target = 'aarch64', [switch]$NoPin)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$root = Split-Path $here -Parent

if (-not $env:NDK_HOME) { $env:NDK_HOME = $env:ANDROID_NDK_HOME }
if (-not $env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME = $env:NDK_HOME }
foreach ($v in 'ANDROID_HOME', 'NDK_HOME', 'JAVA_HOME') {
    if (-not (Get-Item "env:$v" -ErrorAction SilentlyContinue).Value) { throw "Variabel $v belum diisi." }
}
if (-not $Version) { $d = Get-Date; $Version = '{0}.{1}.{2}.{3}' -f $d.Year, $d.Month, $d.Day, ($d.Hour * 60 + $d.Minute) }
$env:CLOUDPC_RS_VERSION = $Version

# Nomor versi Android (versionCode) harus NAIK tiap rilis, kalau tidak Android menolak memasang pembaruan. Diturunkan
# dari versi "TAHUN.BULAN.HARI.N" supaya urutannya sama dengan urutan versi di server: hari ke-berapa x 1440 + N.
$v = $Version.Split('.') | ForEach-Object { [int]$_ }
if ($v.Count -ne 4 -or $v[3] -gt 1439) { throw "Versi harus berbentuk TAHUN.BULAN.HARI.N dengan N 0..1439 (mis. 2026.11.23.615): $Version" }
$env:CLOUDPC_ANDROID_VERSION_CODE = ((($v[0] - 2026) * 372 + ($v[1] - 1) * 31 + $v[2]) * 1440 + $v[3]).ToString()

# Kunci tanda tangan TETAP (di luar repo). Semua APK yang dibagikan harus ditandatangani kunci yang sama; kalau kunci ini
# hilang, pemain harus mencopot aplikasi lalu memasang ulang. CADANGKAN folder ini.
$keystore = Join-Path $env:USERPROFILE '.cloudpc\android\keystore.properties'
if (Test-Path $keystore) { $env:CLOUDPC_ANDROID_KEYSTORE_PROPS = $keystore }
else { $env:CLOUDPC_ANDROID_KEYSTORE_PROPS = ''; Write-Warning "Tidak ada ${keystore}; APK ditandatangani kunci debug laptop ini (jangan dibagikan)." }

$map = @{ aarch64 = 'aarch64-linux-android', 'arm64-v8a', 'Arm64'; armv7 = 'armv7-linux-androideabi', 'armeabi-v7a', 'Arm'
          i686 = 'i686-linux-android', 'x86', 'X86'; x86_64 = 'x86_64-linux-android', 'x86_64', 'X86_64' }[$Target]
if (-not $map) { throw "Target tidak dikenal: $Target" }
$triple, $abi, $arch = $map
$jni = Join-Path $here "gen\android\app\src\main\jniLibs\$abi"
New-Item -ItemType Directory -Force $jni | Out-Null
$log = Join-Path $root 'target\android-build.log'
New-Item -ItemType Directory -Force (Split-Path $log -Parent) | Out-Null

$cfgDir = Join-Path $root 'target\android-config'
New-Item -ItemType Directory -Force $cfgDir | Out-Null
$cfgFile = Join-Path $cfgDir 'mobile_config.json'
$json = [ordered]@{ api_url = $ApiUrl; brand_name = 'CloudPC'; moonlight_app = 'Desktop' } | ConvertTo-Json
[IO.File]::WriteAllText($cfgFile, $json, (New-Object Text.UTF8Encoding $false))   # tanpa BOM
$env:CLOUDPC_MOBILE_CONFIG = $cfgFile
if ($NoPin) { $env:CLOUDPC_MOBILE_CA = '' } else { $env:CLOUDPC_MOBILE_CA = Join-Path $root 'deploy\cloudpc-server.pem' }

# Ikon aplikasi = logo CloudPC (bukan ikon bawaan Tauri). Butuh Pillow di .venv proyek; tanpa itu ikon lama dipakai.
$python = Join-Path $root '.venv\Scripts\python.exe'
if (Test-Path $python) {
    & $python (Join-Path $here 'tools\android_icons.py')
    if ($LASTEXITCODE -ne 0) { throw 'Pembuatan ikon Android gagal.' }
} else { Write-Warning "Tidak ada ${python}; ikon aplikasi tidak diperbarui." }

# Layar PC: pustaka Rust client stream (workspace stream-rs, GPL-3.0, TERPISAH dari aplikasi ini). Aplikasi tidak
# menautkannya; ia dimuat komponen Android id.cloudpc.stream (sumber Kotlin di stream-rs\android\kotlin, dikompilasi
# Gradle dari sana). Perintah lewat cmd dengan 2>&1 ke file: cargo/CLI menulis kemajuan ke stderr, dan PowerShell 5.1
# menganggap stderr program yang dialihkan sebagai galat.
"Membangun pustaka layar PC (stream-rs) untuk Android ($Target); log: $log"
Push-Location (Join-Path $root 'stream-rs')
try {
    & cmd /c "cargo ndk -t $abi -P 26 build -p cloudpc-stream-android --release > `"$log`" 2>&1"
    if ($LASTEXITCODE -ne 0) { Get-Content $log -Tail 40; throw "Build pustaka layar PC gagal (kode $LASTEXITCODE)." }
} finally { Pop-Location }
Copy-Item (Join-Path $root "stream-rs\target\$triple\release\libcloudpc_stream.so") $jni -Force

Push-Location $here
try {
    "Membangun aplikasi (Rust + Gradle)..."
    & cmd /c "npx --yes @tauri-apps/cli@2 android build --debug --apk --target $Target >> `"$log`" 2>&1"
    if ($LASTEXITCODE -ne 0) {
        # Dua kegagalan CLI yang sudah dikenal SETELAH pustaka Rust terbangun, dan keduanya ditangani di bawah dengan cara
        # yang sama (salin pustaka + jalankan Gradle sendiri):
        #  - symlink ke jniLibs ditolak Windows (tanpa Developer Mode);
        #  - symlink berhasil, tetapi tugas Gradle "rustBuild" memanggil `npm run tauri` dan proyek ini tidak punya
        #    package.json (CLI diambil lewat npx).
        # (Baris "Finished `dev` profile" di log berisi kode warna di tengahnya, jadi yang dicocokkan hanya ekornya.)
        if ((Get-Content $log -Raw) -notmatch 'symbolic link|`dev` profile \[') {
            Get-Content $log -Tail 40
            throw "Build Android gagal (kode $LASTEXITCODE)."
        }
        # CLI Tauri menautkan (symlink) pustaka Rust ke jniLibs, dan Windows hanya mengizinkan symlink bila Developer Mode
        # menyala. Tanpa itu: pustaka yang SUDAH terbangun disalin (simbol debug dibuang, 175 MB -> belasan MB) lalu Gradle
        # dijalankan langsung tanpa langkah build Rust-nya. Developer Mode = setelan Windows, keputusan pemilik.
        $so = Join-Path $jni 'libcloudpc_client_lib.so'
        # Hapus dulu: bila CLI sempat membuat symlink, menyalin "ke" symlink itu berarti menimpa pustaka aslinya sendiri.
        Remove-Item $so -Force -ErrorAction SilentlyContinue
        Copy-Item (Join-Path $root "target\$triple\debug\libcloudpc_client_lib.so") $so -Force
        & (Join-Path $env:NDK_HOME 'toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-strip.exe') --strip-debug $so
        if ($LASTEXITCODE -ne 0) { throw 'llvm-strip gagal.' }
        # APK lama dihapus dulu: pengemas inkremental Gradle menimpa di tempat dan menyisakan ruang kosong (APK tetap
        # 180 MB walau isinya 50 MB).
        Remove-Item (Join-Path $here 'gen\android\app\build\outputs\apk') -Recurse -Force -ErrorAction SilentlyContinue
        "Tanpa Developer Mode: pustaka disalin ke jniLibs, merakit APK dengan Gradle..."
        Push-Location (Join-Path $here 'gen\android')
        try {
            & cmd /c ".\gradlew.bat assemble${arch}Debug -x rustBuild${arch}Debug --console=plain >> `"$log`" 2>&1"
            if ($LASTEXITCODE -ne 0) { Get-Content $log -Tail 60; throw "Gradle gagal (kode $LASTEXITCODE)." }
        } finally { Pop-Location }
    }
} finally { Pop-Location }

$apk = Get-ChildItem (Join-Path $here 'gen\android\app\build\outputs\apk') -Recurse -Filter *.apk | Sort-Object LastWriteTime | Select-Object -Last 1
$out = Join-Path $here 'dist\android'
New-Item -ItemType Directory -Force $out | Out-Null
$dest = Join-Path $out "CloudPC-Android-$Version-debug.apk"
Copy-Item $apk.FullName $dest -Force
"APK: $dest ({0:N1} MB), sha256 {1}" -f ((Get-Item $dest).Length / 1MB), (Get-FileHash $dest -Algorithm SHA256).Hash.ToLower()
