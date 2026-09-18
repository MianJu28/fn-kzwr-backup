#!/bin/bash
#
# Build and package fnos-backup fnOS application (.fpk)
#
# Steps:
#   1. Build Rust backend (--release)
#   2. Build frontend (vite build -> dist)
#   3. Assemble fnOS app package (binaries -> app/bin, frontend -> app/www)
#   4. fnpack build to produce .fpk
#
# Usage:
#   ./Scripts/build_fnos_app.sh          # build current arch + package
#   ./Scripts/build_fnos_app.sh --no-fpk # assemble only, skip fnpack
#
# Output:
#   dist/fnos-backup-app/     assembled package (gitignored)
#   dist/fnos-backup-app/*.fpk  final package
#
# Dependencies:
#   - cargo / rust (backend)
#   - node / npm (frontend)
#   - fnpack (executable, or set FNPACK env to its path)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Package source dir (committed: manifest/config/cmd/wizard/app/ui)
PACK_SRC="$ROOT/packaging/fnos-backup-app"
# Assembled build dir + .fpk output (gitignored)
PACK_DIR="$ROOT/dist/fnos-backup-app"
BACKEND_DIR="$ROOT/backend"
FRONTEND_DIR="$ROOT/frontend"
FNPACK="${FNPACK:-fnpack}"

DO_FPK=1
if [ "${1:-}" = "--no-fpk" ]; then
    DO_FPK=0
fi

echo "==> [1/4] Build backend (release, musl static) ..."
# fnOS embedded Linux needs musl static linking to avoid glibc dependency issues.
# Set MUSL_TARGET=0 to fall back to default glibc (local WSL dev only).
if [ "${MUSL_TARGET:-1}" = "1" ]; then
    if ! rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-linux-musl; then
        rustup target add x86_64-unknown-linux-musl 2>&1 | tail -1
    fi
    ( cd "$BACKEND_DIR" && cargo build --release --target x86_64-unknown-linux-musl )
    BIN_REL="$BACKEND_DIR/target/x86_64-unknown-linux-musl/release/fnos-backup"
else
    ( cd "$BACKEND_DIR" && cargo build --release )
    BIN_REL="$BACKEND_DIR/target/release/fnos-backup"
fi

echo "==> [2/4] Build frontend ..."
( cd "$FRONTEND_DIR" && npm run build )

echo "==> [3/4] Assemble fnOS app package $PACK_DIR ..."
# 1) Copy package skeleton from source dir (manifest/config/cmd/wizard/app/ui)
if [ ! -d "$PACK_SRC" ]; then
    echo "ERROR: package source dir missing: $PACK_SRC" >&2
    exit 1
fi
rm -rf "$PACK_DIR"
mkdir -p "$PACK_DIR"
cp -r "$PACK_SRC"/. "$PACK_DIR"/

# 2) Main binary (path set in step 1 depending on MUSL_TARGET)
if [ ! -f "$BIN_REL" ]; then
    echo "ERROR: backend binary not found: $BIN_REL" >&2
    exit 1
fi
mkdir -p "$PACK_DIR/app/bin"
cp -f "$BIN_REL" "$PACK_DIR/app/bin/fnos-backup"

# 3) Frontend dist -> app/www
if [ ! -d "$FRONTEND_DIR/dist" ]; then
    echo "ERROR: frontend build output missing: $FRONTEND_DIR/dist" >&2
    exit 1
fi
rm -rf "$PACK_DIR/app/www"
cp -r "$FRONTEND_DIR/dist" "$PACK_DIR/app/www"

# 4) Ensure scripts are executable
chmod +x "$PACK_DIR"/cmd/* 2>/dev/null || true
chmod +x "$PACK_DIR/app/bin/"* 2>/dev/null || true

echo "==> [4/4] Package .fpk ..."
if [ "$DO_FPK" = "1" ]; then
    ( cd "$PACK_DIR" && "$FNPACK" build )
    FPK_PATH="$(ls -t "$PACK_DIR"/*.fpk 2>/dev/null | head -1 || true)"
    if [ -n "$FPK_PATH" ]; then
        echo "==> Package done: $FPK_PATH"
    fi
else
    echo "==> Skipped fnpack (--no-fpk). Package assembled at $PACK_DIR"
fi

echo "Done."
