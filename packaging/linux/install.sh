#!/usr/bin/env bash
# QuantsMind — Linux install script
# Supports: Debian/Ubuntu (apt), Fedora/RHEL (dnf), Arch (pacman), Alpine (apk)
# Usage: curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/linux/install.sh | bash

set -euo pipefail

VERSION="${QMIND_VERSION:-0.1.0}"
INSTALL_DIR="${QMIND_INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${QMIND_DATA_DIR:-$HOME/.qmind}"
REPO="ajit-ai/Quantsmind-Relational-DB"
BINARY="qmind-server"

detect_arch() {
    local arch
    arch=$(uname -m)
    case "$arch" in
        x86_64|amd64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
        armv7l)        echo "armv7-unknown-linux-gnueabihf" ;;
        *)             echo "Unsupported architecture: $arch" >&2; exit 1 ;;
    esac
}

detect_distro() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        echo "$ID"
    elif [ -f /etc/alpine-release ]; then
        echo "alpine"
    else
        echo "unknown"
    fi
}

install_deps() {
    local distro
    distro=$(detect_distro)
    echo "Detected distro: $distro"

    case "$distro" in
        ubuntu|debian)
            sudo apt-get update -qq
            sudo apt-get install -y -qq libssl-dev 2>/dev/null || true
            ;;
        fedora|rhel|centos|rocky|alma)
            sudo dnf install -y openssl-devel 2>/dev/null || true
            ;;
        arch|manjaro)
            sudo pacman -S --noconfirm openssl 2>/dev/null || true
            ;;
        alpine)
            sudo apk add --no-cache openssl 2>/dev/null || true
            ;;
        opensuse*|sles)
            sudo zypper install -y libopenssl-devel 2>/dev/null || true
            ;;
        *)
            echo "Warning: could not detect distro, skipping dependency install" >&2
            ;;
    esac
}

download_binary() {
    local arch=$1
    local tmpdir
    tmpdir=$(mktemp -d)

    local asset="qmind-linux-x64-${VERSION}.tar.gz"
    case "$arch" in
        aarch64*) asset="qmind-linux-arm64-${VERSION}.tar.gz" ;;
        *)        asset="qmind-linux-x64-${VERSION}.tar.gz" ;;
    esac

    local url="https://github.com/${REPO}/releases/download/v${VERSION}/${asset}"
    echo "Downloading ${asset}..."

    if command -v curl &>/dev/null; then
        curl -sSL -o "${tmpdir}/${asset}" "$url"
    elif command -v wget &>/dev/null; then
        wget -q -O "${tmpdir}/${asset}" "$url"
    else
        echo "Error: curl or wget required" >&2
        exit 1
    fi

    echo "Extracting..."
    tar -xzf "${tmpdir}/${asset}" -C "$tmpdir"
    mkdir -p "$INSTALL_DIR"
    sudo install -m 755 "${tmpdir}/qmind-server" "${INSTALL_DIR}/qmind-server" 2>/dev/null || \
        sudo cp "${tmpdir}/qmind-server" "${INSTALL_DIR}/qmind-server"
    sudo install -m 755 "${tmpdir}/qmind-cli" "${INSTALL_DIR}/qmind-cli" 2>/dev/null || \
        sudo cp "${tmpdir}/qmind-cli" "${INSTALL_DIR}/qmind-cli"

    rm -rf "$tmpdir"
}

setup_data_dir() {
    mkdir -p "$DATA_DIR"
    echo "Data directory: $DATA_DIR"
}

create_systemd_service() {
    if [ -d /etc/systemd/system ]; then
        sudo tee /etc/systemd/system/qmind.service > /dev/null <<EOF
[Unit]
Description=QuantsMind Database Server
After=network.target

[Service]
Type=simple
User=$USER
ExecStart=${INSTALL_DIR}/qmind-server 5432
WorkingDirectory=${DATA_DIR}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
        sudo systemctl daemon-reload
        echo "Systemd service created. Start with: sudo systemctl start qmind"
        echo "Enable on boot: sudo systemctl enable qmind"
    fi
}

create_deb() {
    local pkg_dir
    pkg_dir=$(mktemp -d)
    local pkg_name="qmind_${VERSION}_$(dpkg --print-architecture)"

    mkdir -p "${pkg_dir}/DEBIAN"
    mkdir -p "${pkg_dir}/usr/local/bin"
    mkdir -p "${pkg_dir}/etc/systemd/system"

    cp "${INSTALL_DIR}/qmind-server" "${pkg_dir}/usr/local/bin/"
    cp "${INSTALL_DIR}/qmind-cli" "${pkg_dir}/usr/local/bin/"

    cat > "${pkg_dir}/DEBIAN/control" <<EOF
Package: qmind
Version: ${VERSION}
Architecture: $(dpkg --print-architecture)
Depends: libssl-dev
Maintainer: QuantsMind contributors
Description: QuantsMind Relational Database Engine
 A production-grade, embeddable relational database engine
 written in Rust. Supports HTAP workloads, Postgres wire
 protocol, and includes a desktop GUI studio.
EOF

    dpkg-deb --build "${pkg_dir}" "${pkg_name}.deb"
    echo "Created: ${pkg_name}.deb"
    echo "Install: sudo dpkg -i ${pkg_name}.deb"
    rm -rf "$pkg_dir"
}

main() {
    echo "=== QuantsMind v${VERSION} installer ==="
    echo ""

    local arch
    arch=$(detect_arch)
    echo "Architecture: $arch"

    install_deps
    download_binary "$arch"
    setup_data_dir
    create_systemd_service

    echo ""
    echo "=== Installation complete ==="
    echo ""
    echo "Server:  ${INSTALL_DIR}/qmind-server [PORT]"
    echo "CLI:     ${INSTALL_DIR}/qmind-cli [HOST:PORT]"
    echo "Data:    ${DATA_DIR}"
    echo ""
    echo "Quick start:"
    echo "  qmind-server 5432           # start server on port 5432"
    echo "  qmind-cli 127.0.0.1:5432    # connect with CLI"
    echo "  systemctl start qmind       # start as systemd service"
}

main "$@"
