#!/usr/bin/env bash
# 部署包的一键入口（打包时复制到包根，改名 deploy.sh）。
#
# 它只做「客户端那一半」：把包里的 source/ 打成 tar，连同 payload/ 里的前端产物，
# 交给 source/tools/deploy_update.sh 完成上传 → 服务器构建 → 原子切换 → 自检 → 清理。
# 服务器侧的行为**一行都不重写** —— 那些脚本连同源码一起在包里，唯一事实源只有一份。
#
# 用法（在包根目录）：
#   bash deploy.sh                 更新已经在跑的服务器
#   bash deploy.sh --bootstrap     全新机器：先铺 PG/venv/systemd/web 容器，再走一遍更新流程
#   bash deploy.sh --dry-run       只做本机自检与打包，不连服务器
#
# 连接参数按此优先级取：环境变量 → 交互输入。密码只在内存里传给子进程，不落盘、不进日志。
#   DEPLOY_HOST / DEPLOY_USER(默认 root) / DEPLOY_PORT(默认 22) / DEPLOY_PW
#   DMS_RUNTIME_ROOT(默认 /opt/dms-ai)
set -euo pipefail
cd "$(dirname "$0")"

# Windows/Git-Bash 会把看着像 POSIX 路径的**参数**改写成 Windows 路径，远端命令因此写错目录，
# 报出来却是一句莫名其妙的 `Socket is closed`。对 Linux/macOS 无副作用，无条件导出。
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

die() { echo "ERROR: $*" >&2; exit 1; }

# 🔴 Git-Bash 的 `/c/Users/...` 交给 **Windows 版 Python** 会被当成 `C:\c\Users\...`，
# 于是「文件明明在那儿却说找不到」。凡是要跨过 bash→python 边界的绝对路径，都过一遍 cygpath。
# Linux/macOS 没有 cygpath，原样返回。
native() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi; }

MODE=update
case "${1:-}" in
  --bootstrap) MODE=bootstrap ;;
  --dry-run)   MODE=dryrun ;;
  "")          ;;
  *)           die "未知参数：$1（可用：--bootstrap / --dry-run）" ;;
esac

[ -d source ] || die "包不完整：缺 source/ 目录"
[ -s payload/web-dist.tar.gz ] || die "包不完整：缺 payload/web-dist.tar.gz"
[ -s config/settings.docker.json ] || die "包不完整：缺 config/settings.docker.json"
[ -s config/secret.key ] || die "包不完整：缺 config/secret.key"

echo "== 0/4 本机前置"
PY=""
for cand in python3 python py; do
  command -v "$cand" >/dev/null 2>&1 || continue
  "$cand" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 8) else 1)' 2>/dev/null || continue
  PY="$cand"; break
done
[ -n "$PY" ] || die "没找到 Python 3.8+，请先安装（https://www.python.org/downloads/）"
if ! "$PY" -c 'import paramiko' 2>/dev/null; then
  echo "缺 paramiko（SSH 传输层），正在安装…"
  "$PY" -m pip install --quiet --user paramiko || die "paramiko 安装失败，请手动 pip install paramiko"
fi
# 🔴 校验配置与密钥**成对**：这两个文件必须同源，任何一个换过整包就是废的，
# 而症状是部署到最后一步才报「敏感字段解密失败」。宁可在没连服务器之前就红。
if ! "$PY" -c 'import cryptography' 2>/dev/null; then
  "$PY" -m pip install --quiet --user cryptography || die "cryptography 安装失败"
fi
DMS_SECRET_KEY="$(cat config/secret.key)" DMSAI_SETTINGS="$(native "$PWD/config/settings.docker.json")" \
  "$PY" - <<'PY' || die "包内 config/ 的配置与密钥对不上（密文解不开），这个包不可用"
import sys, os
sys.path.insert(0, os.path.abspath(os.path.join("source", "tools")))
import settings as st
cfg = st.load()
still = [k for k in ("pg_url", "mysql_url", "llm_api_key") if str(cfg.get(k, "")).startswith("enc:v1:")]
raise SystemExit(f"仍是密文：{still}" if still else 0)
PY
echo "OK  python=$($PY -V 2>&1 | awk '{print $2}')，config/ 的凭据可解密"

echo "== 1/4 打包 source/"
TMP="${TMPDIR:-/tmp}/dms-ai-bundle-$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT
tar -czf "$TMP/src.tar.gz" -C source .
echo "OK  $TMP/src.tar.gz（$(du -h "$TMP/src.tar.gz" | cut -f1)）"

if [ "$MODE" = dryrun ]; then
  echo
  echo "--dry-run：本机自检与打包都通过，未连接服务器。"
  exit 0
fi

