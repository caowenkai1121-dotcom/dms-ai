//! 确定性快路径里**吃结构化意图与实体合同**的那几件（T8-B8）。
//!
//! 为什么在 agent 而不是 semantic：它们要 `intent::{IntentAttempt, IntentV1, TimeSlot}` 与
//! `entity_resolver::CustomerBinding` —— 那是 agent 的语义状态，而 semantic → agent 是反向边。
//! 其余快路径（纯注册表 + 纯 SQL 装配）已迁 `dms_semantic::fastpath::*`。
//!
//! 逐行搬自 `server/src/direct.rs`，只搬不改。

#![allow(clippy::too_many_arguments)]

use dms_semantic::compose::*;
use dms_semantic::compose::{assemble::*, metric::*, path::*, values::*};
use dms_semantic::fastpath::*;
use dms_semantic::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};
use dms_semantic::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use dms_semantic::{ExecutionEvidence, IntentSlotKind, Relation};
use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};
use std::collections::{HashMap, HashSet};
use sqlx::PgPool;
use dms_semantic::sales_fact;


/// 注册表读失败时**不许静默按缺省走**。
///
/// 🔴 历史事故（旧 ODS 分类销量路径已废止）：一趟评测曾记为 `llm+repair 97.9s`
/// 并答错，而同一个镜像、同一句问句事后连跑 5 次都稳定 `direct-agg` 且对数 ——
/// 也就是说那一刻 `try_compose` 返了 `None`，而**当时没有任何一行日志说为什么**。
///
/// 更坏的是原来那几个 `unwrap_or_default()`：`load_table_scopes` 读失败 = 装配器**不带表级口径**
/// 往下拼，出一个确定性的错数、route 仍是 `direct-agg`、连回炉的机会都没有
/// （那正是「明细表漏 `deleted_flag = 0` 致销量虚高 41%」的失败面）。
/// 判据：**缺了会改数的声明（表级口径 / 快照 / join 边）读失败就整条不装配**，并且吼出来。
macro_rules! reg_load {
    ($what:literal, $call:expr) => {
        match $call {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("注册表读失败：{} —— 确定性装配整条放弃，回落 LLM（{e}）", $what);
                return None;
            }
        }
    };
}

use dms_semantic::{DirectHit, DirectOutcome};

