"""【K1】文档解析器端到端验收：xlsx / pdf 各造一份自己的夹具，走 doc 服务的 `/parse`。

为什么要它：`GET /health` 的 `parse_ok` 只说「依赖能不能 import」，说不了「解析出来的东西对不对」。
而两者会脱节 —— `parse_ok` 本来用 `find_spec` 判可用，那只查包在不在：装了 `python-docx`
但它的 `lxml` 编译扩展被本机 SAC 拦掉时，它照样报 true（已改成真 import 一次）。

判据（缺一即退出码非 0）：
 ① `.pdf` → `blocks` 里有正文。**PDF 的依赖分三级**：`pymupdf4llm`/`fitz`（AGPL-3.0，保标题层级）
    → `pypdf`（BSD-3，逐页纯文本）。本判据只要求「能出正文」，不要求装到哪一级。
 ② `.xlsx` → **数据在 `sheets` 不在 `blocks`**（表格走结构化通道，markdown 由 Rust 侧
    `tabular::sheet_blocks` 渲染）。拿 `blocks` 判 xlsx 会永远红，那是判据写错不是功能坏。
 ③ 空 sheet **必须出现在 `sheets` 里**（空表头 + 空行）。它曾在 Python 侧被 `return None` 丢掉，
    于是 Rust 的 `TabularSource.skipped` 永远看不到它 —— 而那条契约写的是
    「不建零列表，但**不能静默**」。两个 sheet 的 xlsx 只回 1 个、另一个无声消失。

用法：`python -u tools/parse_probe.py`（doc 服务需已起在 8077）。
"""
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

sys.stdout.reconfigure(encoding="utf-8")
BASE = os.environ.get("PARSE_PROBE_BASE", "http://127.0.0.1:8077")
# 夹具输出目录。**必须可覆盖**，两个实测理由：
# ① 容器里 `tools/` 常挂成 `:ro`（解析服务不该写仓库）→ 写这里是 `OSError: Read-only file system`；
# ② 解析服务的 `guard_path` 只许读 `PARSE_ROOTS`（默认 `/kbdata:/tmp`）之内的文件 ——
#    夹具落在 `tools/kb_fixtures/` 会被 **403 forbidden** 拦掉（实测三条判据全红）。
#    那道守卫是对的（`/parse` 曾是无鉴权任意文件读），**不该为了让探针跑通去放宽它** ——
#    把 `/app/tools` 加进允许根就等于把源码与 settings 又读回来了。
OUT = Path(os.environ.get("PARSE_PROBE_OUT") or (Path(__file__).resolve().parent / "kb_fixtures"))


def make_xlsx(p):
    import openpyxl
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "报销标准"
    for row in [["项目", "上限", "备注"], ["市内交通", 200, "凭票"],
                ["住宿", 500, "一线城市 800"], ["招待费", 0, "无金额下限但需总监审批"]]:
        ws.append(row)
    wb.create_sheet("空表")          # 判据③
    wb.save(p)


def make_pdf(p):
    """最小单页 PDF，xref 偏移**真算**（不靠解析器容错重建 —— 那样测的是容错不是解析）。
    正文用 ASCII：base-14 字体下中文抽不出来，那是字体问题不是解析器问题。"""
    content = b"BT /F1 24 Tf 72 700 Td (PDF PARSE OK 12345) Tj ET\n"
    objs = [
        b"<</Type/Catalog/Pages 2 0 R>>",
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>",
        b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R"
        b"/Resources<</Font<</F1 5 0 R>>>>>>",
        b"<</Length %d>>stream\n" % len(content) + content + b"endstream",
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>",
    ]
    buf, offsets = bytearray(b"%PDF-1.4\n"), []
    for i, o in enumerate(objs, 1):
        offsets.append(len(buf))
        buf += b"%d 0 obj" % i + o + b"endobj\n"
    xref_at = len(buf)
    buf += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    for off in offsets:
        buf += b"%010d 00000 n \n" % off
    buf += b"trailer<</Size %d/Root 1 0 R>>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref_at)
    p.write_bytes(bytes(buf))


def parse(p):
    r = urllib.request.Request(BASE + "/parse", data=json.dumps({"path": str(p)}).encode(),
                               method="POST", headers={"Content-Type": "application/json"})
    try:
        return json.loads(urllib.request.urlopen(r, timeout=120).read().decode())
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.read().decode()[:300]}"}


h = json.loads(urllib.request.urlopen(BASE + "/health", timeout=20).read().decode())
ok = h.get("parse_ok") or {}
print(f"parse_ok = {json.dumps(ok, ensure_ascii=False)}")

OUT.mkdir(parents=True, exist_ok=True)
bad = []

# ── ① pdf ──
if not ok.get("pdf"):
    print("  ⏭️ pdf 依赖缺席（装 pypdf 即可，BSD-3）")
else:
    p = OUT / "_probe.pdf"
    make_pdf(p)
    j = parse(p)
    txt = "".join(b.get("text", "") for b in (j.get("blocks") or []))
    hit = "PDF PARSE OK" in txt
    print(f"  {'✅' if hit else '✗'} pdf: {len(j.get('blocks') or [])} 块 {len(txt)} 字")
    if not hit:
        bad.append(f"pdf: {j.get('error') or txt[:150]!r}")

# ── ②③ xlsx ──
if not ok.get("xlsx"):
    print("  ⏭️ xlsx 依赖缺席（装 openpyxl 即可，MIT）")
else:
    p = OUT / "_probe.xlsx"
    make_xlsx(p)
    j = parse(p)
    sheets = j.get("sheets") or []
    names = [s.get("name") for s in sheets]
    cells = "".join(str(c) for s in sheets for r in (s.get("rows") or []) for c in r)
    has_data = "招待费" in cells
    has_empty = any(s.get("name") == "空表" and not s.get("header") and not s.get("rows")
                    for s in sheets)
    print(f"  {'✅' if has_data else '✗'} xlsx 数据在 sheets: {names}")
    print(f"  {'✅' if has_empty else '✗'} 空 sheet 被报出来（不静默）")
    if not has_data:
        bad.append(f"xlsx 数据: {j.get('error') or cells[:150]!r}")
    if not has_empty:
        bad.append("空 sheet 没被报出来 → Rust 侧 skipped 永远看不到它")

# 夹具自清理：它们与 kb_eval 的正式语料同住 `tools/kb_fixtures/`，留着会让
# 「哪些是正式语料」看不清（kb_eval 只上传 spec 里列的，所以留着不会被误上传，
# 但目录本身是给人看的）。清理失败只提示，不改判据成败。
for p in (OUT / "_probe.pdf", OUT / "_probe.xlsx"):
    try:
        p.unlink(missing_ok=True)
    except OSError as e:
        print(f"  ⚠ 未能清理 {p.name}：{e}")

for b in bad:
    print(f"  ✗ {b}")
sys.exit(1 if bad else 0)
