# 向量 / 精排 / 文档解析服务的**生产**镜像 —— 替掉宿主机上的 systemd 单元 `dms-ai-embed`。
#
# 为什么要有它（2026-08-17 现场账）：这套服务此前只以「宿主机 venv + systemd 单元」的形态存在，
# 而那套形态**没有任何一行在仓库里**。代价一天之内撞到两次：
#   ① 第二台生产机上根本没装单元，8078 上是个手工起的裸 python 进程 ——
#      重启机器即失、部署换代码它也不跟着变，而 `/api/health` 全绿；
#   ② 换千问那次，`/opt/dms-ai/tools/embed_service.py` 与 release 里那份是两份拷贝，
#      靠人手同步（`scripts/embed-sync.sh` 就是为它写的补丁）。
# 装进镜像之后：依赖、代码、启动方式一起进版本库，`docker run --restart unless-stopped`
# 天然开机自启，部署换代码＝重建镜像换容器，两份拷贝的问题从根上消失。
#
# 与 `docker/parser/` 的区别（那个别删也别混用）：parser 是**开发机专用的运输壳** ——
# Windows Smart App Control 拦 lxml 的编译扩展，宿主机上 word/ppt 恒不可用，
# 所以它把解析放进容器、`/embed` 反过来转发给宿主机上游。本镜像是**完整服务**：
# embed（千问 text-embedding-v4）+ rerank（gte-rerank-v2）+ parse 全在里面，不转发任何东西。
#
# 构建（context = 仓库根，要用到 tools/）：
#   docker build -f docker/embed/Dockerfile -t dms-ai-embed .
# 运行：见 scripts/embed-install.sh（幂等安装 + 接管旧单元 + 起飞自检），别手敲 docker run。
#
# 🔴 凭据红线：镜像里**一个凭据都没有**。settings 运行时挂载（`/app/settings.json`），
# `DMS_SECRET_KEY` 运行时注入。镜像层可 `docker save`、可推仓库，明文 DSN 一旦进层就删不掉。
FROM python:3.12-slim

ENV PIP_ROOT_USER_ACTION=ignore \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PYTHONIOENCODING=utf-8

# apt 与 pip 分两层：apt 这层几百 MB 且极少变，pip 那层跟着 requirements 走。
# --no-install-recommends 是关键：`libreoffice-writer` 带 recommends 实测 587 个包、
# 不带 160 个，那 427 个里含 default-jre 与整套 -l10n，文本抽取一个字节都用不上。
#
# LibreOffice 三件套：.doc/.xls/.ppt 三种旧二进制格式各要一个（headless --convert-to）。
# 不装 fonts-noto-cjk：本服务只做 XML→XML 转换，中文以码点存在、不经字形栅格化，
# 缺字体不出豆腐块；只有导出 PDF/图片才需要字体，那条路本服务不走。
#
# tesseract + chi_sim：`_p_image` 调的就是 pytesseract（扫描件 OCR 档）。
RUN apt-get update && apt-get install -y --no-install-recommends \
        libreoffice-writer libreoffice-calc libreoffice-impress \
        tesseract-ocr tesseract-ocr-chi-sim \
    && rm -rf /var/lib/apt/lists/*

# 依赖清单是**唯一事实源**：与宿主机 venv 用同一份 tools/requirements-embed.txt，
# 不在这里重抄一遍版本号 —— 抄第二份就会有「容器装的和 venv 装的不是一套」那天。
COPY tools/requirements-embed.txt /tmp/requirements-embed.txt
RUN pip install --no-cache-dir -r /tmp/requirements-embed.txt && rm -f /tmp/requirements-embed.txt

# 只 COPY 这两个文件：`embed_service.py` 从 `sys.path[0]`（同目录）import `settings`，
# 少一个就 ImportError。整个 tools/ 不 COPY —— 那里面有回归脚本、夹具与判官，跟本服务无关。
COPY tools/embed_service.py tools/settings.py /app/tools/
WORKDIR /app

# settings 运行时挂到这里（与 dms-ai-server 容器同一个约定：宿主叫 settings.docker.json，
# 挂进容器一律改名 /app/settings.json）。`tools/settings.py` 认这个环境变量。
ENV DMSAI_SETTINGS=/app/settings.json

# EXPOSE 仅文档。8078 与宿主发布口同号，改端口要同时改 embed-install.sh 与 settings 的 service_url。
EXPOSE 8078
# 停机语义：SIGTERM 后 10s 强杀。解析是无状态请求，in-flight 丢了由 Rust 侧重试。
STOPSIGNAL SIGTERM
# 探活走容器内回环。注意它只证明「HTTP 起来了」，证不了「千问 key 能用」——
# 后者由 serve() 启动时的 `_qwen_key()` 硬失败保证（key 没配就根本起不来）。
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD python -c "import urllib.request;urllib.request.urlopen('http://127.0.0.1:8078/health',timeout=3)"
# 🔴 容器内绑 0.0.0.0 是对的，与 `serve()` 注释里「0.0.0.0 会把解析面暴露给公网」不矛盾：
# 那句说的是**宿主机裸跑**。容器里回环谁都连不上（-p 转发到容器网卡），而对外暴露面由
# 发布地址决定 —— embed-install.sh 发布的是 `172.17.0.1:8078`（Docker 网桥网关），不是 0.0.0.0。
CMD ["python", "-u", "/app/tools/embed_service.py", "serve", "8078", "0.0.0.0"]
