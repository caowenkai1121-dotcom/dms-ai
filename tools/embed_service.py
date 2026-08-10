#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""向量层 + 文档服务：bge-small-zh-v1.5(512维, fastembed 本地自包含)。
  build    —— 为 meta.table_doc / sql_exemplar / element / datasource 算 embedding 存 pgvector
               + HNSW 索引（离线跑；`--ds` 限定注册表行属于哪个源，默认 dms）
  revec    —— build 的第五个目标，**单独跑**（`revec` 或 `build --revec`）：
               补 kb.chunk 里 embedding IS NULL 的行 + 把 kb.doc 从 chunked 推到 embedded。
               仍缺一行即非 0 退出（理由见 `revec_exit`）
  serve    —— 常驻 HTTP :8077（裁决 V1：文档解析复用本进程本端口）
               POST /embed  {"texts":[...],"query":true} → {"embeddings":[[...]]}
               POST /parse  {"path":"<绝对路径>","mime":""} → {"blocks":[...],"page_count":n,"sheets":[...]}
               POST /chunk  {"blocks":[...],"target_tokens":400,"overlap":60} → {"chunks":[...]}
               GET  /health → {"ok","model","dim","parse_ok":{...},"parse_caps":{".docx":{ok,why}}}
  selftest —— 自造 md/csv 跑一遍 parse+chunk（不需要任何第三方解析库）+ 把**每种扩展名**的
               可用/不可用与原因列全 + 钉住能力表纪律（见 `_selftest_caps`）
               + 扫描件 PDF 的 OCR 档判定与夹具（见 `_selftest_pdf_scan`）
用法: python embed_service.py build [--ds dms] | revec | serve [port] | selftest

外部依赖的位置可用环境变量覆盖（容器里可能不在 PATH 上）：
  DMS_SOFFICE   LibreOffice headless 可执行文件（旧二进制 .doc/.xls/.ppt 靠它转格式）
  DMS_TESSERACT tesseract 可执行文件（图片 OCR）
  DMS_OCR_LANG  OCR 语言包，默认 chi_sim+eng
"""
import os, sys, json, re, csv, io, math, itertools, importlib.util, shutil, subprocess, tempfile, threading, urllib.request
from html.parser import HTMLParser   # _p_html 去标签用标准库（SAC 拦的是编译扩展，stdlib 两侧都一定有）
sys.stdout.reconfigure(encoding='utf-8')

MODEL = 'BAAI/bge-small-zh-v1.5'
DIM = 512
QUERY_INSTRUCT = '为这个句子生成表示以用于检索相关文章：'


def pg_conf():
    """自有 PG 的连接参数，来自 `settings.json` 的 `pg_url`。

    🔴 这里原先写着**明文口令**（原文已删），违反「明文凭据只在 settings.json」。
    而 settings 里本来就有 `pg_url` —— 也就是说那是无谓的第二份真相源：
    改了 settings 的口令，本服务照旧拿旧口令连，连不上还得去猜为什么。
    **惰性读**（不是模块顶层常量）：`serve`/`selftest` 这些不碰 PG 的路径，
    缺 settings.json 时不该起不来 —— 只有真去连库的那一步才需要凭据。"""
    from settings import pg_kwargs   # tools/ 与本文件同目录，走 sys.path[0]
    return pg_kwargs()

_embedder = None
def embedder():
    global _embedder
    if _embedder is None:
        from fastembed import TextEmbedding
        _embedder = TextEmbedding(model_name=MODEL)
    return _embedder

# 推理的全局锁：fastembed/onnxruntime 的 session 不是线程安全的，所以推理**仍然串行**。
# 它存在的唯一理由是让 `serve` 能换成 ThreadingHTTPServer（见那里的注释）——
# /parse（上限 120s）与 /health 从此不再排在一次 275 块的 /embed 后面。
# 实测（本文件 `_selftest_serve_unblocked`，/embed 桩睡 0.6s）：单线程 /health 要 0.605s，
# 多线程 0.002s。顺带把首次惰性加载也串了：两个线程同时进来会各造一个 TextEmbedding。
# ponytail: 一把进程级锁 —— 本进程只有一个模型；真要并发推理得起多进程，那时再拆。
_EMBED_LOCK = threading.Lock()

def embed(texts, is_query=False):
    if is_query:
        texts = [QUERY_INSTRUCT + t for t in texts]
    with _EMBED_LOCK:
        return [v.tolist() for v in embedder().embed(texts)]

# ============ 文档服务：解析（K1）============
# 解析库一律惰性 import：缺依赖只让该类型报 unsupported，embed 功能不受影响。
MAX_ROWS, MAX_COLS = 200000, 200   # 单 sheet 上限，与 knowledge::tabular 一致；超出即报错不截断

class ParseError(Exception):
    """固定 JSON 形状 + HTTP 状态的解析失败（error 码给 Rust 侧枚举）"""
    def __init__(self, error, detail='', status=422):
        super().__init__(error)
        self.status = status
        self.payload = {'error': error}
        if detail:
            self.payload['detail'] = detail

def _push(stack, level, title):
    """标题栈：同级或更深的先弹出，再压入当前标题"""
    while stack and stack[-1][0] >= level:
        stack.pop()
    stack.append((level, title))

def _path(stack):
    return ' > '.join(t for _, t in stack)

def _blk(text, page, stack):
    return {'text': text, 'page': page, 'heading_path': _path(stack)}

_H_MD = re.compile(r'^(#{1,6})\s+(.+?)\s*#*$')

def md_blocks(text, page, stack):
    """markdown/纯文本 → blocks：# 层级维护 heading_path，空行分段。pdf 的 markdown 也走这里"""
    out, buf = [], []
    def flush():
        t = '\n'.join(buf).strip()
        buf.clear()
        if t:
            out.append(_blk(t, page, stack))
    for line in text.splitlines():
        m = _H_MD.match(line.strip())
        if m:
            flush()
            _push(stack, len(m.group(1)), m.group(2).strip())
            out.append(_blk(m.group(2).strip(), page, stack))
        elif line.strip():
            buf.append(line.rstrip())
        else:
            flush()
    flush()
    return out

def _read_text(path):
    """中文文档常是 GBK：utf-8 → gbk → 替换式兜底，绝不因编码整份失败"""
    with open(path, 'rb') as f:
        raw = f.read()
    for enc in ('utf-8-sig', 'gbk'):
        try:
            return raw.decode(enc)
        except UnicodeDecodeError:
            pass
    return raw.decode('utf-8', 'replace')

def _p_text(path):
    return md_blocks(_read_text(path), None, []), 0, []

def _p_json(path):
    """JSON → 美化后的 ```json 代码块（口径：json 转代码块，与文本同一条分块链）。
    校验失败不拒收 —— 按原文入库仍检索得到，但必须在 notes 留痕：
    静默降级成纯文本而不说，界面会以为结构化解析成功。"""
    text = _read_text(path)
    try:
        pretty = json.dumps(json.loads(text), ensure_ascii=False, indent=2)
    except ValueError as e:
        return md_blocks(text, None, []), 0, [], [f'JSON 校验失败（{e}），已按纯文本入库']
    return md_blocks(f'```json\n{pretty}\n```', None, []), 0, []


