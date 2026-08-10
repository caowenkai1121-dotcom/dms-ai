# 一次性清洗：autodiscover 首跑（无防误配闸版本）的 41 条注册按新三闸规则复核删除。
# 规则与 meta.rs best_dict_match 一致：A) cov=1.0 且 ≥8 不同值；B) cov=1.0 且码集含非纯数字码；C) 列注释↔字典名 ≥3 字公共子串。
import argparse, json
from pathlib import Path

ap = argparse.ArgumentParser(description="autodiscover 误配注册的一次性清洗（按新三闸规则复核删除）")
# 🔴 三条 meta.* 语句都按它限定：对应 Rust 侧 `meta::DS_PRED`（单一事实源，crates/server/src/meta.rs）。
# python 侧不受那条漂移守卫保护 —— 少一处就跨源乱删，把别的源自动发现出来的资产删掉，只能重跑找回。
# 下面的码集读自分析目标中的 DMS 镜像，故 --ds 必须是对应源的 ds_id（默认就是它）。
ap.add_argument("--ds", default="dms", help="只清洗该 ds_id 的注册资产（默认 dms）")
DS = ap.parse_args().ds

import psycopg2, pymysql   # noqa: E402 —— 放在 argparse 之后：--help 不该因为缺驱动而失败

ROOT = Path(__file__).resolve().parent.parent
reg = json.load(open(ROOT / "autodiscover.result.json", encoding="utf-8"))["registered"]

# dict 码集（判特征码用，分析库只读）。凭据一律走 settings.json，见 tools/settings.py 的文件头。
import settings as st   # noqa: E402 —— 与上面两个驱动同理，放在 argparse 之后
my = pymysql.connect(**st.analysis_mysql_kwargs())
mycur = my.cursor()
mycur.execute("SELECT k.key_code, v.value_code FROM t_dict_key k JOIN t_dict_value v ON v.dict_key_id=k.dict_key_id AND v.deleted_flag=0 WHERE k.deleted_flag=0")
codes_by_key = {}
for kc, vc in mycur.fetchall():
    codes_by_key.setdefault(kc, set()).add(vc)

pg = psycopg2.connect(**st.pg_kwargs())
pgcur = pg.cursor()

def grams(s): return {s[i:i+3] for i in range(len(s) - 2)} if len(s) >= 3 else set()
def aligns(a, b): return bool(grams(a) & grams(b))

kept, dropped = [], []
for r in reg:
    t, c, dk, dn = r["table"], r["column"], r["dict"], r["dict_name"]
    pgcur.execute("SELECT col_comment FROM meta.column_doc"
                  " WHERE table_name=%s AND column_name=%s AND ds_id=%s", (t, c, DS))   # meta::DS_PRED
    row = pgcur.fetchone()
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
        # 两条删除同样按 ds 限定（meta::DS_PRED）：跨源删掉的是别人重跑一遍才回来的资产
        pgcur.execute("DELETE FROM meta.value_map"
                      " WHERE table_name=%s AND column_name=%s AND ds_id=%s", (t, c, DS))
        pgcur.execute("DELETE FROM meta.dimension WHERE dim_code=%s AND ds_id=%s",
                      (f"auto_{t}_{c}"[:80], DS))
pg.commit()
print(f"[ds={DS}] 保留 {len(kept)} / 删除 {len(dropped)}")
print("--- 保留")
print("\n".join(kept))
print("--- 删除（误配）")
print("\n".join(dropped))
