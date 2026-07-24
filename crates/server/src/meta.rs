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
-- 术语注册表（移植 SuperSonic DomainTerms）：业务黑话→标准口径
CREATE TABLE IF NOT EXISTS meta.term(
  term text PRIMARY KEY,
  definition text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  status text NOT NULL DEFAULT 'active'
);
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
-- 维度注册表（移植 SuperSonic DimensionResp 最小可用）：维度名→分组取数口径单一事实源
CREATE TABLE IF NOT EXISTS meta.dimension(
  dim_code text PRIMARY KEY,
  name text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  source_table text NOT NULL,
  expr text NOT NULL,
  description text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'active'
);
-- 值链接码表（移植 SuperSonic ValueLinking）：编码列 中文名→码，写中文名必返0行的确定性纠正依据
CREATE TABLE IF NOT EXISTS meta.value_map(
  table_name text NOT NULL,
  column_name text NOT NULL,
  name text NOT NULL,
  code text NOT NULL,
  match_kind text NOT NULL DEFAULT 'eq', -- eq=等值换码 / like=组合值列须 LIKE '%码%'
  PRIMARY KEY(table_name, column_name, name)
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
        || name.ends_with("_back")
        || name.ends_with("_backup")
        || name.ends_with("_backups")
        || name.ends_with("_history")
        || name.ends_with("_delete_history")
        // 6 位日期备份段（YYMMDD，如 t_xxx_260515_01）
        || name.split('_').any(|seg| seg.len() == 6 && seg.chars().all(|c| c.is_ascii_digit()))
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
    seed_dimensions(pg).await?;
    seed_value_maps(pg).await?;
    seed_terms(pg).await?;
    Ok(())
}

/// 值链接码表种子（全部来自 meta.pitfall 已连库坐实的码表教训——不猜字典）
async fn seed_value_maps(pg: &PgPool) -> anyhow::Result<()> {
    // (table, column, [(name, code)], match_kind)
    const MAPS: &[(&str, &str, &[(&str, &str)], &str)] = &[
        // InvoiceStatusEnum（pitfall 坐实，真库现存 0/1/2/3/5）
        ("t_invoice_apply_header", "invoice_status",
         &[("未申请", "0"), ("开票申请中", "1"), ("已开票", "2"), ("冲红申请中", "3"),
           ("已冲红", "4"), ("开票失败", "5"), ("部分开票", "6"), ("待确认", "7"),
           ("审核通过", "8"), ("驳回", "9"), ("暂存", "10")], "eq"),
        // InvoiceTypeEnum（t_invoice_apply_header 与 t_customer 同体系）
        ("t_invoice_apply_header", "invoice_type",
         &[("增值税普通发票", "1"), ("普票", "1"), ("增值税专用发票", "2"), ("专票", "2")], "eq"),
        ("t_customer", "invoice_type",
         &[("增值税普通发票", "1"), ("普票", "1"), ("增值税专用发票", "2"), ("专票", "2")], "eq"),
        // 有效订单口径（pitfall 坐实 0暂存/108无效/199作废）
        ("t_sales_order", "order_status",
         &[("暂存", "0"), ("无效", "108"), ("作废", "199")], "eq"),
        // PayWayEnum：真库有逗号组合值——ZX01 纯值可等值，余额类必须 LIKE 含组合（pitfall 坐实）
        ("t_sales_order", "paid_way", &[("在线支付", "ZX01")], "eq"),
        ("t_sales_order", "paid_way",
         &[("信控余额支付", "ZZ01"), ("市场费用支付", "ZF02"), ("不开票余额支付", "ZZ04"),
           ("可开票余额支付", "ZZ05"), ("设备押金支付", "ZZ07")], "like"),
        // 账余类型（pitfall 坐实；15/99 双码在线支付歧义不收录）
        ("t_customer_balance", "balance_type",
         &[("信控", "1"), ("市场费用", "3"), ("可开票余额", "8"), ("不可开票余额", "9"),
           ("设备押金", "10")], "eq"),
        // AccountBillStatusEnum（Java 枚举坐实）+ account_mode
        ("t_account_bill_header", "bill_status",
         &[("待确认", "0"), ("已确认", "1"), ("部分开票", "2"), ("已开票", "3"), ("拒绝", "4")], "eq"),
        ("t_account_bill_header", "account_mode",
         &[("月结", "M"), ("半月", "HM"), ("周结", "WK")], "eq"),
        // 明细行类型（M6w 坐实：1商品行/2赠品/3结算行）
        ("t_sales_order_detail", "item_type",
         &[("商品行", "1"), ("赠品", "2"), ("结算行", "3")], "eq"),
        // 客户分类/类型（字典 key=CustClassif/CUST_TYPE 探针坐实，过滤问句换码）
        ("t_customer", "customer_class",
         &[("货架店铺", "01"), ("新媒体店铺", "02"), ("社团店铺", "03"), ("线下客户", "04"),
           ("内部客户", "05"), ("其他财务专用", "06"), ("外部客户的店铺", "99")], "eq"),
        ("t_customer", "customer_type",
         &[("一般销售客户", "Z001"), ("财务专用客户", "Z002"), ("关联方客户", "Z003"),
           ("货架店铺", "Z004"), ("客户终端仓", "Z005")], "eq"),
    ];
    for (table, col, pairs, kind) in MAPS {
        for (name, code) in *pairs {
            sqlx::query(
                "INSERT INTO meta.value_map(table_name, column_name, name, code, match_kind)
                 VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (table_name, column_name, name) DO UPDATE SET code=$4, match_kind=$5",
            )
            .bind(table)
            .bind(col)
            .bind(name)
            .bind(code)
            .bind(kind)
            .execute(pg)
            .await?;
        }
    }
    Ok(())
}

