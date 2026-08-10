# B3 一次性量具：recall@20 vs recall@6 —— 「检索融合后要不要加 LLM 重排」的量化裁决。
#
# 计划原文（2026-07-31-three-framework-integration.md B3）：先量 recall@20 与 @6 的差，
# **只有当 @20 明显高于 @6，重排才有收益**，否则这条直接毙掉，省一整个功能。
#
# 方法：离线复刻 `knowledge/src/retrieve.rs` 的三路召回（VEC/FTS/TRGM 的 SQL 逐字照抄常量）
# + RRF 融合（k=60），对 kb_eval 主题题逐题算「答案所在文档的块在融合榜上的最好名次」：
#   名次 ≤6   → 现状已命中（重排无收益）
#   名次 7..20 → 真相关但被挤出 TOP_K（重排**能**救 —— 收益真实存在）
#   名次 >20 / 三路飞榜外 → 召回层的问题，重排救不了（别拿重排当召回的补丁）
#
# 口径声明：探针**不过 ACL**（可见集合 = 全库）。ACL 只会删文档、让金块名次更靠前，
# 所以这里的名次是**上界** —— 上界都在 7..20 的题，真实排名只会更好，重排收益只会更小。
# 即：本探针若判「毙」，结论在真实 ACL 下依然成立；若判「做」，收益是保守估计。
#
# 用法: python tools/kb_rerank_probe.py
# 环境: embed(:8077) 与 PG(:15433) 在线；kb_eval 的夹具已入库（跑过 kb_eval 即为真）。
import json, sys, urllib.request
from pathlib import Path

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))
from settings import pg_kwargs  # noqa: E402  （tools/settings.py 统一读 settings.json，明文口令不落本文件）

EMBED = "http://127.0.0.1:8077/embed"
# ↓ 三路 SQL 与 RRF 参数**逐字照抄** retrieve.rs（改那边要同步这边，一次性量具不做抽象）
VEC_TOP, FTS_TOP, TRGM_TOP, TRGM_MIN, VEC_MAX_DIST, RRF_K, TOP_K = 20, 20, 10, 0.2, 0.55, 60.0, 6
VEC_SQL = ("SELECT chunk_id FROM kb.chunk WHERE embedding IS NOT NULL "
           "AND (embedding <=> %s::vector) < %s ORDER BY embedding <=> %s::vector LIMIT %s")
FTS_SQL = ("SELECT chunk_id FROM kb.chunk WHERE ts @@ plainto_tsquery('simple', %s) "
           "ORDER BY ts_rank_cd(ts, plainto_tsquery('simple', %s)) DESC, chunk_id LIMIT %s")
TRGM_SQL = ("SELECT chunk_id FROM kb.chunk WHERE word_similarity(%s, text) > %s "
            "ORDER BY word_similarity(%s, text) DESC, chunk_id LIMIT %s")


def embed_query(text):
    body = json.dumps({"texts": [text], "query": True}).encode()
    r = urllib.request.urlopen(urllib.request.Request(EMBED, body, {"Content-Type": "application/json"}), timeout=10)
    v = json.loads(r.read())["embeddings"][0]
    return "[" + ",".join(f"{x:.6f}" for x in v) + "]"  # to_pgvector 同款 6 位小数


def rrf(lists):
    score, order = {}, []
    for lst in lists:
        for i, cid in enumerate(lst):
            if cid not in score:
                order.append(cid)
            score[cid] = score.get(cid, 0.0) + 1.0 / (RRF_K + i + 1)
    order.sort(key=lambda c: (-score[c], c))  # 与 retrieve.rs 同：分数降序、同分按 chunk_id
    return order


def main():
    import psycopg2
    cases = [c for c in json.load(open(ROOT / "tools" / "kb_eval_cases.json", encoding="utf-8"))["cases"]
             if c.get("kind") == "recall"]
    pg = psycopg2.connect(**pg_kwargs())
    cur = pg.cursor()
    # 夹具名 → 该文档全部 chunk_id（金块集合；答案在文档的哪一块事先不知道，取最好名次）
    cur.execute("SELECT name, doc_id FROM kb.doc")
    docs = {}
    for name, doc_id in cur.fetchall():
        cur2 = pg.cursor()
        cur2.execute("SELECT chunk_id FROM kb.chunk WHERE doc_id = %s", (doc_id,))
        docs[name] = {r[0] for r in cur2.fetchall()}
    rows = []
    for c in cases:
        gold = docs.get(c["fixture"], set())
        if not gold:
            rows.append((c["name"], "夹具未入库", -1))
            continue
        q = c["question"]
        vlit = embed_query(q)
        cur.execute(VEC_SQL, (vlit, VEC_MAX_DIST, vlit, VEC_TOP))
        vec = [r[0] for r in cur.fetchall()]
        cur.execute(FTS_SQL, (q, q, FTS_TOP))
        fts = [r[0] for r in cur.fetchall()]
        cur.execute(TRGM_SQL, (q, TRGM_MIN, q, TRGM_TOP))
        trg = [r[0] for r in cur.fetchall()]
        board = rrf([vec, fts, trg])
        best = next((i + 1 for i, cid in enumerate(board) if cid in gold), None)
        rows.append((c["name"], q[:18] + "…", best))
    pg.close()
    print(f"{'题':<28} 金块最好名次")
    buckets = {"≤6": 0, "7..20": 0, ">20/飞榜": 0}
    for name, q, best in rows:
        tag = "≤6" if best and best <= 6 else ("7..20" if best else ">20/飞榜")
        buckets[tag] += 1
        print(f"{name:<28} {best or '飞榜':>6}  {tag}  {q}")
    print(f"\n汇总 {buckets}")
    verdict = ("做：有题落在 7..20，重排收益真实存在" if buckets["7..20"] > 0
               else "毙：没有题落在 7..20 —— 真相关的都已进 TOP_K，重排救不回飞榜的（那是召回层的问题）")
    print("B3 裁决建议：", verdict)


if __name__ == "__main__":
    main()
