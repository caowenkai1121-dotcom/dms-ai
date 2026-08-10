//! 线下销售 DWS 事实合同。
//!
//! 查询端只从这里取得事实表、指标、维度与聚合 SQL，避免 direct/entity/daily_digest
//! 各自复制口径。当前字段只包含业务方明确给出的列；新增字段必须先取得业务方确认，
//! 不能仅因物理表存在该列就自动公开给问数链路。

use dms_connector::source::{ColumnInfo, SchemaSnapshot, TableInfo};

/// Doris 线下销售日事实表（库名不可省略，避免数据源默认库切换后查错表）。
pub const TABLE: &str = "sales_dw.dws_off_offline_sale_dfn";
/// 物理表名，供 schema/血缘校验使用。
pub const TABLE_NAME: &str = "dws_off_offline_sale_dfn";
/// 所有合同 SQL 使用同一个固定别名。
pub const ALIAS: &str = "sf";
/// 维度注册使用的来源声明。
pub const SOURCE_WITH_ALIAS: &str = "sales_dw.dws_off_offline_sale_dfn sf";
/// 事实时间列。
pub const ORDER_DATE: &str = "order_date";
/// 未指定时间时的受信全期边界。调用方不得各自发明不同的“全历史”起点。
pub const ALL_TIME_BEGIN: &str = "'1000-01-01'";
pub const ALL_TIME_END: &str = "'9999-12-31'";
/// 口径变更版本；切换事实源时必须递增。
pub const VERSION: &str = "2026.08.06-dws-contract-v2";

const TABLE_COMMENT: &str = "已验证线下销售 DWS 主事实；只公开业务确认的销售字段，不可按事实行数推算订单数。storecode/storename 是客户编码/客户名称，不是门店";

// 顺序与业务确认 SELECT 一致；毛利率是 gross_profit / revenue_excluding_tax
// 的派生指标，不登记为物理列。未列出的真实表字段也不向问数链路公开。
const SNAPSHOT_COLUMNS: &[(&str, &str, &str)] = &[
    ("order_date", "date", "销售事实日期；默认时间列"),
    ("storecode", "varchar(150)", "客户编码；不是门店编码"),
    ("storename", "varchar(150)", "客户名称；不是门店名称"),
    ("skucode", "varchar(150)", "商品编码"),
    ("skuname", "varchar(100)", "商品名称"),
    ("war_zone", "varchar(100)", "战区原值"),
    ("region", "varchar(100)", "省区原值"),
    ("qty", "decimal(13,4)", "销量事实；聚合口径 SUM(qty)"),
    ("amount", "decimal(13,4)", "销售额事实；聚合口径 SUM(amount)"),
    ("cost_excluding_tax", "decimal(13,4)", "不含税成本事实"),
    ("revenue_excluding_tax", "decimal(13,4)", "不含税收入事实"),
    ("gross_profit", "decimal(13,4)", "毛利额事实"),
];

pub fn contract_columns() -> impl Iterator<Item = &'static str> {
    SNAPSHOT_COLUMNS.iter().map(|(name, _, _)| *name)
}

