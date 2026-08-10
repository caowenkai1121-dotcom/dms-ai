//! 数据权限集合的**纯裁决**：1:1 复刻 DMS Java DefaultEmployee.java 语义。
//! 逐行搬自 server/src/scope.rs:20-153（连库的 7 个查询与缓存留 IO 侧）。
//! 权威源码：infrastructure/.../common/service/impl/DefaultEmployee.java（下方注释引用其行号）。
//!
//! 关键语义（与 Java 逐条对齐）：
//! - 基础档(type=1)取 view_type 最大的一行：0本人/1本部门/2本部门及下级/3结算客户(哨兵-1)/10全部(空=不限制且整体短路)。
//! - 无 type=1 行 → 基础集合为空 → 整体短路不限制（Java L281-292 else 分支）。
//! - 哨兵：集合=[-1] 表示拒绝(0行)；集合为空 = 该维度不注入 = 放行。二者语义相反。
//!
//! 「kernel 零 DMS 语料」的切法 = 数值 vs 字符串：这里只有 i32/i64/String 集合运算与 SENTINEL=-1，
//! 表名/列名/中文业务词一律留 IO 侧（见 docs/ARCHITECTURE.md §5「权限纯算法」行）。

use serde::Serialize;
use std::collections::HashSet;

use crate::errors::PolicyError;

pub const SENTINEL: i64 = -1;

#[derive(Debug, Default, Serialize, Clone, PartialEq)]
pub struct ScopeSets {
    /// 空 = 该维度不注入（放行）；[-1] = 哨兵拒绝
    pub employee_ids: Vec<i64>,
    pub employee_codes: Vec<String>,
    pub customer_codes: Vec<String>,
    /// 当前登录名（只含本人，不含下属）。用于 DMS 中按 `currentLoginName` 判定的页面。
    pub login_names: Vec<String>,
    /// `area_manager_id IN employee_ids` 对应的客户集合，不混入公用客户/客户组/团队客户。
    pub manager_customer_codes: Vec<String>,
    /// 当前身份可见的有效门店编码。与 customer_codes 分开，避免门店联系人扩大到同客户全部门店。
    pub shop_codes: Vec<String>,
}

