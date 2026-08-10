//! 裸名称实体总览卡（业主 2026-08-01 裁决的形态）：只发一个客户名/商品名时，
//! 出**实体总览卡**而不是反问 —— need-intent 曾是裸名称的唯一落点，
//! 业主实测「客户名称和商品类型/名称都识别不了」（截图 tp/08abfcde）。
//!
//! 位置：Router 第四位（direct-doc 之后、semantic-cache 之前）。
//! 裸名称**必然**走不到这里之外的任何确定性路径（graph 要关系词、装配要指标、
//! direct-doc 要单号形），所以 accept 可以只用词法门：有指标/关系/单号/红线词的
//! 一律不接（那些归别人）；剩下的短问句才轮到我。商品名带时间词（「可颂香肠卷，本月」）
//! 合法 —— 时间词是卡片窗口的参数，不是指标。

use std::collections::HashSet;

use dms_connector::source::RowSet;
use dms_kernel::present::{Block, ColumnSpec, Interact, Kpi, Role, Semantic, ViewSpec};
use dms_semantic::sales_fact::{
    self, Dimension as SalesDimension, Metric as SalesMetric, Predicate as SalesPredicate,
    QueryOptions, Sort as SalesSort, SortDirection as SalesSortDirection,
};
use futures::future::join_all;

use crate::answerers::Answerer;
use crate::ctx::{AskCtx, AskResult};
use crate::gate::{gate_on, EXEC_TIMEOUT, MAX_ROWS};

mod category;

const LEADING_INTENT: &[&str] = &[
    "请帮我查询一下", "请帮我查一下", "帮我查询一下", "帮我查一下", "请帮我看看",
    "帮我看一下", "帮我看看", "请查询一下", "请查一下", "查询一下", "查一下", "请问",
    "请查询", "请查", "查询", "查", "看看", "看一下",
];

const TRAILING_INTENT: &[&str] = &[
    "的详细资料", "的详细信息", "的基础资料", "的基本信息", "的详细情况", "详细资料",
    "详细信息", "基础资料", "基本信息", "的详情", "的资料", "的信息", "的情况", "详情",
    "资料", "信息", "介绍一下", "介绍", "怎么样", "是什么", "吗", "呢",
];

/// 只从问句边界剥时间词；不能在正文 `replace`，否则会破坏合法客户/商品名称。
const TIME_AFFIXES: &[&str] = &[
    "今年以来", "本季度的", "上季度的", "这个月的", "上个月的", "本月的", "上月的",
    "本周的", "上周的", "今天的", "昨天的", "昨日的", "今年的", "去年的", "本季度",
    "上季度", "这个月", "上个月", "本月", "上月", "本周", "上周", "今天", "昨天",
    "昨日", "今年", "去年",
];

/// 这些是“要分析什么”的尾部语义，不是实体名。只检查完整尾部，不再对正文做子串黑名单。
const ANALYSIS_TAILS: &[&str] = &[
    "的销售额", "的销量", "的销售量", "的成本", "的毛利", "的毛利额", "的毛利率",
    "的订单数", "的订单量", "的订单明细", "的下单信息", "的销售明细", "有哪些订单",
    "有哪些客户", "有哪些商品", "有多少订单", "卖了多少", "哪个卖得好", "卖得好",
    "销售趋势", "销量趋势", "销售额趋势", "销售额", "销售量", "销量", "成本",
    "毛利额", "毛利率", "订单数", "订单量", "买过的客户", "购买客户", "的客户",
    "的库存", "的费用", "的退款", "的售后", "按省份", "按月份", "按客户", "按商品",
];

/// 用户已经给出具体实体，只是在末尾说明希望总览卡重点带出哪一类上下文。
/// 这些问法仍由实体卡承接：客户/商品卡本身就同时包含主档、经营摘要与最近订单。
/// 不收“多少/排行/趋势/有哪些客户”等分析目标，避免抢 graph/direct 的确定性查询。
const ENTITY_VIEW_TAILS: &[&str] = &[
    "的订单明细", "的下单信息", "的订单信息", "的销售明细", "的销售表现",
    "的下单情况", "的订单情况", "的销售情况",
];

const QUESTION_MARKERS: &[&str] = &[
    "多少", "哪些", "哪个", "为什么", "怎么", "如何", "排行", "排名", "对比", "趋势",
    "明细", "清单", "汇总", "合计", "分别", "按省", "按月", "按客户", "按商品",
];

const METRIC_ONLY: &[&str] = &[
    "销售额", "销量", "销售量", "订单数", "订单量", "客单价", "成本", "毛利", "毛利额",
    "毛利率", "库存", "退款", "售后", "费用", "收入", "利润",
];

/// 只拦问句首部的写操作意图，不能在实体正文中做子串匹配。
const WRITE_INTENT_PREFIXES: &[&str] = &[
    "请帮我删除", "请帮我修改", "请帮我更新", "请帮我新增", "帮我删除", "帮我修改",
    "帮我更新", "帮我新增", "请删除", "请修改", "请更新", "请新增", "删除", "清空",
    "作废", "修改", "更新", "新增", "添加", "插入", "创建", "编辑", "撤销", "取消",
];

pub struct EntityAnswerer;

impl EntityAnswerer {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Customer,
    Category,
    Goods,
    Brand,
    Shop,
    Employee,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Customer => "客户",
            Self::Category => "商品分类",
            Self::Goods => "商品",
            Self::Brand => "品牌",
            Self::Shop => "门店",
            Self::Employee => "员工",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MatchField {
    Auto,
    Code,
    Name,
    Alias,
    Model,
}

#[derive(Clone, Debug)]
struct ParsedEntity {
    kind: Option<Kind>,
    field: MatchField,
    value: String,
}

/// 顺序即最长前缀优先；短前缀永远放在同族长前缀之后。
const ENTITY_PREFIXES: &[(&str, Kind, MatchField)] = &[
    ("商品分类", Kind::Category, MatchField::Name),
    ("商品类型", Kind::Category, MatchField::Name),
    ("商品品类", Kind::Category, MatchField::Name),
    ("产品类型", Kind::Category, MatchField::Name),
    ("客户名称", Kind::Customer, MatchField::Name),
    ("客户简称", Kind::Customer, MatchField::Alias),
    ("客户编码", Kind::Customer, MatchField::Code),
    ("客户代码", Kind::Customer, MatchField::Code),
    ("商品名称", Kind::Goods, MatchField::Name),
    ("产品名称", Kind::Goods, MatchField::Name),
    ("商品简称", Kind::Goods, MatchField::Alias),
    ("商品编码", Kind::Goods, MatchField::Code),
    ("商品代码", Kind::Goods, MatchField::Code),
    ("产品编码", Kind::Goods, MatchField::Code),
    ("产品代码", Kind::Goods, MatchField::Code),
    ("SKU编码", Kind::Goods, MatchField::Code),
    ("SKU代码", Kind::Goods, MatchField::Code),
    ("商品型号", Kind::Goods, MatchField::Model),
    ("产品型号", Kind::Goods, MatchField::Model),
    ("规格型号", Kind::Goods, MatchField::Model),
    ("品牌名称", Kind::Brand, MatchField::Name),
    ("门店名称", Kind::Shop, MatchField::Name),
    ("门店编码", Kind::Shop, MatchField::Code),
    ("门店代码", Kind::Shop, MatchField::Code),
    ("业务员姓名", Kind::Employee, MatchField::Name),
    ("业务员名称", Kind::Employee, MatchField::Name),
    ("业务员编码", Kind::Employee, MatchField::Code),
    ("员工姓名", Kind::Employee, MatchField::Name),
    ("员工名称", Kind::Employee, MatchField::Name),
    ("员工编码", Kind::Employee, MatchField::Code),
    ("型号", Kind::Goods, MatchField::Model),
    ("分类", Kind::Category, MatchField::Name),
    ("品类", Kind::Category, MatchField::Name),
    ("客户", Kind::Customer, MatchField::Auto),
    ("商品", Kind::Goods, MatchField::Auto),
    ("产品", Kind::Goods, MatchField::Auto),
    ("品牌", Kind::Brand, MatchField::Name),
    ("门店", Kind::Shop, MatchField::Auto),
    ("业务员", Kind::Employee, MatchField::Auto),
    ("员工", Kind::Employee, MatchField::Auto),
];

fn trim_edge(s: &str) -> &str {
    s.trim_matches(|c: char| {
        c.is_whitespace() || "，。？?、,.~～!！:：;；「」『』()（）".contains(c)
    })
}

fn strip_leading<'a>(mut s: &'a str, words: &[&str]) -> &'a str {
    loop {
        s = trim_edge(s);
        let Some(rest) = words.iter().find_map(|word| s.strip_prefix(word)) else {
            return s;
        };
        s = rest;
    }
}

fn strip_trailing<'a>(mut s: &'a str, words: &[&str]) -> &'a str {
    loop {
        s = trim_edge(s);
        let Some(rest) = words.iter().find_map(|word| s.strip_suffix(word)) else {
            return s;
        };
        s = rest;
    }
}

