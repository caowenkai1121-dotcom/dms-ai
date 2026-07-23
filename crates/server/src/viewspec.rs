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
pub struct Kpi {
    pub label: String,
    pub value: Value,
    pub semantic: Semantic,
}

#[derive(Serialize)]
pub struct ViewSpec {
    pub columns: Vec<ColumnSpec>,
    pub blocks: Vec<Block>,
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
    } else if n.contains("单号") || n.contains("订单") {
        Semantic::Order
    } else if n.contains('数') || n.contains("销量") || n.contains("笔数") {
        Semantic::Count
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
    // 时间
    if n.contains("时间") || n.contains("日期") || n.ends_with("date") || n.ends_with("time") {
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

const PIE_MAX: usize = 6;
const BAR_MAX: usize = 50;
const BAR_TOP: usize = 18;
const ENTITY_MIN_COLS: usize = 6;

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

    let mut blocks: Vec<Block> = vec![];

    // 1. 单行全指标 → KPI 卡
    if rows.len() == 1 && !metric_idx.is_empty() && metric_idx.len() == columns.len() {
        let items = metric_idx
            .iter()
            .map(|&i| Kpi {
                label: specs[i].name.clone(),
                value: rows[0][i].clone(),
                semantic: specs[i].semantic,
            })
            .collect();
        blocks.push(Block::Kpis { items });
        return ViewSpec { columns: specs, blocks };
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
        return ViewSpec { columns: specs, blocks };
    }

    // 3. 有时间列 + ≥2 行 + ≥1 指标 → 趋势线图
    if !time_idx.is_empty() && rows.len() >= 2 && !metric_idx.is_empty() {
        blocks.push(Block::Chart {
            kind: ChartKind::Line,
            x: time_idx[0],
            y: metric_idx.clone(),
            top: None,
        });
        blocks.push(Block::Table);
        return ViewSpec { columns: specs, blocks };
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
        return ViewSpec { columns: specs, blocks };
    }

    // 5. 兜底表格
    blocks.push(Block::Table);
    ViewSpec { columns: specs, blocks }
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
            Block::Chart { kind: ChartKind::Bar, top: Some(18), .. } => {}
            b => panic!("expected bar top18, got {}", serde_json::to_string(b).unwrap()),
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
}
