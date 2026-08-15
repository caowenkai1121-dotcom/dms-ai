# 本机 Smart App Control 处于强制态，按内容哈希拦所有新链接的未签名产物 →
# 后端一律在容器内构建/运行（裁决 二·E）。SAC 叙事全文见 docker/parser/Dockerfile 头注，此处不重复。
#
# 构建：docker build -f docker/server/Dockerfile -t dms-ai-server .
# 运行：settings.json **运行时挂载，不进镜像层**（见 scripts/serve.ps1）。
# 注意文件名有两个：宿主侧叫 settings.docker.json，挂进容器改名 /app/settings.json（serve.ps1:52）。
#
# 为什么不 COPY settings：镜像层是可分发、可 `docker save`、可推仓库的产物，
# 明文 DSN 与 LLM key 一旦进层就再也删不掉（F8「镜像凭据」）。红线：密钥只在 settings.json。
FROM rust:1-bookworm AS builder
# 容器走镜像自带的 Linux stable 工具链；rust-toolchain.toml（stable-x86_64-pc-windows-gnu）只管
# 宿主机 —— 别 COPY 进来，rustup shim 会在容器里尝试装 windows-gnu 通道，直接炸。
WORKDIR /ws
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# legacy builder 约束：服务器 Docker 29 没装 buildx 插件，`--mount=type=cache` 直接报
# "requires BuildKit"（2026-08-11 事故：加 cache mount 后服务器构建即炸）——RUN 保持单行无 mount。
# 另：服务器构建脚本用行首锚定的 sed 给下面 RUN 行前插 rsproxy 镜像配置（纯净 Dockerfile 不内嵌
# 镜像源）——本 RUN 行首保持原样、别拆行，注释里也绝不能再写一遍同款行首字面量（sed 自匹配事故）。
# 🔴 debug 构建的代价（2026-08-15 生产实测踩到）：`debug_assert!` 在生产**是活的** ——
# 一条断言失败 = 整个请求 panic。C08 那次是「中文数字归一必须等字节长」被一句模型产物打破，
# CLI 直接非 0 退出、回归判执行错误。纪律：**模型产物驱动的不变量一律不许用 debug_assert**，
# 要判就判成可降级的分支（`answer_contract::normalize_chinese_numerals` 已按此改）。
# 内部结构不变量（调用方自觉那类）仍可用 debug_assert。
# debug 是有意取舍：本机/CI 迭代优先构建快；要切 --release 必须同步改下面 COPY 的源路径
# （target/release/），两处没有对账机制，改一处忘另一处就是构建失败。
RUN cargo build --locked && cp /ws/target/debug/dms-ai-server /usr/local/bin/dms-ai-server

FROM debian:bookworm-slim
LABEL org.opencontainers.image.source="https://github.com/caowenkai1121-dotcom/dms-ai"
# 服务器在国内，deb.debian.org 拉源列表常年卡死（2026-08-11 实测 8.8MB Packages 6 分钟拉不动）→
# 构建时先切阿里云镜像；bookworm-slim 的源在 deb822 格式的 sources.list.d/debian.sources
RUN sed -i 's|deb.debian.org|mirrors.aliyun.com|g; s|security.debian.org|mirrors.aliyun.com|g' /etc/apt/sources.list.d/debian.sources \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
# Office 原件级预览（Yuxi 同款方案）：server 侧 `soffice --headless --convert-to pdf` 把
# doc/docx/ppt/pptx/xls/xlsx/xlsm 转 PDF 后 inline 返回（缓存见 kb_api `office_pdf_path`）。
# CJK 字体必须装：不装转换出来的 PDF 中文全是方块。--no-install-recommends 控镜像体积。
RUN apt-get update && apt-get install -y --no-install-recommends \
    libreoffice-core libreoffice-writer libreoffice-calc libreoffice-impress \
    fonts-wqy-zenhei fonts-wqy-microhei && rm -rf /var/lib/apt/lists/*
# /app 是 serve.ps1 挂载契约的一部分（why-not-compose 读相对路径 tools/eval_cases.json → /app/tools/...），勿改
WORKDIR /app
# 源路径跟着上面 build 的 profile 走：改 profile 两处必须同步
COPY --from=builder /usr/local/bin/dms-ai-server /app/dms-ai-server
# 知识库落盘目录：现行方案是 bind mount 宿主目录（serve.ps1:53 `D:\kbdata` → `/kbdata`，kb_root=/kbdata），
# 不是 docker volume；不挂则容器重建即丢文件（DB 里的 doc 行会指向不存在的文件）。
# 这里建的 /app/data/kb 只是对齐 settings.example.json 默认相对落点 data/kb 的空壳，生产不从这走。
RUN mkdir -p /app/data/kb
# EXPOSE 仅文档：实际端口以 settings 的 listen 为准（settings.docker.json:6），改 listen 这里不会跟着变
EXPOSE 8100
# 停机语义：docker stop 发 SIGTERM，默认 10s 后强杀（in-flight 写库由事务兜底）。显式写出即文档化。
STOPSIGNAL SIGTERM
# slim 没有 curl，用 bash 的 /dev/tcp 探活（端口同 EXPOSE 口径；listen 改端口这里会失效，
# 届时以 serve.ps1:95-102 的外部轮询兜底）
HEALTHCHECK CMD bash -c '</dev/tcp/127.0.0.1/8100'
CMD ["/app/dms-ai-server"]
