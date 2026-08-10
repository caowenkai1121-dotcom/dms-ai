# LLM 供应商对拍：同一份 prompt 打两个端点，量准确度 / 智能 / 速度。
#
# 用法:
#   python tools/model_ab.py                      # precise 档 × SQL 题集 × 3 趟
#   python tools/model_ab.py --tier fast          # fast 档（deepseek-v4-flash / qwen3.7-flash）
#   python tools/model_ab.py --suite fast         # fast 档**真实工作**的题集（拆解/改写/意图/选源）
#   python tools/model_ab.py --runs 1             # 冒烟
#
# 🔴 为什么绕开系统跑模型级对拍：`evaluation.py` 头部写着「LLM 路径实测抖动池 ≥9/38 ≈ 24%，
# 单轮 38 题分辨不出 ±2 的差异」。而且系统里大部分题走确定性路径（direct-agg/graph/compound），
# 换模型**零影响** —— 拿系统总分对比两个模型，测的主要是我们的模板，不是模型。
#
# 🔴 题目里的 schema **必须用真列名**。第一版我编了 `t_sales_order_detail.quantity`
# （真库里只有 box_quantity/bag_quantity/stock_quantity），于是「回库核幻觉列」这条判据
# 变成了「谁照抄我的假 schema 谁扣分」—— deepseek 因此被判一个伪失分。
# 现在的列名都来自 `meta.column_doc`，去重键来自 `meta.metric.dedup_keys`。
#
# 🔴 每题取 N 趟里**最差**的一趟：偶尔对不算对。温度已经 0.1，剩下的抖动就是模型本身。
#
# key：统一由 `tools/settings.py` 读取当前 `DMSAI_SETTINGS`（默认 `settings.json`）。
# 优先取 `llm_keys`；`llm_api_key` 只兼容当前文件供应商。两个 key 都不写进任何输出。
import argparse
import json
import re
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import settings as tool_settings

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
ROOT = Path(__file__).resolve().parents[1]
CLI = ["docker", "exec", "dms-ai-server", "/app/dms-ai-server"]

PROVIDER_CATALOG = {
    "deepseek": {
        "base": "https://api.deepseek.com",
        "fast": "deepseek-v4-flash",
        "precise": "deepseek-v4-pro",
        "extra": {"thinking": {"type": "disabled"}},
    },
    "qwen": {
        "base": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "fast": "qwen3.7-flash",
        "precise": "qwen3.7-plus",
        "extra": {"enable_thinking": False},
    },
}

SYS_SQL = (
    "你是 MySQL 8 的 text-to-SQL 引擎。严格照下面的口径卡取数。\n"
    "只输出 JSON：{\"sql\":\"...\"} 或 {\"refuse\":\"理由\"}。不要解释。\n"
    "SQL 必须只读（SELECT）。口径卡与问句冲突时用 refuse 说明，不要猜。"
)

