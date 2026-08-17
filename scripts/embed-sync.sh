#!/usr/bin/env bash
# 让向量·精排·解析服务跟上刚构建好的这份代码，并**证明**它跟上了。
#
# 服务有两种形态，本脚本认形态分派（部署侧只调这一条，不必知道对面是哪种）：
#   ① 容器（2026-08-17 起的生产形态）：代码在镜像里 → 重建镜像 + 换容器，
#      交给 `scripts/embed-install.sh`（幂等；占用者是自家容器时不需要 TAKEOVER）；
#   ② 宿主机 systemd 单元（存量兼容）：代码是 RUNTIME_ROOT 下一份**独立拷贝**。
#
# 为什么需要这一步：dms-ai-server 走容器、跟着 app 链接换版本，而向量服务是宿主机 systemd
# 单元。现网这个单元执行的是 `$RUNTIME_ROOT/tools/embed_service.py` —— 一份与 release 无关的
# 独立拷贝，deploy_update.sh 从来没同步过它。于是「改了 embed_service.py 部署上去」这件事
# 实际不生效，而症状只是检索/解析变差，任何健康检查都是绿的（哑掉的降级）。
# 2026-08-16 换千问那次两份能对上，只是因为有人手工传了一遍。
#
# 用法：DMS_RUNTIME_ROOT=/opt/dms-ai bash <app>/scripts/embed-sync.sh
set -euo pipefail

die() { echo "ERROR: $*" >&2; exit 1; }

APP_ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
RUNTIME_ROOT="${RUNTIME_ROOT%/}"
UNIT="${DMS_EMBED_UNIT:-dms-ai-embed}"
NAME="${DMS_EMBED_CONTAINER:-dms-ai-embed}"
RELEASE_FILE="$APP_ROOT/tools/embed_service.py"

[ -f "$RELEASE_FILE" ] || die "release 里没有 tools/embed_service.py：$RELEASE_FILE"

# ── 形态①：容器。代码在镜像里，重建即换代码；install 脚本自带起飞自检。
if command -v docker >/dev/null 2>&1 && docker inspect "$NAME" >/dev/null 2>&1; then
  echo "解析服务是容器形态（$NAME）：重建镜像并换容器"
  # 占用 8078 的就是它自己，接管无需人工确认 —— TAKEOVER 那道闸防的是「踩掉别人的服务」。
  DMS_RUNTIME_ROOT="$RUNTIME_ROOT" DMS_EMBED_CONTAINER="$NAME" DMS_EMBED_TAKEOVER=1 \
    bash "$APP_ROOT/scripts/embed-install.sh"
  exit 0
fi

# ── 形态②：宿主机 systemd 单元（存量）。
if ! command -v systemctl >/dev/null || ! systemctl cat "$UNIT" >/dev/null 2>&1; then
  echo "SKIP: 既没有容器 $NAME，也没有 systemd 单元 $UNIT。"
  echo "      若 8078 上仍有响应，那是个**没人管的裸进程**：重启机器即失，部署换代码它也不跟着变。"
  echo "      装成容器：DMS_RUNTIME_ROOT=$RUNTIME_ROOT DMS_EMBED_TAKEOVER=1 bash $APP_ROOT/scripts/embed-install.sh"
  exit 0
fi

# 单元实际在跑哪一份，以 systemd 自己的记录为准 —— 不假设路径，免得再多一份「约定」。
# 🔴 ExecStart 里的路径**可能是相对的**：现网那条就是
#   `sh -c '... exec /opt/dms-ai/venv/bin/python tools/embed_service.py serve 8078 ...'`
#   配 `WorkingDirectory=/opt/dms-ai`。只 grep 不还原工作目录，拿到的是 `tools/embed_service.py`，
#   再去 cp 就会打到本脚本自己所在的 release 上 —— 同步到错的地方，还一声不吭。
RUNNING_FILE="$(systemctl show -p ExecStart --value "$UNIT" \
  | grep -oE '[^ "]*embed_service\.py' | head -1)"
