# 把 workflow 产出的审校通过题目合并进 tools/eval_cases.json（幂等：同名题覆盖）。
# 用法: python tools/merge_eval_cases.py <workflow_journal.jsonl>
import io, json, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DST = ROOT / "tools" / "eval_cases.json"
KEEP = ("name", "login", "role", "q", "tags", "gold_sql", "note")


def pick(c):
    out = {k: c[k] for k in KEEP if c.get(k)}
    # verified/fixed 是过程证据，折进 note 保留可追溯
    extra = " ".join(x for x in (c.get("verified"), c.get("fixed"), c.get("why_hard")) if x)
    if extra:
        out["note"] = (out.get("note", "") + " ｜ " + extra).strip(" ｜")
    return out


def main():
    journal = Path(sys.argv[1])
    accepted = []
    for line in io.open(journal, encoding="utf-8", errors="replace"):
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        if rec.get("type") != "result":
            continue
        res = rec.get("result")
        if isinstance(res, dict) and res.get("accepted"):
            accepted = res["accepted"]      # 审校 agent 的产出（最后一条为准）
    if not accepted:
        print("未找到审校通过的题目"); sys.exit(1)

    doc = json.loads(io.open(DST, encoding="utf-8").read())
    by_name = {c["name"]: c for c in doc["cases"]}
    added = updated = 0
    for c in accepted:
        c = pick(c)
        if not c.get("gold_sql") or not c.get("q"):
            continue
        if c["name"] in by_name:
            by_name[c["name"]].update(c); updated += 1
        else:
            by_name[c["name"]] = c; added += 1
    doc["cases"] = sorted(by_name.values(), key=lambda x: x["name"])
    io.open(DST, "w", encoding="utf-8").write(json.dumps(doc, ensure_ascii=False, indent=2) + "\n")
    print(f"合并完成：新增 {added} / 更新 {updated} / 题集共 {len(doc['cases'])} 题")


main()
