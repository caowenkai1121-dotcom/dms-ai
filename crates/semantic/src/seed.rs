//! 表级业务语料种子：⚠️ 警告 / 关键词强制补表 / 口径教训 / 表级标准口径 / JOIN 边 / 数据源。
//! 变更原因＝DMS 业务语料本身。四张语义注册表（指标/维度/码值/术语）的种子在 `seed_defs.rs`。
//!
//! 搬运源 `server/src/meta.rs:392-519/631-677/1705-1722`。**字面量一个字符不许改**：
//! 那些 ⚠️ 文案是连库验证过的资产，且 LLM 会逐字读它们。
//! 本轮**不外置成 `.sql`**：验收标准是「逐表 SELECT 全量对拍全等」，那要连库，而 PG 连不上。

use crate::registry::datasource::DMS_DS_ID;
use crate::registry::element::sync_elements;
use crate::ops_caliber::seed_ops_caliber;
use crate::seed_defs::{seed_dimensions, seed_metrics, seed_terms, seed_value_maps};
use sqlx::PgPool;

/// 种子编排（幂等）。**顺序即行为**：元素派生必须在四张注册表灌完之后。
pub async fn seed(pg: &PgPool) -> anyhow::Result<()> {
    seed_warns(pg).await?;
    seed_table_comments(pg).await?;
    crate::warehouse_catalog::seed(pg, DMS_DS_ID).await?;
    seed_kw_force(pg).await?;
    seed_metrics(pg).await?;
    seed_dimensions(pg).await?;
    seed_value_maps(pg).await?;
    // 运营看板 v0.1.19：独立模块承载活动/巡店口径，避免继续膨胀通用 DMS 种子。
    seed_ops_caliber(pg).await?;
    invalidate_stale_exemplars(pg).await?;
    seed_join_edges(pg).await?;
    seed_table_scopes(pg).await?;
    // 顺序即行为：**手写的先插**，这一步才 `ON CONFLICT DO NOTHING` 地补剩下的。
    // 反过来会让手写的业务口径（订单表的「有效订单」）被一条 deleted_flag = 0 抢占。
    seed_soft_delete_scopes(pg).await?;
    seed_table_snapshots(pg).await?;
    seed_value_domains(pg).await?;
    seed_document_families(pg).await?;
    seed_pitfalls(pg).await?;
    seed_terms(pg).await?;
    sync_elements(pg).await?;
    Ok(())
}

