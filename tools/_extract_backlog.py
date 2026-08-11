# -*- coding: utf-8 -*-
"""把两个 AgentSwarm 输出文件里的 markdown 表格行抽成 backlog 清单。"""
import io
import re
import sys
from pathlib import Path

FILES = [
    r"C:/Users/caowe/.kimi-code/sessions/wd_dms_ai_96dda0b82f7d/session_81478743-5e57-4269-94be-1f875b7a4339/agents/main/tool-results/AgentSwarm-tool_mj5szcnO5GqiAIweXjkKqaHq-c4f9e0ab-491c-4237-b251-0b1503184314.txt",
    r"C:/Users/caowe/.kimi-code/sessions/wd_dms_ai_96dda0b82f7d/session_81478743-5e57-4269-94be-1f875b7a4339/agents/main/tool-results/AgentSwarm-tool_BwWZJme2yh3u8JAeSghmB4RJ-84062e8a-1dfe-4623-9c08-517bd9c1dfa8.txt",
]

ROW = re.compile(r"^\|(.+)\|$")
items = []
for f in FILES:
    text = io.open(f, encoding="utf-8").read()
    for line in text.splitlines():
        line = line.strip()
        if not ROW.match(line):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) < 4:
            continue
        loc = cells[0]
        if "文件" in loc and "行" in loc:  # 表头
            continue
        if set(loc) <= set("-: "):  # 分隔行
            continue
        if ":" not in loc and ".rs" not in loc and ".vue" not in loc and ".py" not in loc and ".ts" not in loc and ".js" not in loc:
            continue
        items.append((loc, cells[1], cells[2], cells[3] if len(cells) > 3 else ""))

print("raw items:", len(items))
# 去重：同文件同行号同问题前缀
seen = set()
uniq = []
for it in items:
    key = (it[0], it[1][:30])
    if key in seen:
        continue
    seen.add(key)
    uniq.append(it)
print("unique items:", len(uniq))
safe = sum(1 for i in uniq if "safe" in i[3].lower())
print("safe:", safe, " test/其他:", len(uniq) - safe)

# 按文件分组写出
by_file = {}
for it in uniq:
    by_file.setdefault(it[0].split(":")[0], []).append(it)

out = ["# 优化清单（六角色评审 + 全仓分文件审计 + 三路调研）\n",
       f"共 {len(uniq)} 条（safe {safe} / test {len(uniq)-safe}）。来源：全仓逐文件审计 swarm×2 + DMS 后端源码校准 + 开源系统差距 + 小程序集成点。\n"]
for f in sorted(by_file, key=lambda x: -len(by_file[x])):
    out.append(f"\n## {f}（{len(by_file[f])} 条）\n")
    out.append("| 位置 | 问题 | 修法 | 级别 |")
    out.append("|---|---|---|---|")
    for it in by_file[f]:
        out.append("| " + " | ".join(c.replace("|", "\\|") for c in it) + " |")
Path("docs/OPTIMIZATION-BACKLOG.md").write_text("\n".join(out), encoding="utf-8")
print("written docs/OPTIMIZATION-BACKLOG.md")
