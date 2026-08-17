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

# 🔴 判据必须钉在**代码**上。注释里把「为什么」写清楚是本仓的纪律，但不能让注释把判据喂饱 ——
# 2026-08-17 一天之内在反向验证里抓到三次同一形状：`grep -Fq '--restart unless-stopped'`
# 命中的是头注那句说明，把 `docker run` 里真正的那个参数拆掉，判据纹丝不动。
# 所以下面所有源码变量一律先剥掉整行注释（`#` 开头），一处根治，不再逐条改字符串。
code_only() { grep -vE '^[[:space:]]*#'; }

restart="$(code_only < scripts/server-restart.sh)"
deploy="$(code_only < tools/deploy_update.sh)"
web_update="$(code_only < scripts/web-update.sh)"

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

deploy_py="$(code_only < tools/_deploy.py)"
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
bootstrap="$(code_only < scripts/server-bootstrap.sh)"
for required in \
  'scripts/embed-install.sh' \
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
# 🔴 向量/解析服务 2026-08-17 起是**容器**：代码进镜像，部署换代码＝重建镜像换容器。
# 退回 systemd 单元那套的代价已经付过两次（8078 上的孤儿裸进程 / RUNTIME_ROOT 下第二份
# 代码拷贝），所以 bootstrap 里不许再出现写单元文件的写法。
! grep -Fq 'ExecStart=' <<<"$bootstrap" || {
  echo "server-bootstrap 又在写 systemd 单元：应改用容器（scripts/embed-install.sh）" >&2
  exit 1
}
# 🔴 钉「可执行构造」，不是「字符串出现过」。剥掉注释还不够 —— echo/die 的**文案**里
# 同样会写这些字面量（`step "…--restart unless-stopped…"`、die 里的用法提示），
# 那和注释一样能把判据喂饱。所以带上行继续符或判断骨架。
install="$(code_only < scripts/embed-install.sh)"
for required in \
  'docker build -f docker/embed/Dockerfile' \
  '--restart unless-stopped \' \
  'target=/kbdata' \
  'target=/app/settings.json,readonly' \
  '[ "$TAKEOVER" = 1 ] ||'   'TAKEOVER="${DMS_EMBED_TAKEOVER:-0}"'; do
  grep -Fq -- "$required" <<<"$install" || {
    echo "embed-install 缺容器安装合同：$required" >&2
    exit 1
  }
