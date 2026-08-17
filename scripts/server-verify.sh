#!/usr/bin/env bash
# 上线验收：**部署完到底成没成**，一条命令给出裁决。
#
# 为什么需要它：`/api/health` 的 ok=true 答的是「进程活着、库连得上」，答不了
# 「这台机器答题能力和现网一样吗」。2026-08-17 现场那台第二生产机就是这样 ——
# ok=true、vector_ready 三个 true、breakers 全 false，而 sql_exemplar 少 90 行、
# memory 少 48 行、98 条样例没有向量、embed 服务是个手工起的孤儿进程。
# 部署"成功"、服务"健康"、答案变差 —— 本仓最不能接受的那一类。
#
# 谁部署、怎么部署都躲不开这一条：手工解包也好、跑 deploy.sh 也好，跑它就知道。
#
# 用法：
#   bash scripts/server-verify.sh            # 裁决模式：有短缺就非零退出
#   ADVISORY=1 bash scripts/server-verify.sh # 只报不拦（server-restart.sh 收尾用）
#
# 可覆盖：DMS_RUNTIME_ROOT(/opt/dms-ai) DMS_PG_CONTAINER(dms-ai-pg)
#         DMS_SNAPSHOT(默认找 $RUNTIME_ROOT/seed/registry_snapshot.json)
set -uo pipefail

RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
RUNTIME_ROOT="${RUNTIME_ROOT%/}"
PG="${DMS_PG_CONTAINER:-dms-ai-pg}"
ADVISORY="${ADVISORY:-0}"
SNAPSHOT="${DMS_SNAPSHOT:-}"
if [ -z "$SNAPSHOT" ]; then
  for cand in "$RUNTIME_ROOT/seed/registry_snapshot.json" "$RUNTIME_ROOT/registry_snapshot.json"; do
    [ -s "$cand" ] && SNAPSHOT="$cand" && break
  done
fi

BAD=0
note() { echo "  $*"; }
bad()  { echo "  ❌ $*"; BAD=1; }
ok()   { echo "  ✅ $*"; }

echo "── 上线验收（$RUNTIME_ROOT）"

if ! docker inspect "$PG" >/dev/null 2>&1; then
  bad "找不到元数据库容器 $PG —— 注册表规模无法核对"
else
  # 注册表规模：有快照就拿快照的行数当基准（带上来多少行，库里就该不少于多少行）；
  # 没快照就只报数，并说清「这台机器的答题能力可能低于现网」——不知道基准就不假装知道。
  COUNTS="$(docker exec "$PG" psql -U postgres -d dms_ai -tA -F'|' -c "
    SELECT 'dimension', count(*) FROM meta.dimension UNION ALL
    SELECT 'value_map', count(*) FROM meta.value_map UNION ALL
    SELECT 'sql_exemplar', count(*) FROM meta.sql_exemplar UNION ALL
    SELECT 'term', count(*) FROM meta.term UNION ALL
    SELECT 'kw_force', count(*) FROM meta.kw_force UNION ALL
    SELECT 'memory', count(*) FROM meta.memory UNION ALL
    SELECT 'sql_exemplar_vec', count(*) FILTER (WHERE embedding IS NOT NULL) FROM meta.sql_exemplar
  " 2>/dev/null)"
  if [ -z "$COUNTS" ]; then
    bad "查不出注册表规模（$PG 里没有 dms_ai 库？）"
  elif [ -n "$SNAPSHOT" ]; then
    # 基准来自快照自己，不写死数字 —— 写死的数字下个月就是假的。
    WANT="$(python3 - "$SNAPSHOT" <<'PY'
import json, sys
tables = json.load(open(sys.argv[1], encoding="utf-8"))["tables"]
for name in ("dimension", "value_map", "sql_exemplar", "term", "kw_force", "memory"):
    rows = tables.get(name)
    rows = rows.get("rows") if isinstance(rows, dict) else rows
    print(f"{name}|{len(rows or [])}")
PY
)"
    note "基准取自 $SNAPSHOT"
    while IFS='|' read -r name want; do
      [ -n "${name:-}" ] || continue
      got="$(printf '%s\n' "$COUNTS" | awk -F'|' -v n="$name" '$1==n{print $2}')"
      if [ "${got:-0}" -lt "${want:-0}" ] 2>/dev/null; then
        bad "meta.$name：库里 ${got:-0} 行 < 快照 $want 行 —— 业务字典种子没导全，问数会明显变笨"
      else
        ok "meta.$name：${got:-0} / $want"
      fi
    done <<EOF
