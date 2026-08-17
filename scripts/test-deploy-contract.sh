#!/usr/bin/env bash
# 不连服务器的部署契约钉：路径分层、同源校验与健康判定不得退回旧实现。
set -euo pipefail
cd "$(dirname "$0")/.."

# 🔴 `-U`：msys 的 grep 文本模式会先吃掉 CR，不加 -U 这条闸在 Windows 上恒绿（实测
# `grep -c $'\r'` 对逐行 CRLF 的脚本返 0/rc=1 放行，`grep -Uc` 才返 6/rc=0）。
# 名单也换成正判据：凡是 .sh 全查，不点名 —— 点名版漏过 server-cleanup.sh 与 server-bootstrap.sh。
for script in tools/*.sh scripts/*.sh; do
  [ -f "$script" ] || continue
  bash -n "$script"
  ! LC_ALL=C grep -Uq $'\r' "$script" || {
    echo "CRLF shell script: $script" >&2
    exit 1
  }
done

restart="$(cat scripts/server-restart.sh)"
deploy="$(cat tools/deploy_update.sh)"
web_update="$(cat scripts/web-update.sh)"

# 🔴 精排两条也在合同里：它默认接本机 embed 服务（同进程同端口的千问适配层）。
# 掉回「未设即关」不会报错 —— 检索只是排不到第一（生产实测 recall@6=0.95 / recall@1=0.15），
# 而那种回退在任何健康检查上都是绿的。
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
  'data.get("ok") is True' \
  'DMS_RERANK_BASE_URL+set' \
  'DMS_RERANK_MODEL:-gte-rerank-v2'; do
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

# 🔴 CRLF 判据自身的判据：`-U` 被谁改回去，这里当场红。少了它整条闸在 Windows 上恒绿，
# 而恒绿的闸比没有闸更坏 —— 它让人以为查过了。
for required in "grep -Uq \$'\\r'" 'for script in tools/*.sh scripts/*.sh'; do
  grep -Fq "$required" <<<"$deploy" || {
    echo "deploy_update 的 CRLF 判据退回无牙版（缺 -U 或退回点名清单）：$required" >&2
    exit 1
  }
done

# 容器部署下 listen 必须绑 0.0.0.0：绑 127.0.0.1 时容器内 loopback healthy、
# 外部端口映射打不通，症状是 90×2s 空转后超时回滚，报错一个字都不指向 listen。
grep -Fq '"0.0.0.0:", "[::]:"' <<<"$restart" || {
  echo "server-restart 缺 listen 绑定校验：照抄 settings.example.json 的新机器会静默不可达" >&2
  exit 1
}

# 部署包模式（DEPLOY_SRC_TAR/DEPLOY_WEB_TAR）必须成对，只给一个会把旧前端留在现网。
grep -Fq 'DEPLOY_SRC_TAR 与 DEPLOY_WEB_TAR 必须同时提供' <<<"$deploy" || {
  echo "deploy_update 的部署包模式丢了成对校验，可能只发布半套" >&2
  exit 1
}

# 全新机器 bootstrap：这几条是现网靠人手搭出来、仓库里长期没有记录的部分，掉一条就得重新摸索。
bootstrap="$(cat scripts/server-bootstrap.sh)"
for required in \
  'app/tools/embed_service.py' \
  'DMSAI_SETTINGS=' \
  '--add-host host.docker.internal:host-gateway' \
  'DMS_AI_PG_BIND' \
  "unnest(ARRAY['age','vector','pg_trgm'])" \
  'nginx:1.27-alpine' \
  'requirements-embed'; do
  grep -Fq -- "$required" <<<"$bootstrap" || {
    echo "server-bootstrap 缺全新机器前置：$required" >&2
    exit 1
  }
done
# 单元里的 ExecStart 必须走 app/（跟着 release 走）。指回 RUNTIME_ROOT 下的独立拷贝，
# 部署换了代码 embed 服务不会跟着变，而症状只是「检索变差」，不报任何错。
# 正判据：直接钉 ExecStart 那一行本身，而不是「不许出现某种写法」——
# 第一版写的是负判据 `[^/]tools/...`，而真实的坏写法是 `$RUNTIME_ROOT/tools/...`，
# 前面恰好是斜杠，于是那条判据反向验证时纹丝不动（2026-08-17 实测 rc=0）。
grep -Eq 'ExecStart=.*app/tools/embed_service\.py' <<<"$bootstrap" || {
  echo "server-bootstrap 的 systemd 单元没指向 app/tools，改 embed_service.py 将不生效" >&2
  exit 1
}

# embed 服务与 nginx.conf 都是宿主机上跟 release 无关的独立拷贝，部署不同步 = 改了等于没改，
# 且两条都不报错（哑掉的降级）。这两条判据钉住「部署会去同步它们」这件事本身。
grep -Fq 'scripts/embed-sync.sh' <<<"$deploy" || {
  echo "deploy_update 不再同步 embed 服务：改 embed_service.py 将静默不生效" >&2
  exit 1
}
grep -Fq "docker/web/nginx.conf" <<<"$deploy" || {
  echo "deploy_update 不再同步 nginx.conf：改 Web 网关配置将静默不生效" >&2
  exit 1
}
embed_sync="$(cat scripts/embed-sync.sh)"
for required in 'systemctl show -p ExecStart' 'systemctl show -p WorkingDirectory' 'sha256sum' 'restore' 'd.get("ok") is not True'; do
  grep -Fq -- "$required" <<<"$embed_sync" || {
    echo "embed-sync 缺同步/自证/回滚合同：$required" >&2
    exit 1
  }
done

echo "deploy contract ok"
