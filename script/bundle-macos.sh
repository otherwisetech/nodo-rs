#!/usr/bin/env bash
# Wraps the `nodo-app` binary (and the `nodo` CLI) in a macOS .app so Finder and the Dock show
# the app icon.
# Usage: script/bundle-macos.sh [debug|release]   (default: release; builds first)
#        UNIVERSAL=1 script/bundle-macos.sh       (release for Apple silicon + Intel, joined with lipo)
set -euo pipefail
profile="${1:-release}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
target="${CARGO_TARGET_DIR:-target}"
if [ "${UNIVERSAL:-0}" = 1 ]; then
  profile=release
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t" >/dev/null
    cargo build -p nodo-app -p nodo-cli --release --target "$t"
  done
  mkdir -p "$target/release"
  for bin in nodo-app nodo; do
    lipo -create -output "$target/release/$bin" "$target/aarch64-apple-darwin/release/$bin" "$target/x86_64-apple-darwin/release/$bin"
  done
elif [ "$profile" = release ]; then cargo build -p nodo-app -p nodo-cli --release; else cargo build -p nodo-app -p nodo-cli; fi
version="$(cargo pkgid -p nodo-app | sed 's/.*[#@]//')"
app="$target/$profile/Nodo.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/$profile/nodo-app" "$app/Contents/MacOS/nodo-app"
cp "$target/$profile/nodo" "$app/Contents/MacOS/nodo"
cp crates/app/assets/logo/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Nodo</string>
  <key>CFBundleDisplayName</key><string>Nodo</string>
  <key>CFBundleIdentifier</key><string>io.github.otherwisetech.nodo-app</string>
  <key>CFBundleExecutable</key><string>nodo-app</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key><string>Nodo maps the devices on your local network (neighbour table, Bonjour and UPnP, and a subnet scan when you ask).</string>
  <key>NSBonjourServices</key>
  <array>
    <string>_http._tcp</string><string>_https._tcp</string><string>_ssh._tcp</string><string>_sftp-ssh._tcp</string>
    <string>_smb._tcp</string><string>_afpovertcp._tcp</string><string>_adisk._tcp</string><string>_ipp._tcp</string>
    <string>_ipps._tcp</string><string>_printer._tcp</string><string>_pdl-datastream._tcp</string><string>_scanner._tcp</string>
    <string>_airplay._tcp</string><string>_raop._tcp</string><string>_googlecast._tcp</string><string>_spotify-connect._tcp</string>
    <string>_sonos._tcp</string><string>_hap._tcp</string><string>_homeassistant._tcp</string><string>_hue._tcp</string>
    <string>_matter._tcp</string><string>_esphomelib._tcp</string><string>_rfb._tcp</string><string>_rdp._tcp</string>
    <string>_workstation._tcp</string><string>_device-info._tcp</string><string>_companion-link._tcp</string>
    <string>_apple-mobdev2._tcp</string><string>_rtsp._tcp</string><string>_amzn-wplay._tcp</string>
  </array>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Nodo network map</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>CFBundleTypeExtensions</key><array><string>nodo</string></array>
      <key>CFBundleTypeIconFile</key><string>AppIcon</string>
    </dict>
  </array>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>io.github.otherwisetech.nodo-app</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>CFBundleURLSchemes</key><array><string>otherwise-nodo</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
plutil -lint -s "$app/Contents/Info.plist"
# Make LaunchServices pick up the (possibly changed) icon.
touch "$app"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app" || true
echo "$app"
