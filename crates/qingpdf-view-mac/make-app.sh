#!/bin/sh
# Builds the Mac reader and wraps it as target/release/qingpdf.app (icon, Info.plist, ad-hoc signature).
# Usage: crates/qingpdf-view-mac/make-app.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
command -v cargo >/dev/null 2>&1 || PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export PATH
cd "$root"

cargo build --release -p qingpdf-view-mac
target="${CARGO_TARGET_DIR:-$root/target}/release"
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)
version=${version:-0.1.0}

app="$target/qingpdf.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/qingpdf-view-mac" "$app/Contents/MacOS/qingpdf-view-mac"

# The icon: the program draws its own mark (hidden --write-icon), sips makes the sizes, iconutil the .icns.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
"$target/qingpdf-view-mac" --write-icon "$work/icon.png"
iconset="$work/qingpdf.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$work/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$work/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/qingpdf.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleExecutable</key><string>qingpdf-view-mac</string>
    <key>CFBundleIconFile</key><string>qingpdf</string>
    <key>CFBundleIdentifier</key><string>org.qingpdf.reader</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>qingpdf</string>
    <key>CFBundleDisplayName</key><string>qingpdf</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>PDF Document</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key><array><string>com.adobe.pdf</string></array>
            <key>CFBundleTypeExtensions</key><array><string>pdf</string></array>
            <key>CFBundleTypeMIMETypes</key><array><string>application/pdf</string></array>
        </dict>
    </array>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Ad hoc signature (no developer identity): enough to run on this Mac; a build for other Macs needs a real identity and notarization.
codesign --force --deep -s - "$app"
codesign --verify --deep --strict "$app"
echo "built $app ($(du -sk "$app" | cut -f1) KB)"