[ -n "$RUNNING_FILE" ] || die "无法从 $UNIT 的 ExecStart 里解析出 embed_service.py 路径"
case "$RUNNING_FILE" in
  /*) ;;
  *)
    UNIT_WD="$(systemctl show -p WorkingDirectory --value "$UNIT")"
    RUNNING_FILE="${UNIT_WD:-$RUNTIME_ROOT}/$RUNNING_FILE"
    ;;
esac
[ -f "$RUNNING_FILE" ] || die "$UNIT 声明在跑 $RUNNING_FILE，但这个文件不存在"

if [ "$RUNNING_FILE" -ef "$RELEASE_FILE" ]; then
  # 单元已经指向 app/（跟着 release 走）：无需拷贝，重启即换代码。
  echo "单元已跟随 release：$RUNNING_FILE"
  BACKUP=""
else
  # 独立拷贝形态：先备份，拷过去，失败能原样退回。
  echo "单元跑的是独立拷贝：$RUNNING_FILE（将从 release 同步）"
  BACKUP="$RUNNING_FILE.rollback-$$"
  cp -p "$RUNNING_FILE" "$BACKUP"
  cp -p "$RELEASE_FILE" "$RUNNING_FILE"
fi

restore() {
  [ -n "$BACKUP" ] || return 0
  cp -p "$BACKUP" "$RUNNING_FILE" 2>/dev/null || true
  systemctl restart "$UNIT" >/dev/null 2>&1 || true
}

systemctl restart "$UNIT" || { restore; die "$UNIT 重启失败"; }

# service_url 是唯一事实源；宿主 curl 未必认得 host.docker.internal，换成网关地址探同一个端口。
PROBE_URL="$(python3 - "$RUNTIME_ROOT/settings.docker.json" <<'PY'
import json, sys
from urllib.parse import urlsplit
with open(sys.argv[1], encoding="utf-8-sig") as f:
    url = str(json.load(f).get("service_url", "")).rstrip("/")
u = urlsplit(url)
host = "172.17.0.1" if u.hostname == "host.docker.internal" else u.hostname
print(f"{u.scheme}://{host}:{u.port or 80}")
PY
)" || { restore; die "无法从 settings 解析 service_url"; }

HEALTH=""
for _ in $(seq 1 30); do
  if HEALTH="$(curl -fsS -m 3 "$PROBE_URL/health" 2>/dev/null)"; then break; fi
  sleep 2
done
[ -n "$HEALTH" ] || { restore; die "$UNIT 重启后 $PROBE_URL/health 不可达（journalctl -u $UNIT -n 50）"; }
printf '%s' "$HEALTH" | python3 -c '
import json, sys
d = json.load(sys.stdin)
if d.get("ok") is not True or not d.get("model") or not d.get("dim"):
    raise SystemExit(f"embed 服务不健康：{d}")
# 同 embed-install.sh：3.10 的 f-string 表达式里不许有反斜杠，改 %% 格式化
print("embed-synced: model=%s dim=%s rerank=%s" % (d.get("model"), d.get("dim"), d.get("rerank_model")))
' || { restore; die "$UNIT 重启后健康检查未通过"; }

# 🔴 收尾判据：跑着的那份必须与 release 逐字节一致。没有这一条，上面全绿也可能只是
# 「旧代码依然健康」—— 这正是这个脚本要消灭的那种绿。
[ "$(sha256sum < "$RUNNING_FILE")" = "$(sha256sum < "$RELEASE_FILE")" ] \
  || { restore; die "同步后 $RUNNING_FILE 仍与 release 不一致"; }
[ -z "$BACKUP" ] || rm -f "$BACKUP"
echo "提示：宿主机形态是存量兼容。新装/迁移一律用容器（scripts/embed-install.sh）——"
echo "      依赖、代码、启动方式一起进版本库，且 --restart unless-stopped 天然开机自启。"
