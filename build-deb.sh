#!/usr/bin/env bash
# Build a Debian package for Atlas.
# Requires: cargo, cargo-deb, librsvg2-bin (rsvg-convert) or imagemagick

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "==> Checking dependencies..."
if ! command -v cargo &>/dev/null; then
    echo "ERROR: cargo not found. Install Rust: https://rustup.rs"
    exit 1
fi

if ! cargo deb --help &>/dev/null 2>&1; then
    echo "==> Installing cargo-deb..."
    cargo install cargo-deb
fi

echo "==> Generating PNG icon from SVG..."
if command -v rsvg-convert &>/dev/null; then
    rsvg-convert -w 256 -h 256 assets/atlas.svg -o assets/atlas.png
elif command -v convert &>/dev/null; then
    convert -background none assets/atlas.svg -resize 256x256 assets/atlas.png
elif command -v inkscape &>/dev/null; then
    inkscape --export-type=png --export-width=256 --export-height=256 \
        --export-filename=assets/atlas.png assets/atlas.svg
else
    echo "WARNING: No SVG converter found — using existing assets/atlas.png"
fi

echo "==> Building release binary..."
cargo build --release

echo "==> Building .deb package..."
cargo deb

DEB=$(ls target/debian/atlas_*.deb 2>/dev/null | sort -V | tail -1)
if [ -n "$DEB" ]; then
    echo ""
    echo "==> SUCCESS: $DEB"
    echo ""
    echo "Install with:"
    echo "  sudo apt install ./$DEB"
else
    echo "ERROR: .deb file not found after build"
    exit 1
fi
