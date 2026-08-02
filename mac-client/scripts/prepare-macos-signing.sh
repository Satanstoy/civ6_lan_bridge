#!/usr/bin/env bash
set -Eeuo pipefail

: "${APPLE_CERTIFICATE:?APPLE_CERTIFICATE is required}"
: "${APPLE_CERTIFICATE_PASSWORD:?APPLE_CERTIFICATE_PASSWORD is required}"
: "${KEYCHAIN_PASSWORD:?KEYCHAIN_PASSWORD is required}"
: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is required}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"
: "${APPLE_APP_PROVISIONING_PROFILE:?APPLE_APP_PROVISIONING_PROFILE is required}"
: "${APPLE_PACKET_TUNNEL_PROVISIONING_PROFILE:?APPLE_PACKET_TUNNEL_PROVISIONING_PROFILE is required}"

SIGNING_DIR="$GITHUB_WORKSPACE/mac-client/signing"
KEYCHAIN_PATH="$RUNNER_TEMP/civ6-lan-bridge-build.keychain-db"
CERTIFICATE_PATH="$RUNNER_TEMP/civ6-lan-bridge-developer-id.p12"
APP_PROFILE_PATH="$SIGNING_DIR/app.provisionprofile"
PACKET_PROFILE_PATH="$SIGNING_DIR/packet-tunnel.provisionprofile"

mkdir -p "$SIGNING_DIR"
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$CERTIFICATE_PATH"
printf '%s' "$APPLE_APP_PROVISIONING_PROFILE" | base64 --decode > "$APP_PROFILE_PATH"
printf '%s' "$APPLE_PACKET_TUNNEL_PROVISIONING_PROFILE" | base64 --decode > "$PACKET_PROFILE_PATH"

security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security set-keychain-settings -lut 21600 "$KEYCHAIN_PATH"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security import "$CERTIFICATE_PATH" \
  -k "$KEYCHAIN_PATH" \
  -P "$APPLE_CERTIFICATE_PASSWORD" \
  -T /usr/bin/codesign \
  -T /usr/bin/security
security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s \
  -k "$KEYCHAIN_PASSWORD" \
  "$KEYCHAIN_PATH"
security list-keychains -d user -s "$KEYCHAIN_PATH" login.keychain-db
security default-keychain -s "$KEYCHAIN_PATH"

if ! security find-identity -v -p codesigning "$KEYCHAIN_PATH" | grep -Fq "$APPLE_SIGNING_IDENTITY"; then
  echo "APPLE_SIGNING_IDENTITY was not found in the imported keychain" >&2
  security find-identity -v -p codesigning "$KEYCHAIN_PATH" >&2 || true
  exit 1
fi

profile_value() {
  local profile_path="$1"
  local key="$2"
  local decoded_path="$RUNNER_TEMP/$(basename "$profile_path").plist"
  security cms -D -i "$profile_path" > "$decoded_path"
  /usr/libexec/PlistBuddy -c "Print :$key" "$decoded_path"
}

APP_PROFILE_NAME="$(profile_value "$APP_PROFILE_PATH" Name)"
APP_PROFILE_UUID="$(profile_value "$APP_PROFILE_PATH" UUID)"
APP_PROFILE_APP_ID="$(profile_value "$APP_PROFILE_PATH" Entitlements:application-identifier)"
PACKET_PROFILE_NAME="$(profile_value "$PACKET_PROFILE_PATH" Name)"
PACKET_PROFILE_UUID="$(profile_value "$PACKET_PROFILE_PATH" UUID)"
PACKET_PROFILE_APP_ID="$(profile_value "$PACKET_PROFILE_PATH" Entitlements:application-identifier)"
APP_NETWORK_EXTENSION="$(profile_value "$APP_PROFILE_PATH" Entitlements:com.apple.developer.networking.networkextension)"
PACKET_NETWORK_EXTENSION="$(profile_value "$PACKET_PROFILE_PATH" Entitlements:com.apple.developer.networking.networkextension)"

EXPECTED_APP_ID="$APPLE_TEAM_ID.com.civ6lanbridge.macos"
EXPECTED_PACKET_ID="$APPLE_TEAM_ID.com.civ6lanbridge.macos.packet-tunnel"
if [[ "$APP_PROFILE_APP_ID" != "$EXPECTED_APP_ID" ]]; then
  echo "App provisioning profile is for $APP_PROFILE_APP_ID, expected $EXPECTED_APP_ID" >&2
  exit 1
fi
if [[ "$PACKET_PROFILE_APP_ID" != "$EXPECTED_PACKET_ID" ]]; then
  echo "Packet Tunnel provisioning profile is for $PACKET_PROFILE_APP_ID, expected $EXPECTED_PACKET_ID" >&2
  exit 1
fi
if [[ "$APP_NETWORK_EXTENSION" != *packet-tunnel-provider* ]]; then
  echo "App provisioning profile does not contain the Packet Tunnel Provider entitlement" >&2
  exit 1
fi
if [[ "$PACKET_NETWORK_EXTENSION" != *packet-tunnel-provider* ]]; then
  echo "Packet Tunnel provisioning profile does not contain the Packet Tunnel Provider entitlement" >&2
  exit 1
fi

{
  echo "MACOS_SIGNING_KEYCHAIN=$KEYCHAIN_PATH"
  echo "MACOS_APP_PROFILE_UUID=$APP_PROFILE_UUID"
  echo "MACOS_APP_PROFILE_NAME=$APP_PROFILE_NAME"
  echo "MACOS_PACKET_TUNNEL_PROFILE_UUID=$PACKET_PROFILE_UUID"
  echo "MACOS_PACKET_TUNNEL_PROFILE_NAME=$PACKET_PROFILE_NAME"
} >> "$GITHUB_ENV"

echo "Developer ID signing identity and both provisioning profiles are ready"
