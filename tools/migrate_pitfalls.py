# 旧库 dms_meta.skill_memory（45 pitfall + 26 值域 + 20 列修正 + 142 码表）→ 新库 meta.pitfall
# 幂等：按 (kind, trigger_words, lesson) 去重插入。
import json, re, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

def psql(container, port_db_user, sql):
    r = subprocess.run(["docker", "exec", container, "psql", "-U", "postgres", "-d", port_db_user,
                        "-t", "-A", "-F", "\x1f", "-c", sql], capture_output=True, text=True, encoding="utf-8")
    assert r.returncode == 0, r.stderr[-500:]
    return [line.split("\x1f") for line in r.stdout.splitlines() if line.strip()]

# 1. 旧库导出（status 非 disabled 的全部）
rows = psql("xhcrm-postgres", "postgres",
            "SELECT kind, trigger, content FROM dms_meta.skill_memory WHERE COALESCE(status,'active') != 'disabled'")
print(f"旧库导出 {len(rows)} 条")

# 2. 新库插入（去重）
ins = 0
for kind, trig, content in rows:
    trig = (trig or "").replace("'", "''")
    content = (content or "").replace("'", "''")
    kind = (kind or "pitfall").replace("'", "''")
    sql = f"""INSERT INTO meta.pitfall(kind, trigger_words, lesson)
        SELECT '{kind}', '{trig}', '{content}'
        WHERE NOT EXISTS (SELECT 1 FROM meta.pitfall WHERE kind='{kind}' AND trigger_words='{trig}' AND lesson='{content}')"""
    r = subprocess.run(["docker", "exec", "dms-ai-pg", "psql", "-U", "postgres", "-d", "dms_ai", "-c", sql],
                       capture_output=True, text=True, encoding="utf-8")
    assert r.returncode == 0, r.stderr[-300:]
    if "INSERT 0 1" in r.stdout:
        ins += 1
n = psql("dms-ai-pg", "dms_ai", "SELECT kind, COUNT(*) FROM meta.pitfall GROUP BY 1 ORDER BY 1")
print(f"新插入 {ins} 条；新库分布: {n}")
