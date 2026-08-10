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
# 体积实测（`docker run --rm <img> du -sxm /` + `docker history` 层大小，本机 2026-07-29）：
#   python:3.12-slim 基底                      124MB
#   + LibreOffice writer/calc/impress          +501MB  ← .doc/.xls/.ppt 的全部本钱（/usr/lib/libreoffice 280MB）
#   + pymupdf4llm/pypdf/docx/pptx/openpyxl/PIL +118MB  （pymupdf 自己 64MB）
#   + tesseract-ocr + chi_sim + pytesseract    +31MB
#   = **741MB**（业主批的是「+500MB（LibreOffice headless）」，LibreOffice 那一项 501MB 正好落在批面内；
#     另外 149MB 是 PDF/Office/OCR 的解析栈本身，不是 LibreOffice 超支）。
# 要砍体积：去掉 libreoffice-calc(27.8MB) + -impress(2.5MB) 只留 writer(32.7MB)，省 ~30MB
# （apt-cache Installed-Size 实测；共享的 libreoffice-core 那 ~200MB 砍不掉）。
# 代价是 .xls/.ppt 变成**明确报不支持**而不是静默返空 —— `_cap_legacy` 会照实报，
# 见 tools/embed_service.py。省 30MB 换两种格式，不值，所以三件套都装。
FROM python:3.12-slim

# --no-install-recommends 是关键：`apt-get install -s libreoffice-writer` 实测 **587 个包**，
# 加 --no-install-recommends 后 **160 个**（三件套一起 186 个）—— 那 427 个里含 default-jre 与
# 整套 -l10n，文本抽取一个字节都用不上。
# 只装 writer/calc/impress（doc/xls/ppt 各一），不装 libreoffice 全家桶（-base/-draw/-math）。
#
# 不装 fonts-noto-cjk（apt-cache Installed-Size 实测 89MB）：本服务只做
# `--convert-to docx/xlsx/pptx`，是 XML→XML，中文以码点存在、不经过字形栅格化 →
# 缺字体不出豆腐块（probe 里 .doc 解出的中文就是证据）。只有导出 PDF/图片才需要字体，
# 那条路本服务不走。夹具要画中文字（测 OCR）时挂宿主机的 C:\Windows\Fonts，见 make_fixtures.py。
RUN apt-get update && apt-get install -y --no-install-recommends \
        libreoffice-writer libreoffice-calc libreoffice-impress \
    && rm -rf /var/lib/apt/lists/*

# pymupdf4llm：AGPL-3.0，业主已裁决可用（保标题层级）。pypdf 是 BSD 兜底，
# embed_service._p_pdf 的三级降级会自己选，两个都装着才有降级可言。
# 版本钉死：镜像是可分发产物，`pip install 最新` 会让「上周能解析的 pptx 今天报错」查不出原因。
RUN pip install --no-cache-dir \
        pymupdf4llm==0.0.17 pypdf==5.1.0 \
        python-docx==1.1.2 python-pptx==1.0.2 openpyxl==3.1.5 pillow==11.0.0

# OCR：tesseract 二进制 + chi_sim 语言包 + pytesseract 包装器。
# **选它是因为 tools/embed_service.py 的 `_p_image` 调的就是 pytesseract**（`_cap_ocr` 探
# `PIL.Image`/`pytesseract`/`tesseract` 三样），镜像装别的引擎再好也走不到那条路。
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
RUN apt-get update && apt-get install -y --no-install-recommends \
        tesseract-ocr tesseract-ocr-chi-sim \
    && rm -rf /var/lib/apt/lists/* \
    && pip install --no-cache-dir pytesseract==0.3.13

COPY parse_service.py make_fixtures.py /app/
WORKDIR /app
# 8077 = 与宿主机 embed 服务同一个端口号（同形，见 parse_service.py）；宿主机侧发布哪个端口由 parser.ps1 决定
EXPOSE 8077
CMD ["python", "-u", "/app/parse_service.py"]
