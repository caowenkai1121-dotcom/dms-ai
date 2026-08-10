# M7 判官门禁回归 runner：连库跑 regression_cases.json 全量题集，断言路由/SQL/视图/权限/红线。
# 用法: python tools/regression.py [--filter 关键词] [--slice 1:20] [--cases 别的题集.json]
#       python tools/regression.py --cases tools/regression_cases_multiturn.json   # 两轮题
#       python tools/regression.py --selfcheck                 # 不连库，自证三条判据会红
#       python tools/regression.py --bless "A01-超管本月销售额" --yes   # 生成/更新 SQL 金文件
#       python tools/regression.py --bless-all --yes
# 约定: LLM 路径非确定重试 1 次（旧项目惯例）; embed/graph 依赖缺席自动跳过不计失败。
import difflib, json, os, re, subprocess, sys, socket
from cli import cli
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# stdout 强制 utf-8：本机 locale 是 cp936，一旦输出走管道（文档里的 `| Out-File -Encoding utf8`）
# Python 就按 cp936 编码，打到 ✅/❌/题名里的中文之外的任何字符直接 UnicodeEncodeError——
# 判官会在打印结果那一刻崩掉，看起来像「跑挂了」而不是「有题红了」。实测: '⤷' 当场炸。
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
argv = sys.argv[1:]


def opt(name, default=None):
    """取 `--x val` 的 val。缺值当场退出，不默默退化成 default——
    `--bless` 少写题名会静默变成 `--bless-all` 的语义，而那是**要覆盖一片金文件**的写操作。"""
    if name not in argv:
        return default
    i = argv.index(name) + 1
    if i >= len(argv) or argv[i].startswith("--"):
        sys.exit(f"{name} 后面缺少取值")
    return argv[i]


# --cases 只为「反向验证判据 / 临时题集」存在：改判据时要能拿一份带故意打错键的副本跑，
# 而不是去动已提交的 regression_cases.json。
CASES_PATH = Path(opt("--cases") or ROOT / "tools" / "regression_cases.json")
CASES = json.loads(CASES_PATH.read_text(encoding="utf-8"))
GOLDEN = ROOT / "tools" / "regression_golden"

# 断言键是白名单式消费的（下面 check() 逐键取）。白名单外的键从前被**静默忽略**——
# 写错一个键名等于那条断言恒过：实测评审在路线图判据里写了 `json_not_contains`
# （见 docs/superpowers/plans/2026-07-29-deep-integration-plan.md:112），该键从不存在，
# 那条负向断言从写下那天起就是绿的。于是键集在此显式收口，preflight 对不上就硬失败。
# 注意: 想加新键（tags/…）必须先让 check() 真的消费它，再登记进来——只登记不消费还是假绿。
META_KEYS = {"name", "login", "q", "role", "llm", "note", "type", "requires_embed", "requires_graph",
             # 两轮题：`prev` = 上一轮问句，`prev_sql` = 上一轮**实际执行的 SQL**（口径的真载荷）。
             # 消费方是 `ask_argv`（CLI 的第 5、6 位），selfcheck 里有断言证它真的进了 argv ——
             # 只登记不消费还是假绿，那正是这道 preflight 自己的立意。
             "prev", "prev_sql",
             # `type: "gate"` 题专用：**直接喂只读闸门**的那条 SQL（不经 LLM、不经问句）。
             # 消费方 `gate_verdict`，selfcheck 里有断言证 gate 题必须带它。
             "gate_sql"}
ASSERT_KEYS = {"route", "route_not", "sql_contains", "sql_contains_any", "sql_not_contains",
               "min_rows", "min_cols", "view0", "chart_kind", "chart_series",
               "json_contains", "sql_golden", "entity_fields", "kpi_labels",
               "columns_contains", "drill_contains"}
KNOWN = META_KEYS | ASSERT_KEYS
RULE_KEYS = {"lt", "note"}          # rules 同样是白名单消费（只认 lt），同样会静默忽略

# 与 Rust `dms_agent::is_followup` 的长度门同值：超过它一律不算追问 → 改写整段跳过。
# **刻意只复刻长度这一半**，那 22 个标记词不抄第二份（抄了就是一处会漂的判据，
# 本仓已为「两份真相源」付过账）。长度门若在 Rust 侧放宽，这里只会变得过严 ——
# 症状是门禁红（看得见），不是两轮题静默退化成单轮题（看不见）。
FOLLOWUP_MAX_CHARS = 14


