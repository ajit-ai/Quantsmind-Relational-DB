#!/usr/bin/env bash
# QuantsMind — Linux uninstall script
set -euo pipefail

INSTALL_DIR="${QMIND_INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${QMIND_DATA_DIR:-$HOME/.qmind}"

echo "=== QuantsMind uninstall ==="

# Stop and remove systemd service
if systemctl is-active --quiet qmind 2>/dev/null; then
    sudo systemctl stop qmind
    echo "Stopped qmind service"
fi
if [ -f /etc/systemd/system/qmind.service ]; then
    sudo rm /etc/systemd/system/qmind.service
    sudo systemctl daemon-reload
    echo "Removed systemd service"
fi

# Remove binaries
for bin in qmind-server qmind-cli; do
    if [ -f "${INSTALL_DIR}/${bin}" ]; then
        sudo rm "${INSTALL_DIR}/${bin}"
        echo "Removed ${INSTALL_DIR}/${bin}"
    fi
done

# Remove data (ask first)
if [ -d "$DATA_DIR" ]; then
    read -p "Remove data directory ${DATA_DIR}? [y/N] " confirm
    if [ "$confirm" = "y" ] || [ "$confirm" = "Y" ]; then
        rm -rf "$DATA_DIR"
        echo "Removed ${DATA_DIR}"
    fi
fi

echo "=== Uninstall complete ==="
