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
pub mod pitfall;
pub mod failure;
pub mod learn;
pub mod lexicon;
pub mod memory;
pub mod model;
pub mod user_pref;

// ─────────────────────────── 判官模式（学习写口总闸） ───────────────────────────

/// 判官/评测进程不许往学习面写字。
///
/// 由来：`tools/regression.py` 与 `tools/evaluation.py` 走的就是生产 `ask` 链路，于是每跑一趟
/// 全量题集就把 79 条评测问句连同**那一刻**的 SQL 写进 `meta.sql_exemplar` 与 `meta.memory`，
/// 再由 few-shot 与经验召回喂回给真实用户。跑得越勤，语料池被评测样本挤占得越狠，而且学的是
/// 评测当时的写法（口径一改就成了错教材）。判官必须能观察系统而不改变系统（2026-08-13 审计）。
///
/// `ponytail:` 进程级全局而不是把 `learn: bool` 一路穿到 `AskCtx` —— 判官本来就是独立进程，
/// 「这个进程不学习」正是进程级事实；真需要同进程内分会话开关时再改成 ctx 字段。
static JUDGE_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 由 server 启动时按 `DMSAI_JUDGE=1` 设一次（`main.rs`），其余地方只读。
pub fn set_judge_mode(on: bool) {
    JUDGE_MODE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// 学习写口（`exemplar::save*` / `memory::save_memory` / 教训候选）统一在入口处问它一句。
pub fn judge_mode() -> bool {
    JUDGE_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

// ─────────────────────────── 【K3-B ②】ds 谓词 ───────────────────────────

/// **ds 谓词的单一事实源**：所有召回/加载 SQL 都从这里拼，不许各写一份。
/// `'*'` = 跨源生效的全局条目；`$Q.` 是可选的表别名前缀（多表 JOIN 里 `ds_id` 会歧义）。
/// sqlx(PG) 只有位置占位符，故 `$DS` 由 `ds_pred_at` 换成该查询里 ds 的绑定序号。
/// 漂移守卫单测 `every_meta_recall_is_ds_scoped` 盯着这条：新加召回忘了拼即红。
pub const DS_PRED: &str = " AND $Q.ds_id IN ($DS, '*')";

/// `DS_PRED` 的**唯一**拼接点。`alias` 为空 = 单表查询（不加前缀）。
pub fn ds_pred_at(alias: &str, n: usize) -> String {
    expand_pred(DS_PRED, alias, n)
}

/// 谓词模板的单趟展开（`$Q.` → 别名前缀、`$DS` → 绑定序号）：`ds_pred_at` 与
/// `scoped_asset_pred` 共用这一份，不开第二份双 replace 拷贝。
fn expand_pred(template: &str, alias: &str, n: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(template.len() + alias.len() + 8);
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        if let Some(r) = after.strip_prefix("$Q.") {
            if !alias.is_empty() {
                out.push_str(alias);
                out.push('.');
            }
            rest = r;
        } else if let Some(r) = after.strip_prefix("$DS") {
            let _ = write!(out, "${n}");
            rest = r;
        } else {
            out.push('$');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

/// 单表召回的谓词（99% 的调用点）
pub fn ds_pred(n: usize) -> String {
    ds_pred_at("", n)
}

/// `ds_pred(1) + source_asset_live_pred_at("", 1)` 的进程内缓存成品：
/// 单表源资产活性谓词组合，召回 SQL 每问句都拼这同一串（对固定入参是确定串）。
pub fn source_live_pred_single() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| format!("{}{}", ds_pred(1), source_asset_live_pred_at("", 1)))
}

/// 「ds_pred(1) + 某活性谓词 ("",1)」的拼装：registry/model 七个加载点同一形态，共用一份。
pub(crate) fn scoped_pred_1(live_pred_at: fn(&str, usize) -> String) -> String {
    format!("{}{}", ds_pred(1), live_pred_at("", 1))
}

// 业务库热切换后，手工/自动发现的语义资产可能仍引用旧库表。`table_doc` 是当前物理
// schema 的事实源；DMS 必须命中 enabled 行才放行，尚未采集 schema 时 fail-closed。
// 其他数据源保留原冷启动兼容。`source_table` 允许写 JOIN/UNION 说明，因此提取全部表引用。
// 🔴 下面正则里的七类表前缀与 `TABLE_PREFIXES`（Rust 侧锚定清单）互为拷贝，改前缀两边一起改。
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
    expand_pred(template, alias, n)
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
/// 已验证的静态白名单复用到召回、注册表投影与历史样例，避免各模块复制表清单。
/// pub(crate)：`recall::ods` 的 JOIN 证据 forms 归一复用同一判定（去空白/反引号）。
pub(crate) fn catalog_ident(value: &str) -> &str {
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

/// 表名（目录字面量全小写）→ 资产的静态索引：热路径零线性扫（原来每调一次扫 57 项）。
fn asset_index()
-> &'static std::collections::HashMap<&'static str, &'static crate::warehouse_catalog::Asset> {
    static INDEX: std::sync::LazyLock<
        std::collections::HashMap<&'static str, &'static crate::warehouse_catalog::Asset>,
    > = std::sync::LazyLock::new(|| {
        crate::warehouse_catalog::ASSETS.iter().map(|a| (a.table, a)).collect()
    });
    &INDEX
}

pub fn warehouse_asset(table: &str) -> Option<&'static crate::warehouse_catalog::Asset> {
    let (database, table) = warehouse_table_parts(table);
    // 目录表名全小写：精确命中零分配；混入大写的输入回落线性的大小写不敏感扫（同旧行为）
    let asset = asset_index().get(table).copied().or_else(|| {
        crate::warehouse_catalog::ASSETS
            .iter()
            .find(|asset| asset.table.eq_ignore_ascii_case(table))
    })?;
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
    // 直接写入 out（原来经 `warehouse_qualified_table` 每 ident 一个临时 String）
    match warehouse_asset(ident) {
        Some(asset) => {
            use std::fmt::Write as _;
            let _ = write!(
                out,
                "{}.{}",
                crate::warehouse_catalog::database_of(asset),
                asset.table
            );
        }
        None => out.push_str(ident),
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

/// 业务表锚定前缀（七类分层/业务表）：本模块 SQL 正则（`SOURCE_ASSET_LIVE_PRED` 等三处）、
/// `source_refs`、`extract_tables 共用同一份清单 —— 改前缀只许改这里。
pub(crate) const TABLE_PREFIXES: &[&str] = &["t_", "dws_", "dwd_", "ads_", "ods_", "dim_", "fact_"];

fn source_refs(source: &str) -> Vec<(Option<String>, String)> {
    // 单趟抹掉引号段：字符串字面量里的 `t_x` 不是表引用（与 `warehouse_qualified_source`
    // 同一引号语义），同时完成去引号（原两次 replace 两遍全串扫描）
    let mut cleaned = String::with_capacity(source.len());
    let mut quote = None;
    let mut escaped = false;
    for ch in source.chars() {
        if let Some(end) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == end {
                quote = None;
            }
            cleaned.push(' ');
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            cleaned.push(' ');
        } else {
            cleaned.push(ch);
        }
    }
    cleaned
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .filter_map(|token| {
            // 三段名 a.b.c 取 database="a.b"（与 `warehouse_table_parts` 同一 rsplitn 语义）
            let mut parts = token.rsplitn(2, '.');
            let table = parts.next()?.to_ascii_lowercase();
            TABLE_PREFIXES
                .iter()
                .any(|p| table.starts_with(p))
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
    // 逐词大小写不敏感比较（原来每列先 to_ascii_lowercase 分配一个 String）
    const ALLOWED: &[&str] = &[
        "order_date",
        "storecode",
        "storename",
        "skucode",
        "skuname",
        "war_zone",
        "region",
        "qty",
        "amount",
        "cost_excluding_tax",
        "revenue_excluding_tax",
        "gross_profit",
    ];
    ALLOWED.iter().any(|c| c.eq_ignore_ascii_case(column))
}

pub fn forbidden_default_sales_column(column: &str) -> bool {
    const FORBIDDEN: &[&str] = &[
        "id",
        "type",
        "purchase_company_name",
        "clear_code",
        "ea_convert_quantity",
        "group_number",
        "ref_order_type",
        "state",
        "city",
        "price_group_name",
        "class2",
        "classfinal",
        "manger",
        "goods_type",
    ];
    FORBIDDEN.iter().any(|c| c.eq_ignore_ascii_case(column))
}

/// 合同表达式规范化：去空白/反引号 + 小写。pub(crate)：`registry::exemplar` 的语料
/// SQL 比对复用同一份（别开第三份拷贝）。
pub(crate) fn compact_contract_expr(expr: &str) -> String {
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
        // 非 DMS 源才放行；DMS 源但目录不放行 → 拒
        if ds != datasource::DMS_DS_ID {
            return true;
        }
        return false;
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

/// 只剥**标识符前缀位置**的 `sf.` 别名（`asf.qty` 里的子串不是别名，误剥会让合同比对假阴性）。
/// pub(crate)：`registry::exemplar` 的历史样例比对复用同一份。
pub(crate) fn strip_sf_alias(compact: &str) -> String {
    let mut out = String::with_capacity(compact.len());
    let mut chars = compact.chars().peekable();
    let mut at_boundary = true; // 串首即边界；边界 = 前一字符不是 [A-Za-z0-9_]
    while let Some(c) = chars.next() {
        if at_boundary && c == 's' && chars.peek() == Some(&'f') {
            let mut probe = chars.clone();
            probe.next(); // 'f'
            if probe.next() == Some('.') {
                chars.next(); // 吃掉 'f'
                chars.next(); // 吃掉 '.'
                at_boundary = true; // '.' 是边界字符
                continue;
            }
        }
        at_boundary = !(c.is_ascii_alphanumeric() || c == '_');
        out.push(c);
    }
    out
}

/// 指标卡/元素向量必须使用当前默认事实公式；仅换到正确表名不足以让旧口径重新生效。
pub fn catalog_allows_metric_expr(ds: &str, name: &str, source: &str, expr: &str) -> bool {
    if !catalog_allows_metric(ds, name, source) {
        return false;
    }
    if ds != datasource::DMS_DS_ID {
        return true;
    }
    let compact = strip_sf_alias(&compact_contract_expr(expr));
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
    metric_dimension_checker(ds, source)(dimension)
}

/// `catalog_allows_metric_dimension` 的批量版：source 解析一次，返回逐维度判定闭包
/// （逐行过滤 allowed_dimensions 的调用点原来每个维度都重跑一遍 source_refs）。
pub fn metric_dimension_checker(ds: &str, source: &str) -> impl Fn(&str) -> bool {
    // 「与默认事实无关就整批放行」的半边在闭包外只算一次
    let free = ds != datasource::DMS_DS_ID || {
        let refs = source_refs(source);
        !refs
            .iter()
            .any(|(_, table)| table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME))
    };
    move |dimension| {
        free || crate::sales_fact::DIMENSIONS
            .iter()
            .any(|d| d.name() == dimension)
    }
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
            database.as_deref().is_some_and(|db| db.eq_ignore_ascii_case("sales_dw"))
                && table == crate::sales_fact::TABLE_NAME
        });
    match default_sales_metric(name) {
        Some(_) => exact_default_fact,
        None => !exact_default_fact,
    }
}

/// 备份/快照表（t_employee_260410、bak_*、*_copy1、*_del_log 之类）不入元数据
pub fn is_backup_table(name: &str) -> bool {
    // 只用尾段长度，不收集字符串（原实现为取长度收集了整个尾部）
    let tail_len = name.chars().rev().take_while(|c| c.is_ascii_digit()).count();
    tail_len >= 4
        || name.starts_with("bak_")
        || name.contains("_copy")
        || name.ends_with("_del_log")
        || name.ends_with("_bak")
        || name.ends_with("_back")
        || name.ends_with("_backup")
        || name.ends_with("_backups")
        || name.ends_with("_history")
        || name.ends_with("_delete_history")
        // 6/8 位数字日期段单遍判（YYMMDD 如 t_xxx_260515_01；bak_sales_order_20251016_01 形态）
        || name.split('_').any(|seg| {
            matches!(seg.len(), 6 | 8) && seg.chars().all(|c| c.is_ascii_digit())
        })
}

/// 敏感列词表：**全仓单一事实源**已收进 kernel（F5），schema 过滤与 is_safe_select 共用一份。
pub use dms_kernel::nl::lexicon::SENSITIVE_COLS;

/// 敏感列：绝不进给 LLM 的 schema（旧项目 live.rs 同款，治本）
pub fn is_sensitive_col(name: &str) -> bool {
    // 物理列名惯例全小写：只有混入大写时才为小写化分配（schema 渲染热路径逐列调）
    let lowered;
    let n = if name.bytes().any(|b| b.is_ascii_uppercase()) {
        lowered = name.to_lowercase();
        &lowered
    } else {
        name
    };
    SENSITIVE_COLS.iter().any(|k| n.contains(k))
}

/// 表域归类（按名前缀，供检索上下文分组展示）。
/// 长前缀排前：`t_marketing_*` 不许被 `t_market` 抢中（测试钉着）。
pub fn domain_of(table: &str) -> &'static str {
    for (pre, d) in [
        ("t_sales_order", "订单"), ("t_after_sales", "售后"), ("t_customer", "客户"),
        ("t_goods", "商品"), ("t_marketing", "营销"), ("t_market", "市场费用"),
        ("t_activity", "活动"),
        ("t_invoice", "开票"), ("t_account", "对账"), ("t_device", "设备"),
        ("t_shop", "门店"), ("t_warehouse", "仓库"), ("t_winc", "赢销通"),
        ("t_employee", "组织"), ("t_department", "组织"), ("t_role", "权限"), ("t_menu", "权限"),
        ("t_points", "积分"),
    ] {
        if table.starts_with(pre) {
            return d;
        }
    }
    "其他"
}

/// 从 SQL 提取物理表名（复盘教训的锚定触发词，写路径使用）。
/// 大小写不敏感（LLM 大写 SQL 的 `T_SALES_ORDER` 同样锚定），产出统一小写；
/// 前缀清单 = `TABLE_PREFIXES`（与目录闸门的七类同一份）。
pub fn extract_tables(sql: &str) -> String {
    let mut tabs: Vec<String> = vec![];
    let mut cur = String::new();
    let push = |cur: &str, tabs: &mut Vec<String>| {
        let lower = cur.to_ascii_lowercase();
        if lower.len() > 2
            && lower.len() < 60
            && TABLE_PREFIXES.iter().any(|p| lower.starts_with(p))
            && !tabs.iter().any(|t| *t == lower)
        {
            tabs.push(lower);
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

    /// sf. 别名只剥标识符前缀位置：asf.qty 这类子串不许被误伤（合同比对假阴性防线）。
    #[test]
    fn strip_sf_alias_only_at_identifier_start() {
        assert_eq!(strip_sf_alias("sum(sf.amount)"), "sum(amount)");
        assert_eq!(strip_sf_alias("sf.amount"), "amount");
        assert_eq!(strip_sf_alias("asf.qty"), "asf.qty", "asf. 子串不许误剥");
        assert_eq!(strip_sf_alias("coalesce(sf.x,sf.y),'asf.z'"), "coalesce(x,y),'asf.z'");
        assert_eq!(strip_sf_alias("无别名"), "无别名");
    }

    /// 表域归类：长前缀优先（t_marketing_* 归「营销」，不被 t_market 抢成「市场费用」）。
    #[test]
    fn domain_of_prefers_longer_prefix() {
        assert_eq!(domain_of("t_marketing_goods"), "营销");
        assert_eq!(domain_of("t_marketing_zone_product"), "营销");
        assert_eq!(domain_of("t_market_total_expense"), "市场费用");
        assert_eq!(domain_of("t_sales_order"), "订单");
    }

    /// 锚定提取：大小写不敏感、产出小写、七类前缀全认（与目录闸门同一份清单）。
    #[test]
    fn extract_tables_case_insensitive_and_all_prefixes() {
        assert_eq!(extract_tables("SELECT * FROM T_SALES_ORDER"), "t_sales_order");
        let got = extract_tables("SELECT * FROM dws_x JOIN t_a ON 1=1 JOIN ods_y");
        assert_eq!(got, "dws_x,t_a,ods_y", "{got}");
        assert_eq!(extract_tables("select 1"), "");
    }

    /// source_refs：字符串字面量里的 t_x 不是表引用；三段名取 rsplitn 语义（db="a.b"）。
    #[test]
    fn source_refs_skip_string_literals() {
        // 字面量里的 t_secret 不算引用：真实目录表在 → 通过
        assert!(source_uses_warehouse_catalog(
            "sales_dw.dws_off_offline_sale_dfn WHERE note='t_secret'"
        ));
        // 只有字面量引用 = 没有任何表引用 → 不过
        assert!(!source_uses_warehouse_catalog("SELECT 't_sales_order'"));
    }

    #[test]
    fn sensitive_cols_filtered() {
        assert!(is_sensitive_col("login_pwd"));
        assert!(is_sensitive_col("api_token"));
        assert!(!is_sensitive_col("customer_code"));
    }

    /// `catalog_allows_metric_record` 连传 10+ 个同类型参数：字段两两不同的样本必须过判据，
    /// 任两个实参换位必须翻转 —— 传参顺序错位在这里直接红。
    #[test]
    fn metric_record_args_order_is_pinned() {
        let pass = catalog_allows_metric_record(
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
        );
        assert!(pass);
        // scope_filter 与 time_col 换位：判据必须翻 false（两槽位语义不同）
        let swapped = catalog_allows_metric_record(
            datasource::DMS_DS_ID,
            "销售额",
            crate::sales_fact::TABLE,
            crate::sales_fact::Metric::SalesAmount.expression(),
            crate::sales_fact::ORDER_DATE, // 挪到 scope_filter 槽位 → 非空即拒
            "",
            "",
            crate::sales_fact::Metric::SalesAmount.description(),
            crate::sales_fact::Metric::SalesAmount.unit(),
            "",
            crate::sales_fact::VERSION,
        );
        assert!(!swapped, "scope_filter/time_col 换位必须拒");
        // description 与 version 换位同样拒
        let swapped2 = catalog_allows_metric_record(
            datasource::DMS_DS_ID,
            "销售额",
            crate::sales_fact::TABLE,
            crate::sales_fact::Metric::SalesAmount.expression(),
            "",
            crate::sales_fact::ORDER_DATE,
            "",
            crate::sales_fact::VERSION,
            crate::sales_fact::Metric::SalesAmount.unit(),
            "",
            crate::sales_fact::Metric::SalesAmount.description(),
        );
        assert!(!swapped2, "description/version 换位必须拒");
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