: "${DEPLOY_HOST:=}"
if [ -z "$DEPLOY_HOST" ]; then
  read -r -p "服务器地址（IP 或域名）: " DEPLOY_HOST
fi
: "${DEPLOY_USER:=root}"
: "${DEPLOY_PORT:=22}"
if [ -z "${DEPLOY_PW:-}" ]; then
  read -r -s -p "$DEPLOY_USER@$DEPLOY_HOST 的密码: " DEPLOY_PW
  echo
fi
[ -n "$DEPLOY_PW" ] || die "密码为空"
RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
export DEPLOY_HOST DEPLOY_USER DEPLOY_PORT DEPLOY_PW DMS_RUNTIME_ROOT="$RUNTIME_ROOT"

# 首次连接会因为主机键未知被拒（_deploy.py 用 RejectPolicy，防中间人）。这里明确告知怎么处理，
# 而不是让人对着一句 paramiko 异常发呆。
if ! "$PY" source/tools/_deploy.py run 'echo ok' 30 >/dev/null 2>&1; then
  cat >&2 <<EOF
ERROR: 连不上 $DEPLOY_USER@$DEPLOY_HOST:$DEPLOY_PORT。常见两种原因：
  1) 主机键不在 known_hosts（部署脚本**不会**自动信任陌生主机）。先执行一次：
       ssh-keyscan -p $DEPLOY_PORT $DEPLOY_HOST >> ~/.ssh/known_hosts
     并从可信渠道核对指纹；也可用 DEPLOY_KNOWN_HOSTS=/path/to/known_hosts 指定专用文件。
  2) 地址/端口/密码不对。
EOF
  exit 1
fi

if [ "$MODE" = bootstrap ]; then
  echo "== 2/4 全新机器前置（PG / venv / systemd / web 容器；每步幂等，已有的不动）"
  SEED="$RUNTIME_ROOT/seed"
  BOOT_RELEASE="$RUNTIME_ROOT/releases/bootstrap-$(date -u '+%Y%m%dT%H%M%SZ')"
  "$PY" source/tools/_deploy.py run "mkdir -p '$SEED' '$BOOT_RELEASE' && chmod 700 '$SEED'" 60
  "$PY" source/tools/_deploy.py bput "$(native "$PWD/config/settings.docker.json")" "$SEED/settings.docker.json"
  "$PY" source/tools/_deploy.py bput "$(native "$PWD/config/secret.key")" "$SEED/secret.key"
  [ -s payload/registry_snapshot.json ] \
    && "$PY" source/tools/_deploy.py bput "$(native "$PWD/payload/registry_snapshot.json")" "$SEED/registry_snapshot.json"
  [ -s payload/requirements-embed.lock.txt ] \
    && "$PY" source/tools/_deploy.py bput "$(native "$PWD/payload/requirements-embed.lock.txt")" "$SEED/requirements-embed.lock.txt"
  "$PY" source/tools/_deploy.py bput "$(native "$TMP/src.tar.gz")" "$RUNTIME_ROOT/src.tar.gz"
  "$PY" source/tools/_deploy.py run "set -e; tar -xzf '$RUNTIME_ROOT/src.tar.gz' -C '$BOOT_RELEASE'; chmod 600 '$SEED'/*" 120
  "$PY" source/tools/_deploy.py run \
    "DMS_RUNTIME_ROOT='$RUNTIME_ROOT' DMS_SEED_DIR='$SEED' bash '$BOOT_RELEASE/scripts/server-bootstrap.sh'" 1800
else
  echo "== 2/4 更新模式：跳过前置铺设（要铺请加 --bootstrap）"
fi

echo "== 3/4 上传 + 服务器构建 + 原子切换 + 自检（构建 5-10 分钟）"
DEPLOY_SRC_TAR="$(native "$TMP/src.tar.gz")" DEPLOY_WEB_TAR="$(native "$PWD/payload/web-dist.tar.gz")" \
  bash source/tools/deploy_update.sh

if [ "$MODE" = bootstrap ] && [ -s payload/registry_snapshot.json ]; then
  echo "== 4/4 导入业务字典种子（幂等；它决定问数准确性的一半）"
  "$PY" source/tools/_deploy.py run \
    "cd '$RUNTIME_ROOT' && DMS_SECRET_KEY=\"\$(cat .secret_key)\" DMSAI_SETTINGS='$RUNTIME_ROOT/settings.docker.json' \
     venv/bin/python3 app/tools/registry_snapshot.py import '$RUNTIME_ROOT/seed/registry_snapshot.json'" 900
else
  echo "== 4/4 业务字典种子：更新模式不需要重导（库里已有）"
fi

echo
echo "部署完成。人工冒烟三题：本月销售额 / 现在总库存量是多少 / 市场费用的报销政策是什么"
