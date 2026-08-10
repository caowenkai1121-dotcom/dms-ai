"""【K4】上传表格「双通道」端到端验收：上传 → 建表+登记+采 schema → 对该源问数。

为什么必须有这个脚本：K4 落地后 `INTEGRATION-TRACE` 长期把这条通道标成「半 —— 建表与
入库已落地，问数通道未连库实测」。那句「未实测」底下压着三个各自独立、且都**不报错**
的缺陷（2026-07-28 一次实测同时暴露）：

  1. prompt 与闸门都硬写 MySQL 方言 → LLM 对 PG 源写出 ``AS `别名` `` → PG 当场
     ``syntax error at or near "`"``。非 MySQL 源问数**恒失败**。
  2. 上传源共用一条 `pg_ro_url`、schema 一份一个，建池不置 `search_path` →
     `probe_schema()`（按 `current_schema()` 过滤）一张表都采不到 → LLM 拿到空 schema。
  3. `sync_schema` 的备份表启发式（表名结尾连续 ≥4 位数字）会误伤上传表名
     `t0_<uuid 去横线>`，概率约 (10/16)^4 ≈ 15% —— 每 6 份上传约 1 份静默不可问数。

三个都属于「建表成功、检索可用、只有问数死掉」，日志里一个字都没有。故本脚本的存在
不是为了测新功能，是为了让这条通道**不能再无声退化**。

判据：
 ① 响应带 `datasource`（通道②触发：schema 建出 + 数据源登记 + 上传者授权）
 ② schema 已进注册表 —— **由行为反推**，不直连 PG：模型写得出清洗后的列名 `c0`/`c2`，
    就说明 `meta.column_doc` 的列注释（中文表头）确实进了 prompt。中文表头进注释而非
    列名是刻意的（标识符安全 + I5），所以「用对 c2」是这条链路唯一的可观测证据。
 ③ 数值正确（销售部 340+260=600），且 SQL 是本方言的写法（PG 用双引号别名、裸表名）。

用法：`python -u tools/up_probe.py`（服务需已起在 8100）。退出码非 0 = 有判据没过。
加 `--keep` 保留上传的文档（默认跑完删掉，见下）。

🔴 **默认自清理，别去掉**：每跑一次留一份台账 = 留一个 `active` 数据源。
`select_source` 在「可见源 > 1」时会 embed 问句去挑源，也就是说**每份残留的测试台账都会
去竞争所有问句的路由**。今天还看不出来，只因为 `meta.datasource.embedding` 一行都没写
（`nearest_datasources` 恒空 → 降级主源）；那一天一开，攒下来的垃圾源就会开始抢答。
`DELETE /api/kb/doc/{id}` 会连带 `DROP SCHEMA up_… CASCADE` 与注销数据源，一次干净。
"""
import io
import json
import sys
import urllib.error
import urllib.request
import uuid

sys.stdout.reconfigure(encoding="utf-8")
BASE = "http://127.0.0.1:8100"

TAG = uuid.uuid4().hex[:6]
# 🔴 随机串必须进**内容**：ingest 按 sha256 去重，只改文件名会命中旧 doc，
# 通道②整段不重跑，脚本会拿到 `datasource: null` 而误报成「通道②没触发」。
CSV = (
    "部门,员工姓名,月度销量,入职日期\n"
    "财务部,张三,120,2024-01-15\n"
    "销售部,李四,340,2023-06-01\n"
    "销售部,王五,260,2025-03-20\n"
    f"仓储部,赵六{TAG},80,2022-11-11\n"
)
NAME = f"部门销量台账{TAG}.csv"
EXPECT = 600  # 销售部 340 + 260


def multipart(fields, fname, data):
    b = uuid.uuid4().hex
    out = io.BytesIO()
    for k, v in fields.items():
        out.write(f'--{b}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode())
    out.write(
        f'--{b}\r\nContent-Disposition: form-data; name="file"; filename="{fname}"\r\n'.encode()
    )
    out.write(b"Content-Type: text/csv\r\n\r\n" + data + b"\r\n")
    out.write(f"--{b}--\r\n".encode())
    return f"multipart/form-data; boundary={b}", out.getvalue()


KEEP = "--keep" in sys.argv
DOC = {"id": None}


def cleanup():
    """删掉本次上传的文档（连带 DROP schema + 注销数据源）。失败只告警：
    判据的成败不该被清理动作改写，但残留必须说出来，否则下次跑的人不知道库里多了个源。"""
    if KEEP or not DOC["id"]:
        return
    r = urllib.request.Request(
        f"{BASE}/api/kb/doc/{DOC['id']}?login_name=admin", method="DELETE"
    )
    try:
        urllib.request.urlopen(r, timeout=120)
        print(f"  ↺ 已清理 doc {DOC['id']}（schema 与数据源一并注销）")
    except urllib.error.HTTPError as e:
        print(f"  ⚠ 清理失败 HTTP {e.code}：库里残留了一个 active 上传源 {DOC['id']}")


def fail(msg):
    print(f"  ✗ {msg}")
    cleanup()
    sys.exit(1)


ct, body = multipart({"login_name": "admin"}, NAME, CSV.encode("utf-8"))
req = urllib.request.Request(
    BASE + "/api/kb/upload?login_name=admin", data=body, method="POST",
    headers={"Content-Type": ct},
)
try:
    up = json.loads(urllib.request.urlopen(req, timeout=300).read().decode())
except urllib.error.HTTPError as e:
    fail(f"上传 HTTP {e.code}: {e.read().decode()[:400]}")

src = up.get("datasource") or {}
ds = src.get("ds_id")
DOC["id"] = up.get("doc_id")
print(f"① 上传 doc_id={up.get('doc_id')}")
print(f"   datasource={json.dumps(src, ensure_ascii=False)}")
if not ds:
    fail("通道②没触发（响应无 datasource）——建表或登记失败，看 server 日志的 warn")

r = urllib.request.Request(
    BASE + "/api/ask", method="POST", headers={"Content-Type": "application/json"},
    data=json.dumps({"question": "销售部一共卖了多少", "login_name": "admin", "ds": ds}).encode(),
)
try:
    a = json.loads(urllib.request.urlopen(r, timeout=300).read().decode())
except urllib.error.HTTPError as e:
    fail(f"问数 HTTP {e.code}: {e.read().decode()[:600]}")

sql = a.get("sql") or ""
rows = a.get("rows") or []
print(f"② SQL（route={a.get('route')}）：{sql}")
# 清洗后的列名出现 = 列注释（中文表头）确实进了 prompt
if "c0" not in sql and "c2" not in sql:
    fail("SQL 没用清洗后的列名 c0/c2 —— schema 没进 prompt（注册表采集那一步断了）")
if "`" in sql:
    fail("SQL 带反引号 —— 方言没跟着源走，PG 会直接语法错")

print(f"③ rows={rows}")
got = None
for row in rows:
    for c in row:
        try:
            got = float(str(c).replace(",", ""))
        except (TypeError, ValueError):
            continue
if got is None or abs(got - EXPECT) > 1e-6:
    fail(f"数值不对：期望 {EXPECT}（340+260），实得 {got}")
print(f"  ✅ 三条判据全过（{ds}）")
cleanup()