/// sales_fact 两条路（装配 / 「解析失败」卡）**共用同一份**消化词构造：
/// 指标名/别名/extras + 维度名/别名/extras + 「最低」补丁 + 已探明客户名 + 可答尾词，
/// 顺带给出标量判定（无维度单指标）。各抄一份必漂出「一边说能消化、另一边报残留」。
pub fn sales_fact_consumed(
    question: &str,
    metric_hits: &[(dms_semantic::sales_fact::Metric, &'static str)],
    dimension_hits: &[(dms_semantic::sales_fact::Dimension, &'static str)],
    customer: Option<&crate::entity_resolver::CustomerBinding>,
) -> (Vec<String>, bool) {
    let mut consumed: Vec<String> = vec![];
    for (metric, _) in metric_hits {
        consumed.push(metric.name().to_string());
        consumed.extend(metric.aliases().iter().map(|word| (*word).to_string()));
        consumed.extend(sales_fact_metric_extra_words(*metric).iter().map(|word| (*word).to_string()));
    }
    for (dimension, _) in dimension_hits {
        consumed.push(dimension.name().to_string());
        consumed.extend(dimension.aliases().iter().map(|word| (*word).to_string()));
        consumed.extend(sales_fact_dimension_extra_words(*dimension).iter().map(|word| (*word).to_string()));
    }
    // 「最低」补丁：STRIP_WORDS 有最高/最多/最少/最大/最小独缺它（kernel 词表侧另有纪律，不动），
    // 与 `has_entity_residue` 手工补的那颗对齐 —— 否则「本月销售额最低的客户」残留「最低」，
    // 落「未确认限定」卡，而下面的 ASC 排序分支本来就为它写着。
    consumed.push("最低".to_string());
    // 🔴 已识别的时间表面词天然是被消化的：模板会把它编译进时间窗。
    //
    // 由来（2026-08-15 生产直打）：「上个季度销售额」落「不可计算 · 未能识别的限定「上」」——
    // 残留守卫从不剥时间词，靠的是虚词表里恰好有「本」「今」；「上」不在表里，
    // 于是一个孤字把整条问句拒掉。谓词认得、时间表面词也认得，只有这里不认。
    //
    // 只能用**词级**的 `time_phrase_of`：`intent_time_surface` 带整句兜底
    //（`time_predicate(question).map(|_| question.to_string())`），拿它当消化词会把
    // 「长沙」这类没兑现的限定一起吞掉 —— 那正是静默丢限定。
    if let Some(phrase) = dms_kernel::nl::time::time_phrase_of(question) {
        consumed.push(phrase.to_string());
    }
    if let Some(binding) = customer {
        consumed.push(binding.surface.clone());
    }
    // 尾部问法修饰词（怎么样/同比增长多少/其中X占多少…）：兑现得了的才剥，判残留前并进 consumed。
    let scalar = dimension_hits.is_empty() && metric_hits.len() == 1;
    consumed.extend(answerable_tail_words(question, scalar));
    (consumed, scalar)
}





pub fn warehouse_sales_fact(question: &str) -> Option<DirectHit> {
    warehouse_sales_fact_predicated(question, None)
}

/// Fast 意图输出截断或轻微改写时，只允许已经被销售事实确定性快路径完整接住的问句
/// 恢复成最小 Data 合同。快路径的残留、时间、唯一省区与合同能力门仍是唯一裁决器；
/// 任一限定无法兑现就返回 None，继续走原来的 fail-closed 澄清。
pub fn recover_sales_intent(
    question: &str,
    warehouse: bool,
) -> Option<crate::intent::IntentAttempt> {
    if !warehouse {
        return None;
    }
    warehouse_sales_fact(question)?;
    let metrics = warehouse_sales_metrics(question)
        .into_iter()
        .map(|(metric, _)| metric.name().to_string())
        .collect();
    let breakdowns = warehouse_sales_dimensions(question)
        .into_iter()
        .map(|(_, surface)| surface.to_string())
        .collect();
    let regions = sales_fact_province_filter(question, None)
        .ok()?
        .map(|(surface, _)| vec![surface])
        .unwrap_or_default();
    let time = intent_time_surface(question).map(|surface| crate::intent::TimeSlot {
        surface,
        start: String::new(),
        end: String::new(),
        grain: String::new(),
    });
    let comparisons = ["同比", "环比"]
        .into_iter()
        .filter(|surface| question.contains(surface))
        .map(str::to_string)
        .collect();
    let requested_detail = ["明细", "详情", "逐条"]
        .into_iter()
        .any(|surface| question.contains(surface));
    let attempt = crate::intent::IntentAttempt::validated(
        crate::intent::IntentV1 {
            mode: crate::intent::IntentMode::Data,
            metrics,
            regions,
            time,
            breakdowns,
            comparisons,
            requested_detail,
            ..Default::default()
        },
        question,
    );
    attempt.is_ready().then_some(attempt)
}





/// `customer`：共享实体解析器唯一确认的客户绑定。执行谓词落 canonical customer_code，
/// 意图证据仍绑定用户原文 surface；不再用模糊名称当事实谓词。
pub fn warehouse_sales_fact_predicated(
    question: &str,
    customer: Option<&crate::entity_resolver::CustomerBinding>,
) -> Option<DirectHit> {
    use dms_semantic::sales_fact::{Dimension, Sort, SortDirection};

    let metric_hits = warehouse_sales_metrics(question);
    if metric_hits.is_empty() || warehouse_sales_has_unsupported_semantics(question) {
        return None;
    }
    let dimension_hits = warehouse_sales_dimensions(question);
    let metrics = metric_hits.iter().map(|(metric, _)| *metric).collect::<Vec<_>>();
    let dimensions = dimension_hits.iter().map(|(dimension, _)| *dimension).collect::<Vec<_>>();
    // 标量 = 无维度单指标：只有它能挂 KPI delta（prev/comparisons 只在这时装配），
    // 「同比/环比多少」类尾词因此只在标量下才允许剥（见 `answerable_tail_words` 的纪律）。
    // 消化词与「解析失败」卡共用同一份构造（`sales_fact_consumed`，含「最低」补丁与客户名）。
    let (consumed, scalar) = sales_fact_consumed(question, &metric_hits, &dimension_hits, customer);
    // 已探明客户的名字自带渠道前缀（线下-/线上-）：问句里的渠道词由实体解释，不算残留——
    // 「潍坊程祥商贸有限公司本月线下销售额」的「线下」不许把整条拦回落（2026-08-12 生产实测）。
    // 只在有实体时消化渠道词：无实体的「本月线上销售额」族不受本路径影响。
    let mut consumed = consumed;
    if customer.is_some() {
        consumed.extend(["线下".to_string(), "线上".to_string()]);
    }
    let province = sales_fact_province_filter(
        question,
        customer.map(|binding| binding.canonical_name.as_str()),
    )
    .ok()?;
    let mut predicates = vec![];
    if let Some((province_name, predicate)) = province {
        consumed.push(province_name);
        predicates.push(predicate);
    }
    if let Some(binding) = customer {
        predicates.push(dms_semantic::sales_fact::Predicate::eq(
            Dimension::CustomerCode,
            &binding.canonical_code,
        ));
    }
    if has_residue(question, &consumed) {
        return None;
    }

    let (begin, end) = warehouse_sales_time_bounds(question)?;
    // 「最低 N 个」走带补丁的入口：`detect_top_n` 的极值词表不含「最低」，`ranking_limit` 有
    let requested_top_n = ranking_limit(question);
    let explicit_limit = (requested_top_n < 200).then_some(requested_top_n as u32);
    let ranking = ["排行", "排名", "最高", "最多", "最大", "最少", "最小", "最低", "最好"]
        .iter()
        .any(|word| question.contains(word))
        || explicit_limit.is_some();
    let direction = if rank_direction(question) == "ASC" {
        SortDirection::Asc
    } else {
        SortDirection::Desc
    };
    let time_dimension = dimensions
        .iter()
        .copied()
        .find(|dimension| matches!(dimension, Dimension::Month | Dimension::OrderDate));
    let sort = match (dimensions.is_empty(), time_dimension, ranking) {
        (true, _, _) => None,
        (false, Some(td), false) => Some(Sort::dimension(td, SortDirection::Asc)),
        _ => Some(Sort::metric(metrics[0], direction)),
    };
    let has_categorical_dimension = dimensions
        .iter()
        .any(|dimension| !matches!(dimension, Dimension::Month | Dimension::OrderDate));
    let limit = if has_categorical_dimension {
        Some(explicit_limit.unwrap_or(200))
    } else {
        explicit_limit
    };
    let sql = sales_fact_sql(
        &metrics,
        &dimensions,
        &begin,
        &end,
        &predicates,
        sort,
        limit,
    );

    // 问句点名「同比」时，同比就是主 delta（KPI 卡第一个比较位），环比退居 comparisons；
    // 未点名维持原序（环比为主、同比为辅）。两种问法两个比较都会执行，只是展示位次不同。
    let (primary, secondary) = if question.contains("同比") {
        (yoy_window(question), prev_window(question))
    } else {
        (prev_window(question), yoy_window(question))
    };
    let prev = scalar.then(|| primary).flatten().and_then(|(template, label)| {
        let (begin, end) = dms_semantic::sales_fact::comparison_time_bounds(question, template)?;
        Some((
            sales_fact_sql(&metrics, &[], &begin, &end, &predicates, None, None),
            label.to_string(),
        ))
    });
    let comparisons = scalar
        .then(|| secondary)
        .flatten()
        .and_then(|(template, label)| {
            let (begin, end) = dms_semantic::sales_fact::comparison_time_bounds(question, template)?;
            Some((
                sales_fact_sql(&metrics, &[], &begin, &end, &predicates, None, None),
                label.to_string(),
            ))
        })
        .into_iter()
        .collect();
    let detail = scalar.then(|| dms_semantic::sales_fact::detail_sql(&begin, &end, &predicates, 100));
    // 同窗补充（裁决：销售类单指标 KPI 顺带成本/收入/毛利额/毛利率）：与主查询**同一时间窗、
    // 同一批谓词**，一条 SQL 取齐五值。仅标量命中挂它 —— 维度拆解/多指标的主结果自带这些列。
    let sales_context = scalar.then(|| {
        sales_fact_sql(
            dms_semantic::sales_fact::CONTEXT_METRICS,
            &[],
            &begin,
            &end,
            &predicates,
            None,
            None,
        )
    });

    let mut intent_evidence = customer
        .map(crate::entity_resolver::CustomerBinding::execution_evidence)
        .unwrap_or_default();
    for (metric, _) in &metric_hits {
        intent_evidence = intent_evidence.resolve(
            crate::intent::IntentSlotKind::Metric,
            metric.name(),
        );
    }
    for (_, surface) in &dimension_hits {
        intent_evidence = intent_evidence.resolve(
            crate::intent::IntentSlotKind::Breakdown,
            *surface,
        );
    }
    if let Some((surface, _)) = sales_fact_province_filter(
        question,
        customer.map(|binding| binding.canonical_name.as_str()),
    )
    .ok()
    .flatten()
    {
        intent_evidence = intent_evidence.resolve(
            crate::intent::IntentSlotKind::Region,
            surface,
        );
    }
    if let Some(surface) = intent_time_surface(question) {
        intent_evidence = intent_evidence.resolve(
            crate::intent::IntentSlotKind::Time,
            surface,
        );
    }
    Some(DirectHit {
        outcome: DirectOutcome::Data,
        sql,
        route: "direct-agg".into(),
        prev,
        comparisons,
        detail,
        sales_context,
        intent_evidence,
    })
}





pub fn try_direct(question: &str) -> Option<DirectHit> {
    sniff_doc_code(question, false)
        .or_else(|| device_orders(question))
        .or_else(|| relation_rows(question))
        .or_else(|| sales_order_rows(question))
        .or_else(|| balance_ranking(question))
        .or_else(|| stock_snapshot(question))
        .or_else(|| agg_template(question))
}




pub fn try_direct_for(question: &str, warehouse: bool) -> Option<DirectHit> {
    if warehouse {
        warehouse_finance(question).or_else(|| try_direct_warehouse(question))
    } else {
        sales_fact_unavailable(
            question,
            warehouse_sales_unsupported_semantic(question),
            "默认销售经营指标只允许在已验证数仓事实上查询；当前业务库不执行替代统计",
            "请切换到已验证数仓，或先补齐独立事实合同",
        )
        .or_else(|| try_direct(question))
    }
}




pub fn try_direct_warehouse(question: &str) -> Option<DirectHit> {
    sniff_doc_code(question, true)
        .or_else(|| device_orders(question))
        .or_else(|| sales_breakdown(question))
        .or_else(|| warehouse_sales_fact(question))
        .or_else(|| warehouse_sales_semantics_unavailable(question))
        .or_else(|| relation_rows(question))
        // 小程序下单必须拦在 sales_order_rows 前面：后者没有渠道过滤能力（t_sales_order
        // 全表 source_platform_code='DMS'），让它先接就是静默丢「小程序」限定
        .or_else(|| mini_program_order_agg(question))
        .or_else(|| sales_order_rows(question))
        .or_else(|| balance_ranking(question))
        .or_else(|| stock_snapshot(question))
        // 订单口径高频模板（订单数/成交客户数/客单价）——ODS 订单表在数仓同样可查，
        // 缺了它这类题在数仓目标下全掉 LLM（实测 A07-A12 集体 route 漂移）
        .or_else(|| agg_template(question))
}




/// 默认销售额维度快路径只使用已验证的 DWS 事实合同。
///
/// SQL 的表、指标、维度、时间列、排序与谓词全部由
/// `dms_semantic::sales_fact` 构造；此处不再维护任何发货/退货 UNION 口径。
/// 用户行级权限仍由执行层 `gate_on` 注入到事实表 `storecode`，本函数不得复制权限条件。
pub fn sales_breakdown(question: &str) -> Option<DirectHit> {
    use dms_semantic::sales_fact::Metric;

    let metrics = warehouse_sales_metrics(question);
    if metrics.len() != 1 || metrics[0].0 != Metric::SalesAmount {
        return None;
    }
    if warehouse_sales_has_unsupported_semantics(question) {
        return None;
    }
    if warehouse_sales_dimensions(question).is_empty() {
        return None;
    }
    warehouse_sales_fact(question)
}




/// 通用组合器（S3，SuperSonic 语义层组合思想）：指标×维度 数据驱动装配，退役手工模板。
/// 问句同时命中指标注册表与维度注册表 → 装配 GROUP BY 查询。门控（宁缺毋滥，装配不出就回落）：
/// 同基表直拼 / 跨基表走 join_edge BFS 路径（≤3 跳，扇出边仅 COUNT(DISTINCT) 聚合可过）、
/// 口径无子查询、实体守卫、时间窗=order_time 在 FROM 内或可经一条边桥接 t_sales_order。
/// 【K3-B ②】`ds` 只作用于**注册表加载**（四条 SQL 在 `registry::model`，各带一条 `ds_pred`），
/// 下面的装配逻辑一字未动 —— DMS 的口径卡绝不能被别的库用上，反之亦然。
/// 谁会服务这个问句里的**确定性**部分：`agg_template`（订单口径指标）/
/// `sales_breakdown`（受信 DWS 销售事实维度）/ 单号直查。空串 = 没有确定性模板接它。
///
/// 🔴 为什么要单独报出来：这三样是 DMS 专用的写死逻辑，也就是「本系统还有多少不通用」的度量。
/// 声明层每接走一道题，这里就该少一道 —— 而 `direct.rs` 的解体（T8）正是以此为验收。
/// 不量它就只能靠感觉说「越来越声明化了」。
pub fn hardcoded_producer(question: &str) -> &'static str {
    if doc_binding_hit(question) {
        return "单号直查";
    }
    if device_orders(question).is_some() {
        return "device_orders（设备订单·SO04）";
    }
    if agg_template(question).is_some() {
        return "agg_template（订单口径指标）";
    }
    if sales_breakdown(question).is_some() {
        return "sales_breakdown（DWS 销售事实）";
    }
    ""
}




pub async fn try_compose(pg: &sqlx::PgPool, ds: &str, question: &str) -> Option<DirectHit> {
    // 默认销售经营指标只允许 `warehouse_sales_fact` 通过共享 DWS 合同构造。
    // 这道门放在公开入口，避免绕过 Router 的调用方重新按注册表 JOIN 旧订单事实。
    if !warehouse_sales_metrics(question).is_empty() {
        return None;
    }
    use dms_semantic::registry::model as reg;
    // 【并行读注册表】七张声明表互不依赖（入参全是同一份 pg+ds），原来 7 次串行 =
    // 每个问句在 Router 第一站就白付 7 个 PG 往返。并发拿回后按**原顺序**逐个判失败，
    // warn 文案与「读到哪张表失败」的对应关系不变。
    let (metrics, policies, dims, edges, scopes, snaps, vals) = tokio::join!(
        reg::load_metrics(pg, ds),
        reg::load_metric_policies(pg, ds),
        reg::load_dimensions(pg, ds),
        reg::load_join_edges(pg, ds),
        reg::load_table_scopes(pg, ds),
        reg::load_table_snapshots(pg, ds),
        reg::load_value_map(pg, ds),
    );
    let metrics = reg_load!("meta.metric", metrics);
    let policies = reg_load!("meta.metric policy", policies);
    let dims = reg_load!("meta.dimension", dims);
    let edges = reg_load!("meta.join_edge", edges);
    // 表级标准口径（SuperSonic model filter）：JOIN 到的表恒需附加的过滤
    let scopes = reg_load!("meta.table_scope", scopes);
    // 快照/流水表声明：同一分区键有多条历史行，取数须只留最新一条（见 `compose_gated`）
    let snaps = reg_load!("meta.table_snapshot", snaps);
    // 码值域：问句里的值过滤能被声明解释时装进 WHERE（见 `value_filters`）。
    // 这一个**可以**按缺省走：空表 = 没有任何值名被消化 = 带值过滤的问句照旧被残留守卫拦下，
    // 即读不到只会少一点确定性覆盖，不会出错数。仍然吼一声，别让它静默退化。
    let vals = match vals {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("注册表读失败：meta.value_map —— 值过滤本轮不生效（{e}）");
            vec![]
        }
    };
    let (metric, m_word) = pick(question, &metrics, |m| (&m.name, &m.aliases))?;
    // 维度侧**减去指标已消化的词**（见 `pick_excluding`：「成交客户数」里的「客户」不是维度）
    let dim = pick_excluding(question, &dims, |d| (&d.name, &d.aliases), &m_word)?;
    if !metric_dimension_allowed(&policies, &metric.name, &dim.name) {
        tracing::info!(metric = %metric.name, dimension = %dim.name, "指标维度未在白名单，回落 LLM");
        return None;
    }
    compose_gated(metric, dim, question, &edges, &scopes, &snaps, &vals)
        .map(|sql| hit(dms_semantic::registry::warehouse_qualified_source(ds, &sql), "direct-agg"))
}




/// 图谱只允许全量权限账号；受限账号用同义只读 SQL 回答关系问题，继续经过 `gate_on` 行权限注入。
/// Router 顺序仍是 graph 在先，所以全量账号不会被这里抢走。
pub fn relation_rows(question: &str) -> Option<DirectHit> {
    let rel = detect_relation(question)?;
    let sql = match rel {
        Relation::BuyersOfGoods(name) => {
            let safe = rel_quote(&name);
            format!(
                "SELECT o.customer_code AS `客户编码`, MAX(o.customer_name) AS `客户`, \
                        COUNT(DISTINCT o.sales_order_code) AS `订单数`, \
                        COALESCE(SUM(d.box_quantity),0) AS `购买数量`, \
                        COALESCE(SUM(d.amount),0) AS `下单金额`, MAX(o.order_time) AS `最近下单时间` \
                 FROM t_sales_order o \
                 JOIN (SELECT sales_order_code, sku_code, MAX(sku_name) AS sku_name, \
                              SUM(box_quantity) AS box_quantity, SUM(amount) AS amount \
                       FROM t_sales_order_detail WHERE deleted_flag = 0 AND item_type = '1' \
                       GROUP BY sales_order_code, sku_code) d ON d.sales_order_code = o.sales_order_code \
                 WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199') \
                   AND (d.sku_name LIKE '%{safe}%' OR d.sku_code = '{safe}') \
                 GROUP BY o.customer_code ORDER BY `下单金额` DESC, `订单数` DESC LIMIT 200"
            )
        }
        Relation::GoodsOfCustomer(name) => {
            let safe = rel_quote(&name);
            format!(
                "SELECT d.sku_code AS `商品编码`, MAX(d.sku_name) AS `商品`, \
                        COUNT(DISTINCT o.sales_order_code) AS `订单数`, \
                        COALESCE(SUM(d.box_quantity),0) AS `购买数量`, \
                        COALESCE(SUM(d.amount),0) AS `下单金额`, MAX(o.order_time) AS `最近下单时间` \
                 FROM t_sales_order o \
                 JOIN (SELECT sales_order_code, sku_code, MAX(sku_name) AS sku_name, \
                              SUM(box_quantity) AS box_quantity, SUM(amount) AS amount \
                       FROM t_sales_order_detail WHERE deleted_flag = 0 AND item_type = '1' \
                       GROUP BY sales_order_code, sku_code) d ON d.sales_order_code = o.sales_order_code \
                 WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199') \
                   AND (o.customer_name LIKE '%{safe}%' OR o.customer_code = '{safe}') \
                 GROUP BY d.sku_code ORDER BY `下单金额` DESC, `订单数` DESC LIMIT 200"
            )
        }
        Relation::Copurchase(name) => {
            let safe = rel_quote(&name);
            format!(
                "SELECT d.sku_code AS `商品编码`, MAX(d.sku_name) AS `商品`, \
                        COUNT(DISTINCT o.sales_order_code) AS `共同订单数`, \
                        COALESCE(SUM(d.box_quantity),0) AS `购买数量`, \
                        COALESCE(SUM(d.amount),0) AS `下单金额`, MAX(o.order_time) AS `最近共同下单时间` \
                 FROM t_sales_order o \
                 JOIN (SELECT sales_order_code, sku_code, MAX(sku_name) AS sku_name, \
                              SUM(box_quantity) AS box_quantity, SUM(amount) AS amount \
                       FROM t_sales_order_detail WHERE deleted_flag = 0 AND item_type = '1' \
                       GROUP BY sales_order_code, sku_code) d ON d.sales_order_code = o.sales_order_code \
                 JOIN (SELECT DISTINCT sales_order_code FROM t_sales_order_detail \
                       WHERE deleted_flag = 0 AND item_type = '1' \
                         AND (sku_name LIKE '%{safe}%' OR sku_code = '{safe}')) target \
                   ON target.sales_order_code = o.sales_order_code \
                 WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199') \
                   AND NOT (d.sku_name LIKE '%{safe}%' OR d.sku_code = '{safe}') \
                 GROUP BY d.sku_code ORDER BY `共同订单数` DESC, `下单金额` DESC LIMIT 200"
            )
        }
    };
    Some(hit(sql, "direct-doc"))
}




/// 问句尾部的「问法修饰词」——不携带口径限定、且装配器**兑现得了**才允许剥
/// （与 `lexicon::STRIP_WORDS` 同一条纪律：剥了却不兑现 = 静默丢限定，E16 同形态）。
///
/// - 「怎么样/如何」是纯语气，恒可剥；
/// - 「同比/环比（增长/下降）多少」由 KPI delta（prev/comparisons）用百分数兑现 ——
///   只在**标量**（delta 只挂单指标 KPI）且对应比较窗口确实存在时才剥；
///   剥了却没有 delta 可答，就是把「问同比」答成「只有总量」；
/// - 「其中 X 占多少」：X 在合同里无可验证谓词（compound 只接「其中+极值词」，不接占比族），
///   按裁决以 KPI+delta 形态答总量；含极值词的「其中」族归 compound，这里一个字都不剥。
/// 判残留前由 `warehouse_sales_fact_predicated` 并进 consumed。
pub fn answerable_tail_words(question: &str, scalar: bool) -> Vec<String> {
    let mut words: Vec<String> = ["怎么样", "如何"]
        .iter()
        .filter(|word| question.contains(**word))
        .map(|word| (*word).to_string())
        .collect();
    if !scalar {
        return words;
    }
    // 「同比」由 yoy_window 兑现、「环比」由 prev_window 兑现；窗口认不得就不剥。
    if question.contains("同比") && yoy_window(question).is_some() {
        for word in ["同比增长多少", "同比下降多少", "同比增长", "同比下降", "同比"] {
            if question.contains(word) {
                words.push(word.to_string());
            }
        }
    }
    if question.contains("环比") && prev_window(question).is_some() {
        for word in ["环比增长多少", "环比下降多少", "环比增长", "环比下降", "环比"] {
            if question.contains(word) {
                words.push(word.to_string());
            }
        }
    }
    if words.iter().any(|w| w.contains("同比") || w.contains("环比")) {
        for word in ["增长多少", "下降多少", "增长", "下降"] {
            if question.contains(word) {
                words.push(word.to_string());
            }
        }
    }
    // 「其中 X 占多少」：占比请求。含极值词的是 compound 的地盘，不碰。
    if let Some(pos) = question.find("其中") {
        let tail = &question[pos..];
        let superlative = ["最高", "最低", "最多", "最少"].iter().any(|w| tail.contains(w));
        if tail.contains('占') && !superlative {
            words.push(tail.to_string());
        }
    }
    words
}





/// 「解析失败」卡要点名的那段：剥掉指标/维度词、通用虚词与可答尾词后剩下的实义残留。
/// 与 `has_residue` 的剥离完全同构（同一词表、同一算法），只是返回残留文本而不是布尔。
pub fn unrecognized_residue(question: &str) -> String {
    let metric_hits = warehouse_sales_metrics(question);
    let dimension_hits = warehouse_sales_dimensions(question);
    // 消化词与装配路径共用同一份构造（`sales_fact_consumed`），各写一份会漂
    let (consumed, _) = sales_fact_consumed(question, &metric_hits, &dimension_hits, None);
    residual_text(question, &consumed.iter().map(String::as_str).collect::<Vec<_>>())
}





/// 商品库存必须在实际 WMS 表中唯一解析；零匹配、多个 SKU、未兑现限定、探针失败都终止并澄清。
pub async fn stock_product_filtered(
    question: &str,
    source: &dyn dms_connector::source::SqlSource,
    principal: &dms_policy::Principal,
    scope: &dms_policy::Scope,
    ds_global: bool,
) -> Option<DirectHit> {
    if !source.is_warehouse() || !["库存", "存货"].iter().any(|word| question.contains(word)) {
        return None;
    }
    let fragment = stock_product_fragment(question)?;
    // 当前适配器只兑现“商品 + 正品现行库存”。其他维度/状态/历史窗口保留为未解析限定，
    // 交给上层模型规划或澄清，不能把它们当成商品名的一部分后再静默放宽。
    const UNSUPPORTED: &[&str] = &[
        "仓库", "仓", "库位", "批次", "效期", "临期", "过期", "残损", "报损", "冻结", "锁定",
        "昨天", "上月", "本月", "去年", "今年", "同比", "环比", "各", "按", "分别", "排行", "排名",
        "最高", "最低", "最多", "最少",
    ];
    if UNSUPPORTED.iter().any(|word| fragment.contains(word)) {
        return Some(stock_product_unavailable(
            &fragment,
            "问句含当前库存路径无法兑现的仓库、状态或时间限定",
        ));
    }
    let safe = rel_quote(&fragment);
    let probe = format!(
        "SELECT sku_code \
         FROM ywzt_ods.scm_warehous_manage \
         WHERE inventory_status = 'ZP' AND (sku_code = '{safe}' OR INSTR(sku_name, '{safe}') > 0) \
         GROUP BY sku_code ORDER BY sku_code LIMIT 2"
    );
    let scoped = match crate::gate_on(principal, &probe, scope, ds_global, source.dialect()) {
        Ok(scoped) => scoped,
        Err(e) => {
            tracing::warn!(err = %e, question, "库存商品探针未过闸门 → 终止并澄清");
            return Some(stock_product_unavailable(
                &fragment,
                "商品唯一性校验未能安全执行",
            ));
        }
    };
    let rows = match source.fetch(&scoped, 2, crate::EXEC_TIMEOUT).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(err = %e, question, "库存商品探针执行失败 → 终止并澄清");
            return Some(stock_product_unavailable(
                &fragment,
                "商品唯一性校验暂时失败",
            ));
        }
    };
    if rows.rows.len() != 1 {
        tracing::info!(
            question,
            candidates = rows.rows.len(),
            "库存商品未唯一解析 → 终止并澄清"
        );
        let reason = if rows.rows.is_empty() {
            "未在现行正品库存中找到匹配商品"
        } else {
            "匹配到多个商品编码，不能任选一个"
        };
        return Some(stock_product_unavailable(&fragment, reason));
    }
    let row = &rows.rows[0];
    let Some(code) = row.first().and_then(|value| value.as_str()) else {
        return Some(stock_product_unavailable(&fragment, "商品编码解析失败"));
    };
    // 唯一性按 sku_code 决定；sku_name 可能因历史空格/别名在不同批次略有差异，只用于结果展示。
    // 若再把探针的 MAX(sku_name) 写回谓词，会漏掉同 SKU 的其余行并低报库存。
    Some(stock_product_snapshot(
        &stock_sku_predicate(code),
        &fragment,
    ))
}






