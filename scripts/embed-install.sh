#!/usr/bin/env bash
# 一键安装/更新向量·精排·解析服务（容器形态）。**幂等**：重复跑收敛，不重复建。
#
# 装完即自启：`--restart unless-stopped` —— 机器重启、docker 重启都自己回来。
# 这正是它要替掉的那套形态解决不了的问题（2026-08-17 现场：8078 上是个手工起的裸 python，
# 重启即失、部署换代码也不跟着变，而 `/api/health` 全绿）。
#
# 用法（在服务器上，源码树里）：
#   DMS_RUNTIME_ROOT=/opt/dms-ai bash scripts/embed-install.sh
#
# 接管旧形态：8078 若被 systemd 单元或裸进程占着，**默认拒绝**并说清占用者是谁。
# 确认可以中断几秒后：
#   DMS_EMBED_TAKEOVER=1 bash scripts/embed-install.sh
set -euo pipefail

die() { echo "ERROR: $*" >&2; exit 1; }
step() { echo; echo "── $*"; }

APP_ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
RUNTIME_ROOT="${RUNTIME_ROOT%/}"
IMAGE="${DMS_EMBED_IMAGE:-dms-ai-embed}"
NAME="${DMS_EMBED_CONTAINER:-dms-ai-embed}"
BIND="${DMS_EMBED_BIND:-172.17.0.1}"
PORT="${DMS_EMBED_PORT:-8078}"
TAKEOVER="${DMS_EMBED_TAKEOVER:-0}"
SETTINGS="$RUNTIME_ROOT/settings.docker.json"
SECRET="$RUNTIME_ROOT/.secret_key"
KBDATA="$RUNTIME_ROOT/kbdata"

command -v docker >/dev/null || die "缺 docker"
[ -f "$SETTINGS" ] || die "缺运行时配置：$SETTINGS"
[ -s "$SECRET" ] || die "缺运行时密钥：$SECRET"
mkdir -p "$KBDATA"
KBDATA_REAL="$(readlink -f "$KBDATA")"

step "1/5 配置自检（起飞前就要知道 key 在不在）"
# serve() 启动时 `_qwen_key()` 没配会直接 SystemExit —— 与其让容器起不来再去翻日志，
# 不如在这里先说清楚。顺便证明 settings 与 .secret_key 是**成对**的。
DMS_SECRET_KEY="$(cat "$SECRET")" DMSAI_SETTINGS="$SETTINGS" \
  python3 - "$APP_ROOT" <<'PY' || die "配置读不通：凭据解不开，或千问 key 没配"
import sys, os
sys.path.insert(0, os.path.join(sys.argv[1], "tools"))
import settings as st
cfg = st.load()
key = (cfg.get("llm_keys") or {}).get("qwen") or cfg.get("llm_api_key") or ""
if str(cfg.get("pg_url", "")).startswith("enc:v1:"):
    raise SystemExit("凭据仍是密文：settings 与 .secret_key 不成对")
if not key or key.startswith("enc:v1:"):
    raise SystemExit("千问 key 未配置或解不开（llm_keys.qwen / llm_api_key）")
url = str(cfg.get("service_url", ""))
print(f"OK  凭据可解密，千问 key 在；settings 的 service_url = {url}")
PY

step "2/5 构建镜像 $IMAGE（首次几分钟，之后 apt/pip 两层走缓存）"
( cd "$APP_ROOT" && docker build -f docker/embed/Dockerfile -t "$IMAGE" . ) || die "镜像构建失败"

step "3/5 端口 $BIND:$PORT 的占用情况"
OCCUPANT=""
if docker inspect --type container "$NAME" >/dev/null 2>&1; then
  OCCUPANT="本服务的旧容器 $NAME"
elif command -v systemctl >/dev/null 2>&1 && systemctl cat dms-ai-embed >/dev/null 2>&1 \
     && [ "$(systemctl is-active dms-ai-embed 2>/dev/null || true)" = active ]; then
  OCCUPANT="systemd 单元 dms-ai-embed（宿主机 venv 形态）"
