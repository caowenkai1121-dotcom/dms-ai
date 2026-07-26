//! ViewSpec 呈现协议：列语义推断 + 图表自动决策。
//! 参考 SuperSonic chat-sdk：后端给列语义（showType/role），前端按语义智能选呈现形态。
//! 决策树对齐 SuperSonic getMsgContentType + 旧项目 viewspec 增强。

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Metric,   // 数值指标（可聚合）
    Category, // 类别维度
    Time,     // 时间
    Id,       // 编码/单号（可点击语义）
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Semantic {
    Money,
    Count,
    Percent,
    Geo,
    Customer,
    Goods,
    Order,
    None,
}

#[derive(Serialize)]
pub struct ColumnSpec {
    pub name: String,
    pub role: Role,
    #[serde(skip_serializing_if = "is_none_sem")]
    pub semantic: Semantic,
}

fn is_none_sem(s: &Semantic) -> bool {
    matches!(s, Semantic::None)
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Block {
    /// 单行全指标 → KPI 卡
    Kpis { items: Vec<Kpi> },
    /// 单行多列实体 → 键值卡（单据卡）
    Entity { pairs: Vec<(String, Value)> },
    /// 图表（bar/line/pie）
    Chart {
        kind: ChartKind,
        x: usize,        // 类别/时间轴列下标
        y: Vec<usize>,   // 值轴列下标
        #[serde(skip_serializing_if = "Option::is_none")]
        top: Option<usize>,
    },
    /// 表格（默认兜底）
    Table,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ChartKind {
    Bar,
    Line,
    Pie,
}

#[derive(Serialize)]
pub struct Delta {
    pub pct: f64,
    pub dir: &'static str, // "up" | "down" | "flat"
    pub label: String,     // "较上月" 等
}

#[derive(Serialize)]
pub struct Kpi {
    pub label: String,
    pub value: Value,
    pub semantic: Semantic,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<Delta>,
}

/// 交互声明（对齐 SuperSonic recommendedDimensions/DrillDownDimensions）：
/// 可下钻的维度——前端渲染 chips，点击后带"按X"重问（参数化下钻）。
#[derive(Serialize, Default)]
pub struct Interact {
    pub drill: Vec<String>,
}

#[derive(Serialize)]
pub struct ViewSpec {
    pub columns: Vec<ColumnSpec>,
    pub blocks: Vec<Block>,
    #[serde(skip_serializing_if = "drill_empty")]
    pub interact: Interact,
    /// 结论洞察（移植 SuperSonic textSummary）：排行占比/趋势涨跌一句话解读
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insight: Option<String>,
}

fn drill_empty(i: &Interact) -> bool {
    i.drill.is_empty()
}

/// 常用下钻维度池（对齐主表已知维度）。剔除结果里已出现的维度。
const DIM_POOL: &[&str] = &["省份", "商品分类", "业务员", "客户", "门店", "月份"];

fn infer_drill(specs: &[ColumnSpec], has_metric: bool) -> Vec<String> {
    if !has_metric {
        return vec![]; // 无指标（明细/实体卡）不下钻
    }
    let used: String = specs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("");
    DIM_POOL
        .iter()
        .filter(|d| {
            // 该维度关键词未在结果列名出现才建议
            let key = d.chars().take(2).collect::<String>();
            !used.contains(&key)
        })
        .map(|s| s.to_string())
        .collect()
}

/// 列名 → 语义
fn infer_semantic(name: &str) -> Semantic {
    let n = name;
    if n.contains('率') || n.contains("占比") || n.contains('%') {
        Semantic::Percent
    } else if n.contains("金额") || n.contains("销售额") || n.contains("营业额") || n.contains("客单价")
        || n.contains("余额") || n.contains("费用") || n.ends_with('额')
    {
        Semantic::Money
    } else if n.contains("省") || n.contains("市") || n.contains("区县") || n.contains("地区") {
        Semantic::Geo
    } else if n.contains("客户") {
        Semantic::Customer
    } else if n.contains("商品") || n.contains("SKU") || n.contains("sku") {
        Semantic::Goods
    } else if n.contains('数') || n.contains("销量") || n.contains("笔数") {
        // 「订单数/售后单数」是计数指标——必须先于 Order 判定，否则被"订单"抢走永远不算指标列
        Semantic::Count
    } else if n.contains("单号") || n.contains("订单") {
        Semantic::Order
    } else {
        Semantic::None
    }
}

/// 一列是否数值（所有非空值可解析为数字）
fn is_numeric_col(rows: &[Vec<Value>], i: usize) -> bool {
    let mut saw = false;
    for row in rows {
        match row.get(i) {
            Some(Value::Null) | None => {}
            Some(Value::Number(_)) => saw = true,
            Some(Value::String(s)) => {
                if s.trim().parse::<f64>().is_err() {
                    return false;
                }
                saw = true;
            }
            _ => return false,
        }
    }
    saw
}

/// 列名 + 数据 → role
fn infer_role(name: &str, rows: &[Vec<Value>], i: usize) -> Role {
    let n = name;
    // 时间（趋势线 x 轴）：含时间/日期关键词，或月份/季度/年月等时间维度
    if n.contains("时间") || n.contains("日期") || n.contains("月份") || n.contains("季度")
        || n.contains("年月") || n.ends_with("date") || n.ends_with("time")
    {
        return Role::Time;
    }
    // 编码/单号：名称信号 + 值非纯聚合数字
    if n.contains("编码") || n.contains("单号") || n.contains("编号") || n.ends_with("code") || n.ends_with("_id") {
        return Role::Id;
    }
    // 指标：名称含指标语义 且 列数值
    let sem = infer_semantic(n);
    let metric_sem = matches!(sem, Semantic::Money | Semantic::Count | Semantic::Percent);
    if metric_sem && is_numeric_col(rows, i) {
        return Role::Metric;
    }
    // 纯数值列（无指标名）且列名不像类别 → 也算 metric
    if is_numeric_col(rows, i) && !n.contains("年") && !n.contains("月") {
        // 但短枚举类数值（如状态码）不算——这里保守：有指标名才算，否则 category
    }
    Role::Category
}

const PIE_MAX: usize = 10; // 对齐 SuperSonic 桌面阈值
const BAR_MAX: usize = 50;
const BAR_TOP: usize = 20; // 对齐 SuperSonic Trend slice(0,20)
const ENTITY_MIN_COLS: usize = 6;

/// 单指标 KPI 环比补丁：view 首块为单项 Kpis 时，按当前/上期值算 Δ%。
pub fn patch_kpi_delta(view: &mut ViewSpec, cur: f64, prev: f64, label: String) {
    if let Some(Block::Kpis { items }) = view.blocks.first_mut() {
        if items.len() == 1 {
            if prev.abs() < f64::EPSILON {
                return; // 上期为 0，环比无意义
            }
            let pct = (cur - prev) / prev * 100.0;
            let dir = if pct > 0.05 { "up" } else if pct < -0.05 { "down" } else { "flat" };
            items[0].delta = Some(Delta { pct: (pct * 10.0).round() / 10.0, dir, label });
        }
    }
}

/// 组装 ViewSpec：推断下钻维度 + 结论洞察（has_metric 从列 role 判定）
fn mk(specs: Vec<ColumnSpec>, blocks: Vec<Block>, rows: &[Vec<Value>]) -> ViewSpec {
    let has_metric = specs.iter().any(|c| c.role == Role::Metric);
    let drill = infer_drill(&specs, has_metric);
    let insight = compute_insight(&specs, rows);
    ViewSpec { columns: specs, blocks, interact: Interact { drill }, insight }
}

/// 结论洞察（移植 SuperSonic textSummary）：排行占比+CR3集中度 / 趋势涨跌，确定性 0-LLM。
fn compute_insight(specs: &[ColumnSpec], rows: &[Vec<Value>]) -> Option<String> {
    let metric_idx: Vec<usize> = specs.iter().enumerate().filter(|(_, s)| s.role == Role::Metric).map(|(i, _)| i).collect();
    if metric_idx.len() != 1 {
        return None;
    }
    let mi = metric_idx[0];
    let cat_i = specs.iter().position(|s| s.role == Role::Category);
    let time_i = specs.iter().position(|s| s.role == Role::Time);
    let sem = specs[mi].semantic;
    let unit = |v: f64| match sem {
        Semantic::Money => format!("¥{}", compress(v)),
        Semantic::Percent => format!("{:.1}%", v),
        _ => compress(v),
    };

    // 排行（类别+单指标，≥5 行）：榜首占比 + 前三合计占比（CR3）
    if let Some(ci) = cat_i {
        if rows.len() >= 5 {
            let is_geo = matches!(specs[ci].semantic, Semantic::Geo);
            let mut vals: Vec<(String, f64)> = rows
                .iter()
                .filter_map(|r| {
                    let v = cell_f64(&r[mi])?;
                    let raw = r.get(ci).map(val_str).unwrap_or_default();
                    let mut name = if is_geo { province_cn(&raw).map(|s| s.to_string()).unwrap_or(raw) } else { raw };
                    if name.trim().is_empty() {
                        name = "未知".to_string();
                    }
                    Some((name, v))
                })
                .collect();
            let total: f64 = vals.iter().map(|(_, v)| v).sum();
            if total > 0.0 && vals.len() >= 3 {
                vals.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let top = &vals[0];
                let cr3: f64 = vals.iter().take(3).map(|(_, v)| v).sum();
                return Some(format!(
                    "榜首「{}」{}，占 {:.1}%；前三合计占 {:.1}%（共 {} 项）",
                    top.0, unit(top.1), top.1 / total * 100.0, cr3 / total * 100.0, vals.len()
                ));
            }
        }
    }
    // 趋势（时间+单指标，≥2 行）：首末对比涨跌
    if let Some(_ti) = time_i {
        if rows.len() >= 2 {
            let first = cell_f64(&rows[0][mi])?;
            let last = cell_f64(&rows[rows.len() - 1][mi])?;
            if first.abs() > f64::EPSILON {
                let pct = (last - first) / first * 100.0;
                let dir = if pct >= 0.0 { "增长" } else { "下降" };
                return Some(format!(
                    "从 {} 到 {}，整体{} {:.1}%",
                    unit(first), unit(last), dir, pct.abs()
                ));
            }
        }
    }
    None
}

fn val_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// 省级区划码 → 省名（insight 里 geo 列翻名，与前端 format.ts 一致）
fn province_cn(code: &str) -> Option<&'static str> {
    Some(match code {
        "110000" => "北京", "120000" => "天津", "130000" => "河北", "140000" => "山西",
        "150000" => "内蒙古", "210000" => "辽宁", "220000" => "吉林", "230000" => "黑龙江",
        "310000" => "上海", "320000" => "江苏", "330000" => "浙江", "340000" => "安徽",
        "350000" => "福建", "360000" => "江西", "370000" => "山东", "410000" => "河南",
        "420000" => "湖北", "430000" => "湖南", "440000" => "广东", "450000" => "广西",
        "460000" => "海南", "500000" => "重庆", "510000" => "四川", "520000" => "贵州",
        "530000" => "云南", "540000" => "西藏", "610000" => "陕西", "620000" => "甘肃",
        "630000" => "青海", "640000" => "宁夏", "650000" => "新疆", "710000" => "台湾",
        "810000" => "香港", "820000" => "澳门",
        _ => return None,
    })
}

/// 决策树（SuperSonic getMsgContentType 对齐 + 增强）
pub fn build(columns: &[String], rows: &[Vec<Value>]) -> ViewSpec {
    let specs: Vec<ColumnSpec> = columns
        .iter()
        .enumerate()
        .map(|(i, name)| ColumnSpec {
            name: name.clone(),
            role: infer_role(name, rows, i),
            semantic: infer_semantic(name),
        })
        .collect();

    let metric_idx: Vec<usize> = specs.iter().enumerate().filter(|(_, s)| s.role == Role::Metric).map(|(i, _)| i).collect();
    let cat_idx: Vec<usize> = specs.iter().enumerate().filter(|(_, s)| s.role == Role::Category).map(|(i, _)| i).collect();
    let time_idx: Vec<usize> = specs.iter().enumerate().filter(|(_, s)| s.role == Role::Time).map(|(i, _)| i).collect();
    let id_idx: Vec<usize> = specs.iter().enumerate().filter(|(_, s)| s.role == Role::Id).map(|(i, _)| i).collect();

    let mut blocks: Vec<Block> = vec![];

    // 1. 单行全指标 → KPI 卡
    if rows.len() == 1 && !metric_idx.is_empty() && metric_idx.len() == columns.len() {
        let items = metric_idx
            .iter()
            .map(|&i| Kpi {
                label: specs[i].name.clone(),
                value: rows[0][i].clone(),
                semantic: specs[i].semantic,
                delta: None,
            })
            .collect();
        blocks.push(Block::Kpis { items });
        return mk(specs, blocks, rows);
    }

    // 2. 单行多列 → 实体卡（单据卡）
    if rows.len() == 1 && columns.len() >= ENTITY_MIN_COLS {
        let pairs = columns
            .iter()
            .enumerate()
            .filter(|(i, _)| !matches!(rows[0][*i], Value::Null))
            .map(|(i, name)| (name.clone(), rows[0][i].clone()))
            .collect();
        blocks.push(Block::Entity { pairs });
        return mk(specs, blocks, rows);
    }

    // 3. 明细/多维形态 → 纯表格（对齐 SuperSonic：categoryField>1 或有 id 列 → TABLE）。
    //    防止 200 行订单明细被误画成趋势线（每行不同订单，时间轴无意义）。
    if rows.len() > 1 && (cat_idx.len() > 1 || !id_idx.is_empty()) {
        blocks.push(Block::Table);
        return mk(specs, blocks, rows);
    }

    // 4. 有时间列 + ≥2 行 + ≥1 指标 + 类别≤1（趋势序列，非明细）→ 趋势线图
    if !time_idx.is_empty() && rows.len() >= 2 && !metric_idx.is_empty() && cat_idx.len() <= 1 {
        blocks.push(Block::Chart {
            kind: ChartKind::Line,
            x: time_idx[0],
            y: metric_idx.clone(),
            top: None,
        });
        blocks.push(Block::Table);
        return mk(specs, blocks, rows);
    }

    // 4. 恰一类别列 + 恰一指标列 → 饼/柱
    if cat_idx.len() == 1 && metric_idx.len() == 1 && rows.len() <= BAR_MAX {
        let x = cat_idx[0];
        let y = metric_idx[0];
        let all_nonneg = rows.iter().all(|r| cell_f64(&r[y]).map(|v| v >= 0.0).unwrap_or(true));
        let is_pct = matches!(specs[y].semantic, Semantic::Percent);
        if rows.len() <= PIE_MAX && all_nonneg && !is_pct {
            blocks.push(Block::Chart { kind: ChartKind::Pie, x, y: vec![y], top: None });
        } else {
            let top = if rows.len() > BAR_TOP { Some(BAR_TOP) } else { None };
            blocks.push(Block::Chart { kind: ChartKind::Bar, x, y: vec![y], top });
        }
        blocks.push(Block::Table);
        return mk(specs, blocks, rows);
    }

    // 4b. 恰一类别列 + ≥2 指标列 → 分组柱图（对齐 SuperSonic：多指标同类别并排呈现）
    if cat_idx.len() == 1 && metric_idx.len() >= 2 && rows.len() <= BAR_MAX {
        let top = if rows.len() > BAR_TOP { Some(BAR_TOP) } else { None };
        blocks.push(Block::Chart { kind: ChartKind::Bar, x: cat_idx[0], y: metric_idx.clone(), top });
        blocks.push(Block::Table);
        return mk(specs, blocks, rows);
    }

    // 5. 兜底表格
    blocks.push(Block::Table);
    mk(specs, blocks, rows)
}

/// 万/亿压缩（对齐前端 format.ts：亿2位/万1位）
fn compress(n: f64) -> String {
    let abs = n.abs();
    if abs >= 1e8 {
        format!("{:.2}亿", n / 1e8)
    } else if abs >= 1e4 {
        format!("{:.1}万", n / 1e4)
    } else {
        format!("{:.0}", n)
    }
}

fn cell_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cols(c: &[&str]) -> Vec<String> {
        c.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_metric_row_kpi() {
        let v = build(&cols(&["销售额"]), &[vec![json!("163000000")]]);
        assert!(matches!(v.blocks[0], Block::Kpis { .. }));
        assert_eq!(v.columns[0].role, Role::Metric);
        assert_eq!(v.columns[0].semantic, Semantic::Money);
    }

    #[test]
    fn category_metric_small_pie() {
        let rows = vec![
            vec![json!("广东"), json!("100")],
            vec![json!("山东"), json!("90")],
            vec![json!("河南"), json!("80")],
        ];
        let v = build(&cols(&["省份", "销售额"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Pie, .. } => {}
            b => panic!("expected pie, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn category_metric_many_bar_top() {
        let rows: Vec<Vec<Value>> = (0..30).map(|i| vec![json!(format!("省{i}")), json!(i.to_string())]).collect();
        let v = build(&cols(&["省份", "销售额"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Bar, top: Some(20), .. } => {}
            b => panic!("expected bar top20, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn time_series_line() {
        let rows = vec![
            vec![json!("2026-07-01"), json!("10")],
            vec![json!("2026-07-02"), json!("20")],
        ];
        let v = build(&cols(&["下单时间", "销售额"]), &rows);
        assert!(matches!(v.blocks[0], Block::Chart { kind: ChartKind::Line, .. }));
        assert_eq!(v.columns[0].role, Role::Time);
    }

    #[test]
    fn category_multi_metric_grouped_bar() {
        // 一类别 + 两指标 → 分组柱（y 含两个指标列），不再落纯表格
        let rows = vec![
            vec![json!("广东"), json!("100"), json!("5")],
            vec![json!("山东"), json!("90"), json!("4")],
        ];
        let v = build(&cols(&["省份", "销售额", "订单数"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Bar, y, .. } => assert_eq!(y.len(), 2),
            b => panic!("expected grouped bar, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn time_multi_metric_line_series() {
        // 时间 + 两指标 → 双序列趋势线
        let rows = vec![
            vec![json!("2026-07-01"), json!("10"), json!("2")],
            vec![json!("2026-07-02"), json!("20"), json!("3")],
        ];
        let v = build(&cols(&["下单时间", "销售额", "订单数"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Line, y, .. } => assert_eq!(y.len(), 2),
            b => panic!("expected multi-series line, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn single_row_many_cols_entity() {
        let cols_v = cols(&["单号", "客户", "金额", "状态", "时间", "备注", "经办"]);
        let row = vec![json!("HJXH-1"), json!("恒众"), json!("100"), json!("完成"), json!("2026"), json!("x"), json!("张三")];
        let v = build(&cols_v, &[row]);
        assert!(matches!(v.blocks[0], Block::Entity { .. }));
    }

    #[test]
    fn multi_dim_table() {
        let rows = vec![
            vec![json!("广东"), json!("烤肠"), json!("100")],
            vec![json!("山东"), json!("包子"), json!("90")],
        ];
        let v = build(&cols(&["省份", "商品", "销量"]), &rows);
        assert!(matches!(v.blocks.last().unwrap(), Block::Table));
    }

    #[test]
    fn percent_uses_bar_not_pie() {
        let rows = vec![vec![json!("广东"), json!("50.5")], vec![json!("山东"), json!("30.2")]];
        let v = build(&cols(&["省份", "占比"]), &rows);
        assert!(matches!(v.blocks[0], Block::Chart { kind: ChartKind::Bar, .. }));
    }

    #[test]
    fn kpi_delta_up_down_and_zero() {
        let mut v = build(&cols(&["销售额"]), &[vec![json!("120")]]);
        patch_kpi_delta(&mut v, 120.0, 100.0, "较上月".into());
        if let Block::Kpis { items } = &v.blocks[0] {
            let d = items[0].delta.as_ref().unwrap();
            assert_eq!(d.dir, "up");
            assert_eq!(d.pct, 20.0);
        } else {
            panic!("no kpi");
        }
        // 上期为 0 → 不填 delta
        let mut v2 = build(&cols(&["销售额"]), &[vec![json!("120")]]);
        patch_kpi_delta(&mut v2, 120.0, 0.0, "较上月".into());
        if let Block::Kpis { items } = &v2.blocks[0] {
            assert!(items[0].delta.is_none());
        }
    }
}
