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
# 依赖：cargo
#
# 插件走**稳定 C ABI v1**（唯一机制）：跨边界只有 `repr(C)` 函数表 + UTF-8 JSON，
# 插件只依赖 `plugins/sdk`（零第三方依赖）→ **升级主程序后不需要重新编译插件**。
# （~~Rust 直连需同版本编译~~ 该机制已移除，其「宿主版本不一致」校验不再存在。）
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
skipped=0
for d in "$PLUGINS_DIR"/*/; do
    [ -f "${d}Cargo.toml" ] || continue
    name="$(basename "$d")"
    # 只构建动态库插件（crate-type 含 cdylib）；SDK 之类的普通库跳过
    if ! grep -q 'cdylib' "${d}Cargo.toml"; then
        echo "==> 跳过 $name（不是 cdylib 插件）"
        skipped=$((skipped + 1))
        continue
    fi
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

echo "==> 完成：构建 $built 个插件（跳过 $skipped 个），输出目录 $OUT"
ls -1 "$OUT" 2>/dev/null || true
