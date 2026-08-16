#!/usr/bin/env bash
# dms-ai-server 容器（重）启动。
# 版本化源码在 APP_ROOT；配置、密钥和知识库原件在独立的 RUNTIME_ROOT，升级不得覆盖。
set -euo pipefail

die() {
  echo "ERROR: $*" >&2
  exit 1
}

APP_ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
RUNTIME_ROOT="${RUNTIME_ROOT%/}"
SERVER_HOST="${DMS_SERVER_HOST:-172.17.0.1}"
HEALTH_URL="http://${SERVER_HOST}:8100/api/health"

case "$RUNTIME_ROOT" in
  /*) ;;
  *) die "DMS_RUNTIME_ROOT 必须是绝对路径：$RUNTIME_ROOT" ;;
esac

SETTINGS_FILE="$RUNTIME_ROOT/settings.docker.json"
SECRET_FILE="$RUNTIME_ROOT/.secret_key"
KBDATA_DIR="$RUNTIME_ROOT/kbdata"
TOOLS_DIR="$APP_ROOT/tools"

[ -f "$SETTINGS_FILE" ] || die "缺少运行时配置：$SETTINGS_FILE"
[ -s "$SECRET_FILE" ] || die "缺少或为空的运行时密钥：$SECRET_FILE"
[ -d "$TOOLS_DIR" ] || die "缺少版本化 tools 目录：$TOOLS_DIR"
[ "$(wc -c < "$SECRET_FILE")" -ge 32 ] || die "运行时密钥必须至少 32 字节：$SECRET_FILE"

# 容器内写入和解析服务接收的路径字面量必须都是 /kbdata；配置成 data/kb 会写进镜像层。
SERVICE_URL="$(python3 - "$SETTINGS_FILE" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8-sig") as f:
        cfg = json.load(f)
except Exception as exc:
    print(f"无法读取配置 JSON：{exc}", file=sys.stderr)
    raise SystemExit(1)

if cfg.get("kb_root") != "/kbdata":
    print("容器部署要求 settings.docker.json 的 kb_root 严格等于 /kbdata", file=sys.stderr)
    raise SystemExit(1)
service_url = str(cfg.get("service_url", "")).rstrip("/")
if not service_url.startswith(("http://", "https://")):
    print("settings.docker.json 的 service_url 必须是 http/https 地址", file=sys.stderr)
    raise SystemExit(1)
print(service_url)
PY
 )" || die "settings.docker.json 校验失败"

mkdir -p "$KBDATA_DIR"
KBDATA_REAL="$(readlink -f "$KBDATA_DIR")"
[ -n "$KBDATA_REAL" ] || die "无法解析知识库目录：$KBDATA_DIR"

# 先在宿主持久目录真写一份探针；后面让配置中的真实解析服务解析它，再由 server 容器读取。
umask 077
PROBE_NAME=".dms-kb-probe-$$"
PROBE_HOST="$KBDATA_REAL/$PROBE_NAME"
PROBE_TOKEN="dms-kb-shared-path-probe-$$-$(date +%s)"
cleanup_probe() {
  rm -f -- "$PROBE_HOST"
}
trap cleanup_probe EXIT
printf '%s\n' "$PROBE_TOKEN" > "$PROBE_HOST" || die "知识库目录不可写：$KBDATA_REAL"
[ -s "$PROBE_HOST" ] || die "知识库目录写入探针失败：$KBDATA_REAL"

if docker inspect dms-ai-parser >/dev/null 2>&1; then
  # parser 容器必须把同一个宿主目录 bind 到 /kbdata；Docker volume 或另一目录都会稳定 404。
  [ "$(docker inspect --format '{{.State.Running}}' dms-ai-parser)" = "true" ] \
    || die "dms-ai-parser 容器存在但未运行"
  PARSER_KB_MOUNT="$(docker inspect --format '{{range .Mounts}}{{if eq .Destination "/kbdata"}}{{.Type}}|{{.Source}}{{end}}{{end}}' dms-ai-parser)"
  case "$PARSER_KB_MOUNT" in
    bind\|*) ;;
    *) die "dms-ai-parser 必须把宿主目录 bind mount 到 /kbdata" ;;
  esac
  PARSER_KB_SOURCE="${PARSER_KB_MOUNT#bind|}"
  [ -e "$PARSER_KB_SOURCE" ] || die "parser 的 /kbdata 源目录不存在：$PARSER_KB_SOURCE"
  [ "$PARSER_KB_SOURCE" -ef "$KBDATA_REAL" ] \
    || die "parser 与 server 的 /kbdata 不是同一宿主目录：$PARSER_KB_SOURCE != $KBDATA_REAL"
  docker exec dms-ai-parser sh -c 'test -r "$1"' _ "/kbdata/$PROBE_NAME" \
    || die "parser 容器看不到知识库探针，请检查 /kbdata mount"
else
  # host parser 直接按收到的 /kbdata/<id> 路径读文件；不存在时建软链，已有异源路径绝不覆盖。
  if [ -e /kbdata ] || [ -L /kbdata ]; then
    [ /kbdata -ef "$KBDATA_REAL" ] \
      || die "宿主 /kbdata 已存在但未指向 $KBDATA_REAL，请人工处理冲突"
  else
    ln -s "$KBDATA_REAL" /kbdata \
      || die "无法建立宿主解析路径：/kbdata -> $KBDATA_REAL"
  fi
  [ "/kbdata/$PROBE_NAME" -ef "$PROBE_HOST" ] \
    || die "宿主解析服务看到的 /kbdata 与持久目录不同源"
fi

# 宿主上的 curl 不一定能解析容器专用的 host.docker.internal；用 server 容器同款网关映射，
# 但 URL、端口与 Host 头仍严格来自 settings。
SERVICE_RESOLVE="$(python3 - "$SERVICE_URL" <<'PY'
import sys
import socket
from urllib.parse import urlsplit

u = urlsplit(sys.argv[1])
if u.hostname == "host.docker.internal":
    try:
        gateway = socket.gethostbyname(u.hostname)
    except OSError:
        gateway = "172.17.0.1"
    print(f"{u.hostname}:{u.port or (443 if u.scheme == 'https' else 80)}:{gateway}")
PY
)" || die "无法解析 service_url：$SERVICE_URL"
SERVICE_CURL=(curl -fsS -m 10)
[ -z "$SERVICE_RESOLVE" ] || SERVICE_CURL+=(--resolve "$SERVICE_RESOLVE")

# 路径可见只证明 mount 正确；必须让 settings 中的真实服务读回唯一 token。
PARSE_BODY="$(printf '{\"path\":\"/kbdata/%s\",\"mime\":\"text/plain\"}' "$PROBE_NAME")"
PARSER_PARSE="$("${SERVICE_CURL[@]}" -H 'Content-Type: application/json' \
  --data-binary "$PARSE_BODY" "$SERVICE_URL/parse")" \
  || die "配置中的解析服务无法读取知识库探针：$SERVICE_URL/parse"
printf '%s' "$PARSER_PARSE" | python3 -c '
import json, sys
data = json.load(sys.stdin)
blocks = data.get("blocks") or []
text = "\n".join(str(x.get("text", "")) for x in blocks if isinstance(x, dict))
if sys.argv[1] not in text:
    raise SystemExit("解析服务响应未包含知识库探针 token")
' "$PROBE_TOKEN" || die "配置中的解析服务没有读取同一份 /kbdata 探针"

# 能力检查也必须打 settings 中 server 实际使用的服务，不能检查碰巧存在的固定容器。
PARSER_HEALTH_URL="$SERVICE_URL/health"
# 只要求基础文本 + 至少一种常用文档格式，避免把全部可选格式变成生产硬依赖。
PARSER_HEALTH="$("${SERVICE_CURL[@]}" "$PARSER_HEALTH_URL")" \
  || die "解析服务健康检查不可达：$PARSER_HEALTH_URL"
printf '%s' "$PARSER_HEALTH" | python3 -c '
import json, sys
data = json.load(sys.stdin)
caps = data.get("parse_ok") or {}
document_ok = any(caps.get(name) is True for name in ("xlsx", "pdf", "docx"))
if data.get("ok") is not True or caps.get("text") is not True or not document_ok:
    raise SystemExit("解析服务能力不足：要求 ok=true、text=true，且 xlsx/pdf/docx 至少一个为 true")
' || die "解析服务健康或基础解析能力校验失败"

# 所有会影响现有服务的动作都放在预检之后。旧容器先保留为回滚副本；新版本只有
# mount 探针和 health 都通过后才删除旧容器。
ROLLBACK_CONTAINER="dms-ai-server-rollback"
if docker inspect "$ROLLBACK_CONTAINER" >/dev/null 2>&1; then
  die "发现上次部署遗留容器 $ROLLBACK_CONTAINER，请先核对并人工处理"
fi
HAD_OLD_SERVER=0
if docker inspect dms-ai-server >/dev/null 2>&1; then
  docker rename dms-ai-server "$ROLLBACK_CONTAINER" || die "无法保留旧 server 容器"
  if ! docker stop "$ROLLBACK_CONTAINER" >/dev/null; then
    docker rename "$ROLLBACK_CONTAINER" dms-ai-server >/dev/null 2>&1 || true
    die "无法停止旧 server 容器"
  fi
  HAD_OLD_SERVER=1
fi

rollback_server() {
  docker rm -f dms-ai-server >/dev/null 2>&1 || true
  if [ "$HAD_OLD_SERVER" -eq 1 ]; then
    docker rename "$ROLLBACK_CONTAINER" dms-ai-server >/dev/null 2>&1 || return 1
    docker start dms-ai-server >/dev/null || return 1
  fi
}

DEPLOY_COMMITTED=0
rollback_on_exit() {
  rc=$?
  if [ "$DEPLOY_COMMITTED" -ne 1 ]; then
    rollback_server || echo "ERROR: 新版本失败，且旧版本自动恢复失败，请立即人工处理" >&2
  fi
  trap - EXIT HUP INT TERM
  cleanup_probe
  exit "$rc"
}
trap rollback_on_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

DMS_SECRET_KEY="$(< "$SECRET_FILE")"
# 精排默认接本机的 embed 服务 —— 它已经是千问适配层（`/rerank` 走 gte-rerank-v2），
# 与 `/embed` 同一个进程同一个端口，所以 base 直接取 settings 的 `service_url`。
# 🔴 默认接上而不是默认关：关着的时候检索**不报错、只是排不到第一**（生产实测
# recall@6=0.95 / recall@1=0.15），这类「哑掉的降级」在本仓按默认开处理。
# 显式传空串即可关掉（`${VAR+set}` 判的是「设过没有」，不是「是不是空」）。
if [ -z "${DMS_RERANK_BASE_URL+set}" ]; then
  DMS_RERANK_BASE_URL="$(python3 -c "import json,io;print(json.load(io.open('$SETTINGS_FILE',encoding='utf-8')).get('service_url',''))" 2>/dev/null || true)"
fi
# 🔴 rerank（B5 精排）的三个变量随 docker run 透传：接一个 Cohere/Jina 形状的端点即生效，
# 不改代码。它是唯一专治「块召回到了但排不到第一」的组件 —— 生产度量
# recall@6=0.95 / recall@1=0.15，0.80 的头寸全在这条链上。变量未设时
# `RerankClient::from_env` 返 None，检索退回纯 RRF 排序（与接线前逐字一致）并留一句日志。
#
# ⚠️ 注释**只能写在 docker run 之外**：行继续符后面接 `#` 会把命令的剩余参数
# 整段吃成注释，而 `bash -n` 查不出来（语法仍合法）—— 第一版就是这么写的。
# RUST_LOG 透传（默认空 = 与接线前逐字一致）：要诊断某条路时
# `RUST_LOG=dms_agent=debug bash scripts/server-restart.sh` 就能开，不改脚本、不重建镜像。
if ! docker run -d --name dms-ai-server --restart unless-stopped \
  --add-host host.docker.internal:host-gateway \
  --env DMS_SECRET_KEY="$DMS_SECRET_KEY" \
  --env DMS_RERANK_BASE_URL="${DMS_RERANK_BASE_URL:-}" \
  --env DMS_RERANK_API_KEY="${DMS_RERANK_API_KEY:-}" \
  --env DMS_RERANK_MODEL="${DMS_RERANK_MODEL:-gte-rerank-v2}" \
  --env RUST_LOG="${RUST_LOG:-}" \
  --mount "type=bind,source=$SETTINGS_FILE,target=/app/settings.json" \
  --mount "type=bind,source=$KBDATA_REAL,target=/kbdata" \
  --mount "type=bind,source=$TOOLS_DIR,target=/app/tools" \
  -p "${SERVER_HOST}:8100:8100" \
  dms-ai-server; then
  die "新 server 容器启动失败"
fi
echo "container started: app=$APP_ROOT runtime=$RUNTIME_ROOT"

if ! docker exec dms-ai-server sh -c 'test -r "$1" && test -w /kbdata' _ "/kbdata/$PROBE_NAME"; then
  docker logs --tail 15 dms-ai-server || true
  die "server 容器无法读写持久化 /kbdata"
fi

health_ok() {
  python3 -c 'import json,sys; data=json.load(sys.stdin); raise SystemExit(0 if data.get("ok") is True else 1)'
}

LAST_HEALTH=""
for _ in $(seq 1 90); do
  if HEALTH_BODY="$(curl -fsS -m 5 "$HEALTH_URL" 2>/dev/null)"; then
    LAST_HEALTH="$HEALTH_BODY"
    if printf '%s' "$HEALTH_BODY" | health_ok; then
      printf 'HEALTH: %s\n' "$HEALTH_BODY"
      if [ "$HAD_OLD_SERVER" -eq 1 ]; then
        docker rm -f "$ROLLBACK_CONTAINER" >/dev/null
      fi
      DEPLOY_COMMITTED=1
      trap cleanup_probe EXIT
      trap - HUP INT TERM
      exit 0
    fi
  fi
  sleep 2
done

echo "HEALTH TIMEOUT: $HEALTH_URL" >&2
[ -z "$LAST_HEALTH" ] || printf 'LAST HEALTH: %s\n' "$LAST_HEALTH" >&2
docker logs --tail 15 dms-ai-server || true
echo "新版本健康检查失败，将恢复旧版本" >&2
exit 1
