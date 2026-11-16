#!/usr/bin/env bash
# Packages release binaries into dist/:
#   rustworks-<version>-<platform>.tar.gz (.zip on Windows)
# containing rustworks (desktop app), rustworks-mcp (MCP server), README and
# examples. On macOS the app is also wrapped as RustWorks.app.
#
# usage: scripts/package.sh <platform-name> [version]
set -euo pipefail
platform="${1:?platform name, e.g. macos-arm64}"
version="${2:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
exe=""
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) exe=".exe" ;; esac

name="rustworks-${version}-${platform}"
stage="dist/${name}"
rm -rf "$stage"
mkdir -p "$stage"
cp "target/release/rustworks${exe}" "target/release/rustworks-mcp${exe}" "$stage/"
cp README.md "$stage/"
cp -R examples "$stage/examples"
cp -R tools/solidworks "$stage/solidworks"

if [[ "$(uname -s)" == "Darwin" ]]; then
  app="$stage/RustWorks.app/Contents"
  mkdir -p "$app/MacOS"
  cp target/release/rustworks "$app/MacOS/rustworks"
  cat > "$app/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>RustWorks</string>
  <key>CFBundleDisplayName</key><string>RustWorks</string>
  <key>CFBundleIdentifier</key><string>dev.rustworks.app</string>
  <key>CFBundleVersion</key><string>${version#v}</string>
  <key>CFBundleShortVersionString</key><string>${version#v}</string>
  <key>CFBundleExecutable</key><string>rustworks</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>RustWorks document</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>CFBundleTypeExtensions</key><array><string>rwpart</string><string>rwasm</string><string>rwdrw</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
fi

cd dist
if [[ -n "$exe" ]]; then
  7z a -tzip "${name}.zip" "${name}" >/dev/null
else
  tar czf "${name}.tar.gz" "${name}"
fi
rm -rf "${name}"
ls -la
