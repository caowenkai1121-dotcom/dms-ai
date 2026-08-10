"""核 `docs/INTEGRATION-TRACE.md` 自己：它引用的符号/路径在仓库里是否真的存在。

这份矩阵是回答「SuperSonic / deepagents / SQLBot 三个项目的哪些机制进来了、落在哪个符号上、
由哪条判据证明」的唯一凭据。它最容易出的问题是**静默腐烂**：
声明还在、引用的函数早改名或删了 —— 而读者无从分辨「已落地」是真的还是过期的。
这与本仓反复抓到的那类缺陷同形（判据的输入没了，断言恒真），故用同一条纪律核它：
**引用必须能落到代码上。**

用法：`python -u tools/audit_trace.py`（退出码非 0 = 有引用失效）
顺带打印按来源项目的行数分布 —— 那是「整合了多少」这个问题的可核对答案。

⚠️ 本机 SAC 拦掉了 `rg` 的进程创建（与容器化那条裁决同一个拦截器），故用纯 Python 扫。
"""
import re
import sys
from collections import Counter
from pathlib import Path

sys.stdout.reconfigure(encoding="utf-8")
ROOT = Path(__file__).resolve().parents[1]
TRACE = ROOT / "docs/INTEGRATION-TRACE.md"

if not TRACE.exists():
    print(f"✗ 找不到 {TRACE}")
    sys.exit(1)

text = TRACE.read_text(encoding="utf-8")
rows = [
    l for l in text.splitlines()
    if l.startswith("|") and l.count("|") >= 4 and "---" not in l
]

# 来源项目分布：矩阵按章节分组，这里用行文里出现的项目名粗算
srcs = Counter()
for name in ("SuperSonic", "supersonic", "deepagents", "DeepAgents", "SQLBot", "sqlbot"):
    srcs[name.lower()] += text.count(name)

SKIP_STATUS = ("待做", "拒抄")
print(f"表格行数 {len(rows)}（其中待做/拒抄 {sum(1 for r in rows if any(s in r for s in SKIP_STATUS))}）")
print("来源项目提及：" + "  ".join(f"{k}={v}" for k, v in srcs.items() if v))

# 一次读入全部源码
SRC: list[str] = []
for d in ("crates", "tools", "scripts", "docker", "web/src"):
    p0 = ROOT / d
    if not p0.exists():
        continue
    for p in p0.rglob("*"):
        if not p.is_file() or "target" in p.parts:
            continue
        if p.suffix in {".rs", ".py", ".ps1", ".toml", ".md", ".json", ".vue", ".ts"}:
            try:
                SRC.append(p.read_text(encoding="utf-8", errors="ignore"))
            except OSError:
                pass
print(f"扫描源文件 {len(SRC)} 个")

CITE = re.compile(r"`([A-Za-z0-9_][A-Za-z0-9_:./\-]*)`")
cites: dict[str, int] = {}
for i, l in enumerate(rows):
    for m in CITE.finditer(l):
        s = m.group(1)
        # 只核「像符号/路径」的：含 :: 或 / 或以源码后缀结尾
        if "::" in s or "/" in s or s.endswith((".rs", ".py", ".ps1")):
            cites.setdefault(s, i)


def exists(sym: str) -> bool:
    if "/" in sym and "::" not in sym:
        if (ROOT / sym).exists() or (ROOT / "crates" / sym).exists():
            return True
        return bool(list(ROOT.rglob(sym.split("/")[-1])))
    ident = sym.split("::")[-1].split("/")[-1]
    if ident.endswith((".rs", ".py", ".ps1")):
        return bool(list(ROOT.rglob(ident)))
    if len(ident) < 3:
        return True  # 太短不判：满仓命中，判了也没信息
    return any(ident in blob for blob in SRC)


missing = [s for s in sorted(cites) if not exists(s)]
print(f"符号/路径引用 {len(cites)} 个，失效 {len(missing)} 个")
for s in missing:
    print(f"  ✗ {s}\n      行: {rows[cites[s]][:100]}")
sys.exit(1 if missing else 0)
