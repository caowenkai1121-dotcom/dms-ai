#!/usr/bin/env bash
# 一键迭代部署：本机打包 → 上传 → 服务器构建/重启/自检。
#
# 用法：DEPLOY_PW='服务器密码' [DEPLOY_HOST=119.39.97.141] [DEPLOY_PORT=22] [DEPLOY_USER=root] bash tools/deploy_update.sh
#
# 前提：服务器已按 docs/DEPLOY.md 首装（/opt/dms-ai/{settings.docker.json,kbdata,tools,.secret_key}
# 在位、docker 可用）。本脚本只做「换版本」：源码走 git archive（服务器不连 GitHub 也行）→
# docker 构建 → 重启容器 → 更新 web 产物 → 健康检查。
set -euo pipefail
cd "$(dirname "$0")/.."

HOST="${DEPLOY_HOST:?DEPLOY_HOST 未设置}"
export DEPLOY_PW="${DEPLOY_PW:?DEPLOY_PW 未设置}" DEPLOY_HOST="$HOST"
export DEPLOY_USER="${DEPLOY_USER:-root}" DEPLOY_PORT="${DEPLOY_PORT:-22}"
REMOTE=/opt/dms-ai
APP=$REMOTE/app

echo "== 1/5 本机打包（git archive HEAD + web/dist）"
mkdir -p target/tmp
git archive -o target/tmp/src.tar.gz HEAD
if [ ! -f tools/web-dist.tar.gz ] || [ web/dist/index.html -nt tools/web-dist.tar.gz ]; then
  (cd web && tar -czf ../tools/web-dist.tar.gz -C dist .)
fi
ls -la target/tmp/src.tar.gz tools/web-dist.tar.gz

echo "== 2/5 上传（bput：base64 走 exec 通道，SFTP 坏也能传）"
python tools/_deploy.py bput target/tmp/src.tar.gz $REMOTE/src.tar.gz
python tools/_deploy.py bput tools/web-dist.tar.gz $REMOTE/web-dist.tar.gz

echo "== 3/5 服务器解包 + Docker 构建（数分钟，耐心）"
python tools/_deploy.py run "mkdir -p $APP && tar -xzf $REMOTE/src.tar.gz -C $APP && echo extracted" 120
# 若服务器拉 crates.io 慢：先在服务器放 /root/.cargo/config.toml（rsproxy 镜像）再构建，
# 纯净 Dockerfile 不内嵌镜像源（纪律见 docker/server/Dockerfile 头注）。
python tools/_deploy.py run "cd $APP && bash scripts/server-build.sh 2>&1 | tail -3" 3600

echo "== 4/5 重启 dms-ai-server（健康探针内置）+ 更新 web 产物"
python tools/_deploy.py run "bash $APP/scripts/server-restart.sh" 300
# web 容器形态以现网为准：若 dms-ai-web 存在且挂了 html 卷，直接刷进卷目录
python tools/_deploy.py run "set -e; docker cp $REMOTE/web-dist.tar.gz dms-ai-web:/tmp/wd.tgz 2>/dev/null && \
  docker exec dms-ai-web sh -c 'tar -xzf /tmp/wd.tgz -C /usr/share/nginx/html && rm /tmp/wd.tgz' && \
  echo web-refreshed || echo 'SKIP: dms-ai-web 不在或卷只读——按现网 web 发布流程更新 $REMOTE/web-dist.tar.gz'" 180

echo "== 5/5 健康检查"
python tools/_deploy.py run "curl -s -m 5 http://127.0.0.1:8100/api/health | head -c 300" 30
echo
echo "部署完成。人工冒烟三题：本月销售额 / 现在总库存量是多少 / 市场费用的报销政策是什么"
