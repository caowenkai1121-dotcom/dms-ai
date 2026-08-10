//! 注册表：`meta.*` 的**唯一读写口**，与 `ds_pred` 这个多源总闸。
//!
//! 变更原因＝作用域与共用小工具。逐组落点：
//! `model`（装配侧行类型）/ `lexicon`（文本命中侧）/ `exemplar`（语料与教训）/
//! `element`（元素派生）/ `datasource`（【K3-A】数据源注册表与 ds 级可见性）/
//! `caliber`（声明 → `dms_kernel::CaliberRule` 的唯一构造点）。
//!
//! 搬运源 `server/src/meta.rs:242-304/701-720/1166-1167`。

pub mod caliber;
pub mod datasource;
pub mod element;
pub mod embed_fill;
pub mod exemplar;
pub mod lexicon;
pub mod memory;
pub mod model;

// ─────────────────────────── 【K3-B ②】ds 谓词 ───────────────────────────

/// **ds 谓词的单一事实源**：所有召回/加载 SQL 都从这里拼，不许各写一份。
/// `'*'` = 跨源生效的全局条目；`$Q.` 是可选的表别名前缀（多表 JOIN 里 `ds_id` 会歧义）。
/// sqlx(PG) 只有位置占位符，故 `$DS` 由 `ds_pred_at` 换成该查询里 ds 的绑定序号。
/// 漂移守卫单测 `every_meta_recall_is_ds_scoped` 盯着这条：新加召回忘了拼即红。
pub const DS_PRED: &str = " AND $Q.ds_id IN ($DS, '*')";

/// `DS_PRED` 的**唯一**拼接点。`alias` 为空 = 单表查询（不加前缀）。
pub fn ds_pred_at(alias: &str, n: usize) -> String {
    let q = if alias.is_empty() { String::new() } else { format!("{alias}.") };
    DS_PRED.replace("$Q.", &q).replace("$DS", &format!("${n}"))
}

/// 单表召回的谓词（99% 的调用点）
pub fn ds_pred(n: usize) -> String {
    ds_pred_at("", n)
}

// 业务库热切换后，手工/自动发现的语义资产可能仍引用旧库表。`table_doc` 是当前物理
// schema 的事实源；DMS 必须命中 enabled 行才放行，尚未采集 schema 时 fail-closed。
// 其他数据源保留原冷启动兼容。`source_table` 允许写 JOIN/UNION 说明，因此提取全部表引用。
pub const SOURCE_ASSET_LIVE_PRED: &str = r#" AND (
  ($DS <> 'dms' AND NOT EXISTS (SELECT 1 FROM meta.table_doc live_any
                                WHERE live_any.enabled AND live_any.ds_id IN ($DS, '*')))
  OR NOT EXISTS (
    SELECT 1
    FROM regexp_matches($Q.source_table,
      '(t_[A-Za-z0-9_]+|dws_[A-Za-z0-9_]+|dwd_[A-Za-z0-9_]+|ods_[A-Za-z0-9_]+|ads_[A-Za-z0-9_]+|dim_[A-Za-z0-9_]+|fact_[A-Za-z0-9_]+)',
      'g') AS asset_ref(parts)
    WHERE NOT EXISTS (
      SELECT 1 FROM meta.table_doc live_doc
      WHERE live_doc.enabled AND live_doc.ds_id IN ($DS, '*')
        AND lower(live_doc.table_name) = lower(asset_ref.parts[1])
    )
  )
)"#;

pub const TABLE_ASSET_LIVE_PRED: &str = r#" AND (
  ($DS <> 'dms' AND NOT EXISTS (SELECT 1 FROM meta.table_doc live_any
                                WHERE live_any.enabled AND live_any.ds_id IN ($DS, '*')))
  OR EXISTS (
    SELECT 1 FROM meta.table_doc live_doc
    WHERE live_doc.enabled AND live_doc.ds_id IN ($DS, '*')
      AND lower(live_doc.table_name) = lower(regexp_replace($Q.table_name, '^.*[.]', ''))
  )
)"#;