/// 高频订单聚合模板：时间窗 + 单指标，无维度、无实体。
/// 默认销售额不在这里处理，避免业务 MySQL 重新生成旧口径。
pub fn agg_template(question: &str) -> Option<DirectHit> {
    if !warehouse_sales_metrics(question).is_empty() {
        return None;
    }
    // 维度词（触发分组下钻，回落 sales_breakdown/LLM）。不含"客户/商品"——它们是实体名常见字，
    // "各客户/按商品"靠"各/按"拦，避免误伤"成交客户数""商品销量"这类指标问句。
    const DIM_WORDS: &[&str] = &["排行", "排名", "前", "各", "按", "分类", "省", "市", "区域", "门店", "占比", "对比", "趋势", "明细"];
    if DIM_WORDS.iter().any(|w| question.contains(w)) {
        return None;
    }
    // 🔴 `STRIP_WORDS` 剥得掉、但**本模板兑现不了**的词：与 DIM_WORDS 同一道门先拒。
    //
    // 下面的剥词表换成 `STRIP_WORDS` 之后，这些词会跟着被剥掉，而剥掉兑现不了的词
    // ＝**静默答另一个问题**（`lexicon.rs` 里「只剥装配器能兑现的词」那条纪律）。逐词的账：
    // - 最值/排序（最高/最多/…）：本模板出**单行**，表达不了 ORDER BY/LIMIT；
    // - 疑问实体（哪些/哪个/谁）：要的是名单，本模板给的是一个数；
    // - 多指标（和/与/分别）：模板只会返回一个订单口径指标；
    // - 单位「箱」属于销量语义，不是订单标量模板的职责。
    //
    // 这道门**今天是行为中性的**：这些词一个都不在原来的内联剥词表里，所以含它们的问句
    // 今天全部被剥词守卫拦着（返 None）。它保的是「换用 STRIP_WORDS 之后不许放宽」。
    const UNSUPPORTED: &[&str] =
        &[
            // 最值/排序语义：模板只会出一个合计，答它就是答另一个问题
            "最高", "最多", "最少", "最大", "最小",
            // 「名」挡住「第一名」「排名第一」族。**刻意不挡「第」** ——
            // 挡了会连带拒掉「第二季度销售额是多少」（那句是本轮的正收益）。
            "名",
            // 枚举式提问（模板出的是标量）
            "哪些", "哪个", "谁",
            // 多指标并列：`is_compound` 拆不出「本月销售额和订单数」，模板只会答后一个
            "和", "与", "分别",
            // 单位词：「卖了多少箱」是注册表指标「销量」，模板会把它答成金额
            "箱",
        ];
    if UNSUPPORTED.iter().any(|w| question.contains(w)) {
        return None;
    }
    // 剥词守卫（旧项目实证）：去掉时间/指标/语气/连接词后仍有残留=实体问句，回落 LLM。
    // 例：「恒众餐饮本月客单价」剥后仍有实体名，因此不命中。
    let mut stripped = question.to_string();
    for w in agg_strip_words() {
        stripped = stripped.replace(w, "");
    }
    if stripped.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    let time_pred = time_window(question)?;
    let metric = if question.contains("客户数") || question.contains("成交客户") || question.contains("多少客户") {
        "COUNT(DISTINCT customer_code) AS `成交客户数`"
    } else if question.contains("订单数") || question.contains("多少单") || question.contains("几单") {
        "COUNT(DISTINCT sales_order_code) AS `订单数`"
    } else if question.contains("客单价") {
        "ROUND(SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0), 2) AS `客单价`"
    } else {
        return None;
    };
    let base = |pred: &str| {
        format!(
            "SELECT {metric} FROM t_sales_order \
             WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199') AND {pred}"
        )
    };
    // 上期查询（环比）：平移时间窗。prev_window 出的是列名占位模板，本表的时间列是 order_time
    let prev = prev_window(question).map(|(pred, label)| (base(&fill_time_col(pred, "order_time")), label.to_string()));
    let comparisons = yoy_window(question)
        .map(|(pred, label)| (base(&fill_time_col(pred, "order_time")), label.to_string()))
        .into_iter()
        .collect();
    Some(DirectHit { prev, comparisons, ..hit(base(&time_pred), "direct-agg") })
}





