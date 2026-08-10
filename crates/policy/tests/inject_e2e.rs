//! 端到端：决策 → 合并 → SQL 注入（跨模块语义锁）。
//!
//! 3 个断言**一字不改**物理搬自 server/src/scope.rs:556-597（裁决 C3 / T5）。
//! 唯一改动：测试头的 `use super::*` → 下面两条 `use`，以及被调符号的模块前缀
//! `crate::inject::inject` → `dms_policy::inject`。

use dms_policy::inject;
use dms_policy::scope::*;

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
        ..Default::default()
    };
    assert!(!sets.is_unrestricted());
    let sql = inject("SELECT * FROM t_sales_order so", &sets).unwrap();
    let n = sql.to_lowercase().replace(' ', "");
    assert!(n.contains("so.owner_managerin(-1)"), "{sql}");
    assert!(n.contains("so.customer_codein('-1')"), "{sql}");
}

#[test]
fn all_view_injects_nothing() {
    // view_type=10 → 不限制 → SQL 原样（超管路径同）
    assert_eq!(decide_base(&[10]).unwrap(), BaseDecision::Unrestricted);
    let sql = "SELECT * FROM t_sales_order so WHERE so.deleted_flag = 0";
    assert_eq!(inject(sql, &ScopeSets::default()).unwrap(), sql);
}

#[test]
fn me_view_injects_own_id_only() {
    // 本人档(0) + 无定制 → 只注入自己
    let base = vec![199_i64]; // 调用方填本人 id
    let sets = ScopeSets {
        employee_ids: merge_employee_ids(&base, &[]),
        employee_codes: vec![],
        customer_codes: vec![],
        ..Default::default()
    };
    let sql = inject("SELECT * FROM t_sales_order so", &sets).unwrap();
    let n = sql.to_lowercase().replace(' ', "");
    assert!(n.contains("so.owner_managerin(199)"), "{sql}");
    assert!(!n.contains("customer_code"), "客户集合为空不得注入该段: {sql}");
}
