#!/usr/bin/env bash
# Renders Fairspoken Server's app icon from the brand's dark icon composite
# (design/brand/logo/app-icon/app-icon-dark-1024.png) into its asset catalog.
# Rerun when the brand icon changes; the shipping icon should eventually come from
# Icon Composer (see design/brand/README.md §2).
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=../../design/brand/logo/app-icon/app-icon-dark-1024.png
OUT=FairspokenServer/Resources/Assets.xcassets/AppIcon.appiconset
for size in 16 32 128 256 512; do
  sips -z $size $size "$SRC" --out "$OUT/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$SRC" --out "$OUT/icon_${size}x${size}@2x.png" >/dev/null
done
echo "Wrote $OUT"
