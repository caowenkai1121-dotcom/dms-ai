//! fail-closed 回归锁（**不计入 46**）：注入路径上「失败必须拒绝」的四条底线，
//! 外加一条钉住待业务裁决的**现状**行为。
//!
//! 前两个断言一字不改搬自 server/src/inject.rs:336-354（按 ARCHITECTURE §4.3 归本文件）；
//! 后三个是本轮新增。新增的三条刻意**不碰全局注册表**（直接调 kernel 的 `rewrite` 传自造
//! `RuleSet`）——`rules::install()` 会污染同进程内其它测试。

use std::collections::HashMap;

use dms_kernel::policy::inject::rewrite;
use dms_kernel::{Binding, CustomerKind, MysqlDialect, OwnerKind, PolicyError, RuleSet, TableRule};
use dms_policy::scope::ScopeSets;
use dms_policy::{inject, parse_full_expr};

fn sets(ids: &[i64], codes: &[&str], cust: &[&str]) -> ScopeSets {
    ScopeSets {
        employee_ids: ids.to_vec(),
        employee_codes: codes.iter().map(|s| s.to_string()).collect(),
        customer_codes: cust.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn norm(s: &str) -> String {
    s.to_lowercase().replace(' ', "")
}

/// 只在本文件内可见的一次性档案，永不进全局注册表
fn one(table: &str, rule: TableRule) -> RuleSet {
    RuleSet::from(HashMap::from([(table.to_string(), rule)]))
}

#[test]
fn full_expr_required() {
    // 正常条件：解析完整
    assert!(parse_full_expr("(x.owner_manager in (1,2) or x.customer_code in ('c1'))").is_some());
    // 截断式：`owner manager` 中间有空格，parse_expr 只吃到 x.owner 就返回成功 → 必须判失败
    assert!(parse_full_expr("(x.owner manager in (1))").is_none());
    // 语法坏：直接失败
    assert!(parse_full_expr("(").is_none());
    assert!(parse_full_expr("").is_none());
}

#[test]
fn empty_segments_allows_today() {
    // ⚠️ 钉住**现状**行为（与 Java 一致：段全空 → 不注入 → 放行全部），不是主张它正确。
    // t_customer_balance 只有 customer 段；受限用户客户集合为空时该表整表可见。
    // 收紧成 (1=0) 会让 Rust 与 judge_scope.py 的独立 Java 复刻分叉，属业务裁决（见 docs/ARCHITECTURE.md §3）。
    let s = sets(&[7], &[], &[]);
    let out = inject("SELECT * FROM t_customer_balance b", &s).unwrap();
    assert!(!norm(&out).contains("in("), "现状=不注入（Java 同语义）: {out}");
}

/// F1 的截断式变体走完整注入路径：坏列档案 → 条件前缀解析成功但吃不到 EOF → **必须拒表**。
/// 旧实现在这里 `if let Ok` 静默丢掉条件，查询照跑 = 语法合法的越权。
#[test]
fn broken_owner_col_blocks_the_table() {
    let bad = one(
        "t_sales_order",
        TableRule::Scoped(Binding {
            customer_col: None,
            customer_kind: CustomerKind::Codes,
            // 列名里带空格：`so.owner manager in (7)` 只会被吃到 `so.owner`
            owner_col: Some("owner manager".into()),
            owner_kind: OwnerKind::Ids,
        }),
    );
    let err = rewrite("SELECT * FROM t_sales_order so", &sets(&[7], &[], &[]), &bad, &MysqlDialect)
        .unwrap_err();
    assert!(matches!(err, PolicyError::ConditionParse(_)), "{err}");
    assert!(err.to_string().contains("已按 fail-closed 拒绝"), "{err}");
}

/// via 明细表独查时头表没有 scoped 档案 → 拼不出 EXISTS 条件 → **拒表**，绝不裸放行。
#[test]
fn via_head_without_scoped_rule_is_rejected() {
    let orphan = one(
        "t_sales_order_detail",
        TableRule::Via {
            table: "t_sales_order".into(),
            local_col: "sales_order_code".into(),
            remote_col: "sales_order_code".into(),
        },
    );
    let err =
        rewrite("SELECT * FROM t_sales_order_detail d", &sets(&[7], &[], &[]), &orphan, &MysqlDialect)
            .unwrap_err();
    assert!(matches!(err, PolicyError::ViaHeadUnregistered { .. }), "{err}");
}

/// 空档案（PG 加载失败/表被清空）对受限用户 = 每张表都拒，**不是**放行全部。
#[test]
fn empty_ruleset_rejects_every_table_for_restricted_user() {
    let err = rewrite(
        "SELECT * FROM t_sales_order so",
        &sets(&[7], &[], &[]),
        &RuleSet::default(),
        &MysqlDialect,
    )
    .unwrap_err();
    assert!(matches!(err, PolicyError::UnregisteredTable(_)), "{err}");
}

#[test]
fn dws_sales_requires_customer_codes_for_restricted_user() {
    let rules = one(
        "dws_off_offline_sale_dfn",
        TableRule::Scoped(Binding {
            customer_col: Some("storecode".into()),
            customer_kind: CustomerKind::RequiredCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        }),
    );

    let out = rewrite(
        "SELECT SUM(amount) FROM sales_dw.dws_off_offline_sale_dfn s",
        &sets(&[7], &[], &[]),
        &rules,
        &MysqlDialect,
    )
    .unwrap();
    assert!(norm(&out).contains("1=0"), "受限账号空客户集合必须恒假: {out}");

    let out = rewrite(
        "SELECT SUM(amount) FROM sales_dw.dws_off_offline_sale_dfn s",
        &sets(&[7], &[], &["C001"]),
        &rules,
        &MysqlDialect,
    )
    .unwrap();
    assert!(norm(&out).contains("s.storecodein('c001')"), "必须按 storecode 隔离: {out}");
}

#[test]
fn dws_sales_stays_full_for_explicit_unrestricted_identity() {
    let rules = one(
        "dws_off_offline_sale_dfn",
        TableRule::Scoped(Binding {
            customer_col: Some("storecode".into()),
            customer_kind: CustomerKind::RequiredCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        }),
    );
    let sql = "SELECT SUM(amount) FROM sales_dw.dws_off_offline_sale_dfn";
    assert_eq!(rewrite(sql, &ScopeSets::default(), &rules, &MysqlDialect).unwrap(), sql);
}
