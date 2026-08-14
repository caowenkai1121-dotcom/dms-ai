//! 统一回答协议。**前端与三个 python 判官的字节契约**：serde 属性逐字不许动。
//!
//! `#[serde(tag = "kind")]` 是写死的（ARCHITECTURE §5）：默认的 externally tagged 形态
//! 与 `Answer.body` 上的 `#[serde(flatten)]` 组合会在**运行时**报
//! `can only flatten structs and maps` —— 症状是 `/api/ask` 500 + 判官 JSONDecodeError，
//! 编译期一声不响。
//!
//! 三个变体覆盖今天的全部生产者：`Table`（NL2SQL 问数）、`Text`（知识库回答——
//! 知识库问答是它的首个消费者）、`Composite`（复合/hybrid 容器，主体是占位键 + `subs`）。
//! `Steps` 变体不建 —— 零生产者（ReAct 不做，ARCHITECTURE §8），真做时是 5 行。

use serde::Serialize;
use serde_json::Value;

use crate::present::ViewSpec;

/// 一次问答的最终产物。顶层键 = `route` + 变体展开的键 + 可选 `view`/`subs` + `elapsed_ms`
/// + 可选 `trace_id`。
#[derive(Debug, Serialize)]
pub struct Answer {
    /// 命中的路由标签（取 `hit.route`，不是 `Answerer::route()` 的表标签 —— 混用即回归全红）
    pub route: String,
    #[serde(flatten)]
    pub body: AnswerBody,
    /// Table 路径恒 `Some`；Text/Composite 路径 `None`（前端 `ResultPanel` 早退）。
    /// 「Table 恒 Some」是生产者约定（`view` 是 pub 字段，类型层不强制）——
    /// 各生产者构造 Table 时都必须给 view，前端按这个约定早退。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<ViewSpec>,
    /// 复合问题的子结果；单结果时空数组不上线
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subs: Vec<SubAnswer>,
    pub elapsed_ms: u128,
    /// 一次问答的关联键（`meta.query_log` / `meta.query_feedback` 靠它拼回同一次问答）。
    /// 只有自带落账的生产者（知识库问答）会填；`None` 不上线，问数/复合的 wire 一字不变。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// 回答主体。`tag = "kind"` + snake_case：`kind` 与主体键同层，前端按 `kind` 分派渲染。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerBody {
    /// 取数结果集（`sql` 是**注入后**的 wire 串，与迁移前 server 侧 `AskResult.sql` 同义）
    Table {
        sql: String,
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        /// wire 内行数 == `rows.len()`（截断时 `truncated=true`；不是总行数）
        row_count: usize,
        truncated: bool,
    },
    /// 引用式回答：markdown + 角标来源（角标 = `citations` 下标 + 1，不存字段；
    /// web 渲染侧按同一句契约实现，改动两处同步）
    Text { markdown: String, citations: Vec<Citation> },
    /// 复合容器：主体空但**继续输出 `sql`/`row_count`/`truncated` 占位键**（前端老字段兼容）
    Composite {
        sql: String,
        row_count: usize,
        truncated: bool,
        /// 🔴 `None` 时上线 `"summary": null` 是历史兼容（前端按 null 判空），
        /// 与全文件其它 Option「None 不上线」纪律不一致但**勿顺手加 skip** —— 那是 wire 变更。
        summary: Option<String>,
    },
}

/// 复合子问题：一句题目 + 完整结果（结构与顶层同形，前端分面板渲染）
#[derive(Debug, Serialize)]
pub struct SubAnswer {
    pub question: String,
    pub result: Answer,
}

/// 引用来源。字段集是裁决过的（`docs/superpowers/plans/_DECISIONS.md` 二·C）：
/// 前端点开原文要 `chunk_id` + `page`，塞进一个字符串 locator 等于让前端解字符串。
///
/// `Default` 只为测试构造用（字段多、且都是「有就填」的可选治理信息）；
/// 生产侧一律由 `knowledge::answer::citations` 从 `Hit` 填满。
#[derive(Debug, Serialize, Default)]
pub struct Citation {
    pub doc_id: String,
    pub doc_name: String,
    /// PG bigint 落 i64
    pub chunk_id: i64,
    /// PG int4 落 i32
    pub page: Option<i32>,
    pub heading_path: String,
    pub score: f32,
    /// 目录和关系召回线索；可见性已由 knowledge SQL 判定，前端只负责展示。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub folder_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<String>,
    /// 文档治理信息：用于核对生效期、来源与检索命中原因，不由前端猜文件名。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_revision: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_hash: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub doc_updated_at: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,
    /// 该引用实际覆盖的**连续块数**（检索会把同文档相邻块合并成一条命中，`chunk_id` 取首块）。
    ///
    /// 🔴 为什么必须带出来：不带它，「点开引用核对原文」就还原不出模型真正看到的那段 ——
    /// 实测一条引用合并了 5 块、支撑答案的那句话在第 5 块，而回查窗口只有 ±3，
    /// 读者点进去看不到那句话。引用的全部价值在**可核对**，还原不出等于没有引用。
    /// `None`/`Some(1)` = 单块（不出现在 JSON 里，老前端不改也不崩）。
    /// 块计数非负，故 u32。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<u32>,
}