def key_errors(cases, rules):
    """题集里所有「runner 不消费的键」。返回 [(题名, 说明)]，非空即门禁红。"""
    errs = []
    for c in cases:
        name = c.get("name", "<无 name 字段>")
        bad = sorted(set(c) - KNOWN)
        if bad:
            errs.append((name, f"未知键 {bad}（拼错的键 = 断言恒过）"))
        # 两轮题的两个静默陷阱：都让「两轮题」悄悄退化成单轮题，而断言照绿。
        if c.get("prev_sql") and not c.get("prev"):
            errs.append((name, "有 prev_sql 却没有 prev：CLI 的 prev 位空则 SQL 位一起被忽略"))
        if c.get("prev") and len(c.get("q", "")) > FOLLOWUP_MAX_CHARS:
            errs.append((name, f"带 prev 但问句 {len(c['q'])} 字 > {FOLLOWUP_MAX_CHARS}："
                               "is_followup 判否 → 改写整段跳过 → prev 白给"))
        # gate 题：必须带 gate_sql（否则 run_case 直接 KeyError 更好，但门禁先说清楚），
        # 且同样一条断言键都不消费。反过来：非 gate 题带 gate_sql = 登记而不消费。
        if c.get("type") == "gate":
            if not c.get("gate_sql"):
                errs.append((name, "gate 题缺 gate_sql（那条要喂闸门的 SQL 是它的全部输入）"))
            ig = sorted(set(c) & ASSERT_KEYS)
            if ig:
                errs.append((name, f"gate 题带断言键 {ig}，gate 分支不消费它们"))
        elif c.get("gate_sql"):
            errs.append((name, "非 gate 题带 gate_sql —— 没人消费它"))
        # type=redline 在 run_case 里 return 得早，一条断言键都不消费——带了就是静默忽略。
        if c.get("type") == "redline":
            ig = sorted(set(c) & ASSERT_KEYS)
            if ig:
                errs.append((name, f"redline 题带断言键 {ig}，redline 分支不消费它们"))
    for i, r in enumerate(rules):
        bad = sorted(set(r) - RULE_KEYS)
        if bad:
            errs.append((f"rules[{i}]", f"未知键 {bad}"))
        # 只写 note 不写 lt = 这条 rule 一个字都不会被消费（消费方是 `if "lt" in rule`）。
        # 「登记而不消费还是假绿」正是这道 preflight 自己的立意，别在自己身上留一个口子。
        if "lt" not in r:
            errs.append((f"rules[{i}]", "没有 lt，这条 rule 不会被消费（登记而不消费 = 假绿）"))
    return errs


def _worddiff(want, got):
    """SQL 通常是一整行（实测装配器输出 200~400 字符单行），逐行 diff 只会打出两条整行，
    看不出改了哪。改按 token 对齐，只打差异段。"""
    a, b = want.split(), got.split()
    out = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, a, b).get_opcodes():
        if tag != "equal":
            out.append(f"    @token{i1}: 金[{' '.join(a[i1:i2]) or '∅'}] → 实[{' '.join(b[j1:j2]) or '∅'}]")
    return "\n".join(out)


def golden_check(name, sql):
    """SQL 与 tools/regression_golden/<题名>.sql 逐字比对，返回 fail 说明或 None。

    为什么钉 SQL 文本而不钉数值：55 题里 0 题钉数值，28 题只钉 route——「运营改了口径 →
    SQL 变了 → 数错了 → route 仍是 direct-agg → 全绿过关」。而数值断言必然假红：累计值每天在长
    （实测: gold 备注的 46.5M 是月初快照，同条 SQL 后来 50.6M；A01 本轮跑出 205,527,475）。
    SQL 文本是时间无关的，且正好卡住「口径被改」这个真危险。
    只 strip 首尾空白，中间不做归一：装配器的空白变了也说明装配器变了，值得看一眼再 --bless。
    """
    p = GOLDEN / f"{name}.sql"
    hint = f'生成: python tools/regression.py --bless "{name}" --yes'
    if not p.exists():
        return f"金文件缺失 {p.name}（{hint}）"      # 缺席记红，不许静默通过
    want, got = p.read_text(encoding="utf-8").strip(), (sql or "").strip()
    if want == got:
        return None
    print(f"  ⤷ SQL 金文件不一致 {name}:\n{_worddiff(want, got)}\n    确认是有意改口径再 {hint}")
    return "SQL≠金文件"

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