class _HtmlToText(HTMLParser):
    """HTML → 纯文本：script/style/noscript 整棵丢弃，块级标签换成换行，实体反转义。
    只出文本 —— 任何标签结构都不进检索正文（预览侧原件也只按 text/plain 展示）。"""
    _BLOCK = {'p', 'div', 'br', 'hr', 'li', 'tr', 'table', 'ul', 'ol', 'pre', 'title',
              'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'section', 'article', 'header', 'footer', 'blockquote'}
    _SKIP = {'script', 'style', 'noscript'}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts, self.skip = [], 0

    def handle_starttag(self, tag, attrs):
        if tag in self._SKIP:
            self.skip += 1
        elif tag in self._BLOCK:
            self.parts.append('\n')

    def handle_endtag(self, tag):
        if tag in self._SKIP:
            self.skip = max(0, self.skip - 1)
        elif tag in self._BLOCK:
            self.parts.append('\n')

    def handle_data(self, data):
        if not self.skip:
            self.parts.append(data)

    def text(self):
        raw = re.sub(r'\n+', '\n', ''.join(self.parts))
        return '\n'.join(x for x in (' '.join(l.split()) for l in raw.split('\n')) if x)


def _p_html(path):
    """HTML 去标签按纯文本入库，只用标准库。剥完无文本不报错：
    空 blocks 由 Rust 入库链统一判「文档里没有可索引的文本」（与 _p_text 同口径）。"""
    parser = _HtmlToText()
    parser.feed(_read_text(path))
    parser.close()
    return md_blocks(parser.text(), None, []), 0, []

def _p_pdf(path):
    """三级降级：pymupdf4llm（**保章节层级**）→ fitz（逐页纯文本）→ pypdf（逐页纯文本）。

    🔴 **许可分层是刻意的**：前两个都是 **AGPL-3.0**（pymupdf4llm 与 PyMuPDF 同源）。
    业主已裁决 AGPL-3.0 可用，所以第一级现在是**推荐**的那一级 —— 只有它给出章节层级：
    它输出 markdown，`#` 标题经 `md_blocks` 变成块的 `heading_path`（与 docx 的 Heading 同一条通道，
    检索引用因此能说清「在第几章」）。第二三级只出逐页纯文本，`heading_path` 恒为空串。
    第三级 `pypdf` 是 **BSD-3-Clause**、纯 Python 无二进制依赖，留作兜底：
    法务口径变了、或本机 Smart App Control 拦掉编译产物时，PDF 仍能进库。
    本机 `.venv` 实测正是这一档：pymupdf4llm/fitz 都没装上，`parse_ok['pdf']=True`
    而 `parse_caps()['.pdf']['why']` 报「pypdf 兜底（BSD-3）：只有逐页纯文本，heading_path 为空」。
    装到哪一级必须照实上报 —— 不上报，运维就无从知道自己拿到的是「带章节」还是「纯文本」
    那种质量（两者 `ok` 都是 true，肉眼分不出来）。

    降级用的 `except Exception` 不是偷懒：本机实测 **DLL 加载失败是 OSError 不是 ImportError**
    （`DLL load failed while importing etree`），只捕 ImportError 会让 PyMuPDF 装了但加载不了时
    整份 PDF 报 500，而不是降到 pypdf。同 `_have` 的口径。

    Y1 高阶档：**低文本量即判扫描件**（`_pdf_is_scanned`），低文本页经 `_pdf_ocr_fill` 渲染成图
    走 OCR（千问 vision 优先、tesseract 降级，与 `_p_image` 同一通道）。三级文本引擎共用这一档。
    """
    try:
        import pymupdf4llm
    except Exception:
        return _pdf_fitz(path)
    pages = pymupdf4llm.to_markdown(path, page_chunks=True)
    stack, out, page_chars = [], [], {}
    for i, pg in enumerate(pages, 1):
        no = (pg.get('metadata') or {}).get('page', i)
        raw = pg.get('text') or ''
        page_chars[no] = _page_chars(raw)   # 扫描件判定用**过滤前**的原始页文本；键与块的页号同源
        out += md_blocks(raw, no, stack)
    # 🔴 **先滤掉没有任何词字符的块，再判空。** 缺这一行的后果是实测出来的：
    # `pymupdf4llm.to_markdown` 每页尾部会输出一条 markdown 分隔线 `-----`，
    # `md_blocks` 把它当正文块 emit，于是 `if not out` 对**扫描件恒不成立** ——
    # 同一份无文本层的 PDF：容器（pymupdf4llm 一级）返 `200 {"blocks":[{"text":"-----"}]}`，
    # 宿主机（pypdf 三级）返 `422 no_text_layer`。
    # 前者意味着 Rust 侧拿到 1 块 → `status=embedded` → 界面「已入库」→ 问什么都答不出来，
    # 而那条 `-----` 还会进向量索引、甚至成为引用。正是本仓反复抓的静默失败族。
    # 第二个面（同一根因）：有文本层的 PDF 每页也会多一个 `-----` 垃圾块，
    # 被 `chunk_blocks` 按 heading_path 合并时拼进相邻 chunk 的正文，污染入库原文与原文回查判据。
    # 实测：员工公寓管理办法_文本层.pdf（2 页）从 12 块降到 10 块，第 5/11 块那两条 `-----` 消失。
    out = [b for b in out if re.search(r'\w', b.get('text', ''))]
    # 低文本量即判扫描件（Y1）：低文本页渲染成图走 OCR —— 判定/页数护栏/失败口径全在
    # `_pdf_ocr_fill`（钉在 `_selftest_pdf_scan`），实测动机写在那里，不重复。
    notes = _pdf_ocr_fill(path, len(pages), page_chars, out)
    # 第三位恒是 sheets（PDF 没有工作表 → 空），说明走第四位 notes（理由见 parse_doc）
    return out, len(pages), [], notes


def _pdf_page_ocr(path, page_no):
    """把 PDF 某一页渲染成图再 OCR（`fitz` + 已有的 `_p_image` 通道）。

    只在 `_pdf_ocr_fill` 补页时调用，**不登记进 `CAPS`**（`.pdf` 的实现者只有 `_p_pdf` 一个）。
    dpi 取 `OCR_DPI`（默认 200，实测折中：150 下 12pt 中文 tesseract 常丢字，300 单页多花 ~1.5 倍）。
    单页成本 ≈ fitz 渲染 0.1s + 识别 0.2~1s；「页数 × 单页成本」就是 `OCR_PAGE_CAP` 护栏的理由。
    """
    import fitz
    with fitz.open(path) as doc:
        pix = doc[page_no - 1].get_pixmap(dpi=OCR_DPI)
        png = pix.tobytes('png')
    import tempfile
    with tempfile.NamedTemporaryFile(suffix='.png', delete=False) as f:
        f.write(png)
        tmp = f.name
    try:
        # ⚠️ 只取第一位。`_p_image` 返的是**4 元**（blocks/frames/sheets/notes）——
        # 写成 `blocks, n, skipped = _p_image(tmp)` 会 `too many values to unpack`，
        # 而它发生在 `_p_pdf` 内部、表现成整份 PDF **HTTP 500**（实测踩过）。
        blocks = _p_image(tmp)[0]
    finally:
        os.unlink(tmp)
    # `_p_image` 按帧编号（渲染出来的单页图恒 1 帧），改回真实页号；heading 标明是 OCR 来的
    for b in blocks:
        b['page'] = page_no
        b['heading_path'] = f'第 {page_no} 页（OCR）'
    return blocks


def _page_chars(text):
    """页文本量口径：非空白字符数。比「词字符」宽（中文标点也是内容）；
    pymupdf4llm 每页尾部分隔线 `-----` 贡献 5 个字符，对 50 的阈值没有影响。"""
    return len(re.sub(r'\s+', '', text or ''))


def _pdf_is_scanned(total_chars, page_count):
    """整份判扫描件（Y1 裁决阈值）：全文 < PDF_DOC_MIN_CHARS，或页均 < PDF_PAGE_MIN_CHARS。
    0 页的退化输入不算扫描件（交给 `_pdf_ocr_fill` 的 `not out` 统一报 no_text_layer）。"""
    if page_count <= 0:
        return False
    return total_chars < PDF_DOC_MIN_CHARS or total_chars / page_count < PDF_PAGE_MIN_CHARS


def _pdf_low_text_pages(page_chars, page_count):
    """要 OCR 补的页号：该页文本层 < PDF_PAGE_MIN_CHARS 个非空白字符（缺席页按 0 计）。
    原「没有任何块的页」判据是它的特例（0 < 50 恒真）—— 低文本页（垃圾文本层）是新抓的形态。"""
    return [i for i in range(1, page_count + 1) if page_chars.get(i, 0) < PDF_PAGE_MIN_CHARS]


def _pdf_ocr_fill(path, page_count, page_chars, out, ocr_fn=None, cap=None):
    """低文本页渲染成图走 OCR，把块补进 `out`（原地），返回 notes（人话，进响应第四位）。

    `ocr_fn(path, page_no) -> blocks`，默认 `_pdf_page_ocr`；参数化只为 selftest 用桩驱动。
    判定与失败口径（`_selftest_pdf_scan` 逐条钉住）：

    🔴 为什么逐页都要补（只判整份是不够的）：实测造一份 2 页 PDF（页 1 真文本层、页 2 只有
    一张图）→ `200 page_count=2`，页 2 图上的字一个都没进索引，文档照旧推 `embedded`。
    带扫描附件/签字页的 PDF 在制度库里非常常见 —— 那是「静默成功」，比整份失败坏得多
    （整份失败会 422，用户知道要先 OCR；丢半份不会报错，用户以为全文都在里面）。

      - 整份判扫描件（`_pdf_is_scanned`）时 **need = 全部页**：「每页 60 个垃圾字符」那种
        文本层单页看能过 50 的线，合计仍是什么都没有 —— 判了扫描件，每页都可疑。
      - 需要 OCR 的页 > cap → `too_large` 响亮失败，**不「OCR 前 N 页然后报已入库」**。
      - 单页 OCR 失败（含引擎缺席/意外异常）不炸整份：记入 notes（会经 Rust `notes` 带到界面）。
      - OCR 成功的页：块按页号排回（chunk 的相邻关系/span 依赖页序）。文本层那 <50 字符的
        残块**保留**：页脚/页码在 OCR 文本里通常也读得出，重复上限 49 字符/页，可接受；
        丢掉才是把「唯一的真文本」赌在 OCR 质量上。
      - 整份判扫描件、OCR 一字未补、文本层合计仍 < PDF_DOC_MIN_CHARS → `no_text_layer`：
        带着零星字符按「已入库」静默过去，正是本仓反复抓的那个失败族。
        （页均腿判扫描但全文 ≥200 的，保留文本层 + 留痕，不升级为整份失败。）
    """
    ocr_fn = ocr_fn or _pdf_page_ocr
    cap = OCR_PAGE_CAP if cap is None else cap
    total = sum(page_chars.get(i, 0) for i in range(1, page_count + 1))
    scanned = _pdf_is_scanned(total, page_count)
    need = list(range(1, page_count + 1)) if scanned else _pdf_low_text_pages(page_chars, page_count)
    if len(need) > cap:
        raise ParseError(
            'too_large',
            f'{len(need)} 页无文本层/文本量过低需要 OCR，超过上限 {cap} 页；'
            f'请先在源头 OCR 后再上传（避免只处理前几页却报「已入库」）',
        )
    ocr_pages, failed = [], []
    for i in need:
        try:
            blocks = ocr_fn(path, i)
        except ParseError as e:
            failed.append((i, e.payload.get('error')))
            continue
        except Exception as e:
            # PIL/fitz/子进程的意外错误不许 500 整份：按单页失败留痕（与 ParseError 同口径）
            failed.append((i, f'{type(e).__name__}: {e}'[:120]))
            continue
        if not blocks:
            # 引擎读不出字（空白页/全图无字）：按失败记，不算「已补」—— 不然 scanned 判定被骗过
            failed.append((i, 'OCR 未识别出文字'))
            continue
        out += blocks
        ocr_pages.append(i)
    # 页序：OCR 补进来的块追加在末尾，按页号排回去 —— 否则 chunk 的相邻关系（span）会错
    out.sort(key=lambda b: (b.get('page') or 0))
    if not out:
        raise ParseError('no_text_layer', '整份扫描版：文本层为空，OCR 也未补出文字')
    if scanned and not ocr_pages and \
            sum(_page_chars(b.get('text', '')) for b in out) < PDF_DOC_MIN_CHARS:
        why = failed[0][1] if failed else 'OCR 引擎不可用'
        raise ParseError('no_text_layer',
                         f'文本层共 {total} 个非空白字符，按扫描件处理；OCR 未补出文字（{why}）')
    notes = []
    if ocr_pages:
        notes.append(f'第 {", ".join(map(str, ocr_pages))} 页无文本层/文本量过低，已用 OCR 补')
    if failed:
        notes.append(f'第 {", ".join(str(i) for i, _ in failed)} 页 OCR 未成（{failed[0][1]}）')
    return notes

# 降级层**不叫 `_p_*`**：`_p_` 前缀在本文件的约定是「登记在 `CAPS` 里的入口解析器」，
# `_selftest_caps` 按这个前缀双向核对登记表与实现者。这两个是 `_p_pdf` 内部的备选层，
# 登记它们反而会让 `.pdf` 有三个「实现者」。
def _pdf_fitz(path):
    """第二级（逐页纯文本）。同样过 `_pdf_ocr_fill`：pymupdf4llm 缺席时（部分容器/宿主机），
    混合扫描件的图像页曾经在这一级**静默消失**（只出文本页、无 notes、照推 embedded）——
    与 `_p_pdf` 同一个失败族，修一处漏一处等于没修。`not out` 的 no_text_layer 由 fill 统一报。"""
    try:
        import fitz
    except Exception:
        return _pdf_pypdf(path)
    doc = fitz.open(path)
    texts = [p.get_text() for p in doc]
    n = doc.page_count
    doc.close()
    page_chars = {i: _page_chars(t) for i, t in enumerate(texts, 1)}
    out = [_blk(t.strip(), i, []) for i, t in enumerate(texts, 1) if t.strip()]
    notes = _pdf_ocr_fill(path, n, page_chars, out)
    return out, n, [], notes

def _pdf_pypdf(path):
    """BSD 许可的最后一级。三级全不可用时**复用 `_cap_pdf()` 那一句**（不再自己写一遍文案）：
    两份文案会漂移，而漂移方向恰好是「能力上报说可用、真解析报另一件事」。
    fitz 缺席才轮到这一级 —— 渲染不了页图，OCR 档只能判、不能补。"""
    try:
        from pypdf import PdfReader
    except Exception:
        raise ParseError('unsupported', _cap_pdf() or 'PDF 依赖不可用')
    r = PdfReader(path)
    texts = [(p.extract_text() or '') for p in r.pages]
    out = [_blk(t.strip(), i, []) for i, t in enumerate(texts, 1) if t.strip()]
    if not out:
        raise ParseError('no_text_layer')      # 扫描版：与上面两级同一口径，显式失败
    # 低文本量判定在这一级也要做：垃圾文本层（每页零星几个字符）按「已入库」静默过去，
    # 就是 `_pdf_ocr_fill` 文档里那个失败族 —— 这一级补不了 OCR，至少要响亮失败。
    total = sum(_page_chars(t) for t in texts)
    if _pdf_is_scanned(total, len(texts)):
        raise ParseError('no_text_layer',
                         f'文本层共 {total} 个非空白字符，按扫描件处理；'
                         'OCR 档需要 fitz 渲染页面（pip install PyMuPDF），当前不可用')
    return out, len(texts), []

_H_DOCX = re.compile(r'(?:Heading|标题)\s*(\d)')

def _p_docx(path):
    # 依赖不在时轮不到这里：`parse_doc` 的能力门先拒（缺依赖 = 明确 unsupported，见那里的注释）。
    # 解析器**不再各自判一次依赖**：以前每个解析器写一句自己的「缺少依赖 X」，与 PARSE_DEPS 两份真相。
    import docx
    from docx.table import Table
    from docx.text.paragraph import Paragraph
    doc = docx.Document(path)
    stack, out = [], []
    for child in doc.element.body.iterchildren():      # 按正文顺序走，表格不许漏
        tag = child.tag.rsplit('}', 1)[-1]
        if tag == 'p':
            p = Paragraph(child, doc)
            if not (t := p.text.strip()):
                continue
            if m := _H_DOCX.search(getattr(p.style, 'name', '') or ''):
                _push(stack, int(m.group(1)), t)
            out.append(_blk(t, None, stack))
        elif tag == 'tbl':
            rows = [' | '.join(c.text.strip() for c in r.cells) for r in Table(child, doc).rows]
            if rows:
                out.append(_blk('\n'.join(rows), None, stack))
    return out, 0, []

def _p_pptx(path):
    from pptx import Presentation      # 依赖门在 parse_doc，同 _p_docx
    prs = Presentation(path)
    out, n = [], 0
    for i, slide in enumerate(prs.slides, 1):
        n = i
        title = (slide.shapes.title.text.strip() if slide.shapes.title is not None else '') or f'第{i}页'
        texts = [s.text_frame.text.strip() for s in slide.shapes
                 if s.has_text_frame and s.text_frame.text.strip()]
        if texts:
            out.append({'text': '\n'.join(texts), 'page': i, 'heading_path': title})
    return out, n, []

def _cell(v):
    return '' if v is None else str(v)

def _sheet(name, rows):
    """行列上限：超出立刻报错（不截断——截断=用户以为传成功但数据少一半；也不先吃满内存，本进程还托着 /embed）"""
    keep = []
    for r in rows:
        if not any(x != '' for x in r):
            continue
        if len(r) > MAX_COLS:
            raise ParseError('too_large', f'{name} 列数超 {MAX_COLS}')
        keep.append(r)
        if len(keep) > MAX_ROWS + 1:
            raise ParseError('too_large', f'{name} 行数超 {MAX_ROWS}')
    if not keep:
        # 🔴 空 sheet **也要报出来**（空表头 + 空行），不能在这里丢掉。
        # Rust 侧 `tabular::plan` 的契约是「空表/无表头不建表，但把名字放进 `skipped`，
        # 不能静默 —— 用户以为整份文件都能问数，结果少了一个 sheet 是个安静的数据缺口」。
        # 它只能报**它收到过**的 sheet：在这里 return None 就等于让那条契约对「全空的 sheet」
        # 恒不成立。实测抓到：两个 sheet 的 xlsx 只回 1 个，另一个无声消失。
        # 文本通道那侧由 `tabular::sheet_blocks` 跳过它（否则会多一个只有标题的垃圾块）。
        return {'name': name, 'header': [], 'rows': []}
    return {'name': name, 'header': keep[0], 'rows': keep[1:]}

def _p_xlsx(path):
    """表格只出 sheets（单元格矩阵），markdown 文本通道由 knowledge::tabular 的 sheet_blocks 出"""
    import openpyxl                    # 依赖门在 parse_doc，同 _p_docx
    wb = openpyxl.load_workbook(path, read_only=True, data_only=True)
    try:
        sheets = [s for ws in wb.worksheets
                  if (s := _sheet(ws.title, ([_cell(c) for c in r]
                                             for r in ws.iter_rows(values_only=True))))]
    finally:
        wb.close()
    return [], 0, sheets

def _p_csv(path):
    text = _read_text(path)
    try:
        dialect = csv.Sniffer().sniff(text[:4096], delimiters=',;\t|')
    except csv.Error:
        dialect = csv.excel
    rows = ([_cell(c) for c in r] for r in csv.reader(io.StringIO(text), dialect))
    s = _sheet(os.path.splitext(os.path.basename(path))[0], rows)
    return [], 0, ([s] if s else [])

OCR_LANG = os.environ.get('DMS_OCR_LANG', 'chi_sim+eng')

def _p_image(path):
    """图片 OCR → 一个块（图片没有章节结构，`heading_path` 用文件名，引用时看得出来源）。

    🔴 **识别不出文字必须报错**，不许 `return [], 0, []`。静默返空的后果不是「少一份文档」：
    Rust `ingest::run` 拿到 0 块会走 `chunks.is_empty()` 那条 BadInput，但只要同一份文件
    还带出过 sheets 就会照旧推到 `embedded` —— 界面显示「已入库」、chunk_count=0、
    问什么都答不出来。那是本仓反复抓的静默失败族，不许在新增格式时再造一遍。
    `no_text_layer` 与扫描版 PDF 同一个码：Rust 侧已把它映成「需先 OCR」的确定性失败。

    🔴 **OCR 引擎：千问 flash 优先，tesseract 降级**（业主 2026-07-31 裁决「图片识别用千问」）。
    实测（`_silent/` 三个扫描件，全部只读实测）：
      - `multiframe.tif` 两帧 → 千问 689/712ms 全对（`TIFFPAGE2-7788`）；tesseract 只认第一帧
      - `scanned.pdf` → 千问 896ms 读出 `SCANONLY-3344`；tesseract 产空文本
      - `mixed.pdf` 文本层+图像页 → 千问逐页全对（`TEXTPAGE1-5566` / `PDFOCR2-9911`）
    千问 flash 是 vision 模型（自己吃图，988ms 级，3/3 全对，比 tesseract 准且**逐帧不丢**）。
    零新依赖（`urllib.request` 发 HTTP，不引 openai 包 —— 宿主机 SAC 会拦新编译扩展）。
    配置：`DMS_QWEN_OCR_KEY`（或复用 `llm_api_key`）、`DMS_QWEN_OCR_MODEL`（默认 qwen3.7-flash）、
    `DMS_QWEN_OCR_BASE`（默认 dashscope compatible-mode）。千问不可用时回落 tesseract。
    """
    from PIL import Image, ImageSequence
    name = os.path.basename(path)
    blocks, frames = [], 0
    try:
        with Image.open(path) as im:
            for i, frame in enumerate(ImageSequence.Iterator(im), 1):
                frames = i
                # 优先千问；不可用/失败回落 tesseract（两路同一形状：一帧一块）
                t = _ocr_qwen_frame(frame) or _ocr_tesseract_frame(frame)
                if t:
                    blocks.append({'text': t, 'page': i, 'heading_path': name})
    except Exception as e:
        raise ParseError('unsupported', f'OCR 失败（{e}）')
    if not blocks:
        raise ParseError('no_text_layer', f'图片 OCR 未识别出文字（{frames} 帧全空）')
    notes = [] if len(blocks) == frames else [
        f'第 {i} 帧未识别出文字' for i in range(1, frames + 1)
        if not any(b['page'] == i for b in blocks)
    ]
    return blocks, frames, [], notes


def _ocr_tesseract_frame(frame):
    """tesseract 那一帧。`ParseError` 留给 `_p_image` 统一处理（语言包缺失要报出来）。"""
    import pytesseract
    if exe := _exe('DMS_TESSERACT', 'tesseract'):
        pytesseract.pytesseract.tesseract_cmd = exe
    try:
        return pytesseract.image_to_string(frame, lang=OCR_LANG).strip()
    except Exception as e:
        raise ParseError('unsupported', f'tesseract OCR 失败（lang={OCR_LANG}）：{e}')


def _ocr_qwen_frame(frame):
    """千问 flash 那一帧。**任何失败都返 None**（由 `_p_image` 回落 tesseract），不抛 —
    一张图的网络抖动不该让整份扫描件失败（回落的方向是「降级但入库」，不是「整份拒收」）。
    返回空串也算 None：模型读不出字时静默回落，让 tesseract 再试一次。"""
    key = os.environ.get('DMS_QWEN_OCR_KEY') or os.environ.get('QWEN_KEY', '')
    if not key:
        return None
    import base64
    import io as _io
    b = _io.BytesIO()
    frame.convert('RGB').save(b, 'PNG')
    b64 = base64.b64encode(b.getvalue()).decode()
    body = {
        'model': os.environ.get('DMS_QWEN_OCR_MODEL', 'qwen3.7-flash'),
        'temperature': 0.1,
        'enable_thinking': False,
        'messages': [{'role': 'user', 'content': [
            {'type': 'text', 'text':
                '读出这张图里的全部文字。如果是一则通知/制度/标准，'
                '把标题、要点、数字、金额、日期逐条列出。只输出读到的内容，不要解释。'},
            {'type': 'image_url', 'image_url': {'url': f'data:image/png;base64,{b64}'}},
        ]}],
    }
    base = os.environ.get(
        'DMS_QWEN_OCR_BASE', 'https://dashscope.aliyuncs.com/compatible-mode/v1')
    try:
        req = urllib.request.Request(
            f'{base}/chat/completions', data=json.dumps(body).encode(),
            headers={'Authorization': f'Bearer {key}', 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=60) as r:
            v = json.load(r)
        return (v['choices'][0]['message'].get('content') or '').strip() or None
    except Exception:
        return None

IMG_EXTS = ('.png', '.jpg', '.jpeg', '.bmp', '.tif', '.tiff', '.webp', '.gif')
# ── 扫描件检测与 OCR 档（Y1）：文本层「低文本量」即判扫描件 → 页面渲染成图 → vision OCR ──
# 阈值是**可调常量**（环境变量覆盖），判据由 `_selftest_pdf_scan` 钉住 —— 改阈值先改钉。
# 单页非空白字符 < 50 → 该页送 OCR（零文本页是它的特例：原「没有块的页」判据被它覆盖）。
PDF_PAGE_MIN_CHARS = int(os.environ.get('DMS_PDF_PAGE_MIN_CHARS', '50'))
# 整份判扫描件：全文 < 200，或页均 < 单页阈值（垃圾文本层：每页零星几个字符，合计不少、内容没有）。
PDF_DOC_MIN_CHARS = int(os.environ.get('DMS_PDF_DOC_MIN_CHARS', '200'))
# 渲染 dpi（`_pdf_page_ocr`）：200 是实测折中 —— 150 下 12pt 中文 tesseract 常丢字，300 单页多花 ~1.5 倍。
OCR_DPI = int(os.environ.get('DMS_OCR_DPI', '200'))
# PDF 逐页补 OCR 的页数上限（`_pdf_ocr_fill`）。可用 DMS_OCR_PAGE_CAP 覆盖。
# 成本口径：OCR 档每页 ≈ fitz 渲染 0.1s（200dpi）+ 识别 0.2~1s（千问 vision ~1s/页，tesseract 0.2~1s/页）。
# 30 页 × ~1s ≈ 半分钟，仍低于 Rust 侧 120s 解析超时（connector/src/doc.rs PARSE_TIMEOUT_SECS）；
# 超 cap 不「OCR 前 N 页然后报已入库」—— too_large 响亮失败。
OCR_PAGE_CAP = int(os.environ.get('DMS_OCR_PAGE_CAP', '30'))
LEGACY_TARGET = {'.doc': '.docx', '.xls': '.xlsx', '.ppt': '.pptx'}
SOFFICE_TIMEOUT = 120      # 首次转换含 LibreOffice 建 profile，慢；超时要响亮而不是挂死请求

def _p_legacy(path):
    """旧二进制 Office（OLE/CFB 的 .doc/.xls/.ppt）：**不自己解**，交 LibreOffice headless
    转成对应的新格式，再走已有的 `_p_docx/_p_xlsx/_p_pptx`（一条解析通道，不是两套代码）。

    `-env:UserInstallation` 给每次调用一个独立 profile：共享默认 profile 时，第二个并发
    soffice **直接退出且什么都不产出，返回码仍是 0** —— 那正好是「转换成功但没有文件」
    这种静默形态。本函数据此只信「产物文件在不在」，不信返回码。
    soffice 缺席时轮不到这里：`parse_doc` 的能力门先拒（`_cap_legacy` 同时要求目标格式的解析器，
    少查那一半就会变成「转出来了却解析不了」）。
    """
    ext = os.path.splitext(path)[1].lower()
    tgt = LEGACY_TARGET[ext]
    with tempfile.TemporaryDirectory() as d:
        prof = 'file:///' + os.path.join(d, 'profile').replace('\\', '/').lstrip('/')
        try:
            r = subprocess.run([_soffice(), f'-env:UserInstallation={prof}', '--headless',
                                '--convert-to', tgt[1:], '--outdir', d, path],
                               capture_output=True, timeout=SOFFICE_TIMEOUT)
        except subprocess.TimeoutExpired:
            raise ParseError('unsupported', f'soffice 转换 {ext} 超过 {SOFFICE_TIMEOUT}s')
        out = os.path.join(d, os.path.splitext(os.path.basename(path))[0] + tgt)
        if not os.path.isfile(out):
            why = (r.stderr or r.stdout or b'').decode('utf-8', 'replace').strip()[:200]
            raise ParseError('unsupported', f'soffice 未能把 {ext} 转成 {tgt}（rc={r.returncode}）{why}')
        return CAPS[tgt][0](out)

# ── 依赖探测：每种格式「装了没」的**唯一实现**。返回 '' = 可用，否则返回给运维看的原因 ──
# 🔴 原先是两份真相：`PARSE_DEPS` 一份、每个解析器自己那句「缺少依赖 python-docx」一份。
# 两份文案会漂移，而漂移方向恰好是**上报说可用、真解析报另一件事**（`find_spec` 那次就是这么发生的）。
# 现在解析器不判依赖，`parse_doc` 在入口过这道门 —— 能力上报与拒绝理由必然同一句话。

def _mod(mod, pip):
    """单模块依赖。`mod` 是 **import 名**（python-docx 的 import 名是 docx），`pip` 是装包名。"""
    return lambda: '' if _have(mod) else f'{mod} 不可用（pip install {pip}）：{_why(mod)}'

def _cap_ok():
    return ''      # md/txt/csv 纯标准库，恒可用

def _cap_pdf():
    """三级任一即可（见 `_p_pdf`）。装到哪一级决定有没有章节层级，由 `_pdf_tier` 照实上报。"""
    if _have('pymupdf4llm') or _have('fitz') or _have('pypdf'):
        return ''
    return ('PDF 三级依赖全不可用（pip install pymupdf4llm —— AGPL-3.0，业主已裁决可用，'
            '带章节层级 heading_path；或 pypdf —— BSD-3，只出逐页纯文本）：'
            + '；'.join(f'{m}（{_why(m)}）' for m in ('pymupdf4llm', 'fitz', 'pypdf')))

def _pdf_text_engine():
    """text 档三级引擎里当前会用的那一级（`_pdf_tier` 的人话与 `tiers` 的机读值同源，不许漂）。"""
    return ('pymupdf4llm' if _have('pymupdf4llm')
            else 'fitz' if _have('fitz') else 'pypdf')

def _ocr_engine():
    """OCR 档实际会用的引擎：'qwen' / 'tesseract' / None。
    与 `_cap_ocr` 同一判定、只是报名字 —— 两处判定不许漂（上报可用、真解析不可用 = 两套口径）。"""
    if (os.environ.get('DMS_QWEN_OCR_KEY') or os.environ.get('QWEN_KEY')) and _have('PIL.Image'):
        return 'qwen'
    if _have('PIL.Image') and _have('pytesseract') and _exe('DMS_TESSERACT', 'tesseract'):
        return 'tesseract'
    return None

def _pdf_tier():
    """text 档 + ocr 档的自报人话（Y1：扫描件高阶档）。ocr 档要**两半**：渲染（fitz 把页
    渲成图）+ 识别引擎（千问 vision / tesseract）—— 缺一半就是不可用，自报必须说清缺哪半。"""
    text = {'pymupdf4llm': 'pymupdf4llm：带章节层级 heading_path',
            'fitz': 'fitz 兜底：只有逐页纯文本，heading_path 为空',
            'pypdf': 'pypdf 兜底（BSD-3）：只有逐页纯文本，heading_path 为空'}[_pdf_text_engine()]
    eng = _ocr_engine()
    if not _have('fitz'):
        ocr = '不可用（fitz 缺席，扫描件页面渲不成图；pip install PyMuPDF）'
    elif eng == 'qwen':
        ocr = '千问 vision flash 优先，tesseract 降级'
    elif eng == 'tesseract':
        ocr = 'tesseract（本地）'
    else:
        ocr = '不可用（OCR 引擎缺席：配 DMS_QWEN_OCR_KEY 走千问，或装 tesseract，详见 .png 能力行）'
    return f'{text}；扫描件 OCR 档：{ocr}'

def _cap_ocr():
    """OCR 引擎：**千问 flash 或 tesseract 有一样就能用**（`_p_image` 优先千问、降级 tesseract）。
    千问要 `DMS_QWEN_OCR_KEY`（或 `QWEN_KEY`）+ Pillow（帧转 png 用）。
    tesseract 要三样：Pillow、pytesseract（只是包装器）、tesseract **二进制**。
    探 `PIL.Image` 而不是 `PIL`：编译扩展 `_imaging` 在子模块里，正是本机 SAC 拦的那一类。"""
    qwen_ok = bool(os.environ.get('DMS_QWEN_OCR_KEY') or os.environ.get('QWEN_KEY'))
    if qwen_ok and _have('PIL.Image'):
        return ''       # 千问路通，tesseract 那套不是必需
    miss = [f'{m}（{_why(m)}）' for m in ('PIL.Image', 'pytesseract') if not _have(m)]
    if miss:
        return 'OCR 依赖不可用（pip install pillow pytesseract）：' + '；'.join(miss)
    if not _exe('DMS_TESSERACT', 'tesseract'):
        return ('找不到 tesseract 可执行文件（pytesseract 只是包装器，二进制要单独装）：'
                'apt-get install tesseract-ocr tesseract-ocr-chi-sim，或 DMS_TESSERACT 指全路径；'
                '或配 DMS_QWEN_OCR_KEY 走千问 flash（业主裁决，图片识别用千问）')
    return ''

def _cap_legacy(target):
    """旧二进制 Office 要**两半**：soffice 转格式 + 目标格式的解析器（.doc→.docx 仍要 python-docx）。"""
    return lambda: ('找不到 soffice/libreoffice 可执行文件（旧二进制 Office 靠 LibreOffice headless '
                    '转成新格式）：apt-get install libreoffice-writer/-calc/-impress，'
                    '或 DMS_SOFFICE 指全路径') if not _soffice() else CAPS[target][2]()

# 🔴 **单一事实源**：扩展名 → (解析器, 能力名, 依赖探测)。
# 以前是两张表（`PARSERS` 管扩展名、`PARSE_DEPS` 管依赖），于是「登记而不消费」查不出来：
# 声明支持某扩展名却没有实现者、或写了解析器却没登记（/parse 永远走不到），两种都静默。
# 现在 `parse_doc` / `parse_ok` / `parse_caps` 全从这一张表派生，`_selftest_caps` 双向钉住。
CAPS = {
    '.pdf': (_p_pdf, 'pdf', _cap_pdf),
    '.docx': (_p_docx, 'docx', _mod('docx', 'python-docx')),
    '.pptx': (_p_pptx, 'pptx', _mod('pptx', 'python-pptx')),
    '.xlsx': (_p_xlsx, 'xlsx', _mod('openpyxl', 'openpyxl')),
    '.xlsm': (_p_xlsx, 'xlsx', _mod('openpyxl', 'openpyxl')),
    '.csv': (_p_csv, 'text', _cap_ok),
    '.md': (_p_text, 'text', _cap_ok),
    '.markdown': (_p_text, 'text', _cap_ok),
    '.txt': (_p_text, 'text', _cap_ok),
    # json/log/html：纯文本族，零第三方依赖（html 去标签走标准库 HTMLParser）
    '.json': (_p_json, 'text', _cap_ok),
    '.log': (_p_text, 'text', _cap_ok),
    '.html': (_p_html, 'text', _cap_ok),
    # 旧二进制 Office：各自一个能力名（三个 LibreOffice 组件是分开装的，合成一个键会
    # 让「只装了 writer」上报成整族可用）
    '.doc': (_p_legacy, 'doc', _cap_legacy('.docx')),
    '.xls': (_p_legacy, 'xls', _cap_legacy('.xlsx')),
    '.ppt': (_p_legacy, 'ppt', _cap_legacy('.pptx')),
    **{e: (_p_image, 'image', _cap_ocr) for e in IMG_EXTS},
}
MIME_EXT = {
    'application/pdf': '.pdf',
    'application/vnd.openxmlformats-officedocument.wordprocessingml.document': '.docx',
    'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': '.xlsx',
    'application/vnd.openxmlformats-officedocument.presentationml.presentation': '.pptx',
    'application/msword': '.doc',
    'application/vnd.ms-excel': '.xls',
    'application/vnd.ms-powerpoint': '.ppt',
    'image/png': '.png', 'image/jpeg': '.jpg', 'image/bmp': '.bmp',
    'image/tiff': '.tif', 'image/webp': '.webp', 'image/gif': '.gif',
    'text/csv': '.csv', 'text/markdown': '.md', 'text/plain': '.txt',
    'application/json': '.json', 'text/html': '.html',
}

def parse_doc(path, mime=''):
    ext = os.path.splitext(path)[1].lower()
    if ext not in CAPS:
        ext = MIME_EXT.get((mime or '').split(';')[0].strip(), ext)
    cap = CAPS.get(ext)
    if cap is None:
        raise ParseError('unsupported', ext or mime or path)
    if not os.path.isfile(path):
        raise ParseError('not_found', path, 404)
    # 依赖门**只有这一处**：缺依赖 = 明确的 `unsupported` + 一句原因（Rust 侧映成 BadInput，
    # 文档落 failed 且 error 里写清缺什么）。绝不许让解析器悄悄返回空 blocks —— 那条路的终点是
    # 「status=embedded、chunk_count=0、界面显示已入库、问什么都答不出来」。
    if why := cap[2]():
        raise ParseError('unsupported', f'{ext} 暂不可用：{why}')
    got = cap[0](path)
    # 解析器可以返 3 元或 4 元。**第三位恒是 `sheets`（表格类文件的工作表）**，
    # 第四位是可选的 `notes`（人话说明，例：「第 2 页无文本层，已用 OCR 补」）。
    #
    # 🔴 为什么必须分开：`sheets` 在 Rust 侧是 `Vec<Sheet>`（`connector/src/doc.rs::ParsedDoc`），
    # 往里塞字符串会让**整份响应反序列化失败**。我第一版把 PDF 的页注塞进第三位，
    # 混合 PDF 在 Rust 路径上就会直接解析失败 —— 自己实测响应体时才看出来
    # （`sheets = "第 2 页无文本层，已用 OCR 补"`）。
    # `notes` 是新键；Rust 侧没有 `deny_unknown_fields`，多这个键不影响老逻辑，
    # 将来要在界面上显示它只需加一个字段。
    blocks, page_count, sheets = got[0], got[1], got[2]
    out = {'blocks': blocks, 'page_count': page_count, 'sheets': sheets}
    if len(got) > 3 and got[3]:
        out['notes'] = got[3]
    return out

_HAVE_CACHE = {}
_HAVE_ERR = {}
_EXE_CACHE = {}

def _exe(env, *names):
    """外部**可执行文件**探测：soffice / tesseract 不是 python 模块，`_have` 探不到它们。
    `env` 优先（容器里可能装在非 PATH 位置）。缓存同 `_have`（`/health` 每次都调）——
    代价是改了 PATH/环境变量要重启服务，与依赖装卸同一个口径。"""
    if env not in _EXE_CACHE:
        _EXE_CACHE[env] = os.environ.get(env) or next(
            (p for n in names if (p := shutil.which(n))), '')
    return _EXE_CACHE[env]

def _soffice():
    return _exe('DMS_SOFFICE', 'soffice', 'libreoffice')

def _have(mod):
    """**真 import 一次**，不是 `find_spec`。

    🔴 `find_spec` 只查「包在不在」，查不出「import 会不会炸」。实测踩到：
    `python-docx` / `python-pptx` 装好了（spec 在），但它们依赖的 `lxml` 编译扩展被本机
    Smart App Control 拦掉（`DLL load failed while importing etree`）→ `find_spec` 说可用、
    真解析当场抛。那正是本函数文档里那句「不许假装可用」要防的事，而它自己犯了。
    结果缓存：`/health` 每次都调，重复 import 走 `sys.modules` 已经很快，但异常路径不缓存
    会每次重跑一遍失败的 DLL 加载。
    """
    if mod not in _HAVE_CACHE:
        try:
            importlib.import_module(mod)
            _HAVE_CACHE[mod] = True
        except Exception as e:      # ImportError / OSError(DLL) / 任何加载期异常都算不可用
            _HAVE_CACHE[mod] = False
            _HAVE_ERR[mod] = f'{type(e).__name__}: {e}'[:200]
    return _HAVE_CACHE[mod]

def _why(mod):
    """`_have` 失败的**原文**。必须进能力上报：不然「没装」与「装了但加载不了」长得一样，
    而处置完全相反。本机 `.venv` 实测两种都有：
      docx/pptx → `ImportError: DLL load failed while importing etree: 应用程序控制策略已阻止此文件。`
                  （装了，是 lxml 的编译 DLL 被 Smart App Control 拦）
      pymupdf4llm → `ModuleNotFoundError: No module named 'pymupdf4llm'`（真没装）
    只报一句「pip install python-docx」会让运维对着第一种白装一遍，再回来问为什么还是红的。"""
    _have(mod)
    return _HAVE_ERR.get(mod, '')

def parse_ok():
    """能力名 → 是否真的可用（粗粒度，历史形状）。`tools/parse_probe.py` 按 `pdf`/`xlsx`
    两个键决定跳不跳夹具 —— 键改名它会**静默全跳过并 exit 0**（判据恒绿），
    所以 `_selftest_caps` 有一条断言钉着这两个键。逐扩展名的原因看 `parse_caps()`。
    同名能力取**与**（不是任取一个）：一个能力名下的扩展名探测可以不同（.doc/.xls/.ppt 就是），
    落到 `dict` 推导里会被后写的覆盖，那正是「上报可用、其实只装了一半」。"""
    ok = {}
    for _, fam, probe in CAPS.values():
        ok[fam] = ok.get(fam, True) and not probe()
    return ok

def parse_caps():
    """**逐扩展名**的能力上报：`{'.docx': {'ok': bool, 'why': '一句人话'}}`。
    判定只看 `ok`；`why` 给人看 —— 不可用时说缺什么怎么装，可用时可能说降级（见下面 pdf 那句）。"""
    caps = {ext: {'ok': not (w := probe()), 'why': w} for ext, (_, _, probe) in CAPS.items()}
    # pdf 三级依赖里装到哪一级决定有没有章节层级，而那是升到 pymupdf4llm 的**全部理由**。
    # 上报里不说，运维就无从知道自己拿到的是「带章节」还是「纯文本」那种质量。
    if caps['.pdf']['ok']:
        caps['.pdf']['why'] = _pdf_tier()
        # 机读两档（Y1）：text = 三级文本引擎；ocr = 扫描件 OCR 引擎（fitz 缺席时恒 None，
        # 渲染那半先断了）。自报与真解析同源 —— `_pdf_ocr_fill` 用的就是这两样。
        caps['.pdf']['tiers'] = {'text': _pdf_text_engine(),
                                 'ocr': _ocr_engine() if _have('fitz') else None}
    return caps

# ============ 文档服务：分块（K1）============
CHARS_PER_TOKEN = 1.6      # 中文口径：1.6 字符/token（不是英文那套 4 字符/token，照搬会切出两倍大的块）
TARGET_TOKENS, OVERLAP = 400, 60
MAX_TOKENS = 480           # 硬上限：bge-small-zh-v1.5 max_seq_len=512，超了 fastembed 静默截断（症状是检索时好时坏）
_SENT = re.compile(r'(?<=[。！？；!?;\n])')

def est_tokens(text):
    return math.ceil(len(text) / CHARS_PER_TOKEN)

def _split_long(text, cap):
    """超长块先按句末标点切，仍超长则硬切"""
    parts, buf = [], ''
    for piece in _SENT.split(text):
        if buf and len(buf) + len(piece) > cap:
            parts.append(buf)
            buf = ''
        while len(piece) > cap:
            parts.append(piece[:cap])
            piece = piece[cap:]
        buf += piece
    if buf.strip():
        parts.append(buf)
    return parts

def _emit(chunks, text, hp, page):
    if not (text := text.strip()):
        return
    n = est_tokens(text)
    assert n <= MAX_TOKENS, f'块 {n} token 超上限 {MAX_TOKENS}（bge 512 窗口会静默截断）'
    chunks.append({'text': text, 'heading_path': hp, 'page': page, 'tokens': n})

def _fill(chunks, blocks, hp, target_chars, overlap_chars, cap):
    """同一 heading_path 内：按目标长度合并/切分，块间重叠 overlap_chars。

    🔴 `page` 由 buf **吸收过的页集合**定，跨页就置 None。
    原实现是 `if page is None: page = b.get('page')` —— 只取**第一个**贡献块的页，
    而 buf 之后还会吸收后面几页的块（PDF 的 heading stack 跨页延续，见 `_p_pdf`：
    一个 heading_path 的块常横跨 2-3 页）⇒ 一个跨 3 页的块标着「第 1 页」，
    用户翻到第 1 页找不到引用的那句话，还会以为引用是编的。
    `answer.rs::source_of` 与 `KbAnswer.vue::loc` 都在 page 为 None 时不显示页码：
    **「不知道」比「说错」好**。判据见 `_selftest_pages`。
    重叠尾巴不算跨页（它只是上一块末尾 overlap_chars 个字符的上下文，不是本块的实质内容）；
    `pages.add` 必须在 flush **之后**，否则触发 flush 的那一页会污染刚发出去的块。
    """
    def one_page(pages):
        real = {p for p in pages if p is not None}   # 无页码的块（md）不算一页
        return real.pop() if len(real) == 1 else None

    buf, pages, fresh = '', set(), False
    for b in blocks:
        for unit in _split_long((b.get('text') or '').strip(), cap):
            if buf and len(buf) + 1 + len(unit) > target_chars:
                # ponytail: 重叠取字符尾巴（可能从句中开始），检索质量不满意再改成按句边界回退
                _emit(chunks, buf, hp, one_page(pages))
                buf, pages, fresh = buf[len(buf) - overlap_chars:] if overlap_chars else '', set(), False
            pages.add(b.get('page'))
            buf = (buf + '\n' + unit) if buf else unit
            fresh = True
    if fresh:
        _emit(chunks, buf, hp, one_page(pages))

def _selftest_pages():
    """`_fill` 的页码判据，三条都反向验证过（2026-07-30，实际打坏再跑 `selftest`）：
    - `one_page` 换回「取第一个非空页」（`min(real)`）→ `across` 那条红：跨 3 页的块 page==1（原 bug）
    - `one_page` 一律返回 None → `same` 那条红：单页块也丢了页码（别把修法做成「一律不显示」）
    - `pages.add` 挪到 flush **之前** → `split` 那条红：两块都 None（第 2 页串味给刚发出去的第 1 块）"""
    same = [{'text': '甲' * 20, 'heading_path': 'H', 'page': 1},
            {'text': '乙' * 20, 'heading_path': 'H', 'page': 1}]
    across = [{'text': '甲' * 20, 'heading_path': 'H', 'page': 1},
              {'text': '乙' * 20, 'heading_path': 'H', 'page': 2},
              {'text': '丙' * 20, 'heading_path': 'H', 'page': 3}]
    a, b = chunk_blocks(same), chunk_blocks(across)
    assert len(a) == 1 and len(b) == 1, (a, b)        # 都合成一块，唯一差别就是页
    assert a[0]['page'] == 1, a
    assert b[0]['page'] is None, b                    # 跨 3 页 → 宁可不显示
    # 无页码的块（md）不算一页：单页 + 无页码仍按那唯一的真页算（保持原行为）
    mix = chunk_blocks([{'text': '甲' * 20, 'heading_path': 'H'},
                        {'text': '乙' * 20, 'heading_path': 'H', 'page': 7}])
    assert len(mix) == 1 and mix[0]['page'] == 7, mix
    # 切成多块时各记自己的页（别把「跨页置 None」做成「一律 None」，也别让 flush 那一页串味）
    split = chunk_blocks([{'text': '甲' * 400, 'heading_path': 'H', 'page': 1},
                          {'text': '乙' * 400, 'heading_path': 'H', 'page': 2}], overlap=0)
    assert [c['page'] for c in split] == [1, 2], split

def chunk_blocks(blocks, target_tokens=TARGET_TOKENS, overlap=OVERLAP):
    overlap = max(0, min(overlap, MAX_TOKENS // 4))
    target = max(1, min(target_tokens, MAX_TOKENS - overlap))
    tc, oc = int(target * CHARS_PER_TOKEN), int(overlap * CHARS_PER_TOKEN)
    cap = max(1, tc - oc - 1)   # 单元留出重叠余量：短标题块才能与正文合并，且 重叠+单元 不破 MAX_TOKENS
    chunks = []
    for hp, group in itertools.groupby(blocks, key=lambda b: b.get('heading_path') or ''):
        _fill(chunks, list(group), hp, tc, oc, cap)
    return chunks

def _vlit(v):
    """pgvector 字面量（与 Rust 侧 embed::to_pgvector 同一形状）"""
    return '[' + ','.join(f'{x:.6f}' for x in v) + ']'

def _revec(cur, what, sel, upd, sel_params=(), upd_tail=(), is_query=False):
    """两列 (key, 向量化文本) 的 SELECT → embed → 按 key 回写 embedding，返回条数。
    🔴 sel/upd 的 ds 限定由调用方拼死：对应 Rust 侧 `meta::DS_PRED`（单一事实源，
       `crates/server/src/meta.rs`）。python 侧不受那条漂移守卫保护 —— 漏一处就是跨源
       乱改 embedding，而 embedding 正是表召回与选源的依据。
    空结果直接返回：连模型都不加载（离线重跑常态是「没有待处理行」）。"""
    cur.execute(sel, sel_params)
    rows = cur.fetchall()
    if not rows:
        return 0
    print(f'计算 {len(rows)} {what} embedding …', flush=True)
    for r, v in zip(rows, embed([(r[1] or '')[:1000] for r in rows], is_query)):
        cur.execute(upd, (_vlit(v), r[0]) + upd_tail)
    return len(rows)

def build(ds='dms'):
    import psycopg2
    pg = psycopg2.connect(**pg_conf()); pg.autocommit = True; cur = pg.cursor()
    cur.execute("CREATE EXTENSION IF NOT EXISTS vector")
    cur.execute(f"ALTER TABLE meta.table_doc ADD COLUMN IF NOT EXISTS embedding vector({DIM})")
    n_tbl = _revec(
        cur, '表',
        "SELECT table_name, coalesce(nullif(search_doc, ''), table_name) FROM meta.table_doc"
        " WHERE ds_id = %s",                      # ds 限定 = Rust 侧 meta::DS_PRED
        "UPDATE meta.table_doc SET embedding = %s WHERE table_name = %s AND ds_id = %s",
        (ds,), (ds,))
    cur.execute("DROP INDEX IF EXISTS meta.idx_doc_hnsw")
    cur.execute("CREATE INDEX IF NOT EXISTS idx_doc_hnsw ON meta.table_doc"
                " USING hnsw (embedding vector_cosine_ops)")
    # 语料问句向量（供语义缓存召回；问句侧 query embedding，与 Rust 回写一致）
    n_ex = _revec(
        cur, '条语料问句',
        "SELECT id, question FROM meta.sql_exemplar"
        " WHERE status = 'enabled' AND embedding IS NULL AND ds_id = %s",   # 同上：meta::DS_PRED
        "UPDATE meta.sql_exemplar SET embedding = %s WHERE id = %s AND ds_id = %s",
        (ds,), (ds,), True)
    # 元素注册表向量（SuperSonic SchemaMapper 元素召回；search_text 变了自动 NULL 待重建）
    # 建表 DDL 的事实源在 Rust `meta::bootstrap_meta`；这份兜底副本必须跟着带 ds_id，
    # 否则「先跑 build 再启服务」会建出没有 ds_id 的表，下面那条 SQL 当场报缺列。
    cur.execute("CREATE TABLE IF NOT EXISTS meta.element("
                "element_id text PRIMARY KEY, kind text NOT NULL, name text NOT NULL,"
                "aliases text[] NOT NULL DEFAULT '{}', ref_expr text NOT NULL DEFAULT '',"
                "description text NOT NULL DEFAULT '', search_text text NOT NULL DEFAULT '',"
                "status text NOT NULL DEFAULT 'active',"
                "ds_id text NOT NULL DEFAULT 'dms')")
    cur.execute(f"ALTER TABLE meta.element ADD COLUMN IF NOT EXISTS embedding vector({DIM})")
    n_el = _revec(
        cur, '元素',
        "SELECT element_id, search_text FROM meta.element"
        " WHERE status = 'active' AND embedding IS NULL AND ds_id = %s",    # 同上：meta::DS_PRED
        "UPDATE meta.element SET embedding = %s WHERE element_id = %s AND ds_id = %s",
        (ds,), (ds,))
    cur.execute("DROP INDEX IF EXISTS meta.idx_element_hnsw")
    cur.execute("CREATE INDEX IF NOT EXISTS idx_element_hnsw ON meta.element"
                " USING hnsw (embedding vector_cosine_ops)")
    n_ds = _revec_datasources(cur)
    pg.close()
    print(f'完成[ds={ds}]：{n_tbl} 表 / {n_ex} 语料问句 / {n_el} 元素 / {n_ds} 数据源'
          f' 向量化 + HNSW 索引', flush=True)

def _revec_datasources(cur):
    """向量选源（`pipeline::select_source` → `meta::nearest_datasources`）的唯一写入点。
    ⚠️ 这里**不加也不能加 ds 限定**：meta.datasource 是 ds 注册表本身（Rust 那条漂移守卫
       也把它列为豁免），按 ds 过滤就只有当前源有向量 → 选源永远选不到别的源。
    只处理 embedding IS NULL：Rust `upsert_datasource` 在 description 变更时置 NULL 作失效。
    文本 = name + description，与 `pick_by_llm` 给模型看的两个字段一致；
    问句侧是 embed_query（带指令前缀），故这里是文档侧 is_query=False，同 table_doc。"""
    return _revec(cur, '数据源',
                  "SELECT ds_id, name || '。' || description FROM meta.datasource"
                  " WHERE status = 'active' AND embedding IS NULL",
                  "UPDATE meta.datasource SET embedding = %s WHERE ds_id = %s")

# ============ 第五个 build 目标：知识库向量补齐（revec）============
# 缺陷背景（已实测）：`knowledge/src/ingest.rs` 在向量服务不可用时把文档停在 `chunked` 并写
# 「向量服务不可用，稍后可重建」——**而重建它的实现者一直不存在**。
# 后果不是「少一路召回」而是那份文档基本永久检索不到：`knowledge/src/retrieve.rs` 三路里
# 中文 FTS 走 `to_tsvector('simple')`（连写中文切不出词）、trgm 又被 `TRGM_MIN=0.3` 挡着。
# 这两句不是推测，是量过的：14 个块 × 5 道题 = 70 组，**FTS 命中恒 0**；
# 「一线城市出差住宿费上限是多少」对答案所在块的 word_similarity 只有 0.267（<0.3，被挡）。
# 于是向量为空 = 零命中，而 UI 上那份文档显示「已入库」。
# 实测复现（报销制度.md，5 块）：embedding 置 NULL + status 退回 chunked → 同一问句
# citations **0 条**、回答「知识库里没有相关内容。」；跑本节后 citations 4 条、答出「500 元/晚」，
# 且 5 个块的 embedding md5 与置 NULL 前**逐个相同**（证明这里算的是文档侧向量、
# 和当初 ingest 同一个空间 —— 库因此也真回到了原状）。
#
# 一批的块数。实测本机：64 块（合 9675 字）一次 embed 2.21s，模型加载另 0.57s。
# 一批的文本总量因此是 ~10KB 量级 —— 几万块的库也不会被整份拉进内存（这是分批的全部理由；
# 每批立即回写 + autocommit，中途挂掉已补的不回滚，重跑从游标之后接着补）。
KB_BATCH = 64
KB_RECIPE = 1

def kb_embedding_text(name, folder_path, heading_path, body):
    """recipe v1；必须与 knowledge::store::chunk_embedding_text 逐字一致。"""
    return (f'文件：{name or ""}\n目录：{folder_path or "/"}\n章节：'
            f'{heading_path or "正文"}\n\n{body or ""}')

# 键集游标（`chunk_id > %s`）而不是「反复重查 embedding IS NULL」：失败的批**保持 NULL**，
# 重查同一条件就是死循环（永远拿回同一批）。游标只前进，失败的行落进「仍缺 K」。
KB_SEL = ("SELECT c.chunk_id,c.doc_id,d.name,c.folder_path,c.heading_path,c.text,"
          " c.embedding_text,c.embedding_recipe FROM kb.chunk c JOIN kb.doc d ON d.doc_id=c.doc_id"
          " WHERE c.embedding IS NULL AND c.embedding_recipe=%s AND c.chunk_id>%s"
          " AND d.status='chunked' AND d.enabled=true"
          " AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE)"
          " AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE)"
          " ORDER BY chunk_id LIMIT %s")
# `::vector` 显式转换：驱动对 str 参数的推断类型不一样（psycopg2 送 unknown 能隐式转，
# 送成 text 的驱动会当场报「column is of type vector but expression is of type text」）。
KB_UPD = ("UPDATE kb.chunk c SET embedding_text=%s,embedding_recipe=%s,embedding=%s::vector"
          " FROM kb.doc d WHERE c.chunk_id=%s AND c.embedding_text=%s AND c.embedding_recipe=%s"
          " AND c.embedding IS NULL AND d.doc_id=c.doc_id AND d.status='chunked' AND d.enabled=true"
          " AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE)"
          " AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE)")
KB_MISS = ("SELECT count(*) FROM kb.chunk c JOIN kb.doc d ON d.doc_id=c.doc_id"
           " WHERE (c.embedding IS NULL OR c.embedding_recipe<>%s) AND d.status='chunked' AND d.enabled=true"
           " AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE)"
           " AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE)")
KB_DOCS = ("SELECT doc_id,count(*),count(*) FILTER (WHERE embedding IS NULL OR embedding_recipe<>%s)"
           " FROM kb.chunk WHERE doc_id = ANY(%s) GROUP BY doc_id")
# `AND status = 'chunked'` 留在 SQL 里：failed / 已 embedded 的行不许被本节碰。
# error 只清那一句降级文案（其它文案可能是「表格已入知识库，建表失败」——那是真问题，不许抹）。
# 文案是 ingest.rs 的字面量，这里对不上时**只会不清**（不会误清），刻意选可失败安全的那一侧。
KB_PROMOTE = ("UPDATE kb.doc SET status = 'embedded',"
              " error = CASE WHEN error = %s THEN '' ELSE error END, updated_at = now()"
              " WHERE doc_id = ANY(%s) AND status = 'chunked' AND enabled=true"
              " AND (effective_from IS NULL OR effective_from<=CURRENT_DATE)"
              " AND (effective_to IS NULL OR effective_to>=CURRENT_DATE)")
DOWNGRADE_MSG = '向量服务不可用，稍后可重建'


def _promotable(n_chunks, n_missing):
    """能否把这份文档从 `chunked` 推到 `embedded`：**有块 且 一块不缺**。

    `n_chunks > 0` 不是废话：0 块的文档推成 embedded 就是「UI 显示已入库、其实一个字
    都检索不到」—— 正是本节要修的那个缺陷形态，不许在修它的代码里再造一遍。"""
    return n_chunks > 0 and n_missing == 0


def revec_exit(scanned, fixed, still):
    """退出码：**仍缺一行就非 0**。补了一半报成功正是这个缺陷活下来的方式
    （ingest 那边也是「记一句话然后返回 Ok」）。scanned/fixed 只进报告，不参与判定。"""
    return 1 if still else 0


def revec_chunks(cur, embed_fn=embed, batch=KB_BATCH):
    """扫空向量 → 按批补 → 按文档推进状态。返回 `(扫到, 补上, 仍缺, 推进的文档数)`。

    `embed_fn` 默认 `embed`（**文档侧** `is_query=False`，与 `EmbedClient::embed_passages`
    一致 —— 用 query 侧向量回写会让这些块和其它块不在同一个向量空间，检索时恒排在后面）。
    参数化只为 selftest：「一批向量化失败」这条路径不连库也得能验。
    **不截断正文**（`_revec` 那边截 1000 字是为注册表长描述）：ingest 时 Rust 侧原文入模型，
    这里截了就会算出和当初不同的向量；块本来 ≤480 token，模型 512 窗口自己会管。"""
    scanned = fixed = 0
    last, docs = 0, set()
    while True:
        cur.execute(KB_SEL, (KB_RECIPE, last, batch))
        rows = cur.fetchall()
        if not rows:
            break
        last = rows[-1][0]          # 游标先前进：失败的批也不许重来（否则死循环）
        scanned += len(rows)
        try:
            texts = [kb_embedding_text(name, folder, heading, body)
                     for _, _, name, folder, heading, body, _, _ in rows]
            vecs = embed_fn(texts)
        except Exception as e:
            # 保持 NULL 并让它计入「仍缺」——静默推进状态才是本节要修的缺陷
            print(f'  chunk …{last} 这批向量化失败，保持 NULL：{e}', flush=True)
            continue
        for (cid, doc, name, folder, heading, body, old_text, old_recipe), text, v in zip(rows, texts, vecs):
            cur.execute(KB_UPD, (text, KB_RECIPE, _vlit(v), cid, old_text, old_recipe))
            if cur.rowcount:
                fixed += 1
                docs.add(doc)
        print(f'  已补到 chunk {last}（{fixed}/{scanned}）', flush=True)
    promoted = _promote(cur, sorted(docs)) if docs else 0
    cur.execute(KB_MISS, (KB_RECIPE,))
    # 以库里的实际计数为准，不是 scanned - fixed：UPDATE 影响 0 行、别人并发置 NULL 都要算进来。
    # 代价（刻意选的）：正好有一次上传在「块已落、向量未回写」的窗口里时它会被算进 K → 非 0 退出。
    # 宁可多喊一次，也不要「补一半报成功」——后者正是这个缺陷的成因。
    still = cur.fetchone()[0]
    return scanned, fixed, still, promoted


def _promote(cur, docs):
    """这些文档里「一块不缺」的 → `embedded`，返回真正推进的份数。
    判据用**库里现在的计数**而不是「本次补了它几块」：并发上传/另一次 revec 都可能改动，
    只有 count 是事实源（也让重跑天然幂等 —— 已是 embedded 的被 SQL 里那条 status 挡住）。"""
    cur.execute(KB_DOCS, (KB_RECIPE, docs))
    ok = [d for d, n, miss in cur.fetchall() if _promotable(n, miss)]
    if not ok:
        return 0
    cur.execute(KB_PROMOTE, (DOWNGRADE_MSG, ok))
    return cur.rowcount


def revec():
    """`revec` / `build --revec` 的入口。返回退出码（仍缺即非 0）。"""
    import psycopg2
    pg = psycopg2.connect(connect_timeout=5, **pg_conf())
    pg.autocommit = True            # 每批即时落地：中途挂掉也不回滚已补好的（重跑接着补）
    cur = pg.cursor()
    # 卡在锁上不许无声等下去：本节是运维手动跑的，超时要响亮。
    # 口径注意：这两个只管**单条 SQL**，最慢的 embed 根本不是 SQL（一批 64 块实测 2.21s，
    # 不受 statement_timeout 约束）。实测 6 行那趟一共 10 条 SQL + 模型加载 + embed 合计 0.97s，
    # 所以 60s 对单条语句是纯兜底：真触发说明有锁或有全表扫，那时候该报错而不是等。
    cur.execute("SET statement_timeout = '60s'; SET lock_timeout = '5s'")
    try:
        scanned, fixed, still, promoted = revec_chunks(cur)
    finally:
        pg.close()
    print(f'revec：扫到 {scanned} 行缺向量 / 补上 {fixed} 行 / 仍缺 {still} 行 / '
          f'状态推进 {promoted} 份文档', flush=True)
    return revec_exit(scanned, fixed, still)


def handle_post(path, body):
    """POST 路由。未知路径按 /embed 处理（兼容原来忽略 path 的行为）"""
    if path.startswith('/parse'):
        return parse_doc(body.get('path') or '', body.get('mime') or '')
    if path.startswith('/chunk'):
        return {'chunks': chunk_blocks(body.get('blocks') or [],
                                      int(body.get('target_tokens') or TARGET_TOKENS),
                                      int(body.get('overlap') or OVERLAP))}
    texts = body.get('texts', [])
    return {'embeddings': embed(texts, is_query=bool(body.get('query', True))) if texts else []}

def serve(port=8077):
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    embedder()
    # 启动就把「哪些扩展名不可用 + 为什么」打全：这条日志是运维唯一会看的地方，
    # 只打一个 True/False 的字典等于让人自己去猜缺哪个包。
    bad = [f"{e}（{c['why']}）" for e, c in sorted(parse_caps().items()) if not c['ok']]
    print(f'embed 服务就绪 :{port}（{MODEL}, {DIM}维）解析能力 {parse_ok()}'
          + (''.join(f'\n  ⛔ {b}' for b in bad) if bad else ''), flush=True)
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a): pass
        def do_GET(self):
            # 健康检查（run.ps1 常驻化轮询用）
            if self.path == '/health':
                resp = json.dumps({'ok': True, 'model': MODEL, 'dim': DIM,
                                   'parse_ok': parse_ok(), 'parse_caps': parse_caps()},
                                  ensure_ascii=False).encode()
                self.send_response(200)
            else:
                resp = b'{"error":"not found"}'
                self.send_response(404)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(resp)))
            self.end_headers()
            self.wfile.write(resp)
        def do_POST(self):
            n = int(self.headers.get('Content-Length', 0))
            try:
                body = json.loads(self.rfile.read(n) or b'{}')
                resp = json.dumps(handle_post(self.path, body), ensure_ascii=False).encode()
                self.send_response(200)
            except ParseError as e:
                resp = json.dumps(e.payload, ensure_ascii=False).encode()
                self.send_response(e.status)
            except Exception as e:
                resp = json.dumps({'error': str(e)}, ensure_ascii=False).encode()
                self.send_response(500)
            self.send_header('Content-Type', 'application/json; charset=utf-8')
            self.send_header('Content-Length', str(len(resp)))
            self.end_headers()
            self.wfile.write(resp)
    # 🔴 ThreadingHTTPServer，不是 HTTPServer：stdlib 的 `socketserver.TCPServer` **串行**处理，
    # 而 /parse（上限 120s）/chunk /embed 全在同一个 handler 里。于是一次文档上传期间，
    # 每个问句的 embed 请求排在它后面 → 各自撞客户端 3s 超时
    # （`connector/src/embed.rs` TIMEOUT_SECS）→ send 失败把**进程级共享**的熔断打到 300s
    # → 之后 5 分钟语义缓存整条不生效、KB 向量路降级、表召回向量路降级，**全部静默**。
    # /health 探针也一起被堵（容器编排读成「服务挂了」）。
    # 同一件事 `docker/parser/parse_service.py:142` 早就修了，宿主这份漏了 —— 一份实现两个入口，
    # 只修一个入口等于没修。判据：`_selftest_serve_unblocked`（换回 HTTPServer 立刻红）。
    # 模型推理仍然串行（`_EMBED_LOCK`），并发的只有 IO 与解析。
    ThreadingHTTPServer(('127.0.0.1', port), H).serve_forever()

def selftest():
    """parse+chunk 自检：只用标准库（md/csv/json/html 四类），不依赖任何解析库与模型。
    末尾把**每种扩展名**的可用/不可用列全 —— 本机（Smart App Control 拦编译产物）预期
    pdf/docx/pptx/图片全是「不可用」，**那是正确输出**：判据是「不可用时明确说不可用」，
    不是「本机必须全可用」。真解析质量在容器里验（`tools/parse_probe.py`）。"""
    d = tempfile.mkdtemp()
    md = os.path.join(d, 'a.md')
    with open(md, 'w', encoding='utf-8') as f:
        f.write('# 一级标题\n开头一段。\n\n## 二级标题\n'
                + ''.join(f'第{i}句正文内容。' for i in range(200)))
    r = parse_doc(md)
    assert [b['heading_path'] for b in r['blocks']] == \
        ['一级标题', '一级标题', '一级标题 > 二级标题', '一级标题 > 二级标题'], r['blocks']
    ch = chunk_blocks(r['blocks'])
    assert len(ch) > 2 and all(c['tokens'] <= MAX_TOKENS for c in ch), ch
    assert ch[0]['heading_path'] == '一级标题' and ch[-1]['heading_path'] == '一级标题 > 二级标题'
    oc = int(OVERLAP * CHARS_PER_TOKEN)
    assert ch[2]['text'].startswith(ch[1]['text'][-oc:]), '同段内块间重叠丢了'
    cv = os.path.join(d, 'b.csv')
    with open(cv, 'w', encoding='utf-8') as f:
        f.write('客户,金额\n甲,1\n乙,2\n')
    r2 = parse_doc(cv)
    assert r2 == {'blocks': [], 'page_count': 0,
                  'sheets': [{'name': 'b', 'header': ['客户', '金额'],
                              'rows': [['甲', '1'], ['乙', '2']]}]}, r2
    # json：合法 → ```json 美化代码块；非法 → 原文入库且 notes 留痕（不许静默降级）
    js = os.path.join(d, 'c.json')
    with open(js, 'w', encoding='utf-8') as f:
        f.write('{"name": "甲", "tags": ["a", "b"]}')
    r3 = parse_doc(js)
    assert r3['blocks'][0]['text'].startswith('```json'), r3
    assert '"name": "甲"' in r3['blocks'][0]['text'], r3
    bad = os.path.join(d, 'bad.json')
    with open(bad, 'w', encoding='utf-8') as f:
        f.write('{not json')
    r4 = parse_doc(bad)
    assert r4['blocks'] and r4.get('notes') and 'JSON 校验失败' in r4['notes'][0], r4
    # html：去标签留文本；script/style 整棵丢弃；实体反转义
    ht = os.path.join(d, 'e.html')
    with open(ht, 'w', encoding='utf-8') as f:
        f.write('<html><head><title>制度</title><style>.a{color:red}</style></head>'
                '<body><h1>报销</h1><p>上限 &amp; 3000</p><script>alert(1)</script></body></html>')
    r5 = parse_doc(ht)
    joined = '\n'.join(b['text'] for b in r5['blocks'])
    assert '上限 & 3000' in joined and '报销' in joined, r5
    assert 'alert' not in joined and 'color' not in joined and '<p>' not in joined, r5
    open(os.path.join(d, 'x.rar'), 'wb').close()      # 真的没登记的扩展名
    for path, err in ((os.path.join(d, 'x.rar'), 'unsupported'), (os.path.join(d, 'nope.md'), 'not_found')):
        try:
            parse_doc(path)
            raise AssertionError(f'{path} 应该报 {err}')
        except ParseError as e:
            assert e.payload['error'] == err, e.payload
    caps = _selftest_caps(d)
    _selftest_pages()
    _selftest_pdf_scan(d)
    _selftest_revec()
    dt = _selftest_serve_unblocked()
    print(f'selftest ok: md块={len(r["blocks"])} 分块={len(ch)} tokens={[c["tokens"] for c in ch]}'
          f'（含 revec 纯逻辑 + 能力表双向一致 + /embed 忙 2s 时 /health {dt * 1000:.0f}ms 返回）'
          f'\n  parse_ok={parse_ok()}', flush=True)
    for ext, c in sorted(caps.items()):
        print(f"  {'✅' if c['ok'] else '⛔'} {ext:<10}{c['why']}", flush=True)


def _selftest_pdf_scan(tmpdir):
    """扫描件「OCR 档」（Y1）的钉。四段，每段对应一种不许回去的形态：
    ① 阈值纯逻辑：页均 <50 或全文 <200 判扫描件（裁决阈值，环境变量可覆盖 —— 改默认先改钉）
    ② 补页编排：桩 OCR 驱动 `_pdf_ocr_fill` —— 成功补页/单页失败留痕/整份响亮失败/页数护栏
    ③ 真实夹具：造一份**无文本** PDF，断言降级链不炸（确定性 ParseError，不是 500/静默返空）；
       fitz 环境再造一份**有墨图像页** PDF，真跑一次 OCR 档
    ④ 两档自报：`parse_caps()['.pdf']` 必须带 text|ocr 两档，且与真实可用性同源"""
    # ① 阈值钉
    assert _pdf_is_scanned(0, 3) and _pdf_is_scanned(199, 3)          # 全文 < 200
    assert _pdf_is_scanned(400, 10)                                   # 页均 40 < 50（全文 ≥200 也算）
    assert not _pdf_is_scanned(200, 3) and not _pdf_is_scanned(600, 3)
    assert not _pdf_is_scanned(0, 0), '0 页退化输入不许误判扫描件'
    assert _pdf_low_text_pages({1: 0, 2: 49, 3: 50, 4: 60}, 4) == [1, 2]
    assert _pdf_low_text_pages({}, 2) == [1, 2]                       # 缺席页按 0 计
    assert _page_chars(' 第 一 章　') == 3                            # 口径：非空白字符（含全角空格）
    # ② 补页编排（桩 OCR，不碰文件/引擎）
    ok = lambda p, i: [{'text': f'第{i}页内容' * 20, 'page': i, 'heading_path': f'第 {i} 页（OCR）'}]
    def bad(p, i):
        raise ParseError('unsupported', 'OCR 引擎缺席')
    empty = lambda p, i: []
    # 混合件：第 2 页低文本 → OCR 补上、按页号排回、notes 留痕
    out = [{'text': '正' * 400, 'page': 1, 'heading_path': ''}]
    notes = _pdf_ocr_fill('x.pdf', 2, {1: 400, 2: 0}, out, ocr_fn=ok)
    assert [b['page'] for b in out] == [1, 2] and notes and 'OCR 补' in notes[0], (out, notes)
    # 单页 OCR 失败不炸整份：原文保留 + 留痕
    out = [{'text': '正' * 400, 'page': 1, 'heading_path': ''}]
    notes = _pdf_ocr_fill('x.pdf', 2, {1: 400, 2: 0}, out, ocr_fn=bad)
    assert len(out) == 1 and notes and 'OCR 未成' in notes[-1], (out, notes)
    # 整份扫描件 + OCR 一字未补 → no_text_layer（不许带零星字符静默入库）；
    # 全文 <200 的「低文本量真文本」（3×60=180）同口径 —— 裁决阈值钉死，不许松
    for chars in ({1: 0, 2: 10}, {1: 60, 2: 60, 3: 60}):
        out = [{'text': '残' * c, 'page': i, 'heading_path': ''} for i, c in chars.items() if c]
        try:
            _pdf_ocr_fill('x.pdf', len(chars), chars, out, ocr_fn=bad)
            raise AssertionError(f'{chars} 应判扫描件报 no_text_layer')
        except ParseError as e:
            assert e.payload['error'] == 'no_text_layer', e.payload
    # 页均腿判扫描但全文 ≥200：OCR 全败时**保留**文本层 + 留痕（不升级为整份失败）
    chars = {i: 40 for i in range(1, 11)}
    out = [{'text': '残' * 40, 'page': i, 'heading_path': ''} for i in chars]
    notes = _pdf_ocr_fill('x.pdf', 10, chars, out, ocr_fn=bad)
    assert len(out) == 10 and notes and 'OCR 未成' in notes[-1], (len(out), notes)
    # 页数护栏：低文本页 > cap → too_large（不「OCR 前 N 页然后报已入库」）
    try:
        _pdf_ocr_fill('x.pdf', 5, {i: 0 for i in range(1, 6)}, [], ocr_fn=ok, cap=2)
        raise AssertionError('超 cap 应报 too_large')
    except ParseError as e:
        assert e.payload['error'] == 'too_large', e.payload
    # ocr_fn 返空按失败记（引擎读不出字不算「已补」），整份扫描件同样响亮失败
    try:
        _pdf_ocr_fill('x.pdf', 1, {1: 0}, [], ocr_fn=empty)
        raise AssertionError('OCR 返空应报 no_text_layer')
    except ParseError as e:
        assert e.payload['error'] == 'no_text_layer', e.payload
    # ③ 真实夹具：无文本 PDF（pypdf/fitz 有哪个用哪个；都没有 = pdf 能力本身不可用，跳过）
    fixture = os.path.join(tmpdir, 'scan_blank.pdf')
    made = False
    if _have('pypdf'):
        from pypdf import PdfWriter
        w = PdfWriter()
        w.add_blank_page(612, 792)
        w.add_blank_page(612, 792)
        with open(fixture, 'wb') as f:
            w.write(f)
        made = True
    elif _have('fitz'):
        import fitz
        d0 = fitz.open()
        d0.new_page()
        d0.new_page()
        d0.save(fixture)
        d0.close()
        made = True
    if made:
        try:
            r = parse_doc(fixture)
            # 空白页连 OCR 也读不出字 —— 能 200 只有一种解释：引擎把空白「读」出了内容。
            # 那时至少必须走了 OCR 档且留痕（不许返空块 —— 静默返空族）
            assert ''.join(b['text'] for b in r['blocks']).strip() and r.get('notes'), r
        except ParseError as e:
            # 本机形态（pypdf 兜底 + 无 OCR 引擎）：确定性失败 —— 不炸、不静默
            assert e.payload['error'] in ('no_text_layer', 'too_large'), e.payload
    # ③b 有墨的图像页夹具（仅 fitz 环境可造可验）：先排一版文字页，渲成 pixmap 再整页塞图，
    # 零第三方库造出「无文本层、图上有字」的真扫描件形态，真跑一次 OCR 档
    if _have('fitz'):
        import fitz
        src = fitz.open()
        tp = src.new_page()
        tp.insert_text((72, 120), 'DMS-SCAN-FIXTURE 7788', fontsize=28)
        pix = tp.get_pixmap(dpi=150)
        doc = fitz.open()
        ip = doc.new_page(width=tp.rect.width, height=tp.rect.height)
        ip.insert_image(ip.rect, pixmap=pix)
        img_pdf = os.path.join(tmpdir, 'scan_image.pdf')
        doc.save(img_pdf)
        doc.close()
        src.close()
        try:
            r = parse_doc(img_pdf)
            # OCR 引擎在：必须读出夹具暗号，且 notes 留痕「已用 OCR 补」
            assert any('7788' in b['text'] for b in r['blocks']), r
            assert any('OCR' in b.get('heading_path', '') for b in r['blocks']), r
            assert r.get('notes'), r
        except ParseError as e:
            # OCR 引擎缺席：no_text_layer 是确定性失败 —— 不炸、不静默返空
            assert e.payload['error'] == 'no_text_layer', e.payload
    # ④ 两档自报
    caps = parse_caps()
    if caps['.pdf']['ok']:
        t = caps['.pdf'].get('tiers')
        assert t and set(t) == {'text', 'ocr'}, caps['.pdf']
        assert t['text'] in ('pymupdf4llm', 'fitz', 'pypdf'), t
        assert t['ocr'] in ('qwen', 'tesseract', None), t
        if not _have('fitz'):
            assert t['ocr'] is None, 'fitz 缺席还上报 OCR 档可用 = 上报与真解析两套口径'
        assert 'OCR' in caps['.pdf']['why'], caps['.pdf']


def _selftest_caps(tmpdir):
    """能力表纪律。四条断言，每条都对应一种已经发生过的静默失败：
    ① 扩展名表与解析器表**双向一致** —— 「登记而不消费」（声明支持却没有实现者）与
       「实现了却没登记」（写了解析器但 /parse 永远走不到）本轮各抓到过一次
    ② MIME 表只许映到登记过的扩展名（映到没登记的 = 按 mime 上传时莫名 unsupported）
    ③ **上报不可用的扩展名，真去解析必须抛 `unsupported` 且带原因** —— 静默返空是本仓
       反复抓的失败族（status=embedded + chunk 0 + 界面「已入库」+ 问不出东西）
    ④ `parse_ok` 里 `pdf`/`xlsx` 两个键在：`parse_probe.py` 靠它们决定跳不跳，
       键改名它会静默全跳过并 exit 0（判据恒绿）"""
    impl = {n: f for n, f in globals().items() if n.startswith('_p_') and callable(f)}
    reg = {f for f, _, _ in CAPS.values()}
    assert set(impl.values()) == reg, (
        f'解析器登记不一致：写了却没登记 {sorted(n for n, f in impl.items() if f not in reg)}'
        f' / 登记的不是模块级 _p_* 函数 {sorted(e for e, (f, _, _) in CAPS.items() if f not in impl.values())}')
    assert set(MIME_EXT.values()) <= set(CAPS), sorted(set(MIME_EXT.values()) - set(CAPS))
    assert {'pdf', 'xlsx'} <= set(parse_ok()), parse_ok()
    caps = parse_caps()
    assert set(caps) == set(CAPS), '能力上报漏了扩展名'
    for ext, c in caps.items():
        if c['ok']:
            continue
        p = os.path.join(tmpdir, 'cap' + ext)   # 0 字节即可：门在依赖探测，轮不到读内容
        open(p, 'wb').close()
        try:
            got = parse_doc(p)
        except ParseError as e:
            assert e.payload['error'] == 'unsupported' and e.payload.get('detail'), (ext, e.payload)
            continue
        raise AssertionError(f'{ext} 上报不可用，解析却返回了 {got} —— 静默返空族')
    return caps


class _FakeCur:
    """selftest 的假游标：按 SQL **常量身份**（`is`）分派，不解析 SQL 文本。
    好处是 SQL 改了字它不会假装还在测同一条路 —— 认不出来的语句当场 AssertionError。"""

    def __init__(self, batches, groups, missing):
        self.batches, self.groups, self.missing = list(batches), groups, missing
        self.rows, self.cursors, self.updated, self.promoted = [], [], [], []
        self.rowcount = 0

    def execute(self, sql, params=()):
        self.rows = []
        if sql is KB_SEL:
            self.cursors.append(params[1])
            self.rows = self.batches.pop(0) if self.batches else []
        elif sql is KB_UPD:
            self.updated.append(params[3])
            self.rowcount = 1
        elif sql is KB_DOCS:
            self.rows = self.groups
        elif sql is KB_MISS:
            self.rows = [(self.missing,)]
        elif sql is KB_PROMOTE:
            self.promoted = list(params[1])
            self.rowcount = len(self.promoted)
        else:
            raise AssertionError(f'假游标不认识这条 SQL：{sql}')

    def fetchall(self):
        return self.rows

    def fetchone(self):
        return self.rows[0] if self.rows else None


def _selftest_revec():
    """revec 纯逻辑自检（不连库、不加载模型）。三条都是这个缺陷的实际形状：
    ① 一批向量化失败 → 那些行保持 NULL、计入「仍缺」，且**游标仍要前进**（不然死循环）
    ② 状态推进的条件是「有块且一块不缺」，不是「本次补过它」
    ③ 仍缺一行就非 0 退出（补一半报成功 = 复刻缺陷）"""
    def flaky(texts):
        if len(texts) == 1:            # 第二批只剩一行时假装向量服务抖了
            raise RuntimeError('模拟向量服务抖动')
        return [[0.5] * DIM] * len(texts)

    cur = _FakeCur(batches=[[
            (1, 'd1', 'a.md', '/制度', '甲', 'a', 'old-a', KB_RECIPE),
            (2, 'd1', 'a.md', '/制度', '乙', 'b', 'old-b', KB_RECIPE)],
            [(3, 'd2', 'b.md', '/', '', 'c', 'old-c', KB_RECIPE)]],
                   groups=[('d1', 2, 0), ('d2', 1, 1)], missing=1)
    got = revec_chunks(cur, flaky, batch=2)
    assert got == (3, 2, 1, 1), got                     # 扫 3 / 补 2 / 仍缺 1 / 推进 1
    assert cur.updated == [1, 2], cur.updated           # 失败那行没被回写
    assert cur.cursors == [0, 2, 3], cur.cursors        # 游标越过失败批：0→2→3 后取空退出
    assert cur.promoted == ['d1'], cur.promoted         # d2 还缺一块 → 不许推进
    assert revec_exit(*got[:3]) == 1, '仍缺 1 行还给 0 退出码 = 补一半报成功'
    assert revec_exit(0, 0, 0) == 0 and revec_exit(9, 9, 0) == 0
    assert not _promotable(0, 0), '0 块的文档不许推成 embedded'
    assert _promotable(3, 0) and not _promotable(3, 1)
    assert kb_embedding_text('制度.md', '/财务/报销', '第一章 > 范围', '正文') == \
        '文件：制度.md\n目录：/财务/报销\n章节：第一章 > 范围\n\n正文'

def _selftest_serve_unblocked():
    """**真起一次 serve**：/embed 慢的时候 /health 必须仍在 500ms 内返回。

    这条判据钉的是一个已经发生过的回归：同一件事在 `docker/parser/parse_service.py` 修过
    （ThreadingHTTPServer），宿主这份漏了。后果不是「慢」而是**静默错答** ——
    一次上传把每个问句的 embed 堵到 3s 超时，客户端进程级熔断 300s，
    之后 5 分钟语义缓存/KB 向量路/表召回向量路全降级且零报错。

    ⚠️ 为什么不写成「源码里有 ThreadingHTTPServer」那种断言：本仓刚被同一形态咬过一次 ——
    `check-arch.ps1` 的 reqwest 那条把**注释里的字**当真实用点。注释里提一句
    ThreadingHTTPServer、代码里照旧 `HTTPServer(...)`，字符串断言会绿。所以这里量的是墙钟。

    不加载模型（patch 掉 `embedder`/`embed`）：selftest 的纪律是不碰第三方库与 95MB 模型。"""
    import socket
    import time
    import urllib.request
    g = globals()
    keep = g['embed'], g['embedder']
    # 桩睡 2s。**别调小**：断言在 /embed 发出后 0.15s 才开始计时，剩余时间必须仍远超 500ms 阈值。
    # 第一版写 0.6s，单线程时剩 0.45s < 0.5s ⇒ 反向验证时这条判据是**绿的**（恒真）。
    # 修好后这 2s 不进 selftest 的墙钟：/health 立刻返回，embed 桩在 daemon 线程里被丢下。
    g['embed'] = lambda texts, is_query=False: (time.sleep(2), [[0.0] * DIM] * len(texts))[1]
    g['embedder'] = lambda: None
    with socket.socket() as s:                 # 借一个空闲端口：别撞常驻的 8077
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
    base = f'http://127.0.0.1:{port}'

    def get_health(timeout=5):
        with urllib.request.urlopen(base + '/health', timeout=timeout) as r:
            return json.loads(r.read())

    try:
        threading.Thread(target=serve, args=(port,), daemon=True).start()
        for _ in range(50):                    # 等监听就绪（bind 之后到 accept 之前有个窗口）
            try:
                assert get_health()['ok']
                break
            except Exception:
                time.sleep(0.1)
        else:
            raise AssertionError(f'serve 没能在 5s 内起在 {port}')
        threading.Thread(target=lambda: urllib.request.urlopen(
            urllib.request.Request(base + '/embed', json.dumps({'texts': ['a']}).encode(),
                                   {'Content-Type': 'application/json'}), timeout=10),
            daemon=True).start()
        time.sleep(0.15)                       # 让 /embed 先进 handler 睡着
        t0 = time.time()
        assert get_health()['ok']
        dt = time.time() - t0
        assert dt < 0.5, (f'/embed 睡 2s 期间 /health 等了 {dt:.3f}s —— serve 又变回单线程了；'
                          '一次上传会把每个问句的 embed 顶到超时 + 300s 熔断')
        return dt
    finally:
        g['embed'], g['embedder'] = keep


if __name__ == '__main__':
    import argparse
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('mode', nargs='?', default='serve',
                    choices=('build', 'revec', 'serve', 'selftest'))
    ap.add_argument('port', nargs='?', type=int, default=8077, help='serve 端口（默认 8077）')
    ap.add_argument('--ds', default='dms',
                    help='build 只处理该 ds_id 的注册表行（对应 Rust 侧 meta::DS_PRED，默认 dms）')
    ap.add_argument('--revec', action='store_true',
                    help='只跑第五个目标：补 kb.chunk 空向量 + 推进 kb.doc 状态（= mode revec）')
    a = ap.parse_args()
    if a.revec or a.mode == 'revec':
        sys.exit(revec())      # 仍缺即非 0：判定在 revec_exit，别在这里改成恒 0
    elif a.mode == 'build':
        build(a.ds)
    elif a.mode == 'selftest':
        selftest()
    else:
        serve(a.port)
