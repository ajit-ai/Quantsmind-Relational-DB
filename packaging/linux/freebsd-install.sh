#!/usr/bin/env bash
# QuantsMind — FreeBSD install script
set -euo pipefail

VERSION="${QMIND_VERSION:-0.1.0}"
INSTALL_DIR="${QMIND_INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${QMIND_DATA_DIR:-$HOME/.qmind}"
REPO="ajit-ai/Quantsmind-Relational-DB"

detect_arch() {
    local arch
    arch=$(uname -m)
    case "$arch" in
        amd64)  echo "x86_64-unknown-freebsd" ;;
        arm64)  echo "aarch64-unknown-freebsd" ;;
        *)      echo "Unsupported architecture: $arch" >&2; exit 1 ;;
    esac
}

main() {
    echo "=== QuantsMind v${VERSION} installer (FreeBSD) ==="
    echo ""

    local arch
    arch=$(detect_arch)
    echo "Architecture: $arch"

    # Install runtime dependencies
    echo "Installing dependencies..."
    pkg install -y openssl 2>/dev/null || true

    # Download release binary
    local tmpdir
    tmpdir=$(mktemp -d)
    local asset="qmind-freebsd-x64-${VERSION}.tar.gz"
    local url="https://github.com/${REPO}/releases/download/v${VERSION}/${asset}"

    echo "Downloading ${asset}..."
    fetch -q -o "${tmpdir}/${asset}" "$url" 2>/dev/null || \
        curl -sSL -o "${tmpdir}/${asset}" "$url"

    tar -xzf "${tmpdir}/${asset}" -C "$tmpdir"
    mkdir -p "$INSTALL_DIR"
    install -m 755 "${tmpdir}/qmind-server" "${INSTALL_DIR}/qmind-server"
    install -m 755 "${tmpdir}/qmind-cli" "${INSTALL_DIR}/qmind-cli"
    rm -rf "$tmpdir"

    mkdir -p "$DATA_DIR"

    # Create rc.d service
    if [ -d /usr/local/etc/rc.d ]; then
        cat > /usr/local/etc/rc.d/qmind <<'EOF'
#!/bin/sh
# PROVIDE: qmind
# REQUIRE: NETWORKING
# KEYWORD: shutdown

. /etc/rc.subr

name="qmind"
rcvar="${name}_enable"
command="/usr/local/bin/qmind-server"
command_args="5432"
pidfile="/var/run/${name}.pid"

load_rc_config $name
run_rc_command "$1"
EOF
        chmod 755 /usr/local/etc/rc.d/qmind
        echo "rc.d service created. Start with: service qmind start"
    fi

    echo ""
    echo "=== Installation complete ==="
    echo ""
    echo "Server:  ${INSTALL_DIR}/qmind-server [PORT]"
    echo "CLI:     ${INSTALL_DIR}/qmind-cli [HOST:PORT]"
    echo "Data:    ${DATA_DIR}"
}

main "$@"
