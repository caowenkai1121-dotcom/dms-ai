# 只读探针：候选维度列值域抽样（SELECT DISTINCT/GROUP BY，小表，DMS 生产库只读红线）
# 用法: python tools/probe_values.py
import json, pymysql, re
from urllib.parse import unquote
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
cfg = json.load(open(ROOT / "settings.json", encoding="utf-8"))
m = re.match(r"mysql://([^:]+):([^@]+)@([^:]+):(\d+)/(\w+)", cfg["mysql_url"])
user, pwd, host, port, db = m.groups()
conn = pymysql.connect(host=host, port=int(port), user=unquote(user), password=unquote(pwd),
                       database=db, charset="utf8mb4")
cur = conn.cursor()
cur.execute("SET SESSION TRANSACTION READ ONLY")  # 只读铁律

PROBES = [
    ("t_customer", "customer_class"),
    ("t_customer", "customer_type"),
    ("t_customer", "group1"),
    ("t_customer", "enterprise_type"),
    ("t_customer", "business_type"),
    ("t_account_bill_header", "sale_platform"),
]

for table, col in PROBES:
    cur.execute(f"SELECT COUNT(*) FROM {table} WHERE deleted_flag = 0")
    total = cur.fetchone()[0]
    cur.execute(f"SELECT COUNT(*) FROM {table} WHERE deleted_flag = 0 AND `{col}` IS NOT NULL AND `{col}` != ''")
    nonnull = cur.fetchone()[0]
    cur.execute(
        f"SELECT CAST(`{col}` AS CHAR), COUNT(*) FROM {table} WHERE deleted_flag = 0 "
        f"GROUP BY `{col}` ORDER BY 2 DESC LIMIT 12"
    )
    vals = cur.fetchall()
    print(f"\n== {table}.{col}  总行={total} 非空={nonnull} ({nonnull * 100 // max(total,1)}%)")
    for v, n in vals:
        print(f"   {repr(v)[:50]:52s} {n}")
