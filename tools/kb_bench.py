# B8 知识库评估闭环：基准生成 → 检索指标 → 基线对比（调研依据 docs/research/yuxi.json「RAG 评估闭环」）。
#
# 三个子命令：
#   python tools/kb_bench.py generate [--docs 3] [--per-doc 3] [--per-chunk 2] [--out PATH]
#       从 kb.chunk 抽样（PG **只读**），用 LLM 生成「问题 + 应命中块证据」基准，
#       落 tools/kb_fixtures/kb_bench_cases.json（--out 可换）。
#   python tools/kb_bench.py run [--cases PATH] [--out PATH] [--k 6] [--login admin]
#                                [--judge] [--baseline PATH]
#       逐题走 POST /api/kb/search 原始 hits（与 kb_eval 同一入口），算
#       Recall@k / Precision@k / MRR，写基线报告（默认 tools/kb_bench_baseline.json）。
#       --baseline PATH：与旧报告逐题对比，列出提升/回退/新增/失效，有回退退出码 1。
#       --judge：可选 LLM 打分（问题+金块原文+top-k 命中摘要 → 0/1/2），均值进报告。
#   python tools/kb_bench.py selftest
#       无网自检：相关性判定/指标数学/对比逻辑/题集 schema 闸。不连库、不起服务。
#
# 检索身份（与 kb_eval.py 同一套约定）：
#   DMSAI_KB_TOKEN     会话 token，Bearer 直用；
#   DMSAI_KB_PASSWORD  账号密码，仅用于 POST /api/login 换 token（不打印、不落盘）；
#   都没有则按 login_name 裸调 —— 只在服务端开了 insecure_login_fallback 的本机判官
#   实例上才过得了 401（serve.ps1 有这条的警告）。
# 环境变量：DMSAI_BASE（默认 http://127.0.0.1:8100）。
#
# 退出码（对齐 kb_eval.py 的反空转闸：「一题没跑」绝不许是 0）：
#   0 = 真跑了 ≥1 题并完成报告；对比模式额外要求 0 回退
#   1 = 对比模式发现回退项
#   2 = 门没开：依赖缺席、认证失败、题集/金块失效、LLM 全灭、0 题执行
#
# 口径（改动前先读，免得把「口径变了」当「指标涨了」）：
# - 金块：每题 1 个证据块（generate 从 kb.chunk 抽样产生）。重新入库会换 chunk_id/ord，
#   题集即失效 —— run 时发现金块在 PG 里查不到，标记 stale 并剔除（全 stale → 退出 2）。
# - 相关性：命中块与金块同文档，且 金块.ord ∈ [命中.ord, 命中.ord + span)。span 是检索
#   合并的连续块数（retrieve.rs merge_adjacent），ord 从 PG 只读批量回查 —— 用 chunk_id
#   数值区间是错的（重入库后 id 不连续），这是本工具只许 ord 口径的原因。
# - Recall@k：金块被 top-k 覆盖即 1（单金块），宏平均。Precision@k = 相关命中数 /
#   min(k, 实际返回数) —— 返回不足 k 条不按 k 罚（引擎 TOP_K=6 恒截，见 retrieve.rs）。
# - MRR：首个相关命中的名次倒数（返回榜全长内，返回榜即 top-6）。
# - judge：0=无关 1=部分 2=足够，报告存 mean/2。它是补充信号，不进对比闸。
#
# 红线：
# - 不连生产 MySQL（本体只 import psycopg2；身份由服务端自己验）。
# - PG 只读：连接即 set_session(readonly=True)，全程只有 SELECT。
# - LLM key 经 tools/settings.py 从 settings.json 读，不打印、不写进任何输出文件。
import hashlib
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

# 同 kb_eval.py：Windows 控制台默认 GBK，Emoji/特殊字符一崩报告就不可读。
sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))
from settings import load as load_settings, pg_kwargs  # noqa: E402  凭据唯一入口，不落本文件

FIXTURES = ROOT / "tools" / "kb_fixtures"
CASES_DEFAULT = FIXTURES / "kb_bench_cases.json"
REPORT_DEFAULT = ROOT / "tools" / "kb_bench_baseline.json"
BASE = os.environ.get("DMSAI_BASE", "http://127.0.0.1:8100")
SEARCH_PATH = "/api/kb/search"
TOP_K_ENGINE = 6  # retrieve.rs TOP_K：端点恒截 6 条，--k 超过它没有意义
CASES_VERSION = 1
REPORT_VERSION = 1


