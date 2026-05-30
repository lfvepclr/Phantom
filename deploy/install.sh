#!/bin/bash
set -e

echo "Building phantom-server..."
cargo build --release -p phantom-server

echo "Installing binary..."
cp target/release/phantom-server /usr/local/bin/

echo "Creating config directory..."
mkdir -p /etc/phantom

if [ ! -f /etc/phantom/server.toml ]; then
    echo "Installing default config..."
    cp config/server.toml /etc/phantom/
fi

echo "Installing systemd service..."
cp deploy/phantom.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable phantom

echo ""
echo "Install complete. Next steps:"
echo "  1. Run 'phantom keygen -o /etc/phantom' to generate keys"
echo "  2. Edit /etc/phantom/server.toml with your key file path"
echo "  3. Run 'systemctl start phantom' to start the server"