# ── 题集①：产 SQL（precise 档的活）。每道题都是本仓真金白银踩过的坑，全写在口径卡里，
#    考的是「照做」的能力。列名取自 meta.column_doc，去重键取自 meta.metric.dedup_keys。
SQL_CASES = [
    {"id": "01基线聚合", "why": "两边都该满分；验判据不恒红",
     "user": "表 t_sales_order(order_time, total_amount, deleted_flag, order_status)\n"
             "口径：有效订单 = deleted_flag=0 AND order_status NOT IN ('0','108','199')\n问：本月销售额",
     "need": [r"SUM\(\s*total_amount", r"deleted_flag\s*=\s*0", r"order_status\s+NOT\s+IN"]},
    {"id": "02去重", "why": "明细有系统级 2x 重复行；漏去重 = 金额虚增一倍（评测真抓过 41%）",
     "user": "表 t_sales_order(sales_order_code, order_time, deleted_flag, order_status)\n"
             "表 t_sales_order_detail(sales_order_code, sku_code, sku_name, box_quantity, amount, deleted_flag)\n"
             "口径：有效订单 = deleted_flag=0 AND order_status NOT IN ('0','108','199')\n"
             "口径：⚠️ t_sales_order_detail 含系统级 2x 重复行（整行×2，ETL 双写），聚合前【必须】按 "
             "(sales_order_code, sku_code, sku_name, box_quantity, amount) GROUP BY 去重，否则金额虚增一倍\n"
             "问：本月订单明细的总金额",
     "need": [r"SUM\(", r"deleted_flag\s*=\s*0"], "dedup5": True},
    {"id": "03快照", "why": "每日快照直接 SUM 会把 N 天累加，虚增几十倍",
     "user": "表 t_winc_stock_report(product_stock_date, warehouse_code, sku_code, stock_quantity, deleted_flag)\n"
             "口径：⚠️ 本表是每日快照（N 天 × N 仓）。聚合库存必须限定 "
             "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)\n问：当前总库存数量",
     "need": [r"MAX\(\s*product_stock_date", r"SUM\(\s*stock_quantity"]},
    {"id": "04双流", "why": "发票老表/新表都在写、交集为 0；只查一张漏 52%",
     "user": "表 t_invoice_apply_header(apply_code, apply_time, invoice_time, invoice_amount, deleted_flag)\n"
             "表 t_invoice_new_apply_header(apply_code, apply_time, invoice_time, invoice_amount, deleted_flag)\n"
             "口径：⚠️ 发票双流并行且两表都在持续写入、交集为 0，问全量开票必须 UNION ALL 两表\n"
             "口径：时间列用 apply_time —— invoice_time 全表全 NULL\n问：今年开票总金额",
     "need": [r"UNION\s+ALL", r"t_invoice_apply_header", r"t_invoice_new_apply_header", r"apply_time"],
     "forbid": [r"invoice_time"]},
    {"id": "05幻觉列", "why": "正确外键是 goods_category_code=cat.id；写 category_code/category_id 必 1054",
     "user": "表 t_sales_order_detail(sales_order_code, sku_code, amount, deleted_flag)\n"
             "表 t_goods(goods_code, goods_name, goods_category_code, deleted_flag)\n"
             "表 t_goods_category(id, category_name)\n"
             "口径：⚠️ 连商品分类的正确外键是 t_goods.goods_category_code = t_goods_category.id，"
             "不是 category_code/category_id（那两个是幻觉列，写了必 1054）\n"
             "口径：明细的 sku_code 对应 t_goods.goods_code\n问：各商品分类的销售额",
     "need": [r"goods_category_code\s*=\s*\w*\.?\s*id",
              r"sku_code\s*=\s*\w*\.?goods_code|goods_code\s*=\s*\w*\.?sku_code"],
     "forbid": [r"(?<!_)category_code\s*=", r"(?<!_)category_id\s*="]},
    {"id": "06全NULL列", "why": "按全 NULL 的列筛必得 0 行假结论",
     "user": "表 t_market_claim_header(claim_application_no, application_date, applied_time, created_time, "
             "applied_amount, deleted_flag)\n"
             "口径：⚠️ application_date 全表【全 NULL】，按它筛必假 0。申请时间用 applied_time\n问：本月报销申请金额",
     "need": [r"applied_time", r"SUM\("], "forbid": [r"application_date"]},
    {"id": "07滚动快照禁SUM", "why": "balance 是滚动累计快照，SUM 会 10 倍虚增；要 ROW_NUMBER 取各桶最新",
     "user": "表 t_customer_balance(customer_code, balance_type, balance, amount, created_time, id)\n"
             "口径：⚠️ balance 列是滚动快照，**绝不可 SUM**（实测 10 倍虚增）。余额排行必须 "
             "ROW_NUMBER() OVER(PARTITION BY customer_code,balance_type ORDER BY created_time DESC, id DESC) "
             "取各桶最新再 SUM\n口径：现金余额 balance_type IN ('8','9')\n问：现金余额最高的前 10 个客户",
     "need": [r"ROW_NUMBER\(\)", r"PARTITION\s+BY", r"balance_type\s+IN", r"LIMIT\s+10"]},
    {"id": "08环比", "why": "上期窗口要与本期同锚点",
     "user": "表 t_sales_order(order_time, total_amount, deleted_flag, order_status)\n"
             "口径：有效订单 = deleted_flag=0 AND order_status NOT IN ('0','108','199')\n"
             "问：本月销售额和上月销售额，放在同一行两列",
     "need": [r"CASE|SUM\(.*CASE|IF\(", r"INTERVAL\s+1\s+MONTH"]},
    {"id": "09口径冲突该拒绝", "why": "库里没有的东西该 refuse，不该编一条能跑的 SQL",
     "user": "表 t_customer_price(customer_code, goods_code, price, updated_time, deleted_flag)\n"
             "口径：⚠️ 本表每行 = 一个客户×商品的**现行**价目档，本库【无价格变更历史】，"
             "『调整/变更次数』不可算\n问：今年每个客户的价格调整了多少次",
     "refuse": True},
    {"id": "10破坏性该拒绝", "why": "破坏性指令必须拒绝（我们有闸门兜，但模型层就该拒）",
     "user": "表 t_sales_order(order_time, deleted_flag)\n问：把今天的订单删掉",
     "refuse": True, "forbid": [r"\bDELETE\b", r"\bUPDATE\b", r"\bTRUNCATE\b", r"\bDROP\b"]},
    {"id": "11稀疏维度", "why": "门店维度 99% 为空，按它分组无意义 —— 该说明而不是答稀疏结果",
     "user": "表 t_sales_order(shop_name, total_amount, order_time, deleted_flag, order_status)\n"
             "口径：⚠️ shop_name 在 20.6 万单中有 20.5 万单为空（仅约 1000 单有值），"
             "按门店分组的经营分析无意义，应向用户说明\n问：各门店本月销售额",
     "refuse": True},
    {"id": "12多跳JOIN", "why": "三表桥接 + 各自的表级口径都要带上；漏一个就虚高",
     "user": "表 t_sales_order o(sales_order_code, customer_code, order_time, deleted_flag, order_status)\n"
             "表 t_sales_order_detail d(sales_order_code, sku_code, amount, deleted_flag)\n"
             "表 t_customer c(customer_code, province, deleted_flag)\n"
             "口径：有效订单 = o.deleted_flag=0 AND o.order_status NOT IN ('0','108','199')\n"
             "口径：t_sales_order_detail 与 t_customer 各自也有 deleted_flag，都要 =0\n问：本月各省份的明细金额",
     "need": [r"province", r"GROUP\s+BY", r"o\.deleted_flag\s*=\s*0",
              r"d\.deleted_flag\s*=\s*0", r"c\.deleted_flag\s*=\s*0"]},
]

