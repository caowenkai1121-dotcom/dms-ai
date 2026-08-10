# 只读探针：候选维度列值域抽样（SELECT DISTINCT/GROUP BY，走非 DMS 分析目标）
# 用法: python tools/probe_values.py
import pymysql
import settings as st

# 凭据与 URL 解析都在 tools/settings.py（明文口令只许住在 settings.json）
conn = pymysql.connect(**st.analysis_mysql_kwargs())
cur = conn.cursor()

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
