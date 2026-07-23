# M1 判官：数据权限双实现对拍
# Python 侧按 Java DefaultEmployee.java 语义独立重算集合，与 Rust CLI(scope 子命令)输出对拍；
# 再用双方条件各跑 t_sales_order COUNT 验证行级一致。全程只读。
import json, pymysql, re, subprocess, sys
from urllib.parse import unquote
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
cfg = json.load(open(ROOT / "settings.json", encoding="utf-8"))
m = re.match(r"mysql://([^:]+):([^@]+)@([^:]+):(\d+)/(\w+)", cfg["mysql_url"])
user, pwd, host, port, db = m.groups()
conn = pymysql.connect(host=host, port=int(port), user=unquote(user), password=unquote(pwd),
                       database=db, charset="utf8mb4")
cur = conn.cursor()
cur.execute("SET SESSION TRANSACTION READ ONLY")

def q(sql, args=None):
    cur.execute(sql, args)
    return [r for r in cur.fetchall()]

def col(sql, args=None):
    return [r[0] for r in q(sql, args)]

SENTINEL = -1

def py_scope(login, role_code):
    """Java DefaultEmployee 语义的独立 Python 复刻（与 Rust 双实现互证）"""
    emp = q("SELECT employee_id, actual_name, administrator_flag, department_id FROM t_employee WHERE login_name=%s AND deleted_flag=0", (login,))
    assert emp, f"员工不存在 {login}"
    eid, name, admin_flag, dept_id = emp[0]
    role = q("SELECT r.role_id FROM t_role_employee re JOIN t_role r ON r.role_id=re.role_id WHERE re.employee_id=%s AND TRIM(r.role_code)=%s", (eid, role_code))
    if admin_flag == 1 or role_code == "admin":
        return dict(employee_ids=[], employee_codes=[], customer_codes=[])
    assert role, f"{login} 无角色 {role_code}"
    role_id = role[0][0]
    rows = q("SELECT data_scope_type, view_type FROM t_role_data_scope WHERE role_id=%s", (role_id,))
    assert rows, "角色未设定数据范围"
    base_rows = [v for t, v in rows if t == 1]
    custom = {v for t, v in rows if t == 2}

    def user_depts():
        d = col("SELECT department_id FROM t_employee_department WHERE employee_id=%s AND deleted_flag=0", (eid,))
        return d or ([dept_id] if dept_id is not None else [])

    def dept_children(roots):
        allrows = q("SELECT department_id, parent_id FROM t_department WHERE status=1 AND deleted_flag=0")
        seen, result = set(), []
        for r in roots:
            if r in seen: continue
            seen.add(r); result.append(r)
            frontier = [r]
            while frontier:
                nxt = [i for i, p in allrows if p in frontier and i not in seen]
                for i in nxt: seen.add(i)
                result += nxt; frontier = nxt
        return result

    def dept_employees(depts):
        if not depts: return []
        ph = ",".join(["%s"] * len(depts))
        return col(f"""SELECT DISTINCT t.employee_id FROM t_employee t
            INNER JOIN t_employee_department td ON td.employee_id=t.employee_id AND td.deleted_flag=0 AND td.service_status=0
            WHERE t.department_id IN ({ph}) OR td.department_id IN ({ph})""", depts * 2)

    def subordinates():
        result, frontier = {eid}, [eid]
        while frontier:
            ph = ",".join(["%s"] * len(frontier))
            found = col(f"SELECT DISTINCT employee_id FROM t_employee_department WHERE deleted_flag=0 AND service_status=0 AND manager_id IN ({ph})", frontier)
            frontier = [f for f in found if f not in result]
            result |= set(frontier)
        return list(result)

    base_v = max(base_rows) if base_rows else None
    if base_v is None or base_v == 10:
        return dict(employee_ids=[], employee_codes=[], customer_codes=[])
    if base_v == 0: base_ids = [eid]
    elif base_v == 1: base_ids = dept_employees(user_depts())
    elif base_v == 2: base_ids = dept_employees(dept_children(user_depts()))
    elif base_v == 3: base_ids = [SENTINEL]
    else: raise AssertionError(f"未知 view_type {base_v}")

    sub_ids = subordinates() if 101 in custom else []
    if 101 in custom and not sub_ids: sub_ids = [SENTINEL]

    # employee_ids 合并
    ids, flag = [], True
    if SENTINEL in base_ids: flag = False
    else: ids += base_ids
    if sub_ids:
        if SENTINEL in sub_ids: flag = False
        else: ids += sub_ids
    if not ids and not flag: ids = [SENTINEL]
    ids = list(dict.fromkeys(ids))

    def logins(idlist):
        if not idlist: return []
        ph = ",".join(["%s"] * len(idlist))
        return col(f"SELECT login_name FROM t_employee WHERE employee_id IN ({ph})", idlist)

    codes, cflag = [], True
    if SENTINEL in base_ids: cflag = False
    else: codes += logins(base_ids)
    if sub_ids:
        if SENTINEL in sub_ids: cflag = False
        else: codes += logins(sub_ids)
    if not codes and not cflag: codes = ["-1"]
    codes = list(dict.fromkeys(codes))

    # customer_codes
    cust, kflag = [], True
    if base_ids:
        ph = ",".join(["%s"] * len(base_ids))
        cust += col(f"SELECT customer_code FROM t_customer WHERE deleted_flag=0 AND area_manager_id IN ({ph})", base_ids)
    cust += col("""SELECT DISTINCT v.value_code FROM t_dict_value v JOIN t_dict_key k ON k.dict_key_id=v.dict_key_id
        WHERE k.key_code IN ('payment_customer_for_inside','payment_customer_for_all') AND k.deleted_flag=0 AND v.deleted_flag=0""")
    def group_cust(eids):
        if not eids: return []
        ph = ",".join(["%s"] * len(eids))
        return col(f"""SELECT DISTINCT tc.customer_code FROM t_customer tc WHERE EXISTS (
            SELECT 1 FROM t_employee_customer_group t WHERE t.employee_id IN ({ph})
            AND FIND_IN_SET(t.customer_group, tc.customer_group) > 0)""", eids)
    def mgr_cust(names):
        if not names: return []
        ph = ",".join(["%s"] * len(names))
        return [c for c in col(f"""SELECT DISTINCT customer_code FROM t_customer_contacts_info
            WHERE deleted_flag=0 AND contact_type IN ('Y1','Y3') AND contact_name IN ({ph})""", names) if c and c.strip()]
    if 102 in custom:
        g = group_cust([eid])
        if not g: kflag = False
        else: cust += g
    if 103 in custom:
        mc = mgr_cust([name])
        if not mc: kflag = False
        else: cust += mc
    if 101 in custom and sub_ids and SENTINEL not in sub_ids:
        ph = ",".join(["%s"] * len(sub_ids))
        names = col(f"SELECT actual_name FROM t_employee WHERE employee_id IN ({ph})", sub_ids)
        sc = mgr_cust(names) + group_cust(sub_ids)
        if not sc: kflag = False
        else: cust += sc
    cust = [c for c in dict.fromkeys(cust) if c and c.strip()]
    if not cust and not kflag: cust = ["-1"]
    return dict(employee_ids=ids, employee_codes=codes, customer_codes=cust)