# ── 题集②：fast 档的**真实工作**。它在系统里不产 SQL，干的是四件判断题：
#    复合拆解（compound::try_compound）、追问改写（rewrite_followup）、
#    意图充分性（need_intent_reply 的语义）、选源（source::select_source）。
#    拿产 SQL 的题去评 fast 档，测的不是它在系统里的岗位。
SYS_FAST = (
    "你是问数系统的**路由前置**模型，不产 SQL。只输出 JSON，不要解释。\n"
    "字段按任务说明给；拿不准就给保守答案（不拆/不改写/判意图不足）。"
)
FAST_CASES = [
    {"id": "F1拆解-两指标", "why": "compound::try_compound：一句话两个指标要拆成两个子问句",
     "sys": SYS_FAST,
     "user": "任务：判断问句是否包含**多个独立指标**，需要拆成多个子问句分别取数。\n"
             "输出 {\"split\": true/false, \"subs\": [\"子问句1\",\"子问句2\"]}\n"
             "问句：本月销售额和订单数分别是多少",
     "json": {"split": True}, "need_json": [("subs_len", 2)]},
    {"id": "F2拆解-单指标不拆", "why": "**不该拆的别拆**：一个指标带一个维度是单问句，拆了就多付一倍",
     "sys": SYS_FAST,
     "user": "任务：判断问句是否包含**多个独立指标**，需要拆成多个子问句分别取数。\n"
             "输出 {\"split\": true/false, \"subs\": [...]}\n问句：本月各省份的销售额",
     "json": {"split": False}},
    {"id": "F3改写-追问", "why": "rewrite_followup：把省略主语的追问补全成独立问句",
     "sys": SYS_FAST,
     "user": "任务：结合上一轮把追问改写成**独立完整**的问句。输出 {\"q\": \"改写后的问句\"}\n"
             "上一轮问句：本月销售额是多少\n本轮追问：那上月呢",
     "need_q": [r"上月", r"销售额"], "forbid_q": [r"那", r"呢"]},
    {"id": "F4改写-不是追问", "why": "**不是追问就别改**：完整问句被改写会把用户的原意改掉",
     "sys": SYS_FAST,
     "user": "任务：结合上一轮把追问改写成独立完整的问句；若本轮**本身已完整**，原样返回。"
             "输出 {\"q\": \"...\"}\n上一轮问句：本月销售额是多少\n本轮：今年各省份的退款额是多少",
     "need_q": [r"今年", r"各省", r"退款"], "forbid_q": [r"销售额"]},
    {"id": "F5意图-裸实体名", "why": "用户只发一个客户名 → 意图不足该反问，不该猜（业主报的准确度 bug）",
     "sys": SYS_FAST,
     "user": "任务：判断问句是否**表达了明确的取数意图**（要什么指标/要什么口径）。\n"
             "输出 {\"clear\": true/false, \"ask_back\": \"若不明确，反问什么\"}\n"
             "问句：嗨肉\n（提示：这是一个客户名称，问句里没有任何指标词、时间词、疑问词）",
     "json": {"clear": False}},
    {"id": "F6意图-完整问句", "why": "**别把正常问句判成意图不足**：那样系统一句都答不出",
     "sys": SYS_FAST,
     "user": "任务：判断问句是否表达了明确的取数意图。输出 {\"clear\": true/false, \"ask_back\": \"...\"}\n"
             "问句：今年审核通过的对账单有多少笔",
     "json": {"clear": True}},
    {"id": "F7选源", "why": "source::select_source：按数据源描述挑对的那个",
     "sys": SYS_FAST,
     "user": "任务：从数据源里挑**唯一**能回答问句的那个。输出 {\"ds\": \"数据源id\"}\n"
             "数据源：\n- dms：经销商管理系统，含销售订单、客户主档、商品、库存、开票、售后\n"
             "- crm_pg：客户关系管理，含销售线索、商机、跟进记录、合同\n"
             "- up_xlsx_2026q1：用户上传的表格，含 2026 年一季度的门店拜访打卡明细\n"
             "问句：本月销售额是多少",
     "json": {"ds": "dms"}},
    {"id": "F8选源-上传表", "why": "问句明确指向上传的表格时不许回落到主源（那会答出无关的数）",
     "sys": SYS_FAST,
     "user": "任务：从数据源里挑**唯一**能回答问句的那个。输出 {\"ds\": \"数据源id\"}\n"
             "数据源：\n- dms：经销商管理系统，含销售订单、客户主档、商品、库存、开票、售后\n"
             "- crm_pg：客户关系管理，含销售线索、商机、跟进记录、合同\n"
             "- up_xlsx_2026q1：用户上传的表格，含 2026 年一季度的门店拜访打卡明细\n"
             "问句：一季度门店拜访打卡了多少次",
     "json": {"ds": "up_xlsx_2026q1"}},
]

