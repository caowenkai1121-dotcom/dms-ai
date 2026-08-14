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
    ///
    /// `sections` 是同一份 markdown 的**分节视图**（`split_sections` 从 markdown 切出来，
    /// 唯一生产者是 `Answer::text`，不可能与 markdown 漂）。空数组不上线，老前端一字不改。
    Text {
        markdown: String,
        citations: Vec<Citation>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        sections: Vec<Section>,
    },
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
            body: AnswerBody::Text {
                sections: split_sections(&markdown),
                markdown,
                citations,
            },
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

/// 一节的**形态**。由这一节的正文实际是什么判定 —— 不看标题的中文措辞。
///
/// 🔴 由来（业主 2026-08-15：「以后你给出的答案类型不是固定的，要结合数据让大模型
/// 动态调整，来优化显示」）：此前「知识库答案长什么样」被钉死在两处 ——
/// 后端提示词点名四个模块名（直接结论/关键要点/操作步骤/版本与差异），
/// 前端再用一串中文正则（`headingClass`）按标题措辞上色。模型换个说法叫「费用标准」，
/// 前端就只剩默认样式。两处白名单，一个固定模板。
///
/// 现在：标题由模型自己写（写什么就是什么），**渲染看形态**——
/// 这一节是表就当表渲染，是步骤就编号，是要点就列点。内容决定显示。
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SectionShape {
    /// 有 Markdown 表格（表头 + `---` 分隔行）
    Table,
    /// 有序列表：有先后的流程
    Steps,
    /// 无序列表：并列要点
    Bullets,
    /// 其余散文段落
    Prose,
}

/// 引用式回答的一节：模型自己写的标题 + 内容判出的形态 + 该节原文。
#[derive(Debug, Serialize, Clone)]
pub struct Section {
    /// 模型写的标题原文（首节可能没有标题，此时为空串）
    pub title: String,
    pub shape: SectionShape,
    /// 该节正文（**不含**标题行）。角标 `[^n]` 原样保留 —— 分节不碰引用。
    pub markdown: String,
}

/// Markdown → 分节。纯函数：同一份 markdown 恒得同一组节。
///
/// 切分点是 `#` 标题行（1–4 级）。首个标题之前的正文自成一节（标题空串）。
/// 围栏代码块内的 `#` 与 `|` 一律不算 —— 那是代码不是结构。
pub fn split_sections(markdown: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = vec![];
    let mut title = String::new();
    let mut body: Vec<&str> = vec![];
    let mut in_code = false;
    let mut push = |title: &mut String, body: &mut Vec<&str>| {
        let text = body.join("\n");
        // 标题与正文都空的节不产出（文首空行、连续标题之间）
        if !title.is_empty() || !text.trim().is_empty() {
            sections.push(Section {
                shape: shape_of(&text),
                title: std::mem::take(title),
                markdown: text.trim_matches('\n').to_string(),
            });
        }
        body.clear();
    };
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            body.push(line);
            continue;
        }
        match heading_text(line).filter(|_| !in_code) {
            Some(text) => {
                push(&mut title, &mut body);
                title = text.to_string();
            }
            None => body.push(line),
        }
    }
    push(&mut title, &mut body);
    sections
}

/// `## 标题` → `Some("标题")`。`#` 后必须有空白且标题非空，否则不是标题行
/// （`#tag`、`####` 不是）。
fn heading_text(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let hashes = rest.chars().take_while(|c| *c == '#').count();
    if !(1..=4).contains(&hashes) {
        return None;
    }
    let text = rest[hashes..].trim();
    (rest[hashes..].starts_with(char::is_whitespace) && !text.is_empty()).then_some(text)
}

/// 形态判定：表 > 步骤 > 要点 > 散文。围栏内的行不参与。
fn shape_of(body: &str) -> SectionShape {
    let mut in_code = false;
    let mut pipes = 0usize;
    let mut delimiter = false;
    let mut steps = false;
    let mut bullets = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if t.starts_with('|') && t.ends_with('|') && t.len() > 2 {
            pipes += 1;
            // 分隔行：`| --- | :--: |` —— 只由 `|`/`-`/`:`/空白组成
            if t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) && t.contains('-') {
                delimiter = true;
            }
        }
        let after_marker = t.trim_start_matches(|c: char| c.is_ascii_digit());
        if after_marker.len() < t.len()
            && (after_marker.starts_with(". ") || after_marker.starts_with(") "))
        {
            steps = true;
        }
        if (t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")) && t.len() > 2 {
            bullets = true;
        }
    }
    // 表要求「表头 + 分隔行 + 至少一行数据」——只有一根竖线的散文行不算表
    if delimiter && pipes >= 3 {
        SectionShape::Table
    } else if steps {
        SectionShape::Steps
    } else if bullets {
        SectionShape::Bullets
    } else {
        SectionShape::Prose
    }
}

#[cfg(test)]
mod section_tests {
    use super::*;

    #[test]
    fn sections_split_on_headings_and_keep_the_models_own_titles() {
        let md = "## 直接结论\n\n上限每晚八百元[^1]。\n\n## 费用标准\n\n| 项目 | 上限 |\n| --- | --- |\n| 住宿 | 800[^1] |\n\n## 报销步骤\n\n1. 上传发票[^2]\n2. 关联单号[^2]\n";
        let s = split_sections(md);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].title, "直接结论");
        assert_eq!(s[0].shape, SectionShape::Prose);
        // 标题不在白名单里也照样成节 —— 这正是要治的那件事
        assert_eq!(s[1].title, "费用标准");
        assert_eq!(s[1].shape, SectionShape::Table);
        assert_eq!(s[2].shape, SectionShape::Steps);
        // 角标不许在分节时丢
        assert!(s[1].markdown.contains("[^1]") && s[2].markdown.contains("[^2]"));
        // 正文不含标题行
        assert!(!s[0].markdown.contains('#'));
    }

    #[test]
    fn prose_before_the_first_heading_is_its_own_untitled_section() {
        let s = split_sections("先说一句[^1]。\n\n## 要点\n\n- 甲[^1]\n- 乙[^2]\n");
        assert_eq!(s.len(), 2);
        assert!(s[0].title.is_empty());
        assert_eq!(s[0].shape, SectionShape::Prose);
        assert_eq!(s[1].shape, SectionShape::Bullets);
    }

    /// 围栏里的 `#` 是注释、`|` 是表格字符画，都不是结构
    #[test]
    fn fenced_code_never_creates_structure() {
        let md = "## 示例\n\n```sql\n# 这不是标题\n| a | b |\n| --- | --- |\n```\n\n正文一句[^1]。\n";
        let s = split_sections(md);
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(s[0].title, "示例");
        assert_eq!(s[0].shape, SectionShape::Prose, "围栏里的表不算表");
    }

    /// 一根竖线的散文不是表（「A|B 两种口径」）
    #[test]
    fn a_stray_pipe_is_not_a_table() {
        let s = split_sections("## 说明\n\n甲|乙 两种口径都可以[^1]。\n");
        assert_eq!(s[0].shape, SectionShape::Prose);
    }

    /// 空 markdown 不产出空节
    #[test]
    fn empty_markdown_yields_no_sections() {
        assert!(split_sections("").is_empty());
        assert!(split_sections("\n\n  \n").is_empty());
    }
}