/// 诊断：这个问句为什么没走确定性装配。逐条报出**第一个**不成立的门。
///
/// 🔴 为什么值得有：实测 38 题里 route 分布是 `llm 24 / direct-agg 8 / llm+repair 5 /
/// semantic-cache 1` —— **76% 过 LLM，而全部失败都出在 LLM 路径**（确定性路径至今 0 失败）。
/// 也就是说「提高确定性覆盖」是质量的第一杠杆，而要提高就得先知道**每一句被哪道门卡住**。
/// 靠读代码猜是不行的：`try_compose` 有五道门（指标命中 / 维度命中 / 快照 / 装配 / 残留），
/// 它们只回一个 `None`。
///
/// 与 `try_compose` **共用同一批加载与判据**，不抄第二份：抄了会漂出
/// 「诊断说能装配、实际回落 LLM」。
pub async fn why_not_compose(pg: &sqlx::PgPool, ds: &str, question: &str) -> String {
    if !warehouse_sales_metrics(question).is_empty() {
        return match hardcoded_producer(question) {
            "" => "⓪ 默认销售经营指标只允许共享 DWS 事实路径；注册表旧订单/物流装配已禁用".into(),
            h => format!(
                "⓪ 默认销售经营指标只允许共享 DWS 事实路径；注册表旧订单/物流装配已禁用\n    ⚙ 硬编码兜底：{h}"
            ),
        };
    }
    let verdict = compose_verdict(pg, ds, question).await;
    // 硬编码那一维统一在这里附上（而不是在每个 return 里拼）——
    // 它回答的是「这道题即便装配不了，是不是还有 DMS 专用写死逻辑在兜」，
    // 也就是「本系统还剩多少不通用」。两维一起看才知道该往哪边推。
    match hardcoded_producer(question) {
        "" => verdict,
        h => format!("{verdict}\n    ⚙ 硬编码兜底：{h}"),
    }
}




