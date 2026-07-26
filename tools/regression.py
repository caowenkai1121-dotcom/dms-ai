# M7 判官门禁回归 runner：连库跑 regression_cases.json 全量题集，断言路由/SQL/视图/权限/红线。
# 用法: python tools/regression.py [--filter 关键词]
# 约定: LLM 路径非确定重试 1 次（旧项目惯例）; embed/graph 依赖缺席自动跳过不计失败。
import json, re, subprocess, sys, socket
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "debug" / "dms-ai-server.exe"
CASES = json.loads((ROOT / "tools" / "regression_cases.json").read_text(encoding="utf-8"))

norm = lambda s: "".join(str(s).lower().split())

def service_up(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=1).close()
        return True
    except OSError:
        return False

def graph_up():
    r = subprocess.run(["docker", "ps", "--format", "{{.Names}}"], capture_output=True, text=True)
    return "dms-ai-pg" in r.stdout

def ask(login, question, role=None, retries=1):
    cmd = [str(EXE), "ask", login, question] + ([role] if role else [])
    last = {}
    for _ in range(retries + 1):
        r = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", cwd=str(ROOT))
        if r.returncode == 0:
            try:
                j = json.loads(r.stdout)
                if j.get("columns") or j.get("route") == "compound":
                    return j
                last = j
            except json.JSONDecodeError:
                last = {"error": r.stdout[-300:]}
        else:
            last = {"error": r.stderr.strip()[-300:]}
    return last

DML = ["insert", "update", "delete", "drop", "truncate", "alter", "create", "merge", "grant", "revoke"]

def sql_tokens(sql):
    """SQL → 小写标识符 token 集合。先剥字符串字面量与注释，再按非标识符字符切。
    对齐 Rust is_safe_select 的词法判定——子串匹配会把 deleted_flag 判成 delete、
    created_time 判成 update（H01-H03 曾因此假红）。"""
    s, out, i, n = sql, [], 0, len(sql)
    buf = []
    while i < n:
        c = s[i]
        if c in "'\"":                      # 字符串字面量整段丢弃
            q, i = c, i + 1
            while i < n:
                if s[i] == "\\":
                    i += 2; continue
                if s[i] == q:
                    if i + 1 < n and s[i + 1] == q:
                        i += 2; continue
                    i += 1; break
                i += 1
            buf.append(" ")
        elif s.startswith("--", i) or c == "#":   # 行注释
            while i < n and s[i] != "\n":
                i += 1
        elif s.startswith("/*", i):               # 块注释
            j = s.find("*/", i + 2)
            i = n if j < 0 else j + 2
            buf.append(" ")
        else:
            buf.append(c); i += 1
    for tok in re.split(r"[^A-Za-z0-9_]+", "".join(buf)):
        if tok:
            out.append(tok.lower())
    return out

def run_case(c, results):
    name = c["name"]
    fails = []
    if c.get("requires_embed") and not EMBED_UP:
        results.append((name, None, "embed 服务缺席跳过")); return
    if c.get("requires_graph") and not GRAPH_UP:
        results.append((name, None, "PG 容器缺席跳过")); return

    j = ask(c["login"], c["q"], c.get("role"), retries=1 if (c.get("llm") or c.get("type") == "redline") else 0)
    sql = j.get("sql", "") or ""
    nsql = norm(sql)

    if c.get("type") == "redline":
        # 红线：执行出去的 SQL 绝不含 DML 语句（报错/拒绝也算守住）。
        # 按 token 判定——deleted_flag/created_time 这类列名不得算作 DML。
        toks = sql_tokens(sql)
        bad = sorted({k for k in DML if k in toks})
        # 有 SQL 时必须是 SELECT/WITH 开头（AST 级只读红线的外部复核）
        if toks and toks[0] not in ("select", "with"):
            bad.append(f"首token={toks[0]}")
        ok = not bad
        results.append((name, ok, f"sql_dml={bad or '无'} route={j.get('route')}"))
        return

    if "error" in j and not j.get("columns"):
        results.append((name, False, f"执行错误: {j['error'][:120]}")); return

    if c.get("route") and j.get("route") != c["route"]:
        fails.append(f"route={j.get('route')}≠{c['route']}")
    if c.get("route_not") and j.get("route") == c["route_not"]:
        fails.append(f"route={j.get('route')}命中排除项")
    for frag in c.get("sql_contains", []):
        if norm(frag) not in nsql:
            fails.append(f"SQL缺[{frag}]")
    anyfrags = c.get("sql_contains_any", [])
    if anyfrags and not any(norm(f) in nsql for f in anyfrags):
        fails.append(f"SQL缺任一{anyfrags}")
    for frag in c.get("sql_not_contains", []):
        if norm(frag) in nsql:
            fails.append(f"SQL含禁词[{frag}]")
    if c.get("min_rows") and j.get("row_count", len(j.get("rows", []))) < c["min_rows"]:
        fails.append(f"行数{j.get('row_count')}<{c['min_rows']}")
    if c.get("min_cols") and len(j.get("columns", [])) < c["min_cols"]:
        fails.append(f"列数{len(j.get('columns', []))}<{c['min_cols']}")
    blocks = (j.get("view") or {}).get("blocks", [])
    if c.get("view0"):
        t0 = blocks[0].get("type") if blocks else None
        if t0 != c["view0"]:
            fails.append(f"view0={t0}≠{c['view0']}")
    if c.get("chart_kind"):
        k0 = blocks[0].get("kind") if blocks else None
        if k0 != c["chart_kind"]:
            fails.append(f"chart={k0}≠{c['chart_kind']}")
    raw = json.dumps(j, ensure_ascii=False)
    for frag in c.get("json_contains", []):
        if frag not in raw:
            fails.append(f"JSON缺[{frag}]")

    detail = f"route={j.get('route')} {j.get('elapsed_ms')}ms" + (" · " + ";".join(fails) if fails else "")
    results.append((name, not fails, detail))
    # 供关系断言取数
    if j.get("rows") and j["rows"] and j["rows"][0]:
        try:
            VALUES[name] = float(j["rows"][0][0])
        except (TypeError, ValueError):
            pass

EMBED_UP = service_up(8077)
GRAPH_UP = graph_up()
VALUES = {}
results = []
flt = sys.argv[sys.argv.index("--filter") + 1] if "--filter" in sys.argv else None

print(f"embed={'up' if EMBED_UP else 'DOWN'} graph={'up' if GRAPH_UP else 'DOWN'} 题数={len(CASES['cases'])}")
for c in CASES["cases"]:
    if flt and flt not in c["name"]:
        continue
    run_case(c, results)

for rule in CASES.get("rules", []):
    if "lt" in rule:
        a, b = rule["lt"]
        if a in VALUES and b in VALUES:
            ok = 0 < VALUES[a] < VALUES[b]
            results.append((f"R-{a}<{b}", ok, f"{VALUES[a]:,.0f} < {VALUES[b]:,.0f}"))
        else:
            results.append((f"R-{a}<{b}", None, "取值缺失跳过"))

print("=" * 60)
skipped = [x for x in results if x[1] is None]
fails = [x for x in results if x[1] is False]
passed = [x for x in results if x[1] is True]
for name, ok, detail in results:
    mark = "✅" if ok else ("⏭️" if ok is None else "❌")
    print(f"{mark} {name} · {detail}")
print("=" * 60)
print(f"通过 {len(passed)} / 失败 {len(fails)} / 跳过 {len(skipped)}")
sys.exit(1 if fails else 0)
