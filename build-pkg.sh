#!/usr/bin/env bash
# Build a signed + notarized + stapled .pkg installer for apytti.
# Uses the already-signed binary from sign.sh.
#
# Pulls signing creds from Vault:
#   secret/infra/apple-asc                       - notary API key
#   secret/infra/apple-cert-developer-id-installer - Developer ID Installer p12
#
# Required env: VAULT_ADDR, VAULT_TOKEN
# Required tools: jq, codesign, pkgbuild, productsign, xcrun

set -euo pipefail

cd "$(dirname "$0")"

BINARY="${1:-target/release/apytti}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
IDENTIFIER="net.calii.apytti"
PKG_OUT="apytti-${VERSION}-arm64.pkg"

WORKDIR="$(mktemp -d)"
KEYCHAIN_PATH="$WORKDIR/sign.keychain-db"
KEYCHAIN_PASSWORD="apytti-build"
trap 'security delete-keychain "$KEYCHAIN_PATH" 2>/dev/null; rm -rf "$WORKDIR"' EXIT

if [[ ! -x "$BINARY" ]]; then
    echo "fatal: binary not found at $BINARY" >&2
    exit 1
fi

if [[ -z "${VAULT_TOKEN:-}" || -z "${VAULT_ADDR:-}" ]]; then
    echo "fatal: VAULT_TOKEN and VAULT_ADDR must be set" >&2
    exit 1
fi

echo "==> Fetching credentials from Vault"
ASC=$(curl -fsS -H "X-Vault-Token: $VAULT_TOKEN" \
    "$VAULT_ADDR/v1/secret/data/infra/apple-asc" | jq -r '.data.data')
INSTALLER=$(curl -fsS -H "X-Vault-Token: $VAULT_TOKEN" \
    "$VAULT_ADDR/v1/secret/data/infra/apple-cert-developer-id-installer" | jq -r '.data.data')

KEY_ID=$(echo "$ASC" | jq -r '.key_id')
ISSUER_ID=$(echo "$ASC" | jq -r '.issuer_id')
KEY_PATH=$(echo "$ASC" | jq -r '.key_path')

INSTALLER_P12_B64=$(echo "$INSTALLER" | jq -r '.p12_base64')
INSTALLER_P12_PW=$(echo "$INSTALLER" | jq -r '.p12_password')

if [[ ! -f "$KEY_PATH" ]]; then
    echo "==> notary API key not at $KEY_PATH; writing from Vault"
    KEY_PATH="$WORKDIR/AuthKey.p8"
    echo "$ASC" | jq -r '.key // empty' > "$KEY_PATH"
    if [[ ! -s "$KEY_PATH" ]]; then
        echo "fatal: notary key not in Vault and not on disk" >&2
        exit 1
    fi
fi

echo "==> Creating temporary keychain for installer cert"
P12_PATH="$WORKDIR/installer.p12"
echo "$INSTALLER_P12_B64" | base64 -d > "$P12_PATH"

security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security set-keychain-settings -lut 1800 "$KEYCHAIN_PATH"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"

# Import to the temp keychain only
security import "$P12_PATH" -k "$KEYCHAIN_PATH" -P "$INSTALLER_P12_PW" \
    -T /usr/bin/productsign -T /usr/bin/codesign

# Allow non-interactive use
security set-key-partition-list -S apple-tool:,apple:,codesign:,productsign: \
    -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH" >/dev/null 2>&1

# Add temp keychain to search list (preserve existing)
ORIG_KEYCHAINS=$(security list-keychains -d user | tr -d '"' | xargs)
security list-keychains -d user -s "$KEYCHAIN_PATH" $ORIG_KEYCHAINS

INSTALLER_IDENTITY=$(security find-identity -v -p basic "$KEYCHAIN_PATH" \
    | grep "Developer ID Installer" | head -1 | awk -F'"' '{print $2}')

if [[ -z "$INSTALLER_IDENTITY" ]]; then
    echo "fatal: Developer ID Installer identity not found in temp keychain" >&2
    security find-identity -v "$KEYCHAIN_PATH"
    exit 1
fi

echo "==> Installer identity: $INSTALLER_IDENTITY"

echo "==> Staging payload"
STAGE="$WORKDIR/stage"
mkdir -p "$STAGE/usr/local/bin"
cp "$BINARY" "$STAGE/usr/local/bin/apytti"
chmod 755 "$STAGE/usr/local/bin/apytti"

echo "==> Building unsigned component pkg"
COMPONENT="$WORKDIR/component.pkg"
pkgbuild --root "$STAGE" \
    --identifier "$IDENTIFIER" \
    --version "$VERSION" \
    --install-location "/" \
    "$COMPONENT"

echo "==> Signing pkg with Developer ID Installer"
productsign --sign "$INSTALLER_IDENTITY" --keychain "$KEYCHAIN_PATH" \
    "$COMPONENT" "$PKG_OUT"

echo "==> Verifying pkg signature"
pkgutil --check-signature "$PKG_OUT" | head -10

echo "==> Submitting pkg for notarization"
xcrun notarytool submit "$PKG_OUT" \
    --key "$KEY_PATH" \
    --key-id "$KEY_ID" \
    --issuer "$ISSUER_ID" \
    --wait

echo "==> Stapling notarization ticket to pkg"
xcrun stapler staple "$PKG_OUT"
xcrun stapler validate "$PKG_OUT"

# Restore original keychain search list
security list-keychains -d user -s $ORIG_KEYCHAINS

echo
echo "Built and notarized: $PKG_OUT"
ls -lh "$PKG_OUT"
