//! 呈现层的**模型编排**：块的种类、顺序、标题由模型按真实数据定，数值一律由本模块算。
//!
//! ## 它解决什么
//!
//! `dms_semantic::present::build` 是一棵确定性决策树，「首条命中即止」，一次只出**一种**块：
//! 单行全指标 → KPI 卡；单行多列 → 实体卡；多类别或有 ID 列 → **裸表格**。
//! 于是业主输一个单号拿到 6 行订单明细时，页面就是一张裸表 —— 没有合计、没有构成图，
//! 而深度页那边又拿固定模板硬套出一个「头部贡献与集中度」，按 `实收数量=0` 排名、占比 0%。
//! 两头都不是「结合这份数据该怎么讲」。
//!
//! 这里补的正是那一层：**模型看真实列与样本行，决定这份数据该以什么形态呈现**。
//!
//! ## 铁律：模型选列，代码算数
//!
//! 模型只允许输出**列下标 + 聚合算子 + 标题**。所有数字由 [`aggregate`] 在 Rust 里从
//! `rows` 现算 —— 模型没有任何通道把一个数写进结果。这条不是风格，是这一层能存在的前提：
//! 呈现层一旦能凭空产出数字，整套「数字必须可核验」的纪律就从背面被绕开了。
//!
//! 标题同理禁数字（[`safe_title`]）：一个带数字的标题就是一句未经核验的断言。
//!
//! ## 降级
//!
//! 模型挂了 / 回空 / JSON 不合约 / 下标越界 / 一个块都没验过 → **原样保留确定性视图**。
//! 这一层是加分项，不是必需品；它自己失败不许把一次成功取数变成失败。

use std::sync::Arc;

use dms_kernel::present::{Block, ChartKind, Kpi, Role, Semantic, ViewSpec};
use dms_kernel::{ChatModel, ModelTier};
use serde_json::Value;

use crate::ctx::{AskCtx, AskResult};

/// 送进 prompt 的样本行数上限。列多行少的形态足够模型判断，再多只是烧 token。
const SAMPLE_ROWS: usize = 6;
/// 模型最多编排几个块。超出即截断 —— 一屏塞不下更多，且块越多越容易凑数。
const MAX_BLOCKS: usize = 4;
/// 标题字数上限（汉字计）。长标题在卡片头会换行挤掉内容。
const TITLE_MAX_CHARS: usize = 16;
/// 触发编排的最小行数：单行结果确定性树已经出 KPI/实体卡，没有可编排的余地。
const MIN_ROWS: usize = 2;

const SYSTEM: &str = "\
你是 BI 呈现编排器。给你一次查询的列定义与样本行，你决定这份数据该以什么形态展示。

