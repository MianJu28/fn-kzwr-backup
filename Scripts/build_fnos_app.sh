#!/bin/bash
#
# Build and package fn-kzwr-backup fnOS application (.fpk)
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
#   dist/fn-kzwr-backup-app/     assembled package (gitignored)
#   dist/fn-kzwr-backup-app/*.fpk  final package
#
# Dependencies:
#   - cargo / rust (backend)
#   - node / npm (frontend)
#   - fnpack (executable, or set FNPACK env to its path)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Package source dir (committed: manifest/config/cmd/wizard/app/ui)
PACK_SRC="$ROOT/packaging/fn-kzwr-backup-app"
# Assembled build dir + .fpk output (gitignored)
PACK_DIR="$ROOT/dist/fn-kzwr-backup-app"
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
#
# 输出目录同样遵循 CARGO_TARGET_DIR/--target：CI 常把构建产物集中到共享目录，
# 若这里写死 backend/target 就会「明明构建成功却说找不到二进制」。
TARGET_ROOT="${CARGO_TARGET_DIR:-$BACKEND_DIR/target}"
case "$TARGET_ROOT" in
    /*) ;;
    *) TARGET_ROOT="$BACKEND_DIR/$TARGET_ROOT" ;;
esac
if [ "${MUSL_TARGET:-1}" = "1" ]; then
    if ! rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-linux-musl; then
        rustup target add x86_64-unknown-linux-musl 2>&1 | tail -1
    fi
    ( cd "$BACKEND_DIR" && cargo build --release --target x86_64-unknown-linux-musl )
    BIN_REL="$TARGET_ROOT/x86_64-unknown-linux-musl/release/fn-kzwr-backup"
else
    ( cd "$BACKEND_DIR" && cargo build --release )
    BIN_REL="$TARGET_ROOT/release/fn-kzwr-backup"
fi

echo "==> [2/4] Build frontend ..."
( cd "$FRONTEND_DIR" && npm run build )
# vite 会把 public/ 下文件的权限**照抄**进 dist（含 000）。这类文件读得进却 `cp` 不了，
# 随后「dist → app/www」与 fnpack 都会失败，故构建后先归一化再使用。
chmod -R u+rwX "$FRONTEND_DIR/dist" 2>/dev/null || true

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
cp -f "$BIN_REL" "$PACK_DIR/app/bin/fn-kzwr-backup"

# 3) Frontend dist -> app/www
if [ ! -d "$FRONTEND_DIR/dist" ]; then
    echo "ERROR: frontend build output missing: $FRONTEND_DIR/dist" >&2
    exit 1
fi
rm -rf "$PACK_DIR/app/www"
cp -r "$FRONTEND_DIR/dist" "$PACK_DIR/app/www"

# 4) 归一化权限：`cp -r` 会**原样保留源文件权限**，而 .fpk 的内容不该带任意权限位。
#    多用户/迁移过的仓库里源文件可能是 000（属主是别人、当前构建用户无权 chmod），
#    vite 又会把 public/ 下这种文件的权限照抄进 dist → `cp`、fnpack 与运行时都会读失败。
#    这里统一成「目录 755 / 文件 644」，随后再恢复需要的可执行位。
find "$PACK_DIR" -type d -exec chmod 755 {} + 2>/dev/null || true
find "$PACK_DIR" -type f -exec chmod 644 {} + 2>/dev/null || true

# 4.0) Ensure scripts are executable
chmod +x "$PACK_DIR"/cmd/* 2>/dev/null || true
chmod +x "$PACK_DIR/app/bin/"* 2>/dev/null || true

# 4.1) 外置插件（可选）：构建 plugins/* 并放入 app/plugins/
#      插件是 cdylib（*.so），宿主在 $TRIM_APPDEST/plugins 或 $TRIM_PKGETC/plugins 下加载；
#      加载默认关闭（设置页或 FN_KZWR_PLUGINS=1 开启）。失败不阻断打包。
#
#      签名：build_plugins.sh **默认**用 Scripts/keys/sign.key 签名（存在即启用），
#      因为宿主默认强制验签——未签名的随包插件在用户机器上会被拒绝加载。
#      该脚本还会核对私钥与宿主内置官方公钥（loader.rs 的 OFFICIAL_PUBKEYS）是否配对。
if [ "${SKIP_PLUGINS:-0}" != "1" ] && [ -d "$ROOT/plugins" ]; then
    echo "==> [3.1/4] Build external plugins ..."
    if bash "$ROOT/Scripts/build_plugins.sh" "$PACK_DIR/app/plugins"; then
        echo "    插件已放入 app/plugins/"
        # 缺 .sig 的随包插件在本机（强制验签）必然加载失败 —— 打包时就喊出来，
        # 而不是等用户装完发现「插件列表是空的」。
        unsigned="$(find "$PACK_DIR/app/plugins" -maxdepth 1 -type f -name '*.so' ! -exec test -f '{}.sig' \; -print 2>/dev/null || true)"
        if [ -n "$unsigned" ]; then
            echo "    ⚠️ 以下随包插件缺少 .sig，将在强制验签下被拒绝加载：" >&2
            printf '       %s\n' $unsigned >&2
            echo "       （本机调试可设 FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1）" >&2
        fi
    else
        echo "    ⚠️ 外置插件构建失败（不阻断打包；如需跳过可设 SKIP_PLUGINS=1）" >&2
    fi
fi

# 4.5) 统一换行符为 LF（CRLF 会导致飞牛生命周期脚本 "bad interpreter"、manifest 解析值带 \r 报"不是有效 fpk"）
find "$PACK_DIR" -type f \( -name manifest -o -path "*/cmd/*" -o -path "*/wizard/*" -o -path "*/config/*" -o -name config \) -print0 2>/dev/null \
  | xargs -0 -r sed -i 's/\r$//' 2>/dev/null || true

# 4.6) 再归一化一次权限：`sed -i` 是「写临时文件 + 改名」，会把权限重置为 755，
#      把上一步（4）的结果抹掉。这里重新钉成「目录 755 / 文件 644 + 恢复可执行位」，
#      保证 .fpk 内的权限只由本脚本决定，不受构建机 umask / 源文件权限影响。
find "$PACK_DIR" -type d -exec chmod 755 {} + 2>/dev/null || true
find "$PACK_DIR" -type f -exec chmod 644 {} + 2>/dev/null || true
chmod +x "$PACK_DIR"/cmd/* 2>/dev/null || true
chmod +x "$PACK_DIR/app/bin/"* 2>/dev/null || true
chmod +x "$PACK_DIR/app/plugins/"*.so 2>/dev/null || true

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
