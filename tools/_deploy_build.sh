#!/bin/bash
# 每次 git archive 源码包都是纯净 Dockerfile —— cargo 镜像配置要在服务器侧重打
set -e
pkill -f 'docker build' 2>/dev/null || true
sleep 1
cd /opt/dms-ai
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = 'rsproxy-sparse'
[source.rsproxy-sparse]
registry = 'sparse+https://rsproxy.cn/index/'
[registries.rsproxy]
index = 'sparse+https://rsproxy.cn/index/'
[net]
git-fetch-with-cli = true
EOF
grep -q 'cargo/config.toml' docker/server/Dockerfile || \
  sed -i 's|RUN cargo build --locked|COPY .cargo/config.toml /usr/local/cargo/config.toml\nRUN cargo build --locked|' docker/server/Dockerfile
grep -n 'cargo' docker/server/Dockerfile
setsid nohup docker build -f docker/server/Dockerfile -t dms-ai-server . > server-build.log 2>&1 < /dev/null &
echo BUILD_RESTARTED
