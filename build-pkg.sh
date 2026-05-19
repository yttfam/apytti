#!/usr/bin/env bash
# Build Apytti.app, sign it, wrap in a signed/notarized/stapled .pkg.
#
# This is the production pipeline used by both local dev (`./build-pkg.sh`)
# and CI (`.gitea/workflows/release.yaml`). All Apple credentials come from
# Vault — no per-machine keychain profile required.
#
# Vault paths consumed:
#   secret/infra/apple-asc                            — notary API key (.p8)
#   secret/infra/apple-cert-developer-id-installer    — Installer cert .p12
#
# Per palazzo memory id=1777633948413: bundle ID is `net.calii.apytti.app`
# (with .app suffix) so a fresh TCC decision path is available; pkg receipt
# id stays `net.calii.apytti` to keep installer history clean.
#
# Required env: VAULT_ADDR, VAULT_TOKEN
# Required tools: jq, codesign, pkgbuild, productsign, xcrun

set -euo pipefail
cd "$(dirname "$0")"

BINARY_PATH="${1:-target/release/apytti}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
APP_BUNDLE_ID="net.calii.apytti.app"
PKG_RECEIPT_ID="net.calii.apytti"
PKG_OUT="target/apytti-${VERSION}.pkg"
APP_BUNDLE="target/Apytti.app"
PKG_ROOT="target/pkg-root"

WORKDIR="$(mktemp -d)"
KEYCHAIN_PATH="$WORKDIR/sign.keychain-db"
KEYCHAIN_PASSWORD="apytti-build"
trap 'security delete-keychain "$KEYCHAIN_PATH" 2>/dev/null; rm -rf "$WORKDIR"' EXIT

[[ -x "$BINARY_PATH" ]] || { echo "fatal: binary not found at $BINARY_PATH"; exit 1; }

# Credentials source: env JSON (CI) -> Vault (local dev). Both paths produce
# the same shape so downstream jq calls don't care which one wins.
get_secret_json() {
    local env_var="$1" vault_path="$2"
    local value="${!env_var:-}"
    if [[ -n "$value" ]]; then
        echo "$value"; return 0
    fi
    if [[ -n "${VAULT_TOKEN:-}" && -n "${VAULT_ADDR:-}" ]]; then
        curl -fsS -H "X-Vault-Token: $VAULT_TOKEN" \
            "$VAULT_ADDR/v1/secret/data/$vault_path" | jq -r '.data.data'
        return $?
    fi
    echo "fatal: need $env_var env var, or VAULT_TOKEN+VAULT_ADDR with $vault_path" >&2
    return 1
}

echo "==> Resolving credentials"
ASC=$(get_secret_json APPLE_ASC_JSON infra/apple-asc)
INSTALLER=$(get_secret_json APPLE_INSTALLER_CERT_JSON infra/apple-cert-developer-id-installer)

KEY_ID=$(echo "$ASC" | jq -r '.key_id')
ISSUER_ID=$(echo "$ASC" | jq -r '.issuer_id')
KEY_PATH=$(echo "$ASC" | jq -r '.key_path')
APP_SIGNING_IDENTITY=$(echo "$ASC" | jq -r '.signing_identity')

INSTALLER_P12_B64=$(echo "$INSTALLER" | jq -r '.p12_base64')
INSTALLER_P12_PW=$(echo "$INSTALLER" | jq -r '.p12_password')

if [[ ! -f "$KEY_PATH" ]]; then
    KEY_PATH="$WORKDIR/AuthKey.p8"
    echo "$ASC" | jq -r '.key // empty' > "$KEY_PATH"
    [[ -s "$KEY_PATH" ]] || { echo "fatal: notary key not on disk and not in Vault"; exit 1; }
fi

echo "==> Creating temporary keychain for installer cert"
P12_PATH="$WORKDIR/installer.p12"
echo "$INSTALLER_P12_B64" | base64 -d > "$P12_PATH"