$WANT
EOF
  else
    note "没找到 registry_snapshot.json —— 只报数，不判合格（基准未知）"
    printf '%s\n' "$COUNTS" | while IFS='|' read -r n v; do note "meta.$n = $v"; done
    bad "缺业务字典种子快照：这台机器的答题能力可能明显低于现网（人工沉淀的样例/教训不在代码里）"
  fi

  # 样例向量：health 的 vector_ready 只覆盖 datasource/element/table_doc 三张表，
  # sql_exemplar **不在里面** —— 现场那台 98 条样例无向量，召回不到，而 health 全绿。
  TOT="$(printf '%s\n' "$COUNTS" | awk -F'|' '$1=="sql_exemplar"{print $2}')"
  VEC="$(printf '%s\n' "$COUNTS" | awk -F'|' '$1=="sql_exemplar_vec"{print $2}')"
  if [ "${TOT:-0}" -gt 0 ] 2>/dev/null; then
    if [ "${VEC:-0}" -lt "$TOT" ]; then
      bad "SQL 样例向量 ${VEC:-0}/$TOT —— 缺向量的召回不到（向量自愈每 10 分钟一轮，稍等再看；一直不动就查 embed 服务）"
    else
      ok "SQL 样例向量 $VEC/$TOT"
    fi
  fi
fi

# embed 服务：端口有响应**不等于**单元活着。现场那台单元 inactive，8078 上是个手工起的
# 裸 python 孤儿 —— 重启机器即失，且部署换代码它不会跟着变。
# 托管形态：容器（新）或 systemd 单元（存量）二者有一即可。两者都没有而端口还有响应，
# 说明那是个**没人管的裸进程** —— 重启机器即失，且部署换代码它不跟着变。
MANAGED=""
if command -v docker >/dev/null 2>&1 && docker inspect "${DMS_EMBED_CONTAINER:-dms-ai-embed}" >/dev/null 2>&1; then
  C="${DMS_EMBED_CONTAINER:-dms-ai-embed}"
  RUNNING="$(docker inspect --format '{{.State.Running}}' "$C" 2>/dev/null || echo false)"
  POLICY="$(docker inspect --format '{{.HostConfig.RestartPolicy.Name}}' "$C" 2>/dev/null || echo no)"
  if [ "$RUNNING" = true ]; then
    MANAGED="容器 $C"
    ok "向量/解析服务：容器 $C 运行中"
    # 没有重启策略 = 机器重启后服务不回来，而这正是换成容器要解决的事。
    case "$POLICY" in
      always|unless-stopped) ok "  重启策略 $POLICY（开机自启）" ;;
      *) bad "  容器 $C 的重启策略是 '$POLICY' —— 机器重启后不会自己回来（重装：scripts/embed-install.sh）" ;;
    esac
  else
    bad "容器 $C 存在但没在跑"
  fi
fi
if [ -z "$MANAGED" ] && command -v systemctl >/dev/null 2>&1 && systemctl cat dms-ai-embed >/dev/null 2>&1; then
  STATE="$(systemctl is-active dms-ai-embed 2>/dev/null || echo unknown)"
  if [ "$STATE" = active ]; then
    MANAGED="systemd 单元"
    ok "向量/解析服务：systemd 单元 active（存量形态；新装建议换容器 scripts/embed-install.sh）"
  else
    bad "dms-ai-embed 单元 $STATE —— 端口若仍有响应，那是孤儿进程"
  fi
fi
[ -n "$MANAGED" ] || bad "向量/解析服务没有被托管（既无容器也无 systemd 单元）—— 装法：bash scripts/embed-install.sh"

# 版本布局：源码平铺在 RUNTIME_ROOT 上说明没走 release 流程，回滚位与原子切换都不存在。
if [ -L "$RUNTIME_ROOT/app" ]; then
  ok "app -> $(readlink -f "$RUNTIME_ROOT/app")"
elif [ -d "$RUNTIME_ROOT/crates" ]; then
  bad "源码平铺在 $RUNTIME_ROOT（没有 app 链接与 releases/）—— 手工解包的布局：没有回滚位，也没有原子切换"
fi

echo
if [ "$BAD" -eq 0 ]; then
  echo "验收通过。"
  exit 0
fi
echo "验收未通过（上面带 ❌ 的几行）。补救：在部署包目录跑一次 \`bash deploy.sh\` —— 它会探测缺口、补齐前置、导入种子，全部幂等。" >&2
[ "$ADVISORY" = 1 ] && { echo "（ADVISORY=1：只报不拦）"; exit 0; }
exit 1
