//! M2 语义层元数据：schema 采集（MySQL information_schema → PG）+ 三路召回。
//! 有效性阶梯（旧项目 90+ 轮验证的方法论）：
//!   1. schema ⚠️ 表头警告（LLM 读 schema 必见，唯一稳定遵守通道）
//!   2. 关键词强制补表（正主表必须在候选里）
//!   3. pitfall 触发召回
//! 检索：关键词强制 + pg_trgm 相似度排序（向量召回 M3 接入，embedding 列已预留）。

use sqlx::{MySqlPool, PgPool};

pub async fn migrate(pg: &PgPool) -> anyhow::Result<()> {
    let ddl = r#"
CREATE SCHEMA IF NOT EXISTS meta;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS vector;
CREATE TABLE IF NOT EXISTS meta.table_doc(
  table_name text PRIMARY KEY,
  table_comment text NOT NULL DEFAULT '',
  domain text NOT NULL DEFAULT '',
  warn text NOT NULL DEFAULT '',
  row_estimate bigint NOT NULL DEFAULT 0,
  search_doc text NOT NULL DEFAULT '',
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_table_doc_trgm ON meta.table_doc USING gin (search_doc gin_trgm_ops);
CREATE TABLE IF NOT EXISTS meta.column_doc(
  table_name text NOT NULL,
  column_name text NOT NULL,
  data_type text NOT NULL DEFAULT '',
  col_comment text NOT NULL DEFAULT '',
  ordinal int NOT NULL DEFAULT 0,
  PRIMARY KEY(table_name, column_name)
);
CREATE TABLE IF NOT EXISTS meta.kw_force(
  keyword text PRIMARY KEY,
  table_name text NOT NULL
);
CREATE TABLE IF NOT EXISTS meta.pitfall(
  id bigserial PRIMARY KEY,
  kind text NOT NULL DEFAULT 'pitfall',
  trigger_words text NOT NULL,
  lesson text NOT NULL,
  status text NOT NULL DEFAULT 'active',
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS meta.sql_exemplar(
  id bigserial PRIMARY KEY,
  question text NOT NULL,
  sql text NOT NULL,
  embedding vector(512),
  created_at timestamptz NOT NULL DEFAULT now()
);
-- 复核态（移植 SuperSonic MemoryReviewTask）：pending 未复核 / enabled 复核通过 / disabled 判错剔除
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS status text NOT NULL DEFAULT 'pending';
-- 指标注册表（移植 SuperSonic 语义层 MetricResp 最小可用）：指标名→口径单一事实源
CREATE TABLE IF NOT EXISTS meta.metric(
  metric_code text PRIMARY KEY,
  name text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  source_table text NOT NULL,
  agg_expr text NOT NULL,
  scope_filter text NOT NULL DEFAULT '',
  description text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'active'
);
"#;
    for stmt in ddl.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

/// 备份/快照表（t_employee_260410、bak_*、*_copy1、*_del_log 之类）不入元数据
fn is_backup_table(name: &str) -> bool {
    let tail: String = name.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    tail.len() >= 4
        || name.starts_with("bak_")
        || name.contains("_copy")
        || name.ends_with("_del_log")
        || name.ends_with("_bak")
        || name.ends_with("_backup")
        || name.ends_with("_backups")
        // bak_sales_order_20251016_01 形态：含 8 位日期段
        || name.split('_').any(|seg| seg.len() == 8 && seg.chars().all(|c| c.is_ascii_digit()))
}

/// 敏感列：绝不进给 LLM 的 schema（旧项目 live.rs 同款，治本）
pub fn is_sensitive_col(name: &str) -> bool {
    let n = name.to_lowercase();
    ["login_pwd", "password", "passwd", "secret", "private_key", "id_card", "id_number", "token", "salt"]
        .iter()
        .any(|k| n.contains(k))
}

/// 表域归类（按名前缀，供检索上下文分组展示）
fn domain_of(table: &str) -> &'static str {
    for (pre, d) in [
        ("t_sales_order", "订单"), ("t_after_sales", "售后"), ("t_customer", "客户"),
        ("t_goods", "商品"), ("t_market", "市场费用"), ("t_activity", "活动"),
        ("t_invoice", "开票"), ("t_account", "对账"), ("t_device", "设备"),
        ("t_shop", "门店"), ("t_warehouse", "仓库"), ("t_winc", "赢销通"),
        ("t_employee", "组织"), ("t_department", "组织"), ("t_role", "权限"), ("t_menu", "权限"),
        ("t_points", "积分"), ("t_marketing", "营销"),
    ] {
        if table.starts_with(pre) {
            return d;
        }
    }
    "其他"
}

/// 从 MySQL information_schema 采集表/列注释，写入 PG 元数据（幂等 upsert）
pub async fn sync_schema(mysql: &MySqlPool, pg: &PgPool) -> anyhow::Result<(usize, usize)> {
    let tables: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT CAST(TABLE_NAME AS CHAR), CAST(TABLE_COMMENT AS CHAR), CAST(TABLE_ROWS AS SIGNED)
         FROM information_schema.TABLES
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'",
    )
    .fetch_all(mysql)
    .await?;
    let cols: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), CAST(DATA_TYPE AS CHAR),
                CAST(COLUMN_COMMENT AS CHAR), CAST(ORDINAL_POSITION AS SIGNED)
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE()",
    )
    .fetch_all(mysql)
    .await?;

    let mut n_tables = 0usize;
    let mut n_cols = 0usize;
    let kept: Vec<String> = tables
        .iter()
        .filter(|(n, _, _)| !is_backup_table(n))
        .map(|(n, _, _)| n.clone())
        .collect();
    // 清理陈旧行（现网删表/规则收紧后不留幽灵）
    sqlx::query("DELETE FROM meta.table_doc WHERE table_name != ALL($1)")
        .bind(&kept)
        .execute(pg)
        .await?;
    sqlx::query("DELETE FROM meta.column_doc WHERE table_name != ALL($1)")
        .bind(&kept)
        .execute(pg)
        .await?;
    for (name, comment, rows) in &tables {
        if is_backup_table(name) {
            continue;
        }
        let tcols: Vec<_> = cols.iter().filter(|c| &c.0 == name).collect();
        let col_doc: String = tcols
            .iter()
            .map(|c| format!("{} {}", c.1, c.3))
            .collect::<Vec<_>>()
            .join(" ");
        let search_doc = format!("{name} {comment} {col_doc}");
        sqlx::query(
            "INSERT INTO meta.table_doc(table_name, table_comment, domain, row_estimate, search_doc, updated_at)
             VALUES ($1, $2, $3, $4, $5, now())
             ON CONFLICT (table_name) DO UPDATE SET table_comment = $2, domain = $3,
               row_estimate = $4, search_doc = $5, updated_at = now()",
        )
        .bind(name)
        .bind(comment)
        .bind(domain_of(name))
        .bind(rows.unwrap_or(0))
        .bind(&search_doc)
        .execute(pg)
        .await?;
        n_tables += 1;
        for c in &tcols {
            sqlx::query(
                "INSERT INTO meta.column_doc(table_name, column_name, data_type, col_comment, ordinal)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (table_name, column_name) DO UPDATE SET data_type = $3, col_comment = $4, ordinal = $5",
            )
            .bind(&c.0)
            .bind(&c.1)
            .bind(&c.2)
            .bind(&c.3)
            .bind(c.4)
            .execute(pg)
            .await?;
            n_cols += 1;
        }
    }
    Ok((n_tables, n_cols))
}

