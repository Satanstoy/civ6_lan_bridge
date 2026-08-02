#!/usr/bin/env bash
set -Eeuo pipefail

: "${APPLE_ID:?APPLE_ID is required}"
: "${APPLE_PASSWORD:?APPLE_PASSWORD is required}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"
: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is required}"
: "${MACOS_SIGNING_KEYCHAIN:?MACOS_SIGNING_KEYCHAIN is required}"
: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"

APP_PATH="$(find "$GITHUB_WORKSPACE/mac-client/src-tauri/target/release/bundle/macos" -maxdepth 1 -name '*.app' -print -quit)"
if [[ -z "$APP_PATH" ]]; then
  echo "Tauri did not produce a macOS App bundle" >&2
  exit 1
fi

PACKET_TUNNEL_PATH="$APP_PATH/Contents/PlugIns/PacketTunnel.appex"
APP_ENTITLEMENTS="$GITHUB_WORKSPACE/mac-client/src-tauri/Entitlements.plist"
PACKET_TUNNEL_ENTITLEMENTS="$GITHUB_WORKSPACE/mac-client/PacketTunnel/PacketTunnel.entitlements"
DMG_DIR="$GITHUB_WORKSPACE/mac-client/src-tauri/target/release/bundle/dmg"
DMG_PATH="$DMG_DIR/civ6-lan-bridge-macos-arm64.dmg"

test -d "$PACKET_TUNNEL_PATH"
test -f "$APP_PATH/Contents/embedded.provisionprofile"
test -f "$PACKET_TUNNEL_PATH/Contents/embedded.provisionprofile"

codesign --force --options runtime --timestamp \
  --keychain "$MACOS_SIGNING_KEYCHAIN" \
  --entitlements "$PACKET_TUNNEL_ENTITLEMENTS" \
  --sign "$APPLE_SIGNING_IDENTITY" \
  "$PACKET_TUNNEL_PATH"

codesign --force --options runtime --timestamp \
  --keychain "$MACOS_SIGNING_KEYCHAIN" \
  --entitlements "$APP_ENTITLEMENTS" \
  --sign "$APPLE_SIGNING_IDENTITY" \
  "$APP_PATH"

codesign --verify --deep --strict --verbose=4 "$APP_PATH"
codesign -dvvv "$APP_PATH" 2>&1 | tee "$RUNNER_TEMP/civ6-lan-bridge-app-signature.txt"
codesign -dvvv "$PACKET_TUNNEL_PATH" 2>&1 | tee "$RUNNER_TEMP/civ6-lan-bridge-packet-tunnel-signature.txt"

rm -rf "$RUNNER_TEMP/civ6-lan-bridge-dmg-root"
mkdir -p "$RUNNER_TEMP/civ6-lan-bridge-dmg-root" "$DMG_DIR"
ditto "$APP_PATH" "$RUNNER_TEMP/civ6-lan-bridge-dmg-root/Civ6 LAN Bridge.app"
ln -s /Applications "$RUNNER_TEMP/civ6-lan-bridge-dmg-root/Applications"

rm -f "$DMG_PATH"
hdiutil create \
  -volname "Civ6 LAN Bridge" \
  -srcfolder "$RUNNER_TEMP/civ6-lan-bridge-dmg-root" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

xcrun notarytool submit "$DMG_PATH" \
  --apple-id "$APPLE_ID" \
  --password "$APPLE_PASSWORD" \
  --team-id "$APPLE_TEAM_ID" \
  --wait
xcrun stapler staple "$DMG_PATH"
xcrun stapler validate "$DMG_PATH"
spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG_PATH"

echo "Signed and notarized DMG: $DMG_PATH"