pub async fn compose_verdict(pg: &sqlx::PgPool, ds: &str, question: &str) -> String {
    use dms_semantic::registry::model as reg;
    // 🔴 这几个**必须与真正的判定同处置**（读失败 = 整条不装配，见 `try_compose` 的 `reg_load!`）。
    // 原来这里对 edges/scopes/snaps 用 `unwrap_or_default()`，于是读不到时诊断照旧报
    // 「✅ 可装配」而运行时回落 LLM —— 正是本文件反复警告的那种「诊断与判定漂」。
    macro_rules! say {
        ($what:literal, $call:expr) => {
            match $call.await {
                Ok(v) => v,
                Err(e) => return format!("⑥ 注册表读失败（{}）：{e}", $what),
            }
        };
    }
    let metrics = say!("meta.metric", reg::load_metrics(pg, ds));
    let policies = say!("meta.metric policy", reg::load_metric_policies(pg, ds));
    let dims = say!("meta.dimension", reg::load_dimensions(pg, ds));
    let edges = say!("meta.join_edge", reg::load_join_edges(pg, ds));
    let scopes = say!("meta.table_scope", reg::load_table_scopes(pg, ds));
    let snaps = say!("meta.table_snapshot", reg::load_table_snapshots(pg, ds));
    let Some((m, m_word)) = pick(question, &metrics, |x| (&x.name, &x.aliases)) else {
        return "① 指标不命中（问句里没有任何已声明指标的名/别名）".into();
    };
    // 与 `try_compose` 逐字同一份判定（含减词）—— 诊断自己重判一遍就会漂
    let Some(d) = pick_excluding(question, &dims, |x| (&x.name, &x.aliases), &m_word)
    else {
        // 维度不命中不等于走不了确定性路径：无维度问句由指标 only 接。
        // 这里**真去跑同一份判定**（`metric_only`）并报出具名理由 ——
        // 诊断自己重判一遍会漂出「诊断说能装配、实际回落」，把三种理由合成一句
        // 则会让下一个人照着报告去补一个不需要的声明。
        return match metric_only(pg, ds, question, agg_template(question).is_some()).await {
            MetricOnly::Hit(_) => format!("✅ 可装配（指标 only）：「{}」", m.name),
            MetricOnly::YieldToTemplate => {
                "⓿ 让路给硬编码模板（`agg_template`）—— 数与 KPI 环比以模板为准，这是刻意的".into()
            }
            MetricOnly::Snapshot => {
                format!("③ 快照门（指标 only）：「{}」的来源表是快照表", m.name)
            }
            MetricOnly::ComposeRefused => format!(
                "② 装配器拒（指标 only，「{}」）—— 口径/聚合含子查询、来源含 UNION、\
                 去重键不全、时间窗放不下（声明的 time_col 不在基表且桥不到），或残留守卫拦",
                m.name
            ),
            MetricOnly::RegistryDown(what) => format!(
                "⑥ 注册表读失败（{what}）—— 不是声明没写，是读不到。确定性装配整条放弃、\
                 回落 LLM，且服务端已 warn。实测这类静默回落让一道稳定 direct-agg 的题\
                 变成 llm+repair 并答错，而当时没有一行日志"
            ),
            // 走到这两支说明上面的 `pick` 与 `metric_only` 判得不一致 —— 那是判据漂了
            MetricOnly::NoMetric | MetricOnly::DimPresent => {
                format!("⚠️ 诊断与判定不一致（「{}」）—— 两处 pick 漂了，查 `metric_only`", m.name)
            }
        };
    };
    if !metric_dimension_allowed(&policies, &m.name, &d.name) {
        return format!("③ 指标维度白名单拒绝：「{}」尚未审定可按「{}」组合", m.name, d.name);
    }
    // 🔴 **不在这里重判快照门**。我原先在这里复制了一份「见快照就拒」，
    // 而 `compose_gated` 本轮改成了「按声明装配」—— 于是诊断继续报「一律不装配」，
    // 说的是一个**已经不存在的行为**。这正是本文件反复强调的那件事：
    // 诊断必须调真正的判定（下面的 `compose_gated`），不许自己再判一遍。
    let vals = say!("meta.value_map", reg::load_value_map(pg, ds));
    // `value_filters` 这里算一遍喂残留守卫，下面 `compose_gated` 内部还会再算一遍 ——
    // 诊断路径低频，这点重复可接受（省掉它得给 compose_gated 加形参，不值）。
    if has_entity_residue(question, m, d, &value_filters(question, &vals, &registry_words(m, d))) {
        // 残留守卫是 E16 抓出来的实证防线，报出来的是「它认为还没被消化的东西」
        return format!(
            "⑤ 残留守卫：命中「{}」×「{}」，但剥掉两者的名/别名与通用虚词后仍有实义残留 \
             → 问句含装配器表达不了的限定（实体名/值过滤/显式年份/单位词）",
            m.name, d.name
        );
    }
    // 走**真正的判定**（含快照声明），不是 `compose_sql_with` —— 后者拿不到 `snaps`，
    // 于是诊断会对快照类问句给出与运行时不同的答案。
    match compose_gated(m, d, question, &edges, &scopes, &snaps, &vals) {
        Some(_) => format!("✅ 可装配：「{}」×「{}」", m.name, d.name),
        None => format!(
            "④ 装配器拒绝：「{}」×「{}」（含 SELECT 的口径 / 多流来源 / UNION / 去重键不全 / \
             找不到 join 路径 / 快照声明不全或与去重键并存 / 值过滤装不上）",
            m.name, d.name
        ),
    }
}




