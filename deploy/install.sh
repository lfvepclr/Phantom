#!/bin/bash
set -e

echo "Building phantom-server..."
cargo build --release -p phantom-server

echo "Installing binary..."
cp target/release/phantom-server /usr/local/bin/

# Auto-bootstrap state directory. The systemd unit runs with this as CWD
# so that ./server.key and ./server.toml are created here on first start.
if ! getent group phantom >/dev/null; then
    groupadd --system phantom
fi
if ! id -u phantom >/dev/null 2>&1; then
    useradd --system --gid phantom --home /var/lib/phantom --shell /usr/sbin/nologin phantom
fi
mkdir -p /var/lib/phantom
chown -R phantom:phantom /var/lib/phantom
chmod 750 /var/lib/phantom

# Optional: a default TOML config (kept for users who prefer load mode).
if [ ! -f /etc/phantom/server.toml ]; then
    mkdir -p /etc/phantom
    echo "Installing default config to /etc/phantom/server.toml (for load mode users)..."
    cp config/server.toml /etc/phantom/
fi

echo "Installing systemd service..."
cp deploy/phantom.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable phantom

echo ""
echo "Install complete. Next steps:"
echo "  1. Start the service (auto-bootstrap mode writes /var/lib/phantom/server.key"
echo "     and /var/lib/phantom/server.toml on first run):"
echo "       sudo systemctl start phantom"
echo ""
echo "  2. Fetch the client quick link (URI is the first phantom:// line in the toml):"
echo "       sudo grep '^#   phantom://' /var/lib/phantom/server.toml | sed 's/^#   //'"
echo "       # e.g. phantom://PUBKEY@host:443?cipher=auto&proto=tcp#default"
echo ""
echo "  3. Distribute the URI to clients:"
echo "       phantom client --server \"\$(sudo grep '^#   phantom://' \\\necho "         /var/lib/phantom/server.toml | sed 's/^#   //')\""
echo ""
echo "Notes:"
echo "  - To use a TOML config instead, edit /etc/systemd/system/phantom.service"
echo "    and change ExecStart to:"
echo "      ExecStart=/usr/local/bin/phantom-server /etc/phantom/server.toml"
echo "  - To add a client whitelist, edit /var/lib/phantom/server.toml and add"
echo "    entries to the inline [[allowed_clients]] table, then restart."
echo "  - See deploy/README.md for the full deployment guide."
