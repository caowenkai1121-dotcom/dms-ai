#!/usr/bin/env bash
# 服务器侧镜像构建（git archive 源码包没有 git 目录，cargo 镜像源要在服务器侧注入）。
# sed 行首锚定 `^RUN cargo build`（2026-08-11 事故：未锚定时连 Dockerfile 注释里的同款字面量也替换）。
# 用法：bash scripts/server-build.sh   （在源码树根目录执行；需要 .cargo/config.toml 带 rsproxy 镜像）
set -euo pipefail
cd "$(dirname "$0")/.."

mkdir -p .cargo
if [ ! -f .cargo/config.toml ]; then
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
fi
grep -q '^COPY .cargo/config.toml' docker/server/Dockerfile || \
  sed -i 's|^RUN cargo build --locked|COPY .cargo/config.toml /usr/local/cargo/config.toml\nRUN cargo build --locked|' docker/server/Dockerfile
grep -n 'cargo' docker/server/Dockerfile

docker build -f docker/server/Dockerfile -t dms-ai-server .
echo "BUILD_OK $(date '+%F %T')"
