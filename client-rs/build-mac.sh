#!/bin/bash
# Membuat "CloudPC Rust.app" dan CloudPCRust.dmg (universal: Apple Silicon + Intel) dari aplikasi Rust. HARUS di Mac (atau runner macOS di CI).
# BELUM pernah dijalankan di Mac sungguhan (ditulis di Windows). Jalankan dari akar repo:
#   CLOUDPC_RS_VERSION=2026.11.2.1 bash client-rs/build-mac.sh
# Prasyarat: Rust (rustup) + Xcode command line tools. Client stream bermerek diunduh dari rilis cloudpc-stream.
# Tanda tangan ad-hoc: penerima sekali "Open Anyway" di System Settings > Privacy & Security (belum ada notarisasi Apple).
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${CLOUDPC_RS_VERSION:-0.0.0}"
export CLOUDPC_RS_VERSION="$VERSION"
API_HOST="${API_HOST:-server.example}"
OUT=client-rs/dist/mac
APP="$OUT/CloudPC Rust.app"

echo "1/4 Build release $VERSION (universal: Apple Silicon + Intel)..."
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release -p cloudpc-client --target aarch64-apple-darwin
cargo build --release -p cloudpc-client --target x86_64-apple-darwin

echo "2/4 Menyusun bundel..."
rm -rf "$OUT"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/CloudPCRust" \
    target/aarch64-apple-darwin/release/cloudpc-client target/x86_64-apple-darwin/release/cloudpc-client
cp client/assets/cloudpc.icns "$APP/Contents/Resources/cloudpc.icns"
cp deploy/cloudpc-server.pem "$APP/Contents/Resources/"
printf '{\n  "api_url": "https://%s",\n  "ca_file": "cloudpc-server.pem",\n  "brand_name": "CloudPC",\n  "moonlight_app": "Desktop"\n}\n' \
    "$API_HOST" > "$APP/Contents/Resources/client_config.json"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>CloudPCRust</string>
  <key>CFBundleIdentifier</key><string>id.cloudpc.app.rust</string>
  <key>CFBundleName</key><string>CloudPC Rust</string>
  <key>CFBundleDisplayName</key><string>CloudPC Rust</string>
  <key>CFBundleIconFile</key><string>cloudpc</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

# Client stream bermerek (sama dengan aplikasi Python); curl tidak menandai file "dari internet" sehingga Gatekeeper diam.
STREAM_DMG_URL="${STREAM_DMG_URL:-https://github.com/HusniHeartnet/cloudpc-stream/releases/latest/download/CloudPCStream-macOS.dmg}"
tmp=$(mktemp -d)
curl -fL --retry 3 -o "$tmp/stream.dmg" "$STREAM_DMG_URL"
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$tmp/mnt" "$tmp/stream.dmg"
mkdir -p "$APP/Contents/Resources/stream"
cp -R "$tmp/mnt/CloudPC Stream.app" "$APP/Contents/Resources/stream/"
hdiutil detach -quiet "$tmp/mnt"; rm -rf "$tmp"

# Client stream versi Rust (opsional): folder hasil CI stream-rs-mac (isi stream-rs-mac.zip). Bila ada, aplikasi memakainya
# lebih dulu daripada "CloudPC Stream.app". Dylib di sampingnya dicari lewat @executable_path.
# Dibungkus sebagai aplikasi Mac sendiri ("CloudPC Stream", bundle id com.cloudpc.stream-rs): program lepas tanpa bundel TIDAK
# muncul di daftar "Select program" Window Capture TikTok LIVE (temuan tester 20 Nov 2026), yang hanya menampilkan aplikasi.
if [ -n "${STREAM_RS_DIR:-}" ] && [ -x "$STREAM_RS_DIR/cloudpc-stream-client" ]; then
  RSAPP="$APP/Contents/Resources/stream-rs/CloudPC Stream Rust.app"
  mkdir -p "$RSAPP/Contents/MacOS" "$RSAPP/Contents/Resources"
  for f in "$STREAM_RS_DIR"/*; do
    # Mach-O (program + dylib) di MacOS/ (dicari lewat @executable_path); berkas lain (lisensi) di Resources/.
    if file -b "$f" | grep -q Mach-O; then cp "$f" "$RSAPP/Contents/MacOS/"; else cp -R "$f" "$RSAPP/Contents/Resources/"; fi
  done
  cp client/assets/cloudpc.icns "$RSAPP/Contents/Resources/cloudpc.icns"
  cat > "$RSAPP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.cloudpc.stream-rs</string>
  <key>CFBundleName</key><string>CloudPC Stream</string>
  <key>CFBundleDisplayName</key><string>CloudPC Stream</string>
  <key>CFBundleExecutable</key><string>cloudpc-stream-client</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>cloudpc</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key><string>CloudPC Stream menyambung ke PC cloud kamu.</string>
</dict></plist>
PLIST
  codesign --force --sign - "$RSAPP"
  echo "Client stream Rust dibundel sebagai aplikasi dari $STREAM_RS_DIR"
fi

echo "3/4 Tanda tangan ad-hoc..."
codesign --force --deep --sign - "$APP"

echo "4/4 Membuat DMG..."
stage=$(mktemp -d)
cp -R "$APP" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -quiet -volname "CloudPC Rust" -srcfolder "$stage" -ov -format UDZO "$OUT/CloudPCRust.dmg"
rm -rf "$stage"
echo "$VERSION" > "$OUT/version.txt"
echo "Selesai: $OUT/CloudPCRust.dmg"