def _opt(name, default=None):
    """取一个 `--x <值>` 参数。缺值或取值以 `--` 开头一律当场退出（同 regression.py/kb_eval.py 那道闸）。"""
    if name not in sys.argv:
        return default
    i = sys.argv.index(name) + 1
    if i >= len(sys.argv) or sys.argv[i].startswith("--"):
        sys.exit(f"{name} 后面缺少取值")
    return sys.argv[i]


# 🔴 未知参数硬失败（house style）：打错一个参数 → 跑了别的东西 → 报绿，与「跑绿但没测」同一种坏。
_KNOWN_FLAGS = {
    "--help", "-h", "--docs", "--per-doc", "--per-chunk", "--min-chars", "--max-cases",
    "--doc-id", "--out", "--cases", "--k", "--login", "--judge", "--baseline",
}
_bad = [a for a in sys.argv[1:] if a.startswith("-") and a not in _KNOWN_FLAGS]
if _bad:
    sys.exit(f"未知参数 {_bad}；可用：{' '.join(sorted(_KNOWN_FLAGS))}")


def _int_opt(name, default, lo, hi):
    raw = _opt(name)
    if raw is None:
        return default
    try:
        v = int(raw)
    except ValueError:
        sys.exit(f"{name} 必须是整数，拿到 {raw!r}")
    if not lo <= v <= hi:
        sys.exit(f"{name} 必须在 [{lo},{hi}]，拿到 {v}")
    return v


def gate_fail(msg):
    """门没开（依赖缺席/认证失败/题集失效/0 题执行）：退出 **2**，与用法错误（1）和
    「对比发现回退」（1）分开 —— 归因不同，混在一起没法定位（kb_eval.py 同款约定）。"""
    print(msg)
    raise SystemExit(2)


# ---------------------------------------------------------------- 基础件

def req(method, path, body=None, ctype=None, timeout=120, token=None):
    """HTTP 调用形状同 kb_eval.py：返回 (status, json)；网络错误 status=0。绝不打印 token。"""
    r = urllib.request.Request(BASE + path, data=body, method=method)
    if ctype:
        r.add_header("Content-Type", ctype)
    if token:
        r.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read().decode("utf-8") or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, {"error": raw[:300]}
    except json.JSONDecodeError as e:
        return 0, {"error": f"响应不是 JSON: {e}"}
    except (urllib.error.URLError, OSError, TimeoutError) as e:
        return 0, {"error": str(e)}


def post(path, obj, timeout=120, token=None):
    body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
    return req("POST", path, body, "application/json", timeout, token)


def tcp_up(host, port):
    try:
        socket.create_connection((host, port), timeout=2).close()
        return True
    except OSError:
        return False


def pg_readonly():
    """自有 PG 只读连接。import 挪进函数：selftest 不许因为缺驱动而崩（它本来就不该碰库）。"""
    import psycopg2
    conn = psycopg2.connect(connect_timeout=5, **pg_kwargs())
    # 双保险：会话只读 + 语句超时。本工具全程只有 SELECT，写出错就是 bug，让它当场红。
    conn.set_session(readonly=True, autocommit=True)
    with conn.cursor() as cur:
        cur.execute("SET statement_timeout = 20000")
    return conn


