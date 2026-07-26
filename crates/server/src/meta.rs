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
-- 默认时间列（SuperSonic 分区时间维度）：同表多时间列语义不同且有全 NULL 坑列，口径必须钉死
ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS time_col text NOT NULL DEFAULT '';
-- 去重键：来源表含系统级重复行（ETL 双写）时聚合前须按这些列 DISTINCT，否则数值虚增
ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS dedup_keys text NOT NULL DEFAULT '';
-- 表级标准口径（SuperSonic 数据模型 model filter）：无论谁 JOIN 这张表都恒成立的过滤。
-- 解决「明细类指标 JOIN 订单主表却漏掉有效订单过滤 → 数值虚增」（评测抓获销量虚高 41%）。
CREATE TABLE IF NOT EXISTS meta.table_scope(
  table_name text PRIMARY KEY,
  filter text NOT NULL,
  note text NOT NULL DEFAULT ''
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
-- 元素注册表（移植 SuperSonic SchemaElement）：metric/dimension/value/term 统一为可向量召回的元素
CREATE TABLE IF NOT EXISTS meta.element(
  element_id text PRIMARY KEY,       -- kind:标识
  kind text NOT NULL,                -- metric / dimension / value / term
  name text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  ref_expr text NOT NULL DEFAULT '', -- agg_expr / 维度取值表达式 / 码值 / 术语定义
  description text NOT NULL DEFAULT '',
  search_text text NOT NULL DEFAULT '', -- 名+别名+描述（向量化文本）
  status text NOT NULL DEFAULT 'active'
);
ALTER TABLE meta.element ADD COLUMN IF NOT EXISTS embedding vector(512);
-- 纠错反哺日志（自进化引擎B+）：确定性校正器每次出手都记录，同错累计→升格 pitfall 教训
CREATE TABLE IF NOT EXISTS meta.correction_log(
  id bigserial PRIMARY KEY,
  kind text NOT NULL,        -- schema-fix / groupby-fix / agg-fix / value-fix
  question text NOT NULL,
  detail text NOT NULL,      -- 纠正要点（幻觉列名/聚合改写/码值换写等）
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_correction_kind ON meta.correction_log(kind, created_at);
-- 失败复盘日志（自进化引擎C）：执行报错/超时/0行 记录，报错类由 LLM 复盘产出候选教训
CREATE TABLE IF NOT EXISTS meta.failure_log(
  id bigserial PRIMARY KEY,
  kind text NOT NULL,        -- exec-error / zero-rows
  question text NOT NULL,
  sql text NOT NULL DEFAULT '',
  error text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_failure_kind ON meta.failure_log(kind, created_at);
-- JOIN 边注册表（SuperSonic JoinPath 思想）：表间可连接边+基数，组合器跨基表路径推导用
CREATE TABLE IF NOT EXISTS meta.join_edge(
  left_table text NOT NULL,
  left_col text NOT NULL,
  right_table text NOT NULL,
  right_col text NOT NULL,
  card text NOT NULL DEFAULT 'N:1',  -- left→right 基数：1:N(扇出,聚合危险) / N:1(收敛,安全)
  note text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'active',
  PRIMARY KEY(left_table, left_col, right_table, right_col)
);
-- 表权限档案（fail-closed）：scoped=注入条件 / global=Java 无 @DataScope 审定全量可见 / via=独查借头表条件
CREATE TABLE IF NOT EXISTS meta.scope_binding(
  table_name text PRIMARY KEY,
  mode text NOT NULL DEFAULT 'scoped',
  customer_col text,
  owner_col text,
  owner_kind text,          -- ids | codes
  via_table text,
  via_local_col text,
  via_remote_col text,
  note text NOT NULL DEFAULT ''
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
        // ── 以下来自 2026-07-26 六域并行作题 workflow 的连库实测疑点 ──
        ("t_activity_promoter_fee", "【⚠️person_type 已退化(3828 行空串+2036 行 NULL，仅 7 行有码)，按人员类型统计临促费用不可行；is_expense 列注释称『1是0否』但全表【全 NULL】，按它过滤必得 0 行假结论。时间列 start_date 与 created_time 在年粒度差异<0.02%】"),
        ("t_master_shop", "【⚠️门店维度在本库基本不可用：t_sales_order.shop_name 20.6 万单中 20.5 万单为空(仅约 1000 单有值/419 个门店)，按门店分组的经营分析无意义，应向用户说明而非答出稀疏结果】"),
        ("t_invoice_apply_header", "【⚠️发票双流并行且【两表都在持续写入】：老表 IO* 单今年 1925 单 7612 万 > 新表 t_invoice_new_apply_header SQ* 单 826 单 6986 万，交集为 0。问全量开票必须 UNION ALL 两表(只查一张漏 52%)；时间列用 apply_time——invoice_time 全表【全 NULL】】"),
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
    seed_join_edges(pg).await?;
    seed_table_scopes(pg).await?;
    seed_pitfalls(pg).await?;
    seed_terms(pg).await?;
    sync_elements(pg).await?;
    Ok(())
}

/// JOIN 边种子（全部来自已连库坐实的模板连接键；cardinality 标注扇出方向）
/// 表级标准口径种子：该表被任何查询触及时都应成立的过滤（口径单一事实源）
async fn seed_table_scopes(pg: &PgPool) -> anyhow::Result<()> {
    const SCOPES: &[(&str, &str, &str)] = &[
        ("t_sales_order",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "有效订单：剔除暂存0/无效108/作废199。明细类指标 JOIN 订单主表时同样适用（漏则销量/金额虚增）"),
        ("t_customer", "deleted_flag = 0", "客户主档软删过滤"),
        ("t_goods", "deleted_flag = 0", "商品主档软删过滤"),
    ];
    for (t, f, note) in SCOPES {
        sqlx::query(
            "INSERT INTO meta.table_scope(table_name, filter, note) VALUES ($1,$2,$3)
             ON CONFLICT (table_name) DO UPDATE SET filter=$2, note=$3",
        )
        .bind(t).bind(f).bind(note)
        .execute(pg)
        .await?;
    }
    Ok(())
}

async fn seed_join_edges(pg: &PgPool) -> anyhow::Result<()> {
    // (left_table, left_col, right_table, right_col, card, note)
    const EDGES: &[(&str, &str, &str, &str, &str, &str)] = &[
        ("t_sales_order", "sales_order_code", "t_sales_order_detail", "sales_order_code", "1:N",
         "单头→明细扇出（且 detail 有 2x 重复行，须去重——SUM 单头列严禁走此边）"),
        ("t_sales_order", "customer_code", "t_customer", "customer_code", "N:1", "订单→客户主档"),
        ("t_sales_order", "owner_manager", "t_employee", "employee_id", "N:1", "订单→业务员"),
        ("t_sales_order_detail", "sku_code", "t_goods", "goods_code", "N:1", "明细→商品主档"),
        ("t_goods", "goods_category_code", "t_goods_category", "id", "N:1", "商品→分类"),
    ];
    for (lt, lc, rt, rc, card, note) in EDGES {
        sqlx::query(
            "INSERT INTO meta.join_edge(left_table, left_col, right_table, right_col, card, note)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (left_table, left_col, right_table, right_col)
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

/// 元素注册表同步（SuperSonic SchemaElement 统一层）：
/// metric/dimension/value_map/term 四注册表 → 统一元素（向量化召回的原子单位）。
/// 幂等 upsert；元素变更后重跑即可（search_text 变了需重跑 embed build 补向量）。
pub async fn sync_elements(pg: &PgPool) -> anyhow::Result<()> {
    // metric
    let metrics: Vec<(String, String, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT metric_code, name, aliases, agg_expr, description FROM meta.metric WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    for (code, name, aliases, agg, desc) in metrics {
        upsert_element(pg, &format!("metric:{code}"), "metric", &name, &aliases, &agg, &desc).await?;
    }
    // dimension
    let dims: Vec<(String, String, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT dim_code, name, aliases, expr, description FROM meta.dimension WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    for (code, name, aliases, expr, desc) in dims {
        upsert_element(pg, &format!("dimension:{code}"), "dimension", &name, &aliases, &expr, &desc).await?;
    }
    // value（码值也是元素：「已开票」「线下客户」应能向量命中）
    let vals: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT table_name, column_name, name, code FROM meta.value_map",
    )
    .fetch_all(pg)
    .await?;
    for (table, col, name, code) in vals {
        let id = format!("value:{table}.{col}:{code}");
        let desc = format!("{table}.{col} 的码值 {code}");
        upsert_element(pg, &id, "value", &name, &[], &code, &desc).await?;
    }
    // term
    let terms: Vec<(String, String, Vec<String>)> =
        sqlx::query_as("SELECT term, definition, aliases FROM meta.term WHERE status = 'active'")
            .fetch_all(pg)
            .await?;
    for (term, def, aliases) in terms {
        upsert_element(pg, &format!("term:{term}"), "term", &term, &aliases, &def, "").await?;
    }
    Ok(())
}

/// 单元素幂等 upsert（search_text=名+别名+描述 截 500 字；文本变化时清 embedding 待重建）
async fn upsert_element(
    pg: &PgPool,
    id: &str,
    kind: &str,
    name: &str,
    aliases: &[String],
    ref_expr: &str,
    desc: &str,
) -> anyhow::Result<()> {
    let search = {
        let mut s = name.to_string();
        if !aliases.is_empty() {
            s.push_str(&format!("（{}）", aliases.join("、")));
        }
        if !desc.is_empty() {
            s.push_str(&format!("：{desc}"));
        }
        s.chars().take(500).collect::<String>()
    };
    sqlx::query(
        "INSERT INTO meta.element(element_id, kind, name, aliases, ref_expr, description, search_text)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         ON CONFLICT (element_id) DO UPDATE SET
           kind=$2, name=$3, aliases=$4, ref_expr=$5, description=$6, search_text=$7,
           embedding = CASE WHEN meta.element.search_text = $7 THEN meta.element.embedding ELSE NULL END",
    )
    .bind(id)
    .bind(kind)
    .bind(name)
    .bind(aliases.to_vec())
    .bind(ref_expr)
    .bind(desc)
    .bind(&search)
    .execute(pg)
    .await?;
    Ok(())
}

/// 纠错反哺日志（引擎 B+）：校正器出手即记录，供同错累计升格 pitfall（自进化，不静默修）
pub async fn log_correction(pg: &PgPool, kind: &str, question: &str, detail: &str) {
    let _ = sqlx::query("INSERT INTO meta.correction_log(kind, question, detail) VALUES ($1,$2,$3)")
        .bind(kind)
        .bind(question.chars().take(200).collect::<String>())
        .bind(detail.chars().take(500).collect::<String>())
        .execute(pg)
        .await;
}

/// 失败记录（引擎 C）：执行报错/0 行落日志，报错类供 LLM 复盘产出候选教训
pub async fn log_failure(pg: &PgPool, kind: &str, question: &str, sql: &str, error: &str) {
    let _ = sqlx::query("INSERT INTO meta.failure_log(kind, question, sql, error) VALUES ($1,$2,$3,$4)")
        .bind(kind)
        .bind(question.chars().take(200).collect::<String>())
        .bind(sql.chars().take(2000).collect::<String>())
        .bind(error.chars().take(500).collect::<String>())
        .execute(pg)
        .await;
}

/// 口径教训种子（幂等）：连库实测坐实的坑，直接 active 参与召回。
/// 与 save_lesson_candidate（复盘产物，需复核）区分——这些是人工/workflow 已验证的。
async fn seed_pitfalls(pg: &PgPool) -> anyhow::Result<()> {
    // (触发表, 教训)
    const LESSONS: &[(&str, &str)] = &[
        ("t_sales_order_detail",
         "筛赠品/正品一律用 item_type（1正品 2赠品 3结算行，SystemConsant.java L38-39 权威）；\
          is_gift 列与之冲突（item_type='1' 但 is_gift=1 有 537 行，item_type='2' 但 is_gift=0 有 2591 行），勿用 is_gift"),
        ("t_sales_order_detail",
         "商品排行必须先定分组键：真库 sku_code 344 个 / sku_name 427 个 /(code,name) 组合 488（一码多名与一名多码并存），\
          按名分组冠军是蛋挞液、按码分组冠军是原味烤肠——答案完全不同。默认按 sku_name 分组并在结论里注明口径"),
        ("t_sales_order_detail",
         "明细 2x 重复行集中在【非有效订单】的明细上：JOIN 有效订单后重复率<0.01%，整表 item_type='1' 则 100.7万→83.2万(21%)。\
          即『必须 JOIN t_sales_order 并筛有效订单』比 DISTINCT 更关键，两者都要有"),
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
         "return_reason='中台作废' 有 23790 条，与 after_sales_type='3'(中台售后) 高度重合；\
          现行『售后单数/退款额』口径不剔除中台单，若用户语义是真实客户售后需显式说明该差异"),
        ("t_customer",
         "province 存 6 位行政区划码（430000=湖南 410000=河南 440000=广东…）不是省名：\
          按省过滤必须用码，展示时再翻名；空串归'未知'"),
    ];
    for (t, lesson) in LESSONS {
        sqlx::query(
            "INSERT INTO meta.pitfall(kind, trigger_words, lesson, status)
             SELECT 'pitfall', $1, $2, 'active'
             WHERE NOT EXISTS (SELECT 1 FROM meta.pitfall WHERE trigger_words = $1 AND lesson = $2)",
        )
        .bind(t)
        .bind(lesson)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 存候选教训（复盘产物）：status='candidate' 不参与召回，复核启用后生效；同 trigger+lesson 去重
pub async fn save_lesson_candidate(pg: &PgPool, trigger_tables: &str, lesson: &str) -> bool {
    sqlx::query(
        "INSERT INTO meta.pitfall(kind, trigger_words, lesson, status)
         SELECT 'pitfall', $1, $2, 'candidate'
         WHERE NOT EXISTS (SELECT 1 FROM meta.pitfall WHERE trigger_words = $1 AND lesson = $2)",
    )
    .bind(trigger_tables)
    .bind(lesson)
    .execute(pg)
    .await
    .map(|r| r.rows_affected() > 0)
    .unwrap_or(false)
}

/// 从 SQL 提取物理表名（复盘教训的锚定触发词）
pub fn extract_tables(sql: &str) -> String {
    let mut tabs: Vec<String> = vec![];
    let mut cur = String::new();
    let push = |cur: &str, tabs: &mut Vec<String>| {
        if cur.starts_with("t_") && cur.len() > 2 && cur.len() < 60 && !tabs.contains(&cur.to_string()) {
            tabs.push(cur.to_string());
        }
    };
    for c in sql.chars() {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else {
            push(&cur, &mut tabs);
            cur.clear();
        }
    }
    push(&cur, &mut tabs);
    tabs.join(",")
}

/// 元素级向量召回（移植 SuperSonic SchemaMapper）：问句 embed → ANN 近邻元素。
/// 返回 (元素名, 渲染卡) 供 pipeline 与 substring 命中去重合并——口语化问法的语义双保险。
/// embed 服务缺席自动降级为空（熔断在 embed 客户端内）。
pub async fn recall_elements(pg: &PgPool, question: &str, limit: usize) -> Vec<(String, String)> {
    let Some(vec) = crate::embed::embed_query(question).await else {
        return vec![];
    };
    let lit = crate::embed::to_pgvector(&vec);
    let rows: Vec<(String, String, String, String, f64)> = sqlx::query_as(
        "SELECT element_id, kind, name, ref_expr, (embedding <=> $1::vector) AS dist
         FROM meta.element WHERE status = 'active' AND embedding IS NOT NULL
         ORDER BY embedding <=> $1::vector LIMIT $2",
    )
    .bind(&lit)
    .bind(limit as i64)
    .fetch_all(pg)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .filter(|(_, _, _, _, dist)| *dist < 0.35) // 余弦距离阈值：语义相关才入
        .map(|(id, kind, name, ref_expr, _)| {
            let card = match kind.as_str() {
                "metric" => format!("【指标·{name}】= {ref_expr}"),
                "dimension" => format!("【维度·{name}】分组取值 {ref_expr}"),
                "value" => format!("【码值·{name}】编码列码值（{id}）"),
                _ => format!("【术语·{name}】{ref_expr}"),
            };
            (name, card)
        })
        .collect()
}

/// 值链接码表种子（全部来自 meta.pitfall 已连库坐实的码表教训——不猜字典）
async fn seed_value_maps(pg: &PgPool) -> anyhow::Result<()> {
    // (table, column, [(name, code)], match_kind)
    const MAPS: &[(&str, &str, &[(&str, &str)], &str)] = &[
        // 省份=行政区划码（实测 t_customer.province 存 '430000' 这类 6 位码，不是省名）。
        // 缺这组映射时问「湖南省销售额」LLM 无从下手——实测直接漏掉省份过滤答成全量。
        ("t_customer", "province",
         &[("北京", "110000"), ("天津", "120000"), ("河北", "130000"), ("山西", "140000"),
           ("内蒙古", "150000"), ("辽宁", "210000"), ("吉林", "220000"), ("黑龙江", "230000"),
           ("上海", "310000"), ("江苏", "320000"), ("浙江", "330000"), ("安徽", "340000"),
           ("福建", "350000"), ("江西", "360000"), ("山东", "370000"), ("河南", "410000"),
           ("湖北", "420000"), ("湖南", "430000"), ("广东", "440000"), ("广西", "450000"),
           ("海南", "460000"), ("重庆", "500000"), ("四川", "510000"), ("贵州", "520000"),
           ("云南", "530000"), ("西藏", "540000"), ("陕西", "610000"), ("甘肃", "620000"),
           ("青海", "630000"), ("宁夏", "640000"), ("新疆", "650000"), ("台湾", "710000"),
           ("香港", "810000"), ("澳门", "820000")], "eq"),
        // 客户分类（字典 CustClassif，与 meta.dimension customer_class 的 CASE 同源）：
        // 「线下客户」这类问法必须换成 '04'，否则 LLM 会去猜别的列（实测猜到了 customer_channel）
        ("t_customer", "customer_class",
         &[("货架店铺", "01"), ("新媒体店铺", "02"), ("社团店铺", "03"), ("线下客户", "04"),
           ("内部客户", "05"), ("其他财务专用", "06"), ("外部客户的店铺", "99")], "eq"),
        // 客户类型（字典 CUST_TYPE）
        ("t_customer", "customer_type",
         &[("一般销售客户", "Z001"), ("财务专用客户", "Z002"), ("关联方客户", "Z003"),
           ("货架店铺", "Z004"), ("客户终端仓", "Z005")], "eq"),
        // 售后类型（SystemConsant.java L148-149 / AfterSalesServiceImpl L1218-1222；Java 无枚举类）
        ("t_after_sales_order_header", "after_sales_type",
         &[("退货", "1"), ("退款", "2"), ("中台售后", "3")], "eq"),
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
    let matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, (term, _, aliases))| match_word(question, term, aliases).map(|w| (i, w)))
        .collect();
    let pairs: Vec<(String, String)> =
        matched.iter().map(|(i, w)| (rows[*i].0.clone(), w.clone())).collect();
    Ok(map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let (term, def, _) = &rows[matched[k].0];
            format!("{term} = {def}")
        })
        .collect())
}

/// 首批指标注册（口径全部旧项目连库验证过——单一事实源，根治 LLM 用错表/算错口径）
async fn seed_metrics(pg: &PgPool) -> anyhow::Result<()> {
    // (code, name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description)
    // time_col = 默认时间列（DMS Java mapper 权威口径 + 连库核实非空）；空=该指标无时间语义（快照类）
    // dedup_keys = 来源表含系统级重复行时的去重键；空=该表无重复问题
    const METRICS: &[(&str, &str, &[&str], &str, &str, &str, &str, &str, &str)] = &[
        ("sales_amount", "销售额", &["销售总额", "营业额", "销售业绩", "业绩", "卖了多少"],
         "t_sales_order", "SUM(total_amount)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "order_time",
         "",
         "有效订单销售金额（剔除暂存0/无效108/作废199）"),
        // 别名须覆盖口语问法：「有多少个订单」不含"订单数"三字，漏召回则口径卡与口径补全全失效（评测抓获）
        ("order_count", "订单数", &["订单量", "单量", "成交订单数", "多少单", "多少个订单", "多少订单", "几个订单", "几单", "订单笔数", "下了多少"],
         "t_sales_order", "COUNT(DISTINCT sales_order_code)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "order_time",
         "",
         "有效订单数（按单号去重）"),
        ("avg_order_value", "客单价", &["单均", "平均客单"],
         "t_sales_order", "SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "order_time",
         "",
         "有效订单客单价=销售额/订单数"),
        ("market_expense", "市场费用", &["营销费用", "费用总额", "推广费"],
         "t_market_total_expense", "SUM(expense_amount)",
         "deleted_flag = 0",
         "created_time",
         "",
         "泛指市场/营销费用一律用合计摘要表 t_market_total_expense，勿用费用族专项子表（会漏10倍级）"),
        ("aftersales_count", "售后单数", &["退货数", "售后量", "退货单数", "售后单有多少", "多少售后", "几个售后单"],
         "t_after_sales_order_header", "COUNT(DISTINCT after_sales_code)",
         "deleted_flag = 0",
         "after_sales_time",
         "",
         "售后单数（按售后单号去重）"),
        ("refund_amount", "退款额", &["售后退款", "退款金额", "售后金额"],
         "t_after_sales_order_header", "SUM(refund_amount)",
         "deleted_flag = 0",
         "after_sales_time",
         "",
         "售后退款金额"),
        ("stock_qty", "库存量", &["库存数量", "存货量", "库存"],
         "t_winc_stock_report", "SUM(stock_quantity)",
         "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)",
         "",
         "",
         "库存必须取最新快照(每日全量快照,直接SUM会把多天累加虚增几十倍)"),
        ("stock_amount", "库存金额", &["库存额", "存货金额", "库存价值"],
         "t_winc_stock_report", "SUM(stock_amount)",
         "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)",
         "",
         "",
         "库存金额必须取最新快照(每日全量快照,直接SUM会累加虚增)"),
        ("sales_qty", "销量", &["销售量", "出货量", "卖了多少箱", "销售数量"],
         "t_sales_order_detail(JOIN t_sales_order 有效订单)", "SUM(box_quantity)",
         "item_type = '1'",
         "order_time",
         "sales_order_code,sku_code,sku_name,box_quantity,amount",
         "销量=商品行箱数：item_type分列(1商品行/2赠品/3结算行)，销量只取 item_type='1' 的 box_quantity；须 JOIN t_sales_order 且 o.order_status NOT IN('0','108','199')；detail 有2x重复须先按(单号,sku,数量)去重"),
        ("invoice_amount", "开票金额", &["开票额", "发票金额", "发票"],
         "t_invoice_apply_header UNION ALL t_invoice_new_apply_header", "SUM(invoice_amount)",
         "deleted_flag = 0 AND invoice_status = '2'",
         "apply_time",
         "",
         "开票金额必须筛 invoice_status='2'(已开票,码表InvoiceStatusEnum 0未申请/1申请中/2已开票/3冲红申请中/4已冲红/5失败/6部分开票),不筛会把申请中/失败虚增；【发票双流并行,必须 UNION ALL 两表】老表 t_invoice_apply_header(IO*单,存量少)+新表 t_invoice_new_apply_header(SQ*单,当前主流,实测本月 275 单 2819 万 vs 老表 16 单 73 万),交集为0,只查一张表必严重漏算"),
        ("activity_expense", "活动费用", &["活动经费", "市场活动费用"],
         "t_activity_main", "SUM(total_amount)",
         "deleted_flag = 0",
         "created_time",
         "",
         "市场活动费用合计金额；status 分 暂存/待申请/已申请/完成(暂存未生效)，只算生效活动加 status IN('已申请','完成')"),
        ("activity_count", "活动场次", &["活动数量", "多少场活动", "办了多少活动"],
         "t_activity_main", "COUNT(DISTINCT activity_no)",
         "deleted_flag = 0",
         "created_time",
         "",
         "市场活动场次数(按活动编号 activity_no 去重)；status 暂存/待申请/已申请/完成"),
    ];
    for (code, name, aliases, src, agg, scope, tcol, dedup, desc) in METRICS {
        sqlx::query(
            "INSERT INTO meta.metric(metric_code, name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (metric_code) DO UPDATE SET
               name=$2, aliases=$3, source_table=$4, agg_expr=$5, scope_filter=$6, time_col=$7, dedup_keys=$8, description=$9",
        )
        .bind(code)
        .bind(name)
        .bind(aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .bind(src)
        .bind(agg)
        .bind(scope)
        .bind(tcol)
        .bind(dedup)
        .bind(desc)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 问句对某元素的命中：返回命中词（名或最长命中别名），未命中返回 None。
/// 取最长——同一元素多个别名同时命中时，长词更具体（"多少个订单" 优于 "多少单"）。
pub fn match_word(question: &str, name: &str, aliases: &[String]) -> Option<String> {
    let mut best: Option<String> = None;
    let mut consider = |w: &str| {
        if !w.is_empty() && question.contains(w) {
            let better = best.as_ref().map(|b| w.chars().count() > b.chars().count()).unwrap_or(true);
            if better {
                best = Some(w.to_string());
            }
        }
    };
    consider(name);
    for a in aliases {
        consider(a);
    }
    best
}

/// MapFilter（移植 SuperSonic SchemaMapper 命中净化五规则的中文适配版）：
/// 召回命中往往互相干扰——问「库存金额」会同时命中指标「库存量」(别名"库存")；
/// autodiscover 把列注释当维度名导致同名重复 10 条。不净化则口径卡互相打架且 prompt 膨胀。
///
/// 输入 (元素名, 命中词)，输出保留下标（保持原序）：
/// - R1 命中词 <2 字 剔除（中文单字无区分度）
/// - R2 同名去重（保留首个）
/// - R3 命中词被另一命中词真包含 → 剔除较短者（"客户" vs "客户分类" 取后者）
/// - R4 同一命中词多元素命中时，元素名==命中词（满分）优先，其余剔除
pub fn map_filter(hits: &[(String, String)]) -> Vec<usize> {
    let words: Vec<&str> = hits.iter().map(|(_, w)| w.as_str()).collect();
    // R4 预备：哪些命中词存在满分元素
    let exact_words: std::collections::HashSet<&str> = hits
        .iter()
        .filter(|(n, w)| n == w)
        .map(|(_, w)| w.as_str())
        .collect();
    let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = vec![];
    for (i, (name, word)) in hits.iter().enumerate() {
        if word.chars().count() < 2 {
            continue; // R1
        }
        if !seen_names.insert(name.as_str()) {
            continue; // R2
        }
        // R3：存在更长且真包含本命中词的命中 → 本条让位
        if words.iter().any(|w| w.len() > word.len() && w.contains(word.as_str())) {
            continue;
        }
        // R4：同词有满分命中而本条非满分 → 让位
        if name != word && exact_words.contains(word.as_str()) {
            continue;
        }
        out.push(i);
    }
    out
}

/// 命中的指标（结构化，供口径卡渲染与口径校正器共用——单一事实源）
pub struct MetricHit {
    pub name: String,
    pub source_table: String,
    pub agg_expr: String,
    pub scope_filter: String,
    pub time_col: String,
    pub dedup_keys: String,
    pub description: String,
}

/// 召回命中的指标（问句含指标名或别名）
pub async fn recall_metric_hits(pg: &PgPool, question: &str) -> anyhow::Result<Vec<MetricHit>> {
    let rows: Vec<(String, Vec<String>, String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description
         FROM meta.metric WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    // 命中 + MapFilter 净化（"库存金额" 不该同时拖出 "库存量"）
    let matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, (name, aliases, ..))| match_word(question, name, aliases).map(|w| (i, w)))
        .collect();
    let pairs: Vec<(String, String)> =
        matched.iter().map(|(i, w)| (rows[*i].0.clone(), w.clone())).collect();
    Ok(map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let (name, _a, source_table, agg_expr, scope_filter, time_col, dedup_keys, description) =
                rows[matched[k].0].clone();
            MetricHit { name, source_table, agg_expr, scope_filter, time_col, dedup_keys, description }
        })
        .collect())
}

/// 指标口径卡文本（注入 prompt 让 LLM 严格按口径）
pub fn metric_card(m: &MetricHit) -> String {
    let filter = if m.scope_filter.is_empty() { String::new() } else { format!("；口径过滤：{}", m.scope_filter) };
    // 时间列是最高频错误源（同表多个时间列语义不同，且有全 NULL 的坑列）——口径卡必须钉死
    let tcol = if m.time_col.is_empty() {
        String::new()
    } else {
        format!("；时间过滤【必须】用 {} 列", m.time_col)
    };
    let dedup = if m.dedup_keys.is_empty() {
        String::new()
    } else {
        format!("；⚠️该表含系统级重复行，聚合前【必须】先按 ({}) DISTINCT 去重再算，否则数值虚增", m.dedup_keys)
    };
    format!("【{}】= {}，来源表 {}{}{}{}。说明：{}", m.name, m.agg_expr, m.source_table, filter, tcol, dedup, m.description)
}

pub async fn recall_metrics(pg: &PgPool, question: &str) -> anyhow::Result<Vec<String>> {
    Ok(recall_metric_hits(pg, question).await?.iter().map(metric_card).collect())
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

/// 码值提示：问句里出现的中文值若是某编码列的码名 → 直接告诉 LLM 该列存码及对应码值。
/// ValueLinker（correct_value）只能在 LLM **已写出** `col='中文名'` 时换码；
/// 问「湖南省销售额」LLM 压根不知道 province 存的是 '430000'，实测直接漏掉省份过滤答成全量。
/// 这一层把「值→列→码」在生成前就摆给 LLM，是确定性的（不依赖向量召回）。
pub async fn recall_value_hints(pg: &PgPool, question: &str) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT table_name, column_name, name, code, match_kind FROM meta.value_map",
    )
    .fetch_all(pg)
    .await?;
    let matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter(|(_, (_, _, name, ..))| name.chars().count() >= 2 && question.contains(name.as_str()))
        .map(|(i, (_, _, name, ..))| (i, name.clone()))
        .collect();
    // 同名多列（如"货架店铺"既是 customer_class 又是 customer_type）全部保留——
    // 由 LLM 结合问句选列；MapFilter 仅做包含关系净化（"线下客户" 压过 "客户"）
    let pairs: Vec<(String, String)> = matched
        .iter()
        .map(|(i, w)| (format!("{}.{}:{}", rows[*i].0, rows[*i].1, rows[*i].2), w.clone()))
        .collect();
    Ok(map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let (t, c, name, code, kind) = &rows[matched[k].0];
            if kind == "like" {
                format!("「{name}」在 {t}.{c} 列的码是 '{code}'，该列是逗号组合值，必须用 {c} LIKE '%{code}%'")
            } else {
                format!("「{name}」在 {t}.{c} 列存的是编码 '{code}'，过滤必须写 {c} = '{code}'（写中文名必返 0 行）")
            }
        })
        .collect())
}

/// 列注释 → 干净维度名：截到首个分隔符（中英文冒号/括号/逗号/斜杠/空格）之前。
/// 结果须是 2~8 字的纯中文词；否则 None（调用方退回字典名）。
pub fn clean_dim_name(comment: &str) -> Option<String> {
    let head: String = comment
        .trim()
        .chars()
        .take_while(|c| !":：(（)）,，、/ \t".contains(*c))
        .collect();
    let n = head.chars().count();
    if (2..=8).contains(&n) && head.chars().all(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)) {
        Some(head)
    } else {
        None
    }
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
    // 命中 + MapFilter 净化。维度表被 autodiscover 灌入过列注释原文（同名重复 10 条、
    // 名字带码值说明），不净化会重复注入同一张卡并淹没真正的维度口径。
    let matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, (name, aliases, ..))| match_word(question, name, aliases).map(|w| (i, w)))
        .collect();
    let pairs: Vec<(String, String)> =
        matched.iter().map(|(i, w)| (rows[*i].0.clone(), w.clone())).collect();
    Ok(map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let (name, _a, src, expr, desc) = rows[matched[k].0].clone();
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

/// A1 自动发现引擎：字典码列自动对码（数据驱动注册——字典变了重跑即自适应，不再需要手工播种）。
/// 候选=码型后缀列(*_code/_type/_status/_class/_mode/_way/_level)+小表(row_estimate<100万)；
/// 只读 DISTINCT 抽样(≤61 值)；值集 ⊆ 某 dict key 码集(覆盖≥80% 且 ≥2 值)→
/// 自动注册 value_map(eq 换码,字典全码)+dimension(CASE 翻名)。人工种子优先：已覆盖 (表,列) 跳过。
pub async fn autodiscover_dict_columns(
    mysql: &MySqlPool,
    pg: &PgPool,
) -> anyhow::Result<serde_json::Value> {
    use std::collections::{HashMap, HashSet};

    // 1. 生产字典（t_dict_key/value，全量小表）
    let dict_rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(k.key_code AS CHAR), CAST(k.key_name AS CHAR),
                CAST(v.value_code AS CHAR), CAST(v.value_name AS CHAR)
         FROM t_dict_key k
         JOIN t_dict_value v ON v.dict_key_id = k.dict_key_id AND v.deleted_flag = 0
         WHERE k.deleted_flag = 0",
    )
    .fetch_all(mysql)
    .await?;
    let mut dicts: HashMap<String, (String, Vec<(String, String)>)> = HashMap::new();
    for (kc, kn, vc, vn) in dict_rows {
        dicts.entry(kc).or_insert_with(|| (kn, vec![])).1.push((vc, vn));
    }

    // 2. 候选列（码型后缀 + 小表）
    let cands: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT c.table_name, c.column_name, c.col_comment
         FROM meta.column_doc c
         JOIN meta.table_doc t ON t.table_name = c.table_name
         WHERE t.row_estimate < 1000000
           AND c.column_name ~ '(_code|_type|_status|_class|_mode|_way|_level)$'
         ORDER BY c.table_name, c.ordinal",
    )
    .fetch_all(pg)
    .await?;

    // 3. 人工已覆盖的 (表,列)（value_map + dimension expr 提及）→ 跳过
    let manual_vm: HashSet<(String, String)> =
        sqlx::query_as::<_, (String, String)>("SELECT DISTINCT table_name, column_name FROM meta.value_map")
            .fetch_all(pg)
            .await?
            .into_iter()
            .map(|(t, c)| (t.to_lowercase(), c.to_lowercase()))
            .collect();
    let manual_dims: Vec<(String, String)> =
        sqlx::query_as("SELECT source_table, expr FROM meta.dimension WHERE status = 'active'")
            .fetch_all(pg)
            .await?;

    // 4. 有 deleted_flag 的表集合（拼 WHERE 用；部分表无此列）
    let del_tables: HashSet<String> =
        sqlx::query_as::<_, (String,)>("SELECT DISTINCT table_name FROM meta.column_doc WHERE column_name = 'deleted_flag'")
            .fetch_all(pg)
            .await?
            .into_iter()
            .map(|(t,)| t)
            .collect();

    let mut probed = 0usize;
    let mut skipped_manual = 0usize;
    let mut registered: Vec<serde_json::Value> = vec![];

    for (table, col, comment) in &cands {
        if is_backup_table(table) || is_sensitive_col(col) {
            continue;
        }
        let key = (table.to_lowercase(), col.to_lowercase());
        if manual_vm.contains(&key)
            || manual_dims.iter().any(|(src, expr)| src.contains(table.as_str()) && expr.contains(col.as_str()))
        {
            skipped_manual += 1;
            continue;
        }
        // 只读抽样（生产库连接池会话级 READ ONLY 兜底）。单探针 10s 超时：
        // row_estimate 可能严重失真（29 行的表真实扫描分钟级），悬挂探针跳过不拖全局
        let where_del = if del_tables.contains(table) { "WHERE deleted_flag = 0" } else { "" };
        let probe_sql =
            format!("SELECT DISTINCT CAST(`{col}` AS CHAR) FROM `{table}` {where_del} LIMIT 61");
        let probe_fut = sqlx::query_as::<_, (Option<String>,)>(&probe_sql).fetch_all(mysql);
        let rows: Vec<(Option<String>,)> =
            match tokio::time::timeout(std::time::Duration::from_secs(10), probe_fut).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    tracing::warn!("autodiscover 抽样失败 {table}.{col}: {e}");
                    continue;
                }
                Err(_) => {
                    tracing::warn!("autodiscover 抽样超时(10s)跳过 {table}.{col}");
                    continue;
                }
            };
        probed += 1;
        let values: Vec<String> = rows
            .into_iter()
            .filter_map(|(v,)| v)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
        let Some((dict_key, dict_name, pairs, coverage)) = best_dict_match(&values, &dicts, comment) else {
            continue;
        };

        // 注册 value_map（eq）——字典全码注册，未来新值也自适应
        for (code, name) in &pairs {
            sqlx::query(
                "INSERT INTO meta.value_map(table_name, column_name, name, code, match_kind)
                 VALUES ($1,$2,$3,$4,'eq')
                 ON CONFLICT (table_name, column_name, name) DO UPDATE SET code=$4, match_kind='eq'",
            )
            .bind(table)
            .bind(col)
            .bind(name)
            .bind(code)
            .execute(pg)
            .await?;
        }
        // 注册 dimension（CASE 翻名；码数 >60 仅注册值映射，CASE 过长伤 prompt）
        if pairs.len() <= 60 {
            let cases: String = pairs
                .iter()
                .map(|(c, n)| format!("WHEN '{}' THEN '{}'", c.replace('\'', ""), n.replace('\'', "")))
                .collect::<Vec<_>>()
                .join(" ");
            let expr = format!("CASE `{col}` {cases} END");
            let dim_code: String = format!("auto_{table}_{col}").chars().take(80).collect();
            // 维度名取列注释的**首段**：注释常是「配送状态：100:待配送, 200:配送中」这种带码值说明的长句，
            // 整句当维度名既不可能被问句命中，又污染注册表（同名重复十几条）。清洗不出就退回字典名。
            let dim_name = match clean_dim_name(comment) {
                Some(n) => n,
                None => dict_name.clone(),
            };
            let desc =
                format!("自动发现：编码列对码字典 {dict_name}({dict_key})，抽样覆盖率 {coverage:.0}%");
            sqlx::query(
                "INSERT INTO meta.dimension(dim_code, name, aliases, source_table, expr, description)
                 VALUES ($1,$2,'{}',$3,$4,$5)
                 ON CONFLICT (dim_code) DO UPDATE SET name=$2, source_table=$3, expr=$4, description=$5",
            )
            .bind(&dim_code)
            .bind(&dim_name)
            .bind(table)
            .bind(&expr)
            .bind(&desc)
            .execute(pg)
            .await?;
        }
        registered.push(serde_json::json!({
            "table": table, "column": col, "dict": dict_key, "dict_name": dict_name,
            "distinct_values": values.len(), "coverage": coverage,
        }));
    }

    // 新注册的维度/码值同步进元素注册表（向量化召回原子单位）
    sync_elements(pg).await?;

    Ok(serde_json::json!({
        "dict_keys": dicts.len(),
        "candidates": cands.len(),
        "probed": probed,
        "skipped_manual": skipped_manual,
        "registered_count": registered.len(),
        "registered": registered,
    }))
}

