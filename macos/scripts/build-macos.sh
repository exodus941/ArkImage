#!/bin/bash
# Builds dist/ArkImage.app (release, this Mac's architecture; UNIVERSAL=1 for arm64 + x86_64).
# File types come from IMAGE_EXTS and ARCHIVE_EXTS in src/source.rs, minus NOT_ASSOCIATED.
# Signing: ad hoc unless SIGN_ID names a Developer ID certificate.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/Library/Caches/arkimage-target}"
scripts/build-rlottie-macos.sh
# Release builds must not carry this Mac's paths: map the home folder away (a plain build
# embeds source paths in panic messages and debug info).
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build --remap-path-prefix=$PWD=/build/arkimage"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/ArkImage.app

if [ "${UNIVERSAL:-0}" = 1 ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  BIN=$(mktemp)
  lipo -create -output "$BIN" "$CARGO_TARGET_DIR"/{aarch64,x86_64}-apple-darwin/release/arkimage
else
  cargo build --release
  BIN="$CARGO_TARGET_DIR/release/arkimage"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ArkImage"

ICONSET=$(mktemp -d)/ArkImage.iconset
cargo run -q --release --example make_iconset -- "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/ArkImage.icns"

exts() { tr -d '\r' < src/source.rs | sed -n "/pub const $1/,/];/p" | grep -o '"[a-z0-9]*"' | tr -d '"'; }
skip=$(exts NOT_ASSOCIATED)
doc_type() { # name role exts...
  local name=$1; shift
  printf '\t\t<dict>\n\t\t\t<key>CFBundleTypeName</key><string>%s</string>\n\t\t\t<key>CFBundleTypeRole</key><string>Viewer</string>\n\t\t\t<key>LSHandlerRank</key><string>Alternate</string>\n\t\t\t<key>CFBundleTypeIconFile</key><string>ArkImage</string>\n\t\t\t<key>CFBundleTypeExtensions</key>\n\t\t\t<array>\n' "$name"
  for e in "$@"; do grep -qx "$e" <<<"$skip" || printf '\t\t\t\t<string>%s</string>\n' "$e"; done
  printf '\t\t\t</array>\n\t\t</dict>\n'
}
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>ArkImage</string>
	<key>CFBundleDisplayName</key><string>ArkImage</string>
	<key>CFBundleIdentifier</key><string>io.github.exodus941.arkimage</string>
	<key>CFBundleExecutable</key><string>ArkImage</string>
	<key>CFBundleIconFile</key><string>ArkImage</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>$VERSION</string>
	<key>CFBundleVersion</key><string>$VERSION</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>NSRequiresAquaSystemAppearance</key><false/>
	<key>CFBundleDocumentTypes</key>
	<array>
$(doc_type "Image" $(exts IMAGE_EXTS))
$(doc_type "Archive" $(exts ARCHIVE_EXTS))
	</array>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Refuse a build that still names this Mac's user (paths, account names).
for word in "$(id -un)" "$(id -F 2>/dev/null | cut -d' ' -f1)"; do
  if [ -n "$word" ] && LC_ALL=C grep -a -q -i -- "$word" "$APP/Contents/MacOS/ArkImage"; then
    echo "refused: the binary contains '$word'" >&2; exit 1
  fi
done

codesign --force --deep --sign "${SIGN_ID:--}" ${SIGN_ID:+--options runtime --timestamp} "$APP"
echo "built $APP ($VERSION, $(du -sh "$APP" | cut -f1))"

# Updater asset: the app zipped with ditto (keeps the bundle's attributes); src/update.rs
# downloads ArkImage-macOS.zip from the latest release.
rm -f dist/ArkImage-macOS.zip
ditto -c -k --keepParent "$APP" dist/ArkImage-macOS.zip
echo "built dist/ArkImage-macOS.zip ($(du -h dist/ArkImage-macOS.zip | cut -f1))"

# DMG: the app beside an Applications link, so it installs by drag and drop.
DMG=dist/ArkImage-$VERSION-macOS.dmg
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -quiet -volname "ArkImage $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -imagekey zlib-level=9 "$DMG"
rm -rf "$STAGE"
if [ -n "${SIGN_ID:-}" ]; then codesign --sign "$SIGN_ID" --timestamp "$DMG"; fi
echo "built $DMG ($(du -h "$DMG" | cut -f1))"