/// 高危表 ⚠️ 警告 + 关键词强制补表：旧项目 90+ 轮连库判官坐实的资产，全量迁移（幂等）
pub async fn seed(pg: &PgPool) -> anyhow::Result<()> {
    const WARNS: &[(&str, &str)] = &[
        ("t_sales_order_detail", "【⚠️含系统级2x重复行：同单号+sku+名称+数量+金额整行×2(ETL双写)。聚合/展示前【必须】按(单号,sku,名称,数量,金额) GROUP BY 去重，否则销量/金额虚增一倍】"),
        ("t_sales_order_his_detai", "【⚠️含系统级2x重复行：同单号+sku+名称+数量+金额整行×2(ETL双写)。聚合/展示前【必须】按(单号,sku,名称,数量,金额) GROUP BY 去重，否则销量/金额虚增一倍】"),
        ("t_marketing_goods", "【⚠️这不是商品主档！分类映射全空(答出全是'其他/未分类')。商品主档用 t_goods，商品分类用 t_goods_category 且按 g.goods_category_code=cat.id 连】"),
        ("t_goods", "【⚠️连商品分类表的正确外键是 goods_category_code(=cat.id)，不是 category_code/category_id——那两个是幻觉列,写了必 1054】"),
        ("t_sales_order_import", "【⚠️这是订单【导入批次】表(手工导入暂存),不是正式订单明细！统计/明细/品类分析用 t_sales_order JOIN t_sales_order_detail，分类再经 t_goods→t_goods_category(cat.id)】"),
        ("t_customer_balance", "【⚠️balance 列是滚动快照，绝不可 SUM(会把每行累计重复加，实测答出 10 倍虚增)。流量必须 SUM(amount) 并按 created_time 筛；余额排行必须 ROW_NUMBER() OVER(PARTITION BY customer_code,balance_type ORDER BY created_time DESC,id DESC) 取各桶最新再 SUM；余额含义按 balance_type 分桶，现金余额='8'/'9'】"),
        ("t_winc_stock_report", "【⚠️每日快照表(N 天×N 仓)：聚合库存必须 product_stock_date=(SELECT MAX(product_stock_date) FROM t_winc_stock_report) 取最新快照，直接 SUM 会把快照天数累加虚增几十倍】"),
        ("t_warehouse_manage", "【⚠️管理档案表，不是实时库存(actual_quantity 全 NULL，数值严重失真)。库存问答用 t_winc_stock_report 最新快照】"),
        ("t_device_requisition", "【⚠️actual_deduct_amount(实际扣款) 全 0(用了必答假 0)。设备金额用 t_device_receive_item.amount(金额) 或 t_device_requisition.purchase_amount(购买金额)；押金流量用 t_customer_balance balance_type IN('10','11','12','13','14')+SUM(amount)+created_time 筛】"),
        ("t_device_receive_item", "【⚠️actual_deduct_amount(实际扣款) 全 0(用了必答假 0)。设备金额用本表 amount(金额) 或 t_device_requisition.purchase_amount(购买金额)】"),
        ("t_sales_order_short", "【⚠️精简子表仅十几行，统计/发货问答勿用。订单用 t_sales_order，物流用 t_sales_order_logistics】"),
        ("t_market_claim_header", "【⚠️application_date(报销申请提交时间) 全表【全 NULL】(按它筛必假0)。申请时间用 applied_time(全覆盖) 或 created_time；accounting_date=财务记账时间(未记账单为空)】"),
        ("t_market_activity_promoter_expense", "【⚠️activity_date 全 NULL，时间过滤只准用 created_time，按 activity_date 筛月份必得 0 行假结论】"),
        ("t_winc_sale_transfer", "【⚠️本表【没有 deleted_flag 列】，加 deleted_flag=0 必 1054。它是 ETL 原始流水，经营分析主数据源用 t_winc_sale_report(有 deleted_flag)】"),
        ("t_winc_stock_transfer", "【⚠️本表【没有 deleted_flag 列】，加 deleted_flag=0 必 1054。它是 ETL 原始流水，经营分析主数据源用 t_winc_sale_report(有 deleted_flag)】"),
        ("t_market_marketing_expense", "【⚠️只是费用族的一个专项子表(约占全部费用5%)。泛指市场费用/营销费用/费用总额一律用 t_market_total_expense(合计摘要表) SUM(expense_amount)；只在明确问『营销专项费用』时才用本表】"),
        ("t_device_demand_apply_detail", "【⚠️设备需求申请中间表(全表仅个位数行)，device_total_amount 不能当设备费用总额。设备金额用 t_device_receive_item.amount 或 t_device_requisition.purchase_amount】"),
        ("t_marketing_zone_product", "【⚠️营销专区商品配置表(仅十几行,start_time 常空)，不是商品上架记录！『新上架/新品』用 t_goods：on_sale=1(上架中)+created_time 近N天(近似新上架时间)】"),
        ("t_new_market_product", "【⚠️运营位配置表(人工挑选的展示清单)，不是商品上架记录！『新上架/新品的销售表现』用 t_goods：on_sale=1+created_time 近N天 为新品集，再 JOIN 订单明细算销售】"),
        ("t_customer_price", "【⚠️每行=一个客户×商品的现行价目档，本库【无价格变更历史】：『调整/变更次数』不可算。goods_name 带『重复-』前缀是人工标记脏档宜排除；price=9999 哨兵必须排除】"),
    ];
    for (t, w) in WARNS {
        sqlx::query("UPDATE meta.table_doc SET warn = $2 WHERE table_name = $1")
            .bind(t)
            .bind(w)
            .execute(pg)
            .await?;
    }

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
        ("活动", "t_activity_main"),
        ("促销员", "t_activity_promoter_fee"),
        // 核心域主表（中文短问句 trgm 召回弱，主表必须保底在候选）
        ("销售", "t_sales_order"), ("订单", "t_sales_order"), ("销量", "t_sales_order_detail"),
        ("买过", "t_sales_order_detail"), ("购买", "t_sales_order_detail"),
        ("客户", "t_customer"), ("商品", "t_goods"),
        ("员工", "t_employee"), ("门店", "t_master_shop"),
    ];
    for (kw, t) in KW_FORCE {
        sqlx::query(
            "INSERT INTO meta.kw_force(keyword, table_name) VALUES ($1, $2)
             ON CONFLICT (keyword) DO UPDATE SET table_name = $2",
        )
        .bind(kw)
        .bind(t)
        .execute(pg)
        .await?;
    }
    seed_metrics(pg).await?;
    Ok(())
}