def llm_chat(system, user, timeout=120):
    """经 settings.json 的 fast 模型打一次 chat/completions（urllib，零新依赖）。

    → (content, error)。key 只经 settings 入口读，不进任何输出。"""
    cfg = load_settings()
    base = str(cfg.get("llm_base_url") or "").rstrip("/")
    key = str(cfg.get("llm_api_key") or "").strip()
    model = str(cfg.get("llm_model_fast") or "").strip()
    if not (base and key and model):
        return None, "settings.json 缺 llm_base_url/llm_api_key/llm_model_fast"
    body = {"model": model, "temperature": 0.2,
            "response_format": {"type": "json_object"},
            "messages": [{"role": "system", "content": system},
                         {"role": "user", "content": user}]}
    r = urllib.request.Request(
        f"{base}/chat/completions", data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            v = json.load(resp)
        return (v["choices"][0]["message"].get("content") or "").strip(), None
    except urllib.error.HTTPError as e:
        return None, f"LLM HTTP {e.code}: {e.read().decode('utf-8', 'replace')[:150]}"
    except (urllib.error.URLError, OSError, TimeoutError, json.JSONDecodeError, KeyError) as e:
        return None, f"LLM 调用失败：{type(e).__name__}: {e}"


# ---------------------------------------------------------------- generate

GEN_SYSTEM = (
    "你是知识库检索基准的出题器。给定一个文档块（来自企业知识库），提出用户真实会问的问题。"
    "硬性要求：①答案必须能从该块内容直接得出；②问题具体，含块内的关键实体/数字/条件/步骤名；"
    "③像真实用户的口语化提问，不得照抄原文长句；④不要「这篇文档讲了什么」这类泛问题；"
    "⑤只输出 JSON：{\"questions\": [\"...\", ...]}，数量按要求。"
)


def sample_docs(conn, n_docs, doc_ids):
    """→ [(doc_id, name, space_id)]。只取可检索的文档（embedded 且启用），与检索可见集一致。"""
    with conn.cursor() as cur:
        if doc_ids:
            cur.execute(
                "SELECT doc_id, name, space_id FROM kb.doc"
                " WHERE doc_id = ANY(%s) AND status='embedded' AND enabled",
                (doc_ids,))
            rows = cur.fetchall()
            missing = set(doc_ids) - {r[0] for r in rows}
            if missing:
                gate_fail(f"❌ --doc-id 里有非 embedded/启用 的文档：{sorted(missing)}")
            return rows
        cur.execute(
            "SELECT doc_id, name, space_id FROM kb.doc"
            " WHERE status='embedded' AND enabled ORDER BY updated_at DESC LIMIT %s",
            (n_docs,))
        return cur.fetchall()


def sample_chunks(conn, doc_id, per_doc, min_chars):
    """按 ord 均匀抽样（确定性，复跑同库必出同题集），跳过过短块（没东西可问）。"""
    with conn.cursor() as cur:
        cur.execute(
            "SELECT chunk_id, ord, heading_path, text FROM kb.chunk"
            " WHERE doc_id=%s AND length(text) >= %s ORDER BY ord",
            (doc_id, min_chars))
        rows = cur.fetchall()
    if not rows:
        return []
    if len(rows) <= per_doc:
        return rows
    step = len(rows) / per_doc
    return [rows[int(i * step)] for i in range(per_doc)]


def gen_questions(doc_name, heading, text, n):
    """一个块 → n 个问题；→ (questions, error)。LLM 输出坏 JSON 不算系统错，按空题处理。"""
    user = f"文档：{doc_name}\n章节：{heading or '正文'}\n\n{text[:1800]}\n\n请提出 {n} 个问题。"
    content, err = llm_chat(GEN_SYSTEM, user)
    if err:
        return [], err
    try:
        v = json.loads(content)
        qs = v.get("questions")
        if not isinstance(qs, list):
            return [], f"LLM 输出缺 questions 数组：{content[:120]}"
        return [str(q).strip() for q in qs if str(q).strip()], None
    except json.JSONDecodeError:
        return [], f"LLM 输出不是 JSON：{content[:120]}"


def case_id(doc_id, question):
    """稳定 id：同文档同问题跨轮同 id —— 前后对比靠它逐题对齐。"""
    return hashlib.sha1(f"{doc_id}|{question}".encode("utf-8")).hexdigest()[:12]


def question_ok(question, text):
    """题质量闸：过短/过长/整句照抄原文的题不进基准（那是背答案，不是考检索）。"""
    if not 8 <= len(question) <= 80:
        return False
    return question not in text


def cmd_generate():
    n_docs = _int_opt("--docs", 3, 1, 20)
    per_doc = _int_opt("--per-doc", 3, 1, 20)
    per_chunk = _int_opt("--per-chunk", 2, 1, 5)
    min_chars = _int_opt("--min-chars", 80, 20, 2000)
    max_cases = _int_opt("--max-cases", 30, 1, 200)
    out = Path(_opt("--out") or CASES_DEFAULT)
    doc_ids = [s for s in (_opt("--doc-id") or "").split(",") if s.strip()] or None

    conn = pg_readonly()
    docs = sample_docs(conn, n_docs, doc_ids)
    if not docs:
        gate_fail("❌ kb.doc 里没有 embedded 且启用的文档 —— 0 题生成，先入库语料")
    print(f"基准文档 {len(docs)} 篇：{[d[1] for d in docs]}")

    cases, llm_fail, seen = [], 0, set()
    for doc_id, name, _space in docs:
        chunks = sample_chunks(conn, doc_id, per_doc, min_chars)
        if not chunks:
            print(f"⚠️ {name} 没有长度 ≥{min_chars} 的块，跳过")
            continue
        for chunk_id, ord_, heading, text in chunks:
            if len(cases) >= max_cases:
                break
            qs, err = gen_questions(name, heading, text, per_chunk)
            if err:
                llm_fail += 1
                print(f"⚠️ {name} 块#{chunk_id} 出题失败：{err}")
                continue
            kept = 0
            for q in qs:
                if kept >= per_chunk or len(cases) >= max_cases:
                    break
                if not question_ok(q, text) or q in seen:
                    continue
                seen.add(q)
                kept += 1
                cases.append({
                    "id": case_id(doc_id, q),
                    "question": q,
                    "doc_id": doc_id,
                    "doc_name": name,
                    "gold_chunk_id": chunk_id,
                    "gold_ord": ord_,
                    "gold_heading": heading or "",
                    "gold_preview": text[:120],
                })
            print(f"  {name} 块#{chunk_id}（ord {ord_}）→ 出题 {kept}/{len(qs)}")
    conn.close()

    # 反空转：LLM 全灭/题全被闸掉而写出 0 题基准，等于给后续 run 发空头支票。
    if not cases:
        gate_fail(f"❌ 0 题生成（LLM 失败 {llm_fail} 次）——不写基准文件，先修上游")
    spec = {
        "version": CASES_VERSION,
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "generator": {"model": "settings.llm_model_fast", "docs": [d[1] for d in docs]},
        "cases": cases,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"✅ 基准 {len(cases)} 题 → {out}（LLM 失败 {llm_fail} 次）")
    return 0


# ---------------------------------------------------------------- run：检索与指标

def auth_token(login):
    """→ (token 或 None, error 或 None)。三套约定与 kb_eval.py 完全一致。"""
    token = os.environ.get("DMSAI_KB_TOKEN", "").strip()
    if token:
        return token, None
    password = os.environ.get("DMSAI_KB_PASSWORD", "")
    if password:
        code, j = post("/api/login", {"login_name": login, "password": password}, timeout=30)
        if code == 200 and j.get("token"):
            return j["token"], None
        return None, f"/api/login 失败 HTTP {code}: {str(j.get('error'))[:80]}"
    return None, None  # 裸 login_name：只在开了 insecure_login_fallback 的本机实例上能过


def probe_auth(login, token):
    """认证探针：真打一发 search。401/403 → 门没开（退出 2），不许带着坏身份跑完全程再猜。"""
    code, j = post(SEARCH_PATH, {"question": "基准连通性探针", "login_name": login},
                   timeout=60, token=token)
    if code == 200:
        return None
    if code in (401, 403):
        return (f"{SEARCH_PATH} 认证失败（HTTP {code}）：设 DMSAI_KB_TOKEN，"
                "或 DMSAI_KB_PASSWORD（换 token），或对本机判官实例开 insecure_login_fallback")
    return f"{SEARCH_PATH} 探针 HTTP {code}: {str(j.get('error'))[:120]}"


def search_hits(question, login, token):
    """→ (hits, error)。hits 原样取自端点（含 chunk_id/doc_id/span/score/preview）。"""
    code, j = post(SEARCH_PATH, {"question": question, "login_name": login},
                   timeout=90, token=token)
    if code != 200:
        return None, f"HTTP {code}: {str(j.get('error'))[:90]}"
    hits = j.get("hits")
    if not isinstance(hits, list):
        return None, "响应缺 hits 数组"
    return hits, None


def chunk_ords(conn, chunk_ids):
    """批量回查 chunk_id → (doc_id, ord)。只读；查不到的（已删块）不进表。"""
    if not chunk_ids:
        return {}
    with conn.cursor() as cur:
        cur.execute(
            "SELECT chunk_id, doc_id, ord FROM kb.chunk WHERE chunk_id = ANY(%s)",
            (list({int(c) for c in chunk_ids}),))
        return {r[0]: (r[1], r[2]) for r in cur.fetchall()}


def relevant(hit, gold_doc_id, gold_ord, ords):
    """相关性判据（口径见文件头）：同文档 + 金块 ord 落在该命中的合并跨度内。

    命中块的 ord 查不到（返回后被删）→ 按不相关计，fail-closed，不许放水。"""
    if not isinstance(hit, dict):
        return False
    info = ords.get(hit.get("chunk_id"))
    if info is None:
        return False
    hit_doc, hit_ord = info
    span = hit.get("span") or 1
    return hit_doc == gold_doc_id and hit_ord <= gold_ord < hit_ord + span


def case_metrics(flags, k, n_hits):
    """flags = top-k 内各命中是否相关（单金块基准下 Recall@k=any(flags)）。"""
    topk = flags[:k]
    recall = 1.0 if any(topk) else 0.0
    denom = min(k, n_hits)
    precision = (sum(1 for f in topk if f) / denom) if denom else 0.0
    rr = 0.0
    for i, f in enumerate(flags, 1):
        if f:
            rr = 1.0 / i
            break
    rank = next((i for i, f in enumerate(flags, 1) if f), None)
    return {"recall@k": recall, "precision@k": round(precision, 4),
            "rr": round(rr, 4), "first_relevant_rank": rank}


def aggregate(rows):
    """→ 宏平均指标。rows 为空不许算出 0.0 冒充成绩 —— 调用方先保证非空（反空转闸）。"""
    n = len(rows)
    out = {
        "recall@k": round(sum(r["recall@k"] for r in rows) / n, 4),
        "precision@k": round(sum(r["precision@k"] for r in rows) / n, 4),
        "mrr": round(sum(r["rr"] for r in rows) / n, 4),
    }
    judged = [r["judge"]["score"] for r in rows if r.get("judge")]
    if judged:
        out["judge_mean"] = round(sum(judged) / len(judged) / 2, 4)
        out["judge_cases"] = len(judged)
    return out


JUDGE_SYSTEM = (
    "你是检索结果评审。给定用户问题、金标准证据块原文、以及检索系统返回的 top 命中摘要，"
    "判检索是否把能回答该问题的证据找回来了：0=命中与问题无关；1=部分相关但不足以回答；"
    "2=命中足以回答（通常应包含金块要点）。只输出 JSON：{\"score\": 0|1|2, \"reason\": \"≤40字\"}。"
)


def judge_case(question, gold_text, hits):
    top = "\n".join(f"[{i}] {h.get('doc_name')}：{h.get('preview', '')}"
                    for i, h in enumerate(hits[:TOP_K_ENGINE], 1))
    user = f"问题：{question}\n\n金标准证据：\n{gold_text[:800]}\n\n检索 top 命中：\n{top or '（无命中）'}"
    content, err = llm_chat(JUDGE_SYSTEM, user)
    if err:
        return None, err
    try:
        v = json.loads(content)
        score = int(v.get("score"))
        if score not in (0, 1, 2):
            raise ValueError
        return {"score": score, "reason": str(v.get("reason", ""))[:80]}, None
    except (json.JSONDecodeError, TypeError, ValueError):
        return None, f"judge 输出非法：{content[:100]}"


CASE_KEYS = {"id", "question", "doc_id", "doc_name", "gold_chunk_id", "gold_ord",
             "gold_heading", "gold_preview"}


def load_cases(path):
    """题集 schema 闸：runner 不消费的键当场红（kb_eval.py validate_spec 同款理由）。"""
    spec = json.loads(Path(path).read_text(encoding="utf-8"))
    if spec.get("version") != CASES_VERSION or not isinstance(spec.get("cases"), list):
        gate_fail(f"❌ {path} 不是 version={CASES_VERSION} 的 kb_bench 题集")
    errors = []
    for c in spec["cases"]:
        unknown = set(c) - CASE_KEYS
        if unknown:
            errors.append(f"{c.get('id', '?')}: 未登记键 {sorted(unknown)}")
        need = ["id", "question", "doc_id", "gold_chunk_id", "gold_ord"]
        if any(c.get(k) is None for k in need):
            errors.append(f"{c.get('id', '?')}: 缺必填键 {need}")
    if errors:
        for e in errors:
            print(f"❌ 题集契约错误：{e}")
        sys.exit(2)
    return spec


def cmd_run():
    cases_path = Path(_opt("--cases") or CASES_DEFAULT)
    out = Path(_opt("--out") or REPORT_DEFAULT)
    k = _int_opt("--k", 6, 1, TOP_K_ENGINE)
    login = _opt("--login") or "admin"
    use_judge = "--judge" in sys.argv
    baseline_path = _opt("--baseline")
    if not cases_path.exists():
        gate_fail(f"❌ 题集不存在：{cases_path}（先跑 generate）")
    spec = load_cases(cases_path)

    from urllib.parse import urlparse
    u = urlparse(BASE)
    if not tcp_up(u.hostname or "127.0.0.1", u.port or 80):
        gate_fail(f"❌ 服务未起（{BASE}）——0 题执行")
    token, err = auth_token(login)
    if err:
        gate_fail(f"❌ {err}")
    err = probe_auth(login, token)
    if err:
        gate_fail(f"❌ {err}")

    conn = pg_readonly()
    # 金块活性核验：重入库会换 chunk_id/ord，旧题集不许再产出「指标」。
    gold_ids = [c["gold_chunk_id"] for c in spec["cases"]]
    gold_live = chunk_ords(conn, gold_ids)
    stale = [c for c in spec["cases"] if c["gold_chunk_id"] not in gold_live]
    live = [c for c in spec["cases"] if c["gold_chunk_id"] in gold_live]
    for c in stale:
        print(f"⚠️ 金块失效（重入库过？）：{c['id']} {c['question'][:40]} —— 剔除，重跑 generate")
    if not live:
        gate_fail("❌ 全部题目金块失效 —— 0 题执行，重跑 generate")

    rows, errors = [], 0
    for i, c in enumerate(live, 1):
        hits, err = search_hits(c["question"], login, token)
        if err:
            errors += 1
            print(f"❌ [{i}/{len(live)}] {c['question'][:40]}：检索失败 {err}")
            continue
        ords = chunk_ords(conn, [h.get("chunk_id") for h in hits])
        flags = [relevant(h, c["doc_id"], c["gold_ord"], ords) for h in hits]
        m = case_metrics(flags, k, len(hits))
        row = {
            "id": c["id"], "question": c["question"], "doc_name": c["doc_name"],
            "gold_chunk_id": c["gold_chunk_id"], **m,
            "hits_returned": len(hits),
            "hits": [{"rank": i, "chunk_id": h.get("chunk_id"), "doc_name": h.get("doc_name"),
                      "score": h.get("score"), "relevant": f}
                     for i, (h, f) in enumerate(zip(hits, flags), 1)],
        }
        if use_judge:
            with conn.cursor() as cur:
                cur.execute("SELECT text FROM kb.chunk WHERE chunk_id=%s", (c["gold_chunk_id"],))
                gold_text = (cur.fetchone() or [""])[0]
            j, jerr = judge_case(c["question"], gold_text, hits)
            if j:
                row["judge"] = j
            else:
                print(f"⚠️ judge 失败（{c['id']}）：{jerr}")
        rows.append(row)
        print(f"[{i}/{len(live)}] rank={m['first_relevant_rank']} rr={m['rr']} "
              f"{c['question'][:40]}")
    conn.close()

    if not rows:
        gate_fail(f"❌ 0 题跑成（检索错误 {errors} 次）——不写报告")
    if use_judge and not any(r.get("judge") for r in rows):
        gate_fail("❌ --judge 指定了但 0 题判成 —— judge 口径缺席，不许假装带过")

    report = {
        "meta": {
            "version": REPORT_VERSION, "tool": "tools/kb_bench.py",
            "created_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "base": BASE, "source": SEARCH_PATH, "login": login, "k": k,
            "cases_path": str(cases_path), "cases_total": len(spec["cases"]),
            "cases_stale": len(stale), "cases_run": len(rows), "search_errors": errors,
            "judge": use_judge,
        },
        "metrics": aggregate(rows),
        "per_case": rows,
    }
    out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"✅ 报告 → {out}")
    print(f"指标：{json.dumps(report['metrics'], ensure_ascii=False)}")

    if baseline_path:
        reg = compare(Path(baseline_path), report)
        return 1 if reg else 0
    return 0


