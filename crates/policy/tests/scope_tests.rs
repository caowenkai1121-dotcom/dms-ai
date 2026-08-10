//! 权限决策纯逻辑离线单测（对拍依据：Java DefaultEmployee + tools/judge_scope.py 连库判官）。
//! 连库判官仍是最终验收；此处锁住不依赖数据的语义，防重构静默改语义。
//!
//! 28 个断言**一字不改**物理搬自 server/src/scope.rs:363-554（裁决 C3 / T5）。
//! 唯一改动：测试头的 `use super::*` → `use dms_policy::scope::*`。

use dms_policy::scope::*;

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
    let s4 = ScopeSets { manager_customer_codes: s(&["C1"]), ..Default::default() };
    assert!(!s4.is_unrestricted());
    let s5 = ScopeSets { shop_codes: s(&["S001"]), ..Default::default() };
    assert!(!s5.is_unrestricted());
}