def rust_scope(login, role_code):
    exe = ROOT / "target" / "debug" / "dms-ai-server.exe"
    out = subprocess.run([str(exe), "scope", login, role_code], capture_output=True, text=True,
                         encoding="utf-8", cwd=str(ROOT))
    assert out.returncode == 0, f"rust scope 失败: {out.stderr[-800:]}"
    return json.loads(out.stdout)

def count_with(cond):
    where = "so.deleted_flag = 0" + (f" AND {cond}" if cond else "")
    return q(f"SELECT COUNT(*) FROM t_sales_order so WHERE {where}")[0][0]

def py_condition(s):
    segs = []
    if s["employee_ids"]:
        segs.append(f"so.owner_manager in ({','.join(map(str, s['employee_ids']))})")
    if s["customer_codes"]:
        codes = ",".join("'" + c.replace("'", "''") + "'" for c in s["customer_codes"])
        segs.append(f"so.customer_code in ({codes})")
    return f"({' or '.join(segs)})" if segs else ""

# ── 选判官用户：每档一个在职员工 ──
def pick(role_code):
    r = q("""SELECT e.login_name FROM t_role r JOIN t_role_employee re ON re.role_id=r.role_id
             JOIN t_employee e ON e.employee_id=re.employee_id AND e.deleted_flag=0 AND e.disabled_flag=0
             WHERE TRIM(r.role_code)=%s AND e.administrator_flag=0 ORDER BY e.employee_id LIMIT 1""", (role_code,))
    return r[0][0] if r else None

cases = []
for rc in ["city_manager", "XXJL", "STYY01", "financial_accounting", "provincial_general_manager"]:
    ln = pick(rc)
    if ln: cases.append((ln, rc))
cases.append(("admin", "admin"))  # 超管短路

print(f"判官对拍 {len(cases)} 用户\n" + "=" * 60)
fails = 0
for login, rc in cases:
    try:
        py = py_scope(login, rc)
        rs_full = rust_scope(login, rc)
        rs = rs_full["sets"]
        ok = True
        for k in ["employee_ids", "employee_codes", "customer_codes"]:
            a, b = set(map(str, py[k])), set(map(str, rs[k]))
            if a != b:
                ok = False
                print(f"❌ {login}({rc}) {k} 不一致: py独有{list(a-b)[:5]} rust独有{list(b-a)[:5]} (py={len(a)} rust={len(b)})")
        # 行级对拍：单语句双子查询（同一快照，免受现网实时写入干扰）
        py_where = "so.deleted_flag = 0" + (f" AND {py_condition(py)}" if py_condition(py) else "")
        demo = rs_full["demo_sql"]
        c_py, c_rs = q(f"SELECT (SELECT COUNT(*) FROM t_sales_order so WHERE {py_where}), ({demo})")[0]
        if c_py != c_rs:
            ok = False
            print(f"❌ {login}({rc}) COUNT 不一致: py={c_py} rust={c_rs}")
        if ok:
            sizes = {k: len(py[k]) for k in py}
            print(f"✅ {login}({rc}) 集合一致 {sizes} · t_sales_order 在域行数={c_py}")
        else:
            fails += 1
    except Exception as e:
        fails += 1
        print(f"❌ {login}({rc}) 异常: {str(e)[:200]}")

print("=" * 60)
print("全部通过 ✅" if fails == 0 else f"{fails} 个用例失败 ❌")
conn.close()
sys.exit(1 if fails else 0)