# ---------------------------------------------------------------- 对比

def compare(old_path, new_report):
    """逐题对齐（case id）列提升/回退。→ 回退列表（非空即调用方给退出码 1）。

    只钉 first_relevant_rank：单金块基准下 recall@k 与「rank 是否 ≤k」等价，
    rr 是 rank 的单调函数 —— 一个 rank 就是该题的全部信息，不多造第二份真相。"""
    old = json.loads(old_path.read_text(encoding="utf-8"))
    old_cases = {c["id"]: c for c in old.get("per_case", [])}
    new_cases = {c["id"]: c for c in new_report.get("per_case", [])}
    if not old_cases:
        gate_fail(f"❌ 基线 {old_path} 没有 per_case —— 不是本工具的报告，对比中止")

    improved, regressed, added, missing = [], [], [], []
    for cid, cur in new_cases.items():
        prev = old_cases.get(cid)
        if prev is None:
            added.append(cid)
            continue
        r_old, r_new = prev.get("first_relevant_rank"), cur.get("first_relevant_rank")
        if r_old is None and r_new is not None:
            improved.append((cid, "未命中", r_new, cur))
        elif r_old is not None and r_new is None:
            regressed.append((cid, r_old, "未命中", cur))
        elif r_old is not None and r_new is not None and r_new < r_old:
            improved.append((cid, r_old, r_new, cur))
        elif r_old is not None and r_new is not None and r_new > r_old:
            regressed.append((cid, r_old, r_new, cur))
    for cid in set(old_cases) - set(new_cases):
        missing.append(cid)

    om, nm = old.get("metrics", {}), new_report.get("metrics", {})
    print("=" * 60)
    print(f"基线 {old_path.name} → 本轮（共 {len(new_cases)} 题）")
    for key in ("recall@k", "precision@k", "mrr", "judge_mean"):
        if key in om and key in nm:
            print(f"  {key}: {om[key]} → {nm[key]}（Δ{round(nm[key] - om[key], 4):+}）")
    for tag, items in (("提升", improved), ("回退", regressed)):
        for cid, a, b, cur in items:
            print(f"  {'🟢' if tag == '提升' else '🔴'} {tag} [{cid}] rank {a} → {b}：{cur['question'][:40]}")
    print(f"提升 {len(improved)} / 回退 {len(regressed)} / 新增 {len(added)} / 失效 {len(missing)}")
    if added or missing:
        print("⚠️ 两轮题集不完全相同，指标均值只在交集上可比")
    return regressed


