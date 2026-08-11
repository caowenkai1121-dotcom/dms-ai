#!/usr/bin/env python
"""深度模式结果合同评测：SQL + 图表 + AI 分析，且 AI 必须位于产物末尾。

用法：
  python tools/deep_contract_eval.py --selfcheck
  set DMSAI_EVAL_LOGIN=... & set DMSAI_EVAL_PASSWORD=... & python tools/deep_contract_eval.py
  set DMSAI_EVAL_TOKEN=... & python tools/deep_contract_eval.py --filter DEEP02
  # bash 写法：
  DMSAI_EVAL_LOGIN=... DMSAI_EVAL_PASSWORD=... python tools/deep_contract_eval.py

环境变量：DMSAI_EVAL_TOKEN，或 DMSAI_EVAL_LOGIN + DMSAI_EVAL_PASSWORD；
  DMSAI_BASE=服务地址（默认 http://127.0.0.1:8100，末尾斜杠会被剥掉）。
退出码：0=全过；1=有题判红；2=门没开（缺凭据/token 预检失败/一题没评到），与题红分开归因。

只读取环境变量中的会话凭据，不把 token、密码写入题集、日志或报告。
"""
import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

BASE = os.environ.get("DMSAI_BASE", "http://127.0.0.1:8100").rstrip("/")
# 题集刻意内联：只有 2 题，抽 JSON 文件反而多一个要同步的载体（kb_eval/evaluation 题多才走 JSON）。
# 题量膨胀到需要 --cases 切换时再抽。
CASES = [
    {"name": "DEEP01-销售额完整BI", "q": "本月销售额是多少"},
    {"name": "DEEP02-客户名单完整BI", "q": "昨天有哪些客户"},
]

# 图表 kind 白名单与后端产物模板绑定：后端新增图表类型（scatter/组合图）时必须同步这里，
# 否则新类型会被恒判「缺有数据的图表板块」。
CHART_KINDS = {"bar", "line", "pie"}


def request(path, body=None, token=None, timeout=240):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    headers = {"Content-Type": "application/json"} if data is not None else {}
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(BASE + path, data=data, headers=headers,
                                 method="POST" if data is not None else "GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", "replace")
            ctype = resp.headers.get("Content-Type", "")
            if "json" not in ctype:
                return resp.status, raw
            # 空 body（如 204）json.loads 会直接抛 JSONDecodeError，按空对象处理
            return resp.status, json.loads(raw) if raw.strip() else {}
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode("utf-8", "replace")
        try:
            return exc.code, json.loads(raw)
        except json.JSONDecodeError:
            return exc.code, raw[:500]
    except (OSError, urllib.error.URLError) as exc:     # TimeoutError 是 OSError 子类，不单列
        return 0, str(exc)


def login_token():
    token = os.environ.get("DMSAI_EVAL_TOKEN")
    if token:
        return token
    login = os.environ.get("DMSAI_EVAL_LOGIN")
    password = os.environ.get("DMSAI_EVAL_PASSWORD")
    if not login or not password:
        raise RuntimeError("缺少 DMSAI_EVAL_TOKEN，或 DMSAI_EVAL_LOGIN + DMSAI_EVAL_PASSWORD")
    code, body = request("/api/login", {"login_name": login, "password": password}, timeout=30)
    if code != 200 or not isinstance(body, dict) or not body.get("token"):
        raise RuntimeError(f"登录失败（HTTP {code}）：{str(body)[:120]}")
    return body["token"]