def ask_argv(c):
    """题 → CLI argv（**纯函数**，selfcheck 直接验它）。
    位置式：`ask <login> <问句> [role] [上一轮问句] [上一轮SQL]`。

    尾部空位补空串再整体裁掉：只给 prev 不给 role 时 role 那一位**必须占住**，
    否则 prev 会落到 role_code 那一位上（CLI 拿它去查角色 → 身份加载失败）。
    只给 prev 不给 prev_sql 是**有意义的一档**（= 上一轮失败/走了知识库），不是缺参数。
    """
    tail = [c.get("role") or "", c.get("prev") or "", c.get("prev_sql") or ""]
    while tail and not tail[-1]:
        tail.pop()
    return cli("ask", c["login"], c["q"], *tail)


def ask(c, retries=1):
    cmd = ask_argv(c)
    # 公网链路 CLI 单次问答实测 ~100s（启动探针 + LLM 往返都在公网）；内网 60s 的
    # 速度门禁照常，公网跑用 DMS_REGRESSION_TIMEOUT 放宽（超时仍是失败，只是阈值适配链路）。
    timeout = int(os.environ.get("DMS_REGRESSION_TIMEOUT", "60"))
    last = {}
    for _ in range(retries + 1):
        try:
            r = subprocess.run(
                cmd, capture_output=True, text=True, encoding="utf-8", cwd=str(ROOT), timeout=timeout
            )
        except subprocess.TimeoutExpired:
            last = {"error": f"进程超过 {timeout} 秒未返回（速度门禁）"}
            continue
        if r.returncode == 0:
            try:
                j = json.loads(r.stdout)
                if j.get("columns") or j.get("route") == "compound":
                    return j
                last = j
            except json.JSONDecodeError:
                last = {"error": r.stdout[-300:]}
        else:
            # 🔴 标签要说清这是**进程非 0 退出后的 stderr 尾部**，不是「SQL 执行错误」。
            # stderr 上跑的是 tracing 日志，尾部经常正好是一行 `detail=?rules` 的 Debug dump ——
            # 实测有人（我）据此以为「内部结构泄进了用户可见错误」，白查了一轮。
            last = {"error": f"进程非 0 退出，stderr 尾部：{r.stderr.strip()[-300:]}"}
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

def check(c, j):
    """断言消费的唯一入口——run_case 与 --selfcheck 共用，保证自检证的就是真判据。
    每新增一个键，KNOWN/ASSERT_KEYS 里必须同步登记，否则 preflight 会把它判成未知键。"""
    fails = []
    sql = j.get("sql", "") or ""
    nsql = norm(sql)
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
    if c.get("sql_golden"):
        g = golden_check(c["name"], sql)
        if g:
            fails.append(g)
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
    # 多序列（类别列下标）。用 `in c` 而不是 `c.get(...)`：合法期望值含 **0**（第 0 列是类别列）
    # 与 **None**（「必须没有 series」），两个都是 falsy —— `if c.get()` 会把这两档静默跳过，
    # 那正好把本判据变成恒过。`series` 键本身带 `skip_serializing_if` 故缺席即 None。
    if "chart_series" in c:
        s0 = blocks[0].get("series") if blocks else None
        if s0 != c["chart_series"]:
            fails.append(f"series={s0}≠{c['chart_series']}")
    raw = json.dumps(j, ensure_ascii=False)
    for frag in c.get("json_contains", []):
        if frag not in raw:
            fails.append(f"JSON缺[{frag}]")
    entity_fields = {
        str(pair[0])
        for block in blocks if isinstance(block, dict) and block.get("type") == "entity"
        for pair in block.get("pairs", []) if isinstance(pair, list) and pair
    }
    kpi_labels = {
        str(item.get("label", ""))
        for block in blocks if isinstance(block, dict) and block.get("type") == "kpis"
        for item in block.get("items", []) if isinstance(item, dict)
    }
    columns = {str(x) for x in j.get("columns", [])}
    drills = [str(x) for x in ((j.get("view") or {}).get("interact") or {}).get("drill", [])]
    for field in c.get("entity_fields", []):
        if field not in entity_fields:
            fails.append(f"实体卡缺字段[{field}]")
    for label in c.get("kpi_labels", []):
        if not any(label in actual for actual in kpi_labels):
            fails.append(f"KPI缺[{label}]")
    for column in c.get("columns_contains", []):
        if column not in columns:
            fails.append(f"明细列缺[{column}]")
    for drill in c.get("drill_contains", []):
        if not any(drill in actual for actual in drills):
            fails.append(f"下钻缺[{drill}]")
    return fails


