#!/usr/bin/env bash
# Deploy apytti to staging.calii.lan (10.10.0.14).
#
# Builds the linux-musl binary, ships it to /opt/apytti, installs the
# apytti-staging.service systemd unit (matching grytti-staging conventions),
# and restarts the service. Idempotent.
#
# Hermytt-staging lives on the same VM at :7777, so apytti announces to
# http://localhost:7777 by default.

set -euo pipefail

cd "$(dirname "$0")"

HOST="${APYTTI_DEPLOY_HOST:-staging}"
TARGET="x86_64-unknown-linux-musl"
REMOTE_DIR="/opt/apytti"
SERVICE_NAME="apytti-staging"
PORT="${APYTTI_PORT:-7781}"
BIND="${APYTTI_BIND:-0.0.0.0}"
HERMYTT_URL="${APYTTI_HERMYTT_URL:-http://localhost:7777}"

echo "==> Building for $TARGET (release)"
cargo build --release --target "$TARGET"

# Resolve binary path through cargo metadata so this works with shared target dirs.
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['target_directory'])" 2>/dev/null \
    || echo "target")
BINARY="$TARGET_DIR/$TARGET/release/apytti"
[[ -x "$BINARY" ]] || { echo "fatal: binary missing at $BINARY"; exit 1; }
SIZE=$(du -h "$BINARY" | cut -f1)
echo "==> Binary: $BINARY ($SIZE)"

echo "==> Ensuring remote dir on $HOST"
ssh "$HOST" "sudo mkdir -p $REMOTE_DIR && sudo chown cali:cali $REMOTE_DIR"

echo "==> Uploading binary (atomic via .new + mv)"
scp -q "$BINARY" "$HOST:$REMOTE_DIR/apytti.new"
ssh "$HOST" "chmod +x $REMOTE_DIR/apytti.new && mv $REMOTE_DIR/apytti.new $REMOTE_DIR/apytti"

echo "==> Seeding config.toml if not present"
if ! ssh "$HOST" "test -f $REMOTE_DIR/config.toml"; then
    ssh "$HOST" "tee $REMOTE_DIR/config.toml > /dev/null" <<EOF
# apytti staging config — managed by deploy-staging.sh on first boot.
# After deploy, edit live via PUT /config (hermytt UI) or apytti setup.

# active = "ollama"  # uncomment after enabling at least one backend

[backends.ollama]
enabled = true
endpoint = "http://10.10.0.13:11434"
model = "mistral:7b"
resume = true

[hermytt]
url = "$HERMYTT_URL"
endpoint = "http://staging:$PORT"
EOF
fi

echo "==> Installing systemd unit"
ssh "$HOST" "sudo tee /etc/systemd/system/$SERVICE_NAME.service > /dev/null" <<UNIT
[Unit]
Description=apytti staging — multi-backend AI gateway
After=hermytt-staging.service network-online.target
Wants=hermytt-staging.service

[Service]
Type=simple
User=cali
Group=cali
WorkingDirectory=$REMOTE_DIR
ExecStart=$REMOTE_DIR/apytti --config $REMOTE_DIR/config.toml run --port $PORT --host $BIND
Restart=always
RestartSec=3
Environment=RUST_LOG=info,apytti=info

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=$REMOTE_DIR
PrivateTmp=true

[Install]
WantedBy=multi-user.target
UNIT

echo "==> Reloading systemd, enabling and restarting $SERVICE_NAME"
ssh "$HOST" "sudo systemctl daemon-reload && sudo systemctl enable $SERVICE_NAME && sudo systemctl restart $SERVICE_NAME"

echo "==> Waiting 2s for startup"
sleep 2

echo "==> Health check"
ssh "$HOST" "curl -fsS http://127.0.0.1:$PORT/health" \
    || echo "  warning: /health didn't respond yet, check 'journalctl -u $SERVICE_NAME -n 50'"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
echo
echo "Deployed apytti $VERSION to $HOST"
echo "  binary: $REMOTE_DIR/apytti"
echo "  config: $REMOTE_DIR/config.toml"
echo "  unit:   /etc/systemd/system/$SERVICE_NAME.service"
echo "  health: http://staging:$PORT/health"
echo "  logs:   ssh $HOST 'journalctl -u $SERVICE_NAME -f'"