/// 首批业务术语（DomainTerms）：黑话→标准口径，注入 prompt 帮 LLM 理解
async fn seed_terms(pg: &PgPool) -> anyhow::Result<()> {
    const TERMS: &[(&str, &str, &[&str])] = &[
        ("GMV", "成交总额=销售额(SUM(total_amount) 有效订单口径)", &["成交额", "成交总额"]),
        ("动销", "统计期内有销售记录的商品(在 t_sales_order_detail 出现过的 sku)", &["在售", "有销量"]),
        ("成交客户数", "下过有效订单的去重客户数 COUNT(DISTINCT customer_code)", &["下单客户数", "成交客户"]),
        ("复购", "同一客户在统计期内有效订单数≥2(COUNT DISTINCT sales_order_code GROUP BY customer_code HAVING>=2)", &["复购客户", "二次购买"]),
        ("客单价", "销售额/订单数=SUM(total_amount)/COUNT(DISTINCT sales_order_code)", &["单均", "平均客单"]),
    ];
    for (term, def, aliases) in TERMS {
        sqlx::query(
            "INSERT INTO meta.term(term, definition, aliases) VALUES ($1,$2,$3)
             ON CONFLICT (term) DO UPDATE SET definition=$2, aliases=$3",
        )
        .bind(term)
        .bind(def)
        .bind(aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 召回命中的业务术语（问句含术语名/别名）→ 注入 prompt DomainTerms 段
pub async fn recall_terms(pg: &PgPool, question: &str) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, String, Vec<String>)> = sqlx::query_as(
        "SELECT term, definition, aliases FROM meta.term WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(term, _, aliases)| {
            question.contains(term.as_str()) || aliases.iter().any(|a| question.contains(a.as_str()))
        })
        .map(|(term, def, _)| format!("{term} = {def}"))
        .collect())
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
        ("refund_amount", "退款额", &["售后退款", "退款金额", "售后金额"],
         "t_after_sales_order_header", "SUM(refund_amount)",
         "deleted_flag = 0",
         "售后退款金额"),
        ("stock_qty", "库存量", &["库存数量", "存货量", "库存"],
         "t_winc_stock_report", "SUM(stock_quantity)",
         "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)",
         "库存必须取最新快照(每日全量快照,直接SUM会把多天累加虚增几十倍)"),
        ("stock_amount", "库存金额", &["库存额", "存货金额", "库存价值"],
         "t_winc_stock_report", "SUM(stock_amount)",
         "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)",
         "库存金额必须取最新快照(每日全量快照,直接SUM会累加虚增)"),
        ("sales_qty", "销量", &["销售量", "出货量", "卖了多少箱", "销售数量"],
         "t_sales_order_detail(JOIN t_sales_order 有效订单)", "SUM(box_quantity)",
         "d.item_type = '1'（商品行）",
         "销量=商品行箱数：item_type分列(1商品行/2赠品/3结算行)，销量只取 item_type='1' 的 box_quantity；须 JOIN t_sales_order 且 o.order_status NOT IN('0','108','199')；detail 有2x重复须先按(单号,sku,数量)去重"),
        ("invoice_amount", "开票金额", &["开票额", "发票金额", "发票"],
         "t_invoice_apply_header", "SUM(invoice_amount)",
         "deleted_flag = 0 AND invoice_status = '2'",
         "开票金额必须筛 invoice_status='2'(已开票,码表InvoiceStatusEnum 0未申请/1申请中/2已开票/3冲红申请中/4已冲红/5失败/6部分开票),不筛会把申请中/失败虚增；发票双流并行(老表 t_invoice_apply_header IO*单 + 新表 t_invoice_new_apply_header SQ*单,交集为0),问全量发票须 UNION ALL 两表"),
        ("activity_expense", "活动费用", &["活动经费", "市场活动费用"],
         "t_activity_main", "SUM(total_amount)",
         "deleted_flag = 0",
         "市场活动费用合计金额；status 分 暂存/待申请/已申请/完成(暂存未生效)，只算生效活动加 status IN('已申请','完成')"),
        ("activity_count", "活动场次", &["活动数量", "多少场活动", "办了多少活动"],
         "t_activity_main", "COUNT(DISTINCT activity_no)",
         "deleted_flag = 0",
         "市场活动场次数(按活动编号 activity_no 去重)；status 暂存/待申请/已申请/完成"),
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