BAD_FN = re.compile(r"\b(CUR|NOW_DATE|TODAY|CURRENT_DAY)\s*\(", re.I)

# 🔴 时间口径必须写进 prompt，否则测的是「猜常识」不是「照口径卡做」。
#
# 实测：`qwen3.7-flash` 对「本月销售额」**三趟全部拒答**，理由「口径卡未定义『本月』的时间范围」
# —— 而我的 system prompt 里写着「拿不准就 refuse」，等于我自己诱导了它。
# 补上这一段后同样两道题 3/3 满分。deepseek 在同样条件下不拒（模型风格差异），
# 但公平的题集不该让「肯猜的模型」占便宜：本仓真实的 LLM prompt 里时间窗是给足的
# （`time_window`/`prev_window` 是确定性模板），所以这里也给足。
TIME_CARD = (
    "\n口径：本月 = 时间列 >= DATE_FORMAT(CURDATE(),'%Y-%m-01') "
    "AND 时间列 < DATE_ADD(DATE_FORMAT(CURDATE(),'%Y-%m-01'), INTERVAL 1 MONTH)"
    "\n口径：今年 = YEAR(时间列) = YEAR(CURDATE())"
)
for _c in SQL_CASES:
    if ("本月" in _c["user"] or "今年" in _c["user"]) and not _c.get("refuse"):
        _c["user"] += TIME_CARD


def _file_provider(cfg):
    configured = str(cfg.get("llm_provider") or "").strip()
    if configured:
        return configured
    base = str(cfg.get("llm_base_url") or "").lower()
    if "dashscope.aliyuncs.com" in base:
        return "qwen"
    if "deepseek.com" in base:
        return "deepseek"
    return ""


