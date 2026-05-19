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
SECRET=$(get_secret_json APPLE_ASC_JSON infra/apple-asc)

SIGNING_IDENTITY=$(echo "$SECRET" | jq -r '.signing_identity')
KEY_ID=$(echo "$SECRET" | jq -r '.key_id')
ISSUER_ID=$(echo "$SECRET" | jq -r '.issuer_id')
KEY_PATH=$(echo "$SECRET" | jq -r '.key_path')

if [[ ! -f "$KEY_PATH" ]]; then
    # CI path: the key file isn't on disk; pull it from the JSON's `.key`
    # field (CI sets it as plain text in APPLE_ASC_JSON).
    KEY_PATH="$WORKDIR/AuthKey.p8"
    echo "$SECRET" | jq -r '.key_p8 // .key // empty' > "$KEY_PATH"
    [[ -s "$KEY_PATH" ]] || { echo "fatal: notary key not on disk and not in JSON .key" >&2; exit 1; }
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