pub fn warehouse_sales_semantics_unavailable(question: &str) -> Option<DirectHit> {
    if warehouse_sales_metrics(question).is_empty() {
        return None;
    }
    // 合同缺失：问句点名的维度/语义确实不在 sales_fact 合同里（文案与回归钉的字节不变）。
    if let Some(unsupported) = warehouse_sales_unsupported_semantic(question) {
        return sales_fact_unavailable(
            question,
            Some(unsupported),
            "当前 sales_fact 合同没有该维度或语义；禁止关联旧订单或物流事实猜算",
            "请切换到已验证数仓，或先补齐独立事实合同",
        );
    }
    // 解析失败：指标认得出、但残余限定没消化完 —— 不是合同缺东西，卡面不许再栽给
    // 「合同没有该维度」（修前两支共用一句文案，解析失败被误读成合同缺失）。
    // 未确认范围保持「未确认限定」：`direct_hit` 靠这四个字识别本卡去探客户主档，一个字不许改。
    let residue = unrecognized_residue(question);
    let residue = if residue.is_empty() { "问句中的部分限定".to_string() } else { residue };
    // 先截断再转义（卡面不是日志，别把整句问句塞进去）：转义会把 `\`/`'` 翻倍，
    // 先转义后截断可能把 `\\`/`''` 对劈开、留下奇数引号 —— 兜底卡自己的 SQL 语法错误。
    let residue: String = residue.chars().take(20).collect();
    let residue = residue.replace('\\', "\\\\").replace('\'', "''");
    sales_fact_unavailable(
        question,
        Some("未确认限定"),
        &format!(
            "问句含未能识别的限定「{residue}」（解析失败，非合同缺失）；禁止关联旧订单或物流事实猜算"
        ),
        "请换个问法重试（如去掉该限定），或先补齐同义词/维度登记",
    )
}





// ── T8-B9：Router 适配器（吃 `AskCtx`，本来就该住在 agent）──

/// 组合器（S3，指标×维度注册表装配）：Router 的 `direct-agg` 成员。
pub fn compose_hit<'a>(cx: &'a crate::ctx::AskCtx<'a>) -> dms_kernel::BoxFut<'a, Option<DirectHit>> {
    Box::pin(async move {
        // ⓿ **让路门必须在这里，管住两条路** —— 不是只放在 `try_compose_metric_only` 里。
        //
        // 🔴 实测翻车（我自己引入又当场抓到的）：给「成交客户数」补了指标声明之后，
        // 「本月成交客户数」被 `try_compose` 装配成**按客户分组的客户数** —— 200 行、每行 1。
        // 因为 `pick(dims)` 会被「成交客户**数**」里的「客户」命中维度「客户」，
        // 而残留守卫剥完指标名+维度名后正好为空，于是一路绿灯。
        // route 仍是 `direct-agg`，**只看路由的断言看不出来**（回归 A09/A12 正是只断言路由）。
        //
        // 判据为什么成立：`agg_template` 自己有 DIM_WORDS 门（含「各/按/排行/分类/省…」即拒），
        // 所以**它接得住的问句必然没有维度词** —— 此时任何维度命中都是伪命中，让路一定对。
        //
        // ⚠️ 这里**只能用 `agg_template`，不能用 `try_direct`**：后者还包含 `sales_breakdown`
        //（销售额×维度的硬编码模板）。拿 `try_direct` 当门会让**所有**销售额×维度问句
        // 都让路给硬编码模板，而它们今天走的是注册表装配 —— 那是把一次窄修变成一次
        // 宽的行为变更（两套模板的 SQL 不同）。第一版我就写错成 `try_direct`，被测试③抓到。
        let agg_template_hit = agg_template(cx.question).is_some();
        if agg_template_hit
            || device_orders(cx.question).is_some()
            || balance_ranking(cx.question).is_some()
            || warehouse_sales_question(cx.question)
            || (cx.source.is_warehouse() && warehouse_finance(cx.question).is_some())
            // 小程序下单有专用 DWS 快照模板（mini_program_order_agg）：组合器的注册表
            // 装配兑现不了「小程序」这个渠道限定，必须让路，不许装出一份丢限定的 SQL
            || (cx.source.is_warehouse() && mini_program_order_agg(cx.question).is_some())
        {
            return None;
        }
        // 顺序即行为：**带维度的先试**。反过来的话「销售额按省份」会被指标 only 接走、
        // 出一个单值 —— 用户要了分组却拿到总数，是答非所问。
        // 让路判定结果透传给指标 only：它对同一问句刚算过 `agg_template`，别再全句重扫
        match try_compose(cx.pg, cx.ds, cx.question).await {
            Some(h) => Some(h),
            None => try_compose_metric_only(cx.pg, cx.ds, cx.question, agg_template_hit).await,
        }
    })
}

