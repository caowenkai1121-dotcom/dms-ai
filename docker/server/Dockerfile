# 本机 Smart App Control 处于强制态，按内容哈希拦所有新链接的未签名产物 →
# 后端一律在容器内构建/运行（裁决 二·E）。
#
# 构建：docker build -f docker/server/Dockerfile -t dms-ai-server .
# 运行：settings.json **运行时挂载，不进镜像层**（见 scripts/serve.ps1）。
#
# 为什么不 COPY settings：镜像层是可分发、可 `docker save`、可推仓库的产物，
# 明文 DSN 与 LLM key 一旦进层就再也删不掉（F8「镜像凭据」）。红线：密钥只在 settings.json。
FROM rust:1-bookworm AS builder
WORKDIR /ws
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /ws/target/debug/dms-ai-server /app/dms-ai-server
# 知识库落盘目录：挂 volume，否则容器重建即丢文件（DB 里的 doc 行会指向不存在的文件）
RUN mkdir -p /app/kb_data
EXPOSE 8100
CMD ["/app/dms-ai-server"]
