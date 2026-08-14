#!/usr/bin/env bash
# 服务器磁盘清理。**默认只报告不动手**，加 --apply 才真删。
#
# 为什么需要它：部署链路此前没有任何保留策略 —— 每次 `deploy_update.sh` 都
#   ① 解一份新的 `$RUNTIME_ROOT/releases/<版本>`（源码，几十 MB，只增不减）
#   ② `docker build -t dms-ai-server .` 重建镜像：旧的同名镜像立刻变成悬空 `<none>`，
#      而 builder 阶段带着整个 cargo target，**每次几 GB**
#   ③ BuildKit 构建缓存同样只涨不清
# 于是「部署几次磁盘就满」。本脚本清 ①②③ 的历史垃圾；`deploy_update.sh` 末尾已接入
# 同一份保留策略，防止再攒起来。
#
# 用法：
#   bash scripts/server-cleanup.sh                 # 只报告（安全，随时可跑）
#   bash scripts/server-cleanup.sh --apply         # 执行清理，releases 保留最近 3 个
#   KEEP_RELEASES=5 bash scripts/server-cleanup.sh --apply
#
# 🔴 永不触碰：$RUNTIME_ROOT/kbdata（知识库原件，丢了 DB 里的 doc 行就指向空）、
#    settings.docker.json、.secret_key（丢了凭据解不开）、正在使用的镜像与运行中的容器。
set -euo pipefail

RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
RUNTIME_ROOT="${RUNTIME_ROOT%/}"
RELEASES_ROOT="$RUNTIME_ROOT/releases"
APP_ROOT="$RUNTIME_ROOT/app"
KEEP_RELEASES="${KEEP_RELEASES:-3}"
APPLY=0
[ "${1:-}" = "--apply" ] && APPLY=1

say() { printf '%s\n' "$*"; }
run() {
  if [ "$APPLY" = 1 ]; then
    say "  执行: $*"
    eval "$@"
  else
    say "  [dry-run] $*"
  fi
}

say "== 0/5 现状"
df -h "$RUNTIME_ROOT" 2>/dev/null || df -h /
say ""
docker system df 2>/dev/null || say "（docker system df 不可用）"
say ""

say "== 1/5 悬空镜像（历次部署的旧 dms-ai-server，每个含 builder 层，通常是最大头）"
dangling=$(docker images -f dangling=true -q 2>/dev/null | wc -l | tr -d ' ')
say "  悬空镜像 $dangling 个"
if [ "$dangling" != "0" ]; then
  docker images -f dangling=true --format '    {{.ID}}  {{.Size}}  {{.CreatedSince}}' 2>/dev/null | head -20
  run "docker image prune -f"
fi
say ""

say "== 2/5 BuildKit 构建缓存"
run "docker builder prune -f"
say ""

say "== 3/5 已退出的容器（不含运行中的）"
exited=$(docker ps -aq -f status=exited 2>/dev/null | wc -l | tr -d ' ')
say "  已退出容器 $exited 个"
[ "$exited" != "0" ] && run "docker container prune -f"
say ""

say "== 4/5 历史 release 目录（保留最近 $KEEP_RELEASES 个 + 当前 app 指向的那个）"
if [ -d "$RELEASES_ROOT" ]; then
  current=""
  [ -L "$APP_ROOT" ] && current="$(readlink -f "$APP_ROOT")"
  du -sh "$RELEASES_ROOT" 2>/dev/null | sed 's/^/  合计 /'
  # 按名字倒序（RELEASE_ID 是 UTC 时间戳前缀，字典序即时间序），跳过要保留的
  mapfile -t all < <(ls -1 "$RELEASES_ROOT" 2>/dev/null | sort -r)
  kept=0
  for name in "${all[@]}"; do
    dir="$RELEASES_ROOT/$name"
    if [ -n "$current" ] && [ "$(readlink -f "$dir")" = "$current" ]; then
      say "  保留（当前生效）: $name"
      continue
    fi
    kept=$((kept + 1))
    if [ "$kept" -le "$KEEP_RELEASES" ]; then
      say "  保留（回滚位 $kept/$KEEP_RELEASES）: $name"
      continue
    fi
    say "  待删: $name  $(du -sh "$dir" 2>/dev/null | cut -f1)"
    run "rm -rf '$dir'"
  done
else
  say "  无 $RELEASES_ROOT"
fi
say ""

say "== 5/5 其它可回收"
# 上传包残留：deploy 每次都会写一份 src.tar.gz，用完即可删
[ -f "$RUNTIME_ROOT/src.tar.gz" ] && { say "  $RUNTIME_ROOT/src.tar.gz $(du -sh "$RUNTIME_ROOT/src.tar.gz" | cut -f1)"; run "rm -f '$RUNTIME_ROOT/src.tar.gz'"; }
[ -f "$RUNTIME_ROOT/web-dist.tar.gz" ] && { say "  $RUNTIME_ROOT/web-dist.tar.gz $(du -sh "$RUNTIME_ROOT/web-dist.tar.gz" | cut -f1)"; run "rm -f '$RUNTIME_ROOT/web-dist.tar.gz'"; }
# 容器日志：json-file 驱动默认无上限，长跑服务的 *-json.log 能涨到几 GB
for c in dms-ai-server dms-ai-parser dms-ai-web; do
  logf=$(docker inspect --format '{{.LogPath}}' "$c" 2>/dev/null || true)
  [ -n "$logf" ] && [ -f "$logf" ] && {
    say "  $c 日志 $(du -sh "$logf" 2>/dev/null | cut -f1)"
    run ": > '$logf'"
  }
done
# systemd 日志
if command -v journalctl >/dev/null 2>&1; then
  say "  journal $(journalctl --disk-usage 2>/dev/null | sed 's/.*take up //')"
  run "journalctl --vacuum-size=200M >/dev/null 2>&1 || true"
fi
say ""

say "== 清理后"
df -h "$RUNTIME_ROOT" 2>/dev/null || df -h /
docker system df 2>/dev/null || true
[ "$APPLY" = 1 ] || say ""
[ "$APPLY" = 1 ] || say "以上为 dry-run。确认无误后加 --apply 真正执行。"