只输出 JSON，不要解释、不要围栏：
{\"blocks\":[...]}

blocks 每项是下列之一（最多 4 个，按展示顺序）：
  {\"type\":\"stat\",\"col\":<列下标>,\"agg\":\"sum|avg|max|min|count|distinct\",\"label\":\"<标题>\"}
  {\"type\":\"chart\",\"kind\":\"bar|line|pie\",\"x\":<类别或时间列下标>,\"y\":[<数值列下标>],\"title\":\"<标题>\"}
  {\"type\":\"table\"}

硬规则：
1. col/x/y 必须是给定列下标；y 只能是数值列；x 不能同时出现在 y 里。
2. **不要写任何数字进 label/title** —— 数值一律由系统从原始数据计算，你写的会被丢弃。
3. 只选真正有信息量的：类别列取值全同、数值列全为 0 的，不要拿来画图或做统计。
4. line 只用于时间列；pie 只在类别数 ≤ 6 且数值全为正时用；其余用 bar。
5. 逐行明细表由系统固定保底，你不必也不能把它去掉。";

/// 出口钩子：在 `localize` 之后调用（列名与码值都已中文化，模型看到的就是用户看到的）。
///
/// 失败一律静默保留原视图。
pub(crate) async fn refine(cx: &AskCtx<'_>, r: &mut AskResult) {
    if !worth_composing(r) {
        return;
    }
    // 模型那半失败也要有确定性摘要：合计与行数是代码算得出的确定事实，不该跟着一起没有。
    let composed = match compose(cx.llm, cx.question, &r.columns, &r.rows, &r.view).await {
        Some(blocks) => blocks,
        None => deterministic_summary(&r.view, &r.rows),
    };
    if composed.is_empty() {
        return;
    }
    // 单据/实体头卡**原样留在最前**：它是「这是哪张单」的身份，不是可编排的展示形态。
    // 编排只接管它后面那一段（此前那段就是一张裸表格）。
    let header: Vec<Block> = std::mem::take(&mut r.view.blocks)
        .into_iter()
        .take_while(|block| matches!(block, Block::Entity { .. }))
        .collect();
    r.view.blocks = header.into_iter().chain(composed).collect();
}

/// 值不值得多烧一次 fast 调用。
///
/// 判据只看**结果形状**，不看路由：确定性快路与 LLM SQL 出来的表格是一样的表格。
fn worth_composing(r: &AskResult) -> bool {
    if r.rows.len() < MIN_ROWS || r.columns.len() < 2 || !r.subs.is_empty() {
        return false;
    }
    // 🔴 结果被截断时不编排：`aggregate` 只看回传的这几行，算出来的「合计」是**小计**，
    // 而卡上写着「合计」。宁可少一张 KPI 卡，不给一个悄悄少算的数（2026-08-14 自审）。
    if r.truncated {
        return false;
    }
    // 反问卡/出界卡没有数据可编排（它们的 blocks 是文案载体）
    if r.sql.trim().is_empty() {
        return false;
    }
    // 确定性树已经给出图表时不抢：那些形态（趋势线、单类别柱/饼）它判得比模型稳。
    // 只在它**退成裸表格**时接手 —— 那正是「规则没意见」的那一档。
    //
    // 🔴 「实体卡 + 裸表格」同样算这一档（2026-08-14 生产实测补）：业主抱怨的那张
    // 单号卡就是这个形状 —— 头卡下面挂 6 行订单明细，一张裸表，没有合计也没有构成图。
    // 第一版只认 `[Table]`，恰好把要治的那张卡漏在门外。
    let tail: Vec<&Block> = r
        .view
        .blocks
        .iter()
        .skip_while(|block| matches!(block, Block::Entity { .. }))
        .collect();
    matches!(tail.as_slice(), [Block::Table])
}

async fn compose(
    llm: &Arc<dyn ChatModel>,
    question: &str,
    columns: &[String],
    rows: &[Vec<Value>],
    view: &ViewSpec,
) -> Option<Vec<Block>> {
    let user = format!(
        "原问题：{question}\n\n列定义（下标 | 列名 | 角色 | 语义）：\n{}\n\n样本行（共 {} 行，此处只列前 {}）：\n{}\n\n请编排：",
        columns_brief(view),
        rows.len(),
        rows.len().min(SAMPLE_ROWS),
        rows_brief(columns, rows),
    );
    let reply = crate::insight::guarded(&**llm, SYSTEM, &user, "呈现编排", ModelTier::Fast).await?;
    let plan: Plan = serde_json::from_str(strip_fence(&reply))
        .map_err(|e| tracing::warn!(err = %e, reply = %clip(&reply), "呈现编排 JSON 不合约 → 保留确定性视图"))
        .ok()?;
    let blocks = build_blocks(&plan, view, rows);
    if blocks.is_empty() {
        // 🔴 一个**不留痕**的层没法诊断（生产实测：编排跑了、什么都没出、日志一个字没有）。
        tracing::info!(reply = %clip(&reply), "呈现编排未产出可用块 → 走确定性兜底");
    }
    (!blocks.is_empty()).then_some(blocks)
}

/// 零模型的确定性摘要：明细表至少要有「合计金额 / 记录数」。
///
/// 🔴 为什么不全交给模型：这一层的价值有两半 —— **选什么图、起什么标题**只有模型判得出，
/// 而「这张明细表的金额合计是多少」是代码算得出的确定事实。模型那半失败（超时、
/// JSON 不合约、选的块全被判据拒掉）时，不该连这半也一起没有。
/// 业主抱怨的那张单据卡就是这一档：6 行订单明细，一张裸表，没有合计。
fn deterministic_summary(view: &ViewSpec, rows: &[Vec<Value>]) -> Vec<Block> {
    let mut items: Vec<Kpi> = Vec::new();
    for (index, spec) in view.columns.iter().enumerate() {
        if items.len() >= MAX_STATS || spec.role != Role::Metric {
            continue;
        }
        // 只对**金额**求和：数量类列相加常常没有业务含义（箱数 + 袋数），
        // 占比类相加恒等于 100%（`aggregate` 已经拒了）。
        if spec.semantic != Semantic::Money {
            continue;
        }
        let Some((value, semantic)) = aggregate(rows, index, "sum", spec.semantic) else { continue };
        let Some(label) = default_stat_label("sum", &spec.name) else { continue };
        if items.iter().any(|k| k.label == label) {
            continue;
        }
        items.push(Kpi { label, value, semantic, delta: None });
    }
    if items.is_empty() {
        return vec![];
    }
    items.push(Kpi {
        label: "记录数".into(),
        value: Value::from(rows.len()),
        semantic: Semantic::Count,
        delta: None,
    });
    vec![Block::Kpis { items }, Block::Table]
}

/// 确定性摘要最多几张 KPI（再多一屏塞不下，且金额列本来就不该有很多）。
const MAX_STATS: usize = 3;

#[derive(serde::Deserialize)]
struct Plan {
    #[serde(default)]
    blocks: Vec<PlanBlock>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum PlanBlock {
    Stat { col: usize, #[serde(default)] agg: String, #[serde(default)] label: String },
    Chart {
        #[serde(default)] kind: String,
        x: usize,
        #[serde(default)] y: Vec<usize>,
        #[serde(default)] title: String,
    },
    Table,
    /// 模型偶尔会发明新块名。整份合同不因此作废，认不出的那一项丢掉即可。
    #[serde(other)]
    Unknown,
}

/// 逐块校验 + 落成真实 `Block`。任何一块不合规**只丢那一块**，不废整份编排。
fn build_blocks(plan: &Plan, view: &ViewSpec, rows: &[Vec<Value>]) -> Vec<Block> {
    let n = view.columns.len();
    let mut out: Vec<Block> = Vec::new();
    let mut stats: Vec<Kpi> = Vec::new();
    let mut has_table = false;
    for item in plan.blocks.iter().take(MAX_BLOCKS * 2) {
        match item {
            PlanBlock::Stat { col, agg, label } => {
                let Some(spec) = view.columns.get(*col) else { continue };
                if spec.role != Role::Metric && agg != "count" && agg != "distinct" {
                    continue; // 非数值列只允许计数类聚合
                }
                let Some((value, semantic)) = aggregate(rows, *col, agg, spec.semantic) else {
                    continue;
                };
                // 🔴 计数类的标题**只由代码拼**：`count` 数的是**行数**，而模型常写成
                // 「客户数」——一行一个客户时它对，一个客户多行时它就是个错数，
                // 且错在标题上（数字本身没错），比错数更难发现。
                let label = match agg.as_str() {
                    "count" | "distinct" => default_stat_label(agg, &spec.name),
                    _ => safe_title(label).or_else(|| default_stat_label(agg, &spec.name)),
                };
                let Some(label) = label else { continue };
                if stats.iter().any(|k| k.label == label) {
                    continue;
                }
                stats.push(Kpi { label, value, semantic, delta: None });
            }
            PlanBlock::Chart { kind, x, y, title } => {
                if *x >= n {
                    continue;
                }
                let y: Vec<usize> = y
                    .iter()
                    .copied()
                    .filter(|i| *i < n && *i != *x && view.columns[*i].role == Role::Metric)
                    .collect();
                if y.is_empty() {
                    continue;
                }
                let kind = match kind.as_str() {
                    "line" if view.columns[*x].role == Role::Time => ChartKind::Line,
                    // 时间轴之外一律不画折线：类别轴上的折线把无序的类别连成趋势，是错图
                    "line" => ChartKind::Bar,
                    "pie" if pie_ok(rows, *x, y[0]) => ChartKind::Pie,
                    "pie" => ChartKind::Bar,
                    _ => ChartKind::Bar,
                };
                out.push(Block::Chart {
                    kind,
                    x: *x,
                    y,
                    // 收纳阈值与确定性决策树**共用一份**（`present::bar_top`）：
                    // 「类别轴挤爆看不清」在两处是同一件事，各写一个数字就是两个阈值。
                    top: dms_semantic::present::bar_top(rows.len()),
                    series: None,
                    title: safe_title(title),
                });
            }
            PlanBlock::Table => has_table = true,
            PlanBlock::Unknown => {}
        }
    }
    if !stats.is_empty() {
        out.insert(0, Block::Kpis { items: stats });
    }
    // 表格是**保底**：本层只在多行结果上跑，图和 KPI 都是概览，逐行核对的能力不许被换走。
    // （模型给没给 `table` 都一样 —— 它忘了写不该由用户承担。）
    let _ = has_table;
    out.truncate(MAX_BLOCKS.saturating_sub(1));
    out.push(Block::Table);
    // 只剩一张裸表 = 与确定性视图等价，不值得替换（也就不必让调用方多改一次 view）
    if matches!(out.as_slice(), [Block::Table]) {
        return vec![];
    }
    out
}

/// 饼图只在类别少且数值全正时成立（负值切不出扇形，类别多了看不清）。
fn pie_ok(rows: &[Vec<Value>], x: usize, y: usize) -> bool {
    if rows.len() > 6 {
        return false;
    }
    let mut seen: Vec<String> = Vec::new();
    for row in rows {
        match number(row.get(y)) {
            Some(v) if v > 0.0 => {}
            _ => return false,
        }
        let key = cell_text(row.get(x));
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    seen.len() == rows.len()
}

/// 聚合**在这里算**，模型只给算子名。返回值与展示语义一并给出。
fn aggregate(rows: &[Vec<Value>], col: usize, agg: &str, semantic: Semantic) -> Option<(Value, Semantic)> {
    let nums = || rows.iter().filter_map(|row| number(row.get(col)));
    match agg {
        "count" => Some((Value::from(rows.len()), Semantic::Count)),
        "distinct" => {
            let mut seen: Vec<String> = Vec::new();
            for row in rows {
                let key = cell_text(row.get(col));
                if !key.is_empty() && !seen.contains(&key) {
                    seen.push(key);
                }
            }
            Some((Value::from(seen.len()), Semantic::Count))
        }
        "sum" | "avg" | "max" | "min" => {
            let values: Vec<f64> = nums().collect();
            if values.is_empty() {
                return None;
            }
            let v = match agg {
                "sum" => values.iter().sum::<f64>(),
                "avg" => values.iter().sum::<f64>() / values.len() as f64,
                "max" => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                _ => values.iter().copied().fold(f64::INFINITY, f64::min),
            };
            // 百分比列求和没有业务含义（占比之和恒 100%），拒掉而不是算一个假指标
            if agg == "sum" && semantic == Semantic::Percent {
                return None;
            }
            Some((serde_json::json!(round2(v)), semantic))
        }
        _ => None,
    }
}

/// 金额保留两位：`f64` 求和会拖出 `930.0000000000001` 这种尾巴，上屏就是一个错数的观感。
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 模型没给标题时的确定性兜底（不许因为标题缺席就丢掉一个有效统计）。
fn default_stat_label(agg: &str, column: &str) -> Option<String> {
    let prefix = match agg {
        "sum" => "合计",
        "avg" => "平均",
        "max" => "最大",
        "min" => "最小",
        "count" => return Some("记录数".to_string()),
        "distinct" => return Some(format!("{column}数")),
        _ => return None,
    };
    Some(format!("{prefix}{column}"))
}

/// 标题清洗：**含数字即拒**（一个带数字的标题就是一句未经核验的断言），去围栏字符，限长。
fn safe_title(raw: &str) -> Option<String> {
    let t = raw.trim().replace(['`', '|', '\n'], "");
    // 中文数字与全角数字同样是数字：只拒 ASCII 的话，模型写「合计一万二千元」照样进标题
    const CN_DIGITS: &str = "零〇一二两三四五六七八九十百千万亿０１２３４５６７８９";
    if t.is_empty() || t.chars().any(|c| c.is_ascii_digit() || CN_DIGITS.contains(c)) {
        return None;
    }
    let t: String = t.chars().take(TITLE_MAX_CHARS).collect();
    (!t.trim().is_empty()).then(|| t.trim().to_string())
}

fn columns_brief(view: &ViewSpec) -> String {
    view.columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let role = match c.role {
                Role::Metric => "数值",
                Role::Category => "类别",
                Role::Time => "时间",
                Role::Id => "编号",
            };
            format!("{i} | {} | {role} | {:?}", c.name, c.semantic)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rows_brief(columns: &[String], rows: &[Vec<Value>]) -> String {
    let head = columns.join(" | ");
    let body = rows
        .iter()
        .take(SAMPLE_ROWS)
        .map(|row| {
            row.iter()
                .map(|v| clip_cell(&cell_text(Some(v))))
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{head}\n{body}")
}

fn clip_cell(s: &str) -> String {
    let s = s.replace(['\n', '\r'], " ");
    if s.chars().count() > 24 {
        s.chars().take(24).collect::<String>() + "…"
    } else {
        s
    }
}

fn cell_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// DECIMAL 走字符串保精度（`ctx` 的既有口径），所以数字要从两种形态取。
fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', "").parse().ok(),
        _ => None,
    }
}

fn strip_fence(s: &str) -> &str {
    let t = s.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t).trim()
}

fn clip(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dms_kernel::present::{ColumnSpec, Interact};

    fn view(cols: &[(&str, Role, Semantic)]) -> ViewSpec {
        ViewSpec {
            columns: cols
                .iter()
                .map(|(n, role, semantic)| ColumnSpec { name: (*n).into(), role: *role, semantic: *semantic })
                .collect(),
            blocks: vec![Block::Table],
            interact: Interact::default(),
            insight: None,
        }
    }

    fn plan(json: &str) -> Plan {
        serde_json::from_str(json).expect("测试 JSON")
    }

    /// 🔴 这一层能存在的前提：**模型不许写数字**。
    /// 标题里带数字 = 一句未经核验的断言，直接拒掉标题（不是拒掉整个块）。
    #[test]
    fn model_cannot_write_a_number_anywhere() {
        assert_eq!(safe_title("各商品金额构成"), Some("各商品金额构成".into()));
        assert_eq!(safe_title("合计 930 元"), None, "带数字的标题必须拒");
        assert_eq!(safe_title("TOP5 商品"), None);
        assert_eq!(safe_title("   "), None);
        // 兜底标题由代码拼，永远不含数字
        assert_eq!(default_stat_label("sum", "明细金额").as_deref(), Some("合计明细金额"));
        assert!(!default_stat_label("distinct", "商品名称").unwrap().chars().any(|c| c.is_ascii_digit()));
    }

    /// 数值一律由 Rust 从原始行算出来。
    #[test]
    fn aggregates_are_computed_in_rust_not_taken_from_the_model() {
        let rows = vec![
            vec![Value::from("A"), Value::from("100.50")],
            vec![Value::from("B"), Value::from("200.25")],
            vec![Value::from("A"), Value::Null],
        ];
        assert_eq!(aggregate(&rows, 1, "sum", Semantic::Money), Some((serde_json::json!(300.75), Semantic::Money)));
        assert_eq!(aggregate(&rows, 1, "max", Semantic::Money), Some((serde_json::json!(200.25), Semantic::Money)));
        assert_eq!(aggregate(&rows, 0, "count", Semantic::None), Some((Value::from(3), Semantic::Count)));
        assert_eq!(aggregate(&rows, 0, "distinct", Semantic::None), Some((Value::from(2), Semantic::Count)));
        // 占比列求和恒 100%，是个假指标 —— 拒掉而不是算出来
        assert_eq!(aggregate(&rows, 1, "sum", Semantic::Percent), None);
        // 浮点尾巴不许上屏
        let cents = vec![vec![Value::from(0.1)], vec![Value::from(0.2)]];
        assert_eq!(aggregate(&cents, 0, "sum", Semantic::Money), Some((serde_json::json!(0.3), Semantic::Money)));
    }

    /// 越界下标、把类别列当数值、在类别轴上画折线 —— 逐块拒，不废整份编排。
    #[test]
    fn invalid_blocks_are_dropped_one_by_one_not_wholesale() {
        let v = view(&[
            ("商品名称", Role::Category, Semantic::Goods),
            ("明细金额", Role::Metric, Semantic::Money),
        ]);
        let rows = vec![
            vec![Value::from("烤肠"), Value::from(10.0)],
            vec![Value::from("烧麦"), Value::from(20.0)],
        ];
        let p = plan(
            r#"{"blocks":[
                {"type":"chart","kind":"bar","x":9,"y":[1],"title":"越界"},
                {"type":"stat","col":0,"agg":"sum","label":"类别列求和"},
                {"type":"chart","kind":"line","x":0,"y":[1],"title":"各商品金额"},
                {"type":"stat","col":1,"agg":"sum","label":"合计金额"},
                {"type":"newfangled"}
            ]}"#,
        );
        let blocks = build_blocks(&p, &v, &rows);
        // KPI 在最前；越界图与类别列求和都被丢掉；类别轴上的 line 降级成 bar
        match &blocks[0] {
            Block::Kpis { items } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].label, "合计金额");
                assert_eq!(items[0].value, serde_json::json!(30.0));
            }
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(&blocks[1], Block::Chart { kind: ChartKind::Bar, x: 0, .. }),
            "类别轴上的折线是错图，必须降级成柱：{blocks:?}"
        );
        assert!(matches!(blocks.last(), Some(Block::Table)), "多行结果必须保底留表：{blocks:?}");
    }

    /// 只剩一张裸表 = 与确定性视图等价，不替换（省一次无意义的 view 改写）。
    #[test]
    fn a_bare_table_plan_is_not_worth_replacing_the_deterministic_view() {
        let v = view(&[("单号", Role::Id, Semantic::Order), ("金额", Role::Metric, Semantic::Money)]);
        let rows = vec![vec![Value::from("A"), Value::from(1.0)]];
        assert!(build_blocks(&plan(r#"{"blocks":[{"type":"table"}]}"#), &v, &rows).is_empty());
    }

    /// 饼图的两条硬约束：类别 ≤6 且互不重复、数值全正。
    #[test]
    fn pie_needs_few_positive_and_distinct_categories() {
        let ok = vec![vec![Value::from("A"), Value::from(1.0)], vec![Value::from("B"), Value::from(2.0)]];
        assert!(pie_ok(&ok, 0, 1));
        let negative = vec![vec![Value::from("A"), Value::from(-1.0)], vec![Value::from("B"), Value::from(2.0)]];
        assert!(!pie_ok(&negative, 0, 1));
        let dup = vec![vec![Value::from("A"), Value::from(1.0)], vec![Value::from("A"), Value::from(2.0)]];
        assert!(!pie_ok(&dup, 0, 1), "同一类别两行画饼是错图");
    }

    /// 只在确定性树**退成裸表格**时接手；它已经出图/出卡的形态不抢。
    #[test]
    fn only_takes_over_when_the_rules_had_no_opinion() {
        let mut r = crate::ctx::AskResult {
            sql: "SELECT 商品, 金额 FROM t".into(),
            columns: vec!["商品".into(), "金额".into()],
            rows: vec![
                vec![Value::from("A"), Value::from(1.0)],
                vec![Value::from("B"), Value::from(2.0)],
            ],
            row_count: 2,
            view: view(&[("商品", Role::Category, Semantic::Goods), ("金额", Role::Metric, Semantic::Money)]),
            ..empty_result()
        };
        assert!(worth_composing(&r));
        r.view.blocks =
            vec![Block::Chart { kind: ChartKind::Bar, x: 0, y: vec![1], top: None, series: None, title: None }];
        assert!(!worth_composing(&r), "确定性树已经出图就不抢");
        // 🔴 单据/实体卡（头卡 + 裸表格）同样接手：业主抱怨的那张单号卡就是这个形状。
        r.view.blocks = vec![
            Block::Entity { pairs: vec![("单据类型".into(), Value::from("销售订单"))] },
            Block::Table,
        ];
        assert!(worth_composing(&r), "头卡 + 裸表格必须接手（那正是要治的那张卡）");
        // 头卡后面已经有图了就不抢
        r.view.blocks = vec![
            Block::Entity { pairs: vec![("单据类型".into(), Value::from("销售订单"))] },
            Block::Chart { kind: ChartKind::Bar, x: 0, y: vec![1], top: None, series: None, title: None },
            Block::Table,
        ];
        assert!(!worth_composing(&r), "确定性树已经出图就不抢");
        r.view.blocks = vec![Block::Table];
        r.rows.truncate(1);
        assert!(!worth_composing(&r), "单行结果没有可编排的余地");
    }

    /// 模型那半失败时仍要有确定性摘要：合计与行数是代码算得出的确定事实。
    ///
    /// 生产实测（2026-08-14）：单据卡的编排跑了、什么都没出、日志一个字没有 ——
    /// 用户看到的还是那张没有合计的裸表。
    #[test]
    fn deterministic_summary_survives_a_useless_model() {
        let v = view(&[
            ("商品名称", Role::Category, Semantic::Goods),
            ("数量", Role::Metric, Semantic::Count),
            ("明细金额", Role::Metric, Semantic::Money),
        ]);
        let rows = vec![
            vec![Value::from("烤肠"), Value::from(2.0), Value::from("100.50")],
            vec![Value::from("烧麦"), Value::from(3.0), Value::from("200.25")],
        ];
        let blocks = deterministic_summary(&v, &rows);
        match &blocks[0] {
            Block::Kpis { items } => {
                assert_eq!(items[0].label, "合计明细金额");
                assert_eq!(items[0].value, serde_json::json!(300.75), "合计由代码算");
                assert_eq!(items.last().unwrap().label, "记录数");
                assert_eq!(items.last().unwrap().value, Value::from(2));
                // 数量类不求和：箱数 + 袋数相加没有业务含义
                assert!(!items.iter().any(|k| k.label.contains("数量")), "{items:?}");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(blocks.last(), Some(Block::Table)), "逐行核对能力保底");
        // 没有金额列时不硬凑一张卡
        let no_money = view(&[("商品名称", Role::Category, Semantic::Goods), ("数量", Role::Metric, Semantic::Count)]);
        assert!(deterministic_summary(&no_money, &rows).is_empty());
    }

    /// 头卡**原样留在最前**：它是「这是哪张单」的身份，编排只接管它后面那一段。
    #[test]
    fn document_header_survives_composition() {
        let v = view(&[("商品名称", Role::Category, Semantic::Goods), ("明细金额", Role::Metric, Semantic::Money)]);
        let rows = vec![
            vec![Value::from("烤肠"), Value::from(10.0)],
            vec![Value::from("烧麦"), Value::from(20.0)],
        ];
        let composed = build_blocks(
            &plan(r#"{"blocks":[{"type":"stat","col":1,"agg":"sum","label":"合计金额"}]}"#),
            &v,
            &rows,
        );
        let header = Block::Entity { pairs: vec![("单号".into(), Value::from("HJXH-DXO1"))] };
        let merged: Vec<Block> = std::iter::once(header).chain(composed).collect();
        assert!(matches!(merged[0], Block::Entity { .. }), "头卡必须还在最前：{merged:?}");
        assert!(matches!(merged[1], Block::Kpis { .. }), "{merged:?}");
        assert!(matches!(merged.last(), Some(Block::Table)), "{merged:?}");
    }

    /// 造一个字段全空的 `AskResult`（本模块只读 sql/columns/rows/subs/view）。
    fn empty_result() -> AskResult {
        AskResult {
            sql: String::new(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            truncated: false,
            elapsed_ms: 0,
            route: "test".into(),
            view: view(&[]),
            supplemental: None,
            comparisons: vec![],
            subs: vec![],
            caliber_note: None,
            reinterpret_note: None,
            resolved_question: None,
            truncation_note: None,
            redacted: vec![],
            scope_note: None,
            trust: None,
            steps: vec![],
            clarify_options: vec![],
            value_labels: vec![],
            sales_context: None,
            intent_summary: None,
            kb: None,
        }
    }

    #[test]
    fn fence_and_unknown_block_names_do_not_break_the_contract() {
        assert_eq!(strip_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_fence("  {\"a\":1}  "), "{\"a\":1}");
        let p: Plan = serde_json::from_str(r#"{"blocks":[{"type":"heatmap"},{"type":"table"}]}"#).unwrap();
        assert_eq!(p.blocks.len(), 2);
        assert!(matches!(p.blocks[0], PlanBlock::Unknown));
    }
}
