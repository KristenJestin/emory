#!/usr/bin/env bash
# Renders the PNGs of assets/brand from its SVGs, with the fonts of assets/brand/fonts only: a
# fontconfig made for the run, so no font of the machine changes the drawing. Needs rsvg-convert
# (librsvg). The PNGs are committed; run this again after changing an SVG.
set -euo pipefail

brand="$(cd "$(dirname "$0")/../assets/brand" && pwd)"
run="$(mktemp -d)"
trap 'rm -rf "$run"' EXIT

cat >"$run/fonts.conf" <<CONF
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "fonts.dtd">
<fontconfig><dir>$brand/fonts</dir><cachedir>$run/cache</cachedir></fontconfig>
CONF
export FONTCONFIG_FILE="$run/fonts.conf"

for name in social-preview social-preview-en card card-en screen screen-en; do
  rsvg-convert "$brand/$name.svg" -o "$brand/png/$name.png"
done