/// 首批指标注册（口径全部旧项目连库验证过——单一事实源，根治 LLM 用错表/算错口径）
async fn seed_metrics(pg: &PgPool) -> anyhow::Result<()> {
    // (code, name, aliases, source_table, agg_expr, scope_filter, description)
    const METRICS: &[(&str, &str, &[&str], &str, &str, &str, &str)] = &[
        ("sales_amount", "销售额", &["销售总额", "营业额", "销售业绩", "业绩", "卖了多少"],
         "t_sales_order", "SUM(total_amount)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "有效订单销售金额（剔除暂存0/无效108/作废199）"),
        ("order_count", "订单数", &["订单量", "单量", "成交订单数", "多少单"],
         "t_sales_order", "COUNT(DISTINCT sales_order_code)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "有效订单数（按单号去重）"),
        ("avg_order_value", "客单价", &["单均", "平均客单"],
         "t_sales_order", "SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "有效订单客单价=销售额/订单数"),
        ("market_expense", "市场费用", &["营销费用", "费用总额", "推广费"],
         "t_market_total_expense", "SUM(expense_amount)",
         "deleted_flag = 0",
         "泛指市场/营销费用一律用合计摘要表 t_market_total_expense，勿用费用族专项子表（会漏10倍级）"),
        ("aftersales_count", "售后单数", &["退货数", "售后量", "退货单数"],
         "t_after_sales_order_header", "COUNT(DISTINCT after_sales_code)",
         "deleted_flag = 0",
         "售后单数（按售后单号去重）"),
    ];
    for (code, name, aliases, src, agg, scope, desc) in METRICS {
        sqlx::query(
            "INSERT INTO meta.metric(metric_code, name, aliases, source_table, agg_expr, scope_filter, description)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (metric_code) DO UPDATE SET
               name=$2, aliases=$3, source_table=$4, agg_expr=$5, scope_filter=$6, description=$7",
        )
        .bind(code)
        .bind(name)
        .bind(aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .bind(src)
        .bind(agg)
        .bind(scope)
        .bind(desc)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 召回命中的指标口径卡（问句含指标名或别名）→ 注入 prompt 让 LLM 严格按口径
pub async fn recall_metrics(pg: &PgPool, question: &str) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, Vec<String>, String, String, String, String)> = sqlx::query_as(
        "SELECT name, aliases, source_table, agg_expr, scope_filter, description
         FROM meta.metric WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(name, aliases, ..)| {
            question.contains(name.as_str()) || aliases.iter().any(|a| question.contains(a.as_str()))
        })
        .map(|(name, _aliases, src, agg, scope, desc)| {
            let filter = if scope.is_empty() { String::new() } else { format!("；口径过滤：{scope}") };
            format!("【{name}】= {agg}，来源表 {src}{filter}。说明：{desc}")
        })
        .collect())
}

pub struct TableCtx {
    pub table_name: String,
    pub schema_text: String,
    pub score: f32,
    pub forced: bool,
}

/// 三路召回：关键词强制补表（必入）+ trgm 相似排序补足到 k。返回渲染好的 schema 上下文。
pub async fn retrieve(pg: &PgPool, question: &str, k: usize) -> anyhow::Result<Vec<TableCtx>> {
    let mut out: Vec<TableCtx> = vec![];

    let forces: Vec<(String, String)> =
        sqlx::query_as("SELECT keyword, table_name FROM meta.kw_force").fetch_all(pg).await?;
    for (kw, t) in &forces {
        if question.contains(kw.as_str()) && !out.iter().any(|c| &c.table_name == t) {
            if let Some(text) = render_schema(pg, t).await? {
                out.push(TableCtx { table_name: t.clone(), schema_text: text, score: 1.0, forced: true });
            }
        }
    }

    // word_similarity：短问句在长文档中的非对称匹配，中文场景优于 similarity
    let ranked: Vec<(String, f32)> = sqlx::query_as(
        "SELECT table_name, word_similarity($1, search_doc) AS s FROM meta.table_doc
         ORDER BY s DESC LIMIT $2",
    )
    .bind(question)
    .bind((k * 2) as i64)
    .fetch_all(pg)
    .await?;
    for (t, s) in ranked {
        if out.len() >= k + out.iter().filter(|c| c.forced).count() {
            break;
        }
        if out.iter().any(|c| c.table_name == t) {
            continue;
        }
        if let Some(text) = render_schema(pg, &t).await? {
            out.push(TableCtx { table_name: t, schema_text: text, score: s, forced: false });
        }
        if out.len() >= k {
            break;
        }
    }
    Ok(out)
}

/// 命中的口径教训。触发词形态=「表名.列名」或关键词（旧库设计：trigger 锚到会被检索到的表名上）——
/// 表名部分命中召回表集合，或触发词直接出现在问题里，均算命中。
pub async fn recall_pitfalls(
    pg: &PgPool,
    question: &str,
    tables: &[String],
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT trigger_words, lesson FROM meta.pitfall WHERE status = 'active' AND kind IN ('pitfall','routing','column_fix')",
    )
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(trig, _)| {
            trig.split([',', '，', '|']).any(|w| {
                let w = w.trim();
                if w.is_empty() {
                    return false;
                }
                let table_part = w.split('.').next().unwrap_or(w);
                question.contains(w) || tables.iter().any(|t| t == table_part)
            })
        })
        .map(|(_, lesson)| lesson)
        .take(limit)
        .collect())
}