fn parse_entity(question: &str) -> Option<ParsedEntity> {
    let mut body = trim_edge(question);
    loop {
        let before = body;
        body = strip_leading(body, LEADING_INTENT);
        body = strip_leading(body, TIME_AFFIXES);
        if body == before {
            break;
        }
    }
    if WRITE_INTENT_PREFIXES.iter().any(|intent| body.starts_with(intent)) {
        return None;
    }

    let mut kind = None;
    let mut field = MatchField::Auto;
    for (prefix, candidate_kind, candidate_field) in ENTITY_PREFIXES {
        if let Some(rest) = body.strip_prefix(prefix) {
            kind = Some(*candidate_kind);
            field = *candidate_field;
            body = trim_edge(rest);
            break;
        }
    }
    body = strip_leading(body, LEADING_INTENT);
    body = strip_leading(body, TIME_AFFIXES);

    let mut entity_view = false;
    loop {
        let before = body;
        body = strip_trailing(body, TIME_AFFIXES);
        let without_view = strip_trailing(body, ENTITY_VIEW_TAILS);
        entity_view |= without_view != body;
        body = without_view;
        if body == before {
            break;
        }
    }
    // 无显式“客户名称/商品编码”等字段提示时，完整关系/分析问题交给 graph、direct 或
    // LLM；只有“具体名称 + 总览侧面”例外。否则“昨天的设备订单”会被当成名为
    // “设备订单”的实体做模糊查询。
    if kind.is_none()
        && crate::triage::analytical_question_hit(question)
        && (!entity_view
            || !looks_like_named_entity(body)
            || crate::triage::analytical_question_hit(body))
    {
        return None;
    }

    let pre_tail = trim_edge(body);
    if ANALYSIS_TAILS.iter().any(|tail| pre_tail.ends_with(tail)) {
        return None;
    }
    loop {
        let before = body;
        body = strip_trailing(body, TRAILING_INTENT);
        body = strip_trailing(body, TIME_AFFIXES);
        if body == before {
            break;
        }
    }
    let value = trim_edge(body);
    if value.chars().count() < 2
        || value.chars().count() > 80
        || looks_like_doc_code(value)
        || ['\'', '"', '%', '\\'].iter().any(|ch| value.contains(*ch))
    {
        return None;
    }
    if ANALYSIS_TAILS.iter().any(|tail| value.ends_with(tail))
        || QUESTION_MARKERS
            .iter()
            .any(|marker| value.starts_with(marker) || value.ends_with(marker))
        || (kind.is_none() && METRIC_ONLY.contains(&value))
    {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if ["delete", "drop", "truncate", "update", "insert"]
        .iter()
        .any(|word| lower.starts_with(word))
    {
        return None;
    }
    // 裸型号窄判据：字母+数字+连字符的纯 ASCII 码按商品型号解析（DHT150-6）。
    // 单据码已被上面的 `looks_like_doc_code` 整体拒掉；名称/编码自动匹配接不住裸型号 ——
    // 实测它只嵌在商品名尾部（goods_specification_name 是「450g*20袋」那路包装规格）。
    let (kind, field) = if kind.is_none() && looks_like_goods_model(value) {
        (Some(Kind::Goods), MatchField::Model)
    } else {
        (kind, field)
    };
    Some(ParsedEntity { kind, field, value: value.to_string() })
}

/// 组织/公司形态证据：DMS 客户名带渠道前缀（customer_class 04=线下客户 的「线下-」命名约定），
/// 或以公司类后缀收尾。命中即不可能是自然人姓名 —— 员工目录里的同名行是客户的登录账号
/// （实测 t_employee 里 actual_name 含「有限公司」的有 1022 行），不是员工本人。
fn looks_like_company(value: &str) -> bool {
    const CHANNEL_PREFIXES: &[&str] = &["线下-", "线上-"];
    const ORG_SUFFIXES: &[&str] = &[
        "有限责任公司", "股份有限公司", "有限公司", "公司", "集团", "工厂", "厂",
        "合作社", "经营部", "商行", "超市", "中心", "门市部", "经销部", "批发部",
    ];
    let v = value.trim();
    CHANNEL_PREFIXES.iter().any(|p| v.starts_with(p))
        || ORG_SUFFIXES.iter().any(|s| v.ends_with(s))
}

/// 商品形态证据：名称内嵌「数字+字母」混编的型号段（0400G00、DHT150-6），
/// 或「数字+度量/包装单位」的数量规格（450克、20袋、1箱）。客户/员工/制度名都没有
/// 这种结构；判据只认形态不认词，与具体商品无关。
fn looks_like_goods_spec(value: &str) -> bool {
    // ① 型号段：连续 ASCII（含连字符）同时含数字与字母，长度 ≥ 4 —— 短规格（400g）让给单位判据。
    let mut len = 0usize;
    let mut digit = false;
    let mut alpha = false;
    let mut model = false;
    for b in value.bytes().chain(std::iter::once(b' ')) {
        if b.is_ascii_alphanumeric() || b == b'-' {
            len += 1;
            digit |= b.is_ascii_digit();
            alpha |= b.is_ascii_alphabetic();
        } else {
            model |= len >= 4 && digit && alpha;
            len = 0;
            digit = false;
            alpha = false;
        }
    }
    if model {
        return true;
    }
    // ② 数量规格：数字紧跟中文度量/包装单位（400克 / 500毫升 / 20袋 / 1箱）。
    const UNITS: &[&str] = &[
        "毫升", "公斤", "克", "袋", "瓶", "盒", "箱", "包", "件", "罐", "杯", "支", "斤", "升",
    ];
    UNITS.iter().any(|unit| {
        value
            .match_indices(unit)
            .any(|(i, _)| value[..i].chars().last().is_some_and(|c| c.is_ascii_digit()))
    })
}

/// 裸实体名的形态证据（公司形态 || 商品规格形态）。triage 用它把这类裸名称钉死在 Data 路，
/// 不再交给 fast-LLM 二分类抛硬币 —— 实测同一句「线下-揭阳市和利食品有限公司」17 秒内
/// 被判成 knowledge 两次、entity-card 一次（query_log 2026-08-10 01:18）。
pub(crate) fn entity_form_hit(question: &str) -> bool {
    let Some(parsed) = parse_entity(question) else { return false };
    looks_like_company(&parsed.value) || looks_like_goods_spec(&parsed.value)
}

/// auto 模式（无「客户/商品/员工」显式前缀）的类型收窄：公司形态证据只留组织类实体。
/// 显式前缀是用户的明确指示，形态证据无权覆盖。
fn narrow_kinds(kinds: Vec<Kind>, parsed: &ParsedEntity) -> Vec<Kind> {
    if parsed.kind.is_none() && looks_like_company(&parsed.value) {
        kinds
            .into_iter()
            .filter(|k| matches!(k, Kind::Customer | Kind::Shop))
            .collect()
    } else {
        kinds
    }
}

/// 并列候选的类型优先级：经营对象（客户/商品）排在目录对象（员工）之前。
/// 之前按 label 的 UTF-8 字节序排，「员工」(U+5458) 永远压在「客户」(U+5BA2) 头上 ——
/// 实测「线下-云南食左食右食品有限公司」候选卡首行是「员工 / 3832」。
fn kind_priority(kind: Kind) -> u8 {
    match kind {
        Kind::Customer => 0,
        Kind::Goods => 1,
        Kind::Brand => 2,
        Kind::Shop => 3,
        Kind::Category => 4,
        Kind::Employee => 5,
    }
}

/// 裸型号窄判据：「字母 + 数字 + 连字符」的纯 ASCII 码（DHT150-6）。
/// 三类字符缺一不可：纯数字日期段（2026-08）、纯字母连字符词（ABC-DEF）都不算型号。
fn looks_like_goods_model(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.len() >= 4
        && value.contains('-')
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && bytes.iter().any(|b| b.is_ascii_alphabetic())
        && bytes.iter().any(|b| b.is_ascii_digit())
}

fn looks_like_named_entity(value: &str) -> bool {
    let value = trim_edge(value);
    value.chars().count() >= 2
        && !METRIC_ONLY.contains(&value)
        && !["客户", "商品", "产品", "订单", "设备", "销售"].contains(&value)
}

/// 返回值保留给路由词法门与已有调用方；字段/类型意图由 `parse_entity` 单独携带。
#[cfg(test)]
pub(crate) fn bare_name(question: &str) -> Option<String> {
    parse_entity(question).map(|parsed| parsed.value)
}

/// 只拦 DMS 已知单据码族。不能用“连字符 + 数字”泛判：设备型号 DHT150-6
/// 也是合法商品名的一部分，旧判据会把整条实体问答误送到 need-intent。
fn looks_like_doc_code(name: &str) -> bool {
    let upper = name.trim().to_ascii_uppercase();
    upper.starts_with("HJXH-")
        || upper.starts_with("HJXH_")
        || upper.starts_with("SPC-")
        || ["IO", "SQ", "CG"].iter().any(|p| {
            upper.strip_prefix(p).is_some_and(|rest| {
                rest.len() >= 6 && rest.chars().all(|c| c.is_ascii_digit())
            })
        })
}

/// 组合装/拼装编码（名称带 `|` 或 `:数量` 尾，如「皇家小虎可颂香肠卷0400G00:4」）不是单品实体：
/// 模糊命中里先剔除；剔完一个都不剩时原样返回（宁可让用户挑，不静默答错）。
fn drop_combo_goods(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let is_combo = |c: &Candidate| {
        c.kind == Kind::Goods && {
            let name = c.name.trim();
            name.contains('|')
                || name.contains('：')
                || name
                    .rsplit_once(':')
                    .is_some_and(|(_, tail)| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()))
        }
    };
    let kept: Vec<Candidate> = candidates.iter().filter(|c| !is_combo(c)).cloned().collect();
    if kept.is_empty() { candidates } else { kept }
}

fn prefix_hint(question: &str) -> Option<Kind> {
    parse_entity(question).and_then(|parsed| parsed.kind)
}

/// 一次「查名取行」。SQL 全走闸门（只读红线 + 权限注入），失败一律 None（回落后续成员）。
async fn fetch_rows(
    cx: &AskCtx<'_>,
    sql: &str,
) -> anyhow::Result<Option<RowSet>> {
    let scoped = match gate_on(cx.p, sql, cx.scope, cx.ds_global, cx.source.dialect()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("实体卡 SQL 未过闸门，回落: {e}");
            return Ok(None);
        }
    };
    match cx.source.fetch(&scoped, MAX_ROWS, EXEC_TIMEOUT).await {
        Ok(rs) => Ok(Some(rs)),
        Err(e) => {
            tracing::warn!("实体卡取数失败，回落: {e}");
            Ok(None)
        }
    }
}

fn num(rs: &RowSet) -> f64 {
    num_at(rs, 0)
}

fn num_at(rs: &RowSet, index: usize) -> f64 {
    opt_num_at(rs, index).unwrap_or(0.0)
}

fn opt_num_at(rs: &RowSet, index: usize) -> Option<f64> {
    rs.rows
        .first()
        .and_then(|r| r.get(index))
        .and_then(crate::answerers::hits::cell_num)
}

fn entity_pairs(rs: Option<&RowSet>) -> Vec<(String, serde_json::Value)> {
    let Some(rs) = rs else { return vec![] };
    let Some(row) = rs.rows.first() else { return vec![] };
    rs.columns
        .iter()
        .cloned()
        .zip(row.iter().cloned())
        .map(|(label, value)| {
            let empty = matches!(&value, serde_json::Value::Null)
                || matches!(&value, serde_json::Value::String(s) if s.trim().is_empty());
            if empty {
                let placeholder = if rs.redacted.iter().any(|column| column == &label) {
                    "因权限隐藏"
                } else if label.contains("首次") || label.contains("最近") {
                    "暂无"
                } else {
                    "未维护"
                };
                (label, serde_json::Value::from(placeholder))
            } else {
                (label, value)
            }
        })
        .collect()
}

