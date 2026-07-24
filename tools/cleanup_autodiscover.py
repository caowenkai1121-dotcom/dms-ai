# 一次性清洗：autodiscover 首跑（无防误配闸版本）的 41 条注册按新三闸规则复核删除。
# 规则与 meta.rs best_dict_match 一致：A) cov=1.0 且 ≥8 不同值；B) cov=1.0 且码集含非纯数字码；C) 列注释↔字典名 ≥3 字公共子串。
import json, psycopg2, pymysql, re
from urllib.parse import unquote
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
reg = json.load(open(ROOT / "autodiscover.result.json", encoding="utf-8"))["registered"]

# dict 码集（判特征码用，生产只读）
cfg = json.load(open(ROOT / "settings.json", encoding="utf-8"))
m = re.match(r"mysql://([^:]+):([^@]+)@([^:]+):(\d+)/(\w+)", cfg["mysql_url"])
u, p, h, po, db = m.groups()
my = pymysql.connect(host=h, port=int(po), user=unquote(u), password=unquote(p), database=db, charset="utf8mb4")
mycur = my.cursor(); mycur.execute("SET SESSION TRANSACTION READ ONLY")
mycur.execute("SELECT k.key_code, v.value_code FROM t_dict_key k JOIN t_dict_value v ON v.dict_key_id=k.dict_key_id AND v.deleted_flag=0 WHERE k.deleted_flag=0")
codes_by_key = {}
for kc, vc in mycur.fetchall():
    codes_by_key.setdefault(kc, set()).add(vc)

pg = psycopg2.connect(host="localhost", port=15433, user="postgres", password="dmsai_pg_2026", dbname="dms_ai")
cur = pg.cursor()

def grams(s): return {s[i:i+3] for i in range(len(s) - 2)} if len(s) >= 3 else set()
def aligns(a, b): return bool(grams(a) & grams(b))

kept, dropped = [], []
for r in reg:
    t, c, dk, dn = r["table"], r["column"], r["dict"], r["dict_name"]
    cur.execute("SELECT col_comment FROM meta.column_doc WHERE table_name=%s AND column_name=%s", (t, c))
    row = cur.fetchone()
    comment = (row[0] if row else "") or ""
    codes = codes_by_key.get(dk, set())
    has_alpha = any(not str(x).isdigit() for x in codes)
    ok = (r["coverage"] >= 1.0 and r["distinct_values"] >= 8) \
         or (r["coverage"] >= 1.0 and has_alpha) \
         or aligns(comment, dn)
    if ok:
        kept.append(f"{t}.{c} -> {dn} (注释[{comment}])")
    else:
        dropped.append(f"{t}.{c} -> {dn} (注释[{comment}] values={r['distinct_values']})")
        cur.execute("DELETE FROM meta.value_map WHERE table_name=%s AND column_name=%s", (t, c))
        cur.execute("DELETE FROM meta.dimension WHERE dim_code=%s", (f"auto_{t}_{c}"[:80],))
pg.commit()
print(f"保留 {len(kept)} / 删除 {len(dropped)}")
print("--- 保留")
print("\n".join(kept))
print("--- 删除（误配）")
print("\n".join(dropped))
