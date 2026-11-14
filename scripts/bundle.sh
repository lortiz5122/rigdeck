#!/usr/bin/env bash
# Build RigDeck.app (release) into dist/ and sign it ad hoc for this Mac.
# Developer ID signing and notarisation are separate release steps and need explicit approval.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"
cargo build --release -p rigdeck-app -p rigdeck-tools
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/RigDeck.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/rigdeck-app "$APP/Contents/MacOS/RigDeck"
cp target/release/rigdeck-tools "$APP/Contents/MacOS/rigdeck-tools"
cp assets/icon/RigDeck.icns "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>RigDeck</string>
  <key>CFBundleDisplayName</key><string>RigDeck</string>
  <key>CFBundleIdentifier</key><string>io.github.lortiz5122.rigdeck</string>
  <key>CFBundleExecutable</key><string>RigDeck</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>RigDeck, written fresh from Icom and Yaesu documentation.</string>
</dict></plist>
PLIST
codesign --force --sign - --options runtime "$APP/Contents/MacOS/rigdeck-tools"
codesign --force --sign - --options runtime "$APP"
codesign --verify --strict "$APP" && echo "BUNDLED: $(pwd)/$APP (version $VERSION)"