/// 值集对码：找覆盖率最高的 dict key。防误配硬闸（两轮实跑教训）：
///   教训① 数值小码集互相撞车（menu_type 撞对账单状态、wms_type 撞 28 项发票类型）；
///   教训② 含字母码的字典一样是撞车磁铁（data_scope_type={1,2} 撞联系人类型、审批状态撞设备处置状态）——
///          小值集证据本质不足，除名称对齐外无捷径。
/// 规则：A. 注释点名优先：列注释里出现某 dict 的 key_code/key_name（如「数据字典 MARKETING_GOODS_CATEGORY」）→ 只评该字典；
///        B. 直通：覆盖率 100% 且 ≥8 个不同值；
///        C. 名称对齐：列注释与字典名有 ≥3 字连续公共子串。
/// 值集需 2~60 个不同值，覆盖 ≥80%。纯函数可单测。
fn best_dict_match(
    values: &[String],
    dicts: &std::collections::HashMap<String, (String, Vec<(String, String)>)>,
    col_comment: &str,
) -> Option<(String, String, Vec<(String, String)>, f64)> {
    use std::collections::HashSet;
    let uniq: HashSet<&String> = values.iter().collect();
    if uniq.len() < 2 || uniq.len() > 60 {
        return None;
    }
    // A. 注释点名的字典优先（只评点名的；点名了但不匹配也宁缺毋滥）
    let comment_low = col_comment.to_lowercase();
    let named: Vec<&String> = dicts
        .keys()
        .filter(|kc| {
            (!kc.is_empty() && kc.len() >= 4 && comment_low.contains(&kc.to_lowercase()))
                || dicts
                    .get(*kc)
                    .map(|(kn, _)| !kn.is_empty() && kn.len() >= 3 && col_comment.contains(kn.as_str()))
                    .unwrap_or(false)
        })
        .collect();
    let candidates: Vec<&String> = if !named.is_empty() {
        named
    } else {
        dicts.keys().collect()
    };
    let mut best: Option<(String, String, Vec<(String, String)>, f64, usize)> = None;
    for kc in candidates {
        let (kn, pairs) = &dicts[kc];
        let codes: HashSet<&String> = pairs.iter().map(|(c, _)| c).collect();
        let hit = uniq.iter().filter(|v| codes.contains(**v)).count();
        let cov = hit as f64 / uniq.len() as f64;
        if hit < 2 || cov < 0.8 {
            continue;
        }
        let pass = (cov >= 1.0 && uniq.len() >= 8) || name_aligns(col_comment, kn);
        if !pass {
            continue;
        }
        let better = match &best {
            Some((_, _, _, bcov, bhit)) => (cov, hit) > (*bcov, *bhit),
            None => true,
        };
        if better {
            best = Some((kc.clone(), kn.clone(), pairs.clone(), cov, hit));
        }
    }
    best.map(|(kc, kn, pairs, cov, _)| (kc, kn, pairs, cov))
}

