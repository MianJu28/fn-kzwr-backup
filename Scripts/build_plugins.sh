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
# 签名：宿主**默认强制验签**（ADR-013 决策 3），因此本脚本**默认签名**——
# 只要 Scripts/keys/sign.key 存在（发布私钥），产出的每个 .so 都会带上 `<so>.sig`，
# 用户无需任何配置即可加载随包插件。私钥不在时退回未签名并给出提示。
#
# 依赖：cargo
#
# 插件走**稳定 C ABI v1**（唯一机制）：跨边界只有 `repr(C)` 函数表 + UTF-8 JSON，
# 插件只依赖 `plugins/sdk`（零第三方依赖）→ **升级主程序后不需要重新编译插件**。
# （~~Rust 直连需同版本编译~~ 该机制已移除，其「宿主版本不一致」校验不再存在。）
#
# 环境变量：
#   CARGO_TARGET_DIR  可指定共享构建目录
#   SIGN_KEY          签名私钥路径（**默认** Scripts/keys/sign.key，存在即启用）
#   SKIP_SIGN=1       显式跳过签名（产出未签名插件，仅供本机调试）
#   ALLOW_KEY_MISMATCH=1  允许「私钥与宿主内置官方公钥不配对」（第三方自建分发用）

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/dist/plugins}"
PLUGINS_DIR="$ROOT/plugins"
DEFAULT_KEY="$ROOT/Scripts/keys/sign.key"

# 默认用发布私钥签名：随包插件必须带 `.sig` 才能在「默认强制验签」下加载。
# 私钥不存在（如 CI/第三方）则退回未签名，并给出明确提示（见文末）。
if [ "${SKIP_SIGN:-0}" = "1" ]; then
    SIGN_KEY=""
elif [ -z "${SIGN_KEY:-}" ] && [ -f "$DEFAULT_KEY" ]; then
    SIGN_KEY="$DEFAULT_KEY"
fi
# 必须 export：sign_plugin.sh 是**子进程**，只认环境变量里的 SIGN_KEY
export SIGN_KEY

# 漂移守卫：随包插件靠宿主**编译期内置**的官方公钥（loader.rs 的 OFFICIAL_PUBKEYS）
# 通过验签。若签名私钥与内置公钥不是一对，插件会被全部拒绝加载，而现象只是
# 「插件列表里什么都没有」——这里提前把不一致喊出来。
if [ -n "${SIGN_KEY:-}" ]; then
    loader_rs="$ROOT/backend/src/plugin/loader.rs"
    key_pub="$(bash "$ROOT/Scripts/sign_plugin.sh" pubkey "$SIGN_KEY" 2>/dev/null | tr -d '[:space:]')"
    if [ -n "$key_pub" ] && [ -f "$loader_rs" ] && [ "${ALLOW_KEY_MISMATCH:-0}" != "1" ]; then
        if ! grep -qF "\"$key_pub\"" "$loader_rs"; then
            echo "ERROR: 签名私钥与宿主内置官方公钥不一致：" >&2
            echo "       私钥 $SIGN_KEY 的公钥为 $key_pub" >&2
            echo "       该公钥未出现在 $loader_rs 的 OFFICIAL_PUBKEYS 中" >&2
            echo "       → 随包插件将无法通过强制验签。三种修法：" >&2
            echo "         1) 换回官方私钥（Scripts/keys/sign.key）" >&2
            echo "         2) 把该公钥补进 OFFICIAL_PUBKEYS 后重新编译宿主" >&2
            echo "         3) 自建分发：设 ALLOW_KEY_MISMATCH=1，让用户把公钥填进设置页「插件公钥」" >&2
            exit 1
        fi
    fi
fi

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
    # --offline 优先（NAS 上依赖已缓存）；失败再回退联网。
    # 错误信息要留住：否则构建失败只会看到「未产出 *.so」，排查时无从下手。
    log="$(mktemp)"
    if ! ( cd "$d" && cargo build --release --offline ) >"$log" 2>&1; then
        echo "    离线构建失败，回退联网重试…" >&2
        if ! ( cd "$d" && cargo build --release ) >"$log" 2>&1; then
            echo "ERROR: 插件 $name 构建失败：" >&2
            tail -30 "$log" >&2
            rm -f "$log"
            exit 1
        fi
    fi
    rm -f "$log"
    # 输出目录：遵循 CARGO_TARGET_DIR（相对路径按 cargo 规则以插件目录为基准解析）。
    # 本脚本文件头对外承诺支持该变量，这里必须真的认它，否则共享构建目录时找不到 .so。
    target_dir="${CARGO_TARGET_DIR:-target}"
    case "$target_dir" in
        /*) ;;
        *) target_dir="$d$target_dir" ;;
    esac
    # 精确算出本插件应产出的文件名，而不是 `find -quit` 撞运气：
    # 共用 CARGO_TARGET_DIR 时同级目录里躺着**所有**插件的 .so，取第一个会把
    # 别的插件拷成本插件（静默发错二进制）。
    libname="$(sed -n 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' "${d}Cargo.toml" | tail -1)"
    so="$target_dir/release/lib${libname}.so"
    if [ ! -f "$so" ]; then
        # lib.name 未显式声明时 cargo 会用包名（连字符转下划线）；两种都试过再报错
        pkgname="$(sed -n 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' "${d}Cargo.toml" | head -1)"
        alt="$target_dir/release/lib$(printf '%s' "$pkgname" | tr '-' '_').so"
        if [ -f "$alt" ]; then
            so="$alt"
        else
            echo "ERROR: 插件 $name 未产出 *.so（确认 crate-type = [\"cdylib\"]）" >&2
            echo "       期望文件：$so" >&2
            echo "       目录内容：$(ls -1 "$target_dir/release" 2>/dev/null | grep '\.so$' | tr '\n' ' ')" >&2
            exit 1
        fi
    fi
    cp -f "$so" "$OUT/"
    echo "    -> $(basename "$so")"
    built=$((built + 1))

    # 构建后签名（默认启用，见文件头）：默认强制验签下，未签名插件在正式环境会被拒绝加载。
    if [ -n "${SIGN_KEY:-}" ]; then
        if bash "$ROOT/Scripts/sign_plugin.sh" sign "$OUT/$(basename "$so")" >/dev/null 2>&1; then
            echo "    -> 已签名 $(basename "$so").sig"
        else
            echo "ERROR: 签名失败（SIGN_KEY=$SIGN_KEY）；该插件将无法通过强制验签" >&2
            exit 1
        fi
    fi
done

# 未签名时的提示与后果说明，避免「打包后插件全部加载失败」的困惑
if [ -z "${SIGN_KEY:-}" ]; then
    echo "==> 提示：未找到签名私钥（$DEFAULT_KEY），产出为**未签名**插件；"
    echo "    正式环境默认强制验签（ADR-013 决策 3）会拒绝加载它们。"
    echo "    自签：SIGN_KEY=<私钥> $0 $OUT（或放入 $DEFAULT_KEY）"
fi

echo "==> 完成：构建 $built 个插件（跳过 $skipped 个），输出目录 $OUT"
ls -1 "$OUT" 2>/dev/null || true