/// KPI 标签的时间语义：有时间词按窗口（本月/今年…），没有就是**累计**（全期）——
/// 标签跟着谓词走，不许在没有窗口时顶「本月」（实测：裸名称不带时间词，
/// `time_predicate` 为 None 时卡上写的是全期数）。
fn period_label(question: &str, what: &str) -> String {
    match dms_kernel::nl::time::time_phrase_of(question) {
        Some(ph) => format!("{ph}{what}"),
        None => format!("累计{what}（全期）"),
    }
}

fn entity_time_suffix(question: &str, column: &str) -> String {
    use dms_kernel::nl::time::{fill_time_col, time_predicate};
    let Some(tpl) = time_predicate(question) else {
        return String::new();
    };
    format!(" AND {}", fill_time_col(&tpl, column))
}

/// 实体销售 KPI 只通过共享 DWS 合同构造。合同负责事实表、字段与聚合表达式；
/// 此处仅追加同一套自然语言时间谓词，所有 SQL 仍会经过 `gate_on` 权限注入。
pub(super) fn dws_entity_sql(
    question: &str,
    metrics: &[SalesMetric],
    predicates: &[SalesPredicate],
) -> Option<String> {
    let (begin, end) = sales_fact::question_time_bounds(question)?;
    Some(sales_fact::aggregate_sql_with_options(
        metrics,
        &[],
        &begin,
        &end,
        QueryOptions {
            predicates,
            sort: None,
            limit: None,
        },
    ))
}

fn push_sales_kpis(
    items: &mut Vec<Kpi>,
    question: &str,
    sales: &RowSet,
    specs: &[(usize, &str, Semantic)],
) {
    for &(index, label, semantic) in specs {
        if let Some(value) = opt_num_at(sales, index) {
            items.push(Kpi {
                label: period_label(question, label),
                value: serde_json::Value::from(value),
                semantic,
                delta: None,
            });
        }
    }
}

fn dws_relation_sql(
    question: &str,
    metrics: &[SalesMetric],
    dimensions: &[SalesDimension],
    predicates: &[SalesPredicate],
    sort: SalesSort,
) -> Option<String> {
    let (begin, end) = sales_fact::question_time_bounds(question)?;
    Some(sales_fact::aggregate_sql_with_options(
        metrics,
        dimensions,
        &begin,
        &end,
        QueryOptions { predicates, sort: Some(sort), limit: Some(10) },
    ))
}

impl Answerer for EntityAnswerer {
    fn route(&self) -> &'static str {
        "entity-card"
    }

    /// 词法门（同步无 IO）：只解析“实体本身/实体资料”问法；带指标或关系的长问句交给分析链。
    fn accept(&self, cx: &AskCtx<'_>) -> bool {
        parse_entity(cx.question).is_some()
    }

    fn answer<'a>(&'a self, cx: &'a AskCtx<'a>) -> dms_kernel::BoxFut<'a, anyhow::Result<Option<AskResult>>> {
        Box::pin(async move {
            let Some(parsed) = parse_entity(cx.question) else { return Ok(None) };
            if parsed.kind == Some(Kind::Category) {
                return category::card(cx, &parsed.value).await;
            }
            resolve_entity(cx, &parsed).await
        })
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    kind: Kind,
    code: String,
    name: String,
}

fn can_view_employee(cx: &AskCtx<'_>) -> bool {
    cx.p.administrator_flag || cx.p.role_code == "admin"
}

fn candidate_kinds(cx: &AskCtx<'_>, requested: Option<Kind>) -> Vec<Kind> {
    let mut kinds = match requested {
        Some(kind) => vec![kind],
        None => vec![Kind::Customer, Kind::Goods, Kind::Brand, Kind::Shop, Kind::Employee],
    };
    if !can_view_employee(cx) {
        kinds.retain(|kind| *kind != Kind::Employee);
    }
    kinds
}

async fn resolve_entity(
    cx: &AskCtx<'_>,
    parsed: &ParsedEntity,
) -> anyhow::Result<Option<AskResult>> {
    if parsed.kind == Some(Kind::Employee) && !can_view_employee(cx) {
        return Ok(Some(employee_denied(cx)));
    }
    let kinds = narrow_kinds(candidate_kinds(cx, parsed.kind), parsed);
    let exact = collect_candidates(cx, &kinds, parsed, true).await?;
    if exact.len() == 1 {
        return render_candidate(cx, &exact[0]).await;
    }
    if exact.len() > 1 {
        return Ok(Some(candidate_card(cx, &parsed.value, exact)));
    }
    if parsed.field == MatchField::Code {
        return Ok(None);
    }
    let fuzzy = drop_combo_goods(collect_candidates(cx, &kinds, parsed, false).await?);
    if fuzzy.len() == 1 {
        return render_candidate(cx, &fuzzy[0]).await;
    }
    if fuzzy.len() > 1 {
        return Ok(Some(candidate_card(cx, &parsed.value, fuzzy)));
    }
    if parsed.kind.is_none() {
        return category::card(cx, &parsed.value).await;
    }
    Ok(None)
}

async fn collect_candidates(
    cx: &AskCtx<'_>,
    kinds: &[Kind],
    parsed: &ParsedEntity,
    exact: bool,
) -> anyhow::Result<Vec<Candidate>> {
    let results = join_all(
        kinds
            .iter()
            .copied()
            .map(|kind| candidates_for(cx, kind, parsed.field, &parsed.value, exact)),
    )
    .await;
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for result in results {
        for candidate in result? {
            let key = (candidate.kind, candidate.code.clone(), candidate.name.clone());
            if seen.insert(key) {
                candidates.push(candidate);
            }
        }
    }
    candidates.sort_by(|a, b| {
        kind_priority(a.kind)
            .cmp(&kind_priority(b.kind))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.code.cmp(&b.code))
    });
    candidates.truncate(20);
    Ok(candidates)
}

async fn candidates_for(
    cx: &AskCtx<'_>,
    kind: Kind,
    field: MatchField,
    value: &str,
    exact: bool,
) -> anyhow::Result<Vec<Candidate>> {
    if kind == Kind::Employee && !can_view_employee(cx) {
        return Ok(Vec::new());
    }
    let safe = esc(value);
    if kind == Kind::Goods && field == MatchField::Model {
        // 型号嵌在商品**名称**里（2026-08-07 实测数仓：DHT150-6 只在 goods_name/sku_name
        // 尾部；规格字段是「450g*20袋」那路包装规格，不是设备型号）。
        // 型号没有「精确等于整名」的形态，精确/模糊两轮同一条名称 LIKE。
        let sql = format!(
            "SELECT g.goods_code, g.goods_name FROM t_goods g \
             WHERE g.deleted_flag = 0 AND g.goods_name LIKE '%{safe}%' \
             ORDER BY g.goods_name LIMIT 8"
        );
        let Some(rows) = fetch_rows(cx, &sql).await? else { return Ok(Vec::new()) };
        return Ok(rows
            .rows
            .iter()
            .filter_map(|row| {
                let code = row.first().and_then(value_text)?.trim().to_string();
                let name = row.get(1).and_then(value_text)?.trim().to_string();
                (!code.is_empty() && !name.is_empty()).then_some(Candidate { kind, code, name })
            })
            .collect());
    }
    let condition = candidate_condition(kind, field, &safe, exact);
    let sql = match kind {
        Kind::Customer => format!(
            "SELECT c.customer_code, c.customer_name FROM t_customer c \
             WHERE c.deleted_flag = 0 AND ({condition}) ORDER BY c.customer_name LIMIT 8"
        ),
        Kind::Goods => format!(
            "SELECT DISTINCT g.goods_code, g.goods_name FROM t_goods g \
             WHERE g.deleted_flag = 0 AND ({condition}) ORDER BY g.goods_name LIMIT 8"
        ),
        Kind::Brand => format!(
            "SELECT DISTINCT COALESCE(NULLIF(g.brand_code,''),g.brand_name), g.brand_name FROM t_goods g \
             WHERE g.deleted_flag = 0 AND g.brand_name <> '' AND ({condition}) ORDER BY g.brand_name LIMIT 8"
        ),
        Kind::Shop => format!(
            "SELECT DISTINCT COALESCE(NULLIF(o.shop_code,''),o.shop_name), o.shop_name FROM t_sales_order o \
             WHERE o.deleted_flag = 0 AND o.shop_name <> '' AND ({condition}) ORDER BY o.shop_name LIMIT 8"
        ),
        Kind::Employee => format!(
            "SELECT CAST(e.employee_id AS CHAR), e.actual_name FROM t_employee e \
             WHERE e.deleted_flag = 0 AND e.disabled_flag = 0 AND ({condition}) \
             ORDER BY e.actual_name LIMIT 8"
        ),
        Kind::Category => return Ok(Vec::new()),
    };
    let Some(rows) = fetch_rows(cx, &sql).await? else { return Ok(Vec::new()) };
    Ok(rows
        .rows
        .iter()
        .filter_map(|row| {
            let code = row.first().and_then(value_text)?.trim().to_string();
            let name = row.get(1).and_then(value_text)?.trim().to_string();
            (!code.is_empty() && !name.is_empty()).then_some(Candidate { kind, code, name })
        })
        .collect())
}