pub const JOIN_ASSET_LIVE_PRED: &str = r#" AND (
  ($DS <> 'dms' AND NOT EXISTS (SELECT 1 FROM meta.table_doc live_any
                                WHERE live_any.enabled AND live_any.ds_id IN ($DS, '*')))
  OR (
    EXISTS (SELECT 1 FROM meta.table_doc live_left
            WHERE live_left.enabled AND live_left.ds_id IN ($DS, '*')
              AND lower(live_left.table_name) = lower(regexp_replace($Q.left_table, '^.*[.]', '')))
    AND EXISTS (SELECT 1 FROM meta.table_doc live_right
                WHERE live_right.enabled AND live_right.ds_id IN ($DS, '*')
                  AND lower(live_right.table_name) = lower(regexp_replace($Q.right_table, '^.*[.]', '')))
  )
)"#;

pub const ELEMENT_ASSET_LIVE_PRED: &str = r#" AND (
  ($DS <> 'dms' AND NOT EXISTS (SELECT 1 FROM meta.table_doc live_any
                                WHERE live_any.enabled AND live_any.ds_id IN ($DS, '*')))
  OR $Q.kind NOT IN ('metric', 'dimension', 'value')
  OR ($Q.kind = 'metric' AND EXISTS (
    SELECT 1 FROM meta.metric live_metric
    WHERE live_metric.status = 'active' AND live_metric.ds_id = $Q.ds_id
      AND $Q.element_id = 'metric:' || live_metric.metric_code
      AND NOT EXISTS (
        SELECT 1
        FROM regexp_matches(live_metric.source_table,
          '(t_[A-Za-z0-9_]+|dws_[A-Za-z0-9_]+|dwd_[A-Za-z0-9_]+|ods_[A-Za-z0-9_]+|ads_[A-Za-z0-9_]+|dim_[A-Za-z0-9_]+|fact_[A-Za-z0-9_]+)',
          'g') AS asset_ref(parts)
        WHERE NOT EXISTS (
          SELECT 1 FROM meta.table_doc live_doc
          WHERE live_doc.enabled AND live_doc.ds_id IN ($DS, '*')
            AND lower(live_doc.table_name) = lower(asset_ref.parts[1])
        )
      )
  ))
  OR ($Q.kind = 'dimension' AND EXISTS (
    SELECT 1 FROM meta.dimension live_dim
    WHERE live_dim.status = 'active' AND live_dim.ds_id = $Q.ds_id
      AND $Q.element_id = 'dimension:' || live_dim.dim_code
      AND NOT EXISTS (
        SELECT 1
        FROM regexp_matches(live_dim.source_table,
          '(t_[A-Za-z0-9_]+|dws_[A-Za-z0-9_]+|dwd_[A-Za-z0-9_]+|ods_[A-Za-z0-9_]+|ads_[A-Za-z0-9_]+|dim_[A-Za-z0-9_]+|fact_[A-Za-z0-9_]+)',
          'g') AS asset_ref(parts)
        WHERE NOT EXISTS (
          SELECT 1 FROM meta.table_doc live_doc
          WHERE live_doc.enabled AND live_doc.ds_id IN ($DS, '*')
            AND lower(live_doc.table_name) = lower(asset_ref.parts[1])
        )
      )
  ))
  OR ($Q.kind = 'value' AND EXISTS (
    SELECT 1 FROM meta.value_map live_value
    WHERE live_value.ds_id = $Q.ds_id
      AND $Q.element_id = 'value:' || live_value.table_name || '.' ||
                          live_value.column_name || ':' || live_value.code
  ) AND EXISTS (
    SELECT 1 FROM meta.table_doc live_doc
    WHERE live_doc.enabled AND live_doc.ds_id IN ($DS, '*')
      AND lower(live_doc.table_name) = lower(
        regexp_replace(
          regexp_replace(split_part($Q.element_id, ':', 2), '[.][^.]+$', ''),
          '^.*[.]', ''
        )
      )
  ))
)"#;

fn scoped_asset_pred(template: &str, alias: &str, n: usize) -> String {
    let q = if alias.is_empty() { String::new() } else { format!("{alias}.") };
    template.replace("$Q.", &q).replace("$DS", &format!("${n}"))
}

pub fn source_asset_live_pred_at(alias: &str, n: usize) -> String {
    scoped_asset_pred(SOURCE_ASSET_LIVE_PRED, alias, n)
}

pub fn table_asset_live_pred_at(alias: &str, n: usize) -> String {
    scoped_asset_pred(TABLE_ASSET_LIVE_PRED, alias, n)
}