# ---------------------------------------------------------------- selftest

def selftest():
    """无网自检：判定/数学/对比三段。不连库、不起服务、不调 LLM。"""
    ords = {101: ("d1", 5), 102: ("d1", 6), 103: ("d1", 7), 900: ("d2", 1)}
    gold_doc, gold_ord = "d1", 6
    # ① 相关性：合并跨度覆盖 / 同文档跨度外 / 跨文档 / 查不到 ord 一律不相关
    assert relevant({"chunk_id": 101, "span": 3}, gold_doc, gold_ord, ords)      # 覆盖 5..7
    assert not relevant({"chunk_id": 101, "span": 1}, gold_doc, gold_ord, ords)  # 只有 5
    assert not relevant({"chunk_id": 103, "span": 1}, gold_doc, gold_ord, ords)  # 7 不含 6
    assert not relevant({"chunk_id": 900, "span": 16}, gold_doc, gold_ord, ords)  # 别的文档
    assert not relevant({"chunk_id": 555, "span": 3}, gold_doc, gold_ord, ords)  # 已删块
    assert not relevant({"chunk_id": None}, gold_doc, gold_ord, ords)

    # ② 指标数学：rank2 命中 → recall 1、rr 1/2、precision=1/min(k,返回数)
    m = case_metrics([False, True, False], 6, 3)
    assert m == {"recall@k": 1.0, "precision@k": round(1 / 3, 4),
                 "rr": 0.5, "first_relevant_rank": 2}, m
    m = case_metrics([], 6, 0)
    assert m["recall@k"] == 0.0 and m["precision@k"] == 0.0 and m["rr"] == 0.0
    assert m["first_relevant_rank"] is None
    m = case_metrics([True], 6, 1)          # 只返回 1 条且相关 → precision 不按 k=6 罚
    assert m["precision@k"] == 1.0 and m["rr"] == 1.0
    agg = aggregate([{"recall@k": 1.0, "precision@k": 1.0, "rr": 1.0},
                     {"recall@k": 0.0, "precision@k": 0.0, "rr": 0.0}])
    assert agg == {"recall@k": 0.5, "precision@k": 0.5, "mrr": 0.5}, agg
    agg = aggregate([{"recall@k": 1.0, "precision@k": 1.0, "rr": 1.0,
                      "judge": {"score": 2, "reason": ""}}])
    assert agg["judge_mean"] == 1.0 and agg["judge_cases"] == 1

    # ③ 题质量闸与稳定 id
    assert question_ok("报销审批要走哪些流程？", "正文")
    assert not question_ok("短", "正文")
    assert not question_ok("x" * 81, "正文")
    assert not question_ok("整句照抄", "开头整句照抄结尾")
    assert case_id("d1", "q") == case_id("d1", "q") and case_id("d1", "q") != case_id("d1", "p")

    # ④ 对比：未命中↔命中、名次进退、新增/失效
    old = {"metrics": {"recall@k": 0.5, "precision@k": 0.5, "mrr": 0.5},
           "per_case": [{"id": "a", "question": "qa", "first_relevant_rank": 1},
                        {"id": "b", "question": "qb", "first_relevant_rank": None},
                        {"id": "c", "question": "qc", "first_relevant_rank": 2},
                        {"id": "gone", "question": "qg", "first_relevant_rank": 1}]}
    new = {"metrics": {"recall@k": 1.0}, "per_case": [
        {"id": "a", "question": "qa", "first_relevant_rank": 1},
        {"id": "b", "question": "qb", "first_relevant_rank": 3},
        {"id": "c", "question": "qc", "first_relevant_rank": 5},
        {"id": "new1", "question": "qn", "first_relevant_rank": 1}]}
    import tempfile
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
        json.dump(old, f)
        old_path = f.name
    try:
        reg = compare(Path(old_path), new)
    finally:
        os.unlink(old_path)
    assert [r[0] for r in reg] == ["c"], reg          # 2→5 是回退；a 持平、b 0→3 提升
    print("selftest ok")