/// 名称对齐：列注释与字典名存在 ≥3 字连续公共子串（CJK 3-gram 双向包含判定）
fn name_aligns(comment: &str, dict_name: &str) -> bool {
    let c: Vec<char> = comment.chars().collect();
    let d: Vec<char> = dict_name.chars().collect();
    let has_common_3gram = |a: &[char], b: &[char]| {
        b.windows(3).any(|w| a.windows(3).any(|x| x == w))
    };
    c.len() >= 3 && d.len() >= 3 && (has_common_3gram(&c, &d) || has_common_3gram(&d, &c))
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

    #[test]
    fn dict_match_basic() {
        let mut dicts = std::collections::HashMap::new();
        dicts.insert(
            "CustClassif".to_string(),
            (
                "客户分类".to_string(),
                vec![
                    ("01".into(), "货架店铺".into()),
                    ("04".into(), "线下客户".into()),
                    ("06".into(), "其他财务专用".into()),
                ],
            ),
        );
        let vals = vec!["04".to_string(), "06".to_string(), "01".to_string()];
        // 小集合（3 值）须名称对齐：注释「客户分类」与字典名「客户分类」对齐 → 过
        let (kc, kn, _, cov) = best_dict_match(&vals, &dicts, "客户分类").unwrap();
        assert_eq!(kc, "CustClassif");
        assert_eq!(kn, "客户分类");
        assert!((cov - 1.0).abs() < 1e-9);
    }

    #[test]
    fn dict_match_rejects() {
        let mut dicts = std::collections::HashMap::new();
        dicts.insert(
            "K".to_string(),
            ("k".to_string(), vec![("01".into(), "a".into()), ("02".into(), "b".into())]),
        );
        // 单值不匹配（<2 个不同值）
        assert!(best_dict_match(&["01".to_string()], &dicts, "任意注释").is_none());
        // 覆盖率不足（2/4=50% < 80%）
        let mixed = vec!["01".to_string(), "02".to_string(), "xx".to_string(), "yy".to_string()];
        assert!(best_dict_match(&mixed, &dicts, "任意注释").is_none());
        // 值过多（非码列）
        let many: Vec<String> = (0..80).map(|i| i.to_string()).collect();
        assert!(best_dict_match(&many, &dicts, "任意注释").is_none());
    }

    #[test]
    fn dict_match_collision_guard() {
        // 实跑误配复现：menu_type 值{0,1,2} ⊆ 对账单状态码 —— 小集合+名称不对齐 → 拒
        let mut dicts = std::collections::HashMap::new();
        dicts.insert(
            "BillStatus".to_string(),
            (
                "对账单状态".to_string(),
                vec![
                    ("0".into(), "待确认".into()),
                    ("1".into(), "已确认".into()),
                    ("2".into(), "部分开票".into()),
                    ("3".into(), "已开票".into()),
                    ("4".into(), "拒绝".into()),
                ],
            ),
        );
        let vals = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        assert!(best_dict_match(&vals, &dicts, "菜单类型").is_none());
        // 含字母码的字典一样是撞车磁铁（data_scope_type={1,2} 撞联系人类型的教训）→ 拒
        let mut dicts2 = std::collections::HashMap::new();
        dicts2.insert(
            "ContactType".to_string(),
            (
                "联系人类型".to_string(),
                vec![("1".into(), "业务".into()), ("2".into(), "财务".into()), ("Y1".into(), "主联系人".into())],
            ),
        );
        assert!(best_dict_match(&vals, &dicts2, "数据范围id").is_none());
        // ≥8 个不同值 cov=1.0 → 大集合直通
        let nine: Vec<String> = (0..9).map(|i| i.to_string()).collect();
        dicts.get_mut("BillStatus").unwrap().1.extend([
            ("5".into(), "x5".into()),
            ("6".into(), "x6".into()),
            ("7".into(), "x7".into()),
            ("8".into(), "x8".into()),
        ]);
        assert!(best_dict_match(&nine, &dicts, "任意列注释").is_some());
        // 注释点名优先：注释写了「数据字典 K」→ 只评 K（值 ⊆ K 即中，不被其他字典抢）
        let mut dicts3 = std::collections::HashMap::new();
        dicts3.insert(
            "GOODS_CAT".to_string(),
            ("商品分类字典".to_string(), vec![("A".into(), "肠类".into()), ("B".into(), "挞类".into())]),
        );
        dicts3.insert(
            "CustClassif".to_string(),
            ("客户分类".to_string(), vec![("A".into(), "货架".into()), ("B".into(), "线下".into())]),
        );
        let ab = vec!["A".to_string(), "B".to_string()];
        let (kc, ..) = best_dict_match(&ab, &dicts3, "商品分类（数据字典 GOODS_CAT）").unwrap();
        assert_eq!(kc, "GOODS_CAT");
        // 名称对齐判据
        assert!(name_aligns("订单状态", "销售订单状态"));
        assert!(name_aligns("所属公司", "所属公司"));
        assert!(!name_aligns("数据范围类型", "合同类型"));
        assert!(!name_aligns("菜单类型", "对账单状态"));
    }

    // ── MapFilter 召回净化（SuperSonic SchemaMapper 五规则中文适配）──
    fn hits(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(n, w)| (n.to_string(), w.to_string())).collect()
    }
    fn kept(v: &[(&str, &str)]) -> Vec<String> {
        let h = hits(v);
        map_filter(&h).into_iter().map(|i| h[i].0.clone()).collect()
    }

    #[test]
    fn map_filter_longest_wins() {
        // 问「库存金额」不该同时拖出「库存量」(别名"库存")——两张口径卡打架
        assert_eq!(kept(&[("库存量", "库存"), ("库存金额", "库存金额")]), vec!["库存金额"]);
        assert_eq!(kept(&[("客户", "客户"), ("客户分类", "客户分类")]), vec!["客户分类"]);
    }

    #[test]
    fn map_filter_dedups_same_name() {
        // autodiscover 把同名列注册成多条维度 → 只留一条
        assert_eq!(kept(&[("所属公司编码", "公司编码"), ("所属公司编码", "公司编码")]), vec!["所属公司编码"]);
    }

    #[test]
    fn map_filter_drops_single_char() {
        assert!(kept(&[("费用", "费")]).is_empty());
    }

    #[test]
    fn map_filter_exact_beats_partial() {
        // 同一命中词下，名字与命中词完全相等的（满分）胜出
        assert_eq!(
            kept(&[("订单状态(0:暂存 108:无效)", "订单状态"), ("订单状态", "订单状态")]),
            vec!["订单状态"]
        );
    }

    #[test]
    fn map_filter_keeps_unrelated() {
        // 不同概念互不影响
        assert_eq!(kept(&[("销售额", "销售额"), ("省份", "各省")]), vec!["销售额", "省份"]);
    }

    #[test]
    fn match_word_takes_longest_alias() {
        let al: Vec<String> = ["多少单", "多少个订单"].iter().map(|s| s.to_string()).collect();
        assert_eq!(match_word("本月有多少个订单", "订单数", &al).as_deref(), Some("多少个订单"));
        assert_eq!(match_word("本月销售额", "订单数", &al), None);
    }

    #[test]
    fn clean_dim_name_cuts_at_separator() {
        assert_eq!(clean_dim_name("配送状态：100:待配送, 200:配送中").as_deref(), Some("配送状态"));
        assert_eq!(clean_dim_name("行类型（赠品，正品，结算）").as_deref(), Some("行类型"));
        assert_eq!(clean_dim_name("所属公司编码").as_deref(), Some("所属公司编码"));
        // 非中文/超长/过短 → None（退回字典名）
        assert_eq!(clean_dim_name("status"), None);
        assert_eq!(clean_dim_name("云之家附件上传状态说明补充文字"), None);
        assert_eq!(clean_dim_name("是"), None);
    }
}