def check_payload(payload, html):
    failures = []
    if not isinstance(payload, dict):
        return ["响应不是 JSON 对象"]
    # `or {}` 只挡 falsy：值是 list/str 时下游 .get 会 AttributeError 崩整趟——非 dict 判红并按空对象走
    result = payload.get("result")
    page = payload.get("page")
    artifact = payload.get("artifact")
    for key, val in (("result", result), ("page", page), ("artifact", artifact)):
        if val is not None and not isinstance(val, dict):
            failures.append(f"{key} 字段不是对象")
    result = result if isinstance(result, dict) else {}
    page = page if isinstance(page, dict) else {}
    artifact = artifact if isinstance(artifact, dict) else {}
    sqls = page.get("sqls") or []
    sections = page.get("sections") or []
    comparisons = page.get("comparisons") or []
    insight = page.get("insight")

    if not (result.get("sql") or any(x.get("sql") for x in sqls if isinstance(x, dict))):
        failures.append("缺 SQL")
    valid_sqls = [x for x in sqls if isinstance(x, dict) and x.get("title") and x.get("sql")]
    if not sqls or len(valid_sqls) != len(sqls):
        failures.append("缺执行 SQL 清单")
    if not sections:
        failures.append("缺分析板块")
    elif not any(s.get("kind") in CHART_KINDS and s.get("rows")
                 for s in sections if isinstance(s, dict)):
        failures.append("缺有数据的图表板块")
    if not isinstance(insight, str) or not insight.strip():
        failures.append("缺 AI 分析")
    kpi = page.get("kpi")
    kpi = kpi if isinstance(kpi, dict) else {}
    # 「销售额」是通用判定器里刻意写死的业务词：只有 DEEP01 的 KPI 会命中，触发同比/环比校验。
    # 题集膨胀后若更多题要走这档，再抽成 CASES 配置（如 expect_comparisons）。
    if "销售额" in str(kpi.get("label") or ""):
        labels = {str(item.get("label")) for item in comparisons if isinstance(item, dict)}
        if not {"环比", "同比"}.issubset(labels):
            failures.append("销售额缺同比或环比")
        for item in comparisons:
            if not isinstance(item, dict) or not all(key in item for key in ("baseline", "change", "pct", "basis")):
                failures.append("销售对比缺基期、变化额或比较口径")
                break
    if not artifact.get("preview_url"):
        failures.append("缺可预览产物")

    visible = json.dumps(page, ensure_ascii=False) + "\n" + (html or "")
    if re.search(r"(?:KPI|SEC|CON)-\d{2}", visible, re.I):
        failures.append("用户结果泄漏内部核验编号")
    for marker in ("数据边界", "证据目录", "证据链", "已验证"):
        if marker in visible:
            failures.append(f"用户结果仍展示内部核验内容：{marker}")

    if html:
        sql_at = html.find('class="sqlx"')
        ai_at = html.find('class="bi-ai"')
        if "<svg" not in html:
            failures.append("产物页缺图表 SVG")
        if sql_at < 0:
            failures.append("产物页缺执行 SQL")
        if ai_at < 0:
            failures.append("产物页缺 AI 分析区")
        elif sql_at >= 0 and ai_at < sql_at:
            failures.append("AI 分析未位于 SQL/数据证据之后")
        elif any(html.rfind(marker) > ai_at for marker in (
            'class="bi-brief"', 'class="fact-sec"', 'class="highlight-grid"',
            'class="bi-sec', 'class="method-sec"', 'class="sqlx"',
        )):
            failures.append("AI 分析后仍有数据或方法区块，不是结果末尾")
        if ai_at >= 0 and "</main>" in html and ai_at > html.rfind("</main>"):
            failures.append("AI 分析区不在报表主体内")
    return failures