def providers(tier):
    cfg = tool_settings.load()
    keys = cfg.get("llm_keys") or {}
    if not isinstance(keys, dict):
        sys.exit(f"{tool_settings.settings_path()} 的 llm_keys 必须是对象")

    file_provider = _file_provider(cfg)
    legacy_key = str(cfg.get("llm_api_key") or "").strip()
    out, missing = {}, []
    for name, spec in PROVIDER_CATALOG.items():
        key = str(keys.get(name) or "").strip()
        if not key and name == file_provider:
            key = legacy_key
        if not key:
            missing.append(name)
            continue
        out[name] = {
            "base": spec["base"].rstrip("/"),
            "key": key,
            "model": spec[tier],
            "extra": spec["extra"],
        }
    if missing:
        names = "、".join(missing)
        sys.exit(
            f"{tool_settings.settings_path()} 缺少 {names} 的模型密钥"
            "（请配置 llm_keys；llm_api_key 仅兜底当前 llm_provider）"
        )
    return out


def call(p, system, user, timeout=180):
    body = {"model": p["model"], "temperature": 0.1,
            "response_format": {"type": "json_object"},
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            **p["extra"]}
    req = urllib.request.Request(
        f"{p['base']}/chat/completions", data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {p['key']}", "Content-Type": "application/json"})
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            v = json.load(r)
        ms = round((time.time() - t0) * 1000)
        m = v["choices"][0]["message"]
        return ms, (m.get("content") or "").strip(), v.get("usage", {}).get("completion_tokens", 0), None
    except urllib.error.HTTPError as e:
        return round((time.time() - t0) * 1000), "", 0, e.read().decode("utf-8", "replace")[:150]
    except Exception as e:
        return round((time.time() - t0) * 1000), "", 0, f"{type(e).__name__}: {e}"


def as_json(txt):
    t = re.sub(r"^```(?:json)?|```$", "", txt.strip(), flags=re.M).strip()
    try:
        return json.loads(t)
    except json.JSONDecodeError:
        return None


def hallucinated_cols(sql):
    """回库核幻觉列（`check-sql` 走字段白名单）。库不可用时返 ''（不判）。"""
    if not sql:
        return ""
    r = subprocess.run(CLI + ["check-sql", sql], capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    out = (r.stdout or "") + (r.stderr or "")
    if "发现幻觉列" in out:
        return [l for l in out.strip().splitlines() if l.strip()][-1][:100]
    return ""


def grade_sql(c, txt):
    o = as_json(txt) or {}
    sql, refuse = (o.get("sql") or "").strip(), (o.get("refuse") or "").strip()
    if not o:
        sql = txt
    miss = []
    if c.get("refuse"):
        ok = bool(refuse) and not sql
        for p in c.get("forbid", []):
            if sql and re.search(p, sql, re.I):
                miss.append(f"**产了禁止语句** /{p}/")
        if not ok:
            miss.append(f"该拒绝却产了 SQL：{' '.join(sql.split())[:80]}" if sql else "既没拒绝也没 SQL")
        return (1 if ok and not miss else 0), 1, miss
    if refuse:
        return 0, len(c.get("need", [])) + int(bool(c.get("dedup5"))), [f"不该拒绝却拒绝了：{refuse[:70]}"]
    hits, pats = 0, c.get("need", [])
    for p in pats:
        if re.search(p, sql, re.I):
            hits += 1
        else:
            miss.append(f"缺 /{p}/")
    total = len(pats)
    for p in c.get("forbid", []):
        if re.search(p, sql, re.I):
            miss.append(f"**含禁词** /{p}/")
    if c.get("dedup5"):
        total += 1
        keys = ["sales_order_code", "sku_code", "sku_name", "box_quantity", "amount"]
        seg = re.split(r"GROUP\s+BY|SELECT\s+DISTINCT", sql, flags=re.I)
        if len(seg) > 1 and any(all(k in s for k in keys) for s in seg[1:]):
            hits += 1
        else:
            miss.append("**去重不全**（GROUP BY/DISTINCT 里没同时出现五元组）")
    if BAD_FN.search(sql):
        miss.append(f"**幻觉函数** {BAD_FN.search(sql).group(0)!r}（执行即报错）")
    if h := hallucinated_cols(sql):
        miss.append(f"**幻觉列**（回库核）{h}")
    return hits, total, miss


def grade_fast(c, txt):
    """fast 档判据：JSON 字段等值 + 改写结果的 need/forbid。满分 = 判据条数。"""
    o = as_json(txt)
    if o is None:
        return 0, 1, [f"不是合法 JSON：{txt[:80]}"]
    miss, hits, total = [], 0, 0
    for k, want in (c.get("json") or {}).items():
        total += 1
        got = o.get(k)
        if got == want:
            hits += 1
        else:
            miss.append(f"{k}={got!r} ≠ {want!r}")
    for kind, want in c.get("need_json", []):
        if kind == "subs_len":
            total += 1
            n = len(o.get("subs") or [])
            if n == want:
                hits += 1
            else:
                miss.append(f"subs 有 {n} 个 ≠ {want}")
    q = (o.get("q") or "").strip()
    for p in c.get("need_q", []):
        total += 1
        if re.search(p, q):
            hits += 1
        else:
            miss.append(f"改写结果缺 /{p}/：{q[:60]!r}")
    for p in c.get("forbid_q", []):
        total += 1
        if re.search(p, q):
            miss.append(f"改写结果**残留** /{p}/：{q[:60]!r}")
        else:
            hits += 1
    return hits, max(1, total), miss


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tier", choices=["fast", "precise"], default="precise")
    ap.add_argument("--suite", choices=["sql", "fast"], default="sql")
    ap.add_argument("--runs", type=int, default=3)
    a = ap.parse_args()
    cases = SQL_CASES if a.suite == "sql" else FAST_CASES
    grade = grade_sql if a.suite == "sql" else grade_fast
    prov = providers(a.tier)
    names = list(prov)
    print(f"对拍【{a.tier} 档 / {a.suite} 题集】：{len(cases)} 题 × {len(names)} 模型 × {a.runs} 趟"
          f"（temperature=0.1，每题取最差的一趟）")
    print("  " + "   ".join(f"{n}={prov[n]['model']}" for n in names) + "\n")
    agg = {n: {"score": 0, "total": 0, "ms": [], "tok": 0, "fail": [], "err": 0} for n in names}
    for c in cases:
        print(f"{'='*16} 【{c['id']}】{c['why']}")
        for n in names:
            best, lat = None, []
            for _ in range(a.runs):
                ms, txt, tok, err = call(prov[n], c.get("sys", SYS_SQL), c["user"])
                lat.append(ms)
                if err:
                    agg[n]["err"] += 1
                    cand = (0, 1, [f"API 失败 {err[:70]}"])
                else:
                    cand = grade(c, txt)
                    agg[n]["tok"] += tok
                if best is None or (cand[0] - len(cand[2])) < (best[0] - len(best[2])):
                    best = cand
            h, t, miss = best
            agg[n]["score"] += h
            agg[n]["total"] += t
            agg[n]["ms"] += lat
            ok = h == t and not miss
            if not ok:
                agg[n]["fail"].append(c["id"])
            print(f"  {n:9} {'✅' if ok else '❌'} {h}/{t}  中位 {int(statistics.median(lat))}ms"
                  f"（{min(lat)}~{max(lat)}）")
            for x in miss[:3]:
                print(f"      {x}")
    print(f"\n{'='*16} 汇总【{a.tier} / {a.suite}】（{a.runs} 趟取最差）")
    out = {}
    for n in names:
        g = agg[n]
        med = int(statistics.median(g["ms"]))
        p95 = int(sorted(g["ms"])[max(0, int(len(g["ms"]) * 0.95) - 1)])
        print(f"  {n:9} 准确 {g['score']}/{g['total']} = {g['score']/max(1,g['total']):.0%}"
              f"   题级全对 {len(cases)-len(g['fail'])}/{len(cases)}"
              f"   延迟 中位 {med}ms / p95 {p95}ms   输出 {g['tok']} tok   API 失败 {g['err']}")
        if g["fail"]:
            print(f"            失分题：{', '.join(g['fail'])}")
        out[n] = {"model": prov[n]["model"], "score": g["score"], "total": g["total"],
                  "cases_ok": len(cases) - len(g["fail"]), "cases": len(cases),
                  "median_ms": med, "p95_ms": p95, "tok": g["tok"], "fail": g["fail"], "err": g["err"]}
    dst = ROOT / "tools" / f"model_ab_{a.tier}_{a.suite}.json"
    dst.write_text(json.dumps(out, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"\n→ {dst.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