/// 将默认库探针看不到的跨库销售事实幂等补入 schema 快照。
///
/// 保留探针已经取得的表规模、类型和注释，并补齐缺失的合同警示与列元数据。
pub fn enrich_schema_snapshot(snapshot: &mut SchemaSnapshot) -> bool {
    let mut changed = false;
    let table_name = if let Some(table) = snapshot
        .tables
        .iter_mut()
        .find(|table| table.name.eq_ignore_ascii_case(TABLE_NAME))
    {
        if table.comment.trim().is_empty() {
            table.comment = TABLE_COMMENT.to_string();
            changed = true;
        } else if !table.comment.contains(TABLE_COMMENT) {
            table.comment = format!("{}；{TABLE_COMMENT}", table.comment.trim());
            changed = true;
        }
        table.name.clone()
    } else {
        snapshot.tables.push(TableInfo {
            name: TABLE_NAME.to_string(),
            comment: TABLE_COMMENT.to_string(),
            row_estimate: 0,
        });
        changed = true;
        TABLE_NAME.to_string()
    };

    // 跨库 DESCRIBE 可能带回更多真实字段；默认销售事实合同仍只暴露业务确认列。
    let before = snapshot.columns.len();
    snapshot.columns.retain(|(source, column)| {
        !source.eq_ignore_ascii_case(&table_name)
            || SNAPSHOT_COLUMNS
                .iter()
                .any(|(name, _, _)| column.name.eq_ignore_ascii_case(name))
    });
    changed |= snapshot.columns.len() != before;

    for (index, &(name, data_type, comment)) in SNAPSHOT_COLUMNS.iter().enumerate() {
        if let Some((_, column)) = snapshot.columns.iter_mut().find(|(source, column)| {
            source.eq_ignore_ascii_case(&table_name) && column.name.eq_ignore_ascii_case(name)
        }) {
            if column.data_type.trim().is_empty() {
                column.data_type = data_type.to_string();
                changed = true;
            }
            if column.comment.trim().is_empty() {
                column.comment = comment.to_string();
                changed = true;
            }
            if column.ordinal <= 0 {
                column.ordinal = index as i64 + 1;
                changed = true;
            }
            continue;
        }
        snapshot.columns.push((
            table_name.clone(),
            ColumnInfo {
                name: name.to_string(),
                data_type: data_type.to_string(),
                comment: comment.to_string(),
                ordinal: index as i64 + 1,
            },
        ));
        changed = true;
    }
    changed
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Metric {
    SalesAmount,
    SalesQuantity,
    CostExcludingTax,
    RevenueExcludingTax,
    GrossProfit,
    GrossMargin,
}

pub const METRICS: &[Metric] = &[
    Metric::SalesAmount,
    Metric::SalesQuantity,
    Metric::CostExcludingTax,
    Metric::RevenueExcludingTax,
    Metric::GrossProfit,
    Metric::GrossMargin,
];

/// 同窗补充五值（裁决：销售类单指标 KPI 的答案顺带成本/收入/毛利）。
/// 顺序即 SELECT 列序；毛利率仍走「先汇总分子分母再相除」口径，不改用 amount。
/// 只用于无维度单指标命中后的补充查询；维度拆解/明细自带这些列，不走它。
pub const CONTEXT_METRICS: &[Metric] = &[
    Metric::SalesAmount,
    Metric::CostExcludingTax,
    Metric::RevenueExcludingTax,
    Metric::GrossProfit,
    Metric::GrossMargin,
];

impl Metric {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SalesAmount => "sales_amount",
            Self::SalesQuantity => "sales_qty",
            Self::CostExcludingTax => "sales_cost",
            Self::RevenueExcludingTax => "sales_revenue_ex_tax",
            Self::GrossProfit => "gross_profit_amount",
            Self::GrossMargin => "gross_margin",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::SalesAmount => "销售额",
            Self::SalesQuantity => "销量",
            Self::CostExcludingTax => "不含税成本",
            Self::RevenueExcludingTax => "不含税收入",
            Self::GrossProfit => "毛利额",
            Self::GrossMargin => "毛利率",
        }
    }

    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::SalesAmount => &[
                "销售总额", "营业额", "销售业绩", "业绩", "卖了多少", "买了多少", "销售趋势",
                "销售走势", "线下销售额", "经营销售额", "DWS销售额",
            ],
            Self::SalesQuantity => &[
                "销售量", "卖了多少件", "销售数量", "卖得最好", "最畅销",
                "最好卖", "卖得最多", "卖得好",
            ],
            Self::CostExcludingTax => &["成本", "成本额", "销售成本", "销售成本额", "不含税销售成本"],
            Self::RevenueExcludingTax => &[
                "未税收入", "销售不含税收入", "不含税销售收入", "净收入",
            ],
            Self::GrossProfit => &["毛利润", "销售毛利额", "销售毛利润"],
            Self::GrossMargin => &["销售毛利率", "毛利占比"],
        }
    }

    /// 注册表使用的无别名聚合表达式。
    pub const fn expression(self) -> &'static str {
        match self {
            Self::SalesAmount => "SUM(amount)",
            Self::SalesQuantity => "SUM(qty)",
            Self::CostExcludingTax => "SUM(cost_excluding_tax)",
            Self::RevenueExcludingTax => "SUM(revenue_excluding_tax)",
            Self::GrossProfit => "SUM(gross_profit)",
            Self::GrossMargin => "SUM(gross_profit)/NULLIF(SUM(revenue_excluding_tax),0)",
        }
    }

    /// 本合同 SQL builder 使用的别名限定表达式。
    pub const fn sql_expression(self) -> &'static str {
        match self {
            Self::SalesAmount => "SUM(sf.amount)",
            Self::SalesQuantity => "SUM(sf.qty)",
            Self::CostExcludingTax => "SUM(sf.cost_excluding_tax)",
            Self::RevenueExcludingTax => "SUM(sf.revenue_excluding_tax)",
            Self::GrossProfit => "SUM(sf.gross_profit)",
            Self::GrossMargin => "SUM(sf.gross_profit)/NULLIF(SUM(sf.revenue_excluding_tax),0)",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::SalesAmount => {
                "默认销售额＝Doris 线下销售日事实表 amount 求和，统计时间使用 order_date；不再使用旧发货明细 UNION/JOIN，旧订单口径见「订单额」"
            }
            Self::SalesQuantity => "销量＝Doris 线下销售日事实表 qty 求和，统计时间使用 order_date",
            Self::CostExcludingTax => {
                "不含税成本＝Doris 线下销售日事实表 cost_excluding_tax 求和，统计时间使用 order_date"
            }
            Self::RevenueExcludingTax => {
                "不含税收入＝Doris 线下销售日事实表 revenue_excluding_tax 求和，统计时间使用 order_date"
            }
            Self::GrossProfit => {
                "毛利额＝Doris 线下销售日事实表 gross_profit 求和，统计时间使用 order_date"
            }
            Self::GrossMargin => {
                "毛利率＝SUM(gross_profit) / NULLIF(SUM(revenue_excluding_tax), 0)，按小数比值返回；不得改用 amount，也不得按行数推算"
            }
        }
    }

    pub const fn unit(self) -> &'static str {
        match self {
            Self::GrossMargin => "ratio",
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dimension {
    OrderDate,
    CustomerCode,
    Customer,
    SkuCode,
    Goods,
    WarZone,
    Region,
    Month,
}

pub const DIMENSIONS: &[Dimension] = &[
    Dimension::OrderDate,
    Dimension::CustomerCode,
    Dimension::Customer,
    Dimension::SkuCode,
    Dimension::Goods,
    Dimension::WarZone,
    Dimension::Region,
    Dimension::Month,
];

impl Dimension {
    pub const fn code(self) -> &'static str {
        match self {
            Self::OrderDate => "sales_order_date",
            Self::CustomerCode => "customer_code",
            Self::Customer => "customer",
            Self::SkuCode => "sku_code",
            Self::Goods => "goods",
            Self::WarZone => "war_zone",
            Self::Region => "sales_region",
            Self::Month => "month",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::OrderDate => "销售日期",
            Self::CustomerCode => "客户编码",
            Self::Customer => "客户",
            Self::SkuCode => "商品编码",
            Self::Goods => "商品",
            Self::WarZone => "战区",
            Self::Region => "省区",
            Self::Month => "月份",
        }
    }

    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::OrderDate => &["日期", "按日", "每日", "每天"],
            Self::CustomerCode => &["客户代码", "客户编号", "经销商编码"],
            Self::Customer => &["客户名称", "客户名", "经销商"],
            Self::SkuCode => &["SKU编码", "商品代码", "SKU代码"],
            Self::Goods => &["商品名称", "单品", "SKU"],
            Self::WarZone => &["销售战区", "大战区"],
            Self::Region => &["销售省区", "区域", "销售区域", "片区"],
            Self::Month => &["按月", "每月", "每个月", "按月份", "各月", "月度"],
        }
    }

    pub const fn column(self) -> &'static str {
        match self {
            Self::OrderDate | Self::Month => ORDER_DATE,
            Self::CustomerCode => "storecode",
            Self::Customer => "storename",
            Self::SkuCode => "skucode",
            Self::Goods => "skuname",
            Self::WarZone => "war_zone",
            Self::Region => "region",
        }
    }

    pub const fn expression(self) -> &'static str {
        match self {
            Self::OrderDate => "DATE(sf.order_date)",
            Self::CustomerCode => "COALESCE(NULLIF(sf.storecode,''),'未知')",
            Self::Customer => {
                "COALESCE(NULLIF(sf.storename,''),NULLIF(sf.storecode,''),'未知')"
            }
            Self::SkuCode => "COALESCE(NULLIF(sf.skucode,''),'未知')",
            Self::Goods => "COALESCE(NULLIF(sf.skuname,''),NULLIF(sf.skucode,''),'未知')",
            Self::WarZone => "COALESCE(NULLIF(sf.war_zone,''),'未归属')",
            Self::Region => "COALESCE(NULLIF(sf.region,''),'未归属')",
            Self::Month => "DATE_FORMAT(sf.order_date,'%Y-%m')",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::OrderDate => "销售日期取 order_date，按自然日分组",
            Self::CustomerCode => "客户编码取 storecode；该列不是门店编码",
            Self::Customer => "客户名称优先取 storename，空名称回退客户编码 storecode；该列不是门店名称",
            Self::SkuCode => "商品编码取 skucode，空值归未知",
            Self::Goods => "商品优先取 skuname，空名称回退 skucode",
            Self::WarZone => "战区取 war_zone，空值归未归属",
            Self::Region => "省区只取业务确认字段 region，空值归未归属；不得改用 state",
            Self::Month => "月份由 order_date 截取到 YYYY-MM",
        }
    }
}

