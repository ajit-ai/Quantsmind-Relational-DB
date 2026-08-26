#!/usr/bin/env bash
# QuantsMind — macOS install script
# Supports: Homebrew formula, direct binary download
# Usage: curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/macos/install.sh | bash

set -euo pipefail

VERSION="${QMIND_VERSION:-0.1.0}"
INSTALL_DIR="${QMIND_INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${QMIND_DATA_DIR:-$HOME/.qmind}"
REPO="ajit-ai/Quantsmind-Relational-DB"

detect_arch() {
    local arch
    arch=$(uname -m)
    case "$arch" in
        arm64)  echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *)      echo "Unsupported architecture: $arch" >&2; exit 1 ;;
    esac
}

download_binary() {
    local arch=$1
    local tmpdir
    tmpdir=$(mktemp -d)

    local asset="qmind-macos-arm64-${VERSION}.tar.gz"
    case "$arch" in
        x86_64*) asset="qmind-macos-x64-${VERSION}.tar.gz" ;;
        *)       asset="qmind-macos-arm64-${VERSION}.tar.gz" ;;
    esac

    local url="https://github.com/${REPO}/releases/download/v${VERSION}/${asset}"
    echo "Downloading ${asset}..."

    curl -sSL -o "${tmpdir}/${asset}" "$url"
    tar -xzf "${tmpdir}/${asset}" -C "$tmpdir"

    mkdir -p "$INSTALL_DIR"
    install -m 755 "${tmpdir}/qmind-server" "${INSTALL_DIR}/qmind-server"
    install -m 755 "${tmpdir}/qmind-cli" "${INSTALL_DIR}/qmind-cli"

    rm -rf "$tmpdir"
}

main() {
    echo "=== QuantsMind v${VERSION} installer (macOS) ==="
    echo ""

    local arch
    arch=$(detect_arch)
    echo "Architecture: $arch"

    download_binary "$arch"
    mkdir -p "$DATA_DIR"

    # Create launchd plist for auto-start
    local plist_dir="$HOME/Library/LaunchAgents"
    mkdir -p "$plist_dir"
    cat > "${plist_dir}/com.quantsmind.server.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.quantsmind.server</string>
    <key>ProgramArguments</key>
    <array>
        <string>${INSTALL_DIR}/qmind-server</string>
        <string>5432</string>
    </array>
    <key>RunAtLoad</key>
    <false/>
    <key>KeepAlive</key>
    <false/>
    <key>WorkingDirectory</key>
    <string>${DATA_DIR}</string>
    <key>StandardOutPath</key>
    <string>${DATA_DIR}/qmind.log</string>
    <key>StandardErrorPath</key>
    <string>${DATA_DIR}/qmind.log</string>
</dict>
</plist>
EOF

    echo ""
    echo "=== Installation complete ==="
    echo ""
    echo "Server:  ${INSTALL_DIR}/qmind-server [PORT]"
    echo "CLI:     ${INSTALL_DIR}/qmind-cli [HOST:PORT]"
    echo "Data:    ${DATA_DIR}"
    echo ""
    echo "Quick start:"
    echo "  qmind-server 5432            # start server on port 5432"
    echo "  qmind-cli 127.0.0.1:5432     # connect with CLI"
    echo ""
    echo "Auto-start (optional):"
    echo "  launchctl load ~/Library/LaunchAgents/com.quantsmind.server.plist"
    echo "  launchctl start com.quantsmind.server"
}

main "$@"