def selfcheck():
    good = {
        "result": {"sql": "SELECT 1"},
        "artifact": {"preview_url": "/api/artifact/1/view"},
        "page": {
            "sqls": [{"title": "主查询", "sql": "SELECT 1"}],
            "sections": [{"kind": "bar", "rows": [["A", 1]]}],
            "insight": "结论与建议",
        },
    }
    good_html = '<main><section><svg></svg></section><details class="sqlx"></details><section class="bi-ai"></section></main>'
    assert check_payload(good, good_html) == []
    assert "缺 SQL" in check_payload({**good, "result": {}, "page": {**good["page"], "sqls": []}}, good_html)
    assert "缺有数据的图表板块" in check_payload(
        {**good, "page": {**good["page"], "sections": [{"kind": "table", "rows": [[1]]}]}}, good_html)
    assert "缺 AI 分析" in check_payload({**good, "page": {**good["page"], "insight": ""}}, good_html)
    assert "用户结果泄漏内部核验编号" in check_payload(
        {**good, "page": {**good["page"], "insight": "结论[SEC-99]"}}, good_html)
    assert "AI 分析未位于 SQL/数据证据之后" in check_payload(
        good, '<main><section class="bi-ai"></section><svg></svg><details class="sqlx"></details></main>')
    assert "AI 分析后仍有数据或方法区块，不是结果末尾" in check_payload(
        good, '<main><details class="sqlx"></details><section class="bi-ai"></section>'
              '<section class="bi-sec"><svg></svg></section></main>')
    assert "销售额缺同比或环比" in check_payload(
        {**good, "page": {**good["page"], "kpi": {"label": "销售额", "value": "1"}}}, good_html)
    # 以下分支从前没有自检覆盖（判据恒过风险与「缺 SQL」同级）
    assert "缺执行 SQL 清单" in check_payload(
        {**good, "page": {**good["page"], "sqls": [{"title": "主查询"}]}}, good_html)   # 条目缺 sql 键
    assert "缺分析板块" in check_payload(
        {**good, "page": {**good["page"], "sections": []}}, good_html)
    assert "缺可预览产物" in check_payload({**good, "artifact": {}}, good_html)
    assert "产物页缺图表 SVG" in check_payload(
        good, '<main><details class="sqlx"></details><section class="bi-ai"></section></main>')
    assert "AI 分析区不在报表主体内" in check_payload(
        good, '<main><details class="sqlx"></details><svg></svg></main><section class="bi-ai"></section>')
    assert "销售对比缺基期、变化额或比较口径" in check_payload(
        {**good, "page": {**good["page"], "kpi": {"label": "销售额"},
                          "comparisons": [{"label": "环比", "change": 1, "pct": 1, "basis": "上月"},
                                          {"label": "同比", "change": 1, "pct": 1, "basis": "去年"}]}},
        good_html)                                                                      # 两条都缺 baseline
    # 畸形字段类型：从前 .get 直接 AttributeError 崩整趟，现在必须判红而不是崩
    assert "result 字段不是对象" in check_payload({**good, "result": "x"}, good_html)
    assert "page 字段不是对象" in check_payload({**good, "page": ["x"]}, good_html)
    print("selfcheck 通过：缺 SQL / 缺图表 / 缺执行 SQL 清单 / 缺分析板块 / 缺可预览产物 / "
          "缺 SVG / AI 不在 main 内 / 对比缺基期 / 字段类型畸形 / "
          "销售缺同比环比 / 内部编号泄漏 / AI 顺序错误均会判红")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--filter", default="")
    ap.add_argument("--selfcheck", action="store_true")
    args = ap.parse_args()
    if args.selfcheck:
        selfcheck()
        return 0
    selected = [c for c in CASES if not args.filter or args.filter in c["name"]]
    if not selected:
        print("❌ 没有匹配用例；0 题执行不构成结论")
        return 2
    try:
        token = login_token()
    except RuntimeError as exc:
        print(f"评测未启动：{exc}")
        return 2
    # token 预检：坏 token 若直接开跑，每题 401 会被记成「题红」退 1——那是「门没开」，不是题红
    code, _ = request("/api/suggest", token=token, timeout=15)
    if code != 200:
        print(f"评测未启动：token 预检失败（/api/suggest HTTP {code}）—— 门没开，非题红")
        return 2

    failures = 0
    evaluated = 0
    for case in selected:
        # compose 默认 240s 超时，逐题打一行进度：「在跑」与「卡死」要分得清
        print(f"▶ {case['name']} 评测中…", flush=True)
        code, payload = request("/api/deep/compose", {"question": case["q"], "mode": "deep"}, token=token)
        if code != 200 or not isinstance(payload, dict):
            print(f"❌ {case['name']} · compose HTTP {code}")
            failures += 1
            continue
        art = payload.get("artifact")
        art = art if isinstance(art, dict) else {}
        # 约定 preview_url 是相对路径（/api/artifact/{id}/view），request() 内部拼 BASE；
        # 后端若改吐绝对 URL 这里会拼坏，到时按 urlparse 判断再拼。
        preview = art.get("preview_url")
        html = ""
        if preview:
            pcode, html = request(preview, token=token, timeout=60)
            if pcode != 200 or not isinstance(html, str):
                print(f"❌ {case['name']} · preview HTTP {pcode}")
                failures += 1
                continue
        else:
            # 没有产物页时 `if html:` 会让全部 HTML 结构断言静默跳过——显式说出来
            print(f"  ⤷ {case['name']} 无 preview_url，HTML 结构断言未执行", flush=True)
        evaluated += 1
        errs = check_payload(payload, html)
        if errs:
            print(f"❌ {case['name']} · {'；'.join(errs)}")
            failures += 1
        else:
            page = payload.get("page") or {}
            print(f"✅ {case['name']} · SQL {len(page.get('sqls') or [])} 条 / "
                  f"图表板块 {len(page.get('sections') or [])} 个 / AI 分析在末尾")
    print(f"执行 {len(selected)} 题 / 通过 {len(selected) - failures} / 失败 {failures}")
    # 一题都没实际评到（compose/preview 全挂，入口没落地）= 门没开，与题红分开归因（对齐 kb_eval）
    if evaluated == 0:
        return 2
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