pub fn join_asset_live_pred_at(alias: &str, n: usize) -> String {
    scoped_asset_pred(JOIN_ASSET_LIVE_PRED, alias, n)
}

pub fn element_asset_live_pred_at(alias: &str, n: usize) -> String {
    scoped_asset_pred(ELEMENT_ASSET_LIVE_PRED, alias, n)
}

/// Doris 运行时目录的共享读取口。物理存在/启停仍由 `meta.table_doc` 决定；这里仅把
/// 已验证的 57 项白名单复用到召回、注册表投影与历史样例，避免各模块复制表清单。
fn catalog_ident(value: &str) -> &str {
    value
        .trim()
        .trim_matches(|c| matches!(c, '`' | '"'))
}

fn warehouse_table_parts(table: &str) -> (Option<&str>, &str) {
    let mut parts = table.trim().rsplitn(2, '.');
    let table = catalog_ident(parts.next().unwrap_or_default());
    let database = parts.next().map(catalog_ident);
    (database, table)
}

pub fn warehouse_asset(table: &str) -> Option<&'static crate::warehouse_catalog::Asset> {
    let (database, table) = warehouse_table_parts(table);
    let asset = crate::warehouse_catalog::ASSETS
        .iter()
        .find(|asset| asset.table.eq_ignore_ascii_case(table))?;
    database
        .map_or(true, |database| {
            database.eq_ignore_ascii_case(crate::warehouse_catalog::database_of(asset))
        })
        .then_some(asset)
}

/// 将裸表名或正确全限定名统一成目录中的裸表名，供 `meta.table_doc` 查询使用。
pub fn warehouse_table_name(table: &str) -> Option<&'static str> {
    warehouse_asset(table).map(|asset| asset.table)
}

pub fn warehouse_qualified_table(table: &str) -> Option<String> {
    warehouse_asset(table).map(|asset| {
        format!(
            "{}.{}",
            crate::warehouse_catalog::database_of(asset),
            asset.table
        )
    })
}

fn push_warehouse_ident(out: &mut String, ident: &mut String) {
    if ident.is_empty() {
        return;
    }
    if let Some(qualified) = warehouse_qualified_table(ident) {
        out.push_str(&qualified);
    } else {
        out.push_str(ident);
    }
    ident.clear();
}

/// 给 DMS 提示词或生成 SQL 补全目录库名；保留引号和字符串字面量，其他数据源不改写。
pub fn warehouse_qualified_source(ds: &str, source: &str) -> String {
    if ds != datasource::DMS_DS_ID {
        return source.to_string();
    }
    let mut out = String::with_capacity(source.len() + 32);
    let mut ident = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in source.chars() {
        if let Some(end) = quote {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == end {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            push_warehouse_ident(&mut out, &mut ident);
            out.push(ch);
            quote = Some(ch);
        } else if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
            ident.push(ch);
        } else {
            push_warehouse_ident(&mut out, &mut ident);
            out.push(ch);
        }
    }
    push_warehouse_ident(&mut out, &mut ident);
    out
}

/// 运行时提示直接从编译期目录投影完整合同，不依赖 `seed` 是否已经刷新过旧注释/向量。
pub fn warehouse_contract(table: &str) -> Option<String> {
    warehouse_asset(table).map(|asset| {
        format!(
            "【{}·{}】物理表：{}.{}（生成 SQL 必须使用完整库表名）。粒度：{}。时间/快照：{}。可用指标：{}。禁用规则：{}。比较能力：{}",
            asset.layer,
            asset.domain,
            crate::warehouse_catalog::database_of(asset),
            asset.table,
            asset.grain,
            asset.time_rule,
            asset.metrics,
            asset.forbidden,
            asset.comparison,
        )
    })
}

pub fn catalog_allows_table(ds: &str, table: &str) -> bool {
    ds != datasource::DMS_DS_ID || warehouse_asset(table).is_some()
}

fn source_refs(source: &str) -> Vec<(Option<String>, String)> {
    source
        .replace('`', "")
        .replace('"', "")
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .filter_map(|token| {
            let mut parts = token.rsplit('.');
            let table = parts.next()?.to_ascii_lowercase();
            (table.starts_with("t_")
                || table.starts_with("dws_")
                || table.starts_with("dwd_")
                || table.starts_with("ads_")
                || table.starts_with("ods_")
                || table.starts_with("dim_")
                || table.starts_with("fact_"))
            .then(|| (parts.next().map(str::to_ascii_lowercase), table))
        })
        .collect()
}