security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security set-keychain-settings -lut 1800 "$KEYCHAIN_PATH"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security import "$P12_PATH" -k "$KEYCHAIN_PATH" -P "$INSTALLER_P12_PW" \
    -T /usr/bin/codesign -T /usr/bin/pkgbuild -T /usr/bin/productsign -T /usr/bin/productbuild
security set-key-partition-list -S apple-tool:,apple:,codesign:,pkgbuild:,productsign:,productbuild: \
    -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH" >/dev/null 2>&1

ORIG_KEYCHAINS=$(security list-keychains -d user | tr -d '"' | xargs)
security list-keychains -d user -s "$KEYCHAIN_PATH" $ORIG_KEYCHAINS

INSTALLER_IDENTITY=$(security find-identity -v -p basic "$KEYCHAIN_PATH" \
    | grep "Developer ID Installer" | head -1 | awk -F'"' '{print $2}')
[[ -n "$INSTALLER_IDENTITY" ]] || { echo "fatal: installer identity not found"; exit 1; }

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
    "$APP_BUNDLE/Contents/MacOS/apytti"
codesign --force --options runtime --timestamp \
    --sign "$APP_SIGNING_IDENTITY" \
    --identifier "$APP_BUNDLE_ID" \
    --entitlements entitlements.plist \
    "$APP_BUNDLE"
codesign -dvvv "$APP_BUNDLE" 2>&1 | sed -n '1,5p' || true

echo "==> Staging pkg root (.app + /usr/local/bin symlink)"
rm -rf "$PKG_ROOT"
mkdir -p "$PKG_ROOT/Applications" "$PKG_ROOT/usr/local/bin"
cp -R "$APP_BUNDLE" "$PKG_ROOT/Applications/Apytti.app"
ln -sf /Applications/Apytti.app/Contents/MacOS/apytti "$PKG_ROOT/usr/local/bin/apytti"

echo "==> Building signed pkg"
# Component plist with BundleIsRelocatable=false: macOS otherwise "relocates"
# the install to any existing copy of net.calii.apytti.app on disk (e.g. the
# build staging dir target/Apytti.app), shoving the new bits there instead of
# /Applications/. This locks the install path to /Applications/Apytti.app.
COMPONENT_PLIST="$WORKDIR/component.plist"
cat > "$COMPONENT_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
  <dict>
    <key>BundleHasStrictIdentifier</key><true/>
    <key>BundleIsRelocatable</key><false/>
    <key>BundleIsVersionChecked</key><true/>
    <key>BundleOverwriteAction</key><string>upgrade</string>
    <key>RootRelativeBundlePath</key><string>Applications/Apytti.app</string>
  </dict>
</array>
</plist>
EOF

pkgbuild --root "$PKG_ROOT" \
    --component-plist "$COMPONENT_PLIST" \
    --identifier "$PKG_RECEIPT_ID" \
    --version "$VERSION" \
    --install-location "/" \
    --scripts bundle/scripts \
    --sign "$INSTALLER_IDENTITY" \
    --keychain "$KEYCHAIN_PATH" \
    "$PKG_OUT"

echo "==> Verifying pkg signature"
pkgutil --check-signature "$PKG_OUT" | sed -n '1,8p' || true

echo "==> Submitting pkg for notarization"
xcrun notarytool submit "$PKG_OUT" \
    --key "$KEY_PATH" \
    --key-id "$KEY_ID" \
    --issuer "$ISSUER_ID" \
    --wait

echo "==> Stapling notarization ticket"
xcrun stapler staple "$PKG_OUT"
xcrun stapler validate "$PKG_OUT"

security list-keychains -d user -s $ORIG_KEYCHAINS

# Clean staging dirs so LaunchServices doesn't index them as spurious copies
# of net.calii.apytti.app (which would also confuse PackageKit's bundle
# relocation, even with BundleIsRelocatable=false in the component plist).
rm -rf "$APP_BUNDLE" "$PKG_ROOT"

echo
echo "Done: $PKG_OUT"
ls -lh "$PKG_OUT"