/// 由合同构造的受信谓词。内部 SQL 不对调用方开放，避免传入任意列名或 FROM 片段。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Predicate(String);

impl Predicate {
    pub fn eq(dimension: Dimension, value: &str) -> Self {
        Self(format!("{} = {}", dimension.expression(), quote(value)))
    }

    pub fn contains(dimension: Dimension, value: &str) -> Self {
        // INSTR 而不是 LIKE ESCAPE：Doris 不支持 ESCAPE 子句（实测 1105 语法错误），
        // 且子串语义本就是字面的（%/_ 不再特殊），两种方言同形。
        Self(format!("INSTR({}, {}) > 0", dimension.expression(), quote(value)))
    }

    pub fn one_of(dimension: Dimension, values: &[&str]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        Some(Self(format!(
            "{} IN ({})",
            dimension.expression(),
            values.iter().map(|value| quote(value)).collect::<Vec<_>>().join(", ")
        )))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    const fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Dimension(Dimension),
    Metric(Metric),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sort {
    pub key: SortKey,
    pub direction: SortDirection,
}

impl Sort {
    pub const fn dimension(dimension: Dimension, direction: SortDirection) -> Self {
        Self { key: SortKey::Dimension(dimension), direction }
    }

    pub const fn metric(metric: Metric, direction: SortDirection) -> Self {
        Self { key: SortKey::Metric(metric), direction }
    }

    fn expression(self) -> &'static str {
        match self.key {
            SortKey::Dimension(dimension) => dimension.expression(),
            SortKey::Metric(metric) => metric.sql_expression(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct QueryOptions<'a> {
    pub predicates: &'a [Predicate],
    pub sort: Option<Sort>,
    pub limit: Option<u32>,
}

impl<'a> Default for QueryOptions<'a> {
    fn default() -> Self {
        Self { predicates: &[], sort: None, limit: None }
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

/// 当前事实合同允许的全部分组维度名称。
pub fn dimension_names() -> Vec<&'static str> {
    DIMENSIONS.iter().map(|dimension| dimension.name()).collect()
}

/// 构造半开区间时间条件。`begin_sql`/`end_sql` 必须是调用方生成的可信 SQL 片段。
pub fn time_predicate(begin_sql: &str, end_sql: &str) -> String {
    format!(
        "{ALIAS}.{ORDER_DATE} >= {begin_sql} AND {ALIAS}.{ORDER_DATE} < {end_sql}"
    )
}

/// 把统一自然语言时间解析器产出的 `{}` 谓词还原为半开区间。
///
/// 只接受 kernel 生成的固定形态；无法证明边界时返回 `None`，调用方必须回落，不能静默改成全期。
pub fn time_bounds_from_template(template: &str) -> Option<(String, String)> {
    if let Some(rest) = template.strip_prefix("{} >= ") {
        let (begin, end) = rest.split_once(" AND {} < ")?;
        return Some((begin.to_string(), end.to_string()));
    }

    let (base, explicit_end) = template
        .split_once(" AND {} < ")
        .map_or((template, None), |(base, end)| (base, Some(end)));
    if let Some(rhs) = base.strip_prefix("DATE({}) = ") {
        return Some((rhs.to_string(), format!("DATE_ADD({rhs}, INTERVAL 1 DAY)")));
    }
    if let Some(rhs) = base.strip_prefix("YEAR({}) = ") {
        return Some((
            format!("MAKEDATE(({rhs}),1)"),
            format!("MAKEDATE(({rhs}) + 1,1)"),
        ));
    }
    if let Some(anchor) = base
        .strip_prefix("YEARWEEK({}, 1) = YEARWEEK(")
        .and_then(|rest| rest.strip_suffix(", 1)"))
    {
        let begin = format!("DATE_SUB(DATE({anchor}), INTERVAL WEEKDAY({anchor}) DAY)");
        let end = explicit_end
            .map(str::to_string)
            .unwrap_or_else(|| format!("DATE_ADD({begin}, INTERVAL 7 DAY)"));
        return Some((begin, end));
    }
    if base == "QUARTER({}) = QUARTER(CURDATE()) AND YEAR({}) = YEAR(CURDATE())" {
        let begin = "MAKEDATE(YEAR(CURDATE()),1) + INTERVAL QUARTER(CURDATE())*3-3 MONTH";
        return Some((begin.into(), format!("DATE_ADD({begin}, INTERVAL 3 MONTH)")));
    }
    None
}

fn is_current_period_to_date(question: &str) -> bool {
    if ![
        "本月", "这个月", "当月", "本周", "这周", "今年", "本年", "年初至今",
        "本季度", "这个季度",
    ]
    .iter()
    .any(|word| question.contains(word))
    {
        return false;
    }
    // 显式年份/季度/月覆盖相对词。例如“今年7月”已经被解析成完整 7 月，
    // 不能再把右端截到今天；只有纯“本月/本周/今年/本季度”才是进行中周期。
    dms_kernel::nl::time::explicit_year(question).is_none()
        && !has_explicit_month(question)
        && !has_explicit_quarter(question)
}

fn has_explicit_month(question: &str) -> bool {
    let chars = question.chars().collect::<Vec<_>>();
    chars.iter().enumerate().any(|(index, ch)| {
        if *ch != '月' || index == 0 {
            return false;
        }
        let previous = chars[index - 1];
        previous.is_ascii_digit() || "一二三四五六七八九十".contains(previous)
    })
}

fn has_explicit_quarter(question: &str) -> bool {
    let Some(index) = question.find("季度") else { return false };
    let head = question[..index].chars().rev().take(3).collect::<String>();
    head.chars().any(|ch| ch.is_ascii_digit() || "一二三四".contains(ch))
}

/// 当前销售事实窗口。进行中的月/周/年/季度只取到今天（含今天），避免未来日期脏数据混入；
/// 未写时间词时使用统一全期边界。
pub fn question_time_bounds(question: &str) -> Option<(String, String)> {
    let Some(template) = dms_kernel::nl::time::time_predicate(question) else {
        return Some((ALL_TIME_BEGIN.into(), ALL_TIME_END.into()));
    };
    let (begin, mut end) = time_bounds_from_template(&template)?;
    if is_current_period_to_date(question) {
        end = "DATE_ADD(CURDATE(), INTERVAL 1 DAY)".into();
    }
    Some((begin, end))
}

/// DWS 销售环比/同比窗口。进行中周期的基期同样包含对应日，和当前窗口保持同进度；
/// `DATE({}) = ...` 这类单日模板本身已是完整自然日，不再额外扩一天。
pub fn comparison_time_bounds(
    question: &str,
    template: &str,
) -> Option<(String, String)> {
    let has_explicit_end = template.contains(" AND {} < ");
    let (begin, mut end) = time_bounds_from_template(template)?;
    if has_explicit_end && is_current_period_to_date(question) {
        end = format!("DATE_ADD({end}, INTERVAL 1 DAY)");
    }
    Some((begin, end))
}

/// 构造单指标时间窗子查询；派生指标必须调用它复用基础指标口径。
pub fn metric_subquery(metric: Metric, begin_sql: &str, end_sql: &str) -> String {
    format!(
        "(SELECT {} FROM {TABLE} {ALIAS} WHERE {})",
        metric.sql_expression(),
        time_predicate(begin_sql, end_sql)
    )
}

/// 构造一个时间窗内的单指标聚合 SQL；空维度返回单值，非空维度自动生成 SELECT/GROUP BY。
pub fn aggregate_sql(
    metric: Metric,
    dimensions: &[Dimension],
    begin_sql: &str,
    end_sql: &str,
) -> String {
    aggregate_sql_many(&[metric], dimensions, begin_sql, end_sql)
}

/// 一次构造多个事实指标，供 BI 与日报避免重复扫描同一时间窗。
pub fn aggregate_sql_many(
    metrics: &[Metric],
    dimensions: &[Dimension],
    begin_sql: &str,
    end_sql: &str,
) -> String {
    aggregate_sql_with_options(
        metrics,
        dimensions,
        begin_sql,
        end_sql,
        QueryOptions::default(),
    )
}

/// 同一事实表上的受信查询入口：统一 FROM、聚合、时间、追加谓词、排序与 LIMIT。
pub fn aggregate_sql_with_options(
    metrics: &[Metric],
    dimensions: &[Dimension],
    begin_sql: &str,
    end_sql: &str,
    options: QueryOptions<'_>,
) -> String {
    assert!(!metrics.is_empty() || !dimensions.is_empty(), "事实查询至少选择一个指标或维度");
    assert!(
        dimensions.iter().all(|dimension| DIMENSIONS.contains(dimension)),
        "默认销售事实只允许业务确认维度"
    );
    let mut select = dimensions
        .iter()
        .map(|dimension| format!("{} AS `{}`", dimension.expression(), dimension.name()))
        .collect::<Vec<_>>();
    select.extend(
        metrics
            .iter()
            .map(|metric| format!("{} AS `{}`", metric.sql_expression(), metric.name())),
    );

    let mut predicates = vec![time_predicate(begin_sql, end_sql)];
    predicates.extend(options.predicates.iter().map(|predicate| predicate.0.clone()));
    let mut sql = format!(
        "SELECT {} FROM {TABLE} {ALIAS} WHERE {}",
        select.join(", "),
        predicates.join(" AND ")
    );
    if !dimensions.is_empty() {
        let group_by = dimensions
            .iter()
            .map(|dimension| dimension.expression())
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(" GROUP BY ");
        sql.push_str(&group_by);
    }
    if let Some(sort) = options.sort {
        sql.push_str(" ORDER BY ");
        sql.push_str(sort.expression());
        sql.push(' ');
        sql.push_str(sort.direction.sql());
    }
    if let Some(limit) = options.limit {
        sql.push_str(" LIMIT ");
        sql.push_str(&limit.clamp(1, 1000).to_string());
    }
    sql
}

/// 销售经营明细的唯一受信构造器。字段顺序与业务确认 SQL 一致；毛利率按当前明细行的
/// `gross_profit / revenue_excluding_tax` 展示，汇总毛利率仍必须使用 `Metric::GrossMargin`
/// 的“先汇总分子分母再相除”口径。
pub fn detail_sql(
    begin_sql: &str,
    end_sql: &str,
    predicates: &[Predicate],
    limit: u32,
) -> String {
    let mut filters = vec![time_predicate(begin_sql, end_sql)];
    filters.extend(predicates.iter().map(|predicate| predicate.0.clone()));
    format!(
        "SELECT sf.order_date AS `日期`, sf.storecode AS `客户编码`, \
                sf.storename AS `客户名称`, sf.skucode AS `商品编码`, \
                sf.skuname AS `商品名称`, sf.war_zone AS `战区`, sf.region AS `省区`, \
                sf.qty AS `数量`, sf.amount AS `销售额`, \
                sf.cost_excluding_tax AS `不含税成本`, \
                sf.revenue_excluding_tax AS `不含税收入`, sf.gross_profit AS `毛利额`, \
                sf.gross_profit / NULLIF(sf.revenue_excluding_tax, 0) AS `毛利率` \
         FROM {TABLE} {ALIAS} WHERE {} \
         ORDER BY sf.order_date DESC, ABS(sf.amount) DESC LIMIT {}",
        filters.join(" AND "),
        limit.clamp(1, 500)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_database_schema_enrichment_is_complete_and_idempotent() {
        let mut snapshot = SchemaSnapshot::default();
        assert!(enrich_schema_snapshot(&mut snapshot));
        assert_eq!(snapshot.tables.len(), 1);
        assert_eq!(snapshot.tables[0].name, TABLE_NAME);
        assert!(snapshot.tables[0].comment.contains("不可按事实行数推算订单数"));
        assert_eq!(snapshot.columns.len(), 12);

        let customer = snapshot
            .columns
            .iter()
            .find(|(_, column)| column.name == "storename")
            .expect("storename must be injected");
        assert_eq!(customer.1.data_type, "varchar(150)");
        assert!(customer.1.comment.contains("不是门店"));
        snapshot.tables[0].comment = format!("探针已有表注释；{TABLE_COMMENT}");
        {
            let amount = snapshot
                .columns
                .iter_mut()
                .find(|(_, column)| column.name == "amount")
                .expect("amount must be injected");
            amount.1.data_type = "decimal(20,6)".to_string();
            amount.1.comment = "探针已有列注释".to_string();
        }
        assert!(!enrich_schema_snapshot(&mut snapshot));
        assert!(snapshot.tables[0].comment.starts_with("探针已有表注释；"));
        let amount = snapshot
            .columns
            .iter()
            .find(|(_, column)| column.name == "amount")
            .expect("amount must remain present");
        assert_eq!(amount.1.data_type, "decimal(20,6)");
        assert_eq!(amount.1.comment, "探针已有列注释");
        assert_eq!(snapshot.tables.len(), 1);
        assert_eq!(snapshot.columns.len(), 12);

        let mut discovered = SchemaSnapshot {
            tables: vec![TableInfo {
                name: TABLE_NAME.to_string(),
                comment: "BI线下销售报表".to_string(),
                row_estimate: 42,
            }],
            columns: Vec::new(),
        };
        assert!(enrich_schema_snapshot(&mut discovered));
        assert!(discovered.tables[0].comment.starts_with("BI线下销售报表；"));
        assert!(discovered.tables[0].comment.contains(TABLE_COMMENT));
        assert_eq!(discovered.tables[0].row_estimate, 42);
        assert!(!enrich_schema_snapshot(&mut discovered));
    }

    #[test]
    fn detail_projection_is_fixed_bounded_and_uses_the_same_fact() {
        let predicates = [Predicate::eq(Dimension::Region, "湖南省区")];
        let sql = detail_sql("'2026-08-01'", "'2026-08-08'", &predicates, 2000);
        assert!(sql.contains("FROM sales_dw.dws_off_offline_sale_dfn sf"), "{sql}");
        assert!(sql.contains("sf.order_date AS `日期`"), "{sql}");
        assert!(sql.contains("sf.storecode AS `客户编码`"), "{sql}");
        assert!(sql.contains("sf.gross_profit / NULLIF(sf.revenue_excluding_tax, 0) AS `毛利率`"), "{sql}");
        assert!(sql.contains("COALESCE(NULLIF(sf.region,''),'未归属') = '湖南省区'"), "{sql}");
        assert!(sql.ends_with("LIMIT 500"), "{sql}");
        assert!(!sql.contains("SELECT *"), "{sql}");
    }

    #[test]
    fn sales_quantity_does_not_claim_logistics_events() {
        let aliases = Metric::SalesQuantity.aliases();
        assert!(!aliases.contains(&"出货量"));
        assert!(!aliases.contains(&"发货量"));
        assert_eq!(Metric::SalesQuantity.expression(), "SUM(qty)");
        assert_eq!(Metric::SalesQuantity.sql_expression(), "SUM(sf.qty)");
    }

    #[test]
    fn current_and_comparison_windows_share_the_same_progress_day() {
        let current = question_time_bounds("本月销售额").expect("本月窗口");
        assert_eq!(current.0, "DATE_FORMAT(CURDATE(),'%Y-%m-01')");
        assert_eq!(current.1, "DATE_ADD(CURDATE(), INTERVAL 1 DAY)");

        let previous = comparison_time_bounds(
            "本月销售额",
            dms_kernel::nl::time::prev_window("本月销售额").unwrap().0,
        )
        .expect("上月同期窗口");
        assert_eq!(previous.0, "DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01')");
        assert_eq!(previous.1, "DATE_ADD(CURDATE() - INTERVAL 1 MONTH, INTERVAL 1 DAY)");

        let yoy = comparison_time_bounds(
            "本月销售额",
            dms_kernel::nl::time::yoy_window("本月销售额").unwrap().0,
        )
        .expect("去年同期窗口");
        assert_eq!(yoy.1, "DATE_ADD(CURDATE() - INTERVAL 1 YEAR, INTERVAL 1 DAY)");

        assert_eq!(
            question_time_bounds("销售额"),
            Some((ALL_TIME_BEGIN.into(), ALL_TIME_END.into()))
        );
        let july = question_time_bounds("今年7月销售额").expect("显式月份窗口");
        assert_ne!(july.1, "DATE_ADD(CURDATE(), INTERVAL 1 DAY)");
        let explicit_year = question_time_bounds("2025年销售额").expect("显式年份窗口");
        assert_ne!(explicit_year.1, "DATE_ADD(CURDATE(), INTERVAL 1 DAY)");
    }

    /// 同窗补充五值的合同钉：指标集与列序固定，一条 SQL 同时间窗取齐；
    /// 毛利率仍是「汇总分子分母再相除」，无维度、无 GROUP BY（单行五值）。
    #[test]
    fn context_pack_is_five_metrics_one_row_same_window() {
        assert_eq!(
            CONTEXT_METRICS.iter().map(|metric| metric.name()).collect::<Vec<_>>(),
            ["销售额", "不含税成本", "不含税收入", "毛利额", "毛利率"]
        );
        let sql = aggregate_sql_many(CONTEXT_METRICS, &[], ":begin", ":end");
        for select in [
            "SUM(sf.amount) AS `销售额`",
            "SUM(sf.cost_excluding_tax) AS `不含税成本`",
            "SUM(sf.revenue_excluding_tax) AS `不含税收入`",
            "SUM(sf.gross_profit) AS `毛利额`",
            "SUM(sf.gross_profit)/NULLIF(SUM(sf.revenue_excluding_tax),0) AS `毛利率`",
        ] {
            assert!(sql.contains(select), "同窗补充缺 {select}: {sql}");
        }
        assert!(sql.contains("sf.order_date >= :begin AND sf.order_date < :end"), "{sql}");
        assert!(!sql.contains("GROUP BY"), "补充五值必须单行：{sql}");
        assert!(!sql.contains("ORDER BY") && !sql.contains("LIMIT"), "{sql}");
    }
}
