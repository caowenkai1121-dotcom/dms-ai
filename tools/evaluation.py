# 执行级评测门禁（SuperSonic evaluation exec-only 思路移植）。
# 不比 SQL 文本，比【生成 SQL 与 gold SQL 各自执行的结果集】——「SQL 看着对、数字错」才拦得住。
# 顺带产出延迟基线（p50/p95）与 tags 分层通过率，带 commit hash 归档，供各期改动前后对照。
#
# 用法:
#   python tools/evaluation.py                # 全量
#   python tools/evaluation.py --filter E05   # 按题名筛
#   python tools/evaluation.py --baseline     # 结果写 tools/eval_baseline.csv（作为后续对照基线）
import json, subprocess, sys, time, csv
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "debug" / "dms-ai-server.exe"
CASES = json.loads((ROOT / "tools" / "eval_cases.json").read_text(encoding="utf-8"))["cases"]
FLOAT_TOL = 0.005  # 相对容差 0.5%：DECIMAL 舍入与 ROUND 位数差异不算错


TRANSIENT = ("error communicating with database", "os error 10054", "os error 10060",
             "pool timed out", "Connection reset")


def run(args, tries=3):
    # 连库抖动重试：批量评测会把远程 MySQL 打到拒连（实测 38 题跑到一半全线 10054），
    # 这类失败与 SQL 对错无关，退避重试而非记为失败。
    for attempt in range(tries):
        out = _run_once(args)
        err = out.get("error", "")
        if err and any(t in err for t in TRANSIENT) and attempt < tries - 1:
            time.sleep(5 * (attempt + 1))
            continue
        return out
    return out


def _run_once(args):
    r = subprocess.run([str(EXE), *args], capture_output=True, text=True,
                       encoding="utf-8", cwd=str(ROOT))
    if r.returncode != 0:
        # stderr 混有 sqlx 慢查询日志，尾部截断会挤掉真实错误——优先抓 Error: 行
        err = next((ln for ln in reversed((r.stderr or "").splitlines()) if "Error:" in ln),
                   (r.stderr or r.stdout).strip()[-300:])
        return {"error": err.strip()[:300]}
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return {"error": r.stdout[-300:]}


def ask(c, retries=1):
    args = ["ask", c["login"], c["q"]] + ([c["role"]] if c.get("role") else [])
    for _ in range(retries + 1):
        j = run(args)
        if j.get("columns"):
            return j
    return j


def exec_gold(c):
    args = ["exec-sql", c["login"], c["gold_sql"]] + ([c["role"]] if c.get("role") else [])
    return run(args)


def cell(v):
    """单元格归一：数字按浮点比，其余按去空白字符串比。
    百分比/千分位/货币符号只是呈现差异（'95.81%' 与 95.81 是同一答案），统一剥掉再比。"""
    if v is None:
        return None
    s = str(v).strip()
    body = s.rstrip("%").replace(",", "").lstrip("¥$")
    try:
        return float(body)
    except ValueError:
        return s


def rows_key(rows):
    """行集合归一：单元格归一 + 行排序（结果集语义无序，除非题目要 TopN——TopN 同样按值排序后仍等价）"""
    return sorted([[cell(v) for v in r] for r in rows], key=lambda r: json.dumps(r, default=str, ensure_ascii=False))


def close(a, b):
    if isinstance(a, float) and isinstance(b, float):
        if a == b:
            return True
        scale = max(abs(a), abs(b), 1e-9)
        return abs(a - b) / scale <= FLOAT_TOL
    return a == b


def compare(got, gold):
    """结果集比对：列数一致 + 逐行逐格等价（列名不比——中文别名允许不同措辞）"""
    g_rows, d_rows = got.get("rows") or [], gold.get("rows") or []
    if len(g_rows) != len(d_rows):
        return False, f"行数 {len(g_rows)}≠{len(d_rows)}"
    if g_rows and len(g_rows[0]) != len(d_rows[0]):
        return False, f"列数 {len(g_rows[0])}≠{len(d_rows[0])}"
    for i, (ra, rb) in enumerate(zip(rows_key(g_rows), rows_key(d_rows))):
        for j, (a, b) in enumerate(zip(ra, rb)):
            if not close(a, b):
                return False, f"第{i+1}行第{j+1}列 {a!r}≠{b!r}"
    return True, f"{len(g_rows)}行一致"


def main():
    flt = sys.argv[sys.argv.index("--filter") + 1] if "--filter" in sys.argv else None
    results, latencies = [], []
    for c in CASES:
        if flt and flt not in c["name"]:
            continue
        time.sleep(2)   # 题间节流：连续 30+ 题猛打远程 MySQL 会把它打到拒连（实测 os error 10054）
        t0 = time.time()
        got = ask(c)
        ms = int((time.time() - t0) * 1000)
        latencies.append(ms)
        if got.get("error") or not got.get("columns"):
            results.append((c, False, f"生成失败: {str(got.get('error'))[:100]}", ms, ""))
            continue
        gold = exec_gold(c)
        if gold.get("error"):
            results.append((c, None, f"gold 执行失败(题目待修): {gold['error'][:100]}", ms, got.get("route", "")))
            continue
        ok, detail = compare(got, gold)
        results.append((c, ok, detail, ms, got.get("route", "")))

    print("=" * 72)
    for c, ok, detail, ms, route in results:
        mark = "✅" if ok else ("⏭️" if ok is None else "❌")
        print(f"{mark} {c['name']} · {route} {ms}ms · {detail}")
    print("=" * 72)

    passed = [r for r in results if r[1] is True]
    failed = [r for r in results if r[1] is False]
    skipped = [r for r in results if r[1] is None]
    graded = len(passed) + len(failed)
    rate = len(passed) / graded * 100 if graded else 0.0
    lat = sorted(latencies)
    p50 = lat[len(lat) // 2] if lat else 0
    p95 = lat[min(len(lat) - 1, int(len(lat) * 0.95))] if lat else 0
    print(f"通过 {len(passed)}/{graded} = {rate:.1f}%  跳过 {len(skipped)}  ·  p50={p50}ms p95={p95}ms")

    # tags 分层
    tag_stat = {}
    for c, ok, *_ in results:
        if ok is None:
            continue
        for t in c.get("tags", []):
            a, b = tag_stat.get(t, (0, 0))
            tag_stat[t] = (a + (1 if ok else 0), b + 1)
    print("分层：" + "  ".join(f"{t} {a}/{b}" for t, (a, b) in sorted(tag_stat.items())))

    if failed:
        (ROOT / "tools" / "eval_error_case.json").write_text(
            json.dumps([{"name": c["name"], "q": c["q"], "detail": d} for c, _, d, *_ in failed],
                       ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"失败明细 → tools/eval_error_case.json（{len(failed)} 例）")

    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True,
                            text=True, cwd=str(ROOT)).stdout.strip()
    row = [time.strftime("%F %T"), commit, graded, len(passed), f"{rate:.1f}", p50, p95]
    if "--baseline" in sys.argv:
        f = ROOT / "tools" / "eval_baseline.csv"
        new = not f.exists()
        with f.open("a", newline="", encoding="utf-8") as fh:
            w = csv.writer(fh)
            if new:
                w.writerow(["time", "commit", "graded", "passed", "rate", "p50_ms", "p95_ms"])
            w.writerow(row)
        print(f"基线已归档 → tools/eval_baseline.csv：{row}")

    sys.exit(1 if failed else 0)


main()
