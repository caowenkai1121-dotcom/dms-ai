//! 呈现协议**类型**（`Answer.view` 需要它们）：列语义 + 图表块 + KPI + 下钻声明。
//!
//! 这里只有类型与 serde 形状——**前端字节兼容的唯一事实源**，serde 属性逐字不许动。
//! 推断算法与中文词表（`infer_role`/`infer_semantic`/`build`/`DIM_POOL`/`PROVINCE_LABELS`）
//! 整块留在业务侧（server `viewspec.rs`，K3/T7 迁 semantic）——它们全是 DMS 业务语料。
//!
//! 搬运源 `server/src/viewspec.rs:8-105`。

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
        /// 🔴 **多序列切分列**（类别列下标）：一根线只连同一类别的点。
        ///
        /// 没有它的时候「时间 + 恰 1 类别 + 1 指标」（例「今年各月各品类销售额」，
        /// 12 月 × 6 品类 = 72 行）被画成**一条混轴折线** —— x 轴上「2026-01」重复 6 次，
        /// 不同品类的点被连成一根线，图是错的而没有任何判据会红。
        ///
        /// `None` ＝ 单序列，序列化时整键不上线 —— 旧 JSON 逐字节不变
        /// （serde 形状是前端 + `tools/regression.py` 的契约，只许加 `Option`）。
        #[serde(skip_serializing_if = "Option::is_none")]
        series: Option<usize>,
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

#[derive(Serialize, Clone)]
pub struct Delta {
    pub pct: f64,
    pub dir: &'static str, // "up" | "down" | "flat"
    pub label: String,     // "较上月" 等
    /// 基期原值与绝对变化额。旧消费者忽略新增键；深度 BI 用它展示可核数的比较卡。
    pub baseline: f64,
    pub change: f64,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// serde 形状即前端契约：块的 `type` 标签、枚举小写、三个 skip_serializing_if。
    #[test]
    fn block_tag_and_lowercase_enums() {
        let v = ViewSpec {
            columns: vec![ColumnSpec { name: "c".into(), role: Role::Metric, semantic: Semantic::None }],
            blocks: vec![
                Block::Chart { kind: ChartKind::Pie, x: 0, y: vec![1], top: None, series: None },
                Block::Chart { kind: ChartKind::Line, x: 0, y: vec![2], top: None, series: Some(1) },
                Block::Table,
            ],
            interact: Interact::default(),
            insight: None,
        };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["columns"][0]["role"], "metric");
        assert!(j["columns"][0].get("semantic").is_none(), "Semantic::None 不上线");
        assert_eq!(j["blocks"][0]["type"], "chart");
        assert_eq!(j["blocks"][0]["kind"], "pie");
        assert!(j["blocks"][0].get("top").is_none(), "top=None 不上线");
        // series=None 整键不上线（旧 JSON 逐字节不变）；Some 时是**列下标**，不是列名
        assert!(j["blocks"][0].get("series").is_none(), "series=None 不上线");
        assert_eq!(j["blocks"][1]["series"], 1);
        assert_eq!(j["blocks"][2]["type"], "table");
        assert!(j.get("interact").is_none(), "空下钻不上线");
        assert!(j.get("insight").is_none());
    }

    #[test]
    fn kpi_delta_and_drill_shape() {
        let v = ViewSpec {
            columns: vec![],
            blocks: vec![Block::Kpis {
                items: vec![Kpi {
                    label: "k".into(),
                    value: serde_json::json!(1),
                    semantic: Semantic::Money,
                    delta: Some(Delta { pct: -1.5, dir: "down", label: "较上期".into(), baseline: 2.0, change: -1.0 }),
                }],
            }],
            interact: Interact { drill: vec!["d".into()] },
            insight: Some("s".into()),
        };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["blocks"][0]["type"], "kpis");
        assert_eq!(j["blocks"][0]["items"][0]["semantic"], "money");
        assert_eq!(j["blocks"][0]["items"][0]["delta"]["dir"], "down");
        assert_eq!(j["blocks"][0]["items"][0]["delta"]["baseline"], 2.0);
        assert_eq!(j["interact"]["drill"][0], "d");
        assert_eq!(j["insight"], "s");
    }
}
