# Build aplikasi pemain versi Rust: client-rs\dist\CloudPC-Rust (siap dibungkus installer) dan, bila -Installer,
# CloudPCRustSetup.exe lewat Inno Setup 6 (harus terpasang; ISCC.exe).
#   powershell -ExecutionPolicy Bypass -File E:\cloudpc\client-rs\build.ps1
#   ... build.ps1 -Installer                       # + installer
#   ... build.ps1 -Installer -Version 2026.10.30.7 # versi manual (format angka bertitik, maks 4 bagian)
#   ... build.ps1 -SkipBuild                       # hanya susun folder dari exe release yang sudah ada (untuk uji skrip;
#                                                  # versi di exe TIDAK diganti, jadi jangan dipakai untuk rilis)
# Versi ditanam ke exe lewat CLOUDPC_RS_VERSION; tanpa -Version dipakai tanggal + menit hari ini (selalu naik dalam sehari).
# Build lokal TANPA versi dianggap "0.0.0" (paling lama) oleh server, jadi ditawari update dari rilis mana pun.
param([string]$Version = '', [switch]$Installer, [string]$ApiHost = 'server.example', [switch]$SkipBuild, [switch]$WithRustStream)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
Set-Location $root
if (-not $Version) { $Version = '{0}.{1}' -f (Get-Date -Format 'yyyy.M.d'), [int](Get-Date).TimeOfDay.TotalMinutes }
if ($Version -notmatch '^\d+(\.\d+){1,3}$') { throw "Format versi tidak valid: $Version (contoh 2026.10.30.7)" }
$env:CLOUDPC_RS_VERSION = $Version

if ($SkipBuild) {
    Write-Warning "-SkipBuild: memakai exe release yang sudah ada apa adanya (versi di dalam exe tidak diubah)."
} else {
    Write-Host "1/3 Build release $Version..."
    cargo build --release -p cloudpc-client
    if ($LASTEXITCODE -ne 0) { throw "cargo build gagal" }
}

Write-Host "2/3 Menyusun client-rs\dist\CloudPC-Rust..."
$dist = Join-Path $here 'dist\CloudPC-Rust'
Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $dist | Out-Null
Copy-Item (Join-Path $root 'target\release\cloudpc-client.exe') (Join-Path $dist 'CloudPCRust.exe')
Copy-Item (Join-Path $root 'deploy\cloudpc-server.pem') $dist
$cfg = [ordered]@{ api_url = "https://$ApiHost"; ca_file = 'cloudpc-server.pem'; brand_name = 'CloudPC'; moonlight_app = 'Desktop' }
[IO.File]::WriteAllText((Join-Path $dist 'client_config.json'), ($cfg | ConvertTo-Json))
# Client stream bermerek (sama dengan aplikasi Python); tanpa ini aplikasi memakai Moonlight yang dipasang pengguna.
$stream = Join-Path $root 'client\vendor\stream'
if (Test-Path $stream) { Copy-Item $stream (Join-Path $dist 'stream') -Recurse } else { Write-Warning "client\vendor\stream tidak ada: installer tanpa client stream bermerek" }
# Client stream versi Rust: dengan -WithRustStream. Keputusan pemilik 19 Nov 2026 (malam): sumber stream-rs DITERBITKAN (GPL-3.0,
# github.com/HusniHeartnet/cloudpc-stream-rs, lewat tools\teman\terbitkan-stream-rs.ps1), jadi client Rust boleh ikut installer
# penguji. Terbitkan sumber versi yang sama SEBELUM menerbitkan installer-nya. Tanpa -WithRustStream: hanya Moonlight bermerek.
if ($WithRustStream) {
    $streamRs = Join-Path $root 'stream-rs\target\package\stream-rs'
    if (Test-Path $streamRs) { Copy-Item $streamRs (Join-Path $dist 'stream-rs') -Recurse; Write-Host "Client stream Rust ikut dibundel (sumber: github.com/HusniHeartnet/cloudpc-stream-rs)." } else { throw "stream-rs\target\package\stream-rs tidak ada (jalankan stream-rs\tools\package-client.ps1)" }
}
Set-Content (Join-Path $here 'dist\version.txt') $Version -Encoding ascii
"{0:N1} MB (exe)" -f ((Get-Item (Join-Path $dist 'CloudPCRust.exe')).Length / 1MB)

if ($Installer) {
    Write-Host "3/3 Membuat installer..."
    $iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup 6 belum terpasang (ISCC.exe). Pasang dari jrsoftware.org/isinfo.php atau pakai CI (client-rs\ci\build.yml)." }
    & $iscc /Q "/DAppVersion=$Version" (Join-Path $here 'installer\CloudPC-Rust.iss')
    if ($LASTEXITCODE -ne 0) { throw "ISCC gagal ($LASTEXITCODE)" }
    "Installer: {0} ({1:N1} MB)" -f (Join-Path $here 'dist\CloudPCRustSetup.exe'), ((Get-Item (Join-Path $here 'dist\CloudPCRustSetup.exe')).Length / 1MB)
} else {
    Write-Host "3/3 Dilewati (tambahkan -Installer untuk membuat CloudPCRustSetup.exe)."
}
Write-Host "Versi $Version selesai." -ForegroundColor Green
