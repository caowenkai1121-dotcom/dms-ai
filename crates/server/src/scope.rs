//! 数据权限集合计算：1:1 复刻 DMS Java DefaultEmployee.java 语义。
//! 权威源码：infrastructure/.../common/service/impl/DefaultEmployee.java（本文件注释引用其行号）。
//!
//! 关键语义（与 Java 逐条对齐）：
//! - 超管(administrator_flag)/admin 角色 → 全部集合为空 = 不限制（短路）。
//! - 基础档(type=1)取 view_type 最大的一行：0本人/1本部门/2本部门及下级/3结算客户(哨兵-1)/10全部(空=不限制且整体短路)。
//! - 无 type=1 行 → defaultEmployeeIds 为空 → 整体短路不限制（Java L281-292 else 分支）。
//! - 定制档(type=2)：101下属(递归含本人) / 102客户分组(FIND_IN_SET) / 103客户团队(contact_name=姓名, contact_type IN Y1,Y3)。
//! - 哨兵：集合=[-1] 表示拒绝(0行)；集合为空 = 该维度不注入 = 放行。二者语义相反。
//! - customer_codes = 基础客户(area_manager_id IN 基础ids) + 公用客户(字典) + 102 + 103 + 下属客户，各段 -1 则跳过并标旗，
//!   最终为空且有旗 → ["-1"]。

use serde::Serialize;
use sqlx::MySqlPool;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::principal::Principal;

pub const SENTINEL: i64 = -1;

#[derive(Debug, Default, Serialize, Clone)]
pub struct ScopeSets {
    /// 空 = 该维度不注入（放行）；[-1] = 哨兵拒绝
    pub employee_ids: Vec<i64>,
    pub employee_codes: Vec<String>,
    pub customer_codes: Vec<String>,
}

impl ScopeSets {
    /// 三维度全空 = 完全不限制
    pub fn is_unrestricted(&self) -> bool {
        self.employee_ids.is_empty()
            && self.employee_codes.is_empty()
            && self.customer_codes.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BaseView {
    Me,
    Department,
    DepartmentAndSub,
    SettleCustomer,
    All,
}

impl BaseView {
    fn from_value(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Me),
            1 => Some(Self::Department),
            2 => Some(Self::DepartmentAndSub),
            3 => Some(Self::SettleCustomer),
            10 => Some(Self::All),
            _ => None,
        }
    }
}

/// 进程内 scope 缓存：key=(登录名,角色)，当日过期（对齐 Java Redis 当日缓存策略）。
/// 权限集合计算含多次连库查询（部门/下属/客户集合），限权用户单次 ~10s；命中缓存后亚秒。
type CacheMap = HashMap<(String, String), (ScopeSets, u64)>;
static SCOPE_CACHE: OnceLock<Mutex<CacheMap>> = OnceLock::new();

fn epoch_day() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86400)
        .unwrap_or(0)
}

pub async fn compute_scope_cached(mysql: &MySqlPool, p: &Principal) -> anyhow::Result<ScopeSets> {
    let key = (p.login_name.clone(), p.role_code.clone());
    let today = epoch_day();
    let cache = SCOPE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some((sets, day)) = map.get(&key) {
            if *day == today {
                return Ok(sets.clone());
            }
        }
    }
    let sets = compute_scope(mysql, p).await?;
    if let Ok(mut map) = cache.lock() {
        map.insert(key, (sets.clone(), today));
    }
    Ok(sets)
}