done
# 镜像里一个凭据都没有：settings 与密钥都必须运行时注入。COPY 进层就再也删不掉。
dockerfile="$(code_only < docker/embed/Dockerfile)"
! grep -Eq '^COPY .*settings[.]docker[.]json|^COPY .*[.]secret_key' <<<"$dockerfile" || {
  echo "embed 镜像 COPY 了凭据：镜像层可 docker save/可推仓库，明文一旦进层就删不掉" >&2
  exit 1
}
grep -Fq 'COPY tools/requirements-embed.txt' <<<"$dockerfile" || {
  echo "embed 镜像不再用 tools/requirements-embed.txt：依赖清单会分裂成两份" >&2
  exit 1
}
grep -Fq 'COPY tools/embed_service.py tools/settings.py' <<<"$dockerfile" || {
  echo "embed 镜像少 COPY 了 settings.py：embed_service 从同目录 import 它，会 ImportError" >&2
  exit 1
}
# 解析服务容器名不许写死一个：新形态叫 dms-ai-embed，旧运输壳叫 dms-ai-parser，都要认；
# 认不出就整段跳过 /kbdata 同源校验，而那条校验防的是「稳定 404」。
grep -Fq 'for cand in ${DMS_PARSER_CONTAINER:-} dms-ai-embed dms-ai-parser' <<<"$restart" || {
  echo "server-restart 又把解析服务容器名写死了：容器形态的 /kbdata 同源校验会整段跳过" >&2
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
embed_sync="$(code_only < scripts/embed-sync.sh)"
for required in 'systemctl show -p ExecStart' 'systemctl show -p WorkingDirectory' 'sha256sum' 'restore' 'd.get("ok") is not True'; do
  grep -Fq -- "$required" <<<"$embed_sync" || {
    echo "embed-sync 缺同步/自证/回滚合同：$required" >&2
    exit 1
  }
done

# 部署包的上线判据：2026-08-17 现场那台「部署成功、/api/health 全绿、答案变差」——
# 业务字典种子没导（sql_exemplar 少 90 行、memory 少 48 行），而 health 的 vector_ready
# 只覆盖 datasource/element/table_doc 三张表，样例表根本不在里面。三条一起钉。
bundle="$(code_only < tools/bundle-deploy.sh)"
# 判据本体 2026-08-17 起收口进 scripts/server-verify.sh（下面另有一组钉它），
# 入口这边只需保证「探测缺口 + 导种子 + 调那份共享判据」三件都还在。
for required in   '探测目标形态'   'registry_snapshot.py import'   'server-verify.sh'; do
  grep -Fq -- "$required" <<<"$bundle" || {
    echo "部署包入口缺上线判据：$required" >&2
    exit 1
  }
done
# 快照导入不许再退回「只在 --bootstrap 下跑」：忘了加开关的代价是静默变笨。
! grep -Fq 'if [ "$MODE" = bootstrap ] && [ -s payload/registry_snapshot.json ]' <<<"$bundle" || {
  echo "快照导入又被关回 --bootstrap 专属：忘加开关就静默少 90 条样例" >&2
  exit 1
}

# 上线判据必须是**一份**，且挂在「谁部署都躲不开」的位置上：server-restart.sh 收尾。
# 手工解包的人不会跑 deploy.sh，但一定会跑 server-restart.sh（2026-08-17 现场教训）。
verify="$(code_only < scripts/server-verify.sh)"
# 🔴 断言必须钉在**代码**上，不能钉在「文里提到过」上：注释里写清楚为什么，
# 结果判据被注释喂饱、拆掉真正的实现照样绿 —— 2026-08-17 反向验证当场抓到三条这样的。
for required in   'meta.sql_exemplar'   'count(*) FILTER (WHERE embedding IS NOT NULL)'   'STATE="$(systemctl is-active dms-ai-embed'   '[ "$ADVISORY" = 1 ]'   '"$RUNTIME_ROOT/seed/registry_snapshot.json"'; do
  grep -Fq -- "$required" <<<"$verify" || {
    echo "server-verify 缺验收项：$required" >&2
    exit 1
  }
done
grep -Fq 'ADVISORY=1 DMS_RUNTIME_ROOT="$RUNTIME_ROOT" bash "$APP_ROOT/scripts/server-verify.sh"' <<<"$restart" || {
  echo "server-restart 收尾不再跑上线验收（或丢了 ADVISORY 只报不拦）：手工部署的机器将无人告知它缺东西" >&2
  exit 1
}
# 包的入口现在是**服务器侧**的就地安装器（运维传 tar 上去解开再跑），
# 它同样必须以共享验收收口 —— 不然又回到「装完不知道成没成」。
inplace="$(code_only < tools/bundle-install-inplace.sh)"
for required in   'scripts/server-verify.sh'   'scripts/embed-install.sh'   'registry_snapshot.py import'   'rollback-before-'; do
  grep -Fq -- "$required" <<<"$inplace" || {
    echo "就地安装器缺步骤：$required" >&2
    exit 1
  }
done
# 运行时状态一律不许被源码同步覆盖：覆盖 settings 要重配，覆盖 kbdata 是永久数据损坏。
for keep in 'settings.docker.json' '.secret_key' 'kbdata' 'venv'; do
  grep -Fq -- "$keep" <<<"$inplace" || {
    echo "就地安装器没写明保留 $keep：同步源码时会连运行时状态一起盖掉" >&2
    exit 1
  }
done
# 基准不许写死数字：写死的下个月就是假的，必须从快照现读。
grep -Fq 'json.load(open(sys.argv[1]' <<<"$verify" || {
  echo "server-verify 的基准不再取自快照本身" >&2
  exit 1
}

# 🔴 `docker inspect NAME` 不加 --type 会**连镜像一起匹配** —— 存在同名镜像时
# 「容器存在吗」这个判断会答错，随后 `.State.Running` 取空、脚本走进错误的分支。
# 这条是运维在 1.95.7.181 生产机上自己发现并手工打的补丁（server-restart.sh.bak-20260817），
# 现已吸收进上游全部存在性检查，别再退回去。
for f in scripts/server-restart.sh scripts/embed-install.sh scripts/embed-sync.sh          scripts/server-verify.sh scripts/server-bootstrap.sh scripts/web-update.sh; do
  bad="$(grep -n 'docker inspect' "$f" | grep -v -- '--type container' || true)"
  [ -z "$bad" ] || {
    echo "$f 的 docker inspect 少了 --type container（会把同名镜像误判成容器）：$bad" >&2
    exit 1
  }
done

echo "deploy contract ok"
