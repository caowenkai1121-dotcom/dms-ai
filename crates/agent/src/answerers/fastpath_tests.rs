//! `server/src/direct.rs` 的测试全集（T8-B9 整体搬运，**一条不改**）。
//!
//! 为什么整块落在 agent 而不是分散到各自的实现文件：被测符号已按依赖切成三处
//! （`dms_semantic::compose::*` / `dms_semantic::fastpath::*` / 本 crate 的 `fastpath_intent`），
//! 而 149 条断言里有大量跨族用例（一个 test 同时调装配器与模板）。先整块搬保证
//! **一条断言都不丢**；按族拆开另起一笔（那是纯测试重构，不动生产码）。
//!
//! `ponytail:` 单文件 3300 行远超 D2 的 450 —— 这是搬运期的已知天花板，
//! 拆分排在 T8 收尾之后，判据是「拆完仍 149 条全绿」。

#![allow(unused_imports)]

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};
use dms_semantic::compose::*;
use dms_semantic::compose::{assemble::*, metric::*, path::*, values::*};
use dms_semantic::fastpath::*;
use dms_semantic::fastpath::{
    derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*,
};
use dms_semantic::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use dms_semantic::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

use super::fastpath_intent::*;

/// 🔴 「确定性路径的生产源码」这一判据面的**唯一入口**。
///
/// T8 之前它就是 `direct.rs` 一个文件；搬完之后拆成三处（semantic 的 compose/fastpath +
/// 本 crate 的 fastpath_intent）。下面几条源码扫描断言必须吃**全部**这三处 ——
/// 指向测试文件或只指其中一处，判据就恒真了（本仓反复抓到的那类缺陷）。
const DETERMINISTIC_SRC: &str = concat!(
    include_str!("../../../semantic/src/compose/mod.rs"),
    include_str!("../../../semantic/src/compose/assemble.rs"),
    include_str!("../../../semantic/src/compose/metric.rs"),
    include_str!("../../../semantic/src/compose/path.rs"),
    include_str!("../../../semantic/src/compose/values.rs"),
    include_str!("../../../semantic/src/fastpath/mod.rs"),
    include_str!("../../../semantic/src/fastpath/derive.rs"),
    include_str!("../../../semantic/src/fastpath/finance.rs"),
    include_str!("../../../semantic/src/fastpath/graph_rows.rs"),
    include_str!("../../../semantic/src/fastpath/ops.rs"),
    include_str!("../../../semantic/src/fastpath/relation.rs"),
    include_str!("../../../semantic/src/fastpath/sales.rs"),
    include_str!("../../../semantic/src/fastpath/stock.rs"),
    include_str!("../../../semantic/src/fastpath/template.rs"),
    include_str!("fastpath_intent.rs"),
);


