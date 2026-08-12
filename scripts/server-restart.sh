#!/usr/bin/env bash
# dms-ai-server 容器（重）启动。配置运行时挂载不进镜像层；kbdata 双容器同路径。
# 端口绑 172.17.0.1（docker 网桥）：nginx（dms-ai-web）经网桥反代；不对宿主机 127.0.0.1 暴露。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"

docker rm -f dms-ai-server 2>/dev/null || true
DMS_SECRET_KEY=$(cat "$ROOT/.secret_key")
docker run -d --name dms-ai-server --restart unless-stopped \
  --add-host host.docker.internal:host-gateway \
  -e DMS_SECRET_KEY="$DMS_SECRET_KEY" \
  -v "$ROOT/settings.docker.json:/app/settings.json" \
  -v "$ROOT/kbdata:/kbdata" \
  -v "$ROOT/tools:/app/tools" \
  -p 172.17.0.1:8100:8100 \
  dms-ai-server
echo "container started"
for i in $(seq 1 90); do
  r=$(curl -s -m 2 http://172.17.0.1:8100/api/health 2>/dev/null)
  if [ -n "$r" ]; then echo "HEALTH: $r"; exit 0; fi
  sleep 2
done
echo "HEALTH TIMEOUT"; docker logs --tail 15 dms-ai-server
exit 1
