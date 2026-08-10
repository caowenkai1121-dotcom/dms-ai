# M1 判官：数据权限双实现对拍
# Python 侧按 Java DefaultEmployee/Visitor/CustomerContacts/ShopContacts 独立重算集合，
# 与 Rust CLI(scope 子命令)输出对拍；
# 再到分析目标用双方条件各跑 t_sales_order COUNT 验证行级一致。全程只读。
import json, pymysql, subprocess, sys
import settings as st
from cli import cli
from pathlib import Path

# 🔴 本机 locale 是 cp936，而本脚本满屏 ✅/❌：stdout 一旦不是 UTF-8 控制台
# （管道、重定向、被别的程序 subprocess 调用）就在**打印结论那一刻** UnicodeEncodeError。
# 实测：`python tools/judge_scope.py --selfcheck > out.txt` → `'gbk' codec can't encode '❌'`。
# 后果与姊妹脚本 evaluation.py 记的完全一样：崩的退出码是 1，与本脚本「1 = 对拍失败」撞车，
# 接门禁的人读成「双实现不一致」，而其实一个用例都没跑。evaluation.py / regression.py /
# kb_eval.py 都有这一行，这里漏了 —— 本轮加反向验证时被重定向当场抓到。
sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
# `--selfcheck` 不连库（本脚本唯一的连库点就是这里，模块级）：判据得能在没有生产库
# 的机器上跑，否则「挑不到人就静默变绿」这条洞永远没有反向验证。
SELFCHECK = "--selfcheck" in sys.argv
if not SELFCHECK:
    # 凭据与 URL 解析都在 tools/settings.py（手写 `mysql://` 正则曾在三处各抄一份，
    # 而口令里的 `@` 必须 percent-encode —— 忘一处 `unquote` 就是「口令看着对、连不上」）
    auth_conn = pymysql.connect(**st.mysql_kwargs())
    auth_cur = auth_conn.cursor()
    auth_cur.execute("SET SESSION TRANSACTION READ ONLY")
    analysis_conn = pymysql.connect(**st.analysis_mysql_kwargs())
    analysis_cur = analysis_conn.cursor()

def auth_q(sql, args=None):
    auth_cur.execute(sql, args)
    return [r for r in auth_cur.fetchall()]

def auth_col(sql, args=None):
    return [r[0] for r in auth_q(sql, args)]

def analysis_q(sql, args=None):
    analysis_cur.execute(sql, args)
    return [r for r in analysis_cur.fetchall()]

SENTINEL = -1
SPECIAL_ROLES = ("visitor", "customer_contact", "shop_contact")
SCOPE_KEYS = ("employee_ids", "employee_codes", "customer_codes",
              "login_names", "manager_customer_codes", "shop_codes")


def empty_scope():
    return dict(employee_ids=[], employee_codes=[], customer_codes=[],
                login_names=[], manager_customer_codes=[], shop_codes=[])


def clean_strings(values):
    return list(dict.fromkeys(v for v in values if v is not None and v.strip()))


def deny_strings(values):
    values = clean_strings(values)
    return values or ["-1"]


def deny_ids(values):
    values = list(dict.fromkeys(v for v in values if v is not None))
    return values or [SENTINEL]


def area_cust(eids):
    if not eids:
        return []
    ph = ",".join(["%s"] * len(eids))
    return auth_col(
        f"SELECT customer_code FROM t_customer WHERE deleted_flag=0 AND area_manager_id IN ({ph})",
        eids,
    )


def active_shops_by_customers(customer_codes):
    if not customer_codes:
        return []
    if not any(c != "-1" and c.strip() for c in customer_codes):
        return ["-1"]
    ph = ",".join(["%s"] * len(customer_codes))
    return deny_strings(auth_col(
        f"SELECT DISTINCT shop_code FROM t_master_shop WHERE customer_code IN ({ph}) "
        "AND status=0 AND deleted_flag=0",
        customer_codes,
    ))


