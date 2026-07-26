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

/// 基础档裁决（纯函数，可离线单测）：max(view_type) → 该走哪条路。
/// 无 type=1 行 / ALL → Unrestricted（Java L281-292 else 分支）；未知值 → Err（fail-closed）。
#[derive(Debug, PartialEq)]
enum BaseDecision {
    /// 整体短路不限制
    Unrestricted,
    /// 直接给定 id 集合（本人 / 结算客户哨兵）
    Ids(Vec<i64>),
    /// 需查部门树（bool = 是否含子部门）
    Departments { with_children: bool },
}

fn decide_base(base_rows: &[i32]) -> anyhow::Result<BaseDecision> {
    let Some(&max_v) = base_rows.iter().max() else {
        return Ok(BaseDecision::Unrestricted);
    };
    let view = BaseView::from_value(max_v)
        .ok_or_else(|| anyhow::anyhow!("角色数据权限配置错误 view_type={max_v}"))?;
    Ok(match view {
        BaseView::All => BaseDecision::Unrestricted,
        BaseView::Me => BaseDecision::Ids(vec![]), // 占位：调用方填本人 id
        BaseView::SettleCustomer => BaseDecision::Ids(vec![SENTINEL]),
        BaseView::Department => BaseDecision::Departments { with_children: false },
        BaseView::DepartmentAndSub => BaseDecision::Departments { with_children: true },
    })
}

/// 员工 id 合并（纯函数）：哨兵段跳过并落旗，全空且有旗 → [-1]（Java L382-410）
fn merge_employee_ids(base_ids: &[i64], sub_ids: &[i64]) -> Vec<i64> {
    let mut out: Vec<i64> = vec![];
    let mut perm_flag = true;
    if base_ids.contains(&SENTINEL) {
        perm_flag = false;
    } else {
        out.extend(base_ids);
    }
    if !sub_ids.is_empty() {
        if sub_ids.contains(&SENTINEL) {
            perm_flag = false;
        } else {
            out.extend(sub_ids);
        }
    }
    if out.is_empty() && !perm_flag {
        out.push(SENTINEL);
    }
    dedup_i64(out)
}

/// 客户编码合并（纯函数）：段为空即落旗（该维度本应有值却查空 = 拒绝），
/// 全空且有旗 → ["-1"]；空串剔除（Java L568-621）。
/// segs = (该段编码集, 该段是否"必须有值"即落旗段)
fn merge_customer_codes(segs: &[(Vec<String>, bool)]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut flag = true;
    for (codes, is_flagging) in segs {
        if *is_flagging && codes.is_empty() {
            flag = false;
            continue;
        }
        out.extend(codes.iter().cloned());
    }
    out.retain(|c| !c.trim().is_empty());
    out = dedup_str(out);
    if out.is_empty() && !flag {
        out.push("-1".into());
    }
    out
}