/// bare schema 渲染：⚠️ 警告进表头注释（LLM 读 schema 必见），敏感列剔除
async fn render_schema(pg: &PgPool, table: &str) -> anyhow::Result<Option<String>> {
    let doc: Option<(String, String, String)> = sqlx::query_as(
        "SELECT table_comment, domain, warn FROM meta.table_doc WHERE table_name = $1",
    )
    .bind(table)
    .fetch_optional(pg)
    .await?;
    let Some((comment, domain, warn)) = doc else {
        return Ok(None);
    };
    let cols: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT column_name, data_type, col_comment FROM meta.column_doc
         WHERE table_name = $1 ORDER BY ordinal",
    )
    .bind(table)
    .fetch_all(pg)
    .await?;
    let mut s = format!("-- [{domain}] {table}（{comment}）{warn}\nCREATE TABLE {table} (\n");
    for (name, ty, cmt) in cols.iter().filter(|(n, _, _)| !is_sensitive_col(n)) {
        s.push_str(&format!("  {name} {ty}"));
        if !cmt.trim().is_empty() {
            s.push_str(&format!(" COMMENT '{}'", cmt.replace('\'', "")));
        }
        s.push_str(",\n");
    }
    s.push_str(");\n");
    Ok(Some(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_tables_skipped() {
        assert!(is_backup_table("t_employee_260410"));
        assert!(is_backup_table("t_employee_20260228"));
        assert!(is_backup_table("t_role_employee_0929"));
        assert!(is_backup_table("bak_sales_order_20251016_01"));
        assert!(is_backup_table("t_warehouse_copy1"));
        assert!(is_backup_table("t_warehouse_manage_backups"));
        assert!(!is_backup_table("t_sales_order"));
        assert!(!is_backup_table("t_customer_balance"));
    }

    #[test]
    fn sensitive_cols_filtered() {
        assert!(is_sensitive_col("login_pwd"));
        assert!(is_sensitive_col("api_token"));
        assert!(!is_sensitive_col("customer_code"));
    }
}