/// `source_table` 允许写 `库.表 alias / 库.表` 或含 JOIN/子查询的说明串；只提取业务表
/// 形态的 token。DMS 数仓语义资产必须至少引用一张表，且每张都在静态目录内。
pub fn source_uses_warehouse_catalog(source: &str) -> bool {
    let refs = source_refs(source);
    !refs.is_empty()
        && refs.iter().all(|(database, table)| {
            warehouse_asset(table).is_some_and(|asset| {
                // 历史注册值允许裸表名，避免合法目录资产被全过滤；所有提示/SQL 出口再
                // 由 warehouse_qualified_source 规范成目录中的库.表。显式错误库名仍拒绝。
                database.as_deref().map_or(true, |db| {
                    db.eq_ignore_ascii_case(crate::warehouse_catalog::database_of(asset))
                })
            })
        })
}

pub fn catalog_allows_source(ds: &str, source: &str) -> bool {
    ds != datasource::DMS_DS_ID || source_uses_warehouse_catalog(source)
}

/// 默认销售事实只公开业务方确认 SELECT 中的物理列。目录内其他专用资产仍使用各自合同。
pub fn catalog_allows_column(ds: &str, table: &str, column: &str) -> bool {
    if ds != datasource::DMS_DS_ID {
        return true;
    }
    let Some(asset) = warehouse_asset(table) else {
        return false;
    };
    if !asset.table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME) {
        return true;
    }
    matches!(
        column.to_ascii_lowercase().as_str(),
        "order_date"
            | "storecode"
            | "storename"
            | "skucode"
            | "skuname"
            | "war_zone"
            | "region"
            | "qty"
            | "amount"
            | "cost_excluding_tax"
            | "revenue_excluding_tax"
            | "gross_profit"
    )
}

pub fn forbidden_default_sales_column(column: &str) -> bool {
    matches!(
        column.to_ascii_lowercase().as_str(),
        "id"
            | "type"
            | "purchase_company_name"
            | "clear_code"
            | "ea_convert_quantity"
            | "group_number"
            | "ref_order_type"
            | "state"
            | "city"
            | "price_group_name"
            | "class2"
            | "classfinal"
            | "manger"
            | "goods_type"
    )
}

fn compact_contract_expr(expr: &str) -> String {
    expr.chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '`')
        .flat_map(char::to_lowercase)
        .collect()
}

fn default_sales_metric(name: &str) -> Option<crate::sales_fact::Metric> {
    crate::sales_fact::METRICS.iter().copied().find(|metric| {
        metric.name() == name || metric.aliases().iter().any(|alias| *alias == name)
    })
}

/// 默认销售事实维度必须同时满足：完整库名、单表来源、确认名称与确认表达式。
/// 商品分类、城市、价格组等分析应由目录中的专用 DWS/ADS 提供，不能借默认事实旧列兜底。
pub fn catalog_allows_dimension(ds: &str, name: &str, source: &str, expr: &str) -> bool {
    if ds != datasource::DMS_DS_ID || !catalog_allows_source(ds, source) {
        return ds != datasource::DMS_DS_ID;
    }
    let refs = source_refs(source);
    let references_default_sales = refs
        .iter()
        .any(|(_, table)| table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME));
    if !references_default_sales {
        return true;
    }
    if refs.len() != 1
        || !refs[0]
            .0
            .as_deref()
            .is_some_and(|database| database.eq_ignore_ascii_case("sales_dw"))
    {
        return false;
    }
    use crate::sales_fact::Dimension;
    [
        Dimension::OrderDate,
        Dimension::CustomerCode,
        Dimension::Customer,
        Dimension::SkuCode,
        Dimension::Goods,
        Dimension::WarZone,
        Dimension::Region,
        Dimension::Month,
    ]
    .iter()
    .any(|dimension| {
        dimension.name() == name
            && compact_contract_expr(dimension.expression()) == compact_contract_expr(expr)
    })
}