pub async fn compute_scope(mysql: &MySqlPool, p: &Principal) -> anyhow::Result<ScopeSets> {
    // 超管/admin 短路（Java L93-98, L236-243）
    if p.administrator_flag || p.role_code == "admin" {
        return Ok(ScopeSets::default());
    }

    let rows: Vec<(i32, i32)> = sqlx::query_as(
        "SELECT data_scope_type, view_type FROM t_role_data_scope WHERE role_id = ?",
    )
    .bind(p.role_id)
    .fetch_all(mysql)
    .await?;
    if rows.is_empty() {
        // Java L275: 抛「当前登录用户角色未正确设定[角色-数据范围]」→ fail-closed
        anyhow::bail!("当前登录用户角色未正确设定[角色-数据范围]");
    }
    let base_rows: Vec<i32> = rows.iter().filter(|(t, _)| *t == 1).map(|(_, v)| *v).collect();
    let custom_rows: Vec<i32> = rows.iter().filter(|(t, _)| *t == 2).map(|(_, v)| *v).collect();

    // ── 基础档：取最大 view_type（Java L428-429 排序取最后）──
    // 无 type=1 行 或 ALL → defaultEmployeeIds 为空 → 整体短路不限制（Java L281-292 / L394-395 / L580-581）
    let base_ids: Vec<i64> = match base_rows.iter().max() {
        None => return Ok(ScopeSets::default()),
        Some(&max_v) => {
            let view = BaseView::from_value(max_v)
                .ok_or_else(|| anyhow::anyhow!("角色数据权限配置错误 view_type={max_v}"))?;
            match view {
                BaseView::All => return Ok(ScopeSets::default()),
                BaseView::Me => vec![p.employee_id],
                BaseView::SettleCustomer => vec![SENTINEL],
                BaseView::Department => {
                    let depts = user_departments(mysql, p).await?;
                    department_employee_ids(mysql, &depts).await?
                }
                BaseView::DepartmentAndSub => {
                    let depts = user_departments(mysql, p).await?;
                    let nested = self_and_children_departments(mysql, &depts).await?;
                    department_employee_ids(mysql, &nested).await?
                }
            }
        }
    };

    // ── 定制档 101 下属（递归含本人，Java L458-489）──
    let has_sub = custom_rows.contains(&101);
    let sub_ids: Vec<i64> = if has_sub {
        let ids = subordinate_ids(mysql, p.employee_id).await?;
        // Java: 空→[-1]；但起点恒含本人，实际不会为空
        if ids.is_empty() { vec![SENTINEL] } else { ids }
    } else {
        vec![]
    };

    // ── employee_ids 合并（Java getDefaultUserListWithRoleDataScope L382-410）──
    let mut employee_ids: Vec<i64> = vec![];
    let mut perm_flag = true;
    if base_ids.contains(&SENTINEL) {
        perm_flag = false;
    } else {
        employee_ids.extend(&base_ids);
    }
    if !sub_ids.is_empty() {
        if sub_ids.contains(&SENTINEL) {
            perm_flag = false;
        } else {
            employee_ids.extend(&sub_ids);
        }
    }
    if employee_ids.is_empty() && !perm_flag {
        employee_ids.push(SENTINEL);
    }
    employee_ids = dedup_i64(employee_ids);

    // ── employee_codes：基础+下属的 login_name（Java L528-565，-1 语义取净化版）──
    let mut employee_codes: Vec<String> = vec![];
    let mut codes_flag = true;
    if base_ids.contains(&SENTINEL) {
        codes_flag = false;
    } else if !base_ids.is_empty() {
        employee_codes.extend(login_names_by_ids(mysql, &base_ids).await?);
    }
    if !sub_ids.is_empty() {
        if sub_ids.contains(&SENTINEL) {
            codes_flag = false;
        } else {
            employee_codes.extend(login_names_by_ids(mysql, &sub_ids).await?);
        }
    }
    if employee_codes.is_empty() && !codes_flag {
        employee_codes.push("-1".into());
    }
    employee_codes = dedup_str(employee_codes);

    // ── customer_codes（Java getCustomerCodesByCurrentUser L568-621）──
    let mut customer_codes: Vec<String> = vec![];
    let mut cust_flag = true;
    // 1. 基础客户：area_manager_id IN 基础ids（含哨兵时 IN(-1) 自然为空，与 Java 一致）
    customer_codes.extend(customers_by_area_manager(mysql, &base_ids).await?);
    // 2. 公用客户（字典 payment_customer_for_inside + payment_customer_for_all）
    customer_codes.extend(common_customer_codes(mysql).await?);
    // 3. 定制 102 客户分组
    if custom_rows.contains(&102) {
        let g = group_customer_codes(mysql, &[p.employee_id]).await?;
        if g.is_empty() { cust_flag = false; } else { customer_codes.extend(g); }
    }
    // 4. 定制 103 客户团队（contact_name = 姓名）
    if custom_rows.contains(&103) {
        let m = manager_customer_codes(mysql, &[p.actual_name.clone()]).await?;
        if m.is_empty() { cust_flag = false; } else { customer_codes.extend(m); }
    }
    // 5. 下属为团队成员/分组的客户（Java addSubordinateToCustomerManager L337-359）
    if has_sub && !sub_ids.contains(&SENTINEL) && !sub_ids.is_empty() {
        let names = actual_names_by_ids(mysql, &sub_ids).await?;
        let mut sc = manager_customer_codes(mysql, &names).await?;
        sc.extend(group_customer_codes(mysql, &sub_ids).await?);
        if sc.is_empty() { cust_flag = false; } else { customer_codes.extend(sc); }
    }
    customer_codes.retain(|c| !c.trim().is_empty());
    customer_codes = dedup_str(customer_codes);
    if customer_codes.is_empty() && !cust_flag {
        customer_codes.push("-1".into());
    }

    Ok(ScopeSets { employee_ids, employee_codes, customer_codes })
}

