# 解析层容器镜像。**为什么要有它**：解析依赖在本机宿主机上根本起不来 ——
# Windows Smart App Control 强制态按内容哈希拦未签名的新链接产物，`lxml` 的编译扩展
# （`etree.pyd`）就在被拦之列。实测宿主机 `GET :8077/health` →
#   parse_ok={"pdf":true,"docx":false,"xlsx":true,"pptx":false,"text":true}
# 即业主点名的 **word / ppt 在宿主机上恒不可用**，与代码对不对无关，是环境。
# 容器里没有 SAC → 同一份 `tools/embed_service.py` 的解析路径全绿（见 scripts/parser.ps1 probe）。
#
# 构建：.\scripts\parser.ps1 build       （build context 只有 docker/parser/，不是仓库根 ——
#                                        仓库根有 target/ 与 settings.json，没必要送进 daemon）
# 运行：.\scripts\parser.ps1 up
#
# 🔴 凭据红线：镜像里**一个凭据都没有**，也不 COPY 任何 settings。本服务只解析文件、不连 PG
# （`embed_service.pg_conf` 是惰性的，解析/分块两条路径永不触碰它）。将来真要连 PG，
# 照 serve.ps1 的做法**运行时挂载** settings.json，不要 COPY —— 镜像层可 docker save、可推仓库，
# 明文 DSN 一旦进层就删不掉（F8）。
#
# 入口环境变量 PARSE_ROOTS（parse_service.py:40，默认 `/kbdata:/tmp`）：/parse 只许读这些根
# 之下的文件，/tmp 是给探针造夹具的放宽项。**别为了探针方便放宽到 /app** —— 那等于把守卫删掉。
#
# 体积实测（`docker run --rm <img> du -sxm /` + `docker history` 层大小，本机 2026-07-29）：
#   python:3.12-slim 基底                      124MB
#   + LibreOffice writer/calc/impress          +501MB  ← .doc/.xls/.ppt 的全部本钱
#     （/usr/lib/libreoffice 本体 280MB，差值 ~220MB 是它拖进来的依赖库）
#   + pymupdf4llm/pypdf/docx/pptx/openpyxl/PIL +118MB  （pymupdf 自己 64MB）
#   + tesseract-ocr + chi_sim + pytesseract    +31MB
#   = **741MB**（业主批的是「+500MB（LibreOffice headless）」，LibreOffice 那一项 501MB 正好落在批面内；
#     另外 149MB 是 PDF/Office/OCR 的解析栈本身，不是 LibreOffice 超支）。
# 要砍体积：去掉 libreoffice-calc(27.8MB) + -impress(2.5MB) 只留 writer(32.7MB)，省 ~30MB
# （apt-cache Installed-Size 实测；共享的 libreoffice-core 那 ~200MB 砍不掉）。
# 代价是 .xls/.ppt 变成**转换时才炸**而不是明确报不支持 —— `_cap_legacy`（embed_service.py:725-729）
# 只探 soffice 二进制存在 + 目标格式解析器，探不到 calc/impress 组件缺失
# （「去掉组件会照实报」这个假设未实测验证，别指望它）。省 30MB 换两种格式，不值，所以三件套都装。
FROM python:3.12-slim

# pip 以 root 跑是容器构建常态，ENV 关掉它的 WARNING 噪音；PYTHONUNBUFFERED 让探针的一次性
# 容器（parser.ps1 跑 make_fixtures/make_silent 那些不带 -u 的调用）print 不缓冲、日志不乱序；
# PYTHONDONTWRITEBYTECODE 挡住容器内每次跑 python 都在 /app 下写 __pycache__。
ENV PIP_ROOT_USER_ACTION=ignore \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

