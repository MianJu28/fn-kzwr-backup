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
# 链接方式（2026-09-27 起）：默认 **glibc 动态链接**（父目录 README/文档曾写 musl 静态）。
#   原因：musl 不支持 cdylib、静态 musl 不支持 dlopen ⇒ 与「外置插件」互斥。
#   需要纯静态包（不含插件）：MUSL_TARGET=1 SKIP_PLUGINS=1 ./Scripts/build_fnos_app.sh
#
# 环境变量：
#   TARGET_TRIPLE    交叉编译目标三元组（如 aarch64-unknown-linux-gnu）；留空 = 本机
#   PLATFORM         manifest 的 platform（x86/arm/all）；留空 = 按 TARGET_TRIPLE 推断
#   PREBUILT_BIN     复用已构建的后端二进制（CI 交叉编译后走这里，避免重复构建）
#   CARGO_TARGET_DIR 共享构建目录
#   SKIP_PLUGINS=1   不打包外置插件         SKIP_SIGN=1  产出未签名插件（仅调试）
#   MUSL_TARGET=1    改用 x86_64-unknown-linux-musl 静态（必须配合 SKIP_PLUGINS=1）
#   FNPACK           fnpack 可执行文件路径
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

echo "==> [1/4] Build backend (release) ..."
# ── 链接方式：**默认 glibc 动态链接**（2026-09-27 改，原默认 musl 静态） ──────────
# 为什么不再用 musl 静态：外置插件（ADR-013）与 musl 静态**不可能共存**——
#   1) musl 目标不支持 `cdylib`：rustc 直接报
#      "the target ... does not support these crate types"，插件根本编译不出来；
#   2) 静态链接的二进制没有动态装载器，`dlopen` 不可用（libloading 必然失败）。
# 插件是本应用的一等功能，故发布包统一走 glibc 动态链接
# （当前线上已安装的 v0.3.9 也是动态链接 glibc，且宿主 import dlopen@GLIBC_2.34）。
#
# 若确实需要纯静态包（**不含插件**）：MUSL_TARGET=1 SKIP_PLUGINS=1 $0
MUSL_TARGET="${MUSL_TARGET:-0}"

# 目标三元组：默认「本机 glibc」（不加 --target，产物在 target/release/）。
# 交叉编译（如为 ARM 设备打包）设 TARGET_TRIPLE=aarch64-unknown-linux-gnu，
# 并自行准备交叉链接器（CARGO_TARGET_<TRIPLE>_LINKER 等）。
if [ -n "${TARGET_TRIPLE:-}" ]; then
    case "$TARGET_TRIPLE" in
        *musl*) MUSL_TARGET=1 ;;
    esac
elif [ "$MUSL_TARGET" = "1" ]; then
    TARGET_TRIPLE="x86_64-unknown-linux-musl"
fi

if [ "$MUSL_TARGET" = "1" ] && [ "${SKIP_PLUGINS:-0}" != "1" ]; then
    echo "ERROR: musl 静态目标与「随包外置插件」不能同时使用：" >&2
    echo "       musl 不支持 cdylib（插件无法编译），静态 musl 也无法 dlopen。" >&2
    echo "       请二选一：" >&2
    echo "         $0                # glibc 动态链接，插件可用（默认，推荐）" >&2
    echo "         SKIP_PLUGINS=1 $0 # musl 静态包，不含插件" >&2
    exit 1
fi
#
# 输出目录同样遵循 CARGO_TARGET_DIR/--target：CI 常把构建产物集中到共享目录，
# 若这里写死 backend/target 就会「明明构建成功却说找不到二进制」。
TARGET_ROOT="${CARGO_TARGET_DIR:-$BACKEND_DIR/target}"
case "$TARGET_ROOT" in
    /*) ;;
    *) TARGET_ROOT="$BACKEND_DIR/$TARGET_ROOT" ;;
esac
# PREBUILT_BIN：CI 里后端可能已由交叉编译步骤产出，直接复用，避免重复构建
if [ -n "${PREBUILT_BIN:-}" ]; then
    echo "    使用预构建二进制：$PREBUILT_BIN"
    BIN_REL="$PREBUILT_BIN"
elif [ -n "${TARGET_TRIPLE:-}" ]; then
    if ! rustup target list --installed 2>/dev/null | grep -q "^$TARGET_TRIPLE$"; then
        rustup target add "$TARGET_TRIPLE" 2>&1 | tail -1
    fi
    ( cd "$BACKEND_DIR" && cargo build --release --target "$TARGET_TRIPLE" )
    BIN_REL="$TARGET_ROOT/$TARGET_TRIPLE/release/fn-kzwr-backup"
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

# 1.1) manifest 的 `platform`（x86 / arm / all）：源码里写死 x86，交叉编译 ARM 包
#      时必须改，否则 ARM 设备装不上（或装上了跑不起来）。
#      未显式给 PLATFORM 时按目标三元组推断。
if [ -z "${PLATFORM:-}" ]; then
    case "${TARGET_TRIPLE:-}" in
        *aarch64*|*arm*) PLATFORM="arm" ;;
        *)               PLATFORM="x86" ;;
    esac
fi
if [ -f "$PACK_DIR/manifest" ]; then
    sed -i "s/^\(platform[[:space:]]*=[[:space:]]*\).*/\1$PLATFORM/" "$PACK_DIR/manifest"
    echo "    manifest platform = $PLATFORM"
fi

# 2) Main binary (path set in step 1 depending on TARGET_TRIPLE / PREBUILT_BIN)
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
#      插件架构必须与宿主一致，否则 dlopen 失败：把同一个 TARGET_TRIPLE 透传下去。
if [ "${SKIP_PLUGINS:-0}" != "1" ] && [ -d "$ROOT/plugins" ]; then
    echo "==> [3.1/4] Build external plugins ..."
    if PLUGIN_TARGET="${TARGET_TRIPLE:-}" bash "$ROOT/Scripts/build_plugins.sh" "$PACK_DIR/app/plugins"; then
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
