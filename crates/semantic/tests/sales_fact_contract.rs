use dms_semantic::sales_fact::{
    self, Dimension, Metric, Predicate, QueryOptions, Sort, SortDirection,
};

#[test]
fn dws_sales_contract_has_only_confirmed_metrics_and_dimensions() {
    assert_eq!(sales_fact::TABLE, "sales_dw.dws_off_offline_sale_dfn");
    assert_eq!(
        sales_fact::METRICS.iter().map(|metric| metric.name()).collect::<Vec<_>>(),
        ["销售额", "销量", "不含税成本", "不含税收入", "毛利额", "毛利率"]
    );
    assert_eq!(Metric::SalesAmount.expression(), "SUM(amount)");
    assert_eq!(Metric::SalesQuantity.expression(), "SUM(qty)");
    assert_eq!(
        Metric::GrossMargin.expression(),
        "SUM(gross_profit)/NULLIF(SUM(revenue_excluding_tax),0)"
    );

    let dimension_terms = sales_fact::DIMENSIONS
        .iter()
        .flat_map(|dimension| {
            std::iter::once(dimension.name()).chain(dimension.aliases().iter().copied())
        })
        .collect::<Vec<_>>();
    for forbidden in ["manger", "经理", "业务员", "销售员", "门店"] {
        assert!(
            dimension_terms.iter().all(|term| !term.contains(forbidden)),
            "未证实人员/门店语义不得进入默认 DWS 维度：{forbidden}"
        );
    }
    assert!(dimension_terms.contains(&"客户"));
    assert!(dimension_terms.contains(&"客户编码"));
}

#[test]
fn trusted_builder_owns_fact_filters_sort_and_limit() {
    let predicates = [Predicate::contains(Dimension::Customer, "测试客户")];
    let sql = sales_fact::aggregate_sql_with_options(
        &[Metric::SalesAmount, Metric::GrossProfit],
        &[Dimension::Customer],
        ":begin",
        ":end",
        QueryOptions {
            predicates: &predicates,
            sort: Some(Sort::metric(Metric::SalesAmount, SortDirection::Desc)),
            limit: Some(20),
        },
    );
    assert!(sql.contains("FROM sales_dw.dws_off_offline_sale_dfn sf"), "{sql}");
    assert!(sql.contains("sf.order_date >= :begin AND sf.order_date < :end"), "{sql}");
    assert!(sql.contains("sf.storename"), "客户必须取 storename：{sql}");
    assert!(sql.contains("ORDER BY SUM(sf.amount) DESC LIMIT 20"), "{sql}");
    assert!(!sql.to_uppercase().contains("COUNT("), "订单数不得按事实行数推算：{sql}");
}

#[test]
fn seed_clears_legacy_sales_time_cap_and_disables_untrusted_manager() {
    let seed = include_str!("../src/seed_defs.rs");
    let clears_time_cap = concat!(
        "UPDATE meta.metric SET version=$1, allowed_dimensions=$2, ",
        "time_cap='' WHERE ds_id=$3 AND metric_code=$4"
    );
    assert!(seed.contains(clears_time_cap), "DWS 销售指标必须显式清空旧 yesterday time_cap");

    let disables_manager = concat!(
        "dim_code IN ('owner',",
        "'manager_name')"
    );
    assert!(seed.contains(disables_manager), "历史经理维度必须在播种时禁用");

    assert!(seed.contains("(\"order_amount\", \"订单额\", &[\"订单金额\", \"下单金额\", \"订单总额\"]"));
    assert!(seed.contains("(\"avg_order_value\", \"订单客单价\", &[\"客单价\", \"订单单均\", \"平均客单\"]"));
    for ambiguous in [
        "下单口径销售额",
        "订单口径销售额",
        "成交总额=销售额",
        "成交总额＝销售额",
        "销售额/订单数",
        "销售额÷订单数",
    ] {
        assert!(!seed.contains(ambiguous), "订单口径不得吸收默认销售额语义：{ambiguous}");
    }
}
