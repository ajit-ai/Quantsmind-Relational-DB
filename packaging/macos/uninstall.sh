#!/usr/bin/env bash
# QuantsMind — macOS uninstall script
set -euo pipefail

INSTALL_DIR="${QMIND_INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${QMIND_DATA_DIR:-$HOME/.qmind}"

echo "=== QuantsMind uninstall (macOS) ==="

# Unload launchd agent
if launchctl list | grep -q com.quantsmind.server 2>/dev/null; then
    launchctl unload ~/Library/LaunchAgents/com.quantsmind.server.plist 2>/dev/null || true
    echo "Unloaded launchd agent"
fi
rm -f ~/Library/LaunchAgents/com.quantsmind.server.plist

# Remove binaries
for bin in qmind-server qmind-cli; do
    if [ -f "${INSTALL_DIR}/${bin}" ]; then
        sudo rm "${INSTALL_DIR}/${bin}"
        echo "Removed ${INSTALL_DIR}/${bin}"
    fi
done

# Remove data
if [ -d "$DATA_DIR" ]; then
    read -p "Remove data directory ${DATA_DIR}? [y/N] " confirm
    if [ "$confirm" = "y" ] || [ "$confirm" = "Y" ]; then
        rm -rf "$DATA_DIR"
        echo "Removed ${DATA_DIR}"
    fi
fi

echo "=== Uninstall complete ==="
