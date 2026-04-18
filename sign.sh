#!/usr/bin/env bash
# Sign + notarize the macOS apytti binary.
# Pulls signing creds from Vault: secret/infra/apple-asc
#
# Required env: VAULT_ADDR, VAULT_TOKEN
# Required: jq, codesign, ditto, xcrun

set -euo pipefail

cd "$(dirname "$0")"

BINARY="${1:-target/release/apytti}"
IDENTIFIER="net.calii.apytti"
ENTITLEMENTS="$(pwd)/entitlements.plist"
WORKDIR="$(mktemp -d)"
trap "rm -rf $WORKDIR" EXIT

if [[ ! -x "$BINARY" ]]; then
    echo "fatal: binary not found at $BINARY" >&2
    exit 1
fi

if [[ -z "${VAULT_TOKEN:-}" || -z "${VAULT_ADDR:-}" ]]; then
    echo "fatal: VAULT_TOKEN and VAULT_ADDR must be set" >&2
    exit 1
fi

echo "==> Fetching credentials from Vault"
SECRET=$(curl -fsS -H "X-Vault-Token: $VAULT_TOKEN" \
    "$VAULT_ADDR/v1/secret/data/infra/apple-asc" | jq -r '.data.data')

SIGNING_IDENTITY=$(echo "$SECRET" | jq -r '.signing_identity')
KEY_ID=$(echo "$SECRET" | jq -r '.key_id')
ISSUER_ID=$(echo "$SECRET" | jq -r '.issuer_id')
KEY_PATH=$(echo "$SECRET" | jq -r '.key_path')

if [[ ! -f "$KEY_PATH" ]]; then
    echo "fatal: API key not found at $KEY_PATH" >&2
    exit 1
fi

echo "==> Signing $BINARY"
codesign --force --options runtime --timestamp \
    --sign "$SIGNING_IDENTITY" \
    --identifier "$IDENTIFIER" \
    --entitlements "$ENTITLEMENTS" \
    "$BINARY"

echo "==> Verifying signature"
codesign --verify --verbose=2 "$BINARY"

echo "==> Zipping for notarization"
ZIP="$WORKDIR/apytti.zip"
ditto -c -k --keepParent "$BINARY" "$ZIP"

echo "==> Submitting to Apple notary service (this can take a few minutes)"
xcrun notarytool submit "$ZIP" \
    --key "$KEY_PATH" \
    --key-id "$KEY_ID" \
    --issuer "$ISSUER_ID" \
    --wait

echo
echo "Done. Binary signed and notarized."
echo "Note: raw binaries can't be stapled — Gatekeeper checks online on first launch."