/// 单据族注册表持久化：定义来自 `document::DOCUMENT_FAMILIES`，这里只负责幂等写入审核面。
async fn seed_document_families(pg: &PgPool) -> anyhow::Result<()> {
    for f in crate::document::DOCUMENT_FAMILIES {
        let details: Vec<String> = f.details.iter().map(|(t, c)| format!("{t}:{c}")).collect();
        sqlx::query(
            "INSERT INTO meta.document_family
             (ds_id, family_code, name, prefixes, header_table, header_code_col, detail_bindings,
              evidence, warehouse_available)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (ds_id, family_code) DO UPDATE SET
               name=$3, prefixes=$4, header_table=$5, header_code_col=$6,
               detail_bindings=$7, evidence=$8, warehouse_available=$9,
               status='active', updated_at=now()",
        )
        .bind(crate::registry::datasource::DMS_DS_ID)
        .bind(f.code)
        .bind(f.name)
        .bind(f.prefixes)
        .bind(f.header_table)
        .bind(f.header_code_col)
        .bind(details)
        .bind(f.evidence)
        .bind(f.warehouse_available)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 指标版本变化后，引用旧版本的 VQR 样例自动失效，等待重新执行验证。
async fn invalidate_stale_exemplars(pg: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE meta.sql_exemplar e
         SET validation_status='stale', invalid_reason='指标口径版本已变化，需要重新执行验证'
         WHERE e.validation_status='valid' AND e.metric_versions<>'' AND EXISTS (
           SELECT 1 FROM unnest(string_to_array(e.metric_versions, ',')) v
           LEFT JOIN meta.metric m ON m.ds_id=e.ds_id AND m.metric_code=split_part(v,'@',1)
           WHERE m.metric_code IS NULL OR m.version<>split_part(v,'@',2)
         )",
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// 高危表 ⚠️ 警告 + 关键词强制补表：旧项目 90+ 轮连库判官坐实的资产，全量迁移（幂等）
async fn seed_warns(pg: &PgPool) -> anyhow::Result<()> {
    const WARNS: &[(&str, &str)] = &[
        ("t_sales_order_detail", "【⚠️订单明细含系统级2x重复行：同单号+sku+名称+数量+金额整行×2(ETL双写)。订单明细聚合/展示前【必须】按(单号,sku,名称,数量,金额) GROUP BY 去重，否则订单明细数量/订单明细金额虚增一倍；默认销售额=销售宽表 SUM(amount)、默认销量=SUM(qty)，均不取本表】"),
        ("t_sales_order_his_detai", "【⚠️历史订单明细含系统级2x重复行：同单号+sku+名称+数量+金额整行×2(ETL双写)。订单明细聚合/展示前【必须】按(单号,sku,名称,数量,金额) GROUP BY 去重，否则订单明细数量/订单明细金额虚增一倍；默认销售额=销售宽表 SUM(amount)、默认销量=SUM(qty)，均不取本表】"),
        ("t_marketing_goods", "【⚠️这不是商品主档！分类映射全空(答出全是'其他/未分类')。商品主档用 t_goods，商品分类用 t_goods_category 且按 g.goods_category_code=cat.id 连】"),
        ("t_goods", "【⚠️连商品分类表的正确外键是 goods_category_code(=cat.id)，不是 category_code/category_id——那两个是幻觉列,写了必 1054】"),
        ("t_sales_order_import", "【⚠️这是订单【导入批次】表(手工导入暂存),不是正式订单明细！订单额、订单数与订单单据明细使用 t_sales_order / t_sales_order_detail；默认销售额=销售宽表 SUM(amount)、默认销量=SUM(qty)；商品分类必须使用独立已验证 ADS/DWS 资产，不能从默认销售事实猜字段】"),
        ("t_customer_balance", "【⚠️balance 列是滚动快照，绝不可 SUM(会把每行累计重复加，实测答出 10 倍虚增)。流量必须 SUM(amount) 并按 created_time 筛；余额排行必须 ROW_NUMBER() OVER(PARTITION BY customer_code,balance_type ORDER BY created_time DESC,id DESC) 取各桶最新再 SUM；行级条件(如 balance > 0)必须放在 rn=1 之后的【外层】——先取最新一行再判条件；放进子查询＝拿到过期快照(实测多算 5 个客户 29≠24)。余额含义按 balance_type 分桶，现金余额='8'/'9'】"),
        ("t_winc_stock_report", "【⚠️每日快照表(N 天×N 仓)：聚合库存必须 product_stock_date=(SELECT MAX(product_stock_date) FROM t_winc_stock_report) 取最新快照，直接 SUM 会把快照天数累加虚增几十倍】"),
        ("t_warehouse_manage", "【⚠️管理档案表，不是实时库存(actual_quantity 全 NULL，数值严重失真)。库存问答用 t_winc_stock_report 最新快照】"),
        ("t_device_requisition", "【⚠️actual_deduct_amount(实际扣款) 全 0(用了必答假 0)。设备金额用 t_device_receive_item.amount(金额) 或 t_device_requisition.purchase_amount(购买金额)；押金流量用 t_customer_balance balance_type IN('10','11','12','13','14')+SUM(amount)+created_time 筛】"),
        ("t_device_receive_item", "【⚠️actual_deduct_amount(实际扣款) 全 0(用了必答假 0)。设备金额用本表 amount(金额) 或 t_device_requisition.purchase_amount(购买金额)】"),
        ("t_sales_order_short", "【⚠️精简子表仅十几行，订单统计/单据与履约问答勿用。订单额、订单数用 t_sales_order，物流追踪用 t_sales_order_logistics；默认销售额、销量不取这些表】"),
        ("t_market_claim_header", "【⚠️application_date(报销申请提交时间) 全表【全 NULL】(按它筛必假0)。申请时间用 applied_time(全覆盖) 或 created_time；accounting_date=财务记账时间(未记账单为空)】"),
        ("t_market_activity_promoter_expense", "【⚠️activity_date 全 NULL，时间过滤只准用 created_time，按 activity_date 筛月份必得 0 行假结论】"),
        ("t_winc_sale_transfer", "【⚠️本表【没有 deleted_flag 列】，加 deleted_flag=0 必 1054。它是 WinC ETL 原始流水；仅在用户明确询问 WinC/渠道流水时才考虑 t_winc_sale_report(有 deleted_flag)。默认销售额必须取 sales_dw.dws_off_offline_sale_dfn 的 SUM(amount)，默认销量取 SUM(qty)】"),
        ("t_winc_stock_transfer", "【⚠️本表【没有 deleted_flag 列】，加 deleted_flag=0 必 1054。它是 WinC ETL 原始流水；仅在用户明确询问 WinC/渠道流水时才考虑 t_winc_sale_report(有 deleted_flag)。默认销售额必须取 sales_dw.dws_off_offline_sale_dfn 的 SUM(amount)，默认销量取 SUM(qty)】"),
        ("t_market_marketing_expense", "【⚠️只是费用族的一个专项子表(约占全部费用5%)。泛指市场费用/营销费用/费用总额一律用 t_market_total_expense(合计摘要表) SUM(expense_amount)；只在明确问『营销专项费用』时才用本表】"),
        ("t_device_demand_apply_detail", "【⚠️设备需求申请中间表(全表仅个位数行)，device_total_amount 不能当设备费用总额。设备金额用 t_device_receive_item.amount 或 t_device_requisition.purchase_amount】"),
        ("t_marketing_zone_product", "【⚠️营销专区商品配置表(仅十几行,start_time 常空)，不是商品上架记录！『新上架/新品』集合用 t_goods：on_sale=1(上架中)+created_time 近N天(近似新上架时间)；销售额按商品编码过滤 sales_dw.dws_off_offline_sale_dfn 后 SUM(amount)，销量 SUM(qty)】"),
        ("t_new_market_product", "【⚠️运营位配置表(人工挑选的展示清单)，不是商品上架记录！『新上架/新品』集合用 t_goods：on_sale=1+created_time 近N天；销售额=销售宽表 SUM(amount)、销量=SUM(qty)、毛利额=SUM(gross_profit)，禁止从订单明细推算】"),
        ("t_customer_price", "【⚠️每行=一个客户×商品的现行价目档，本库【无价格变更历史】：『调整/变更次数』不可算。goods_name 带『重复-』前缀是人工标记脏档宜排除；price=9999 哨兵必须排除】"),
        // ── 以下来自 2026-07-26 六域并行作题 workflow 的连库实测疑点 ──
        ("t_activity_promoter_fee", "【⚠️person_type 已退化(3828 行空串+2036 行 NULL，仅 7 行有码)，按人员类型统计临促费用不可行；is_expense 列注释称『1是0否』但全表【全 NULL】，按它过滤必得 0 行假结论。时间列 start_date 与 created_time 在年粒度差异<0.02%】"),
        ("t_master_shop", "【⚠️门店维度在本库基本不可用：t_sales_order.shop_name 20.6 万单中 20.5 万单为空(仅约 1000 单有值/419 个门店)，按门店分组的经营分析无意义，应向用户说明而非答出稀疏结果】"),
        ("t_invoice_apply_header", "【⚠️发票双流并行且【两表都在持续写入】：老表 IO* 单今年 1925 单 7612 万 > 新表 t_invoice_new_apply_header SQ* 单 826 单 6986 万，交集为 0。问全量开票必须 UNION ALL 两表(只查一张漏 52%)；时间列用 apply_time——invoice_time 全表【全 NULL】】"),
    ];
    // 这一整套种子都是 DMS 业务语料 → 固定落 'dms' 那一格（其余列靠 DDL 的 DEFAULT 'dms'）
    for (t, w) in WARNS {
        sqlx::query("UPDATE meta.table_doc SET warn = $2 WHERE table_name = $1 AND ds_id = $3")
            .bind(t)
            .bind(w)
            .bind(DMS_DS_ID)
            .execute(pg)
            .await?;
    }
    Ok(())
}

/// 表注释里**张冠李戴**的那几条修正（表名 → 正确用途 → 证据）。
///
/// 🔴 由来：`meta.table_doc.table_comment` 采自 MySQL 的 `TABLE COMMENT`，而 DMS 建表时
/// 有几张是复制粘贴没改注释的。这个字段**直接进 LLM prompt**，于是：
///   `t_regions`（4715 行省市区表）在 prompt 里的说明是「开票申请单」——
///   问「湖南省的销售额」时模型看到的地区表自称开票单，会绕开它去猜别的表。
///
/// 判据是「**同一条 comment 被多张表共用**」，不是我觉得哪条不对：
///   「开票申请单」×7（含 `t_regions`、`t_xh_bom_detail`）、「活动场地费用表」×4。
/// 共用本身不必然是错（`t_erp_invoice_header`/`_detail` 同族共用是对的，
/// `t_device_demand_*` 与其 `_3` 分表共用也是对的），所以逐张核过表名与列注释才动手。
///
/// **源码不是权威**：`xh-dms` 的 javadoc 同样会复制粘贴 ——
/// `t_interface_log`（接口日志）的类注释写的是「商品分类数据对象」。
/// 所以每条修正都记了它的独立证据，而不是「源码这么说」。
async fn seed_table_comments(pg: &PgPool) -> anyhow::Result<()> {
    // (表名, 正确用途, 证据)
    const FIX: &[(&str, &str, &str)] = &[
        (
            "t_regions",
            "行政区域主档（省/市/区三级，4715 行）",
            "库里原写「开票申请单」；xh-dms `Regions.java` javadoc = 行政区域",
        ),
        (
            "t_xh_bom_detail",
            "BOM 组套明细：一个组套由哪些 SKU 按什么数量和分摊比例组成",
            "库里原写「开票申请单」；源码无 javadoc，按列自证 \
             (bom_code/sku_code/quantity/share_ratio/quantity_um)",
        ),
        (
            "t_delivery_warehouse_address",
            "收货地址 → 发货仓库的对应关系（315 行）",
            "库里原写「活动场地费用表」（真主是 t_activity_venue_fee）；\
             xh-dms javadoc = 地址对应发货仓库",
        ),
        (
            "t_delivery_warehouse_stock",
            "地址对应发货仓库的库存（10227 行）",
            "库里原写「活动场地费用表」；xh-dms javadoc = 地址对应发货仓库存",
        ),
    ];
    let mut missed: Vec<&str> = vec![];
    for (t, c, _why) in FIX {
        // 🔴 写 `custom_comment` 不写 `table_comment`：后者是 MySQL `COMMENT` 的原文副本，
        // 被 `ingest::schema_sync::upsert_table_doc` 的 `ON CONFLICT DO UPDATE` **无条件覆盖**。
        // 上一版写原生列，能活下来纯靠 `meta sync` 子命令里 seed 恰好排在 sync 之后 ——
        // 顺序一换就静默失效，而症状是「prompt 里 t_regions 又自称开票申请单」，没有断言会红。
        // 渲染侧 `recall::schema::render_schema` 用 `COALESCE(NULLIF(custom_comment,''), …)` 取它。
        let r = sqlx::query(
            "UPDATE meta.table_doc SET custom_comment = $2 WHERE table_name = $1 AND ds_id = $3",
        )
        .bind(t)
        .bind(c)
        .bind(DMS_DS_ID)
        .execute(pg)
        .await?;
        if r.rows_affected() == 0 {
            missed.push(t);
        }
    }
    // 一条都没改到是**合法**的（首次启动、`sync_schema` 还没跑，table_doc 是空的），
    // 所以不 bail；但静默也不行 —— 表名打错时症状正是「一条都没改到」，而那看不见。
    if !missed.is_empty() {
        tracing::warn!(
            tables = ?missed,
            "表注释修正没落到任何行 —— 若 meta.table_doc 非空，说明表名写错了"
        );
    }
    warn_shared_table_comments(pg).await
}

/// 🔴 `sync_schema` 的 upsert **不许**碰人工列。源码扫描判据 ——
/// 那两条 `ON CONFLICT DO UPDATE` 一旦把 `custom_comment` 写进 SET 列表，
/// 人工注释就又活不过下一次 `meta sync`，而症状（prompt 里表注释变回原文）没有断言会红。
#[cfg(test)]
#[test]
fn schema_sync_never_overwrites_custom_comment() {
    let src = include_str!("ingest/schema_sync.rs");
    for f in ["upsert_table_doc", "upsert_column_doc"] {
        let body = src
            .split(&format!("async fn {f}"))
            .nth(1)
            .unwrap_or_else(|| panic!("{f} 不见了 —— 判据锚点失效"))
            .split("\n}")
            .next()
            .unwrap();
        let set = body
            .split("DO UPDATE SET")
            .nth(1)
            .unwrap_or_else(|| panic!("{f} 里没有 DO UPDATE SET —— 判据锚点失效"));
        // SET 列表到该语句结尾（`"` 收尾）
        let set = set.split('"').next().unwrap_or(set);
        assert!(
            !set.contains("custom_comment"),
            "{f} 的 DO UPDATE SET 里出现了 custom_comment —— 人工注释会被 meta sync 抹掉：{set}"
        );
    }
    // 反向自证：两个函数确实都有 DO UPDATE SET（否则上面的断言是空转的）
    assert_eq!(src.matches("DO UPDATE SET").count(), 2, "DO UPDATE 的数量变了，回来核判据");
}

/// 防复发守卫：扫出**仍被多张不同族表共用**的 comment 并 warn。
///
/// 「同族」判据 = 表名**前两段**相同（`t_erp_invoice_header` / `t_erp_invoice_detail` 是同族，
/// `t_device_demand_month_quota` 与它的 `_3` 分表是同族）。所以这条对分表和主从表天然免疫，
/// 只会对 `t_regions` 撞上 `t_erp_invoice_*` 这种真·张冠李戴开火。
/// 只 warn 不 bail：DMS 那边新增一张复制粘贴注释的表不该让本服务起不来。
async fn warn_shared_table_comments(pg: &PgPool) -> anyhow::Result<()> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_comment, string_agg(table_name, ', ' ORDER BY table_name)
         FROM meta.table_doc
         WHERE ds_id = $1 AND COALESCE(table_comment, '') <> ''
         GROUP BY table_comment
         HAVING count(*) >= 3",
    )
    .bind(DMS_DS_ID)
    .fetch_all(pg)
    .await?;
    for (c, ts) in rows {
        let families: std::collections::HashSet<String> =
            ts.split(", ").map(|t| family_of(t)).collect();
        if families.len() >= 2 {
            tracing::warn!(
                comment = %c, tables = %ts, families = families.len(),
                "同一表注释被多个不同族的表共用 —— 疑似复制粘贴，它会原样进 LLM prompt"
            );
        }
    }
    Ok(())
}

/// 表名的「族」= 前两个下划线段（`t_erp_invoice_header` → `t_erp`）。
/// 纯函数，判据见 `family_immunises_shards_not_strangers`。
fn family_of(t: &str) -> String {
    t.split('_').take(2).collect::<Vec<_>>().join("_")
}

/// 指标/维度来源表的**软删除口径**，数据驱动登记（不手写清单）。
///
/// 🔴 由来：`table_scope.filter` 会被装配器**确定性地补**到每条 SQL 上，而实测
/// 45 张来源表里只有 4 张登记了 —— 其余 41 张的查询一律不带 `deleted_flag = 0`，
/// 已删行照算。其中 `t_after_sales_order_header` 是**指标**来源表（退款额），
/// 少这一条就是错数；维度来源表少这一条则多出已删的分组。
///
/// 证据来自 DMS 后端自己的 182 个 Mapper XML：把每张表的查询块里的固定过滤按频次统计，
/// `deleted_flag = 0` 在 `t_customer_device_ledger` 5/5、`t_account_bill_header` 4/4、
/// `t_goods_sale_information` 8/8 —— 也就是说这是 DMS 侧的恒成立口径，不是某个场景的条件。
/// （状态类口径没捞到，因为 DMS 那边是 `#{}` 参数化的，那本来就不该当表级口径。）
///
/// **为什么数据驱动而不是手写 41 行**：手写清单会漂 —— 以后新增指标/维度时没人记得回来补，
/// 而漏补的症状是「数悄悄虚高」，没有任何报错。这条 SQL 让新增来源表自动受益。
///
/// 三条安全约束：
/// ① 只对**真有 `deleted_flag` 列**的表登记（`meta.column_doc` 反查）—— 否则 SQL 报 1054。
///    实测 42 张候选里有 1 张没这列（`t_sales_order_detail(JOIN …)` 那种带注解的写法，
///    本体已单独登记），所以这条 EXISTS 是承重的，不是保险。
/// ② `ON CONFLICT DO NOTHING`：**手写的登记优先**，绝不覆盖（手写那 4 张带业务口径，
///    比如订单表的「有效订单」，被一条 `deleted_flag = 0` 盖掉就是口径倒退）。
/// ③ 来源表名要剥掉注解与别名：声明里有 `t_sales_order_detail(JOIN …)` 和 `t_x b0` 两种形态。
///
/// 已知风险与它的失败方向：若某张表的 `deleted_flag` 语义相反（1=未删），这条过滤会把
/// 全部行滤掉 → **返 0 行**。那是响亮的失败（用户当场看见「没有数据」），
/// 不是静默错数 —— 与本仓「宁可回落/报空，不出错数」的口径一致。
async fn seed_soft_delete_scopes(pg: &PgPool) -> anyhow::Result<()> {
    let r = sqlx::query(
        "INSERT INTO meta.table_scope (table_name, filter, note, ds_id)
         SELECT s.tbl, 'deleted_flag = 0',
                'DMS Mapper XML 实测的恒成立口径（数据驱动登记，见 seed_soft_delete_scopes）',
                $1
         FROM (
             SELECT DISTINCT regexp_replace(split_part(trim(source_table), ' ', 1), '\\(.*$', '') AS tbl
             FROM meta.metric WHERE ds_id = $1
             UNION
             SELECT DISTINCT regexp_replace(split_part(trim(source_table), ' ', 1), '\\(.*$', '')
             FROM meta.dimension WHERE ds_id = $1 AND status = 'active'
         ) s
         WHERE s.tbl LIKE 't\\_%'
           AND EXISTS (
               SELECT 1 FROM meta.column_doc c
               WHERE c.ds_id = $1 AND c.table_name = s.tbl AND c.column_name = 'deleted_flag'
           )
         ON CONFLICT DO NOTHING",
    )
    .bind(DMS_DS_ID)
    .execute(pg)
    .await?;
    if r.rows_affected() > 0 {
        tracing::info!(added = r.rows_affected(), "补登来源表的软删除口径（deleted_flag = 0）");
    }
    Ok(())
}

async fn seed_kw_force(pg: &PgPool) -> anyhow::Result<()> {
    // 召回读取允许 ds_id='*'；历史全局核心词会与当前 DMS 锚点同时命中，必须先 fail-closed 清掉。
    sqlx::query(
        "DELETE FROM meta.kw_force WHERE ds_id = '*' AND keyword IN \
         ('销售','销售额','销量','毛利','订单','订单额','订单数')",
    )
    .execute(pg)
    .await?;
    const KW_FORCE: &[(&str, &str)] = &[
        ("押金", "t_customer_balance"), ("信控", "t_customer_balance"), ("余额", "t_customer_balance"), ("欠款", "t_customer_balance"),
        ("售后", "t_after_sales_order_header"), ("退货", "t_after_sales_order_header"), ("退款", "t_after_sales_order_header"),
        ("开票", "t_invoice_apply_header"), ("发票", "t_invoice_apply_header"),
        ("对账", "t_account_bill_header"), ("账单", "t_account_bill_header"),
        ("发货", "t_sales_order_logistics"), ("运单", "t_sales_order_logistics"), ("物流", "t_sales_order_logistics"),
        ("分类", "t_goods_category"), ("品类", "t_goods_category"), ("类别", "t_goods_category"),
        ("市场费用", "t_market_total_expense"), ("营销费用", "t_market_total_expense"),
        ("推广费", "t_market_total_expense"), ("费用总额", "t_market_total_expense"),
        ("新上架", "t_goods"), ("新品", "t_goods"), ("上架", "t_goods"),
        ("库存", "t_winc_stock_report"),
        // 专项费用短语必须先于泛「活动」登记；同一句会同时命中时，专项表先进入候选。
        ("活动临促人员费用", "t_activity_promoter_fee"),
        ("活动临促人员的费用", "t_activity_promoter_fee"),
        ("临促人员费用", "t_activity_promoter_fee"),
        ("活动执行人员费用", "t_market_activity_promoter_expense"),
        ("活动执行人员的费用", "t_market_activity_promoter_expense"),
        ("执行人员费用", "t_market_activity_promoter_expense"),
        ("活动", "t_activity_main"),
        ("促销员", "t_activity_promoter_fee"),
        // 默认销售经营指标只认已验证 DWS 事实；订单行为仍由订单头承载。
        // 显式写 ds_id：这些 DMS 语义资产不得泄漏到其它数据源。
        ("销售", crate::sales_fact::TABLE_NAME), ("销售额", crate::sales_fact::TABLE_NAME),
        ("销量", crate::sales_fact::TABLE_NAME), ("毛利", crate::sales_fact::TABLE_NAME),
        ("订单", "t_sales_order"), ("订单额", "t_sales_order"), ("订单数", "t_sales_order"),
        ("买过", "t_sales_order_detail"), ("购买", "t_sales_order_detail"),
        ("客户", "t_customer"), ("商品", "t_goods"),
        ("员工", "t_employee"), ("门店", "t_master_shop"),
    ];
    for (kw, t) in KW_FORCE {
        sqlx::query(
            "INSERT INTO meta.kw_force(ds_id, keyword, table_name) VALUES ($1, $2, $3)
             ON CONFLICT (ds_id, keyword) DO UPDATE SET table_name = $3",
        )
        .bind(DMS_DS_ID)
        .bind(kw)
        .bind(t)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 表级标准口径声明 `(表, 过滤, note)`：该表被任何查询触及时都**恒需**成立的过滤
/// （口径单一事实源）。`filter` 里的列会被 `registry::caliber` 变成 `RequireCols` 判据，
/// 且被 `correct_caliber` 确定性补到 LLM 生成的 SQL 上（裁决 二·G3）——
/// **所以放进来的必须是恒需的，任何随问法而变的口径都属指标级**（裁决 二·J′）。
///
/// 模块级 `pub`：构建期要用 `check_caliber` 校验确定性模板是否满足这些声明，
/// 而声明在函数体里测试够不到。**只提这一组** —— `seed_defs.rs` 的 `METRICS`/`MAPS`
/// 靠逐行对拍验收，外提会改动种子行的行首空白（见那个文件头）。
pub const TABLE_SCOPES: &[(&str, &str, &str)] = &[
    ("t_sales_order",
     "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
     "有效订单：剔除暂存0/无效108/作废199。显式订单额、订单数或订单明细查询 JOIN 订单主表时同样适用（漏则订单口径虚增）"),
    ("t_customer", "deleted_flag = 0", "客户主档软删过滤"),
    ("t_goods", "deleted_flag = 0", "商品主档软删过滤"),
    // 订单明细表只留软删；`item_type` 属于显式订单明细指标，不能扩散到默认 DWS 销售指标。
    ("t_sales_order_detail",
     "deleted_flag = 0",
     "软删过滤是这张表唯一恒需的口径；item_type 随显式订单明细指标变化，不得登记为表级过滤。\
      本表只用于订单额相关明细、买过关系和单据下钻；默认销售额=销售宽表 SUM(amount)、\
      默认销量=SUM(qty)、毛利额=SUM(gross_profit)，禁止从订单明细推算默认销售经营指标"),
];

/// JOIN 边种子（全部来自已连库坐实的模板连接键；cardinality 标注扇出方向）
/// 表级标准口径种子：该表被任何查询触及时都应成立的过滤（口径单一事实源）
async fn seed_table_scopes(pg: &PgPool) -> anyhow::Result<()> {
    for (t, f, note) in TABLE_SCOPES {
        sqlx::query(
            "INSERT INTO meta.table_scope(table_name, filter, note) VALUES ($1,$2,$3)
             ON CONFLICT (ds_id, table_name) DO UPDATE SET filter=$2, note=$3",
        )
        .bind(t).bind(f).bind(note)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 快照/流水表种子：同一分区键有多条历史行，取数必须只留最新一条。
async fn seed_table_snapshots(pg: &PgPool) -> anyhow::Result<()> {
    // (table, partition_cols, order_cols, extra_filter, note)
    const SNAPS: &[(&str, &str, &str, &str, &str)] = &[
        ("t_customer_balance", "customer_code,balance_type", "created_time DESC, id DESC",
         "balance_status = '4'",
         "余额流水快照表：同一 (客户,余额类型) 有多条历史，必须取最新一条\
          （ROW_NUMBER() OVER (PARTITION BY customer_code,balance_type ORDER BY created_time DESC, id DESC) 后取 rn = 1），\
          否则历史行被重复求和。balance_status='4' 是生效行（CustomerBalanceMapper.xml L40/L113 权威）。\
          实测漏此口径：「账户余额最高的10个客户」第 1 名客户答错、「哪些客户还有信控余额」21 行 vs 正确 23 行"),
    ];
    for (t, part, ord, extra, note) in SNAPS {
        sqlx::query(
            "INSERT INTO meta.table_snapshot(table_name, partition_cols, order_cols, extra_filter, note)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (ds_id, table_name)
             DO UPDATE SET partition_cols=$2, order_cols=$3, extra_filter=$4, note=$5",
        )
        .bind(t).bind(part).bind(ord).bind(extra).bind(note)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 实体名值域种子：只登记「哪一列的取值是业务实体名」。**取值本身**由 `meta autodiscover`
/// 的名称型探针灌进 `meta.value_map`（`name = code = 取值`，复用码值表不新建；重跑即自适应）。
async fn seed_value_domains(pg: &PgPool) -> anyhow::Result<()> {
    // (table, column, note)
    const DOMAINS: &[(&str, &str, &str)] = &[
        ("t_goods_category", "category_name",
         "商品分类名（60 个取值，如「手抓饼」「烤肠」）。问「X 这个分类」时过滤必须写 \
          cat.category_name LIKE '%X%' 并按 d.sku_code=g.goods_code→g.goods_category_code=cat.id 连过来，\
          【不要】写 d.sku_name LIKE '%X%'——商品名含「手抓饼」却属别的分类的商品会被算进来\
          （实测「手抓饼这个分类卖了多少箱」156847 vs 正确 115175，虚高 36%）"),
    ];
    for (t, c, note) in DOMAINS {
        sqlx::query(
            "INSERT INTO meta.value_domain(table_name, column_name, note) VALUES ($1,$2,$3)
             ON CONFLICT (ds_id, table_name, column_name) DO UPDATE SET note=$3",
        )
        .bind(t).bind(c).bind(note)
        .execute(pg)
        .await?;
    }
    Ok(())
}

async fn seed_join_edges(pg: &PgPool) -> anyhow::Result<()> {
    // (left_table, left_col, right_table, right_col, card, note)
    const EDGES: &[(&str, &str, &str, &str, &str, &str)] = &[
        ("t_sales_order", "sales_order_code", "t_sales_order_detail", "sales_order_code", "1:N",
         "订单头→订单明细扇出（且 detail 有 2x 重复行，须去重——SUM 订单额等订单头列严禁走此边；默认销售额不走此边）"),
        ("t_sales_order", "customer_code", "t_customer", "customer_code", "N:1", "订单→客户主档"),
        ("t_sales_order", "owner_manager", "t_employee", "employee_id", "N:1", "订单→业务员"),
        ("t_sales_order_detail", "sku_code", "t_goods", "goods_code", "N:1", "明细→商品主档"),
        ("t_goods", "goods_category_code", "t_goods_category", "id", "N:1", "商品→分类"),
        // ── 售后域（AX71：装配器 LEFT JOIN + 口径进 ON 前置已完成，这条正式进图）──────
        ("t_after_sales_order_header", "sales_order_code", "t_sales_order", "sales_order_code", "N:1",
         "售后单→原销售订单（DMS Mapper 实证；经此边可下钻到客户/业务员/省份等空间维度。\
          ⚠️ 时间维度不许经此边：售后按 after_sales_time、订单按 order_time，跨表分组是错口径）"),
        ("t_after_sales_order_header", "owner_manager", "t_employee", "employee_id", "N:1",
         "售后单→业务员（Mapper 实证；不经原单的直连边）"),
        ("t_after_sales_order_detail", "after_sales_code", "t_after_sales_order_header", "after_sales_code", "N:1",
         "售后明细→售后头（退货金额/数量都在明细，维度在头表）"),
        // ── 主档与空间（base 实测：COUNT(*) vs COUNT(DISTINCT)，card 非猜）──────────
        ("t_customer", "area_manager_id", "t_employee", "employee_id", "N:1",
         "客户→大区经理；仅用于显式订单人员或客户归属语境，不作为 DWS 销售人员维度"),
        ("t_customer_balance", "customer_code", "t_customer", "customer_code", "N:1",
         "余额快照→客户主档（余额侧多行/客户，快照取最新 rn=1）"),
        ("t_sales_order", "delivery_warehouse_code", "t_warehouse", "wms_code", "N:1",
         "订单→发货仓（wms_code 是仓库码表主键）"),
        // ── 活动费用族（5+1 张子表挂主表；Mapper 实证）────────────────────────
        ("t_activity_freight_fee", "activity_id", "t_activity_main", "id", "N:1", "运费明细→活动主表"),
        ("t_activity_material_fee", "activity_id", "t_activity_main", "id", "N:1", "物料费明细→活动主表"),
        ("t_activity_other_fee", "activity_id", "t_activity_main", "id", "N:1", "其他费明细→活动主表"),
        ("t_activity_promoter_fee", "activity_id", "t_activity_main", "id", "N:1", "促销员费明细→活动主表"),
        ("t_activity_tasting_fee", "activity_id", "t_activity_main", "id", "N:1", "品尝费明细→活动主表"),
        ("t_activity_venue_fee", "activity_id", "t_activity_main", "id", "1:1", "场地费→活动主表（实测一活动至多一条）"),
        // ── 票据与对账 ────────────────────────────────────────────────
        ("t_invoice_apply_detail", "invoice_code", "t_invoice_apply_header", "invoice_code", "N:1",
         "开票明细→开票申请头"),
        ("t_account_bill_detail", "bill_code", "t_account_bill_header", "bill_code", "N:1",
         "对账明细→对账单头"),
        // ── 履约与设备 ────────────────────────────────────────────────
        ("t_sales_order_logistics", "sales_order_code", "t_sales_order", "sales_order_code", "N:1",
         "履约物流行→订单头（用于物流追踪与单号关联；一订单多发货批，N:1；不作为默认销售额事实）"),
        ("t_device_receive_item", "requisition_code", "t_device_requisition", "requisition_code", "N:1",
         "设备收货明细→设备需求单（DMS Mapper 与真实单据实证）"),
        ("t_device_delivery_item", "requisition_code", "t_device_requisition", "requisition_code", "N:1",
         "设备投放明细→设备需求单（DMS Mapper 与真实单据实证）"),
        ("t_device_requisition", "customer_code", "t_customer", "customer_code", "N:1",
         "设备需求单→客户主档（用于客户与区域经理权限/维度）"),
        ("t_customer_device_ledger", "sku_code", "t_goods", "goods_code", "N:1",
         "设备台账→商品主档"),
        ("dws_fin_shipment_check_dnf", "dms_order_code", "t_sales_order", "sales_order_code", "N:1",
         "数仓发货对账行→DMS销售单；只用于单号映射和差异核对，金额聚合仍以各自系统口径为准"),
    ];
    // ✅ 二·AW 的前置**已完成**（AX71，2026-08-03）：装配器路径/桥接一律 LEFT JOIN +
    // 被连表口径进 ON、`scope_parts` 跳过 ON 里已带口径的表 —— 售后边据此进图。
    // 上面的 16 条新边全部两证：DMS Mapper.xml 真实 JOIN（tools/mine_joins.py，384 个
    // XML 出 79 候选）+ 生产库 COUNT 实测基数（tools/probe_card.py，全非扇出 N:1/1:1）。
    // 「逐题对拍数字」的验收在回归（61 题数值断言）+ 「今年各省份的售后单数」实测
    // ≥20073（13 张原单作废的售后单靠 LEFT JOIN 保留）。
    for (lt, lc, rt, rc, card, note) in EDGES {
        sqlx::query(
            "INSERT INTO meta.join_edge(left_table, left_col, right_table, right_col, card, note)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (ds_id, left_table, left_col, right_table, right_col)
             DO UPDATE SET card=$5, note=$6",
        )
        .bind(lt)
        .bind(lc)
        .bind(rt)
        .bind(rc)
        .bind(card)
        .bind(note)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 口径教训种子（幂等）：连库实测坐实的坑，直接 active 参与召回。
/// 与 save_lesson_candidate（复盘产物，需复核）区分——这些是人工/workflow 已验证的。
async fn seed_pitfalls(pg: &PgPool) -> anyhow::Result<()> {
    // (触发表, 教训)
    const LESSONS: &[(&str, &str)] = &[
        ("t_sales_order_detail",
         "仅在显式订单明细查询中，筛赠品/正品使用 item_type（1正品 2赠品 3结算行，SystemConsant.java L38-39 权威）；\
          is_gift 列与之冲突（item_type='1' 但 is_gift=1 有 537 行，item_type='2' 但 is_gift=0 有 2591 行），勿用 is_gift；默认销售额/销量不取本表"),
        ("t_sales_order_detail",
         "显式订单明细商品排行必须先定分组键：真库 sku_code 344 个 / sku_name 427 个 /(code,name) 组合 488（一码多名与一名多码并存），\
          按名与按码结果不同，必须在结论中注明。默认商品销售额/销量排行直接使用销售宽表 skucode/skuname，分别 SUM(amount)/SUM(qty)"),
        ("t_sales_order_detail",
         "订单明细 2x 重复行集中在【非有效订单】的明细上：JOIN 有效订单后重复率<0.01%，整表 item_type='1' 则 100.7万→83.2万(21%)。\
          显式订单明细查询必须 JOIN t_sales_order 并筛有效订单，同时正确去重；默认销售额/销量不走该链路"),
        ("t_market_total_expense",
         "费用按项目分组必须用 expense_item_name（名称），不能用 expense_item（编码）——两列全库 48673 行取值 100% 不同；\
          本表现库 item_type 全为 '0' 无合计行，不必额外筛"),
        ("t_activity_main",
         "活动时间列有歧义：created_time 集中在 2026-05~07，start_date 跨 2026-04~08（含未来日期）。\
          问『某月办了几场活动』须先明确按创建时间还是按活动开始时间，默认用 created_time 并注明"),
        ("t_after_sales_order_header",
         "退款额口径：actual_refund_amount 仅在 after_sales_status IN ('4','5') 时有值（其余为 0）。\
          全量 SUM(refund_amount) 与 SUM(actual_refund_amount) 差 0.002%，但『仅完成单』口径与全量差约 1.2%——\
          注册表现行口径为不按状态过滤，问『实际退了多少』才筛状态"),
        ("t_after_sales_order_header",
         "after_sales_type：1退货 2退款 3中台售后。【默认口径：售后单数/退款额一律不加 after_sales_type 过滤】\
          （中台售后 23843 单也是真实售后单）；只有用户明确说『退货类/退款类/剔除中台』时才加对应条件。\
          切勿自作主张写 after_sales_type != '3'——那会漏算约 72% 的售后单"),
        ("t_customer",
         "province 存 6 位行政区划码（430000=湖南 410000=河南 440000=广东…）不是省名：\
          按省过滤必须用码，展示时再翻名；空串归'未知'"),
        ("t_customer_balance",
         "问『【还】有多少余额 / 【还剩】多少 / 哪些客户还有额度』时**必须加 balance > 0** ——\
          『还有』的语义是余额未用尽，余额为 0 的客户不算。实测（信控 balance_type='1'）：\
          取每客户最新一行共 28 个客户，其中 balance > 0 只有 23 个，差的 5 个正是余额为 0 的。\
          这 5 行差与 ROW_NUMBER 取最新无关（本表每客户每类型本就一行，rn=1 对行数恒等）——\
          漏的是 > 0 这一条。反之问『余额合计/总额度』不加此条件（0 余额不影响求和）"),
    ];
    for (t, lesson) in LESSONS {
        sqlx::query(
            "INSERT INTO meta.pitfall(kind, trigger_words, lesson, status, ds_id)
             SELECT 'pitfall', $1, $2, 'active', $3
             WHERE NOT EXISTS (SELECT 1 FROM meta.pitfall
                               WHERE trigger_words = $1 AND lesson = $2 AND ds_id = $3)",
        )
        .bind(t)
        .bind(lesson)
        .bind(DMS_DS_ID)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// DMS 主源那一行（**存量行为的显式表达**：让「只有一个源」与「多源」走同一套代码路径）。
/// `DO NOTHING` 而非 `DO UPDATE`：管理端改过 name/description 后重启不该被种子冲回去。
pub async fn seed_datasources(pg: &PgPool) -> anyhow::Result<()> {
    // description 是向量选源的唯一素材，必须写清「这是什么业务的库」
    const DESC: &str = "DMS 生产库（MySQL）：销售订单与明细、售后退货退款、客户与经销商、\
        商品与商品分类、市场费用与营销活动、开票与对账、设备与押金、门店、仓库与库存快照、\
        赢销通经营分析、组织与角色权限、积分。所有 DMS 业务域的取数都在这个库。";
    sqlx::query(
        "INSERT INTO meta.datasource(ds_id, name, kind, dialect, dsn_ref, policy_kind, description)
         VALUES ($1, 'DMS 生产库', 'mysql', 'mysql', 'mysql_url', 'dms_datascope', $2)
         ON CONFLICT (ds_id) DO NOTHING",
    )
    .bind(DMS_DS_ID)
    .bind(DESC)
    .execute(pg)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{family_of, TABLE_SCOPES};

    #[test]
    fn specialised_activity_keywords_precede_the_generic_activity_keyword() {
        let src = include_str!("seed.rs");
        let body = src
            .split("const KW_FORCE:")
            .nth(1)
            .expect("KW_FORCE 不见了")
            .split("];")
            .next()
            .expect("KW_FORCE 结束锚点不见了");
        let generic = body.find("(\"活动\", \"t_activity_main\")").expect("泛活动映射不见了");
        for (phrase, table) in [
            ("活动临促人员费用", "t_activity_promoter_fee"),
            ("活动临促人员的费用", "t_activity_promoter_fee"),
            ("临促人员费用", "t_activity_promoter_fee"),
            ("活动执行人员费用", "t_market_activity_promoter_expense"),
            ("活动执行人员的费用", "t_market_activity_promoter_expense"),
            ("执行人员费用", "t_market_activity_promoter_expense"),
        ] {
            let mapping = format!("(\"{phrase}\", \"{table}\")");
            let pos = body.find(&mapping).unwrap_or_else(|| panic!("专项映射缺失：{mapping}"));
            assert!(pos < generic, "专项短语必须排在泛「活动」之前：{phrase}");
        }
    }

    /// 「族」判据必须对**分表/主从表免疫**、对**真·张冠李戴开火**。
    /// 两个方向各测一遍：只测一边的话，把 `take(2)` 写成 `take(1)`（那时所有表都是族 `t`、
    /// 守卫永不开火）或 `take(9)`（那时连 `_3` 分表都算异族、天天误报）都能过。
    #[test]
    fn family_immunises_shards_not_strangers() {
        // 同族：主从表、分表 —— 共用注释是**对的**，守卫不许开火
        assert_eq!(family_of("t_erp_invoice_header"), family_of("t_erp_invoice_detail"));
        assert_eq!(
            family_of("t_device_demand_month_quota"),
            family_of("t_device_demand_month_quota_3")
        );
        // 异族：这两对是实测抓到的真错（库里都写「开票申请单」/「活动场地费用表」）
        assert_ne!(family_of("t_regions"), family_of("t_erp_invoice_header"));
        assert_ne!(family_of("t_xh_bom_detail"), family_of("t_erp_invoice_header"));
        assert_ne!(family_of("t_delivery_warehouse_stock"), family_of("t_activity_venue_fee"));
        // 退化输入不许 panic（表名理论上总有 `t_` 前缀，但 `family_of` 不该假设）
        assert_eq!(family_of("t"), "t");
        assert_eq!(family_of(""), "");
    }

    /// 🔴 软删除口径那条 SQL 的三个承重部件，缺一个都会出事，而三个都是**只在真库上才发作**
    /// 的问题（无库单测测不到）—— 所以扫源码。
    /// ① 缺 `column_doc` 的 EXISTS ⇒ 给没有该列的表登记 ⇒ 那张表的每条 SQL 报 1054
    ///    （实测 42 张候选里就有 1 张没这列）；
    /// ② 缺 `ON CONFLICT DO NOTHING` ⇒ 覆盖手写登记 ⇒ 订单表的「有效订单」口径倒退成软删除；
    /// ③ 缺 `regexp_replace` ⇒ `t_sales_order_detail(JOIN …)` 这种带注解的声明被当成表名。
    #[test]
    fn soft_delete_scope_sql_keeps_its_three_guards() {
        let src = include_str!("seed.rs");
        let body = src
            .split("async fn seed_soft_delete_scopes")
            .nth(1)
            .expect("seed_soft_delete_scopes 不见了 —— 判据锚点失效")
            .split("\n}")
            .next()
            .unwrap();
        for (frag, why) in [
            ("c.column_name = 'deleted_flag'", "没验列存在 → 给没这列的表登记 → SQL 报 1054"),
            ("ON CONFLICT DO NOTHING", "会覆盖手写登记 → 业务口径倒退成软删除"),
            ("regexp_replace", "带注解的来源表名（`t_x(JOIN …)`）会被当成表名"),
            ("meta.table_scope", "插错表了"),
        ] {
            assert!(body.contains(frag), "SQL 缺 `{frag}`：{why}");
        }
        // 顺序：必须排在 `seed_table_scopes` **之后**（手写先插，这一步只补空缺）
        let order = src.split("pub async fn seed(").nth(1).unwrap().split("\n}").next().unwrap();
        let hand = order.find("seed_table_scopes(pg)").expect("seed() 里没调 seed_table_scopes");
        let auto = order.find("seed_soft_delete_scopes(pg)").expect("seed() 里没调 seed_soft_delete_scopes");
        assert!(hand < auto, "seed_soft_delete_scopes 必须排在 seed_table_scopes 之后");
    }

    /// 修正表自身的两条卫生：表名不许重复（后者静默胜出）、新注释不许被两条修正共用
    /// （这批修正治的病就是「一条注释套多张表」，修正本身犯同样的错就荒谬了）。
    #[test]
    fn comment_fixes_are_one_to_one() {
        // 常量在 `seed_table_comments` 的函数体里，测试拿不到 → 扫源码（同 `MAPS` 的处置）
        let src = include_str!("seed.rs");
        let body = src
            .split("const FIX: &[(&str, &str, &str)] = &[")
            .nth(1)
            .expect("FIX 表不见了 —— 判据的锚点失效")
            .split("];")
            .next()
            .unwrap();
        let quoted: Vec<&str> = body.match_indices('"').collect::<Vec<_>>().chunks(2)
            .filter_map(|p| match p {
                [(a, _), (b, _)] => Some(&body[a + 1..*b]),
                _ => None,
            })
            .collect();
        // 三元组：表名 / 用途 / 证据，各取每 3 个里的第 1、2 个
        let tables: Vec<&&str> = quoted.iter().step_by(3).collect();
        let purposes: Vec<&&str> = quoted.iter().skip(1).step_by(3).collect();
        assert!(tables.len() >= 4, "只解析出 {} 条修正，判据的解析坏了", tables.len());
        for t in &tables {
            assert!(t.starts_with("t_"), "解析错位：`{t}` 不像表名");
            assert_eq!(tables.iter().filter(|x| x == &t).count(), 1, "表名 `{t}` 有两条修正");
        }
        for p in &purposes {
            assert!(!p.starts_with("t_"), "解析错位：`{p}` 像表名不像用途");
            assert_eq!(
                purposes.iter().filter(|x| x == &p).count(),
                1,
                "用途「{p}」被两条修正共用 —— 那正是这批修正要治的病"
            );
        }
    }

    /// 🔴 表级声明里不许出现 `item_type`：它只属于显式订单明细指标，默认 DWS 销售指标不走此表。
    /// 提到表级不只是「说明不准」—— `correct_caliber` 会把表级声明确定性补到所有相关 SQL，
    /// 从而把某个订单明细场景的过滤错误扩散到其它指标。
    /// 判的是 `filter` 而不是整条元组：note 里必须写清 item_type 为何不在这里（那是给 LLM 读的）。
    #[test]
    fn table_scope_holds_no_metric_level_caliber() {
        for (t, filter, _) in TABLE_SCOPES {
            assert!(
                !filter.contains("item_type"),
                "{t} 的表级声明含 item_type —— 它只属于显式订单明细指标，\
                 表级声明会把局部过滤错误扩散到其它查询"
            );
        }
        // 收窄不等于删空：软删仍是这张表恒需的（漏了会把已删行算进任何明细类统计）
        let d = TABLE_SCOPES
            .iter()
            .find(|(t, ..)| *t == "t_sales_order_detail")
            .expect("明细表的表级声明必须在册");
        assert_eq!(d.1, "deleted_flag = 0");
    }

    #[test]
    fn warehouse_and_device_lineage_are_seeded() {
        let src = include_str!("seed.rs");
        for edge in [
            "(\"t_device_receive_item\", \"requisition_code\", \"t_device_requisition\"",
            "(\"t_device_delivery_item\", \"requisition_code\", \"t_device_requisition\"",
            "(\"t_device_requisition\", \"customer_code\", \"t_customer\"",
            "(\"dws_fin_shipment_check_dnf\", \"dms_order_code\", \"t_sales_order\"",
        ] {
            assert!(src.contains(edge), "高置信血缘缺失：{edge}");
        }
    }
}
