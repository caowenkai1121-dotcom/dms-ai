# 执行级评测门禁（SuperSonic evaluation exec-only 思路移植）。
# 不比 SQL 文本，比【生成 SQL 与 gold SQL 各自执行的结果集】——「SQL 看着对、数字错」才拦得住。
# 顺带产出延迟基线（p50/p95）与 tags 分层通过率，带 commit hash 归档，供各期改动前后对照。
#
# 用法:
#   python tools/evaluation.py                # 全量
#   python tools/evaluation.py --filter E05   # 按题名筛
#   python tools/evaluation.py --baseline     # 结果写 tools/eval_baseline.csv（作为后续对照基线）
#   python tools/evaluation.py --progress p.txt  # 每出一题就**立刻落盘**一行（长跑可观测）
#   python tools/evaluation.py --runs 3       # 同镜像连跑 3 趟；判据 = 三趟失败集交集（见下）
#   python tools/evaluation.py --throttle-seconds 0.5  # 题间节流；默认 0
#   python tools/evaluation.py --timeout-seconds 180   # 每个 CLI 子进程硬超时
#   python tools/evaluation.py --legacy-cli  # 逐题冷启动，仅用于诊断 batch 差异
#   python tools/evaluation.py --selfcheck    # 只验多趟聚合，不连库
#
# 🔴 为什么判据是 `--runs N` 的**失败集交集**、而不是单轮总分：
# LLM 路径实测抖动池 ≥9/38 ≈ 24%，所以单轮 38 题总分**分辨不出 ±2 的差异**。
# 实测两例：E05 有一趟走 `llm+repair 97.9s` 答错，事后连跑 5 次全部 `direct-agg` 且对数；
# B10 连跑两趟拿到 `llm 93.5s` / `direct-agg 27.9s`（那条 SQL 本身要 28s，超预算就静默降级）。
# 拿单轮 34/38 对比 36/38 下结论 = 在读噪声。今天这活是人工跑三趟再手工比，故收进脚本。
#
# 🔴 为什么要 `--progress`：逐题结果本来只在**全部跑完**之后才打印，
# 而全量一趟 40 分钟起（开了 sc_samples 更久）。中途完全看不到进度，
# 于是「在跑」与「卡死」长得一模一样 —— 实测为此误判过两次、还杀掉过一趟快跑完的。
# 别指望在外面用管道解决：PowerShell 的 `Tee-Object` 到管道结束才落盘（撞过两次），
# `> 文件` 同样缓冲。可靠的做法只有让脚本自己写、自己 flush。
#
# ⚠️ **副作用警告**：本脚本每跑一趟都**无条件覆盖写** `tools/eval_error_case.json`（失败明细）。
# 那是个 git 跟踪的共享路径 —— 任何人跑一趟评测都会盖掉别人上一趟的明细，
# 而「上一趟的明细」经常是唯一还留着的证据。本轮就因此丢过一份（裁决 二·AH5）。
# 跑测量前想留证据的话，先把它另存一份；长期修法是写成 `eval_error_case.<commit>.json`。
#
# 🔴 退出码口径（接门禁的人必须先读这一段）：
#   0 = 干净（单趟：0 题红；多趟：失败集交集为空）
#   1 = 有稳定失败（单趟：有题红；多趟：交集非空）
#   2 = **一题都没评到**（`--filter` 打错、题库筛空）—— 反空转闸，「全绿」不许是「一题没跑」
#   3 = 判据自检报警（N≥3 却零抖动，先怀疑度量坏了）
# ⚠️ 多趟的 `exit 0` **不等于本趟无失败**：按实测 24% 抖动率，「每趟 9 题红但交集为空」是常态，
#   那正是「这些红是噪声」的意思。所以接门禁时必须**同时**断言逐趟的「第N趟 通过 X/Y」行，
#   否则会把一道比单趟更宽的门当成更严的门用（评审实测指出过这一点）。
import io, json, queue, re, subprocess, sys, tempfile, threading, time, csv
from contextlib import redirect_stdout
from cli import cli, cli_stdin
from pathlib import Path

# 🔴 本机 locale 是 cp936，而本脚本满屏 ✅/❌/⏭️/🌀/⚠️：stdout 一旦不是 UTF-8 控制台
# （管道、重定向、被别的程序 subprocess 调用）就在**打印结论那一刻** UnicodeEncodeError。
# 后果特别坏：① 崩的退出码是 1，与本脚本自己定义的「1 = 有稳定失败」撞车，CI 读成「题红了」；
# ② `--runs 3` 是在第 1 趟跑完（40 分钟后）打印时崩，三趟工作全丢。
# 姊妹脚本 regression.py / kb_eval.py 都有这一行，这里漏了 —— 评审实测抓到。
sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
CASES = json.loads((ROOT / "tools" / "eval_cases.json").read_text(encoding="utf-8"))["cases"]
FLOAT_TOL = 0.005  # 相对容差 0.5%：DECIMAL 舍入与 ROUND 位数差异不算错
PROCESS_TIMEOUT_SECONDS = 180.0


