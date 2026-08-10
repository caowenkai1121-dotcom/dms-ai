//! 深度模式的问题分类与报告合同。
//!
//! 这里只根据问句与已经执行出的结果形状决定“该做哪一类报告”；不生成 SQL、不访问数据源。
//! DWS 销售板块由 server 复用共享 `sales_fact` 合同和主查询权限谓词编译；其它板块走统一
//! `ask()` 管线。分类只影响报告丰富度，任何执行路径都不能绕过口径与权限闸门。

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalysisKind {
    Metric,
    Breakdown,
    Trend,
    Comparison,
    Attribution,
    Document,
    Entity,
    Detail,
    General,
}

impl AnalysisKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Metric => "metric",
            Self::Breakdown => "breakdown",
            Self::Trend => "trend",
            Self::Comparison => "comparison",
            Self::Attribution => "attribution",
            Self::Document => "document",
            Self::Entity => "entity",
            Self::Detail => "detail",
            Self::General => "general",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Metric => "指标总览",
            Self::Breakdown => "维度分析",
            Self::Trend => "趋势分析",
            Self::Comparison => "对比分析",
            Self::Attribution => "归因分析",
            Self::Document => "单据核验",
            Self::Entity => "实体洞察",
            Self::Detail => "明细核查",
            Self::General => "综合分析",
        }
    }
}

pub struct AnalysisShape<'a> {
    pub route: &'a str,
    pub row_count: usize,
    pub columns: &'a [String],
    /// 主结果是否来自统一的 DWS 销售事实合同。只有这类结果才允许深度模式补齐
    /// 销售经营结构、趋势和同口径明细，避免把订单额或其它金额误套进销售模板。
    pub dws_sales_metric: bool,
    /// 单据快路已返回“单据类型/主表/明细表”证据；不能仅凭 `direct-doc` 路由判断，
    /// 因为设备订单列表等确定性查询也可能使用该路由。
    pub document_evidence: bool,
}

// ───────────────── 【D8】验收断言：分析前先把「要回答什么、怎么算答好」透出 ─────────────────
// DataFoundry 需求/契约抽取器思想：规划产出计划时，同步产出每个板块的验收标准
// （该板块要证明什么、用什么数据校验），随进度事件与最终结果透出给用户。
// 铁律：**断言是加分项不是必需品** —— LLM 规划失败/模型没给 = 无断言，绝不阻塞报告。

/// 末次证据解读对一条断言的自评档（与证据解读**同一发** LLM 调用产出，不加串行调用）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Acceptance {
    /// 证据直接支持断言
    Met,
    /// 证据只覆盖断言的一部分（或板块数据不全）
    Partial,
    /// 证据不支持 / 板块缺席
    Unmet,
}

impl Acceptance {
    /// 透出/落库用的稳定英文码（前端按它映射颜色与文案）
    pub const fn code(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Partial => "partial",
            Self::Unmet => "unmet",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Met => "满足",
            Self::Partial => "部分满足",
            Self::Unmet => "未满足",
        }
    }

    /// LLM/DB 文本 → 档位。无法识别 = None：该条断言降级为「无判词」，
    /// 不报错、不重试（判词缺席比瞎判一档诚实）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().trim_end_matches(['。', '.', ',', '，']).to_ascii_lowercase().as_str() {
            "met" | "满足" => Some(Self::Met),
            "partial" | "部分" | "部分满足" => Some(Self::Partial),
            "unmet" | "未满足" | "不满足" => Some(Self::Unmet),
            _ => None,
        }
    }
}

/// 一条验收断言（板块级）：本报告该板块要证明什么、用什么数据校验。
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Assertion {
    /// 断言针对的板块标题；空串 = 全报告级（当前只产板块级）。
    #[serde(default)]
    pub section: String,
    /// 验收陈述（一句话，透出原文）。
    pub text: String,
}

