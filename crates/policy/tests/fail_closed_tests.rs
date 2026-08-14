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
    // 收紧成 (1=0) 会让 Rust 与 judge_scope.py 的独立 Java 复刻分叉，属业务裁决（见 docs/ARCHITECTURE.md §3）。
    //
    // 被测表 2026-08-14 从 `t_customer_balance` 换成 `t_customer_device_ledger`：
    // 前者已按 Java `CustomerBalanceMapper.java:36` 改成 via（借 t_customer 的
    // `customer_code IN codes OR area_manager_id IN ids`），不再是「只有 customer 段」那一族。
    // 现存这一族只剩三张：device_ledger / disposal_order / shop_inspection_records。
    let s = sets(&[7], &[], &[]);
    let out = inject("SELECT * FROM t_customer_device_ledger b", &s).unwrap();
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

/// 🔴 无 owner 维度兜底的 scoped 表必须用 `RequiredCodes`，不能用 `Codes`。
///
/// `Codes` 臂在客户集合为空时一个段都不 push（`kernel/policy/inject.rs:450-465`），
/// 没有 owner 段兜底就等于**不注入**＝整表可见。这两张数仓表此前正是这个形态，
/// 而紧邻的注释白纸黑字写着「fail-closed」（2026-08-13 审计抓出的 fail-open）。
///
/// 断言直接读 `builtin_rules()`：改回 `Codes` 立刻红，改档案顺序不影响。
#[test]
fn warehouse_customer_only_tables_are_required_codes() {
    let rules = dms_policy::builtin::builtin_rules();
    for table in ["ads_off_sales_cost_customer_dnf", "dws_mkt_app_place_order_dnf"] {
        let Some(TableRule::Scoped(binding)) = rules.get(table) else {
            panic!("{table} 必须是 scoped 档案");
        };
        assert!(binding.owner_col.is_none(), "{table} 档案形态变了，本断言需同步复核");
        assert!(
            binding.customer_kind == CustomerKind::RequiredCodes,
            "{table} 空客户集合会变成不注入＝整表可见"
        );
    }
}

/// 同一条不变量的行为面：受限身份 + 空客户集合 → 恒假；有客户集合 → 按 store_code 隔离。
#[test]
fn market_expense_blocks_restricted_identity_without_customers() {
    let rules = one(
        "ads_off_sales_cost_customer_dnf",
        TableRule::Scoped(Binding {
            customer_col: Some("store_code".into()),
            customer_kind: CustomerKind::RequiredCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        }),
    );
    let sql = "SELECT SUM(cost) FROM ads_off_sales_cost_customer_dnf a";
    let out = rewrite(sql, &sets(&[7], &[], &[]), &rules, &MysqlDialect).unwrap();
    assert!(norm(&out).contains("1=0"), "空客户集合必须恒假: {out}");
    let out = rewrite(sql, &sets(&[7], &[], &["C001"]), &rules, &MysqlDialect).unwrap();
    assert!(norm(&out).contains("a.store_codein('c001')"), "必须按 store_code 隔离: {out}");
}

/// 🔴 Java 对拍：余额表的 `area_manager_id` 分支不许丢。
///
/// `CustomerBalanceMapper.java:36` 的 joinSql 是
/// `c.customer_code in (#customerCodes) #or c.area_manager_id in (#employeeIds)`，
/// 其中 `c` 是 XML 里 LEFT JOIN 进来的 `t_customer` —— 那一列不在 balance 表上，
/// 所以只能借 t_customer 的档案（via）。此前只按 balance 自己的 customer_code 过滤，
/// 区域经理看不到本该可见的余额行（答少了）。
#[test]
fn customer_balance_keeps_the_area_manager_branch() {
    let out = inject("SELECT * FROM t_customer_balance b", &sets(&[7], &[], &[])).unwrap();
    let n = norm(&out);
    assert!(n.contains("exists(select1fromt_customer"), "必须借 t_customer 的档案: {out}");
    assert!(n.contains("area_manager_idin(7)"), "Java 的 area_manager 分支丢了: {out}");
}

/// 🔴 Java 对拍（2026-08-14 第 5 轮）：五张「Java 有 @DataScope、我们没档案」的表。
///
/// 缺档案不是放行、是 `UnregisteredTable` 整句拒 —— DMS 页面里看得见的单据，问数说没权限。
/// 本判据走**行为面**（真注入一次），档案形态漂了或列名写错都会红。
#[test]
fn java_scoped_tables_actually_inject_their_condition() {
    let s = sets(&[7], &["E7"], &["C001"]);
    for (sql, must) in [
        // ApplicationListHeaderMapper.java:21（别名 invoice = t_application_list_header）
        ("SELECT * FROM t_application_list_header invoice", &["invoice.customer_codein('c001')", "invoice.managerin(7)"][..]),
        // DeviceTransferOrderMapper.java:20：只有客户段，列是 out_customer_code
        ("SELECT * FROM t_device_transfer_order o", &["o.out_customer_codein('c001')"]),
        // StatementApplicationMapper.java:23：owner 段是 #employeeCodes → 登录名族
        ("SELECT * FROM t_statement_apply t", &["t.customer_codein('c001')", "t.created_byin('e7')"]),
        // DeviceRequisitionMapper.xml:201 —— 条件挂在 JOIN 进来的 t_customer 上
        ("SELECT * FROM t_device_requisition tdr", &["exists(select1fromt_customer", "area_manager_idin(7)"]),
        ("SELECT * FROM t_application_list_detail d", &["exists(select1fromt_application_list_header"]),
    ] {
        let out = inject(sql, &s).unwrap_or_else(|e| panic!("{sql} 被拒了：{e}"));
        let n = norm(&out);
        for frag in must {
            assert!(n.contains(frag), "{sql} 少了 Java 的条件片段 {frag}：{out}");
        }
    }
}