# ---------------------------------------------------------------- main

def help_text():
    """帮助＝本文件顶部注释块（单一事实源，别复述）。"""
    out = []
    for ln in Path(__file__).read_text(encoding="utf-8").splitlines():
        if not ln.startswith("#"):
            break
        out.append(ln[1:].strip())
    return "\n".join(out)


_VALUE_FLAGS = _KNOWN_FLAGS - {"--help", "-h", "--judge"}


def positional_args():
    """位置参数（子命令）：旗标的取值不算。`--out x` 的 x 与 `generate` 都是非 `-` 开头，
    不剔除取值就会把 `run --k 6` 看成两个位置参数。"""
    out, skip = [], False
    for a in sys.argv[1:]:
        if skip:
            skip = False
            continue
        if a in _VALUE_FLAGS:
            skip = True           # _opt 已闸过「缺值当场退出」，这里放心跳
            continue
        if a.startswith("-"):
            continue              # 布尔旗标（--judge/--help），_KNOWN_FLAGS 已闸
        out.append(a)
    return out


def main():
    if "--help" in sys.argv or "-h" in sys.argv:
        print(help_text())
        return 0
    modes = positional_args()
    if len(modes) != 1 or modes[0] not in ("generate", "run", "selftest"):
        sys.exit("用法：kb_bench.py generate|run|selftest（--help 看全量说明）")
    if modes[0] == "selftest":
        selftest()
        return 0
    if modes[0] == "generate":
        return cmd_generate()
    return cmd_run()


if __name__ == "__main__":
    sys.exit(main())
