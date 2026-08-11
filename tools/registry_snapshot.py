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
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import settings as dms_settings  # noqa: E402  （仓库统一配置入口：凭据只从 settings.json 来）

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
        try:
            port = u.port  # 非法端口（abc/超界）在取值时才抛 ValueError，给它一句人话
        except ValueError:
            raise SystemExit("--pg-url 端口非法（要 scheme://user:pwd@host:port/db）") from None
        # 查询串只透传白名单键：拼错的 sslmod= 之类原样传给 psycopg2 是裸 TypeError
        q = {k: v for k, v in parse_qsl(u.query)
             if k in ("sslmode", "connect_timeout", "application_name")}
        q.setdefault("connect_timeout", 10)  # 目标不可达时 10s 内报错，不无限挂起
        return psycopg2.connect(host=u.hostname, port=port, user=u.username,
                                password=u.password, dbname=u.path.lstrip('/'), **q)
    cfg = dms_settings.load()
    kw = dms_settings.pg_kwargs(cfg)
    kw.setdefault("connect_timeout", 10)
    return psycopg2.connect(**kw)


def columns_of(cur, table):
    cur.execute(
        "SELECT column_name FROM information_schema.columns "
        "WHERE table_schema='meta' AND table_name=%s ORDER BY ordinal_position", (table,))
    return [r[0] for r in cur.fetchall()]


def do_export(path):
    conn = connect()
    try:
        out = {"version": 1, "tables": {}, "custom_comments": {}}
        cur = conn.cursor()
        for table in TABLES:
            cols = [c for c in columns_of(cur, table) if c not in DROP_COLS]
            if not cols:
                raise SystemExit(f"meta.{table} 不存在或无任何列（库连错了？）")
            # ds_id 以 upload_ 开头的是上传库登记：上传库元数据不随部署快照走
            # （新部署里没有那些临时库）。`\_` 是 LIKE 的转义，匹配字面下划线。
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
    finally:
        conn.close()  # 导出中途异常也不许挂连接
    # 先写 .tmp 再原子替换：中途崩溃不留半截 JSON 给导入侧撞 JSONDecodeError
    tmp = Path(str(path) + ".tmp")
    tmp.write_text(json.dumps(out, ensure_ascii=False, indent=1, default=str), encoding="utf-8")
    os.replace(tmp, path)
    n = sum(len(t["rows"]) for t in out["tables"].values())
    nc = sum(len(t["rows"]) for t in out["custom_comments"].values())
    print(f"导出完成 {path}：注册表 {n} 行 + 人工注释 {nc} 行")


def do_import(path, pg_url=None):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(data, dict) or "tables" not in data:
        raise SystemExit(f"{path} 不是注册表快照（缺 version/tables 键），拿错文件了？")
    if data.get("version") != 1:
        print(f"警告：快照 version={data.get('version')!r}，本工具按 version=1 的格式解读", file=sys.stderr)
    conn = connect(pg_url)
    # 显式事务（autocommit=False 本就是 psycopg2 默认，不重复赋值），收尾统一 commit：要么全进要么不进
    try:
        cur = conn.cursor()
        total = 0
        # 勿并发导入：check-then-insert 没有唯一约束兜底，两个 import 并发会出重复行
        for table, spec in data["tables"].items():
            cols, key, rows = spec["columns"], spec["key"], spec["rows"]
            inserted = 0
            missing = set()
            for row in rows:
                missing |= {c for c in cols if c not in row}  # 快照与代码列漂移时不静默补 None
                vals = [row.get(c) for c in cols]
                # WHERE NOT EXISTS 去重：不依赖表上有没有唯一约束，幂等收敛
                cond = " AND ".join(f"{k} IS NOT DISTINCT FROM %s" for k in key)
                sql = (f"INSERT INTO meta.{table} ({', '.join(cols)}) "
                       f"SELECT {', '.join(['%s'] * len(cols))} "
                       f"WHERE NOT EXISTS (SELECT 1 FROM meta.{table} WHERE {cond})")
                # 参数是两段：前段 vals 给 INSERT 列值，后段给 NOT EXISTS 的去重条件（不是传重了）
                try:
                    cur.execute(sql, vals + [row.get(k) for k in key])
                except Exception as e:
                    raise SystemExit(
                        f"导入 meta.{table} 失败"
                        f"（去重键 {dict(zip(key, [row.get(k) for k in key]))}）：{e}") from e
                inserted += cur.rowcount
            total += inserted
            print(f"  meta.{table}: +{inserted}（快照 {len(rows)} 行）")
            if missing:
                print(f"  警告：meta.{table} 快照缺列 {sorted(missing)}，已按 NULL 插入（快照与代码列漂移）",
                      file=sys.stderr)
        for table, spec in data.get("custom_comments", {}).items():
            key = spec["key"]
            updated = 0
            for row in spec["rows"]:
                # 键列均 NOT NULL，这里用 = 即可（上面注册表去重要兼容可空列才用 IS NOT DISTINCT FROM，别照抄反）
                cond = " AND ".join(f"{k}=%s" for k in key)
                cur.execute(
                    f"UPDATE meta.{table} SET custom_comment=%s WHERE {cond} AND custom_comment = ''",
                    [row["custom_comment"]] + [row[k] for k in key])
                updated += cur.rowcount
            print(f"  meta.{table}.custom_comment: 补空白 {updated} 行")
        conn.commit()
    except BaseException:
        conn.rollback()  # 中途异常不留悬挂事务
        raise
    finally:
        conn.close()
    print(f"导入完成：新增 {total} 行（既有行一律未动）。向量列未带——服务启动即自愈一轮，最迟 10 分钟内补齐。")


if __name__ == "__main__":
    args = sys.argv[1:]
    pg_url = None
    if "--pg-url" in args:
        i = args.index("--pg-url")
        if i + 1 >= len(args):
            raise SystemExit("--pg-url 缺值：--pg-url postgres://user:pwd@host:port/db")
        pg_url = args[i + 1]
        del args[i:i + 2]
    if len(args) > 2:
        raise SystemExit(f"多余参数 {args[2:]}：用法是 export|import [路径] [--pg-url ...]")
    cmd = args[0] if args else ""
    path = args[1] if len(args) > 1 else str(Path(__file__).parent / "registry_snapshot.json")
    if cmd == "export":
        do_export(path)
    elif cmd == "import":
        do_import(path, pg_url)
    else:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
