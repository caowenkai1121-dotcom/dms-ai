# -*- coding: utf-8 -*-
"""把 store.rs 1193-1217 的 SQL 拼出来（去续行符），PREPARE 验语法（不执行）。"""
import io
import re

s = io.open('crates/knowledge/src/store.rs', encoding='utf-8').read()
start = s.find('"WITH locked AS')
# 逐行收集字符串字面量直到 ")" 结束 fixed(
lines = s[start:].split('\n')
parts = []
for ln in lines:
    ln = ln.strip()
    if ln.startswith('")'):
        break
    if ln.startswith('"'):
        # 去掉首尾引号与行尾续行反斜杠
        body = ln
        if body.endswith('\\'):
            body = body[:-1]
        if body.endswith('",'):
            body = body[:-2]
        elif body.endswith('"'):
            body = body[:-1]
        body = body[1:]  # 去首引号
        parts.append(body)
sql = ''.join(parts)
open(r'C:/Users/caowe/AppData/Local/Temp/upsert.sql', 'w', encoding='utf-8').write(sql)
print(sql[:400])
print('...')
# 括号平衡
d = 0
for idx, ch in enumerate(sql):
    if ch == '(':
        d += 1
    elif ch == ')':
        d -= 1
        if d < 0:
            print('EXTRA CLOSE at', idx, repr(sql[idx - 80:idx + 20]))
            break
print('depth at end:', d)