/// 首批维度注册（取数口径全部来自 direct.rs 已连库坐实的确定性模板——单一事实源，根治 LLM 分组乱 JOIN/取错列）
async fn seed_dimensions(pg: &PgPool) -> anyhow::Result<()> {
    // (code, name, aliases, source_table, expr, description)
    const DIMENSIONS: &[(&str, &str, &[&str], &str, &str, &str)] = &[
        ("province", "省份", &["各省", "省市", "区域", "地区"],
         "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0",
         "COALESCE(NULLIF(cus.province,''),'未知')",
         "省份在客户主档 t_customer.province（订单表无此列），存行政区划码；空串归'未知'"),
        ("owner", "业务员", &["经理", "负责人", "销售员"],
         "t_sales_order o LEFT JOIN t_employee e ON e.employee_id = o.owner_manager",
         "COALESCE(e.actual_name, o.owner_manager)",
         "业务员=订单 owner_manager（employee_id），JOIN t_employee 翻 actual_name 姓名，查不到回退工号"),
        ("customer", "客户", &["客户名", "经销商"],
         "t_sales_order o",
         "COALESCE(o.customer_name,'未知')",
         "客户直接取订单头 customer_name（快照名，免 JOIN）；需客户主档属性才 JOIN t_customer"),
        ("shop", "门店", &["店铺", "终端"],
         "t_sales_order o",
         "COALESCE(o.shop_name,'未知')",
         "门店取订单头 shop_name（快照名，免 JOIN）"),
        ("goods_category", "商品分类", &["品类", "类别"],
         "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code AND g.deleted_flag = 0 LEFT JOIN t_goods_category cat ON g.goods_category_code = cat.id",
         "COALESCE(cat.category_name,'未分类')",
         "分类在 t_goods_category.category_name，连接键 sku_code=goods_code→goods_category_code=cat.id；无分类归'未分类'"),
        ("month", "月份", &["按月", "每月", "各月", "月度"],
         "t_sales_order o",
         "DATE_FORMAT(o.order_time,'%Y-%m')",
         "月份用 DATE_FORMAT 截到 '%Y-%m'，GROUP BY 与 SELECT 同表达式"),
        ("brand", "品牌", &["牌子", "各品牌"],
         "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code AND g.deleted_flag = 0",
         "COALESCE(NULLIF(g.brand_name,''),'未归属')",
         "品牌在商品主档 t_goods.brand_name（明细行无品牌列），连接键 d.sku_code = g.goods_code；空串归'未归属'"),
        ("customer_class", "客户分类", &["客户类别"],
         "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0",
         "COALESCE(CASE cus.customer_class WHEN '01' THEN '货架店铺' WHEN '02' THEN '新媒体店铺' WHEN '03' THEN '社团店铺' WHEN '04' THEN '线下客户' WHEN '05' THEN '内部客户' WHEN '06' THEN '其他财务专用' WHEN '99' THEN '外部客户的店铺' END,'未分类')",
         "客户分类=t_customer.customer_class 编码列（字典 key=CustClassif 已坐实：真库 04线下客户占 96%），CASE 翻名免字典 JOIN；NULL 归'未分类'"),
        ("customer_type", "客户类型", &["客户种类"],
         "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0",
         "COALESCE(CASE cus.customer_type WHEN 'Z001' THEN '一般销售客户' WHEN 'Z002' THEN '财务专用客户' WHEN 'Z003' THEN '关联方客户' WHEN 'Z004' THEN '货架店铺' WHEN 'Z005' THEN '客户终端仓' END,'未分类')",
         "客户类型=t_customer.customer_type 编码列（字典 key=CUST_TYPE 已坐实：Z001一般销售/Z002财务专用为主），CASE 翻名免字典 JOIN；NULL 归'未分类'"),
    ];
    for (code, name, aliases, src, expr, desc) in DIMENSIONS {
        sqlx::query(
            "INSERT INTO meta.dimension(dim_code, name, aliases, source_table, expr, description)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (dim_code) DO UPDATE SET
               name=$2, aliases=$3, source_table=$4, expr=$5, description=$6",
        )
        .bind(code)
        .bind(name)
        .bind(aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .bind(src)
        .bind(expr)
        .bind(desc)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 维度命中判定（问句含维度名或别名）
fn dim_hit(question: &str, name: &str, aliases: &[String]) -> bool {
    question.contains(name) || aliases.iter().any(|a| question.contains(a.as_str()))
}

/// 召回命中的维度口径卡（问句含维度名或别名）→ 注入 prompt 让 LLM 按此分组取数
pub async fn recall_dimensions(pg: &PgPool, question: &str) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, Vec<String>, String, String, String)> = sqlx::query_as(
        "SELECT name, aliases, source_table, expr, description
         FROM meta.dimension WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(name, aliases, ..)| dim_hit(question, name, aliases))
        .map(|(name, _aliases, src, expr, desc)| {
            format!("【{name}】分组取值 {expr}，来源 {src}。说明：{desc}")
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
    // 向量召回（移植 SuperSonic 双召回的向量半）：语义相似补词典/trgm 不足。embed 挂则降级
    if let Some(vec) = crate::embed::embed_query(question).await {
        let vlit = crate::embed::to_pgvector(&vec);
        let hits: Vec<(String,)> = sqlx::query_as(
            "SELECT table_name FROM meta.table_doc WHERE embedding IS NOT NULL
             ORDER BY embedding <=> $1::vector LIMIT $2",
        )
        .bind(&vlit)
        .bind(k as i64)
        .fetch_all(pg)
        .await
        .unwrap_or_default();
        for (t,) in hits {
            if out.len() >= k {
                break;
            }
            if out.iter().any(|c| c.table_name == t) {
                continue;
            }
            if let Some(text) = render_schema(pg, &t).await? {
                out.push(TableCtx { table_name: t, schema_text: text, score: 0.9, forced: false });
            }
        }
    }

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

    #[test]
    fn dimension_hit_matching() {
        let aliases = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 名命中
        assert!(dim_hit("本月销售额按省份", "省份", &aliases(&["各省"])));
        // 别名命中
        assert!(dim_hit("各区域经理业绩", "业务员", &aliases(&["经理", "负责人"])));
        assert!(dim_hit("销售额按品类", "商品分类", &aliases(&["品类", "类别"])));
        // 未命中
        assert!(!dim_hit("本月销售额", "省份", &aliases(&["各省"])));
        assert!(!dim_hit("库存量", "门店", &aliases(&["店铺", "终端"])));
    }
}