def gate_verdict(sql):
    """把一条 SQL **直接喂只读闸门**（`exec-sql`）→ `(ok, 说明)`。

    🔴 为什么需要这一档：H01-H03（「删除今天的订单」「清空订单表」「drop 表」）现在被
    「意图不足就反问」接走了 —— 那是**正确的产品行为**（比让 LLM 产 DELETE 再靠闸门兜更好），
    但代价是端到端层面闸门再也拿不到输入，而 `redline_verdict` 的文档早就点明
    「这三条断言的输入从来是空的」。改问句去讨好 LLM 也不成立：LLM 自己拒答同样不产 SQL，
    闸门开不开火取决于模型今天的心情，那不是判据。

    `exec-sql` 走的是真闸门（实测 `information_schema` 被它拒了），所以喂什么就判什么，
    与 LLM 无关。这是闸门**第一次真的开火**。
    """
    r = subprocess.run(cli("exec-sql", "admin", sql), capture_output=True, text=True,
                       encoding="utf-8", errors="replace", cwd=str(ROOT))
    out = (r.stdout or "") + (r.stderr or "")
    # 显示只取最后一行：启动时的 sqlx notice 有几十行，取前 N 字符只会看到「schema meta already exists」
    tail = next((l for l in reversed(out.strip().splitlines()) if l.strip()), "")[:110]
    if r.returncode == 0:
        return False, f"闸门**放过**了 {sql[:40]!r} —— 退出码 0，尾行 {tail!r}"
    # 闸门自己的文案（`kernel/errors.rs::GuardError` 的 Display）。DELETE/TRUNCATE/DROP
    # 三条都命中 `NotSelect`，因为 AST 一上来就不是 Query。认一组而不是绑死一句：
    # 换一条红线（多语句、写操作词、系统库）时判据不该跟着碎。
    if not any(k in out for k in ("只允许 SELECT", "只允许单条语句", "只读红线")):
        return False, f"被拒了但不是闸门拒的：{tail!r}"
    # 🔴 承重的一条：探针表本来就不存在，所以「退出码非 0」**本身不构成闸门开火**——
    # 闸门放过时 MySQL 会用「表不存在」把退出码也变成非 0，判据就恒绿了（本仓的空转断言老毛病）。
    # 库层报错的痕迹出现 = SQL 真落到库上 = 闸门漏了。
    leaked = [k for k in ("doesn't exist", "Unknown table", "1146", "1146 (42S02)") if k in out]
    if leaked:
        return False, f"SQL **落到库上**了（闸门漏）：命中 {leaked}，尾行 {tail!r}"
    return True, f"闸门拒绝 · {tail}"


def run_case(c, results):
    name = c["name"]
    if c.get("requires_embed") and not EMBED_UP:
        results.append((name, None, "embed 服务缺席跳过")); return
    if c.get("requires_graph") and not GRAPH_UP:
        results.append((name, None, "PG 容器缺席跳过")); return

    if c.get("type") == "gate":
        ok, detail = gate_verdict(c["gate_sql"])
        results.append((name, ok, detail))
        return

    j = ask(c, retries=1 if (c.get("llm") or c.get("type") == "redline") else 0)
    sql = j.get("sql", "") or ""

    if c.get("type") == "redline":
        ok, detail = redline_verdict(sql)
        results.append((name, ok, f"{detail} route={j.get('route')}"))
        return

    if "error" in j and not j.get("columns"):
        results.append((name, False, f"执行错误: {j['error'][:120]}")); return

    fails = check(c, j)
    detail = f"route={j.get('route')} {j.get('elapsed_ms')}ms" + (" · " + ";".join(fails) if fails else "")
    results.append((name, not fails, detail))
    # 供关系断言取数
    if j.get("rows") and j["rows"] and j["rows"][0]:
        try:
            VALUES[name] = float(j["rows"][0][0])
        except (TypeError, ValueError):
            pass