/// 部门树展开（纯函数）：给定全部 (department_id, parent_id) 边，从 roots 递归取自身+子孙，含环保护
fn expand_department_tree(all: &[(i64, Option<i64>)], roots: &[i64]) -> Vec<i64> {
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
    result
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
    let base_ids: Vec<i64> = match decide_base(&base_rows)? {
        BaseDecision::Unrestricted => return Ok(ScopeSets::default()),
        // Me 走占位空集分支：由此处填本人 id（纯函数不碰 principal）
        BaseDecision::Ids(ids) if ids.is_empty() => vec![p.employee_id],
        BaseDecision::Ids(ids) => ids,
        BaseDecision::Departments { with_children } => {
            let depts = user_departments(mysql, p).await?;
            let scope_depts = if with_children {
                self_and_children_departments(mysql, &depts).await?
            } else {
                depts
            };
            department_employee_ids(mysql, &scope_depts).await?
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
    let employee_ids = merge_employee_ids(&base_ids, &sub_ids);

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
    // 段序同 Java；bool=该段"必须有值否则落旗"（落旗段查空 → 最终 ["-1"] 拒绝）
    let mut segs: Vec<(Vec<String>, bool)> = vec![
        // 1. 基础客户：area_manager_id IN 基础ids（含哨兵时 IN(-1) 自然为空，与 Java 一致）
        (customers_by_area_manager(mysql, &base_ids).await?, false),
        // 2. 公用客户（字典 payment_customer_for_inside + payment_customer_for_all）
        (common_customer_codes(mysql).await?, false),
    ];
    // 3. 定制 102 客户分组
    if custom_rows.contains(&102) {
        segs.push((group_customer_codes(mysql, &[p.employee_id]).await?, true));
    }
    // 4. 定制 103 客户团队（contact_name = 姓名）
    if custom_rows.contains(&103) {
        segs.push((manager_customer_codes(mysql, &[p.actual_name.clone()]).await?, true));
    }
    // 5. 下属为团队成员/分组的客户（Java addSubordinateToCustomerManager L337-359）
    if has_sub && !sub_ids.contains(&SENTINEL) && !sub_ids.is_empty() {
        let names = actual_names_by_ids(mysql, &sub_ids).await?;
        let mut sc = manager_customer_codes(mysql, &names).await?;
        sc.extend(group_customer_codes(mysql, &sub_ids).await?);
        segs.push((sc, true));
    }
    let customer_codes = merge_customer_codes(&segs);

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
    Ok(expand_department_tree(&all, roots))
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

/// 权限决策纯逻辑离线单测（对拍依据：Java DefaultEmployee + tools/judge_scope.py 连库判官）。
/// 连库判官仍是最终验收；此处锁住不依赖数据的语义，防重构静默改语义。
#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // ── 基础档裁决：view_type 全值域 ──
    #[test]
    fn base_empty_is_unrestricted() {
        // 无 type=1 行 → 整体短路不限制（Java L281-292）
        assert_eq!(decide_base(&[]).unwrap(), BaseDecision::Unrestricted);
    }

    #[test]
    fn base_all_10_is_unrestricted() {
        assert_eq!(decide_base(&[10]).unwrap(), BaseDecision::Unrestricted);
    }

    #[test]
    fn base_me_0_placeholder() {
        // Me 返回空占位，由调用方填本人 id
        assert_eq!(decide_base(&[0]).unwrap(), BaseDecision::Ids(vec![]));
    }

    #[test]
    fn base_settle_customer_3_is_sentinel() {
        assert_eq!(decide_base(&[3]).unwrap(), BaseDecision::Ids(vec![SENTINEL]));
    }

    #[test]
    fn base_department_1() {
        assert_eq!(decide_base(&[1]).unwrap(), BaseDecision::Departments { with_children: false });
    }

    #[test]
    fn base_department_and_sub_2() {
        assert_eq!(decide_base(&[2]).unwrap(), BaseDecision::Departments { with_children: true });
    }

    #[test]
    fn base_takes_max_not_first() {
        // 多行取 MAX（Java L428-429 排序取最后）：0+2 → 部门及下级
        assert_eq!(decide_base(&[0, 2]).unwrap(), BaseDecision::Departments { with_children: true });
        // 任一行为 10 → 不限制（10 是最大值）
        assert_eq!(decide_base(&[0, 1, 10]).unwrap(), BaseDecision::Unrestricted);
        // 3 与 2 并存取 3（结算客户哨兵）
        assert_eq!(decide_base(&[2, 3]).unwrap(), BaseDecision::Ids(vec![SENTINEL]));
    }

    #[test]
    fn base_unknown_view_type_fails_closed() {
        // 未知枚举值绝不当"看全部"（红队教训：catch-all 不能 = 放行）
        assert!(decide_base(&[7]).is_err());
        assert!(decide_base(&[99]).is_err());
        // 未知值是 MAX 时同样拒绝，不因存在合法小值而放行
        assert!(decide_base(&[0, 42]).is_err());
    }

    // ── employee_ids 合并：哨兵 vs 空集语义相反 ──
    #[test]
    fn merge_ids_base_only() {
        assert_eq!(merge_employee_ids(&[1, 2], &[]), vec![1, 2]);
    }

    #[test]
    fn merge_ids_union_with_sub() {
        assert_eq!(merge_employee_ids(&[1], &[2, 3]), vec![1, 2, 3]);
    }

    #[test]
    fn merge_ids_dedups() {
        assert_eq!(merge_employee_ids(&[1, 2], &[2, 3, 3]), vec![1, 2, 3]);
    }

    #[test]
    fn merge_ids_base_sentinel_dropped_but_sub_wins() {
        // 基础哨兵段跳过，下属段有值 → 用下属集合（拒绝旗被有值段覆盖）
        assert_eq!(merge_employee_ids(&[SENTINEL], &[5]), vec![5]);
    }

    #[test]
    fn merge_ids_all_sentinel_rejects() {
        // 全部段哨兵 → [-1] 拒绝（0 行），绝非空集放行
        assert_eq!(merge_employee_ids(&[SENTINEL], &[SENTINEL]), vec![SENTINEL]);
        assert_eq!(merge_employee_ids(&[SENTINEL], &[]), vec![SENTINEL]);
    }

    #[test]
    fn merge_ids_empty_base_no_sub_is_passthrough() {
        // 都空且无哨兵 → 空集 = 该维度不注入（放行），与 [-1] 语义相反
        assert!(merge_employee_ids(&[], &[]).is_empty());
    }

    #[test]
    fn merge_ids_sub_sentinel_with_base_values() {
        assert_eq!(merge_employee_ids(&[8], &[SENTINEL]), vec![8]);
    }

    // ── customer_codes 合并：落旗段查空 = 拒绝 ──
    #[test]
    fn merge_cust_basic_union() {
        let out = merge_customer_codes(&[(s(&["C1"]), false), (s(&["C2"]), false)]);
        assert_eq!(out, s(&["C1", "C2"]));
    }

    #[test]
    fn merge_cust_dedup_and_trim_empty() {
        let out = merge_customer_codes(&[(s(&["C1", "C1", "", "  "]), false)]);
        assert_eq!(out, s(&["C1"]));
    }

    #[test]
    fn merge_cust_flagging_empty_rejects() {
        // 102/103 类段本应有值却查空 → 整体 ["-1"] 拒绝（fail-closed）
        let out = merge_customer_codes(&[(vec![], true)]);
        assert_eq!(out, s(&["-1"]));
    }

    #[test]
    fn merge_cust_flagging_empty_but_other_seg_has_value() {
        // 有其他段有值 → 用有值段，不落 -1（Java 语义：并集非空即放行该并集）
        let out = merge_customer_codes(&[(s(&["C9"]), false), (vec![], true)]);
        assert_eq!(out, s(&["C9"]));
    }

    #[test]
    fn merge_cust_all_empty_no_flag_is_passthrough() {
        // 无落旗段且全空 → 空集 = 客户维度不注入（放行），非拒绝
        assert!(merge_customer_codes(&[(vec![], false)]).is_empty());
        assert!(merge_customer_codes(&[]).is_empty());
    }

    #[test]
    fn merge_cust_multi_flagging_segments() {
        // 多个落旗段：任一空即落旗；全空 → 拒绝
        let out = merge_customer_codes(&[(vec![], true), (vec![], true)]);
        assert_eq!(out, s(&["-1"]));
    }

    // ── 部门树递归（含环保护/多根/去重）──
    #[test]
    fn dept_tree_single_root_chain() {
        let all = vec![(2, Some(1)), (3, Some(2)), (4, Some(3))];
        let mut got = expand_department_tree(&all, &[1]);
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    #[test]
    fn dept_tree_leaf_root_only_self() {
        let all = vec![(2, Some(1)), (3, Some(2))];
        assert_eq!(expand_department_tree(&all, &[3]), vec![3]);
    }

    #[test]
    fn dept_tree_multi_root_dedup() {
        let all = vec![(2, Some(1)), (3, Some(1)), (5, Some(4))];
        let mut got = expand_department_tree(&all, &[1, 4, 1]);
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn dept_tree_cycle_terminates() {
        // 脏数据成环（1→2→1）必须终止且不重复
        let all = vec![(2, Some(1)), (1, Some(2))];
        let mut got = expand_department_tree(&all, &[1]);
        got.sort();
        assert_eq!(got, vec![1, 2]);
    }

    #[test]
    fn dept_tree_null_parent_not_matched() {
        // parent_id 为 NULL 的行不因任何 frontier 被拉入
        let all = vec![(9, None), (2, Some(1))];
        assert_eq!(expand_department_tree(&all, &[1]), vec![1, 2]);
    }

    #[test]
    fn dept_tree_unknown_root_returns_self() {
        assert_eq!(expand_department_tree(&[], &[42]), vec![42]);
    }

    // ── ScopeSets 语义 ──
    #[test]
    fn unrestricted_only_when_all_empty() {
        assert!(ScopeSets::default().is_unrestricted());
        let s1 = ScopeSets { employee_ids: vec![SENTINEL], ..Default::default() };
        assert!(!s1.is_unrestricted(), "哨兵不是不限制");
        let s2 = ScopeSets { customer_codes: s(&["-1"]), ..Default::default() };
        assert!(!s2.is_unrestricted());
        let s3 = ScopeSets { employee_codes: s(&["zhangsan"]), ..Default::default() };
        assert!(!s3.is_unrestricted());
    }

    // ── 端到端：决策 → 合并 → SQL 注入（跨模块语义锁）──
    #[test]
    fn sentinel_injects_reject_condition() {
        // 结算客户档(3) → 哨兵 → 注入 in(-1) → 真库 0 行
        let base = match decide_base(&[3]).unwrap() {
            BaseDecision::Ids(ids) => ids,
            other => panic!("{other:?}"),
        };
        let sets = ScopeSets {
            employee_ids: merge_employee_ids(&base, &[]),
            employee_codes: vec![],
            customer_codes: merge_customer_codes(&[(vec![], true)]),
        };
        assert!(!sets.is_unrestricted());
        let sql = crate::inject::inject("SELECT * FROM t_sales_order so", &sets).unwrap();
        let n = sql.to_lowercase().replace(' ', "");
        assert!(n.contains("so.owner_managerin(-1)"), "{sql}");
        assert!(n.contains("so.customer_codein('-1')"), "{sql}");
    }

    #[test]
    fn all_view_injects_nothing() {
        // view_type=10 → 不限制 → SQL 原样（超管路径同）
        assert_eq!(decide_base(&[10]).unwrap(), BaseDecision::Unrestricted);
        let sql = "SELECT * FROM t_sales_order so WHERE so.deleted_flag = 0";
        assert_eq!(crate::inject::inject(sql, &ScopeSets::default()).unwrap(), sql);
    }

    #[test]
    fn me_view_injects_own_id_only() {
        // 本人档(0) + 无定制 → 只注入自己
        let base = vec![199_i64]; // 调用方填本人 id
        let sets = ScopeSets {
            employee_ids: merge_employee_ids(&base, &[]),
            employee_codes: vec![],
            customer_codes: vec![],
        };
        let sql = crate::inject::inject("SELECT * FROM t_sales_order so", &sets).unwrap();
        let n = sql.to_lowercase().replace(' ', "");
        assert!(n.contains("so.owner_managerin(199)"), "{sql}");
        assert!(!n.contains("customer_code"), "客户集合为空不得注入该段: {sql}");
    }
}