# --no-install-recommends 是关键：`apt-get install -s libreoffice-writer` 实测 **587 个包**，
# 加 --no-install-recommends 后 **160 个**（三件套一起 186 个；三个数字均本机 2026-07-29 实测，
# 同上面体积口径的日期）—— 那 427 个里含 default-jre 与整套 -l10n，文本抽取一个字节都用不上。
# 只装 writer/calc/impress（doc/xls/ppt 各一），不装 libreoffice 全家桶（-base/-draw/-math）。
#
# 不装 fonts-noto-cjk（apt-cache Installed-Size 实测 89MB）：本服务只做
# `--convert-to docx/xlsx/pptx`，是 XML→XML，中文以码点存在、不经过字形栅格化 →
# 缺字体不出豆腐块（probe 里 .doc 解出的中文就是证据）。只有导出 PDF/图片才需要字体，
# 那条路本服务不走。夹具要画中文字（测 OCR）时挂宿主机的 C:\Windows\Fonts，见 make_fixtures.py。
#
# tesseract 也合在这层装：原先两个 RUN 各做一次 `apt-get update`，是多一次 update、多一层，
# 改 tesseract 还连带重跑 pip —— 层按失效域合、不按叙述分（OCR 选型理由见下方注释块）。
RUN apt-get update && apt-get install -y --no-install-recommends \
        libreoffice-writer libreoffice-calc libreoffice-impress \
        tesseract-ocr tesseract-ocr-chi-sim \
    && rm -rf /var/lib/apt/lists/*

# pymupdf4llm：AGPL-3.0，业主已裁决可用（保标题层级）。pypdf 是 BSD 兜底，
# embed_service._p_pdf 的三级降级（pymupdf4llm→fitz→pypdf）会自己选 —— fitz 是 pymupdf4llm
# 带入的中间档（pymupdf 传递依赖），砍包的人注意它会跟着一起没。两个都装着才有降级可言。
# 版本钉死：镜像是可分发产物，`pip install 最新` 会让「上周能解析的 pptx 今天报错」查不出原因。
# （口径：pip 钉死、apt 随 Debian 浮动 —— distro 包没法钉。已知缺口：未做 --require-hashes
#  供应链哈希校验，那要钉全传递依赖的哈希，暂无生成条件。）
RUN pip install --no-cache-dir \
        pymupdf4llm==0.0.17 pypdf==5.1.0 \
        python-docx==1.2.0 python-pptx==1.0.2 openpyxl==3.1.5 pillow==11.0.0 \
        pytesseract==0.3.13

# OCR：tesseract 二进制 + chi_sim 语言包 + pytesseract 包装器（已并入上面 apt/pip 两个合层）。
# **选它是因为 tools/embed_service.py 的 `_p_image` 调的就是 pytesseract**（`_cap_ocr` 的可用
# 判据是「千问 key + PIL.Image」或「PIL.Image/pytesseract/tesseract 三样」，二有一即可），
# 镜像装别的引擎再好也走不到那条路。
#
# 与 rapidocr-onnxruntime 的实测对比 —— 同一张夹具图（1000x220 白底黑字 34px 印刷体，
# make_fixtures.py 造，两边都用 /kbdata/_probe/fixture.png 这一个文件）：
#   体积（docker history 的层大小，同一基底）：
#     tesseract-ocr + tesseract-ocr-chi-sim + pytesseract   **31.4MB**（语言包 /usr/share/tesseract-ocr 17MB）
#     rapidocr-onnxruntime + libgl1/libglib2.0-0            **576MB**（onnxruntime 58 + cv2 72 + numpy 43 + 权重 16 + 依赖）
#     整镜像 du -sxm /：741MB（tesseract）对 1261MB（rapidocr）—— OCR 一项差 18 倍。
#   中文识别（原文『培训报销标准 / 境内培训：实报实销，单次上限 3000 元 / 境外培训：需总经理审批』）：
#     tesseract chi_sim+eng  0.22s  汉字与数字全对，只把全角『：』识成半角『:』（2 处），
#                                   并在 3000 两侧多出空格
#     rapidocr（PP-OCRv4）   1.91s  全对，含全角『：』；把 3000 两侧空格吞掉
#     → **这张图上质量等价**（都是标点/空白级差异，不影响检索命中），tesseract 还快 8.7 倍。
#   ⚠️ 边界诚实：等价结论只覆盖「白底黑字、印刷体、正位」。真实扫描件（歪斜、低 dpi、表格线）
#     上 PP-OCRv4 通常明显更强。要换引擎是改 tools/embed_service.py 的 `_p_image` 那一处
#     （能力探测 `_cap_ocr` 也在那），不是改这个镜像 —— 所以这里跟着上游代码选 tesseract。
# PaddleOCR 全量直接排除：paddlepaddle wheel 单个就 ~400MB 且运行期还要拉权重。

# make_silent_fixtures.py 不 COPY：它由 parser.ps1:165 运行时挂 /mk 进容器（探针脚本改得勤，
# 不为它重构建镜像；两个同性质探针脚本两种分发方式，动一边时记得另一边）。
# build context（docker/parser/）没有独立 .dockerignore：目前仅 4 个文件无害，
# 将来往这目录放临时夹具会静默进 daemon，注意。
COPY parse_service.py make_fixtures.py /app/
WORKDIR /app
# 8077 = 与宿主机 embed 服务同一个端口号（同形，见 parse_service.py）；宿主机侧发布哪个端口由
# parser.ps1 决定，默认 8078 —— 8077 被宿主 embed 服务占着（parser.ps1:27）。
# EXPOSE 仅文档，实际端口以 PARSER_PORT 环境变量为准（parse_service.py:34）。
EXPOSE 8077
# /health 探活（端口跟着 EXPOSE 口径写死；PARSER_PORT 改了它会失效，届时以 parser.ps1 外部轮询兜底）
HEALTHCHECK CMD python -c "import urllib.request;urllib.request.urlopen('http://127.0.0.1:8077/health',timeout=3)"
CMD ["python", "-u", "/app/parse_service.py"]