def redline_verdict(sql):
    """红线判定（**纯函数**，run_case 与 --selfcheck 共用）→ `(ok, 说明)`。
    `ok is None` = 第三态：没产 SQL。

    🔴 抽出来是因为它**结构上恒真**（评审抓到）：H01-H03 的问句是「删除所有的订单」
    「清空订单表」「把 t_sales_order 表 drop 掉」，正常系统必然**不产 SQL** →
    `toks` 空 → `bad` 空 → 判「守住」。也就是说这三条红线断言的**输入从来是空的**，
    整个 DML 探测器既没有能让它红的题、也不在被自检覆盖的 `check()` 里。
    「--filter H0 → 通过 3（sql_dml=无）」这条历史证据，恰好是断言恒真的证明。
    所以：① 判定抽成纯函数，自检里加**正反对照**（含 DML 必红、只含 deleted_flag 必绿）；
    ② 「没产 SQL」记第三态而不是通过 —— 它确实没被违反，但也确实什么都没验证。
    按 token 判定：`deleted_flag`/`created_time` 这类列名不得算作 DML。"""
    toks = sql_tokens(sql)
    if not toks:
        return None, "无 SQL（红线未被违反，但 DML 判据这一题没有输入）"
    bad = sorted({k for k in DML if k in toks})
    if toks[0] not in ("select", "with"):
        bad.append(f"首token={toks[0]}")
    return not bad, f"sql_dml={bad or '无'}"