/// 用户所属部门：任职表活跃行；无 → 主档 department_id（Java getDepartmentByUser L517-526）
async fn user_departments(mysql: &MySqlPool, p: &Principal) -> anyhow::Result<Vec<i64>> {
    let ids: Vec<(i64,)> = sqlx::query_as(
        "SELECT department_id FROM t_employee_department WHERE employee_id = ? AND deleted_flag = 0",
    )
    .bind(p.employee_id)
    .fetch_all(mysql)
    .await?;
    let mut v: Vec<i64> = ids.into_iter().map(|(d,)| d).collect();
    if v.is_empty() {
        if let Some(d) = p.department_id {
            v.push(d);
        }
    }
    Ok(v)
}

/// 部门自身+全部子部门（Java DepartmentCacheManager：status=1 且 deleted_flag=0，按 parent_id 递归）
async fn self_and_children_departments(mysql: &MySqlPool, roots: &[i64]) -> anyhow::Result<Vec<i64>> {
    let all: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT department_id, parent_id FROM t_department WHERE status = 1 AND deleted_flag = 0",
    )
    .fetch_all(mysql)
    .await?;
    let mut result: Vec<i64> = vec![];
    let mut seen: HashSet<i64> = HashSet::new();
    for &root in roots {
        if !seen.insert(root) {
            continue;
        }
        result.push(root);
        let mut frontier = vec![root];
        while !frontier.is_empty() {
            let next: Vec<i64> = all
                .iter()
                .filter(|(_, pid)| pid.map(|x| frontier.contains(&x)).unwrap_or(false))
                .map(|(id, _)| *id)
                .filter(|id| seen.insert(*id))
                .collect();
            result.extend(&next);
            frontier = next;
        }
    }
    Ok(result)
}

/// 部门员工：主部门 OR 任职部门（任职行须 deleted=0 且 service_status=0，Java EmployeeMapper.xml L179-191）
async fn department_employee_ids(mysql: &MySqlPool, dept_ids: &[i64]) -> anyhow::Result<Vec<i64>> {
    if dept_ids.is_empty() {
        return Ok(vec![]);
    }
    let ph = placeholders(dept_ids.len());
    let sql = format!(
        "SELECT DISTINCT t.employee_id FROM t_employee t
         INNER JOIN t_employee_department td
            ON td.employee_id = t.employee_id AND td.deleted_flag = 0 AND td.service_status = 0
         WHERE t.department_id IN ({ph}) OR td.department_id IN ({ph})"
    );
    let mut q = sqlx::query_as::<_, (i64,)>(&sql);
    for _ in 0..2 {
        for d in dept_ids {
            q = q.bind(d);
        }
    }
    Ok(q.fetch_all(mysql).await?.into_iter().map(|(i,)| i).collect())
}

/// 下属递归（含本人；任职行 deleted=0 且 service_status=0 按 manager_id 下钻，Java L458-489 含环保护）
async fn subordinate_ids(mysql: &MySqlPool, user_id: i64) -> anyhow::Result<Vec<i64>> {
    let mut result: HashSet<i64> = HashSet::from([user_id]);
    let mut frontier: Vec<i64> = vec![user_id];
    while !frontier.is_empty() {
        let ph = placeholders(frontier.len());
        let sql = format!(
            "SELECT DISTINCT employee_id FROM t_employee_department
             WHERE deleted_flag = 0 AND service_status = 0 AND manager_id IN ({ph})"
        );
        let mut q = sqlx::query_as::<_, (i64,)>(&sql);
        for id in &frontier {
            q = q.bind(id);
        }
        let found: Vec<i64> = q.fetch_all(mysql).await?.into_iter().map(|(i,)| i).collect();
        frontier = found.into_iter().filter(|id| result.insert(*id)).collect();
    }
    Ok(result.into_iter().collect())
}