#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use dms_connector::source::{DsPolicy, RowSet, SchemaSnapshot, SourceKind, SqlSource};
    use dms_kernel::{BoxFut, DsId, MysqlDialect, ScopedSql};

    struct StockSource {
        ds: DsId,
        candidates: Vec<(&'static str, &'static str)>,
        failure: Option<dms_connector::ConnectorError>,
        probes: Mutex<Vec<String>>,
    }

    impl StockSource {
        fn new(candidates: Vec<(&'static str, &'static str)>) -> Self {
            Self {
                ds: DsId::new("dms"),
                candidates,
                failure: None,
                probes: Mutex::new(vec![]),
            }
        }

        fn failing() -> Self {
            Self {
                ds: DsId::new("dms"),
                candidates: vec![],
                failure: Some(dms_connector::ConnectorError::query("dms", "probe failed")),
                probes: Mutex::new(vec![]),
            }
        }
    }

    impl SqlSource for StockSource {
        fn ds_id(&self) -> &DsId {
            &self.ds
        }
        fn kind(&self) -> SourceKind {
            SourceKind::Mysql
        }
        fn is_warehouse(&self) -> bool {
            true
        }
        fn dialect(&self) -> &'static dyn dms_kernel::Dialect {
            &MysqlDialect
        }
        fn set_ds_policy(&self, _policy: DsPolicy) {}
        fn fetch<'a>(
            &'a self,
            sql: &'a ScopedSql,
            _max: usize,
            _t: std::time::Duration,
        ) -> BoxFut<'a, Result<RowSet, dms_connector::ConnectorError>> {
            self.probes.lock().unwrap().push(sql.wire().to_string());
            Box::pin(async move {
                if let Some(err) = &self.failure {
                    return Err(err.clone());
                }
                Ok(RowSet {
                    columns: vec!["sku_code".into()],
                    rows: self
                        .candidates
                        .iter()
                        .map(|(code, _)| vec![(*code).into()])
                        .collect(),
                    redacted: vec![], truncated: false })
            })
        }
        fn explain<'a>(
            &'a self,
            _sql: &'a ScopedSql,
            _t: std::time::Duration,
        ) -> BoxFut<'a, Result<Option<String>, dms_connector::ConnectorError>> {
            Box::pin(async { Ok(None) })
        }
        fn probe_schema<'a>(
            &'a self,
        ) -> BoxFut<'a, Result<SchemaSnapshot, dms_connector::ConnectorError>> {
            Box::pin(async { Ok(SchemaSnapshot::default()) })
        }
    }

    async fn product_stock_hit(
        question: &str,
        candidates: Vec<(&'static str, &'static str)>,
    ) -> Option<DirectHit> {
        let source = StockSource::new(candidates);
        let principal = dms_policy::Principal {
            employee_id: 1,
            login_name: "admin".into(),
            actual_name: "管理员".into(),
            administrator_flag: true,
            department_id: None,
            role_id: 1,
            role_code: "admin".into(),
        };
        let scope = dms_policy::Scope::new(Default::default(), true);
        stock_product_filtered(question, &source, &principal, &scope, false).await
    }

    /// 自扫描断言的函数体切刀：`src` 里 `start` 标记之后、`end` 标记之前的那段。
    /// 三处接线钉共用 —— 各写一套 split/nth/expect，函数改名/顺序调整时会以难懂的方式红。
    fn body_between<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
        src.split(start)
            .nth(1)
            .unwrap_or_else(|| panic!("标记不见了：{start}"))
            .split(end)
            .next()
            .unwrap_or_else(|| panic!("边界不见了：{end}"))
    }

    fn policy(name: &str, dims: &[&str]) -> dms_semantic::registry::model::MetricPolicy {
        dms_semantic::registry::model::MetricPolicy {
            metric_code: "m".into(), name: name.into(), aliases: vec![], version: "1".into(),
            allowed_dimensions: dims.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn metric_dimension_whitelist_is_fail_closed() {
        assert!(metric_dimension_allowed(&[policy("销售额", &["省份"])], "销售额", "省份"));
        assert!(!metric_dimension_allowed(&[policy("销售额", &["省份"])], "销售额", "品牌"));
        assert!(metric_dimension_allowed(&[policy("销售额", &["*"])], "销售额", "品牌"));
        assert!(!metric_dimension_allowed(&[], "销售额", "省份"), "政策行缺失不能默认放行");
        assert!(metric_dimension_allowed(&[], "销售额", ""), "无维度总量不受维度白名单影响");
    }

    #[test]
    fn stock_and_order_detail_use_verified_business_shapes() {
        // 库存量默认源=业务中台 WMS 现行库存（2026-08-11 用户指定）；营销通快照只剩金额/省份问法
        let qty = try_direct("现在库存量是多少").expect("库存量应走中台现行库存模板");
        assert_eq!(qty.route, "direct-agg");
        assert!(qty.sql.contains("ywzt_ods.scm_warehous_manage"), "{}", qty.sql);
        assert!(qty.sql.contains("SUM(in_stock_quantity)"), "{}", qty.sql);
        assert!(qty.sql.contains("inventory_status = 'ZP'"), "{}", qty.sql);
        assert!(!qty.sql.contains("t_winc_stock_report"), "默认库存量不许再走营销通快照：{}", qty.sql);
        assert!(qty.detail.as_deref().unwrap_or_default().contains("sku_name"));

        let amount = try_direct("库存金额").expect("库存金额应走营销通快照模板（中台表无金额列）");
        assert!(amount.sql.contains("SUM(stock_amount)"), "{}", amount.sql);
        assert!(amount.sql.contains("SELECT MAX(product_stock_date)"), "{}", amount.sql);

        let orders = try_direct("昨天销售订单明细").expect("订单明细应走业务模板");
        assert_eq!(orders.route, "direct-doc");
        assert!(orders.sql.contains("sales_order_code AS `订单号`"), "{}", orders.sql);
        assert!(orders.sql.contains("DATE(o.order_time) = CURDATE() - INTERVAL 1 DAY"), "{}", orders.sql);
        assert!(orders.sql.contains("AS `订单状态`"), "{}", orders.sql);
    }

    #[test]
    fn stock_snapshot_filters_one_province_in_total_and_detail() {
        for q in ["湖南库存金额", "湖南省库存金额", "430000库存金额"] {
            let hit = stock_snapshot(q).unwrap_or_else(|| panic!("未识别：{q}"));
            let detail = hit.detail.as_deref().expect("库存必须带商品类型明细");
            for sql in [&hit.sql, detail] {
                assert!(sql.contains("province IN ('湖南','湖南省'"), "{q}: {sql}");
                assert!(sql.contains("'430000')"), "{q}: {sql}");
                assert!(sql.contains("product_stock_date = (SELECT MAX(product_stock_date)"), "{q}: {sql}");
            }
        }
        assert!(stock_snapshot("湖南和湖北库存金额").is_none(), "多省不能静默只取一个省");
        assert!(stock_snapshot("北京烤鸭库存金额").is_none(), "省名是商品实体的一部分时不能吞掉限定");
        let all = stock_snapshot("现在库存金额").expect("无省区仍应查全量");
        assert!(!all.sql.contains("province IN"), "{}", all.sql);
        assert!(!all.detail.unwrap().contains("province IN"));
    }

    #[test]
    fn stock_snapshot_groups_by_requested_business_dimension() {
        let provinces = stock_snapshot("各省份库存金额").expect("省份库存应走快照分组");
        assert!(provinces.sql.contains("AS `省份`"), "{}", provinces.sql);
        assert!(provinces.sql.contains("GROUP BY COALESCE(NULLIF(province,''),'未知')"), "{}", provinces.sql);
        assert!(!provinces.sql.contains("province IN"), "{}", provinces.sql);
        assert!(provinces.detail.is_none(), "分组结果本身就是明细，不应再附另一张表");

        // 🔴 库存**量**掉到门店快照表时，口径必须写在列名上（2026-08-15 生产直打）：
        // 「福建库存量」这类带省份的问法会从中台 WMS 掉到门店/经销商进销存快照 ——
        // 那是另一个口径，而此前答案里没有任何提示。中台表无省份列，掉落本身是有意的，
        // 缺的只是说清楚。金额不加后缀：它本来就只有这一张表，加了是噪声。
        let by_province = stock_snapshot("福建库存量").expect("省份库存走门店快照");
        assert!(
            by_province.sql.contains("AS `库存量（门店进销存口径）`"),
            "省份库存量必须披露口径：{}",
            by_province.sql
        );
        // （「用户自己点名 WinC/门店」那一档不在这里钉：`stock_snapshot` 对
        //   「福建门店库存量」「福建营销通库存量」都返 None —— 那是省份+渠道词的
        //   另一条既有拒答路径，与本条无关。分支本身在 stock.rs 里带注释。）

        let warehouses = stock_snapshot("库存金额最高的10个仓库").expect("仓库排行应走快照分组");
        assert!(warehouses.sql.contains("AS `仓库`"), "{}", warehouses.sql);
        assert!(warehouses.sql.contains("GROUP BY COALESCE(NULLIF(warehouse_name,''),'未知')"), "{}", warehouses.sql);
        assert!(warehouses.sql.contains("ORDER BY `库存金额` DESC LIMIT 10"), "{}", warehouses.sql);
        assert!(warehouses.detail.is_none());

        let largest = stock_snapshot("库存金额最大的7个仓库").expect("最大仓库排行不能退化成库存总额");
        assert!(largest.sql.contains("AS `仓库`"), "{}", largest.sql);
        assert!(largest.sql.contains("ORDER BY `库存金额` DESC LIMIT 7"), "{}", largest.sql);

        for word in ["最少", "最小", "最低"] {
            let q = format!("库存金额{word}的10个仓库");
            let low = stock_snapshot(&q).unwrap_or_else(|| panic!("低值仓库排行未识别：{q}"));
            assert!(low.sql.contains("AS `仓库`"), "{q}: {}", low.sql);
            assert!(low.sql.contains("ORDER BY `库存金额` ASC LIMIT 10"), "{q}: {}", low.sql);
            assert!(low.detail.is_none(), "低值排行的主结果就是仓库明细：{q}");
        }
    }

    #[test]
    fn stock_product_fragment_separates_generic_totals_from_entity_questions() {
        for question in ["现在库存量是多少", "当前总库存", "库存还有多少"] {
            assert!(
                stock_product_fragment(question).is_none(),
                "通用总量不应抽出商品：{question}"
            );
            assert!(
                stock_snapshot(question).is_some(),
                "通用总量仍应命中同步模板：{question}"
            );
        }
        assert_eq!(
            stock_product_fragment("小虎黑椒味烤肠500G的库存信息").as_deref(),
            Some("小虎黑椒味烤肠500G")
        );
        for (question, product) in [
            ("美的冰箱库存多少", "美的冰箱"),
            ("请帮我查一下有友凤爪的库存信息", "有友凤爪"),
            (
                "查询商品小虎黑椒味烤肠500G库存量是多少",
                "小虎黑椒味烤肠500G",
            ),
        ] {
            assert_eq!(
                stock_product_fragment(question).as_deref(),
                Some(product),
                "{question}"
            );
        }
        assert!(
            stock_snapshot("小虎黑椒味烤肠500G的库存信息").is_none(),
            "带商品限定的库存题不许再生成全库 SUM"
        );
    }

    #[tokio::test]
    async fn stock_product_inventory_uses_one_resolved_wms_sku_everywhere() {
        let source = StockSource::new(vec![("SKU-500G", "小虎黑椒味烤肠500G")]);
        let principal = dms_policy::Principal {
            employee_id: 1,
            login_name: "admin".into(),
            actual_name: "管理员".into(),
            administrator_flag: true,
            department_id: None,
            role_id: 1,
            role_code: "admin".into(),
        };
        let scope = dms_policy::Scope::new(Default::default(), true);
        let hit = stock_product_filtered(
            "小虎黑椒味烤肠500G的库存信息",
            &source,
            &principal,
            &scope,
            false,
        )
        .await
        .expect("唯一商品应命中确定性库存路径");
        let probe = source
            .probes
            .lock()
            .unwrap()
            .first()
            .cloned()
            .expect("必须先探实际 WMS 商品");
        assert!(
            probe.contains("FROM ywzt_ods.scm_warehous_manage"),
            "{probe}"
        );
        assert!(probe.contains("sku_code = '小虎黑椒味烤肠500G'"), "{probe}");
        assert!(
            probe.contains("INSTR(sku_name, '小虎黑椒味烤肠500G') > 0"),
            "{probe}"
        );
        assert!(
            !probe.contains(" LIKE ") && !probe.contains(" ESCAPE "),
            "Doris 不支持该 ESCAPE 形态，商品探针必须使用 INSTR：{probe}"
        );
        assert!(
            probe.contains("GROUP BY sku_code") && probe.contains("LIMIT 2"),
            "{probe}"
        );
        let predicate = "sku_code = 'SKU-500G'";
        assert!(
            hit.sql.contains(predicate),
            "主查询必须带唯一 SKU 谓词：{}",
            hit.sql
        );
        let detail = hit
            .detail
            .as_deref()
            .expect("商品库存应带仓库/库位/批次明细");
        assert!(
            detail.contains(predicate),
            "明细必须共享同一 SKU 谓词：{detail}"
        );
        assert!(
            !hit.sql.contains("sku_name =") && !detail.contains("sku_name ="),
            "SKU 名称可能跨批次有历史别名，只能展示、不能二次缩窄：{} / {detail}",
            hit.sql
        );
        assert!(hit.sql.contains("SUM(in_stock_quantity)"), "{}", hit.sql);
        assert!(
            hit.sql.contains("SUM(lock_quantity)") && hit.sql.contains("SUM(freeze_quantity)"),
            "{}",
            hit.sql
        );
        assert!(
            detail.contains("wms_code") && detail.contains("location") && detail.contains("batch"),
            "{detail}"
        );
        for sql in [&hit.sql, detail] {
            assert!(
                sql.contains("inventory_status = 'ZP'"),
                "正品口径必须全链共享：{sql}"
            );
        }
    }

    #[tokio::test]
    async fn stock_product_inventory_fails_closed_on_none_or_ambiguous_match() {
        let none = product_stock_hit("不存在商品的库存信息", vec![])
            .await
            .expect("零匹配必须返回终止澄清卡");
        for hit in [
            none,
            product_stock_hit(
                "小虎烤肠的库存信息",
                vec![
                    ("SKU-500G", "小虎黑椒味烤肠500G"),
                    ("SKU-1KG", "小虎黑椒味烤肠1KG"),
                ],
            )
            .await
            .expect("多匹配必须返回终止澄清卡"),
        ] {
            assert_eq!(hit.route, "direct-doc");
            assert!(
                matches!(&hit.outcome, DirectOutcome::Clarification(note) if note.contains("商品限定"))
            );
            assert!(hit.sql.is_empty(), "澄清结果不得携带占位 SQL：{}", hit.sql);
        }

        let source = StockSource::failing();
        let principal = dms_policy::Principal {
            employee_id: 1,
            login_name: "admin".into(),
            actual_name: "管理员".into(),
            administrator_flag: true,
            department_id: None,
            role_id: 1,
            role_code: "admin".into(),
        };
        let scope = dms_policy::Scope::new(Default::default(), true);
        let failed = stock_product_filtered(
            "小虎黑椒味烤肠500G的库存信息",
            &source,
            &principal,
            &scope,
            false,
        )
        .await
        .expect("探针失败必须返回终止澄清卡");
        assert!(
            matches!(&failed.outcome, DirectOutcome::Clarification(note) if note.contains("暂时失败"))
        );
        assert!(
            failed.sql.is_empty(),
            "澄清结果不得携带占位 SQL：{}",
            failed.sql
        );
    }

    #[tokio::test]
    async fn stock_product_probe_treats_wildcards_as_literal_text_without_doris_escape() {
        let source = StockSource::new(vec![("SKU-A", "A_50%")]);
        let principal = dms_policy::Principal {
            employee_id: 1,
            login_name: "admin".into(),
            actual_name: "管理员".into(),
            administrator_flag: true,
            department_id: None,
            role_id: 1,
            role_code: "admin".into(),
        };
        let scope = dms_policy::Scope::new(Default::default(), true);
        stock_product_filtered("A_50%的库存信息", &source, &principal, &scope, false)
            .await
            .expect("通配符字符应作为 INSTR 的普通文本探测");
        let probe = source.probes.lock().unwrap().first().cloned().unwrap();
        assert!(probe.contains("INSTR(sku_name, 'A_50%') > 0"), "{probe}");
        assert!(
            !probe.contains("LIKE") && !probe.contains("ESCAPE"),
            "{probe}"
        );
    }

    #[tokio::test]
    async fn stock_product_inventory_rejects_unconsumed_qualifiers() {
        for question in [
            "小虎黑椒味烤肠500G山东仓的库存信息",
            "小虎黑椒味烤肠500G临期库存",
            "小虎黑椒味烤肠500G上月库存",
        ] {
            assert!(
                stock_snapshot(question).is_none(),
                "同步总量模板不许吞掉额外限定：{question}"
            );
            let hit = product_stock_hit(question, vec![("SKU-500G", "小虎黑椒味烤肠500G")])
                .await
                .expect("未兑现限定必须返回终止澄清卡");
            assert!(
                matches!(&hit.outcome, DirectOutcome::Clarification(note) if note.contains("无法兑现")),
                "{question}: {:?}",
                hit.outcome
            );
            assert!(
                hit.sql.is_empty(),
                "澄清结果不得携带占位 SQL：{question}: {}",
                hit.sql
            );
        }
    }

    #[test]
    fn inventory_amount_and_region_questions_stay_on_snapshot_before_product_probe() {
        for question in ["湖南库存金额", "各省份库存金额", "库存金额多少"] {
            let hit = stock_snapshot(question)
                .unwrap_or_else(|| panic!("库存快照应先接住，不能把整句当商品：{question}"));
            assert!(matches!(hit.outcome, DirectOutcome::Data));
            assert!(hit.sql.contains("stock_amount"), "{question}: {}", hit.sql);
            assert!(
                !hit.sql.contains("scm_warehous_manage"),
                "{question}: {}",
                hit.sql
            );
        }
        assert!(
            stock_snapshot("小虎黑椒味烤肠500G的库存信息").is_none(),
            "具体商品必须继续让路给唯一 SKU 探针"
        );

        let source = DETERMINISTIC_SRC;
        let body = body_between(
            source,
            "pub fn direct_hit<'a>",
            "// ─────────── ODS 推导降级",
        );
        let snapshot = body
            .find("stock_snapshot(cx.question)")
            .expect("同步库存模板必须接线");
        let product = body
            .find("stock_product_filtered(cx.question")
            .expect("商品探针必须接线");
        assert!(snapshot < product, "总量/金额/省份库存必须先于商品探针");
    }

    #[test]
    fn account_balance_ranking_uses_latest_customer_snapshot_without_order_join() {
        let hit = balance_ranking("账户余额最高的10个客户").expect("余额排行应走确定性快照模板");
        assert!(hit.sql.contains("PARTITION BY customer_code, balance_type"), "{}", hit.sql);
        assert!(hit.sql.contains("ORDER BY created_time DESC, id DESC"), "{}", hit.sql);
        assert!(hit.sql.contains("WHERE t.rn = 1"), "{}", hit.sql);
        assert!(hit.sql.contains("JOIN t_customer c ON c.customer_code = t.customer_code"), "{}", hit.sql);
        assert!(!hit.sql.contains("t_sales_order"), "余额排行不能经订单表造成扇出：{}", hit.sql);
        assert!(hit.sql.contains("LIMIT 10"), "{}", hit.sql);
        for q in [
            "湖南省账户余额最高的10个客户",
            "430000账户余额最高的10个客户",
            "本月账户余额最高的10个客户",
            "VIP客户账户余额最高的10个客户",
        ] {
            assert!(balance_ranking(q).is_none(), "未实现限定不得被静默丢弃：{q}");
        }

        let src = DETERMINISTIC_SRC;
        let compose = body_between(src, "pub fn compose_hit", "pub fn direct_hit");
        assert!(
            compose.contains("balance_ranking(cx.question).is_some()"),
            "余额排行必须让路给确定性快照模板，不能先被通用组合器抢走"
        );
    }

    #[test]
    fn yesterday_order_customers_use_a_deterministic_detail_query() {
        for q in [
            "昨天下单的有哪些客户",
            "昨天下单的有那些客户",
            "昨天谁下单了",
            "昨天都有谁下过单啊",
            "昨天有哪些客户",
        ] {
            let hit = try_direct(q).unwrap_or_else(|| panic!("应命中客户订单模板：{q}"));
            assert_eq!(hit.route, "direct-doc");
            assert!(hit.sql.contains("o.customer_name AS `客户`"), "{}", hit.sql);
            assert!(hit.sql.contains("COUNT(DISTINCT o.sales_order_code)"), "{}", hit.sql);
            assert!(hit.sql.contains("DATE(o.order_time) = CURDATE() - INTERVAL 1 DAY"), "{}", hit.sql);
        }
        for q in ["昨天新增了哪些客户", "昨天拜访了哪些客户", "昨天有哪些客户欠款"] {
            assert!(sales_order_rows(q).is_none(), "其他客户业务意图不许套销售订单：{q}");
        }
        assert!(sales_order_rows("客户信息").is_none());
    }

    #[test]
    fn mini_program_orders_use_the_dws_snapshot_fact() {
        // 实测错答案的那句：按客户 + 战区 + 本月 + 数量金额，一个限定都不许丢
        let h = try_direct_for("按客户进行展示山东战区本月小程序的下单数量和金额", true)
            .expect("小程序下单应走 DWS 快照模板");
        assert_eq!(h.route, "direct-agg");
        assert!(h.sql.starts_with("-- 小程序下单口径"), "{}", h.sql);
        assert!(h.sql.contains("FROM sales_dw.dws_mkt_app_place_order_dnf"), "{}", h.sql);
        assert!(h.sql.contains(
            "data_date = (SELECT MAX(data_date) FROM sales_dw.dws_mkt_app_place_order_dnf)"),
            "必须按 data_date 取最新快照：{}", h.sql);
        // 探值形态「山东省区」；词干+惯用后缀候选（dimension_probe_values 同一思路）
        assert!(h.sql.contains("region IN ('山东省区','山东战区','山东大区','山东')"), "{}", h.sql);
        assert!(h.sql.contains("SUM(tomonth_order_count) AS `本月下单数量`"), "{}", h.sql);
        assert!(h.sql.contains("SUM(tomonth_amount) AS `本月下单金额`"), "{}", h.sql);
        assert!(h.sql.contains("SUM(tomonth_wxorder_count) AS `本月微信下单数量`"), "{}", h.sql);
        assert!(h.sql.contains("SUM(tomonth_wxorder_amount) AS `本月微信下单金额`"), "{}", h.sql);
        assert!(h.sql.contains("SUM(tomonth_zyorder_count) AS `本月账余下单数量`"), "{}", h.sql);
        assert!(h.sql.contains("SUM(tomonth_zyorder_amount) AS `本月账余下单金额`"), "{}", h.sql);
        assert!(h.sql.contains("MAX(data_date) AS `数据日期`"), "快照日必须透出：{}", h.sql);
        assert!(h.sql.contains("GROUP BY store_code, store_name"), "{}", h.sql);
        assert!(h.sql.contains("ORDER BY `本月下单金额` DESC LIMIT 200"), "{}", h.sql);
        assert!(!h.sql.contains("today_order_count"), "当月问句不许混当日列：{}", h.sql);

        // 标量形态（无「按客户」）：单行合计，不 GROUP BY
        let s = try_direct_for("本月小程序下单数量和金额", true).expect("标量小程序下单");
        assert_eq!(s.route, "direct-agg");
        assert!(s.sql.contains("SUM(tomonth_order_count) AS `本月下单数量`"), "{}", s.sql);
        assert!(s.sql.contains("SUM(tomonth_amount) AS `本月下单金额`"), "{}", s.sql);
        assert!(!s.sql.contains("GROUP BY"), "{}", s.sql);

        // 今天 → today_* 列族；今日账余列的物理拼写就是 todaty_（原样照抄，钉住）
        let t = mini_program_order_agg("今天小程序下单数量").expect("今天应走当日列族");
        assert!(t.sql.contains("SUM(today_order_count) AS `今日下单数量`"), "{}", t.sql);
        assert!(!t.sql.contains("tomonth_"), "当日问句不许混月累计列：{}", t.sql);
        let zy = mini_program_order_agg("今天小程序账余下单").expect("账余列族");
        assert!(zy.sql.contains("SUM(todaty_zyorder_count)"), "todaty_ 是物理拼写：{}", zy.sql);
        // 微信支付/取消列族；缺省时间词 → 当月累计并透出快照日
        let wx = mini_program_order_agg("本月小程序微信下单金额").expect("微信支付列族");
        assert!(wx.sql.contains("SUM(tomonth_wxorder_amount) AS `本月微信下单金额`"), "{}", wx.sql);
        assert!(!wx.sql.contains("tomonth_order_count"), "只问微信支付不许带总下单列：{}", wx.sql);
        let c = mini_program_order_agg("本月小程序取消订单数").expect("取消列族");
        assert!(c.sql.contains("SUM(tomonth_cancel_order) AS `本月取消订单数`"), "{}", c.sql);
        let d = mini_program_order_agg("小程序下单金额").expect("缺省时间按当月累计");
        assert!(d.sql.contains("SUM(tomonth_amount)"), "{}", d.sql);
        assert!(d.sql.contains("MAX(data_date) AS `数据日期`"), "{}", d.sql);

        // 兑现不了的一律不接（让位 LLM，不许静默丢限定）
        for q in [
            "昨天小程序下单金额",   // 快照表没有「昨天」列
            "上月小程序下单数量",   // 没有「上月」列
            "山东战区和江苏战区本月小程序下单金额", // 多区域值
            "华北战区本月小程序下单金额",  // 非省名词干，探值表里没有
            "本月小程序下单金额按商品",   // 商品维度兑现不了
            "本月小程序取消订单金额",    // 取消只有单数列，没有金额列
            "小程序商城",           // 不是下单问句
            "本月小程序订单",         // 无指标词（明细/聚合不明）
        ] {
            assert!(mini_program_order_agg(q).is_none(), "兑现不了的不许接：{q}");
        }
        // 业务 MySQL 源没有这张数仓表：整条链都不许接（落 LLM）
        assert!(try_direct_for("本月小程序下单数量和金额", false).is_none(), "非数仓源不接小程序下单");

        // 组合器让路门（源码钉，同 balance_ranking 那条）：小程序问句不许被注册表装配劫走
        let src = DETERMINISTIC_SRC;
        let compose = body_between(src, "pub fn compose_hit", "pub fn direct_hit");
        assert!(
            compose.contains("mini_program_order_agg(cx.question).is_some()"),
            "小程序下单必须进 compose 让路门，不许被装配成丢限定的 SQL"
        );
    }

    #[test]
    fn mini_program_war_zone_wording_discloses_region_caliber() {
        // 问句点名「战区」：口径注释必须明示该表无战区字段、按省区（region）统计 ——
        // 不许静默拿 region 冒充战区
        let h = mini_program_order_agg("按客户进行展示山东战区本月小程序的下单数量和金额")
            .expect("战区问句应走快照模板");
        assert!(h.sql.contains("该表无「战区」字段，按省区（region）统计"), "{}", h.sql);
        assert!(!h.sql.contains("war_zone"), "该表无战区列，SQL 里不许出现：{}", h.sql);
        // 没点名的问句不带这句（注释不刷屏）
        let s = mini_program_order_agg("本月小程序下单数量和金额").expect("标量小程序下单");
        assert!(!s.sql.contains("该表无「战区」字段"), "{}", s.sql);
    }

    #[test]
    fn sales_order_rows_narrows_channel_and_region_qualifiers() {
        // 「小程序」两个分支都不许接：t_sales_order 全表 source_platform_code='DMS'，
        // 没有渠道过滤能力，接了就是静默丢限定
        for q in [
            "昨天小程序下单的客户有哪些",
            "昨天小程序订单明细",
            "按客户进行展示山东战区本月小程序的下单数量和金额",
        ] {
            assert!(sales_order_rows(q).is_none(), "含小程序的问句不许套 t_sales_order：{q}");
        }
        // 战区/省区限定值已探值（province_department_name 存「山东战区/山东省区」）→ 补等值谓词
        let h = sales_order_rows("山东战区昨天有哪些客户下单").expect("战区限定应补谓词后接");
        assert!(h.sql.contains("o.province_department_name = '山东战区'"), "{}", h.sql);
        assert!(h.sql.contains("DATE(o.order_time) = CURDATE() - INTERVAL 1 DAY"), "{}", h.sql);
        let d = sales_order_rows("山东省区昨天销售订单明细").expect("省区限定应补谓词后接");
        assert!(d.sql.contains("o.province_department_name = '山东省区'"), "{}", d.sql);
        let shanghai = sales_order_rows("上海省区昨天有哪些客户下单")
            .expect("上海行政词必须落到生产业务省区");
        assert!(shanghai.sql.contains("o.province_department_name = '浙江省区'"), "{}", shanghai.sql);
        assert!(!shanghai.sql.contains("o.province_department_name = '上海省区'"), "{}", shanghai.sql);
        let hainan = sales_order_rows("海南省区昨天有哪些客户下单")
            .expect("海南行政词必须落到生产业务省区");
        assert!(hainan.sql.contains("o.province_department_name = '广东省区'"), "{}", hainan.sql);
        assert!(!hainan.sql.contains("o.province_department_name = '海南省区'"), "{}", hainan.sql);
        // 兑现不了的区域限定 → 不接（让位，不许静默丢）
        for q in [
            "山东战区和江苏战区昨天有哪些客户下单", // 多值
            "华北战区昨天有哪些客户下单",          // 非省名词干
            "各省区昨天有哪些客户下单",            // 分组问法，本模板表达不了
        ] {
            assert!(sales_order_rows(q).is_none(), "区域限定兑现不了不许静默丢：{q}");
        }
        // 老行为一个字不变：无区域限定的问句不多任何一个字符
        let old = sales_order_rows("昨天下单的有哪些客户").expect("老问句照旧接");
        assert!(!old.sql.contains("province_department_name"), "{}", old.sql);
        assert!(
            old.sql.contains("AND DATE(o.order_time) = CURDATE() - INTERVAL 1 DAY GROUP BY"),
            "{}", old.sql);
        // 「昨天+小程序」两个模板都兑现不了（快照表没有昨日列）→ 整链不接、落 LLM 路，
        // 不许变成新的「不可计算」卡，更不许被 sales_order_rows 静默丢限定后接走
        assert!(
            try_direct_for("昨天小程序下单的客户有哪些", true).is_none(),
            "兑现不了的时间词必须让位 LLM"
        );
    }

    #[test]
    fn device_order_term_maps_to_so04_business_document() {
        let h = try_direct("查询下昨天的设备订单").expect("设备订单应走确定性业务模板");
        assert_eq!(h.route, "direct-doc");
        assert!(h.sql.contains("order_type = 'SO04'"), "{}", h.sql);
        assert!(h.sql.contains("DATE(order_time) = CURDATE() - INTERVAL 1 DAY"), "{}", h.sql);
        assert!(h.sql.contains("source_code AS `设备需求单号`"), "{}", h.sql);

        let count = try_direct("昨天设备订单有多少").unwrap().sql;
        assert!(count.contains("COUNT(DISTINCT sales_order_code) AS `设备订单数`"), "{count}");

        let customer = try_direct("昨天设备订单按客户").unwrap().sql;
        assert!(customer.contains("GROUP BY customer_name"), "{customer}");

        let status = try_direct("昨天设备订单按状态").unwrap().sql;
        assert!(status.contains("WHEN '101' THEN '待备货 (101)'"), "{status}");
        assert!(status.contains("WHEN '104' THEN '交易完成 (104)'"), "{status}");
        assert!(status.contains("ELSE CONCAT('未知状态 ('"), "{status}");

        assert!(h.sql.contains("待备货 (101)"), "明细也必须解码状态：{}", h.sql);

        let composition = try_direct("昨天设备订单按设备类型").unwrap().sql;
        assert!(composition.contains("FROM t_sales_order o"), "{composition}");
        assert!(composition.contains("order_type = 'SO04'"), "{composition}");
        assert!(composition.contains("item_type = '1'"), "{composition}");
        assert!(composition.contains("SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity"), "{composition}");
        assert!(composition.contains("LEFT JOIN dim.dim_device"), "{composition}");
        assert!(composition.contains("DATE(o.order_time) = CURDATE() - INTERVAL 1 DAY"), "{composition}");
        assert!(composition.contains("SUM(x.box_quantity) AS `设备数量`"), "{composition}");

        let devices = try_direct("昨天设备订单按设备名称").unwrap().sql;
        assert!(devices.contains("GROUP BY x.sku_name"), "{devices}");
        assert!(try_direct("昨天有哪些设备").is_none(), "泛设备名词不能误认成设备订单");
    }

    /// 「最高/最多」是 N=1 的取值限定，不是「给我整张榜」。
    ///
    /// 🔴 由来（2026-08-15 生产直打 + 复验 2/2）：「本月销售额最高的客户」返回 200 行全榜，
    /// 确定性摘要还把第一名标成「榜首」推给用户。同一个引擎对「前十」严格落 LIMIT 10。
    #[test]
    fn a_superlative_means_one_row_not_the_whole_board() {
        for q in ["本月销售额最高的客户", "哪个客户本月销售额最高", "本月销量最低的商品"] {
            let sql = warehouse_sales_fact(q).unwrap_or_else(|| panic!("{q} 该命中")).sql;
            assert!(sql.contains("LIMIT 1"), "{q} 该只要一行：{sql}");
        }
        // 显式 N 照旧；「排行/排名」用户要的就是一张榜，不许被这条改成 1 行
        let five = warehouse_sales_fact("本月销量最低的5个商品").expect("该命中").sql;
        assert!(five.contains("LIMIT 5"), "{five}");
        let board = warehouse_sales_fact("本月各客户销售额排行").expect("该命中").sql;
        assert!(board.contains("LIMIT 200"), "{board}");
    }

    /// 纯数字客户编码抽到了就必须落进 WHERE。
    ///
    /// 🔴 由来（2026-08-15 生产直打 + 对抗复验 4/4）：
    ///   「180135本月销售额」客户限定一个字都没进 SQL，答全公司 6.34 亿（真值 7.2 万，约 8800 倍）；
    ///   「客户编码180135的本月销售额」收据里明写 `filter:客户编码=180135`，SQL 里却没有 storecode。
    #[test]
    fn a_lone_six_digit_code_becomes_a_customer_filter() {
        for q in ["180135本月销售额", "客户180135本月销售额", "客户编码180135的本月销售额"] {
            let sql = warehouse_sales_fact(q).unwrap_or_else(|| panic!("{q} 该命中")).sql;
            assert!(sql.contains("'180135'"), "{q} 的客户编码没进 SQL：{sql}");
            // 时间也不许被编码末位劫持（同一族的另一半，判据在 kernel/nl/time.rs）
            assert!(sql.contains("DATE_FORMAT(CURDATE(),'%Y-%m-01')"), "{q} 的本月被换掉了：{sql}");
        }
        // 不是「恰好一段 6 位」的一律不认：宁可不认也不猜
        assert!(crate::answerers::fastpath_intent::lone_customer_code("2026年6月销售额").is_none());
        assert!(crate::answerers::fastpath_intent::lone_customer_code("查 HJXH-DSO2026080300838").is_none());
        assert!(crate::answerers::fastpath_intent::lone_customer_code("180135 和 180157 本月销售额").is_none());
        assert_eq!(crate::answerers::fastpath_intent::lone_customer_code("客户180135本月销售额").as_deref(), Some("180135"));
    }

    #[test]
    fn warehouse_finance_uses_available_facts_and_never_invents_invoice_tables() {
        let cost = try_direct_for("本月市场费用花了多少", true).expect("数仓费用快路径");
        assert_eq!(cost.route, "direct-agg");
        assert!(cost.sql.contains("sales_ads.ads_off_sales_cost_customer_dnf"), "{}", cost.sql);
        assert!(cost.sql.contains("f.data_month"), "{}", cost.sql);
        assert!(cost.detail.as_deref().is_some_and(|sql| sql.contains("费用分类")));

        let invoice = try_direct_for("本月专票开了多少金额", true).expect("缺事实必须明确降级");
        assert_eq!(invoice.route, "direct-doc");
        assert!(invoice.sql.contains("'不可计算' AS `数据状态`"), "{}", invoice.sql);
        assert!(invoice.sql.contains("FROM dms_ods.t_dict_value LIMIT 1"), "{}", invoice.sql);
        assert!(!invoice.sql.contains("t_invoice_"), "{}", invoice.sql);

        let account_bill =
            try_direct_for("待确认对账单有多少", true).expect("缺对账事实必须明确降级");
        assert_eq!(account_bill.route, "direct-doc");
        assert!(account_bill.sql.contains("'不可计算' AS `数据状态`"), "{}", account_bill.sql);
        assert!(account_bill.sql.contains("禁止用费用报销或其他相似表替代"), "{}", account_bill.sql);

        let top = try_direct_for("本月市场费用最高的5项", true).expect("费用排行应直接返回分类");
        assert_eq!(top.route, "direct-agg");
        assert!(top.sql.contains("AS `费用分类`"), "{}", top.sql);
        assert!(top.sql.contains("ORDER BY `市场费用` DESC LIMIT 5"), "{}", top.sql);
        assert!(top.detail.is_none(), "排行的主结果已经是费用分类，不应再附重复明细");

        assert!(warehouse_finance("本月开票余额").is_none(), "开票余额不是已开票金额");
        assert!(try_direct_for("本月市场费用花了多少", false).is_none(), "MySQL 源保留原语义层路径");
    }

    #[test]
    fn warehouse_sales_uses_the_shared_fact_contract_and_rejects_mysql_aggregation() {
        use dms_semantic::sales_fact;

        assert!(warehouse_sales_fact("本月各商品分类销量").is_none());
        assert!(warehouse_sales_fact("2026年6月销量最高的5个商品分类是哪些").is_none());

        let sale14 = try_direct_for("今年每个月的销售额趋势", true)
            .expect("SALE14 应走 DWS 月度趋势");
        assert!(sale14.sql.contains("DATE_FORMAT(sf.order_date,'%Y-%m') AS `月份`"), "{}", sale14.sql);
        assert!(sale14.sql.contains("COALESCE(SUM(sf.amount),0) AS `销售额`"), "{}", sale14.sql);
        assert!(sale14.sql.contains("sf.order_date < DATE_ADD(CURDATE(), INTERVAL 1 DAY)"), "{}", sale14.sql);
        assert!(!sale14.sql.contains("sf.order_date < CURDATE()"), "DWS 不得继承发货截止昨天口径：{}", sale14.sql);
        assert!(sale14.sql.contains("ORDER BY DATE_FORMAT(sf.order_date,'%Y-%m') ASC"), "{}", sale14.sql);

        let today = try_direct_for("今天销售额是多少", true).expect("今天应走完整自然日窗口");
        assert!(today.sql.contains("sf.order_date >= CURDATE()"), "{}", today.sql);
        assert!(today.sql.contains("sf.order_date < DATE_ADD(CURDATE(), INTERVAL 1 DAY)"), "{}", today.sql);
        assert!(!today.sql.contains("sf.order_date >= CURDATE() AND sf.order_date < CURDATE()"),
                "DWS 今天窗口不得为空：{}", today.sql);

        for (question, fragment) in [
            ("本月销售额是多少", "SUM(sf.amount)"),
            ("本月销量是多少", "SUM(sf.qty)"),
            ("本月不含税成本是多少", "SUM(sf.cost_excluding_tax)"),
            ("本月不含税收入是多少", "SUM(sf.revenue_excluding_tax)"),
            ("本月毛利额是多少", "SUM(sf.gross_profit)"),
            (
                "本月毛利率是多少",
                "SUM(sf.gross_profit)/NULLIF(SUM(sf.revenue_excluding_tax),0)",
            ),
        ] {
            let hit = try_direct_for(question, true).unwrap_or_else(|| panic!("未命中共享事实：{question}"));
            assert!(hit.sql.contains(sales_fact::TABLE), "{question}: {}", hit.sql);
            assert!(hit.sql.contains(fragment), "{question}: {}", hit.sql);
            for forbidden in [
                " JOIN ", "UNION ALL", "t_sales_order", "t_sales_order_detail",
                "t_after_sales_order_detail", "t_order_logistics",
            ] {
                assert!(!hit.sql.contains(forbidden),
                    "默认销售经营指标不得读取旧事实 {forbidden}: {question}: {}", hit.sql);
            }
            assert!(agg_template(question).is_none(), "默认销售指标不得回退订单模板：{question}");
            let unavailable = try_direct_for(question, false)
                .unwrap_or_else(|| panic!("业务 MySQL 应明确拒绝默认销售指标：{question}"));
            assert_eq!(unavailable.route, "direct-doc");
            assert!(unavailable.sql.contains("'不可计算' AS `数据状态`"), "{}", unavailable.sql);
            assert!(!unavailable.sql.contains(" JOIN ")
                && !["t_sales_order", "t_after_sales", "t_order_logistics", "t_customer", "t_goods"]
                    .iter().any(|t| unavailable.sql.contains(t)),
                "业务 MySQL 失败关闭不得读业务表：{}", unavailable.sql);
        }

        let scalar = try_direct_for("本月销售额是多少", true).expect("标量销售额");
        let detail = scalar.detail.as_deref().expect("标量销售额必须附 DWS 固定明细");
        for projection in [
            "sf.order_date AS `日期`",
            "sf.storecode AS `客户编码`",
            "sf.storename AS `客户名称`",
            "sf.skucode AS `商品编码`",
            "sf.skuname AS `商品名称`",
            "sf.war_zone AS `战区`",
            "sf.region AS `省区`",
            "sf.cost_excluding_tax AS `不含税成本`",
            "sf.revenue_excluding_tax AS `不含税收入`",
            "sf.gross_profit AS `毛利额`",
        ] {
            assert!(detail.contains(projection), "缺少固定明细列 {projection}: {detail}");
        }
        assert!(detail.contains(sales_fact::TABLE), "{detail}");
        assert!(!detail.contains("SELECT *") && !detail.contains(" JOIN "), "明细不得自由扩表：{detail}");

        let customer = try_direct_for("本月销售额按客户", true).expect("storename 是客户维度");
        assert!(customer.sql.contains("sf.storename") && customer.sql.contains("AS `客户`"), "{}", customer.sql);
        assert!(!customer.sql.contains("AS `门店`") && !customer.sql.contains("shop"), "{}", customer.sql);
        let goods = try_direct_for("本月销量按商品", true).expect("skuname 是商品维度");
        assert!(goods.sql.contains("sf.skuname") && goods.sql.contains("AS `商品`"), "{}", goods.sql);
        let region = try_direct_for("本月毛利率按省区", true).expect("region 是省区维度");
        assert!(region.sql.contains("sf.region") && region.sql.contains("AS `省区`"), "{}", region.sql);
        assert!(region.sql.contains("SUM(sf.gross_profit)/NULLIF(SUM(sf.revenue_excluding_tax),0)"), "{}", region.sql);
        assert!(!region.sql.contains("province") && !region.sql.contains("state"), "{}", region.sql);
        let returns = try_direct_for("本月退货销售额", true)
            .expect("退货销售未确认时必须明确失败关闭");
        assert!(returns.sql.contains("'不可计算' AS `数据状态`"), "{}", returns.sql);
        assert!(returns.sql.contains("'退货' AS `未确认范围`"), "{}", returns.sql);
        assert!(!returns.sql.contains("t_after_sales_order") && !returns.sql.contains("UNION ALL"),
                "退货销售不得复活旧售后 UNION：{}", returns.sql);

        for event in ["本月订单销售额", "本月发货销售额", "本月出库销售额", "本月应收销售额"] {
            let unavailable = try_direct_for(event, true)
                .unwrap_or_else(|| panic!("明确事件销售额必须失败关闭：{event}"));
            assert!(unavailable.sql.contains("'不可计算' AS `数据状态`"),
                    "{event}: {}", unavailable.sql);
            assert!(!unavailable.sql.contains("t_sales_order") && !unavailable.sql.contains(" JOIN "),
                    "{event} 不得回退旧订单事实：{}", unavailable.sql);
        }

        for unsupported in [
            "本月销售额按品牌",
            "本月订单数",
            "本月销售额按门店",
            "本月业务员销售额",
            "本月区域经理业绩",
            "本月销售额按业务员ID",
        ] {
            assert!(warehouse_sales_fact(unsupported).is_none(), "DWS 不具备该事实：{unsupported}");
        }
        assert!(warehouse_sales_question("本月订单数"), "订单数必须拦住旧数仓聚合链");

        for warehouse in [false, true] {
            let unavailable = try_direct_for("本月销售额按门店", warehouse)
                .expect("未确认门店维度必须明确失败关闭");
            assert_eq!(unavailable.route, "direct-doc");
            assert!(unavailable.sql.contains("'门店' AS `未确认范围`"), "{}", unavailable.sql);
            assert!(!unavailable.sql.contains(" JOIN ")
                && !["t_sales_order", "t_after_sales", "t_order_logistics", "t_customer", "t_goods"]
                    .iter().any(|t| unavailable.sql.contains(t)),
                "未确认维度不得读业务表或 JOIN 旧事实：{}", unavailable.sql);
        }

        let source = DETERMINISTIC_SRC;
        for duplicate in [
            concat!("DWS_", "OFFLINE_SALE"),
            concat!("Dws", "Metric"),
            concat!("Dws", "Dim"),
        ] {
            assert!(!source.contains(duplicate), "direct.rs 不得复制事实合同：{duplicate}");
        }
        assert!(!source.contains(concat!("sales_dw.dws_offline", "_sale_dfn")),
                "物理事实表名只能存在于 dms_semantic::sales_fact");
        assert!(!source.contains(concat!("fn ship_", "sql")), "旧发货 SQL 构造器不得回归");
        assert!(!source.contains(concat!("struct Ship", "Dim")), "旧发货维度类型不得回归");
        // T8：`sales_fact_unavailable` 已迁 `dms_semantic::fastpath::sales`，
        // `warehouse_sales_semantics_unavailable` 迁 agent —— 判据的输入必须跟着搬，
        // 否则切段切在空气上、断言恒真（本仓反复抓到的那类缺陷）。
        let sales_src = include_str!("../../../semantic/src/fastpath/sales.rs");
        let unavailable = body_between(sales_src, "fn sales_fact_unavailable(", "
pub fn ");
        assert!(!unavailable.contains(" JOIN "),
                "失败关闭不得 JOIN 任何表：{unavailable}");
        // 纯常量投影过不了闸门的 ConstantProjection 防线；`dms_ods.t_dict_value LIMIT 1`
        // 是开票/对账不可计算卡已在用的唯一占位形态（不读业务行，只取常量）。
        assert!(unavailable.contains(" FROM dms_ods.t_dict_value LIMIT 1"),
                "失败关闭只允许字典占位 FROM：{unavailable}");
    }

    /// 🔴 无维度模式（指标 only）的 SQL 形状。
    ///
    /// 它服务的是实测出来的最大一档缺口：`why-not-compose` 诊断 38 题，
    /// **② 维度不命中 17 题** —— `try_compose` 强制要维度，而无维度这条路今天只有
    /// 一个硬编码模板、且只认 4 个指标。这里钉住四件：
    /// 不出维度列、不 GROUP BY、不 ORDER BY/LIMIT（单行结果排序无意义）、
    /// 以及**去重子查询与表级口径照旧生效**（那是数值对不对的关键，不能因为少了维度就丢）。
    #[test]
    fn metric_only_mode_shape() {
        let nodim = DimDef {
            name: String::new(),
            aliases: vec![],
            source_table: "t_sales_order_detail b0".into(),
            expr: String::new(),
        };
        let sql = compose_sql_with(&qty_metric(), &nodim, "本月销量", &edges(), &scopes()).unwrap();
        assert!(!sql.contains("GROUP BY"), "无维度不许 GROUP BY：{sql}");
        assert!(!sql.contains("ORDER BY") && !sql.contains("LIMIT"), "单行不需要排序/限流：{sql}");
        assert!(sql.starts_with("SELECT SUM("), "只出一个指标列：{sql}");
        // 去重子查询仍在（销量的 dedup_keys 非空）——少了它数值直接虚增
        assert!(sql.contains("SELECT DISTINCT"), "去重装配丢了：{sql}");
        // 时间桥接到订单头 + 表级口径（有效订单）仍在
        assert!(sql.contains("t_sales_order o_time"), "时间桥接丢了：{sql}");
        assert!(sql.contains("order_status NOT IN"), "订单头表级口径丢了：{sql}");
    }

    /// 🔴 时间窗按**指标声明的 `time_col`** 放，而不是写死订单头。
    ///
    /// 缺陷现场：`compose_sql_with` 的时间窗原先写死 `t_sales_order` / `order_time` ——
    /// 在 FROM 里找不到订单头就试着桥一条边，桥不到就**整条不装配**。于是时间语义不在订单头上的
    /// 指标（售后单数 `after_sales_time`、开票金额、动销商品数）一律回落 LLM，
    /// 而声明里明明写着该用哪一列。`why-not-compose` 逐题诊断出这是「指标 only 也不接」的主因。
    ///
    /// 两个方向都要钉：声明为别的列 → 用它；声明为 `order_time` → **保持桥接老路**
    /// （明细类指标的 `order_time` 确实在订单头上，那条 JOIN 不可省 ——
    /// 漏了它连「有效订单」这条表级口径一起丢，那是数值虚增的头号来源）。
    #[test]
    fn time_window_follows_declared_time_col() {
        let nodim = |t: &str| DimDef {
            name: String::new(),
            aliases: vec![],
            source_table: format!("{t} b0"),
            expr: String::new(),
        };
        // ① 声明 after_sales_time → 直接放在指标基表上，不去桥订单头
        let as_metric = MetricDef {
            name: "售后单数".into(),
            aliases: vec!["售后单".into()],
            source_table: "t_after_sales_order_header".into(),
            agg_expr: "COUNT(DISTINCT after_sales_code)".into(),
            scope_filter: "deleted_flag = 0".into(),
            dedup_keys: String::new(),
            time_col: "after_sales_time".into(),
        };
        let sql = compose_sql_with(
            &as_metric,
            &nodim("t_after_sales_order_header"),
            "本月售后单有多少",
            &edges(),
            &scopes(),
        )
        .unwrap();
        assert!(sql.contains("b0.after_sales_time"), "没按声明的时间列放：{sql}");
        assert!(!sql.contains("t_sales_order"), "不该去桥订单头：{sql}");
        // ② 声明 order_time 的明细类指标：桥接照旧（连带订单头的表级口径）
        let sql2 =
            compose_sql_with(&qty_metric(), &nodim("t_sales_order_detail"), "本月销量", &edges(), &scopes())
                .unwrap();
        assert!(sql2.contains("t_sales_order o_time"), "订单头桥接被顶掉了：{sql2}");
        assert!(sql2.contains("order_status NOT IN"), "有效订单口径跟着丢了：{sql2}");
    }

    /// 🔴 装配器出 KPI 环比：上期 SQL 与当期**只差时间窗**，别的一个字不许变。
    ///
    /// 这一条消掉的是让路门的**第二条**理由（「指标 only 不出环比，换过去会静默丢功能」）。
    /// 判据必须钉「只差时间窗」：若上期那次重装配换掉了别的东西（口径、去重、JOIN），
    /// Δ% 就是拿两个口径不同的数相除 —— 那种错比没有环比更坏（它看着像个结论）。
    #[test]
    fn composer_prev_differs_only_in_the_time_window() {
        let nodim = DimDef {
            name: String::new(),
            aliases: vec![],
            source_table: "t_sales_order b0".into(),
            expr: String::new(),
        };
        let m = sales_metric();
        let cur = compose_sql_with_snap(&m, &nodim, "本月销售额", &edges(), &scopes(), None, None, &[])
            .unwrap();
        let (tpl, label) = prev_window("本月销售额").expect("本月必须有上期");
        let prev =
            compose_sql_with_snap(&m, &nodim, "本月销售额", &edges(), &scopes(), None, Some(tpl), &[])
                .unwrap();
        assert_eq!(label, "较上月");
        assert_ne!(cur, prev, "上期与当期不能是同一条 SQL");
        // 只差时间窗：把两条 SQL 里的时间谓词段抹掉后必须逐字相同
        let strip = |s: &str| {
            let i = s.find("AND b0.order_time").or_else(|| s.find("AND order_time"));
            match i {
                Some(i) => s[..i].to_string(),
                None => s.to_string(),
            }
        };
        assert_eq!(strip(&cur), strip(&prev), "除时间窗外还有别的差异：\n{cur}\n---\n{prev}");
        // 当期含本月起点、上期含上月起点（方向不许反）
        assert!(cur.contains("DATE_FORMAT(CURDATE(),'%Y-%m-01')"), "{cur}");
        assert!(prev.contains("INTERVAL 1 MONTH"), "{prev}");
    }

    /// 🔴 让路门必须管住**带维度那条路**，不是只管指标 only。
    ///
    /// 这条钉的是我自己引入又当场抓到的回归：给「成交客户数」补指标声明后，
    /// 「本月成交客户数」被 `try_compose` 装配成**按客户分组的客户数**（200 行、每行 1）——
    /// 因为 `pick(dims)` 被「成交客户**数**」里的「客户」命中维度「客户」，
    /// 而残留守卫剥完指标名+维度名后正好为空，一路绿灯。
    /// **route 仍是 `direct-agg`，只断言路由的测试看不出来**（回归 A09/A12 正是只断言路由）。
    #[test]
    fn yield_gate_covers_the_dimension_path_too() {
        let buyer = buyer_metric();
        let cust = dim("客户", "COALESCE(o.customer_name,'未知')");
        // ① **没有让路门时它真的会装配** —— 这一句是本测试的价值所在：
        //    证明那道门是承重的，而不是一句多余的保险。
        let bad =
            compose_gated(&buyer, &cust, "本月成交客户数", &edges(), &scopes(), &[], &[]).expect(
                "前提：不让路的话这句会被装配成「按客户分组的客户数」",
            );
        assert!(bad.contains("GROUP BY"), "{bad}");
        // ② 让路门的判据：`agg_template` 接得住 → compose 一律退出
        assert!(agg_template("本月成交客户数").is_some(), "让路门的前提没了");
        // ③ 反面：带维度词的问句 `agg_template` 自己就拒（DIM_WORDS 门）→ 不会误让路
        assert!(agg_template("本月各省成交客户数").is_none(), "带维度词不该被模板接走");
        // ④ 默认销售额已经退出业务 MySQL 模板；DWS 路径由 `warehouse_sales_fact`
        //    抢在注册表组合器前处理，业务源不允许靠 `try_direct` 生成旧销售 SQL。
        //    省份已并入省区（region，2026-08-11 业务裁决）：DWS 路径必须接住而非回落。
        assert!(try_direct("本月销售额按省份").is_none());
        assert!(warehouse_sales_fact("本月销售额按省份").is_some(), "省份=省区（region），DWS 路径必须接住");
        assert!(agg_template("本月销售额按省份").is_none());
    }

    /// 🔴 硬编码模板能接的，指标 only **必须让路**。
    ///
    /// Router 里 `direct-agg` 排在 `direct-doc`（`agg_template`）之前，不让路就会：
    /// ① 把「本月销售额」的数从订单头 `SUM(total_amount)` 换成明细声明那一套 ——
    ///    而两者差多少正是 `item_type` 那件**未裁决**的事（二·J′ 记的 204.5M/208.1M/131.4M）；
    /// ② 丢掉 KPI 环比（指标 only 不出上期查询）。
    /// 两条都不会报错，只会静默变数/少功能。这条断言就是那道让路门。
    #[test]
    fn metric_only_yields_to_hardcoded_templates() {
        for q in ["本月客单价", "本月订单数", "本月成交客户数"] {
            assert!(agg_template(q).is_some(), "前提：这些本来由 agg_template 接：{q}");
        }
        assert!(agg_template("本月销售额是多少").is_none(), "默认销售额必须交给 DWS 事实");
        // 反面 ①：指标不在模板的四个里 → 让指标 only 接
        assert!(agg_template("本月开票金额").is_none(), "开票金额不该被硬编码模板接");
        // 反面 ②：**同一个指标、换个说法模板就不接了** —— 剥词表里有「订单数」没有「订单」，
        // 于是「本月有多少个订单」剩下「个订单」被残留守卫拦掉。
        // 这一条不是缺陷、是模板的固有窄面：它按字面词表工作，而声明层按名/别名工作。
        // 指标 only 正好补这个面（同一个「订单数」声明，两种说法都能接）。
        assert!(agg_template("本月有多少个订单").is_none(), "模板按字面词表工作，这句它接不了");
    }

    /// 🔴 带维度时**不许**走无维度模式：用户要了分组却拿到单值是答非所问。
    /// 两条路本来不重叠（入口自己判），这里钉的是「顺序写反不会报错、只会静默丢分组」。
    #[test]
    fn metric_only_keeps_group_by_when_dim_present() {
        let sql =
            compose_sql_with(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges(), &scopes())
                .unwrap();
        assert!(sql.contains("GROUP BY"), "有维度必须分组：{sql}");
    }

    #[test]
    fn doc_prefixes() {
        use dms_semantic::document::resolve_code;
        for (code, table) in [
            ("HJXH-DXO2026072300384", "t_sales_order"),
            ("HJXH-DRO2026072300047", "t_after_sales_order_header"),
            ("HJXH-DZD20261230000261", "t_account_bill_header"),
            ("SPC-20260718-8", "t_winc_purchase_transfer"),
            ("HJXH_XQ20260101001", "t_device_requisition"),
            ("DEV_XQ202608040001", "t_device_requisition"),
            ("IO2025123456", "t_invoice_apply_header"),
            ("SQ2026052345", "t_invoice_new_apply_header"),
            ("CG2603090123", "t_winc_purchase_transfer"),
        ] {
            assert_eq!(resolve_code(code, false).unwrap().family.header_table, table, "{code}");
        }
        for bad in ["HJXH-XXX123", "INVOICE2", "IO1234"] {
            assert!(resolve_code(bad, false).is_none(), "{bad}");
        }
    }

    #[test]
    fn sniff_in_sentence() {
        let h = sniff_doc_code("帮我查下 HJXH-DXO2026072300384 这张单", false).unwrap();
        assert!(h.sql.contains("t_sales_order"));
        assert!(h.sql.contains("HJXH-DXO2026072300384"));
        assert_eq!(h.route, "direct-doc");
    }

    #[test]
    fn month_sales_uses_dws_fact_not_the_order_template() {
        let h = warehouse_sales_fact("本月销售额是多少").unwrap();
        assert!(h.sql.contains(dms_semantic::sales_fact::TABLE), "{}", h.sql);
        assert!(h.sql.contains("COALESCE(SUM(sf.amount),0) AS `销售额`"), "{}", h.sql);
        assert!(h.sql.contains("sf.order_date >= DATE_FORMAT(CURDATE(),'%Y-%m-01')"), "{}", h.sql);
        assert!(!h.sql.contains("UNION ALL") && !h.sql.contains("t_sales_order_logistics"), "{}", h.sql);
        assert!(agg_template("本月销售额是多少").is_none());
        assert_eq!(h.route, "direct-agg");
    }

    #[test]
    fn invalid_model_output_can_only_recover_questions_the_sales_fact_fully_accepts() {
        for question in [
            "本月销售额是多少",
            "湖南省 2026-08-10 至 2026-08-12 销售额",
        ] {
            let attempt = recover_sales_intent(question, true)
                .unwrap_or_else(|| panic!("明确销售事实问句应恢复：{question}"));
            assert_eq!(attempt.route(), crate::intent::IntentRoute::Data);
        }
        for ambiguous in [
            "山东省和江苏省本月销售额",
            "嗨肉本月销售额",
            "本月退货销售额",
            "销售额按门店",
        ] {
            assert!(
                recover_sales_intent(ambiguous, true).is_none(),
                "不能越过确定性快路径的 fail-closed 门：{ambiguous}"
            );
        }
        assert!(recover_sales_intent("本月销售额是多少", false).is_none());
    }

    #[test]
    fn weekly_report_primary_sales_scopes_the_unique_province_everywhere() {
        let question = "山东省 2026-08-10 至 2026-08-11 销售额";
        let h = warehouse_sales_fact(question).expect("周报主查询应命中 DWS 销售事实快路径");
        assert_eq!(h.route, "direct-agg");
        assert!(h.sql.contains(dms_semantic::sales_fact::TABLE), "{}", h.sql);
        assert!(
            h.sql.contains("COALESCE(SUM(sf.amount),0) AS `销售额`"),
            "{}",
            h.sql
        );
        assert!(h.sql.contains("sf.order_date >= '2026-08-10'"), "{}", h.sql);
        assert!(
            h.sql
                .contains("sf.order_date < DATE_ADD('2026-08-11', INTERVAL 1 DAY)"),
            "{}",
            h.sql
        );
        let region = "COALESCE(NULLIF(sf.region,''),'未归属') IN ('山东省区', '山东战区', '山东大区', '山东')";
        assert!(h.sql.contains(region), "省份限定必须进入主查询：{}", h.sql);
        assert!(
            !h.sql.contains("GROUP BY"),
            "周报主指标必须是单值：{}",
            h.sql
        );
        assert!(
            !h.sql.contains("不可计算"),
            "省份限定不应再落解析失败卡：{}",
            h.sql
        );
        for forbidden in ["t_sales_order", "UNION ALL", " JOIN "] {
            assert!(
                !h.sql.contains(forbidden),
                "周报主查询不得回退旧事实 {forbidden}: {}",
                h.sql
            );
        }

        let detail = h.detail.as_deref().expect("销售标量应带同窗明细");
        let context = h
            .sales_context
            .as_deref()
            .expect("销售标量应带同窗经营补充");
        assert!(detail.contains(region), "明细必须共享省份谓词：{detail}");
        assert!(
            context.contains(region),
            "同窗补充必须共享省份谓词：{context}"
        );
        assert!(
            !context.contains("GROUP BY"),
            "同窗补充必须保持单值：{context}"
        );

        let relative = warehouse_sales_fact("山东省本月销售额").expect("相对周期单省销售额应命中");
        let (prev, _) = relative.prev.as_ref().expect("本月标量应带环比");
        assert!(
            relative.sql.contains(region),
            "相对周期主查询必须共享省份谓词：{}",
            relative.sql
        );
        assert!(prev.contains(region), "环比必须共享省份谓词：{prev}");
        assert!(
            relative
                .detail
                .as_deref()
                .is_some_and(|sql| sql.contains(region)),
            "相对周期明细必须共享省份谓词：{:?}",
            relative.detail
        );
        assert!(
            relative
                .sales_context
                .as_deref()
                .is_some_and(|sql| sql.contains(region)),
            "相对周期补充必须共享省份谓词：{:?}",
            relative.sales_context
        );

        assert!(
            warehouse_sales_fact("山东省和江苏省 2026-08-10 至 2026-08-11 销售额").is_none(),
            "多省问法必须 fail closed，不能静默查全国或只取一个省"
        );
    }

    #[test]
    fn sales_region_execution_uses_dms_business_region_exceptions() {
        let shandong = warehouse_sales_fact("山东省本月销售额").expect("普通省份兼容路径不能回退");
        assert!(
            shandong.sql.contains(
                "COALESCE(NULLIF(sf.region,''),'未归属') IN ('山东省区', '山东战区', '山东大区', '山东')"
            ),
            "{}",
            shandong.sql
        );

        // 🔴 映射**不是 1:1** 的省份改走 `state` 精确过滤（2026-08-15 生产直打逮到一族倍数级错答）：
        //   海南省 → region='广东省区'（含广东 494.8 万 + 海南 46.1 万）→ 高估 11.7 倍
        //   上海市 → region='浙江省区'                                  → 高估 3.8 倍
        //   西藏   → region='川渝藏大区'（真值 0）                      → 凭空 419 万
        // 全都 trust=verified、caliber_note 为空，用户没有任何途径察觉。
        // region 是**销售组织**口径、与行政省多对一，拿它当行政省的过滤必然多算。
        // 「不许拼出不存在的 'X省区'」这条判据原样保留 —— 它防的是另一件事。
        let shanghai = warehouse_sales_fact("上海市本月销售额").expect("上海销售额应命中 DWS");
        assert!(
            shanghai.sql.contains("INSTR(COALESCE(NULLIF(sf.state,''),'未知'), '上海') > 0"),
            "{}",
            shanghai.sql
        );
        assert!(!shanghai.sql.contains("'上海省区'"), "{}", shanghai.sql);
        assert!(!shanghai.sql.contains("'浙江省区'"), "上海不许被算成整个浙江省区：{}", shanghai.sql);

        let hainan = warehouse_sales_fact("海南省本月销售额").expect("海南销售额应命中 DWS");
        assert!(
            hainan.sql.contains("INSTR(COALESCE(NULLIF(sf.state,''),'未知'), '海南') > 0"),
            "{}",
            hainan.sql
        );
        assert!(!hainan.sql.contains("'海南省区'"), "{}", hainan.sql);
        assert!(!hainan.sql.contains("'广东省区'"), "海南不许被算成整个广东省区：{}", hainan.sql);

        // 1:1 的那一档照旧走 region 四形候选（2026-08-11 业务裁决，实测两侧同值）——
        // 上面 `shandong` 已经钉住；这里再钉一条同族的，防止「顺手全改成 state」。
        let henan = warehouse_sales_fact("河南省本月销售额").expect("河南销售额应命中 DWS");
        assert!(
            henan.sql.contains("COALESCE(NULLIF(sf.region,''),'未归属') IN ('河南省区'"),
            "{}",
            henan.sql
        );

        let mini = mini_program_order_agg("海南省区本月小程序下单金额")
            .expect("小程序销售也必须复用同一业务省区例外");
        assert!(mini.sql.contains("region = '广东省区'"), "{}", mini.sql);
        assert!(!mini.sql.contains("'海南省区'"), "{}", mini.sql);
    }

    /// 【同窗补充】触发真值表 + SQL 口径钉（裁决：销售类单指标 KPI 顺带成本/收入/毛利额/毛利率）。
    /// ① sales_fact 指标族的标量问法都挂补充；② 补充与主查询同一时间窗（谓词逐字相同）、
    /// 五值单行、无 GROUP BY；③ 主 SQL 一个字不变；④ 维度拆解/多指标/非销售标量/失败关闭卡不挂。
    #[test]
    fn sales_context_only_on_scalar_sales_kpi_with_the_same_window() {
        for question in [
            "本月销售额", "本周销售额", "本月销量", "本月毛利额", "本月毛利率",
            "本月不含税成本", "本月不含税收入",
        ] {
            let hit = warehouse_sales_fact(question)
                .unwrap_or_else(|| panic!("销售标量应命中：{question}"));
            assert_eq!(hit.route, "direct-agg", "{question}");
            let context = hit.sales_context.as_deref()
                .unwrap_or_else(|| panic!("销售单指标 KPI 必须带同窗补充：{question}"));
            // ② 同时间窗：主 SQL 的 order_date 半开谓词在补充里逐字重现（含同批谓词）
            let window = hit.sql.split("WHERE ").nth(1).unwrap_or_default().to_string();
            assert!(window.contains("sf.order_date >="), "{question}: {}", hit.sql);
            assert!(context.contains(&format!("WHERE {window}")),
                    "{question} 补充时间窗/谓词 ≠ 主查询：{context}");
            for select in [
                "COALESCE(SUM(sf.amount),0) AS `销售额`",
                "COALESCE(SUM(sf.cost_excluding_tax),0) AS `不含税成本`",
                "COALESCE(SUM(sf.revenue_excluding_tax),0) AS `不含税收入`",
                "COALESCE(SUM(sf.gross_profit),0) AS `毛利额`",
                "SUM(sf.gross_profit)/NULLIF(SUM(sf.revenue_excluding_tax),0) AS `毛利率`",
            ] {
                assert!(context.contains(select), "{question} 补充缺 {select}：{context}");
            }
            assert!(!context.contains("GROUP BY"), "{question} 补充必须单行五值：{context}");
            assert!(context.contains(dms_semantic::sales_fact::TABLE), "{question}: {context}");
            // ③ 主 SQL 一个字不变：补充走独立字段，绝不并进主 SELECT
            assert!(!hit.sql.contains("不含税成本") || question.contains("不含税成本"),
                    "{question} 主 SQL 被补充污染：{}", hit.sql);
        }
        // ④ 不触发族
        let by_region = warehouse_sales_fact("本月销售额按省区").expect("维度拆解命中");
        assert!(by_region.sales_context.is_none(), "维度拆解自带这些列，不挂补充：{}", by_region.sql);
        let multi = warehouse_sales_fact("本月销售额和毛利率").expect("多指标命中");
        assert!(multi.sql.contains("SUM(sf.gross_profit)/NULLIF"), "前提：确为多指标：{}", multi.sql);
        assert!(multi.sales_context.is_none(), "多指标主结果已是多列，不挂补充：{}", multi.sql);
        let stock = try_direct("现在库存量是多少").expect("非销售 direct-agg 标量");
        assert_eq!(stock.route, "direct-agg");
        assert!(stock.sales_context.is_none(), "补充不许挂到非销售指标：{}", stock.sql);
        let unavailable = try_direct_for("本月销售额", false).expect("业务库失败关闭卡");
        assert!(unavailable.sales_context.is_none(), "不可计算卡不挂补充：{}", unavailable.sql);
    }

    #[test]
    fn agg_order_count() {
        let h = agg_template("今天有多少订单数").unwrap();
        assert!(h.sql.contains("COUNT(DISTINCT sales_order_code)"));
        assert!(h.sql.contains("DATE(order_time) = CURDATE()"));
    }

    // prev_window 搬进 kernel 时由写死列名改成占位模板（唯一的语义等价改写）——
    // 这里钉住填回 order_time 后的**字节**，模板化若改了 SQL 立刻红。
    //
    // 🔴 **本轮有意改了这条断言**（pin 断言的用途就是这个）：上期右端从
    // `< DATE_FORMAT(CURDATE(),'%Y-%m-01')`（＝**整个上月**）改成
    // `< DATE_ADD(CURDATE() - INTERVAL 1 MONTH, INTERVAL 1 DAY)`（＝**上月同日次日零点**，
    // 与当期 `< DATE_ADD(CURDATE(), INTERVAL 1 DAY)` 使用同一个含当日进度）。
    //
    // 改的理由是那是个**错数**，不是风格：当期是「本月至今」，旧上期是「上月整月」，
    // 两个不同长度的窗口相除塞进 `items[].delta`，前端照显示。实算偏差（日均恒定假设）：
    //   7-02  当期 2 天 vs 旧上期 30 天 → 「较上月 -93.3%」；新口径 vs 1 天 → +50%
    //   7-15  当期 15 天 vs 30 天       → −50.0%          ；新口径 vs 14 天 → +3.6%
    //   7-30  当期 30 天 vs 30 天       →   0%            ；新口径 vs 29 天 → +1.7%
    // **月末恰好归零**正是它一直没被抓到的原因 —— 月中看到的每一个环比都是错的。
    // 「今年」那档更夸张：去年整年（365 天）比年初至今（211 天）→ −42.2%。
    #[test]
    fn dws_sales_prev_window_uses_order_date() {
        let hit = warehouse_sales_fact("本月销售额是多少").unwrap();
        let (prev, label) = hit.prev.unwrap();
        assert_eq!(label, "较上月");
        assert!(!prev.contains("{}"), "{prev}");
        assert!(prev.contains("sf.order_date >= DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01')"), "{prev}");
        assert!(prev.contains("sf.order_date < DATE_ADD(CURDATE() - INTERVAL 1 MONTH, INTERVAL 1 DAY)"), "{prev}");
        // 🔴 反面①：**不许**再出现旧口径那个右端（改回去必须红，而不是「看起来像新的」）
        assert!(
            !prev.contains("< DATE_FORMAT(CURDATE(),'%Y-%m-01')"),
            "上期右端回到了「整个上月」——月中的环比会重新变成 −50% 级的错数：{prev}"
        );
        assert!(!prev.contains("order_time") && !prev.contains("delivery_time"), "环比用了非事实时间列：{prev}");
        let (yoy, yoy_label) = hit.comparisons.into_iter().next().expect("销售额必须有同比");
        assert_eq!(yoy_label, "同比");
        assert!(yoy.contains("sf.order_date >= DATE_FORMAT(CURDATE() - INTERVAL 1 YEAR,'%Y-%m-01')"), "{yoy}");
        assert!(yoy.contains("sf.order_date < DATE_ADD(CURDATE() - INTERVAL 1 YEAR, INTERVAL 1 DAY)"), "{yoy}");
        let (day, day_label) = agg_template("今天有多少订单数").unwrap().prev.unwrap();
        assert_eq!(day_label, "较昨天");
        assert!(day.ends_with("AND DATE(order_time) = CURDATE() - INTERVAL 1 DAY"), "{day}");
    }

    /// ① 尾部问法修饰词剥离：这四句实测全落过「不可计算」卡（残留守卫把
    /// 「怎么样/同比增长多少/其中X占多少」当成未识别限定），而它们 KPI 自带 delta 或可答。
    #[test]
    fn tail_modifier_words_no_longer_false_positive_unavailable() {
        // 「同比多少」：剥尾词后走标量事实，且**点名的同比占主 delta 位**（prev = 同比窗口）
        let yoy = warehouse_sales_fact("上月销售额同比增长多少")
            .expect("同比问法应命中标量事实，不该落不可计算卡");
        assert!(yoy.sql.contains("COALESCE(SUM(sf.amount),0) AS `销售额`"), "{}", yoy.sql);
        let (prev_sql, prev_label) = yoy.prev.as_ref().expect("同比问法必须有主 delta");
        assert_eq!(prev_label, "同比", "点名的同比必须在 KPI 第一比较位：{prev_sql}");
        assert!(prev_sql.contains("INTERVAL 1 YEAR"), "同比窗口必须是去年同期：{prev_sql}");
        assert_eq!(yoy.comparisons.len(), 1, "环比退居 comparisons：{:?}", yoy.comparisons);
        assert_eq!(yoy.comparisons[0].1, "较上上月");

        // 「环比…怎么样」：环比本来就是主 delta 位；「怎么样」是纯语气
        let mom = warehouse_sales_fact("本月销售额环比上月怎么样")
            .expect("环比问法应命中标量事实");
        assert_eq!(mom.prev.as_ref().map(|(_, l)| l.as_str()), Some("较上月"));
        // 主查询时间窗必须是「本月」（rule_relative 里本月先于上月），不能被「环比上月」抢走
        assert!(mom.sql.contains("sf.order_date >= DATE_FORMAT(CURDATE(),'%Y-%m-01')"), "{}", mom.sql);

        // 「怎么样」：纯语气尾词
        let tone = warehouse_sales_fact("昨天的销量怎么样").expect("语气尾词不该挡路");
        assert!(tone.sql.contains("COALESCE(SUM(sf.qty),0) AS `销量`"), "{}", tone.sql);
        assert!(tone.sql.contains("sf.order_date >= CURDATE() - INTERVAL 1 DAY"), "{}", tone.sql);

        // 「其中 X 占多少」：X 在合同里无可验证谓词、compound 只接极值词族 ——
        // 按裁决以 KPI+delta 形态答总量（scalar 命中，自带 prev/同比/明细/同窗补充）
        let share = warehouse_sales_fact("上月销售额，其中直营占多少")
            .expect("占比族按 KPI+delta 形态答总量");
        assert!(share.sql.contains("COALESCE(SUM(sf.amount),0) AS `销售额`"), "{}", share.sql);
        assert!(share.prev.is_some(), "总量答案自带环比 delta");

        // 整条同步链（try_direct_for）同一结论：四句实测题一律不再出「不可计算」卡
        for question in [
            "上月销售额同比增长多少",
            "本月销售额环比上月怎么样",
            "昨天的销量怎么样",
            "上月销售额，其中直营占多少",
        ] {
            let hit = try_direct_for(question, true)
                .unwrap_or_else(|| panic!("整条链应接住：{question}"));
            assert_eq!(hit.route, "direct-agg", "{question}");
            assert!(!is_unavailable_card(&hit), "{question} 不许再误报不可计算卡：{}", hit.sql);
        }

        // 🔴 反面①：窗口兑现不了「同比」时**不许剥**（剥了 = 静默丢限定）
        assert!(warehouse_sales_fact("上半年销售额同比增长多少").is_none(),
                "上半年没有同比窗口，必须照旧拦下");
        // 🔴 反面②：带维度的问句没有 KPI delta 可挂，「同比」不许剥
        assert!(warehouse_sales_fact("上月各省区销售额同比增长多少").is_none(),
                "维度拆解答不了同比，必须照旧拦下");
        // 🔴 反面③：「其中+极值词」是 compound 的地盘，这里一个字都不剥
        assert!(warehouse_sales_fact("上月销售额，其中最高的客户是哪个").is_none(),
                "极值词族不许被占比族剥掉");
    }

    /// ① 卡面文案：「解析失败」与「合同缺失」必须说不同的话 ——
    /// 修前两支共用「合同没有该维度」，解析失败被误读成合同缺失（判官实测误导）。
    #[test]
    fn unavailable_card_distinguishes_parse_failure_from_contract_gap() {
        // 合同缺失：点名的维度不在合同里 —— 文案保持回归钉的字节
        let gap = warehouse_sales_semantics_unavailable("本月销售额按门店")
            .expect("门店不在合同维度里");
        assert!(gap.sql.contains("'门店' AS `未确认范围`"), "{}", gap.sql);
        assert!(gap.sql.contains("sales_fact 合同没有该维度或语义"), "{}", gap.sql);

        // 解析失败：指标认得出、残余限定消化不掉 —— 卡面指名残留、且不栽给合同
        let parse = warehouse_sales_semantics_unavailable("嗨肉本月销售额")
            .expect("客户名残留是解析失败");
        assert!(parse.sql.contains("'未确认限定' AS `未确认范围`"),
                "「未确认限定」是 direct_hit 探客户主档的哨兵，一个字不许改：{}", parse.sql);
        assert!(parse.sql.contains("解析失败，非合同缺失"), "{}", parse.sql);
        assert!(parse.sql.contains("「嗨肉」"), "卡面必须指名没认出来的那段：{}", parse.sql);
        assert!(!parse.sql.contains("合同没有该维度"), "解析失败不许栽给合同缺失：{}", parse.sql);
    }

    #[test]
    fn agg_skips_dimension() {
        // 带维度词 → 回落 LLM
        assert!(agg_template("本月销售额前五的省份").is_none());
        assert!(agg_template("各商品分类的销量").is_none());
        assert!(agg_template("恒众餐饮本月销售额").is_none()); // 含"客户"实体? 不，含"恒众"但无维度词——靠"客户"词挡不住
    }

    #[test]
    fn agg_needs_time_and_metric() {
        assert!(agg_template("销售额").is_none()); // 无时间窗
        assert!(agg_template("本月天气").is_none()); // 无指标
    }

    #[test]
    fn top_n_detect() {
        assert_eq!(detect_top_n("本月销售额前5的省份"), 5);
        assert_eq!(detect_top_n("销售额前十的客户"), 10);
        assert_eq!(detect_top_n("前三名商品分类"), 3);
        assert_eq!(detect_top_n("销售额top20省份"), 20);
        // 无前N默认 200（对齐全局 MAX_ROWS）：50 会把 60 个商品分类静默截成 50
        assert_eq!(detect_top_n("各省份销售额"), 200);
    }

    #[test]
    fn sales_breakdown_top_n() {
        // 省份已并入省区（region，业务确认字段）：前 N 语义照常兑现
        let h0 = sales_breakdown("本月销售额前5的省份").expect("省份=省区（region），必须命中");
        assert!(h0.sql.contains("LIMIT 5") && h0.sql.contains("sf.region"), "{}", h0.sql);
        let h = sales_breakdown("本月销售额前5的客户").unwrap();
        assert!(h.sql.contains("LIMIT 5"), "{}", h.sql);
        let h2 = sales_breakdown("本月销售额按客户").unwrap();
        assert!(h2.sql.contains("LIMIT 200"), "{}", h2.sql);
    }

    #[test]
    fn sales_breakdown_dims() {
        use dms_semantic::sales_fact;

        for (question, fragment) in [
            ("本月销售额按客户", "sf.storename"),
            ("本月销售额按客户编码", "sf.storecode"),
            ("本月销售额按商品", "sf.skuname"),
            ("本月销售额按商品编码", "sf.skucode"),
            ("本月销售额按战区", "sf.war_zone"),
            ("本月销售额按区域", "sf.region"),
            // 省份=省区（region）：2026-08-11 业务裁决后从「必须回落」挪进受信维度
            ("本月销售额按省份", "sf.region"),
            ("今年每月销售额", "DATE_FORMAT(sf.order_date,'%Y-%m')"),
            ("本月每日销售额", "DATE(sf.order_date)"),
        ] {
            let hit = sales_breakdown(question).unwrap_or_else(|| panic!("未命中受信维度：{question}"));
            assert!(hit.sql.contains(sales_fact::TABLE), "{question}: {}", hit.sql);
            assert!(hit.sql.contains(fragment), "{question}: {}", hit.sql);
            assert!(!hit.sql.contains(" JOIN ") && !hit.sql.contains("UNION ALL"), "{question}: {}", hit.sql);
        }

        assert!(sales_breakdown("本月销售额是多少").is_none());
        assert!(sales_breakdown("本月订单数按省份").is_none());
        for question in [
            "本月销售额按品牌",
            "本月销售额按门店",
            "本月销售额按业务员",
            "本月销售额按区域经理",
            "本月销售额按客户分类",
            "本月销售额按TYPE",
            "本月销售额按商品类型",
            "本月销售额按二级分类",
            "本月销售额按末级分类",
            // 「按城市」2026-08-15 移出本清单：city 是 dws_off_offline_sale_dfn 的实有列
            // （318 个取值），此前被写死在 WAREHOUSE_SALES_UNSUPPORTED 里，
            // 「本月销售额最高的城市」被判「合同没有该维度」—— 把「没登记」讲成「库里没有」。
            "本月销售额按价格组",
            "本月销售额按来源订单类型",
        ] {
            assert!(sales_breakdown(question).is_none(), "未经事实验证的维度必须回落：{question}");
        }
        // 反面：已验证的维度必须接得住（否则这条判据会变成「什么都别加」）
        for question in ["本月销售额按城市", "本月销售额按省区", "本月销售额按战区"] {
            assert!(sales_breakdown(question).is_some(), "已验证维度该接：{question}");
        }
    }

    #[test]
    fn relation_detect() {
        assert_eq!(detect_relation("买过烤肠的客户有哪些"), Some(Relation::BuyersOfGoods("烤肠".into())));
        assert_eq!(detect_relation("恒众买过什么"), Some(Relation::GoodsOfCustomer("恒众".into())));
        // 共购：还买优先
        assert_eq!(detect_relation("买烤肠的还买什么"), Some(Relation::Copurchase("烤肠".into())));
        assert!(detect_relation("本月销售额").is_none());
    }

    #[test]
    fn restricted_relation_questions_have_scoped_sql_fallback() {
        let buyers = relation_rows("买过烤肠的客户").expect("购买客户关系该有 SQL 回退");
        assert_eq!(buyers.route, "direct-doc");
        for anchor in [
            "FROM t_sales_order o",
            "FROM t_sales_order_detail",
            "GROUP BY sales_order_code, sku_code",
            "COUNT(DISTINCT o.sales_order_code)",
            "MAX(o.order_time) AS `最近下单时间`",
        ] {
            assert!(buyers.sql.contains(anchor), "受限关系查询缺 {anchor}: {}", buyers.sql);
        }
        let goods = relation_rows("恒众买过什么").expect("客户购买清单该有 SQL 回退");
        assert!(goods.sql.contains("o.customer_name LIKE '%恒众%'"), "{}", goods.sql);
        let together = relation_rows("买烤肠的还买什么").expect("共购关系该有 SQL 回退");
        assert!(together.sql.contains("SELECT DISTINCT sales_order_code")
            && together.sql.contains("NOT (d.sku_name LIKE '%烤肠%' OR d.sku_code = '烤肠')"), "{}", together.sql);
    }

    fn sales_metric() -> MetricDef {
        MetricDef {
            name: "销售额".into(),
            aliases: vec!["业绩".into()],
            source_table: "t_sales_order".into(),
            agg_expr: "SUM(total_amount)".into(),
            scope_filter: "deleted_flag = 0 AND order_status NOT IN ('0','108','199')".into(),
            dedup_keys: String::new(),
            time_col: "order_time".into(),
        }
    }
    /// 别名与 `semantic::seed_defs` 的 `buyer_count` 行**逐字相同** —— 伪维度这件事全靠
    /// 「指标命中词里含『客户』」，用一个删了别名的假指标去测就测不到真正的形态。
    fn buyer_metric() -> MetricDef {
        MetricDef {
            name: "成交客户数".into(),
            aliases: vec!["下单客户数".into(), "成交客户".into(), "多少客户".into(), "客户数".into()],
            source_table: "t_sales_order".into(),
            agg_expr: "COUNT(DISTINCT customer_code)".into(),
            scope_filter: "deleted_flag = 0 AND order_status NOT IN ('0','108','199')".into(),
            dedup_keys: String::new(),
            time_col: "order_time".into(),
        }
    }
    fn qty_metric() -> MetricDef {
        MetricDef {
            name: "销量".into(),
            aliases: vec![],
            source_table: "t_sales_order_detail(JOIN t_sales_order 有效订单)".into(),
            agg_expr: "SUM(box_quantity)".into(),
            scope_filter: "item_type = '1'".into(),
            dedup_keys: "sales_order_code,sku_code,sku_name,box_quantity,amount".into(),
            time_col: "order_time".into(),
        }
    }
    /// 通用时间分桶必须绑定指标声明的时间列，不能沿维度登记表错误 JOIN。
    /// 退款按 `after_sales_time` 分月，销售按 `order_time` 分月；两者共享“月份”定义，
    /// 但 SQL 中的实际列由指标声明决定。
    #[test]
    fn cross_table_time_dimension_binds_metric_time_column() {
        // 🔴 夹具与 `semantic::seed_defs` 的 `month` 行**逐字相同**（含 6 个别名）。
        // 第一版我把 aliases 写成空的，于是「每个月」不被消化 → 残留守卫拦下 →
        // ② 那条「同表不许误伤」当场红，而红的原因**不是**我的门 ——
        // 是夹具不对。同 `buyer_metric` 那条注释的理由：用删了别名的假声明测不到真形态。
        let month = DimDef {
            name: "月份".into(),
            aliases: ["按月", "每月", "每个月", "按月份", "各月", "月度"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            source_table: "t_sales_order o".into(),
            expr: "DATE_FORMAT(o.order_time,'%Y-%m')".into(),
        };
        // ① 跨登记表（售后指标 × 通用月份）→ 改绑售后时间，不 JOIN 销售订单。
        let refund = MetricDef {
            name: "退款额".into(),
            aliases: vec![],
            source_table: "t_after_sales_order_header".into(),
            agg_expr: "SUM(refund_amount)".into(),
            scope_filter: "deleted_flag = 0".into(),
            dedup_keys: String::new(),
            time_col: "after_sales_time".into(),
        };
        let sql = compose_gated(
            &refund,
            &month,
            "今年每个月的退款额",
            &edges(),
            &scopes(),
            &[],
            &[],
        )
        .expect("退款额应按指标自己的时间列装配");
        assert!(sql.contains("DATE_FORMAT(b0.after_sales_time,'%Y-%m')"), "{sql}");
        assert!(sql.contains("YEAR(b0.after_sales_time) = YEAR(CURDATE())"), "{sql}");
        assert!(!sql.contains("t_sales_order"), "退款趋势不应为了月份 JOIN 销售订单：{sql}");
        assert!(!sql.contains("order_time"), "退款趋势不应使用下单时间：{sql}");

        // ② 跨表但不是时间维度时仍沿正常 JOIN 规则处理，不受时间改绑影响。
        let province = DimDef {
            name: "省份".into(),
            aliases: vec![],
            source_table: "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code".into(),
            expr: "COALESCE(NULLIF(cus.province,''),'未知')".into(),
        };
        assert!(
            is_time_expr(&month.expr) && !is_time_expr(&province.expr),
            "时间判据把省份也当成时间维度了"
        );

        // ③ 只改第一个函数参数；不能把格式串或普通维度误改。
        assert_eq!(
            bind_time_dimension("DATE_FORMAT(o.order_time,'%Y-%m')", "h.after_sales_time"),
            Some("DATE_FORMAT(h.after_sales_time,'%Y-%m')".into())
        );
        assert!(bind_time_dimension("COALESCE(NULLIF(cus.province,''),'未知')", "h.after_sales_time").is_none());
    }

    /// `is_time_expr` 的正反对照。**判宽比判窄安全**（多拒一条只是回落 LLM），
    /// 但不能宽到把普通维度也吃掉 —— 那会把一整族下钻打回 LLM。
    #[test]
    fn time_expr_detection_both_ways() {
        for e in [
            "DATE_FORMAT(o.order_time,'%Y-%m')",
            "YEAR(h.after_sales_time)",
            "date_trunc('month', created_time)",
            "EXTRACT(MONTH FROM x)",
            "QUARTER(o.order_time)",
        ] {
            assert!(is_time_expr(e), "该判成时间维度：{e}");
        }
        for e in [
            "COALESCE(NULLIF(cus.province,''),'未知')",
            "COALESCE(NULLIF(g.goods_category_name,''),'未分类')",
            "COALESCE(NULLIF(g.brand_name,''),'未归属')",
            // 列名里带 time 但不是分组函数 —— 不许因为列名就判成时间维度
            "COALESCE(o.order_time_zone,'未知')",
        ] {
            assert!(!is_time_expr(e), "普通维度被当成时间维度了：{e}");
        }
    }

    fn dim(name: &str, expr: &str) -> DimDef {
        DimDef {
            name: name.into(),
            aliases: vec![],
            source_table: "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0".into(),
            expr: expr.into(),
        }
    }
    /// 🔴 注册表选取必须与**行序无关**：命中词最长者胜，等长按名字定序。
    /// 原实现是 `find`（第一条命中的），而 `load_dimensions` 无 `ORDER BY` ——
    /// 返回序是 PG 物理行序，种子重灌就会变，E17 那条回归只是碰巧绿的。
    #[test]
    fn pick_takes_longest_hit_not_row_order() {
        let mut ds = vec![
            DimDef { name: "客户".into(), aliases: vec!["经销商".into()], source_table: "t_sales_order o".into(), expr: "x".into() },
            DimDef { name: "客户分类".into(), aliases: vec![], source_table: "t_sales_order o".into(), expr: "y".into() },
        ];
        let q = "本月按客户分类的销售额";
        // 两条都命中（"客户分类" 含 "客户"）；长者胜，且换行序结论不变
        assert_eq!(pick(q, &ds, |d| (&d.name, &d.aliases)).unwrap().0.name, "客户分类");
        ds.reverse();
        assert_eq!(pick(q, &ds, |d| (&d.name, &d.aliases)).unwrap().0.name, "客户分类");

        // 别名也参与长度比较：4 字的「区域经理」压过 2 字的「区域」/「经理」（回归 B06）
        let three = vec![
            DimDef { name: "省份".into(), aliases: vec!["区域".into()], source_table: "t_sales_order o".into(), expr: "p".into() },
            DimDef { name: "业务员".into(), aliases: vec!["经理".into()], source_table: "t_sales_order o".into(), expr: "o".into() },
            DimDef { name: "大区经理".into(), aliases: vec!["区域经理".into()], source_table: "t_sales_order o".into(), expr: "a".into() },
        ];
        assert_eq!(pick("各区域经理业绩", &three, |d| (&d.name, &d.aliases)).unwrap().0.name, "大区经理");
        // 一条都不命中 → None（不许退化成「取第一条」）
        assert!(pick("今天天气", &three, |d| (&d.name, &d.aliases)).is_none());
    }

    /// SQL 的投影列数（`SELECT` 到 `FROM` 之间的 `` AS `别名` `` 个数）。
    ///
    /// 🔴 判据必须是**列数**，不是 route：二·AS1 那两处错答的 route 全是 `direct-agg`、
    /// 零报错、`caliber_note` 为空 —— 用户问「有多少客户」，拿到的是 200 行客户名单。
    /// 本函数自己的反向自证就在下面那条测试里（分组 SQL 必须数出 2，数不出 2 说明量器坏了）。
    fn proj_cols(sql: &str) -> usize {
        sql.split("FROM").next().unwrap_or(sql).matches("AS `").count()
    }

    /// 🔴 **主修**：指标命中词内部的伪维度命中必须被减掉（审计 二·AS1）。
    ///
    /// 实证错答：「上周成交客户数是多少」→ `direct-agg`、列=[客户, 成交客户数]、200 行，
    /// 首格是「发员工福利样品使用」。因为 `pick(metrics)` 与 `pick(dims)` 各判一次、互不减词，
    /// 「成交客户**数**」里的「客户」被再次当成维度，而残留守卫剥完指标名+维度名后正好为空。
    #[test]
    fn pseudo_dim_hit_inside_a_metric_word_is_not_a_dimension() {
        let buyer = buyer_metric();
        let dims = vec![
            dim("客户", "COALESCE(o.customer_name,'未知')"),
            DimDef {
                name: "省份".into(),
                aliases: vec!["各省".into()],
                source_table: "t_sales_order o".into(),
                expr: "cus.province".into(),
            },
        ];
        // ── ① 伪命中必须被挡 ──
        let q = "上周成交客户数是多少";
        // 前提（也正是错答的成因）：不减词的话「客户」会命中维度。这一句是本测试的承重点：
        // 它证明减词那道判据是**承重**的，而不是一句多余的保险。
        assert_eq!(
            pick(q, &dims, |d| (&d.name, &d.aliases)).map(|(d, _)| d.name.as_str()),
            Some("客户"),
            "前提没了 —— 本条测的就是这个伪命中"
        );
        assert!(
            pick_excluding(q, &dims, |d| (&d.name, &d.aliases), &metric_word(q, &buyer)).is_none(),
            "伪维度没被减掉 —— 二·AS1 的 200 行客户名单会回来"
        );
        // 量器自证：不减词时装配出来的真的是**两列**分组查询（数不出 2 就是 `proj_cols` 坏了）
        let bad = compose_gated(&buyer, &dims[0], q, &edges(), &scopes(), &[], &[])
            .expect("前提：不减词就会装配成「按客户分组的客户数」");
        assert_eq!(proj_cols(&bad), 2, "{bad}");
        assert!(bad.contains("GROUP BY"), "{bad}");
        // 减词后由无维度模式接：一个投影列、不分组
        let nodim = DimDef {
            name: String::new(),
            aliases: vec![],
            source_table: "t_sales_order b0".into(),
            expr: String::new(),
        };
        let good = compose_sql_with(&buyer, &nodim, q, &edges(), &scopes()).expect("减词后该接得住");
        assert_eq!(proj_cols(&good), 1, "单指标问句只该有一个投影列：{good}");
        assert!(!good.contains("GROUP BY"), "{good}");
        // ── ② 真维度不许被误杀 ──
        let q2 = "本月销售额按客户";
        assert_eq!(
            pick_excluding(q2, &dims, |d| (&d.name, &d.aliases), &metric_word(q2, &sales_metric()))
                .map(|d| d.name.as_str()),
            Some("客户"),
            "「客户」不是「销售额」的子串 —— 这是真维度，减词不许碰它"
        );
        // 指标词里含「客户」，但问句在指标词**之外**还写了一次 → 用户真要分组
        let q3 = "各客户成交客户数";
        assert_eq!(
            pick_excluding(q3, &dims, |d| (&d.name, &d.aliases), &metric_word(q3, &buyer))
                .map(|d| d.name.as_str()),
            Some("客户")
        );
        // 伪命中让位后同句里的真维度照旧命中。此前「各省」赢是靠 `(长度, 名字)` 的字典序
        // **碰巧**（「省」> 「客」），换个维度名就翻 —— 现在是判据说了算。
        let q4 = "各省成交客户数";
        assert_eq!(
            pick_excluding(q4, &dims, |d| (&d.name, &d.aliases), &metric_word(q4, &buyer))
                .map(|d| d.name.as_str()),
            Some("省份")
        );
    }

    /// 🔴 **次修**：时间词表只有一份（`kernel::nl::lexicon::STRIP_WORDS`）。
    ///
    /// 两份词表的差集精确地就是二·AS1 的曝光面：「上周」「去年」在 STRIP_WORDS 里（残留守卫
    /// 剥得掉、不拦），却不在 `agg_template` 原来的内联时间词表里（模板返 None → 让路门开）
    /// —— 两条一凑，单指标问句被装配成分组查询。
    #[test]
    fn agg_template_time_words_come_from_the_single_source() {
        use dms_kernel::nl::lexicon::STRIP_WORDS;
        // ③ 差集为空。把 `agg_strip_words` 改回内联表（或往里手抄时间词）立刻红。
        let table = agg_strip_words();
        for w in STRIP_WORDS {
            assert!(table.contains(w), "「{w}」不在 agg_template 的剥词表里 —— 第二份词表又出现了");
        }
        // 🔴 上面那条只锁词表，这条锁**结果**：逐词要求「剥得掉 ⇔ 接得住」。
        // 判据是 `==` 而不是「都得接住」：剥得掉但 `time_predicate` 解析不了的（光秃秃的
        // 「天」「季度」「近」）本来就该返 None —— 这个 `==` 正是「剥词表放宽了而时间解析
        // 没跟上」的判据，也是本轮唯一要逐词核的东西。
        for w in STRIP_WORDS {
            let q = format!("{w}成交客户数是多少");
            assert_eq!(
                agg_template(&q).is_some(),
                time_predicate(&q).is_some(),
                "「{w}」：剥词表与 time_predicate 不一致（{q}）"
            );
        }
        // 二·AS1 的原题：坏的那两句 route 也是 `direct-agg`，所以断言**列数**
        for q in ["本月成交客户数是多少", "上周成交客户数是多少", "去年成交客户数是多少"] {
            let h = agg_template(q).unwrap_or_else(|| panic!("模板必须接住：{q}"));
            assert_eq!(proj_cols(&h.sql), 1, "{}", h.sql);
            assert!(h.sql.contains("COUNT(DISTINCT customer_code)"), "{}", h.sql);
            assert!(!h.sql.contains("GROUP BY"), "{}", h.sql);
        }
        // 🔴 「本/上/这季度」与「最近N个月」曾经**接不住**，而且都不是时间解析的问题：
        // `STRIP_WORDS` 只有「季度」没有「本季度」⇒ 剥完剩一个「本」；
        // 「近」排在「最近」之前 ⇒ 「最近三个月」剥完剩一个「最」。两族都被残留守卫拦下、
        // 静默回落 LLM。已在 `kernel/nl/lexicon.rs` 那侧修好（补三个季度词 + 调「最近/近」词序），
        // 本组断言从「钉住接不住的现状」翻成「必须接住」，并连列数一起判。
        for q in [
            "本季度成交客户数是多少",
            "上季度成交客户数是多少",
            "最近三个月成交客户数是多少",
        ] {
            assert!(time_predicate(q).is_some(), "时间解析该认得：{q}");
            let h = agg_template(q).unwrap_or_else(|| panic!("词表修好后必须接住：{q}"));
            assert_eq!(proj_cols(&h.sql), 1, "{}", h.sql);
            assert!(!h.sql.contains("GROUP BY"), "{}", h.sql);
        }
        // 订单口径模板仍覆盖这些时间形态；默认销售额不参与本模板。
        // 这些问法没有 KPI 环比：
        // `prev_window` 只认 今天/昨日/昨天/本月/这个月/上月/上个月/本周/这周/今年 ——
        // 「上周」不含「本周」、「上半年/下半年」不含「今年」、「近三个月」不含「上个月」，
        // 三句全返 `None` ⇒ `agg_template` 的 `prev` 恒 `None` ⇒ 前端不出环比标签。
        // 下面那条 `prev.is_none()` 断言就是为了让这句话不再漂。
        for q in [
            "去年订单数是多少",
            "上半年客单价是多少",
            "近三个月成交客户数是多少",
        ] {
            let h = agg_template(q).unwrap_or_else(|| panic!("本轮刻意放宽的形态：{q}"));
            assert!(h.prev.is_none(), "{q} 不该有环比 —— prev_window 认不得这些相对词");
        }
        // 反面（防恒真）：`prev_window` 认得的那批**必须**有环比，否则「无环比」那条断言
        // 会因为「环比整体坏了」而假绿
        for q in ["本月客单价是多少", "今年订单数是多少"] {
            assert!(agg_template(q).unwrap().prev.is_some(), "{q} 应有环比");
        }
        // 「最近三个月」曾栽在 `STRIP_WORDS` 的词序上（「近」排在「最近」之前 ⇒ 剥完剩「最」）。
        // 已在 lexicon 那侧修好，这里连**残留为空**一起判 —— 只判 `agg_template` 接住的话，
        // 换一条别的路径接住它也会绿，看不出词表到底修没修。
        assert!(!has_residue("最近三个月成交客户数", &["成交客户数".to_string()]), "「最」应已被剥净");
        assert!(agg_template("最近三个月成交客户数是多少").is_some(), "词序修好后必须接住");
        // 反面：**阿拉伯数字仍算残留**（`is_alphanumeric` 那道判据本轮没动）——
        // 显式年月与「近7天」照旧不走本模板，别把这次放宽读成「时间问句全归模板」。
        assert!(agg_template("2026年6月销售额是多少").is_none(), "数字仍算残留");
        assert!(agg_template("近7天销售额是多少").is_none(), "同上：阿拉伯数字");
        // 🔴 兑现不了的词一个都不许被剥掉 —— 剥了就是静默答另一个问题
        assert!(agg_template("本月销售额最高的一天").is_none(), "最值要 ORDER BY，本模板出单行");
        assert!(agg_template("本月销售额和订单数是多少").is_none(), "两个指标只会返回一个");
        assert!(agg_template("本月卖了多少箱").is_none(), "「箱」是销量，别把箱数答成金额");
        assert!(agg_template("上周成交客户数是谁").is_none(), "问的是名单，不是一个数");
    }

    fn cat_dim() -> DimDef {
        DimDef {
            name: "商品分类".into(),
            aliases: vec![],
            source_table: "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code AND g.deleted_flag = 0".into(),
            expr: "COALESCE(NULLIF(g.goods_category_name,''),'未分类')".into(),
        }
    }
    fn edges() -> Vec<JoinEdge> {
        vec![
            JoinEdge { lt: "t_sales_order".into(), lc: "sales_order_code".into(), rt: "t_sales_order_detail".into(), rc: "sales_order_code".into(), card: "1:N".into() },
            JoinEdge { lt: "t_sales_order".into(), lc: "customer_code".into(), rt: "t_customer".into(), rc: "customer_code".into(), card: "N:1".into() },
            JoinEdge { lt: "t_sales_order".into(), lc: "owner_manager".into(), rt: "t_employee".into(), rc: "employee_id".into(), card: "N:1".into() },
            JoinEdge { lt: "t_sales_order_detail".into(), lc: "sku_code".into(), rt: "t_goods".into(), rc: "goods_code".into(), card: "N:1".into() },
        ]
    }

    #[test]
    fn qualify_bare_cols() {
        // 裸列限定、引号字面量跳过、已有前缀跳过、函数名跳过
        assert_eq!(
            qualify_cols("deleted_flag = 0 AND order_status NOT IN ('0','108','199')", "o"),
            "o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199')"
        );
        assert_eq!(qualify_cols("SUM(total_amount)", "o"), "SUM(o.total_amount)");
        assert_eq!(
            qualify_cols("COUNT(DISTINCT sales_order_code)", "o"),
            "COUNT(DISTINCT o.sales_order_code)"
        );
        assert_eq!(
            qualify_cols("COALESCE(NULLIF(cus.province,''),'未知')", "o"),
            "COALESCE(NULLIF(cus.province,''),'未知')"
        );
    }

    #[test]
    fn compose_province() {
        let sql = compose_sql(&sales_metric(), &dim("省份", "COALESCE(NULLIF(cus.province,''),'未知')"), "本月销售额按省份", &edges()).unwrap();
        assert!(sql.contains("FROM t_sales_order o LEFT JOIN t_customer"), "{sql}");
        assert!(sql.contains("SUM(o.total_amount)"), "{sql}");
        assert!(sql.contains("o.deleted_flag = 0"), "{sql}");
        assert!(sql.contains("o.order_time >="), "{sql}");
        assert!(sql.contains("GROUP BY COALESCE(NULLIF(cus.province,''),'未知')"), "{sql}");
    }

    #[test]
    fn compose_entity_question_skipped() {
        // 实体残留（恒众餐饮）→ 不装配
        assert!(compose_sql(&sales_metric(), &dim("客户", "COALESCE(o.customer_name,'未知')"), "恒众餐饮本月销售额按客户", &edges()).is_none());
    }

    #[test]
    fn compose_topn_and_no_time() {
        let sql = compose_sql(&sales_metric(), &dim("省份", "cus.province"), "销售额前五省份", &edges()).unwrap();
        assert!(sql.contains("LIMIT 5"), "{sql}");
        assert!(!sql.contains("order_time"), "{sql}"); // 没提时间不加（SuperSonic 对齐）
    }

    #[test]
    fn compose_topn_respects_requested_sort_direction() {
        let province = dim("省份", "cus.province");
        let high = compose_sql(&sales_metric(), &province, "销售额最高的5个省份", &edges()).unwrap();
        assert!(high.contains("ORDER BY `销售额` DESC LIMIT 5"), "{high}");

        for word in ["最少", "最小", "最低"] {
            let q = format!("销售额{word}的5个省份");
            let low = compose_sql(&sales_metric(), &province, &q, &edges())
                .unwrap_or_else(|| panic!("低值 TopN 未识别：{q}"));
            assert!(low.contains("ORDER BY `销售额` ASC LIMIT 5"), "{q}: {low}");
        }
    }

    #[test]
    fn compose_skips_mismatch() {
        // 子查询口径（库存快照）→ 不装配
        let stock = MetricDef {
            name: "库存量".into(),
            aliases: vec![],
            source_table: "t_winc_stock_report".into(),
            agg_expr: "SUM(stock_quantity)".into(),
            scope_filter: "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)".into(),
            dedup_keys: String::new(),
            time_col: String::new(),
        };
        assert!(compose_sql(&stock, &dim("省份", "cus.province"), "本月库存量按省份", &edges()).is_none());
    }

    #[test]
    fn compose_fanout_rejected_for_sum() {
        // 单头 SUM × 明细驱动维度（1:N 扇出）→ 拒绝（防 total_amount 按行数虚增），交手工模板
        assert!(compose_sql(&sales_metric(), &cat_dim(), "本月销售额按商品分类", &edges()).is_none());
    }

    #[test]
    fn compose_qty_province_cross_base() {
        // 销量(detail) × 省份(header→customer)：N:1 链扇出安全 → 装配
        let sql = compose_sql(&qty_metric(), &dim("省份", "COALESCE(NULLIF(cus.province,''),'未知')"), "本月销量按省份", &edges()).unwrap();
        // 基表走去重子查询（明细含系统级重复行），口径过滤下推进子查询
        assert!(sql.contains("FROM (SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity, amount FROM t_sales_order_detail WHERE item_type = '1') b0"), "{sql}");
        assert!(sql.contains("JOIN t_sales_order o ON o.sales_order_code = b0.sales_order_code"), "{sql}");
        assert!(sql.contains("SUM(b0.box_quantity)"), "{sql}");
        assert!(sql.contains("o.order_time >="), "{sql}");
    }

    #[test]
    fn compose_qty_category_time_bridge() {
        // 销量 × 商品分类（同基表 detail）：时间窗经边桥接 t_sales_order o_time
        let sql = compose_sql(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges()).unwrap();
        // 前置①：桥接一律 LEFT JOIN（INNER + 口径进 WHERE = 被连表口径不满足时整行丢）
        assert!(sql.contains("LEFT JOIN t_sales_order o_time ON o_time.sales_order_code = d.sales_order_code"), "{sql}");
        assert!(sql.contains("SUM(d.box_quantity)"), "{sql}");
        assert!(sql.contains("o_time.order_time >="), "{sql}");
    }

    #[test]
    fn dedup_subquery_for_detail_metric() {
        // 明细类指标（含系统级重复行）必须走 DISTINCT 子查询，否则 SUM 虚增 41%（评测抓获）
        let sql = compose_sql(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges()).unwrap();
        assert!(sql.contains("SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity, amount"), "{sql}");
        assert!(sql.contains("WHERE item_type = '1') d"), "口径过滤下推进子查询: {sql}");
        // 外层不再重复加口径过滤
        assert_eq!(sql.matches("item_type").count(), 1, "{sql}");
    }

    #[test]
    fn dedup_skipped_when_col_not_in_keys() {
        // 外层引用了不在去重键里的列 → 子查询取不到 → 不装配（回落 LLM），绝不出错数
        let m = MetricDef {
            name: "销量".into(), aliases: vec![],
            source_table: "t_sales_order_detail".into(),
            agg_expr: "SUM(box_quantity)".into(),
            scope_filter: "item_type = '1'".into(),
            dedup_keys: "sales_order_code,sku_code".into(), // 缺 box_quantity
            time_col: "order_time".into(),
        };
        assert!(compose_sql(&m, &cat_dim(), "本月销量按商品分类", &edges()).is_none());
    }

    #[test]
    fn no_dedup_metric_unchanged() {
        // 无去重键的指标保持原装配（不引入子查询开销）
        let sql = compose_sql(&sales_metric(), &dim("省份", "cus.province"), "本月销售额按省份", &edges()).unwrap();
        assert!(!sql.contains("SELECT DISTINCT"), "{sql}");
    }

    #[test]
    fn base_col_refs_extracts() {
        assert_eq!(base_col_refs("SUM(d.box_quantity)", "d"), vec!["box_quantity"]);
        assert_eq!(base_col_refs("g.goods_code = d.sku_code AND d.sku_code > 0", "d"), vec!["sku_code"]);
        // 别名前缀不得被相似别名误命中
        assert!(base_col_refs("xd.foo", "d").is_empty());
        assert!(base_col_refs("COALESCE(cat.category_name,'未分类')", "d").is_empty());
    }

    fn scopes() -> Vec<(String, String)> {
        vec![
            ("t_sales_order".into(), "deleted_flag = 0 AND order_status NOT IN ('0','108','199')".into()),
            ("t_customer".into(), "deleted_flag = 0".into()),
        ]
    }

    #[test]
    fn table_scope_applied_to_bridge() {
        // 明细指标经时间桥 JOIN 订单主表 → 必须带上有效订单口径（漏则销量虚高 41%，评测抓获）。
        // 前置①②后：口径在桥接的 **ON** 里（LEFT 保留主表行），不再重复进 WHERE（打回 INNER）。
        let sql = compose_sql_with(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges(), &scopes()).unwrap();
        assert!(sql.contains("LEFT JOIN t_sales_order o_time ON o_time.sales_order_code = d.sales_order_code AND o_time.deleted_flag = 0 AND o_time.order_status NOT IN ('0','108','199')"), "{sql}");
        // 口径只出现一次（在 ON）—— 出现两次 = ON 一份 + WHERE 一份 = 打回 INNER
        assert_eq!(sql.matches("o_time.deleted_flag").count(), 1, "口径应只在 ON 出现一次：{sql}");
    }

    /// 🔴 裁决 二·AW 前置①②的完整判据：路径桥接全 LEFT JOIN、被连表口径在 ON、
    /// ON 里的口径**不再**重复进 WHERE（退化的那一半）。
    /// 数值语义：被连表口径不满足时主表行**保留**（被连列 NULL 落「未知」），不再整行丢。
    #[test]
    fn path_joins_are_left_with_caliber_in_on_not_where() {
        let m = MetricDef {
            name: "动销商品数".into(), aliases: vec![],
            source_table: "t_sales_order_detail".into(),
            agg_expr: "COUNT(DISTINCT sku_code)".into(),
            scope_filter: "item_type = '1' AND deleted_flag = 0".into(),
            dedup_keys: "sales_order_code,sku_code,sku_name,box_quantity,amount".into(),
            time_col: "order_time".into(),
        };
        let sql = compose_sql_with(&m, &dim("省份", "COALESCE(NULLIF(cus.province,''),'未知')"),
                                   "本月动销商品数按省份", &edges(), &scopes()).unwrap();
        // ① 跨基表路径桥接是 LEFT JOIN（t_sales_order_detail → t_sales_order）
        assert!(sql.contains(" LEFT JOIN t_sales_order o ON o.sales_order_code = "), "{sql}");
        // ② 桥进来的 t_sales_order 的表级口径在 ON 里
        assert!(sql.contains("ON o.sales_order_code") && sql.contains("AND o.order_status NOT IN ('0','108','199')"), "{sql}");
        // ③ dim_rest 的 LEFT JOIN t_customer 自带口径在 ON（声明原文），
        //    scope_parts 不再重复（出现两次 = ON 一份 + WHERE 一份 = 打回 INNER）
        assert_eq!(sql.matches("cus.deleted_flag").count(), 1, "口径应只在 ON 出现一次：{sql}");
        assert!(sql.contains("LEFT JOIN t_customer cus ON cus.customer_code"), "{sql}");
        // ④ 基表自己的口径不受影响（仍在 WHERE / 下推子查询）
        assert!(sql.contains("item_type = '1'"), "{sql}");
    }

    #[test]
    fn table_scope_not_duplicated_for_metric_base() {
        // 指标基表本身已有 scope_filter → 不重复叠加同一条件
        let sql = compose_sql_with(&sales_metric(), &dim("省份", "cus.province"), "本月销售额按省份", &edges(), &scopes()).unwrap();
        assert_eq!(sql.matches("order_status NOT IN").count(), 1, "{sql}");
        // 维度侧 JOIN 的客户表也吃到表级口径
        assert!(sql.contains("cus.deleted_flag = 0"), "{sql}");
    }

    fn balance_metric() -> MetricDef {
        MetricDef {
            name: "账户余额".into(),
            aliases: vec!["账余".into()],
            source_table: "t_customer_balance".into(),
            agg_expr: "SUM(balance)".into(),
            scope_filter: "deleted_flag = 0 AND balance_status = '4' AND balance_type IN ('8','9')".into(),
            dedup_keys: String::new(),
            time_col: String::new(),
        }
    }
    fn balance_dim() -> DimDef {
        DimDef {
            name: "客户".into(),
            aliases: vec![],
            source_table: "t_customer_balance cb JOIN t_customer c ON c.customer_code = cb.customer_code AND c.deleted_flag = 0".into(),
            expr: "c.customer_name".into(),
        }
    }
    fn balance_snap() -> TableSnapshot {
        TableSnapshot {
            table_name: "t_customer_balance".into(),
            partition_cols: "customer_code,balance_type".into(),
            order_cols: "created_time DESC, id DESC".into(),
            extra_filter: "balance_status = '4'".into(),
            note: "快照表取最新一条".into(),
        }
    }

    /// 🔴 快照表**按声明装配**（本轮从「一律不装配」改过来的）。
    ///
    /// 旧行为：见 `meta.table_snapshot` 就拒 —— 正确但过度。它把余额/库存这一族**永久**
    /// 留在 LLM 路径上，而实测 LLM 把 `rn = 1` 写对的概率约 1/3。
    /// 而声明里已经有分区键、取最新的排序、该表恒需的额外过滤三样，装配器照它包一层即可。
    ///
    /// 这条测试的**前身断言的是旧行为**（`is_none()`）。改行为就必须改钉它的断言 ——
    /// 留着旧断言让它红，或者删掉它，都是在掩盖「行为变了」这件事。
    #[test]
    fn snapshot_source_metric_composed_per_declaration() {
        let q = "各客户账户余额";
        let sql =
            compose_gated(&balance_metric(), &balance_dim(), q, &edges(), &scopes(), &[balance_snap()], &[])
                .expect("有完整声明就该装配");
        // ① 窗口按声明的分区键与排序，且取 rn = 1
        assert!(sql.contains("PARTITION BY customer_code, balance_type"), "{sql}");
        assert!(sql.contains("ORDER BY created_time DESC, id DESC"), "{sql}");
        assert!(sql.contains("rk.rn = 1"), "{sql}");
        // ② 口径**下推进最内层**：窗口要在已过滤的集合上算，否则 rn=1 可能取到一条被口径排除的行
        let inner = &sql[..sql.find("rk.rn = 1").unwrap()];
        assert!(inner.contains("balance_status = '4'"), "口径没下推进窗口子查询：{sql}");
        // ③ 同一个条件在指标口径与 extra_filter 里都出现时不重复拼
        assert_eq!(sql.matches("balance_status = '4'").count(), 1, "{sql}");
        // ④ 聚合仍在基表别名上（外层看到的是派生表）
        assert!(sql.contains("SUM(cb.balance)"), "{sql}");

        // 🔴 两种仍然拒的：声明不全 / 与去重键并存（两层怎么叠是未定义的）
        let mut bad = balance_snap();
        bad.partition_cols = String::new();
        assert!(
            compose_gated(&balance_metric(), &balance_dim(), q, &edges(), &scopes(), &[bad], &[]).is_none(),
            "缺分区键就包不出确定的「最新一条」"
        );
        let mut m2 = balance_metric();
        m2.dedup_keys = "customer_code".into();
        assert!(
            compose_gated(&m2, &balance_dim(), q, &edges(), &scopes(), &[balance_snap()], &[]).is_none(),
            "去重键与快照并存时不许装配"
        );
        // 非快照表来源的指标不受快照清单影响，且不该凭空多出 ROW_NUMBER
        let s2 = compose_gated(&sales_metric(), &dim("省份", "cus.province"), "本月销售额按省份", &edges(), &scopes(), &[balance_snap()], &[]).unwrap();
        assert!(!s2.contains("ROW_NUMBER"), "{s2}");
    }

    #[test]
    fn from_table_aliases_parses() {
        let f = "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code JOIN t_sales_order o_time ON o_time.sales_order_code = d.sales_order_code";
        let got = from_table_aliases(f);
        assert_eq!(got, vec![
            ("t_sales_order_detail".to_string(), "d".to_string()),
            ("t_goods".to_string(), "g".to_string()),
            ("t_sales_order".to_string(), "o_time".to_string()),
        ]);
        // 去重子查询形态：括号内不算 FROM 项
        let f2 = "(SELECT DISTINCT a, b FROM t_sales_order_detail WHERE item_type = '1') d JOIN t_goods g ON g.goods_code = d.sku_code";
        assert_eq!(from_table_aliases(f2), vec![("t_goods".to_string(), "g".to_string())]);
    }

    #[test]
    fn breakdown_handles_declared_filter_and_rejects_unknown_entities() {
        // 值过滤不由本薄模板猜测；带实体名的问句交给实体/安全分析路径。
        assert!(sales_breakdown("线下客户本月销售额").is_none());
        assert!(sales_breakdown("恒众餐饮本月销售额按客户").is_none());
        assert!(sales_breakdown("烤肠本月销售额按省份").is_none());
    }

    #[test]
    fn breakdown_accepts_clean_questions() {
        // 纯「指标×维度(×时间×TopN)」问句照常走确定性模板
        for q in ["本月按省区销售额", "销售额前5的客户",
                  "各月销售额趋势", "本月按战区销售额"] {
            assert!(sales_breakdown(q).is_some(), "{q}");
        }
        for q in ["本月各省销售额", "各二级分类销售额", "本月销售额按业务员",
                  "本月各门店销售额", "本月销售额按品牌"] {
            assert!(sales_breakdown(q).is_none(), "未经验证的事实维度不可硬接：{q}");
        }
    }

    /// 🔴 残留守卫的边界：**只多剥「上半年/下半年」，别的一个字都没放宽**。
    ///
    /// 本测试的前身断言过「显式年份被消化」—— **枪测证明那是恒真的**：
    /// `has_residue_with` 本来就过滤掉所有 ASCII 数字，阿拉伯年份从来不是残留。
    /// 那段「消化年份」的代码因此是死代码，已删；`_DECISIONS` 二·O5a 里对应的判断也已订正。
    /// 留下这条测试是为了钉住**真正会成为残留的东西**（单位词与实体名/值过滤），
    /// 以及 E16 那条实测防线（「**线下**客户本月销售额」被装配成「全部客户 TOP200」，
    /// "线下"这个过滤被静默丢弃）。
    #[test]
    fn residue_guard_boundary_after_half_year_words() {
        let nodim = DimDef {
            name: String::new(),
            aliases: vec![],
            source_table: "t_sales_order_detail b0".into(),
            expr: String::new(),
        };
        let qty = qty_metric();
        // ① 「上半年」不再留下「上半」（这条是本轮唯一真的放宽）
        assert!(!has_entity_residue("2026年上半年的销量", &qty, &nodim, &[]), "上半年应被剥净");
        assert!(!has_entity_residue("2026年6月的销量", &qty, &nodim, &[]));
        // ② 单位量词「箱」已进 `STRIP_WORDS`（GOODS13 那类问句真正的拦路石，不是年份）。
        // 这是本轮第二处放宽，与「上半年」同样只加实测挡住过的那一个词。
        assert!(!has_entity_residue("2026年上半年的销量是多少箱", &qty, &nodim, &[]), "「箱」应被剥");
        // 但**带值过滤的仍要拦**：「整箱订单」剥掉「箱」还剩「整」「订单」
        assert!(has_entity_residue("整箱订单的销量", &qty, &nodim, &[]), "剥「箱」不许放过值过滤");
        // ③ E16 那条必须仍被拦
        let sales = sales_metric();
        let cust = dim("客户", "COALESCE(o.customer_name,'未知')");
        assert!(has_entity_residue("线下客户本月销售额", &sales, &cust, &[]), "E16 的防线被放宽掉了");
        // ④ 值过滤照旧拦住
        assert!(has_entity_residue("2026年6月手抓饼的销量", &qty, &nodim, &[]), "「手抓饼」是值过滤");
    }

    fn vref(t: &str, c: &str, n: &str, code: &str) -> ValueRef {
        ValueRef {
            table: t.into(),
            column: c.into(),
            name: n.into(),
            code: code.into(),
            match_kind: "eq".into(),
        }
    }

    /// 🔴 `match_kind = 'like'` 的行**一律不认**。实测 5 行都在 `t_sales_order.paid_way`
    /// 上（一单多种支付方式，列里存的是多值串）—— 对它写 `= '码'` 是确定性地取错集合。
    /// 我第一版忘了读这一列、无条件拼 `=`；这条断言就是那道闸。
    #[test]
    fn like_match_kind_is_never_composed_as_equality() {
        let mut v = vref("t_sales_order", "paid_way", "信控余额支付", "ZZ01");
        v.match_kind = "like".into();
        assert!(
            value_filters("本月信控余额支付的销售额", &[v], &[]).is_empty(),
            "like 行被当成 = 装配了 —— 那是确定性地取错集合"
        );
    }

    /// 值过滤的**歧义门**：实测 `meta.value_map` 936 行里有 **109 个名字跨 ≥2 个 (表, 列)**，
    /// 猜错一个就是把过滤加在错的表上（数会变，且是确定性路径，没有回炉机会）。
    /// 歧义时必须**当作没命中** —— 那个词照旧是残留，整条回落 LLM，与上线前同形。
    #[test]
    fn value_filters_skip_ambiguous_names() {
        let vals = vec![
            vref("t_customer", "province", "湖南", "430000"),
            // 同名跨两张表 → 歧义
            vref("t_sales_order", "company_code", "湖南虎家", "1242"),
            vref("t_sales_order_detail", "company_code", "湖南虎家", "1242"),
        ];
        let got = value_filters("湖南省本月销售额", &vals, &[]);
        assert_eq!(got.len(), 1, "唯一的那条该认下来");
        assert_eq!(got[0].code, "430000");
        // 歧义名不许被认（否则会挑一张表加过滤）
        assert!(
            value_filters("湖南虎家本月销售额", &vals, &[]).iter().all(|v| v.name != "湖南虎家"),
            "同名跨两张表还敢认 = 在猜"
        );
        // 长名吃短名：问句同时含公司名与省名时，只留最长那个（短的是长的一部分）
        let un = vec![
            vref("t_customer", "province", "湖南", "430000"),
            vref("t_sales_order", "company_code", "湖南虎家", "1242"),
        ];
        let got2 = value_filters("湖南虎家本月销售额", &un, &[]);
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0].name, "湖南虎家", "该取最长命中，不是两个都加");
        // 单字名与破引号的码：一律不认
        let bad = vec![vref("t_x", "c", "男", "1"), vref("t_y", "c", "带引号", "a'b")];
        assert!(value_filters("男的销量带引号", &bad, &[]).is_empty());
        // 🔴 歧义要判在**未过滤**的命中集上：`like` 那行被 match_kind 筛掉后，
        // 剩下的 eq 行不许因此「看起来无歧义」—— 那等于在两列之间猜。
        let mut mixed_like = vref("t_b", "col_b", "某类", "9");
        mixed_like.match_kind = "like".into();
        let mixed = vec![vref("t_a", "col_a", "某类", "1"), mixed_like];
        assert!(
            value_filters("本月某类的销量", &mixed, &[]).is_empty(),
            "eq 落 A 列、like 落 B 列 —— 这是歧义，不许因为 like 被筛掉就当无歧义"
        );
    }

    /// 🔴 **G1**：名字被残留守卫消化掉了，过滤就必须真的装上；装不上必须 `return None`。
    ///
    /// 这是 E16 那类翻车的一般形式：消化了词却不加过滤 = 静默丢限定 = 答非所问而没人报错。
    /// 具体两种装不上：① 声明的表根本不在 FROM 里；② 基表被去重/快照派生表包住了
    /// （派生表只 SELECT 去重键，`v.column` 那一列在外层引用不到）。
    #[test]
    fn consumed_value_name_that_cannot_be_applied_refuses_the_whole_compose() {
        let qty = qty_metric();
        let nodim =
            DimDef { name: String::new(), aliases: vec![], source_table: "t_sales_order_detail b0".into(), expr: String::new() };
        // ① 声明在一张 FROM 里没有的表上 → 整条拒（而不是「消化了词、SQL 里没这条过滤」）
        let elsewhere = vec![vref("t_warehouse", "wh_type", "中心仓", "1")];
        assert!(
            compose_sql_with_snap(&qty, &nodim, "本月中心仓的销量", &edges(), &scopes(), None, None, &elsewhere)
                .is_none(),
            "消化了「中心仓」却装不上过滤 —— 必须拒，不许出一条丢了限定的 SQL"
        );
        // 枪测：把 G1 换成「装不上就跳过」时，上面那条会装配成功且 SQL 里没有 wh_type，
        // 即「本月中心仓的销量」返回全部仓库的销量。下面这条钉住那个失败面。
        let sql_ok = compose_sql_with_snap(
            &qty,
            &nodim,
            "本月销量",
            &edges(),
            &scopes(),
            None,
            None,
            &elsewhere,
        )
        .expect("问句不含该值名时不受影响");
        assert!(!sql_ok.contains("wh_type"), "没提到的值过滤不许自己冒出来：{sql_ok}");
        // ② 基表被去重派生表包住：`t_sales_order_detail` 的列在外层引用不到 → 拒
        let on_base = vec![vref("t_sales_order_detail", "item_type", "赠品", "2")];
        assert!(
            compose_sql_with_snap(&qty, &nodim, "本月赠品的销量", &edges(), &scopes(), None, None, &on_base)
                .is_none(),
            "去重派生表里没有 item_type 列，装上去就是引用不存在的列"
        );
    }

    /// 🔴 值名被已消化的指标/维度词包含（含相等）→ **不是**值过滤。
    ///
    /// 这两条是拿全部 92 道题面（38 评测 + 54 回归）对 `meta.value_map` 全量扫出来的
    /// **唯一**两个危险命中 —— 都是无歧义命中，所以歧义门救不了，只能靠这一刀：
    /// ① 「本月各**业务**员的销售额」：`业务` 唯一命中 `contact_type = 1`，是维度名「业务员」
    ///    的子串。认下来 = 给一道现在全绿的题桥一张联系人表 + 加一条毫无关系的过滤。
    /// ② 「今年**市场费用**…」：`市场费用` 既是指标名、又是 `balance_type = 3` 的码值名。
    ///    相等也必须让给指标（否则会往余额表上加过滤）。
    #[test]
    fn value_name_swallowed_by_a_metric_or_dimension_word_is_not_a_filter() {
        let vals = vec![
            vref("t_customer_contacts_account", "contact_type", "业务", "1"),
            vref("t_customer_balance", "balance_type", "市场费用", "3"),
            vref("t_customer", "customer_class", "线下客户", "04"),
        ];
        // ① 子串：维度名「业务员」在问句里 → 「业务」不认
        let w1: Vec<String> = ["销售额", "业务员"].iter().map(|s| s.to_string()).collect();
        assert!(
            value_filters("本月各业务员的销售额是多少", &vals, &w1).is_empty(),
            "「业务」是「业务员」的子串，认下来就会给全绿的题加错过滤"
        );
        // ② 相等：指标名就叫「市场费用」→ 让给指标
        let w2: Vec<String> = ["市场费用", "费用项目"].iter().map(|s| s.to_string()).collect();
        assert!(
            value_filters("今年市场费用花得最多的前5个费用项目是哪些", &vals, &w2).is_empty(),
            "值名与指标名相等时必须让给指标"
        );
        // ③ 但**不**被包含的照旧认：E16 的「线下客户」不是任何指标/维度词的子串
        let w3: Vec<String> = ["销售额", "客户"].iter().map(|s| s.to_string()).collect();
        let got = value_filters("线下客户本月销售额", &vals, &w3);
        assert_eq!(got.len(), 1, "这一刀不许把真值过滤也切掉");
        assert_eq!(got[0].code, "04");
    }

    /// 值过滤的表不在 FROM 里时，按 `meta.join_edge` 桥一条（与时间窗桥订单头同形）。
    /// 实测阻塞：「本月湖南省的销售额」的 `t_customer.province` 声明在那儿，
    /// 而伪维度的 FROM 只有指标基表 —— 桥不进来就只能整条回落 LLM。
    /// **扇出边一律拒**：`SUM` 沿 1:N 边会把单头列乘一遍（实测销量虚高 41% 的成因）。
    #[test]
    fn value_filter_bridges_its_table_over_a_converging_edge() {
        let m = sales_metric(); // 基表 t_sales_order，无去重键
        let nodim =
            DimDef { name: String::new(), aliases: vec![], source_table: "t_sales_order b0".into(), expr: String::new() };
        let prov = vec![vref("t_customer", "province", "湖南", "430000")];
        let sql = compose_sql_with_snap(
            &m,
            &nodim,
            "本月湖南省的销售额是多少",
            &edges(),
            &scopes(),
            None,
            None,
            &prov,
        )
        .expect("N:1 边该桥得通");
        assert!(sql.contains("JOIN t_customer"), "没把客户表桥进来：{sql}");
        assert!(sql.contains(".province = '430000'"), "值过滤没落进 WHERE：{sql}");
        // 位置性同位语：「湖南**省**」的「省」被消化（否则残留守卫会拦下整条）
        assert_eq!(consumed_phrase("本月湖南省的销售额是多少", "湖南"), "湖南省");
        // 但「省」不许进全局虚词表 —— 那会放宽所有问句的守卫
        assert!(
            !dms_kernel::nl::lexicon::STRIP_WORDS.contains(&"省"),
            "「省」进了全局虚词表 = 位置性这一层白写了"
        );
        // 没有紧跟同位语时不许乱吃（「湖南的销售额」只消化「湖南」）
        assert_eq!(consumed_phrase("湖南的销售额", "湖南"), "湖南");
    }

    /// 🔴 **G2**：目标列已被口径约束 → 拒。销量口径写死 `item_type = '1'`，
    /// 问句说「赠品」（声明 `item_type = '2'`）时若两条都拼上去就是恒 0 行 ——
    /// 确定性路径静默返回「0」，比回落 LLM 坏得多。口径与问句冲突该由人看，不是装配器调和。
    #[test]
    fn value_filter_on_a_column_the_caliber_already_pins_refuses() {
        // 不带去重键，好让基表留在 FROM 里（否则先被 G1 拒，测不到 G2）
        let m = MetricDef {
            name: "销量".into(),
            aliases: vec!["销售数量".into()],
            source_table: "t_sales_order_detail".into(),
            agg_expr: "SUM(quantity)".into(),
            scope_filter: "item_type = '1'".into(),
            dedup_keys: String::new(),
            time_col: "order_time".into(),
        };
        let nodim =
            DimDef { name: String::new(), aliases: vec![], source_table: "t_sales_order_detail b0".into(), expr: String::new() };
        let clash = vec![vref("t_sales_order_detail", "item_type", "赠品", "2")];
        assert!(
            compose_sql_with_snap(&m, &nodim, "本月赠品的销量", &edges(), &scopes(), None, None, &clash)
                .is_none(),
            "口径钉了 item_type='1' 还叠一条 ='2' = 恒 0 行"
        );
        // 同一指标、换一列不冲突的值过滤 → 该装上
        let ok = vec![vref("t_sales_order_detail", "sku_type", "整箱", "9")];
        let sql = compose_sql_with_snap(&m, &nodim, "本月整箱的销量", &edges(), &scopes(), None, None, &ok)
            .expect("不冲突的列该装上");
        assert!(sql.contains("b0.sku_type = '9'"), "值过滤没落进 WHERE：{sql}");
        assert!(sql.contains("item_type = '1'"), "口径不许被值过滤顶掉：{sql}");
    }

    #[test]
    fn has_residue_basics() {
        let w: Vec<String> = ["销售额", "客户"].iter().map(|s| s.to_string()).collect();
        assert!(has_residue("线下客户本月销售额", &w));
        assert!(!has_residue("本月客户销售额排行前十", &w));
        // 长词优先剥离：不因先剥"客户"而在"客户分类"上留下"分类"
        let w2: Vec<String> = ["销售额", "客户", "客户分类"].iter().map(|s| s.to_string()).collect();
        assert!(!has_residue("本月客户分类销售额", &w2));
    }

    // ── 规则时间解析（SuperSonic TimeRangeParser 思路）──
    fn tp(q: &str) -> String {
        time_predicate(q).unwrap_or_else(|| panic!("未解析: {q}"))
    }

    #[test]
    fn time_recent_n_with_cn_numbers() {
        // 「近 N 天」含今天 = N 个自然日：起点回推 N-1 天（修前回推 N 天 → N+1 天）
        assert!(tp("近7天销售额").contains("INTERVAL 6 DAY"));
        assert!(tp("最近三个月销售额").contains("INTERVAL 3 MONTH"));
        assert!(tp("过去两周订单数").contains("INTERVAL 2 WEEK"));
        assert!(tp("近十天销量").contains("INTERVAL 9 DAY"));
        assert!(tp("最近十五天销售额").contains("INTERVAL 14 DAY"));
    }

    #[test]
    fn time_quarter_and_half_year() {
        assert!(tp("第二季度销售额").contains("-04-01"));
        assert!(tp("三季度销售额").contains("-07-01"));
        assert!(tp("上半年销售额").contains("-01-01"));
        assert!(tp("下半年销售额").contains("-07-01"));
    }

    #[test]
    fn time_explicit_month() {
        assert!(tp("6月销售额").contains("-06-01"));
        assert!(tp("十二月销量").contains("-12-01"));
        // 「上个月/本月」不得被当成 N 月解析
        assert!(tp("上个月销售额").contains("INTERVAL 1 MONTH"));
        assert!(tp("本月销售额").contains("%Y-%m-01"));
    }

    #[test]
    fn time_relative_words() {
        assert!(tp("今天销售额").contains("CURDATE()"));
        assert!(tp("前天订单数").contains("INTERVAL 2 DAY"));
        assert!(tp("上周销售额").contains("YEARWEEK"));
        assert!(tp("去年销售额").contains("YEAR(CURDATE()) - 1"));
        assert!(time_predicate("销售额是多少").is_none(), "无时间词不得臆造时间窗");
    }

    // ─────────── 构建期口径守卫（裁决 二·J2 的修法）───────────
    // 声明层（`meta.table_scope`）今天只对 LLM 路径强制（`check_caliber` → 判红 → 回炉）。
    // 确定性路径刻意不跑 grader —— 裁决 二·G 的理由是「compose 的 SQL 就是按同一批声明装配的，
    // 判红只说明装配器与校验器理解不一致」。**那个前提对硬编码模板不成立**：本文件的模板
    // 早于声明层存在，从来不读 `table_scope`。而运行时给 0-LLM 路径加 grader 是错的修法
    // （会把「回炉改坏对的 SQL」的风险引进确定性路径），所以校验放在**构建期**：
    // 模板产出的 SQL → 喂种子声明 → 断言零违规。零运行时成本、无回炉副作用。

    /// 种子 `TABLE_SCOPES` → `RequireCols` 判据。生产侧是 `registry::caliber::rules_from`
    /// 从 `meta.table_scope` 造的，而那张表由这同一组种子灌 —— 声明是同一份。
    /// 切列名（顶层 AND 切开、每段取首标识符）与 `registry::caliber::cols_of_filter`
    /// **同判据**：那个函数今天是私有的，放开后这里直接调它、删掉这三行。
    fn scope_rules() -> Vec<dms_kernel::CaliberRule> {
        dms_semantic::seed::TABLE_SCOPES
            .iter()
            .map(|(t, filter, note)| dms_kernel::CaliberRule::RequireCols {
                table: t.to_string(),
                cols: dms_kernel::sql::lex::split_top_and(filter)
                    .iter()
                    .filter_map(|c| dms_kernel::sql::lex::first_ident_of(c))
                    .collect(),
                human: note.to_string(),
            })
            .collect()
    }

    /// 同一批声明喂给装配器（`compose_sql_with` 的 `table_scopes` 形参形状）
    fn seed_scopes() -> Vec<(String, String)> {
        dms_semantic::seed::TABLE_SCOPES
            .iter()
            .map(|(t, f, _)| (t.to_string(), f.to_string()))
            .collect()
    }

    /// 一条 SQL 的违规名清单（排序，便于逐条比对）。
    /// 🔴 先断言**解析得动**：`check_caliber` 对解析失败一律返回空（刻意的漏判方向），
    /// 少了这一句，模板里任何 sqlparser 吃不下的写法都会让整条守卫静默变成恒真 ——
    /// 本项目已四次踩「判据入参变空 → 断言恒真 → 报告只显示绿」。
    fn caliber_of(sql: &str) -> Vec<String> {
        assert!(
            dms_kernel::sql::caliber::output_shape(sql).is_some(),
            "解析不动 → check_caliber 恒返空 → 守卫恒真：{sql}"
        );
        let mut v: Vec<String> =
            dms_kernel::check_caliber(sql, &scope_rules()).into_iter().map(|x| x.rule).collect();
        v.sort();
        v
    }

    /// 🔴 每一个确定性模板产出的 SQL 都必须满足表级声明（零违规）。
    /// 两处今天**不满足**的钉在下一条断言里 —— 不许在这里被顺手放宽。
    #[test]
    fn deterministic_templates_satisfy_table_scopes() {
        // DWS 事实查询由单表 builder 生成，不需要旧 MySQL 表级口径规则。
        for q in ["本月按省区销售额", "销售额前5的客户", "本月按商品销售额", "各月销售额趋势"] {
            let sql = sales_breakdown(q).unwrap().sql;
            assert!(sql.contains(dms_semantic::sales_fact::TABLE), "{q} → {sql}");
            assert!(!sql.contains(" JOIN ") && !sql.contains("UNION ALL"), "{q} → {sql}");
        }
        // 高频订单聚合模板：三个订单口径指标分支 + 各自的上期 SQL。
        for q in ["今天有多少订单数", "本月客单价是多少", "本月成交客户数是多少"] {
            let h = agg_template(q).unwrap();
            let v = caliber_of(&h.sql);
            assert!(v.is_empty(), "{q} → {v:?}");
            let (prev, _) = h.prev.expect("三条问句都带时间词，上期 SQL 必在");
            assert!(caliber_of(&prev).is_empty(), "{q} 上期");
        }
        // 组合器的典型装配（无去重键的单头指标 × JOIN 维度）：装配与校验吃同一批声明
        let sql = compose_sql_with(
            &sales_metric(),
            &dim("省份", "COALESCE(NULLIF(cus.province,''),'未知')"),
            "本月销售额按省份",
            &edges(),
            &seed_scopes(),
        )
        .unwrap();
        assert!(caliber_of(&sql).is_empty(), "{sql}");
    }

    /// 🔴 单号直查**今天不满足** `t_sales_order` 的表级声明。不改（要改的是出给用户的数字，
    /// 属业务裁决）—— 断言把现状钉死：改它必须是有意的，且要同时改掉这条断言。
    ///
    /// 另一处（组合器去重子查询丢基表表级口径）**已修**：表级口径与指标口径一起下推进子查询。
    /// 那处是构建期守卫抓到的，且是确定性 0-LLM 路径上的真错数（软删明细行被算进销量），
    /// 与「单号直查该不该带有效订单口径」不同 —— 后者是刻意的（作废单也必须查得到）。
    #[test]
    fn the_doc_lookup_gap_is_pinned_not_quietly_passed() {
        // ① 单号直查按主号查一张单，不带「有效订单」口径。这是**刻意**的（作废单 199 也
        //    必须查得到单据卡），而 `t_sales_order` 的表级声明写的是「任何查询触及都恒需」——
        //    二者矛盾，需 DMS 团队裁决「表级声明是否该把单据卡排除在外」。
        let doc = sniff_doc_code("帮我查下 HJXH-DXO2026072300384 这张单", false).unwrap();
        assert_eq!(caliber_of(&doc.sql), ["require_cols:t_sales_order"]);
        // 另两种单据的表没有表级声明 → 一条不判（声明缺失 ≠ 违规）
        for c in ["HJXH-DRO2026072300047", "HJXH-DZD20261230000261"] {
            assert!(caliber_of(&sniff_doc_code(c, false).unwrap().sql).is_empty(), "{c}");
        }
        // 未证明权限的单据族（2026-08-06 裁决，六族之外一律不产生产 SQL）：
        // 识别层仍认得（`resolve_code` 分类正确），但 sniff 返 None，由 business_lookup 终止为无数据结果。
        for c in ["SPC-20260718-8", "CG2603090123"] {
            assert!(sniff_doc_code(c, false).is_none(), "{c} 未证明族不得产生产查询");
            assert!(dms_semantic::document::resolve_code(c, false).is_some(), "{c} 识别层仍应认得");
        }
    }

    /// 【单据卡】单号族识别 + 明细绑定（真库前缀 2026-08-02 探得）：
    /// 每个真实前缀 → 对的头表 + 对的明细表；英文词不许撞短码前缀。
    #[test]
    fn doc_families_bind_header_and_detail() {
        // 六族带明细：头号列即明细号列（全部真库 SHOW COLUMNS 坐实）
        let with_detail = [
            ("HJXH-DXO2026072300384", "t_sales_order", "t_sales_order_detail", "sales_order_code"),
            ("HJXH-DSO2026010100001", "t_sales_order", "t_sales_order_detail", "sales_order_code"),
            ("HJXH-DRO2026072300047", "t_after_sales_order_header", "t_after_sales_order_detail", "after_sales_code"),
            ("HJXH-DZD20261230000261", "t_account_bill_header", "t_account_bill_detail", "bill_code"),
            ("IO2025123456", "t_invoice_apply_header", "t_invoice_apply_detail", "invoice_code"),
            ("SQ2026052345", "t_invoice_new_apply_header", "t_invoice_new_apply_detail", "invoice_code"),
        ];
        for (code, ht, dt, col) in with_detail {
            let h = sniff_doc_code(code, false).unwrap_or_else(|| panic!("{code} 没识别"));
            assert_eq!(h.route, "direct-doc");
            assert!(h.sql.contains(ht) && h.sql.contains(col), "{code} → {}", h.sql);
            let d = h.detail.as_deref().unwrap_or_else(|| panic!("{code} 缺明细"));
            assert!(d.contains(dt) && d.contains(col), "{code} 明细 → {d}");
        }
        // 两族调拨的生产数据范围未证明（2026-08-06 裁决）：识别但两条源都不产 SQL
        for code in ["CG2603090123", "SPC-20260718-8"] {
            assert!(sniff_doc_code(code, false).is_none(), "{code} 未证明族不得产生产查询");
            assert!(sniff_doc_code(code, true).is_none(), "{code} 数仓同样不产查询");
        }
        // 设备需求单注册了收货+投放两类明细；DirectHit 单条补充 SQL 只取第一张，
        // 生产 business-lookup 会按注册表逐表点查两张明细。
        for code in ["HJXH_XQ20260101001", "DEV_XQ202608040001"] {
            let h = sniff_doc_code(code, false).unwrap_or_else(|| panic!("{code} 没识别"));
            let d = h.detail.as_deref().unwrap_or_else(|| panic!("{code} 缺设备明细"));
            assert!(d.contains("t_device_receive_item") && d.contains("requisition_code"), "{d}");
            assert!(h.sql.contains("t_device_requisition") && h.sql.contains("requisition_code"), "{}", h.sql);
        }
        // 英文词不撞短码前缀（IO/SQ/CG 后必须 ≥6 位纯数字）
        for bad in ["INFOABC", "SQLEET", "CGABCDE", "IO123"] {
            assert!(sniff_doc_code(bad, false).is_none(), "{bad} 被误认成单号");
        }
        // 下划线需求单能过字符集闸（HJXH_XQ 是下划线变体）
        assert!(sniff_doc_code("查 HJXH_XQ20260101001 这单", false).is_some());
    }

    /// 从 direct 私有表归并到 semantic 注册表的五族单据：格式分类必须窄（识别层）。
    /// 2026-08-06 权限裁决：这五族生产权限未证明、数仓缺表 —— 注册表只负责**识别**，
    /// 两条源都不产 SQL（`production=None` / `warehouse=None`），路由由 business_lookup
    /// 终止为无数据结果，绝不回落成宽查询。
    #[test]
    fn semantic_registry_families_classify_narrowly_but_stay_fail_closed() {
        use dms_semantic::document::{resolve_code, DocumentKind};
        let families = [
            ("SHOP_YH20260805100001", DocumentKind::ShopRequisition),
            ("SHOP_TH20260805100002", DocumentKind::ShopReturn),
            ("PZ20260805100003", DocumentKind::Voucher),
            ("SHOP_TZ20260805100004", DocumentKind::StockAdjustment),
            ("SHOP_PH20260805100005", DocumentKind::ShopShipment),
        ];
        for (code, kind) in families {
            assert_eq!(resolve_code(code, false).map(|x| x.family.kind), Some(kind), "{code} 分类");
            for warehouse in [false, true] {
                assert!(
                    sniff_doc_code(&format!("请查{code}这张单"), warehouse).is_none(),
                    "{code} 未证明族不得产 SQL（warehouse={warehouse}）"
                );
            }
        }
    }

    #[test]
    fn semantic_document_classifier_rejects_near_misses_and_keeps_registered_families() {
        // 日历日期、纯数字流水与最短流水三层都要成立；未知前缀继续回落，不猜表。
        for bad in [
            "SHOP_YH20261301100001",
            "SHOP_TH20260230100001",
            "SHOP_PH20260805ABC001",
            "SHOP_TZ20260805123",
            "PZ202608051234",
            "SHOP_XX20260805100001",
            "SHOPPING20260805100001",
        ] {
            assert!(sniff_doc_code(bad, false).is_none(), "{bad} 被误认成业务单号");
        }
        // 大小写归一化（识别层；PZ 族权限未证明，不产 SQL）；既有已证明族仍优先、行为不变。
        let pz = dms_semantic::document::resolve_code("pz20260805100003", false)
            .expect("小写单号应归一化");
        assert_eq!(pz.code, "PZ20260805100003");
        assert_eq!(pz.family.kind, dms_semantic::document::DocumentKind::Voucher);
        assert!(sniff_doc_code("pz20260805100003", false).is_none(), "未证明族不产生产 SQL");
        let sales = sniff_doc_code("查HJXH-DXO2026072300384这张单", false).expect("既有销售单回归");
        assert!(sales.sql.contains("t_sales_order") && sales.sql.contains("sales_order_code"), "{}", sales.sql);
    }

    #[test]
    fn registered_doc_classifier_rejects_malformed_modern_codes_and_mysql_split_aliases() {
        for bad in [
            "HJXH-DXO202613010001",
            "HJXH-DSO202602300001",
            "HJXH-DRO20260805ABC",
            "HJXH-DZD20260805123X",
            "HJXH_XQ20260229001",
            "DEV_XQ_IDEM_001",
        ] {
            assert!(sniff_doc_code(bad, false).is_none(), "{bad} 被误认成单号");
        }

        let split = "HJXH-DSO2026080400071_2";
        assert!(sniff_doc_code(split, false).is_none(), "Doris 拆单号不应查询 MySQL 销售主表");
        let warehouse = sniff_doc_code(split, true).expect("Doris 路径应识别拆单号");
        assert!(warehouse.sql.contains("sales_dw.dws_fin_shipment_check_dnf"));

        let regular_warehouse =
            sniff_doc_code("HJXH-DSO2026080400071", true).expect("Doris 路径应保留普通销售单");
        assert!(regular_warehouse.sql.contains("dms_ods.t_sales_order"));

        for good in [
            "HJXH-DXO202606130001",
            "HJXH-DRO2026010500031",
            "HJXH-DZD20261230000261",
            "HJXH_XQ20260101001",
            "DEV_XQ001",
        ] {
            assert!(sniff_doc_code(good, false).is_some(), "{good} 应保留识别");
        }
    }

    #[test]
    fn warehouse_missing_document_tables_fail_closed() {
        for code in [
            "SHOP_YH20260805100001",
            "SHOP_PH20260805100005",
            "SHOP_TH20260805100002",
            "PZ20260805100003",
            "SHOP_TZ20260805100004",
        ] {
            assert!(sniff_doc_code(code, true).is_none(), "数仓缺表不得生成伪查询：{code}");
        }
    }

    #[test]
    fn warehouse_split_order_maps_back_to_dms_order() {
        for code in ["HJXH-DSO2026073100764*5", "HJXH-DSO2026080400071_2"] {
            let h = sniff_doc_code(&format!("查询 {code}"), true).unwrap();
            assert!(h.sql.contains("sales_dw.dws_fin_shipment_check_dnf"), "{}", h.sql);
            assert!(h.sql.contains("DMS销售单号") && h.sql.contains("金额差异"), "{}", h.sql);
            let d = h.detail.unwrap();
            assert!(d.contains("t_sales_order_detail") && d.contains("商品名称"), "{d}");
            assert!(d.contains("GROUP BY d.id"), "数仓对账多行会放大订单明细：{d}");
        }
        assert_eq!(
            dms_semantic::document::resolve_code("HJXH-DSO2026080400071", true).unwrap().family.name,
            "销售订单"
        );
    }

    #[test]
    fn warehouse_contract_uses_only_registered_full_table_names() {
        for code in ["HJXH-DZD20261230000261", "CG2603090123"] {
            assert!(sniff_doc_code(code, true).is_none(), "{code} 应交给 business-lookup 单表轻查询");
        }
        for (code, table) in [
            ("IO2025123456", "dms_ods.t_invoice_apply_header"),
            ("SQ2026052345", "dms_ods.t_invoice_new_apply_header"),
        ] {
            let hit = sniff_doc_code(code, true).unwrap_or_else(|| panic!("{code} 未识别"));
            assert!(hit.sql.contains(table), "{}", hit.sql);
            assert!(!hit.sql.contains("SELECT *") && !hit.sql.contains(" JOIN "), "{}", hit.sql);
        }
    }

    /// 🔴 去重子查询必须**同时**下推指标口径与表级口径 —— 这条是修复的锁。
    ///
    /// 修之前：`inner_where` 只有指标自己的 `scope_filter`，而外层补表级口径的循环
    /// 又因为基表已被派生表替换（`from_table_aliases` 看不见括号里的表名）而跳过它 ——
    /// 明细表的 `deleted_flag = 0` **两头都漏**，软删的明细行被算进销量。
    /// 这是确定性 0-LLM 路径，连回炉的机会都没有；构建期守卫抓到的正是它。
    #[test]
    fn dedup_subquery_pushes_down_both_calibers() {
        let sql =
            compose_sql_with(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges(), &seed_scopes())
                .unwrap();
        // 零违规（此前是 ["require_cols:t_sales_order_detail"]）
        assert!(caliber_of(&sql).is_empty(), "{sql}");
        // 两条口径都在子查询里，且顺序是「指标口径 AND 表级口径」
        assert!(
            sql.contains("WHERE item_type = '1' AND deleted_flag = 0)"),
            "指标口径与表级口径必须一起下推: {sql}"
        );
    }

    /// 默认销售额只有 DWS 合同这一条真相源；旧发货 UNION 与未验证维度不得复活。
    #[test]
    fn sales_breakdown_is_pinned_to_the_verified_dws_contract() {
        for question in [
            "本月各二级分类销售额",
            "本月销售额按品牌",
            "本月销售额按门店",
            "本月销售额按业务员",
            "本月销售额按区域经理",
            "本月销售额按客户分类",
            "本月各品牌各省份的销售额",
        ] {
            assert!(sales_breakdown(question).is_none(), "未经验证的维度不可猜测：{question}");
        }
    }

    // ── direct-derive（合同未覆盖 → ODS 推导降级）──

    /// 触发面 = 全部「不可计算」卡，且只有卡：合同内的正常命中绝不进推导。
    /// 这是 fail-closed 顺序的第一钉：「合同在就永远走合同」。
    #[test]
    fn derive_triggers_on_every_unavailable_card_and_only_there() {
        for (question, warehouse) in [
            ("本月销售额按门店", true),     // 维度未覆盖
            ("本月退货销售额", true),       // 语义未覆盖
            ("本月订单销售额", true),       // 事件语义未覆盖
            ("本月专票开了多少金额", true), // 开票事实缺失
            ("待确认对账单有多少", true),   // 对账事实缺失
            ("本月销售额是多少", false),    // 非数仓源的销售指标卡
        ] {
            let hit = try_direct_for(question, warehouse)
                .unwrap_or_else(|| panic!("应产出「不可计算」卡：{question}"));
            assert!(is_unavailable_card(&hit), "{question}: {}", hit.sql);
        }
        // 合同内命中（数仓源）不是卡 → 推导不会出手
        let ok = try_direct_for("本月销售额是多少", true).expect("合同内问句");
        assert!(!is_unavailable_card(&ok), "{}", ok.sql);
        assert_eq!(ok.route, "direct-agg");
        // 「未确认限定」卡是不可计算卡的子集：先走客户主档合同探查，合同仍不接才轮到推导
        let vague = try_direct_for("嗨肉本月销售额", true).expect("未确认限定卡");
        assert!(vague.sql.contains("'未确认限定'"), "{}", vague.sql);
        assert!(is_unavailable_card(&vague));
    }

    /// route 值契约：审计（query_log.route）与前端徽标都认它；必须在 agent 的白名单里，
    /// 且不与任何既有 route 撞车（撞了审计就分不开两种答案）。
    #[test]
    fn derive_route_is_whitelisted_and_distinct() {
        assert_eq!(DERIVE_ROUTE, "direct-derive");
        assert!(
            crate::ROUTE_LABELS.contains(&DERIVE_ROUTE),
            "direct-derive 不在 ROUTE_LABELS 白名单里 —— 审计分不清推导与合同答案"
        );
        for existing in ["direct-agg", "direct-doc", "llm", "llm+repair", "semantic-cache", "graph"] {
            assert_ne!(DERIVE_ROUTE, existing);
        }
    }

    /// 用表硬校验：候选集内的表（裸名/正确限定名/CTE 包装）放行；
    /// 候选外表、错误库限定、DWS 汇总表、零实表、解析失败一律拒（= 回落原卡）。
    #[test]
    fn derive_sql_may_only_reference_candidate_tables() {
        let d = &dms_kernel::MysqlDialect;
        let allowed = &["t_sales_order", "t_master_shop"];
        assert!(derive_tables_allowed(
            "SELECT s.shop_name FROM dms_ods.t_master_shop s",
            allowed,
            d
        ));
        assert!(derive_tables_allowed(
            "SELECT o.customer_code FROM t_sales_order o \
             JOIN dms_ods.t_master_shop s ON s.customer_code = o.customer_code",
            allowed,
            d
        ));
        // CTE 名不算实表，但 CTE 内部读的表照样校
        assert!(derive_tables_allowed(
            "WITH x AS (SELECT customer_code FROM dms_ods.t_sales_order) SELECT * FROM x",
            allowed,
            d
        ));
        // 候选外表 / 错误库限定 / 合同层汇总表都拒
        assert!(!derive_tables_allowed("SELECT * FROM dms_ods.t_goods", allowed, d));
        assert!(!derive_tables_allowed("SELECT * FROM sales_dw.t_sales_order", allowed, d));
        assert!(!derive_tables_allowed(
            "SELECT * FROM sales_dw.dws_off_offline_sale_dfn",
            allowed,
            d
        ));
        // 零实表与解析失败同样拒（过不了解析的 SQL 留着也过不了闸门，早判早回落）
        assert!(!derive_tables_allowed("SELECT 1", allowed, d));
        assert!(!derive_tables_allowed("SELEC broken", allowed, d));
    }

    /// 🔴 接线钉点（源码扫描 —— 全链路要 PG/LLM/数仓，无库测不了）：
    /// ① 推导只在「不可计算」卡之后出手（合同优先，顺序不颠倒）；
    /// ② 回落是 `or(Some(hit))` —— 原卡一字不改，不是重新拼一张；
    /// ③ 推导 SQL 过与直连同一个 `gate_on` + 同一组 `MAX_ROWS`/`EXEC_TIMEOUT`；
    /// ④ 用表硬校验在闸门**之前**（越界表连闸门都不必见）。
    #[test]
    fn derive_is_wired_after_the_card_with_verbatim_fallback_and_same_gate() {
        let src = DETERMINISTIC_SRC;
        // ①② direct_hit 的两个卡臂：未确认限定（先客户主档合同、再推导）与普通卡（直接推导），
        //    两个臂的回落都是 or(Some(hit)) —— 删掉任一个，推导失败就跌进 LLM 全目录路径
        let wire = body_between(src, "pub fn direct_hit<", "// ─────────── ODS 推导降级");
        let contract_pos = wire.find("customer_filtered_sales(cx).await").expect("客户主档合同探查没了");
        let derive_pos = wire.find("ods_derive(cx).await").expect("卡臂没接推导");
        assert!(contract_pos < derive_pos, "合同（客户主档探查）必须先于推导");
        assert_eq!(
            wire.matches("ods_derive(cx).await.or(Some(hit))").count(),
            2,
            "两个卡臂都必须「推导失败回落原卡」：{wire}"
        );
        // ③④ 推导本体（ods_derive 两轮壳 + derive_attempt 单轮体）：
        //    候选校验 → 闸门 → 预执行在 derive_attempt 里，顺序即行为
        let body = body_between(src, "async fn derive_attempt(", "\nfn customer_name_fragment(");
        let allow = body.find("derive_tables_allowed").expect("用表硬校验没了");
        let gate = body.find("crate::gate_on").expect("推导必须过与直连同一个 gate_on");
        assert!(allow < gate, "用表硬校验必须在闸门之前：{body}");
        assert!(body.contains("crate::MAX_ROWS") && body.contains("crate::EXEC_TIMEOUT"),
                "行上限/超时不许另搞一套：{body}");
        // 预执行（fetch）必须在 DeriveTry::Hit 之前 —— 执行失败/零行都不许产出命中
        let fetch = body.find("cx.source.fetch").expect("预执行没了");
        let hit = body.find("DeriveTry::Hit(candidate)").expect("命中构造没了");
        assert!(fetch < hit, "必须先预执行成功才许产出推导命中：{body}");
        let shell = body_between(src, "async fn ods_derive(", "async fn derive_attempt(");
        assert!(shell.contains("hit(sql, DERIVE_ROUTE)"), "命中必须带 direct-derive route：{shell}");
        assert!(shell.contains("DeriveTry::Empty") && shell.contains("tried.extend"),
                "空结果必须剔除试过的表再来一轮：{shell}");
        // ⑤ 两道语义闸：用表校验 → 别名对账 → JOIN 证据 → gate_on，顺序即行为；
        //    语料必须来自 schema_card_with_columns（与卡同一次取数，不多查一遍 column_doc）
        assert!(shell.contains("schema_card_with_columns"), "语料必须与卡同源：{shell}");
        assert!(!body.contains("recall::schema_card(") && !shell.contains("recall::schema_card("),
                "不许绕开语料单列的卡接口");
        let labels = body.find("derive_labels_ungrounded").expect("闸 1·别名对账没了");
        let joins = body.find("join_evidence_edges").expect("闸 2·JOIN 证据取数没了");
        assert!(allow < labels && labels < joins && joins < gate,
                "两闸必须在执行闸门之前、用表校验之后：{body}");
    }

    // ── 两道语义闸的钉点（判官 E 系列裁决，2026-08-09）──

    /// 语料夹具：(表, [(列, 注释)]) —— 与 schema 卡带出的列语料同形态。
    fn corpus(tables: &[(&str, Vec<(&str, &str)>)]) -> Vec<(String, Vec<(String, String)>)> {
        tables
            .iter()
            .map(|(t, cols)| {
                (t.to_string(), cols.iter().map(|(c, m)| (c.to_string(), m.to_string())).collect())
            })
            .collect()
    }

    fn shape_of(sql: &str) -> DeriveShape {
        analyze_derive_sql(sql, &dms_kernel::MysqlDialect).expect("钉点 SQL 必须能解析")
    }

    fn edge(lt: &str, lc: &str, rt: &str, rc: &str) -> dms_semantic::recall::JoinEvidenceRow {
        dms_semantic::recall::JoinEvidenceRow {
            left_table: lt.into(),
            left_col: lc.into(),
            right_table: rt.into(),
            right_col: rc.into(),
        }
    }

    /// 闸 1 拒：E05/E08/E15 原型 —— `amount`（明细金额）别名「开票金额」，在
    /// t_sales_order_detail 全表列注释里无出处 → 拒。E18 原型：`created_by`（创建人）
    /// 别名「业务员」—— 码值劫走 → 拒。
    #[test]
    fn derive_gate1_rejects_relabeled_metrics_and_hijacked_codes() {
        let detail = corpus(&[("t_sales_order_detail", vec![
            ("amount", "明细金额（应付金额）"),
            ("created_by", "创建人"),
            ("sku_code", "商品编码"),
        ])]);
        // E05：开票金额 = amount 改名（「开票金额」与「明细金额（应付金额）」互不为子串）
        let s = shape_of(
            "SELECT SUM(d.amount) AS `开票金额` FROM dms_ods.t_sales_order_detail d \
             WHERE d.deleted_flag = 0",
        );
        assert_eq!(derive_labels_ungrounded(&s, &detail, &[]).as_deref(), Some("开票金额"));
        // 即便给了注册指标清单（开票金额登记在 t_invoice_apply_header 系，不在取数表），
        // 通道②也不许放行 —— 指标必须回自己的源表。
        let m = vec![("开票金额".to_string(), "t_invoice_apply_header UNION ALL t_invoice_new_apply_header".to_string())];
        assert_eq!(derive_labels_ungrounded(&s, &detail, &m).as_deref(), Some("开票金额"),
                   "注册指标的源表不是取数表时不许放行");
        // E18：业务员 = created_by 码值（「业务员」与「创建人」互不为子串）
        let s = shape_of(
            "SELECT d.created_by AS `业务员` FROM dms_ods.t_sales_order_detail d GROUP BY d.created_by",
        );
        assert_eq!(derive_labels_ungrounded(&s, &detail, &[]).as_deref(), Some("业务员"));
    }

    /// 通道③：核心销售口径词允许映射到度量列（销售额←total_amount）；非核心词不放行
    #[test]
    fn derive_gate1_core_sales_word_maps_to_measure_column() {
        let ods = corpus(&[("t_sales_order", vec![
            ("total_amount", "订单总金额"),
            ("order_status", "订单状态"),
        ])]);
        let s = shape_of(
            "SELECT SUM(t.total_amount) AS `销售额` FROM dms_ods.t_sales_order t WHERE t.deleted_flag = 0",
        );
        assert!(derive_labels_ungrounded(&s, &ods, &[]).is_none(),
                "销售额←total_amount 是合同覆盖外的合法推导映射");
        // 非核心词（返利率）就算表里有度量列也不许捏造
        let s = shape_of("SELECT SUM(t.total_amount) AS `返利率` FROM dms_ods.t_sales_order t");
        assert_eq!(derive_labels_ungrounded(&s, &ods, &[]).as_deref(), Some("返利率"));
        // 表里没有度量列时，核心词也不许放行
        let no_measure = corpus(&[("t_region", vec![("region_name", "省区名称")])]);
        let s = shape_of("SELECT COUNT(*) AS `销售额` FROM dms_ods.t_region");
        assert_eq!(derive_labels_ungrounded(&s, &no_measure, &[]).as_deref(), Some("销售额"));
    }

    /// 闸 1 过：判官给的正例对照 —— 「销售额」⊂「销售额(元)」、store_name 注释含「门店」、
    /// 「品牌」⊂「品牌名称」。含裸列（无限定符）单表归属与常数占位列跳过。
    #[test]
    fn derive_gate1_accepts_labels_grounded_in_column_comments() {
        let winc = corpus(&[("t_winc_sale_report", vec![
            ("sale_amount", "销售额(元)"),
            ("store_name", "客户门店名称"),
        ])]);
        let s = shape_of(
            "SELECT w.store_name AS `门店`, SUM(w.sale_amount) AS `销售额` \
             FROM dms_ods.t_winc_sale_report w GROUP BY w.store_name",
        );
        assert!(derive_labels_ungrounded(&s, &winc, &[]).is_none(), "门店/销售额必须有出处");
        // 裸列（无限定符）单表：全归该表
        let s = shape_of("SELECT store_name AS `门店` FROM dms_ods.t_winc_sale_report GROUP BY store_name");
        assert!(derive_labels_ungrounded(&s, &winc, &[]).is_none());
        // 常数占位列（'不可计算' AS 数据状态）不算取数别名：整张不可计算卡都能过闸 1
        let s = shape_of(
            "SELECT '不可计算' AS `数据状态`, '销售额' AS `指标` FROM dms_ods.t_dict_value LIMIT 1",
        );
        assert!(s.labeled.is_empty(), "字面量投影不许进对账：{:?}", s.labeled);
        assert!(derive_labels_ungrounded(&s, &[], &[]).is_none());
        // ASCII 别名不需要对账（列名形态，没有「改名」空间）
        let s = shape_of("SELECT w.sale_amount AS total FROM dms_ods.t_winc_sale_report w");
        assert!(s.labeled.is_empty(), "{:?}", s.labeled);
    }

    /// 时间桶别名豁免：「月份」经 DATE_FORMAT 派生 → 不进闸 1 对账；
    /// 但「销售额」挂在 DATE_FORMAT 上也不许蒙混（词表精确匹配守着），
    /// 裸写「月份」不调日期函数同样不许（防只挂时间词的虚构）。
    #[test]
    fn derive_gate1_time_bucket_alias_exemption() {
        let corpus = corpus(&[("t_winc_sale_report", vec![
            ("stat_date", "统计日期"),
            ("sale_amount", "销售额(元)"),
        ])]);
        let s = shape_of(
            "SELECT DATE_FORMAT(t.stat_date,'%Y-%m') AS `月份`, SUM(t.sale_amount) AS `销售额`              FROM dms_ods.t_winc_sale_report t GROUP BY 1 ORDER BY 1",
        );
        assert!(s.time_derived.contains(&"月份".to_string()), "{:?}", s.time_derived);
        assert!(!s.labeled.iter().any(|(l, _)| l == "月份"), "月份不该进闸 1 对账");
        assert!(derive_labels_ungrounded(&s, &corpus, &[]).is_none(), "销售额有出处 + 月份豁免 → 过闸");
        // 指标别名挂日期函数 ≠ 时间桶：词表精确匹配守着
        let s = shape_of("SELECT DATE_FORMAT(t.stat_date,'%Y-%m') AS `销售额` FROM dms_ods.t_winc_sale_report t");
        assert!(s.time_derived.is_empty(), "销售额不是时间词");
        // 时间词但没调日期函数：不豁免
        let s = shape_of("SELECT t.stat_date AS `月份` FROM dms_ods.t_winc_sale_report t");
        assert!(s.time_derived.is_empty(), "没调日期函数的时间词不豁免");
    }

    /// 闸 1 归属按表别名：别名只在它**实际取数**的那张表的语料里找 —— 跨表借出处不许放行。
    #[test]
    fn derive_gate1_attributes_labels_to_the_table_they_read_from() {
        let both = corpus(&[
            ("t_winc_sale_report", vec![("sku_code", "商品编码"), ("sale_amount", "销售额(元)")]),
            ("t_goods", vec![("brand_name", "品牌名称")]),
        ]);
        // 「品牌」取自 winc 的 sku_code（商品编码）→ 无出处，拒
        let s = shape_of(
            "SELECT w.sku_code AS `品牌` FROM dms_ods.t_winc_sale_report w \
             JOIN dms_ods.t_goods g ON w.sku_code = g.goods_code",
        );
        assert_eq!(derive_labels_ungrounded(&s, &both, &[]).as_deref(), Some("品牌"));
        // 「品牌」取自 t_goods.brand_name（品牌名称）→ 有出处，过
        let s = shape_of(
            "SELECT g.brand_name AS `品牌` FROM dms_ods.t_winc_sale_report w \
             JOIN dms_ods.t_goods g ON w.sku_code = g.goods_code",
        );
        assert!(derive_labels_ungrounded(&s, &both, &[]).is_none());
    }

    /// 闸 2 拒：E09 原型 —— `sku_code = goods_code` 的 joinable 置信度只有 0.35，
    /// 取数侧已滤掉，证据集里没有 → 拒。过：同一 JOIN 命中合同边（裸名）、
    /// 命中限定名形态（datamap 归一）、命中反向存储的边。键列不同的边不算数。
    #[test]
    fn derive_gate2_requires_evidence_for_every_join_key() {
        let s = shape_of(
            "SELECT g.brand_name AS `品牌`, SUM(w.sale_amount) AS `销售额` \
             FROM dms_ods.t_winc_sale_report w LEFT JOIN dms_ods.t_goods g \
             ON w.sku_code = g.goods_code GROUP BY g.brand_name",
        );
        assert_eq!(s.join_pairs.len(), 1, "{:?}", s.join_pairs);
        assert_eq!(s.unevidenced_joins, 0);
        // E09：证据集没有这对键 → 拒
        assert!(derive_joins_unevidenced(&s, &[]).is_some());
        // 合同边（join_edge 裸名形态）→ 过
        assert!(derive_joins_unevidenced(&s, &[edge("t_winc_sale_report", "sku_code", "t_goods", "goods_code")]).is_none());
        // 反向 + 限定名（datamap 形态）→ 过
        assert!(derive_joins_unevidenced(&s, &[edge("dms_ods.t_goods", "goods_code", "dms_ods.t_winc_sale_report", "sku_code")]).is_none());
        // 键列不同的边不算证据 → 拒
        assert!(derive_joins_unevidenced(&s, &[edge("t_winc_sale_report", "customer_code", "t_goods", "goods_code")]).is_some());
    }

    /// 闸 2：无等值关联键的 JOIN（CROSS/USING/两端解析不出）一律算无证据；
    /// 无 JOIN 的单表推导对闸 2 无感（空证据集也放行）。
    #[test]
    fn derive_gate2_rejects_joins_without_equality_keys() {
        let s = shape_of("SELECT * FROM dms_ods.t_winc_sale_report w CROSS JOIN dms_ods.t_goods g");
        assert!(s.unevidenced_joins > 0);
        assert!(derive_joins_unevidenced(&s, &[]).is_some());
        // ON 里只有同表条件 / 非等值条件：同样没有跨表键
        let s = shape_of(
            "SELECT * FROM dms_ods.t_goods g JOIN dms_ods.t_goods_category c \
             ON g.goods_name <> c.category_name",
        );
        assert!(s.unevidenced_joins > 0, "{:?}", s.join_pairs);
        let s = shape_of("SELECT w.store_name AS `门店` FROM dms_ods.t_winc_sale_report w");
        assert!(derive_joins_unevidenced(&s, &[]).is_none());
    }

    /// 闸 2 取数侧的纪律钉点（源码扫描）：一次 PG 查询、两源合并、候选表限定、
    /// 置信度/人工确认两个放行档、ds 限定（drift 守卫另有逐字守）。
    #[test]
    fn derive_gate2_evidence_fetch_is_one_scoped_query() {
        let src = include_str!("../../../semantic/src/recall/ods.rs");
        let body = src
            .split("pub async fn join_evidence_edges(")
            .nth(1)
            .expect("join_evidence_edges 没了")
            .split("\n/// ")
            .next()
            .expect("函数边界没了");
        // 取数已收口到 `fetch_or_empty`（读失败留痕返空集）：函数体里不再有 `.fetch_all(`，
        // 「一次取完」钉的是「恰好一次取数调用、且走的是留痕收口」
        assert_eq!(body.matches("fetch_or_empty(").count(), 1, "证据边必须一次取完：{body}");
        assert!(!body.contains(".fetch_all("), "取数必须走 fetch_or_empty 收口：{body}");
        assert!(body.contains("UNION ALL"), "两源必须合并成一条查询：{body}");
        assert!(body.contains("status = 'active'"), "合同边只认 active：{body}");
        assert!(body.contains("kind = 'joinable'"), "{body}");
        // 置信下限提成具名常量：钉常量名的引用（两档缺一不可）+ 钉字面值不许暗降
        assert!(body.contains("JOIN_MIN_CONFIDENCE") && body.contains("OR status = 'accepted'"),
                "高置信/人工确认两档缺一不可：{body}");
        assert!(src.contains("const JOIN_MIN_CONFIDENCE: f64 = 0.9;"), "置信下限 0.9 不许暗降");
        assert!(body.contains("status <> 'rejected'"), "rejected 永远不算证据：{body}");
        assert!(body.contains("ANY($1)"), "证据边必须限定在候选表集合内：{body}");
    }

    // ── 本轮优化条目的行为钉（OPTIMIZATION-BACKLOG · direct.rs）──

    /// 注册表大小写漂移：路径查找、时间桥接、表级口径都不得因 `==` 失效
    /// （后者就是「明细表漏 deleted_flag = 0 致销量虚高 41%」的失败面）。
    #[test]
    fn table_name_matching_is_case_insensitive_against_registry_drift() {
        let drifted_edges = vec![JoinEdge {
            lt: "T_SALES_ORDER".into(), lc: "sales_order_code".into(),
            rt: "t_sales_order_detail".into(), rc: "sales_order_code".into(), card: "1:N".into(),
        }];
        let path = find_path("t_sales_order_detail", "t_sales_order", &drifted_edges)
            .expect("大小写漂移不该让路径找不到");
        assert_eq!(path.len(), 1);
        let drifted_scopes = vec![("T_SALES_ORDER".to_string(), "deleted_flag = 0".to_string())];
        let sql = compose_sql_with(&qty_metric(), &cat_dim(), "本月销量按商品分类", &drifted_edges, &drifted_scopes)
            .expect("大小写漂移不该让装配失败");
        assert!(sql.contains("o_time.deleted_flag = 0"), "表级口径漏挂：{sql}");
    }

    /// 关键字按词元判：'SELECTED' 字面量不误中（过度拒），UNION 后换行不漏（该拒没拒）。
    #[test]
    fn compose_gate_keyword_checks_are_word_bounded() {
        assert!(sql_has_keyword("PRODUCT_STOCK_DATE = (SELECT MAX(X) FROM T)", "SELECT"));
        assert!(sql_has_keyword("T_A UNION\nALL T_B", "UNION"));
        assert!(sql_has_keyword("UNION ALL T_B", "UNION"), "串首的 UNION 也不许漏");
        assert!(!sql_has_keyword("NOTE = 'SELECTED'", "SELECT"), "'SELECTED' 字面量不是子查询");
        // 'SELECTED' 字面量口径不再被误拒（原来 contains("SELECT") 过度拒）
        let mut m = sales_metric();
        m.scope_filter = "deleted_flag = 0 AND remark = 'SELECTED'".into();
        let sql = compose_sql(&m, &dim("省份", "cus.province"), "本月销售额按省份", &edges())
            .expect("'SELECTED' 字面量不是子查询，不该被误拒");
        assert!(sql.contains("remark = 'SELECTED'"), "{sql}");
        // UNION 后换行照样拒（原来 " UNION " 要求两侧都是空格）
        let mut m2 = sales_metric();
        m2.source_table = "t_invoice_apply_header UNION\nALL t_invoice_new_apply_header".into();
        assert!(compose_sql(&m2, &dim("省份", "cus.province"), "本月销售额按省份", &edges()).is_none());
    }

    /// 维度来源与指标侧同规格：剥注解 + 首标识符 + 合并连续空白。
    #[test]
    fn dimension_source_annotations_and_double_spaces_are_normalized() {
        // ① 连续空白：splitn 不合并会把别名错进 rest，FROM 拼出 `o o LEFT JOIN` 坏串
        let spaced = DimDef {
            name: "省份".into(),
            aliases: vec![],
            source_table: "t_sales_order  o  LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0".into(),
            expr: "COALESCE(NULLIF(cus.province,''),'未知')".into(),
        };
        let sql = compose_sql_with(&qty_metric(), &spaced, "本月销量按省份", &edges(), &scopes())
            .expect("连续空格的维度声明该装配得了");
        assert!(sql.contains("LEFT JOIN t_customer cus ON cus.customer_code"), "{sql}");
        assert!(!sql.contains(" o  LEFT JOIN"), "别名被错拼进 FROM：{sql}");
        // ② 带人类注解（跨基表）：基表取出 `t_x(JOIN` 这种串 = 路径找不到（修前返 None）
        let annotated = DimDef {
            name: "省份".into(),
            aliases: vec![],
            source_table: "t_sales_order(登记来源) o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0".into(),
            expr: "COALESCE(NULLIF(cus.province,''),'未知')".into(),
        };
        let sql2 = compose_sql_with(&qty_metric(), &annotated, "本月销量按省份", &edges(), &scopes())
            .expect("带注解的维度声明该装配得了");
        assert!(!sql2.contains("(登记来源)"), "注解原文不该拼进 FROM：{sql2}");
        assert!(sql2.contains("JOIN t_customer cus ON"), "{sql2}");
        // ③ 带人类注解（同基表）：FROM 用剥过注解的来源串（修前把注解原文拼进 SQL）
        let same_base = DimDef {
            name: "商品分类".into(),
            aliases: vec![],
            source_table: "t_sales_order_detail(明细注记) d JOIN t_goods g ON g.goods_code = d.sku_code AND g.deleted_flag = 0".into(),
            expr: "COALESCE(NULLIF(g.goods_category_name,''),'未分类')".into(),
        };
        let sql3 = compose_sql_with(&qty_metric(), &same_base, "本月销量按商品分类", &edges(), &scopes())
            .expect("同基表带注解也该装配得了");
        assert!(!sql3.contains("(明细注记)"), "注解原文不该拼进 FROM：{sql3}");
        assert!(sql3.contains("JOIN t_goods g ON"), "{sql3}");
    }

    /// 扇出门先 trim 再判 COUNT(DISTINCT)：前导空格不该让这道门误判（SUM 沿 1:N 虚增的防线）。
    #[test]
    fn fanout_gate_trims_agg_before_count_distinct_check() {
        let m = MetricDef {
            name: "下单单数".into(),
            aliases: vec![],
            source_table: "t_sales_order".into(),
            agg_expr: " COUNT(DISTINCT sales_order_code)".into(), // 前导空格
            scope_filter: "deleted_flag = 0".into(),
            dedup_keys: String::new(),
            time_col: "order_time".into(),
        };
        let sql = compose_sql_with(&m, &cat_dim(), "本月下单单数按商品分类", &edges(), &scopes())
            .expect("COUNT(DISTINCT) 带前导空格不该被扇出门误拒");
        assert!(sql.contains("COUNT(DISTINCT b0.sales_order_code)"), "{sql}");
    }

    /// 先定别名再填时间列：模板里另有含 `order_time` 的标识符（如 `prev_order_time`）时，
    /// 修前的「填裸列再子串替换」会把它改成 `prev_o_time.order_time` 这种坏串。
    #[test]
    fn aliased_time_fill_does_not_rewrite_lookalike_identifiers() {
        let nodim = DimDef {
            name: String::new(),
            aliases: vec![],
            source_table: "t_sales_order_detail b0".into(),
            expr: String::new(),
        };
        let sql = compose_sql_with_snap(
            &qty_metric(),
            &nodim,
            "本月销量",
            &edges(),
            &scopes(),
            None,
            Some("{} >= DATE(prev_order_time) AND {} < CURDATE()"),
            &[],
        )
        .expect("时间桥接该装得上");
        assert!(sql.contains("o_time.order_time >= DATE(prev_order_time)"), "形似标识符被改坏：{sql}");
        assert!(!sql.contains("prev_o_time.order_time"), "子串替换会把 prev_order_time 改坏：{sql}");
    }

    /// 单字剥词只在边界：实体名里的「的/是/有/都/买」不许吃掉
    /// （修前「买过美的冰箱的客户」剥完剩「美冰箱」，探库/过滤全错）。
    #[test]
    fn relation_entity_names_keep_embedded_single_chars() {
        assert_eq!(strip_relation_words("买过美的冰箱的客户"), "美的冰箱");
        assert_eq!(strip_relation_words("买过所有烤肠的客户"), "所有烤肠");
        assert_eq!(detect_relation("买过美的冰箱的客户"), Some(Relation::BuyersOfGoods("美的冰箱".into())));
        // 边界形态保持原样
        assert_eq!(strip_relation_words("买过烤肠的客户有哪些"), "烤肠");
        assert_eq!(strip_relation_words("买烤肠的还买什么"), "烤肠");
    }

    /// 关系 SQL 的转义与 `sales_fact::quote` 同规格：`\` 也翻倍 ——
    /// 修前实体名以 `\` 结尾会吃掉闭引号，兜底 SQL 自己语法错误。
    #[test]
    fn relation_sql_escapes_backslash_like_sales_fact_quote() {
        assert_eq!(rel_quote("烤肠\\"), "烤肠\\\\");
        assert_eq!(rel_quote("张'记"), "张''记");
        let buyers = relation_rows("买过烤肠\\的客户").expect("反斜杠实体名也该接得住");
        assert!(buyers.sql.contains("LIKE '%烤肠\\\\%'"), "{}", buyers.sql);
    }

    /// 「manger」是拼错的收录；补上正确的「manager」同档拦（多拦一类问句进失败关闭卡）。
    #[test]
    fn warehouse_sales_unsupported_covers_manager_spelled_correctly() {
        assert_eq!(warehouse_sales_unsupported_semantic("本月manager的销售额"), Some("manager"));
        assert!(warehouse_sales_fact("本月manager的销售额").is_none());
        assert!(warehouse_sales_fact("本月manger的销售额").is_none(), "拼错形态照旧拦");
    }

    /// 「最低」全接线：consumed 补词后不再落「未确认限定」卡；TopN 走 `ranking_limit`
    /// （`detect_top_n` 的极值词表不含「最低」，直接用会丢 N，得 ASC LIMIT 200 而非 5）。
    #[test]
    fn lowest_ranking_questions_compose_with_asc_and_requested_n() {
        let hit = warehouse_sales_fact("本月销售额最低的客户").expect("最低不应再落「未确认限定」卡");
        assert!(hit.sql.contains("ASC"), "{}", hit.sql);
        let five = warehouse_sales_fact("本月销售额最低的5个客户").expect("最低 TopN 应命中");
        assert!(five.sql.contains("LIMIT 5"), "{}", five.sql);
        assert!(five.sql.contains("ASC"), "{}", five.sql);
    }

    /// 裸「前」不再误触排行（「目前市场费用」该出总额）；「top」任意大小写都认。
    #[test]
    fn market_cost_rank_trigger_ignores_bare_qian_and_accepts_any_top_case() {
        let total = warehouse_market_cost("目前市场费用");
        assert!(total.sql.starts_with("SELECT COALESCE(SUM("), "该出总额：{}", total.sql);
        assert!(total.detail.is_some(), "非排行应附分类明细");
        let top = warehouse_market_cost("本月市场费用Top5");
        assert!(top.sql.contains("ORDER BY `市场费用` DESC LIMIT 5"), "{}", top.sql);
        assert!(top.detail.is_none(), "排行的主结果就是分类明细");
        assert!(top.prev.is_none(), "排行不出上期");
    }

    /// 探库片段剥 extras（「销售金额/收入/毛利」）：不剥的话「恒众本月销售金额」
    /// 剥出「恒众销售金额」，探库必空 = 漏接（与装配路径 `sales_fact_consumed` 的消化面对齐）。
    #[test]
    fn customer_fragment_strips_metric_extra_words() {
        assert_eq!(customer_name_fragment("恒众本月销售金额").as_deref(), Some("恒众"));
        assert_eq!(customer_name_fragment("恒众本月销售额").as_deref(), Some("恒众"));
        assert_eq!(customer_name_fragment("恒众本月毛利").as_deref(), Some("恒众"));
    }

    /// 证据边表名归一：先取末段再剥引号（修前 `` `db`.`tbl` `` 会剩 `` `tbl `` 残段，
    /// datamap 若以引号限定名存边则证据全失效）。
    #[test]
    fn bare_table_normalizes_quoted_and_qualified_names() {
        assert_eq!(bare_table("t_goods"), "t_goods");
        assert_eq!(bare_table("dms_ods.t_goods"), "t_goods");
        assert_eq!(bare_table("`dms_ods`.`t_goods`"), "t_goods");
        assert_eq!(bare_table("\"DMS_ODS\".\"T_GOODS\""), "t_goods");
        let s = shape_of(
            "SELECT g.brand_name AS `品牌`, SUM(w.sale_amount) AS `销售额` \
             FROM dms_ods.t_winc_sale_report w LEFT JOIN dms_ods.t_goods g \
             ON w.sku_code = g.goods_code GROUP BY g.brand_name",
        );
        assert!(derive_joins_unevidenced(
            &s,
            &[edge("`dms_ods`.`t_goods`", "goods_code", "`dms_ods`.`t_winc_sale_report`", "sku_code")]
        )
        .is_none());
    }

    /// 客户名片段不许被通用虚词表吃掉肚子里的字：「有/和/一/个」在公司名里合法。
    /// 2026-08-11 实测：全局 replace 把「…商贸有限公司」剥成「…商贸限公司」，主档探库必空，
    /// 「线下-潍坊程祥商贸有限公司本月销售额」整题跌进 ODS 推导、被 t_winc_sale_report 出 NULL。
    #[test]
    fn customer_name_fragment_keeps_inner_chars() {
        assert_eq!(
            customer_name_fragment("线下-潍坊程祥商贸有限公司本月销售额和销量是多少？"),
            Some("线下-潍坊程祥商贸有限公司".to_string())
        );
        assert_eq!(
            customer_name_fragment("恒众餐饮本月买了多少"),
            Some("恒众餐饮".to_string())
        );
        // 两头虚词照旧剥掉；领头类别词照旧剥掉
        assert_eq!(
            customer_name_fragment("客户董会琴本月的销售额"),
            Some("董会琴".to_string())
        );
        // 剥完是类别词的照旧拒（分类问句不许错配成名称探库）
        assert_eq!(customer_name_fragment("线下客户本月销售额"), None);
        // 🔴 大区名不是客户名（2026-08-15 生产直打）：DWS 的 region 里没有「华东」，
        // 而客户表里有个 `线下-福建云通供应链有限公司(华东区）`——名字括号里带着地域标注。
        // 不拒的话「本月华东区销售额」会变成「那一家客户本月销售额」，答 0，
        // 用户读到的是「华东区本月没销售」。
        assert_eq!(customer_name_fragment("本月华东区销售额"), None);
        assert_eq!(customer_name_fragment("华南区本月销售额"), None);
        assert_eq!(customer_name_fragment("本月西北大区销售额"), None);
        // 正常客户名一个都不许被这条误伤
        assert_eq!(
            customer_name_fragment("华东实业本月销售额").as_deref(),
            Some("华东实业"),
            "带地域字样的**公司名**照旧是客户名"
        );
        // 渠道词黏在实体名头尾是限定不是名字（2026-08-12 生产实测归一重试两连不中）
        assert_eq!(
            customer_name_fragment("潍坊程祥商贸有限公司本月线下销售额是多少？"),
            Some("潍坊程祥商贸有限公司".to_string())
        );
        // 剥完只剩渠道词本身时保留：「本月线下销售额」的「线下」是渠道过滤本体
        assert_eq!(customer_name_fragment("本月线下销售额是多少"), Some("线下".to_string()));
        // 带渠道词的客户题整条能装配：残留守卫不许把渠道词拦下
        let frag = customer_name_fragment("潍坊程祥商贸有限公司本月线下销售额是多少？").unwrap();
        let binding = crate::entity_resolver::CustomerBinding {
            surface: frag,
            canonical_code: "C-WF".into(),
            canonical_name: "线下-潍坊程祥商贸有限公司".into(),
        };
        let h = warehouse_sales_fact_predicated(
            "潍坊程祥商贸有限公司本月线下销售额是多少？",
            Some(&binding),
        )
        .expect("客户+渠道词+销售额必须能落到共享 DWS 合同");
        assert!(h.sql.contains("dws_off_offline_sale_dfn"), "{}", h.sql);
        assert!(
            h.sql.contains("storecode") && h.sql.contains("C-WF"),
            "{}",
            h.sql
        );
        assert_eq!(
            h.intent_evidence.resolved[0].surface,
            "潍坊程祥商贸有限公司"
        );
        // 唯一绑定交给共享事实合同：DWS 事实表 + canonical storecode 过滤
        let frag =
            customer_name_fragment("线下-潍坊程祥商贸有限公司本月销售额和销量是多少？").unwrap();
        let binding = crate::entity_resolver::CustomerBinding {
            surface: frag,
            canonical_code: "C-WF".into(),
            canonical_name: "线下-潍坊程祥商贸有限公司".into(),
        };
        let h = warehouse_sales_fact_predicated(
            "线下-潍坊程祥商贸有限公司本月销售额和销量是多少？",
            Some(&binding),
        )
        .expect("客户名+销售额必须能落到共享 DWS 合同");
        assert_eq!(h.route, "direct-agg");
        assert!(h.sql.contains("dws_off_offline_sale_dfn"), "{}", h.sql);
        assert!(h.sql.contains("storecode"), "{}", h.sql);
        assert!(h.sql.contains("C-WF"), "{}", h.sql);
        assert_eq!(
            h.intent_evidence.resolved[0].surface,
            "线下-潍坊程祥商贸有限公司"
        );
    }

    /// 推导候选池守卫：没点名 WinC/营销通/经销商上报/进销存 时，营销通专属表不许进池。
    #[test]
    fn derive_pool_winc_guard_drops_report_tables_unless_asked() {
        let mut pool: Vec<&'static str> =
            vec!["t_winc_sale_report", "t_sales_order", "t_winc_stock_report"];
        derive_pool_winc_guard(&mut pool, "线下-潍坊程祥商贸有限公司本月销售额和销量是多少？");
        assert_eq!(pool, vec!["t_sales_order"]);
        let mut asked: Vec<&'static str> = vec!["t_winc_sale_report", "t_sales_order"];
        derive_pool_winc_guard(&mut asked, "营销通里经销商上报的销售流水");
        assert_eq!(asked, vec!["t_winc_sale_report", "t_sales_order"], "点名营销通必须放行");
    }
}



/// 已识别的时间表面词天然是被消化的；没兑现的限定照旧要被抓住。
///
/// 🔴 由来（2026-08-15 生产直打）：「上个季度销售额」落
/// 「不可计算 · 未能识别的限定「上」」—— 残留守卫从不剥时间词，
/// 靠的是虚词表里恰好有「本」「今」；「上」不在表里，一个孤字把整条问句拒掉。
#[test]
fn a_recognized_time_phrase_is_consumed_but_a_real_qualifier_is_not() {
    for q in ["上个季度销售额", "本季度销售额", "上个月销售额", "上周销售额", "去年销售额"] {
        assert_eq!(
            crate::answerers::fastpath_intent::unrecognized_residue(q),
            "",
            "{q} 的时间词该被当成已消化"
        );
    }
    // 没兑现的限定一个都不许被顺手吞掉（那是静默丢限定）
    assert_eq!(
        crate::answerers::fastpath_intent::unrecognized_residue("长沙本月销售额"),
        "长沙"
    );
}