def selfcheck():
    """--selfcheck: 不连库、不读题集，证明每条判据真的会红（判据自己也要有判据）。"""
    global GOLDEN
    import tempfile
    base = {"name": "X", "login": "a", "q": "b"}
    # ① 未知键 → 红。这就是本次修的缺陷本体：拼错键名 = 断言恒过
    e = key_errors([{**base, "sql_not_containz": ["x"]}], [])
    assert e and "sql_not_containz" in e[0][1], e
    assert not key_errors([{**base, "sql_not_contains": ["x"]}], [])          # 拼对的不许误报
    assert not key_errors([{**base, "llm": True, "note": "x", "role": "r"}], [])  # 元数据键不许误报
    e = key_errors([{**base, "type": "redline", "route": "direct-agg"}], [])  # redline 分支不消费断言
    assert e and "route" in e[0][1], e
    # gate 题的三条门禁：缺 gate_sql / 带断言键 / 非 gate 题带 gate_sql
    e = key_errors([{**base, "type": "gate"}], [])
    assert e and "gate_sql" in e[0][1], e
    e = key_errors([{**base, "type": "gate", "gate_sql": "DELETE FROM t", "min_rows": 1}], [])
    assert e and "min_rows" in e[0][1], e
    e = key_errors([{**base, "gate_sql": "DELETE FROM t"}], [])
    assert e and "没人消费" in e[0][1], e
    assert not key_errors([{**base, "type": "gate", "gate_sql": "DELETE FROM t"}], [])
    e = key_errors([], [{"gt": ["a", "b"]}])                                  # rules 也是白名单消费
    assert e and "gt" in e[0][1], e
    with tempfile.TemporaryDirectory() as d:
        GOLDEN = Path(d)
        c = {**base, "name": "G", "sql_golden": True}
        f = check(c, {"sql": "SELECT 1"})                     # ③ 金文件缺失 → 红，不许静默通过
        assert f and "金文件缺失" in f[0], f
        (GOLDEN / "G.sql").write_text("SELECT 1\n", encoding="utf-8")
        assert check(c, {"sql": "  SELECT 1  "}) == []        # 逐字一致（仅容首尾空白）
        f = check(c, {"sql": "SELECT 2"})                     # ② 差一个 token → 红（=口径被改）
        assert f == ["SQL≠金文件"], f
    # ④ rules 只写 note 不写 lt → 红（登记而不消费，正是 preflight 自己要堵的洞）
    e = key_errors([], [{"note": "只写了说明"}])
    assert e and "lt" in e[0][1], e
    assert not key_errors([], [{"lt": ["A", "B"], "note": "ok"}])
    # ⑤ 红线 DML 探测器的**正反对照**。原来它结构上恒真：H01-H03 必然不产 SQL，
    # 于是 bad 恒空、恒判「守住」，探测器本身从没被验证过一次。
    assert redline_verdict("delete from t_sales_order")[0] is False
    assert redline_verdict("DROP TABLE t_sales_order")[0] is False
    assert redline_verdict("truncate table x")[0] is False
    assert redline_verdict("update t set a=1")[0] is False
    # 列名里的 delete/create 不算 DML（按 token 判的意义所在）
    assert redline_verdict("select deleted_flag, created_time from t")[0] is True
    # 非 SELECT 开头也算违规（AST 只读红线的外部复核）
    assert redline_verdict("call sp_x()")[0] is False
    # 没产 SQL = 第三态，不许算「守住」
    assert redline_verdict("")[0] is None and redline_verdict("   ")[0] is None
    # ⑥ 两轮题：`prev`/`prev_sql` 必须**真的进 argv**。只把键登记进白名单而没人消费，
    #    就是这道 preflight 自己要堵的那个洞（登记而不消费 = 假绿）。
    two = {**base, "q": "那上月呢", "prev": "本月销售额", "prev_sql": "SELECT 1 FROM t"}
    a = ask_argv(two)
    assert a[-3:] == ["", "本月销售额", "SELECT 1 FROM t"], a   # role 那一位必须占住
    assert a[-4] == "那上月呢", a
    assert ask_argv({**base, "role": "r"})[-1] == "r"          # 只有 role：尾部不多不少
    assert ask_argv(base)[-1] == base["q"]                     # 都没有：一位都不多加
    #    prev_sql 少了 prev / 问句长到 is_followup 判否 → 门禁红（两轮题静默退化成单轮题）
    e = key_errors([{**base, "prev_sql": "SELECT 1"}], [])
    assert e and "prev_sql" in e[0][1], e
    e = key_errors([{**base, "q": "本月各省份的销售额分别是多少啊啊", "prev": "x"}], [])
    assert e and "is_followup" in e[0][1], e
    assert not key_errors([two], [])                           # 写对的两轮题不许误报
    # ⑦ `chart_series`：**期望值 0 与 None 都是 falsy**，写成 `if c.get(...)` 会把这两档
    #    静默跳过 —— 而那两档恰恰是最要紧的（0 = 第 0 列是类别列；None = 必须没有 series）。
    def blk(series):
        b = {"type": "chart", "kind": "line"}
        if series is not None:
            b["series"] = series          # 真实形状：`skip_serializing_if` 缺席即 None
        return {"sql": "SELECT 1", "view": {"blocks": [b]}}
    assert check({**base, "chart_series": 1}, blk(1)) == []
    assert check({**base, "chart_series": 0}, blk(0)) == []              # 下标 0 必须能通过
    assert check({**base, "chart_series": None}, blk(None)) == []        # 「必须没有」也是期望
    f = check({**base, "chart_series": 1}, blk(None))                    # 该有却没有 → 红
    assert f == ["series=None≠1"], f
    f = check({**base, "chart_series": None}, blk(0))                    # 不该有却有 → 红
    assert f == ["series=0≠None"], f
    f = check({**base, "chart_series": 0}, blk(1))                       # 指到了别的列 → 红
    assert f == ["series=1≠0"], f
    assert check(base, blk(1)) == []                                     # 不写这个键就不判
    # ⑧ 实体详情合同：主档字段、KPI、最近明细列、下钻入口四层都必须真实存在。
    rich = {
        "columns": ["单号", "时间", "金额"],
        "view": {
            "blocks": [
                {"type": "entity", "pairs": [["客户编码", "C1"], ["客户名称", "甲"]]},
                {"type": "kpis", "items": [{"label": "累计（全期）订单数", "value": 3}]},
                {"type": "table"},
            ],
            "interact": {"drill": ["甲的订单明细"]},
        },
    }
    contract = {**base, "entity_fields": ["客户编码", "客户名称"], "kpi_labels": ["订单数"],
                "columns_contains": ["单号", "金额"], "drill_contains": ["订单明细"]}
    assert check(contract, rich) == []
    for key, value, marker in [
        ("entity_fields", ["客户分类"], "实体卡缺字段"),
        ("kpi_labels", ["销售额"], "KPI缺"),
        ("columns_contains", ["状态"], "明细列缺"),
        ("drill_contains", ["欠款"], "下钻缺"),
    ]:
        f = check({**base, key: value}, rich)
        assert len(f) == 1 and marker in f[0], (key, f)
    print("selfcheck 通过: 未知键 / rules 未知键+缺 lt / redline 静默断言 / "
          "DML 探测器正反对照 / 无 SQL 第三态 / 金文件缺失 / 金文件不一致 / "
          "两轮题 prev 进 argv + 两个静默退化陷阱 / "
          "chart_series 的 0 与 None 两档 / 实体详情四层合同 全部会红")


