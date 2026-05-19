#!/usr/bin/env bash
# Build a signed/notarized/stapled .app, wrap it in a signed/notarized/stapled .dmg.
#
# Distinct from build-pkg.sh: a DMG ships a standalone .app the user
# drags to /Applications themselves, so the .app inside MUST be
# independently notarized + stapled (no .pkg installer to vouch for it).
# We then also notarize+staple the .dmg itself for clean Gatekeeper UX.
#
# Vault paths consumed:
#   secret/infra/apple-asc                            — notary API key (.p8)
#   secret/infra/apple-cert-developer-id-installer    — Installer cert .p12
#                                                       (we use the Application
#                                                        cert from the same keychain)
#
# Required env: VAULT_ADDR, VAULT_TOKEN
# Required tools: jq, codesign, hdiutil, xcrun, ditto

set -euo pipefail
cd "$(dirname "$0")"

BINARY_PATH="${1:-target/release/apytti}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
APP_BUNDLE_ID="net.calii.apytti.app"
DMG_OUT="target/apytti-${VERSION}.dmg"
APP_BUNDLE="target/Apytti.app"
DMG_STAGE="target/dmg-stage"

WORKDIR="$(mktemp -d)"
KEYCHAIN_PATH="$WORKDIR/sign.keychain-db"
KEYCHAIN_PASSWORD="apytti-build"
trap 'security delete-keychain "$KEYCHAIN_PATH" 2>/dev/null; rm -rf "$WORKDIR" "$DMG_STAGE"' EXIT

[[ -x "$BINARY_PATH" ]] || { echo "fatal: binary not found at $BINARY_PATH"; exit 1; }
[[ -n "${VAULT_TOKEN:-}" && -n "${VAULT_ADDR:-}" ]] || { echo "fatal: VAULT_TOKEN+VAULT_ADDR required"; exit 1; }

echo "==> Fetching credentials from Vault"
ASC=$(curl -fsS -H "X-Vault-Token: $VAULT_TOKEN" "$VAULT_ADDR/v1/secret/data/infra/apple-asc" | jq -r '.data.data')
APP_CERT=$(curl -fsS -H "X-Vault-Token: $VAULT_TOKEN" "$VAULT_ADDR/v1/secret/data/infra/apple-cert-developer-id-application" | jq -r '.data.data' 2>/dev/null || echo "")

KEY_ID=$(echo "$ASC" | jq -r '.key_id')
ISSUER_ID=$(echo "$ASC" | jq -r '.issuer_id')
KEY_PATH=$(echo "$ASC" | jq -r '.key_path')
APP_SIGNING_IDENTITY=$(echo "$ASC" | jq -r '.signing_identity')

if [[ ! -f "$KEY_PATH" ]]; then
    KEY_PATH="$WORKDIR/AuthKey.p8"
    echo "$ASC" | jq -r '.key // empty' > "$KEY_PATH"
    [[ -s "$KEY_PATH" ]] || { echo "fatal: notary key not on disk and not in Vault"; exit 1; }
fi

# If we have the Application cert as p12 in Vault, import it into a temp
# keychain (CI runners may not have it persistently). Otherwise rely on the
# identity already being in the default keychain (dev-laptop path).
if [[ -n "$APP_CERT" && "$APP_CERT" != "null" ]]; then
    echo "==> Importing Developer ID Application cert into temp keychain"
    APP_P12_B64=$(echo "$APP_CERT" | jq -r '.p12_base64')
    APP_P12_PW=$(echo "$APP_CERT" | jq -r '.p12_password')
    P12_PATH="$WORKDIR/app.p12"
    echo "$APP_P12_B64" | base64 -d > "$P12_PATH"

    security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
    security set-keychain-settings -lut 1800 "$KEYCHAIN_PATH"
    security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
    security import "$P12_PATH" -k "$KEYCHAIN_PATH" -P "$APP_P12_PW" -T /usr/bin/codesign
    security set-key-partition-list -S apple-tool:,apple:,codesign: \
        -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH" >/dev/null 2>&1
    ORIG_KEYCHAINS=$(security list-keychains -d user | tr -d '"' | xargs)
    security list-keychains -d user -s "$KEYCHAIN_PATH" $ORIG_KEYCHAINS
    KEYCHAIN_FLAG=(--keychain "$KEYCHAIN_PATH")
else
    KEYCHAIN_FLAG=()
fi

echo "==> Building Apytti.app bundle"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
cp "$BINARY_PATH" "$APP_BUNDLE/Contents/MacOS/apytti"
chmod 755 "$APP_BUNDLE/Contents/MacOS/apytti"
sed "s/__VERSION__/$VERSION/g" bundle/Info.plist.template > "$APP_BUNDLE/Contents/Info.plist"

echo "==> Codesigning .app with Developer ID Application"
codesign --force --options runtime --timestamp \
    --sign "$APP_SIGNING_IDENTITY" \
    --identifier "$APP_BUNDLE_ID" \
    --entitlements entitlements.plist \
    "${KEYCHAIN_FLAG[@]}" \
    "$APP_BUNDLE/Contents/MacOS/apytti"
codesign --force --options runtime --timestamp \
    --sign "$APP_SIGNING_IDENTITY" \
    --identifier "$APP_BUNDLE_ID" \
    --entitlements entitlements.plist \
    "${KEYCHAIN_FLAG[@]}" \
    "$APP_BUNDLE"

echo "==> Notarizing the .app (ditto zip → notarytool → staple)"
APP_ZIP="$WORKDIR/Apytti.zip"
ditto -c -k --keepParent "$APP_BUNDLE" "$APP_ZIP"
xcrun notarytool submit "$APP_ZIP" \
    --key "$KEY_PATH" \
    --key-id "$KEY_ID" \
    --issuer "$ISSUER_ID" \
    --wait
xcrun stapler staple "$APP_BUNDLE"
xcrun stapler validate "$APP_BUNDLE"

echo "==> Staging DMG contents"
rm -rf "$DMG_STAGE"
mkdir -p "$DMG_STAGE"
cp -R "$APP_BUNDLE" "$DMG_STAGE/Apytti.app"
ln -s /Applications "$DMG_STAGE/Applications"

echo "==> Building DMG (UDZO, compressed read-only)"
rm -f "$DMG_OUT"
hdiutil create \
    -volname "Apytti $VERSION" \
    -srcfolder "$DMG_STAGE" \
    -ov \
    -format UDZO \
    "$DMG_OUT"

echo "==> Codesigning the .dmg"
codesign --force --timestamp \
    --sign "$APP_SIGNING_IDENTITY" \
    "${KEYCHAIN_FLAG[@]}" \
    "$DMG_OUT"

echo "==> Notarizing the .dmg"
xcrun notarytool submit "$DMG_OUT" \
    --key "$KEY_PATH" \
    --key-id "$KEY_ID" \
    --issuer "$ISSUER_ID" \
    --wait
xcrun stapler staple "$DMG_OUT"
xcrun stapler validate "$DMG_OUT"

if [[ -n "${ORIG_KEYCHAINS:-}" ]]; then
    security list-keychains -d user -s $ORIG_KEYCHAINS
fi

rm -rf "$APP_BUNDLE" "$DMG_STAGE"

echo
echo "Done: $DMG_OUT"
ls -lh "$DMG_OUT"
