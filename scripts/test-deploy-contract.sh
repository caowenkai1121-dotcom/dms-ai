#!/usr/bin/env bash
# 不连服务器的部署契约钉：路径分层、同源校验与健康判定不得退回旧实现。
set -euo pipefail
cd "$(dirname "$0")/.."

for script in tools/deploy_update.sh scripts/server-build.sh scripts/server-restart.sh scripts/web-update.sh; do
  bash -n "$script"
  ! LC_ALL=C grep -q $'\r' "$script" || {
    echo "CRLF shell script: $script" >&2
    exit 1
  }
done

restart="$(cat scripts/server-restart.sh)"
deploy="$(cat tools/deploy_update.sh)"
web_update="$(cat scripts/web-update.sh)"

for required in \
  'APP_ROOT=' \
  'RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"' \
  'SETTINGS_FILE="$RUNTIME_ROOT/settings.docker.json"' \
  'SECRET_FILE="$RUNTIME_ROOT/.secret_key"' \
  '运行时密钥必须至少 32 字节' \
  'KBDATA_DIR="$RUNTIME_ROOT/kbdata"' \
  'TOOLS_DIR="$APP_ROOT/tools"' \
  'cfg.get("kb_root") != "/kbdata"' \
  'type=bind,source=$KBDATA_REAL,target=/kbdata' \
  'ROLLBACK_CONTAINER="dms-ai-server-rollback"' \
  'rollback_server()' \
  'trap rollback_on_exit EXIT' \
  "trap 'exit 143' TERM" \
  'PROBE_TOKEN=' \
  'SERVICE_CURL=(curl -fsS -m 10)' \
  'SERVICE_CURL+=(--resolve "$SERVICE_RESOLVE")' \
  '"$SERVICE_URL/parse"' \
  'PARSER_HEALTH_URL="$SERVICE_URL/health"' \
  '解析服务响应未包含知识库探针 token' \
  'caps.get("text") is not True' \
  'curl -fsS' \
  'data.get("ok") is True'; do
  grep -Fq "$required" <<<"$restart" || {
    echo "server-restart 缺部署合同：$required" >&2
    exit 1
  }
done

deploy_py="$(cat tools/_deploy.py)"
for required in 'RejectPolicy()' 'sha256sum' 'sys.exit(rc)'; do
  grep -Fq "$required" <<<"$deploy_py" || {
    echo "_deploy.py 缺安全上传合同：$required" >&2
    exit 1
  }
done

grep -Fq 'DMS_RUNTIME_ROOT='"'"'$RUNTIME_ROOT'"'"'' <<<"$deploy" || {
  echo "deploy_update 未显式传 DMS_RUNTIME_ROOT" >&2
  exit 1
}
for required in \
  'while IFS= read -r -d' \
  'RELEASE_ROOT="$RELEASES_ROOT/$RELEASE_ID"' \
  "tar -xzf '\$RUNTIME_ROOT/src.tar.gz' -C '\$RELEASE_ROOT'" \
  "mv -Tf '\$RUNTIME_ROOT/app.next' '\$APP_ROOT'" \
  'server-restart.sh' \
  ' 900'; do
  grep -Fq "$required" <<<"$deploy" || {
    echo "deploy_update 缺 release/超时合同：$required" >&2
    exit 1
  }
done
grep -Fq 'HEALTH_URL="http://172.17.0.1:8100/api/health"' <<<"$deploy" || {
  echo "deploy_update 健康地址与 server publish 地址漂移" >&2
  exit 1
}
for required in 'npm ci' 'npm test' 'npm run build'; do
  grep -Fq "$required" <<<"$deploy" || {
    echo "deploy_update 未执行前端门禁：$required" >&2
    exit 1
  }
done
grep -Fq "scripts/web-update.sh" <<<"$deploy" || {
  echo "deploy_update 未调用 Web 原子更新脚本" >&2
  exit 1
}
for required in \
  'bind\|*' \
  'mv "$ROOT" "$BACKUP"' \
  'rollback_web()' \
  'docker restart "$CONTAINER"' \
  'MOUNTED_HASH=' \
  'SKIP: $CONTAINER 容器不存在'; do
  grep -Fq "$required" <<<"$web_update" || {
    echo "web-update 缺只读 bind/回滚合同：$required" >&2
    exit 1
  }
done
! grep -Fq "web-refreshed || echo 'SKIP" <<<"$deploy" || {
  echo "deploy_update 会吞掉已存在 Web 容器的发布失败" >&2
  exit 1
}
! grep -Fq 'index.html -nt tools/web-dist.tar.gz' <<<"$deploy" || {
  echo "deploy_update 又退回 index.html mtime 判定，可能发布旧前端" >&2
  exit 1
}
! grep -Fq 'git archive' <<<"$deploy" || {
  echo "deploy_update 只打 HEAD，会静默漏掉当前工作区已验收修复" >&2
  exit 1
}
grep -Fq 'git ls-files -co --exclude-standard -z' <<<"$deploy" || {
  echo "deploy_update 未打包当前受控工作区" >&2
  exit 1
}
! grep -Fq 'curl -s -m 2' <<<"$restart" || {
  echo "健康检查退回了仅判断响应非空的旧实现" >&2
  exit 1
}
! grep -Fq 'PARSER_HEALTH_URL="$(docker inspect' <<<"$restart" || {
  echo "server-restart 又退回检查固定 parser 容器而非 settings service_url" >&2
  exit 1
}

echo "deploy contract ok"