fn candidate_condition(kind: Kind, field: MatchField, safe: &str, exact: bool) -> String {
    let op = if exact { "=" } else { "LIKE" };
    let value = if exact {
        format!("'{safe}'")
    } else {
        format!("'%{safe}%'")
    };
    let fields: &[&str] = match (kind, field) {
        (Kind::Customer, MatchField::Code) => &["c.customer_code"],
        (Kind::Customer, MatchField::Name) => &["c.customer_name"],
        (Kind::Customer, MatchField::Alias) => &["c.customer_short_name"],
        (Kind::Customer, _) if exact => &["c.customer_code", "c.customer_name"],
        (Kind::Customer, _) => &["c.customer_code", "c.customer_name", "c.customer_short_name"],
        (Kind::Goods, MatchField::Code) => &["g.goods_code"],
        (Kind::Goods, MatchField::Name) => &["g.goods_name"],
        (Kind::Goods, MatchField::Alias) => &["g.goods_short_name"],
        (Kind::Goods, MatchField::Model) => &["g.goods_code", "g.goods_name"],
        (Kind::Goods, _) if exact => &["g.goods_code", "g.goods_name"],
        (Kind::Goods, _) => &["g.goods_code", "g.goods_name", "g.goods_short_name"],
        (Kind::Brand, MatchField::Code) => &["g.brand_code"],
        (Kind::Brand, _) => &["g.brand_name"],
        (Kind::Shop, MatchField::Code) => &["o.shop_code"],
        (Kind::Shop, MatchField::Name) => &["o.shop_name"],
        (Kind::Shop, _) => &["o.shop_code", "o.shop_name"],
        (Kind::Employee, MatchField::Code) => &["CAST(e.employee_id AS CHAR)"],
        (Kind::Employee, _) => &["e.actual_name"],
        (Kind::Category, _) => &[],
    };
    fields
        .iter()
        .map(|column| format!("{column} {op} {value}"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn value_text(value: &serde_json::Value) -> Option<&str> {
    value.as_str()
}

async fn render_candidate(
    cx: &AskCtx<'_>,
    candidate: &Candidate,
) -> anyhow::Result<Option<AskResult>> {
    match candidate.kind {
        Kind::Customer => customer_card(cx, candidate).await,
        Kind::Goods => goods_card(cx, candidate).await,
        Kind::Brand => brand_card(cx, candidate).await,
        Kind::Shop => shop_card(cx, candidate).await,
        Kind::Employee => employee_card(cx, candidate).await,
        Kind::Category => category::card(cx, &candidate.name).await,
    }
}

fn exact_question(candidate: &Candidate) -> String {
    match candidate.kind {
        Kind::Customer => format!("客户编码 {}", candidate.code),
        Kind::Goods => format!("商品编码 {}", candidate.code),
        Kind::Brand => format!("品牌名称 {}", candidate.name),
        Kind::Shop if !candidate.code.is_empty() && candidate.code != candidate.name => {
            format!("门店编码 {}", candidate.code)
        }
        Kind::Shop => format!("门店名称 {}", candidate.name),
        Kind::Employee => format!("员工编码 {}", candidate.code),
        Kind::Category => format!("商品分类 {}", candidate.name),
    }
}

fn candidate_card(cx: &AskCtx<'_>, query: &str, candidates: Vec<Candidate>) -> AskResult {
    let rows = candidates
        .iter()
        .map(|candidate| {
            vec![
                serde_json::Value::from(candidate.kind.label()),
                serde_json::Value::from(candidate.code.clone()),
                serde_json::Value::from(candidate.name.clone()),
            ]
        })
        .collect::<Vec<_>>();
    let drill = candidates.iter().map(exact_question).collect();
    let row_count = rows.len();
    AskResult {
        sql: format!("实体候选匹配：{query}（精确优先，未自动选择）"),
        columns: vec!["实体类型".into(), "编码".into(), "名称".into()],
        rows,
        row_count,
        truncated: false,
        elapsed_ms: cx.t0.elapsed().as_millis(),
        route: "entity-card".into(),
        view: ViewSpec {
            columns: vec![
                ColumnSpec { name: "实体类型".into(), role: Role::Category, semantic: Semantic::None },
                ColumnSpec { name: "编码".into(), role: Role::Id, semantic: Semantic::None },
                ColumnSpec { name: "名称".into(), role: Role::Category, semantic: Semantic::None },
            ],
            blocks: vec![Block::Table],
            interact: Interact { drill },
            insight: Some("匹配到多个实体，请选择具体对象；系统未自动猜测。".into()),
        },
        supplemental: None,
        comparisons: vec![],
        subs: vec![],
        caliber_note: None,
        truncation_note: None,
        redacted: vec![],
        scope_note: None,
        trust: None,
        steps: vec![],
    }
}

fn employee_denied(cx: &AskCtx<'_>) -> AskResult {
    let mut answer = candidate_card(cx, "员工目录", Vec::new());
    answer.view.insight = Some(
        "当前 DMS 身份未能证明具备员工目录查看权限，已按最小权限原则拒绝展示。".into(),
    );
    answer
}

/// SQL 字面量转义（库里来的值也要过一道 —— 客户/商品卡已有的约定）
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

/// 品牌不伪造 DWS 品牌维度，只展示已唯一确认品牌的主档商品集合。
async fn brand_card(cx: &AskCtx<'_>, candidate: &Candidate) -> anyhow::Result<Option<AskResult>> {
    let brand = candidate.name.clone();
    let b = esc(&brand);
    let Some(goods) = fetch_rows(
        cx,
        &format!(
            "SELECT goods_name AS `商品`, goods_code AS `商品编码` FROM t_goods \
             WHERE deleted_flag = 0 AND brand_name = '{b}' ORDER BY goods_name LIMIT 20"
        ),
    )
    .await? else { return Ok(None) };
    let goods_n = goods.rows.len() as i64;
    let items = vec![
        Kpi { label: "品牌".into(), value: serde_json::Value::from(brand.clone()), semantic: Semantic::Goods, delta: None },
        Kpi { label: "展示商品数".into(), value: serde_json::Value::from(goods_n), semantic: Semantic::Count, delta: None },
    ];
    let drill = vec![format!("{brand}有哪些商品")];
    Ok(Some(build_card(
        &format!("SELECT … FROM t_goods WHERE brand_name = '{b}'; 品牌总览卡"),
        &brand,
        items,
        goods,
        drill,
        cx,
    )))
}

/// 门店与客户严格分开：DWS 的 storecode/storename 是客户，不用于门店卡。
async fn shop_card(cx: &AskCtx<'_>, candidate: &Candidate) -> anyhow::Result<Option<AskResult>> {
    let shop = candidate.name.clone();
    let shop_code = esc(&candidate.code);
    let shop_predicate = if candidate.code == candidate.name || candidate.code.is_empty() {
        format!("o.shop_name = '{}'", esc(&shop))
    } else {
        format!("o.shop_code = '{shop_code}'")
    };
    let otime = entity_time_suffix(cx.question, "o.order_time");
    let stats_sql = format!(
        "SELECT COUNT(DISTINCT sales_order_code) AS `订单数`, COUNT(DISTINCT customer_code) AS `关联客户数` \
         FROM t_sales_order o WHERE deleted_flag = 0 AND {shop_predicate}{otime}"
    );
    let recent_sql = format!(
        "SELECT sales_order_code AS `单号`, order_time AS `时间`, customer_name AS `客户`, \
                total_amount AS `订单金额`, order_status AS `状态` \
         FROM t_sales_order o WHERE deleted_flag = 0 AND {shop_predicate} \
         ORDER BY order_time DESC LIMIT 5"
    );
    let (stats, recent) = futures::join!(fetch_rows(cx, &stats_sql), fetch_rows(cx, &recent_sql));
    let Some(stats) = stats? else { return Ok(None) };
    let Some(recent) = recent? else { return Ok(None) };
    let orders_n = stats.rows.first().and_then(|r| r.first()).and_then(crate::answerers::hits::cell_num).unwrap_or(0.0);
    let cust_n = stats.rows.first().and_then(|r| r.get(1)).and_then(crate::answerers::hits::cell_num).unwrap_or(0.0);
    let items = vec![
        Kpi { label: "门店".into(), value: serde_json::Value::from(shop.clone()), semantic: Semantic::None, delta: None },
        Kpi { label: period_label(cx.question, "订单数"), value: serde_json::Value::from(orders_n), semantic: Semantic::Count, delta: None },
        Kpi { label: period_label(cx.question, "关联客户数"), value: serde_json::Value::from(cust_n), semantic: Semantic::Count, delta: None },
    ];
    let drill = vec![format!("门店{shop}的订单明细")];
    Ok(Some(build_card(
        &format!("{stats_sql}; 门店总览卡"),
        &shop,
        items,
        recent,
        drill,
        cx,
    )))
}

/// 业务员总览卡：DWS 只有不稳定的经理名称，没有员工 ID，销售额不做名称映射。
async fn employee_card(cx: &AskCtx<'_>, candidate: &Candidate) -> anyhow::Result<Option<AskResult>> {
    if !can_view_employee(cx) {
        return Ok(Some(employee_denied(cx)));
    }
    let eid = candidate.code.clone();
    let ename = candidate.name.clone();
    let e = esc(&eid);
    let otime = entity_time_suffix(cx.question, "o.order_time");
    let profile_sql = format!(
        "SELECT employee_id AS `员工编码`, actual_name AS `员工姓名`, login_name AS `登录名`, \
                department_id AS `部门编码` FROM t_employee \
         WHERE deleted_flag = 0 AND disabled_flag = 0 AND employee_id = '{e}' LIMIT 1"
    );
    let stats_sql = format!(
        "SELECT COUNT(DISTINCT sales_order_code) AS `订单数`, COUNT(DISTINCT customer_code) AS `关联客户数` \
         FROM t_sales_order o WHERE deleted_flag = 0 AND o.owner_manager = '{e}'{otime}"
    );
    let recent_sql = format!(
        "SELECT sales_order_code AS `单号`, order_time AS `时间`, customer_name AS `客户`, \
                total_amount AS `订单金额`, order_status AS `状态` \
         FROM t_sales_order o WHERE deleted_flag = 0 AND o.owner_manager = '{e}' \
         ORDER BY order_time DESC LIMIT 5"
    );
    let (profile, stats, recent) = futures::join!(
        fetch_rows(cx, &profile_sql),
        fetch_rows(cx, &stats_sql),
        fetch_rows(cx, &recent_sql),
    );
    let profile = profile?;
    let Some(stats) = stats? else { return Ok(None) };
    let Some(recent) = recent? else { return Ok(None) };
    let orders_n = stats.rows.first().and_then(|r| r.first()).and_then(crate::answerers::hits::cell_num).unwrap_or(0.0);
    let cust_n = stats.rows.first().and_then(|r| r.get(1)).and_then(crate::answerers::hits::cell_num).unwrap_or(0.0);
    let items = vec![
        Kpi { label: "员工".into(), value: serde_json::Value::from(ename.clone()), semantic: Semantic::None, delta: None },
        Kpi { label: period_label(cx.question, "订单数"), value: serde_json::Value::from(orders_n), semantic: Semantic::Count, delta: None },
        Kpi { label: period_label(cx.question, "关联客户数"), value: serde_json::Value::from(cust_n), semantic: Semantic::Count, delta: None },
    ];
    let drill = vec![format!("员工{ename}的订单明细"), format!("员工{ename}的客户有哪些")];
    Ok(Some(with_entity(build_card(
        &format!("{profile_sql}; 员工总览卡"),
        &ename,
        items,
        recent,
        drill,
        cx,
    ), entity_pairs(profile.as_ref()))))
}

/// 客户总览卡：客户主档 + 销售/订单/信控摘要 + 最近订单。
async fn customer_card(cx: &AskCtx<'_>, candidate: &Candidate) -> anyhow::Result<Option<AskResult>> {
    let code = candidate.code.clone();
    let cname = candidate.name.clone();
    let c = esc(&code);
    let employee_profile = if can_view_employee(cx) {
        (", COALESCE(e.actual_name,'') AS `大区经理`", "LEFT JOIN t_employee e ON e.employee_id = c.area_manager_id AND e.deleted_flag = 0")
    } else {
        ("", "")
    };
    let profile_sql = format!(
        "SELECT c.customer_code AS `客户编码`, c.customer_name AS `客户名称`, \
                COALESCE(c.customer_short_name,'') AS `客户简称`, \
                COALESCE(CASE c.customer_type WHEN 'Z001' THEN '一般销售客户' WHEN 'Z002' THEN '财务专用客户' \
                  WHEN 'Z003' THEN '关联方客户' WHEN 'Z004' THEN '货架店铺' WHEN 'Z005' THEN '客户终端仓' END, c.customer_type, '未分类') AS `客户类型`, \
                COALESCE(NULLIF(c.customer_level,''),'未设置') AS `客户等级`, \
                COALESCE(CASE c.customer_class WHEN '01' THEN '货架店铺' WHEN '02' THEN '新媒体店铺' \
                  WHEN '03' THEN '社团店铺' WHEN '04' THEN '线下客户' WHEN '05' THEN '内部客户' \
                  WHEN '06' THEN '其他财务专用' WHEN '99' THEN '外部客户的店铺' END, c.customer_class, '未分类') AS `客户分类`, \
                COALESCE(rp.region_name,c.province,'') AS `省份`, COALESCE(rc.region_name,c.city,'') AS `城市`{} , \
                COALESCE(c.contacts_name,'') AS `联系人`, \
                CASE c.is_enable WHEN 1 THEN '启用' WHEN 0 THEN '停用' ELSE COALESCE(c.customer_status,'未设置') END AS `主档状态` \
         FROM t_customer c \
         LEFT JOIN t_regions rp ON rp.region_code = c.province AND rp.deleted_flag = 0 \
         LEFT JOIN t_regions rc ON rc.region_code = c.city AND rc.deleted_flag = 0 \
         {} \
         WHERE c.deleted_flag = 0 AND c.customer_code = '{c}' LIMIT 1",
        employee_profile.0,
        employee_profile.1,
    );
    let otime = entity_time_suffix(cx.question, "o.order_time");
    let orders_sql = format!(
        "SELECT COUNT(DISTINCT o.sales_order_code) AS `订单数`, \
                COUNT(DISTINCT NULLIF(o.shop_code,'')) AS `关联门店数`, \
                COUNT(DISTINCT NULLIF(d.sku_code,'')) AS `购买商品数`, \
                COUNT(DISTINCT DATE_FORMAT(o.order_time,'%Y-%m')) AS `活跃月份数`, \
                MIN(o.order_time) AS `首次下单`, MAX(o.order_time) AS `最近下单` \
         FROM t_sales_order o \
         LEFT JOIN t_sales_order_detail d ON d.sales_order_code = o.sales_order_code \
           AND d.deleted_flag = 0 AND d.item_type = '1' \
         WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199') \
           AND o.customer_code = '{c}'{otime}"
    );
    let balance_sql = format!(
        "SELECT balance FROM (SELECT balance, ROW_NUMBER() OVER (ORDER BY created_time DESC, id DESC) AS rn \
         FROM t_customer_balance WHERE deleted_flag = 0 AND balance_status = '4' \
           AND balance_type = '1' AND customer_code = '{c}') t \
         WHERE rn = 1 LIMIT 1"
    );
    let recent_sql = if can_view_employee(cx) {
        format!(
            "SELECT o.sales_order_code AS `单号`, o.order_time AS `时间`, COALESCE(o.shop_name,'') AS `门店`, \
                    COALESCE(e.actual_name,o.owner_manager,'') AS `业务员`, o.total_amount AS `金额`, o.order_status AS `状态` \
             FROM t_sales_order o LEFT JOIN t_employee e ON e.employee_id = o.owner_manager AND e.deleted_flag = 0 \
             WHERE o.deleted_flag = 0 AND o.customer_code = '{c}'{otime} \
             ORDER BY order_time DESC LIMIT 5"
        )
    } else {
        format!(
            "SELECT o.sales_order_code AS `单号`, o.order_time AS `时间`, COALESCE(o.shop_name,'') AS `门店`, \
                    o.total_amount AS `金额`, o.order_status AS `状态` \
             FROM t_sales_order o WHERE o.deleted_flag = 0 AND o.customer_code = '{c}'{otime} \
             ORDER BY order_time DESC LIMIT 5"
        )
    };
    let sales_future = async {
        if !cx.source.is_warehouse() {
            return Ok(None);
        }
        let predicates = [SalesPredicate::eq(SalesDimension::CustomerCode, &code)];
        let Some(sql) = dws_entity_sql(
            cx.question,
            &[
                SalesMetric::SalesAmount,
                SalesMetric::SalesQuantity,
                SalesMetric::CostExcludingTax,
                SalesMetric::RevenueExcludingTax,
                SalesMetric::GrossProfit,
                SalesMetric::GrossMargin,
            ],
            &predicates,
        ) else {
            return Ok(None);
        };
        fetch_rows(
            cx,
            &sql,
        )
        .await
    };
    let goods_future = async {
        if !cx.source.is_warehouse() {
            return Ok(None);
        }
        let predicates = [SalesPredicate::eq(SalesDimension::CustomerCode, &code)];
        let Some(sql) = dws_relation_sql(
            cx.question,
            &[SalesMetric::SalesQuantity, SalesMetric::SalesAmount],
            &[SalesDimension::SkuCode, SalesDimension::Goods],
            &predicates,
            SalesSort::metric(SalesMetric::SalesAmount, SalesSortDirection::Desc),
        ) else {
            return Ok(None);
        };
        fetch_rows(cx, &sql).await
    };
    let (profile, sales, orders, balance, recent, goods) = futures::join!(
        fetch_rows(cx, &profile_sql),
        sales_future,
        fetch_rows(cx, &orders_sql),
        fetch_rows(cx, &balance_sql),
        fetch_rows(cx, &recent_sql),
        goods_future,
    );
    let Some(profile) = profile? else { return Ok(None) };
    if profile.rows.is_empty() {
        return Ok(None);
    }
    let sales = sales?;
    let orders = orders?;
    let balance = balance?;
    let recent = recent?.unwrap_or_default();
    let goods = goods?;
    let bal_val = balance.as_ref().map(num);

    let mut items = vec![];
    if let Some(sales) = sales.as_ref() {
        push_sales_kpis(
            &mut items,
            cx.question,
            sales,
            &[
                (0, "销售额（DWS经营口径）", Semantic::Money),
                (1, "销量（DWS经营口径）", Semantic::Count),
                (2, "不含税成本（DWS）", Semantic::Money),
                (3, "不含税收入（DWS）", Semantic::Money),
                (4, "毛利额（DWS）", Semantic::Money),
                (5, "毛利率（DWS）", Semantic::Percent),
            ],
        );
    }
    if let Some(orders) = orders.as_ref() {
        items.push(Kpi { label: period_label(cx.question, "订单数"), value: serde_json::Value::from(num(orders)), semantic: Semantic::Count, delta: None });
        items.push(Kpi { label: period_label(cx.question, "关联门店数"), value: serde_json::Value::from(num_at(orders, 1)), semantic: Semantic::Count, delta: None });
        items.push(Kpi { label: period_label(cx.question, "购买商品数"), value: serde_json::Value::from(num_at(orders, 2)), semantic: Semantic::Count, delta: None });
        items.push(Kpi { label: period_label(cx.question, "活跃月份数"), value: serde_json::Value::from(num_at(orders, 3)), semantic: Semantic::Count, delta: None });
    }
    if let Some(b) = bal_val {
        items.push(Kpi { label: "信控余额".into(), value: serde_json::Value::from(b), semantic: Semantic::Money, delta: None });
    }
    let drill = vec![
        format!("客户编码 {code} 的订单明细"),
        format!("客户编码 {code} 今年各月销售额"),
        format!("客户编码 {code} 还欠多少"),
    ];

    let mut pairs = entity_pairs(Some(&profile));
    if let Some(orders) = orders.as_ref() {
        pairs.extend(entity_pairs(Some(orders)).into_iter().skip(2));
    }
    let card = with_supplemental(build_card(
        &format!("{profile_sql}; 客户总览卡（经营指标来自 DWS，订单上下文独立）"),
        &cname,
        items,
        recent,
        drill,
        cx,
    ), goods);
    Ok(Some(with_entity(card, pairs)))
}

/// 商品总览卡：商品主档 + 下单/销售摘要 + 最近关联订单。
async fn goods_card(cx: &AskCtx<'_>, candidate: &Candidate) -> anyhow::Result<Option<AskResult>> {
    let code = candidate.code.clone();
    let gname = candidate.name.clone();
    let safe_code = esc(&code);
    let profile_sql = if cx.source.is_warehouse() {
        format!(
            "SELECT g.goods_code AS `商品编码`, g.goods_name AS `商品名称`, COALESCE(g.brand_name,'') AS `品牌`, \
                    COALESCE(g.goods_short_name,'') AS `商品简称`, \
                    COALESCE(NULLIF(TRIM(s.class2),''),NULLIF(g.goods_category_name,''),'未分类') AS `商品分类`, \
                    COALESCE(NULLIF(TRIM(s.classfinal),''),'') AS `末级分类`, COALESCE(NULLIF(TRIM(s.product_channel),''),'') AS `产品渠道`, \
                    COALESCE(NULLIF(TRIM(s.materialtype),''),'') AS `物料类型`, \
                    CASE g.on_sale WHEN 1 THEN '已上架' WHEN 0 THEN '未上架' ELSE '未设置' END AS `销售状态`, \
                    CASE g.frozen_state WHEN 1 THEN '已冻结' WHEN 0 THEN '正常' ELSE '未设置' END AS `冻结状态`, \
                    COALESCE(g.group_number,'') AS `存货组` \
             FROM t_goods g LEFT JOIN DW.dim_sku s ON s.sku_code = g.goods_code \
             WHERE g.deleted_flag = 0 AND g.goods_code = '{safe_code}' LIMIT 1"
        )
    } else {
        format!(
            "SELECT g.goods_code AS `商品编码`, g.goods_name AS `商品名称`, COALESCE(g.brand_name,'') AS `品牌`, \
                    COALESCE(g.goods_short_name,'') AS `商品简称`, COALESCE(NULLIF(g.goods_category_name,''),'未分类') AS `商品分类`, \
                    CASE g.on_sale WHEN 1 THEN '已上架' WHEN 0 THEN '未上架' ELSE '未设置' END AS `销售状态`, \
                    CASE g.frozen_state WHEN 1 THEN '已冻结' WHEN 0 THEN '正常' ELSE '未设置' END AS `冻结状态`, \
                    COALESCE(g.group_number,'') AS `存货组` \
             FROM t_goods g WHERE g.deleted_flag = 0 AND g.goods_code = '{safe_code}' LIMIT 1"
        )
    };
    let otime = entity_time_suffix(cx.question, "o.order_time");
    let stats_sql = format!(
        "SELECT COALESCE(SUM(x.box_quantity), 0) AS `订单下单数量`, \
                COUNT(DISTINCT o.sales_order_code) AS `关联订单数`, \
                COUNT(DISTINCT o.customer_code) AS `购买客户数`, \
                COALESCE(SUM(x.amount),0) AS `订单下单金额`, \
                COUNT(DISTINCT DATE_FORMAT(o.order_time,'%Y-%m')) AS `活跃月份数`, \
                MIN(o.order_time) AS `首次下单`, MAX(o.order_time) AS `最近下单` FROM (\
         SELECT DISTINCT d.sales_order_code, d.sku_code, d.box_quantity, d.amount \
         FROM t_sales_order_detail d \
         WHERE d.deleted_flag = 0 AND d.item_type = '1' AND d.sku_code = '{safe_code}') x \
         JOIN t_sales_order o ON o.sales_order_code = x.sales_order_code \
         WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199'){otime}"
    );
    let recent_sql = format!(
        "SELECT o.sales_order_code AS `单号`, o.order_time AS `时间`, o.customer_name AS `客户`, \
                d.box_quantity AS `数量`, d.amount AS `金额`, o.order_status AS `状态` \
         FROM t_sales_order_detail d \
         JOIN t_sales_order o ON o.sales_order_code = d.sales_order_code AND o.deleted_flag = 0 \
         WHERE d.deleted_flag = 0 AND d.item_type = '1' AND d.sku_code = '{safe_code}'{otime} \
         ORDER BY o.order_time DESC LIMIT 5"
    );
    let sales_future = async {
        if !cx.source.is_warehouse() {
            return Ok(None);
        }
        let predicates = [SalesPredicate::eq(SalesDimension::SkuCode, &code)];
        let Some(sql) = dws_entity_sql(
            cx.question,
            &[
                SalesMetric::SalesQuantity,
                SalesMetric::SalesAmount,
                SalesMetric::CostExcludingTax,
                SalesMetric::RevenueExcludingTax,
                SalesMetric::GrossProfit,
                SalesMetric::GrossMargin,
            ],
            &predicates,
        ) else {
            return Ok(None);
        };
        fetch_rows(
            cx,
            &sql,
        )
        .await
    };
    let customers_future = async {
        if !cx.source.is_warehouse() {
            return Ok(None);
        }
        let predicates = [SalesPredicate::eq(SalesDimension::SkuCode, &code)];
        let Some(sql) = dws_relation_sql(
            cx.question,
            &[SalesMetric::SalesQuantity, SalesMetric::SalesAmount],
            &[SalesDimension::CustomerCode, SalesDimension::Customer],
            &predicates,
            SalesSort::metric(SalesMetric::SalesAmount, SalesSortDirection::Desc),
        ) else {
            return Ok(None);
        };
        fetch_rows(cx, &sql).await
    };
    // 省区分布与客户分布同一共享事实合同、同一 `gate_on`，Rust 侧合并成一张标准化补充表
    let regions_future = async {
        if !cx.source.is_warehouse() {
            return Ok(None);
        }
        let predicates = [SalesPredicate::eq(SalesDimension::SkuCode, &code)];
        let Some(sql) = dws_relation_sql(
            cx.question,
            &[SalesMetric::SalesQuantity, SalesMetric::SalesAmount],
            &[SalesDimension::Region],
            &predicates,
            SalesSort::metric(SalesMetric::SalesAmount, SalesSortDirection::Desc),
        ) else {
            return Ok(None);
        };
        fetch_rows(cx, &sql).await
    };
    let (profile, stats, sales, recent, customers, regions) = futures::join!(
        fetch_rows(cx, &profile_sql),
        fetch_rows(cx, &stats_sql),
        sales_future,
        fetch_rows(cx, &recent_sql),
        customers_future,
        regions_future,
    );
    let Some(profile) = profile? else { return Ok(None) };
    if profile.rows.is_empty() {
        return Ok(None);
    }
    let stats = stats?;
    let sales = sales?;
    let recent = recent?.unwrap_or_default();
    let distribution = merge_distribution(customers?, regions?);
    let gbrand = profile.rows[0].get(2).and_then(value_text).unwrap_or_default().to_string();
    let gcat = profile.rows[0].get(4).and_then(value_text).unwrap_or("未分类").to_string();
    // 设备（物料类型=资产）用设备订单口径措辞：它们不在线下销售事实里，订单上下文就是全部
    let is_device = cx.source.is_warehouse()
        && profile.rows[0].get(7).and_then(value_text).is_some_and(|v| v.trim() == "资产");
    let (qty_label, amt_label) =
        if is_device { ("设备下单数量", "设备下单金额") } else { ("销售下单数量", "销售下单金额") };
    let drill = if is_device {
        vec![
            format!("商品编码 {code} 今年各月设备订单数"),
            format!("哪些客户订过商品编码 {code}"),
            format!("商品编码 {code} 的设备订单明细"),
        ]
    } else {
        vec![
            format!("商品编码 {code} 今年各月销量"),
            format!("买过商品编码 {code} 的客户有哪些"),
            format!("商品编码 {code} 的订单明细"),
        ]
    };
    let mut items = vec![
        Kpi { label: format!("{gcat} · {gbrand}"), value: serde_json::Value::from(gname.clone()), semantic: Semantic::Goods, delta: None },
    ];
    if let Some(stats) = stats.as_ref() {
        items.push(Kpi { label: period_label(cx.question, qty_label), value: serde_json::Value::from(num(stats)), semantic: Semantic::Count, delta: None });
        items.push(Kpi { label: period_label(cx.question, "关联订单数"), value: serde_json::Value::from(num_at(stats, 1)), semantic: Semantic::Count, delta: None });
        items.push(Kpi { label: period_label(cx.question, "购买客户数"), value: serde_json::Value::from(num_at(stats, 2)), semantic: Semantic::Count, delta: None });
        items.push(Kpi { label: period_label(cx.question, amt_label), value: serde_json::Value::from(num_at(stats, 3)), semantic: Semantic::Money, delta: None });
    }
    if let Some(sales) = sales.as_ref() {
        push_sales_kpis(
            &mut items,
            cx.question,
            sales,
            &[
                (0, "销量（DWS经营口径）", Semantic::Count),
                (1, "销售额（DWS经营口径）", Semantic::Money),
                (2, "不含税成本（DWS）", Semantic::Money),
                (3, "不含税收入（DWS）", Semantic::Money),
                (4, "毛利额（DWS）", Semantic::Money),
                (5, "毛利率（DWS）", Semantic::Percent),
            ],
        );
    }
    let mut pairs = entity_pairs(Some(&profile));
    if let Some(stats) = stats.as_ref() {
        pairs.extend(entity_pairs(Some(stats)).into_iter().skip(4));
    }
    let card = with_supplemental(build_card(
        &format!("{profile_sql}; 商品总览卡（经营指标与分布来自 DWS，订单上下文独立）"),
        &gname,
        items,
        recent,
        drill,
        cx,
    ), distribution);
    Ok(Some(with_entity(card, pairs)))
}

/// 客户分布与省区分布合并成一张标准化补充表：两个查询都由 `sales_fact` 生成、
/// 经过同一 `gate_on`，这里只做行拼接（客户行带编码，省区行编码留空）。
fn merge_distribution(customers: Option<RowSet>, regions: Option<RowSet>) -> Option<RowSet> {
    fn cell(row: &[serde_json::Value], index: usize) -> serde_json::Value {
        row.get(index).cloned().unwrap_or(serde_json::Value::Null)
    }
    let mut rows = Vec::new();
    if let Some(c) = customers {
        for row in &c.rows {
            rows.push(vec![
                serde_json::Value::from("客户"),
                cell(row, 0),
                cell(row, 1),
                cell(row, 2),
                cell(row, 3),
            ]);
        }
    }
    if let Some(r) = regions {
        for row in &r.rows {
            rows.push(vec![
                serde_json::Value::from("省区"),
                serde_json::Value::Null,
                cell(row, 0),
                cell(row, 1),
                cell(row, 2),
            ]);
        }
    }
    if rows.is_empty() {
        return None;
    }
    Some(RowSet {
        columns: vec!["分布维度".into(), "编码".into(), "名称".into(), "销量".into(), "销售额".into()],
        rows,
        redacted: vec![],
    })
}

fn with_entity(mut card: AskResult, pairs: Vec<(String, serde_json::Value)>) -> AskResult {
    if !pairs.is_empty() {
        card.view.blocks.insert(0, Block::Entity { pairs });
    }
    card
}

fn with_supplemental(mut card: AskResult, rows: Option<RowSet>) -> AskResult {
    let Some(rows) = rows.filter(|rows| !rows.rows.is_empty()) else {
        return card;
    };
    let row_count = rows.rows.len();
    let view = dms_semantic::present::build(&rows.columns, &rows.rows);
    card.supplemental = Some(crate::ctx::SupplementalResult {
        columns: rows.columns,
        rows: rows.rows,
        row_count,
        truncated: row_count >= 10,
        view,
    });
    card
}

/// 组装卡片（客户/商品共用）：KPI 块 + 最近订单表格 + 下钻 chips。
fn build_card(
    sql: &str,
    _title: &str,
    items: Vec<Kpi>,
    recent: RowSet,
    drill: Vec<String>,
    cx: &AskCtx<'_>,
) -> AskResult {
    let RowSet { columns, rows, redacted } = recent;
    let row_count = rows.len();
    AskResult {
        sql: sql.to_string(),
        columns: columns.clone(),
        rows,
        row_count,
        truncated: false,
        elapsed_ms: cx.t0.elapsed().as_millis(),
        route: "entity-card".into(),
        view: ViewSpec {
            columns: columns
                .iter()
                .map(|c| ColumnSpec {
                    name: c.clone(),
                    role: if c.contains("时间") || c.contains("日期") {
                        Role::Time
                    } else if c.contains('率') || c.contains("金额") || c.contains('额') || c.contains("数量") || c.contains("销量") {
                        Role::Metric
                    } else if c.contains("单号") {
                        Role::Id
                    } else {
                        Role::Category
                    },
                    semantic: if c.contains('率') {
                        Semantic::Percent
                    } else if c.contains("金额") || c.contains('额') {
                        Semantic::Money
                    } else if c.contains("数量") || c.contains("销量") {
                        Semantic::Count
                    } else if c.contains("单号") {
                        Semantic::Order
                    } else {
                        Semantic::None
                    },
                })
                .collect(),
            blocks: vec![Block::Kpis { items }, Block::Table],
            interact: Interact { drill },
            insight: None,
        },
        supplemental: None,
        comparisons: vec![],
        subs: vec![],
        caliber_note: None,
        truncation_note: None,
        redacted,
        scope_note: None,
        trust: None,
        steps: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_precedes_fuzzy_and_ambiguity_never_picks_first() {
        let src = include_str!("entity.rs");
        let body = src
            .split("async fn resolve_entity")
            .nth(1)
            .expect("resolve_entity missing")
            .split("async fn collect_candidates")
            .next()
            .unwrap();
        let exact = body.find("collect_candidates(cx, &kinds, parsed, true)").unwrap();
        let fuzzy = body.find("collect_candidates(cx, &kinds, parsed, false)").unwrap();
        assert!(exact < fuzzy, "必须先精确匹配再模糊匹配：{body}");
        assert!(body.contains("if exact.len() > 1"));
        assert!(body.contains("if fuzzy.len() > 1"));
        assert!(body.matches("candidate_card(cx, &parsed.value").count() >= 2);
    }

    /// 裸型号（无前缀）必须按商品型号解析：字母+数字+连字符的纯 ASCII 码（DHT150-6）。
    /// 窄判据的两侧同样钉住：日期段、纯字母连字符词都不是型号。
    #[test]
    fn bare_goods_model_is_recognized_without_a_prefix() {
        for q in ["DHT150-6", "查一下 DHT150-6", "DHT150-6 的资料"] {
            let parsed = parse_entity(q).unwrap_or_else(|| panic!("{q} 应被实体门接住"));
            assert_eq!(parsed.kind, Some(Kind::Goods), "{q}");
            assert_eq!(parsed.field, MatchField::Model, "{q}");
            assert_eq!(parsed.value, "DHT150-6", "{q}");
        }
        assert!(looks_like_goods_model("DHT150-6"));
        assert!(!looks_like_goods_model("2026-08"), "日期段不是型号");
        assert!(!looks_like_goods_model("ABC-DEF"), "纯字母连字符词不是型号");
        assert!(!looks_like_goods_model("线下-浏阳品元商贸有限公司"), "中文客户名不是型号");
    }

    /// 公司形态证据：渠道前缀（线下-/线上-）与公司类后缀命中即组织名，不可能是自然人。
    /// 反例同样钉住：人名、商品名、品类词都没有公司形态。
    #[test]
    fn company_form_evidence_marks_organizations_not_people() {
        for company in [
            "线下-云南食左食右食品有限公司",
            "线下-浏阳品元商贸有限公司",
            "南京苏宇食品有限公司",
            "线上-某旗舰店超市",
            "某养殖合作社",
        ] {
            assert!(looks_like_company(company), "{company} 应有公司形态证据");
        }
        for not in ["张三", "厚椰乳蛋挞液0400G00", "嗨肉", "可颂香肠卷", "烘焙类"] {
            assert!(!looks_like_company(not), "{not} 不该有公司形态证据");
        }
    }

    /// auto 模式下公司形态把员工/商品/品牌出局，只留组织类（客户/门店）——
    /// 员工表里躺着大量客户登录账号行（actual_name = 客户公司全名），不拦则客户被判成员工。
    /// 显式「员工」前缀是用户明确指示，形态证据无权覆盖。
    #[test]
    fn company_form_excludes_employee_in_auto_mode_only() {
        let all = vec![Kind::Customer, Kind::Goods, Kind::Brand, Kind::Shop, Kind::Employee];
        let company = parse_entity("线下-云南食左食右食品有限公司").unwrap();
        assert_eq!(narrow_kinds(all.clone(), &company), vec![Kind::Customer, Kind::Shop]);
        // 显式员工前缀：形态证据不生效
        let explicit = parse_entity("员工 线下-云南食左食右食品有限公司").unwrap();
        assert_eq!(narrow_kinds(vec![Kind::Employee], &explicit), vec![Kind::Employee]);
        // 人名不收窄：员工与客户继续并列候选
        let person = parse_entity("张三").unwrap();
        assert_eq!(narrow_kinds(all, &person).len(), 5);
    }

    /// 并列候选排序用显式类型优先级，不再是 label 的 UTF-8 字节序
    /// （字节序下「员工」U+5458 永远压在「客户」U+5BA2 头上 —— case2 的直接推手）。
    #[test]
    fn ambiguous_candidates_rank_customer_above_employee() {
        assert!(kind_priority(Kind::Customer) < kind_priority(Kind::Goods));
        assert!(kind_priority(Kind::Shop) < kind_priority(Kind::Employee));
        assert!(kind_priority(Kind::Employee) > kind_priority(Kind::Category));
        let src = include_str!("entity.rs");
        let body = src
            .split("async fn collect_candidates")
            .nth(1)
            .expect("collect_candidates missing")
            .split("async fn candidates_for")
            .next()
            .unwrap();
        assert!(body.contains("kind_priority"), "候选排序必须走显式类型优先级：{body}");
        assert!(!body.contains("label()"), "候选排序不得再按 label 字节序：{body}");
    }

    /// 商品形态证据：型号段（0400G00 / DHT150-6）与数量规格（450克 / 20袋）。
    /// `entity_form_hit` 是 triage 的确定性闸门：这两个真实 case 必须钉死在 Data 路。
    #[test]
    fn goods_spec_evidence_pins_bare_goods_names() {
        for goods in ["厚椰乳蛋挞液0400G00", "DHT150-6", "可颂香肠卷450g*20袋", "鲜肉肠400克"] {
            assert!(looks_like_goods_spec(goods), "{goods} 应有商品规格证据");
        }
        for not in ["高温补贴政策", "报销流程", "张三", "线下-云南食左食右食品有限公司"] {
            assert!(!looks_like_goods_spec(not), "{not} 不该有商品规格证据");
        }
        assert!(entity_form_hit("厚椰乳蛋挞液0400G00"), "裸商品名必须钉死 Data");
        assert!(entity_form_hit("线下-云南食左食右食品有限公司"), "裸客户名必须钉死 Data");
        // 无形态证据的裸词维持原判（LLM 分诊），不抢知识库
        assert!(!entity_form_hit("高温补贴政策"));
        assert!(!entity_form_hit("报销流程怎么办"), "制度类问法不归实体闸门");
    }

    /// 型号解析必须打商品主档的名称字段（实测：DHT150-6 只在 goods_name 尾部；
    /// goods_specification_name 是包装规格「450g*20袋」，不是设备型号）。
    #[test]
    fn goods_model_resolution_uses_the_goods_master_name() {
        let src = include_str!("entity.rs");
        let body = src
            .split("if kind == Kind::Goods && field == MatchField::Model")
            .nth(1)
            .expect("型号分支没了")
            .split("let condition = candidate_condition")
            .next()
            .unwrap();
        assert!(body.contains("g.goods_name LIKE"), "型号必须按商品主档名称解析：{body}");
        assert!(!body.contains(concat!("goods_specification", "_name")), "包装规格字段不是型号：{body}");
    }

    /// 商品卡的分布补充表：客户与省区两路都由共享 DWS 合同生成（同一 `gate_on`），
    /// Rust 侧合并成一张标准化表；数仓最近明细必须用共享明细构造器。
    #[test]
    fn goods_card_merges_customer_and_region_distribution_from_the_same_contract() {
        let src = include_str!("entity.rs");
        let body = src
            .split("async fn goods_card")
            .nth(1)
            .expect("goods_card 没了")
            .split("fn with_entity")
            .next()
            .unwrap();
        assert!(body.contains("SalesDimension::Region"), "商品卡缺省区分布：{body}");
        assert!(body.contains("merge_distribution"), "分布必须合并成标准化补充表");
        assert!(
            body.matches("dws_relation_sql").count() >= 2,
            "客户与省区两个分布查询都必须由 sales_fact 生成：{body}"
        );
    }

    #[test]
    fn longest_prefix_and_match_field_are_preserved() {        for (question, kind, field, value) in [
            ("客户名称 浏阳品元商贸有限公司", Kind::Customer, MatchField::Name, "浏阳品元商贸有限公司"),
            ("客户简称 品元商贸", Kind::Customer, MatchField::Alias, "品元商贸"),
            ("客户编码 C001", Kind::Customer, MatchField::Code, "C001"),
            ("商品名称 可颂香肠卷", Kind::Goods, MatchField::Name, "可颂香肠卷"),
            ("商品简称 可颂卷", Kind::Goods, MatchField::Alias, "可颂卷"),
            ("产品编码 SKU001", Kind::Goods, MatchField::Code, "SKU001"),
            ("产品名称 长才保温柜裸机", Kind::Goods, MatchField::Name, "长才保温柜裸机"),
            ("型号 DHT150-6", Kind::Goods, MatchField::Model, "DHT150-6"),
        ] {
            let parsed = parse_entity(question).unwrap();
            assert_eq!(parsed.kind, Some(kind), "{question}");
            assert_eq!(parsed.field, field, "{question}");
            assert_eq!(parsed.value, value, "{question}");
        }
    }

    #[test]
    fn unsupported_dws_entities_do_not_fabricate_sales_kpis() {
        let src = include_str!("entity.rs");
        for forbidden in [
            concat!("fn ship_", "net_sql"),
            concat!("fn ship_time_", "preds_for"),
            concat!("销售额（发货", "口径）"),
            concat!("UNION", " ALL"),
        ] {
            assert!(!src.contains(forbidden), "实体卡残留旧销售口径：{forbidden}");
        }
        let brand = src.split("async fn brand_card").nth(1).unwrap().split("async fn shop_card").next().unwrap();
        let shop = src.split("async fn shop_card").nth(1).unwrap().split("async fn employee_card").next().unwrap();
        let employee = src.split("async fn employee_card").nth(1).unwrap().split("async fn customer_card").next().unwrap();
        for body in [brand, shop, employee] {
            assert!(!body.contains("SalesMetric::"), "无验证维度的实体不得查询 DWS 销售指标");
        }
    }

    #[test]
    fn entity_parser_only_strips_edge_intent_and_accepts_long_names() {
        assert_eq!(bare_name("嗨肉").as_deref(), Some("嗨肉"));
        assert_eq!(bare_name("线下-浏阳品元商贸有限公司").as_deref(), Some("线下-浏阳品元商贸有限公司"));
        assert_eq!(bare_name("请查一下 客户名称 线下-浏阳销售单品有限公司 的详细信息").as_deref(), Some("线下-浏阳销售单品有限公司"));
        assert_eq!(bare_name("可颂香肠卷").as_deref(), Some("可颂香肠卷"));
        assert_eq!(bare_name("可颂香肠卷，本月").as_deref(), Some("可颂香肠卷"));
        assert_eq!(
            bare_name("线下-广东横琴雨燕供应链管理有限公司的下单信息").as_deref(),
            Some("线下-广东横琴雨燕供应链管理有限公司")
        );
        assert_eq!(
            bare_name("商品名称 可颂香肠卷的订单明细，本月").as_deref(),
            Some("可颂香肠卷")
        );
        assert_eq!(
            bare_name("长才保温柜裸机（鸣忙专用）DHT150-6，昨天").as_deref(),
            Some("长才保温柜裸机（鸣忙专用）DHT150-6")
        );
        let max = "甲".repeat(80);
        assert_eq!(bare_name(&max).as_deref(), Some(max.as_str()));
        assert!(bare_name(&"甲".repeat(81)).is_none());

        for q in [
            "本月销售额",
            "可颂香肠卷卖了多少",
            "买过烤肠的客户",
            "删除今天的订单",
            "查一下 HJXH-DXO2026072300384 这张单",
            "可颂香肠卷和热辣香骨鸡哪个卖得好",
        ] {
            assert!(bare_name(q).is_none(), "{q} 不该被裸名称门接住");
        }
        assert!(bare_name("恒众'%").is_none());
        assert!(looks_like_doc_code("HJXH-DXO2026072300384"));
        assert!(!looks_like_doc_code("DHT150-6"), "设备型号不能被单据码门拒绝");
    }

    #[test]
    fn analytical_questions_never_enter_entity_name_search() {
        for question in [
            "昨天下单的有哪些客户",
            "昨天有下单的那些客户",
            "昨天的设备订单",
            "本月销量最高的商品",
            "客户的订单明细",
            "商品的销售情况",
            "昨天下单客户的订单明细",
        ] {
            assert!(parse_entity(question).is_none(), "分析问句误入实体模糊查询：{question}");
        }
        assert_eq!(
            bare_name("客户名称 下单客户有限公司，本月").as_deref(),
            Some("下单客户有限公司")
        );
        assert_eq!(
            bare_name("客户名称 下单客户有限公司的销售表现").as_deref(),
            Some("下单客户有限公司")
        );
    }

    #[test]
    fn employee_directory_is_fail_closed_at_every_entry() {
        let src = include_str!("entity.rs");
        let auth = src.split("fn can_view_employee").nth(1).unwrap().split("fn candidate_kinds").next().unwrap();
        assert!(auth.contains("cx.p.administrator_flag || cx.p.role_code == \"admin\""));
        let resolve = src.split("async fn resolve_entity").nth(1).unwrap().split("async fn collect_candidates").next().unwrap();
        assert!(resolve.contains("parsed.kind == Some(Kind::Employee) && !can_view_employee(cx)"));
        let lookup = src.split("async fn candidates_for").nth(1).unwrap().split("fn candidate_condition").next().unwrap();
        assert!(lookup.contains("kind == Kind::Employee && !can_view_employee(cx)"));
        let card = src.split("async fn employee_card").nth(1).unwrap().split("async fn customer_card").next().unwrap();
        assert!(card.contains("if !can_view_employee(cx)"));
        assert!(!card.contains("SalesMetric::"));
    }

    #[test]
    fn customer_and_goods_cards_include_master_and_order_context() {
        let src = include_str!("entity.rs");
        for anchor in [
            "c.customer_code AS `客户编码`",
            "c.customer_class",
            "COUNT(DISTINCT o.sales_order_code) AS `订单数`",
            "COUNT(DISTINCT NULLIF(d.sku_code,'')) AS `购买商品数`",
            "COUNT(DISTINCT DATE_FORMAT(o.order_time,'%Y-%m')) AS `活跃月份数`",
            "MIN(o.order_time) AS `首次下单`",
            "balance_type = '1'",
            "Block::Entity { pairs }",
            "g.goods_code AS `商品编码`",
            "AS `商品分类`",
            "s.classfinal",
            "period_label(cx.question, \"关联订单数\")",
            "COUNT(DISTINCT o.customer_code) AS `购买客户数`",
            "pairs.extend(entity_pairs(Some(stats)).into_iter().skip(4))",
            "订单下单数量",
            "订单下单金额",
            "SalesMetric::CostExcludingTax",
            "SalesMetric::RevenueExcludingTax",
            "SalesMetric::GrossProfit",
            "SalesMetric::GrossMargin",
            "futures::join!",
        ] {
            assert!(src.contains(anchor), "实体详情缺关键上下文：{anchor}");
        }
    }

    #[test]
    fn entity_master_fields_keep_empty_labels_visible() {
        let rs = RowSet {
            columns: vec!["联系人".into(), "联系电话".into(), "首次下单".into(), "订单数".into()],
            rows: vec![vec![serde_json::Value::String(String::new()), serde_json::Value::Null, serde_json::Value::Null, serde_json::json!(0)]],
            redacted: vec!["联系电话".into()],
        };
        assert_eq!(
            entity_pairs(Some(&rs)),
            vec![
                ("联系人".into(), serde_json::json!("未维护")),
                ("联系电话".into(), serde_json::json!("因权限隐藏")),
                ("首次下单".into(), serde_json::json!("暂无")),
                ("订单数".into(), serde_json::json!(0)),
            ]
        );
    }

    #[test]
    fn entity_sales_kpis_use_shared_dws_contract() {
        let metrics = [
            SalesMetric::SalesQuantity,
            SalesMetric::SalesAmount,
            SalesMetric::CostExcludingTax,
            SalesMetric::RevenueExcludingTax,
            SalesMetric::GrossProfit,
            SalesMetric::GrossMargin,
        ];
        let customer = dws_entity_sql(
            "本月某客户",
            &metrics,
            &[SalesPredicate::eq(SalesDimension::CustomerCode, "C001")],
        )
        .unwrap();
        assert!(customer.contains(sales_fact::TABLE), "{customer}");
        assert!(customer.contains("sf.storecode"), "{customer}");
        assert!(customer.contains("SUM(sf.amount)"), "{customer}");
        assert!(customer.contains("SUM(sf.cost_excluding_tax)"), "{customer}");
        assert!(customer.contains("SUM(sf.revenue_excluding_tax)"), "{customer}");
        assert!(customer.contains("SUM(sf.gross_profit)"), "{customer}");
        assert!(customer.contains(
            "SUM(sf.gross_profit)/NULLIF(SUM(sf.revenue_excluding_tax),0)"
        ), "毛利率必须先聚合再相除：{customer}");
        assert!(customer.contains("sf.order_date"), "{customer}");
        assert!(customer.contains("sf.order_date < DATE_ADD(CURDATE(), INTERVAL 1 DAY)"), "{customer}");

        let goods = dws_entity_sql(
            "可颂香肠卷",
            &metrics,
            &[SalesPredicate::eq(SalesDimension::SkuCode, "SKU001")],
        )
        .unwrap();
        assert!(goods.contains("sf.skucode"), "{goods}");
        assert!(goods.contains("SUM(sf.qty)") && goods.contains("SUM(sf.amount)"), "{goods}");
        assert!(!customer.contains(" JOIN ") && !goods.contains(" JOIN "),
            "默认销售经营指标只能查询共享 DWS 单表\ncustomer={customer}\ngoods={goods}");
        for forbidden in [
            "sf.state",
            "sf.class2",
            "sf.brand",
            "sf.channel",
            "sf.shop",
            "sf.employee",
            "COUNT(*)",
        ] {
            assert!(!customer.contains(forbidden), "客户销售事实出现未确认字段：{forbidden}\n{customer}");
            assert!(!goods.contains(forbidden), "商品销售事实出现未确认字段：{forbidden}\n{goods}");
        }
        assert!(!customer.contains("AS `门店`"), "storename/storecode 表示客户，不是门店：{customer}");
    }

    #[test]
    fn entity_relationship_tables_use_the_same_dws_contract_and_permissions_dimension() {
        let customer_goods = dws_relation_sql(
            "本月某客户",
            &[SalesMetric::SalesQuantity, SalesMetric::SalesAmount],
            &[SalesDimension::SkuCode, SalesDimension::Goods],
            &[SalesPredicate::eq(SalesDimension::CustomerCode, "C001")],
            SalesSort::metric(SalesMetric::SalesAmount, SalesSortDirection::Desc),
        )
        .unwrap();
        assert!(customer_goods.contains(sales_fact::TABLE), "{customer_goods}");
        assert!(customer_goods.contains("sf.storecode"), "{customer_goods}");
        assert!(customer_goods.contains("sf.skucode") && customer_goods.contains("sf.skuname"), "{customer_goods}");
        assert!(customer_goods.contains("SUM(sf.qty)") && customer_goods.contains("SUM(sf.amount)"), "{customer_goods}");
        assert!(customer_goods.contains("ORDER BY SUM(sf.amount) DESC LIMIT 10"), "{customer_goods}");
        assert!(!customer_goods.contains(" JOIN "), "关联商品只能走共享 DWS 单表：{customer_goods}");
        for retired in [concat!("UNION", " ALL"), "t_sales_order", "t_sales_order_detail", "t_order_logistics"] {
            assert!(!customer_goods.contains(retired), "关联商品不得回退旧发货/订单事实 {retired}：{customer_goods}");
        }

        let goods_customers = dws_relation_sql(
            "可颂香肠卷",
            &[SalesMetric::SalesQuantity, SalesMetric::SalesAmount],
            &[SalesDimension::CustomerCode, SalesDimension::Customer],
            &[SalesPredicate::eq(SalesDimension::SkuCode, "SKU001")],
            SalesSort::metric(SalesMetric::SalesAmount, SalesSortDirection::Desc),
        )
        .unwrap();
        assert!(goods_customers.contains("sf.skucode"), "{goods_customers}");
        assert!(goods_customers.contains("sf.storecode") && goods_customers.contains("sf.storename"), "{goods_customers}");
        assert!(goods_customers.contains("AS `客户编码`") && goods_customers.contains("AS `客户`"), "{goods_customers}");
        assert!(!goods_customers.contains("AS `门店`"), "storename/storecode 表示客户，不是门店：{goods_customers}");
        assert!(!goods_customers.contains(" JOIN "), "关联客户只能走共享 DWS 单表：{goods_customers}");
    }
}
