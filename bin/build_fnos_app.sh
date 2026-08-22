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
#   ./bin/build_fnos_app.sh          # build current arch + package
#   ./bin/build_fnos_app.sh --no-fpk # assemble only, skip fnpack
#
# Dependencies:
#   - cargo / rust (backend)
#   - node / npm (frontend)
#   - fnpack (executable, or set FNPACK env to its path)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Package source dir (committed: manifest/config/cmd/wizard/app/ui)
PACK_SRC="$ROOT/packaging/fnos-backup-app"
# Assembled build dir (gitignored: contains app/bin and app/www)
PACK_DIR="$ROOT/bin/fnos-backup-app"
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

# 3) Login binary (Camoufox static build; copy to app/bin if present)
#    Architecture-aware: x64 (amd64) / arm64. Override with LOGIN_ARCH.
LOGIN_ARCH="${LOGIN_ARCH:-x64}"
LOGIN_BIN="$ROOT/bin/kzwr_login_camoufox-linux-${LOGIN_ARCH}"
if [ -f "$LOGIN_BIN" ]; then
    cp -f "$LOGIN_BIN" "$PACK_DIR/app/bin/kzwr_login_camoufox-linux-${LOGIN_ARCH}"
    echo "==> 已打包登录二进制: $(basename "$LOGIN_BIN")"
else
    echo "提示：未找到登录二进制 $LOGIN_BIN（Camoufox 版）。"
fi

# 3.5) Camoufox 登录环境附属文件（install_init 安装时使用）
#    - uBlock Origin addon（ubo.xpi）：随 fpk 打包，安装时解压到 camoufox/addons/UBO
#    - Camoufox 浏览器 zip（可选，camoufox-browser.zip）：若预下载则打包，安装时免联网
mkdir -p "$PACK_DIR/app/cache"
UBO_XPI="$ROOT/bin/ubo.xpi"
if [ -f "$UBO_XPI" ]; then
    cp -f "$UBO_XPI" "$PACK_DIR/app/cache/ubo.xpi"
    echo "==> 已打包 uBlock addon (ubo.xpi)"
else
    echo "提示：未找到 $UBO_XPI，安装时无法解压 uBlock addon（Camoufox 可能报 InvalidAddonPath）。"
fi
CAMO_BROWSER_ZIP="$ROOT/bin/camoufox-browser.zip"
if [ -f "$CAMO_BROWSER_ZIP" ]; then
    cp -f "$CAMO_BROWSER_ZIP" "$PACK_DIR/app/cache/camoufox-browser.zip"
    echo "==> 已打包 Camoufox 浏览器 zip（安装时免联网）"
fi

# 4) Frontend dist -> app/www
if [ ! -d "$FRONTEND_DIR/dist" ]; then
    echo "ERROR: frontend build output missing: $FRONTEND_DIR/dist" >&2
    exit 1
fi
rm -rf "$PACK_DIR/app/www"
cp -r "$FRONTEND_DIR/dist" "$PACK_DIR/app/www"

# 5) Ensure scripts are executable
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
