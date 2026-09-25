#!/usr/bin/env bash
#
# 构建外置插件（ADR-013 方案 B：动态库）
#
# 把 plugins/<name>/ 下的每个插件 crate（crate-type = cdylib）编译成 *.so，
# 输出到目标目录（默认 dist/plugins/）。宿主启动时扫描
# $TRIM_PKGETC/plugins（用户放置）与 $TRIM_APPDEST/plugins（随包分发）并加载。
#
# 用法：
#   ./Scripts/build_plugins.sh [输出目录]
#
# 依赖：cargo（与主程序相同的 toolchain —— 插件 ABi 依赖宿主 crate 的 trait 定义，
#       升级主程序后必须重新编译插件，否则宿主会因「宿主版本不一致」拒绝加载）
#
# 环境变量：
#   CARGO_TARGET_DIR  可指定共享构建目录

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/dist/plugins}"
PLUGINS_DIR="$ROOT/plugins"

mkdir -p "$OUT"

if [ ! -d "$PLUGINS_DIR" ]; then
    echo "==> 没有 plugins/ 目录，跳过"
    exit 0
fi

built=0
for d in "$PLUGINS_DIR"/*/; do
    [ -f "${d}Cargo.toml" ] || continue
    name="$(basename "$d")"
    echo "==> 构建插件 $name"
    # --offline 优先（NAS 上依赖已缓存）；失败再回退联网
    ( cd "$d" && cargo build --release --offline 2>/dev/null || cargo build --release )
    so="$(find "$d/target/release" -maxdepth 1 -name '*.so' -print -quit 2>/dev/null || true)"
    if [ -z "$so" ]; then
        echo "ERROR: 插件 $name 未产出 *.so（确认 crate-type = [\"cdylib\"]）" >&2
        exit 1
    fi
    cp -f "$so" "$OUT/"
    echo "    -> $(basename "$so")"
    built=$((built + 1))
done

echo "==> 完成：$built 个插件，输出目录 $OUT"
ls -1 "$OUT" 2>/dev/null || true