impl Answer {
    /// 知识库回答（K2 唯一生产者）：无 `view`、无 `subs`。
    /// `route` 取 `qalog::ROUTE_KNOWLEDGE` —— wire 上的值与 KB 落账的 route 同一事实源。
    pub fn text(markdown: String, citations: Vec<Citation>, elapsed_ms: u128) -> Self {
        Answer {
            route: crate::qalog::ROUTE_KNOWLEDGE.into(),
            body: AnswerBody::Text { markdown, citations },
            view: None,
            subs: vec![],
            elapsed_ms,
            trace_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 走真实 wire 路径（to_string → 解析），而不是 `to_value`：flatten 的运行时坑只在实际序列化时暴露。
    fn wire(a: &Answer) -> Value {
        serde_json::from_str(&serde_json::to_string(a).unwrap()).unwrap()
    }

    fn citation() -> Citation {
        Citation {
            doc_id: "d1".into(),
            doc_name: "制度.pdf".into(),
            chunk_id: 42,
            page: Some(3),
            heading_path: "第一章 > 总则".into(),
            score: 0.8,
            folder_path: "/制度/财务".into(),
            relations: vec!["same_folder".into()],
            tags: vec!["制度".into()],
            business_domain: Some("财务".into()),
            effective_from: Some("2026-01-01".into()),
            effective_to: None,
            source_uri: None,
            document_family: Some("报销制度".into()),
            document_revision: Some("v2.1".into()),
            source_hash: "abc123".into(),
            doc_updated_at: "2026-08-06 00:00:00+00".into(), // PG timestamp 输出形态（fixture 按它钉）
            channels: vec!["向量".into(), "元数据".into()],
            span: None,
        }
    }

    #[test]
    fn text_answer_golden() {
        let j = wire(&Answer::text("正文".into(), vec![citation()], 12));
        assert!(j.is_object(), "flatten + tag 必须产出扁平 Object：{j}");
        assert_eq!(j["kind"], "text");
        assert_eq!(j["markdown"], "正文");
        assert_eq!(j["citations"][0]["chunk_id"], 42);
        assert_eq!(j["citations"][0]["page"], 3);
        assert_eq!(j["citations"][0]["business_domain"], "财务");
        assert_eq!(j["citations"][0]["channels"], serde_json::json!(["向量", "元数据"]));
        assert_eq!(j["route"], "knowledge");
        assert_eq!(j["elapsed_ms"], 12);
        assert!(j.get("view").is_none(), "view=None 不上线");
        assert!(j.get("subs").is_none(), "subs 空不上线");
    }

    /// `trace_id`：`None` 不上线（问数/复合 wire 一键不多），`Some` 才出现 ——
    /// KB 回答的 👍/👎 反馈就绑这个键；`route` 与落账常量同值是绑定前提。
    #[test]
    fn trace_id_only_on_wire_when_set() {
        let mut a = Answer::text("正文".into(), vec![], 5);
        let j = wire(&a);
        assert!(j.get("trace_id").is_none(), "None 不许上线: {j}");
        assert_eq!(j["route"], crate::qalog::ROUTE_KNOWLEDGE);
        a.trace_id = Some("t-1".into());
        let j = wire(&a);
        assert_eq!(j["trace_id"], "t-1");
    }

    #[test]
    fn table_answer_has_eight_top_keys() {
        let a = Answer {
            route: "llm".into(),
            body: AnswerBody::Table {
                sql: "SELECT 1 LIMIT 200".into(),
                columns: vec!["c".into()],
                rows: vec![vec![Value::from(1)]],
                row_count: 1,
                truncated: false,
            },
            view: None,
            subs: vec![],
            elapsed_ms: 3,
            trace_id: None,
        };
        let j = wire(&a);
        let obj = j.as_object().unwrap();
        for k in ["kind", "sql", "columns", "rows", "row_count", "truncated", "route", "elapsed_ms"] {
            assert!(obj.contains_key(k), "缺顶层键 {k}：{j}");
        }
        assert_eq!(obj.len(), 8, "顶层键必须恰好 8 个：{j}");
        assert_eq!(j["kind"], "table");
    }

    #[test]
    fn composite_keeps_placeholder_keys_and_subs() {
        let a = Answer {
            route: "compound".into(),
            body: AnswerBody::Composite {
                sql: "[复合问题拆解]".into(),
                row_count: 0,
                truncated: false,
                summary: None,
            },
            view: None,
            subs: vec![SubAnswer {
                question: "子问题".into(),
                result: Answer::text("子答".into(), vec![], 1),
            }],
            elapsed_ms: 9,
            trace_id: None,
        };
        let j = wire(&a);
        assert_eq!(j["kind"], "composite");
        assert_eq!(j["sql"], "[复合问题拆解]");
        assert_eq!(j["row_count"], 0);
        assert_eq!(j["truncated"], false);
        assert_eq!(j["subs"][0]["question"], "子问题");
        assert_eq!(j["subs"][0]["result"]["kind"], "text");
    }

    /// 🔴 Citation 的「全默认字段」键集合金标：15 个字段逐个手写了 skip 属性，
    /// 新增字段忘加 skip 就会悄悄改 wire —— 键集合在这里钉死，多一个键当场红。
    #[test]
    fn citation_minimal_keys_golden() {
        let c = Citation {
            doc_id: "d".into(),
            doc_name: "n".into(),
            chunk_id: 1,
            page: None,
            heading_path: String::new(),
            score: 0.5,
            folder_path: String::new(),
            relations: vec![],
            tags: vec![],
            business_domain: None,
            effective_from: None,
            effective_to: None,
            source_uri: None,
            document_family: None,
            document_revision: None,
            source_hash: String::new(),
            doc_updated_at: String::new(),
            channels: vec![],
            span: None,
        };
        let j = serde_json::to_value(&c).unwrap();
        let mut keys: Vec<&str> = j.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        // page 无 skip 属性（null 也上线，老前端契约）；其余 Option/空集合一律不上线
        assert_eq!(keys, ["chunk_id", "doc_id", "doc_name", "heading_path", "page", "score"]);
    }
}
