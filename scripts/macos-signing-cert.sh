#!/usr/bin/env bash
set -euo pipefail

# 生成一张长期有效的自签名代码签名证书，并导出 CI 所需的 .p12。
#
# 目的：TCC（完全磁盘访问、桌面/文稿/下载等）按代码签名的 designated
# requirement 记录授权。ad-hoc 签名的 requirement 里是本次构建的 cdhash，
# 每次发版都会变，用户就得重新授权一次。改用固定证书后 requirement 只包含
# bundle id + 证书指纹，跨版本稳定。
#
# 注意：自签名不会让 Gatekeeper 放行，用户首次打开仍需「系统设置 → 隐私与安全性 →
# 仍要打开」。这是免费方案能做到的上限。
#
# 用法：
#   bash scripts/macos-signing-cert.sh              # 只生成证书和 p12
#   bash scripts/macos-signing-cert.sh --import     # 同时导入 login keychain（本地构建用）
#
# 可用环境变量覆盖：CERT_NAME / CERT_ORG / CERT_DAYS / OUT_DIR / P12_PASS / FORCE

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CERT_NAME="${CERT_NAME:-Trae ACP Gateway Signing}"
CERT_ORG="${CERT_ORG:-Trae ACP Gateway}"
CERT_DAYS="${CERT_DAYS:-3650}"
OUT_DIR="${OUT_DIR:-$ROOT/.signing}"
P12_PASS="${P12_PASS:-$(openssl rand -hex 16)}"
IMPORT=0

usage() {
  sed -n '4,16p' "$0" | sed 's/^# \{0,1\}//'
}

for arg in "$@"; do
  case "$arg" in
    --import) IMPORT=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数: $arg（用 --help 查看用法）" >&2; exit 1 ;;
  esac
done

umask 077
mkdir -p "$OUT_DIR"

if [ -f "$OUT_DIR/cert.pem" ] && [ "${FORCE:-0}" != "1" ]; then
  echo "✗ 已存在 $OUT_DIR/cert.pem" >&2
  echo "  继续会生成新证书，导致已安装用户的权限记录失效。" >&2
  echo "  确实要重新生成请加 FORCE=1。" >&2
  exit 1
fi

echo "==> 生成自签名证书: $CERT_NAME (有效期 ${CERT_DAYS} 天)"
cat > "$OUT_DIR/openssl.cnf" <<EOF
[ req ]
distinguished_name = dn
prompt = no
x509_extensions = v3_codesign

[ dn ]
CN = $CERT_NAME
O = $CERT_ORG

[ v3_codesign ]
basicConstraints = critical,CA:TRUE
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
EOF

openssl req -x509 -sha256 -newkey rsa:2048 -nodes \
  -keyout "$OUT_DIR/key.pem" \
  -out "$OUT_DIR/cert.pem" \
  -days "$CERT_DAYS" \
  -config "$OUT_DIR/openssl.cnf"

# OpenSSL 3 默认的 PKCS#12 算法 macOS 的 security 命令可能读不了，需要 -legacy。
legacy=""
case "$(openssl version)" in
  OpenSSL\ 3*) legacy="-legacy" ;;
esac

openssl pkcs12 -export $legacy \
  -inkey "$OUT_DIR/key.pem" \
  -in "$OUT_DIR/cert.pem" \
  -name "$CERT_NAME" \
  -out "$OUT_DIR/cert.p12" \
  -passout "pass:$P12_PASS"

printf '%s' "$P12_PASS" > "$OUT_DIR/p12-password.txt"
base64 < "$OUT_DIR/cert.p12" | tr -d '\n' > "$OUT_DIR/cert.p12.b64"

if [ "$IMPORT" = "1" ]; then
  echo "==> 导入 login keychain（本机签名用；首次签名若弹出钥匙串授权，点「始终允许」）"
  security import "$OUT_DIR/cert.p12" \
    -k "$HOME/Library/Keychains/login.keychain-db" \
    -P "$P12_PASS" \
    -T /usr/bin/codesign \
    -T /usr/bin/security
fi

echo ""
echo "==> 完成，文件在 $OUT_DIR"
echo "    cert.pem             证书（保留，换机时还要用，至少留一份离线备份）"
echo "    key.pem              私钥（切勿提交/上传）"
echo "    cert.p12             证书 + 私钥"
echo "    cert.p12.b64         base64 后的 p12（用于 GitHub Secret）"
echo "    p12-password.txt     p12 密码"
echo ""
echo "==> 写进 GitHub Secrets（在仓库根目录执行）"
echo "    gh secret set MACOS_CERT_P12 < \"$OUT_DIR/cert.p12.b64\""
echo "    gh secret set MACOS_CERT_PASSWORD --body \"$P12_PASS\""
echo "    gh secret set MACOS_SIGNING_IDENTITY --body \"$CERT_NAME\""
echo ""
echo "==> 更新包签名密钥（首次发布前执行一次，之后务必离线备份 .signing）"
echo "    bash scripts/updater-key.sh"
echo ""
echo "==> 本地构建签名"
if [ "$IMPORT" = "1" ]; then
  echo "    APPLE_SIGNING_IDENTITY=\"$CERT_NAME\" make build-app SIGN=1"
else
  echo "    先执行 bash scripts/macos-signing-cert.sh --import（或手动双击导入 cert.p12）"
  echo "    然后 APPLE_SIGNING_IDENTITY=\"$CERT_NAME\" make build-app SIGN=1"
fi
echo ""
echo "==> 检查签名身份"
echo "    security find-identity -p codesigning login.keychain-db"