elif command -v ss >/dev/null 2>&1 && ss -lntp 2>/dev/null | grep -q "$BIND:$PORT"; then
  # ss 的 users:((...)) 那段带大量填充空白，直接切出来当文案会糊满一屏；只取 名字,pid=N
  OCCUPANT="裸进程 $(ss -lntpH 2>/dev/null | grep "$BIND:$PORT"     | grep -oE '\("[^"]+",pid=[0-9]+' | tr -d '("' | head -1)"
fi

if [ -n "$OCCUPANT" ]; then
  echo "占用者：$OCCUPANT"
  case "$OCCUPANT" in
    "本服务的旧容器"*) ;;   # 自己的容器，下面直接换掉，不需要额外确认
    *)
      # 🔴 接管 = 向量/解析服务中断几秒。默认不许悄悄干这件事：
      # 上传中的文档、正在检索的问句都会受影响，什么时候做该由运维决定。
      [ "$TAKEOVER" = 1 ] || die "$OCCUPANT 正占着 $BIND:$PORT。
接管会中断向量/解析服务数秒（上传与检索会短暂失败）。确认可以后重跑：
  DMS_EMBED_TAKEOVER=1 bash scripts/embed-install.sh"
      if systemctl cat dms-ai-embed >/dev/null 2>&1; then
        echo "停用并禁用 systemd 单元 dms-ai-embed（容器形态取而代之）"
        systemctl disable --now dms-ai-embed >/dev/null 2>&1 || true
      fi
      # 裸进程（孤儿）：systemd 管不着它，只能按监听端口找出来收掉。
      if command -v ss >/dev/null 2>&1; then
        for pid in $(ss -lntpH 2>/dev/null | grep "$BIND:$PORT" | grep -oE 'pid=[0-9]+' | cut -d= -f2 | sort -u); do
          echo "收掉占用 $BIND:$PORT 的裸进程 pid=$pid"
          kill "$pid" 2>/dev/null || true
        done
        sleep 2
      fi
      ;;
  esac
fi

step "4/5 起容器（--restart unless-stopped：机器重启自己回来）"
docker rm -f "$NAME" >/dev/null 2>&1 || true
# 🔴 kbdata 必须与 dms-ai-server 挂的是**同一个宿主目录**：解析接口收到的是
# `/kbdata/<doc_id>.<ext>` 路径而不是文件字节，两边指到不同目录会稳定 404。
# server-restart.sh 的预检会核对这一条（它按容器名找解析服务并比对 mount 源）。
docker run -d --name "$NAME" --restart unless-stopped \
  --env DMS_SECRET_KEY="$(cat "$SECRET")" \
  --env DMS_EMBED_MODEL="${DMS_EMBED_MODEL:-text-embedding-v4}" \
  --mount "type=bind,source=$SETTINGS,target=/app/settings.json,readonly" \
  --mount "type=bind,source=$KBDATA_REAL,target=/kbdata" \
  -p "$BIND:$PORT:8078" \
  "$IMAGE" >/dev/null || die "容器启动失败（docker logs $NAME）"

step "5/5 起飞自检"
HEALTH=""
for _ in $(seq 1 40); do
  if HEALTH="$(curl -fsS -m 3 "http://$BIND:$PORT/health" 2>/dev/null)"; then break; fi
  sleep 2
done
[ -n "$HEALTH" ] || {
  docker logs --tail 30 "$NAME" 2>&1 || true
  die "80 秒内 http://$BIND:$PORT/health 不可达"
}
printf '%s' "$HEALTH" | python3 -c '
import json, sys
d = json.load(sys.stdin)
caps = d.get("parse_ok") or {}
if d.get("ok") is not True or not caps.get("text"):
    raise SystemExit(f"服务不健康：{d}")
# 容器的意义就在这几项：宿主机上 doc/xls/ppt 常年 false（缺 LibreOffice），镜像里该全绿。
missing = [k for k in ("pdf", "docx", "xlsx", "pptx", "doc", "xls", "ppt", "image") if not caps.get(k)]
# 🔴 不用 f-string 插值带引号的表达式：Python 3.10 的 f-string **不许**表达式部分含反斜杠
# （PEP 701 到 3.12 才放开）。本脚本要跑在别人的服务器上，那里可能是 3.10 ——
# 2026-08-17 在 38.76.188.118（Python 3.10.12）实测炸过一次：容器全绿，自检自己 SyntaxError。
print("OK  model=%s dim=%s rerank=%s" % (d.get("model"), d.get("dim"), d.get("rerank_model")))
print("    解析能力全绿" if not missing else f"    ⚠️ 这些格式不可用：{missing}（镜像该带全，缺了说明构建有问题）")
' || { docker logs --tail 30 "$NAME" 2>&1 || true; die "健康检查未通过"; }

echo
echo "装好了。容器 $NAME 已设 --restart unless-stopped，机器重启自己回来。"
echo "settings 的 service_url 必须指到 $BIND:$PORT（当前值见上面第 1 步）。"
