#!/bin/sh
# Builds an app bundle from this workspace.
#   APP=Funkhaus (default): Funkhaus.app (GUI) + installs the `funkhaus` CLI, copies the app to ~/Applications and puts an alias on the Desktop.
#   APP=Stimmwerk: dist/Stimmwerk.app only (GUI + `stimmwerk` CLI in Contents/Helpers); installs nothing.
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
NAME=${APP:-Funkhaus}
case $NAME in
  Funkhaus)  GUI=funkhaus-gui;  CLI_PKG=funkhaus-cli;  CLI=funkhaus;  BID=local.funkhaus.funkhaus;  INSTALL=1 ;;
  Stimmwerk) GUI=stimmwerk-gui; CLI_PKG=stimmwerk-cli; CLI=stimmwerk; BID=local.funkhaus.stimmwerk; INSTALL=0 ;;
  *) echo "{\"ok\": false, \"error\": \"unbekannte APP: $NAME (Funkhaus|Stimmwerk)\"}"; exit 2 ;;
esac
# also the dist bundle: rm -rf below would pull a running preview from under its feet
if pgrep -f "$NAME.app/Contents/MacOS/$NAME" >/dev/null; then
  echo "{\"ok\": false, \"error\": \"$NAME.app läuft – bitte erst beenden\"}"
  exit 1
fi
cargo build --release -p "$GUI" -p "$CLI_PKG"

DIST="$ROOT/dist"
BUNDLE="$DIST/$NAME.app"
rm -rf "$BUNDLE" "$DIST/icon.iconset"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources" "$DIST/icon.iconset"
cp "target/release/$GUI" "$BUNDLE/Contents/MacOS/$NAME"
if [ "$INSTALL" = 0 ]; then
  # CLI travels inside the bundle (Helpers: MacOS/stimmwerk would clash with MacOS/Stimmwerk on case-insensitive APFS)
  mkdir -p "$BUNDLE/Contents/Helpers"
  cp "target/release/$CLI" "$BUNDLE/Contents/Helpers/$CLI"
fi

# icon: rendered by the GUI binary itself, then sips + iconutil
"target/release/$GUI" --write-icon "$DIST/icon.png"
for s in 16 32 128 256 512; do
  sips -z $s $s "$DIST/icon.png" --out "$DIST/icon.iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d "$DIST/icon.png" --out "$DIST/icon.iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$DIST/icon.iconset" -o "$BUNDLE/Contents/Resources/$NAME.icns"

VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cat > "$BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleIdentifier</key><string>$BID</string>
  <key>CFBundleExecutable</key><string>$NAME</string>
  <key>CFBundleIconFile</key><string>$NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VER</string>
  <key>CFBundleShortVersionString</key><string>$VER</string>
  <key>CFBundleDevelopmentRegion</key><string>de</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --deep -s - "$BUNDLE" >/dev/null 2>&1 || echo "codesign übersprungen" >&2

if [ "$INSTALL" = 0 ]; then
  touch "$DIST/.metadata_never_index"
  echo "{\"ok\": true, \"app\": \"$BUNDLE\", \"installed\": false}"
  exit 0
fi

mkdir -p "$HOME/Applications" "$HOME/.local/bin"
rm -rf "$HOME/Applications/$NAME.app"
ditto "$BUNDLE" "$HOME/Applications/$NAME.app"
cp "target/release/$CLI" "$HOME/.local/bin/$CLI"
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
$LSREG -f "$HOME/Applications/$NAME.app" 2>/dev/null || true
# only one app: drop the build copy, otherwise Spotlight/Launchpad list the app twice
$LSREG -u "$BUNDLE" 2>/dev/null || true
rm -rf "$BUNDLE"
touch "$DIST/.metadata_never_index"

# Desktop alias (Finder alias; symlink as fallback)
if [ ! -e "$HOME/Desktop/$NAME" ]; then
  osascript -e "tell application \"Finder\" to make alias file to (POSIX file \"$HOME/Applications/$NAME.app\") at (POSIX file \"$HOME/Desktop\") with properties {name:\"$NAME\"}" >/dev/null 2>&1 \
    || ln -s "$HOME/Applications/$NAME.app" "$HOME/Desktop/$NAME"
fi
echo "{\"ok\": true, \"app\": \"$HOME/Applications/$NAME.app\", \"cli\": \"$HOME/.local/bin/$CLI\"}"
