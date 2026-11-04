#!/bin/sh
# Render the app icon from assets/icon/droidrescue.svg into the committed
# platform files. Run on macOS (needs rsvg-convert + ImageMagick from
# Homebrew, and iconutil from Xcode) whenever the SVG changes:
#
#   sh scripts/make-icons.sh
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR="$ROOT/assets/icon"
SVG="$DIR/droidrescue.svg"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

render() { rsvg-convert -w "$1" -h "$1" "$SVG" -o "$2"; }

# macOS .icns (iconset naming per iconutil).
SET="$WORK/DroidRescue.iconset"
mkdir -p "$SET"
for size in 16 32 128 256 512; do
  render "$size" "$SET/icon_${size}x${size}.png"
  render $((size * 2)) "$SET/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$SET" -o "$DIR/droidrescue.icns"

# Windows .ico: every size the shell asks for, each rendered from the SVG
# (not downscaled from one bitmap) so small sizes stay crisp, stored as
# PNG entries (Vista+) — a fraction of the size of BMP entries.
ICO_PNGS=""
for size in 16 20 24 32 40 48 64 96 128 256; do
  render "$size" "$WORK/ico-$size.png"
  ICO_PNGS="$ICO_PNGS $WORK/ico-$size.png"
done
# shellcheck disable=SC2086
python3 - "$DIR/droidrescue.ico" $ICO_PNGS <<'PY'
import struct, sys
out, pngs = sys.argv[1], sys.argv[2:]
blobs = [open(p, "rb").read() for p in pngs]
header = struct.pack("<HHH", 0, 1, len(blobs))
offset = 6 + 16 * len(blobs)
entries = b""
for blob in blobs:
    w, h = struct.unpack(">II", blob[16:24])  # PNG IHDR
    entries += struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0, 1, 32, len(blob), offset)
    offset += len(blob)
open(out, "wb").write(header + entries + b"".join(blobs))
PY

# Runtime window / Dock icon (embedded by main.rs).
render 256 "$DIR/droidrescue-256.png"

echo "icons written to $DIR"
