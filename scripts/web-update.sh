#!/usr/bin/env bash
# 原子更新 dms-ai-web。生产容器把宿主 dist 只读 bind 到 nginx，必须在宿主切目录后重启容器。
set -euo pipefail

ARCHIVE="${1:?用法: web-update.sh WEB_DIST_TAR [CONTAINER]}"
CONTAINER="${2:-dms-ai-web}"
DEST="/usr/share/nginx/html"

if ! docker inspect "$CONTAINER" >/dev/null 2>&1; then
  echo "SKIP: $CONTAINER 容器不存在，请按现网 web 发布流程使用 $ARCHIVE"
  exit 0
fi

MOUNT="$(docker inspect --format '{{range .Mounts}}{{if eq .Destination "/usr/share/nginx/html"}}{{.Type}}|{{.Source}}{{end}}{{end}}' "$CONTAINER")"
case "$MOUNT" in
  bind\|*) ;;
  *) echo "ERROR: $CONTAINER 必须把宿主 Web 目录 bind mount 到 $DEST，实际：${MOUNT:-无挂载}" >&2; exit 1 ;;
esac

ROOT="${MOUNT#bind|}"
[ -d "$ROOT" ] || { echo "ERROR: Web 宿主目录不存在：$ROOT" >&2; exit 1; }
PARENT="$(dirname "$ROOT")"
STAMP="$(date -u '+%Y%m%dT%H%M%SZ')-$$"
STAGE="$PARENT/.dist.next-$STAMP"
BACKUP="$PARENT/dist.rollback-$STAMP"
FAILED="$PARENT/dist.failed-$STAMP"

cleanup_stage() { rm -rf -- "$STAGE"; }
trap cleanup_stage EXIT
mkdir -p "$STAGE"
tar -xzf "$ARCHIVE" -C "$STAGE"
[ -s "$STAGE/index.html" ] || { echo "ERROR: Web 包缺少有效 index.html" >&2; exit 1; }
NEW_HASH="$(sha256sum "$STAGE/index.html" | awk '{print $1}')"

rollback_web() {
  docker stop "$CONTAINER" >/dev/null 2>&1 || true
  [ ! -e "$ROOT" ] || mv "$ROOT" "$FAILED" 2>/dev/null || true
  [ ! -e "$BACKUP" ] || mv "$BACKUP" "$ROOT" 2>/dev/null || true
  docker start "$CONTAINER" >/dev/null 2>&1 || true
}

mv "$ROOT" "$BACKUP"
mv "$STAGE" "$ROOT"
if ! docker restart "$CONTAINER" >/dev/null; then
  rollback_web
  exit 1
fi

READY=0
for _ in $(seq 1 20); do
  if docker exec "$CONTAINER" test -s "$DEST/index.html" >/dev/null 2>&1; then
    READY=1
    break
  fi
  sleep 1
done
[ "$READY" -eq 1 ] || { rollback_web; echo "ERROR: Web 容器重启后未就绪" >&2; exit 1; }

MOUNTED_HASH="$(docker exec "$CONTAINER" sha256sum "$DEST/index.html" | awk '{print $1}')"
[ "$MOUNTED_HASH" = "$NEW_HASH" ] || {
  rollback_web
  echo "ERROR: Web 容器仍读取旧资源" >&2
  exit 1
}

trap - EXIT
echo "web-refreshed: host=$ROOT sha256=$NEW_HASH rollback=$BACKUP"
