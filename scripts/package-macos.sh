#!/bin/sh
# Build DroidRescue.app and a compressed .dmg (LZMA) into dist/.
# Ad-hoc signed: runs on Apple Silicon; first launch needs right-click → 打开
# (or `xattr -dr com.apple.quarantine DroidRescue.app`) until a Developer ID
# signature + notarisation is set up.
#
#   sh scripts/package-macos.sh        (after scripts/fetch-vendor.sh)
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)

cargo build --release

STAGE="dist/DroidRescue-$VERSION-macos-$ARCH"
APP="$STAGE/DroidRescue.app"
rm -rf "$STAGE"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/droidrescue "$APP/Contents/MacOS/DroidRescue"
cp assets/icon/droidrescue.icns "$APP/Contents/Resources/AppIcon.icns"
cp docs/使用说明.txt "$STAGE/"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>DroidRescue</string>
  <key>CFBundleDisplayName</key><string>安卓救援宝</string>
  <key>CFBundleIdentifier</key><string>io.github.heisir2014.droidrescue</string>
  <key>CFBundleExecutable</key><string>DroidRescue</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleDevelopmentRegion</key><string>zh_CN</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

codesign --force --deep --sign - "$APP"
codesign --verify --strict "$APP"

# Drag-to-install layout.
ln -s /Applications "$STAGE/Applications"
DMG="dist/DroidRescue-$VERSION-macos-$ARCH.dmg"
rm -f "$DMG"
# hdiutil fails intermittently on CI runners ("Resource busy"): retry.
attempt=1
until hdiutil create -volname "安卓救援宝 DroidRescue" -srcfolder "$STAGE" -format ULMO "$DMG"; do
  if [ "$attempt" -ge 3 ]; then
    echo "hdiutil create failed $attempt times" >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  echo "hdiutil create failed; retrying ($attempt/3)" >&2
  rm -f "$DMG"
  sleep 5
done
echo "packaged $DMG ($(du -h "$DMG" | cut -f1))"
