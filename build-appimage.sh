#!/usr/bin/env bash
# Build an AppImage for Atlas.
# Requires: cargo, appimagetool (downloaded automatically if missing),
#           rsvg-convert or imagemagick (for icon conversion)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c \
    "import sys,json; data=json.load(sys.stdin); \
     print(next(p['version'] for p in data['packages'] if p['name']=='atlas'))")

APPDIR="$SCRIPT_DIR/Atlas.AppDir"
APPIMAGETOOL_URL="https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage"
APPIMAGETOOL="$SCRIPT_DIR/appimagetool"

echo "==> Building Atlas v$VERSION AppImage"

echo "==> Building release binary..."
cargo build --release

echo "==> Generating PNG icon..."
if command -v rsvg-convert &>/dev/null; then
    rsvg-convert -w 256 -h 256 assets/atlas.svg -o assets/atlas.png
elif command -v convert &>/dev/null; then
    convert -background none assets/atlas.svg -resize 256x256 assets/atlas.png
fi

echo "==> Assembling AppDir..."
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/share/applications"
mkdir -p "$APPDIR/usr/share/icons/hicolor/256x256/apps"
mkdir -p "$APPDIR/usr/share/icons/hicolor/scalable/apps"

cp target/release/atlas "$APPDIR/usr/bin/"
cp packaging/atlas.desktop "$APPDIR/usr/share/applications/"
cp assets/atlas.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/atlas.png"
cp assets/atlas.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/atlas.svg"

cp packaging/atlas.desktop "$APPDIR/atlas.desktop"
cp assets/atlas.png "$APPDIR/atlas.png"

cat > "$APPDIR/AppRun" << 'EOF'
#!/usr/bin/env bash
HERE="$(dirname "$(readlink -f "${0}")")"
export PATH="$HERE/usr/bin:$PATH"
exec "$HERE/usr/bin/atlas" "$@"
EOF
chmod +x "$APPDIR/AppRun"

if [ ! -x "$APPIMAGETOOL" ]; then
    echo "==> Downloading appimagetool..."
    curl -fsSL -o "$APPIMAGETOOL" "$APPIMAGETOOL_URL"
    chmod +x "$APPIMAGETOOL"
fi

echo "==> Building AppImage..."
ARCH=x86_64 "$APPIMAGETOOL" "$APPDIR" "Atlas-$VERSION-x86_64.AppImage"

echo ""
echo "==> SUCCESS: Atlas-$VERSION-x86_64.AppImage"
echo ""
echo "Run with:"
echo "  chmod +x Atlas-$VERSION-x86_64.AppImage"
echo "  ./Atlas-$VERSION-x86_64.AppImage"
