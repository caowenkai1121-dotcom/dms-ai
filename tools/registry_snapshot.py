#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""注册表快照导出/导入（部署种子数据）。

为什么有这个工具：问数准确性的一半在 meta.* 注册表（码值映射/维度/表过滤/SQL 样例/口径…），
它们一部分由代码种子灌入，一部分是数据驱动登记（meta autodiscover）与人工复核沉淀。
本工具把现网的这些行导成一个 JSON 快照，部署时一条命令幂等灌回新库
（WHERE NOT EXISTS 去重，重复导入/与代码种子混跑都安全收敛）。

红线：快照里有业务字典值（公司/渠道/码值名），**不进公开仓库**（.gitignore 已挡
registry_snapshot.json）——它随部署包私下传递。

用法：
  python tools/registry_snapshot.py export [路径]   # 从 settings.json 指向的库导出（默认 tools/registry_snapshot.json）
  python tools/registry_snapshot.py import [路径]   # 幂等灌入 settings.json 指向的库
  python tools/registry_snapshot.py import --pg-url postgres://...  # 显式指定目标库
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import settings as ts  # noqa: E402  （仓库统一配置入口：凭据只从 settings.json 来）

# (表, 去重键)。列清单导出时动态取（剔除 id/embedding/时间戳/计数字段），键在这里钉死。
TABLES = {
    "metric": ["ds_id", "metric_code"],
    "dimension": ["ds_id", "dim_code"],
    "term": ["ds_id", "term"],
    "value_map": ["ds_id", "table_name", "column_name", "name"],
    "value_domain": ["ds_id", "table_name", "column_name"],
    "table_scope": ["ds_id", "table_name"],
    "table_snapshot": ["ds_id", "table_name"],
    "kw_force": ["ds_id", "keyword", "table_name"],
    "join_edge": ["ds_id", "left_table", "left_col", "right_table", "right_col"],
    "scope_binding": ["table_name"],
    "document_family": ["ds_id", "family_code"],
    "datasource": ["ds_id"],
    "sql_exemplar": ["question", "sql"],
    "pitfall": ["kind", "trigger_words", "lesson"],
    "memory": ["ds_id", "kind", "question"],
    "skill": ["name"],
}
# 导出时剔除的列：自增 id（新库自己发号）、向量（部署后由服务向量自愈回填）、时间/计数
DROP_COLS = {"id", "embedding", "created_at", "updated_at", "reviewed_at", "validated_at", "hit_count"}
# 人工改过的注释（table_doc/column_doc.custom_comment）单独导出，导入侧只补空白不覆盖
COMMENT_TABLES = {"table_doc": ["ds_id", "table_name"], "column_doc": ["ds_id", "table_name", "column_name"]}


def connect(pg_url=None):
    import psycopg2
    if pg_url:
        from urllib.parse import urlparse, parse_qsl
        u = urlparse(pg_url)
        q = dict(parse_qsl(u.query))
        return psycopg2.connect(host=u.hostname, port=u.port, user=u.username,
                                password=u.password, dbname=u.path.lstrip('/'), **q)
    cfg = ts.load()
    return psycopg2.connect(**ts.pg_kwargs(cfg))


def columns_of(cur, table):
    cur.execute(
        "SELECT column_name FROM information_schema.columns "
        "WHERE table_schema='meta' AND table_name=%s ORDER BY ordinal_position", (table,))
    return [r[0] for r in cur.fetchall()]


def do_export(path):
    conn = connect()
    out = {"version": 1, "tables": {}, "custom_comments": {}}
    cur = conn.cursor()
    for table in TABLES:
        cols = [c for c in columns_of(cur, table) if c not in DROP_COLS]
        where = " WHERE ds_id NOT LIKE 'upload\\_%'" if table == "datasource" else ""
        cur.execute(f"SELECT {', '.join(cols)} FROM meta.{table}{where}")
        rows = [dict(zip(cols, r)) for r in cur.fetchall()]
        out["tables"][table] = {"key": TABLES[table], "columns": cols, "rows": rows}
    for table, key in COMMENT_TABLES.items():
        cols = key + ["custom_comment"]
        cur.execute(f"SELECT {', '.join(cols)} FROM meta.{table} WHERE custom_comment <> ''")
        out["custom_comments"][table] = {
            "key": key, "rows": [dict(zip(cols, r)) for r in cur.fetchall()],
        }
    conn.close()
    Path(path).write_text(json.dumps(out, ensure_ascii=False, indent=1, default=str), encoding="utf-8")
    n = sum(len(t["rows"]) for t in out["tables"].values())
    nc = sum(len(t["rows"]) for t in out["custom_comments"].values())
    print(f"导出完成 {path}：注册表 {n} 行 + 人工注释 {nc} 行")


def do_import(path, pg_url=None):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    conn = connect(pg_url)
    conn.autocommit = False
    cur = conn.cursor()
    total = 0
    for table, spec in data["tables"].items():
        cols, key, rows = spec["columns"], spec["key"], spec["rows"]
        inserted = 0
        for row in rows:
            vals = [row.get(c) for c in cols]
            # WHERE NOT EXISTS 去重：不依赖表上有没有唯一约束，幂等收敛
            cond = " AND ".join(f"{k} IS NOT DISTINCT FROM %s" for k in key)
            sql = (f"INSERT INTO meta.{table} ({', '.join(cols)}) "
                   f"SELECT {', '.join(['%s'] * len(cols))} "
                   f"WHERE NOT EXISTS (SELECT 1 FROM meta.{table} WHERE {cond})")
            cur.execute(sql, vals + [row.get(k) for k in key])
            inserted += cur.rowcount
        total += inserted
        print(f"  meta.{table}: +{inserted}（快照 {len(rows)} 行）")
    for table, spec in data.get("custom_comments", {}).items():
        key = spec["key"]
        updated = 0
        for row in spec["rows"]:
            cond = " AND ".join(f"{k}=%s" for k in key)
            cur.execute(
                f"UPDATE meta.{table} SET custom_comment=%s WHERE {cond} AND custom_comment = ''",
                [row["custom_comment"]] + [row[k] for k in key])
            updated += cur.rowcount
        print(f"  meta.{table}.custom_comment: 补空白 {updated} 行")
    conn.commit()
    conn.close()
    print(f"导入完成：新增 {total} 行（既有行一律未动）。向量列未带——服务启动后 10 分钟内向量自愈自动回填。")


if __name__ == "__main__":
    args = sys.argv[1:]
    pg_url = None
    if "--pg-url" in args:
        i = args.index("--pg-url")
        pg_url = args[i + 1]
        del args[i:i + 2]
    cmd = args[0] if args else ""
    path = args[1] if len(args) > 1 else str(Path(__file__).parent / "registry_snapshot.json")
    if cmd == "export":
        do_export(path)
    elif cmd == "import":
        do_import(path, pg_url)
    else:
        print(__doc__)
        sys.exit(2)
