#!/bin/sh
# Build and package a PDFflow release on macOS:
#   dist/PDFflow-<ver>-macOS-universal.zip / .dmg  (PDFflow.app, Apple Silicon + Intel)
#   dist/PDFflow-<ver>-windows-x64.zip              (pdfflow.exe, cross-compiled with MinGW-w64)
# Needs: rustup targets aarch64-apple-darwin, x86_64-apple-darwin, x86_64-pc-windows-gnu and
# Homebrew's mingw-w64. Pass --no-build to package existing binaries.
set -eu
cd "$(dirname "$0")/.."
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST=dist
APP="$DIST/PDFflow.app"

if [ "${1:-}" != "--no-build" ]; then
    for t in aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-gnu; do
        cargo build --release --target "$t"
    done
fi

rm -rf "$DIST" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# Universal binary.
lipo -create -output "$APP/Contents/MacOS/pdfflow" \
    target/aarch64-apple-darwin/release/pdfflow target/x86_64-apple-darwin/release/pdfflow

# App icon from assets/icon.png.
ICONSET="$DIST/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z $s $s assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    sips -z $d $d assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>PDFflow</string>
    <key>CFBundleDisplayName</key><string>PDFflow</string>
    <key>CFBundleIdentifier</key><string>com.sompote.pdfflow</string>
    <key>CFBundleVersion</key><string>$VER</string>
    <key>CFBundleShortVersionString</key><string>$VER</string>
    <key>CFBundleExecutable</key><string>pdfflow</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>10.15</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>PDF document</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key><array><string>com.adobe.pdf</string></array>
        </dict>
    </array>
</dict>
</plist>
PLIST

# Ad-hoc signature (required for Apple Silicon; the app is not notarised).
codesign --force --deep --sign - "$APP"

ditto -c -k --keepParent "$APP" "$DIST/PDFflow-$VER-macOS-universal.zip"
DMG_SRC="$DIST/dmg"
mkdir -p "$DMG_SRC" && cp -R "$APP" "$DMG_SRC/" && ln -s /Applications "$DMG_SRC/Applications"
hdiutil create -volname "PDFflow $VER" -srcfolder "$DMG_SRC" -ov -format UDZO "$DIST/PDFflow-$VER-macOS-universal.dmg" >/dev/null
rm -rf "$DMG_SRC"

# Windows.
WIN="$DIST/PDFflow-$VER-windows-x64"
mkdir -p "$WIN"
cp target/x86_64-pc-windows-gnu/release/pdfflow.exe "$WIN/PDFflow.exe"
cp README.md "$WIN/README.md"
cp assets/fonts/OFL-Inter.txt assets/fonts/OFL-Sarabun.txt "$WIN/"
(cd "$DIST" && zip -qr "PDFflow-$VER-windows-x64.zip" "PDFflow-$VER-windows-x64")
rm -rf "$WIN"

(cd "$DIST" && shasum -a 256 PDFflow-$VER-* > SHA256SUMS.txt)
ls -la "$DIST"