impl ScopeSets {
    /// 所有权限维度全空 = 完全不限制
    pub fn is_unrestricted(&self) -> bool {
        self.employee_ids.is_empty()
            && self.employee_codes.is_empty()
            && self.customer_codes.is_empty()
            && self.login_names.is_empty()
            && self.manager_customer_codes.is_empty()
            && self.shop_codes.is_empty()
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
pub enum BaseDecision {
    /// 整体短路不限制
    Unrestricted,
    /// 直接给定 id 集合（本人 / 结算客户哨兵）
    Ids(Vec<i64>),
    /// 需查部门树（bool = 是否含子部门）
    Departments { with_children: bool },
}

pub fn decide_base(base_rows: &[i32]) -> Result<BaseDecision, PolicyError> {
    let Some(&max_v) = base_rows.iter().max() else {
        return Ok(BaseDecision::Unrestricted);
    };
    let view = BaseView::from_value(max_v).ok_or(PolicyError::BadViewType(max_v))?;
    Ok(match view {
        BaseView::All => BaseDecision::Unrestricted,
        BaseView::Me => BaseDecision::Ids(vec![]), // 占位：调用方填本人 id
        BaseView::SettleCustomer => BaseDecision::Ids(vec![SENTINEL]),
        BaseView::Department => BaseDecision::Departments { with_children: false },
        BaseView::DepartmentAndSub => BaseDecision::Departments { with_children: true },
    })
}

/// 员工 id 合并（纯函数）：哨兵段跳过并落旗，全空且有旗 → [-1]（Java L382-410）
pub fn merge_employee_ids(base_ids: &[i64], sub_ids: &[i64]) -> Vec<i64> {
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
pub fn merge_customer_codes(segs: &[(Vec<String>, bool)]) -> Vec<String> {
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
pub fn expand_department_tree(all: &[(i64, Option<i64>)], roots: &[i64]) -> Vec<i64> {
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

pub fn dedup_i64(v: Vec<i64>) -> Vec<i64> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|x| seen.insert(*x)).collect()
}

pub fn dedup_str(v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

/// kernel 自守测试（泛化、无库无网）。28 个 DMS 裁决断言原地留在 server/src/scope.rs，
/// T5 才物理迁到 policy/tests（裁决 C3）——此处只锁「重构不静默改语义」的最小网。
#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn decide_base_full_value_domain() {
        assert_eq!(decide_base(&[]).unwrap(), BaseDecision::Unrestricted);
        assert_eq!(decide_base(&[10]).unwrap(), BaseDecision::Unrestricted);
        assert_eq!(decide_base(&[0]).unwrap(), BaseDecision::Ids(vec![]));
        assert_eq!(decide_base(&[3]).unwrap(), BaseDecision::Ids(vec![SENTINEL]));
        assert_eq!(decide_base(&[1]).unwrap(), BaseDecision::Departments { with_children: false });
        assert_eq!(decide_base(&[2]).unwrap(), BaseDecision::Departments { with_children: true });
    }

    #[test]
    fn decide_base_takes_max_and_fails_closed() {
        assert_eq!(decide_base(&[0, 2]).unwrap(), BaseDecision::Departments { with_children: true });
        assert_eq!(decide_base(&[2, 3]).unwrap(), BaseDecision::Ids(vec![SENTINEL]));
        // 未知枚举值绝不当"看全部"（catch-all 不能 = 放行），未知值是 MAX 时同样拒绝
        assert!(decide_base(&[7]).is_err());
        assert!(decide_base(&[0, 42]).is_err());
    }

    #[test]
    fn merge_ids_sentinel_vs_empty_are_opposite() {
        assert_eq!(merge_employee_ids(&[1, 2], &[2, 3, 3]), vec![1, 2, 3]);
        // 哨兵段跳过，有值段胜出
        assert_eq!(merge_employee_ids(&[SENTINEL], &[5]), vec![5]);
        // 全段哨兵 → [-1] 拒绝（0 行）
        assert_eq!(merge_employee_ids(&[SENTINEL], &[]), vec![SENTINEL]);
        // 全空无哨兵 → 空集 = 不注入（放行）
        assert!(merge_employee_ids(&[], &[]).is_empty());
    }

    #[test]
    fn merge_codes_flagging_segment_rejects_when_empty() {
        assert_eq!(merge_customer_codes(&[(s(&["C1", "C1", " "]), false)]), s(&["C1"]));
        assert_eq!(merge_customer_codes(&[(vec![], true)]), s(&["-1"]));
        // 其他段有值 → 用有值段，不落 -1
        assert_eq!(merge_customer_codes(&[(s(&["C9"]), false), (vec![], true)]), s(&["C9"]));
        assert!(merge_customer_codes(&[(vec![], false)]).is_empty());
    }

    #[test]
    fn dept_tree_recurses_with_cycle_guard() {
        let all = vec![(2, Some(1)), (3, Some(2)), (4, Some(3))];
        let mut got = expand_department_tree(&all, &[1]);
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4]);
        // 脏数据成环必须终止且不重复
        let mut cyc = expand_department_tree(&[(2, Some(1)), (1, Some(2))], &[1]);
        cyc.sort();
        assert_eq!(cyc, vec![1, 2]);
        // parent 为 NULL 的行不被任何 frontier 拉入；未知根返回自身
        assert_eq!(expand_department_tree(&[(9, None), (2, Some(1))], &[1]), vec![1, 2]);
        assert_eq!(expand_department_tree(&[], &[42]), vec![42]);
    }

    #[test]
    fn unrestricted_only_when_all_empty() {
        assert!(ScopeSets::default().is_unrestricted());
        let s1 = ScopeSets { employee_ids: vec![SENTINEL], ..Default::default() };
        assert!(!s1.is_unrestricted(), "哨兵不是不限制");
        let s2 = ScopeSets { customer_codes: s(&["-1"]), ..Default::default() };
        assert!(!s2.is_unrestricted());
        let s3 = ScopeSets { login_names: s(&["zhangsan"]), ..Default::default() };
        assert!(!s3.is_unrestricted());
        let s4 = ScopeSets { shop_codes: s(&["S001"]), ..Default::default() };
        assert!(!s4.is_unrestricted());
    }
}
