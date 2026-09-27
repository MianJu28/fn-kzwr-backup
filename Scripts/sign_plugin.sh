#!/usr/bin/env bash
# 外置插件签名工具（ADR-013 决策 3：默认强制验签）
#
# 机制：对插件动态库（`*.so`）整体做 **Ed25519** 签名，签名写到同目录同名文件
# `"<so>.sig"`（64 字节裸签名）。宿主的加载器（`plugin::loader::verify_plugin`）用
# `plugins.pubkeys` 里配置的**公钥**（base64 的 32 字节裸 Ed25519 公钥）逐一验签。
#
# 为什么用 openssl 而不是 Rust 侧生成：宿主验签只用 `ring`（已在依赖里，零新增依赖），
# 而 openssl 3.x 原生支持 Ed25519，两边格式完全兼容（公钥 = DER SPKI 的后 32 字节）。
#
# 用法：
#   Scripts/sign_plugin.sh keygen [密钥目录]           # 生成签名密钥对（默认 Scripts/keys/
#                                                     #   私钥 sign.key 必须保密、勿入库）
#   Scripts/sign_plugin.sh sign   <so 文件|目录> [...] # 就地生成 "<so>.sig"
#   Scripts/sign_plugin.sh verify <so 文件|目录> [...] # 用同目录 .sig 自检（需 openssl pkeyutl -verify）
#   Scripts/sign_plugin.sh pubkey [密钥目录]           # 打印 base64 公钥（填到配置项 plugins.pubkeys）
#
# 典型流程：
#   1) Scripts/sign_plugin.sh keygen
#   2) Scripts/sign_plugin.sh pubkey          # 复制输出，填入 设置页「插件公钥」
#   3) Scripts/build_plugins.sh               # 编译出 dist/plugins/*.so
#   4) Scripts/sign_plugin.sh sign dist/plugins
#
# 环境变量：
#   SIGN_KEY   私钥路径（默认 <密钥目录>/sign.key）
#   PLUGIN_KEYS 密钥目录（默认 Scripts/keys）
#
# 本地开发不想签名：设置 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1` 让宿主放行未签名插件
# （仅限本机调试，正式安装请勿使用）。
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KEYS_DIR="${PLUGIN_KEYS:-$SCRIPT_DIR/keys}"
KEY_FILE="${SIGN_KEY:-$KEYS_DIR/sign.key}"

die() {
  echo "错误：$*" >&2
  exit 1
}

need_openssl() {
  command -v openssl >/dev/null 2>&1 || die "找不到 openssl（Ed25519 签名依赖 openssl 1.1.1+）"
}

# 从私钥导出 base64 的 32 字节裸 Ed25519 公钥
# （`openssl pkey -pubout -outform DER` 得到 44 字节 SPKI，前 12 字节是算法头）
#
# 注：DER 是二进制且**含 0x00 字节**，必须走文件/管道，不能用 `$(...)` 取值
# （bash 会丢弃 NUL，导致公钥错位、验签必然失败）。
pubkey_b64() {
  local key="$1"
  [ -f "$key" ] || die "私钥不存在：$key（先执行：$0 keygen）"
  local tmp
  tmp="$(mktemp)" || die "无法创建临时文件"
  if ! openssl pkey -in "$key" -pubout -outform DER -out "$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "读取私钥失败：$key"
  fi
  tail -c 32 "$tmp" | base64 | tr -d '\n'
  rm -f "$tmp"
}

cmd_keygen() {
  need_openssl
  local dir="${1:-$KEYS_DIR}"
  local key="$dir/sign.key"
  if [ -f "$key" ]; then
    echo "私钥已存在，未覆盖：$key"
  else
    mkdir -p "$dir" || die "无法创建目录：$dir"
    chmod 700 "$dir" 2>/dev/null || true
    openssl genpkey -algorithm ED25519 -out "$key" 2>/dev/null || die "生成私钥失败"
    chmod 600 "$key" 2>/dev/null || true
    echo "已生成签名私钥：$key"
  fi
  echo
  echo "把下面这行填进 设置页 →「插件公钥」（plugins.pubkeys）："
  echo
  echo "  $(pubkey_b64 "$key")"
  echo
  echo "提醒：私钥 $key 用于签名，请离线保管、不要提交到仓库。"
}

# 对单个 .so 生成 "<so>.sig"
sign_one() {
  local so="$1"
  [ -f "$so" ] || die "文件不存在：$so"
  local sig="$so.sig"
  openssl pkeyutl -sign -rawin -inkey "$KEY_FILE" -in "$so" -out "$sig" 2>/dev/null \
    || die "签名失败：$so"
  echo "已签名：$so -> $sig（$(stat -c%s "$sig" 2>/dev/null || echo '?') 字节）"
}

verify_one() {
  local so="$1"
  local sig="$so.sig"
  [ -f "$sig" ] || die "缺少签名文件：$sig"
  # 用私钥派生 PEM 公钥来验签（自检用；等价于宿主用 plugins.pubkeys 验签）
  local pub
  pub="$(mktemp)" || die "无法创建临时文件"
  if ! openssl pkey -in "$KEY_FILE" -pubout -out "$pub" 2>/dev/null; then
    rm -f "$pub"
    die "读取私钥失败：$KEY_FILE"
  fi
  if openssl pkeyutl -verify -rawin -pubin -inkey "$pub" -in "$so" -sigfile "$sig" >/dev/null 2>&1; then
    rm -f "$pub"
    echo "验签通过：$so"
  else
    rm -f "$pub"
    die "验签失败：$so"
  fi
}

# 展开目录为其中的 .so 列表；对文件原样返回
expand_targets() {
  local t
  for t in "$@"; do
    if [ -d "$t" ]; then
      find "$t" -maxdepth 1 -type f -name '*.so' | sort
    else
      printf '%s\n' "$t"
    fi
  done
}

main() {
  local action="${1:-}"
  shift || true
  case "$action" in
    keygen)
      cmd_keygen "$@"
      ;;
    pubkey)
      need_openssl
      pubkey_b64 "$KEY_FILE"
      echo
      ;;
    sign)
      need_openssl
      [ $# -gt 0 ] || die "用法：$0 sign <so 文件|目录> [...]"
      [ -f "$KEY_FILE" ] || die "私钥不存在：$KEY_FILE（先执行：$0 keygen）"
      local n=0 so
      while IFS= read -r so; do
        [ -n "$so" ] || continue
        sign_one "$so"
        n=$((n + 1))
      done < <(expand_targets "$@")
      [ "$n" -gt 0 ] || die "没找到任何 .so（先执行 Scripts/build_plugins.sh）"
      echo "共签名 $n 个插件动态库。"
      ;;
    verify)
      need_openssl
      [ $# -gt 0 ] || die "用法：$0 verify <so 文件|目录> [...]"
      [ -f "$KEY_FILE" ] || die "私钥不存在：$KEY_FILE"
      local n=0 so
      while IFS= read -r so; do
        [ -n "$so" ] || continue
        verify_one "$so"
        n=$((n + 1))
      done < <(expand_targets "$@")
      [ "$n" -gt 0 ] || die "没找到任何 .so"
      ;;
    ""|-h|--help|help)
      sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      ;;
    *)
      die "未知动作：$action（可用：keygen / sign / verify / pubkey）"
      ;;
  esac
}

main "$@"
