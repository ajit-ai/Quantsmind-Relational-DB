#!/usr/bin/env bash
# QuantsMind — cross-compile build script
# Usage: bash scripts/build-all.sh [target]
# Without args: builds for the current platform.
# With a target triple: cross-compiles.

set -euo pipefail

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
OUTDIR="dist"
TARGET="${1:-}"

echo "=== QuantsMind build v${VERSION} ==="

build_native() {
    echo "[1/3] Building release binaries (native)..."
    cargo build --release
    mkdir -p "$OUTDIR"

    if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "win32" || "$OSTYPE" == "cygwin" ]]; then
        cp target/release/qmind-server.exe "$OUTDIR/" 2>/dev/null || true
        cp target/release/qmind-cli.exe   "$OUTDIR/" 2>/dev/null || true
    else
        cp target/release/qmind-server "$OUTDIR/" 2>/dev/null || true
        cp target/release/qmind-cli   "$OUTDIR/" 2>/dev/null || true
    fi
}

build_cross() {
    echo "[1/3] Cross-compiling for ${TARGET}..."
    rustup target add "$TARGET" 2>/dev/null || true

    case "$TARGET" in
        x86_64-pc-windows-gnu)
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
                cargo build --release --target "$TARGET" ;;
        *)
            cargo build --release --target "$TARGET" ;;
    esac

    mkdir -p "$OUTDIR/$TARGET"
    find "target/${TARGET}/release" -maxdepth 1 -type f \( -name 'qmind-server*' -o -name 'qmind-cli*' \) \
        ! -name '*.d' ! -name '*.pdb' -exec cp {} "$OUTDIR/$TARGET/" \;
}

if [[ -n "$TARGET" ]]; then
    build_cross
else
    build_native
fi

echo "[2/3] Running tests..."
cargo test --workspace --release 2>&1 | tail -5

echo "[3/3] Build complete. Artifacts in $OUTDIR/"
ls -lh "$OUTDIR/" 2>/dev/null || dir "$OUTDIR\\" 2>/dev/null || true
echo "=== Done ==="
