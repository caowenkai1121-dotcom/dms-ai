#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""meta.datamap_edge 三处 DDL 常量逐字对账（数据地图完备性包 D2 的纪律闸）。

三处共用一张表（CREATE IF NOT EXISTS 先跑者赢，不同构就是 race），DDL 文本必须逐字一致：
  1. crates/server/src/datamap_api.rs   —— const DDL（正本，复核域）
  2. crates/semantic/src/datamap.rs     —— const DATAMAP_DDL（静态推断写口）
  3. crates/semantic/src/datamap_usage.rs —— const DDL（使用轨迹写口）

用法：python tools/check_datamap_ddl.py
退出码：0 = 三处逐字一致（打印 SHA-256 与 kind 取值集合）；1 = 漂移（逐处 diff 定位）。
"""
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (文件, const 名)
SPOTS = [
    (ROOT / "crates/server/src/datamap_api.rs", "DDL"),
    (ROOT / "crates/semantic/src/datamap.rs", "DATAMAP_DDL"),
    (ROOT / "crates/semantic/src/datamap_usage.rs", "DDL"),
]

def extract(path: Path, name: str) -> str:
    """抽取 `const <NAME>: &str = r#"..."#;` 的原始字符串体（逐字，含首尾空行）。"""
    src = path.read_text(encoding="utf-8")
    m = re.search(
        r"const " + re.escape(name) + r": &str = r#\"(?P<body>.*?)\"#;",
        src,
        re.DOTALL,
    )
    if not m:
        raise SystemExit(f"[FAIL] {path}: 找不到 const {name} 的 r# 原始字符串")
    return m.group("body")

def main() -> int:
    bodies = [(p, n, extract(p, n)) for p, n in SPOTS]
    ref = bodies[0][2]
    ok = all(b == ref for _, _, b in bodies[1:])
    if not ok:
        print("[FAIL] 三处 DDL 漂移：")
        for p, n, b in bodies:
            mark = "（基准）" if b is ref else ("= 一致" if b == ref else "≠ 漂移")
            print(f"  {mark}  {p.relative_to(ROOT)} :: {n}  (len={len(b)})")
        # 简单定位第一处差异
        for p, n, b in bodies[1:]:
            if b != ref:
                for i, (x, y) in enumerate(zip(ref, b)):
                    if x != y:
                        print(f"  首个差异 @ 字节 {i}: 基准 {ref[max(0,i-40):i+40]!r}")
                        print(f"{' ' * 22} 该处 {b[max(0,i-40):i+40]!r}")
                        break
        return 1
    sha = hashlib.sha256(ref.encode("utf-8")).hexdigest()
    kinds = re.search(r"CHECK \(kind IN \(([^)]+)\)\)", ref)
    print("[OK] 三处 DDL 逐字一致")
    for p, n, _ in bodies:
        print(f"  - {p.relative_to(ROOT)} :: {n}")
    print(f"  字节数: {len(ref.encode('utf-8'))}")
    print(f"  SHA-256: {sha}")
    print(f"  kind 取值集合: {kinds.group(1) if kinds else '未找到'}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