/// 指标卡/元素向量必须使用当前默认事实公式；仅换到正确表名不足以让旧口径重新生效。
pub fn catalog_allows_metric_expr(ds: &str, name: &str, source: &str, expr: &str) -> bool {
    if !catalog_allows_metric(ds, name, source) {
        return false;
    }
    if ds != datasource::DMS_DS_ID {
        return true;
    }
    let compact = compact_contract_expr(expr).replace("sf.", "");
    if let Some(metric) = default_sales_metric(name) {
        return compact == compact_contract_expr(metric.expression());
    }
    let refs = source_refs(source);
    if refs
        .iter()
        .any(|(_, table)| table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME))
    {
        return crate::sales_fact::METRICS
            .iter()
            .any(|metric| compact.contains(&compact_contract_expr(metric.expression())));
    }
    true
}

/// 默认销售指标整行都必须与当前合同一致，避免旧元数据只更新表名/公式后，仍通过
/// `scope_filter`、时间列、去重键、单位、说明或版本把历史口径带回装配与 prompt。
#[allow(clippy::too_many_arguments)]
pub fn catalog_allows_metric_record(
    ds: &str,
    name: &str,
    source: &str,
    expr: &str,
    scope_filter: &str,
    time_col: &str,
    dedup_keys: &str,
    description: &str,
    unit: &str,
    time_cap: &str,
    version: &str,
) -> bool {
    if !catalog_allows_metric_expr(ds, name, source, expr) {
        return false;
    }
    if ds != datasource::DMS_DS_ID {
        return true;
    }
    let Some(metric) = default_sales_metric(name) else {
        return true;
    };
    scope_filter.trim().is_empty()
        && time_col.trim().eq_ignore_ascii_case(crate::sales_fact::ORDER_DATE)
        && dedup_keys.trim().is_empty()
        && description.trim() == metric.description()
        && unit.trim() == metric.unit()
        && time_cap.trim().is_empty()
        && version.trim() == crate::sales_fact::VERSION
}

/// 历史 `allowed_dimensions` 只作为候选；默认事实最终以当前确认合同为准。
pub fn catalog_allows_metric_dimension(ds: &str, source: &str, dimension: &str) -> bool {
    if ds != datasource::DMS_DS_ID {
        return true;
    }
    let refs = source_refs(source);
    if !refs
        .iter()
        .any(|(_, table)| table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME))
    {
        return true;
    }
    matches!(
        dimension,
        "销售日期" | "客户编码" | "客户" | "商品编码" | "商品" | "战区" | "省区" | "月份"
    )
}

/// 默认销售事实指标只能绑定唯一受信 DWS；其他已验证指标仍按 57 项目录放行。
pub fn catalog_allows_metric(ds: &str, name: &str, source: &str) -> bool {
    if ds != datasource::DMS_DS_ID {
        return true;
    }
    if !source_uses_warehouse_catalog(source) {
        return false;
    }
    let refs = source_refs(source);
    let exact_default_fact = refs.len() == 1
        && refs.first().is_some_and(|(database, table)| {
            database.as_deref() == Some("sales_dw")
                && table == crate::sales_fact::TABLE_NAME
        });
    match default_sales_metric(name) {
        Some(_) => exact_default_fact,
        None => !exact_default_fact,
    }
}

