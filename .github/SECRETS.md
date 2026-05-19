# GitHub repo secrets for the release pipeline

The release workflow signs and notarizes the macOS bundles, so it needs
Apple Developer credentials. Hosted GitHub runners can't reach the homelab
Vault, so the same JSON payloads we store in Vault are mirrored as
**GitHub repo secrets**.

The build scripts (`build-pkg.sh`, `build-dmg.sh`, `sign.sh`) accept the
creds via either path:

| Vault path                                    | GH secret name              |
|-----------------------------------------------|-----------------------------|
| `secret/infra/apple-asc`                      | `APPLE_ASC_JSON`            |
| `secret/infra/apple-cert-developer-id-installer` | `APPLE_INSTALLER_CERT_JSON` |
| `secret/infra/apple-cert-developer-id-application` | `APPLE_APP_CERT_JSON`     |

## Populate from the homelab (one-shot, run on speedwagon)

```bash
# Apple notary API key + signing identity
vault kv get -format=json secret/infra/apple-asc | jq -c '.data.data' | \
  gh secret set APPLE_ASC_JSON --repo yttfam/apytti

# Developer ID Installer cert (.p12 base64 + password)
vault kv get -format=json secret/infra/apple-cert-developer-id-installer | jq -c '.data.data' | \
  gh secret set APPLE_INSTALLER_CERT_JSON --repo yttfam/apytti

# Developer ID Application cert (.p12 base64 + password)
vault kv get -format=json secret/infra/apple-cert-developer-id-application | jq -c '.data.data' | \
  gh secret set APPLE_APP_CERT_JSON --repo yttfam/apytti
```

## Expected JSON shape

`APPLE_ASC_JSON`:
```json
{
  "key_id": "...",
  "issuer_id": "...",
  "key_path": "/path/on/dev-laptop/AuthKey.p8",
  "key": "-----BEGIN PRIVATE KEY-----\n...p8 contents...\n-----END PRIVATE KEY-----",
  "signing_identity": "Developer ID Application: Nico Bousquet (XJQQCN392F)"
}
```

The scripts try `key_path` first; if the file doesn't exist (CI case), they
fall back to `key` and write it to a temp file. On dev laptops with the key
already on disk, `key` is unused.

`APPLE_INSTALLER_CERT_JSON` / `APPLE_APP_CERT_JSON`:
```json
{
  "p12_base64": "MIIK…",
  "p12_password": "..."
}
```

The .p12 is imported into an ephemeral keychain just for the build —
keychain is deleted by the trap on exit.