/// 断言语句清洗（LLM 产出与 DB 回读**同一口径**，纯函数）：
/// trim；空 = 无断言（降级）；最长 80 字截断（模型写长了不撑爆透出区）。
pub fn clean_assertion(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(80).collect())
}

/// 计划板块（标题, 断言）序列 → 透出用断言清单：跳过无断言板块，最多 8 条
/// （模型失控刷屏也撑不死进度事件与页面载荷）。
pub fn collect_assertions<'a>(
    sections: impl IntoIterator<Item = (&'a str, &'a Option<String>)>,
) -> Vec<Assertion> {
    sections
        .into_iter()
        .filter_map(|(title, assertion)| {
            clean_assertion(assertion.as_deref()?)
                .map(|text| Assertion { section: title.to_string(), text })
        })
        .take(8)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnalysisPlan {
    pub kind: AnalysisKind,
    /// 是否属于统一 DWS 销售事实指标。深度报告据此补齐总值、可比窗口、结构、
    /// 趋势和业务明细；订单数等非 DWS 指标不会进入这套合同。
    pub dws_sales_metric: bool,
    /// 是否允许模型提出关联板块。单据、实体和明细问题只展示同一对象的已执行证据，
    /// 禁止套用通用经营分析模板。
    pub allow_model_sections: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReportSpec {
    pub kind: AnalysisKind,
    pub badge: &'static str,
    pub primary_title: &'static str,
    /// 标量指标优先展示主查询已经算出的同期/环比，不再让模型从趋势图猜基线。
    pub show_comparison: bool,
    /// 指标、对比与归因页都必须有可核数的贡献结构；其它报告不套经营分析模板。
    pub show_contribution: bool,
    pub include_recent_orders: bool,
}

impl AnalysisPlan {
    pub const fn report_spec(self) -> ReportSpec {
        ReportSpec {
            kind: self.kind,
            badge: self.kind.label(),
            primary_title: match self.kind {
                AnalysisKind::Document => "单据明细",
                AnalysisKind::Entity => "实体关联明细",
                AnalysisKind::Detail => "查询明细",
                _ => "主查询结果",
            },
            show_comparison: self.dws_sales_metric || matches!(
                self.kind,
                AnalysisKind::Metric
                    | AnalysisKind::Trend
                    | AnalysisKind::Comparison
                    | AnalysisKind::Attribution
            ),
            show_contribution: self.dws_sales_metric || matches!(
                self.kind,
                AnalysisKind::Metric
                    | AnalysisKind::Breakdown
                    | AnalysisKind::Comparison
                    | AnalysisKind::Attribution
            ),
            // DWS 销售报告使用同时间窗、同实体谓词的经营明细，不能混入旧订单表的
            // “最近活动”。该开关仅保留给非 DWS 的通用经营报告。
            include_recent_orders: !self.dws_sales_metric
                && matches!(
                    self.kind,
                    AnalysisKind::Metric | AnalysisKind::Comparison | AnalysisKind::Attribution
                ),
        }
    }
}

pub fn plan(question: &str, shape: AnalysisShape<'_>) -> AnalysisPlan {
    let kind = if shape.document_evidence {
        AnalysisKind::Document
    } else if shape.route == "entity-card" {
        AnalysisKind::Entity
    } else if has_any(question, &["设备订单", "设备销售单"]) && shape.row_count > 1 {
        // 设备订单列表有一套已经验证过的构成/客户/状态/时段合同；它不是具体单号核验。
        AnalysisKind::Breakdown
    } else if has_any(question, &["为什么", "原因", "归因", "驱动", "贡献因素", "影响因素"]) {
        AnalysisKind::Attribution
    } else if has_any(question, &["同比", "环比", "对比", "相比", "比较", "较上", "较去"]) {
        AnalysisKind::Comparison
    } else if has_any(question, &["趋势", "走势", "各月", "每月", "按月", "月度", "逐日", "每日"]) {
        AnalysisKind::Trend
    } else if has_any(question, &["按", "各", "排行", "排名", "前五", "前十", "占比", "分布", "构成"])
    {
        AnalysisKind::Breakdown
    } else if has_any(question, &["明细", "列表", "记录", "清单", "哪些"]) {
        AnalysisKind::Detail
    } else if shape.row_count == 1 && shape.columns.len() == 1 {
        AnalysisKind::Metric
    } else if shape.columns.len() > 3 && shape.row_count > 1 {
        AnalysisKind::Detail
    } else {
        AnalysisKind::General
    };
    AnalysisPlan {
        kind,
        dws_sales_metric: shape.dws_sales_metric,
        allow_model_sections: (shape.dws_sales_metric
            && matches!(
                kind,
                AnalysisKind::Metric
                    | AnalysisKind::Trend
                    | AnalysisKind::Breakdown
                    | AnalysisKind::Comparison
                    | AnalysisKind::Attribution
            )) || matches!(kind, AnalysisKind::Metric | AnalysisKind::Comparison | AnalysisKind::Attribution | AnalysisKind::General)
            || (kind == AnalysisKind::Breakdown
                && (question.contains("设备订单") || question.contains("设备销售单"))),
    }
}

fn has_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| text.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape<'a>(route: &'a str, rows: usize, columns: &'a [String]) -> AnalysisShape<'a> {
        AnalysisShape {
            route,
            row_count: rows,
            columns,
            dws_sales_metric: columns.iter().any(|column| {
                ["销售额", "销量", "销售数量", "不含税成本", "不含税收入", "毛利额", "毛利率"]
                    .iter()
                    .any(|metric| column.contains(metric))
            }),
            document_evidence: false,
        }
    }

    #[test]
    fn document_and_entity_never_use_generic_planning() {
        let cols = vec!["商品".into(), "数量".into()];
        let doc = plan(
            "查 HJXH-DRO2026080500033",
            AnalysisShape { document_evidence: true, ..shape("direct-doc", 1, &cols) },
        );
        assert_eq!(doc.kind, AnalysisKind::Document);
        assert!(!doc.dws_sales_metric);
        assert!(!doc.allow_model_sections);
        assert_eq!(doc.report_spec().primary_title, "单据明细");

        let entity = plan("商品分类烤肠类", shape("entity-card", 20, &cols));
        assert_eq!(entity.kind, AnalysisKind::Entity);
        assert!(!entity.allow_model_sections);
    }

    #[test]
    fn analytical_questions_receive_their_own_contracts() {
        let kpi = vec!["销售额".into()];
        assert_eq!(plan("本月销售额", shape("direct-agg", 1, &kpi)).kind, AnalysisKind::Metric);
        assert_eq!(plan("本月销售额为什么下降", shape("direct-agg", 1, &kpi)).kind, AnalysisKind::Attribution);
        assert_eq!(plan("本月销售额环比", shape("direct-agg", 1, &kpi)).kind, AnalysisKind::Comparison);
        assert_eq!(plan("今年各月销售额趋势", shape("direct-agg", 12, &kpi)).kind, AnalysisKind::Trend);
        assert_eq!(plan("销售额按省份", shape("direct-agg", 31, &kpi)).kind, AnalysisKind::Breakdown);

        let metric = plan("本月销售额", shape("direct-agg", 1, &kpi)).report_spec();
        assert!(metric.show_comparison && metric.show_contribution);
        assert!(!metric.include_recent_orders, "DWS 销售报告必须展示同窗经营明细，不得混入旧订单表");
        let trend = plan("今年各月销售额趋势", shape("direct-agg", 12, &kpi)).report_spec();
        assert!(trend.show_comparison, "有同口径比较窗口时，趋势页也应展示结构化同比/环比");
        assert!(plan("今年各月销售额趋势", shape("direct-agg", 12, &kpi)).allow_model_sections);
        let order = vec!["订单数".into()];
        let order_plan = plan("本月订单数", shape("direct-agg", 1, &order));
        assert!(!order_plan.dws_sales_metric, "订单数不得进入 DWS 销售事实报告合同");
    }

    #[test]
    fn wide_rows_are_detail_but_device_list_keeps_its_verified_plan() {
        let cols = vec!["单号".into(), "时间".into(), "客户".into(), "金额".into(), "状态".into()];
        let detail = plan("昨天的订单明细", shape("direct-doc", 20, &cols));
        assert_eq!(detail.kind, AnalysisKind::Detail);
        assert!(!detail.allow_model_sections);

        let device = plan("查询昨天的设备订单", shape("direct-doc", 20, &cols));
        assert_eq!(device.kind, AnalysisKind::Breakdown);
        assert!(device.allow_model_sections);
    }

    /// 【D8】自评档解析：三档 + 同义词 + 标点/大小写容错；无法识别 = None（降级不报错）
    #[test]
    fn acceptance_parse_is_tolerant_and_never_guesses() {
        assert_eq!(Acceptance::parse("met"), Some(Acceptance::Met));
        assert_eq!(Acceptance::parse("满足。"), Some(Acceptance::Met));
        assert_eq!(Acceptance::parse(" Partial,"), Some(Acceptance::Partial));
        assert_eq!(Acceptance::parse("部分满足"), Some(Acceptance::Partial));
        assert_eq!(Acceptance::parse("UNMET"), Some(Acceptance::Unmet));
        assert_eq!(Acceptance::parse("未满足"), Some(Acceptance::Unmet));
        assert_eq!(Acceptance::parse(""), None);
        assert_eq!(Acceptance::parse("基本达成"), None, "近义词不许猜档：判词缺席比错判诚实");
        // 序列化契约：稳定英文码，前端按 code 映射
        assert_eq!(serde_json::to_value(Acceptance::Met).unwrap(), serde_json::json!("met"));
        assert_eq!(serde_json::to_value(Acceptance::Partial).unwrap(), serde_json::json!("partial"));
        assert_eq!(serde_json::to_value(Acceptance::Unmet).unwrap(), serde_json::json!("unmet"));
        assert_eq!(Acceptance::Met.label(), "满足");
    }

    /// 【D8】断言清洗与收集：trim/截 80 字/空 = 无断言；收集跳过无断言板块、最多 8 条
    #[test]
    fn assertion_cleaning_and_collecting_degrade_gracefully() {
        assert_eq!(clean_assertion("  证明各省份贡献结构可核  "), Some("证明各省份贡献结构可核".into()));
        assert_eq!(clean_assertion("   "), None, "空白 = 无断言（降级）");
        assert_eq!(clean_assertion(""), None);
        let long = "验".repeat(100);
        assert_eq!(clean_assertion(&long).map(|s| s.chars().count()), Some(80), "超长截 80 字");

        let sections = vec![
            ("省份结构", Some("证明各省份销售额贡献结构清晰可核".to_string())),
            ("趋势", None),
            ("客户", Some("   ".to_string())),
            ("商品", Some("证明头部商品集中度可核".to_string())),
        ];
        let out = collect_assertions(sections.iter().map(|(t, a)| (*t, a)));
        assert_eq!(out.len(), 2, "无断言/空白言板块一律跳过");
        assert_eq!(out[0], Assertion { section: "省份结构".into(), text: "证明各省份销售额贡献结构清晰可核".into() });
        assert_eq!(out[1].section, "商品");
        // 刷屏闸：超过 8 条一律裁
        let flood: Vec<(String, Option<String>)> =
            (0..20).map(|i| (format!("板块{i}"), Some(format!("断言{i}")))).collect();
        assert_eq!(collect_assertions(flood.iter().map(|(t, a)| (t.as_str(), a))).len(), 8);
        // serde 契约：section 缺省可解（老数据/模型没给 section 键不炸）
        let a: Assertion = serde_json::from_str(r#"{"text":"证明口径一致"}"#).unwrap();
        assert_eq!(a.section, "");
    }
}