/// 备份/快照表（t_employee_260410、bak_*、*_copy1、*_del_log 之类）不入元数据
pub fn is_backup_table(name: &str) -> bool {
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

/// 敏感列词表：**全仓单一事实源**已收进 kernel（F5），schema 过滤与 is_safe_select 共用一份。
pub use dms_kernel::nl::lexicon::SENSITIVE_COLS;

/// 敏感列：绝不进给 LLM 的 schema（旧项目 live.rs 同款，治本）
pub fn is_sensitive_col(name: &str) -> bool {
    let n = name.to_lowercase();
    SENSITIVE_COLS.iter().any(|k| n.contains(k))
}

/// 表域归类（按名前缀，供检索上下文分组展示）
pub fn domain_of(table: &str) -> &'static str {
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

/// 列注释 → 干净维度名（截到首个分隔符、限 2~8 字纯中文）：已收进 kernel（`nl::text`）。
pub use dms_kernel::nl::text::clean_dim_name;

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
    fn live_asset_predicates_are_scoped_and_dms_is_fail_closed() {
        let source = source_asset_live_pred_at("m", 2);
        assert!(source.contains("m.source_table"));
        assert!(source.contains("ds_id IN ($2, '*')"));
        assert!(source.contains("regexp_matches"));
        assert!(source.contains("NOT EXISTS (SELECT 1 FROM meta.table_doc live_any"));

        let table = table_asset_live_pred_at("v", 3);
        // 注册表名带 schema 前缀（`sales_dw.xxx`），与 table_doc 的裸表名比较前必须剥掉
        assert!(table.contains("lower(regexp_replace(v.table_name, '^.*[.]', ''))"));
        assert!(table.contains("ds_id IN ($3, '*')"));

        let join = join_asset_live_pred_at("j", 1);
        // 与 table 同款：注册表名带 schema 前缀，比较前剥掉
        assert!(join.contains("lower(regexp_replace(j.left_table, '^.*[.]', ''))"));
        assert!(join.contains("lower(regexp_replace(j.right_table, '^.*[.]', ''))"));

        let element = element_asset_live_pred_at("e", 4);
        assert!(element.contains("e.kind = 'metric'"));
        assert!(element.contains("e.kind = 'dimension'"));
        assert!(element.contains("e.kind = 'value'"));
        assert!(element.contains("ds_id IN ($4, '*')"));
        assert!(source.contains("$2 <> 'dms'"), "DMS 无 schema 时必须 fail-closed");
    }

    #[test]
    fn warehouse_contract_is_the_runtime_prompt_contract() {
        let contract = warehouse_contract("dws_off_offline_sale_dfn").unwrap();
        assert!(contract.contains("sales_dw.dws_off_offline_sale_dfn"));
        for part in ["粒度：", "时间/快照：", "可用指标：", "禁用规则：", "比较能力："] {
            assert!(contract.contains(part), "缺少目录合同段 {part}: {contract}");
        }
        assert!(catalog_allows_table(datasource::DMS_DS_ID, "dws_off_offline_sale_dfn"));
        assert!(!catalog_allows_table(
            datasource::DMS_DS_ID,
            "dws_mkt_app_distribution_inventory_dfn"
        ));
        assert!(catalog_allows_metric(
            datasource::DMS_DS_ID,
            "销售额",
            crate::sales_fact::TABLE
        ));
        assert!(!catalog_allows_metric(
            datasource::DMS_DS_ID,
            "销售额",
            "sales_dw.dws_off_third_party_sales_dnf"
        ));
        assert!(catalog_allows_metric_record(
            datasource::DMS_DS_ID,
            "销售额",
            crate::sales_fact::TABLE,
            crate::sales_fact::Metric::SalesAmount.expression(),
            "",
            crate::sales_fact::ORDER_DATE,
            "",
            crate::sales_fact::Metric::SalesAmount.description(),
            crate::sales_fact::Metric::SalesAmount.unit(),
            "",
            crate::sales_fact::VERSION,
        ));
        assert!(!catalog_allows_metric_record(
            datasource::DMS_DS_ID,
            "销售额",
            crate::sales_fact::TABLE,
            crate::sales_fact::Metric::SalesAmount.expression(),
            "deleted_flag = 0",
            crate::sales_fact::ORDER_DATE,
            "",
            crate::sales_fact::Metric::SalesAmount.description(),
            crate::sales_fact::Metric::SalesAmount.unit(),
            "yesterday",
            "stale-contract-v1",
        ));
        assert!(!source_uses_warehouse_catalog(
            "wrong_db.dws_off_offline_sale_dfn"
        ));
        assert!(source_uses_warehouse_catalog("ads_off_offline_region_sale_dfn"),
                "历史裸表登记应通过目录，SQL 出口再补全库名");
        assert_eq!(
            warehouse_qualified_source(
                datasource::DMS_DS_ID,
                "t_sales_order o JOIN t_customer c"
            ),
            "dms_ods.t_sales_order o JOIN dms_ods.t_customer c"
        );
        assert_eq!(
            warehouse_qualified_source("upload_1", "t_sales_order"),
            "t_sales_order"
        );
        assert_eq!(
            warehouse_qualified_source(
                datasource::DMS_DS_ID,
                "SELECT 't_customer' AS `来源` FROM t_sales_order"
            ),
            "SELECT 't_customer' AS `来源` FROM dms_ods.t_sales_order"
        );
    }
}
