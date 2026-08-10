"""从 DMS 后端 mapper XML 挖 JOIN 关系（源码只读，本脚本只读不写）。
输出：候选 join 边（表.列对）+ 出现次数 + 来源文件，供基数探测与人工裁决。"""
import re, sys, json, glob, os
from collections import Counter, defaultdict

ROOT = r"D:\code\dms\xh-dms"
JOIN_RE = re.compile(
    r"(?i)\b(LEFT|RIGHT|INNER)?\s*JOIN\s+([\w`]+)\s+(?:AS\s+)?(\w+)\s+ON\s+([\s\S]+?)(?=(?:\b(?:LEFT|RIGHT|INNER|FULL)\b[^;]{0,4}\bJOIN\b|\bWHERE\b|\bGROUP\b|\bORDER\b|\bLIMIT\b|$))"
)
COND_RE = re.compile(r"(\w+)\.(\w+)\s*=\s*(\w+)\.(\w+)")
FROM_RE = re.compile(r"(?i)\bFROM\s+([\w`]+)\s+(?:AS\s+)?(\w+)")

edges = Counter()
sources = defaultdict(set)
files = glob.glob(os.path.join(ROOT, "**/*.xml"), recursive=True)
for f in files:
    try:
        text = open(f, encoding="utf-8", errors="ignore").read()
    except Exception:
        continue
    text = re.sub(r"<!--[\s\S]*?-->", "", text)
    # 每个 <select>/<update> 块独立解析别名表
    for m in JOIN_RE.finditer(text):
        table2, alias2, on = m.group(2).strip("`"), m.group(3), m.group(4)
        # 主表别名：JOIN 之前最近的 FROM
        pre = text[: m.start()]
        froms = FROM_RE.findall(pre)
        if not froms:
            continue
        table1, alias1 = froms[-1][0].strip("`"), froms[-1][1]
        amap = {alias1: table1, alias2: table2}
        # 已建别名累积（多 JOIN 链）
        for pm in JOIN_RE.finditer(pre):
            amap[pm.group(3)] = pm.group(2).strip("`")
        for c in COND_RE.finditer(on):
            a1, col1, a2, col2 = c.groups()
            t1, t2 = amap.get(a1), amap.get(a2)
            if not t1 or not t2 or t1 == t2 and col1 == col2:
                continue
            key = tuple(sorted([(t1, col1), (t2, col2)]))
            edges[key] += 1
            sources[key].add(os.path.basename(f))

out = [
    {"t1": k[0][0], "c1": k[0][1], "t2": k[1][0], "c2": k[1][1], "n": v,
     "src": sorted(sources[k])[:3]}
    for k, v in edges.most_common()
]
json.dump(out, open(r"D:\code\dms_ai\_mined_joins.json", "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print(f"{len(files)} xml -> {len(out)} 条候选边")
for e in out[:30]:
    print(f"{e['n']:3d}  {e['t1']}.{e['c1']} = {e['t2']}.{e['c2']}   {e['src'][0]}")
