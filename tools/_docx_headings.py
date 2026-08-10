# -*- coding: utf-8 -*-
"""_p_docx 伪标题识别：没有 Word 标题样式的业务文档按 编号/加粗 推断章节结构。"""
import io

p = 'tools/embed_service.py'
s = io.open(p, encoding='utf-8', newline='').read()

old = "_H_DOCX = re.compile(r'(?:Heading|标题)\\s*(\\d)')"
new = """_H_DOCX = re.compile(r'(?:Heading|标题)\\s*(\\d)')
# 伪标题（业务文档常没用 Word 样式，章节靠编号/加粗）：第X章/一、/（一）/1./1.1 等开头。
# 误判代价是把普通行当章节（导图多一层），不漏判更重要 —— 但长句（>40 字）不像标题，排除。
_PSEUDO_HEADING = re.compile(
    r'^(?:第[一二三四五六七八九十百\\d]+[章节条篇]|[一二三四五六七八九十]+[、.．]|'
    r'[（(][一二三四五六七八九十]+[)）]|\\d+(?:\\.\\d+)*[、.．\\s])'
)
_PSEUDO_HEADING_MAX = 40


def _pseudo_heading_level(p, text):
    \"\"\"无样式段的伪标题定级：中文序号/第X章=1，阿拉伯编号=2，整行加粗短句=3；不像标题返回 0。\"\"\"
    if len(text) > _PSEUDO_HEADING_MAX:
        return 0
    import re as _re
    if _re.match(r'^(?:第[一二三四五六七八九十百\\d]+[章节条篇]|[一二三四五六七八九十]+[、.．]|[（(][一二三四五六七八九十]+[)）])', text):
        return 1
    if _re.match(r'^\\d+(?:\\.\\d+)*[、.．\\s]', text):
        return 2
    runs = [r for r in p.runs if r.text.strip()]
    if runs and all(r.bold for r in runs):
        return 3
    return 0"""
assert s.count(old) == 1, 'regex'
s = s.replace(old, new)

old = """            if m := _H_DOCX.search(getattr(p.style, 'name', '') or ''):
                _push(stack, int(m.group(1)), t)
            out.append(_blk(t, None, stack))"""
new = """            if m := _H_DOCX.search(getattr(p.style, 'name', '') or ''):
                _push(stack, int(m.group(1)), t)
            elif (lvl := _pseudo_heading_level(p, t)):
                # 没用 Word 样式的文档按 编号/加粗 推断章节（导图/引用定位靠这个结构）
                _push(stack, lvl, t)
            out.append(_blk(t, None, stack))"""
assert s.count(old) == 1, 'docx'
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('patched')
