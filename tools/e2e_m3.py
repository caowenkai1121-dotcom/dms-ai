# M3 NL2SQL 全链路 e2e：连库跑真问题，断言功能正确。
import json, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "debug" / "dms-ai-server.exe"

def ask(login, question, role=None, retries=1):
    # LLM 路径非确定，失败重试（旧项目回归惯例）
    cmd = [str(EXE), "ask", login, question] + ([role] if role else [])
    last = {}
    for _ in range(retries + 1):
        r = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", cwd=str(ROOT))
        if r.returncode == 0:
            j = json.loads(r.stdout)
            if j.get("columns"):
                return j
            last = j
        else:
            last = {"error": r.stderr.strip()[-300:]}
    return last

cases = []
def check(name, cond, detail=""):
    cases.append((name, cond, detail))
    print(f"{'✅' if cond else '❌'} {name}{(' · ' + detail) if detail else ''}")

# 1. 超管全量（走确定性快路径 direct-agg）
r = ask("admin", "本月销售额是多少")
v_admin = float(r["rows"][0][0]) if r.get("rows") and r["rows"][0][0] else 0
check("超管本月销售额", v_admin > 1e8, f"值={v_admin:,.0f} route={r.get('route')} {r.get('elapsed_ms')}ms")
check("走确定性快路径", r.get("route") == "direct-agg", f"route={r.get('route')} 耗时={r.get('elapsed_ms')}ms")
check("超管 SQL 无权限注入", "customer_code in" not in r.get("sql", "").lower(), "")

# 1b. 单号直查（direct-doc）
rd = ask("admin", "帮我查下 HJXH-DXO2026072300384")
check("单号直查出卡", rd.get("route") == "direct-doc" and rd.get("row_count", 0) >= 1,
      f"route={rd.get('route')} 列={len(rd.get('columns', []))} {rd.get('elapsed_ms')}ms")

# 2. 限权注入 + 值更小 + 越权隔离
r2 = ask("tanlibo", "本月销售额是多少", "city_manager")
v_lim = float(r2["rows"][0][0]) if r2.get("rows") and r2["rows"][0][0] else 0
check("城市经理注入在场", "owner_manager in" in r2.get("sql", "").lower() or "customer_code in" in r2.get("sql", "").lower(),
      f"值={v_lim:,.0f}")
check("城市经理值 < 超管全量", 0 < v_lim < v_admin, f"{v_lim:,.0f} < {v_admin:,.0f}")

# 3. 明细问句 ≥8 列
r3 = ask("admin", "查一下昨天的订单明细")
check("明细列数 ≥8", len(r3.get("columns", [])) >= 8, f"列数={len(r3.get('columns', []))}")

# 4. 市场费用口径路由（应走 t_market_total_expense，非专项子表）
r4 = ask("admin", "本月市场费用花了多少")
check("市场费用走合计表", "t_market_total_expense" in r4.get("sql", ""),
      f"route={r4.get('route')} SQL含表={'t_market_total_expense' in r4.get('sql','')}")

# 5. 名称过滤用 LIKE 不用等值
r5 = ask("admin", "恒众餐饮本月买了多少")
sql5 = r5.get("sql", "").lower()
check("名称用 LIKE", "like" in sql5 and "恒众" in r5.get("sql", ""), "")

print("=" * 50)
fails = [n for n, c, _ in cases if not c]
print("全部通过 ✅" if not fails else f"{len(fails)} 失败: {fails}")
sys.exit(1 if fails else 0)