TRANSIENT = ("error communicating with database", "os error 10054", "os error 10060",
             "pool timed out", "Connection reset")


class BatchError(RuntimeError):
    pass


class EvalBatch:
    """一个评测趟次复用一个 `eval-batch` 进程；串行写一行、读一行。"""

    def __init__(self):
        spawn_t0 = time.perf_counter()
        self.stderr = tempfile.TemporaryFile(mode="w+b")
        try:
            self.proc = subprocess.Popen(
                cli_stdin("eval-batch"),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=self.stderr,
                text=True,
                encoding="utf-8",
                bufsize=1,
                cwd=str(ROOT),
            )
        except Exception:
            self.stderr.close()
            raise
        self.spawn_ms = elapsed_ms(spawn_t0)
        self.first_request = True
        self.lines = queue.Queue()
        self.reader = threading.Thread(target=self._read_stdout, daemon=True)
        self.reader.start()

    def _read_stdout(self):
        try:
            for line in self.proc.stdout:
                self.lines.put(line)
        except Exception as e:
            self.lines.put(e)
        finally:
            self.lines.put(None)

    def _stderr_tail(self):
        try:
            self.stderr.flush()
            self.stderr.seek(0, 2)
            end = self.stderr.tell()
            self.stderr.seek(max(0, end - 4096))
            return self.stderr.read().decode("utf-8", errors="replace").strip()[-1000:]
        except (OSError, ValueError):
            return ""

    def request(self, payload):
        if self.proc.poll() is not None:
            raise BatchError(f"eval-batch 已退出 rc={self.proc.returncode}: {self._stderr_tail()}")
        try:
            self.proc.stdin.write(json.dumps(payload, ensure_ascii=False) + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError, ValueError) as e:
            raise BatchError(f"eval-batch 写入失败: {e}: {self._stderr_tail()}") from e

        t0 = time.perf_counter()
        try:
            item = self.lines.get(timeout=PROCESS_TIMEOUT_SECONDS)
        except queue.Empty as e:
            tail = self.abort()
            raise BatchError(
                f"eval-batch 单题超时（{PROCESS_TIMEOUT_SECONDS:g}s）"
                + (f": {tail}" if tail else "")
            ) from e
        roundtrip_ms = elapsed_ms(t0)
        if self.first_request:
            roundtrip_ms += self.spawn_ms
            self.first_request = False
        if item is None:
            raise BatchError(f"eval-batch 提前 EOF rc={self.proc.poll()}: {self._stderr_tail()}")
        if isinstance(item, Exception):
            raise BatchError(f"eval-batch stdout 读取失败: {item}: {self._stderr_tail()}")
        try:
            out = json.loads(item)
        except json.JSONDecodeError as e:
            raise BatchError(f"eval-batch 返回非 JSON: {item[-300:]}") from e
        if out.get("id") != payload["id"]:
            raise BatchError(f"eval-batch 响应串题: 请求 {payload['id']!r}，收到 {out.get('id')!r}")
        return out, roundtrip_ms

    def abort(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=2)
        return self._stderr_tail()

    def close(self):
        if self.proc.poll() is None:
            try:
                self.proc.stdin.close()  # EOF = 服务端正常结束 NDJSON 循环
            except (OSError, ValueError):
                pass
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.abort()
        self.reader.join(timeout=1)
        for stream in (self.proc.stdout, self.proc.stdin):
            try:
                stream.close()
            except (AttributeError, OSError, ValueError):
                pass
        self.stderr.close()


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
    try:
        r = subprocess.run(cli(*args), capture_output=True, text=True,
                           encoding="utf-8", cwd=str(ROOT), timeout=PROCESS_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        return {"error": f"子进程超时（{PROCESS_TIMEOUT_SECONDS:g}s）"}
    except OSError as e:
        return {"error": f"子进程启动失败: {e}"}
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
        if j.get("columns") or j.get("rows") or j.get("markdown") or j.get("kind") == "text":
            return j
    return j


def exec_gold(c):
    args = ["exec-sql", c["login"], c["gold_sql"]] + ([c["role"]] if c.get("role") else [])
    return run(args)


def _flatten_text(value):
    if value is None:
        return []
    if isinstance(value, dict):
        return [text for child in value.values() for text in _flatten_text(child)]
    if isinstance(value, (list, tuple)):
        return [text for child in value for text in _flatten_text(child)]
    return [str(value)]


def visible_response_text(got):
    """只读用户真正看得到的结果，不读 SQL，避免 SQL 字面量让不可用题假绿。"""
    fields = ("columns", "rows", "markdown", "message", "answer",
              "caliber_note", "truncation_note")
    return "\n".join(text for field in fields for text in _flatten_text(got.get(field)))


def compare_unavailable(got, expected):
    """结构化不可用判据：明确说明事实未同步/不可安全计算才算守住。"""
    text = visible_response_text(got)
    any_words = expected.get("contains_any") or []
    all_words = expected.get("contains_all") or []
    if not any(word in text for word in any_words):
        return False, f"未明确说明不可计算（期望任一：{'/'.join(any_words)}）"
    missing = [word for word in all_words if word not in text]
    if missing:
        return False, f"不可用说明缺少业务对象：{'/'.join(missing)}"
    return True, "明确说明事实未同步，未输出猜测值"


def case_protocol_errors(cases):
    """题库协议预检；每题必须且只能声明 gold_sql / expected_unavailable 之一。"""
    errors, seen = [], set()
    for c in cases:
        name = c.get("name", "<未命名>")
        if name in seen:
            errors.append(f"{name}: 题名重复")
        seen.add(name)
        has_gold = bool(str(c.get("gold_sql") or "").strip())
        unavailable = c.get("expected_unavailable")
        if has_gold == bool(unavailable):
            errors.append(f"{name}: 必须且只能声明 gold_sql / expected_unavailable 之一")
            continue
        if unavailable:
            any_words = unavailable.get("contains_any")
            all_words = unavailable.get("contains_all", [])
            if not isinstance(any_words, list) or not any_words or not all(isinstance(x, str) and x for x in any_words):
                errors.append(f"{name}: expected_unavailable.contains_any 必须是非空字符串数组")
            if not isinstance(all_words, list) or not all(isinstance(x, str) and x for x in all_words):
                errors.append(f"{name}: expected_unavailable.contains_all 必须是字符串数组")
    return errors


def elapsed_ms(start):
    return int((time.perf_counter() - start) * 1000)


def product_elapsed_ms(got):
    try:
        return max(0, int(float(got.get("elapsed_ms") or 0)))
    except (TypeError, ValueError):
        return 0


def timing_of(got=None, ask_wall_ms=0, gold_ms=0, harness_ms=None):
    product_ms = product_elapsed_ms(got or {})
    # legacy 模式没有服务端批量计时，只能用 ask 子进程墙钟减产品内部耗时估算。
    if harness_ms is None:
        harness_ms = max(0, ask_wall_ms - product_ms)
    return {
        "product_ms": product_ms,
        "ask_wall_ms": ask_wall_ms,
        "gold_ms": gold_ms,
        "harness_ms": max(0, int(harness_ms)),
    }


def batch_payload(c):
    payload = {
        "id": c["name"],
        "login": c["login"],
        "role": c.get("role"),
        "q": c["q"],
    }
    # 事实未同步题只评产品 fail-closed 响应；绝不把不存在的 gold 传给服务端。
    if not c.get("expected_unavailable"):
        payload["gold_sql"] = c["gold_sql"]
    return payload


def batch_timing(out, roundtrip_ms):
    ask_wall_ms = max(0, int(out.get("ask_wall_ms") or 0))
    gold_ms = max(0, int(out.get("gold_ms") or 0))
    # 首题冷启动、NDJSON 传输/解析和 Python 调度都在 roundtrip 中，扣掉服务端两段墙钟即 harness。
    return timing_of(
        out.get("got") or {},
        ask_wall_ms,
        gold_ms,
        max(0, roundtrip_ms - ask_wall_ms - gold_ms),
    )


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
    """结果集比对：列数一致 + 逐行逐格等价（列名不比——中文别名允许不同措辞）。
    第一返回值三态：True 绿 / False 红 / **None ⏭️（题目待修，不计入通过率分母）**。

    🔴 gold 返 0 行必须是 ⏭️ 而不是绿。原来的链路每一步都「通过」：
    `len(g_rows) != len(d_rows)` 在 0==0 时过 → `if g_rows and …` 让空表跳过列数校验
    → `zip([], [])` 是空循环 → `return True, "0行一致"`。
    也就是说任何 gold 查不出数据的题（口径写错 / 时间窗落到无数据区间 / 表被清）
    **永久变成绿题**，而它一个字节都没校验过，还把分子分母一起抬上去。
    第三态只保留给“gold 成功执行但返回空表”的题；gold SQL 执行失败现在由 batch/legacy
    两条路径统一判红，不再静默 ⏭️。`summarize` 仍让真正的空 gold 不进入通过率分母。
    ⚠️ 代价：正确答案**本来**就是 0 行的题（「上周有没有退货」）也会变 ⏭️。
    那种题该改成带非零期望的问法 —— 「gold 空」与「答对了空」在执行级比对里无法分辨。"""
    g_rows, d_rows = got.get("rows") or [], gold.get("rows") or []
    if not d_rows:
        return None, "gold 返 0 行（题目待修，不构成结论）"
    if len(g_rows) != len(d_rows):
        return False, f"行数 {len(g_rows)}≠{len(d_rows)}"
    if g_rows and len(g_rows[0]) != len(d_rows[0]):
        return False, f"列数 {len(g_rows[0])}≠{len(d_rows[0])}"
    for i, (ra, rb) in enumerate(zip(rows_key(g_rows), rows_key(d_rows))):
        for j, (a, b) in enumerate(zip(ra, rb)):
            if not close(a, b):
                return False, f"第{i+1}行第{j+1}列 {a!r}≠{b!r}"
    return True, f"{len(g_rows)}行一致"


def sql_parts(sql):
    """SQL 组件切分（where 条件集 / group by / 聚合调用集 / select 段）。

    🔴 字符串启发式，**不是 AST**：嵌套子查询会混层、`GROUP BY 月份` 与
    `GROUP BY DATE_FORMAT(...)` 也判不同。它是 A22 的**排查提示**（告诉排查者
    先去看哪个组件），不是判据 —— 判红判绿仍由 `compare` 的结果集比对定。"""
    s = " ".join(str(sql).split()).lower()

    def between(a, ends):
        i = s.find(a)
        if i < 0:
            return ""
        j = min([s.find(b, i + len(a)) for b in ends if s.find(b, i + len(a)) >= 0] or [len(s)])
        return s[i:j]

    select = between("select", [" from "])
    where = between(" where ", [" group by ", " order by ", " limit "])
    group = between(" group by ", [" order by ", " limit "])
    aggs = sorted(set(re.findall(r"(?:sum|count|avg|min|max)\s*\([^)]*\)", s)))
    conds = set(
        re.findall(r"[\w.`]+\s*(?:!=|>=|<=|=|>|<|not in|in|like)\s*(?:'[^']*'|\([^)]*\)|[\w.()]+)", where)
    ) if where else set()
    return {"select": select, "where": conds, "group": group, "agg": aggs}


def diff_class(got_sql, gold_sql):
    """【A22】失败 SQL 与 gold 的首个不一致组件（where → group → agg → select 顺序报）。
    今天只知道「数不对」、不知道错在哪一层 —— 这一列就是给排查者的第一现场。"""
    g, d = sql_parts(got_sql), sql_parts(gold_sql)
    for k in ("where", "group", "agg"):
        if g[k] != d[k]:
            return k
    return "select"


MARK = {True: "✅", False: "❌", None: "⏭️"}


def line_of(c, ok, detail, timing, route):
    """逐题一行（进度与汇总共用同一个措辞——两份格式必然漂）"""
    mark = MARK[ok]
    clocks = " ".join(f"{key}={timing[key]}" for key in
                      ("product_ms", "ask_wall_ms", "gold_ms", "harness_ms"))
    return f"{mark} {c['name']} · {route} · {clocks} · {detail}"


def jitter_report(passes):
    """多趟聚合。passes = 每趟的 {题名: ok}，ok ∈ True/False/None(gold 待修跳过)。
    返回（交集失败名单, 抖动名单, {题名: 红绿序列}）。

    只有 `ok is False` 算红。⏭️ 既不算红也不算绿，但它让该题「不是每趟都红」→ 落进抖动池：
    交集是要拿去下结论的，数据不全的题不配进去。"""
    names = list(dict.fromkeys(n for p in passes for n in p))
    seq = {n: "".join(MARK[p[n]] if n in p else "·" for p in passes) for n in names}
    reds = [{n for n, ok in p.items() if ok is False} for p in passes]
    always = sorted(set.intersection(*reds)) if reds else []
    jitter = sorted(set().union(*reds) - set(always)) if reds else []
    return always, jitter, seq


def quiet_alarm(n_runs, jitter, graded=99):
    """判据自检：N≥3 趟却一个抖动都没测到，先怀疑度量坏了而不是系统突然稳定了。
    依据是本仓实测抖动池 ≥9/38 ≈ 24%（E05、B10 两例见文件头），M=0 属于「不可能这么干净」。
    N<3 不报：两趟本来就有 (1-0.24)^2 ≈ 58% 的概率一个都不抖。

    🔴 `graded` 门是评审逼出来的：`--runs 3 --filter E05`（1 题）几乎必然 M=0，
    于是这条报警对任何小样本都恒真 —— 恒真的报警等于没有报警。24% 抖动率下，
    10 题跑 3 趟一个不抖的概率约 (0.76**3 + ...)^10，已经足够小才值得报警。
    样本不足时不报（也不能因此判绿，那由反空转闸另管）。"""
    return n_runs >= 3 and not jitter and graded >= 10


def report(passes):
    """打印多趟判据，返回退出码：0 干净 / 1 有稳定失败 / 2 一题没评到 / 3 判据自检报警。
    3 与 1 分开：「度量坏了」和「题目红了」要能在 CI 里一眼分辨。"""
    n = len(passes)
    always, jitter, seq = jitter_report(passes)
    print("=" * 72)
    for i, p in enumerate(passes, 1):
        graded = sum(1 for ok in p.values() if ok is not None)
        print(f"第{i}趟 通过 {sum(1 for ok in p.values() if ok is True)}/{graded}")
    print("每题红绿序列：")
    for name, s in seq.items():
        print(f"  {name}: {s}" + ("   ← 抖" if name in jitter else ""))
    print(f"❌ 失败集交集（{n} 趟都红 —— **这才是判据**）{len(always)} 题："
          f"{'、'.join(always) or '空'}")
    print(f"🌀 抖动池 M={len(jitter)}：{'、'.join(jitter) or '空'}")
    graded_max = max((sum(1 for ok in p.values() if ok is not None) for p in passes), default=0)
    # 反空转闸：0 题评到一律非 0（与 kb_eval.py 同口径）。「全绿」不许是「一题没跑」。
    if graded_max == 0:
        print("❌ 一题都没评到（--filter 打错？题库筛空？）—— 这不构成任何结论")
        return 2
    # 🔴 顺序：**先判「有稳定失败」**，再判自检报警。反过来的话，
    # 「N≥3 且有稳定失败且恰好零抖动」会返 3（度量坏了）而不是 1（题红了）——
    # 正好在最要紧的场景下把「1 与 3 能一眼分辨」这个立意废掉（评审实测指出）。
    if always:
        return 1
    if quiet_alarm(n, jitter, graded_max):
        print("⚠️ 本次没有测到抖动 —— 要么环境异常安静，要么这个度量没在测它该测的东西")
        return 3
    return 0


def judge_batch_response(c, out, roundtrip_ms):
    timing = batch_timing(out, roundtrip_ms)
    got, gold = out.get("got") or {}, out.get("gold") or {}
    if out.get("error"):
        return c, False, f"batch 执行失败: {str(out['error'])[:160]}", timing, got.get("route", "")
    usable = got.get("columns") or got.get("rows") or got.get("markdown") or got.get("kind") == "text"
    if not usable:
        return c, False, "生成失败: batch 响应缺少可展示结果", timing, got.get("route", "")
    if c.get("expected_unavailable"):
        ok, detail = compare_unavailable(got, c["expected_unavailable"])
        return c, ok, detail, timing, got.get("route", "")
    if not gold.get("columns") and not gold.get("rows"):
        return c, False, "gold 预检失败: batch 响应缺少 gold 结果", timing, got.get("route", "")
    ok, detail = compare(got, gold)
    if ok is False:
        detail = f"{detail} · diff={diff_class(got.get('sql',''), c['gold_sql'])}"
    return c, ok, detail, timing, got.get("route", "")


def run_pass_batch(cases, tick, throttle_seconds=0.0):
    """默认路径：每趟一个驻留进程，严格串行；单题协议/超时故障判红后重启继续。"""
    results, batch = [], None
    try:
        for i, c in enumerate(cases):
            if i and throttle_seconds:
                time.sleep(throttle_seconds)
            row = None
            for attempt in range(3):
                try:
                    if batch is None:
                        batch = EvalBatch()
                    out, roundtrip_ms = batch.request(batch_payload(c))
                    error = str(out.get("error") or "")
                    if error and any(token in error for token in TRANSIENT) and attempt < 2:
                        batch.abort()
                        batch.close()
                        batch = None
                        time.sleep(5 * (attempt + 1))
                        continue
                    row = judge_batch_response(c, out, roundtrip_ms)
                    break
                except (BatchError, OSError) as e:
                    if batch is not None:
                        batch.abort()
                        batch.close()
                        batch = None
                    message = str(e)
                    if any(token in message for token in TRANSIENT) and attempt < 2:
                        time.sleep(5 * (attempt + 1))
                        continue
                    clocks = {"product_ms": 0, "ask_wall_ms": 0, "gold_ms": 0, "harness_ms": 0}
                    row = (c, False, f"batch 通道失败: {message[:180]}", clocks, "")
                    break
            assert row is not None
            results.append(row)
            tick(row)
    finally:
        if batch is not None:
            batch.close()
    return results


def run_pass_legacy(cases, tick, throttle_seconds=0.0):
    """诊断路径：每个 ask/gold 各启一个进程；不用于默认性能基线。"""
    results = []
    for i, c in enumerate(cases):
        if i and throttle_seconds:
            time.sleep(throttle_seconds)

        gold, gold_ms = None, 0
        if not c.get("expected_unavailable"):
            t0 = time.perf_counter()
            gold = exec_gold(c)
            gold_ms = elapsed_ms(t0)
            if gold.get("error"):
                timing = timing_of(gold_ms=gold_ms)
                row = (c, False, f"gold 预检失败: {gold['error'][:100]}", timing, "")
                results.append(row)
                tick(row)
                continue

        t0 = time.perf_counter()
        got = ask(c)
        ask_wall_ms = elapsed_ms(t0)
        timing = timing_of(got, ask_wall_ms, gold_ms)
        usable = got.get("columns") or got.get("rows") or got.get("markdown") or got.get("kind") == "text"
        if got.get("error") or not usable:
            row = (c, False, f"生成失败: {str(got.get('error'))[:100]}", timing, "")
        elif c.get("expected_unavailable"):
            ok, detail = compare_unavailable(got, c["expected_unavailable"])
            row = (c, ok, detail, timing, got.get("route", ""))
        else:
            ok, detail = compare(got, gold)
            # 【A22】红题附上组件分类（启发式提示，见 diff_class 的文档）
            if ok is False:
                detail = f"{detail} · diff={diff_class(got.get('sql',''), c['gold_sql'])}"
            row = (c, ok, detail, timing, got.get("route", ""))
        results.append(row)
        tick(row)
    return results


def summarize(results, archive, runs, i):
    """单趟汇总：逐题 + 通过率 + 延迟 + 分层。多趟时 p50/p95 **按趟分别打印**，
    因为把多趟延迟混进一个分位数会把「被别的工作流挤慢的那趟」摊平掉。"""
    print("=" * 72)
    for row in results:
        print(line_of(*row))
    print("=" * 72)

    passed = [r for r in results if r[1] is True]
    failed = [r for r in results if r[1] is False]
    skipped = [r for r in results if r[1] is None]
    graded = len(passed) + len(failed)
    rate = len(passed) / graded * 100 if graded else 0.0
    def pct(key, q):
        values = sorted(r[3][key] for r in results)
        return values[min(len(values) - 1, int(len(values) * q))] if values else 0

    p50, p95 = pct("product_ms", .5), pct("product_ms", .95)
    wall_p50, wall_p95 = pct("ask_wall_ms", .5), pct("ask_wall_ms", .95)
    gold_p50, gold_p95 = pct("gold_ms", .5), pct("gold_ms", .95)
    harness_p50, harness_p95 = pct("harness_ms", .5), pct("harness_ms", .95)
    tag = f"第{i}趟 " if runs > 1 else ""
    print(f"{tag}通过 {len(passed)}/{graded} = {rate:.1f}%  跳过 {len(skipped)}"
          f"  · product p50={p50}ms p95={p95}ms"
          f"  · ask_wall p50={wall_p50}ms p95={wall_p95}ms"
          f"  · gold p50={gold_p50}ms p95={gold_p95}ms"
          f"  · harness p50={harness_p50}ms p95={harness_p95}ms")
    if runs > 1:
        # 实测：有一趟评测被并发工作流污染，AS01 42s vs 上一趟 21s。拿那趟当基线会得出假结论。
        print("⚠️ 延迟只在无并发时有意义（实测被并发工作流污染时 AS01 21s→42s）")

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
        # 多趟时这份文件是**最后一趟**的失败明细（供 triage）；判据看上面的交集，别拿它下结论。
        (ROOT / "tools" / "eval_error_case.json").write_text(
            json.dumps([{"name": c["name"], "q": c["q"], "detail": d} for c, _, d, *_ in failed],
                       ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"失败明细 → tools/eval_error_case.json（{len(failed)} 例）")

    if archive:
        commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True,
                                text=True, cwd=str(ROOT)).stdout.strip()
        row = [time.strftime("%F %T"), commit, graded, len(passed), f"{rate:.1f}",
               p50, p95, wall_p50, wall_p95, gold_p50, gold_p95, harness_p50, harness_p95]
        f = ROOT / "tools" / "eval_baseline.csv"
        header = ["time", "commit", "graded", "passed", "rate",
                  "product_p50_ms", "product_p95_ms",
                  "ask_wall_p50_ms", "ask_wall_p95_ms",
                  "gold_p50_ms", "gold_p95_ms",
                  "harness_p50_ms", "harness_p95_ms"]
        if f.exists():
            with f.open(encoding="utf-8") as fh:
                old_rows = list(csv.reader(fh))
            if old_rows and old_rows[0] != header:
                # 旧 p50/p95 本来量的是 ask 墙钟，迁移后落在 ask_wall 两列；其余未知留空。
                migrated = [header]
                for old in old_rows[1:]:
                    if len(old) >= 7:
                        migrated.append(old[:5] + ["", "", old[5], old[6], "", "", "", ""])
                with f.open("w", newline="", encoding="utf-8") as fh:
                    csv.writer(fh).writerows(migrated)
        new = not f.exists() or f.stat().st_size == 0
        with f.open("a", newline="", encoding="utf-8") as fh:
            w = csv.writer(fh)
            if new:
                w.writerow(header)
            w.writerow(row)   # 一趟一行：多趟归档 N 行，抖动在基线里也看得见
        print(f"基线已归档 → tools/eval_baseline.csv：{row}")
    return bool(failed)


def selfcheck():
    """不连库自检：证明交集/抖动池算得对、M=0 走非 0 退出。"""
    passes = [{"A": False, "B": False, "C": True},    # 三趟失败集 {A,B} {B,C} {B}
              {"A": True, "B": False, "C": False},
              {"A": True, "B": False, "C": True}]
    always, jitter, seq = jitter_report(passes)
    assert always == ["B"], always
    assert jitter == ["A", "C"], jitter
    assert seq == {"A": "❌✅✅", "B": "❌❌❌", "C": "✅❌✅"}, seq
    assert report(passes) == 1
    # ⏭️ 不算红，也不许把该题算进交集
    assert jitter_report([{"A": False}, {"A": None}])[0] == []
    assert jitter_report([{"A": False}, {"A": None}])[1] == ["A"]
    assert jitter_report([{"A": None}] * 3)[2]["A"] == "⏭️⏭️⏭️"
    # 🔴 三趟全同且有稳定失败 → 必须是 **1（题红了）**，不是 3（度量坏了）。
    # 这一条是评审抓到的顺序错：原来先判 quiet_alarm，于是最要紧的场景反而报「度量坏了」。
    same = [{"A": False, "B": True}] * 3
    assert jitter_report(same) == (["A"], [], {"A": "❌❌❌", "B": "✅✅✅"})
    assert report(same) == 1, "有稳定失败必须优先于自检报警"
    # 三趟全绿且零抖动、样本够大 → 3（这才是「不可能这么干净」该报的场景）
    clean = [{f"Q{i}": True for i in range(12)}] * 3
    assert report(clean) == 3, "12 题跑 3 趟零抖动应报判据自检"
    # 反空转闸：一题都没评到 → 2，不许是 0
    assert report([{}, {}, {}]) == 2
    assert report([{"A": None}] * 3) == 2, "全 ⏭️ 等于一题没评到"
    assert quiet_alarm(3, [], 12) is True
    assert quiet_alarm(3, ["A"], 12) is False and quiet_alarm(2, [], 12) is False
    # 样本不足不报警（`--runs 3 --filter E05` 恒 M=0，恒真的报警等于没有报警）
    assert quiet_alarm(3, [], 1) is False and quiet_alarm(3, [], 9) is False
    # 🔴 空表恒绿（见 `compare` 的长注释）：gold 0 行原来返 (True, "0行一致")，
    # 一个字节都没比就把题算成绿。现在必须是 ⏭️，且不进通过率分母。
    assert compare({"rows": []}, {"rows": []}) == (None, "gold 返 0 行（题目待修，不构成结论）")
    assert compare({"rows": [[1]]}, {"rows": []})[0] is None, "gold 空 = 题目待修，与 got 有没有行无关"
    assert compare({"rows": [[1]]}, {"rows": None})[0] is None, "gold 缺 rows 键同理"
    # 反面：非空 gold 的三条路一条都不许被上面那道门吞掉
    assert compare({"rows": [[1]]}, {"rows": [[1]]}) == (True, "1行一致")
    assert compare({"rows": [[1]]}, {"rows": [[2]]})[0] is False, "值不同必须红"
    assert compare({"rows": []}, {"rows": [[1]]})[0] is False, "got 空 gold 非空必须红"
    assert compare({"rows": [[1, 2]]}, {"rows": [[1]]})[1].startswith("列数"), "列数校验必须还在"
    unavailable = {"contains_any": ["不可计算", "未同步", "无法安全计算"],
                   "contains_all": ["开票"]}
    assert compare_unavailable({"columns": ["状态", "原因"],
                                "rows": [["不可计算", "开票事实未同步，无法安全计算"]]}, unavailable)[0]
    assert not compare_unavailable({"sql": "SELECT '不可计算'", "rows": [[0]]}, unavailable)[0], \
        "不可用词只藏在 SQL 里不能假绿"
    assert not compare_unavailable({"rows": [["开票金额为 0"]]}, unavailable)[0], \
        "缺事实时输出猜测值必须红"
    assert case_protocol_errors([
        {"name": "gold", "gold_sql": "SELECT 1"},
        {"name": "missing", "expected_unavailable": unavailable},
    ]) == []
    assert case_protocol_errors([{"name": "bad"}])
    assert case_protocol_errors([{"name": "bad", "gold_sql": "SELECT 1",
                                  "expected_unavailable": unavailable}])
    assert product_elapsed_ms({"elapsed_ms": "123"}) == 123
    assert timing_of({"elapsed_ms": 80}, 120, 30) == {
        "product_ms": 80, "ask_wall_ms": 120, "gold_ms": 30, "harness_ms": 40,
    }
    assert batch_payload({"name": "E01", "login": "admin", "q": "本月销售额",
                          "gold_sql": "SELECT 1"})["gold_sql"] == "SELECT 1"
    sparse = batch_payload({"name": "E08", "login": "admin", "q": "本月开票金额",
                            "expected_unavailable": unavailable})
    assert sparse == {"id": "E08", "login": "admin", "role": None, "q": "本月开票金额"}, sparse
    batch_out = {
        "id": "E01", "ask_wall_ms": 120, "gold_ms": 30,
        "got": {"elapsed_ms": 80, "columns": ["值"], "rows": [[1]], "route": "direct-agg"},
        "gold": {"columns": ["值"], "rows": [[1]]},
    }
    assert batch_timing(batch_out, 170) == {
        "product_ms": 80, "ask_wall_ms": 120, "gold_ms": 30, "harness_ms": 20,
    }
    assert judge_batch_response({"name": "E01", "gold_sql": "SELECT 1"}, batch_out, 170)[1]
    batch_src = Path(__file__).read_text(encoding="utf-8")
    batch_body = batch_src.split("def run_pass_batch", 1)[1].split("def run_pass_legacy", 1)[0]
    assert "for attempt in range(3)" in batch_body
    assert "any(token in error for token in TRANSIENT)" in batch_body
    assert "time.sleep(5 * (attempt + 1))" in batch_body
    # 【A22】组件分类：where 条件集不同 → where；group 不同 → group；全同 → select。
    # 判据钉「顺序报」与「不误报」，不是钉启发式的完备性（那是提示不是判据）。
    assert diff_class("SELECT a FROM t WHERE x = '1'", "SELECT a FROM t WHERE x = '2'") == "where"
    assert diff_class("SELECT a FROM t WHERE x = '1' GROUP BY a",
                      "SELECT a FROM t WHERE x = '1' GROUP BY b") == "group"
    assert diff_class("SELECT SUM(a) FROM t WHERE x = '1' GROUP BY c",
                      "SELECT COUNT(a) FROM t WHERE x = '1' GROUP BY c") == "agg"
    assert diff_class("SELECT a, SUM(b) FROM t WHERE x = '1' GROUP BY c",
                      "SELECT a, SUM(b) FROM t WHERE x = '1' GROUP BY c") == "select"
    assert diff_class("SELECT SUM(a) FROM t", "SELECT SUM(a) FROM t WHERE y = '2'") == "where"
    assert jitter_report([{"A": None}, {"A": None}])[0] == []   # 也不许把 ⏭️ 算进交集
    # 🔴 「⏭️ 不计入通过率分母」这条口径**只**写在 `summarize` 里（graded = passed + failed），
    # 上面那些 assert 一条都够不着它 —— 而 ③ 的修法完全押在它身上：把 skipped 并进 graded
    # （或算成 passed）的话，gold 空的题就又开始动通过率了。所以捕 stdout 直接断那一行。
    # 不放 ❌ 行是刻意的：`summarize` 一有失败就**无条件覆盖写** tools/eval_error_case.json
    # （git 跟踪的共享路径，见文件头的副作用警告）—— 自检不许有副作用。
    buf = io.StringIO()
    with redirect_stdout(buf):
        clocks = {"product_ms": 10, "ask_wall_ms": 20, "gold_ms": 5, "harness_ms": 10}
        assert summarize([({"name": "A"}, True, "1行一致", clocks, "d"),
                          ({"name": "B"}, None, "gold 返 0 行", clocks, "")], False, 1, 1) is False
    assert "通过 1/1 = 100.0%  跳过 1" in buf.getvalue(), buf.getvalue()
    print("evaluation.py 自检通过")
    return 0


def arg(name, default=None):
    """取值必须存在且不以 `--` 开头。

    没这道守卫时 `--filter --runs 3` 会让 filter 变成 `"--runs"`，静默筛不到题；
    配上「0 题也退 0」那个洞，一条打错的命令行就是一次假绿。姊妹脚本 regression.py
    的 `opt()` 早有这道闸，这里漏了 —— 评审实测抓到。"""
    if name not in sys.argv:
        return default
    i = sys.argv.index(name) + 1
    if i >= len(sys.argv) or sys.argv[i].startswith("--"):
        sys.exit(f"{name} 后面缺少取值")
    return sys.argv[i]


def main():
    global PROCESS_TIMEOUT_SECONDS
    if "--selfcheck" in sys.argv:
        sys.exit(selfcheck())
    flt = arg("--filter")
    runs = max(1, int(arg("--runs", 1)))
    PROCESS_TIMEOUT_SECONDS = max(1.0, float(arg("--timeout-seconds", PROCESS_TIMEOUT_SECONDS)))
    throttle_seconds = max(0.0, float(arg("--throttle-seconds", 0)))
    legacy_cli = "--legacy-cli" in sys.argv
    prog = Path(arg("--progress")) if "--progress" in sys.argv else None
    if prog:
        prog.write_text("", encoding="utf-8")   # 只清一次：多趟往同一个文件追加，靠行首趟次区分
    cases = [c for c in CASES if not flt or flt in c["name"]]
    # 反空转闸（与 kb_eval.py 同口径）：筛空一律 exit 2。
    # 实测原行为：`--filter __no_such_case__` → 「通过 0/0 = 0.0%」→ **exit 0**。
    # 那正是本轮要杀的「跑绿但什么都没测」——判官自己也犯，评审抓到。
    if not cases:
        print(f"❌ 一题都没匹配到（--filter {flt}）—— 0 题执行不构成任何结论")
        sys.exit(2)
    protocol_errors = case_protocol_errors(cases)
    if protocol_errors:
        print("❌ 题库协议预检失败：")
        for error in protocol_errors:
            print(f"  - {error}")
        sys.exit(2)

    def tick_of(i):
        def tick(row):
            """一题一落盘。`flush` 不可省：不 flush 就等于没有进度文件。"""
            if not prog:
                return
            with prog.open("a", encoding="utf-8") as f:
                f.write((f"[趟{i}/{runs}] " if runs > 1 else "") + line_of(*row) + "\n")
                f.flush()
        return tick

    passes = []
    for i in range(1, runs + 1):
        if runs > 1:
            print(f"—— 第 {i}/{runs} 趟（{len(cases)} 题）——", flush=True)
        runner = run_pass_legacy if legacy_cli else run_pass_batch
        results = runner(cases, tick_of(i), throttle_seconds)
        failed = summarize(results, "--baseline" in sys.argv, runs, i)
        passes.append({c["name"]: ok for c, ok, *_ in results})
    if runs > 1:
        sys.exit(report(passes))
    # 单趟同样要反空转：全部 ⏭️（依赖缺席）时 graded=0，「0 失败」不等于「测过了」
    graded = sum(1 for ok in passes[0].values() if ok is not None)
    if graded == 0:
        print("❌ 一题都没评到（全部跳过）—— 不构成任何结论")
        sys.exit(2)
    sys.exit(1 if failed else 0)


main()