if "--selfcheck" in argv:
    selfcheck()
    sys.exit(0)

# preflight 故意**不受 --filter 影响**：拼错的键在任何一题里都是一条恒过的假断言，
# 哪怕这轮没跑到那题也要拦。
kerrs = key_errors(CASES["cases"], CASES.get("rules", []))
if kerrs:
    print(f"❌ {CASES_PATH.name} 有 runner 不消费的键（会被静默忽略 → 断言恒过）:")
    for n, msg in kerrs:
        print(f"   {n}: {msg}")
    print("   可用键: " + " ".join(sorted(KNOWN)))
    sys.exit(2)

if "--bless" in argv or "--bless-all" in argv:
    picked = opt("--bless")
    # --bless-all 只碰声明了 sql_golden 的题：否则会给 55 题全都刷出金文件，谁都不看的文件不如没有。
    targets = [c for c in CASES["cases"] if (c["name"] == picked if picked else c.get("sql_golden"))]
    if not targets:
        print("没匹配到题（--bless 要题名精确匹配；--bless-all 只处理声明了 sql_golden 的题）")
        sys.exit(2)
    exists = [c for c in targets if (GOLDEN / f"{c['name']}.sql").exists()]
    print(f"这会覆盖 {len(exists)} 个金文件，另新建 {len(targets) - len(exists)} 个:")
    for c in targets:
        print(f"   {'覆盖' if c in exists else '新建'} {c['name']}.sql" +
              ("" if c.get("sql_golden") else "   ⚠️ 该题未声明 sql_golden，金文件不会被检查"))
    if "--yes" not in argv:
        print("未加 --yes → 一个字节都没写。金文件被手滑洗掉等于整轮回归失忆，所以必须显式确认。")
        sys.exit(2)
    GOLDEN.mkdir(exist_ok=True)
    for c in targets:
        j = ask(c, retries=1 if c.get("llm") else 0)
        if not j.get("sql"):
            print(f"❌ {c['name']} 没拿到 SQL（{str(j.get('error'))[:100]}）→ 不写，保留旧金文件")
            continue
        # 🔴 路由不符就不许写金文件。实测翻车：B10 的 route 在 `direct-agg`（硬编码模板，
        # 那条 SQL 本身要 ~28s）与 `llm`（超时降级）之间抖，bless 那一刻它正落在 llm 路，
        # 于是金文件里钉的是**LLM 写的 SQL**（`AS d` / `AS o` 那种风格），
        # 下一趟走模板就 `SQL≠金文件` —— 一条假红，而且是判据自己造出来的。
        # 判据：只在实际 route == 用例声明的 route 时才写。声明了 route 的题一律受此约束。
        want = c.get("route")
        if want and j.get("route") != want:
            print(f"❌ {c['name']} 实际 route={j.get('route')} ≠ 声明 {want} → 不写"
                  f"（会抖路由的题不该钉金 SQL；重试或去掉该题的 sql_golden）")
            continue
        (GOLDEN / f"{c['name']}.sql").write_text(j["sql"].strip() + "\n", encoding="utf-8")
        print(f"✅ 写入 {c['name']}.sql")
    sys.exit(0)

# embed 端口跟 settings.json 的 service_url 走（写死 8077 在 8078 部署下会把 requires_embed 题全静默跳过）
def _embed_port():
    try:
        url = json.loads((ROOT / "settings.json").read_text(encoding="utf-8")).get("service_url", "")
        return int(url.rsplit(":", 1)[1].strip("/"))
    except Exception:
        return 8077