/// 手工模板（单号直查 / 高频聚合）：Router 的 `direct-doc` 成员。同步判定，包一层 future。
pub fn direct_hit<'a>(cx: &'a crate::ctx::AskCtx<'a>) -> dms_kernel::BoxFut<'a, Option<DirectHit>> {
    Box::pin(async move {
        // 总量、库存金额、省份/仓库拆解先由同步模板兑现。具体商品库存会被
        // `stock_snapshot` 主动拒绝，才进入下方唯一 SKU 探针；这样既不吞商品限定，
        // 也不会把「湖南库存金额/各省库存金额」误当商品名。
        if let Some(hit) = stock_snapshot(cx.question) {
            return Some(hit);
        }
        if let Some(hit) =
            stock_product_filtered(cx.question, cx.source, cx.p, cx.scope, cx.ds_global).await
        {
            return Some(hit);
        }
        match try_direct_for(cx.question, cx.source.is_warehouse()) {
            // 「未确认限定」兜底卡：残留可能只是客户名。先探一次主档，探明是客户就改走
            // 共享事实合同；探不到再试 ODS 推导，推导也不成照旧返回这张卡（fail-closed 语义不变）。
            // 顺序即行为：合同（共享事实）永远在推导之前，推导只是合同缺失时的降级。
            Some(hit) if hit.sql.contains("'未确认限定'") => match customer_filtered_sales(cx).await {
                Some(contract) => Some(contract),
                None => ods_derive(cx).await.or(Some(hit)),
            },
            // 合同未覆盖卡（销售维度/语义、开票、对账）：先试 ODS 推导，推导不出原卡一字不改。
            Some(hit) if is_unavailable_card(&hit) => ods_derive(cx).await.or(Some(hit)),
            Some(hit) => Some(hit),
            // 同步模板链没接住，而问句是「客户名 + 销售指标」：探主档确认后走共享事实合同
            None => customer_filtered_sales(cx).await,
        }
    })
}

// ─────────── ODS 推导降级（direct-derive）───────────
//
// 合同判定「维度/语义未覆盖」（产出「不可计算」卡）时的显式降级：用 ODS 层明细推导，
// 全程显式标注「推导口径·未经合同验证」。纪律：
// - **fail-closed 顺序不颠倒**：只有 `try_direct_for` 已经产出「不可计算」卡才进这条路；
//   合同在就永远走合同（上面的两个 match 臂就是全部触发点）。
// - **只读红线一条不少**：推导 SQL 过与直连完全同一个 `gate_on`（RawSql → check → 行级
//   注入），行上限 `MAX_ROWS` / 超时 `EXEC_TIMEOUT` 不变。
// - **回落一字不改**：无候选表 / LLM 组不出合法 SQL / 用表越出候选集 / 闸门拒 / 执行失败，
// 一律回落原「不可计算」卡（`or(Some(hit))`），不跌进后面的 LLM 全目录路径 ——
// 那是「合同未覆盖」语义的悄悄改变。
//
// 接线契约：**不需要改 main.rs**。推导复用 `direct_hit` 这条既有接线（Router 的 direct-doc
// 成员），route 值 `direct-derive` 由 `DirectHit.route` 带出，经 `land` 落到
// `AskResult.route` → `query_log.route`；可信凭证等级与 SQL 头标在 agent 侧
// （`ctx::attach_trust` / `hits::mark_derived_sql`），前端徽标认 `route === 'direct-derive'`。

/// 推导资格：只有「DMS 主库 + 数仓源」才有 ODS 层可推。生产 MySQL 在进 Router 前已被
/// business-lookup 硬切；其他数据源没有这份静态目录（召回与卡渲染都按 dms 目录来）。
fn derive_eligible(cx: &crate::ctx::AskCtx<'_>) -> bool {
    // 🔴 推导 = 一次 Precise 模型自由写 SQL。合同没就绪、或这是资料问句的问数臂时
    // （`AskCtx::deterministic_only`），这条路必须关死 —— Router 末位摘掉 `LlmAnswerer`
    // 拦不住它，因为它长在 `direct-doc` 成员**内部**。
    !cx.deterministic_only
        && cx.source.is_warehouse()
        && cx.ds == dms_semantic::registry::datasource::DMS_DS_ID
}

/// LLM 组推导 SQL：仅候选 ODS 表的 schema 卡 + 规则时间窗，一次 precise 调用。
/// `None` = 模型失败 / 没产出可抽取的 SQL —— 调用方回落原卡。
async fn derive_compose(cx: &crate::ctx::AskCtx<'_>, schema: &str) -> Option<String> {
    let pc = crate::PromptCtx {
        schema: schema.to_string(),
        time_tpl: time_predicate(cx.question),
        ..Default::default()
    };
    let system = crate::build_system_prompt(cx.p, &crate::today_cn(), cx.source.dialect());
    let user = crate::prompt::build_user_prompt(&pc, cx.question);
    let req = dms_kernel::ChatRequest::text(
        dms_kernel::ModelTier::Precise,
        &system,
        &user,
        Some(DERIVE_TEMP),
    );
    let reply = match cx.llm.chat(req).await {
        Ok(reply) => reply,
        Err(e) => {
            tracing::warn!(err = %e, "推导 SQL 生成失败 → 回落「不可计算」卡");
            return None;
        }
    };
    (cx.on_usage)(&reply.usage);
    // content 为 None 与「有 content 但抽不出 SQL」是两类失败，日志待遇要一致（都吼出来）
    let Some(content) = reply.content.as_deref() else {
        tracing::warn!(question = %cx.question, "推导 LLM 未返回内容 → 回落「不可计算」卡");
        return None;
    };
    let sql = crate::extract_sql(content);
    if sql.is_none() {
        tracing::warn!(question = %cx.question, "推导未产出 SQL → 回落「不可计算」卡");
    }
    sql
}

async fn ods_derive(cx: &crate::ctx::AskCtx<'_>) -> Option<DirectHit> {
    if !derive_eligible(cx) {
        return None;
    }
    let mut pool =
        dms_semantic::recall::ods_candidate_tables(cx.pg, cx.ds, cx.question, DERIVE_TOP_K).await;
    derive_pool_winc_guard(&mut pool, cx.question);
    if pool.is_empty() {
        tracing::info!(question = %cx.question, "推导无候选 ODS 表 → 回落「不可计算」卡");
        return None;
    }
    // 注册指标清单（闸 1 的通道②语料）：name + source_table，ds 作用域。
    // 与 `reg_load!` 同一条纪律：读失败不许静默吞（静默按空清单走 = 通道②悄悄失效）——
    // 吼出来后按空清单继续（失败方向是更严：通道②不放行，不是放宽）。
    let metrics: Vec<(String, String)> =
        match dms_semantic::registry::model::load_metric_sources(cx.pg, cx.ds).await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "闸 1 通道②语料（meta.metric）读失败 —— 通道②本轮按失效走");
            vec![]
        }
    };
    // 空结果换一轮：候选表「有表无数据」（实测：客户 183507 在 t_winc_sale_report 零行、
    // 在 t_sales_order 4950 行）不等于问题答不出。每轮把试过的表剔出候选池，最多两轮
    // （一轮直连 + 一轮换表，推导是降级路，成本到这里为止）。
    let mut tried: Vec<String> = vec![];
    for _ in 0..2 {
        let remaining: Vec<&str> = pool.iter().copied().filter(|t| !tried.iter().any(|x| x == t)).collect();
        if remaining.is_empty() {
            break;
        }
        // 仅候选表的 schema 卡：LLM 只看得到这些表（卡头即目录合同，粒度/时间/禁用规则随卡给出）。
        // 列语料与卡文本同一次取数 —— 闸 1 的「出处」语料就是 LLM 实际看见的列，一张都不多。
        let mut schema = String::from(
            "（推导口径：合同层未覆盖本问题，以下全部是 ODS 明细表，只允许用这些表推导；\
             禁止引用任何 DWS/ADS 汇总表。结果会标注「未经合同验证」。）\n",
        );
        let mut usable: Vec<&str> = vec![];
        let mut corpus: Vec<(String, Vec<(String, String)>)> = vec![];
        for table in &remaining {
            match dms_semantic::recall::schema_card_with_columns(cx.pg, cx.ds, table).await {
                Ok(Some(card)) => {
                    schema.push_str(&card.text);
                    corpus.push(((**table).to_string(), card.columns));
                    usable.push(*table);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(err = %e, table = %table, "推导候选表 schema 卡读取失败，跳过该表"),
            }
        }
        if usable.is_empty() {
            return None;
        }
        match derive_attempt(cx, &schema, &corpus, &metrics, &usable).await {
            DeriveTry::Hit(sql) => {
                tracing::info!(question = %cx.question, tables = ?usable, "ODS 推导命中（direct-derive）");
                return Some(hit(sql, DERIVE_ROUTE));
            }
            DeriveTry::Empty(used) => {
                tracing::info!(question = %cx.question, tables = ?used, "推导 SQL 合法但零行，换候选表再来一轮");
                tried.extend(used);
            }
            DeriveTry::Failed => return None,
        }
    }
    None
}

