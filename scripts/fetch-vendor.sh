#!/bin/sh
# macOS counterpart of fetch-vendor.ps1: the official scrcpy macOS release,
# pinned by SHA256; only adb and scrcpy-server are kept (vendor/scrcpy-macos).
#
#   sh scripts/fetch-vendor.sh
set -eu

VERSION=5.0.1
case "$(uname -m)" in
  arm64) ARCH=aarch64; SHA256=33611e51977a8289e2e124b220f727895181ef2eb9060e0987a4dc2db9cf0d6b ;;
  x86_64) ARCH=x86_64; SHA256=31a5467a9e907f162b093267cbbab1276b15c6e709c2ea8159566a8d165beb50 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEST="$ROOT/vendor/scrcpy-macos"
NAME="scrcpy-macos-$ARCH-v$VERSION"

if [ "$(cat "$DEST/VERSION.txt" 2>/dev/null)" = "v$VERSION" ] &&
   [ -f "$DEST/adb" ] && [ -f "$DEST/scrcpy-server" ] && [ -f "$DEST/LICENSE.txt" ]; then
  echo "vendor/scrcpy-macos already at v$VERSION"
  exit 0
fi

WORK="${TMPDIR:-/tmp}/droidrescue-vendor-$VERSION-$ARCH"
mkdir -p "$WORK"
TGZ="$WORK/$NAME.tar.gz"
if [ ! -f "$TGZ" ]; then
  URL="https://github.com/Genymobile/scrcpy/releases/download/v$VERSION/$NAME.tar.gz"
  echo "downloading $URL"
  curl -fsSL -o "$TGZ" "$URL"
fi
ACTUAL=$(shasum -a 256 "$TGZ" | cut -d' ' -f1)
if [ "$ACTUAL" != "$SHA256" ]; then
  rm -f "$TGZ"
  echo "SHA256 mismatch for $NAME: expected $SHA256, got $ACTUAL" >&2
  exit 1
fi

rm -rf "$WORK/x" && mkdir -p "$WORK/x"
tar xzf "$TGZ" -C "$WORK/x"
mkdir -p "$DEST"
cp "$WORK/x/$NAME/adb" "$WORK/x/$NAME/scrcpy-server" "$DEST/"
cp "$WORK/x/$NAME/LICENSE" "$DEST/LICENSE.txt"
printf 'v%s' "$VERSION" > "$DEST/VERSION.txt"
echo "vendor/scrcpy-macos ready (v$VERSION)"