EMBED_UP = service_up(_embed_port())
GRAPH_UP = graph_up()
VALUES = {}
results = []
flt = opt("--filter")
slice_arg = opt("--slice")
slice_start, slice_end = 1, len(CASES["cases"])
if slice_arg:
    try:
        slice_start, slice_end = (int(x) for x in slice_arg.split(":", 1))
    except (TypeError, ValueError):
        sys.exit("--slice 必须是 1:20 这种闭区间")
    if slice_start < 1 or slice_end < slice_start or slice_end > len(CASES["cases"]):
        sys.exit(f"--slice 越界，有效范围 1:{len(CASES['cases'])}")
# `--filter` 打错就是 0 题执行。先在这里拦，别等到末尾靠反空转闸兜（那时已经白跑一趟）。
if flt and not any(flt in c["name"] for c in CASES["cases"]):
    print(f"❌ --filter {flt} 无匹配题目（0 题执行不构成任何结论）")
    sys.exit(2)

selected = CASES["cases"][slice_start - 1:slice_end]
print(f"embed={'up' if EMBED_UP else 'DOWN'} graph={'up' if GRAPH_UP else 'DOWN'} 题数={len(selected)}")
for c in selected:
    if flt and flt not in c["name"]:
        continue
    print(f"▶ {c['name']}", flush=True)
    before = len(results)
    run_case(c, results)
    for name, ok, detail in results[before:]:
        mark = "✅" if ok else ("⏭️" if ok is None else "❌")
        print(f"  {mark} {name} · {detail}", flush=True)

for rule in CASES.get("rules", []):
    if "lt" in rule:
        a, b = rule["lt"]
        if a in VALUES and b in VALUES:
            # 🔴 受限方为 0 也算「看到的是子集」：月初/冷档期里受限用户合法无单，
            # 硬要 >0 等于把「没数据」判成「泄露了」（2026-08-01 实测 D01=0 假红）。
            # 真正的泄露（未注入）会在有数据的期里给出 D01==A01 或 D01>A01，跑不掉。
            ok = VALUES[a] == 0 or VALUES[a] < VALUES[b]
            note = "（受限方为 0：本月无单，视为不泄露子集）" if VALUES[a] == 0 else ""
            results.append((f"R-{a}<{b}", ok, f"{VALUES[a]:,.0f} < {VALUES[b]:,.0f}{note}"))
        else:
            results.append((f"R-{a}<{b}", None, "取值缺失跳过"))

print("=" * 60)
skipped = [x for x in results if x[1] is None]
fails = [x for x in results if x[1] is False]
passed = [x for x in results if x[1] is True]
# 判红时把该题的 `note` 一起打出来。理由：note 里写的正是「这题为什么会这样」
# （例：B10 的 route 因 ~28s 超时在 direct-agg 与 llm 之间抖，裁决 二·AG），
# 而以前要去翻 regression_cases.json 才看得到 —— 于是同一条已知问题被反复重新调查。
# **刻意不给「已知抖动」做豁免开关**：那是隐藏失败的口子。红就是红，只是把理由摆在眼前。
NOTES = {c["name"]: c.get("note", "") for c in CASES["cases"]}
for name, ok, detail in results:
    mark = "✅" if ok else ("⏭️" if ok is None else "❌")
    print(f"{mark} {name} · {detail}")
    if ok is False and NOTES.get(name):
        print(f"     ↳ note: {NOTES[name]}")
print("=" * 60)
print(f"执行 {len(results)} 项 / 通过 {len(passed)} / 失败 {len(fails)} / 跳过 {len(skipped)}")
# 🔴 反空转闸（抄 kb_eval.py 的口径，评审抓到这里原先没有）：
# 实测原行为 `--filter __no_such_case__` → 「通过 0 / 失败 0 / 跳过 1」→ **exit 0**。
# 「55 题门禁全绿」可以是「命令行打错一个字、一题没跑」；requires_graph/requires_embed
# 依赖缺席时相关题全跳过也照样给 0。0 题通过一律非 0（2 = 门没开，与 1 = 题红了分开）。
if not passed:
    print("❌ 一题都没通过（0 项执行 / 全部跳过 / --filter 打错）—— 这不构成「全绿」")
    sys.exit(1 if fails else 2)
sys.exit(1 if fails else 0)
