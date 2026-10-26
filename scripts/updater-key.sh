#!/usr/bin/env bash
set -euo pipefail

# 生成 Tauri 自动更新所需的 minisign 密钥对，并提供 GitHub Secrets 设置命令。
#
# 私钥用于 CI 给更新包签名，公钥写进 app/tauri.conf.json。
# 私钥/密码一旦丢失，已安装用户将无法再接收自动更新。
#
# 用法：
#   bash scripts/updater-key.sh            # 不存在时生成
#   bash scripts/updater-key.sh --force    # 重新生成（会让旧版本无法再自动更新）
#
# 可用环境变量覆盖：OUT_DIR / KEY_PASSWORD / FORCE

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT/.signing}"
KEY="$OUT_DIR/updater.key"
PASSWORD_FILE="$OUT_DIR/updater-password.txt"
CONFIG="$ROOT/app/tauri.conf.json"
KEY_PASSWORD="${KEY_PASSWORD:-$(openssl rand -hex 16)}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "✗ 未找到 cargo，请先安装 Rust 工具链" >&2
  exit 1
fi

FORCE="${FORCE:-0}"
for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    -h|--help)
      sed -n '4,14p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "未知参数: $arg（用 --help 查看用法）" >&2
      exit 1
      ;;
  esac
done

umask 077
mkdir -p "$OUT_DIR"

if [ -f "$KEY" ] && [ "$FORCE" != "1" ]; then
  echo "✓ 已存在 $KEY"
else
  echo "==> 生成 Tauri 更新签名密钥"
  printf '%s' "$KEY_PASSWORD" > "$PASSWORD_FILE"
  cargo tauri signer generate --ci -p "$KEY_PASSWORD" -w "$KEY" -f
fi

# Some CLI versions create the files with the default umask.
chmod 600 "$KEY" "$PASSWORD_FILE" 2>/dev/null || true

PUB_KEY="$(cat "$KEY.pub")"

if command -v jq >/dev/null 2>&1; then
  tmp="$(mktemp)"
  jq --arg key "$PUB_KEY" '.plugins.updater.pubkey = $key' "$CONFIG" >"$tmp"
  mv "$tmp" "$CONFIG"
  echo "==> 已将公钥写入 $CONFIG"
else
  echo "==> 未找到 jq，请手动把下面的公钥写入 $CONFIG 的 plugins.updater.pubkey："
  echo "$PUB_KEY"
fi

echo ""
echo "==> 写进 GitHub Secrets（在仓库根目录执行）"
echo "    gh secret set TAURI_SIGNING_PRIVATE_KEY < \"$KEY\""
echo "    gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD < \"$PASSWORD_FILE\""
echo ""
echo "==> ⚠️ 务必备份 $OUT_DIR（至少离线保存一份），丢失后无法再发布可被旧版本验证的更新。"
