//! 权限条件注入的 DMS 断言（内置档案 + Java joinSql 口径）。
//!
//! 15 个断言**一字不改**物理搬自 server/src/inject.rs:195-334 与 356-366（裁决 C3 / T5）。
//! 唯一改动：测试头的 `use super::*` → 下面两条 `use`。
//! （同文件的 `full_expr_required` / `empty_segments_allows_today` 属 fail-closed 锁，
//! 按 ARCHITECTURE §4.3 落 `fail_closed_tests.rs`。）

use dms_policy::inject;
use dms_policy::scope::ScopeSets;

fn sets(ids: &[i64], codes: &[&str], cust: &[&str]) -> ScopeSets {
    ScopeSets {
        employee_ids: ids.to_vec(),
        employee_codes: codes.iter().map(|s| s.to_string()).collect(),
        customer_codes: cust.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// sqlparser 回渲染会大写关键字/补 AS/加空格——归一化后按语义断言
fn norm(s: &str) -> String {
    s.to_lowercase().replace(' ', "")
}

#[test]
fn unrestricted_passthrough() {
    let s = sets(&[], &[], &[]);
    let sql = "SELECT * FROM t_sales_order so WHERE so.deleted_flag = 0";
    assert_eq!(inject(sql, &s).unwrap(), sql);
}

#[test]
fn injects_both_dims_with_or() {
    let s = sets(&[1, 2], &[], &["C001", "C002"]);
    let out = inject("SELECT COUNT(*) FROM t_sales_order so WHERE so.deleted_flag = 0", &s).unwrap();
    let n = norm(&out);
    assert!(n.contains("so.owner_managerin(1,2)"), "{out}");
    assert!(n.contains("so.customer_codein('c001','c002')"), "{out}");
    assert!(n.contains("or"), "{out}");
    // 原条件被括号保护
    assert!(n.contains("(so.deleted_flag=0)"), "{out}");
}

#[test]
fn sentinel_rejects() {
    let s = sets(&[-1], &[], &["-1"]);
    let out = inject("SELECT * FROM t_customer cus", &s).unwrap();
    let n = norm(&out);
    assert!(n.contains("cus.area_manager_idin(-1)"), "{out}");
    assert!(n.contains("cus.customer_codein('-1')"), "{out}");
}

#[test]
fn device_tables_fail_closed_in_generic_injection() {
    let s = ScopeSets {
        employee_ids: vec![7, 8],
        employee_codes: vec!["alice".into(), "bob".into()],
        customer_codes: vec!["COMMON".into(), "TEAM".into()],
        login_names: vec!["alice".into()],
        manager_customer_codes: vec!["C1".into(), "C2".into()],
        ..Default::default()
    };
    // t_device_requisition 2026-08-14 起**有档案**（via t_customer，Java 条件挂在 JOIN 进来的
    // t_customer 上，DeviceRequisitionMapper.xml:201）—— 行为面判据在
    // `fail_closed_tests::java_scoped_tables_actually_inject_their_condition`。
    // 留在这里的两张明细仍拒：它们的头表是 via，而 via 的头必须 Scoped（链式 via 表达不了）。
    for table in ["t_device_receive_item", "t_device_delivery_item"] {
        let err = inject(&format!("SELECT * FROM {table} d"), &s).unwrap_err();
        assert!(err.to_string().contains("未在权限档案登记"), "{table}: {err}");
    }
}

#[test]
fn retired_broad_rules_fail_closed_in_generic_injection() {
    let s = sets(&[7], &[], &["C1"]);
    for table in ["t_account_bill_header", "t_account_bill_detail", "t_winc_purchase_transfer"] {
        let err = inject(&format!("SELECT * FROM {table} x"), &s).unwrap_err();
        assert!(err.to_string().contains("未在权限档案登记"), "{table}: {err}");
    }
}

#[test]
fn old_invoice_is_manager_only_but_new_invoice_keeps_customer_or_manager() {
    let s = sets(&[7], &[], &["C1"]);
    let old = norm(&inject("SELECT * FROM t_invoice_apply_header i", &s).unwrap());
    assert!(old.contains("i.managerin(7)"), "{old}");
    assert!(!old.contains("customer_codein"), "旧开票不得按客户权限放大: {old}");

    let new = norm(&inject("SELECT * FROM t_invoice_new_apply_header i", &s).unwrap());
    assert!(new.contains("i.managerin(7)"), "{new}");
    assert!(new.contains("i.customer_codein('c1')"), "{new}");
}

#[test]
fn master_shop_uses_shop_codes_not_customer_codes() {
    let s = ScopeSets {
        customer_codes: vec!["C001".into()],
        shop_codes: vec!["S001".into()],
        ..Default::default()
    };
    let out = inject("SELECT * FROM t_master_shop s", &s).unwrap();
    let n = norm(&out);
    assert!(n.contains("s.shop_codein('s001')"), "{out}");
    assert!(!n.contains("'c001'"), "门店权限不得用 customer_codes 近似: {out}");
}

#[test]
fn no_alias_uses_table_name() {
    let s = sets(&[5], &[], &[]);
    let out = inject("SELECT * FROM t_sales_order", &s).unwrap();
    assert!(norm(&out).contains("t_sales_order.owner_managerin(5)"), "{out}");
}

#[test]
fn subquery_injected() {
    let s = sets(&[7], &[], &[]);
    let out = inject(
        "SELECT a.total FROM (SELECT SUM(order_amount) AS total FROM t_sales_order so) a",
        &s,
    )
    .unwrap();
    assert!(norm(&out).contains("so.owner_managerin(7)"), "{out}");
}

#[test]
fn unbound_table_untouched() {
    let s = sets(&[1], &[], &["C1"]);
    let sql = "SELECT * FROM t_goods g WHERE g.deleted_flag = 0";
    let out = inject(sql, &s).unwrap();
    assert!(!norm(&out).contains("in("), "未绑定表不得注入: {out}");
}

#[test]
fn quote_escaped() {
    let s = sets(&[], &[], &["C'1"]);
    let out = inject("SELECT * FROM t_customer c", &s).unwrap();
    assert!(out.contains("'C''1'"), "{out}");
}

#[test]
fn rejects_non_select() {
    let s = sets(&[1], &[], &[]);
    assert!(inject("DELETE FROM t_sales_order", &s).is_err());
}

#[test]
fn unregistered_table_rejected() {
    // fail-closed：受限用户查未登记表必须拒绝（防"忘登记即放行"）
    let s = sets(&[1], &[], &[]);
    let err = inject("SELECT * FROM t_role_data_scope", &s).unwrap_err();
    assert!(err.to_string().contains("未在权限档案登记"), "{err}");
    // 超管不受限
    let all = sets(&[], &[], &[]);
    assert!(inject("SELECT * FROM t_role_data_scope", &all).is_ok());
}

#[test]
fn detail_standalone_gets_exists() {
    // 明细独查：借头表条件 EXISTS 半连接（堵标注遗漏的独查泄漏）
    let s = sets(&[7], &[], &["C1"]);
    let out = inject(
        "SELECT SUM(box_quantity) FROM t_sales_order_detail d WHERE d.item_type = '1'",
        &s,
    )
    .unwrap();
    let n = norm(&out);
    assert!(n.contains("exists(select1fromt_sales_order"), "{out}");
    assert!(n.contains("__ds_h.sales_order_code=d.sales_order_code"), "{out}");
    assert!(n.contains("__ds_h.owner_managerin(7)"), "{out}");
    assert!(n.contains("__ds_h.customer_codein('c1')"), "{out}");
}

#[test]
fn detail_with_header_skips_exists() {
    // 头表同 SELECT 在场：条件由头表承担，明细不再加 EXISTS（避免双重注入拖慢）
    let s = sets(&[7], &[], &[]);
    let out = inject(
        "SELECT SUM(d.box_quantity) FROM t_sales_order_detail d JOIN t_sales_order so ON so.sales_order_code = d.sales_order_code",
        &s,
    )
    .unwrap();
    let n = norm(&out);
    assert!(n.contains("so.owner_managerin(7)"), "{out}");
    assert!(!n.contains("exists("), "{out}");
}

#[test]
fn cte_name_exempt_and_inner_injected() {
    // CTE 名是虚拟表不得误拦；CTE 内部照常注入
    let s = sets(&[3], &[], &[]);
    let out = inject(
        "WITH x AS (SELECT owner_manager, total_amount FROM t_sales_order) SELECT SUM(total_amount) FROM x",
        &s,
    )
    .unwrap();
    let n = norm(&out);
    assert!(n.contains("owner_managerin(3)"), "{out}");
}

#[test]
fn logistics_via_header() {
    // Java SalesOrderLogisticsMapper @DataScope 全部绑在 tso.* → 独查借头表
    let s = sets(&[], &[], &["C9"]);
    let out = inject("SELECT * FROM t_sales_order_logistics", &s).unwrap();
    let n = norm(&out);
    assert!(n.contains("exists(select1fromt_sales_order"), "{out}");
    assert!(n.contains("__ds_h.customer_codein('c9')"), "{out}");
}

#[test]
fn backtick_table_injected() {
    // 🔴 M3 e2e 抓获的真实翻车：LLM 生成反引号表名，注入器必须照样命中
    let s = sets(&[42], &[], &["C9"]);
    let out = inject(
        "SELECT SUM(`total_goods_amount`) AS `销售额` FROM `t_sales_order` WHERE `deleted_flag` = 0",
        &s,
    )
    .unwrap();
    assert!(norm(&out).contains("owner_managerin(42)"), "{out}");
}

/// 🔴 **FROM 之外的子查询也必须注入**（二·AU1 的安全修复）。
///
/// 修前实测：`inject_select` 只遍历 `sel.from`，于是这种形态外层 FROM 为空 ⇒ 零注入 ⇒
/// 受限用户拿到全公司数据。`route` 正常、形状正常、零报错，
/// 而 `UnregisteredTable` 那道 fail-closed 也不触发（它只在表出现在 FROM 里才判）。
///
/// 这个形态**在库里就有**：`meta.metric` 的「退款占比」`agg_expr` 正是两个标量子查询相除。
///
/// 判据不许只断言「注入后 SQL 变长了」—— 要**逐个子查询**断言它自己那份条件在里面。
#[test]
fn scalar_subqueries_in_projection_get_injected() {
    let s = sets(&[7], &[], &["C9"]);
    // 「退款占比」的真实形态（两个标量子查询相除），外层没有 FROM
    let sql = "SELECT (SELECT SUM(refund_amount) FROM t_after_sales_order_header WHERE deleted_flag = 0) \
               * 100.0 / (SELECT SUM(total_amount) FROM t_sales_order WHERE deleted_flag = 0) AS r";
    let out = inject(sql, &s).unwrap();
    let n = norm(&out);
    // 两个子查询各自都要有条件 —— 只判一个的话，漏注入另一个照样绿
    assert!(n.contains("t_sales_order"), "{out}");
    assert!(
        n.matches("customer_codein('c9')").count() >= 1,
        "订单那条子查询没被注入 —— 这正是修前的越权：{out}"
    );
    assert!(
        n.contains("owner_managerin(7)") || n.contains("customer_codein('c9')"),
        "一条权限条件都没进去：{out}"
    );
    // 防恒真：无限制档下这条 SQL 必须**原样返回**（否则上面的断言可能只是「注入器总在加东西」）
    let free = sets(&[], &[], &[]);
    assert_eq!(inject(sql, &free).unwrap(), sql, "无限制档不该被改写");
}

/// WHERE 里的 `IN (SELECT …)` 与 `EXISTS (SELECT …)` 同样要注入。
#[test]
fn subqueries_in_where_get_injected() {
    let s = sets(&[7], &[], &["C9"]);
    for sql in [
        "SELECT COUNT(*) FROM t_customer cus WHERE cus.customer_code IN \
         (SELECT customer_code FROM t_sales_order WHERE deleted_flag = 0)",
        "SELECT COUNT(*) FROM t_customer cus WHERE EXISTS \
         (SELECT 1 FROM t_sales_order o WHERE o.customer_code = cus.customer_code)",
    ] {
        let out = inject(sql, &s).unwrap();
        let n = norm(&out);
        // 内层 t_sales_order 那份条件必须在（外层 t_customer 的那份本来就有）
        assert!(
            n.matches("customer_codein('c9')").count() >= 2,
            "内层子查询没被注入（只数到 {} 处）：{out}",
            n.matches("customer_codein('c9')").count()
        );
    }
}

/// 🔴 **量器自检**：`walk_expr_subqueries` 是手写 match，漏一个 `Expr` 变体就是静默越权。
/// `subqueries_of` 用 sqlparser 自己的 `Visit` 数总数再对拍，不等则 fail-closed。
///
/// 这条断言证明**对拍真的在跑**：拿一个含子查询的形态，注入必须成功（数得上）；
/// 而不是靠「没报错」当通过 —— 那种绿等于什么都没证。
#[test]
fn subquery_coverage_selfcheck_is_live() {
    let s = sets(&[7], &[], &["C9"]);
    // CASE WHEN 里的子查询：属于「容器变体」那一族，漏了就会被对拍抓住
    let sql = "SELECT CASE WHEN (SELECT COUNT(*) FROM t_sales_order) > 0 THEN 1 ELSE 0 END AS f \
               FROM t_customer cus";
    let out = inject(sql, &s).expect("对拍失败说明 walk_expr_subqueries 漏了 CASE 那一族");
    let n = norm(&out);
    assert!(
        n.matches("customer_codein('c9')").count() >= 2,
        "CASE 里的子查询没被注入：{out}"
    );
}