/// 一轮推导尝试（组 SQL → 用表校验 → 双语义闸 → 闸门 → 预执行）。
async fn derive_attempt(
    cx: &crate::ctx::AskCtx<'_>,
    schema: &str,
    corpus: &[(String, Vec<(String, String)>)],
    metrics: &[(String, String)],
    usable: &[&str],
) -> DeriveTry {
    let Some(raw) = derive_compose(cx, schema).await else {
        return DeriveTry::Failed;
    };
    // 目录限定名规范化（与组合器同一个出口）：LLM 写裸表名也补成 库.表
    let sql = dms_semantic::registry::warehouse_qualified_source(cx.ds, &raw);
    if !derive_tables_allowed(&sql, usable, cx.source.dialect()) {
        tracing::warn!(question = %cx.question, sql = %sql, "推导 SQL 用表越出候选集 → 回落「不可计算」卡");
        return DeriveTry::Failed;
    }
    // 两道语义闸（判官 E 系列裁决）：只作用于 direct-derive，直连合同路径不经过这里。
    let Some(shape) = analyze_derive_sql(&sql, cx.source.dialect()) else {
        tracing::warn!(question = %cx.question, sql = %sql, "推导 SQL 解析失败 → 回落「不可计算」卡");
        return DeriveTry::Failed;
    };
    // 闸 1 · 标签语义对账：中文取数别名必须在取数表的列名/列注释里有出处
    if let Some(label) = derive_labels_ungrounded(&shape, corpus, metrics) {
        tracing::warn!(question = %cx.question, alias = %label, sql = %sql,
            "推导别名在取数表列名/列注释里无出处（虚构指标/码值劫走）→ 回落「不可计算」卡");
        return DeriveTry::Failed;
    }
    // 闸 2 · JOIN 证据闸：每条跨表等值关联键都要命中合同边或高置信/人工确认的 joinable 边。
    // 无等值关联键的 JOIN 在 `derive_joins_unevidenced` 第一句就返回固定文案、根本不看证据边
    // —— 先判它，省一次 PG 查询
    if shape.unevidenced_joins > 0 {
        tracing::warn!(question = %cx.question, sql = %sql,
            "推导存在无等值关联键的 JOIN（USING/NATURAL/CROSS 或两端表解析不出）→ 回落「不可计算」卡");
        return DeriveTry::Failed;
    }
    if !shape.join_pairs.is_empty() {
        let edges = dms_semantic::recall::join_evidence_edges(cx.pg, cx.ds, usable).await;
        if let Some(join) = derive_joins_unevidenced(&shape, &edges) {
            tracing::warn!(question = %cx.question, join = %join, sql = %sql,
                "推导 JOIN 关联键无证据 → 回落「不可计算」卡");
            return DeriveTry::Failed;
        }
    }
    let candidate = crate::ensure_limit(&sql, cx.source.dialect());
    // 与直连完全同一个闸门：check（只读红线/敏感列/LIMIT）→ 行级权限注入。
    // 红线拒（GuardError）与权限拒（PolicyError，如候选表对受限身份不可证）都回落原卡 ——
    // 回落目标是 fail-closed 占位卡本身，不放大任何可见面。
    let scoped = match crate::gate_on(cx.p, &candidate, cx.scope, cx.ds_global, cx.source.dialect()) {
        Ok(scoped) => scoped,
        Err(e) => {
            tracing::warn!(err = %e, question = %cx.question, "推导 SQL 未过闸门 → 回落「不可计算」卡");
            return DeriveTry::Failed;
        }
    };
    // 预执行一次（行上限/超时与直连相同）：执行失败（列漂移/超时）必须回落原卡，
    // 而不是把失败交给 `land` 跌进后面的 LLM 全目录路径。
    // 零行不报错但报「空」—— 调用方换候选表再来一轮（有表无数据 ≠ 答不出）。
    match cx.source.fetch(&scoped, crate::MAX_ROWS, crate::EXEC_TIMEOUT).await {
        // 聚合查询零命中时返回的是「单行全 NULL」（SUM 恒出一行），不是零行 —— 同样算「空」，
        // 否则「有表无数据」换表机制对聚合题永远失效（实测：t_winc_sale_report 过滤一空
        // 出 [[null,null]]，被当成命中落了地）。
        Ok(rs)
            if rs.rows.is_empty()
                || rs.rows.iter().all(|row| row.iter().all(|v| v.is_null())) =>
        {
            // 试过的表 = 候选集里名字出现在 SQL 中的那些（table_names_of 会把库名限定
            // 解析成「dms_ods」，排除失效 —— 实测）。候选名足够独特，子串匹配即可。
            // 整条 SQL 只小写化一次（原来每张候选表都在 filter 闭包里各算一遍）
            let sql_low = sql.to_lowercase();
            let used: Vec<String> = usable
                .iter()
                .filter(|t| sql_low.contains(&t.to_lowercase()))
                .map(|t| (*t).to_string())
                .collect();
            DeriveTry::Empty(used)
        }
        Ok(_) => DeriveTry::Hit(candidate),
        Err(e) => {
            tracing::warn!(err = %e, question = %cx.question, "推导 SQL 执行失败 → 回落「不可计算」卡");
            DeriveTry::Failed
        }
    }
}

/// 「恒众餐饮本月买了多少」这一族：整条同步模板链接不住（残的是客户名，不是维度词），
/// 但又是明确的有主档可查的取数意图。共享 resolver 必须唯一绑定 canonical code/name；
/// 零命中、歧义或探针故障都不允许任选第一条扩大查询面。
async fn customer_filtered_sales(cx: &crate::ctx::AskCtx<'_>) -> Option<DirectHit> {
    if !cx.source.is_warehouse() {
        return None;
    }
    if warehouse_sales_metrics(cx.question).is_empty()
        || warehouse_sales_has_unsupported_semantics(cx.question)
    {
        return None;
    }
    let fragment = customer_name_fragment(cx.question)?;
    let binding = match crate::entity_resolver::resolve_customer(
        cx,
        &fragment,
        crate::entity_resolver::CustomerMatchField::Auto,
    )
    .await
    {
        Ok(crate::entity_resolver::CustomerResolution::Unique(binding)) => binding,
        Ok(crate::entity_resolver::CustomerResolution::NotFound) => return None,
        Ok(crate::entity_resolver::CustomerResolution::Ambiguous(candidates)) => {
            tracing::info!(
                question = %cx.question,
                fragment,
                candidates = candidates.len(),
                "客户主档解析歧义 → 不选择第一条，回落后续路径"
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(err = %error, question = %cx.question, "共享客户解析失败 → 按未探明回落");
            return None;
        }
    };
    warehouse_sales_fact_predicated(cx.question, Some(&binding))
}