def active_shops_by_codes(shop_codes):
    if not shop_codes:
        return ["-1"]
    ph = ",".join(["%s"] * len(shop_codes))
    return deny_strings(auth_col(
        f"SELECT DISTINCT shop_code FROM t_master_shop WHERE shop_code IN ({ph}) "
        "AND status=0 AND deleted_flag=0",
        shop_codes,
    ))

def py_scope(login, role_code):
    """Java 默认策略与三类特殊策略的独立 Python 复刻（与 Rust 双实现互证）"""
    emp = auth_q("""SELECT employee_id, actual_name, administrator_flag, department_id
        FROM t_employee WHERE login_name=%s AND deleted_flag=0 AND disabled_flag=0
        AND (passwd_expire_time IS NULL OR passwd_expire_time >= CURRENT_TIMESTAMP)""", (login,))
    assert emp, f"员工不存在 {login}"
    eid, name, admin_flag, dept_id = emp[0]
    role = auth_q("SELECT r.role_id FROM t_role_employee re JOIN t_role r ON r.role_id=re.role_id WHERE re.employee_id=%s AND TRIM(r.role_code)=%s", (eid, role_code))
    if admin_flag == 1 or role_code == "admin":
        return empty_scope()
    assert role, f"{login} 无角色 {role_code}"
    role_id = role[0][0]

    # DataScopeManager 对三类角色先分流，不读取 t_role_data_scope。
    if role_code == "visitor":
        config = auth_q("SELECT config_value FROM t_config WHERE config_key='guest_distributor'")
        customer_codes = deny_strings(
            [config[0][0]] if len(config) == 1 and config[0][0] is not None else []
        )
        return dict(
            employee_ids=[SENTINEL],
            employee_codes=["-1"],
            customer_codes=customer_codes,
            login_names=deny_strings([login]),
            manager_customer_codes=[],
            shop_codes=["-1"],
        )

    if role_code == "customer_contact":
        accounts = auth_q("""SELECT contact_id, customer_code
            FROM t_customer_contacts_account
            WHERE customer_code IN (
              SELECT DISTINCT t.customer_code FROM t_customer_contacts_account t
              WHERE t.contact_id=%s
            )""", (eid,))
        employee_ids = deny_ids([contact_id for contact_id, _ in accounts])
        customer_codes = deny_strings([customer_code for _, customer_code in accounts])
        if any(c != "-1" and c.strip() for c in customer_codes):
            ph = ",".join(["%s"] * len(customer_codes))
            employee_codes = deny_strings(auth_col(
                f"SELECT DISTINCT login_name FROM t_customer_contacts_account "
                f"WHERE customer_code IN ({ph}) AND deleted_flag=0",
                customer_codes,
            ))
        else:
            employee_codes = ["-1"]
        return dict(
            employee_ids=employee_ids,
            employee_codes=employee_codes,
            customer_codes=customer_codes,
            login_names=deny_strings([login]),
            manager_customer_codes=clean_strings(area_cust(employee_ids)),
            shop_codes=active_shops_by_customers(customer_codes),
        )

    if role_code == "shop_contact":
        accounts = auth_q("""SELECT customer_code, shop_code
            FROM t_customer_contacts_account
            WHERE contact_id=%s AND deleted_flag=0""", (eid,))
        customer_codes = deny_strings([customer_code for customer_code, _ in accounts])
        bound_shop_codes = clean_strings([shop_code for _, shop_code in accounts])
        login_names = deny_strings([login])
        return dict(
            employee_ids=[eid],
            employee_codes=login_names.copy(),
            customer_codes=customer_codes,
            login_names=login_names,
            manager_customer_codes=clean_strings(area_cust([eid])),
            shop_codes=active_shops_by_codes(bound_shop_codes),
        )

    rows = auth_q("SELECT data_scope_type, view_type FROM t_role_data_scope WHERE role_id=%s", (role_id,))
    assert rows, "角色未设定数据范围"
    base_rows = [v for t, v in rows if t == 1]
    custom = {v for t, v in rows if t == 2}

    def user_depts():
        d = auth_col("SELECT department_id FROM t_employee_department WHERE employee_id=%s AND deleted_flag=0", (eid,))
        return d or ([dept_id] if dept_id is not None else [])

    def dept_children(roots):
        allrows = auth_q("SELECT department_id, parent_id FROM t_department WHERE status=1 AND deleted_flag=0")
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
        return auth_col(f"""SELECT DISTINCT t.employee_id FROM t_employee t
            INNER JOIN t_employee_department td ON td.employee_id=t.employee_id AND td.deleted_flag=0 AND td.service_status=0
            WHERE t.department_id IN ({ph}) OR td.department_id IN ({ph})""", depts * 2)

    def subordinates():
        result, frontier = {eid}, [eid]
        while frontier:
            ph = ",".join(["%s"] * len(frontier))
            found = auth_col(f"SELECT DISTINCT employee_id FROM t_employee_department WHERE deleted_flag=0 AND service_status=0 AND manager_id IN ({ph})", frontier)
            frontier = [f for f in found if f not in result]
            result |= set(frontier)
        return list(result)

    base_v = max(base_rows) if base_rows else None
    if base_v is None or base_v == 10:
        return empty_scope()
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
        return auth_col(f"SELECT login_name FROM t_employee WHERE employee_id IN ({ph})", idlist)

    codes, cflag = [], True
    if SENTINEL in base_ids: cflag = False
    else: codes += logins(base_ids)
    if sub_ids:
        if SENTINEL in sub_ids: cflag = False
        else: codes += logins(sub_ids)
    if not codes and not cflag: codes = ["-1"]
    codes = list(dict.fromkeys(codes))

    # customer_codes
    cust, kflag = area_cust(base_ids), True
    cust += auth_col("""SELECT DISTINCT v.value_code FROM t_dict_value v JOIN t_dict_key k ON k.dict_key_id=v.dict_key_id
        WHERE k.key_code IN ('payment_customer_for_inside','payment_customer_for_all','payment_customer_for_yiming')
          AND k.deleted_flag=0 AND v.deleted_flag=0""")
    def group_cust(eids):
        if not eids: return []
        ph = ",".join(["%s"] * len(eids))
        return auth_col(f"""SELECT DISTINCT tc.customer_code FROM t_customer tc WHERE EXISTS (
            SELECT 1 FROM t_employee_customer_group t WHERE t.employee_id IN ({ph})
            AND FIND_IN_SET(t.customer_group, tc.customer_group) > 0)""", eids)
    def mgr_cust(names):
        if not names: return []
        ph = ",".join(["%s"] * len(names))
        return [c for c in auth_col(f"""SELECT DISTINCT customer_code FROM t_customer_contacts_info
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
        names = auth_col(f"SELECT actual_name FROM t_employee WHERE employee_id IN ({ph})", sub_ids)
        sc = mgr_cust(names) + group_cust(sub_ids)
        if not sc: kflag = False
        else: cust += sc
    cust = [c for c in dict.fromkeys(cust) if c and c.strip()]
    if not cust and not kflag: cust = ["-1"]
    device_full = role_code in ("xiaoyunbp", "shebeiyy")
    return dict(
        employee_ids=ids,
        employee_codes=codes,
        customer_codes=cust,
        login_names=[] if device_full else [login],
        manager_customer_codes=[] if device_full else area_cust(ids),
        shop_codes=active_shops_by_customers(cust),
    )

def rust_scope(login, role_code):
    out = subprocess.run(cli("scope", login, role_code), capture_output=True, text=True,
                         encoding="utf-8", cwd=str(ROOT))
    assert out.returncode == 0, f"rust scope 失败: {out.stderr[-800:]}"
    return json.loads(out.stdout)

def count_with(cond):
    where = "so.deleted_flag = 0" + (f" AND {cond}" if cond else "")
    return analysis_q(f"SELECT COUNT(*) FROM t_sales_order so WHERE {where}")[0][0]

def py_condition(s):
    segs = []
    if s["employee_ids"]:
        segs.append(f"so.owner_manager in ({','.join(map(str, s['employee_ids']))})")
    if s["customer_codes"]:
        codes = ",".join("'" + c.replace("'", "''") + "'" for c in s["customer_codes"])
        segs.append(f"so.customer_code in ({codes})")
    return f"({' or '.join(segs)})" if segs else ""

# ── 选判官用户：当前身份库每种真实权限形态各取一个在职员工 ──
def pick_shapes():
    rows = auth_q("""SELECT TRIM(r.role_code), MIN(e.login_name),
                       GROUP_CONCAT(DISTINCT CONCAT(s.data_scope_type, ':', s.view_type)
                                    ORDER BY s.data_scope_type, s.view_type)
                FROM t_role r
                JOIN t_role_employee re ON re.role_id=r.role_id
                JOIN t_employee e ON e.employee_id=re.employee_id
                    AND e.deleted_flag=0 AND e.disabled_flag=0 AND e.administrator_flag=0
                JOIN t_role_data_scope s ON s.role_id=r.role_id
                WHERE TRIM(r.role_code) NOT IN ('visitor','customer_contact','shop_contact')
                GROUP BY r.role_id, r.role_code
                ORDER BY 3, 1""")
    return [(login, role, shape) for role, login, shape in rows if login and shape]


def pick_special_roles():
    rows = auth_q("""SELECT TRIM(r.role_code), MIN(e.login_name)
                FROM t_role r
                JOIN t_role_employee re ON re.role_id=r.role_id
                JOIN t_employee e ON e.employee_id=re.employee_id
                WHERE TRIM(r.role_code) IN ('visitor','customer_contact','shop_contact')
                  AND e.deleted_flag=0 AND e.disabled_flag=0 AND e.administrator_flag=0
                  AND (e.passwd_expire_time IS NULL OR e.passwd_expire_time >= CURRENT_TIMESTAMP)
                GROUP BY TRIM(r.role_code)""")
    return [(login, role) for role, login in rows if login]


def build_cases(shape_rows, special_rows):
    """挑用例 + 反空转闸。

    🔴 原实现 `if ln: cases.append(...)` —— `pick` 挑不到人就**静默跳过**（无 print、
    不计 fails），而 `cases.append(("admin","admin"))` 保证 cases 恒非空。
    admin 那条两侧都在 `py_scope`/rust 里短路成空集、`py_condition` 因此返空串，
    行级 COUNT 退化成「同一条谓词和自己比」——**结构上不可能红**。
    于是「5 个受限角色一档都没挑到」跑出来是 `✅ admin(admin)` + `全部通过 ✅` + exit 0：
    一个真正判过东西的用例都没有，报告却是绿的。

    固定角色码会随 DMS 环境变化：旧库有 STYY01，本轮身份库没有；硬绑角色会让两套实现
    都正确时仍然空转失败。这里按 `(data_scope_type:view_type)` 完整形态去重，当前库实际
    存在哪些权限组合就全部覆盖哪些组合，同时保留反空转闸。

    exit 2 与 exit 1 分开（同 evaluation.py 的口径）：「门没开」不等于「对拍失败」。"""
    picked = {}
    for login, role, shape in shape_rows:
        picked.setdefault(shape, (login, role))
    restricted = [(login, role) for shape, (login, role) in sorted(picked.items())
                  if not shape.startswith("1:10")]
    custom = {part for shape in picked for part in shape.split(",") if part.startswith("2:")}
    if len(picked) < 3 or len(restricted) < 2 or not custom:
        print(f"❌ 只找到 {len(picked)} 种权限形态 / {len(restricted)} 种受限形态 / "
              f"{len(custom)} 种定制档 —— 对拍没开门，本次结果不构成任何结论")
        sys.exit(2)
    special = {role: login for login, role in special_rows}
    missing_special = [role for role in SPECIAL_ROLES if role not in special]
    if missing_special:
        print(f"❌ 缺少特殊角色判官账号: {', '.join(missing_special)} —— "
              "visitor/customer_contact/shop_contact 未完整覆盖，本次结果不构成任何结论")
        sys.exit(2)
    cases = list(picked.values())
    cases.extend((special[role], role) for role in SPECIAL_ROLES)
    cases.append(("admin", "admin"))  # 超管短路。⚠️ 这条恒绿，不计入上面的覆盖门槛
    print("权限形态覆盖: " + " | ".join(sorted(picked)))
    print("特殊角色覆盖: " + " | ".join(SPECIAL_ROLES))
    return cases


def selfcheck():
    """不连库自检：证明「挑不到人」会跳闸，而不是静默把 admin 那条恒绿用例当成绿报告。"""
    shapes = [("u0", "r0", "1:0,2:101"), ("u2", "r2", "1:2,2:103"),
              ("u10", "r10", "1:10,2:102"), ("duplicate", "r9", "1:0,2:101")]
    specials = [("uv", "visitor"), ("uc", "customer_contact"), ("us", "shop_contact")]
    full = build_cases(shapes, specials)
    assert full == [("u0", "r0"), ("u2", "r2"), ("u10", "r10"),
                    ("uv", "visitor"), ("uc", "customer_contact"),
                    ("us", "shop_contact"), ("admin", "admin")], full
    for bad in ([], shapes[:1], [("u", "r", "1:10,2:101")] * 3):
        try:
            build_cases(bad, specials)
        except SystemExit as e:
            assert e.code == 2, f"覆盖不足应 exit 2，实际 {e.code}"
        else:
            raise AssertionError(f"覆盖不足却没跳闸: {bad}")
    try:
        build_cases(shapes, specials[:2])
    except SystemExit as e:
        assert e.code == 2, f"特殊角色覆盖不足应 exit 2，实际 {e.code}"
    else:
        raise AssertionError("特殊角色覆盖不足却没跳闸")
    # 恒绿的 admin 用例：两侧集合都是空、条件串也是空 —— 断言这一点，是为了钉住
    # 「它不能单独构成一次对拍」这个前提（闸是靠这个前提才有意义的）
    assert py_condition(empty_scope()) == ""
    assert tuple(empty_scope()) == SCOPE_KEYS
    # 生产路径必须真的走 build_cases —— 否则这份自检判的是一个没人调用的函数
    #（本轮抓到的形态：判据打在抽出来的纯函数上而 bug 站点零覆盖）。
    # 🔴 数次数而不是 `in`：`in` 会命中**这一行 assert 自己**写下的那个字面量
    #（自造字面量再断言它 = 恒真）。反向验证当场抓到 —— 把生产那行换成内联挑用例，
    # `in` 版照样 `exit=0` + 「自检通过」。2 = assert 这一处 + 生产那一处。
    assert Path(__file__).read_text(encoding="utf-8").count(
        "build_cases(pick_shapes(), pick_special_roles())"
    ) == 2, \
        "生产路径没走 build_cases，本自检不构成任何结论"
    print("judge_scope.py 自检通过")
    return 0


if SELFCHECK:
    sys.exit(selfcheck())

cases = build_cases(pick_shapes(), pick_special_roles())

print(f"判官对拍 {len(cases)} 用户\n" + "=" * 60)
fails = 0
for login, rc in cases:
    try:
        py = py_scope(login, rc)
        rs_full = rust_scope(login, rc)
        rs = rs_full["sets"]
        ok = True
        for k in SCOPE_KEYS:
            a, b = set(map(str, py[k])), set(map(str, rs[k]))
            if a != b:
                ok = False
                print(f"❌ {login}({rc}) {k} 不一致: py独有{list(a-b)[:5]} rust独有{list(b-a)[:5]} (py={len(a)} rust={len(b)})")
        # 行级对拍：单语句双子查询（同一快照，免受现网实时写入干扰）
        py_where = "so.deleted_flag = 0" + (f" AND {py_condition(py)}" if py_condition(py) else "")
        demo = rs_full["demo_sql"]
        c_py, c_rs = analysis_q(f"SELECT (SELECT COUNT(*) FROM t_sales_order so WHERE {py_where}), ({demo})")[0]
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
analysis_conn.close()
auth_conn.close()
sys.exit(1 if fails else 0)
