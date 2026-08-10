"""枚举归属分析器（数据驱动对拍，AX7 裁决的落地）：
Java 枚举（xh-dms 源码只读）× 列实际取值（非 DMS 分析目标）→ cov 判归属。

判据（seed_defs 注释同一份，改哪边都要对账）：
  ① cov = |列取值 ∩ 枚举码| / |列取值| >= 0.8（分母是**列**的取值）
  ② 枚举类名词干 ∩ 表名/列名 非空（防 '01'-'05' 短码空间巧合）
  ③ 列实际取值数 >= 2
cov == 1.0 ⇒ origin='dict'（RequireKnownValue 可开火）；0.8~1.0 ⇒ 'probe'（不开火）；<0.8 ⇒ 不归属。

用法：python tools/enum_ownership.py [--apply]（--apply 才写 meta.value_maps，默认只出报告）
"""
import re, sys, os, json, glob, argparse
sys.path.insert(0, os.path.dirname(__file__))
from settings import analysis_mysql_kwargs, pg_kwargs

XH = r"D:\code\dms\xh-dms"
PAIR = re.compile(r'\(\s*(?:"([^"]+)"|(\d+))\s*,\s*"([^"]+)"\s*\)')
ENUM_DECL = re.compile(r"public\s+enum\s+(\w+)")
# 候选列名后缀（seed_defs 的码列家族同一份）
SUFFIX = ("_code", "_status", "_type", "_flag", "_level", "_kind", "_class", "_channel", "_state", "_mode", "_category", "_source")
# 已在 meta.value_maps 登记的（.table.column）—— 这些跳过（不重复归属）

def java_enums():
    out = {}
    for f in glob.glob(os.path.join(XH, "**/*.java"), recursive=True):
        text = open(f, encoding="utf-8", errors="ignore").read()
        m = ENUM_DECL.search(text)
        if not m:
            continue
        name = m.group(1)
        # 去注释掉的行（`//` 开头的行不参与）
        live = "\n".join(l for l in text.splitlines() if not l.strip().startswith("//"))
        pairs = {}
        for str_code, int_code, desc in PAIR.findall(live):
            pairs[str_code or int_code] = desc
        if len(pairs) >= 2:
            out[name] = pairs
    return out

def candidate_columns(cur):
    cur.execute(
        "SELECT table_name, column_name FROM information_schema.columns "
        "WHERE table_schema = DATABASE() AND table_name LIKE 't\\_%' AND table_name NOT LIKE '%bak%'"
    )
    cols = [(t, c) for t, c in cur.fetchall() if c.lower().endswith(SUFFIX)]
    return cols

def registered(pgcur):
    pgcur.execute("SELECT table_name, column_name FROM meta.value_map")
    return {(t, c) for t, c in pgcur.fetchall()}

def col_values(cur, t, c, cap=400):
    cur.execute(f"SELECT DISTINCT `{c}` FROM `{t}` WHERE `{c}` IS NOT NULL AND `{c}` <> '' LIMIT %s", (cap,))
    return {str(r[0]) for r in cur.fetchall()}

def stem(name):
    # ActivityStatusEnum → activitystatus；DeviceLedgerStatusEnum → deviceledgerstatus
    s = name.lower().replace("enum", "")
    return s

def affinity(enum_name, t, c):
    e = stem(enum_name)
    toks = {t.lower().replace("t_", "").replace("_", ""), c.lower().replace("_", "")}
    # 词干互含：enum 词干含表/列任一片段（≥4 字），或表名片段含 enum 前半
    for tok in toks:
        if len(tok) >= 4 and tok in e:
            return True
    # 列名后缀剥掉后的词干（customer_class → customerclass）在 enum 名里
    base = c.lower().rsplit("_", 1)[0].replace("_", "")
    return len(base) >= 4 and base in e

def affinity_score(enum_name, column: str) -> int:
    """词干打分：列名逐 token 出现在枚举词干里的总长。
    短码空间巧合（两枚举同码 1-4）时靠它决胜：activity_level 对
    ActivityLevelEnum（activity+level 全中）必须赢过 ActivityFeeTypeEnum（只中 activity）。"""
    e = stem(enum_name)
    toks = [x for x in column.lower().split("_") if len(x) >= 3]
    return sum(len(t) for t in toks if t in e)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true")
    a = ap.parse_args()
    import pymysql, psycopg2
    enums = java_enums()
    mc = pymysql.connect(**analysis_mysql_kwargs())
    cur = mc.cursor()
    pg = psycopg2.connect(**pg_kwargs())
    pgc = pg.cursor()
    reg = registered(pgc)
    print(f"Java 枚举 {len(enums)} 个")
    cols = [x for x in candidate_columns(cur) if x not in reg]
    print(f"未登记候选列 {len(cols)} 条")
    hits, probes, misses = [], [], 0
    for t, c in cols:
        try:
            vals = col_values(cur, t, c)
        except Exception:
            continue
        if len(vals) < 2:
            continue
        best, ambiguous = None, False
        for ename, pairs in enums.items():
            codes = set(pairs.keys())
            cov = len(vals & codes) / len(vals)
            if cov < 0.8:
                continue
            score = affinity_score(ename, c)
            key = (round(cov, 3), score)
            if best is None or key > best[0]:
                if best is not None and key == best[0]:
                    ambiguous = True  # 同 cov 同分：不自动归属（AX6 的同名不同码必须人来定）
                else:
                    ambiguous = False
                best = (key, ename, pairs, cov)
            elif key == best[0]:
                ambiguous = True
        if not best or ambiguous:
            misses += 1
            if ambiguous:
                print(f"  ⚠ 歧义跳过 {t}.{c}（同 cov 同分，人工裁决）")
            continue
        _, ename, pairs, cov = best
        if not affinity(ename, t, c):
            misses += 1
            continue
        rows = sorted(((pairs[v], v) for v in vals if v in pairs), key=lambda x: x[1])
        origin = "dict" if cov >= 0.999 else "probe"
        hits.append({"table": t, "column": c, "enum": ename, "cov": round(cov, 3),
                     "origin": origin, "values": len(vals), "maps": rows})
        if origin == "probe":
            probes.append(hits[-1])
    hits.sort(key=lambda h: (-h["cov"], h["table"]))
    json.dump(hits, open(r"D:\code\dms_ai\_enum_hits.json", "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"归属命中 {len(hits)} 列（dict {sum(1 for h in hits if h['origin']=='dict')} / probe {len(probes)}），无归属 {misses}")
    for h in hits[:20]:
        print(f"  {h['cov']:.3f} {h['origin']:5s} {h['table']}.{h['column']} ← {h['enum']}（{h['values']} 值）")
    if a.apply:
        import re as _re
        n, skipped = 0, 0
        for h in hits:
            # 备份表（copy\d / _20\d{5} / bak）与 probe（不开火）不登记
            if h["origin"] != "dict" or _re.search(r"copy\d|_20\d{5}|bak", h["table"]):
                skipped += 1
                continue
            for name, code in h["maps"]:
                pgc.execute(
                    "INSERT INTO meta.value_map(table_name, column_name, name, code, match_kind, origin, ds_id) "
                    "VALUES (%s,%s,%s,%s,'exact',%s,'dms') "
                    "ON CONFLICT (ds_id, table_name, column_name, name) DO UPDATE SET code=EXCLUDED.code, origin=EXCLUDED.origin",
                    (h["table"], h["column"], name, code, h["origin"]))
                n += 1
        pg.commit()
        print(f"--apply：写入 {n} 行 value_map（跳过 {skipped} 列：备份表/probe）")

if __name__ == "__main__":
    main()