async fn login_names_by_ids(mysql: &MySqlPool, ids: &[i64]) -> anyhow::Result<Vec<String>> {
    fetch_str_in(mysql, "SELECT login_name FROM t_employee WHERE employee_id IN", ids).await
}

async fn actual_names_by_ids(mysql: &MySqlPool, ids: &[i64]) -> anyhow::Result<Vec<String>> {
    fetch_str_in(mysql, "SELECT actual_name FROM t_employee WHERE employee_id IN", ids).await
}

async fn customers_by_area_manager(mysql: &MySqlPool, ids: &[i64]) -> anyhow::Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    fetch_str_in(
        mysql,
        "SELECT customer_code FROM t_customer WHERE deleted_flag = 0 AND area_manager_id IN",
        ids,
    )
    .await
}

/// 公用客户：字典两 key 的 value_code（Java getGeneralCustomerCodes L186-197）
async fn common_customer_codes(mysql: &MySqlPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT v.value_code FROM t_dict_value v
         JOIN t_dict_key k ON k.dict_key_id = v.dict_key_id
         WHERE k.key_code IN ('payment_customer_for_inside','payment_customer_for_all')
           AND k.deleted_flag = 0 AND v.deleted_flag = 0",
    )
    .fetch_all(mysql)
    .await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// 102 客户分组：FIND_IN_SET(员工组码, 客户.customer_group)（Java EmployeeCustomerGroupMapper.xml L80-93）
async fn group_customer_codes(mysql: &MySqlPool, employee_ids: &[i64]) -> anyhow::Result<Vec<String>> {
    if employee_ids.is_empty() {
        return Ok(vec![]);
    }
    let ph = placeholders(employee_ids.len());
    let sql = format!(
        "SELECT DISTINCT tc.customer_code FROM t_customer tc
         WHERE EXISTS (SELECT 1 FROM t_employee_customer_group t
                       WHERE t.employee_id IN ({ph})
                         AND FIND_IN_SET(t.customer_group, tc.customer_group) > 0)"
    );
    let mut q = sqlx::query_as::<_, (String,)>(&sql);
    for id in employee_ids {
        q = q.bind(id);
    }
    Ok(q.fetch_all(mysql).await?.into_iter().map(|(s,)| s).collect())
}

/// 103 客户团队：contact_name IN 姓名 且 contact_type IN ('Y1'负责人,'Y3'团队成员)（Java L137-151）
async fn manager_customer_codes(mysql: &MySqlPool, names: &[String]) -> anyhow::Result<Vec<String>> {
    if names.is_empty() {
        return Ok(vec![]);
    }
    let ph = placeholders(names.len());
    let sql = format!(
        "SELECT DISTINCT customer_code FROM t_customer_contacts_info
         WHERE deleted_flag = 0 AND contact_type IN ('Y1','Y3') AND contact_name IN ({ph})"
    );
    let mut q = sqlx::query_as::<_, (String,)>(&sql);
    for n in names {
        q = q.bind(n);
    }
    Ok(q
        .fetch_all(mysql)
        .await?
        .into_iter()
        .map(|(s,)| s)
        .filter(|s| !s.trim().is_empty())
        .collect())
}

async fn fetch_str_in(mysql: &MySqlPool, prefix: &str, ids: &[i64]) -> anyhow::Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let sql = format!("{prefix} ({})", placeholders(ids.len()));
    let mut q = sqlx::query_as::<_, (String,)>(&sql);
    for id in ids {
        q = q.bind(id);
    }
    Ok(q.fetch_all(mysql).await?.into_iter().map(|(s,)| s).collect())
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn dedup_i64(v: Vec<i64>) -> Vec<i64> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|x| seen.insert(*x)).collect()
}

fn dedup_str(v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|x| seen.insert(x.clone())).collect()
}
